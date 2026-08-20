"""The dynamic memory graph — `M_t = (R_t, S_t, C_t, E_t)` as a typed graph.

Three invariants from the spec are enforced here rather than documented and
hoped for:

1. every non-evidence node reaches at least one evidence node via
   `dependencies` (unsourced facts are rejected at `upsert` time);
2. nothing is deleted — revision is `supersedes`, conflict is `contradicts`;
3. a `stale` node never enters the active context without revalidation.

Invalidation cascades are *bounded*. An unbounded cascade from one small
correction can mark a whole subgraph stale and force a full re-derivation,
which is open question #5 in the wiki; the implementation caps the eager
cascade and defers the rest to a lazy queue drained on read.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field

from .nodes import (
    CONTRADICTS,
    DERIVED_FROM,
    EDGE_TYPES,
    EVIDENCE,
    FRESH,
    STALE,
    SUPERSEDED,
    SUPERSEDES,
    SUPPORTS,
    Edge,
    Node,
)


class ProvenanceError(ValueError):
    """Raised when a node would enter the graph ungrounded."""


@dataclass
class ExplainPath:
    """The audit path returned by `explain()`."""

    root: str
    nodes: list[Node] = field(default_factory=list)
    edges: list[Edge] = field(default_factory=list)
    spans: list[str] = field(default_factory=list)
    complete: bool = True
    """False when the walk hit the depth cap before reaching evidence — the
    audit-path-completeness metric in the evaluation design."""
    truncated_at: list[str] = field(default_factory=list)

    def node_ids(self) -> list[str]:
        return [n.node_id for n in self.nodes]


class MemoryGraph:
    def __init__(self, raw, clock, max_cascade: int = 64, max_depth: int = 8) -> None:
        self.raw = raw
        self.clock = clock
        self.max_cascade = max_cascade
        self.max_depth = max_depth
        self.nodes: dict[str, Node] = {}
        self.edges: list[Edge] = []
        self._out: dict[str, list[Edge]] = {}
        self._in: dict[str, list[Edge]] = {}
        self._by_key: dict[str, list[str]] = {}
        self.version = 0
        """Bumped on every mutation. The planner records it on an assembled
        context so a mid-turn consolidation can be detected (open question
        #9 — Reasoner/Memory consistency)."""
        self.invalidated_since: dict[int, set[str]] = {}
        self.pending_invalidation: deque[str] = deque()
        """Deferred tail of a bounded cascade; drained lazily."""

    # -- mutation ----------------------------------------------------------
    def upsert(self, node: Node, *, validate: bool = True) -> Node:
        if validate:
            self._require_grounding(node)
        existing = self.nodes.get(node.node_id)
        node.timestamp = self.clock.tick()
        if existing is not None:
            node.reads = existing.reads
            node.admits = existing.admits
            changed = existing.fingerprint() != node.fingerprint()
            self.nodes[node.node_id] = node
            self._reindex_key(existing, node)
            if changed:
                self.invalidate(node.node_id, include_self=False)
        else:
            self.nodes[node.node_id] = node
            if node.key:
                self._by_key.setdefault(node.key, []).append(node.node_id)
        for dep in node.dependencies:
            if dep in self.nodes:
                edge_type = DERIVED_FROM if node.kind != "claim" else SUPPORTS
                self.add_edge(node.node_id, dep, edge_type)
                if node.status != SUPERSEDED:
                    self.nodes[dep].meta.pop("superseded_source", None)
        self._bump()
        return node

    def _require_grounding(self, node: Node) -> None:
        if node.kind == EVIDENCE:
            if not node.source_spans:
                raise ProvenanceError("evidence node must carry at least one source span")
            missing = [s for s in node.source_spans if s not in self.raw.spans]
            if missing:
                raise ProvenanceError(f"unknown source spans: {missing}")
            return
        if not node.is_grounded:
            raise ProvenanceError(
                f"{node.kind} {node.node_id} has neither source spans nor dependencies; "
                "a cached fact with no provenance is a hallucination with a database row"
            )
        missing = [s for s in node.source_spans if s not in self.raw.spans]
        if missing:
            raise ProvenanceError(f"unknown source spans: {missing}")
        if not node.source_spans:
            # Must reach evidence transitively through dependencies.
            if not any(self._reaches_evidence(dep) for dep in node.dependencies):
                raise ProvenanceError(
                    f"{node.kind} {node.node_id} does not reach any evidence node"
                )
        node.meta["grounded"] = True

    def _reaches_evidence(self, node_id: str) -> bool:
        """O(1) via the grounding flag stamped at insert time.

        Grounding is inductive — a node is grounded if any dependency is —
        so it does not need a depth-limited walk, and must not have one: a
        legitimately deep derivation chain would be rejected at the cap.
        """
        node = self.nodes.get(node_id)
        if node is None:
            return False
        return bool(node.kind == EVIDENCE or node.source_spans or node.meta.get("grounded"))

    def add_edge(self, src: str, dst: str, edge_type: str) -> Edge:
        if edge_type not in EDGE_TYPES:
            raise ValueError(f"unknown edge type {edge_type!r}")
        edge = Edge(src, dst, edge_type)
        if edge in self._out.get(src, []):
            return edge
        self.edges.append(edge)
        self._out.setdefault(src, []).append(edge)
        self._in.setdefault(dst, []).append(edge)
        self._bump()
        return edge

    def supersede(self, old_id: str, new_id: str) -> None:
        """Revision without deletion: the old node stays, marked superseded."""
        old, new = self.nodes[old_id], self.nodes[new_id]
        self.add_edge(new_id, old_id, SUPERSEDES)
        old.status = SUPERSEDED
        old.meta["superseded_by"] = new_id
        new.meta.setdefault("supersedes", []).append(old_id)
        # Supersession *resolves* the contradiction, so the survivor stops
        # carrying the warning; the edge and the loser both stay for the audit.
        for marks in (new.meta.get("contradicts"), old.meta.get("contradicts")):
            if marks and old_id in marks:
                marks.remove(old_id)
        self.invalidate(old_id, include_self=False)
        self._mark_orphaned_evidence(old)
        self._bump()

    def _mark_orphaned_evidence(self, node: Node) -> None:
        """Flag evidence whose every live dependent has been superseded.

        The span itself stays true as a record — the log really did say
        10.0.4.12 at the time — but admitting it next to the corrected fact is
        how a "solved" contradiction walks back into the window. Flagged
        evidence is annotated when rendered and penalised by the planner.
        """
        for dep in node.dependencies:
            evidence = self.nodes.get(dep)
            if evidence is None or evidence.kind != EVIDENCE:
                continue
            dependents = [
                self.nodes[e.src] for e in self._in.get(dep, [])
                if e.type in (SUPPORTS, DERIVED_FROM) and e.src in self.nodes
            ]
            live = [d for d in dependents if d.status != SUPERSEDED]
            evidence.meta["superseded_source"] = not live
            # Partial case: the span still supports live facts *and* carries a
            # value that was later corrected. Hiding it would break the audit
            # log, so annotate it with where the current value lives instead.
            corrected = node.meta.get("superseded_by")
            if corrected:
                pointers = evidence.meta.setdefault("corrected_by", [])
                if corrected not in pointers:
                    pointers.append(corrected)

    def contradict(self, a_id: str, b_id: str) -> None:
        """Both sides retained with their evidence so the model can adjudicate."""
        self.add_edge(a_id, b_id, CONTRADICTS)
        self.add_edge(b_id, a_id, CONTRADICTS)
        # Stamped on the node so the rendered state object carries the warning.
        # Two conflicting facts in the window without a marker is worse than
        # either one alone — the model has no way to know they disagree.
        for src, dst in ((a_id, b_id), (b_id, a_id)):
            node = self.nodes.get(src)
            if node is None:
                continue
            marks = node.meta.setdefault("contradicts", [])
            if dst not in marks:
                marks.append(dst)

    def invalidate(self, node_id: str, *, include_self: bool = True) -> set[str]:
        """Mark a node and its transitive dependents stale, cascade-bounded."""
        marked: set[str] = set()
        queue: deque[tuple[str, int]] = deque()
        if include_self:
            queue.append((node_id, 0))
        else:
            for edge in self._in.get(node_id, []):
                if edge.type in (SUPPORTS, DERIVED_FROM):
                    queue.append((edge.src, 1))
        while queue:
            current, depth = queue.popleft()
            if current in marked:
                continue
            if len(marked) >= self.max_cascade or depth > self.max_depth:
                # Bounded cascade: the tail is revalidated lazily on read
                # instead of eagerly invalidating an arbitrarily large subgraph.
                self.pending_invalidation.append(current)
                continue
            node = self.nodes.get(current)
            if node is None or node.status == SUPERSEDED:
                continue
            if node.kind != EVIDENCE:
                node.status = STALE
                node.level_cache.pop("L3", None)
            marked.add(current)
            for edge in self._in.get(current, []):
                if edge.type in (SUPPORTS, DERIVED_FROM):
                    queue.append((edge.src, depth + 1))
        if marked:
            self.invalidated_since.setdefault(self.version, set()).update(marked)
            self._bump()
        return marked

    def drain_pending(self, limit: int = 32) -> set[str]:
        """Resume a deferred cascade tail. Called before planning."""
        marked: set[str] = set()
        for _ in range(min(limit, len(self.pending_invalidation))):
            marked |= self.invalidate(self.pending_invalidation.popleft())
        return marked

    def revalidate(self, node_id: str) -> Node:
        """Clear staleness once the node has been re-grounded or recomputed."""
        node = self.nodes[node_id]
        node.status = FRESH
        node.timestamp = self.clock.tick()
        self._bump()
        return node

    def _bump(self) -> None:
        self.version += 1

    def _reindex_key(self, old: Node, new: Node) -> None:
        if old.key == new.key:
            return
        if old.key:
            ids = self._by_key.get(old.key, [])
            if old.node_id in ids:
                ids.remove(old.node_id)
        if new.key:
            self._by_key.setdefault(new.key, []).append(new.node_id)

    # -- reads -------------------------------------------------------------
    def get(self, node_id: str) -> Node | None:
        return self.nodes.get(node_id)

    def neighbors(self, node_id: str, edge_type: str | None = None, direction: str = "out"):
        table = self._out if direction == "out" else self._in
        edges = table.get(node_id, [])
        if edge_type:
            edges = [e for e in edges if e.type == edge_type]
        other = (lambda e: e.dst) if direction == "out" else (lambda e: e.src)
        return [self.nodes[other(e)] for e in edges if other(e) in self.nodes]

    def by_key(self, key: str, *, fresh_only: bool = True) -> list[Node]:
        nodes = [self.nodes[i] for i in self._by_key.get(key, []) if i in self.nodes]
        if fresh_only:
            nodes = [n for n in nodes if n.status != SUPERSEDED]
        return sorted(nodes, key=lambda n: n.timestamp)

    def by_kind(self, kind: str, *, fresh_only: bool = True) -> list[Node]:
        nodes = [n for n in self.nodes.values() if n.kind == kind]
        if fresh_only:
            nodes = [n for n in nodes if n.status == FRESH]
        return nodes

    def active(self) -> list[Node]:
        return [n for n in self.nodes.values() if n.status != SUPERSEDED]

    def explain(self, node_id: str, max_depth: int | None = None) -> ExplainPath:
        """Transitive closure down to evidence — the audit path."""
        max_depth = self.max_depth if max_depth is None else max_depth
        path = ExplainPath(root=node_id)
        seen: set[str] = set()
        queue: deque[tuple[str, int]] = deque([(node_id, 0)])
        reached_evidence = False
        while queue:
            current, depth = queue.popleft()
            if current in seen:
                continue
            node = self.nodes.get(current)
            if node is None:
                continue
            seen.add(current)
            path.nodes.append(node)
            path.spans.extend(s for s in node.source_spans if s not in path.spans)
            if node.kind == EVIDENCE or node.source_spans:
                reached_evidence = True
            if depth >= max_depth:
                if node.dependencies:
                    path.complete = False
                    path.truncated_at.append(current)
                continue
            for edge in self._out.get(current, []):
                if edge.type in (SUPPORTS, DERIVED_FROM):
                    path.edges.append(edge)
                    queue.append((edge.dst, depth + 1))
        path.complete = path.complete and reached_evidence
        return path

    def stats(self) -> dict:
        counts: dict[str, int] = {}
        for node in self.nodes.values():
            counts[node.kind] = counts.get(node.kind, 0) + 1
        return {
            "nodes": len(self.nodes),
            "edges": len(self.edges),
            "by_kind": counts,
            "stale": sum(1 for n in self.nodes.values() if n.status == STALE),
            "superseded": sum(1 for n in self.nodes.values() if n.status == SUPERSEDED),
            "version": self.version,
            "pending_invalidation": len(self.pending_invalidation),
        }

    # -- persistence -------------------------------------------------------
    def to_dict(self) -> dict:
        return {
            "nodes": [n.to_dict() for n in self.nodes.values()],
            "edges": [e.to_dict() for e in self.edges],
            "version": self.version,
        }

    def load_dict(self, data: dict) -> None:
        for nd in data["nodes"]:
            node = Node.from_dict(nd)
            self.nodes[node.node_id] = node
            if node.key:
                self._by_key.setdefault(node.key, []).append(node.node_id)
        for ed in data["edges"]:
            edge = Edge(ed["src"], ed["dst"], ed["type"])
            self.edges.append(edge)
            self._out.setdefault(edge.src, []).append(edge)
            self._in.setdefault(edge.dst, []).append(edge)
        self.version = data.get("version", 0)
