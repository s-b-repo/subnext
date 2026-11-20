//! Alien discovery: coherent-but-unproposed **bridges** between memory nodes.
//!
//! This is the discovery engine (TRISKELE §14) run *inside* the memory runtime,
//! over what has already been ingested, instead of over a prompt. It is the
//! dual of retrieval: retrieval answers "what do I already know about X?";
//! discovery answers "which two things I know are related but were never
//! connected?" — the spaces between the known concept vectors.
//!
//! For every pair of live nodes it computes, from the bundled embedding:
//!
//! - **coherence** = cosine similarity. A bridge is only interesting inside a
//!   band `(lo, hi)`: below `lo` the two nodes are unrelated (an incoherent
//!   pairing), above `hi` they are near-duplicates (nothing new to connect).
//! - **novelty**  = `1 / (1 + shared)`, where `shared` is how many *other*
//!   nodes are similar (`> tau`) to both. Many shared neighbours means the
//!   connection is already well covered by existing material; few or none means
//!   the bridge is unproposed.
//!
//! `gap = coherence * novelty`, ranked descending. By default only nodes from
//! **different sources** bridge, so a hit is a cross-document connection rather
//! than two sentences of one page.
//!
//! It is read-only: it builds nothing, persists nothing, and changes no node.
//! It embeds with an **IDF weighting computed from the store's own node texts**
//! ([`idf_embed`]), so a bridge reflects shared *distinctive* vocabulary rather
//! than shared stopwords. That weighting is local to discovery on purpose: the
//! retrieval and certificate paths keep the bundled embedder unchanged.

use std::collections::{HashMap, HashSet};

use crate::embed::{DIM, cosine};
use crate::graph::MemoryGraph;
use crate::nodes::Node;
use crate::spans::RawStore;
use crate::text::content_tokens;

/// FNV-1a of a token into `[0, dim)` — the same hash the bundled embedder uses,
/// replicated here so this module stays self-contained.
fn hash_bin(token: &str, dim: usize) -> usize {
    let mut hash: u32 = 2_166_136_261;
    for byte in token.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    (hash as usize) % dim
}

/// Inverse document frequency over the store's own node texts. A token in every
/// node carries no signal (idf → ~1 after the +1 floor); a rare, distinctive one
/// dominates. Computed once per `discover` call; never persisted.
fn idf_table(texts: &[String]) -> HashMap<String, f32> {
    let n = texts.len().max(1) as f32;
    let mut df: HashMap<String, usize> = HashMap::new();
    for text in texts {
        let mut seen: HashSet<String> = HashSet::new();
        for token in content_tokens(text) {
            if seen.insert(token.clone()) {
                *df.entry(token).or_insert(0) += 1;
            }
        }
    }
    df.into_iter()
        .map(|(token, d)| (token, ((1.0 + n) / (1.0 + d as f32)).ln() + 1.0))
        .collect()
}

/// IDF-weighted word + char-trigram hashing embedding, L2-normalised. Same shape
/// as [`crate::embed::hashing_embed`] but each token's weight is scaled by its
/// IDF, so bridges reflect shared *distinctive* vocabulary rather than shared
/// stopwords. Unknown tokens (absent from the table) default to weight 1.0.
fn idf_embed(text: &str, idf: &HashMap<String, f32>) -> Vec<f32> {
    let mut vec = vec![0.0f32; DIM];
    let tokens = content_tokens(text);
    if tokens.is_empty() {
        return vec;
    }
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for token in &tokens {
        *counts.entry(token.as_str()).or_insert(0) += 1;
    }
    for (token, count) in counts {
        let weight = (1.0 + (count as f32).ln()) * idf.get(token).copied().unwrap_or(1.0);
        vec[hash_bin(token, DIM)] += weight;
        let chars: Vec<char> = token.chars().collect();
        if chars.len() >= 3 {
            for window in chars.windows(3) {
                let trigram: String = window.iter().collect();
                vec[hash_bin(&trigram, DIM)] += 0.35 * weight;
            }
        }
    }
    let norm = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut vec {
            *v /= norm;
        }
    }
    vec
}

/// Tunables for [`run`].
#[derive(Debug, Clone, Copy)]
pub struct DiscoverOpts {
    /// Minimum coherence: below this the pair is unrelated.
    pub lo: f32,
    /// Maximum coherence: above this the pair is a near-duplicate.
    pub hi: f32,
    /// A node counts as a shared neighbour of a pair when it is this similar to
    /// both. Higher `tau` means only strong overlaps suppress novelty.
    pub tau: f32,
    /// How many bridges to render.
    pub top: usize,
    /// Only bridge nodes whose `meta.source` differs (a cross-document link).
    pub cross_source: bool,
}

impl Default for DiscoverOpts {
    fn default() -> Self {
        Self {
            lo: 0.10,
            hi: 0.70,
            tau: 0.30,
            top: 10,
            cross_source: true,
        }
    }
}

/// A live node paired with its embedding and a stable source label.
struct Cand<'a> {
    node: &'a Node,
    vec: Vec<f32>,
    source: &'a str,
}

/// A scored bridge between two candidate indices.
struct Bridge {
    i: usize,
    j: usize,
    coherence: f32,
    shared: u32,
    gap: f32,
}

