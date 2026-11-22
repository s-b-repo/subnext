//! Relevance Planner — decides what the model actually sees.
//!
//! This is the component that keeps `k` small:
//!
//! 1. **Seed** from `q_t` by index search at the cheapest plausible level.
//! 2. **Expand** along dependency edges (capped depth *and* fan-out — the
//!    over-expansion failure mode pulls in the transitive world).
//! 3. **Level-assign** each node via the decision policy.
//! 4. **Bound** to `B_attention` with the knapsack, which demotes before it
//!    drops.
//! 5. **Speculate** on what is left over.
//!
//! `U(x)` is the dominant failure mode of the whole design: get it wrong and
//! the optimiser confidently fills the window with cheap-and-useless material
//! *while looking efficient*. It is therefore a small, inspectable weighted sum
//! with every term named, and [`RelevancePlanner::explain_plan`] prints the
//! per-node arithmetic so a bad answer can be traced to the term that caused it.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::budget::{Candidate, Choice, solve};
use crate::graph::MemoryGraph;
use crate::index::{HybridIndex, Namespace};
use crate::ladder::{EvidenceBudget, EvidenceExcerpt, Ladder, Level};
use crate::nodes::{EdgeType, Kind, Node, NodeIdx, Status};
use crate::policy::{DecisionPolicy, QueryType};
use crate::spans::RawStore;
use crate::speculation::{Prediction, Speculator};

#[derive(Debug, Clone)]
pub struct Weights {
    pub similarity: f32,
    pub proximity: f32,
    pub explain_path: f32,
    pub confidence: f32,
    pub read_through: f32,
    pub recency: f32,
    pub stale_penalty: f32,
    /// Penalty for evidence whose only dependents were superseded — otherwise a
    /// corrected fact's original wording keeps competing with the correction.
    pub superseded_source: f32,
    /// A node with a live `contradicts` edge is *more* useful, not less: the
    /// model should adjudicate rather than inherit whichever side won.
    pub contradiction_bonus: f32,
    /// Hysteresis: bonus for a node admitted in the previous window, so
    /// membership stops oscillating turn to turn. Deliberately *not* part of
    /// what makes a node worth knowing — it is the minimal-edit objective
    /// `U(S) − λ·edit(S, S_prev)` with λ paid as a bonus, and it exists so a
    /// prompt cache can reuse a prefix. `0.0` disables it, which is the
    /// default: it trades probe correctness for stability, and that trade
    /// must be measured (`bench --cache`), not assumed.
    pub stickiness: f32,
    pub decay: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            similarity: 1.0,
            proximity: 0.8,
            explain_path: 1.5,
            confidence: 0.4,
            read_through: 0.3,
            recency: 0.2,
            stale_penalty: 2.0,
            superseded_source: 1.2,
            contradiction_bonus: 0.6,
            stickiness: 0.0,
            decay: 0.6,
        }
    }
}

impl Weights {
    pub fn kind_prior(&self, kind: Kind) -> f32 {
        match kind {
            Kind::Goal | Kind::Constraint => 0.5,
            Kind::Decision => 0.25,
            Kind::Claim | Kind::Calculation => 0.2,
            Kind::OpenQuestion => 0.1,
            // A skill competes on its description's similarity alone; the
            // prior only keeps a marginally relevant one from crowding out a
            // fact of equal score.
            Kind::Skill => 0.1,
            Kind::Evidence => 0.0,
        }
    }
}

/// Fidelity of a level *for a given question shape*.
fn level_fit(qtype: QueryType, level: Level) -> f32 {
    use Level::{L0, L1, L2, L3};
    match (qtype, level) {
        (QueryType::QuoteExact, L0) => 1.0,
        (QueryType::QuoteExact, L1) => 0.55,
        (QueryType::QuoteExact, L2) => 0.35,
        (QueryType::QuoteExact, L3) => 0.4,
        (QueryType::ValueLookup, L0) => 0.85,
        (QueryType::ValueLookup, L1) => 0.8,
        (QueryType::ValueLookup, L2) => 1.0,
        (QueryType::ValueLookup, L3) => 0.9,
        (QueryType::Justify, L0) => 0.9,
        (QueryType::Justify, L1) => 0.85,
        (QueryType::Justify, L2) => 0.95,
        (QueryType::Justify, L3) => 0.9,
        (QueryType::Recompute, L0) => 0.7,
        (QueryType::Recompute, L1) => 0.5,
        (QueryType::Recompute, L2) => 0.8,
        (QueryType::Recompute, L3) => 1.0,
        (QueryType::Summarize, L0) => 0.7,
        (QueryType::Summarize, L1) => 1.0,
        (QueryType::Summarize, L2) => 0.85,
        (QueryType::Summarize, L3) => 0.6,
        (QueryType::Open, L0) => 0.7,
        (QueryType::Open, L1) => 0.85,
        (QueryType::Open, L2) => 1.0,
        (QueryType::Open, L3) => 0.8,
    }
}

fn merge_seed(seeds: &mut Vec<(NodeIdx, f32)>, idx: NodeIdx, score: f32) {
    if let Some((_, existing)) = seeds.iter_mut().find(|(node, _)| *node == idx) {
        *existing = existing.max(score);
    } else {
        seeds.push((idx, score));
    }
}

/// The named terms of `U(x)`, kept so a plan can be audited term by term.
#[derive(Debug, Clone, Copy, Default)]
pub struct UtilityTerms {
    pub similarity: f32,
    pub proximity: f32,
    pub explain_path: f32,
    pub confidence: f32,
    pub read_through: f32,
    pub recency: f32,
    pub kind_prior: f32,
    pub contradiction: f32,
    pub stale: f32,
    pub superseded_source: f32,
    /// Not a property of the node — hysteresis against last turn's window.
    /// Named separately so a plan can be audited with the retention bonus
    /// visible rather than folded into a relevance term it does not belong to.
    pub stickiness: f32,
}

impl UtilityTerms {
    /// All weight in one term, for the constant-cost scorer control. Not a
    /// scoring mode — it exists so the stage can be removed without disturbing
    /// the candidate set the other stages see.
    pub fn constant(value: f32) -> Self {
        Self {
            similarity: value,
            ..Default::default()
        }
    }

    pub fn base(&self) -> f32 {
        self.similarity
            + self.proximity
            + self.explain_path
            + self.confidence
            + self.read_through
            + self.recency
            + self.kind_prior
            + self.contradiction
            + self.stale
            + self.superseded_source
            + self.stickiness
    }

    pub fn named(&self) -> Vec<(&'static str, f32)> {
        vec![
            ("similarity", self.similarity),
            ("proximity", self.proximity),
            ("explain_path", self.explain_path),
            ("confidence", self.confidence),
            ("read_through", self.read_through),
            ("recency", self.recency),
            ("kind_prior", self.kind_prior),
            ("contradiction", self.contradiction),
            ("stale", self.stale),
            ("superseded_source", self.superseded_source),
            ("stickiness", self.stickiness),
        ]
    }
}

