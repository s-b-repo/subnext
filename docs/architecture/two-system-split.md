# The Two-System Split

Rather than making one model handle unlimited history and unlimited attention simultaneously,
split into **two cooperating systems**.

```text
                 ┌────────────────────────────┐
                 │        Solution A          │
                 │      Working Context       │
                 │                            │
Current task ───►│  small, high-attention     │
                 │  fast, precise, ephemeral  │
                 └─────────────┬──────────────┘
                               │
                        query / update
                               │
                 ┌─────────────▼──────────────┐
                 │        Solution B          │
                 │      Infinite Memory       │
                 │                            │
                 │ raw history                │
                 │ facts                      │
                 │ decisions                  │
                 │ embeddings                 │
                 │ code/results               │
                 │ provenance                 │
                 └────────────────────────────┘
```

## The critical change

**Solution A does not own the history.** It owns only the *currently relevant computational state*.

**Solution B does not need to reason continuously.** It owns the *persistent universe of
information*.

## The loop

```text
1. User/task arrives
        ↓
2. A determines what information it needs
        ↓
3. B retrieves exact state/evidence
        ↓
4. A reasons over the small working set
        ↓
5. A produces result + state changes
        ↓
6. B stores the new state
        ↓
7. A discards irrelevant attention context
```

So you get not only:

```text
unlimited history ≠ unlimited attention
```

but:

```text
unlimited history + bounded attention
```

**without losing information** — because B retains everything and A is reconstructible.

## Make the two systems asymmetric

Do **not** use two identical LLMs.

### Solution A — Reasoner

Optimize for:

- high-quality reasoning
- low latency
- small active context
- tool use
- current state
- immediate decisions

### Solution B — Memory / Indexer

Optimize for:

- extremely large storage
- exact retrieval
- compression
- indexing
- dependency tracking
- provenance
- background consolidation
- detecting contradictions
- predicting future information needs

B can be a **much smaller model plus deterministic data structures**. Most of B's work — span
addressing, index lookup, dependency traversal, memoization, invalidation — needs no frontier
reasoning at all.

## Memory vs computation

The strongest framing of the division:

```text
             ┌───────────────┐
             │    Reasoner   │
             │   attention   │
             └───────┬───────┘
                     │
             ┌───────▼───────┐
             │ Memory Runtime│
             ├───────────────┤
             │ Raw store     │
             │ Fact graph    │
             │ Vector index  │
             │ State cache   │
             │ Code/results  │
             │ Provenance    │
             └───────────────┘
```

The memory runtime decides **what representation to return**. For the same historical information
it might return:

```text
exact text
summary
fact
vector
computed result
code execution result
dependency chain
```

That is more powerful than ordinary RAG because you are not merely retrieving documents.
**You are retrieving state.** This is the [representation ladder](../concepts/representation-ladder.md)
expressed as a system boundary.

## Concrete instantiation

### Solution 1 — Cognitive Workspace
- 32k–128k active context
- fast model
- current task only
- temporary reasoning
- tool execution

### Solution 2 — Persistent Cognitive Memory
- effectively unbounded history
- event log
- fact graph
- semantic index
- exact source store
- summaries
- learned state
- previous computations

## The invariant this buys

The workspace can be **destroyed and rebuilt at any time** from Solution 2. That yields:

> The agent's intelligence is bounded by its working attention;
> its memory is bounded only by storage and retrieval quality.

That is a cleaner answer to "unlimited history ≠ unlimited attention" than continually increasing
the context window.

## Mapping onto DCR components

| Two-system role | DCR component |
|---|---|
| Solution A — Reasoner | the model, fed the tiny active context |
| Solution B — raw store | `R_t`, immutable L0 spans ([state indexer](state-indexer.md)) |
| Solution B — fact graph | [memory graph](memory-graph.md) (`S_t`) |
| Solution B — state cache / code results | `C_t` ([fact cache](../concepts/fact-cache.md)) |
| Solution B — provenance | `E_t` ([provenance](provenance.md)) |
| Solution B — what to return | [decision policy](decision-policy.md) + [attention budget](../concepts/attention-budget.md) |
| Solution B — predicting needs | [speculative context](../concepts/speculative-context.md) |
| Solution B — proving what it stored | [context integrity](context-integrity.md) |

## Open issues

- Where does node extraction live? It is a reasoning task (A-like) running on B's schedule.
- ~~B's background consolidation can contradict what A currently believes mid-task~~ — resolved
  by snapshot isolation plus an interrupt; every plan records `graph.version` and the runtime
  re-plans rather than answering from state that no longer holds.
- ~~Workspace rebuild cost is the real bound on "destroy and rebuild at any time"~~ — measured:
  1.75 ms mean cold rebuild against 27k tokens of history. See
  [workspace rebuild](workspace-rebuild.md).
- B must also be able to prove it returned what it stored. That is a third responsibility the
  original split did not name, and it lives in
  [context integrity](context-integrity.md).

See [open questions](../open-questions.md).
