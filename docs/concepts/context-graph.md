# Context as a Graph

Instead of a linear transcript:

```text
message 1
message 2
message 3
...
message 10000
```

represent context as a dependency graph:

```text
Document
 ├── Claim A
 │    ├── Evidence 14
 │    └── Evidence 91
 ├── Claim B
 │    └── Evidence 207
 ├── Calculation C
 │    ├── Claim A
 │    └── Claim B
 └── Decision D
      └── Calculation C
```

## Retrieval follows edges, not similarity

To justify Decision D, walk `D → C → {A, B} → evidence`. The model does not have to *discover*
the important relationship by attention over a giant window — the edges encode it explicitly.

This is structurally robust against **lost-in-the-middle**: relevance is a graph traversal, not
a positional accident.

## Node types

- **Evidence** — grounded spans in L0.
- **Claim** — asserted fact, depends on evidence.
- **Calculation** — computed result, depends on claims/other calculations.
- **Decision** — action/choice, depends on calculations/claims.

## Edges

Edges are typed dependencies (`supports`, `derived-from`, `contradicts`, `supersedes`).
`contradicts`/`supersedes` edges are how the graph represents revision without deleting history.

See [memory graph](../architecture/memory-graph.md) for the runtime structure.
