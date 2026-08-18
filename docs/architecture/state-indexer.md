# State Indexer

The entry point. Turns incoming context into addressable, multi-level representations.

## Responsibilities

1. **Span addressing (L0).** Every byte range gets a stable identifier so any later claim can
   point back at it. Spans are immutable; corrections arrive as new spans plus a `supersedes`
   edge.
2. **Chunk summarization (L1).** Lazily built per chunk, cached.
3. **Semantic/state vectors (L2).** Embeddings over chunks *and* over extracted state nodes —
   note that indexing state, not just text, is what makes graph retrieval useful.
4. **Node extraction.** Propose candidate `Evidence`, `Claim`, `Calculation`, `Decision` nodes
   from the incoming material, each with source spans and confidence.

## Laziness

Only L0 addressing is eager. L1/L2/L3 are built on first demand and then cached, so ingesting a
huge document is cheap until something actually asks about it.

## Contract

```text
index(event) -> {
  spans:  [span_id, byte_range, ...]
  nodes:  [candidate node with source_spans + confidence]
  stale:  [node_ids invalidated by this event]
}
```

The `stale` set is what makes ingestion safe: new material can contradict cached facts, and the
indexer is responsible for flagging that rather than silently leaving a wrong fact in the cache.
