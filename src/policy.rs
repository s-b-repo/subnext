//! Runtime decision policy — what to do with each piece of context.
//!
//! The wiki's decision table, made executable. The hard part it leaves open is
//! `sufficient(L, q)`: there is no clean definition of the cheapest level that
//! can answer a query. This implements the practical proxy the spec recommends
//! — route by query type, then *measure* escalation and let the escalation rate
//! be the telemetry that catches a bad router — plus a de-escalation rule so a
//! node that keeps being read at L0 without yielding anything new collapses
//! into a cached fact.

use crate::ladder::Level;
use crate::nodes::{Kind, Node, Status};
use crate::text::contains_any;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryType {
    QuoteExact,
    ValueLookup,
    Justify,
    Recompute,
    Summarize,
    Open,
}

impl QueryType {
    pub fn as_str(self) -> &'static str {
        match self {
            QueryType::QuoteExact => "quote_exact",
            QueryType::ValueLookup => "value_lookup",
            QueryType::Justify => "justify",
            QueryType::Recompute => "recompute",
            QueryType::Summarize => "summarize",
            QueryType::Open => "open",
        }
    }

    /// Preferred admission levels, cheapest sufficient first.
    pub fn routing(self) -> [Level; 3] {
        match self {
            QueryType::QuoteExact => [Level::L0, Level::L1, Level::L2],
            QueryType::ValueLookup => [Level::L2, Level::L1, Level::L0],
            QueryType::Justify => [Level::L2, Level::L1, Level::L0],
            QueryType::Recompute => [Level::L3, Level::L2, Level::L0],
            QueryType::Summarize => [Level::L1, Level::L2, Level::L0],
            QueryType::Open => [Level::L2, Level::L1, Level::L0],
        }
    }
}

const QUOTE_MARKERS: &[&str] = &[
    "quote",
    "verbatim",
    "exact",
    "exactly",
    "word for word",
    "word-for-word",
    "literal",
    "raw text",
    "exact wording",
    "full text",
    "transcript",
    "as written",
    "stack trace",
    "exact error",
    "error message",
    "log line",
];

const RECOMPUTE_MARKERS: &[&str] = &[
    "recompute",
    "recalculate",
    "rerun",
    "re-run",
    "compute",
    "calculate",
    "total of",
    "total for",
    "sum of",
    "sum for",
    "average of",
    "cost of",
    "cost for",
    "what's the cost",
    "what is the cost",
    "what's the total",
    "what is the total",
    "what's the result",
    "what is the result",
];

const JUSTIFY_MARKERS: &[&str] = &[
    "why",
    "justify",
    "justification",
    "explain",
    "reason",
    "because",
    "on what basis",
    "how did you",
    "how did we",
    "what led to",
    "audit",
    "evidence for",
    "prove",
    "back up",
    "backing up",
];

const SUMMARIZE_MARKERS: &[&str] = &[
    "summary",
    "summarise",
    "summarize",
    "recap",
    "overview",
    "tldr",
    "tl;dr",
    "gist",
];

const VALUE_MARKERS: &[&str] = &[
    "what is",
    "what was",
    "what are",
    "what were",
    "which",
    "who",
    "when",
    "where",
    "current",
    "status of",
    "value of",
    "how much",
    "how many",
];

/// Kinds that are cheap, near-always relevant, and pinned into every turn.
pub const ALWAYS_ADMIT: [Kind; 2] = [Kind::Goal, Kind::Constraint];

/// What the runtime decided about one node this turn — for telemetry and for
/// `explain_plan()`.
#[derive(Debug, Clone)]
pub struct Decision {
    pub level: Option<Level>,
    pub admitted: bool,
    pub recompute: bool,
    pub reason: &'static str,
}

#[derive(Debug, Clone)]
pub struct DecisionPolicy {
    pub deescalate_after: u32,
}

impl Default for DecisionPolicy {
    fn default() -> Self {
        Self {
            deescalate_after: 3,
        }
    }
}

