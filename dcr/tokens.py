"""Token accounting.

`B_attention` is only meaningful if costs are measured in the same unit the
model bills in. The default estimator is a cheap character heuristic so the
runtime works offline; swap in a real tokenizer (or the Anthropic
`messages.count_tokens` endpoint) by passing any callable of `str -> int`.
"""

from __future__ import annotations

from typing import Callable

TokenEstimator = Callable[[str], int]

CHARS_PER_TOKEN = 4


def estimate_tokens(text: str) -> int:
    if not text:
        return 0
    return max(1, -(-len(text) // CHARS_PER_TOKEN))


def make_count_tokens_estimator(client, model: str = "claude-opus-5") -> TokenEstimator:
    """Exact estimator backed by the Anthropic token-counting endpoint.

    Only worth using when budgets are tight enough that heuristic error
    matters; it costs a network round trip per call, so cache aggressively.
    """

    cache: dict[str, int] = {}

    def count(text: str) -> int:
        if text in cache:
            return cache[text]
        result = client.messages.count_tokens(
            model=model, messages=[{"role": "user", "content": text or " "}]
        )
        cache[text] = result.input_tokens
        return result.input_tokens

    return count
