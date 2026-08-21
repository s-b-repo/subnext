# Results

Measured output of the reference implementation, reproducible from this repo:

```bash
cargo run --release -- bench                        # the context-rot table
cargo run --release -- bench --baselines            # DCR vs RAG / summarize / recursive
cargo run --release -- bench --scaling --budget 800 # the flat-k table
cargo run --release -- bench --rebuild              # what does a workspace rebuild cost?
cargo run --release -- bench --tamper               # can the container detect tampering?
cargo test                                          # 164 tests, ~14s
```

Every benchmark is deterministic and offline. No API key, no network.

**Measured at `8b7162f`.** Benchmark numbers are pinned to a commit on purpose:
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
| tokens/query | 27,362 | 7,968 | **145** |
| attention vs full history | 1x | 3.4x | **189x** |

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
tokens_per_query_mean   : 145.1
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

1. **The token counts.** 145 vs 27,362 is a property of what gets assembled,
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
| **mean tokens/query** | 27,362 | 1,168 | 1,197 | 31,074 | **145** |

- **RAG ties full context at 5/7**, using the same hybrid index DCR uses, at
  8x DCR's token cost. This is the honest headline: plain top-k retrieval is a
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
| 100 | 8,482 | 124 | 240.6 | 764 | 7/7 | 0.02s | 0.2ms | 0.2ms | 240.6 |
| 300 | 27,362 | 298 | 145.1 | 249 | 7/7 | 0.05s | 0.2ms | 0.7ms | 267.1 |
| 1,000 | 93,442 | 911 | 145.1 | 249 | 7/7 | 0.23s | 0.3ms | 1.7ms | 287.3 |
| 3,000 | 283,253 | 2,661 | 145.1 | 249 | 7/7 | 1.10s | 1.0ms | 3.2ms | 278.3 |

The 100-turn row is higher than every row below it, which is not noise: the seed
floor is a fraction of the *top* hit, and on a small store the top hit is weaker,
so the floor sits lower and admits more. The working set falls as the store grows
and then holds exactly flat — 145.1 at 300, at 1,000 and at 3,000 turns.

### The range this table covers, and the range it does not

The standard corpus emits **21 distinct documents at any size** — ten fixed documents, three corrections and eight noise templates. Confirm it by running the generator and grouping its output rather than by reading the source: 13 shapes occur once and 8 repeat, at 300 turns and at 30,000 alike. Growing it grows
the transcript's *length* at fixed lexical *variety* — at 3,000 turns each of its
eight distractor templates already appears 373 times. Flat `k` here shows the
planner does not degrade with length at constant difficulty. It is not evidence
that the working set holds as the retrieval problem gets harder, and a bigger
token count from this generator would overstate what was asked of the system.

`bench --diverse` answers the harder question on a second generator: distractors
drawn from four vocabularies by mixed-radix decomposition, 18,432 distinct
documents rather than eight templates. Added alongside the standard corpus, never
replacing it — every other figure here comes from the standard one.

| turns | history | distinct | nodes | mean k | correct | ingest | query |
|---:|---:|---:|---:|---:|:---:|---:|---:|
| 3,000 | 150,648 | 3,000 | 2,032 | 219 | 7/7 | 1s | 6ms |
| 10,000 | 512,964 | 10,000 | 6,762 | 237 | 7/7 | 16s | 5ms |
| 30,000 | 1,564,065 | 18,445 | 18,473 | 221 | 7/7 | 78s | 47ms |
| 80,000 | **4,191,322** | 18,445 | 48,651 | **259** | **7/7** | 251s | 22ms |

**4.19 million tokens of history, 48,651 state nodes, 259 tokens per query, 7/7.**
Across a 28x growth on this corpus the working set moves 219 → 259.

Note what the seed floor does **not** do here. On the standard corpus raising it
from 0.3 to 0.5 cut the working set from 461.9 tokens to 145.1; on this one the
same change moved 196 → 219 at 3,000 turns, slightly *upward*. The floor is a
fraction of the top hit, so its effect depends on the shape of each corpus's
score distribution, and the 3x saving on the standard corpus is not a general
property of the setting. Reported here rather than left for a reader to discover
by running the harder corpus.

Two limits, stated rather than left to be found. The generator exhausts its
vocabulary at 18,432 combinations, so past ~18,000 turns documents begin to
repeat — the 80,000-turn row averages about four copies of each, far better than
21 and not unbounded. And the probe set is the same seven questions at every
size, so this measures whether a fixed retrieval task stays cheap as the haystack
grows, not whether `k` holds as the number of things worth knowing grows.

### The cost result revises the one above it

