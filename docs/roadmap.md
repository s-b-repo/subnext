# Roadmap

Each phase's deliverable is a written specification. A reference implementation
of the specified behaviour now lives in `src/` — see
[implementation](implementation.md) — so each phase can be checked against
running code rather than only argued about.

## Phase 0 — Framing (done)
- Motivation, thesis, comparison to RLM/RAG.

## Phase 1 — Specify the ladder
- Formal definition of L0–L3 and their build/invalidate rules.
- Written routing table from query type → level.
- Deliverable: worked example tracing one long-context task through all four levels.

## Phase 2 — Specify state and provenance
- Node/edge schema frozen.
- Invalidation semantics with bounded cascades.
- Deliverable: `explain()` walkthrough on a multi-hop decision.

## Phase 2.5 — Specify the system boundary
- Freeze the Reasoner / Memory Runtime split: interface, ownership, consistency protocol.
- Specify the attention-budget optimization: `cost`, `U`, demotion-before-eviction ordering.
- Deliverable: interface spec + workspace rebuild procedure with its cost accounting.

## Phase 3 — Specify the planner
- Seeding, expansion caps, budget demotion order.
- Escalation protocol and its telemetry.
- Deliverable: pseudocode spec + failure-mode catalogue.

## Phase 4 — Specify speculation
- Predictor inputs, `τ` selection, budget isolation, feedback loop.
- Deliverable: cost/benefit analysis under varying prediction accuracy.

## Phase 5 — Evaluation design
- Define metrics: escalation rate, stale-fact read rate, prefetch hit rate, tokens per resolved
  query, audit-path completeness.
- Deliverable: benchmark proposal that can falsify the `O(k + r)` claim.

## Non-goals
- Claiming empirical speedups without the Phase 5 benchmark. The bundled
  benchmark measures attention cost and answer correctness under a fixed
  deterministic reasoner; it is not a latency comparison against RLM, and it
  does not license the claim that DCR is faster.
- Treating the reference implementation as production infrastructure. It is
  there to make the specification falsifiable, not to be depended on.
