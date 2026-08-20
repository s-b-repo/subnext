# Results

Measured output of the reference implementation, reproducible from this repo:

```bash
cargo run --release -- bench                        # the context-rot table
cargo run --release -- bench --baselines            # DCR vs RAG / summarize / recursive
cargo run --release -- bench --scaling --budget 800 # the flat-k table
cargo run --release -- bench --rebuild              # what does a workspace rebuild cost?
cargo run --release -- bench --tamper               # can the container detect tampering?
cargo test                                          # 151 tests, ~14s
```

Every benchmark is deterministic and offline. No API key, no network.

**Measured at `c17abf5`.** Benchmark numbers are pinned to a commit on purpose:
they move whenever retrieval, extraction or the planner moves, and a table
without a revision attached is a table nobody can reproduce. Re-run the commands
above after any change to `src/` and update this file with what they print —
including when the numbers get worse.

---

## Context rot

300-turn incident transcript, 300 documents, 27,362 tokens of history,
`B_attention = 1200`, sliding window = 8,000 tokens.

| | full context | sliding window (8k) | DCR |
|---|---:|---:|---:|
| correct | 5/7 | 2/7 | **7/7** |
| tokens/query | 27,362 | 7,968 | **462** |
| attention vs full history | 1x | 3.4x | **59x** |

Per probe:

| probe | full | window | DCR | DCR tokens |
|---|:---:|:---:|:---:|---:|
| corrected fact (mid-history) | ok | MISS | ok | 196 |
| corrected fact (late) | ok | ok | ok | 419 |
| old fact, never repeated | MISS | MISS | ok | 399 |
| exact quote | MISS | MISS | ok | 963 |
| justification / multi-hop | ok | MISS | ok | 192 |
| detail buried in a long span | ok | MISS | ok | 962 |
| corrected fact (very late) | ok | ok | ok | 102 |

Telemetry for the DCR column:

```
escalation_rate         : 0.143
stale_fact_read_rate    : 0.000
tokens_per_query_mean   : 461.9
tokens_per_query_max    : 963
budget_overflows        : 0
demotions               : 3
audit_path_completeness : 1.000
compression_ratio       : 59.1
prefetch_hit_rate       : n/a
wasted_builds           : 0
```

Escalations are counted and charged: a probe that needed to drop to L0 costs
more, and that cost is inside the DCR token column, not hidden beside it.
`prefetch_hit_rate` is `n/a` rather than `0.000` because these probes never
trigger a prefetch — a rate over zero attempts is undefined, and printing zero
would read as "speculation is failing" instead of "speculation has not run".

### How to read this table

**The benchmark is deliberately unflattering to DCR.** The same dumb
deterministic line-matcher reasons for all three systems, so this compares
*context assemblies*, not models. Full-context accuracy here is a **floor, not
a ceiling** — a real model reading the whole transcript would beat 5/7. Do not
read this as "DCR is more accurate than a long-context model."

The two load-bearing results are:

1. **The token counts.** 462 vs 27,362 is a property of what gets assembled,
   and no amount of model quality changes it.
2. **The window's misses.** They are structural. A fact that fell outside an
   8k window is unrecoverable at any model quality — that is the point of the
   `old fact, never repeated` and `exact quote` rows.

---

## Against baselines that also retrieve

Full context and a sliding window both fail the same way — they truncate by
position — so beating them proves less than it looks. These three retrieve:

| probe | full | RAG | summarize | recurse | DCR |
|---|:---:|:---:|:---:|:---:|:---:|
| corrected fact (mid-history) | ok | ok | MISS | ok | ok |
| corrected fact (late) | ok | ok | MISS | MISS | ok |
| old fact, never repeated | MISS | MISS | MISS | MISS | ok |
| exact quote | MISS | MISS | MISS | MISS | ok |
| justification / multi-hop | ok | ok | MISS | ok | ok |
| detail buried in a long span | ok | ok | MISS | ok | ok |
| corrected fact (very late) | ok | ok | ok | ok | ok |
| **correct** | 5/7 | 5/7 | 1/7 | 4/7 | **7/7** |
| **mean tokens/query** | 27,362 | 1,168 | 1,197 | 31,074 | **462** |

- **RAG ties full context at 5/7**, using the same hybrid index DCR uses, at
  2.5x DCR's token cost. This is the honest headline: plain top-k retrieval is a
  strong baseline. The gap is two specific probes — a fact never repeated (there
  is nothing for similarity to latch onto) and an exact quote (top-k returns the
  right chunk, not the right bytes).
