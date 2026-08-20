"""End-to-end behaviour: memoisation, escalation, consistency, persistence."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from dcr.llm import LocalReasoner
from dcr.runtime import DCR


def build(budget: int = 600, noise: int = 60) -> DCR:
    rt = DCR(budget=budget)
    rt.ingest("Goal: restore checkout by 09:00 UTC.", "t1")
    rt.ingest('The error was "connection refused" when talking to the inventory host.', "t2")
    rt.ingest("The server ip is 10.0.4.12 and the port is 8080.", "t3")
    rt.ingest("The hourly rate is 180 USD.", "t4")
    rt.ingest("The engineer count is 3 and the incident hours are 4.", "t5")
    for i in range(noise):
        rt.ingest(f"Chatter {i}: dashboards refreshed, queue at {i} items, nothing to do.", f"n{i}")
    rt.ingest("Correction: actually the server ip is 10.0.9.7, we misread the dashboard.", "t6")
    return rt


class ExecutionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.rt = build()
        self.calls = []
        self.rt.register(
            "incident_cost",
            lambda rate, hours: self.calls.append(1) or rate * hours,
        )
        self.rate = self.rt.graph.by_key("hourly.rate")[-1]

    def test_memoised_derivation_runs_once(self) -> None:
        for _ in range(3):
            node = self.rt.compute("incident_cost", {"rate": 180, "hours": 4},
                                   deps=(self.rate.node_id,), key="incident.cost")
        self.assertEqual(node.value, "720")
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(self.rt.execution.stats()["memo_hits"], 2)

    def test_changed_input_invalidates_and_recomputes(self) -> None:
        node = self.rt.compute("incident_cost", {"rate": 180, "hours": 4},
                               deps=(self.rate.node_id,), key="incident.cost")
        self.rt.ingest("Correction: the hourly rate is 210 USD.", "t7")
        self.assertEqual(self.rt.graph.nodes[node.node_id].status, "stale")
        again = self.rt.compute("incident_cost", {"rate": 210, "hours": 4},
                                deps=(self.rate.node_id,), key="incident.cost")
        self.assertEqual(again.value, "840")
        self.assertEqual(len(self.calls), 2, "a changed dependency must not hit the memo")


class AnsweringTests(unittest.TestCase):
    def setUp(self) -> None:
        self.rt = build()
        self.reasoner = LocalReasoner()

    def test_answers_with_the_corrected_fact_in_a_tiny_window(self) -> None:
        answer = self.rt.ask("what is the server ip?", reasoner=self.reasoner)
        self.assertIn("10.0.9.7", answer.text)
        self.assertNotIn("10.0.4.12", answer.text)
        self.assertLess(answer.tokens, 400)
        self.assertLess(answer.tokens, self.rt.telemetry.history_tokens / 4)

    def test_escalation_promotes_a_node_to_raw(self) -> None:
        self.rt.ingest(
            "Deploy log: the rollout moved through canary and 10 percent of the fleet "
            "without incident, then readiness probes began failing against the inventory "
            "host. The retry budget was exhausted after 7 attempts and the final failure "
            "code was ERR_CONN_REFUSED_37, at which point the controller paged on-call.",
            "log",
        )
        answer = self.rt.ask("how many retry attempts were made before the failure?",
                             reasoner=self.reasoner)
        self.assertEqual(answer.escalations, 1)
        self.assertIn("7 attempts", answer.text)
        self.assertEqual(self.rt.telemetry.report()["escalation_rate"], 1.0)

    def test_citations_are_recorded_as_read_through(self) -> None:
        answer = self.rt.ask("what is the server ip?", reasoner=self.reasoner)
        self.assertTrue(answer.cited)
        for node_id in answer.cited:
            self.assertGreater(self.rt.graph.nodes[node_id].reads, 0)

    def test_unanswerable_query_does_not_invent(self) -> None:
        answer = self.rt.ask("what is the airspeed velocity of an unladen swallow?",
                             reasoner=self.reasoner)
        self.assertIn("don't have that", answer.text)

    def test_telemetry_reports_the_evaluation_metrics(self) -> None:
        self.rt.ask("what is the server ip?", reasoner=self.reasoner)
        report = self.rt.telemetry.report()
        for metric in ("escalation_rate", "stale_fact_read_rate", "tokens_per_query_mean",
                       "compression_ratio"):
            self.assertIn(metric, report)
        self.assertEqual(report["stale_fact_read_rate"], 0.0)


class ConsistencyTests(unittest.TestCase):
    def test_mid_turn_invalidation_forces_a_replan(self) -> None:
        rt = build()

        class Meddler:
            """Consolidates (and invalidates) while the Reasoner is thinking."""

            def __init__(self) -> None:
                self.calls = 0

            def complete(self, prompt, system=None, max_tokens=1024):
                self.calls += 1
                if self.calls == 1:
                    node = rt.graph.by_key("server.ip")[-1]
                    rt.graph.invalidate(node.node_id)
                return "ok"

        answer = rt.ask("what is the server ip?", reasoner=Meddler())
        self.assertTrue(answer.replanned or answer.context.stale_seen)

    def test_commit_fact_requires_provenance(self) -> None:
        from dcr.graph import ProvenanceError

        rt = build()
        with self.assertRaises(ProvenanceError):
            rt.commit_fact("something I made up", key="invented")

    def test_workspace_can_be_destroyed_and_rebuilt(self) -> None:
        rt = build()
        before = rt.plan("what is the server ip?")
        stats = rt.rebuild_workspace("what is the server ip?")
        after = rt.plan("what is the server ip?")
        self.assertGreater(stats["cleared_level_cache_entries"], 0)
        self.assertEqual(before.node_ids(), after.node_ids())
        self.assertEqual(before.tokens, after.tokens)


class PersistenceTests(unittest.TestCase):
    def test_round_trip_preserves_state_and_answers(self) -> None:
        rt = build()
        expected = rt.ask("what is the server ip?", reasoner=LocalReasoner()).text
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "store.json"
            rt.save(path)
            restored = DCR.load(path, budget=600)
        self.assertEqual(len(restored.graph.nodes), len(rt.graph.nodes))
        self.assertEqual(len(restored.raw.spans), len(rt.raw.spans))
        self.assertEqual(
            restored.ask("what is the server ip?", reasoner=LocalReasoner()).text, expected
        )


if __name__ == "__main__":
    unittest.main()
