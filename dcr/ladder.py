"""The representation ladder.

    L0 = raw bytes            exact, expensive
    L1 = chunk summary        lossy prose, cheap
    L2 = state object         the structured fact + provenance pointers
    L3 = executable derivation / computed result

L2 deserves a note, because the wiki describes it as "semantic/state vectors"
and a vector is not something a model reads. In this implementation the vector
is L2's *index key* and the structured state object is L2's *payload*: admitting
a node at L2 means the model sees `server.ip = 10.0.4.12 [conf 0.95, spans …]`
rather than the paragraph it came from. That is the level the fact cache is
built on, and it is why 10k tokens of transcript collapse to ~20 objects.

Higher levels are built on first demand and cached on the node
(`level_cache`), so ingesting a large document stays cheap until something
actually asks about it.
"""

from __future__ import annotations

from typing import Iterable

from .embed import DIM, cosine, hashing_embed
from .nodes import CALCULATION, EVIDENCE, Node
from .summarize import ExtractiveSummarizer
from .tokens import estimate_tokens

L0, L1, L2, L3 = "L0", "L1", "L2", "L3"
LEVELS = (L0, L1, L2, L3)
LEVEL_ORDER = {L2: 0, L1: 1, L3: 2, L0: 3}
"""Cost ordering used for demotion/escalation. L3 sits above L1 because a
computed result is usually compact but had to be *derived*; L0 is always the
most expensive admission."""


def cheaper(a: str, b: str) -> bool:
    return LEVEL_ORDER[a] < LEVEL_ORDER[b]


class Ladder:
    def __init__(
        self,
        raw,
        summarizer=None,
        embedder=None,
        estimator=estimate_tokens,
        executor=None,
        dim: int = DIM,
    ) -> None:
        self.raw = raw
        self.summarize = summarizer or ExtractiveSummarizer()
        self.embed = embedder or (lambda text: hashing_embed(text, dim))
        self.estimate = estimator
        self.executor = executor
        self.dim = dim
        self.builds: dict[str, int] = {L1: 0, L2: 0, L3: 0}

    # -- materialisation ---------------------------------------------------
    def raw_text(self, node: Node) -> str:
        if node.source_spans:
            return self.raw.text(node.source_spans)
        return node.value

    def render(self, node: Node, level: str) -> str:
        """The payload the model sees for this node at this level."""
        if level == L0:
            text = self.raw_text(node)
            return f'[{node.node_id} L0 {node.label()}] "{text}"'
        if level == L1:
            return f"[{node.node_id} L1 {node.label()}] {self.summary(node)}"
        if level == L2:
            return f"[{node.node_id} L2] {self.state_object(node)}"
        if level == L3:
            result = self.result(node)
            if result is None:
                return ""
            return f"[{node.node_id} L3 {node.label()}] {result}"
        raise ValueError(f"unknown level {level!r}")

    def summary(self, node: Node) -> str:
        cached = node.level_cache.get(L1)
        if cached is None:
            cached = self.summarize(self.raw_text(node))
            node.level_cache[L1] = cached
            self.builds[L1] += 1
        return cached

    def vector(self, node: Node) -> list[float]:
        cached = node.level_cache.get(L2)
        if cached is None:
            basis = " ".join(filter(None, [node.key or "", node.value, self.raw_text(node)[:400]]))
            cached = self.embed(basis)
            node.level_cache[L2] = cached
            self.builds[L2] += 1
        return cached

    def state_object(self, node: Node, max_value_chars: int = 120) -> str:
        """The compact structured form — the fact cache entry.

        The value is clipped: L2 is a *handle* on the material, and an
        evidence node whose value is a whole paragraph must still cost less at
        L2 than at L1, or demotion stops being a saving."""
        value = node.value
        if len(value) > max_value_chars:
            value = value[: max_value_chars - 1].rstrip() + "…"
        head = f"{node.key} = {value}" if node.key else value
        bits = [head]
        if node.confidence < 0.999:
            bits.append(f"conf={node.confidence:.2f}")
        if node.status != "fresh":
            bits.append(f"status={node.status}")
        if node.source_spans:
            bits.append("spans=" + ",".join(node.source_spans[:3]))
        elif node.dependencies:
            bits.append("via=" + ",".join(node.dependencies[:3]))
        live_conflicts = [
            c for c in node.meta.get("contradicts", [])
            if node.status == "fresh"
        ]
        if live_conflicts:
            bits.append("CONTRADICTS=" + ",".join(live_conflicts[:2]) + " — adjudicate, do not pick blindly")
        if node.meta.get("superseded_source"):
            bits.append("NOTE=source of a superseded fact; do not treat as current")
        elif node.meta.get("corrected_by"):
            bits.append(
                "NOTE=contains a value corrected later; current value in "
                + ",".join(node.meta["corrected_by"][:2])
            )
        return " · ".join(bits)

    def result(self, node: Node):
        cached = node.level_cache.get(L3)
        if cached is not None:
            return cached
        derivation = node.meta.get("derivation")
        if derivation and self.executor is not None:
            cached = self.executor.run(node)
            node.level_cache[L3] = cached
            self.builds[L3] += 1
            return cached
        if node.kind == CALCULATION:
            return node.value
        return None

    # -- costing -----------------------------------------------------------
    def cost(self, node: Node, level: str) -> int:
        text = self.render(node, level)
        return self.estimate(text) if text else 0

    def available(self, node: Node) -> list[str]:
        """Which levels this node can actually be admitted at."""
        levels = [L2]
        if node.source_spans:
            levels.append(L0)
            if self.estimate(self.raw_text(node)) > 40:
                levels.append(L1)
        if node.kind == CALCULATION or node.meta.get("derivation"):
            levels.append(L3)
        if node.kind == EVIDENCE and L0 not in levels:
            levels.append(L0)
        return sorted(set(levels), key=lambda l: LEVEL_ORDER[l])

    def similarity(self, node: Node, query_vec: list[float]) -> float:
        return cosine(self.vector(node), query_vec)

    def query_vector(self, query: str) -> list[float]:
        return self.embed(query)

    def prewarm(self, nodes: Iterable[Node], levels: Iterable[str] = (L1, L2)) -> int:
        """Speculative materialisation. Costs storage/compute budget, never
        attention budget."""
        built = 0
        for node in nodes:
            for level in levels:
                if level in node.level_cache:
                    continue
                if level == L1 and node.source_spans:
                    self.summary(node)
                    built += 1
                elif level == L2:
                    self.vector(node)
                    built += 1
                elif level == L3:
                    if self.result(node) is not None:
                        built += 1
        return built
