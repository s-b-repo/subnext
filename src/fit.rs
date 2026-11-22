//! The fit gate — detecting the workloads DCR is a poor fit for, and getting
//! out of the way.
//!
//! Planning is rent paid on history: indexing, scoring and the knapsack buy a
//! small, sufficient window out of a store too big to show whole. When the
//! entire store *fits* in the window untouched, that rent buys nothing — the
//! planner can only remove material the budget could have held. The gate
//! detects that shape and bypasses the machinery rather than pretending the
//! overhead is free.
//!
//! Bypass is for raw-text reasoners and histories whose graph adds no state
//! needed for the answer. Revisions, inferred state and skills need planning.

use crate::graph::MemoryGraph;
use crate::nodes::{Kind, Origin, Status};

/// What the gate decided. Kept as an enum rather than a bool so the reason is
/// part of the type a caller matches on — a bypass is a shortcut that must
/// stay auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Every ingested token fits in `B_attention`: show it all, skip planning.
    Bypass,
    /// History exceeds the window, or capabilities need the planner: the
    /// machinery is earning its rent.
    Plan,
}

/// Assess the size and whether raw history preserves the graph's current state.
pub fn assess(graph: &MemoryGraph, history_tokens: usize, budget: usize) -> Fit {
    if history_tokens <= budget && can_bypass(graph) {
        Fit::Bypass
    } else {
        Fit::Plan
    }
}

pub(crate) fn can_bypass(graph: &MemoryGraph) -> bool {
    graph.nodes().iter().all(|node| {
        matches!(node.status, Status::Fresh | Status::Unresolved)
            && node.origin == Origin::Observed
            && !matches!(node.kind, Kind::Calculation | Kind::Skill)
            && !node.source_spans.is_empty()
            && !node.meta.superseded_source
            && node.meta.corrected_by.is_empty()
    })
}
