//! DCR — the runtime facade.
//!
//! Wires the components into the per-turn data flow from the architecture doc:
//!
//! ```text
//! event → index → graph update → plan(k + r) → model → state update → prefetch
//! ```
//!
//! The struct is deliberately small; all the interesting decisions live in the
//! components it composes. What it owns is the *loop*: escalation, consistency
//! between the Reasoner and the Memory Runtime, and the telemetry that says
//! whether any of this is working.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::execute::{ExecutionLayer, Inputs};
use crate::graph::{DcrError, MemoryGraph};
use crate::ids::Clock;
use crate::index::HybridIndex;
use crate::indexer::{IndexResult, IngestCtx, StateIndexer};
use crate::json::{Json, parse};
use crate::ladder::{Ladder, Level};
use crate::llm::{LocalReasoner, REASONER_SYSTEM, Reasoner, parse_escalation};
use crate::nodes::{
    Derivation, EdgeType, Kind, LevelCache, NewNode, Node, NodeIdx, NodeMeta, Status,
};
use crate::planner::{ActiveContext, PlanCtx, RelevancePlanner, Weights};
use crate::policy::DecisionPolicy;
use crate::spans::{Document, RawStore, Span};
use crate::speculation::Speculator;
use crate::telemetry::{Telemetry, Turn};
use crate::tokens::Estimator;

use std::cell::{Cell, RefCell};

#[derive(Debug, Clone)]
pub struct Answer {
    pub text: String,
    pub context: ActiveContext,
    pub escalations: u32,
    pub cited: Vec<String>,
    pub tokens: usize,
    /// True when a mid-turn invalidation forced the workspace to be rebuilt.
    pub replanned: bool,
}

pub struct Dcr {
    pub clock: Clock,
    pub raw: RawStore,
    pub graph: MemoryGraph,
    pub index: HybridIndex,
    pub ladder: Ladder,
    pub indexer: StateIndexer,
    pub policy: DecisionPolicy,
    pub planner: RelevancePlanner,
    pub speculator: Speculator,
    pub execution: ExecutionLayer,
    pub telemetry: Telemetry,
    pub budget: usize,
    /// How many times a turn may be re-planned at a richer level after the
    /// model reports insufficiency. Zero disables the escalation protocol.
    pub max_escalations: u32,
    reasoner: Box<dyn Reasoner>,
}

impl std::fmt::Debug for Dcr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dcr")
            .field("budget", &self.budget)
            .field("graph", &self.graph.stats())
            .finish_non_exhaustive()
    }
}

impl Dcr {
    pub fn new(budget: usize) -> Self {
        Self {
            clock: Clock::new(),
            raw: RawStore::default(),
            graph: MemoryGraph::new(),
            index: HybridIndex::default(),
            ladder: Ladder::default(),
            indexer: StateIndexer::default(),
            policy: DecisionPolicy::default(),
            planner: RelevancePlanner {
                budget,
                ..Default::default()
            },
            speculator: Speculator::default(),
            execution: ExecutionLayer::default(),
            telemetry: Telemetry::default(),
            budget,
            max_escalations: 2,
            reasoner: Box::new(LocalReasoner::new()),
        }
    }

    pub fn with_weights(mut self, weights: Weights) -> Self {
        self.planner.weights = weights;
        self
    }

    pub fn with_estimator(mut self, estimator: Estimator) -> Self {
        self.ladder.estimator = estimator;
        self
    }

    pub fn with_reasoner(mut self, reasoner: Box<dyn Reasoner>) -> Self {
        self.reasoner = reasoner;
        self
    }

