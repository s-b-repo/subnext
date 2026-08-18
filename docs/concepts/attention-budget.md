# The Attention Budget

The part that makes the [two-system split](../architecture/two-system-split.md) substantially
better: give the memory system an explicit budget and make context assembly an **optimization
problem** rather than a heuristic.

## Formulation

Given an attention budget `B_attention`, the memory runtime solves:

```text
maximize    U(S)
over        S ⊆ memory objects
subject to  Σ_{x ∈ S} cost(x) ≤ B_attention
```

where `U(x)` is the estimated usefulness of `x` to the *current reasoning step*.

This is a knapsack: each candidate memory object has a token cost (which depends on the
[ladder level](representation-ladder.md) chosen for it) and an estimated utility. The runtime picks
the best-value set that fits.

## Why this matters

You stop instructing the system manually:

> summarize the old context

and instead the runtime **continuously performs**:

```text
retain
promote
demote
compress
retrieve
evict
reconstruct
```

Those seven operations are just the knapsack solution changing as `q_t` and `S_t` change.

## Level choice is part of the decision

`cost(x)` is not fixed per object — the same node can be admitted at L0 (expensive, exact), L1/L2
(cheap, lossy), or L3 (a computed result). So the optimizer chooses **object *and* level jointly**:

```text
for each candidate node n:
  for each available level L:
    value = U(n, L) / cost(n, L)
```

This is why **demotion beats eviction** when over budget: dropping a node to a cheaper level
retains some utility, while evicting it retains none. Demote first, evict last.

## Estimating `U`

Open problem. Practical signals:

- graph distance from query seeds (dependency edges, not just similarity)
- whether the node is on an `explain()` path the query demands
- historical usage: was this node actually read when admitted before?
- staleness (a `stale` node has near-zero utility until revalidated)
- prediction `P(m_i | q_t, S_t)` from [speculative context](speculative-context.md)

`U` being wrong is the dominant failure mode: the optimizer will confidently fill the window with
useless-but-cheap material. Escalation rate is the telemetry that catches it.

## Relationship to the cost model

`B_attention` is exactly the `k` in [`O(k + r)`](cost-model.md) — made explicit and enforced
rather than hoped for. Bounding `B_attention` is what makes the complexity claim mean anything.

## Interaction with speculation

Speculative prefetch must be budgeted **separately** from `B_attention`. Speculation that competes
for the active window starves the current computation; it should consume storage/compute budget,
not attention budget.
