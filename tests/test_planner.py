"""Routing, budget pressure and what is allowed into the window."""

from __future__ import annotations

import unittest

from dcr.ladder import L0, L1, L2
from dcr.policy import (
    JUSTIFY,
    QUOTE_EXACT,
    RECOMPUTE,
    SUMMARIZE,
    VALUE_LOOKUP,
    DecisionPolicy,
)
from dcr.runtime import DCR

TRANSCRIPT = [
    ("t1", "Goal: restore checkout by 09:00 UTC.\n\n"
           "Constraint: never restart the payment service during business hours."),
    ("t2", 'The error was "connection refused" when talking to the inventory host.'),
    ("t3", "The server ip is 10.0.4.12 and the port is 8080."),
    ("t4", "The blocker is firewall rule 37, which drops checkout traffic."),
    ("t5", "Decision: roll back to build 4471 because the blocker is firewall rule 37."),
]


class PolicyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.policy = DecisionPolicy()

    def test_query_classification(self) -> None:
        cases = {
            "quote the exact error": QUOTE_EXACT,
            "what was the server ip?": VALUE_LOOKUP,
            "how many retries were there?": VALUE_LOOKUP,
            "why did we roll back?": JUSTIFY,
            "recompute the incident cost": RECOMPUTE,
            "summarize the incident": SUMMARIZE,
        }
        for query, expected in cases.items():
            self.assertEqual(self.policy.classify(query), expected, query)

    def test_stale_nodes_are_never_admitted_by_the_policy(self) -> None:
        from dcr.nodes import new_node

        node = new_node("claim", "x", source_spans=("s",), key="k")
        node.status = "stale"
        decision = self.policy.decide(node, VALUE_LOOKUP, [L2], in_plan=True)
        self.assertFalse(decision.admitted)
        self.assertTrue(decision.recompute)

    def test_deescalation_after_repeated_unproductive_l0_reads(self) -> None:
        from dcr.nodes import new_node

        node = new_node("claim", "x", source_spans=("s",), key="k")
        node.meta["l0_reads"] = 3
        node.meta["l0_yield"] = 0
        self.assertTrue(self.policy.should_deescalate(node))
        node.meta["l0_yield"] = 2
        self.assertFalse(self.policy.should_deescalate(node))


class PlannerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.rt = DCR(budget=800)
        for doc_id, text in TRANSCRIPT:
            self.rt.ingest(text, doc_id)
        for i in range(40):
            self.rt.ingest(f"Chatter {i}: nothing relevant here, queue at {i} items.", f"n{i}")

    def test_value_query_uses_compact_state(self) -> None:
        ctx = self.rt.plan("what is the server ip?")
        levels = {e.level for e in ctx.entries}
        self.assertEqual(levels, {L2})

    def test_quote_query_promotes_to_raw(self) -> None:
        ctx = self.rt.plan("quote the exact error message")
        self.assertTrue(any(e.level == L0 for e in ctx.entries))

    def test_budget_is_never_exceeded(self) -> None:
        for budget in (30, 60, 120, 400, 2000):
            ctx = self.rt.plan("what is the server ip?", budget)
            self.assertLessEqual(ctx.tokens, budget, f"budget {budget}")

    def test_budget_pressure_demotes_before_dropping(self) -> None:
        roomy = self.rt.plan("quote the exact error message", 800)
        tight = self.rt.plan("quote the exact error message", 120)
        self.assertTrue(any(e.level == L0 for e in roomy.entries))
        self.assertTrue(tight.entries, "something should survive at a cheaper level")
        self.assertTrue(all(e.cost <= 120 for e in tight.entries))

    def test_goals_and_constraints_are_pinned(self) -> None:
        ctx = self.rt.plan("what is the server ip?")
        kinds = {e.node.kind for e in ctx.entries}
        self.assertIn("goal", kinds)
        self.assertIn("constraint", kinds)

    def test_justification_pulls_the_dependency_path(self) -> None:
        ctx = self.rt.plan("why did we roll back?")
        values = " ".join(e.node.value for e in ctx.entries)
        self.assertIn("firewall rule 37", values)
        self.assertTrue(ctx.explain_paths)

    def test_noise_is_not_admitted(self) -> None:
        ctx = self.rt.plan("what is the server ip?")
        self.assertFalse(
            [e for e in ctx.entries if "Chatter" in e.node.value],
            "irrelevant chatter must not reach the window",
        )

    def test_stale_nodes_are_skipped_and_reported(self) -> None:
        claim = self.rt.graph.by_key("server.ip")[0]
        self.rt.graph.invalidate(claim.node_id)
        ctx = self.rt.plan("what is the server ip?")
        self.assertNotIn(claim.node_id, ctx.node_ids())
        self.assertIn(claim.node_id, ctx.stale_seen)

    def test_plan_records_a_snapshot_version(self) -> None:
        ctx = self.rt.plan("what is the server ip?")
        self.assertEqual(ctx.snapshot_version, self.rt.graph.version)

    def test_explain_plan_shows_the_arithmetic(self) -> None:
        ctx = self.rt.plan("what is the server ip?")
        text = self.rt.planner.explain_plan(ctx)
        self.assertIn("ADMIT", text)
        self.assertIn("similarity=", text)


if __name__ == "__main__":
    unittest.main()
