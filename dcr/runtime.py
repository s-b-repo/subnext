"""DCR — the runtime facade.

Wires the components into the per-turn data flow from the architecture doc:

    event → index → graph update → plan(k + r) → model → state update → prefetch

The class is deliberately small; all the interesting decisions live in the
components it composes. What it owns is the *loop*: escalation, consistency
between the Reasoner and the Memory Runtime, and the telemetry that says
whether any of this is working.
"""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass, field
from pathlib import Path

from .execute import ExecutionLayer
from .graph import MemoryGraph
from .ids import Clock
from .index import HybridIndex
from .indexer import HeuristicExtractor, IndexResult, StateIndexer
from .ladder import L0, Ladder
from .llm import ESCALATE_RE, REASONER_SYSTEM, LocalReasoner
from .nodes import CLAIM, STALE, Node, new_node
from .planner import RelevancePlanner, Weights
from .policy import DecisionPolicy
from .spans import RawStore
from .speculation import Predictor, Speculator
from .telemetry import Telemetry, Turn
from .tokens import estimate_tokens


class ConsistencyInterrupt(RuntimeError):
    """Solution B invalidated something Solution A was mid-way through using.

    The workspace is rebuilt rather than allowed to reason over a fact that no
    longer holds — snapshot isolation with an interrupt, per open question #9.
    """


@dataclass
class Answer:
    text: str
    context: object
    escalations: int = 0
    cited: list[str] = field(default_factory=list)
    tokens: int = 0
    replanned: bool = False

    def __str__(self) -> str:  # pragma: no cover - convenience
        return self.text


