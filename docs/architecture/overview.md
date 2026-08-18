# Architecture Overview

```text
                 ┌───────────────┐
incoming context → State Indexer │
                 └───────┬───────┘
                         ↓
              ┌─────────────────────┐
              │ Dynamic Memory Graph│
              └─────────┬───────────┘
                        ↓
       ┌────────────────┼────────────────┐
       ↓                ↓                ↓
   exact spans      semantic states   computations
       ↓                ↓                ↓
       └────────────────┼────────────────┘
                        ↓
                relevance planner
                        ↓
                 tiny active context
                        ↓
                      model
```

## Components

| Component | Responsibility | Doc |
|---|---|---|
| Two-system split | System boundary: Reasoner (attention) vs Memory Runtime (storage/retrieval) | [two-system-split.md](two-system-split.md) |
| State Indexer | Ingest raw context, emit L0 spans + L1/L2 representations, extract candidate nodes | [state-indexer.md](state-indexer.md) |
| Dynamic Memory Graph | Store nodes (evidence/claim/calculation/decision) and typed dependency edges | [memory-graph.md](memory-graph.md) |
| Relevance Planner | Choose which nodes, at which ladder level, enter the active context; drive prefetch | [relevance-planner.md](relevance-planner.md) |
| Decision Policy | keep raw / summarize / vectorize / execute / cache / discard / prefetch / recompute | [decision-policy.md](decision-policy.md) |
| Provenance Store | Bind every node to source spans; handle staleness and invalidation | [provenance.md](provenance.md) |
| Execution layer | Run derivations (L3), memoize into `C_t` | [decision-policy.md](decision-policy.md) |

## Relationship to RLM

DCR sits **above** the RLM primitive. RLM contributes: context as a manipulable program object
with recursive model calls. DCR contributes: persistent typed state, a representation ladder, a
planner, and speculation — so recursion operates on a warm, compact working set instead of
re-slicing raw text.

## Data flow per turn

1. Event arrives (user message / tool result / subcall return).
2. State Indexer grounds it into L0 spans, produces L1/L2 as needed.
3. Graph is updated: new evidence nodes, invalidated dependents.
4. Planner assembles the active context (`k`) + bounded fresh retrieval (`r`).
5. Model call updates state: `S_{t+1} = F(S_t, query, M_t)`.
6. New facts/computations are cached with provenance; speculative prefetch is issued for `t+1`.
