# Where this fits

DCR is built for one shape of problem: **a conversation or process that keeps
producing facts, where earlier facts get corrected, and where the answer has to
be traceable.**

If your workload is not that shape, the machinery costs more than it returns.
This page is as specific about the poor fits as the good ones, because a design
that is recommended for everything has not been characterised.

---

## Good fits

### Long-running agent loops

The original motivation. An agent working a task across hundreds of turns
accumulates state faster than any context window holds it, and the failure is
silent: a fluent, confident answer built on a detail that was corrected three
hundred turns ago. Nothing in the output distinguishes it from a correct answer.

DCR's response is that every answer resolves to raw source spans, so a wrong
answer is attributable rather than mysterious, and
`audit_path_completeness` is recorded per run rather than assumed.

### Anything where a correction has to win

Incident response, ops runbooks, case management, long-lived tickets, anything
with a "actually, scratch that" turn in it.

Revision is a first-class event here: nothing is deleted, a correction records a
`supersedes` edge rather than overwriting, and superseded material is excluded
from planning while staying in the record. See
[correction as a first-class event](architecture/memory-graph.md).

The measurement that matters is `bench --mutate`, which establishes a fact,
references it until it has dependents, then supersedes it — and includes an
adversarial variant where the *superseded* value is the better lexical match for
the query, so only the supersession edge can produce the right answer. That case
exposed a real defect: the claim layer was excluding the stale value correctly
while the evidence node sourcing it carried the old text into the window anyway.

### Regulated or auditable settings

An unsourced fact cannot enter the graph — insertion raises rather than
degrading — and every answer walks back to the spans that grounded it. See
[provenance](architecture/provenance.md).

Correctness in the benchmark is *gated* on that path being complete, so an
answer that contains the expected value but cannot be traced scores as a
failure. On an honest run this changes nothing, which is the point of having it.

### Tamper-evident record keeping

The `.context` container stores content-addressed objects under a Merkle root,
with checkpoints chained so that editing history invalidates every generation
after it, and a generation high-water mark that refuses a rollback. An edited
JSON store reloads silently; an edited container does not load.

Useful where the question is not only "what did the system know" but "can anyone
demonstrate it was not edited afterwards". Without a signer this is
tamper-*evident*, not tamper-proof, and
[the limits are stated](architecture/context-integrity.md).

### Cost-sensitive work over large histories

4.19 million tokens of history answered from 235 tokens per query, with 48,651
state nodes. That is a different cost structure from feeding a long window, and
unlike a sliding window it does not lose old material permanently — a fact
outside a window is unrecoverable at any model quality, while a fact outside the
current working set is one plan away.

---

## Poor fits

### Short, single-turn work

One document, one question, no corrections. The indexing, the graph and the
knapsack are pure overhead. Use ordinary retrieval, or put the document in the
prompt.

### Tasks needing the full text verbatim

DCR deliberately serves the cheapest sufficient representation. It escalates to
raw bytes when a probe demands an exact quote, and charges the turn for it — but
if *most* of your queries need the whole source, bounded attention is the wrong
objective for you.

### Semantic search over paraphrase

The bundled embedder is a 256-dimensional hashing embedding, not a learned one.
It finds material sharing vocabulary, and the lexical and vector channels are
therefore correlated rather than independent evidence. Genuine paraphrase
retrieval needs a real embedding model swapped in; the interface accepts any
`str -> Vec<f32>`.

### Reasoners that cannot signal

The [escalation protocol](architecture/decision-policy.md) assumes the model can
say "this compressed form is insufficient, give me the source". A harness with no
way to emit that signal loses the mechanism that carries the exact-quote and
buried-detail probes — the ablation shows correctness dropping to 6/7 with
escalation disabled.

### High-concurrency writes

The consistency path — snapshot isolation with an interrupt — is exercised by
tests and priced by `bench --consolidate`, which lands a write mid-turn and
measures the replan. But it is single-threaded. Nothing runs two turns at once
and no lock is exercised. Do not deploy under concurrent ingest on the strength
of these numbers.

### Very large N where planning cost matters

Retrieval is no longer the bottleneck: an LSH index prunes roughly 96% of scored
vectors and the index call drops to a fraction of a millisecond. Query latency
still grows, and the remaining cost is in the planner. At 4.19M tokens that is
33ms per query, which is fine. Whether it stays fine an order of magnitude
further up is not established, and is
[an open question](open-questions.md).

---

## If you are evaluating it

Run the ablation first:

```bash
cargo run --release -- bench --ablate
```

It reports which mechanisms carry which probes, **including two that carry
nothing on the standard corpus**, and it will tell you faster than any prose
whether the parts you care about are doing work on *these* probes, on *that*
corpus. Your workload is precisely the thing it does not measure — but it is
also the cheapest thing here to re-run against your own material, which is the
only way to answer the question for you.

Then run `bench --diverse` if your histories are large, and read the note under
it: the standard corpus emits 21 distinct documents at any size — thirteen
occurring once, eight repeating — so a large-token figure from it measures
length rather than difficulty. At 30,000 turns each of those eight appears
around 3,750 times.

The check that separates the two costs one line — **does the count of distinct
documents grow with N, or only the token count?** — and is worth running on any
corpus whose size is part of the claim, including ones that have nothing to do
with this system. That is how the problem was found here: not by reasoning about
the generator, but by counting what it emitted.
