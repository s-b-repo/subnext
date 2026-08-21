# Reference Implementation

`src/` in this repository is a working implementation of the specification on
these pages. Rust, zero dependencies, runs offline. Its purpose is to make the
design **falsifiable**: every claim on these pages that could be wrong is either
enforced by code or measured by the bundled benchmark.

```bash
cargo run --release -- demo                            # one task through all four levels
cargo run --release -- bench                           # DCR vs full context vs sliding window
cargo run --release -- bench --scaling --budget 800    # does k stay flat while N grows?
cargo run --release -- bench --baselines               # vs RAG, summarize-all, recursive
cargo run --release -- bench --rebuild                 # what does a workspace rebuild cost?
cargo run --release -- bench --tamper                  # can the container detect tampering?
cargo test
```

Zero dependencies, with one qualification: SHA-256 is implemented in-tree and
checked against the published NIST vectors, because a hash is the one
cryptographic primitive that can be verified against known output. Signatures
and authenticated encryption are traits with no bundled implementation — see
[context integrity](architecture/context-integrity.md).

## What the code enforces that prose cannot

| Spec statement | How it is enforced |
|---|---|
| "Reject unsourced facts at `upsert` time" | `ProvenanceError` — every non-evidence node must reach evidence |
| "Nothing is deleted; revision is `supersedes`" | superseded nodes stay in the graph and are excluded from planning |
| "A `stale` node is never placed in the active context" | planner skips them and records them in `stale_seen` |
| "Demotion beats eviction" | a property of the knapsack, not a heuristic sort |
| "Bound to the budget" | `Σ cost ≤ B_attention`, verified by tests including brute-force optimality |
| "Speculation must not starve the active path" | prefetch has its own materialisation budget, never attention tokens |
| "Only L0 addressing is eager" | L1/L2/L3 built on first demand, build counts reported |
| "An object's address is its content" | an edited object fails its digest and the container refuses to load |
| "Never repair from an unverified replica" | a replica must hash to the address *and* prove against the committed root |
| "A hypothesis is not an observation" | `origin=` is rendered whenever it is anything but `observed` |
| "No context enters trusted reasoning unless…" | the five conditions live in one place, `ContextGateway::admit` |

## What it measures

`O(k + r)` is a target, not a result — so the runtime reports the numbers that
would show it failing: escalation rate, stale-fact read rate, prefetch hit rate,
tokens per resolved query, audit-path completeness, demotions and budget
overflows.

On a 300-turn synthetic incident transcript (27k tokens of history,
`B_attention` = 1200), attention cost is 145 tokens per query — 189x less than
the full history — and stays flat (417 → 413 tokens) while history grows 33x to
283k tokens.

Against baselines that also retrieve rather than truncate, DCR answers 7/7
probes; plain top-k RAG answers 5/7 at 2.5x the tokens, uniform summarisation
1/7, and recursive map-reduce 4/7 while costing more than full context. The
architecture earns its keep on two probes specifically — a fact that was never
repeated, and an exact quote — which is a narrower claim than the table's
headline suggests and is stated that way in [RESULTS.md](../RESULTS.md).

On a second corpus built so that similarity is misleading and refusing is
sometimes correct, **DCR scores 2/5 and loses to recursive map-reduce**. It
serves a derived figure whose inputs were corrected, and it answers a question
nothing in history addresses. Both failures are real and neither is fixed; they
are the most useful output the benchmark has produced.

Destroying and rebuilding the workspace costs 1.55 ms cold against 0.15 ms warm,
which is what makes "reconstructible at any time" a usable property rather than
a slogan. See [workspace rebuild](architecture/workspace-rebuild.md).

## Where the implementation had to decide something the spec left open

- **`sufficient(L, q)`** — query-type routing plus a measured escalation
  protocol (`#ESCALATE <node_id>` → re-plan with that node pinned at L0). See
  [decision policy](architecture/decision-policy.md) and
  [open questions](open-questions.md) #1.
- **L2's prompt payload** — the vector is the index key; the *state object* is
  what the model reads. See [representation ladder](concepts/representation-ladder.md).
- **`U(x)`** — a named, inspectable weighted sum, printed per node by
  `explain_plan()`. See [attention budget](concepts/attention-budget.md) and
  open question #7.
- **Cascade bounds** — eager up to `max_cascade`, tail deferred to a lazy queue.
  Was open question #5, now resolved.
- **Consistency** — snapshot version per plan plus a post-answer interrupt that
  forces a rebuild. Was open question #9, now resolved.
- **What an object's identity is** — content, and nothing else. The generation
  and the read counters live outside the hashed body, because an append-only
  store must grow with knowledge rather than with writes or reads. See
  [context integrity](architecture/context-integrity.md).
- **How far integrity goes** — tamper-evident, not tamper-proof, and the crate
  says so rather than shipping a signer it cannot vouch for.

## What it does not settle

Nothing here resolves the honest research surface. Extraction can still cache a
wrong claim; compression still drops what mattered three turns later; retrieval
in this implementation is a linear scan, so the cost model's third condition
(sub-linear retrieval) is not yet met at scale; and the container detects
tampering without preventing it. See [open questions](open-questions.md), the
[failure modes](architecture/failure-modes.md) catalogue, and the limitations
section of `IMPLEMENTATION.md`.
