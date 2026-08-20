//! Retrieval index over both text chunks and state nodes.
//!
//! Two signals, because neither alone is enough: BM25 finds exact identifiers
//! (`10.0.4.12`, `ERR_CONN_REFUSED`) that an embedding smears away, and the
//! vector side finds paraphrases that share no tokens with the query. Indexing
//! *state nodes* — not only text — is what makes graph-seeded retrieval work at
//! all, and it is the point where DCR stops looking like RAG.
//!
//! Span vectors are deliberately not built eagerly: spans get the cheap lexical
//! index at ingest, and only nodes (few, small) get vectors.

use std::collections::HashMap;

use crate::embed::{DIM, cosine, hashing_embed};
use crate::text::content_tokens;

/// Ties break on the item's own index, never on hash order — the plan a query
/// produces must not depend on which way a `HashMap` happened to iterate.
fn rank(mut scored: Vec<(usize, f32)>, k: usize) -> Vec<(usize, f32)> {
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(k);
    scored
}

/// BM25 over whatever text it is given.
#[derive(Debug)]
pub struct LexicalIndex {
    k1: f32,
    b: f32,
    postings: HashMap<String, HashMap<usize, u32>>,
    lengths: HashMap<usize, usize>,
    total_len: usize,
}

impl Default for LexicalIndex {
    fn default() -> Self {
        Self {
            k1: 1.4,
            b: 0.72,
            postings: HashMap::new(),
            lengths: HashMap::new(),
            total_len: 0,
        }
    }
}

impl LexicalIndex {
    pub fn add(&mut self, id: usize, text: &str) {
        if self.lengths.contains_key(&id) {
            self.remove(id);
        }
        let tokens = content_tokens(text);
        if tokens.is_empty() {
            self.lengths.insert(id, 0);
            return;
        }
        let mut counts: HashMap<String, u32> = HashMap::new();
        for token in &tokens {
            *counts.entry(token.clone()).or_insert(0) += 1;
        }
        for (token, count) in counts {
            self.postings.entry(token).or_default().insert(id, count);
        }
        self.lengths.insert(id, tokens.len());
        self.total_len += tokens.len();
    }

    pub fn remove(&mut self, id: usize) {
        let length = self.lengths.remove(&id).unwrap_or(0);
        self.total_len = self.total_len.saturating_sub(length);
        self.postings.retain(|_, posting| {
            posting.remove(&id);
            !posting.is_empty()
        });
    }

    pub fn search(&self, query: &str, k: usize) -> Vec<(usize, f32)> {
        let n = self.lengths.len().max(1) as f32;
        let avg = if self.total_len == 0 {
            1.0
        } else {
            (self.total_len as f32 / n).max(1.0)
        };
        let mut scores: HashMap<usize, f32> = HashMap::new();
        let mut seen: Vec<String> = content_tokens(query);
        seen.sort();
        seen.dedup();
        for token in seen {
            let Some(posting) = self.postings.get(&token) else {
                continue;
            };
            let df = posting.len() as f32;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for (&id, &tf) in posting {
                let length = self.lengths.get(&id).copied().unwrap_or(0).max(1) as f32;
                let tf = tf as f32;
                let denom = tf + self.k1 * (1.0 - self.b + self.b * length / avg);
                *scores.entry(id).or_insert(0.0) += idf * (tf * (self.k1 + 1.0)) / denom;
            }
        }
        rank(scores.into_iter().collect(), k)
    }

    pub fn len(&self) -> usize {
        self.lengths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lengths.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct VectorIndex {
    vectors: HashMap<usize, Vec<f32>>,
}

impl VectorIndex {
    pub fn add(&mut self, id: usize, vector: Vec<f32>) {
        self.vectors.insert(id, vector);
    }

    pub fn add_text(&mut self, id: usize, text: &str) {
        self.vectors.insert(id, hashing_embed(text, DIM));
    }

    pub fn remove(&mut self, id: usize) {
        self.vectors.remove(&id);
    }

    pub fn search(&self, query_vec: &[f32], k: usize) -> Vec<(usize, f32)> {
        let scored: Vec<(usize, f32)> = self
            .vectors
            .iter()
            .map(|(&id, v)| (id, cosine(query_vec, v)))
            .filter(|(_, s)| *s > 0.0)
            .collect();
        rank(scored, k)
    }

    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    /// State nodes — a hit here is a seed the planner can expand from.
    Node,
    /// Raw chunks — a hit here is material that must first be grounded.
    Span,
}

/// Lexical + vector, score-normalised and blended.
#[derive(Debug)]
pub struct HybridIndex {
    node_lexical: LexicalIndex,
    node_vector: VectorIndex,
    span_lexical: LexicalIndex,
    span_vector: VectorIndex,
    pub lexical_weight: f32,
}

impl Default for HybridIndex {
    fn default() -> Self {
        Self {
            node_lexical: LexicalIndex::default(),
            node_vector: VectorIndex::default(),
            span_lexical: LexicalIndex::default(),
            span_vector: VectorIndex::default(),
            lexical_weight: 0.55,
        }
    }
}

impl HybridIndex {
    pub fn add_node(&mut self, idx: usize, text: &str, vector: Vec<f32>) {
        self.node_lexical.add(idx, text);
        self.node_vector.add(idx, vector);
    }

    pub fn remove_node(&mut self, idx: usize) {
        self.node_lexical.remove(idx);
        self.node_vector.remove(idx);
    }

    pub fn add_span(&mut self, idx: usize, text: &str) {
        self.span_lexical.add(idx, text);
    }

    /// Opt-in eager L2 over raw chunks; off by default (laziness rule).
    pub fn add_span_vector(&mut self, idx: usize, text: &str) {
        self.span_vector.add_text(idx, text);
    }

    pub fn search(
        &self,
        namespace: Namespace,
        query: &str,
        query_vec: &[f32],
        k: usize,
    ) -> Vec<(usize, f32)> {
        let (lexical, vector) = match namespace {
            Namespace::Node => (&self.node_lexical, &self.node_vector),
            Namespace::Span => (&self.span_lexical, &self.span_vector),
        };
        let lex = lexical.search(query, k * 3);
        let vec = vector.search(query_vec, k * 3);
        let mut combined: HashMap<usize, f32> = HashMap::new();
        if let Some(top) = lex.first().map(|(_, s)| *s).filter(|s| *s > 0.0) {
            for (id, score) in lex {
                *combined.entry(id).or_insert(0.0) += self.lexical_weight * (score / top);
            }
        }
        if let Some(top) = vec.first().map(|(_, s)| *s).filter(|s| *s > 0.0) {
            for (id, score) in vec {
                *combined.entry(id).or_insert(0.0) += (1.0 - self.lexical_weight) * (score / top);
            }
        }
        rank(combined.into_iter().collect(), k)
    }

    pub fn stats(&self) -> IndexStats {
        IndexStats {
            nodes_indexed: self.node_lexical.len(),
            spans_indexed: self.span_lexical.len(),
            node_vectors: self.node_vector.len(),
            span_vectors: self.span_vector.len(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct IndexStats {
    pub nodes_indexed: usize,
    pub spans_indexed: usize,
    pub node_vectors: usize,
    pub span_vectors: usize,
}

impl std::fmt::Display for IndexStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "nodes_indexed: {}, spans_indexed: {}, node_vectors: {}, span_vectors: {}",
            self.nodes_indexed, self.spans_indexed, self.node_vectors, self.span_vectors
        )
    }
}
