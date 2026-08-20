# Reference Implementation

`dcr/` in this repository is a working implementation of the specification on
these pages. Pure Python, standard library only, runs offline. Its purpose is to
make the design **falsifiable**: every claim on these pages that could be wrong
is either enforced by code or measured by the bundled benchmark.

```bash
python -m dcr demo                          # one task through all four levels
python -m dcr bench                         # DCR vs full context vs sliding window
python -m dcr --budget 800 bench --scaling  # does k stay flat while N grows?
python -m unittest discover -s tests
```

## What the code enforces that prose cannot

| Spec statement | How it is enforced |
|---|---|
| "Reject unsourced facts at `upsert` time" | `ProvenanceError` — every non-evidence node must reach evidence |
| "Nothing is deleted; revision is `supersedes`" | superseded nodes stay in the graph and are excluded from planning |
| "A `stale` node is never placed in the active context" | planner skips them and records them in `stale_seen` |
| "Demotion beats eviction" | a property of the knapsack, not a heuristic sort |
| "Bound to the budget" | `Σ cost ≤ B_attention`, verified by tests including brute-force optimality |
| "Speculation must not starve the active path" | prefetch has its own materialisation budget, never attention tokens |
| "Only L0 addressing is eager" | L1/L2/L3 built on first demand, build counts reported |

## What it measures

`O(k + r)` is a target, not a result — so the runtime reports the numbers that
would show it failing: escalation rate, stale-fact read rate, prefetch hit rate,
tokens per resolved query, audit-path completeness, demotions and budget
overflows.

On a 300-turn synthetic incident transcript (27k tokens of history,
`B_attention` = 1200), attention cost is 457 tokens per query — 60x less than
the full history — and stays flat (418 → 412 tokens) while history grows 33x to
283k tokens.

## Where the implementation had to decide something the spec left open

- **`sufficient(L, q)`** — query-type routing plus a measured escalation
  protocol (`#ESCALATE <node_id>` → re-plan with that node pinned at L0). See
  [decision policy](architecture/decision-policy.md) and
  [open questions](open-questions.md) #1.
- **L2's prompt payload** — the vector is the index key; the *state object* is
  what the model reads. See [representation ladder](concepts/representation-ladder.md).
- **`U(x)`** — a named, inspectable weighted sum, printed per node by
  `explain_plan()`. See [attention budget](concepts/attention-budget.md) and
  open question #8.
- **Cascade bounds** — eager up to `max_cascade`, tail deferred to a lazy queue.
  Open question #5.
- **Consistency** — snapshot version per plan plus a post-answer interrupt that
  forces a rebuild. Open question #9.

## What it does not settle

Nothing here resolves the honest research surface. Extraction can still cache a
wrong claim; compression still drops what mattered three turns later; retrieval
in this implementation is a linear scan, so the cost model's third condition
(sub-linear retrieval) is not yet met at scale. See
[open questions](open-questions.md) and the limitations section of
`IMPLEMENTATION.md`.
