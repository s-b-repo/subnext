"""L2 vectors without a model dependency.

A hashing embedder (the "hashing trick") is enough for the runtime's actual
requirement: making state nodes *findable by meaning-ish similarity* so the
planner has seeds to expand from. It is deterministic, dependency-free and
instant, which keeps the whole runtime runnable offline. Swap in real
embeddings by passing any callable of `str -> list[float]` to the Ladder.
"""

from __future__ import annotations

import math
import re
from collections import Counter

TOKEN_RE = re.compile(r"[A-Za-z0-9_.:/-]+")
DIM = 256

STOPWORDS = frozenset(
    """a an and are as at be by for from has have in is it its of on or that the
    to was were will with what which who whom this these those do does did not
    you your we our they their he she his her i me my but if then than so""".split()
)

INSTRUCTION_WORDS = frozenset(
    """quote exactly exact verbatim literal tell show give me please line text
    word words message wording""".split()
)
"""Words that describe the *form* of an answer rather than its content.
Scoring on them drags every query toward whichever line happens to contain
"message" or "exactly", so they are stripped alongside stopwords."""

SKIP = STOPWORDS | INSTRUCTION_WORDS


def tokenize(text: str) -> list[str]:
    return [t.lower() for t in TOKEN_RE.findall(text or "")]


def content_tokens(text: str) -> list[str]:
    return [t for t in tokenize(text) if t not in SKIP and len(t) > 1]


def hashing_embed(text: str, dim: int = DIM) -> list[float]:
    """Sub-word + word hashing embedding, L2-normalised."""
    vec = [0.0] * dim
    tokens = content_tokens(text)
    if not tokens:
        return vec
    counts = Counter(tokens)
    for token, count in counts.items():
        weight = 1.0 + math.log(count)
        vec[hash_index(token, dim)] += weight
        # Character trigrams give partial credit for morphological variants
        # and typos, which plain word hashing cannot do.
        for i in range(len(token) - 2):
            vec[hash_index(token[i : i + 3], dim)] += 0.35 * weight
    norm = math.sqrt(sum(v * v for v in vec))
    if norm:
        vec = [v / norm for v in vec]
    return vec


def hash_index(token: str, dim: int = DIM) -> int:
    # Python's builtin hash() is salted per process; use a stable digest so
    # persisted vectors stay comparable across runs.
    h = 2166136261
    for ch in token.encode("utf-8"):
        h = ((h ^ ch) * 16777619) & 0xFFFFFFFF
    return h % dim


def cosine(a: list[float], b: list[float]) -> float:
    if not a or not b:
        return 0.0
    return sum(x * y for x, y in zip(a, b))