    pub fn set_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.planner.budget = budget;
    }

    // -- ingestion ---------------------------------------------------------
    pub fn ingest(&mut self, text: &str, doc_id: Option<&str>) -> Result<IndexResult, DcrError> {
        self.ingest_with_source(text, doc_id, None)
    }

    pub fn ingest_with_source(
        &mut self,
        text: &str,
        doc_id: Option<&str>,
        source: Option<String>,
    ) -> Result<IndexResult, DcrError> {
        let mut ctx = IngestCtx {
            raw: &mut self.raw,
            graph: &mut self.graph,
            index: &mut self.index,
            ladder: &self.ladder,
            clock: &self.clock,
        };
        let result = self.indexer.ingest(&mut ctx, text, doc_id, source)?;
        self.telemetry.history_tokens += self.ladder.estimator.count(text);
        Ok(result)
    }

    pub fn ingest_file(&mut self, path: &Path) -> Result<IndexResult, DcrError> {
        let text = std::fs::read_to_string(path).map_err(|e| DcrError::Io(e.to_string()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        self.ingest_with_source(&text, Some(&name), Some(path.display().to_string()))
    }

    // -- planning and answering -------------------------------------------
    pub fn plan(&mut self, query: &str, budget: Option<usize>) -> ActiveContext {
        self.plan_with(query, budget, &[], &HashMap::new())
    }

    pub fn plan_with(
        &mut self,
        query: &str,
        budget: Option<usize>,
        pin: &[NodeIdx],
        force_level: &HashMap<NodeIdx, Level>,
    ) -> ActiveContext {
        self.graph.drain_pending(32);
        let ctx = PlanCtx {
            raw: &self.raw,
            graph: &self.graph,
            index: &self.index,
            ladder: &self.ladder,
            policy: &self.policy,
            now: self.clock.now(),
            evidence_by_span: &self.indexer.evidence_by_span,
        };
        self.planner.plan(
            &ctx,
            Some(&mut self.speculator),
            query,
            budget.or(Some(self.budget)),
            pin,
            force_level,
        )
    }

    pub fn prompt_for(&self, context: &ActiveContext) -> String {
        format!("{}\n\nQUESTION: {}\n", context.render(), context.query)
    }

    pub fn ask(&mut self, query: &str, budget: Option<usize>) -> Answer {
        let mut reasoner = std::mem::replace(
            &mut self.reasoner,
            Box::new(LocalReasoner::new()) as Box<dyn Reasoner>,
        );
        let answer = self.ask_with(query, budget, reasoner.as_mut());
        self.reasoner = reasoner;
        answer
    }

    pub fn ask_with(
        &mut self,
        query: &str,
        budget: Option<usize>,
        reasoner: &mut dyn Reasoner,
    ) -> Answer {
        self.ask_with_consolidation(query, budget, reasoner, &mut |_| {})
    }

    /// `ask`, with a hook that runs after the model returns and before the
    /// consistency check.
    ///
    /// This is where Solution B's background consolidation lands in a real
    /// deployment: it runs while Solution A is thinking, and it may invalidate
    /// a fact A is mid-way through reasoning over. Passing it explicitly keeps
    /// that race testable instead of hypothetical.
    pub fn ask_with_consolidation(
        &mut self,
        query: &str,
        budget: Option<usize>,
        reasoner: &mut dyn Reasoner,
        consolidate: &mut dyn FnMut(&mut MemoryGraph),
    ) -> Answer {
        let budget = budget.unwrap_or(self.budget);
        let mut force: HashMap<NodeIdx, Level> = HashMap::new();
        let mut pin: Vec<NodeIdx> = Vec::new();
        let mut escalations = 0u32;
        let mut replanned = false;
        let mut consistency_retries = 0u32;

        let (text, context) = loop {
            let context = self.plan_with(query, Some(budget), &pin, &force);
            let text = reasoner.complete(&self.prompt_for(&context), REASONER_SYSTEM);
            consolidate(&mut self.graph);

            // The Reasoner has been thinking; the Memory Runtime may have
            // consolidated underneath it. Snapshot isolation with an interrupt:
            // if anything in the working set was invalidated mid-turn, the
            // answer is discarded and the workspace rebuilt rather than
            // returned from state that no longer holds.
            if self.consistency_violation(&context) && consistency_retries < 1 {
                consistency_retries += 1;
                replanned = true;
                continue;
            }
            if let Some(node_id) = parse_escalation(&text) {
                if escalations < self.max_escalations {
                    if let Some(idx) = self.graph.idx_of(&node_id) {
                        escalations += 1;
                        force.insert(idx, Level::L0);
                        if !pin.contains(&idx) {
                            pin.push(idx);
                        }
                        continue;
                    }
                }
            }
            break (text, context);
        };

        let cited: Vec<String> = context
            .entries
            .iter()
            .filter(|e| text.contains(&e.node_id))
            .map(|e| e.node_id.clone())
            .collect();
        let cited_idx: HashSet<NodeIdx> = context
            .entries
            .iter()
            .filter(|e| cited.contains(&e.node_id))
            .map(|e| e.node)
            .collect();
        for &idx in &cited_idx {
            let node = self.graph.node(idx);
            node.reads.set(node.reads.get() + 1);
        }
        let stale_reads = context
            .entries
            .iter()
            .filter(|e| self.graph.node(e.node).status == Status::Stale)
            .count() as u32;

        self.telemetry.record_turn(Turn {
            query: query.to_string(),
            qtype: context.qtype.unwrap_or(crate::policy::QueryType::Open),
            tokens: context.tokens,
            budget: context.budget,
            admitted: context.entries.len(),
            considered: context.considered,
            escalations,
            stale_reads,
            demotions: context.demoted.len(),
            overflow: context.overflow,
        });
        self.speculator.feedback(&cited_idx);

        Answer {
            tokens: context.tokens,
            text,
            context,
            escalations,
            cited,
            replanned,
        }
    }

    /// Did background consolidation invalidate anything in this plan?
    fn consistency_violation(&self, context: &ActiveContext) -> bool {
        let touched = self.graph.invalidated_since(context.snapshot_version);
        context.entries.iter().any(|e| touched.contains(&e.node))
    }

    // -- state updates -----------------------------------------------------
    /// Cache a settled conclusion. Rejected unless it is groundable.
    pub fn commit_fact(
        &mut self,
        value: &str,
        key: Option<&str>,
        source_spans: &[String],
        dependencies: &[String],
        confidence: f32,
        kind: Kind,
    ) -> Result<NodeIdx, DcrError> {
        let node = NewNode::new(kind, value)
            .spans(source_spans.to_vec())
            .deps(dependencies.to_vec())
            .confidence(confidence)
            .maybe_key(key.map(str::to_string))
            .build();
        let conflicts: Vec<NodeIdx> = match key {
            Some(key) if kind == Kind::Claim => self
                .graph
                .by_key(key, true)
                .into_iter()
                .filter(|&i| {
                    let other = self.graph.node(i);
                    other.id != node.id && other.kind == Kind::Claim && other.value != node.value
                })
                .collect(),
            _ => Vec::new(),
        };
        let idx = self.graph.upsert(node, &self.raw, &self.clock)?;
        self.reindex(idx);
        for other in conflicts {
            self.graph.contradict(idx, other);
            if self.indexer.supersede_on_conflict {
                self.graph.supersede(other, idx);
                self.index.remove_node(other);
            }
        }
        Ok(idx)
    }

    /// Run a registered derivation (L3) and cache it as a calculation node.
    pub fn compute(
        &mut self,
        name: &str,
        inputs: Inputs,
        deps: Vec<String>,
        key: Option<&str>,
    ) -> Result<NodeIdx, DcrError> {
        let node = self.execution.compute_node(
            &self.graph,
            name,
            inputs,
            deps,
            key.map(str::to_string),
        )?;
        let idx = self.graph.upsert(node, &self.raw, &self.clock)?;
        self.reindex(idx);
        Ok(idx)
    }

    pub fn register(&mut self, name: &str, f: impl Fn(&Inputs) -> f64 + 'static) {
        self.execution.register(name, f);
    }

    /// Re-ground a stale node: recompute if derived, drop cheap levels if not.
    pub fn revalidate(&mut self, idx: NodeIdx) -> Result<(), DcrError> {
        let derivation: Option<Derivation> = self.graph.node(idx).meta.derivation.clone();
        if derivation.is_some() {
            let node = self.graph.node(idx);
            if let Some(value) = self.execution.run(&self.graph, node)? {
                self.graph.node_mut(idx).value = crate::ladder::format_number(value);
            }
        }
        {
            let node = self.graph.node(idx);
            let mut cache = node.level_cache.borrow_mut();
            cache.l1 = None;
            cache.l2 = None;
        }
        self.graph.revalidate(idx, &self.clock);
        self.reindex(idx);
        Ok(())
    }

    fn reindex(&mut self, idx: NodeIdx) {
        let mut ctx = IngestCtx {
            raw: &mut self.raw,
            graph: &mut self.graph,
            index: &mut self.index,
            ladder: &self.ladder,
            clock: &self.clock,
        };
        self.indexer.reindex(&mut ctx, idx);
    }

    // -- audit -------------------------------------------------------------
    pub fn explain(&mut self, node_id: &str) -> Result<String, DcrError> {
        let idx = self
            .graph
            .idx_of(node_id)
            .ok_or_else(|| DcrError::UnknownNode(node_id.to_string()))?;
        let path = self.graph.explain(idx, None);
        self.telemetry.record_explain(path.complete);
        let mut lines = vec![format!("explain({node_id})  complete={}", path.complete)];
        for &node_idx in &path.nodes {
            let node = self.graph.node(node_idx);
            let indent = "  ".repeat(path.depths.get(&node_idx).copied().unwrap_or(0));
            let head = match &node.key {
                Some(key) => format!("{key} = {}", node.value),
                None => node.value.clone(),
            };
            let head: String = head.chars().take(96).collect();
            lines.push(format!(
                "{indent}- [{}] {}: {head}  (conf={:.2}, {})",
                node.id,
                node.kind.as_str(),
                node.confidence,
                node.status.as_str()
            ));
        }
        for span_id in &path.spans {
            let text: String = self.raw.text_of(span_id).chars().take(120).collect();
            lines.push(format!("  source {span_id}: \"{text}\""));
        }
        if !path.complete {
            let ids: Vec<&str> = path
                .truncated_at
                .iter()
                .map(|&i| self.graph.node(i).id.as_str())
                .collect();
            lines.push(format!("  ! truncated at: {}", ids.join(", ")));
        }
        Ok(lines.join("\n"))
    }

    pub fn explain_plan(&self, context: &ActiveContext) -> String {
        self.planner.explain_plan(&self.graph, context)
    }

    // -- background work ---------------------------------------------------
    /// Solution B's background pass: drain deferred invalidation and collapse
    /// nodes that keep being read raw without yielding anything new.
    pub fn consolidate(&mut self, limit: usize) -> (usize, Vec<String>) {
        let drained = self.graph.drain_pending(limit).len();
        let mut collapsed = Vec::new();
        for idx in 0..self.graph.len() {
            let node = self.graph.node(idx);
            if node.status != Status::Fresh || !self.policy.should_deescalate(node) {
                continue;
            }
            collapsed.push(node.id.clone());
            self.graph.node_mut(idx).meta.deescalated = true;
        }
        (drained, collapsed)
    }

    /// Destroy the working set and rebuild it from Solution B alone.
    ///
    /// The wiki claims this invariant; the number that makes it meaningful is
    /// the rebuild cost, so it is measured here.
    pub fn rebuild_workspace(&mut self, query: &str, budget: Option<usize>) -> RebuildReport {
        let mut cleared = 0usize;
        for node in self.graph.nodes() {
            let mut cache = node.level_cache.borrow_mut();
            cleared += usize::from(cache.l1.is_some())
                + usize::from(cache.l2.is_some())
                + usize::from(cache.l3.is_some());
            *cache = LevelCache::default();
        }
        let before = self.ladder.builds();
        let context = self.plan(query, budget);
        let after = self.ladder.builds();
        RebuildReport {
            cleared_level_cache_entries: cleared,
            rebuilt_l1: after.l1 - before.l1,
            rebuilt_l2: after.l2 - before.l2,
            rebuilt_l3: after.l3 - before.l3,
            tokens: context.tokens,
            nodes: context.entries.len(),
        }
    }

    // -- reporting ---------------------------------------------------------
    pub fn report(&self) -> String {
        let mut out = self.telemetry.report().to_string();
        out.push_str(&format!("\n[graph]\n  {}\n", self.graph.stats()));
        out.push_str(&format!("[index]\n  {}\n", self.index.stats()));
        out.push_str(&format!("[execution]\n  {}\n", self.execution.stats()));
        out.push_str(&format!("[speculation]\n  {}\n", self.speculator.stats()));
        out.push_str(&format!(
            "\nraw: {} spans, {} chars\nladder builds: {}\n",
            self.raw.len(),
            self.raw.total_chars(),
            self.ladder.builds()
        ));
        out
    }

    // -- persistence -------------------------------------------------------
    pub fn save(&self, path: &Path) -> Result<(), DcrError> {
        let documents = Json::Arr(
            self.raw
                .documents()
                .iter()
                .map(|d| {
                    Json::obj(vec![
                        ("doc_id", Json::str(&d.id)),
                        ("text", Json::str(&d.text)),
                        ("ts", Json::num(d.ts as f64)),
                        ("source", d.source.clone().map_or(Json::Null, Json::Str)),
                    ])
                })
                .collect(),
        );
        let spans = Json::Arr(
            self.raw
                .spans()
                .iter()
                .map(|s| {
                    Json::obj(vec![
                        ("span_id", Json::str(&s.id)),
                        ("doc_id", Json::str(&s.doc_id)),
                        ("start", Json::num(s.start as f64)),
                        ("end", Json::num(s.end as f64)),
                        ("seq", Json::num(s.seq as f64)),
                        ("ts", Json::num(s.ts as f64)),
                    ])
                })
                .collect(),
        );
        let nodes = Json::Arr(self.graph.nodes().iter().map(node_to_json).collect());
        let mut edges = Vec::new();
        for idx in 0..self.graph.len() {
            for edge in self.graph.out_edges(idx) {
                edges.push(Json::obj(vec![
                    ("src", Json::str(&self.graph.node(edge.src).id)),
                    ("dst", Json::str(&self.graph.node(edge.dst).id)),
                    ("type", Json::str(edge.kind.as_str())),
                ]));
            }
        }
        let mut evidence: Vec<(String, Json)> = self
            .indexer
            .evidence_by_span
            .iter()
            .map(|(k, v)| (k.clone(), Json::str(v)))
            .collect();
        evidence.sort_by(|a, b| a.0.cmp(&b.0));

        let payload = Json::obj(vec![
            ("clock", Json::num(self.clock.now() as f64)),
            (
                "raw",
                Json::obj(vec![("documents", documents), ("spans", spans)]),
            ),
            (
                "graph",
                Json::obj(vec![
                    ("nodes", nodes),
                    ("edges", Json::Arr(edges)),
                    ("version", Json::num(self.graph.version as f64)),
                ]),
            ),
            (
                "predictor",
                Json::obj(vec![
                    (
                        "weights",
                        Json::Arr(
                            self.speculator
                                .predictor
                                .weights
                                .iter()
                                .map(|w| Json::num(*w as f64))
                                .collect(),
                        ),
                    ),
                    (
                        "updates",
                        Json::num(self.speculator.predictor.updates as f64),
                    ),
                ]),
            ),
            (
                "history_tokens",
                Json::num(self.telemetry.history_tokens as f64),
            ),
            ("evidence_by_span", Json::Obj(evidence)),
        ]);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| DcrError::Io(e.to_string()))?;
            }
        }
        std::fs::write(path, payload.to_json_string()).map_err(|e| DcrError::Io(e.to_string()))
    }

    pub fn load(path: &Path, budget: usize) -> Result<Dcr, DcrError> {
        let text = std::fs::read_to_string(path).map_err(|e| DcrError::Io(e.to_string()))?;
        let data = parse(&text).map_err(DcrError::Parse)?;
        let mut runtime = Dcr::new(budget);
        runtime
            .clock
            .restore(data.get("clock").and_then(Json::as_u64).unwrap_or(0));

        let raw = data
            .get("raw")
            .ok_or_else(|| DcrError::Parse("no raw".into()))?;
        let documents: Vec<Document> = raw
            .get("documents")
            .and_then(Json::as_array)
            .unwrap_or(&[])
            .iter()
            .map(|d| Document {
                id: d
                    .get("doc_id")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
                text: d
                    .get("text")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
                ts: d.get("ts").and_then(Json::as_u64).unwrap_or(0),
                source: d.get("source").and_then(Json::as_str).map(str::to_string),
            })
            .collect();
        let spans: Vec<Span> = raw
            .get("spans")
            .and_then(Json::as_array)
            .unwrap_or(&[])
            .iter()
            .map(|s| Span {
                id: s
                    .get("span_id")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
                doc_id: s
                    .get("doc_id")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
                start: s.get("start").and_then(Json::as_usize).unwrap_or(0),
                end: s.get("end").and_then(Json::as_usize).unwrap_or(0),
                seq: s.get("seq").and_then(Json::as_usize).unwrap_or(0),
                ts: s.get("ts").and_then(Json::as_u64).unwrap_or(0),
            })
            .collect();
        runtime.raw.restore(documents, spans);

        let graph = data
            .get("graph")
            .ok_or_else(|| DcrError::Parse("no graph".into()))?;
        for node in graph.get("nodes").and_then(Json::as_array).unwrap_or(&[]) {
            runtime.graph.insert_restored(node_from_json(node));
        }
        for edge in graph.get("edges").and_then(Json::as_array).unwrap_or(&[]) {
            let (Some(src), Some(dst), Some(kind)) = (
                edge.get("src").and_then(Json::as_str),
                edge.get("dst").and_then(Json::as_str),
                edge.get("type")
                    .and_then(Json::as_str)
                    .and_then(EdgeType::parse),
            ) else {
                continue;
            };
            runtime.graph.restore_edge(src, dst, kind);
        }
        if let Some(predictor) = data.get("predictor") {
            if let Some(weights) = predictor.get("weights").and_then(Json::as_array) {
                for (slot, value) in runtime
                    .speculator
                    .predictor
                    .weights
                    .iter_mut()
                    .zip(weights.iter())
                {
                    if let Some(v) = value.as_f64() {
                        *slot = v as f32;
                    }
                }
            }
            runtime.speculator.predictor.updates =
                predictor.get("updates").and_then(Json::as_u64).unwrap_or(0) as u32;
        }
        runtime.telemetry.history_tokens = data
            .get("history_tokens")
            .and_then(Json::as_usize)
            .unwrap_or(0);
        if let Some(Json::Obj(pairs)) = data.get("evidence_by_span") {
            for (span_id, node_id) in pairs {
                if let Some(id) = node_id.as_str() {
                    runtime
                        .indexer
                        .evidence_by_span
                        .insert(span_id.clone(), id.to_string());
                }
            }
        }

        // Rebuild the retrieval index from state — it is derived, not source.
        for idx in 0..runtime.raw.len() {
            let text = runtime.raw.text_of(&runtime.raw.span(idx).id).to_string();
            runtime.index.add_span(idx, &text);
        }
        for idx in 0..runtime.graph.len() {
            runtime.reindex(idx);
        }
        Ok(runtime)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RebuildReport {
    pub cleared_level_cache_entries: usize,
    pub rebuilt_l1: usize,
    pub rebuilt_l2: usize,
    pub rebuilt_l3: usize,
    pub tokens: usize,
    pub nodes: usize,
}

impl std::fmt::Display for RebuildReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cleared_level_cache_entries: {}, rebuilt: {{L1: {}, L2: {}, L3: {}}}, tokens: {}, nodes: {}",
            self.cleared_level_cache_entries,
            self.rebuilt_l1,
            self.rebuilt_l2,
            self.rebuilt_l3,
            self.tokens,
            self.nodes
        )
    }
}

