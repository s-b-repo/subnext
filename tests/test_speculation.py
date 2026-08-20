"""Speculative prefetch: budget isolation and learning from feedback."""

from __future__ import annotations

import unittest

from dcr.ids import Clock
from dcr.ladder import Ladder
from dcr.nodes import new_node
from dcr.spans import RawStore
from dcr.speculation import Predictor, Speculator


class PredictorTests(unittest.TestCase):
    def test_learns_from_hit_and_miss_feedback(self) -> None:
        predictor = Predictor()
        useful = {"bias": 1, "query_sim": 0.9, "proximity": 0.9, "read_rate": 0.9,
                  "recency": 0.9, "co_used": 0.6, "stale": 0}
        useless = {"bias": 1, "query_sim": 0.05, "proximity": 0.1, "read_rate": 0.0,
                   "recency": 0.1, "co_used": 0.0, "stale": 0}
        before = predictor.score(useless)
        for _ in range(50):
            predictor.observe(useful, True)
            predictor.observe(useless, False)
        self.assertLess(predictor.score(useless), before)
        self.assertGreater(predictor.score(useful), predictor.score(useless))

    def test_stale_nodes_are_predicted_unlikely(self) -> None:
        predictor = Predictor()
        base = {"bias": 1, "query_sim": 0.8, "proximity": 0.8, "read_rate": 0.5,
                "recency": 0.8, "co_used": 0.3}
        self.assertGreater(predictor.score({**base, "stale": 0}),
                           predictor.score({**base, "stale": 1}))


class SpeculatorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.clock = Clock()
        self.raw = RawStore(self.clock)
        self.ladder = Ladder(self.raw)
        spans = self.raw.add_document(
            "\n\n".join(f"Paragraph {i} with enough words to be worth summarising, "
                        f"repeatedly, so that L1 has something to do." for i in range(12)),
            "d",
        )
        self.nodes = {}
        for i, span in enumerate(spans):
            node = new_node("claim", f"value {i}", source_spans=(span.span_id,), key=f"k{i}")
            node.timestamp = self.clock.tick()
            self.nodes[node.node_id] = node

    def test_prefetch_respects_its_own_budget_and_not_the_attention_budget(self) -> None:
        spec = Speculator(self.ladder, tau=0.0, materialization_budget=4)
        scored = [(node, 0.9, 1.0) for node in self.nodes.values()]
        predictions = spec.predict(scored, self.clock.now())
        issued = spec.prefetch(predictions, self.nodes)
        built = sum(self.ladder.builds.values())
        self.assertLessEqual(built, 4 + len(issued))
        self.assertLess(len(issued), len(self.nodes), "budget must cap materialisation")

    def test_tau_gates_materialisation(self) -> None:
        spec = Speculator(self.ladder, tau=0.99, materialization_budget=10)
        scored = [(node, 0.01, 0.01) for node in self.nodes.values()]
        issued = spec.prefetch(spec.predict(scored, self.clock.now()), self.nodes)
        self.assertEqual(issued, [])

    def test_feedback_records_hit_rate(self) -> None:
        spec = Speculator(self.ladder, tau=0.0, materialization_budget=3)
        scored = [(node, 0.9, 1.0) for node in self.nodes.values()]
        issued = spec.prefetch(spec.predict(scored, self.clock.now()), self.nodes)
        used = {issued[0].node_id} if issued else set()
        result = spec.feedback(used)
        self.assertEqual(result["hits"], len(used))
        self.assertEqual(spec.stats()["prefetch_hits"], len(used))
        self.assertGreater(spec.predictor.updates, 0)

    def test_co_use_is_remembered(self) -> None:
        spec = Speculator(self.ladder)
        ids = list(self.nodes)[:3]
        spec.feedback(set(ids))
        self.assertTrue(spec.co_use[ids[0]])


if __name__ == "__main__":
    unittest.main()
