# Provenance & Evidence

`E_t` is not optional bookkeeping — it is what makes aggressive caching and compression safe.

## Requirements

Every node carries:

- `source_spans` — stable pointers into immutable L0
- `confidence`
- `dependencies`
- `timestamp`

## What provenance buys

1. **Verifiability.** `explain(node)` returns the full path from a decision down to raw spans.
   A compressed active context is auditable rather than a black-box summary.
2. **Invalidation.** When a span is superseded or a dependency changes, dependents are marked
   stale deterministically instead of silently rotting.
3. **Conflict handling.** `contradicts` edges keep both sides with their evidence, so the model
   can adjudicate rather than inherit whichever summary won.
4. **Trust calibration.** Confidence plus evidence count lets the planner prefer well-grounded
   facts when the budget is tight.

## Immutability rule

L0 spans are append-only. Corrections never edit history: they add new spans and a `supersedes`
edge. This keeps the audit log valid and makes the transcript, as described in
[state machine](../concepts/state-machine.md), a genuine append-only log.

## Anti-pattern

A cached fact with no spans is a hallucination with a database row. Reject unsourced facts at
`upsert` time.
