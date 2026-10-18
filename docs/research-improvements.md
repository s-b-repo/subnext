# Research-backed improvements (consolidated gap analysis)

This document packages the consolidated research findings as a design reference.

**Status as of 2026-10-18.** Each item carries its real state: `implemented,
measured` (a before/after number exists), `implemented elsewhere` (the
mechanism already lives in this tree under another name), `wired, inert`
(constructed but never consulted — no claim made), or `not subnext's` (the
ownership rule in `plan.md` Rev 3 assigns it elsewhere). A mechanism without a
number is not claimed as an improvement.

## Subnext improvements

### Context assembly & KV-cache integrity
- Static header, dynamic trailer — **implemented elsewhere, measured.** The
  mechanism is `ActiveContext::render_with_header` in `src/planner.rs`: stable
  blocks first, per-turn stats in a trailing line. Measured
  (`bench --cache`): shared prefix between consecutive turns 2.8% → 23.6%,
  ordering churn 2 of 12 surviving blocks. The `StaticHeaderDynamicTrailer`
  struct in `src/context_assembly.rs` describes this shape but is not the
  mechanism.
- Stable content ordering — **implemented elsewhere, measured.** Sort by node
  id within kind groups, in the same render. The `stable_content_ordering`
  string helper is not the mechanism.
- Escalation-as-append — **implemented, measured** (`ask_with_consolidation` in
  `src/runtime.rs`). An additive re-plan (escalation: pin more, nothing sent
  was falsified) renders as the previous prompt plus an appended `## ESCALATED`
  block, so the prefix is byte-identical by construction. Measured on the
  escalation scenario in `tests/runtime.rs`: shared prefix between the first
  and final prompt sent, **34.6% (full re-render) → 100.0% (append)**. An
  invalidating re-plan (consistency violation) deliberately takes the full
  re-render instead — its prefix contains falsified material and must not be
  reused; the discard is recorded on `RollbackConsistencyGuard`.

### Eviction & memory residency
- Reactivation-aware eviction — **implemented, measured** (2026-10-18). The
  planner's age prefilter is a real eviction decision, and it now consults
  `ReactivationPredictor` (`planner.reactivation_prune`, off by default; the
  predictor has to earn its keep in the A/B before it belongs in a default).
  Measured (`bench --reactivation`, 1,000 turns, both probe sets): plain
  recency at any nonzero cutoff **slows the turn** (0.44 → 2.9–3.2 ms —
  dropped seeds force the span-search fallback) at equal correctness; the
  predictor spares all 18 hot aged candidates and holds baseline speed
  (0.39–0.77 ms), 7/7 + 4/4. Verdict: reactivation dominates plain recency as
  a pruner, and both lose to not pruning when there is no pressure — the
  "when fancy eviction fails" finding, reproduced rather than assumed.
- Recency-weighted baseline — **implemented, measured** — it is the default
  (`recency_cutoff: 0.0`, near-LRU ordering), and the A/B above is the honest
  form of the fancy-vs-plain comparison.
- Partial-session eviction — **structurally subsumed**: the planner prunes at
  node/span granularity, strictly finer than session granularity; there is no
  whole-session eviction to replace. The `PartialSessionEvictor` type remains
  a design note for a future resident layer.

### Unified prediction & scheduling
- Combined residency scheduler (`CombinedResidencyScheduler`), cancellable
  speculative reads (`CancellableSpeculativeReads`), learned tiered placement
  (`LearnedTieredPlacement`) — **wired, inert, and partly not subnext's.**
  Cancellable reads and tier placement are IO-layer decisions: per the
  ownership rule ("subnext decides *what* should exist; peregrine decides
  *where bytes live*"), in-flight prefetch cancellation and GPU/RAM/SSD
  placement belong to peregrine, whose `Reactor`/`MoeCall` were examined in
  plan.md Rev 3 and deliberately left without a deadline path. Expert
  prediction is likewise peregrine's. The stubs here are arithmetic
  placeholders, not schedulers.

### Durability & correctness
- Rollback consistency (`RollbackConsistencyGuard`) — **implemented, honest
  scope.** Used by the consistency-violation path in `ask_with_consolidation`.
  It records *invalidation* (a discarded working set whose cached prefix must
  not be reused) and nothing else; an earlier version recorded fictional
  "clears" of transcripts and KV that nothing performed and nothing read.
- Recoverable session contract (`RecoverableSessionContract`), resident KV as
  provable claim (`ResidentKvClaim`) — **wired, inert.** No crash-recovery or
  KV-residency path exists in this runtime to prove them against; the claims
  are unconsumed until such a path exists.

### Authorization & capability gating
- External PDP/PEP split, scoped attenuable tokens, context-aware
  authorization, fail-closed default-deny, full audit logging
  (`src/authorization.rs`) — **wired, inert.** dcr has no tool-execution seam
  to enforce on: the tool loop, registry and policy gate are omni's (plan.md
  Phase 6), and dcr's own admission control is `src/trust.rs`, which admits
  nothing unsourced on the ingest path. `ContextAwareAuthorization.evaluate`
  currently ignores its inputs — a placeholder, not context-aware anything.

## Training-efficiency references (Unifur)

- Diffusion of Thought (NeurIPS 2024): https://arxiv.org/abs/2402.07754, code https://github.com/HKUNLP/diffusion-of-thoughts
- Chain-of-Thought Distillation: https://dl.acm.org/doi/10.1145/3775073.3775200
- GDPO (ICLR 2026): https://arxiv.org/abs/2510.08554
- GPU-budget: https://arxiv.org/html/2506.08935, https://arxiv.org/pdf/2504.12216

Related measured work in the unifur tree: a token-block diffusion substrate
(`src/tokenblocks.rs`) now exists — block-causal attention, per-block KV
caching, entropy-adaptive block sizing, entropy-gated halo, and a bounded
revision pass, all certificate-pinned (`dblocks verify --group tokenblocks`,
5 certificates). Measured across three seeds at CPU scale, none of the
adaptive mechanisms beat fixed-size blocks yet (adaptive ΔNLL +0.033 ± 0.080
at ~40% more wall-clock; confidence-vs-position curve flat); the machinery and
its instruments are in place for a real checkpoint to re-run them.