Standard corpus at 30,000 turns: 226s ingest, 169ms query. Diverse corpus at the
same document count: 15s and 11ms. The corpus **amplifies** that cost rather than being it: few reused keys
concentrate work into a handful of large buckets. An earlier version of this
paragraph said the growth was "largely a property of the corpus", which
overcorrected in the opposite direction from the claim it replaced. Three parts,
now separated:

- **`by_key`'s whole-bucket copy, filter and sort was a real cost, and is gone.**
  A `live_by_key` index over the non-superseded nodes per key (`c68d472`) is
  faster at every size against its own parent and behaviour-neutral across all
  six deterministic checks.
- **It was not the dominant cost.** Removing it bought 1.2–1.8×.
- **`reference_links` / `backfill_references` own what remains** — a full-node
  scan per proposed fact, present in both corpora. Not fixed; noted at the call
  site in `92f216b`.

**The work is quadratic; the wall-clock is not a reliable ruler for it.** Two of
us measured elapsed time and disagreed. Against `c68d472^` under heavy load:
0.73s → 5.83s → 40.14s at 1,000 / 3,000 / 6,000 turns, growing 8.0× then 6.9×.
At a later revision under light load: 0.24s → 1.11s → 3.76s, growing 4.6× then
**3.4×** — *sub*-quadratic at exactly the step where the first run is firmly
super-quadratic.

Counting node-visits in the two linking scans settles it, because a count does
not move with load:

| turns | node-visits | per fact | growth |
|---:|---:|---:|---|
| 1,000 | 1,002,300 | 1,002 | — |
| 3,000 | 8,584,863 | 2,862 | 8.57× for a 3× corpus |
| 6,000 | 33,914,820 | 5,652 | 3.95× for a 2× corpus |

against quadratic predictions of 9× and 4×. Per-fact visits scale linearly with
`N`, so total work is **Θ(N²)** — and that is required by the code rather than
merely observed in it: `reference_links` and `backfill_references` each iterate
`graph.nodes()` once per proposed fact.

So the split is **quadratic work, unreliable timing**. Both wall-clock runs were
the same quadratic code seen through an instrument that moves with load, which
is why neither could settle the question and the count can. Quote the visits,
not the seconds.

**One inverted index failing is not the approach failing.** An earlier attempt
was slower at scale *and* behaviour-changing — 457.1 tokens per query against
461.9 under the then-default seed floor — and was reverted. Unqualified, that reads as "indexing this does not
work", and the two results are opposite: that attempt **narrowed the candidate
set**, which is why Table 3 moved; `live_by_key` **preserves** it and maintains
it incrementally, which is why it is faster and neutral. The difference is the
lesson, not the failure.

History grew **33x**; active context grew **0.60x**, and accuracy held at 7/7 at
every size. That is the `O(k + r)` shape from the cost model, measured rather
than asserted — for the attention term. The `ann` columns are a second runtime
over the same corpus with LSH pruning enabled; see the known gap below for what
they do and do not show.

Storage still grows `O(N)` — 124 → 2,661 nodes. The claim is bounded
*attention*, not bounded storage.

---

## Which mechanisms carry which probes

`bench --ablate`, 300 turns, `B_attention = 1200`. One mechanism disabled per
row. This table lived only in the paper until now; it is here because two of the
poor fits in [use-cases](docs/use-cases.md) cite it.

| variant | correct | mean k | esc. | probes that fail |
|---|:---:|---:|---:|---|
| full runtime | 7/7 | 145.1 | 0.14 | — |
| no supersession | 5/7 | 177.1 | 0.14 | corrected fact (mid-history); corrected fact (late) |
| no reference linking | 7/7 | 140.7 | 0.14 | — (3% *cheaper*) |
| no escalation | 6/7 | 130.4 | 0.00 | detail buried in a long span |
| no seed floor | 7/7 | 638.9 | 0.14 | — (costs 4.4x the tokens) |
| no graph expansion | 7/7 | 142.6 | 0.14 | — (and 2% *cheaper*) |
| L2 only (no ladder) | 5/7 | 131.6 | 0.00 | exact quote; detail buried in a long span |
| harness cannot signal | 7/7 | 130.4 | 0.00 | — |
| &nbsp;&nbsp;+ runtime infers it | 7/7 | 145.1 | 0.14 | — |

### Two corrections this table makes to earlier claims

**Reference linking is not a no-op.** Table 6 of the paper reported this row as
identical to the full runtime and called it "no effect on this corpus". It is
not identical — 140.7 against 145.1 — and it was further from identical under the
old seed floor, where the gap was 457.1 against 461.9. Correctness is unchanged
either way, so the negative result stands, but the token column moves and the
word "nothing" was wrong.