impl DecisionPolicy {
    // -- routing -----------------------------------------------------------
    pub fn classify(&self, query: &str) -> QueryType {
        let q = query.to_lowercase();
        if contains_any(&q, QUOTE_MARKERS) {
            return QueryType::QuoteExact;
        }
        // Deliberately narrow: a bare "how many" is a lookup far more often
        // than it is a computation, and mis-routing a lookup to L3 wastes an
        // execution and returns nothing.
        if contains_any(&q, RECOMPUTE_MARKERS)
            || (contains_any(&q, &["how much"]) && contains_any(&q, &["cost"]))
        {
            return QueryType::Recompute;
        }
        if contains_any(&q, JUSTIFY_MARKERS) {
            return QueryType::Justify;
        }
        if contains_any(&q, SUMMARIZE_MARKERS) {
            return QueryType::Summarize;
        }
        if contains_any(&q, VALUE_MARKERS) {
            return QueryType::ValueLookup;
        }
        QueryType::Open
    }

    /// Cheapest level in the routing order that this node actually has.
    pub fn preferred_level(&self, node: &Node, qtype: QueryType, available: &[Level]) -> Level {
        for level in qtype.routing() {
            if available.contains(&level) {
                if level == Level::L0 && self.prefers_compact(node, qtype) {
                    continue;
                }
                return level;
            }
        }
        available.first().copied().unwrap_or(Level::L2)
    }

    /// Never promote a settled, well-grounded claim to raw bytes just because
    /// the router said L0 — level over-promotion is the failure mode that
    /// destroys the cost model.
    fn prefers_compact(&self, node: &Node, qtype: QueryType) -> bool {
        qtype != QueryType::QuoteExact && node.kind != Kind::Evidence && node.confidence >= 0.9
    }

    pub fn sufficient(&self, level: Level, qtype: QueryType) -> bool {
        if qtype == QueryType::QuoteExact {
            level == Level::L0
        } else {
            qtype.routing()[..2].contains(&level)
        }
    }

    /// Next richer level to try when the model reports insufficiency.
    pub fn escalation_target(&self, current: Level, available: &[Level]) -> Option<Level> {
        let mut target = match current {
            Level::L2 => Some(Level::L1),
            Level::L1 | Level::L3 => Some(Level::L0),
            Level::L0 => None,
        };
        while let Some(level) = target {
            if available.contains(&level) {
                return Some(level);
            }
            target = match level {
                Level::L2 => Some(Level::L1),
                Level::L1 | Level::L3 => Some(Level::L0),
                Level::L0 => None,
            };
        }
        None
    }

    /// A node read at L0 repeatedly with nothing new extracted should be
    /// collapsed into a cached fact instead of re-quoted every turn.
    pub fn should_deescalate(&self, node: &Node) -> bool {
        node.meta.l0_reads.get() >= self.deescalate_after && node.meta.l0_yield.get() == 0
    }

    // -- the decision table ------------------------------------------------
    pub fn decide(
        &self,
        node: &Node,
        qtype: QueryType,
        available: &[Level],
        in_plan: bool,
    ) -> Decision {
        if node.status == Status::Stale {
            return Decision {
                level: None,
                admitted: false,
                recompute: true,
                reason: "stale — recompute or re-ground before admitting",
            };
        }
        if node.status == Status::Superseded {
            return Decision {
                level: None,
                admitted: false,
                recompute: false,
                reason: "superseded",
            };
        }
        if !in_plan {
            return Decision {
                level: None,
                admitted: false,
                recompute: false,
                reason: "not referenced by current plan",
            };
        }
        let level = self.preferred_level(node, qtype, available);
        Decision {
            level: Some(level),
            admitted: true,
            recompute: false,
            reason: match level {
                Level::L0 => "query needs exact wording",
                Level::L1 => "referenced for gist only",
                Level::L2 => "compact state object is sufficient",
                Level::L3 => "answer is a function of known inputs",
            },
        }
    }

    pub fn pinned_kinds(&self, qtype: QueryType) -> Vec<Kind> {
        let mut kinds = ALWAYS_ADMIT.to_vec();
        match qtype {
            QueryType::Justify => kinds.push(Kind::Decision),
            QueryType::Recompute => kinds.push(Kind::Calculation),
            QueryType::Open => kinds.push(Kind::OpenQuestion),
            _ => {}
        }
        kinds
    }
}