/// Per-stage clocks and rejection counts for one plan.
///
/// Proposed by [@cwahq](https://www.moltbook.com/post/78237a57-17ef-4c78-b05f-8c1e5a944196):
/// *"split candidate generation, scoring, and graph expansion into separate
/// clocks, then publish the rejected-candidate count at each stage… 'planning'
/// is still too broad to own the cost."*
///
/// Every latency figure this repo published before this struct existed was
/// taken at the outer edge of a turn, which makes planning a *residual* rather
/// than a measurement. A stage with no clock can absorb any amount of the
/// budget and never appear in a table, and the failure mode is not that it is
/// slow — it is that the table then reads as complete.
///
/// The rejection counts are the same argument at the correctness layer. A stage
/// that discards candidates without publishing how many is unfalsifiable in the
/// way `stale_fact_read_rate: 0.0` was: nothing about it can fail.
#[derive(Debug, Clone, Copy, Default)]
pub struct StageProfile {
    // -- clocks ------------------------------------------------------------
    /// One clock around the whole `plan()` call, measured independently of the
    /// per-stage clocks below.
    ///
    /// Without this, `total()` is the *sum* of the stages, so "the stages
    /// account for the planner" is a tautology: work in an unclocked stage does
    /// not appear as a residual, it does not appear at all. That is a control
    /// that cannot fail, and it was sitting inside instrumentation built
    /// precisely because an unclocked stage had hidden a cost. A reader summed
    /// the published columns and asked what the leftover meant; the honest
    /// answer was that the leftover could not mean anything until this existed.
    pub measured_time: Duration,
    pub seed_time: Duration,
    pub expand_time: Duration,
    pub pin_time: Duration,
    pub score_time: Duration,
    pub knapsack_time: Duration,
    pub speculate_time: Duration,

    // -- seed --------------------------------------------------------------
    /// Hits the index returned before any of this stage's filters ran.
    pub seed_hits: usize,
    pub seed_dropped_floor: usize,
    pub seed_dropped_recency: usize,
    /// Candidates past the age floor that reactivation-aware pruning spared:
    /// old, but read often enough (or confident enough) that dropping them
    /// would be the expensive kind of forgetting.
    pub seed_spared_reactivation: usize,
    pub seed_dropped_status: usize,
    pub seed_from_spans: usize,
    pub seeds_kept: usize,

    // -- expand ------------------------------------------------------------
    pub expand_reached: usize,
    /// `max_candidates` stopped the BFS early. If this is true at large N then
    /// "planning cost grows with N" and "the cap is silently truncating" are
    /// the same observation, and only one of them is a latency problem.
    pub expand_capped: bool,

    // -- pin ---------------------------------------------------------------
    pub pinned_added: usize,
    /// Nodes `by_kind` visited to find them. Uncapped and linear in the graph.
    pub pin_scanned: usize,

    // -- score -------------------------------------------------------------
    pub score_considered: usize,
    pub score_dropped_cap: usize,
    pub score_dropped_superseded: usize,
    pub score_dropped_stale: usize,
    pub score_dropped_no_level: usize,
    /// Source spans concatenated while pricing the candidates of one plan.
    ///
    /// `Ladder::available` renders `raw_text` — every source span of the node,
    /// joined — just to ask whether the text exceeds 40 tokens. Corroboration
    /// collapses agreeing spans behind one node, so a node's span list grows
    /// with history, and this counter is what shows the scoring stage doing
    /// work linear in N over a candidate set that is capped at 120.
    pub score_spans_priced: usize,

    // -- knapsack ----------------------------------------------------------
    pub knapsack_candidates: usize,
    pub knapsack_dropped: usize,
    pub knapsack_demoted: usize,

    /// Rendering the admitted set after the knapsack has chosen it.
    ///
    /// This stage had no clock in the first version of this struct, which is
    /// the exact failure the struct exists to prevent.
    pub admit_time: Duration,
    pub admitted: usize,
}

impl std::ops::AddAssign for StageProfile {
    /// Sum stage costs across the re-plans of one turn, or across a whole run.
    /// `expand_capped` is a disjunction: the cap bound at least once.
    fn add_assign(&mut self, rhs: Self) {
        self.measured_time += rhs.measured_time;
        self.seed_time += rhs.seed_time;
        self.expand_time += rhs.expand_time;
        self.pin_time += rhs.pin_time;
        self.score_time += rhs.score_time;
        self.knapsack_time += rhs.knapsack_time;
        self.speculate_time += rhs.speculate_time;
        self.seed_hits += rhs.seed_hits;
        self.seed_dropped_floor += rhs.seed_dropped_floor;
        self.seed_dropped_recency += rhs.seed_dropped_recency;
        self.seed_spared_reactivation += rhs.seed_spared_reactivation;
        self.seed_dropped_status += rhs.seed_dropped_status;
        self.seed_from_spans += rhs.seed_from_spans;
        self.seeds_kept += rhs.seeds_kept;
        self.expand_reached += rhs.expand_reached;
        self.expand_capped |= rhs.expand_capped;
        self.pinned_added += rhs.pinned_added;
        self.pin_scanned += rhs.pin_scanned;
        self.score_considered += rhs.score_considered;
        self.score_dropped_cap += rhs.score_dropped_cap;
        self.score_dropped_superseded += rhs.score_dropped_superseded;
        self.score_dropped_stale += rhs.score_dropped_stale;
        self.score_dropped_no_level += rhs.score_dropped_no_level;
        self.knapsack_candidates += rhs.knapsack_candidates;
        self.knapsack_dropped += rhs.knapsack_dropped;
        self.knapsack_demoted += rhs.knapsack_demoted;
        self.score_spans_priced += rhs.score_spans_priced;
        self.admit_time += rhs.admit_time;
        self.admitted += rhs.admitted;
    }
}

impl StageProfile {
    /// Total time inside the planner. Named `planning` everywhere it is
    /// reported, so the residual can finally be checked against a sum.
    pub fn total(&self) -> Duration {
        self.seed_time
            + self.expand_time
            + self.pin_time
            + self.score_time
            + self.knapsack_time
            + self.admit_time
            + self.speculate_time
    }

    /// What the per-stage clocks failed to account for.
    ///
    /// Saturating, because the stages are sampled inside a call the outer clock
    /// brackets, so tiny negative values are measurement noise rather than
    /// findings. A residual that grows with N is an unclocked stage.
    pub fn residual(&self) -> Duration {
        self.measured_time.saturating_sub(self.total())
    }

