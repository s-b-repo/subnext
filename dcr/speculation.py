"""Speculative context — prefetching the next-needed state.

`P(m_i | q_t, S_t)` is estimated by online logistic regression over cheap
features (query similarity, graph proximity to what was just used, historical
read-through, recency, staleness). Nodes above `τ` are materialised at their
cheap levels *before* the model asks.

Two rules the spec is emphatic about and that are enforced here:

* speculation is budgeted **separately** from `B_attention` — prefetch consumes
  storage/compute, never the active window, so it cannot starve the current
  computation;
* the predictor is fed back the truth (was the prefetch actually used?), which
  is the only thing that keeps `τ` honest.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field

FEATURES = ("bias", "query_sim", "proximity", "read_rate", "recency", "co_used", "stale")


@dataclass
class Prediction:
    node_id: str
    p: float
    features: dict[str, float]


@dataclass
class Predictor:
    """Tiny online logistic model. Deterministic init, SGD on feedback."""

    weights: dict[str, float] = field(
        default_factory=lambda: {
            "bias": -1.2,
            "query_sim": 2.4,
            "proximity": 1.6,
            "read_rate": 1.1,
            "recency": 0.7,
            "co_used": 1.3,
            "stale": -2.5,
        }
    )
    lr: float = 0.15
    updates: int = 0

    def score(self, features: dict[str, float]) -> float:
        z = sum(self.weights.get(f, 0.0) * features.get(f, 0.0) for f in FEATURES)
        return 1.0 / (1.0 + math.exp(-max(-30.0, min(30.0, z))))

    def observe(self, features: dict[str, float], used: bool) -> None:
        p = self.score(features)
        error = (1.0 if used else 0.0) - p
        for f in FEATURES:
            self.weights[f] = self.weights.get(f, 0.0) + self.lr * error * features.get(f, 0.0)
        self.updates += 1

    def to_dict(self) -> dict:
        return {"weights": self.weights, "updates": self.updates}


class Speculator:
    def __init__(
        self,
        ladder,
        predictor: Predictor | None = None,
        tau: float = 0.5,
        materialization_budget: int = 24,
    ) -> None:
        self.ladder = ladder
        self.predictor = predictor or Predictor()
        self.tau = tau
        self.materialization_budget = materialization_budget
        """Number of level-builds allowed per turn. Storage/compute budget —
        deliberately *not* denominated in attention tokens."""
        self.outstanding: dict[str, dict[str, float]] = {}
        self.last_used: tuple[str, ...] = ()
        self.co_use: dict[str, dict[str, int]] = {}
        self.issued = 0
        self.hits = 0
        self.misses = 0
        self.wasted_builds = 0

    # -- features ----------------------------------------------------------
    def features(self, node, query_sim: float, proximity: float, now: int) -> dict[str, float]:
        age = max(1, now - node.timestamp)
        recency = 1.0 / (1.0 + math.log(age))
        read_rate = node.reads / node.admits if node.admits else 0.0
        co_used = 0.0
        for last in self.last_used:
            co_used = max(co_used, self._co_use_rate(last, node.node_id))
        return {
            "bias": 1.0,
            "query_sim": max(0.0, query_sim),
            "proximity": proximity,
            "read_rate": read_rate,
            "recency": recency,
            "co_used": co_used,
            "stale": 1.0 if node.status == "stale" else 0.0,
        }

    def _co_use_rate(self, a: str, b: str) -> float:
        row = self.co_use.get(a)
        if not row:
            return 0.0
        total = sum(row.values()) or 1
        return row.get(b, 0) / total

    # -- prefetch ----------------------------------------------------------
    def predict(self, scored_nodes, now: int) -> list[Prediction]:
        out = []
        for node, query_sim, proximity in scored_nodes:
            features = self.features(node, query_sim, proximity, now)
            out.append(Prediction(node.node_id, self.predictor.score(features), features))
        out.sort(key=lambda p: -p.p)
        return out

    def prefetch(self, predictions: list[Prediction], nodes_by_id: dict) -> list[Prediction]:
        """Materialise cheap levels for predictions above τ, within budget."""
        issued: list[Prediction] = []
        spent = 0
        for prediction in predictions:
            if prediction.p < self.tau or spent >= self.materialization_budget:
                break
            node = nodes_by_id.get(prediction.node_id)
            if node is None:
                continue
            spent += self.ladder.prewarm([node])
            self.outstanding[prediction.node_id] = prediction.features
            issued.append(prediction)
        self.issued += len(issued)
        return issued

    def feedback(self, used_ids: set[str]) -> dict:
        """Was the prefetch used? Trains the predictor and reports hit rate."""
        hits = 0
        for node_id, features in list(self.outstanding.items()):
            used = node_id in used_ids
            self.predictor.observe(features, used)
            if used:
                hits += 1
                self.hits += 1
            else:
                self.misses += 1
                self.wasted_builds += 1
        issued = len(self.outstanding)
        self.outstanding.clear()
        # Record co-occurrence so the next turn can use "what was read with
        # what" as a signal.
        for a in used_ids:
            row = self.co_use.setdefault(a, {})
            for b in used_ids:
                if a != b:
                    row[b] = row.get(b, 0) + 1
        self.last_used = tuple(used_ids)
        return {"issued": issued, "hits": hits}

    def stats(self) -> dict:
        total = self.hits + self.misses
        return {
            "prefetch_issued": self.issued,
            "prefetch_hits": self.hits,
            "prefetch_hit_rate": round(self.hits / total, 3) if total else None,
            "wasted_builds": self.wasted_builds,
            "tau": self.tau,
            "predictor_updates": self.predictor.updates,
        }
