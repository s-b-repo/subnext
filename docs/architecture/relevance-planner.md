# Relevance Planner

Decides what the model actually sees. This is the component that keeps `k` small.

## Inputs

- current query / event `q_t`
- current state `S_t`
- graph structure (edges, staleness)
- token budget for the active window

## Output

```text
active_context = {
  nodes:  [(node_id, ladder_level)]
  budget_used: tokens
}
prefetch = [(node_id, ladder_level, P)]
```

## Selection strategy

1. **Seed** from `q_t`: index search at the cheapest plausible level (L1/L2).
2. **Expand** along dependency edges from seeds — `explain()` paths for anything the query asks to
   justify.
3. **Level-assign** each selected node via the [decision policy](decision-policy.md): exact quote
   → L0; value lookup → L1/L2; recomputation → L3/execute.
4. **Bound** to the budget by dropping lowest-scoring nodes and demoting levels (L0 → L1) before
   dropping outright.
5. **Speculate**: score remaining nodes with `P(m_i | q_t, S_t)`; queue those above `τ`.

## Failure modes to watch

- **Over-expansion:** graph traversal pulls in the transitive world. Cap depth and fan-out.
- **Level over-promotion:** defaulting to L0 "to be safe" destroys the cost model.
- **Stale seeds:** never seed from `stale` nodes without re-grounding.

See [cost model](../concepts/cost-model.md) and
[speculative context](../concepts/speculative-context.md).