fn one_line(value: &str, max_chars: usize) -> String {
    let flat: String = value
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let trimmed = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = trimmed.chars().take(max_chars).collect();
    if trimmed.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// Find and render the top coherent-but-unproposed bridges in `graph`.
pub fn run(graph: &MemoryGraph, raw: &RawStore, opts: &DiscoverOpts) -> String {
    let live: Vec<&Node> = graph
        .nodes()
        .iter()
        .filter(|n| n.status.is_live() && n.value.trim().chars().count() >= 20)
        .collect();
    // Weight each node's embedding by IDF over the store's own texts, so a
    // bridge reflects shared distinctive vocabulary. Local to discover: the
    // retrieval/certificate path keeps the bundled embedder untouched.
    let texts: Vec<String> = live.iter().map(|n| n.value.clone()).collect();
    let idf = idf_table(&texts);
    let cands: Vec<Cand> = live
        .iter()
        .map(|n| {
            // A derived node has no source of its own; recover it from the
            // document its first source span belongs to, so a bridge between
            // two documents reads as cross-source.
            let source = n
                .source_spans
                .first()
                .and_then(|sid| raw.span_by_id(sid))
                .map(|sp| sp.doc_id.as_str())
                .or(n.meta.source.as_deref())
                .unwrap_or("");
            Cand {
                node: n,
                vec: idf_embed(&n.value, &idf),
                source,
            }
        })
        .collect();

    let n = cands.len();
    if n < 2 {
        return format!("discover: {n} live node(s) with enough text — need at least 2.\n");
    }

    // Derived nodes often carry no source (source attaches to spans). Requiring
    // a cross-source bridge then rejects every pair, so fall back to in-store
    // bridges when the store has fewer than two distinct sources.
    let distinct_sources = {
        let mut s: Vec<&str> = cands
            .iter()
            .map(|c| c.source)
            .filter(|x| !x.is_empty())
            .collect();
        s.sort_unstable();
        s.dedup();
        s.len()
    };
    let cross_source = opts.cross_source && distinct_sources >= 2;

    // One pass of all pairs: record tau-neighbours as a bitset per node, and
    // collect the pairs that fall inside the coherence band.
    let words = n.div_ceil(64);
    let mut neighbours = vec![0u64; n * words];
    let mut band: Vec<(usize, usize, f32)> = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            let c = cosine(&cands[i].vec, &cands[j].vec);
            if c > opts.tau {
                neighbours[i * words + (j / 64)] |= 1u64 << (j % 64);
                neighbours[j * words + (i / 64)] |= 1u64 << (i % 64);
            }
            if c > opts.lo && c < opts.hi {
                if cross_source
                    && (cands[i].source.is_empty() || cands[i].source == cands[j].source)
                {
                    continue;
                }
                band.push((i, j, c));
            }
        }
    }

    // Score each band pair by shared-neighbour count (bitset intersection).
    let mut bridges: Vec<Bridge> = band
        .into_iter()
        .map(|(i, j, c)| {
            let mut shared = 0u32;
            for w in 0..words {
                shared += (neighbours[i * words + w] & neighbours[j * words + w]).count_ones();
            }
            let gap = c * (1.0 / (1.0 + shared as f32));
            Bridge {
                i,
                j,
                coherence: c,
                shared,
                gap,
            }
        })
        .collect();

    bridges.sort_by(|a, b| b.gap.total_cmp(&a.gap));
    bridges.truncate(opts.top);

    render(&cands, &bridges, n, opts, cross_source)
}

fn render(
    cands: &[Cand],
    bridges: &[Bridge],
    n: usize,
    opts: &DiscoverOpts,
    cross_source: bool,
) -> String {
    let mut out = String::new();
    let scope = if cross_source {
        ", cross-source only"
    } else if opts.cross_source {
        ", in-store (no per-node sources)"
    } else {
        ""
    };
    out.push_str(&format!(
        "discover: {n} live nodes, coherence band ({:.2}, {:.2}), tau {:.2}{scope}\n\n",
        opts.lo, opts.hi, opts.tau,
    ));
    if bridges.is_empty() {
        out.push_str("no bridges in band — widen with --lo/--hi or allow same-source.\n");
        return out;
    }
    for (rank, b) in bridges.iter().enumerate() {
        let a = &cands[b.i];
        let d = &cands[b.j];
        out.push_str(&format!(
            "#{}  gap={:.3}  coherence={:.3}  shared-bridges={}\n",
            rank + 1,
            b.gap,
            b.coherence,
            b.shared,
        ));
        out.push_str(&format!(
            "    A [{} {}] {}\n",
            a.node.kind.as_str(),
            source_label(a.source),
            one_line(&a.node.value, 130),
        ));
        out.push_str(&format!(
            "    B [{} {}] {}\n\n",
            d.node.kind.as_str(),
            source_label(d.source),
            one_line(&d.node.value, 130),
        ));
    }
    out
}

fn source_label(source: &str) -> &str {
    if source.is_empty() {
        return "?";
    }
    match source.rsplit('/').next() {
        Some(name) if !name.is_empty() => name,
        _ => source,
    }
}
