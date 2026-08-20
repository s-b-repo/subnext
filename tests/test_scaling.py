"""The load-bearing claim: `k` stays bounded while `N` grows."""

from __future__ import annotations

import unittest

from dcr.bench import build_corpus
from dcr.llm import LocalReasoner
from dcr.runtime import DCR
from dcr.tokens import estimate_tokens


def measure(turns: int, budget: int = 800) -> dict:
    corpus = build_corpus(turns)
    runtime = DCR(budget=budget)
    for doc_id, text in corpus.docs:
        runtime.ingest(text, doc_id)
    reasoner = LocalReasoner()
    correct = 0
    for probe in corpus.probes:
        answer = runtime.ask(probe.query, reasoner=reasoner)
        correct += probe.expected.lower() in (answer.text or "").lower()
    report = runtime.telemetry.report()
    return {
        "history": estimate_tokens(corpus.text()),
        "mean_k": report["tokens_per_query_mean"],
        "max_k": report["tokens_per_query_max"],
        "correct": correct,
        "probes": len(corpus.probes),
        "stale_reads": report["stale_fact_read_rate"],
    }


class ScalingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.small = measure(60)
        cls.large = measure(600)

    def test_history_actually_grew(self) -> None:
        self.assertGreater(self.large["history"] / self.small["history"], 5)

    def test_active_context_stays_bounded(self) -> None:
        growth = self.large["mean_k"] / self.small["mean_k"]
        self.assertLess(growth, 1.5, "k must not track N")
        self.assertLessEqual(self.large["max_k"], 800)

    def test_answers_stay_correct_as_history_grows(self) -> None:
        for case in (self.small, self.large):
            self.assertEqual(case["correct"], case["probes"])

    def test_no_stale_fact_ever_reaches_the_model(self) -> None:
        for case in (self.small, self.large):
            self.assertEqual(case["stale_reads"], 0.0)


if __name__ == "__main__":
    unittest.main()
