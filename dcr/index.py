"""Retrieval index over both text chunks and state nodes.

Two signals, because neither alone is enough: BM25 finds exact identifiers
(`10.0.4.12`, `ERR_CONN_REFUSED`) that an embedding smears away, and the
vector side finds paraphrases that share no tokens with the query. Indexing
*state nodes* — not only text — is what makes graph-seeded retrieval work at
all, and it is the point where DCR stops looking like RAG.

Span vectors are deliberately not built eagerly: spans get the cheap lexical
index at ingest, and only nodes (few, small) get vectors. This is the
laziness rule from the state-indexer spec.
"""

from __future__ import annotations

import math
from collections import defaultdict

from .embed import DIM, cosine, hashing_embed, content_tokens


class LexicalIndex:
    """BM25 over whatever text it is given."""

    def __init__(self, k1: float = 1.4, b: float = 0.72) -> None:
        self.k1, self.b = k1, b
        self.postings: dict[str, dict[str, int]] = defaultdict(dict)
        self.lengths: dict[str, int] = {}
        self.total_len = 0

    def add(self, doc_id: str, text: str) -> None:
        if doc_id in self.lengths:
            self.remove(doc_id)
        tokens = content_tokens(text)
        if not tokens:
            self.lengths[doc_id] = 0
            return
        counts: dict[str, int] = {}
        for token in tokens:
            counts[token] = counts.get(token, 0) + 1
        for token, count in counts.items():
            self.postings[token][doc_id] = count
        self.lengths[doc_id] = len(tokens)
        self.total_len += len(tokens)

    def remove(self, doc_id: str) -> None:
        length = self.lengths.pop(doc_id, 0)
        self.total_len -= length
        for token, posting in list(self.postings.items()):
            posting.pop(doc_id, None)
            if not posting:
                del self.postings[token]

    def search(self, query: str, k: int = 10) -> list[tuple[str, float]]:
        n = len(self.lengths) or 1
        avg = (self.total_len / n) or 1.0
        scores: dict[str, float] = defaultdict(float)
        for token in set(content_tokens(query)):
            posting = self.postings.get(token)
            if not posting:
                continue
            idf = math.log(1 + (n - len(posting) + 0.5) / (len(posting) + 0.5))
            for doc_id, tf in posting.items():
                length = self.lengths.get(doc_id, 0) or 1
                denom = tf + self.k1 * (1 - self.b + self.b * length / avg)
                scores[doc_id] += idf * (tf * (self.k1 + 1)) / denom
        return sorted(scores.items(), key=lambda kv: -kv[1])[:k]


class VectorIndex:
    def __init__(self, embedder=None, dim: int = DIM) -> None:
        self.embed = embedder or (lambda t: hashing_embed(t, dim))
        self.vectors: dict[str, list[float]] = {}

    def add(self, doc_id: str, text: str = "", vector: list[float] | None = None) -> None:
        self.vectors[doc_id] = vector if vector is not None else self.embed(text)

    def remove(self, doc_id: str) -> None:
        self.vectors.pop(doc_id, None)

    def search(self, query: str, k: int = 10, query_vec: list[float] | None = None):
        qv = query_vec if query_vec is not None else self.embed(query)
        scored = [(doc_id, cosine(qv, vec)) for doc_id, vec in self.vectors.items()]
        scored = [s for s in scored if s[1] > 0.0]
        return sorted(scored, key=lambda kv: -kv[1])[:k]


class HybridIndex:
    """Lexical + vector, score-normalised and blended.

    Namespaces are kept separate (`node` vs `span`) because the planner treats
    a node hit as a seed it can expand from and a span hit as raw material it
    must first ground into a node.
    """

    def __init__(self, embedder=None, dim: int = DIM, lexical_weight: float = 0.55) -> None:
        self.lexical = {"node": LexicalIndex(), "span": LexicalIndex()}
        self.vector = {"node": VectorIndex(embedder, dim), "span": VectorIndex(embedder, dim)}
        self.lexical_weight = lexical_weight

    def add_node(self, node_id: str, text: str, vector: list[float] | None = None) -> None:
        self.lexical["node"].add(node_id, text)
        self.vector["node"].add(node_id, text, vector)

    def add_span(self, span_id: str, text: str) -> None:
        self.lexical["span"].add(span_id, text)

    def add_span_vector(self, span_id: str, text: str) -> None:
        """Opt-in eager L2 over raw chunks; off by default (laziness rule)."""
        self.vector["span"].add(span_id, text)

    def remove_node(self, node_id: str) -> None:
        self.lexical["node"].remove(node_id)
        self.vector["node"].remove(node_id)

    def search(
        self, query: str, k: int = 10, namespace: str = "node", query_vec=None
    ) -> list[tuple[str, float]]:
        lex = self.lexical[namespace].search(query, k * 3)
        vec = self.vector[namespace].search(query, k * 3, query_vec=query_vec)
        return self._blend(lex, vec, k)

    def _blend(self, lex, vec, k):
        combined: dict[str, float] = defaultdict(float)
        if lex:
            top = max(s for _, s in lex) or 1.0
            for doc_id, score in lex:
                combined[doc_id] += self.lexical_weight * (score / top)
        if vec:
            top = max(s for _, s in vec) or 1.0
            for doc_id, score in vec:
                combined[doc_id] += (1 - self.lexical_weight) * (score / top)
        return sorted(combined.items(), key=lambda kv: -kv[1])[:k]

    def stats(self) -> dict:
        return {
            "nodes_indexed": len(self.lexical["node"].lengths),
            "spans_indexed": len(self.lexical["span"].lengths),
            "node_vectors": len(self.vector["node"].vectors),
            "span_vectors": len(self.vector["span"].vectors),
        }
