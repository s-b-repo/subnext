# Context as a State Machine

For a long-running agent, don't think of context as text at all. Compile it into typed state:

```text
Facts
Goals
Constraints
Decisions
Open questions
Dependencies
Evidence
Tools/results
```

Each turn updates **only the affected nodes**. The language transcript becomes an
**append-only audit log**, not the working memory.

## Working memory vs audit log

| | Working memory | Audit log |
|---|---|---|
| Form | Typed state nodes | Raw transcript |
| Size | Small, bounded | Grows without bound |
| Read by model? | Yes (the active set) | Only on demand (L0) |
| Mutable? | Nodes updated in place | Append-only |

## Turn semantics

A turn is a transition:

```text
(state, event) → state'
```

where `event` is a user message, tool result, or subcall return. Only nodes touched by the
event's dependencies change; everything else is untouched and costs nothing to "carry forward".

## Why this is a different architecture

The transcript stops being the source of truth. State is. The transcript is kept solely so any
node can be re-grounded and audited — which is exactly what makes the [fact cache](fact-cache.md)
safe.
