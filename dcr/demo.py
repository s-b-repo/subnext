"""Worked example — one task traced through all four ladder levels.

This is the Phase 1 roadmap deliverable made runnable: a transcript that
contains a correction, an exact string worth quoting, a multi-hop decision to
justify, and a derivation to recompute. It runs offline.
"""

from __future__ import annotations

from .llm import LocalReasoner
from .runtime import DCR

TRANSCRIPT = [
    ("t01", "Goal: restore checkout by 09:00 UTC.\n\n"
            "Constraint: never restart the payment service during business hours."),
    ("t02", "04:12 paging on-call. The service is alpha-checkout and the owner is team-payments."),
    ("t03", 'The error was "connection refused" when talking to the inventory host.'),
    ("t04", "The server ip is 10.0.4.12 and the port is 8080."),
    ("t05", "The blocker is firewall rule 37, which drops traffic to the checkout subnet."),
    ("t06", "Decision: roll back to build 4471 because the blocker is firewall rule 37."),
    ("t07", "The engineer count is 3 and the incident hours are 4."),
    ("t08", "Standup notes: coffee machine still broken, ticket queue at 14 items."),
    ("t09", "Correction: actually the server ip is 10.0.9.7, we misread the dashboard."),
    ("t10", "The hourly rate is 180 USD."),
    ("t11", "Deploy log for build 4471: the rollout started at 04:03 and moved through "
            "canary, then 10 percent, then 50 percent of the fleet without incident. "
            "At 04:11 the checkout pods began failing readiness probes against the "
            "inventory host, the load balancer drained them, and the rollout controller "
            "entered a retry loop. The retry budget was exhausted after 7 attempts and "
            "the final failure code was ERR_CONN_REFUSED_37, at which point the "
            "controller stopped and paged the on-call engineer."),
]


def build(budget: int = 700, noise: int = 60) -> DCR:
    runtime = DCR(budget=budget)
    for doc_id, text in TRANSCRIPT:
        runtime.ingest(text, doc_id)
    for i in range(noise):
        runtime.ingest(
            f"Chatter {i}: dashboards refreshed, {i % 7} alerts acknowledged, "
            f"nothing actionable, standby continues.",
            f"noise{i:03d}",
        )
    runtime.register("incident_cost", lambda rate, hours, engineers: rate * hours * engineers)
    return runtime


def run_demo(budget: int = 700) -> DCR:
    runtime = build(budget=budget)
    reasoner = LocalReasoner()
    rule = "=" * 72

    print(rule)
    print("INGESTED")
    print(rule)
    print(f"{len(runtime.raw.documents)} documents, {len(runtime.raw)} L0 spans, "
          f"{runtime.telemetry.history_tokens} tokens of history")
    print(f"state: {runtime.graph.stats()}")

    def turn(title: str, query: str, budget_override: int | None = None) -> None:
        print(f"\n{rule}\n{title}\n{rule}")
        answer = runtime.ask(query, budget_override, reasoner=reasoner)
        print(answer.context.render())
        print(f"\nQUESTION: {query}")
        print(f"ANSWER:   {answer.text}")
        print(f"[{answer.tokens} tokens of {answer.context.budget} budget, "
              f"{answer.escalations} escalation(s)]")

    turn("1. VALUE LOOKUP — answered from L2 state, after a correction", "what is the server ip?")
    turn("2. EXACT QUOTE — routed straight to L0 by query type",
         "quote the exact error message")
    turn("3. ESCALATION — the compact form is too thin, so the model asks for L0",
         "how many retry attempts were made before the failure?")
    turn("4. JUSTIFICATION — retrieval follows dependency edges", "why did we roll back?")

    print(f"\n{rule}\n5. RECOMPUTE — L3 derivation, memoised into C_t\n{rule}")
    rate = runtime.graph.by_key("hourly.rate")[-1]
    hours = runtime.graph.by_key("incident.hours")[-1]
    engineers = runtime.graph.by_key("engineer.count")[-1]
    cost = runtime.compute(
        "incident_cost",
        {"rate": 180, "hours": 4, "engineers": 3},
        deps=(rate.node_id, hours.node_id, engineers.node_id),
        key="incident.cost",
    )
    print(f"incident.cost = {cost.value} (node {cost.node_id})")
    print(f"execution: {runtime.execution.stats()}")
    print("\nsame derivation again ->", runtime.compute(
        "incident_cost", {"rate": 180, "hours": 4, "engineers": 3},
        deps=(rate.node_id, hours.node_id, engineers.node_id), key="incident.cost").value)
    print(f"execution: {runtime.execution.stats()}  <- memo hit, no recomputation")

    print(f"\n{rule}\n6. INVALIDATION — a corrected input marks the derivation stale\n{rule}")
    runtime.ingest("Correction: the hourly rate is 210 USD, finance updated the figure.", "t12")
    print(f"incident.cost status -> {runtime.graph.nodes[cost.node_id].status}")
    print(runtime.explain(cost.node_id))

    print(f"\n{rule}\n7. AUDIT PATH\n{rule}")
    decision = runtime.graph.by_kind("decision")[0]
    print(runtime.explain(decision.node_id))

    print(f"\n{rule}\n8. WORKSPACE REBUILD — destroy the working set, rebuild from memory\n{rule}")
    print(runtime.rebuild_workspace("what is the server ip?"))

    print(f"\n{rule}\nTELEMETRY\n{rule}")
    print(runtime.report())
    return runtime


if __name__ == "__main__":
    run_demo()