**The `no escalation` row was scored against a control token.** With
`max_escalations = 0` the reasoner still emitted `#ESCALATE <node>`, nothing
consumed it, and the literal string `#ESCALATE clai_e36d7d2dd2bb` was returned
as the turn's answer — then scored for whether it contained the expected value.
The row could not have passed however reachable the answer was. A control that
cannot pass carries as little information as one that cannot fail, and this repo
has now found the same shape three times.

The runtime no longer returns an unserved escalation as an answer (it returns
the same "not in the active context" sentence the reasoner uses, and sets
`Answer::escalation_refused`). The row still reads 6/7, because the refusal is
still not the expected value — but it now fails for the reason the row is
supposed to be testing.

### The last two rows, and what they retract

`no escalation` disables the *runtime's* willingness to serve. The documented
poor fit is a different configuration: a **harness that cannot ask**. Those were
treated as the same thing and they are not.

`harness cannot signal` is the poor fit as written — `LocalReasoner::signal =
false`, so `#ESCALATE` is never emitted, while the runtime keeps the mechanism
enabled. It scores **7/7**. The buried-detail probe is answered from the L1
summary (`"exhausted after 7 attempts"`), and the exact-quote probe is carried by
the query-type router, which sends `QuoteExact` to L0 at plan time without any
model signal.

So the claim that a signal-less harness "loses the mechanism that carries the
exact-quote and buried-detail probes" **does not reproduce**. On this corpus
escalation carries nothing the router and the ladder do not already carry. That
is a third negative result alongside reference linking and graph expansion.

`+ runtime infers it` enables `Dcr::auto_escalate`, which reconstructs the
escalation decision inside the runtime from the query and the assembled window —
the same `policy::overlap` function the reasoner uses, so the two cannot drift.
It also scores 7/7, at 145.1 rather than 130.4. **It costs 15 tokens per query
and buys nothing measurable here.** It is off by default and is reported as a
mechanism without a demonstrated benefit on this corpus, not as a fix.

What would change that verdict is a probe whose answer exists only in raw bytes
that no router keyword reaches. The corpus does not currently contain one.

---

## Where planning time goes

