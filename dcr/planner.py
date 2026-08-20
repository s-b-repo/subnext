"""Relevance Planner — decides what the model actually sees.

This is the component that keeps `k` small:

1. **Seed** from `q_t` by index search at the cheapest plausible level.
2. **Expand** along dependency edges (capped depth *and* fan-out — the
   over-expansion failure mode pulls in the transitive world).
3. **Level-assign** each node via the decision policy.
4. **Bound** to `B_attention` with the knapsack, which demotes before it drops.
5. **Speculate** on what is left over.

`U(x)` is open question #8 and the dominant failure mode of the whole design:
get it wrong and the optimiser confidently fills the window with
cheap-and-useless material while looking efficient. It is therefore a small,
inspectable weighted sum with every term named, and `explain_plan()` prints the
per-node arithmetic so a bad answer can be traced to the term that caused it.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field

from .budget import Candidate, Option, solve
from .ladder import L0, L1, L2, L3, LEVEL_ORDER
from .nodes import CONSTRAINT, EVIDENCE, GOAL, OPEN_QUESTION, STALE, SUPERSEDED, Node
from .policy import JUSTIFY, QUOTE_EXACT, RECOMPUTE, SUMMARIZE, VALUE_LOOKUP


@dataclass
class Weights:
    similarity: float = 1.0
    proximity: float = 0.8
    explain_path: float = 1.5
    confidence: float = 0.4
    read_through: float = 0.3
    recency: float = 0.2
    prefetch: float = 0.3
    stale_penalty: float = 2.0
    superseded_source: float = 1.2
    """Penalty for evidence whose only dependents were superseded — otherwise
    a corrected fact's original wording keeps competing with the correction."""
    contradiction_bonus: float = 0.6
    """A node with a live `contradicts` edge is *more* useful, not less: the
    model should adjudicate rather than inherit whichever side won."""
    kind_prior: dict = field(
        default_factory=lambda: {
            GOAL: 0.5, CONSTRAINT: 0.5, "decision": 0.25,
            "claim": 0.2, "calculation": 0.2, EVIDENCE: 0.0, OPEN_QUESTION: 0.1,
        }
    )
    decay: float = 0.6


#: fidelity of a level *for a given question shape*
LEVEL_FIT = {
    QUOTE_EXACT: {L0: 1.0, L1: 0.55, L2: 0.35, L3: 0.4},
    VALUE_LOOKUP: {L0: 0.85, L1: 0.8, L2: 1.0, L3: 0.9},
    JUSTIFY: {L0: 0.9, L1: 0.85, L2: 0.95, L3: 0.9},
    RECOMPUTE: {L0: 0.7, L1: 0.5, L2: 0.8, L3: 1.0},
    SUMMARIZE: {L0: 0.7, L1: 1.0, L2: 0.85, L3: 0.6},
    "open": {L0: 0.7, L1: 0.85, L2: 1.0, L3: 0.8},
}


@dataclass
class Admission:
    node: Node
    level: str
    cost: int
    utility: float
    reason: str = ""
    pinned: bool = False
    terms: dict = field(default_factory=dict)