    pub fn clocks(&self) -> [(&'static str, Duration); 7] {
        [
            ("seed", self.seed_time),
            ("expand", self.expand_time),
            ("pin", self.pin_time),
            ("score", self.score_time),
            ("knapsack", self.knapsack_time),
            ("admit", self.admit_time),
            ("speculate", self.speculate_time),
        ]
    }

    /// Candidates discarded per stage, in pipeline order.
    pub fn rejections(&self) -> [(&'static str, usize); 8] {
        [
            ("seed:floor", self.seed_dropped_floor),
            ("seed:recency", self.seed_dropped_recency),
            ("seed:status", self.seed_dropped_status),
            ("expand:capped", usize::from(self.expand_capped)),
            ("score:cap", self.score_dropped_cap),
            ("score:superseded", self.score_dropped_superseded),
            ("score:stale", self.score_dropped_stale),
            ("knapsack:dropped", self.knapsack_dropped),
        ]
    }
}

#[derive(Debug, Clone)]
pub struct Admission {
    pub node: NodeIdx,
    pub node_id: String,
    pub kind: Kind,
    pub label: String,
    pub level: Level,
    pub cost: usize,
    pub utility: f32,
    pub reason: &'static str,
    pub pinned: bool,
    pub rendered: String,
    pub terms: UtilityTerms,
}

/// The tiny working set — `k` — plus everything needed to audit it.
#[derive(Debug, Clone, Default)]
pub struct ActiveContext {
    pub query: String,
    pub qtype: Option<QueryType>,
    pub budget: usize,
    pub entries: Vec<Admission>,
    pub tokens: usize, // audit-allow: LM token count, not a credential
    pub seeds: Vec<NodeIdx>,
    pub dropped: Vec<NodeIdx>,
    pub demoted: Vec<(NodeIdx, Level, Level)>,
    pub prefetch: Vec<Prediction>,
    pub considered: usize,
    pub snapshot_version: u64,
    pub explain_paths: Vec<(NodeIdx, Vec<NodeIdx>)>,
    pub stale_seen: Vec<NodeIdx>,
    /// Nodes shown as verbatim excerpts of their L0 spans.
    pub l0_excerpts: HashSet<NodeIdx>,
    pub overflow: bool,
    /// Per-stage clocks and rejection counts. See [`StageProfile`].
    pub profile: StageProfile,
}

impl ActiveContext {
    pub fn node_ids(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.node_id.as_str()).collect()
    }

    pub fn nodes(&self) -> Vec<NodeIdx> {
        self.entries.iter().map(|e| e.node).collect()
    }

    pub fn level_of(&self, node_id: &str) -> Option<Level> {
        self.entries
            .iter()
            .find(|e| e.node_id == node_id)
            .map(|e| e.level)
    }

    /// The prompt text. Grouped by role, not by score, because a model reads a
    /// labelled state block far more reliably than a ranked list.
    ///
    /// Within a group entries are ordered by node identity — creation order,
    /// since ids are assigned sequentially — never by the per-turn utility
    /// score. Membership still moves with the knapsack every turn, but the
    /// *order* of surviving material is stable across turns, which is what lets
    /// a downstream prompt cache reuse the prefix (`bench --cache`).
    pub fn render(&self) -> String {
        self.render_with_header(true)
    }

    pub fn render_with_header(&self, with_header: bool) -> String {
        self.render_ordered(with_header, true)
    }

    /// Legacy per-turn utility ordering + leading header, kept for the
    /// `bench --instrument` before/after comparison. Do not use for assembly.
    pub fn render_legacy(&self) -> String {
        self.render_ordered(true, false)
    }

    pub fn render_legacy_with_header(&self, with_header: bool) -> String {
        self.render_ordered(with_header, false)
    }

    // `_stable` is vestigial: the content-addressed, turn-invariant ordering
    // lives in `render_prefix_stable`; this ranked layout always uses per-turn
    // utility order (see tests/planner.rs::prefix_stable_orders_by_content_not_utility).
    pub fn render_ordered(&self, with_header: bool, _stable: bool) -> String {
        let mut sorted: Vec<&Admission> = self.entries.iter().collect();
        sorted.sort_by(|a, b| {
            b.utility
                .total_cmp(&a.utility)
                .then(a.node_id.cmp(&b.node_id))
        });
        let groups = Self::render_groups(&sorted);
        if with_header {
            format!(
                "# ACTIVE CONTEXT  (k={}/{} tokens, {} of {} candidates, query type: {})\n{groups}",
                self.tokens,
                self.budget,
                self.entries.len(),
                self.considered,
                self.qtype.map(QueryType::as_str).unwrap_or("open")
            )
        } else {
            groups
        }
    }

    /// The prompt text, laid out for prompt-cache reuse across turns.
    ///
    /// `render` optimises for a model reading one turn; this optimises for the
    /// *next* turn reusing this turn's bytes — providers bill a cached prefix
    /// at a large discount, and the ranked layout re-derives those bytes every
    /// turn. Three rules:
    ///
    /// 1. The header is turn-invariant. The per-turn stats `render` leads
    ///    with — k, candidate count, query type — move to a trailer *after*
    ///    all content: they are for the model, and placed first they
    ///    invalidate the cache at byte ~30, before any content is compared.
    /// 2. Membership is the planner's job; order is the content's job.
    ///    Groups keep the fixed kind order, and within a group entries sort
    ///    by `node_id` — content-addressed, so a node whose utility moved
    ///    keeps its place as long as its text did.
    /// 3. Per-turn data never precedes content.
    ///
    /// Membership still churns — that is the knapsack doing its job — so the
    /// shared prefix ends at the first admitted or evicted node. What this
    /// buys is everything before that point: the header, the pinned goals
    /// and constraints, and every retained block up to the first edit.
    pub fn render_prefix_stable(&self) -> String {
        let mut sorted: Vec<&Admission> = self.entries.iter().collect();
        sorted.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        format!(
            "# ACTIVE CONTEXT{}\n\n---\nk={}/{} tokens, {} of {} candidates, query type: {}",
            Self::render_groups(&sorted),
            self.tokens,
            self.budget,
            self.entries.len(),
            self.considered,
            self.qtype.map(QueryType::as_str).unwrap_or("open")
        )
    }

    /// Entries grouped under their fixed kind titles. The caller owns the
    /// sort order; this owns only the grouping.
    fn render_groups(sorted: &[&Admission]) -> String {
        let order = [
            (Kind::Goal, "GOALS"),
            (Kind::Constraint, "CONSTRAINTS"),
            // Rendered only when admitted, and admission is what fires the
            // loader — so "activated" is accurate by construction, not a claim
            // the model has to take on faith.
            (Kind::Skill, "SKILLS (activated)"),
            (Kind::Claim, "FACTS (cached state)"),
            (Kind::Calculation, "COMPUTED"),
            (Kind::Decision, "DECISIONS"),
            (Kind::OpenQuestion, "OPEN QUESTIONS"),
            (Kind::Evidence, "EVIDENCE (raw spans)"),
        ];
        let mut blocks: Vec<String> = Vec::new();
        for (kind, title) in order {
            let group = sorted.iter().filter(|e| e.kind == kind);
            let mut group = group.peekable();
            if group.peek().is_none() {
                continue;
            }
            blocks.push(format!("\n## {title}"));
            for entry in group {
                blocks.push(entry.rendered.clone());
            }
        }
        blocks.join("\n")
    }

    /// Render with an appended `## ESCALATED` block (escalation-as-append), so a
    /// mid-turn re-plan extends the cache prefix instead of invalidating it.
    pub fn render_escalated(&self, escalated: &str) -> String {
        format!("{}\n\n## ESCALATED\n{}", self.render(), escalated)
    }

    /// Alias for [`Self::render_escalated`].
    pub fn render_with_escalation(&self, escalated: &str) -> String {
        self.render_escalated(escalated)
    }

    /// Alias for [`Self::render_escalated`].
    pub fn render_escalation(&self, escalated: &str) -> String {
        self.render_escalated(escalated)
    }

    /// Append a dynamic trailer (per-turn stats) after stable material.
    pub fn render_with_trailer(&self, trailer: &str) -> String {
        format!("{}\n{}", self.render(), trailer)
    }
    pub fn manifest(&self) -> Vec<(String, Level, usize, f32)> {
        self.entries
            .iter()
            .map(|e| (e.node_id.clone(), e.level, e.cost, e.utility))
            .collect()
    }
}

/// Everything the planner reads. Borrowed, never owned — the runtime owns the
/// components and hands out one view of them per turn.
pub struct PlanCtx<'a> {
    pub raw: &'a RawStore,
    pub graph: &'a MemoryGraph,
    pub index: &'a HybridIndex,
    pub ladder: &'a Ladder,
    pub policy: &'a DecisionPolicy,
    pub now: u64,
    pub evidence_by_span: &'a HashMap<String, String>,
    /// Node ids admitted by the previous plan — the `S_prev` of the
    /// minimal-edit objective. Empty on the first turn and whenever stickiness
    /// is disabled.
    pub last_assembly: &'a HashSet<String>,
}

