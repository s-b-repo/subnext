"""L1 — chunk summaries.

Deliberately extractive by default. An abstractive summary is a second place a
hallucination can enter, and the ladder's whole safety story is that L1 is a
lossy *view* of L0 that can always be escalated back to exact bytes. Extractive
summaries keep that property: every character of L1 exists verbatim in L0.
"""

from __future__ import annotations

import re
from collections import Counter
from typing import Callable

from .embed import content_tokens

Summarizer = Callable[[str], str]

_SENTENCE = re.compile(r"(?<=[.!?])\s+|\n")


class ExtractiveSummarizer:
    """Pick the highest term-weight sentences, capped at `max_chars`."""

    def __init__(self, max_chars: int = 200) -> None:
        self.max_chars = max_chars

    def __call__(self, text: str) -> str:
        text = (text or "").strip()
        if len(text) <= self.max_chars:
            return text
        sentences = [s.strip() for s in _SENTENCE.split(text) if s.strip()]
        if not sentences:
            return text[: self.max_chars]
        freq = Counter(content_tokens(text))
        scored = []
        for i, sentence in enumerate(sentences):
            tokens = content_tokens(sentence)
            if not tokens:
                continue
            score = sum(freq[t] for t in tokens) / (len(tokens) ** 0.5)
            # Leading sentences carry disproportionate signal in logs and prose.
            score *= 1.0 + (0.3 if i == 0 else 0.0)
            scored.append((score, i, sentence))
        scored.sort(key=lambda x: (-x[0], x[1]))
        chosen: list[tuple[int, str]] = []
        used = 0
        for score, i, sentence in scored:
            if used + len(sentence) > self.max_chars and chosen:
                break
            chosen.append((i, sentence))
            used += len(sentence) + 1
        chosen.sort()
        summary = " ".join(s for _, s in chosen)
        return summary[: self.max_chars].rstrip()


class LLMSummarizer:
    """Abstractive L1 via a model. Costs a call; loses the verbatim property."""

    def __init__(self, llm, max_chars: int = 200) -> None:
        self.llm = llm
        self.max_chars = max_chars

    def __call__(self, text: str) -> str:
        text = (text or "").strip()
        if len(text) <= self.max_chars:
            return text
        prompt = (
            f"Compress the passage below to at most {self.max_chars} characters. "
            "Keep identifiers, numbers and error strings exactly as written. "
            "Reply with the compression only.\n\n" + text
        )
        return self.llm.complete(prompt).strip()[: self.max_chars]