- **Summarize-all collapses to 1/7.** Uniform compression at the same budget
  destroys exactly the details being asked for — the strongest evidence here
  that *selective* representation beats uniform compression.
- **Recursive costs more than full context** (31k vs 27k tokens), because it
  reads every chunk and then reads its own digest. Unlimited coverage is not
  free, and charging for it is the honest accounting.

### …and against probes built to make DCR lose

Beating truncation baselines proves little, and on the standard corpus RAG ties
full context — most of those probes are findable by similarity. This second
corpus is built so that similarity is actively misleading, and so that
*answering at all* is sometimes the wrong move:

| probe | full | RAG | summarize | recurse | DCR |
|---|:---:|:---:|:---:|:---:|:---:|
| lexical decoy on a stale fact | ok | ok | MISS | ok | ok |
| three-hop dependency (context) | ok | ok | MISS | ok | ok |
| stale derivation (must refuse) | MISS | MISS | ok | ok | **MISS** |
| absent fact (must refuse) | MISS | MISS | ok | MISS | **MISS** |
| contested fact (order decides) | MISS | MISS | MISS | MISS | **MISS** |
| **correct** | 2/5 | 2/5 | 2/5 | **3/5** | 2/5 |
| **mean tokens/query** | 27,382 | 1,133 | 1,180 | 31,180 | **237** |

**DCR does not win this table.** Recursive map-reduce beats it. The three
failures are the useful part:

- **Stale derivation.** A figure stated as text (`the incident cost estimate is
  2160 USD, computed from …`) is served after one of its inputs is corrected.
  Invalidation only tracks derivations the runtime actually *computed*; a number
  that arrived as prose is never recomputed and nothing marks it dead.
- **Absent fact.** Asked for a phone number that was never stated, DCR serves
  the adjacent paging-policy line. A window pre-filled with selected facts makes
  near-miss material easy to reach for — the cost of assembling eagerly.
- **Contested fact.** Two claims disagree and neither is phrased as a
  correction, so ingest order decides and no marker reaches the window. That is
  the documented `may_supersede` rule, and this row prices it.

The two probes it does pass are worth naming too. The **lexical decoy** answers
a question raised in `TODO.md`: the stale document repeats the query's words
four times and the correction states them once, and DCR still serves the
correction — supersession is load-bearing, not lexical luck. The **three-hop**
probe is scored on the assembled context rather than the answer, because the
harness's reasoner is a line-matcher and cannot perform a join; that scoring is
applied to every column equally.

---

## k stays flat while history grows

Same probes, same `B_attention = 800`, history scaled 33x:

| turns | history | nodes | mean k | max k | correct | ingest | query | ann query | ann k |
|---:|---:|---:|---:|---:|:---:|---:|---:|---:|---:|
| 100 | 8,482 | 124 | 416.7 | 764 | 7/7 | 0.02s | 0.6ms | 0.6ms | 416.7 |
| 300 | 27,362 | 298 | 411.6 | 787 | 7/7 | 0.06s | 1.6ms | 1.3ms | 451.7 |
| 1,000 | 93,442 | 911 | 398.6 | 793 | 7/7 | 0.22s | 3.5ms | 2.9ms | 355.6 |
| 3,000 | 283,253 | 2,661 | 413.3 | 795 | 7/7 | 1.04s | 14.0ms | 6.4ms | 346.9 |

History grew **33x**; active context grew **0.99x**, and accuracy held at 7/7 at
every size. That is the `O(k + r)` shape from the cost model, measured rather
than asserted — for the attention term. The `ann` columns are a second runtime
over the same corpus with LSH pruning enabled; see the known gap below for what
they do and do not show.

Storage still grows `O(N)` — 124 → 2,661 nodes. The claim is bounded
*attention*, not bounded storage.

---

## Workspace rebuild

"Destroy and rebuild the workspace at any time" is only a guarantee if rebuild
is cheap, so it is measured:

**Mean cold rebuild 1.66 ms; mean warm assembly 0.14 ms** (300 turns, 298 nodes).

Cold drops every cached representation and reassembles from L0 alone; warm is
the same query with caches populated. The 12x gap tracks how much L1 must be
rebuilt — probes admitting only cached facts rebuild nothing and cost the same
either way. Full table and caveats:
[workspace rebuild](docs/architecture/workspace-rebuild.md).

