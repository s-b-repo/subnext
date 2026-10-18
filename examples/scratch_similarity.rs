//! Scratch probe: for the bench's seven standard queries over a 3,000-turn
//! store, print the true top-12 cosine-similarity profile (exact scan) — the
//! similarity band the LSH candidate generation has to capture.

use dcr::bench::{build_corpus, Corpus};
use dcr::embed::{cosine, hashing_embed, DIM};

fn main() -> Result<(), dcr::DcrError> {
    let corpus: Corpus = build_corpus(3000);
    let mut rt = dcr::Dcr::new(1200);
    for (id, text) in &corpus.docs {
        rt.ingest(text, Some(id.as_str()))?;
    }
    // Node label text, embedded the way the reader would. The index builds node
    // vectors internally from more context than this, so absolute ranks here are
    // an approximation — the shape of the similarity band is what we want, and
    // its cross-comparison between probes is valid under one embedder.
    let mut texts: Vec<String> = Vec::new();
    for idx in 0..rt.graph.len() {
        let node = rt.graph.node(idx);
        texts.push(format!("{} {}", node.id, node.label()));
    }
    let vectors: Vec<Vec<f32>> = texts
        .iter()
        .map(|t| hashing_embed(t.as_str(), DIM))
        .collect();
    for probe in &corpus.probes {
        let q = hashing_embed(probe.query, DIM);
        let mut scored: Vec<(usize, f32)> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| (i, cosine(&q, v)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        let sims: Vec<f32> = scored.iter().take(12).map(|(_, s)| *s).collect();
        println!(
            "{:<30} top12 min={:.3} med={:.3} max={:.3}",
            probe.label,
            sims.last().copied().unwrap_or(0.0),
            sims[sims.len() / 2],
            sims.first().copied().unwrap_or(0.0)
        );
        println!("  {:?}", sims);
    }
    Ok(())
}
