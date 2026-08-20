# Open Questions

These are unresolved and are the honest research surface of the design.
Questions that have since been answered in code are listed as
[resolved](#resolved) at the end, with pointers — an open-questions page that
never closes anything is a page nobody trusts.

## 1. What does `sufficient(L, q)` mean?
The ladder requires deciding the cheapest level that can answer a query. There is no clean
definition. Options: learned router, query-type heuristics, escalation-with-retry, or model
self-report of insufficiency. Escalation rate is a measurable proxy but not a definition.

## 2. Who extracts state — and can it be trusted?
Node extraction is itself a model call. A wrong `Claim` cached with high confidence is worse than
a long prompt. Mitigations: mandatory spans, confidence thresholds, `contradicts` edges. Unclear
what error rate is tolerable.

## 3. Compression is lossy in unknown ways
A summary drops the detail that mattered three turns later. L0 remains available, but only if the
planner knows to escalate — and it can't know what it dropped.

## 4. Graph construction cost
Building and maintaining typed edges may cost more than it saves for short/medium contexts. Where
is the crossover point vs plain long-context or RLM?

## 5. Speculation waste
`τ` trades wasted materialization against latency hiding. Unknown whether prediction accuracy is
high enough in practice to pay for itself.

## 6. Evaluation
There is no accepted benchmark for "did the runtime keep the working set correct". Candidate
metrics: escalation rate, stale-fact read rate, prefetch hit rate, tokens per resolved query,
audit-path completeness.

## 7. How is `U(x)` estimated?
The attention-budget knapsack is only as good as its utility estimate. A wrong `U` fills the window
with cheap useless material and looks efficient while degrading answers. Candidate signals (graph
distance, `explain()` membership, historical read-through, staleness, prefetch probability) are all
proxies. Unresolved.

## 8. How small can Solution B's model be?
The design asserts B can be a much smaller model plus deterministic data structures. Node
extraction and contradiction detection are the parts that plausibly still need strong reasoning —
unclear where the quality cliff is.

## 9. Is `O(k + r)` real?
It holds only if `k`, `r`, and retrieval cost all stay bounded. Adversarial tasks (every turn
needs new distant evidence) degrade toward O(N).

## 10. Is tamper-evidence enough?
The [context integrity](architecture/context-integrity.md) layer detects corruption, editing and
rollback. Against an attacker with write access to the container it is not sufficient, and the
bundled crate ships no signer. Whether "evident but not proof" is the right stopping point for a
memory runtime, or whether a signer should be mandatory, is unresolved.

## 11. Does the origin label change behaviour, or only rendering?
`Origin` keeps an inference from reaching the reasoner disguised as an observation. What it does
*not* do is price it: `U(x)` has no origin term, so a hypothesis and an observation compete for the
window on equal footing. Whether derived material should be discounted — and by how much without
making it unreachable — is unmeasured.

---

## Resolved

These were open and are now answered in code. Kept here because what closed them
is more useful than the fact that they closed.

### ~~Invalidation cascades~~ (was #5)
Bounded: `max_cascade` nodes are invalidated eagerly and the tail is deferred to a lazy queue
drained before the next plan, so one small correction cannot stall a turn.
See `MemoryGraph::invalidate` and `drain_pending`.

### ~~Reasoner/Memory consistency~~ (was #9)
Snapshot isolation plus an interrupt. Every plan records `graph.version`; after the model answers,
the runtime checks whether anything in the working set was invalidated mid-turn and rebuilds rather
than returning an answer grounded in state that no longer holds.
See `Dcr::ask_with_consolidation`.

### ~~Workspace rebuild cost~~ (was #10)
Measured rather than asserted: mean cold rebuild 1.75 ms against 27k tokens of history, versus
0.24 ms warm. The invariant is real at this scale, with the caveat that the retrieval term inside
it is still linear. See [workspace rebuild](architecture/workspace-rebuild.md) and `bench --rebuild`.