pub(crate) enum PlanOutput<'a> {
    Context(usize),
    Evidence(EvidenceBudget, &'a crate::semantic::Matches),
}

#[derive(Debug, Clone)]
pub struct RelevancePlanner {
    pub weights: Weights,
    pub budget: usize,
    pub seed_k: usize,
    pub max_depth: usize,
    pub max_fanout: usize,
    pub max_candidates: usize,
    /// Seeds scoring below this fraction of the top hit are dropped. Without
    /// it, `seed_k` always returns `k` nodes however weak, and weak seeds are
    /// exactly the cheap-but-useless material a utility-per-token optimiser is
    /// happy to pack the window with.
    pub seed_min_ratio: f32,
    /// Drop candidates older than this fraction of the store's age span before
    /// scoring, rather than scoring everything.
    ///
    /// Suggested by a reader who measured a halving of latency at no accuracy
    /// cost on a 1.5k-node graph over a vector store. `0.0` disables it, which
    /// is the default — the trade is real but it is a correctness knob wearing a
    /// latency costume, and the probes it should break first are exactly the
    /// ones that reach furthest back: `old fact, never repeated` and
    /// `corrected fact (mid-history)`.
    pub recency_cutoff: f32,
    /// Reactivation-aware pruning: a candidate past the age floor is dropped
    /// only when its predicted reactivation probability — recency, read
    /// frequency and confidence jointly, via [`crate::eviction::ReactivationPredictor`]
    /// — falls below `reactivation_floor`. Off by default: the
    /// "when fancy eviction fails" finding predicts plain recency is hard to
    /// beat, so the predictor has to earn its keep in the A/B
    /// (`bench --reactivation`) before this belongs in a default.
    pub reactivation_prune: bool,
    /// Probability below which an aged candidate is dropped under
    /// `reactivation_prune`.
    pub reactivation_floor: f32,
    /// Causal control only. When true the utility function is replaced by a
    /// constant, leaving the candidate set, the corpus, the node count and every
    /// other stage untouched.
    ///
    /// Proposed by a reader as the intervention that "scoring is 85% of
    /// planning" needs: an 85% share is a correlation until you remove the
    /// stage and watch the curve. If latency collapses, scoring is the cause;
    /// if it persists, the share was real and the explanation incomplete.
    /// Answers are meaningless under this flag — it exists to move a clock, not
    /// to serve a query. Never enable in real use.
    pub stub_scorer: bool,
    /// Negative control only. When true, stale nodes are planned as if fresh
    /// instead of being skipped — used to prove `stale_fact_read_rate` can
    /// return nonzero, so that a zero in production is a fired guard rather
    /// than a guard that was never exercised. Never enable in real use.
    pub admit_stale: bool,
    /// The reactivation predictor behind `reactivation_prune`. Weights are a
    /// hand-set prior until the A/B justifies learning them.
    pub reactivation: crate::eviction::ReactivationPredictor,
}

impl Default for RelevancePlanner {
    fn default() -> Self {
        Self {
            weights: Weights::default(),
            budget: 1200,
            seed_k: 8,
            max_depth: 3,
            max_fanout: 6,
            max_candidates: 120,
            seed_min_ratio: 0.5,
            recency_cutoff: 0.0,
            reactivation_prune: false,
            reactivation_floor: 0.5,
            stub_scorer: false,
            admit_stale: false,
            reactivation: crate::eviction::ReactivationPredictor::default(),
        }
    }
}

impl RelevancePlanner {
    /// A stable digest of every knob that decides which candidate is rejected
    /// and why.
    ///
    /// A reader's point: a receipt reading `guard fired 4/4` records that *a*
    /// rule rejected the adversarial candidate, not *which version* of it. Change
    /// a weight, a cap or the seed floor and every historical trace silently
    /// re-interprets — the output is identical and its meaning is not. Binding
    /// this to the result makes a pass attributable to a named, versioned rule
    /// rather than to "the policy as it stands today".
    ///
    /// Covers the utility weights, the seed floor, the depth and fan-out caps,
    /// the candidate cap and the two control flags. It does **not** cover the
    /// routing table or the extractor, both of which also change what gets
    /// rejected — so this narrows the gap rather than closing it, and the
    /// credits say so.
    pub fn policy_digest(&self) -> String {
        let w = &self.weights;
        let text = format!(
            "w:{:?}|floor:{}|depth:{}|fanout:{}|cands:{}|decay:{}|stale:{}|reapr:{}|reafloor:{}",
            [
                w.similarity,
                w.proximity,
                w.explain_path,
                w.confidence,
                w.read_through,
                w.recency,
                w.stale_penalty,
                w.superseded_source,
                w.contradiction_bonus,
                w.stickiness,
                w.decay,
            ],
            self.seed_min_ratio,
            self.max_depth,
            self.max_fanout,
            self.max_candidates,
            self.recency_cutoff,
            self.admit_stale,
            self.reactivation_prune,
            self.reactivation_floor,
        );
        crate::hash::sha256(text.as_bytes()).short(12)
    }

    pub fn plan(
        &self,
        ctx: &PlanCtx<'_>,
        speculator: Option<&mut Speculator>,
        query: &str,
        budget: Option<usize>,
        pin: &[NodeIdx],
        force_level: &HashMap<NodeIdx, Level>,
    ) -> ActiveContext {
        self.plan_for(
            ctx,
            speculator,
            query,
            PlanOutput::Context(budget.unwrap_or(self.budget)),
            pin,
            force_level,
        )
    }

