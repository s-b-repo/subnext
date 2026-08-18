# Facts as Cache

Once the model has established a conclusion, the tokens explaining *how* it got there are dead
weight in the active window.

Suppose the model established:

```text
server = 10.0.4.12
error  = "connection refused"
cause  = firewall rule 37
```

A naive system keeps thousands of tokens of reasoning that produced those three lines. Don't.

## Store structured state objects

```text
fact_id
value
source spans
confidence
dependencies
timestamp
```

Then expose only the compact state to the model. You replace:

**10,000 tokens of conversation** → **~20 structured state objects + provenance pointers**

The original text stays available on demand (L0 via the [ladder](representation-ladder.md)).

## Why provenance is mandatory

A cached fact without `source spans` and `confidence` is unverifiable and cannot be safely
invalidated. Every fact must be re-groundable to L0. See
[provenance](../architecture/provenance.md).

## Invalidation

`dependencies` + `timestamp` let the runtime expire a fact when:

- an upstream fact it depends on changes, or
- its source spans are edited/superseded.

This is what keeps the cache honest rather than a stale summary.
