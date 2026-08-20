"""Runtime decision policy — what to do with each piece of context.

The wiki's decision table, made executable. The hard part it leaves open is
`sufficient(L, q)`: there is no clean definition of the cheapest level that can
answer a query. This implements the practical proxy the spec recommends —
route by query type, then *measure* escalation and let the escalation rate be
the telemetry that catches a bad router — plus a de-escalation rule so a node
that keeps being read at L0 without yielding anything new collapses into a
cached fact.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from .ladder import L0, L1, L2, L3
from .nodes import CALCULATION, CONSTRAINT, DECISION, EVIDENCE, GOAL, OPEN_QUESTION, Node

QUOTE_EXACT = "quote_exact"
VALUE_LOOKUP = "value_lookup"
JUSTIFY = "justify"
RECOMPUTE = "recompute"
SUMMARIZE = "summarize"
OPEN = "open"

_PATTERNS = (
    (QUOTE_EXACT, re.compile(
        r"\b(quote|verbatim|exact(ly)?|word[- ]for[- ]word|literal|raw text|"
        r"exact wording|full text|transcript|as written|stack trace|"
        r"exact error|error message|log line)\b", re.I)),
    (RECOMPUTE, re.compile(
        # Deliberately narrow: a bare "how many" is a lookup far more often
        # than it is a computation, and mis-routing a lookup to L3 wastes an
        # execution and returns nothing.
        r"\b(recompute|recalculate|re-?run|compute|calculate|"
        r"(total|sum|average|cost) (of|for)|what.s the (cost|total|result)|"
        r"how much (does|did|would) .* cost)\b", re.I)),
    (JUSTIFY, re.compile(
        r"\b(why|justify|justification|explain|reason|because|on what basis|"
        r"how did (you|we)|what led to|audit|evidence for|prove|back(ing)? up)\b", re.I)),
    (SUMMARIZE, re.compile(r"\b(summar(y|ise|ize)|recap|overview|tl;?dr|gist)\b", re.I)),
    (VALUE_LOOKUP, re.compile(
        r"\b(what (is|was|are|were)|which|who|when|where|current|status of|value of|"
        r"how (much|many))\b", re.I)),
)

#: query type -> preferred admission level, cheapest sufficient first
ROUTING = {
    QUOTE_EXACT: (L0, L1, L2),
    VALUE_LOOKUP: (L2, L1, L0),
    JUSTIFY: (L2, L1, L0),
    RECOMPUTE: (L3, L2, L0),
    SUMMARIZE: (L1, L2, L0),
    OPEN: (L2, L1, L0),
}

#: kinds that are cheap, near-always relevant, and pinned into every turn
ALWAYS_ADMIT = (GOAL, CONSTRAINT)


@dataclass
class Decisions:
    """What the runtime decided about one node this turn — for telemetry
    and for `explain_plan()`."""

    node_id: str
    level: str | None
    admitted: bool
    prefetch: bool = False
    recompute: bool = False
    reason: str = ""


class DecisionPolicy:
    def __init__(
        self,
        deescalate_after: int = 3,
        escalation_confidence: float = 0.55,
    ) -> None:
        self.deescalate_after = deescalate_after
        self.escalation_confidence = escalation_confidence

    # -- routing -----------------------------------------------------------
    def classify(self, query: str) -> str:
        for qtype, pattern in _PATTERNS:
            if pattern.search(query or ""):
                return qtype
        return OPEN

    def preferred_level(self, node: Node, qtype: str, available: list[str]) -> str:
        """Cheapest level in the routing order that this node actually has."""
        for level in ROUTING.get(qtype, ROUTING[OPEN]):
            if level in available:
                if level == L0 and self._prefers_compact(node, qtype):
                    continue
                return level
        return available[0] if available else L2

    def _prefers_compact(self, node: Node, qtype: str) -> bool:
        # Never promote a settled, well-grounded claim to raw bytes just
        # because the router said L0 — level over-promotion is the failure
        # mode that destroys the cost model.
        return (
            qtype != QUOTE_EXACT
            and node.kind not in (EVIDENCE,)
            and node.confidence >= 0.9
        )

    def sufficient(self, level: str, qtype: str) -> bool:
        order = ROUTING.get(qtype, ROUTING[OPEN])
        return level in order[:2] if qtype != QUOTE_EXACT else level == L0

    def escalation_target(self, node: Node, current: str, available: list[str]) -> str | None:
        """Next richer level to try when the model reports insufficiency."""
        ladder_up = {L2: L1, L1: L0, L3: L0, L0: None}
        target = ladder_up.get(current)
        while target is not None and target not in available:
            target = ladder_up.get(target)
        return target

    def should_deescalate(self, node: Node) -> bool:
        """A node read at L0 repeatedly with nothing new extracted should be
        collapsed into a cached fact instead of re-quoted every turn."""
        l0_reads = node.meta.get("l0_reads", 0)
        yielded = node.meta.get("l0_yield", 0)
        return l0_reads >= self.deescalate_after and yielded == 0

    # -- the decision table ------------------------------------------------
    def decide(self, node: Node, qtype: str, available: list[str], *, in_plan: bool) -> Decisions:
        if node.status == "stale":
            return Decisions(node.node_id, None, False, recompute=True,
                             reason="stale — recompute or re-ground before admitting")
        if node.status == "superseded":
            return Decisions(node.node_id, None, False, reason="superseded")
        if not in_plan:
            return Decisions(node.node_id, None, False, reason="not referenced by current plan")
        level = self.preferred_level(node, qtype, available)
        reason = {
            L0: "query needs exact wording",
            L1: "referenced for gist only",
            L2: "compact state object is sufficient",
            L3: "answer is a function of known inputs",
        }[level]
        return Decisions(node.node_id, level, True, reason=reason)

    def pinned_kinds(self, qtype: str) -> tuple[str, ...]:
        if qtype == JUSTIFY:
            return ALWAYS_ADMIT + (DECISION,)
        if qtype == RECOMPUTE:
            return ALWAYS_ADMIT + (CALCULATION,)
        if qtype == OPEN:
            return ALWAYS_ADMIT + (OPEN_QUESTION,)
        return ALWAYS_ADMIT
