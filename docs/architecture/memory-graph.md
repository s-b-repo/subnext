# Dynamic Memory Graph

The store. Holds `M_t = (R_t, S_t, C_t, E_t)` as a typed graph.

## Node schema

```text
node_id
kind         : evidence | claim | calculation | decision | goal | constraint | open_question
value        : compact payload the model can read
source_spans : [span_id]           -> into R_t / E_t
dependencies : [node_id]
confidence   : 0..1
timestamp
level_cache  : { L1?: ..., L2?: vector, L3?: result }
status       : fresh | stale | superseded
```

## Edge types

- `supports` — evidence → claim
- `derived-from` — calculation → inputs
- `contradicts` — conflicting nodes, both retained
- `supersedes` — replacement, preserving history

## Operations

| Op | Meaning |
|---|---|
| `upsert(node)` | add or update; bumps timestamp, marks dependents stale |
| `neighbors(node, edge_type)` | one-hop traversal |
| `explain(node)` | transitive closure down to evidence — the audit path |
| `invalidate(node)` | mark node + transitive dependents stale |
| `search(query, level)` | index lookup at a chosen ladder level |

## Invariants

- Every non-evidence node reaches at least one evidence node via `dependencies`.
- Nothing is deleted. Revision is `supersedes`, contradiction is `contradicts`.
- A `stale` node is never placed in the active context without recomputation or re-grounding.

See [context as a graph](../concepts/context-graph.md) for the conceptual framing.
