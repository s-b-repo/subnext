"""Provenance, invalidation and the audit path."""

from __future__ import annotations

import unittest

from dcr.graph import MemoryGraph, ProvenanceError
from dcr.ids import Clock
from dcr.nodes import FRESH, STALE, SUPERSEDED, new_node
from dcr.spans import RawStore


class GraphTests(unittest.TestCase):
    def setUp(self) -> None:
        self.clock = Clock()
        self.raw = RawStore(self.clock)
        self.graph = MemoryGraph(self.raw, self.clock)
        self.spans = self.raw.add_document(
            "The server ip is 10.0.4.12.\n\nFirewall rule 37 blocks 8080.\n\n"
            "We rolled back to build 4471.",
            "d",
        )
        self.evidence = self.graph.upsert(
            new_node("evidence", self.raw.text(self.spans[0].span_id),
                     source_spans=(self.spans[0].span_id,))
        )

    def _claim(self, value: str, key: str, deps=None):
        return self.graph.upsert(
            new_node("claim", value, dependencies=tuple(deps or (self.evidence.node_id,)), key=key)
        )

    # -- provenance --------------------------------------------------------
    def test_unsourced_fact_is_rejected(self) -> None:
        with self.assertRaises(ProvenanceError):
            self.graph.upsert(new_node("claim", "invented", key="nowhere"))

    def test_evidence_without_spans_is_rejected(self) -> None:
        with self.assertRaises(ProvenanceError):
            self.graph.upsert(new_node("evidence", "no span here"))

    def test_unknown_span_is_rejected(self) -> None:
        with self.assertRaises(ProvenanceError):
            self.graph.upsert(new_node("evidence", "text", source_spans=("s_missing",)))

    def test_every_non_evidence_node_reaches_evidence(self) -> None:
        claim = self._claim("10.0.4.12", "server.ip")
        decision = self.graph.upsert(
            new_node("decision", "roll back", dependencies=(claim.node_id,))
        )
        path = self.graph.explain(decision.node_id)
        self.assertTrue(path.complete)
        self.assertIn(self.evidence.node_id, path.node_ids())
        self.assertIn(self.spans[0].span_id, path.spans)

    def test_explain_reports_incompleteness_when_truncated(self) -> None:
        node = self._claim("10.0.4.12", "server.ip")
        for i in range(6):
            node = self.graph.upsert(
                new_node("calculation", f"step {i}", dependencies=(node.node_id,))
            )
        path = self.graph.explain(node.node_id, max_depth=2)
        self.assertFalse(path.complete)
        self.assertTrue(path.truncated_at)

    # -- invalidation ------------------------------------------------------
    def test_invalidation_cascades_to_dependents(self) -> None:
        claim = self._claim("10.0.4.12", "server.ip")
        calc = self.graph.upsert(
            new_node("calculation", "reachable", dependencies=(claim.node_id,))
        )
        decision = self.graph.upsert(
            new_node("decision", "roll back", dependencies=(calc.node_id,))
        )
        self.graph.invalidate(claim.node_id)
        self.assertEqual(self.graph.nodes[calc.node_id].status, STALE)
        self.assertEqual(self.graph.nodes[decision.node_id].status, STALE)

    def test_cascade_is_bounded_and_defers_the_tail(self) -> None:
        self.graph.max_cascade = 3
        node = self._claim("10.0.4.12", "server.ip")
        chain = [node]
        for i in range(10):
            chain.append(
                self.graph.upsert(
                    new_node("calculation", f"step {i}", dependencies=(chain[-1].node_id,))
                )
            )
        self.graph.pending_invalidation.clear()
        marked = self.graph.invalidate(node.node_id)
        self.assertLessEqual(len(marked), 3)
        self.assertTrue(self.graph.pending_invalidation, "tail must be deferred, not lost")
        drained = self.graph.drain_pending()
        self.assertTrue(drained)

    def test_evidence_is_never_marked_stale(self) -> None:
        claim = self._claim("10.0.4.12", "server.ip")
        self.graph.invalidate(claim.node_id)
        self.assertEqual(self.graph.nodes[self.evidence.node_id].status, FRESH)

    # -- revision ----------------------------------------------------------
    def test_supersede_keeps_history(self) -> None:
        old = self._claim("10.0.4.12", "server.ip")
        new = self._claim("10.0.9.7", "server.ip")
        self.graph.supersede(old.node_id, new.node_id)
        self.assertEqual(self.graph.nodes[old.node_id].status, SUPERSEDED)
        self.assertIn(old.node_id, self.graph.nodes, "nothing is ever deleted")
        self.assertEqual(self.graph.nodes[new.node_id].meta["supersedes"], [old.node_id])
        self.assertEqual([n.value for n in self.graph.by_key("server.ip")], ["10.0.9.7"])

    def test_contradiction_keeps_both_sides(self) -> None:
        a = self._claim("10.0.4.12", "server.ip")
        b = self._claim("10.0.9.7", "server.ip")
        self.graph.contradict(a.node_id, b.node_id)
        self.assertEqual(
            [n.node_id for n in self.graph.neighbors(a.node_id, edge_type="contradicts")],
            [b.node_id],
        )
        self.assertEqual(self.graph.nodes[a.node_id].status, FRESH)

    def test_version_bumps_on_mutation(self) -> None:
        before = self.graph.version
        self._claim("10.0.4.12", "server.ip")
        self.assertGreater(self.graph.version, before)


if __name__ == "__main__":
    unittest.main()
