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

use crate::budget::{Candidate, Choice, solve};
use crate::graph::MemoryGraph;
use crate::index::{HybridIndex, Namespace};
use crate::ladder::{Ladder, Level};
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
}

impl UtilityTerms {
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
    pub tokens: usize,  // audit-allow: LM token count, not a credential
    pub seeds: Vec<NodeIdx>,
    pub dropped: Vec<NodeIdx>,
    pub demoted: Vec<(NodeIdx, Level, Level)>,
    pub prefetch: Vec<Prediction>,
    pub considered: usize,
    pub snapshot_version: u64,
    pub explain_paths: Vec<(NodeIdx, Vec<NodeIdx>)>,
    pub stale_seen: Vec<NodeIdx>,
    pub overflow: bool,
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
    pub fn render(&self) -> String {
        self.render_with_header(true)
    }

    pub fn render_with_header(&self, with_header: bool) -> String {
        let mut sorted: Vec<&Admission> = self.entries.iter().collect();
        sorted.sort_by(|a, b| {
            b.utility
                .total_cmp(&a.utility)
                .then(a.node_id.cmp(&b.node_id))
        });

        let order = [
            (Kind::Goal, "GOALS"),
            (Kind::Constraint, "CONSTRAINTS"),
            (Kind::Claim, "FACTS (cached state)"),
            (Kind::Calculation, "COMPUTED"),
            (Kind::Decision, "DECISIONS"),
            (Kind::OpenQuestion, "OPEN QUESTIONS"),
            (Kind::Evidence, "EVIDENCE (raw spans)"),
        ];
        let mut blocks: Vec<String> = Vec::new();
        if with_header {
            blocks.push(format!(
                "# ACTIVE CONTEXT  (k={}/{} tokens, {} of {} candidates, query type: {})",
                self.tokens,
                self.budget,
                self.entries.len(),
                self.considered,
                self.qtype.map(QueryType::as_str).unwrap_or("open")
            ));
        }
        for (kind, title) in order {
            let group: Vec<&&Admission> = sorted.iter().filter(|e| e.kind == kind).collect();
            if group.is_empty() {
                continue;
            }
            blocks.push(format!("\n## {title}"));
            for entry in group {
                blocks.push(entry.rendered.clone());
            }
        }
        blocks.join("\n")
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
    /// Negative control only. When true, stale nodes are planned as if fresh
    /// instead of being skipped — used to prove `stale_fact_read_rate` can
    /// return nonzero, so that a zero in production is a fired guard rather
    /// than a guard that was never exercised. Never enable in real use.
    pub admit_stale: bool,
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
            seed_min_ratio: 0.3,
            recency_cutoff: 0.0,
            admit_stale: false,
        }
    }
}

impl RelevancePlanner {
    pub fn plan(
        &self,
        ctx: &PlanCtx<'_>,
        speculator: Option<&mut Speculator>,
        query: &str,
        budget: Option<usize>,
        pin: &[NodeIdx],
        force_level: &HashMap<NodeIdx, Level>,
    ) -> ActiveContext {
        let budget = budget.unwrap_or(self.budget);
        let qtype = ctx.policy.classify(query);
        let mut active = ActiveContext {
            query: query.to_string(),
            qtype: Some(qtype),
            budget,
            snapshot_version: ctx.graph.version,
            ..Default::default()
        };

        let query_vec = ctx.ladder.query_vector(query);
        let seeds = self.seed(ctx, query, &query_vec, &mut active);
        let seed_scores: HashMap<NodeIdx, f32> = seeds.iter().copied().collect();
        let mut distances = self.expand(ctx, &seeds, qtype, pin, &mut active);
        let pinned = self.pinned(ctx, qtype, &mut distances, pin);
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut preferred: HashMap<NodeIdx, Level> = HashMap::new();
        let mut scratch: HashMap<NodeIdx, (UtilityTerms, Level)> = HashMap::new();

        for &(idx, distance) in distances.iter().take(self.max_candidates) {
            let node = ctx.graph.node(idx);
            if node.status == Status::Superseded {
                continue;
            }
            if node.status == Status::Stale && !pinned.contains(&idx) && !self.admit_stale {
                active.stale_seen.push(idx);
                continue;
            }
            let available = ctx.ladder.available(ctx.raw, node);
            if available.is_empty() {
                continue;
            }
            let want = force_level
                .get(&idx)
                .copied()
                .unwrap_or_else(|| ctx.policy.preferred_level(node, qtype, &available));
            preferred.insert(idx, want);
            let terms = self.utility_terms(
                ctx,
                (idx, node, distance),
                &query_vec,
                &seed_scores,
                &active,
            );
            let base = terms.base().max(0.01);
            let mut options: Vec<Choice> = Vec::new();
            for level in &available {
                if let Some(forced) = force_level.get(&idx) {
                    if level != forced {
                        continue;
                    }
                }
                options.push(Choice::new(
                    *level,
                    ctx.ladder.cost(ctx.raw, node, *level),
                    base * level_fit(qtype, *level),
                ));
            }
            if options.is_empty() {
                continue;
            }
            candidates.push(Candidate::new(idx, options, pinned.contains(&idx)));
            scratch.insert(idx, (terms, want));
        }

        active.considered = candidates.len();
        let allocation = solve(&candidates, budget, None, &preferred);

        let mut chosen: Vec<(NodeIdx, Choice)> = allocation.chosen.into_iter().collect();
        chosen.sort_by_key(|(idx, _)| *idx);
        for (idx, option) in chosen {
            let node = ctx.graph.node(idx);
            let (terms, _) = scratch[&idx];
            let rendered = ctx.ladder.render(ctx.raw, node, option.level);
            let decision =
                ctx.policy
                    .decide(node, qtype, &ctx.ladder.available(ctx.raw, node), true);
            node.admits.set(node.admits.get() + 1);
            if option.level == Level::L0 {
                node.meta.l0_reads.set(node.meta.l0_reads.get() + 1);
            }
            active.entries.push(Admission {
                node: idx,
                node_id: node.id.clone(),
                kind: node.kind,
                label: node.label(),
                level: option.level,
                cost: option.cost,
                utility: option.utility,
                reason: decision.reason,
                pinned: pinned.contains(&idx),
                rendered,
                terms,
            });
        }
        active.tokens = allocation.tokens;
        active.dropped = allocation.dropped;
        active.demoted = allocation.demoted;
        active.overflow = allocation.overflow;

        if let Some(speculator) = speculator {
            self.speculate(ctx, speculator, &distances, &query_vec, &active);
        }
        active
    }

