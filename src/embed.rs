//! L2 vectors without a model dependency.
//!
//! A hashing embedder is enough for the runtime's actual requirement: making
//! state nodes findable by meaning-ish similarity so the planner has seeds to
//! expand from. Deterministic, dependency-free and instant, which keeps the
//! whole runtime runnable offline.

use std::collections::HashMap;

use crate::text::content_tokens;

pub const DIM: usize = 256;

/// FNV-1a over the token, so persisted vectors stay comparable across runs.
fn hash_index(token: &str, dim: usize) -> usize {
    let mut hash: u32 = 2_166_136_261;
    for byte in token.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    (hash as usize) % dim
}

/// Word + character-trigram hashing embedding, L2-normalised.
///
/// The trigrams give partial credit for morphological variants and typos,
/// which plain word hashing cannot do.
pub fn hashing_embed(text: &str, dim: usize) -> Vec<f32> {
    let mut vec = vec![0.0f32; dim];
    let tokens = content_tokens(text);
    if tokens.is_empty() {
        return vec;
    }
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for token in &tokens {
        *counts.entry(token.as_str()).or_insert(0) += 1;
    }
    for (token, count) in counts {
        let weight = 1.0 + (count as f32).ln();
        vec[hash_index(token, dim)] += weight;
        let chars: Vec<char> = token.chars().collect();
        if chars.len() >= 3 {
            for window in chars.windows(3) {
                let trigram: String = window.iter().collect();
                vec[hash_index(&trigram, dim)] += 0.35 * weight;
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

pub fn embed(text: &str) -> Vec<f32> {
    hashing_embed(text, DIM)
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
