# Representation Ladder

The same context can exist simultaneously at multiple levels of cost and fidelity:

```text
L0 = raw bytes/tokens
L1 = chunk summaries
L2 = semantic/state vectors
L3 = executable derivations/results
```

The model asks for the **cheapest level capable of answering the current question**.

## Routing examples

```text
"what was the server IP?"      → L1/L2
"quote the exact error"        → L0
"derive what caused the failure" → L2/L3
"recalculate the result"       → L3 / execute
```

## Why this beats "just retrieve chunks"

RAG collapses everything to one level (usually L2 similarity over L0 chunks). The ladder lets a
query that needs *exact bytes* get L0, while a query that needs *a value* pays only for a compact
summary. Cost tracks the actual information need, not a fixed retrieval shape.

## Properties

- **Monotone fidelity:** L0 ⊇ derivable(L1) ⊇ derivable(L2); L3 may add computed facts not
  literally present in L0.
- **Lazy materialization:** higher levels are built on first demand and cached.
- **Invalidation:** if L0 spans change, dependent L1/L2/L3 entries are marked stale (see
  [provenance](../architecture/provenance.md)).

## Open issues

- Who decides the level — the planner, the model, or a learned router? See
  [relevance planner](../architecture/relevance-planner.md).
- How to price "sufficient"? See [cost model](cost-model.md).
