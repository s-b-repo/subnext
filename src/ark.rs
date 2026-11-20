//! Noah's ark ("keep one representative per cluster before eviction", from the
//! SubNext memory brainstorm): group the live nodes by similarity and show the
//! representative of each cluster. The point is coverage — a dumb LRU that
//! evicts a whole topic because it went quiet loses a capability; keeping one
//! representative per cluster keeps the store's *breadth* even as it shrinks.
//!
//! This is a **read-only report**: it clusters and prints, it evicts nothing.
//! It answers "if you had to shrink to K survivors, which nodes keep the store's
//! coverage?" — the input an eviction policy would use, measured before it is
//! trusted (the brainstorm's hard-budget rule: beat near-LRU on `bench` first).
//!
//! Clustering is a single greedy pass: a node joins the first existing cluster
//! whose seed it is within `threshold` cosine of, else it seeds a new cluster.
//! Deterministic and O(nodes × clusters); the seed is the representative.

use crate::embed::{cosine, embed};
use crate::nodes::Node;

struct Cluster {
    seed_vec: Vec<f32>,
    rep: usize, // index into the live slice
    members: usize,
}

fn one_line(value: &str, max_chars: usize) -> String {
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(max_chars).collect();
    if flat.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// Cluster the live nodes and render one representative per cluster.
pub fn run(nodes: &[Node], threshold: f32, top: usize) -> String {
    let live: Vec<&Node> = nodes
        .iter()
        .filter(|n| n.status.is_live() && n.value.trim().chars().count() >= 20)
        .collect();
    let n = live.len();
    if n == 0 {
        return "ark: no live nodes with enough text.\n".to_string();
    }

    let mut clusters: Vec<Cluster> = Vec::new();
    for (i, node) in live.iter().enumerate() {
        let v = embed(&node.value);
        let mut joined = false;
        for c in clusters.iter_mut() {
            if cosine(&v, &c.seed_vec) >= threshold {
                c.members += 1;
                joined = true;
                break;
            }
        }
        if !joined {
            clusters.push(Cluster {
                seed_vec: v,
                rep: i,
                members: 1,
            });
        }
    }

    clusters.sort_by_key(|c| std::cmp::Reverse(c.members));
    let covered = clusters.len();
    let mut out = String::new();
    out.push_str(&format!(
        "ark: {n} live nodes → {covered} clusters at cosine ≥ {threshold:.2} \
         (keep these {} representatives to preserve coverage)\n\n",
        covered.min(top),
    ));
    for c in clusters.iter().take(top) {
        out.push_str(&format!(
            "  [{:>3} members] {} {}\n",
            c.members,
            live[c.rep].kind.as_str(),
            one_line(&live[c.rep].value, 96),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_handled() {
        assert!(run(&[], 0.5, 5).contains("no live nodes"));
    }

    #[test]
    fn threshold_in_range() {
        // A sanity anchor: the default threshold the CLI uses is a cosine in
        // [0, 1]; this test documents the contract rather than exercising data.
        let t = 0.5f32;
        assert!((0.0..=1.0).contains(&t));
    }
}
