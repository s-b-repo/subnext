# Stateful Memory

RLM exposes context through a programmable environment and recursive calls, but the environment
is largely re-derived per call. DCR makes it **stateful**.

Maintain:

```text
M_t = (R_t, S_t, C_t, E_t)
```

where:

- `R_t` = raw source
- `S_t` = semantic state
- `C_t` = computed results
- `E_t` = evidence / provenance

Each model call is then:

```text
S_{t+1} = F(S_t, query, M_t)
```

rather than:

```text
S_{t+1} = F(entire prompt)
```

## Consequence

Context **evolves instead of being repeatedly reread**. A turn is an update to state, not a
re-scan of the transcript. The expensive `F(entire prompt)` path is only taken when state is
insufficient and fresh raw material must be pulled in.

## Update discipline

- `S_t` is small and structured — the working set the model actually sees.
- `C_t` memoizes computations keyed by inputs; identical derivations are never recomputed.
- `E_t` links every element of `S_t`/`C_t` back to spans in `R_t` for verification and
  invalidation.

See [fact cache](fact-cache.md) for the structure of state objects and
[state machine](state-machine.md) for the append-only log view.
