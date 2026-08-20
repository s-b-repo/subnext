"""State Indexer — the entry point.

    index(event) -> { spans, nodes, stale }

Ingestion is where the runtime earns the right to throw the transcript away.
Three things happen, in this order:

1. **Span addressing (L0), eagerly.** Everything else is lazy.
2. **Node extraction.** Candidate claims/decisions/goals/constraints, each
   carrying the spans it came from. Pluggable: a regex extractor that needs no
   model, or an LLM extractor for prose.
3. **Contradiction detection.** A new claim that disagrees with a live claim on
   the same key is *not* silently written over the old one — both are kept,
   linked by `contradicts`, and the newer one supersedes the older, which marks
   dependents stale.

Step 3 is the part that actually fixes context rot rather than hiding it: the
stale set is what stops a corrected fact from living on in the cache.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

from .nodes import (
    CLAIM,
    CONSTRAINT,
    DECISION,
    EVIDENCE,
    GOAL,
    OPEN_QUESTION,
    Node,
    new_node,
)


@dataclass
class IndexResult:
    spans: list = field(default_factory=list)
    nodes: list = field(default_factory=list)
    stale: set = field(default_factory=set)
    contradictions: list = field(default_factory=list)
    superseded: list = field(default_factory=list)

    def summary(self) -> str:
        return (
            f"{len(self.spans)} spans, {len(self.nodes)} nodes, "
            f"{len(self.contradictions)} contradictions, {len(self.stale)} stale"
        )


# -- extraction ------------------------------------------------------------

_ASSIGN = re.compile(
    # Left boundary keeps the key from swallowing the preceding sentence, and
    # lets mid-sentence corrections ("actually the server ip is …") match too.
    r"(?:^|[,;(]\s*|\bthe\s+|\bactually\s+|\bnow\s+|\bcurrent\s+)"
    r"(?P<key>[A-Za-z][\w-]*(?:[ .][A-Za-z][\w-]*){0,2}?)\s*"
    # Multi-word transitions first: corrections in the wild carry verbs, and
    # a copula-only pattern silently drops most of them. Longest-first so
    # "was replaced by" is not read as the copula "was". Phrasings whose
    # subject follows the verb ("we switched X to Y") are deliberately absent
    # — they need a different rule and guessing would over-extract.
    r"(?:=|:"
    r"|\bhas moved to\b|\bhave moved to\b|\bhas changed to\b|\bhave changed to\b"
    r"|\bwas replaced by\b|\bwere replaced by\b|\bis replaced by\b|\bwas changed to\b"
    r"|\bmoved to\b|\bchanged to\b|\bshould be\b"
    r"|\bis\b|\bwas\b|\bare\b|\bwere\b)\s+"
    # A value ends at a clause boundary, not at the first period — otherwise
    # every dotted identifier (10.0.4.12, api.v2.stats) is truncated.
    r"(?P<value>[^,;\n]{1,120}?)"
    r"(?=\s*(?:,|;|\band\b|\bbut\b|\.(?:\s|$)|$))",
    re.M,
)
# "the datastore was migrated and is now postgres-15" — the subject carries
# across the conjunction, so the value after "is now" is the live one and the
# participle is not a value at all. Deliberately tight: the copula must be
# present tense *and* followed by `now`, because "X is A and is B" is a real
# ambiguity and over-extraction is not a neutral failure here.
_RESTATEMENT = re.compile(
    r"[\s,;]*(?:and|but|then|so)?\s*(?:it\s+)?(?:is|are)\s+now\s+"
    r"(?P<value>[^,;\n]{1,120}?)"
    r"(?=\s*(?:,|;|\band\b|\bbut\b|\.(?:\s|$)|$))",
    re.I,
)
# A fronted temporal adverb is not part of the value: "is now X" carries X.
_LEADING_ADVERB = re.compile(r"^(?:now|currently|presently|already)\s+", re.I)

_DECISION = re.compile(
    r"^\s*(?:[-*•]\s*)?(?:decision|decided|we (?:will|decided to)|action|resolved)\b[:\s]+"
    r"(?P<value>.{3,160})", re.I | re.M)
_GOAL = re.compile(r"^\s*(?:[-*•]\s*)?(?:goal|objective|aim|target)\b[:\s]+(?P<value>.{3,160})",
                   re.I | re.M)
_CONSTRAINT = re.compile(
    r"^\s*(?:[-*•]\s*)?(?:constraint|requirement|must|never|do not|don't|limit)\b[:\s]+?"
    r"(?P<value>.{3,160})", re.I | re.M)
_QUESTION = re.compile(r"^\s*(?:[-*•]\s*)?(?P<value>[^\n]{8,160}\?)\s*$", re.M)
_CORRECTION = re.compile(
    r"\b(correction|actually|update|revised|no longer|instead|supersedes|scratch that)\b", re.I)

_NOISE_KEYS = frozenset(
    """it this that there here what who when where why how the a an note reply
    answer question thing something anything everyone someone i we you they he
    she and but so then goal objective aim target decision decided action
    resolved correction update constraint requirement summary recap tldr""".split()
)


class HeuristicExtractor:
    """Deterministic, model-free node extraction.

    Deliberately conservative: it proposes state only for shapes that carry an
    unambiguous subject (`key = value`, `Decision: …`). Over-extraction is not
    a neutral failure — a wrongly cached claim is worse than a long prompt
    (open question #2) — so anything it is unsure about stays as raw L0 and
    gets found by search instead.
    """

    def __init__(self, min_confidence: float = 0.55) -> None:
        self.min_confidence = min_confidence

    def __call__(self, text: str, span_id: str, span_text: str) -> list[dict]:
        found: list[dict] = []
        seen: set[tuple[str, str]] = set()
        corrective = bool(_CORRECTION.search(span_text))

        for match in _GOAL.finditer(span_text):
            found.append(self._make(GOAL, match.group("value"), span_id, 0.9))
        for match in _CONSTRAINT.finditer(span_text):
            found.append(self._make(CONSTRAINT, match.group("value"), span_id, 0.85))
        for match in _DECISION.finditer(span_text):
            found.append(self._make(DECISION, match.group("value"), span_id, 0.85))
        for match in _QUESTION.finditer(span_text):
            found.append(self._make(OPEN_QUESTION, match.group("value"), span_id, 0.7))

        for key, value in self._assignments(span_text):
            if (key, value) in seen:
                continue
            seen.add((key, value))
            confidence = 0.9 if corrective else 0.8
            entry = self._make(CLAIM, value, span_id, confidence)
            entry["key"] = key
            entry["corrective"] = corrective
            found.append(entry)

        return [f for f in found if f["confidence"] >= self.min_confidence]

    def _assignments(self, text: str, depth: int = 0) -> list[tuple[str, str]]:
        """Key/value pairs, descending once into structural prefixes.

        "Correction: the server ip is 10.0.9.7" parses first as
        `correction = the server ip is 10.0.9.7`. That key is noise, but its
        *value* holds the real assignment — so noise matches are re-scanned
        instead of discarded, which is how corrections get extracted at all.
        """
        out: list[tuple[str, str]] = []
        consumed_to = 0
        for match in _ASSIGN.finditer(text):
            if match.start() < consumed_to:
                continue
            key = self._normalise_key(match.group("key"))
            value = self._clean_value(match.group("value"))
            consumed_to = match.end()
            restated = _RESTATEMENT.match(text, match.end())
            if restated:
                value = self._clean_value(restated.group("value"))
                consumed_to = restated.end()
            if not key or not value or len(key.split()) > 4:
                continue
            if key in _NOISE_KEYS:
                if depth == 0:
                    # The value group stops at the conjunction, so descending
                    # into it alone would hand the inner parse "was migrated"
                    # and never "is now postgres-15". Descend into the wider
                    # slice the restatement consumed.
                    inner = text[match.start("value") : consumed_to]
                    out.extend(self._assignments(inner, depth + 1))
                continue
            out.append((key, value))
        return out

    @staticmethod
    def _clean_value(value: str) -> str:
        value = value.strip()
        # A quoted value is the value: `error was "connection refused" on 8080`
        quoted = re.match(r'^["\u201c](?P<inner>[^"\u201d]{1,120})["\u201d]', value)
        if quoted:
            return quoted.group("inner").strip()
        return _LEADING_ADVERB.sub("", value.strip('"\u201c\u201d').strip()).strip()

    @staticmethod
    def _normalise_key(key: str) -> str:
        key = key.strip().lower()
        key = re.sub(r"\s+", ".", key)
        key = re.sub(r"^(the|a|an|our|its|his|her|their)\.", "", key)
        # "the error was:" parses with the copula inside the key phrase; drop
        # it so `error.was` and `error` are not two different facts.
        key = re.sub(r"\.(was|is|are|were|will\.be)$", "", key)
        return key.strip(".")

    @staticmethod
    def _make(kind: str, value: str, span_id: str, confidence: float) -> dict:
        return {
            "kind": kind,
            "value": value.strip().rstrip(".").strip(),
            "span_id": span_id,
            "confidence": confidence,
        }


class StateIndexer:
    def __init__(
        self,
        raw,
        graph,
        index,
        ladder,
        extractor=None,
        *,
        supersede_on_conflict: bool = True,
        eager_span_vectors: bool = False,
    ) -> None:
        self.raw = raw
        self.graph = graph
        self.index = index
        self.ladder = ladder
        self.extractor = extractor or HeuristicExtractor()
        self.supersede_on_conflict = supersede_on_conflict
        self.eager_span_vectors = eager_span_vectors
        self.evidence_by_span: dict[str, str] = {}

    # -- the contract ------------------------------------------------------
    def ingest(self, text: str, doc_id: str | None = None, meta: dict | None = None) -> IndexResult:
        result = IndexResult()
        spans = self.raw.add_document(text, doc_id, meta)
        result.spans = spans
        for span in spans:
            span_text = self.raw.text(span.span_id)
            if not span_text.strip():
                continue
            self.index.add_span(span.span_id, span_text)
            if self.eager_span_vectors:
                self.index.add_span_vector(span.span_id, span_text)
            for candidate in self.extractor(text, span.span_id, span_text):
                node = self._admit(candidate, span.span_id, span_text, result)
                if node is not None:
                    result.nodes.append(node)
        return result

    def _admit(self, candidate: dict, span_id: str, span_text: str, result: IndexResult) -> Node | None:
        evidence = self.evidence_node(span_id, span_text)
        links = self._reference_links(candidate["value"], span_id)
        node = new_node(
            candidate["kind"],
            candidate["value"],
            source_spans=(span_id,),
            dependencies=(evidence.node_id,) + links,
            confidence=candidate["confidence"],
            key=candidate.get("key"),
            meta={"corrective": candidate.get("corrective", False)},
        )
        if node.node_id in self.graph.nodes:
            # Same claim, same span — already known. Bump nothing.
            return None
        twin = self._duplicate(node)
        if twin is not None:
            # The same fact restated elsewhere is corroboration, not a second
            # fact. Merging keeps the working set free of near-identical lines
            # and makes repetition raise confidence, which is what repetition
            # actually means.
            self._reinforce(twin, span_id, evidence)
            return None
        conflicts = self._conflicts(node)
        self.graph.upsert(node)
        self.index.add_node(node.node_id, self._index_text(node), self.ladder.vector(node))
        for other in conflicts:
            self.graph.contradict(node.node_id, other.node_id)
            result.contradictions.append((other.node_id, node.node_id))
            if self.supersede_on_conflict and self._may_supersede(other, node):
                self.graph.supersede(other.node_id, node.node_id)
                result.superseded.append(other.node_id)
                result.stale |= {
                    n for n in self.graph.invalidated_since.get(self.graph.version - 1, set())
                }
                self.index.remove_node(other.node_id)
        return node

    def _duplicate(self, node: Node) -> Node | None:
        """A live node of the same kind asserting the same thing."""
        pool = self.graph.by_key(node.key) if node.key else self.graph.by_kind(node.kind)
        for other in pool:
            if other.node_id == node.node_id or other.status != "fresh":
                continue
            if other.kind != node.kind:
                continue
            if node.key and other.key != node.key:
                continue
            if self._same_value(other.value, node.value):
                return other
        return None

    def _reinforce(self, node: Node, span_id: str, evidence: Node) -> None:
        if span_id not in node.source_spans:
            node.source_spans = node.source_spans + (span_id,)
        if evidence.node_id not in node.dependencies:
            node.dependencies = node.dependencies + (evidence.node_id,)
            self.graph.add_edge(node.node_id, evidence.node_id, "supports")
        node.confidence = min(0.97, node.confidence + 0.05)
        node.level_cache.pop("L1", None)
        node.level_cache.pop("L2", None)
        node.meta["corroborations"] = node.meta.get("corroborations", 1) + 1
        # Deliberately not routed through `upsert`: the value did not change,
        # so dependents must not be invalidated.
        self.reindex(node)

    def _reference_links(self, value: str, span_id: str, limit: int = 3) -> tuple[str, ...]:
        """Link a new node to the live facts it mentions by value.

        Without this the graph is a star — every node hanging off its own
        evidence span — and `explain()` on a decision stops one hop down.
        Matching on the *value* of an existing claim is conservative but real:
        "roll back because the blocker is firewall rule 37" genuinely depends
        on `blocker = firewall rule 37`, and the edge is what makes the
        justification walkable instead of re-derivable.
        """
        haystack = value.lower()
        matches: list[tuple[int, str]] = []
        for node in self.graph.nodes.values():
            if node.kind != CLAIM or node.status != "fresh" or not node.key:
                continue
            if span_id in node.source_spans:
                continue
            needle = node.value.lower().strip()
            if len(needle) < 4 or needle not in haystack:
                continue
            matches.append((len(needle), node.node_id))
        matches.sort(reverse=True)
        return tuple(node_id for _length, node_id in matches[:limit])

    @staticmethod
    def _may_supersede(existing: Node, incoming: Node) -> bool:
        """Should the newer claim replace the older one, or should both stand?

        Ingest order is event order, so last-writer-wins is the default. The
        exception is an explicitly corrective statement ("Correction: …",
        "actually …"): plain material arriving after one must not silently
        revert it, because ingest order is only *approximately* chronology —
        files read from a directory, transcripts merged from two sources. In
        that case both sides stay live, linked by `contradicts`, and the model
        adjudicates with the evidence in front of it.
        """
        return not (existing.meta.get("corrective") and not incoming.meta.get("corrective"))

    def _conflicts(self, node: Node) -> list[Node]:
        """Live claims on the same key that disagree on value."""
        if not node.key or node.kind != CLAIM:
            return []
        out = []
        for other in self.graph.by_key(node.key):
            if other.node_id == node.node_id or other.status == "superseded":
                continue
            if other.kind != CLAIM:
                continue
            if self._same_value(other.value, node.value):
                continue
            out.append(other)
        return out

    @staticmethod
    def _same_value(a: str, b: str) -> bool:
        norm = lambda s: re.sub(r"\W+", "", s or "").lower()
        return norm(a) == norm(b)

    def evidence_node(self, span_id: str, span_text: str | None = None) -> Node:
        """Evidence nodes are created lazily, one per referenced span."""
        existing = self.evidence_by_span.get(span_id)
        if existing and existing in self.graph.nodes:
            return self.graph.nodes[existing]
        span_text = span_text if span_text is not None else self.raw.text(span_id)
        node = new_node(EVIDENCE, span_text, source_spans=(span_id,), confidence=1.0)
        if node.node_id not in self.graph.nodes:
            self.graph.upsert(node)
            self.index.add_node(node.node_id, span_text, self.ladder.vector(node))
        self.evidence_by_span[span_id] = node.node_id
        return self.graph.nodes[node.node_id]

    def _index_text(self, node: Node) -> str:
        # Index the dotted key *and* its parts: `server.ip` is a single token
        # to the tokenizer, so a query for "server ip" would otherwise miss it.
        parts = [node.key or "", (node.key or "").replace(".", " "), node.value, node.kind]
        return " ".join(filter(None, parts))

    def reindex(self, node: Node) -> None:
        self.index.add_node(node.node_id, self._index_text(node), self.ladder.vector(node))