    // -- steps -------------------------------------------------------------
    fn seed(
        &self,
        ctx: &PlanCtx<'_>,
        query: &str,
        query_vec: &[f32],
        active: &mut ActiveContext,
    ) -> Vec<(NodeIdx, f32)> {
        let mut hits = ctx
            .index
            .search(Namespace::Node, query, query_vec, self.seed_k);
        if let Some((_, top)) = hits.first().copied() {
            let floor = top * self.seed_min_ratio;
            hits.retain(|(_, score)| *score >= floor);
        }
        let mut seeds: Vec<(NodeIdx, f32)> = Vec::new();
        // Recency prefilter, off by default. Applied before scoring so it is a
        // cost saving rather than a re-ranking: anything older than the cutoff
        // never reaches the utility function at all.
        let age_floor = if self.recency_cutoff > 0.0 {
            let stamps: Vec<u64> = ctx.graph.nodes().iter().map(|n| n.timestamp).collect();
            match (stamps.iter().min(), stamps.iter().max()) {
                (Some(&lo), Some(&hi)) if hi > lo => {
                    Some(lo + ((hi - lo) as f32 * self.recency_cutoff) as u64)
                }
                _ => None,
            }
        } else {
            None
        };
        for (idx, score) in hits {
            let node = ctx.graph.node(idx);
            if let Some(floor) = age_floor {
                if node.timestamp < floor {
                    continue;
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
                active.stale_seen.push(idx);
                continue;
            }
            match node.status {
                Status::Superseded => continue,
                // Never seed from a stale node without re-grounding it first —
                // unless the negative control has bypassed the guard.
                Status::Stale if !self.admit_stale => active.stale_seen.push(idx),
                // Contradicted and unresolved nodes seed like fresh ones: they
                // are current, and they carry their marker into the window.
                status if status.is_live() || self.admit_stale => seeds.push((idx, score)),
                _ => continue,
            }
        }
        // Fall back to the raw lexical index when state search finds nothing —
        // the material may be ingested but not yet extracted into state.
        if seeds.len() < 2 {
            for (span_idx, score) in
                ctx.index
                    .search(Namespace::Span, query, query_vec, self.seed_k)
            {
                let span_id = &ctx.raw.span(span_idx).id;
                if let Some(evidence) = ctx
                    .evidence_by_span
                    .get(span_id)
                    .and_then(|id| ctx.graph.idx_of(id))
                {
                    if !seeds.iter().any(|(i, _)| *i == evidence) {
                        seeds.push((evidence, score * 0.8));
                    }
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
        let push = |distances: &mut Vec<(NodeIdx, usize)>,
                    seen: &mut HashMap<NodeIdx, usize>,
                    idx: NodeIdx,
                    depth: usize| {
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
            if depth >= self.max_depth || distances.len() >= self.max_candidates {
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
    ) -> HashSet<NodeIdx> {
        let mut pinned: HashSet<NodeIdx> = pin.iter().copied().collect();
        let mut known: HashSet<NodeIdx> = distances.iter().map(|(i, _)| *i).collect();
        for kind in ctx.policy.pinned_kinds(qtype) {
            for idx in ctx.graph.by_kind(kind, true) {
                pinned.insert(idx);
                if known.insert(idx) {
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