fn strings(values: &[String]) -> Json {
    Json::Arr(values.iter().map(Json::str).collect())
}

fn node_to_json(node: &Node) -> Json {
    let derivation = match &node.meta.derivation {
        Some(d) => Json::obj(vec![
            ("name", Json::str(&d.name)),
            (
                "inputs",
                Json::Obj(
                    d.inputs
                        .iter()
                        .map(|(k, v)| (k.clone(), Json::num(*v)))
                        .collect(),
                ),
            ),
        ]),
        None => Json::Null,
    };
    Json::obj(vec![
        ("node_id", Json::str(&node.id)),
        ("kind", Json::str(node.kind.as_str())),
        ("value", Json::str(&node.value)),
        ("source_spans", strings(&node.source_spans)),
        ("dependencies", strings(&node.dependencies)),
        ("confidence", Json::num(node.confidence as f64)),
        ("timestamp", Json::num(node.timestamp as f64)),
        ("status", Json::str(node.status.as_str())),
        ("key", node.key.clone().map_or(Json::Null, Json::Str)),
        ("reads", Json::num(node.reads.get() as f64)),
        ("admits", Json::num(node.admits.get() as f64)),
        (
            "l1",
            node.level_cache
                .borrow()
                .l1
                .clone()
                .map_or(Json::Null, Json::Str),
        ),
        (
            "l3",
            node.level_cache.borrow().l3.map_or(Json::Null, Json::num),
        ),
        (
            "meta",
            Json::obj(vec![
                ("corrective", Json::Bool(node.meta.corrective)),
                ("grounded", Json::Bool(node.meta.grounded)),
                (
                    "superseded_by",
                    node.meta
                        .superseded_by
                        .clone()
                        .map_or(Json::Null, Json::Str),
                ),
                ("supersedes", strings(&node.meta.supersedes)),
                ("contradicts", strings(&node.meta.contradicts)),
                ("superseded_source", Json::Bool(node.meta.superseded_source)),
                ("corrected_by", strings(&node.meta.corrected_by)),
                ("corroborations", Json::num(node.meta.corroborations as f64)),
                ("l0_reads", Json::num(node.meta.l0_reads.get() as f64)),
                ("l0_yield", Json::num(node.meta.l0_yield.get() as f64)),
                ("deescalated", Json::Bool(node.meta.deescalated)),
                ("derivation", derivation),
            ]),
        ),
    ])
}

