//! Scratch sweep: recall@12 and candidate-set size for LSH configurations on
//! the REAL node vectors of the standard 3,000-turn corpus.
//!
//! For each of the seven probes we compute the exact top-12, then rebuild the
//! LSH candidate set locally with the same plane generation (splitmix64, same
//! seed and order as index.rs) and sweep table bit-count / probe radius /
//! table count. The winning point goes into index.rs as constants.

use std::collections::HashSet;

use dcr::bench::build_corpus;
use dcr::embed::{hashing_embed, DIM};

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

struct Sweep {
    planes: Vec<Vec<f32>>,
}

impl Sweep {
    /// Same plane layout as index.rs::Lsh::new: TABLES*BITS planes, uniform.
    fn new(tables: usize, bits: usize) -> Self {
        // Plane sequence depends only on the seed; shorter signatures reuse the
        // same leading planes, so comparisons between configs are fair.
        let mut state = 0x5DEE_CE66_D1CE_F00D;
        let planes = (0..tables.max(12) * bits.max(12))
            .map(|_| {
                (0..dim())
                    .map(|_| (splitmix64(&mut state) as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0)
                    .collect()
            })
            .collect();
        Self { planes }
    }

    fn sig(&self, table: usize, bits: usize, v: &[f32]) -> u32 {
        let mut sig = 0u32;
        for bit in 0..bits {
            let plane = &self.planes[table * 12 + bit];
            let dot: f32 = v.iter().zip(plane.iter()).map(|(a, b)| a * b).sum();
            if dot >= 0.0 {
                sig |= 1 << bit;
            }
        }
        sig
    }

    fn candidates(
        &self,
        tables: &[std::collections::HashMap<u32, Vec<usize>>],
        v: &[f32],
        bits: usize,
        radius: u32,
    ) -> HashSet<usize> {
        let mut out = HashSet::new();
        for (t, table) in tables.iter().enumerate().take(tables.len()) {
            let sig = self.sig(t, bits, v);
            // Enumerate probes within radius: flip r distinct bits.
            let flip_sets: Vec<Vec<u32>> = if radius == 0 {
                vec![vec![]]
            } else if radius == 1 {
                (0..bits as u32).map(|b| vec![1u32 << b]).collect()
            } else {
                (0..bits as u32)
                    .flat_map(|a| (a + 1..bits as u32).map(move |b| (a, b)))
                    .map(|(a, b)| vec![1u32 << a, 1u32 << b])
                    .collect()
            };
            for flip in flip_sets {
                let mut s = sig;
                for f in &flip {
                    s ^= f;
                }
                if let Some(ids) = table.get(&s) {
                    out.extend(ids.iter().copied());
                }
            }
        }
        out
    }
}

fn dim() -> usize {
    DIM
}

fn main() -> Result<(), dcr::DcrError> {
    let turns = 3000;
    let budget = 1200;
    let corpus = build_corpus(turns);
    let mut rt = dcr::Dcr::new(budget);
    for (doc_id, text) in &corpus.docs {
        rt.ingest(text, Some(doc_id.as_str()))?;
    }
    println!("nodes: {}", rt.graph.len());

    // Exact top-12 per probe and the real stored vectors from the index.
    rt.index.set_exact(true);
    let mut exact_tops: Vec<Vec<(usize, f32)>> = Vec::new();
    for probe in &corpus.probes {
        let qv = hashing_embed(probe.query, DIM);
        exact_tops.push(rt.index.search(dcr::index::Namespace::Node, probe.query, &qv, 12));
    }

    // The stored node vectors, via a clone of the vector index: read them from
    // the runtime's own channel so the sweep scores what production scores.
    rt.index.set_exact(false);
    let vectors: Vec<Vec<f32>> = {
        // ladder.vector is what indexer::reindex stored. Rebuild it per node.
        let mut out = Vec::new();
        for idx in 0..rt.graph.len() {
            let node = rt.graph.node(idx);
            out.push(rt.ladder.vector(&rt.raw, node));
        }
        out
    };

    for (bits, radius, tables) in [
        (12u64, 1u32, 8usize), // current config
        (8, 1, 8),
        (8, 1, 12),
        (8, 1, 16),
        (7, 1, 8),
        (7, 1, 16),
        (10, 1, 12),
        (8, 2, 8),
        (8, 2, 12),
        (10, 2, 8),
        (6, 1, 16),
        (6, 2, 8),
        (12, 2, 8),
    ] {
        let sweep = Sweep::new(tables, bits as usize);
        // Build tables from the real vectors.
        let mut built: Vec<std::collections::HashMap<u32, Vec<usize>>> =
            vec![Default::default(); tables];
        for (id, v) in vectors.iter().enumerate() {
            for (t, table) in built.iter_mut().enumerate().take(tables) {
                let s = sweep.sig(t, bits as usize, v);
                table.entry(s).or_default().push(id);
            }
        }
        // Sweep per probe.
        let mut hits = 0usize;
        let mut total = 0usize;
        let mut top1 = 0usize;
        let mut cand_total = 0usize;
        let mut shortest = usize::MAX;
        for p in exact_tops.iter() {
            let _ = p;
        }
        for (pi, probe) in corpus.probes.iter().enumerate() {
            let qv = hashing_embed(probe.query, DIM);
            let cands = sweep.candidates(&built, &qv, bits as usize, radius);
            cand_total += cands.len();
            shortest = shortest.min(cands.len());
            let exact: HashSet<usize> =
                exact_tops[pi]
                    .iter()
                    .map(|(i, _)| *i)
                    .collect();
            hits += cands.intersection(&exact).count();
            total += exact.len();
            let best = exact_tops[pi]
                .first()
                .map(|(i, _)| *i);
            if let Some(b) = best {
                if cands.contains(&b) {
                    top1 += 1;
                }
            }
        }
        println!(
            "bits={bits:>2} radius={radius} tables={tables:>2}  recall={:>5.1}%  top1={top1}/{}  cand/turn mean={:>6.0} min={shortest:>4} ({:>4.1}% of N)",
            hits as f64 / total.max(1) as f64 * 100.0,
            corpus.probes.len(),
            cand_total as f64 / corpus.probes.len() as f64,
            cand_total as f64 / corpus.probes.len() as f64 / vectors.len() as f64 * 100.0,
        );
    }
    Ok(())
}
