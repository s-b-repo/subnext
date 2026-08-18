# Glossary

**Active context** — the small working set actually placed in the model's window this turn (`k`).

**`B_attention`** — the explicit token budget for the active context; the constraint in the
attention-budget knapsack.

**Cognitive Workspace (Solution A / Reasoner)** — the small, fast, ephemeral high-attention system
that owns current computational state but not history.

**Demotion** — moving a node to a cheaper ladder level to fit the budget; preferred over eviction
because it retains partial utility.

**Audit log** — the append-only raw transcript, retained for grounding, not used as working memory.

**Claim** — an asserted fact node depending on evidence.

**Context rot** — degradation of model performance as irrelevant/stale material accumulates in a
long context.

**DCR** — Dynamic Context Runtime; the system described by this wiki.

**Escalation** — moving a node to a richer, more expensive ladder level (e.g. L1 → L0).

**Evidence** — a grounded span in raw source; the leaves of the graph.

**Fact object** — cached settled fact: `fact_id, value, source spans, confidence, dependencies, timestamp`.

**L0/L1/L2/L3** — representation ladder: raw tokens / chunk summaries / semantic vectors /
executable derivations.

**Lost in the middle** — failure to use information located mid-context despite it being present.

**`M_t = (R_t, S_t, C_t, E_t)`** — memory state: raw, semantic state, computed results, evidence.

**Memory graph** — typed dependency graph replacing the linear transcript.

**Memory Runtime (Solution B)** — the unbounded persistent system: raw store, fact graph, vector
index, state cache, code/results, provenance. Decides which representation to return.

**Persistent Cognitive Memory** — the concrete instantiation of Solution B: event log, fact graph,
semantic index, exact source store, summaries, learned state, prior computations.

**`U(x)`** — estimated usefulness of memory object `x` to the current reasoning step.

**Provenance** — the binding of any node back to source spans, enabling audit and invalidation.

**Relevance planner** — component selecting nodes and ladder levels for the active context.

**RLM** — Recursive Language Model; context as a program object in a REPL with recursive model
calls.

**Speculative context** — predicted-needed memory objects prefetched before the model asks.

**Span** — immutable addressable byte range in L0.

**Stale** — a node whose dependencies or sources changed since its timestamp.

**Supersedes** — edge marking a node as replaced without deleting history.

**`τ` (tau)** — confidence threshold above which speculative context is materialized.