@dataclass
class ActiveContext:
    """The tiny working set — `k` — plus everything needed to audit it."""

    query: str
    qtype: str
    budget: int
    entries: list[Admission] = field(default_factory=list)
    tokens: int = 0
    seeds: list[str] = field(default_factory=list)
    dropped: list[str] = field(default_factory=list)
    demoted: list[tuple[str, str, str]] = field(default_factory=list)
    prefetch: list = field(default_factory=list)
    considered: int = 0
    snapshot_version: int = 0
    explain_paths: dict = field(default_factory=dict)
    stale_seen: list[str] = field(default_factory=list)
    overflow: bool = False

    def node_ids(self) -> list[str]:
        return [a.node.node_id for a in self.entries]

    def level_of(self, node_id: str) -> str | None:
        for entry in self.entries:
            if entry.node.node_id == node_id:
                return entry.level
        return None

    def render(self, *, with_header: bool = True) -> str:
        """The prompt text. Grouped by role, not by score, because a model
        reads a labelled state block far more reliably than a ranked list."""
        groups: dict[str, list[Admission]] = {}
        for entry in sorted(self.entries, key=lambda a: (-a.utility, a.node.node_id)):
            groups.setdefault(entry.node.kind, []).append(entry)
        order = [GOAL, CONSTRAINT, "claim", "calculation", "decision", OPEN_QUESTION, EVIDENCE]
        titles = {
            GOAL: "GOALS", CONSTRAINT: "CONSTRAINTS", "claim": "FACTS (cached state)",
            "calculation": "COMPUTED", "decision": "DECISIONS",
            OPEN_QUESTION: "OPEN QUESTIONS", EVIDENCE: "EVIDENCE (raw spans)",
        }
        blocks: list[str] = []
        if with_header:
            blocks.append(
                f"# ACTIVE CONTEXT  (k={self.tokens}/{self.budget} tokens, "
                f"{len(self.entries)} of {self.considered} candidates, query type: {self.qtype})"
            )
        for kind in order:
            entries = groups.pop(kind, [])
            if not entries:
                continue
            blocks.append(f"\n## {titles.get(kind, kind.upper())}")
            for entry in entries:
                blocks.append(self._line(entry))
        for kind, entries in groups.items():
            blocks.append(f"\n## {kind.upper()}")
            for entry in entries:
                blocks.append(self._line(entry))
        return "\n".join(blocks)

    def _line(self, entry: Admission) -> str:
        return entry.terms.get("rendered", "")

    def manifest(self) -> list[dict]:
        return [
            {
                "node_id": a.node.node_id,
                "kind": a.node.kind,
                "key": a.node.key,
                "level": a.level,
                "cost": a.cost,
                "utility": round(a.utility, 3),
                "pinned": a.pinned,
            }
            for a in self.entries
        ]