fn node_from_json(data: &Json) -> Node {
    let meta_json = data.get("meta");
    let get_meta = |key: &str| meta_json.and_then(|m| m.get(key));
    let derivation = get_meta("derivation").and_then(|d| {
        let name = d.get("name")?.as_str()?.to_string();
        let inputs = match d.get("inputs") {
            Some(Json::Obj(pairs)) => pairs
                .iter()
                .filter_map(|(k, v)| v.as_f64().map(|v| (k.clone(), v)))
                .collect(),
            _ => Vec::new(),
        };
        Some(Derivation { name, inputs })
    });
    let meta = NodeMeta {
        corrective: get_meta("corrective")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        grounded: get_meta("grounded").and_then(Json::as_bool).unwrap_or(true),
        superseded_by: get_meta("superseded_by")
            .and_then(Json::as_str)
            .map(str::to_string),
        supersedes: get_meta("supersedes")
            .map(Json::string_list)
            .unwrap_or_default(),
        contradicts: get_meta("contradicts")
            .map(Json::string_list)
            .unwrap_or_default(),
        superseded_source: get_meta("superseded_source")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        corrected_by: get_meta("corrected_by")
            .map(Json::string_list)
            .unwrap_or_default(),
        corroborations: get_meta("corroborations")
            .and_then(Json::as_u64)
            .unwrap_or(0) as u32,
        l0_reads: Cell::new(get_meta("l0_reads").and_then(Json::as_u64).unwrap_or(0) as u32),
        l0_yield: Cell::new(get_meta("l0_yield").and_then(Json::as_u64).unwrap_or(0) as u32),
        deescalated: get_meta("deescalated")
            .and_then(Json::as_bool)
            .unwrap_or(false),
        derivation,
        source: None,
    };
    Node {
        id: data
            .get("node_id")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string(),
        kind: data
            .get("kind")
            .and_then(Json::as_str)
            .and_then(Kind::parse)
            .unwrap_or(Kind::Claim),
        value: data
            .get("value")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string(),
        source_spans: data
            .get("source_spans")
            .map(Json::string_list)
            .unwrap_or_default(),
        dependencies: data
            .get("dependencies")
            .map(Json::string_list)
            .unwrap_or_default(),
        confidence: data.get("confidence").and_then(Json::as_f64).unwrap_or(1.0) as f32,
        timestamp: data.get("timestamp").and_then(Json::as_u64).unwrap_or(0),
        status: data
            .get("status")
            .and_then(Json::as_str)
            .and_then(Status::parse)
            .unwrap_or(Status::Fresh),
        key: data.get("key").and_then(Json::as_str).map(str::to_string),
        level_cache: RefCell::new(LevelCache {
            l1: data.get("l1").and_then(Json::as_str).map(str::to_string),
            l2: None,
            l3: data.get("l3").and_then(Json::as_f64),
        }),
        meta,
        reads: Cell::new(data.get("reads").and_then(Json::as_u64).unwrap_or(0) as u32),
        admits: Cell::new(data.get("admits").and_then(Json::as_u64).unwrap_or(0) as u32),
    }
}
