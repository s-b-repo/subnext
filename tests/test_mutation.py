"""The mutation-and-correction probe, and the property that makes it worth having.

``stale_fact_read_rate`` counts context entries whose node carries the STALE
status, and ``supersede_on_conflict`` gates whether that status is ever set — so
the metric reads 0.00 both when the runtime correctly excludes stale nodes and
when it never marked any. These tests pin the ground-truth measurement that
distinguishes those two cases.
"""

from __future__ import annotations

import unittest

from dcr import DCR
from dcr.bench import MUTATIONS, build_mutation_corpus
from dcr.indexer import HeuristicExtractor


def serve(supersede: bool) -> list[tuple[bool, bool]]:
    corpus = build_mutation_corpus(300)
    rt = DCR(budget=1200, supersede_on_conflict=supersede)
    for _, text in corpus.docs:
        rt.ingest(text)
    out = []
    for m in MUTATIONS:
        text = (rt.ask(m.query).text or "").lower()
        out.append((m.live.lower() in text, m.stale.lower() in text))
    return out


class MutationProbe(unittest.TestCase):
    def test_corrections_served_once_original_has_dependents(self) -> None:
        stale = [m.label for (_, s), m in zip(serve(True), MUTATIONS) if s]
        self.assertEqual(stale, [], f"full runtime served superseded values for: {stale}")

    def test_negative_control_can_fail(self) -> None:
        # The whole point of the probe. If disabling supersession produces no
        # stale read, the probe is not measuring what it claims to and the
        # "0 stale reads" result in the full runtime is unfalsifiable.
        reads = sum(1 for _, s in serve(False) if s)
        self.assertGreater(
            reads, 0,
            "disabling supersession served no superseded value; the control "
            "cannot fire, so a passing run proves nothing",
        )

    def test_supersession_improves_correction_delivery(self) -> None:
        with_ = sum(1 for live, _ in serve(True) if live)
        without = sum(1 for live, _ in serve(False) if live)
        self.assertGreater(with_, without, f"{with_} vs {without}")

    def test_ground_truth_values_are_distinguishable(self) -> None:
        for m in MUTATIONS:
            s, l = m.stale.lower(), m.live.lower()
            self.assertFalse(s in l or l in s, f"{m.label}: {s!r} vs {l!r}")


class Restatement(unittest.TestCase):
    def test_coordinated_restatement_carries_the_subject(self) -> None:
        ex = HeuristicExtractor()
        for text in (
            "The primary datastore was migrated and is now postgres-15 on host db-omega.",
            "Correction: the primary datastore was migrated and is now postgres-15 on host db-omega.",
        ):
            claims = [v for k, v in ex._assignments(text) if k == "primary.datastore"]
            self.assertEqual(len(claims), 1, f"{text!r} -> {claims}")
            self.assertIn("postgres-15", claims[0])

    def test_fronted_adverb_is_not_part_of_the_value(self) -> None:
        ex = HeuristicExtractor()
        claims = dict(ex._assignments("The failover region is now eu-central-1."))
        self.assertEqual(claims.get("failover.region"), "eu-central-1")


if __name__ == "__main__":
    unittest.main()


class VerbPhrasedCorrections(unittest.TestCase):
    """Corrections in the wild carry verbs; a copula-only extractor drops them."""

    def test_transitions_are_extracted(self) -> None:
        ex = HeuristicExtractor()
        for text in (
            "The primary datastore has moved to postgres-15.",
            "The primary datastore was replaced by postgres-15.",
            "The primary datastore changed to postgres-15.",
            "The primary datastore should be postgres-15.",
            "The primary datastore is now postgres-15.",
            "The primary datastore was migrated and is now postgres-15.",
        ):
            values = [v for k, v in ex._assignments(text) if k == "primary.datastore"]
            self.assertTrue(
                any("postgres-15" in v for v in values),
                f"{text!r} extracted {values}",
            )
            self.assertFalse(
                any("postgres-11" in v for v in values),
                f"{text!r} also extracted a stale value: {values}",
            )