    pub(crate) fn plan_for(
        &self,
        ctx: &PlanCtx<'_>,
        speculator: Option<&mut Speculator>,
        query: &str,
        output: PlanOutput<'_>,
        pin: &[NodeIdx],
        force_level: &HashMap<NodeIdx, Level>,
    ) -> ActiveContext {
        // Independent of every per-stage clock below, so that
        // `profile.residual()` can detect a stage nobody timed.
        let whole_call = Instant::now();
        let (budget, encoded_bytes, semantic) = match output {
            PlanOutput::Context(budget) => (budget, None, None),
            PlanOutput::Evidence(budget, matches) => {
                (budget.tokens, budget.encoded_bytes, Some(matches))
            }
        };
        let compact = semantic.is_some();
        let learned = semantic.is_some_and(|matches| !matches.is_empty());
        let answer_checked = semantic.is_some_and(|matches| matches.answer_checked);
        let qtype = ctx.policy.classify(query);
        let mut active = ActiveContext {
            query: query.to_string(),
            qtype: Some(qtype),
            budget,
            snapshot_version: ctx.graph.version,
            ..Default::default()
        };

        let query_vec = if learned {
            Vec::new()
        } else {
            ctx.ladder.query_vector(query)
        };

        let clock = Instant::now();
        let seeds = self.seed(ctx, query, &query_vec, &mut active, semantic);
        active.profile.seed_time = clock.elapsed();
        active.profile.seeds_kept = seeds.len();

        let seed_scores: HashMap<NodeIdx, f32> = seeds.iter().copied().collect();

        let clock = Instant::now();
        let mut distances = self.expand(ctx, &seeds, qtype, pin, &mut active);
        active.profile.expand_time = clock.elapsed();
        active.profile.expand_reached = distances.len();

        let clock = Instant::now();
        let pinned = self.pinned(ctx, qtype, &mut distances, pin, &mut active.profile);
        active.profile.pin_time = clock.elapsed();

        let clock = Instant::now();
        active.profile.score_dropped_cap = distances.len().saturating_sub(self.max_candidates);
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut preferred: HashMap<NodeIdx, Level> = HashMap::new();
        let mut terms_by_node: HashMap<NodeIdx, UtilityTerms> = HashMap::new();
        let mut rendered_overrides = HashMap::new();

        for &(idx, distance) in distances.iter().take(self.max_candidates) {
            active.profile.score_considered += 1;
            let node = ctx.graph.node(idx);
            if answer_checked && !seed_scores.contains_key(&idx) && !pinned.contains(&idx) {
                continue;
            }
            active.profile.score_spans_priced += node.source_spans.len();
            if node.status == Status::Superseded {
                active.profile.score_dropped_superseded += 1;
                continue;
            }
            if node.status == Status::Stale && !pinned.contains(&idx) && !self.admit_stale {
                active.profile.score_dropped_stale += 1;
                active.stale_seen.push(idx);
                continue;
            }
            if compact && node.kind == Kind::Skill {
                continue;
            }
            let available = ctx.ladder.available(ctx.raw, node);
            if available.is_empty() {
                active.profile.score_dropped_no_level += 1;
                continue;
            }
            // A forced level the node does not have is meaningless — it cannot
            // be rendered there. Skill nodes (L2 only) made this reachable: a
            // reasoner can `#ESCALATE` one, and dropping it from the re-plan
            // would remove exactly the context the escalation asked about.
            // Fall back to the node's own levels instead.
            let forced = force_level
                .get(&idx)
                .copied()
                .filter(|level| available.contains(level));
            let want =
                forced.unwrap_or_else(|| ctx.policy.preferred_level(node, qtype, &available));
            preferred.insert(idx, want);
            // Under the causal control the utility function is skipped entirely
            // and every other stage sees an identical candidate.
            let terms = if self.stub_scorer {
                UtilityTerms::constant(1.0)
            } else {
                self.utility_terms(
                    ctx,
                    (idx, node, distance),
                    &query_vec,
                    &seed_scores,
                    &active,
                )
            };
            // For learned retrieval, content match dominates state priors.
            // A cached fact's generic utility must not outrank closer evidence.
            let base = if learned {
                terms.similarity + 0.1 * (terms.base() - terms.similarity)
            } else {
                terms.base()
            }
            .max(0.01);
            let is_pinned = pinned.contains(&idx);
            let excerpts = semantic
                .into_iter()
                .flat_map(|matches| &matches.spans)
                .filter_map(|passage| {
                    let id = &ctx.raw.span(passage.span).id;
                    if node.source_spans.contains(id) {
                        ctx.raw
                            .text_of(id)
                            .get(passage.range.clone())
                            .map(|text| EvidenceExcerpt {
                                text,
                                focus: passage.focus.clone(),
                            })
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            let checked_passage = excerpts.iter().any(|excerpt| excerpt.focus.is_some());
            let mut options: Vec<Choice> = Vec::new();
            for level in &available {
                if let Some(forced) = forced {
                    if *level != forced {
                        continue;
                    }
                }
                let cost = if compact {
                    if *level != Level::L0
                        && (checked_passage || (qtype == QueryType::QuoteExact && !is_pinned))
                    {
                        continue;
                    }
                    let Some((text, cost)) = ctx.ladder.render_evidence(
                        ctx.raw,
                        node,
                        *level,
                        query,
                        EvidenceBudget {
                            tokens: budget,
                            encoded_bytes,
                        },
                        &excerpts,
                    ) else {
                        continue;
                    };
                    rendered_overrides.insert((idx, *level), text);
                    cost
                } else {
                    ctx.ladder.cost(ctx.raw, node, *level)
                };
                options.push(Choice::new(*level, cost, base * level_fit(qtype, *level)));
            }
            if options.is_empty() {
                active.profile.score_dropped_no_level += 1;
                if !is_pinned {
                    continue;
                }
            }
            candidates.push(Candidate::new(idx, options, is_pinned));
            terms_by_node.insert(idx, terms);
        }

        active.profile.score_time = clock.elapsed();

        active.considered = candidates.len();
        active.profile.knapsack_candidates = candidates.len();
        let clock = Instant::now();
        let line_bytes = |node, level| {
            crate::json::evidence_line_bytes(rendered_overrides[&(node, level)].trim())
        };
        // A byte allocation satisfies both limits when every option uses at
        // least as much of the byte allowance as of the token allowance.
        let byte_budget = encoded_bytes.filter(|&bytes| {
            candidates.iter().all(|candidate| {
                candidate.options.iter().all(|option| {
                    bytes as u128 * option.cost as u128
                        <= budget as u128 * line_bytes(candidate.node, option.level) as u128
                })
            })
        });
        if byte_budget.is_some() {
            for candidate in &mut candidates {
                for option in &mut candidate.options {
                    option.cost = line_bytes(candidate.node, option.level);
                }
            }
        }
        let capacity = byte_budget.unwrap_or(budget);
        if learned {
            Self::reserve_best_match(&mut candidates, &seed_scores, capacity);
        }
        if !compact && !force_level.is_empty() {
            Self::fit_escalations(
                ctx,
                query,
                budget,
                force_level,
                &mut candidates,
                &mut rendered_overrides,
            );
        }
        let allocation = solve(&candidates, capacity, None, &preferred);
        active.profile.knapsack_time = clock.elapsed();

        let clock = Instant::now();
        let mut chosen: Vec<(NodeIdx, Choice)> = allocation.chosen.into_iter().collect();
        chosen.sort_by_key(|(idx, _)| *idx);
        for (idx, option) in chosen {
            let node = ctx.graph.node(idx);
            let terms = terms_by_node[&idx];
            let rendered = match rendered_overrides.remove(&(idx, option.level)) {
                Some(text) => {
                    if !compact {
                        active.l0_excerpts.insert(idx);
                    }
                    text
                }
                None => ctx.ladder.render(ctx.raw, node, option.level),
            };
            let decision =
                ctx.policy
                    .decide(node, qtype, &ctx.ladder.available(ctx.raw, node), true);
            let cost = if byte_budget.is_some() {
                ctx.ladder.estimator.count(&rendered)
            } else {
                option.cost
            };
            if !compact {
                node.admits.set(node.admits.get() + 1);
                if option.level == Level::L0 {
                    node.meta.l0_reads.set(node.meta.l0_reads.get() + 1);
                }
            }
            active.entries.push(Admission {
                node: idx,
                node_id: node.id.clone(),
                kind: node.kind,
                label: node.label(),
                level: option.level,
                cost,
                utility: option.utility,
                reason: decision.reason,
                pinned: pinned.contains(&idx),
                rendered,
                terms,
            });
        }
        active.profile.admit_time = clock.elapsed();
        active.profile.admitted = active.entries.len();

        active.tokens = active.entries.iter().map(|entry| entry.cost).sum();
        active.dropped = allocation.dropped;
        active.demoted = allocation.demoted;
        active.overflow = allocation.overflow;
        active.profile.knapsack_dropped = active.dropped.len();
        active.profile.knapsack_demoted = active.demoted.len();

        if let Some(speculator) = speculator {
            let clock = Instant::now();
            self.speculate(ctx, speculator, &distances, &query_vec, &active);
            active.profile.speculate_time = clock.elapsed();
        }
        active.profile.measured_time = whole_call.elapsed();
        active
    }

    fn fit_escalations(
        ctx: &PlanCtx<'_>,
        query: &str,
        budget: usize,
        force_level: &HashMap<NodeIdx, Level>,
        candidates: &mut [Candidate],
        rendered_overrides: &mut HashMap<(NodeIdx, Level), String>,
    ) {
        let requests_raw = |candidate: &Candidate| {
            candidate.pinned
                && force_level.get(&candidate.node) == Some(&Level::L0)
                && matches!(candidate.options.as_slice(), [option] if option.level == Level::L0)
        };
        let mut remaining = budget;
        let mut raw_cost = 0usize;
        for candidate in candidates.iter().filter(|candidate| candidate.pinned) {
            if let Some(choice) = candidate.cheapest() {
                if requests_raw(candidate) {
                    raw_cost = raw_cost.saturating_add(choice.cost);
                } else {
                    remaining = remaining.saturating_sub(choice.cost);
                }
            }
        }
        if raw_cost <= remaining {
            return;
        }
        // Keep the other mandatory entries and fit the requested raw detail
        // with a whole sentence when full spans exceed the budget.
        for candidate in candidates
            .iter_mut()
            .filter(|candidate| requests_raw(candidate))
        {
            let node = ctx.graph.node(candidate.node);
            if let Some((text, cost)) = ctx.ladder.l0_excerpt(ctx.raw, node, query, remaining) {
                let choice = &mut candidate.options[0];
                if cost < choice.cost {
                    choice.cost = cost;
                    rendered_overrides.insert((candidate.node, Level::L0), text);
                }
            }
        }
    }

    fn reserve_best_match(
        candidates: &mut [Candidate],
        seeds: &HashMap<NodeIdx, f32>,
        budget: usize,
    ) {
        let mandatory = candidates
            .iter()
            .filter(|candidate| candidate.pinned)
            .filter_map(Candidate::cheapest)
            .map(|choice| choice.cost)
            .fold(0, usize::saturating_add);
        let remaining = budget.saturating_sub(mandatory);
        // Keep room for the best fitting direct hit before packing lower
        // scoring entries. Several cheap distractions can outbid one answer.
        let best = candidates
            .iter_mut()
            .filter(|candidate| {
                !candidate.pinned && seeds.get(&candidate.node).is_some_and(|score| *score > 0.0)
            })
            .filter_map(|candidate| {
                let utility = candidate
                    .options
                    .iter()
                    .filter(|option| option.cost <= remaining)
                    .map(|option| option.utility)
                    .max_by(f32::total_cmp)?;
                let relevance = seeds.get(&candidate.node).copied()?;
                Some((candidate, relevance, utility))
            })
            .max_by(|a, b| {
                a.1.total_cmp(&b.1)
                    .then(a.2.total_cmp(&b.2))
                    .then(b.0.node.cmp(&a.0.node))
            });
        if let Some((candidate, _, _)) = best {
            candidate.pinned = true;
        }
    }

    // -- steps -------------------------------------------------------------
    fn seed(
        &self,
        ctx: &PlanCtx<'_>,
        query: &str,
        query_vec: &[f32],
        active: &mut ActiveContext,
        semantic: Option<&crate::semantic::Matches>,
    ) -> Vec<(NodeIdx, f32)> {
        // The compact path starts with lexical and learned matches. Hashing
        // vectors alone do not seed evidence.
        let compact = semantic.is_some();
        let answer_checked = semantic.is_some_and(|matches| matches.answer_checked);
        let search = |namespace| {
            if compact {
                ctx.index.lexical_matches(namespace, query, self.seed_k)
            } else {
                ctx.index.search(namespace, query, query_vec, self.seed_k)
            }
        };
        let mut hits = if answer_checked {
            Vec::new()
        } else {
            search(Namespace::Node)
        };
        active.profile.seed_hits = hits.len();
        if let Some((_, top)) = hits.first().copied() {
            let floor = top * self.seed_min_ratio;
            let before = hits.len();
            hits.retain(|(_, score)| *score >= floor);
            active.profile.seed_dropped_floor = before - hits.len();
        }
        if let Some(matches) = semantic.filter(|m| !m.is_empty()) {
            // BM25 normalizes its top hit to 1.0 even if it matches one word.
            // Preserve exact matches without letting a single shared term
            // outrank passages with stronger learned support.
            for (idx, score) in &mut hits {
                let node = ctx.graph.node(*idx);
                let text = ctx.ladder.state_object(node);
                *score *= crate::policy::overlap(query, &text).min(1.0);
            }
            for &(idx, similarity) in &matches.nodes {
                merge_seed(&mut hits, idx, crate::semantic::relevance(similarity));
            }
            hits.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        }
        let mut seeds: Vec<(NodeIdx, f32)> = Vec::new();
        // Recency prefilter, off by default. Applied before scoring so it is a
        // cost saving rather than a re-ranking: anything older than the cutoff
        // never reaches the utility function at all.
        let age_span = if self.recency_cutoff > 0.0 {
            let stamps: Vec<u64> = ctx.graph.nodes().iter().map(|n| n.timestamp).collect();
            match (stamps.iter().min(), stamps.iter().max()) {
                (Some(&lo), Some(&hi)) if hi > lo => Some((lo, hi)),
                _ => None,
            }
        } else {
            None
        };
        let age_floor =
            age_span.map(|(lo, hi)| lo + ((hi - lo) as f32 * self.recency_cutoff) as u64);
        for (idx, score) in hits {
            let node = ctx.graph.node(idx);
            if let (Some(floor), Some((lo, hi))) = (age_floor, age_span) {
                if node.timestamp < floor {
                    // Reactivation-aware pruning: age alone is not the whole
                    // story — an old node that keeps getting read and scores
                    // well is exactly the item the "fancy eviction fails"
                    // literature says comes back expensive. Recency maps to
                    // the predictor's 0..1 scale with 1 = newest.
                    if self.reactivation_prune {
                        let recency = 1.0 - (node.timestamp - lo) as f32 / (hi - lo) as f32;
                        let p = self.reactivation.probability(
                            recency,
                            node.read_rate(),
                            node.confidence,
                        );
                        if p >= self.reactivation_floor {
                            active.profile.seed_spared_reactivation += 1;
                        } else {
                            active.profile.seed_dropped_recency += 1;
                            continue;
                        }
                    } else {
                        active.profile.seed_dropped_recency += 1;
                        continue;
                    }
                }
            }
            // Evidence whose every live dependent has been superseded supports
            // nothing current, and it still carries the old value verbatim. It
            // used to be admitted with a NOTE telling the model not to treat it
            // as current, which is only a guard if the model reads notes: on the
            // adversarial mutation set the superseded sentence out-scores the
            // correction on lexical overlap, so a matcher that ignores the note
            // answers from it every time. Exclude it from seeding for the same
            // reason a superseded claim is excluded. `corrected_by` evidence is
            // deliberately left in — it still supports live facts, so its note
            // has to do the work.
            if node.meta.superseded_source && !self.admit_stale {
                active.profile.seed_dropped_status += 1;
                active.stale_seen.push(idx);
                continue;
            }
            match node.status {
                Status::Superseded => {
                    active.profile.seed_dropped_status += 1;
                    continue;
                }
                // Never seed from a stale node without re-grounding it first —
                // unless the negative control has bypassed the guard.
                Status::Stale if !self.admit_stale => {
                    active.profile.seed_dropped_status += 1;
                    active.stale_seen.push(idx);
                }
                // Contradicted and unresolved nodes seed like fresh ones: they
                // are current, and they carry their marker into the window.
                status if status.is_live() || self.admit_stale => seeds.push((idx, score)),
                _ => {
                    active.profile.seed_dropped_status += 1;
                    continue;
                }
            }
        }
        // Fall back to the raw lexical index when state search finds nothing —
        // the material may be ingested but not yet extracted into state.
        if !answer_checked && (compact || seeds.len() < 2) {
            for (span_idx, score) in search(Namespace::Span) {
                let span_id = &ctx.raw.span(span_idx).id;
                if let Some(evidence) = ctx
                    .evidence_by_span
                    .get(span_id)
                    .and_then(|id| ctx.graph.idx_of(id))
                {
                    if !seeds.iter().any(|(i, _)| *i == evidence) {
                        active.profile.seed_from_spans += 1;
                        let coverage = if semantic.is_some_and(|m| !m.is_empty()) {
                            crate::policy::overlap(query, ctx.raw.text_of(span_id)).min(1.0)
                        } else {
                            1.0
                        };
                        seeds.push((evidence, score * coverage * 0.8));
                    }
                }
            }
        }
        if let Some(matches) = semantic {
            for passage in &matches.spans {
                let span_id = &ctx.raw.span(passage.span).id;
                if let Some(idx) = ctx
                    .evidence_by_span
                    .get(span_id)
                    .and_then(|id| ctx.graph.idx_of(id))
                {
                    merge_seed(
                        &mut seeds,
                        idx,
                        crate::semantic::relevance(passage.similarity),
                    );
                }
            }
        }
        active.seeds = seeds.iter().map(|(i, _)| *i).collect();
        seeds
    }

    /// BFS over dependency edges, capped in depth and fan-out.
    fn expand(
        &self,
        ctx: &PlanCtx<'_>,
        seeds: &[(NodeIdx, f32)],
        qtype: QueryType,
        pin: &[NodeIdx],
        active: &mut ActiveContext,
    ) -> Vec<(NodeIdx, usize)> {
        let mut distances: Vec<(NodeIdx, usize)> = Vec::new();
        let mut seen: HashMap<NodeIdx, usize> = HashMap::new();
        let mut queue: VecDeque<(NodeIdx, usize)> = VecDeque::new();
        // The same rule `seed` applies, applied here too.
        //
        // Seeding excludes evidence whose every live dependent has been
        // superseded, because it still carries the old value verbatim and a
        // matcher that ignores notes will answer from it. Expansion used to
        // re-admit exactly those nodes through a dependency or dependent edge,
        // so the guard held only for material that arrived by lexical match and
        // was silently bypassed by material that arrived through the graph.
        //
        // That is a guard on one entrance of a room with two doors. It was
        // found by tightening an unrelated threshold, which changed which nodes
        // seeded, which changed which expansions ran, and let the excluded node
        // in through the second door on an adversarial corpus where the
        // superseded sentence is the better lexical match for the query.
        let admit_stale = self.admit_stale;
        let excluded =
            |idx: NodeIdx| -> bool { !admit_stale && ctx.graph.node(idx).meta.superseded_source };
        let push = |distances: &mut Vec<(NodeIdx, usize)>,
                    seen: &mut HashMap<NodeIdx, usize>,
                    idx: NodeIdx,
                    depth: usize| {
            if excluded(idx) {
                return false;
            }
            match seen.entry(idx) {
                Entry::Vacant(slot) => {
                    slot.insert(depth);
                    distances.push((idx, depth));
                    true
                }
                Entry::Occupied(_) => false,
            }
        };
        for &(idx, _) in seeds {
            if push(&mut distances, &mut seen, idx, 0) {
                queue.push_back((idx, 0));
            }
        }
        for &idx in pin {
            if idx < ctx.graph.len() && push(&mut distances, &mut seen, idx, 0) {
                queue.push_back((idx, 0));
            }
        }
        while let Some((idx, depth)) = queue.pop_front() {
            if distances.len() >= self.max_candidates {
                active.profile.expand_capped = true;
                continue;
            }
            if depth >= self.max_depth {
                continue;
            }
            let node = ctx.graph.node(idx);
            for dep in node.dependencies.iter().take(self.max_fanout) {
                if let Some(dep_idx) = ctx.graph.idx_of(dep) {
                    if push(&mut distances, &mut seen, dep_idx, depth + 1) {
                        queue.push_back((dep_idx, depth + 1));
                    }
                }
            }
            // One hop *up* to whatever was derived from this node, so a fact
            // brings its decision with it rather than arriving orphaned.
            if depth == 0 {
                for dependent in ctx
                    .graph
                    .neighbors(idx, None, false)
                    .into_iter()
                    .take(self.max_fanout)
                {
                    if push(&mut distances, &mut seen, dependent, 1) {
                        queue.push_back((dependent, self.max_depth));
                    }
                }
            }
            for other in ctx.graph.neighbors(idx, Some(EdgeType::Contradicts), true) {
                if ctx.graph.node(other).status != Status::Superseded {
                    push(&mut distances, &mut seen, other, depth + 1);
                }
            }
        }

        if qtype == QueryType::Justify {
            // A justification demands the whole audit path, whatever it costs.
            let seeds: Vec<NodeIdx> = active.seeds.iter().copied().take(2).collect();
            for seed in seeds {
                let path = ctx.graph.explain(seed, None);
                active.explain_paths.push((seed, path.nodes.clone()));
                for (i, node) in path.nodes.iter().enumerate() {
                    push(&mut distances, &mut seen, *node, i.min(self.max_depth));
                }
            }
        }
        distances
    }

    /// Goals and constraints are admitted every turn whether or not the query
    /// mentioned them — they are cheap, they bound what any answer may say, and
    /// a plan that silently drops a constraint is worse than one that costs
    /// twenty more tokens. They join the candidate set even when nothing in the
    /// query reached them.
    fn pinned(
        &self,
        ctx: &PlanCtx<'_>,
        qtype: QueryType,
        distances: &mut Vec<(NodeIdx, usize)>,
        pin: &[NodeIdx],
        profile: &mut StageProfile,
    ) -> HashSet<NodeIdx> {
        let mut pinned: HashSet<NodeIdx> = pin.iter().copied().collect();
        let mut known: HashSet<NodeIdx> = distances.iter().map(|(i, _)| *i).collect();
        for kind in ctx.policy.pinned_kinds(qtype) {
            // `by_kind` filters the whole node vector, so this stage visits the
            // entire graph once per pinned kind. It is the only uncapped step
            // in the planner, and `pin_scanned` is here so that fact appears in
            // a table rather than being inferred from a latency curve.
            profile.pin_scanned += ctx.graph.len();
            for idx in ctx.graph.by_kind(kind, true) {
                pinned.insert(idx);
                if known.insert(idx) {
                    profile.pinned_added += 1;
                    distances.push((idx, 1));
                }
            }
        }
        pinned
    }

    fn utility_terms(
        &self,
        ctx: &PlanCtx<'_>,
        (idx, node, distance): (NodeIdx, &Node, usize),
        query_vec: &[f32],
        seed_scores: &HashMap<NodeIdx, f32>,
        active: &ActiveContext,
    ) -> UtilityTerms {
        let w = &self.weights;
        let similarity = ctx.ladder.similarity(ctx.raw, node, query_vec).max(0.0);
        let seed_score = seed_scores.get(&idx).copied().unwrap_or(0.0);
        let proximity = w.decay.powi(distance as i32);
        let age = ctx.now.saturating_sub(node.timestamp).max(1) as f32;
        let recency = 1.0 / (1.0 + age.sqrt() / 8.0);
        let on_path = active
            .explain_paths
            .iter()
            .any(|(_, nodes)| nodes.contains(&idx));
        let contradicted = !ctx
            .graph
            .neighbors(idx, Some(EdgeType::Contradicts), true)
            .is_empty();
        UtilityTerms {
            similarity: w.similarity * similarity.max(seed_score),
            proximity: w.proximity * proximity,
            explain_path: if on_path { w.explain_path } else { 0.0 },
            confidence: w.confidence * node.confidence,
            read_through: w.read_through * node.read_rate(),
            recency: w.recency * recency,
            kind_prior: w.kind_prior(node.kind),
            contradiction: if contradicted {
                w.contradiction_bonus
            } else {
                0.0
            },
            stale: if node.status == Status::Stale {
                -w.stale_penalty
            } else {
                0.0
            },
            superseded_source: if node.meta.superseded_source {
                -w.superseded_source
            } else if !node.meta.corrected_by.is_empty() {
                -w.superseded_source / 3.0
            } else {
                0.0
            },
            stickiness: if w.stickiness > 0.0 && ctx.last_assembly.contains(&node.id) {
                w.stickiness
            } else {
                0.0
            },
        }
    }

    fn speculate(
        &self,
        ctx: &PlanCtx<'_>,
        speculator: &mut Speculator,
        distances: &[(NodeIdx, usize)],
        query_vec: &[f32],
        active: &ActiveContext,
    ) {
        let admitted: HashSet<NodeIdx> = active.entries.iter().map(|e| e.node).collect();
        let scored: Vec<(NodeIdx, &Node, f32, f32)> = distances
            .iter()
            .filter(|(idx, _)| !admitted.contains(idx))
            .filter_map(|&(idx, distance)| {
                let node = ctx.graph.node(idx);
                if node.status == Status::Superseded {
                    return None;
                }
                Some((
                    idx,
                    node,
                    ctx.ladder.similarity(ctx.raw, node, query_vec),
                    self.weights.decay.powi(distance as i32),
                ))
            })
            .collect();
        let predictions = speculator.predict(&scored, ctx.now);
        speculator.prefetch(&predictions, ctx.graph.nodes(), ctx.ladder, ctx.raw);
    }

    // -- introspection -----------------------------------------------------
    pub fn explain_plan(&self, graph: &MemoryGraph, active: &ActiveContext) -> String {
        let mut lines = vec![
            format!(
                "query: {:?}  type={}  budget={}  used={}  candidates={}",
                active.query,
                active.qtype.map(QueryType::as_str).unwrap_or("open"),
                active.budget,
                active.tokens,
                active.considered
            ),
            format!(
                "seeds: {}",
                if active.seeds.is_empty() {
                    "(none)".to_string()
                } else {
                    active
                        .seeds
                        .iter()
                        .map(|&i| graph.node(i).id.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        ];
        let mut entries: Vec<&Admission> = active.entries.iter().collect();
        entries.sort_by(|a, b| {
            b.utility
                .total_cmp(&a.utility)
                .then(a.node_id.cmp(&b.node_id))
        });
        for entry in entries {
            let terms = entry
                .terms
                .named()
                .into_iter()
                .filter(|(_, v)| *v != 0.0)
                .map(|(name, v)| format!("{name}={v:+.2}"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "  ADMIT {} {:<22} {} cost={:<4} U={:.2} {}({})",
                entry.node_id,
                entry.label,
                entry.level.as_str(),
                entry.cost,
                entry.utility,
                if entry.pinned { "[pinned] " } else { "" },
                terms
            ));
        }
        for (idx, wanted, got) in &active.demoted {
            lines.push(format!(
                "  DEMOTE {} {} -> {} (budget pressure)",
                graph.node(*idx).id,
                wanted.as_str(),
                got.as_str()
            ));
        }
        for idx in &active.dropped {
            lines.push(format!("  DROP   {}", graph.node(*idx).id));
        }
        for idx in &active.stale_seen {
            lines.push(format!(
                "  SKIP   {} (stale — needs revalidation)",
                graph.node(*idx).id
            ));
        }
        for prediction in &active.prefetch {
            lines.push(format!(
                "  PREFETCH {} p={:.2}",
                graph.node(prediction.node).id,
                prediction.p
            ));
        }
        lines.join("\n")
    }
}
