"""Benchmark: DCR vs full context vs a sliding window.

The roadmap's Phase 5 asks for a benchmark that could *falsify* the `O(k + r)`
claim, so this measures the things that would show it failing: tokens per
resolved query as history grows, and whether the answers stay correct while the
window stays small.

The reasoner is the same deterministic line-matcher for every system, so the
comparison is between *context assemblies*, not between models. It cannot
paper over a bad plan with model cleverness — if the answer is wrong, the
runtime put the wrong thing in the window.

Baselines are given every advantage that is honest:

* full context sees the entire transcript, every turn;
* the sliding window sees the most recent `--window` tokens;
* on a scoring tie both prefer the *latest* matching line, so they get
  corrections right whenever the correction is inside their window.

What the baselines cannot do is bounded: a fact that falls out of the window is
gone, and neither can recompute a derived value.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

from .embed import content_tokens
from .llm import LocalReasoner
from .runtime import DCR
from .tokens import estimate_tokens


@dataclass
class Probe:
    query: str
    expected: str
    label: str
    note: str = ""


@dataclass
class Corpus:
    docs: list[tuple[str, str]] = field(default_factory=list)
    probes: list[Probe] = field(default_factory=list)

    def text(self) -> str:
        return "\n\n".join(t for _, t in self.docs)


NOISE = [
    "Standup notes for slot {n}: dashboards were refreshed after the overnight batch, "
    "{n} alerts acknowledged and closed without action, and the on-call rotation stays "
    "as published. Nobody raised a blocker. The mobile team asked about the shared "
    "staging cluster again; the answer is unchanged, they should book a slot in the "
    "calendar rather than grabbing it. No follow-up needed from this thread.",

    "Triage sweep {n}: the queue is at {n} items, mostly duplicate reports of the same "
    "flaky integration test. Two were closed as cannot-reproduce, one was merged into an "
    "existing thread, and the rest are waiting on logs from the reporter. Nothing here "
    "touches the checkout path. The label taxonomy is still a mess and someone should "
    "clean it up when there is time, which there is not.",

    "Chat log {n}: the coffee machine is broken again, facilities ticket {n} is filed, "
    "and the descaling kit is apparently on back-order until next month. Somebody "
    "suggested a kettle. Somebody else suggested going outside. The thread then drifted "
    "into a long argument about tabs versus spaces which nobody won and which is "
    "reproduced here in full for completeness of the audit log.",

    "Metrics digest {n}: p99 latency held steady at {n}ms across all regions, error "
    "budget consumption is flat, and the saturation alarms did not fire. Cache hit rate "
    "drifted down by a fraction of a percent, which is within noise. The synthetic "
    "probes from the eu-west region were briefly red during a network maintenance "
    "window that was announced two weeks ago and is not an incident.",

    "Retro prep {n}: the agenda doc has {n} comments, most of them about process rather "
    "than the outage itself. Recurring themes are alert fatigue, unclear ownership of "
    "the shared queues, and the fact that runbooks are out of date. Someone will "
    "volunteer to own runbook cleanup and then not do it, as is traditional. Meeting "
    "is Thursday, same room, coffee situation permitting.",

    "Deploy bot {n}: build {n} passed CI in eleven minutes, artifacts uploaded, images "
    "signed, and the staging smoke suite is green. No action needed. The flaky browser "
    "test failed once and passed on retry, as it does roughly one run in nine. The "
    "changelog is auto-generated and contains nothing but dependency bumps and a typo "
    "fix in a comment nobody will ever read.",

    "Capacity note {n}: disk usage on runner-{n} sits at 61 percent, below the "
    "threshold, and the cleanup cron is doing its job. Memory headroom is comfortable. "
    "The build cache could be pruned more aggressively but the savings do not justify "
    "the churn. Nothing about this affects the checkout service or any of its "
    "dependencies, and no action is required from anyone reading this.",

    "Handover {n}: on-call handover completed, {n} open threads carried over, none of "
    "them customer-facing. The incoming engineer has the runbook links and the "
    "escalation ladder. One long-running investigation into intermittent DNS timeouts "
    "continues with no conclusion; it has been open for weeks and remains a mystery "
    "that everyone has quietly agreed to live with for now.",
]


def build_corpus(turns: int = 300) -> Corpus:
    """A long ops transcript: a few facts, two corrections, a lot of noise."""
    corpus = Corpus()
    add = lambda doc_id, text: corpus.docs.append((doc_id, text))

    add("t000", "Goal: restore checkout by 09:00 UTC.\n\n"
                "Constraint: never restart the payment service during business hours.")
    add("t001", "The service is alpha-checkout and the owner is team-payments.")
    add("t002", 'The error was "connection refused" when talking to the inventory host.')
    add("t003", "The server ip is 10.0.4.12 and the port is 8080.")
    add("t004", "The deploy window is 23:00-01:00 UTC.")
    add("t005", "The blocker is firewall rule 37, which drops traffic to the checkout subnet.")
    add("t006", "Decision: roll back to build 4471 because the blocker is firewall rule 37.")
    add("t007", "The engineer count is 3 and the incident hours are 4.")
    add("t008", "The hourly rate is 180 USD.")
    add("t009",
        "Deploy log for build 4471: the rollout started at 04:03 and moved through canary, "
        "then 10 percent, then 50 percent of the fleet without incident. At 04:11 the "
        "checkout pods began failing readiness probes against the inventory host, the load "
        "balancer drained them, and the rollout controller entered a retry loop. The retry "
        "budget was exhausted after 7 attempts and the final failure code was "
        "ERR_CONN_REFUSED_37, at which point the controller stopped and paged on-call.")

    fixed = len(corpus.docs)
    correction_at = {
        int(turns * 0.35): "Correction: actually the server ip is 10.0.9.7, "
                           "we misread the dashboard.",
        int(turns * 0.80): "Update: the deploy window is 02:00-04:00 UTC "
                           "after the change-freeze review.",
        int(turns * 0.90): "Correction: the hourly rate is 210 USD, finance updated the figure.",
    }
    for i in range(fixed, max(fixed + 1, turns)):
        if i in correction_at:
            add(f"t{i:03d}", correction_at[i])
        else:
            add(f"t{i:03d}", NOISE[i % len(NOISE)].format(n=i))

    corpus.probes = [
        Probe("what is the server ip?", "10.0.9.7", "corrected fact (mid-history)"),
        Probe("what is the deploy window?", "02:00-04:00", "corrected fact (late)"),
        Probe("who is the owner of the checkout service?", "team-payments",
              "old fact, never repeated"),
        Probe("quote the exact error message", "connection refused", "exact quote"),
        Probe("why did we roll back?", "firewall rule 37", "justification / multi-hop"),
        Probe("how many retry attempts were made before the failure?", "7 attempts",
              "detail buried in a long span"),
        Probe("what is the hourly rate?", "210", "corrected fact (very late)"),
    ]
    return corpus


class BaselineReasoner:
    """Line-matcher over raw text — the same scoring the DCR reasoner uses.

    On a tie it prefers the *latest* line, which is the strongest honest
    baseline: it gets a correction right whenever the correction is in view.
    """

    def complete(self, prompt: str, system: str | None = None, max_tokens: int = 1024) -> str:
        query, body = self._split(prompt)
        q = set(content_tokens(query))
        if not q:
            return ""
        best, best_score, best_index = "", 0.0, -1
        for i, line in enumerate(body.splitlines()):
            if not line.strip():
                continue
            tokens = set(content_tokens(line.replace(".", " ") + " " + line))
            score = len(q & tokens) / len(q)
            if score >= best_score:  # >= keeps the latest on a tie
                best, best_score, best_index = line, score, i
        return best if best_score >= 0.2 else "I don't have that in the context."

    @staticmethod
    def _split(prompt: str) -> tuple[str, str]:
        match = re.search(r"\nQUESTION:\s*(.+)\s*$", prompt)
        query = match.group(1).strip() if match else ""
        body = prompt[: match.start()] if match else prompt
        return query, body


def _truncate_tokens(text: str, budget: int) -> str:
    """Keep the last `budget` tokens — the sliding-window baseline."""
    if estimate_tokens(text) <= budget:
        return text
    lines = text.splitlines()
    kept: list[str] = []
    used = 0
    for line in reversed(lines):
        cost = estimate_tokens(line)
        if used + cost > budget:
            break
        kept.append(line)
        used += cost
    return "\n".join(reversed(kept))


def run_benchmark(turns: int = 300, budget: int = 800, window: int = 8000) -> dict:
    corpus = build_corpus(turns)
    full_text = corpus.text()
    full_tokens = estimate_tokens(full_text)
    windowed = _truncate_tokens(full_text, window)
    windowed_tokens = estimate_tokens(windowed)

    runtime = DCR(budget=budget)
    for doc_id, text in corpus.docs:
        runtime.ingest(text, doc_id)

    baseline = BaselineReasoner()
    reasoner = LocalReasoner()
    rows: list[dict] = []

    for probe in corpus.probes:
        answers = {
            "full": baseline.complete(f"{full_text}\n\nQUESTION: {probe.query}"),
            "window": baseline.complete(f"{windowed}\n\nQUESTION: {probe.query}"),
        }
        dcr_answer = runtime.ask(probe.query, reasoner=reasoner)
        answers["dcr"] = dcr_answer.text
        # Auditability is part of the deliverable, not a bonus: every DCR
        # answer must be walkable back to raw spans. Baselines have no
        # equivalent — a matched line is its own and only justification.
        for node_id in dcr_answer.cited:
            runtime.explain(node_id)
        rows.append(
            {
                "probe": probe,
                "correct": {k: probe.expected.lower() in (v or "").lower()
                            for k, v in answers.items()},
                "tokens": {"full": full_tokens, "window": windowed_tokens,
                           "dcr": dcr_answer.tokens},
                "escalations": dcr_answer.escalations,
                "answers": answers,
            }
        )

    totals = {
        system: sum(1 for r in rows if r["correct"][system]) for system in ("full", "window", "dcr")
    }
    mean_tokens = {
        system: round(sum(r["tokens"][system] for r in rows) / len(rows), 1)
        for system in ("full", "window", "dcr")
    }
    _print_report(corpus, rows, totals, mean_tokens, runtime, turns, budget, window)
    return {
        "turns": turns,
        "history_tokens": full_tokens,
        "correct": totals,
        "mean_tokens": mean_tokens,
        "telemetry": runtime.telemetry.report(),
    }


def _print_report(corpus, rows, totals, mean_tokens, runtime, turns, budget, window) -> None:
    full_tokens = rows[0]["tokens"]["full"]
    line = "-" * 88
    print(line)
    print(f"CONTEXT ROT BENCHMARK — {turns} turns, {len(corpus.docs)} documents, "
          f"{full_tokens} tokens of history")
    print(f"B_attention = {budget} tokens · sliding window = {window} tokens")
    print(line)
    header = f"{'probe':<38} {'full':>6} {'window':>7} {'DCR':>6}   {'DCR tokens':>10}"
    print(header)
    print("-" * len(header))
    mark = lambda ok: "  ok  " if ok else " MISS "
    for row in rows:
        probe = row["probe"]
        print(
            f"{probe.label[:38]:<38}{mark(row['correct']['full'])} "
            f"{mark(row['correct']['window'])} {mark(row['correct']['dcr'])} "
            f"{row['tokens']['dcr']:>10}"
        )
    print("-" * len(header))
    print(f"{'correct':<38}{totals['full']:>6} {totals['window']:>7} {totals['dcr']:>6}"
          f"   of {len(rows)}")
    print(f"{'mean tokens per query':<38}{mean_tokens['full']:>6} "
          f"{mean_tokens['window']:>7} {mean_tokens['dcr']:>6}")
    reduction = full_tokens / mean_tokens["dcr"] if mean_tokens["dcr"] else 0
    window_ratio = full_tokens / max(1.0, mean_tokens["window"])
    dcr_ratio = full_tokens / max(1.0, mean_tokens["dcr"])
    print(f"{'attention vs full history':<38}{'1x':>6} "
          f"{f'{window_ratio:.1f}x':>7} {f'{dcr_ratio:.0f}x':>6}")
    print(line)
    print("per-probe answers (DCR):")
    for row in rows:
        status = "ok  " if row["correct"]["dcr"] else "MISS"
        print(f"  [{status}] {row['probe'].query}")
        print(f"          -> {(row['answers']['dcr'] or '')[:110]}")
    print(line)
    print("DCR telemetry")
    print(runtime.telemetry.format_report())
    print(line)
    print("one-time indexing cost (amortised over all queries, not per query):")
    stats = runtime.stats()
    print(f"  state nodes: {stats['graph']['nodes']}   edges: {stats['graph']['edges']}   "
          f"L0 spans: {stats['raw_spans']}")
    print(f"  ladder builds: {stats['ladder_builds']}   "
          f"superseded: {stats['graph']['superseded']}   stale: {stats['graph']['stale']}")
    print("  note: storage stays O(N); the claim is bounded *attention*, not bounded storage.")
    print(line)
    print("how to read this")
    print("  * The reasoner is a deterministic line-matcher for all three systems, so this")
    print("    compares context assemblies, not models. Full-context accuracy here is a")
    print("    FLOOR, not a ceiling: a real model reading the whole transcript would do")
    print("    better on the retrieval probes. Do not read these columns as 'DCR is more")
    print("    accurate than a long-context model'.")
    print("  * The load-bearing results are (a) the token counts, which no model quality")
    print("    changes, and (b) the sliding window's misses, which are structural — a fact")
    print("    outside the window is unrecoverable at any model quality.")
    print("  * Escalations are counted and charged: a probe that needed L0 costs more, and")
    print("    that cost is in the DCR token column, not hidden.")


def run_scaling(sizes=(100, 300, 1000, 3000), budget: int = 800) -> list[dict]:
    """Does `k` stay flat while `N` grows? That is the whole claim.

    Also prints query latency, which is the honest counterweight: this
    implementation's vector search is a linear scan over state nodes, so
    retrieval cost grows with the graph even though attention does not.
    """
    import time

    rows = []
    print(f"{'turns':>7} {'history':>9} {'nodes':>7} {'mean k':>8} {'max k':>7} "
          f"{'correct':>8} {'ingest':>8} {'query':>8}")
    print("-" * 68)
    for turns in sizes:
        corpus = build_corpus(turns)
        started = time.perf_counter()
        runtime = DCR(budget=budget)
        for doc_id, text in corpus.docs:
            runtime.ingest(text, doc_id)
        ingest_s = time.perf_counter() - started
        started = time.perf_counter()
        reasoner = LocalReasoner()  # fresh per size: node ids are content-derived
        correct = 0
        for probe in corpus.probes:
            answer = runtime.ask(probe.query, reasoner=reasoner)
            correct += probe.expected.lower() in (answer.text or "").lower()
        query_ms = (time.perf_counter() - started) * 1000 / len(corpus.probes)
        report = runtime.telemetry.report()
        row = {
            "turns": turns,
            "history_tokens": estimate_tokens(corpus.text()),
            "nodes": len(runtime.graph.nodes),
            "mean_k": report["tokens_per_query_mean"],
            "max_k": report["tokens_per_query_max"],
            "correct": f"{correct}/{len(corpus.probes)}",
            "ingest_s": round(ingest_s, 2),
            "query_ms": round(query_ms, 1),
        }
        rows.append(row)
        print(f"{row['turns']:>7} {row['history_tokens']:>9} {row['nodes']:>7} "
              f"{row['mean_k']:>8} {row['max_k']:>7} {row['correct']:>8} "
              f"{row['ingest_s']:>7}s {row['query_ms']:>7}ms")
    print("-" * 68)
    first, last = rows[0], rows[-1]
    growth = last["history_tokens"] / max(1, first["history_tokens"])
    k_growth = last["mean_k"] / max(1.0, first["mean_k"])
    print(f"history grew {growth:.0f}x; active context grew {k_growth:.2f}x  "
          f"<- the O(k + r) claim")
    print("query latency is NOT flat: vector search here is a linear scan over state")
    print("nodes. The cost model needs sub-linear retrieval (ANN index) to hold at scale.")
    return rows


if __name__ == "__main__":
    run_benchmark()


# ---------------------------------------------------------------------------
# Mutation-and-correction probe
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Mutation:
    """A fact planted early, referenced until it has dependents, then corrected.

    The repeated references are the point. A correction arriving at 85% of the
    way through has to overcome whatever weight the original accumulated — graph
    proximity, read-through, and the dependents built on top of it. Correcting
    an isolated leaf is the easy case and proves little.
    """

    label: str
    query: str
    stale: str          # ground truth: the value that must no longer be served
    live: str           # ground truth: the value that must be served instead
    establish: str
    references: tuple[str, ...]
    correction: str


MUTATIONS: tuple[Mutation, ...] = (
    Mutation(
        label="datastore (4 dependents)",
        query="what is the primary datastore?",
        stale="postgres-11",
        live="postgres-15",
        establish="The primary datastore is postgres-11 on host db-alpha.",
        references=(
            "Nightly backup job targets the primary datastore postgres-11 on db-alpha; retention is 14 days.",
            "The read replica lags the primary datastore postgres-11 by under 200ms during business hours.",
            "Schema migration 0042 was applied against postgres-11 and verified on the replica.",
            "Capacity note: postgres-11 on db-alpha is at 61 percent disk with no growth alarm set.",
        ),
        correction="Correction: the primary datastore was migrated and is now postgres-15 on host db-omega.",
    ),
    Mutation(
        label="autoscaler threshold (3 dependents)",
        query="what is the autoscaler threshold?",
        stale="65 percent",
        live="80 percent",
        establish="The autoscaler threshold is 65 percent CPU.",
        references=(
            "Load test 19 held the fleet just under the autoscaler threshold of 65 percent CPU for forty minutes.",
            "The scale-out event at 03:12 fired because sustained CPU crossed 65 percent.",
            "Cost review flagged that a 65 percent trigger keeps roughly two extra nodes warm overnight.",
        ),
        correction="Update: the autoscaler threshold is 80 percent CPU after the cost review.",
    ),
    Mutation(
        label="failover region (3 dependents)",
        query="what is the failover region?",
        stale="eu-west-2",
        live="eu-central-1",
        establish="The failover region is eu-west-2.",
        references=(
            "The disaster recovery runbook fails traffic over to eu-west-2 and expects a 12 minute RTO.",
            "Cross-region replication to eu-west-2 was re-enabled after the maintenance window.",
            "The last failover drill exercised eu-west-2 and completed inside the RTO budget.",
        ),
        correction="Correction: the failover region is eu-central-1, eu-west-2 is being decommissioned.",
    ),
    Mutation(
        label="escalation extension (2 dependents)",
        query="what is the escalation extension?",
        stale="4412",
        live="4419",
        establish="The escalation extension is 4412 for the platform on-call rota.",
        references=(
            "Page at 02:40 was routed to extension 4412 and acknowledged in ninety seconds.",
            "The incident bridge dials extension 4412 before opening a severity review.",
        ),
        correction="Update: the escalation extension is 4419, the rota moved to a new bridge.",
    ),
)


def build_mutation_corpus(turns: int = 300) -> Corpus:
    """Establish each fact early, reference it across the first 60%, correct at 85%."""
    corpus = Corpus()
    for i, m in enumerate(MUTATIONS):
        corpus.docs.append((f"m{i:02d}e", m.establish))

    established = len(corpus.docs)
    correct_at = int(turns * 0.85)
    ref_span_end = int(turns * 0.60)
    refs: list[tuple[int, str]] = []
    total_refs = sum(len(m.references) for m in MUTATIONS) or 1
    slot = 0
    for m in MUTATIONS:
        for ref in m.references:
            at = established + (max(ref_span_end - established, 0) * slot) // total_refs
            refs.append((max(at, established), ref))
            slot += 1

    corrections = {correct_at + i: m.correction for i, m in enumerate(MUTATIONS)}
    for i in range(established, max(turns, established + 1)):
        doc_id = f"t{i:03d}"
        if i in corrections:
            corpus.docs.append((doc_id, corrections[i]))
            continue
        planted = [r for at, r in refs if at == i]
        if planted:
            corpus.docs.append((doc_id, "\n\n".join(planted)))
            continue
        corpus.docs.append((doc_id, NOISE[i % len(NOISE)].format(n=i)))
    return corpus


def run_mutation_probe(turns: int = 300, budget: int = 1200) -> None:
    """Is a correction served once the original has dependents, and can the
    runtime show the supersession edge that justifies it?

    Both measurements are made against ground truth held by the corpus, not
    against the runtime's own ``STALE`` marking. That is the whole point:
    ``stale_fact_read_rate`` counts entries whose node the runtime has *marked*
    stale, and ``supersede_on_conflict`` gates whether anything is ever marked —
    so the metric cannot fire in the one configuration where superseded values
    are actually served. A control that cannot fire and a control that passes
    are indistinguishable. The ``stale served`` column below can fire.
    """
    from . import DCR

    variants: list[tuple[str, dict]] = [
        ("full runtime", {}),
        ("no supersession", {"supersede_on_conflict": False}),
    ]

    corpus = build_mutation_corpus(turns)
    n = len(MUTATIONS)
    print(f"MUTATION AND CORRECTION - {turns} turns, "
          f"{estimate_tokens(corpus.text())} tokens of history, B_attention = {budget}")
    print(f"{n} facts established early, "
          f"{sum(len(m.references) for m in MUTATIONS)} references in total, "
          f"superseded at 85% of history")
    header = (f"{'variant':<24}{'corrected':>11}{'stale served':>14}"
              f"{'edge shown':>12}{'stale k':>9}   notes")
    print("-" * len(header))
    print(header)
    print("-" * len(header))

    control_fired = False
    served: list[tuple[str, str, int]] = []
    for name, kwargs in variants:
        rt = DCR(budget=budget, **kwargs)
        for _, text in corpus.docs:
            rt.ingest(text)
        corrected = stale = edges = 0
        notes: list[str] = []
        for m in MUTATIONS:
            answer = rt.ask(m.query)
            text = (answer.text or "").lower()
            live_hit = m.live.lower() in text
            stale_hit = m.stale.lower() in text
            if live_hit:
                corrected += 1
            elif not stale_hit:
                notes.append(f"no answer {m.label}")
            if stale_hit:
                stale += 1
                notes.append(f"STALE {m.label}")
            def _supersedes_stale(node_id: str) -> bool:
                node = rt.graph.get(node_id)
                if node is None:
                    return False
                for old_id in node.meta.get("supersedes", ()) or ():
                    old = rt.graph.get(old_id)
                    if old is not None and m.stale.lower() in (old.value or "").lower():
                        return True
                return False

            shown = any(_supersedes_stale(cid) for cid in answer.cited)
            if shown:
                edges += 1
            elif live_hit:
                notes.append(f"no edge {m.label}")
            if name == "full runtime":
                served.append((m.label, answer.text or "", answer.tokens))
        rate = rt.telemetry.report().get("stale_fact_read_rate")
        if name == "no supersession" and stale > 0:
            control_fired = True
        print(f"{name:<24}{corrected:>8}/{n}{stale:>11}/{n}{edges:>9}/{n}"
              f"{('n/a' if rate is None else f'{rate:.2f}'):>9}   "
              f"{'; '.join(notes) if notes else '-'}")

    print("-" * len(header))
    print("'stale k' is the runtime's own stale_fact_read_rate, shown for comparison: it stays\n"
          "0 even on the row where superseded values are provably served, because disabling\n"
          "supersession means nothing is ever marked. 'stale served' is ground truth.")
    print("\nwhat the full runtime served:")
    for label, text, tokens in served:
        print(f"  {label:<34} {tokens:>4} tok  {' '.join(text.split())[:88]}")
    print()
    print("negative control FIRED: the instrument can distinguish a pass from a no-op."
          if control_fired else
          "negative control DID NOT FIRE: this run does not establish that the probe can\n"
          "detect the failure it tests for.")
