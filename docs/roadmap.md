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

## Phase 2.5 — Specify the system boundary (done)
- Freeze the Reasoner / Memory Runtime split: interface, ownership, consistency protocol.
- Specify the attention-budget optimization: `cost`, `U`, demotion-before-eviction ordering.
- Deliverable: interface spec + workspace rebuild procedure with its cost accounting —
  [workspace rebuild](architecture/workspace-rebuild.md).

## Phase 3 — Specify the planner (done)
- Seeding, expansion caps, budget demotion order.
- Escalation protocol and its telemetry.
- Deliverable: pseudocode spec + failure-mode catalogue —
  [failure modes](architecture/failure-modes.md).

## Phase 4 — Specify speculation
- Predictor inputs, `τ` selection, budget isolation, feedback loop.
- Deliverable: cost/benefit analysis under varying prediction accuracy.

## Phase 5 — Evaluation design
- Define metrics: escalation rate, stale-fact read rate, prefetch hit rate, tokens per resolved
  query, audit-path completeness.
- Deliverable: benchmark proposal that can falsify the `O(k + r)` claim.

## Phase 6 — Authenticated memory (done)
- Content-addressed objects, Merkle root, chained generations, anti-rollback.
- Integrity separated from authenticity, confidentiality and semantic trust.
- Bit-rot scrubbing with repair only from independently verified replicas.
- Deliverable: [context integrity](architecture/context-integrity.md), the `.context`
  container, and `bench --tamper` as its falsification.

## Phase 7 — Falsification (in progress)
- Baselines that can beat DCR: RAG, uniform summarisation, recursive map-reduce
  (`bench --baselines`). Done.
- Rebuild cost measured rather than claimed (`bench --rebuild`). Done.
- Still missing: a non-synthetic corpus, a latency comparison against a real
  model, and sub-linear retrieval — without which the `O(k + r)` claim holds
  only for the attention term.

## Non-goals
- Claiming empirical speedups without the Phase 5 benchmark. The bundled
  benchmark measures attention cost and answer correctness under a fixed
  deterministic reasoner; it is not a latency comparison against RLM, and it
  does not license the claim that DCR is faster.
- Treating the reference implementation as production infrastructure. It is
  there to make the specification falsifiable, not to be depended on.
