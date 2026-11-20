//! Node importance ("importance" from the SubNext memory brainstorm): a
//! per-node priority that an eviction or prefetch policy can rank by, combining
//! four signals the runtime already tracks or can derive cheaply:
//!
//! - **surprise** — `1 - max cosine to any other node`: a node unlike everything
//!   else carries information nothing else does, so losing it loses the most.
//! - **recency** — newer nodes score higher (linear in timestamp over the store).
//! - **usage** — how often the node was admitted *and* cited (`reads`), log-scaled.
//! - **confidence** — the node's own stated confidence.
//!
//! `importance = 0.35·surprise + 0.25·recency + 0.25·usage + 0.15·confidence`,
//! each term normalised to `[0, 1]`. This is a **read-only report**: it changes
//! no node and evicts nothing. Wiring it into [`crate::eviction`] as a priority
//! is a deliberate, separately-measured next step — the brainstorm's hard-budget
//! caveat means a new eviction policy has to beat the near-LRU baseline on the
//! `bench` harness before it replaces it, not just exist.

use crate::embed::{cosine, embed};
use crate::nodes::Node;

/// Weights on the four importance signals; they sum to 1.0.
const W_SURPRISE: f32 = 0.35;
const W_RECENCY: f32 = 0.25;
const W_USAGE: f32 = 0.25;
const W_CONFIDENCE: f32 = 0.15;

struct Scored<'a> {
    node: &'a Node,
    surprise: f32,
    recency: f32,
    usage: f32,
    importance: f32,
}

fn one_line(value: &str, max_chars: usize) -> String {
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(max_chars).collect();
    if flat.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// Rank live nodes by importance and render the most- and least-important.
pub fn run(nodes: &[Node], top: usize) -> String {
    let live: Vec<&Node> = nodes
        .iter()
        .filter(|n| n.status.is_live() && n.value.trim().chars().count() >= 20)
        .collect();
    let n = live.len();
    if n < 2 {
        return format!("importance: {n} live node(s) with enough text — need at least 2.\n");
    }

    let vecs: Vec<Vec<f32>> = live.iter().map(|x| embed(&x.value)).collect();
    let min_ts = live.iter().map(|x| x.timestamp).min().unwrap_or(0);
    let max_ts = live.iter().map(|x| x.timestamp).max().unwrap_or(0);
    let ts_span = (max_ts.saturating_sub(min_ts)).max(1) as f32;
    let max_reads = live.iter().map(|x| x.reads.get()).max().unwrap_or(0);
    let usage_norm = (1.0 + max_reads as f32).ln();

    let mut scored: Vec<Scored> = (0..n)
        .map(|i| {
            // surprise: 1 - the highest similarity to any OTHER node.
            let mut max_sim = 0.0f32;
            for (j, vj) in vecs.iter().enumerate() {
                if j != i {
                    max_sim = max_sim.max(cosine(&vecs[i], vj));
                }
            }
            let surprise = (1.0 - max_sim).clamp(0.0, 1.0);
            let recency = (live[i].timestamp.saturating_sub(min_ts)) as f32 / ts_span;
            let usage = if usage_norm > 0.0 {
                (1.0 + live[i].reads.get() as f32).ln() / usage_norm
            } else {
                0.0
            };
            let confidence = live[i].confidence.clamp(0.0, 1.0);
            let importance = W_SURPRISE * surprise
                + W_RECENCY * recency
                + W_USAGE * usage
                + W_CONFIDENCE * confidence;
            Scored {
                node: live[i],
                surprise,
                recency,
                usage,
                importance,
            }
        })
        .collect();

    scored.sort_by(|a, b| b.importance.total_cmp(&a.importance));

    let mut out = String::new();
    out.push_str(&format!(
        "importance: {n} live nodes (0.35 surprise + 0.25 recency + 0.25 usage + 0.15 confidence)\n\n"
    ));
    let k = top.min(n);
    out.push_str("MOST important (keep):\n");
    for s in scored.iter().take(k) {
        out.push_str(&row(s));
    }
    out.push_str("\nLEAST important (evict candidates):\n");
    for s in scored.iter().rev().take(k) {
        out.push_str(&row(s));
    }
    out
}

fn row(s: &Scored) -> String {
    format!(
        "  {:.3}  [surp {:.2} rec {:.2} use {:.2}] {} {}\n",
        s.importance,
        s.surprise,
        s.recency,
        s.usage,
        s.node.kind.as_str(),
        one_line(&s.node.value, 90),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_handled() {
        assert!(run(&[], 5).contains("need at least 2"));
    }

    #[test]
    fn weights_sum_to_one() {
        let sum = W_SURPRISE + W_RECENCY + W_USAGE + W_CONFIDENCE;
        assert!(
            (sum - 1.0).abs() < 1e-6,
            "weights must sum to 1.0, got {sum}"
        );
    }

    #[test]
    fn one_line_truncates() {
        let s = one_line("a b c d e f g h i j k l m n o p q r s t", 10);
        assert!(s.chars().count() <= 11);
    }
}
