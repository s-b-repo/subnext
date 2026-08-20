"""L0 — the immutable raw store.

Every byte that enters the runtime is addressed by a stable span id before
anything else happens. Spans are the ground truth every claim, calculation and
decision must eventually point back at; nothing here is ever mutated or
deleted. Corrections arrive as new spans plus a `supersedes` edge in the graph
(see `dcr.graph`), which is what keeps the transcript a valid audit log.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Iterable, Iterator

from .ids import make_id

_PARAGRAPH = re.compile(r"\n\s*\n")
_SENTENCE_END = re.compile(r"(?<=[.!?])\s+")


@dataclass(frozen=True)
class Span:
    """An immutable, addressable byte range in a document."""

    span_id: str
    doc_id: str
    start: int
    end: int
    seq: int
    ts: int

    @property
    def length(self) -> int:
        return self.end - self.start


@dataclass
class Document:
    doc_id: str
    text: str
    ts: int
    meta: dict = field(default_factory=dict)


class RawStore:
    """Append-only store of documents and the spans addressing them."""

    def __init__(self, clock, max_span_chars: int = 600) -> None:
        self.clock = clock
        self.max_span_chars = max_span_chars
        self.documents: dict[str, Document] = {}
        self.spans: dict[str, Span] = {}
        self._doc_spans: dict[str, list[str]] = {}

    # -- ingestion ---------------------------------------------------------
    def add_document(
        self, text: str, doc_id: str | None = None, meta: dict | None = None
    ) -> list[Span]:
        doc_id = doc_id or make_id("doc", text)
        if doc_id in self.documents:
            # Append-only: re-ingesting identical material is a no-op, and a
            # *different* body under a used id is a programming error rather
            # than an edit.
            if self.documents[doc_id].text != text:
                raise ValueError(
                    f"document {doc_id} already exists with different content; "
                    "L0 is immutable — ingest a new document and supersede"
                )
            return self.spans_for(doc_id)

        ts = self.clock.tick()
        self.documents[doc_id] = Document(doc_id, text, ts, dict(meta or {}))
        spans: list[Span] = []
        for seq, (start, end) in enumerate(self._chunk(text)):
            span_id = make_id("s", doc_id, start, end)
            span = Span(span_id, doc_id, start, end, seq, ts)
            self.spans[span_id] = span
            spans.append(span)
        self._doc_spans[doc_id] = [s.span_id for s in spans]
        return spans

    def _chunk(self, text: str) -> Iterator[tuple[int, int]]:
        """Split into paragraph-ish spans, hard-wrapped at sentence bounds."""
        for para_start, para_end in self._paragraphs(text):
            block = text[para_start:para_end]
            if len(block) <= self.max_span_chars:
                if block.strip():
                    yield para_start, para_end
                continue
            cursor = 0
            buf_start = 0
            for piece in _SENTENCE_END.split(block):
                piece_len = len(piece)
                if cursor - buf_start + piece_len > self.max_span_chars and cursor > buf_start:
                    yield para_start + buf_start, para_start + cursor
                    buf_start = cursor
                cursor += piece_len + 1
            if buf_start < len(block):
                yield para_start + buf_start, para_end

    @staticmethod
    def _paragraphs(text: str) -> Iterator[tuple[int, int]]:
        pos = 0
        for match in _PARAGRAPH.finditer(text):
            if match.start() > pos:
                yield pos, match.start()
            pos = match.end()
        if pos < len(text):
            yield pos, len(text)

    # -- reads -------------------------------------------------------------
    def get(self, span_id: str) -> Span:
        return self.spans[span_id]

    def text(self, span_ids: str | Iterable[str], joiner: str = "\n") -> str:
        if isinstance(span_ids, str):
            span_ids = [span_ids]
        out = []
        for sid in span_ids:
            span = self.spans.get(sid)
            if span is None:
                continue
            out.append(self.documents[span.doc_id].text[span.start : span.end].strip())
        return joiner.join(out)

    def spans_for(self, doc_id: str) -> list[Span]:
        return [self.spans[s] for s in self._doc_spans.get(doc_id, [])]

    def neighbours(self, span_id: str, radius: int = 1) -> list[Span]:
        """Adjacent spans — used when an exact quote needs its surroundings."""
        span = self.spans[span_id]
        siblings = self._doc_spans[span.doc_id]
        lo = max(0, span.seq - radius)
        hi = min(len(siblings), span.seq + radius + 1)
        return [self.spans[s] for s in siblings[lo:hi]]

    def total_chars(self) -> int:
        return sum(len(d.text) for d in self.documents.values())

    def __len__(self) -> int:
        return len(self.spans)

    # -- persistence -------------------------------------------------------
    def to_dict(self) -> dict:
        return {
            "documents": [
                {"doc_id": d.doc_id, "text": d.text, "ts": d.ts, "meta": d.meta}
                for d in self.documents.values()
            ],
            "spans": [
                {
                    "span_id": s.span_id,
                    "doc_id": s.doc_id,
                    "start": s.start,
                    "end": s.end,
                    "seq": s.seq,
                    "ts": s.ts,
                }
                for s in self.spans.values()
            ],
        }

    @classmethod
    def from_dict(cls, data: dict, clock, max_span_chars: int = 600) -> "RawStore":
        store = cls(clock, max_span_chars)
        for d in data["documents"]:
            store.documents[d["doc_id"]] = Document(d["doc_id"], d["text"], d["ts"], d["meta"])
            store._doc_spans[d["doc_id"]] = []
        for s in data["spans"]:
            span = Span(s["span_id"], s["doc_id"], s["start"], s["end"], s["seq"], s["ts"])
            store.spans[span.span_id] = span
            store._doc_spans.setdefault(span.doc_id, []).append(span.span_id)
        for doc_id, ids in store._doc_spans.items():
            ids.sort(key=lambda i: store.spans[i].seq)
        return store
