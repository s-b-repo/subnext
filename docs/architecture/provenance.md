# Provenance & Evidence

`E_t` is not optional bookkeeping — it is what makes aggressive caching and compression safe.

## Requirements

Every node carries:

- `source_spans` — stable pointers into immutable L0
- `confidence`
- `dependencies`
- `timestamp`
- `origin` — where the content came from, epistemically

## The evidence hierarchy

`Kind` says what role a node plays. `Origin` says how it came to be believed, and
the two are orthogonal:

```text
observed             read directly out of ingested material
externally sourced   another agent, a fetched document, a tool result
computed             produced by running a derivation over known inputs
inferred             concluded by a model rather than read or computed
hypothetical         proposed, not established
```

`server.ip = 10.0.9.7` extracted from a log line and the same string guessed
from two adjacent facts are the same value, the same kind and the same status.
They are not the same claim, and only one of them should settle an argument. The
rendered state object carries `origin=` whenever it is anything but `observed`,
so **a derived hypothesis never reaches the reasoner dressed as an observation**.

`Origin::is_grounds()` draws the line that matters: observed, external and
computed material can support a conclusion; an inference or a hypothesis *is* a
conclusion, and citing one as its own evidence is how a guess becomes a fact.

## Status is not origin

A node's status is its lifecycle, not its provenance:

```text
fresh          current
contradicted   live, and in an open disagreement with another live node
unresolved     live and open — an open question nothing has answered
stale          a dependency changed; needs re-grounding before use
superseded     replaced; retained for the audit log, never admitted
```

The first three are *live* (`Status::is_live()`), so a contested fact still
reaches the window — carrying its `CONTRADICTS=` marker. Hiding a disagreement
is worse than showing one.

## What provenance buys

1. **Verifiability.** `explain(node)` returns the full path from a decision down to raw spans.
   A compressed active context is auditable rather than a black-box summary.
2. **Invalidation.** When a span is superseded or a dependency changes, dependents are marked
   stale deterministically instead of silently rotting.
3. **Conflict handling.** `contradicts` edges keep both sides with their evidence, so the model
   can adjudicate rather than inherit whichever summary won — *for the case the rule covers*.
   See [when both sides are kept](#when-both-sides-are-kept), which is narrower than this
   sentence used to imply.
4. **Trust calibration.** Confidence plus evidence count lets the planner prefer well-grounded
   facts when the budget is tight.

## When both sides are kept

Not every disagreement becomes a contradiction, and the boundary is worth stating plainly because
an earlier version of this page implied it was.

```text
existing corrective, incoming plain    → both stay live, contradicts edge, adjudicate
existing corrective, incoming corrective → newer supersedes
existing plain,      incoming plain    → newer supersedes, silently
```

Ingest order is event order, so **last-writer-wins is the default** (`may_supersede` in
`src/indexer.rs`). The protected case is an explicitly corrective statement — "Correction: …",
"Update: …", "actually …" — which plain material arriving afterwards must not silently revert,
because ingest order is only *approximately* chronology when files are read from a directory or
two transcripts are merged.

The consequence is real and is measured rather than hidden: when two sources state conflicting
values and neither is phrased as a correction, the later one wins and no marker reaches the
window. `bench --baselines` prices that as its own row. The alternative — treating every
plain-vs-plain disagreement as a live contradiction — would mark every routine restatement as a
dispute and flood the window with conflicts that are not conflicts, which is worse.

## Immutability rule

L0 spans are append-only. Corrections never edit history: they add new spans and a `supersedes`
edge. This keeps the audit log valid and makes the transcript, as described in
[state machine](../concepts/state-machine.md), a genuine append-only log.

## Anti-pattern

A cached fact with no spans is a hallucination with a database row. Reject unsourced facts at
`upsert` time — and again at write time, where
[the store](context-integrity.md#11-provenance-is-part-of-admission) refuses a derived object
that names no sources.
