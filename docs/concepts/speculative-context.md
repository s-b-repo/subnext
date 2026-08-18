# Speculative Context

Go one step beyond reactive retrieval. At timestep `t`, predict which memory objects will be
needed:

```text
P(m_i | q_t, S_t)   for each memory object m_i
```

Then **prefetch** the likely-needed state before the model asks for it.

The system holds two tiers:

```text
active context      — materialized, in the working set now
speculative context — predicted, materialized only when confidence crosses a threshold
```

## Why it matters

Recursive systems are slow largely because subcalls **stall on retrieval/compression**.
Prefetching the next-needed state overlaps that latency with the current computation, so a
subcall finds what it needs already warm.

## Control

- **Threshold `τ`:** materialize `m_i` when `P(m_i | q_t, S_t) > τ`. Lower `τ` = more speculative
  work, higher hit rate, more wasted materialization.
- **Budget:** speculative materialization is capped so it never starves the active path.
- **Feedback:** actual usage updates the predictor (was the prefetch used?).

This is analogous to branch prediction / prefetching in CPUs: cheap speculative work to hide
latency, discarded when wrong.

Related: [relevance planner](../architecture/relevance-planner.md), [cost model](cost-model.md).
