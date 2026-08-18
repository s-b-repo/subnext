# Open Questions

These are unresolved and are the honest research surface of the design.

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

## 5. Invalidation cascades
Marking transitive dependents stale can invalidate large subgraphs from one small correction.
Needs bounded/lazy revalidation.

## 6. Speculation waste
`τ` trades wasted materialization against latency hiding. Unknown whether prediction accuracy is
high enough in practice to pay for itself.

## 7. Evaluation
There is no accepted benchmark for "did the runtime keep the working set correct". Candidate
metrics: escalation rate, stale-fact read rate, prefetch hit rate, tokens per resolved query,
audit-path completeness.

## 8. How is `U(x)` estimated?
The attention-budget knapsack is only as good as its utility estimate. A wrong `U` fills the window
with cheap useless material and looks efficient while degrading answers. Candidate signals (graph
distance, `explain()` membership, historical read-through, staleness, prefetch probability) are all
proxies. Unresolved.

## 9. Reasoner/Memory consistency
Solution B performs background consolidation and contradiction detection while Solution A is
mid-task. B can therefore invalidate a fact A is actively reasoning over. Needs a consistency
protocol: snapshot isolation for the workspace, or an interrupt that forces A to re-plan.

## 10. Workspace rebuild cost
The claimed invariant is that the workspace can be destroyed and rebuilt at any time from B. That
is only useful if rebuild is cheap. If rebuilding requires re-deriving state, the invariant is
nominal rather than practical.

## 11. How small can Solution B's model be?
The design asserts B can be a much smaller model plus deterministic data structures. Node
extraction and contradiction detection are the parts that plausibly still need strong reasoning —
unclear where the quality cliff is.

## 12. Is `O(k + r)` real?
It holds only if `k`, `r`, and retrieval cost all stay bounded. Adversarial tasks (every turn
needs new distant evidence) degrade toward O(N).