`bench --stages`. Every latency figure in this file before this section was
measured at the outer edge of a turn, which made "planning" a residual rather
than a measurement — and that is how the previous, confidently wrong diagnosis
("the vector index is a linear scan") survived. The instrument was proposed by
[@cwahq](https://www.moltbook.com/post/78237a57-17ef-4c78-b05f-8c1e5a944196):
separate clocks per stage, and publish the rejected-candidate count at each one.

The clocks say scoring dominates and grows with history. The load-bearing column
is not a clock, though — timings on a shared machine carry more spread than the
effect. It is **source spans concatenated per query**, which is deterministic:

| turns | nodes | spans concatenated / query | L0 builds / query |
|---:|---:|---:|---:|
| 100 | 124 | 36 | 6.1 |
| 300 | 298 | 113 | 6.9 |
| 1,000 | 911 | 272 | 6.5 |
| 3,000 | 2,661 | 1,177 | 7.0 |

The candidate set is hard-capped at 120 and the rejection counts confirm the cap
never binds — so the planner was doing work linear in history over a *bounded*
number of candidates. The cause: `Ladder::available()` concatenated a node's
entire span list to compare its length against 40, and `Ladder::cost()`
concatenated it again to price an L0 admission the knapsack usually drops.
Corroboration collapses agreeing spans behind one node, so that list grows with
N. Both counts are now memoised on the node (`LevelCache::l0_sizes`, keyed on
span count and value length so new corroboration invalidates it).

The control, because a speed-up nobody can make disappear on purpose is not a
measured speed-up — `Ladder::memoise_l0 = false` restores the old behaviour:

```
Control, 3000 turns: with the L0 memo disabled the planner concatenates
3529 source spans per query; with it enabled, 612.
```

**This is a constant-factor result, not a complexity result.** The number of
concatenations per query is now flat in N (6.1 → 7.0), but each remaining build
still walks a span list that grows, so 612 is not 36. Planning has not been
shown to be sub-linear and the `O(k + r)` claim is still unestablished end to
end. What changed is that the cost now has a name and a clock on it.

---

## How independent are the two retrieval channels?

`bench --channels`. The claim that the lexical and vector channels are
"correlated rather than independent evidence" was in the paper and both
poor-fit lists, and had never been measured. Standard corpus, 300 turns, 298
nodes, top-20 per channel:

| | per probe |
|---|---:|
| shared by both channels | 3.6 |
| expected if independent (`K²/N`) | 1.3 |
| control, measured | 0.7 |
| **ratio, observed to expected** | **2.7×** |

Rank correlation, mean over the seven probes: **−0.24**, against a control of
**−0.80**.

Read rho against that control and not against zero: ranking absent items worst
makes any two mostly-disjoint lists correlate negatively regardless of order, so
the number carries the scoring convention as much as the data. The overlap ratio
is the load-bearing figure. Why the first control was wrong, and why 2.7x is an
upper bound rather than an estimate, are in
[the report](paper/dcr-bounded-attention.pdf) §5.14 — kept there rather than
repeated here, because two copies of an argument are two things to update and
one of them will be missed.

**The verdict: dependent, and much less so than the documentation implied.**
2.7× chance overlap is real dependence. But 195 of 220 ranked positions across
the probe set were surfaced by exactly one channel, so calling the two
"correlated rather than independent evidence" overstates it.

**What this does not measure is paraphrase**, which is the property the claim
exists to warn about — so 2.7x is an upper bound on the channels' agreement, not
an estimate of it. See §5.14.


## Workspace rebuild

"Destroy and rebuild the workspace at any time" is only a guarantee if rebuild
is cheap, so it is measured:

**Mean cold rebuild 1.55 ms; mean warm assembly 0.15 ms** (300 turns, 298 nodes).

Cold drops every cached representation and reassembles from L0 alone; warm is
the same query with caches populated. The roughly ten-fold gap tracks how much
L1 must be rebuilt — probes admitting only cached facts rebuild nothing and cost the same
either way. These are single-run figures on an idle machine; under load the same
binary spans 1.98–4.18 ms cold and 0.15–1.02 ms warm, so reproduce the *ordering*
rather than the milliseconds. Full table and caveats:
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

**Query latency is not flat: 0.6ms → 12.6ms across the scaling run**, and the
previous explanation for that was wrong. This file used to attribute it to the
vector index being a linear scan. An LSH index now prunes roughly 96% of the
vectors a query scores and takes the index call to a fraction of a millisecond
— and end-to-end latency still grows, only more slowly (12.6ms → 5.8ms at 3,000
turns). The cost has moved into planning, which has not been shown to be
sub-linear either.

So the `O(k + r)` claim stands for **attention** and remains unestablished
**end to end**. The retraction is worth stating plainly because the earlier
diagnosis was confidently wrong in exactly the way a benchmark is supposed to
catch.

The `ann k` column is the part not to skip. Approximate retrieval does not
merely find the same nodes faster — it assembles a partly *different* working
set, 10% larger at 300 turns and 16% smaller at 3,000. Correctness is identical
at every size, and `run_scaling` asserts that with a live `assert_eq!` — checked
by deliberately diverging the approximate path and confirming the benchmark
aborts, because the check was previously a `debug_assert!` that `--release`
compiled out, making it a control that could not fire inside the table it
guarded. Four rows still do not establish equivalence.

That check is reproducible, and worth reproducing rather than believing:

```bash
git worktree add /tmp/ctl HEAD          # never sabotage the working tree
# in the copy, force the approximate path to answer differently at one size
cargo run --release -- bench --scaling  # must abort, naming both figures
```

**It is not automated.** Four checks in this repository have turned out not to
be exercisable. Three were controls that could not fire — a metric that could
not be exercised in the configuration that disabled it, a pass reachable through
an unobserved shortcut, and a debug assertion compiled out of the release build
that ran it. The fourth was the verification of the third: it fired correctly,
on purpose, and kept no record a reader could repeat, so this file briefly
claimed a check that had to be taken on trust. That is the same failure,
committed while fixing an instance of it — which is why the procedure above is
written out rather than summarised.

All four were found by someone else asking a differently-shaped question about
something already checked, rather than by anyone re-running an existing check
more carefully. It is tempting to conclude from that "cross-review, not
tooling", and a fifth instance — in the editing script used to write this very
finding, which called `str.replace` without checking the pattern matched and so
produced a successful-looking run against an unchanged file — is the
counter-example. **Tooling found that one, because the retry failed loudly.**

So the narrower and more useful claim: tooling finds these exactly when it is
built to fail noisily, and most tooling is not. Of the five, two would have been
caught by that discipline alone — the compiled-out assertion and the silent
`replace` — and a third, the unexercisable metric, by its close relative of
running a control in the configuration where it must fire. The remaining two
needed someone to ask a different question.

"Find another reviewer" is expensive and often unavailable. **"Make your checks
fail loudly" is available this afternoon**, and it covers most of this list.

A narrow lint — no debug assertions in paths that only ever run under
`--release` — would have caught the third when it was written; `src/` currently
contains none, so such a gate would be green today and would fire the moment one
returns. It covers one of the four. The other three have no mechanical form that
anyone here has proposed.

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