class DCR:
    def __init__(
        self,
        *,
        budget: int = 1200,
        reasoner=None,
        extractor=None,
        summarizer=None,
        embedder=None,
        weights: Weights | None = None,
        tau: float = 0.5,
        max_span_chars: int = 600,
        estimator=estimate_tokens,
        supersede_on_conflict: bool = True,
    ) -> None:
        self.clock = Clock()
        self.raw = RawStore(self.clock, max_span_chars=max_span_chars)
        self.graph = MemoryGraph(self.raw, self.clock)
        self.execution = ExecutionLayer(graph=self.graph)
        self.ladder = Ladder(
            self.raw, summarizer=summarizer, embedder=embedder,
            estimator=estimator, executor=self.execution,
        )
        self.index = HybridIndex(embedder=embedder)
        self.indexer = StateIndexer(
            self.raw, self.graph, self.index, self.ladder,
            extractor or HeuristicExtractor(),
            supersede_on_conflict=supersede_on_conflict,
        )
        self.policy = DecisionPolicy()
        self.speculator = Speculator(self.ladder, Predictor(), tau=tau)
        self.planner = RelevancePlanner(
            self.graph, self.index, self.ladder, self.policy,
            speculator=self.speculator, weights=weights, budget=budget,
            evidence_lookup=self.indexer.evidence_by_span.get,
        )
        self.telemetry = Telemetry()
        self.reasoner = reasoner or LocalReasoner()
        self.budget = budget

    # -- ingestion ---------------------------------------------------------
    def ingest(self, text: str, doc_id: str | None = None, meta: dict | None = None) -> IndexResult:
        result = self.indexer.ingest(text, doc_id, meta)
        self.telemetry.history_tokens += self.ladder.estimate(text)
        return result

    def ingest_file(self, path: str | os.PathLike) -> IndexResult:
        path = Path(path)
        return self.ingest(path.read_text(encoding="utf-8"), doc_id=path.name,
                           meta={"source": str(path)})

    def observe(self, text: str, kind: str = "tool_result", doc_id: str | None = None):
        """A tool result or subcall return is just another event to index."""
        return self.ingest(text, doc_id, meta={"event": kind})

    # -- planning and answering -------------------------------------------
    def plan(self, query: str, budget: int | None = None, **kwargs):
        return self.planner.plan(query, budget, **kwargs)

    def prompt_for(self, ctx) -> str:
        return f"{ctx.render()}\n\nQUESTION: {ctx.query}\n"

    def ask(
        self,
        query: str,
        budget: int | None = None,
        *,
        reasoner=None,
        max_escalations: int = 2,
    ) -> Answer:
        reasoner = reasoner or self.reasoner
        budget = self.budget if budget is None else budget
        force: dict[str, str] = {}
        pin: tuple[str, ...] = ()
        escalations = 0
        replanned = False
        consistency_retries = 0

        while True:
            ctx = self.plan(query, budget, pin=pin, force_level=force)
            text = reasoner.complete(self.prompt_for(ctx), system=REASONER_SYSTEM)
            # The Reasoner has been thinking; the Memory Runtime may have
            # consolidated underneath it. Snapshot isolation with an interrupt:
            # if anything in the working set was invalidated mid-turn, the
            # answer is discarded and the workspace rebuilt rather than
            # returned from state that no longer holds.
            try:
                self._check_consistency(ctx)
            except ConsistencyInterrupt:
                if consistency_retries < 1:
                    consistency_retries += 1
                    replanned = True
                    continue
            match = ESCALATE_RE.search(text or "")
            if match and escalations < max_escalations:
                node_id = match.group(1)
                if node_id in self.graph.nodes:
                    escalations += 1
                    force[node_id] = L0
                    pin = tuple(set(pin) | {node_id})
                    self.graph.nodes[node_id].meta["l0_yield"] = (
                        self.graph.nodes[node_id].meta.get("l0_yield", 0)
                    )
                    continue
            break

        cited = self._cited(text, ctx)
        for node_id in cited:
            node = self.graph.get(node_id)
            if node is not None:
                node.reads += 1
        stale_reads = sum(
            1 for entry in ctx.entries if entry.node.status == STALE
        )
        self.telemetry.record_turn(
            Turn(
                query=query, qtype=ctx.qtype, tokens=ctx.tokens, budget=ctx.budget,
                admitted=len(ctx.entries), considered=ctx.considered,
                escalations=escalations, stale_reads=stale_reads,
                demotions=len(ctx.demoted), overflow=ctx.overflow,
            )
        )
        self.speculator.feedback(set(cited))
        return Answer(text=text, context=ctx, escalations=escalations,
                      cited=cited, tokens=ctx.tokens, replanned=replanned)

    def _cited(self, text: str, ctx) -> list[str]:
        ids = set(ctx.node_ids())
        found = []
        for token in set(re.findall(r"\b([a-z]{3,4}_[0-9a-f]{12})\b", text or "")):
            if token in ids:
                found.append(token)
        return sorted(found)

    def _check_consistency(self, ctx) -> None:
        """Did background consolidation invalidate anything in this plan?"""
        touched: set[str] = set()
        for version, nodes in self.graph.invalidated_since.items():
            if version >= ctx.snapshot_version:
                touched |= nodes
        collision = touched & set(ctx.node_ids())
        if collision:
            raise ConsistencyInterrupt(
                f"{len(collision)} node(s) in the active context were invalidated mid-turn"
            )

    # -- state updates -----------------------------------------------------
    def commit_fact(
        self,
        value: str,
        *,
        key: str | None = None,
        source_spans=(),
        dependencies=(),
        confidence: float = 0.9,
        kind: str = CLAIM,
    ) -> Node:
        """Cache a settled conclusion. Rejected unless it is groundable."""
        node = new_node(
            kind, value, source_spans=tuple(source_spans), dependencies=tuple(dependencies),
            confidence=confidence, key=key,
        )
        conflicts = self.indexer._conflicts(node)
        self.graph.upsert(node)
        self.indexer.reindex(node)
        for other in conflicts:
            self.graph.contradict(node.node_id, other.node_id)
            if self.indexer.supersede_on_conflict:
                self.graph.supersede(other.node_id, node.node_id)
                self.index.remove_node(other.node_id)
        return node

    def compute(self, name: str, inputs: dict | None = None, deps=(), *, key: str | None = None) -> Node:
        """Run a registered derivation (L3) and cache it as a calculation node."""
        node = self.execution.compute_node(name, inputs, deps, key=key)
        self.graph.upsert(node)
        self.indexer.reindex(node)
        return node

    def register(self, name: str, fn):
        return self.execution.register(name, fn)

    def revalidate(self, node_id: str) -> Node:
        """Re-ground a stale node: recompute if derived, re-read spans if not."""
        node = self.graph.nodes[node_id]
        if node.meta.get("derivation"):
            value = self.execution.run(node)
            node.value = str(value)
        node.level_cache.pop("L1", None)
        node.level_cache.pop("L2", None)
        self.graph.revalidate(node_id)
        self.indexer.reindex(node)
        return node

    # -- audit -------------------------------------------------------------
    def explain(self, node_id: str, max_depth: int | None = None) -> str:
        path = self.graph.explain(node_id, max_depth)
        self.telemetry.record_explain(path.complete)
        lines = [f"explain({node_id})  complete={path.complete}"]
        depth_of = {node_id: 0}
        for edge in path.edges:
            depth_of.setdefault(edge.dst, depth_of.get(edge.src, 0) + 1)
        for node in path.nodes:
            indent = "  " * depth_of.get(node.node_id, 0)
            head = f"{node.key} = {node.value}" if node.key else node.value
            lines.append(
                f"{indent}- [{node.node_id}] {node.kind}: {head[:96]}"
                f"  (conf={node.confidence:.2f}, {node.status})"
            )
        for span_id in path.spans:
            lines.append(f"  source {span_id}: \"{self.raw.text(span_id)[:120]}\"")
        if not path.complete:
            lines.append(f"  ! truncated at: {', '.join(path.truncated_at)}")
        return "\n".join(lines)

    # -- background work ---------------------------------------------------
    def consolidate(self, limit: int = 64) -> dict:
        """Solution B's background pass: drain deferred invalidation, collapse
        nodes that keep being read raw without yielding anything new, and
        re-check contradictions."""
        drained = self.graph.drain_pending(limit)
        collapsed = []
        for node in list(self.graph.nodes.values()):
            if node.status != "fresh":
                continue
            if self.policy.should_deescalate(node):
                node.meta["deescalated"] = True
                node.level_cache.pop(L0, None)
                collapsed.append(node.node_id)
        return {"drained": len(drained), "deescalated": collapsed}

    def rebuild_workspace(self, query: str, budget: int | None = None) -> dict:
        """Destroy the working set and rebuild it from Solution B alone.

        The wiki claims this invariant; the number that makes it meaningful is
        the rebuild cost, so it is measured here (open question #10).
        """
        cleared = 0
        for node in self.graph.nodes.values():
            cleared += len(node.level_cache)
            node.level_cache.clear()
        before = self.ladder.builds.copy()
        ctx = self.plan(query, budget)
        rebuilt = {k: self.ladder.builds[k] - before.get(k, 0) for k in self.ladder.builds}
        return {
            "cleared_level_cache_entries": cleared,
            "rebuilt": rebuilt,
            "tokens": ctx.tokens,
            "nodes": len(ctx.entries),
        }

    # -- reporting ---------------------------------------------------------
    def stats(self) -> dict:
        return self.telemetry.report(
            {
                "graph": self.graph.stats(),
                "index": self.index.stats(),
                "execution": self.execution.stats(),
                "speculation": self.speculator.stats(),
                "raw_spans": len(self.raw),
                "raw_chars": self.raw.total_chars(),
                "ladder_builds": dict(self.ladder.builds),
            }
        )

    def report(self) -> str:
        stats = self.stats()
        flat = {k: v for k, v in stats.items() if not isinstance(v, dict)}
        out = [self.telemetry.format_report()]
        for section in ("graph", "index", "execution", "speculation"):
            out.append(f"\n[{section}]")
            for k, v in stats[section].items():
                out.append(f"  {k}: {v}")
        out.append(f"\nraw: {flat.get('raw_spans')} spans, {flat.get('raw_chars')} chars")
        out.append(f"ladder builds: {stats['ladder_builds']}")
        return "\n".join(out)

    # -- persistence -------------------------------------------------------
    def save(self, path: str | os.PathLike) -> Path:
        path = Path(path)
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "clock": self.clock.now(),
            "raw": self.raw.to_dict(),
            "graph": self.graph.to_dict(),
            "predictor": self.speculator.predictor.to_dict(),
            "history_tokens": self.telemetry.history_tokens,
            "evidence_by_span": self.indexer.evidence_by_span,
        }
        path.write_text(json.dumps(payload), encoding="utf-8")
        return path

    @classmethod
    def load(cls, path: str | os.PathLike, **kwargs) -> "DCR":
        data = json.loads(Path(path).read_text(encoding="utf-8"))
        runtime = cls(**kwargs)
        runtime.clock.restore(data["clock"])
        runtime.raw = RawStore.from_dict(data["raw"], runtime.clock)
        runtime.graph = MemoryGraph(runtime.raw, runtime.clock)
        runtime.graph.load_dict(data["graph"])
        runtime.execution.graph = runtime.graph
        runtime.ladder.raw = runtime.raw
        runtime.indexer.raw = runtime.raw
        runtime.indexer.graph = runtime.graph
        runtime.indexer.evidence_by_span = data.get("evidence_by_span", {})
        runtime.planner.evidence_lookup = runtime.indexer.evidence_by_span.get
        runtime.planner.graph = runtime.graph
        runtime.telemetry.history_tokens = data.get("history_tokens", 0)
        runtime.speculator.predictor.weights.update(data["predictor"]["weights"])
        # Rebuild the retrieval index from state — it is derived, not source.
        for span_id in runtime.raw.spans:
            runtime.index.add_span(span_id, runtime.raw.text(span_id))
        for node in runtime.graph.nodes.values():
            runtime.indexer.reindex(node)
        return runtime
