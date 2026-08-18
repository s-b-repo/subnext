# RLM vs RAG vs DCR

| | RAG | RLM | DCR |
|---|---|---|---|
| Unit retrieved | document chunks | slices of the context variable | cheapest sufficient representation |
| Retrieval signal | similarity | program logic in a REPL | dependency edges + similarity + prediction |
| Context across turns | rebuilt per query | re-derived per call | persistent typed state `M_t` |
| Settled conclusions | re-retrieved | re-derived | cached as fact objects + provenance |
| Handles "quote exactly" | poorly (chunk boundaries) | yes (raw slicing) | yes (L0 spans) |
| Handles "recompute" | no | yes (sub-calls / code) | yes (L3, memoized) |
| Latency shape | one retrieval hop | serial stalls per recursion level | prefetch overlaps retrieval |
| Cost vs history size | O(chunks read) | O(slices read), repeatedly | ~O(k + r) with N growing |
| Provenance | citation of chunk | implicit in the program | first-class, mandatory |
| System shape | one model + retriever | one model recursing on itself | asymmetric pair: Reasoner + Memory Runtime |
| Context assembly | top-k similarity | whatever the program slices | budgeted optimization under `B_attention` |

## Summary

- **RAG** collapses everything to one representation and one retrieval signal.
- **RLM** gives the crucial primitive — context as a manipulable program object with recursive
  model calls — but re-reads and re-derives.
- **DCR** keeps that primitive and adds state, a representation ladder, graph retrieval, and
  speculation.

DCR is not a replacement for RLM. It is a runtime that makes RLM-style recursion cheap.
