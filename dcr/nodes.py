"""Typed state nodes — `S_t`, `C_t` and the leaves of `E_t`.

This is the schema the wiki freezes in architecture/memory-graph.md. The one
rule worth restating here: `source_spans` and `dependencies` are not metadata,
they are what make the node admissible at all. A cached fact with no spans is a
hallucination with a database row.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from .ids import make_id

EVIDENCE = "evidence"
CLAIM = "claim"
CALCULATION = "calculation"
DECISION = "decision"
GOAL = "goal"
CONSTRAINT = "constraint"
OPEN_QUESTION = "open_question"

KINDS = (EVIDENCE, CLAIM, CALCULATION, DECISION, GOAL, CONSTRAINT, OPEN_QUESTION)

FRESH = "fresh"
STALE = "stale"
SUPERSEDED = "superseded"

SUPPORTS = "supports"
DERIVED_FROM = "derived-from"
CONTRADICTS = "contradicts"
SUPERSEDES = "supersedes"

EDGE_TYPES = (SUPPORTS, DERIVED_FROM, CONTRADICTS, SUPERSEDES)


@dataclass
class Node:
    node_id: str
    kind: str
    value: str
    source_spans: tuple[str, ...] = ()
    dependencies: tuple[str, ...] = ()
    confidence: float = 1.0
    timestamp: int = 0
    status: str = FRESH
    key: str | None = None
    """Subject key (e.g. `server.ip`). Two fresh claims sharing a key and
    disagreeing on value are a contradiction the indexer must flag."""
    level_cache: dict[str, Any] = field(default_factory=dict)
    meta: dict[str, Any] = field(default_factory=dict)
    reads: int = 0
    """How often this node was actually admitted *and* read through. Feeds
    `U(x)` and the de-escalation rule."""
    admits: int = 0

    def fingerprint(self) -> str:
        """Identity of the node's *content* — changes when the value or its
        inputs change, which is what invalidates memoised derivations."""
        return make_id(
            "fp", self.kind, self.value, ",".join(sorted(self.dependencies)),
            ",".join(sorted(self.source_spans)),
        )

    @property
    def is_grounded(self) -> bool:
        return bool(self.source_spans) or bool(self.dependencies)

    def label(self) -> str:
        return f"{self.kind}:{self.key}" if self.key else self.kind

    def to_dict(self) -> dict:
        return {
            "node_id": self.node_id,
            "kind": self.kind,
            "value": self.value,
            "source_spans": list(self.source_spans),
            "dependencies": list(self.dependencies),
            "confidence": self.confidence,
            "timestamp": self.timestamp,
            "status": self.status,
            "key": self.key,
            "level_cache": {k: v for k, v in self.level_cache.items() if k != "L2"},
            "meta": self.meta,
            "reads": self.reads,
            "admits": self.admits,
        }

    @classmethod
    def from_dict(cls, data: dict) -> "Node":
        return cls(
            node_id=data["node_id"],
            kind=data["kind"],
            value=data["value"],
            source_spans=tuple(data.get("source_spans", ())),
            dependencies=tuple(data.get("dependencies", ())),
            confidence=data.get("confidence", 1.0),
            timestamp=data.get("timestamp", 0),
            status=data.get("status", FRESH),
            key=data.get("key"),
            level_cache=dict(data.get("level_cache", {})),
            meta=dict(data.get("meta", {})),
            reads=data.get("reads", 0),
            admits=data.get("admits", 0),
        )


@dataclass(frozen=True)
class Edge:
    src: str
    dst: str
    type: str

    def to_dict(self) -> dict:
        return {"src": self.src, "dst": self.dst, "type": self.type}


def new_node(
    kind: str,
    value: str,
    *,
    source_spans: tuple[str, ...] | list[str] = (),
    dependencies: tuple[str, ...] | list[str] = (),
    confidence: float = 1.0,
    timestamp: int = 0,
    key: str | None = None,
    meta: dict | None = None,
    node_id: str | None = None,
) -> Node:
    if kind not in KINDS:
        raise ValueError(f"unknown node kind {kind!r}")
    source_spans = tuple(source_spans)
    dependencies = tuple(dependencies)
    # The value is part of the identity: two claims sharing a key but
    # disagreeing on the value are *different nodes* that contradict each
    # other, not one node overwriting the other. Collapsing them would delete
    # the very history `supersedes` exists to preserve.
    node_id = node_id or make_id(kind[:4], kind, key or "", value, ",".join(source_spans))
    return Node(
        node_id=node_id,
        kind=kind,
        value=value,
        source_spans=source_spans,
        dependencies=dependencies,
        confidence=confidence,
        timestamp=timestamp,
        key=key,
        meta=dict(meta or {}),
    )