---

## Tamper detection

`bench --tamper` corrupts a container three ways and asserts each is caught:

| attack | detected | by |
|---|:---:|---|
| flip one bit in an object | yes | content address |
| …and the runtime refuses to load it | yes | gateway |
| rewrite a historical checkpoint | yes | hash chain |
| roll back to an older signed state | yes | generation high-water mark |

This probe earned its place: the first version of the chain digest covered only
the Merkle root and the delta, so a rewritten checkpoint passed. The probe caught
it, and the chain now covers the whole checkpoint body.

**What it does not show:** resistance to an attacker who rewrites the objects,
the chain, the manifest and the high-water mark together. Hashes make tampering
evident, not impossible. See
[context integrity §9](docs/architecture/context-integrity.md#9-what-this-does-not-defend-against).

---

## Known gap: end-to-end latency still grows, for a different reason

**Query latency is not flat: 0.6ms → 14.0ms across the scaling run**, and the
previous explanation for that was wrong. This file used to attribute it to the
vector index being a linear scan. An LSH index now prunes roughly 96% of the
vectors a query scores and takes the index call to a fraction of a millisecond
— and end-to-end latency still grows, only more slowly (14.0ms → 6.4ms at 3,000
turns). The cost has moved into planning, which has not been shown to be
sub-linear either.

So the `O(k + r)` claim stands for **attention** and remains unestablished
**end to end**. The retraction is worth stating plainly because the earlier
diagnosis was confidently wrong in exactly the way a benchmark is supposed to
catch.

The `ann k` column is the part not to skip. Approximate retrieval does not
merely find the same nodes faster — it assembles a partly *different* working
set, 10% larger at 300 turns and 16% smaller at 3,000. Correctness is identical
at every size, and `run_scaling` asserts that rather than assuming it, but four
rows do not establish equivalence.

---

## Spec-to-code mapping

Every module maps to a page in [`docs/`](docs/):

| module | page |
|---|---|
| `src/spans.rs` | L0 of the [representation ladder](docs/concepts/representation-ladder.md) |
| `src/ladder.rs` | [Representation ladder](docs/concepts/representation-ladder.md) |
| `src/graph.rs` | [Memory graph](docs/architecture/memory-graph.md) + [provenance](docs/architecture/provenance.md) |
| `src/budget.rs` | [Attention budget](docs/concepts/attention-budget.md) — the knapsack |
| `src/planner.rs` | [Relevance planner](docs/architecture/relevance-planner.md) |
| `src/speculation.rs` | [Speculative context](docs/concepts/speculative-context.md) — prefetch |
| `src/indexer.rs` | [State indexer](docs/architecture/state-indexer.md) |
| `src/policy.rs` | [Decision policy](docs/architecture/decision-policy.md) |
| `src/telemetry.rs` | Phase-5 metrics in the [roadmap](docs/roadmap.md) |
| `src/hash.rs`, `src/merkle.rs`, `src/context_store.rs`, `src/trust.rs`, `src/scrub.rs` | [Context integrity](docs/architecture/context-integrity.md) |
| `src/baselines.rs` | The falsification set in [audit-2026-08](docs/audit-2026-08.md) |

Invariants the wiki states as prose are enforced in code and covered by tests:

- an unsourced fact raises `ProvenanceError` — a claim cannot exist without a
  path down to raw spans (`graph.rs`, `tests/graph.rs`)
- nothing is ever deleted; superseded nodes are marked, not removed
- stale nodes never reach the model (`stale_fact_read_rate: 0.000`)
- demotion-beats-eviction falls out of the knapsack, verified against a
  brute-force optimum in the tests rather than hand-asserted
- an object's address is its content; an edited object cannot load
  (`tests/context_store.rs`)
- a repair may only use a replica that verifies independently (`tests/scrub.rs`)
- a derived hypothesis never renders like an observation (`tests/evidence.rs`)

`audit_path_completeness: 1.000` means every answer above resolves to raw source
spans via `rt.explain(...)`.

---

## Scope note

[`docs/roadmap.md`](docs/roadmap.md) listed "shipping a library from this repo"
as a non-goal, and this implementation crosses that line. It is framed as a
**reference implementation that makes the spec falsifiable** — a way to check
whether the invariants hold when they meet real code — not as production
infrastructure. Easy to revert if you disagree with that call.
