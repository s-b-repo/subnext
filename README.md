# Dynamic Context Runtime (DCR)

> Docs-only project. No implementation here — this is a specification and design wiki.

**Thesis:** RLMs (Recursive Language Models) don't solve context rot. They move the problem
out of the transformer's fixed attention window and into a program. That is the right primitive,
but the next step is a **dynamic context runtime**: unlimited external state plus a small,
planned working set.

> unlimited history ≠ unlimited attention

Concretely that means two cooperating systems: a small high-attention **Reasoner** and an
unbounded **Memory Runtime** that decides which representation to hand it. See
[the two-system split](docs/architecture/two-system-split.md).

## The shift

```text
                 ┌───────────────┐
incoming context → State Indexer │
                 └───────┬───────┘
                         ↓
              ┌─────────────────────┐
              │ Dynamic Memory Graph│
              └─────────┬───────────┘
                        ↓
       ┌────────────────┼────────────────┐
       ↓                ↓                ↓
   exact spans      semantic states   computations
       ↓                ↓                ↓
       └────────────────┼────────────────┘
                        ↓
                relevance planner
                        ↓
                 tiny active context
                        ↓
                      model
```

The model never receives the entire context. Unlike RAG, the runtime does not retrieve
documents — it retrieves *whichever representation is cheapest and sufficient* for the
current computation.

## Core ideas

| Idea | One line |
|---|---|
| [Representation ladder](docs/concepts/representation-ladder.md) | Same context exists as L0 raw / L1 summaries / L2 vectors / L3 executable derivations. |
| [Stateful memory](docs/concepts/stateful-memory.md) | `M_t = (R_t, S_t, C_t, E_t)`; context evolves instead of being reread. |
| [Facts as cache](docs/concepts/fact-cache.md) | Replace 10k tokens of reasoning with ~20 structured state objects + provenance. |
| [Context as a graph](docs/concepts/context-graph.md) | Retrieval follows dependency edges, not similarity. |
| [Speculative context](docs/concepts/speculative-context.md) | Predict `P(m_i | q_t, S_t)` and prefetch before the model asks. |
| [Context as a state machine](docs/concepts/state-machine.md) | Transcript becomes an append-only audit log, not working memory. |
| [Attention budget](docs/concepts/attention-budget.md) | Context assembly as constrained optimization: max `U(S)` s.t. `Σcost(x) ≤ B_attention`. |
| [Cost model](docs/concepts/cost-model.md) | Aim at `O(k + r)` instead of `O(N²)` while `N` keeps growing. |

## Architecture

- [Overview](docs/architecture/overview.md)
- [Two-system split: Reasoner + Memory Runtime](docs/architecture/two-system-split.md)
- [State Indexer](docs/architecture/state-indexer.md)
- [Memory Graph](docs/architecture/memory-graph.md)
- [Relevance Planner](docs/architecture/relevance-planner.md)
- [Runtime decision policy](docs/architecture/decision-policy.md)
- [Provenance & evidence](docs/architecture/provenance.md)

## Also

- [Comparison: RLM vs RAG vs DCR](docs/comparison.md)
- [Open questions](docs/open-questions.md)
- [Glossary](docs/glossary.md)
- [Roadmap](docs/roadmap.md)
- [FAQ](docs/faq.md)

## Status

Specification stage. Contributions are docs, critiques, and worked examples —
see [CONTRIBUTING.md](CONTRIBUTING.md).

License: [CC BY 4.0](LICENSE) for docs.