class RelevancePlanner:
    def __init__(
        self,
        graph,
        index,
        ladder,
        policy,
        speculator=None,
        weights: Weights | None = None,
        *,
        budget: int = 1200,
        seed_k: int = 8,
        max_depth: int = 3,
        max_fanout: int = 6,
        max_candidates: int = 120,
        seed_min_ratio: float = 0.3,
        evidence_lookup=None,
    ) -> None:
        self.graph = graph
        self.index = index
        self.ladder = ladder
        self.policy = policy
        self.speculator = speculator
        self.weights = weights or Weights()
        self.budget = budget
        self.seed_k = seed_k
        self.max_depth = max_depth
        self.max_fanout = max_fanout
        self.max_candidates = max_candidates
        self.evidence_lookup = evidence_lookup
        self.seed_min_ratio = seed_min_ratio
        """Seeds scoring below this fraction of the top hit are dropped. Without
        it, `seed_k` always returns `k` nodes however weak, and weak seeds are
        exactly the cheap-but-useless material that a utility-per-token
        optimiser is happy to pack the window with."""

    # -- planning ----------------------------------------------------------
    def plan(
        self,
        query: str,
        budget: int | None = None,
        *,
        pin: tuple[str, ...] = (),
        force_level: dict[str, str] | None = None,
    ) -> ActiveContext:
        budget = self.budget if budget is None else budget
        force_level = dict(force_level or {})
        self.graph.drain_pending()
        qtype = self.policy.classify(query)
        ctx = ActiveContext(query=query, qtype=qtype, budget=budget,
                            snapshot_version=self.graph.version)

        query_vec = self.ladder.query_vector(query)
        seeds = self._seed(query, query_vec, ctx)
        distances = self._expand(seeds, qtype, ctx, pin)

        pinned = set(pin) | self._pinned(qtype, distances)
        candidates: list[Candidate] = []
        preferred: dict[str, str] = {}
        scratch: dict[str, Admission] = {}

        for node_id, distance in list(distances.items())[: self.max_candidates]:
            node = self.graph.get(node_id)
            if node is None or node.status == SUPERSEDED:
                continue
            if node.status == STALE and node_id not in pinned:
                ctx.stale_seen.append(node_id)
                continue
            available = self.ladder.available(node)
            if not available:
                continue
            want = force_level.get(node_id) or self.policy.preferred_level(node, qtype, available)
            preferred[node_id] = want
            terms = self._utility_terms(node, query_vec, distance, ctx)
            options = []
            for level in available:
                if node_id in force_level and level != force_level[node_id]:
                    continue
                fit = LEVEL_FIT.get(qtype, LEVEL_FIT["open"]).get(level, 0.6)
                utility = max(0.01, terms["base"]) * fit
                options.append(Option(level, self.ladder.cost(node, level), utility))
            if not options:
                continue
            candidates.append(Candidate(node_id, options, pinned=node_id in pinned))
            scratch[node_id] = Admission(node, want, 0, 0.0, pinned=node_id in pinned, terms=terms)

        ctx.considered = len(candidates)
        allocation = solve(candidates, budget, preferred=preferred)

        for node_id, option in allocation.chosen.items():
            entry = scratch[node_id]
            entry.level = option.level
            entry.cost = option.cost
            entry.utility = option.utility
            entry.terms["rendered"] = self.ladder.render(entry.node, option.level)
            entry.reason = self.policy.decide(
                entry.node, qtype, self.ladder.available(entry.node), in_plan=True
            ).reason
            ctx.entries.append(entry)
            entry.node.admits += 1
            if option.level == L0:
                entry.node.meta["l0_reads"] = entry.node.meta.get("l0_reads", 0) + 1
        ctx.tokens = allocation.tokens
        ctx.dropped = allocation.dropped
        ctx.demoted = allocation.demoted
        ctx.overflow = allocation.overflow

        self._speculate(ctx, distances, query_vec, allocation)
        return ctx

    # -- steps -------------------------------------------------------------
    def _seed(self, query: str, query_vec, ctx: ActiveContext) -> list[tuple[str, float]]:
        hits = self.index.search(query, self.seed_k, namespace="node", query_vec=query_vec)
        if hits:
            floor = hits[0][1] * self.seed_min_ratio
            hits = [h for h in hits if h[1] >= floor]
        seeds: list[tuple[str, float]] = []
        for node_id, score in hits:
            node = self.graph.get(node_id)
            if node is None or node.status == SUPERSEDED:
                continue
            if node.status == STALE:
                # Never seed from a stale node without re-grounding it first.
                ctx.stale_seen.append(node_id)
                continue
            seeds.append((node_id, score))
        # Fall back to the raw lexical index when state search finds nothing —
        # the material may be ingested but not yet extracted into state.
        if len(seeds) < 2:
            for span_id, score in self.index.search(query, self.seed_k, namespace="span"):
                evidence = self._evidence_for_span(span_id)
                if evidence is not None and all(evidence != s for s, _ in seeds):
                    seeds.append((evidence, score * 0.8))
        ctx.seeds = [s for s, _ in seeds]
        self._seed_scores = dict(seeds)
        return seeds

    def _evidence_for_span(self, span_id: str) -> str | None:
        if self.evidence_lookup is not None:
            found = self.evidence_lookup(span_id)
            if found:
                return found
        # Fallback scan — O(nodes), so only reached when no lookup is wired in.
        for node in self.graph.nodes.values():
            if node.kind == EVIDENCE and span_id in node.source_spans:
                return node.node_id
        return None

    def _expand(self, seeds, qtype: str, ctx: ActiveContext, pin: tuple[str, ...]) -> dict[str, int]:
        """BFS over dependency edges, capped in depth and fan-out."""
        distances: dict[str, int] = {}
        queue: deque[tuple[str, int]] = deque()
        for node_id, _score in seeds:
            distances[node_id] = 0
            queue.append((node_id, 0))
        for node_id in pin:
            if node_id not in distances and node_id in self.graph.nodes:
                distances[node_id] = 0
                queue.append((node_id, 0))
        while queue:
            node_id, depth = queue.popleft()
            if depth >= self.max_depth or len(distances) >= self.max_candidates:
                continue
            node = self.graph.get(node_id)
            if node is None:
                continue
            downstream = list(node.dependencies)[: self.max_fanout]
            for dep in downstream:
                if dep not in distances and dep in self.graph.nodes:
                    distances[dep] = depth + 1
                    queue.append((dep, depth + 1))
            # One hop *up* to whatever was derived from this node, so a fact
            # brings its decision with it rather than arriving orphaned.
            if depth == 0:
                for dependent in self.graph.neighbors(node_id, direction="in")[: self.max_fanout]:
                    if dependent.node_id not in distances:
                        distances[dependent.node_id] = 1
                        queue.append((dependent.node_id, self.max_depth))
            for other in self.graph.neighbors(node_id, edge_type="contradicts"):
                if other.node_id not in distances and other.status != SUPERSEDED:
                    distances[other.node_id] = depth + 1

        if qtype == JUSTIFY and ctx.seeds:
            # A justification demands the whole audit path, whatever it costs.
            for seed in ctx.seeds[:2]:
                path = self.graph.explain(seed)
                ctx.explain_paths[seed] = path.node_ids()
                for i, node in enumerate(path.nodes):
                    distances.setdefault(node.node_id, min(i, self.max_depth))
        return distances

    def _pinned(self, qtype: str, distances: dict[str, int]) -> set[str]:
        pinned: set[str] = set()
        for kind in self.policy.pinned_kinds(qtype):
            for node in self.graph.by_kind(kind):
                pinned.add(node.node_id)
                distances.setdefault(node.node_id, 1)
        return pinned

    def _utility_terms(self, node: Node, query_vec, distance: int, ctx: ActiveContext) -> dict:
        w = self.weights
        similarity = max(0.0, self.ladder.similarity(node, query_vec))
        seed_score = getattr(self, "_seed_scores", {}).get(node.node_id, 0.0)
        proximity = w.decay ** max(0, distance)
        read_rate = node.reads / node.admits if node.admits else 0.0
        age = max(1, self.graph.clock.now() - node.timestamp)
        recency = 1.0 / (1.0 + (age ** 0.5) / 8.0)
        on_path = any(node.node_id in ids for ids in ctx.explain_paths.values())
        contradicted = bool(self.graph.neighbors(node.node_id, edge_type="contradicts"))
        terms = {
            "similarity": w.similarity * max(similarity, seed_score),
            "proximity": w.proximity * proximity,
            "explain_path": w.explain_path if on_path else 0.0,
            "confidence": w.confidence * node.confidence,
            "read_through": w.read_through * read_rate,
            "recency": w.recency * recency,
            "kind_prior": w.kind_prior.get(node.kind, 0.0),
            "contradiction": w.contradiction_bonus if contradicted else 0.0,
            "stale": -w.stale_penalty if node.status == STALE else 0.0,
            "superseded_source": (
                -w.superseded_source if node.meta.get("superseded_source")
                else (-w.superseded_source / 3 if node.meta.get("corrected_by") else 0.0)
            ),
        }
        terms["base"] = sum(v for k, v in terms.items() if k != "base")
        return terms

    def _speculate(self, ctx: ActiveContext, distances, query_vec, allocation) -> None:
        if self.speculator is None:
            return
        admitted = set(allocation.chosen)
        scored = []
        for node_id, distance in distances.items():
            if node_id in admitted:
                continue
            node = self.graph.get(node_id)
            if node is None or node.status == SUPERSEDED:
                continue
            scored.append((node, self.ladder.similarity(node, query_vec),
                           self.weights.decay ** max(0, distance)))
        predictions = self.speculator.predict(scored, self.graph.clock.now())
        ctx.prefetch = self.speculator.prefetch(predictions, self.graph.nodes)

    # -- introspection -----------------------------------------------------
    def explain_plan(self, ctx: ActiveContext) -> str:
        lines = [
            f"query: {ctx.query!r}  type={ctx.qtype}  "
            f"budget={ctx.budget}  used={ctx.tokens}  candidates={ctx.considered}",
            f"seeds: {', '.join(ctx.seeds) or '(none)'}",
        ]
        for entry in sorted(ctx.entries, key=lambda a: -a.utility):
            terms = ", ".join(
                f"{k}={v:+.2f}" for k, v in entry.terms.items()
                if k not in ("base", "rendered") and v
            )
            lines.append(
                f"  ADMIT {entry.node.node_id} {entry.node.label():<22} "
                f"{entry.level} cost={entry.cost:<4} U={entry.utility:.2f} "
                f"{'[pinned] ' if entry.pinned else ''}({terms})"
            )
        for node_id, wanted, got in ctx.demoted:
            lines.append(f"  DEMOTE {node_id} {wanted} -> {got} (budget pressure)")
        for node_id in ctx.dropped:
            lines.append(f"  DROP   {node_id}")
        for node_id in ctx.stale_seen:
            lines.append(f"  SKIP   {node_id} (stale — needs revalidation)")
        for prediction in ctx.prefetch:
            lines.append(f"  PREFETCH {prediction.node_id} p={prediction.p:.2f}")
        return "\n".join(lines)
