# Cost Model

## The problem

A giant context imposes `O(N²)` attention pressure, where `N` is total historical context.

## The target

DCR aims for something closer to:

```text
O(k + r)
```

where:

- `k` = active state required by the current operation
- `r` = newly retrieved evidence

while `N`, the total historical context, can keep growing unbounded.

## Where the savings come from

| Source | Naive cost | DCR cost |
|---|---|---|
| Re-reading settled facts | O(N) tokens/turn | O(1) — read cached state |
| Re-deriving computations | full recompute | O(1) — `C_t` memoized |
| Finding relevant material | attention over N | edge traversal + top-`r` retrieval |
| Retrieval latency in subcalls | serial stalls | hidden by speculative prefetch |

## Making `k` explicit

`k` is not an emergent quantity — it is `B_attention`, an enforced budget. The
[attention budget](attention-budget.md) page defines how the runtime fills it optimally.

## The honesty check

`O(k + r)` only holds if:

1. `k` (active set) stays small — enforced by the [state machine](state-machine.md) and
   [fact cache](fact-cache.md).
2. `r` (fresh retrieval) is bounded per turn — enforced by the
   [relevance planner](../architecture/relevance-planner.md).
3. Retrieval itself is sub-linear — the [memory graph](../architecture/memory-graph.md) plus an
   index over L1/L2, not a linear scan of L0.

If any of those degrades, cost trends back toward `O(N)` or worse. The runtime's job is to keep
all three bounded.
