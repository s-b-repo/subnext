"""L0 store, ladder and budget solver."""

from __future__ import annotations

import itertools
import random
import unittest

from dcr.budget import Candidate, Option, solve
from dcr.graph import MemoryGraph
from dcr.ids import Clock
from dcr.ladder import L0, L1, L2, LEVEL_ORDER, Ladder
from dcr.nodes import new_node
from dcr.spans import RawStore


class SpanStoreTests(unittest.TestCase):
    def setUp(self) -> None:
        self.clock = Clock()
        self.raw = RawStore(self.clock)

    def test_span_ids_are_stable_across_stores(self) -> None:
        text = "alpha beta\n\ngamma delta"
        first = RawStore(Clock()).add_document(text, "d")
        second = RawStore(Clock()).add_document(text, "d")
        self.assertEqual([s.span_id for s in first], [s.span_id for s in second])

    def test_reingesting_identical_content_is_a_noop(self) -> None:
        spans = self.raw.add_document("hello world", "d")
        again = self.raw.add_document("hello world", "d")
        self.assertEqual([s.span_id for s in spans], [s.span_id for s in again])
        self.assertEqual(len(self.raw.documents), 1)

    def test_l0_is_immutable(self) -> None:
        self.raw.add_document("original", "d")
        with self.assertRaises(ValueError):
            self.raw.add_document("edited", "d")

    def test_long_documents_split_into_addressable_spans(self) -> None:
        text = " ".join(f"Sentence number {i} about the incident." for i in range(120))
        spans = self.raw.add_document(text, "d")
        self.assertGreater(len(spans), 1)
        self.assertEqual("".join(self.raw.documents["d"].text[s.start:s.end] for s in spans), text)

    def test_neighbours_returns_surrounding_spans(self) -> None:
        spans = self.raw.add_document("a\n\nb\n\nc\n\nd", "d")
        around = self.raw.neighbours(spans[2].span_id, radius=1)
        self.assertEqual([s.seq for s in around], [1, 2, 3])


class LadderTests(unittest.TestCase):
    def setUp(self) -> None:
        self.clock = Clock()
        self.raw = RawStore(self.clock)
        self.graph = MemoryGraph(self.raw, self.clock)
        self.ladder = Ladder(self.raw)
        long_text = (
            "The deploy failed at 04:12 with connection refused against the inventory host. "
            "The controller retried seven times before giving up and paging the on-call "
            "engineer, who rolled the fleet back to the previous build."
        )
        self.spans = self.raw.add_document(long_text, "d")
        self.node = new_node(
            "claim", "connection refused", source_spans=(self.spans[0].span_id,),
            key="error", confidence=0.8,
        )

    def test_cost_ordering_l2_cheapest_l0_dearest(self) -> None:
        costs = {lvl: self.ladder.cost(self.node, lvl) for lvl in (L0, L1, L2)}
        self.assertLess(costs[L2], costs[L1])
        self.assertLess(costs[L1], costs[L0])
        self.assertLess(LEVEL_ORDER[L2], LEVEL_ORDER[L0])

    def test_higher_levels_are_built_lazily_and_cached(self) -> None:
        self.assertEqual(self.ladder.builds[L1], 0)
        self.ladder.summary(self.node)
        self.ladder.summary(self.node)
        self.assertEqual(self.ladder.builds[L1], 1)
        self.assertIn(L1, self.node.level_cache)

    def test_l0_is_verbatim(self) -> None:
        rendered = self.ladder.render(self.node, L0)
        self.assertIn(self.raw.text(self.spans[0].span_id), rendered)

    def test_l1_is_extractive(self) -> None:
        source = self.raw.text(self.spans[0].span_id)
        for sentence in self.ladder.summary(self.node).split(". "):
            self.assertIn(sentence.strip(". "), source)


class KnapsackTests(unittest.TestCase):
    def _brute_force(self, candidates, budget):
        best_value, best_cost = -1.0, 0
        option_lists = [[None] + c.options for c in candidates]
        for combo in itertools.product(*option_lists):
            cost = sum(o.cost for o in combo if o)
            if cost > budget:
                continue
            if any(o is None for o, c in zip(combo, candidates) if c.pinned):
                continue
            value = sum(o.utility for o in combo if o)
            if value > best_value:
                best_value, best_cost = value, cost
        return best_value

    def test_matches_brute_force_on_small_instances(self) -> None:
        rng = random.Random(7)
        for trial in range(25):
            candidates = [
                Candidate(
                    f"n{i}",
                    [Option(lvl, rng.randint(3, 40), round(rng.uniform(0.1, 3.0), 2))
                     for lvl in (L2, L1, L0)[: rng.randint(1, 3)]],
                    pinned=(i == 0 and trial % 3 == 0),
                )
                for i in range(6)
            ]
            budget = rng.randint(20, 120)
            got = solve(candidates, budget, quantum=1)
            expected = self._brute_force(candidates, budget)
            self.assertLessEqual(got.tokens, budget)
            if expected < 0:  # no feasible set — the pinned item cannot fit
                self.assertTrue(got.overflow)
                continue
            self.assertAlmostEqual(got.utility, expected, places=6)

    def test_never_exceeds_the_budget(self) -> None:
        rng = random.Random(3)
        candidates = [
            Candidate(f"n{i}", [Option(L2, rng.randint(5, 30), rng.random()),
                                Option(L0, rng.randint(40, 200), rng.random() + 1)])
            for i in range(60)
        ]
        for budget in (0, 17, 250, 4000):
            allocation = solve(candidates, budget)
            self.assertLessEqual(allocation.tokens, budget)

    def test_demotes_before_evicting(self) -> None:
        candidates = [
            Candidate("a", [Option(L2, 10, 1.0), Option(L0, 90, 1.4)]),
            Candidate("b", [Option(L2, 10, 1.0), Option(L0, 90, 1.4)]),
        ]
        allocation = solve(candidates, 40, preferred={"a": L0, "b": L0})
        self.assertEqual(len(allocation.chosen), 2, "both nodes should survive, demoted")
        self.assertTrue(all(o.level == L2 for o in allocation.chosen.values()))
        self.assertEqual(len(allocation.demoted), 2)
        self.assertEqual(allocation.dropped, [])

    def test_pinned_candidates_are_admitted_first(self) -> None:
        candidates = [
            Candidate("pinned", [Option(L2, 30, 0.05)], pinned=True),
            Candidate("tasty", [Option(L2, 30, 5.0)]),
        ]
        allocation = solve(candidates, 40)
        self.assertIn("pinned", allocation.chosen)
        self.assertNotIn("tasty", allocation.chosen)

    def test_overflow_is_reported_not_silently_exceeded(self) -> None:
        candidates = [Candidate("pinned", [Option(L0, 500, 1.0)], pinned=True)]
        allocation = solve(candidates, 50)
        self.assertTrue(allocation.overflow)
        self.assertLessEqual(allocation.tokens, 50)


if __name__ == "__main__":
    unittest.main()
