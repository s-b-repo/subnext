"""Extraction, contradiction handling and corroboration."""

from __future__ import annotations

import unittest

from dcr.indexer import HeuristicExtractor
from dcr.runtime import DCR


class ExtractorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.extract = HeuristicExtractor()

    def _keys(self, text: str) -> dict:
        return {c.get("key"): c["value"] for c in self.extract(text, "s1", text)}

    def test_dotted_identifiers_survive(self) -> None:
        self.assertEqual(self._keys("The server ip is 10.0.4.12.")["server.ip"], "10.0.4.12")

    def test_two_facts_in_one_sentence(self) -> None:
        keys = self._keys("The server ip is 10.0.4.12 and the port is 8080.")
        self.assertEqual(keys["server.ip"], "10.0.4.12")
        self.assertEqual(keys["port"], "8080")

    def test_structural_prefix_does_not_hide_the_fact(self) -> None:
        keys = self._keys("Correction: actually the server ip is 10.0.9.7, we misread it.")
        self.assertEqual(keys["server.ip"], "10.0.9.7")

    def test_quoted_value(self) -> None:
        keys = self._keys('The error was "connection refused" on port 8080.')
        self.assertEqual(keys["error"], "connection refused")

    def test_kinds(self) -> None:
        kinds = {
            c["kind"]
            for text in (
                "Goal: restore checkout by 09:00.",
                "Constraint: never restart payments during business hours.",
                "Decision: roll back to build 4471.",
                "Should we page the on-call engineer?",
            )
            for c in self.extract(text, "s1", text)
        }
        self.assertTrue({"goal", "constraint", "decision", "open_question"} <= kinds)

    def test_unstructured_chatter_yields_nothing(self) -> None:
        text = "just some chatter about lunch, nobody said anything actionable"
        self.assertEqual(self.extract(text, "s1", text), [])


class ContradictionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.rt = DCR(budget=400)
        self.rt.ingest("The server ip is 10.0.4.12.", "t1")

    def test_conflicting_value_supersedes_and_keeps_both(self) -> None:
        result = self.rt.ingest("Correction: the server ip is 10.0.9.7.", "t2")
        self.assertEqual(len(result.contradictions), 1)
        history = self.rt.graph.by_key("server.ip", fresh_only=False)
        self.assertEqual([n.value for n in history], ["10.0.4.12", "10.0.9.7"])
        self.assertEqual([n.status for n in history], ["superseded", "fresh"])

    def test_superseded_fact_never_reaches_the_model(self) -> None:
        self.rt.ingest("Correction: the server ip is 10.0.9.7.", "t2")
        rendered = self.rt.plan("what is the server ip?").render()
        self.assertIn("10.0.9.7", rendered)
        self.assertNotIn("server.ip = 10.0.4.12", rendered)

    def test_restating_a_fact_is_corroboration_not_duplication(self) -> None:
        before = len(self.rt.graph.by_key("server.ip"))
        confidence_before = self.rt.graph.by_key("server.ip")[0].confidence
        self.rt.ingest("As noted, the server ip is 10.0.4.12.", "t3")
        after = self.rt.graph.by_key("server.ip")
        self.assertEqual(len(after), before)
        self.assertGreater(after[0].confidence, confidence_before)
        self.assertEqual(len(after[0].source_spans), 2)

    def test_reference_links_build_a_multi_hop_path(self) -> None:
        rt = DCR(budget=400)
        rt.ingest("The blocker is firewall rule 37.", "a")
        rt.ingest("Decision: roll back to build 4471 because the blocker is firewall rule 37.", "b")
        decision = rt.graph.by_kind("decision")[0]
        path = rt.graph.explain(decision.node_id)
        values = [n.value for n in path.nodes]
        self.assertTrue(path.complete)
        self.assertIn("firewall rule 37", values)

    def test_out_of_order_material_cannot_revert_a_correction(self) -> None:
        rt = DCR(budget=400)
        rt.ingest("Correction: actually the server ip is 10.0.9.7.", "b")
        rt.ingest("The server ip is 10.0.4.12.", "a")
        live = rt.graph.by_key("server.ip")
        self.assertEqual(len(live), 2, "both sides stay live for adjudication")
        rendered = rt.plan("what is the server ip?").render()
        self.assertIn("CONTRADICTS=", rendered)

    def test_resolved_contradictions_lose_the_warning(self) -> None:
        self.rt.ingest("Correction: the server ip is 10.0.9.7.", "t2")
        rendered = self.rt.plan("what is the server ip?").render()
        self.assertNotIn("CONTRADICTS=", rendered)
        self.assertIn("superseded", rendered)

    def test_ingest_is_idempotent(self) -> None:
        nodes_before = len(self.rt.graph.nodes)
        self.rt.ingest("The server ip is 10.0.4.12.", "t1")
        self.assertEqual(len(self.rt.graph.nodes), nodes_before)


if __name__ == "__main__":
    unittest.main()
