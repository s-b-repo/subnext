# Alternatives

Designs the runtime could have taken instead, and why it did not — or did.
Written from first principles rather than as a defence of what already exists,
because a design nobody argued against is a design nobody checked.

Each entry ends in one of three verdicts:

- **adopted** — it is in the runtime
- **rejected** — the complexity is not repaid
- **open** — plausible, unmeasured, and worth trying

---

## Memory representation

### Event sourcing as the primary store — *partly adopted*

Store only an ordered log of events and derive every view from it. Nothing is
ever updated; state is a fold over history.

DCR already has the load-bearing half: L0 is append-only, corrections add spans
plus a `supersedes` edge, and the transcript is an audit log rather than working
memory. What it does *not* do is derive the graph from the log on every read —
the graph is materialised and persisted alongside.

Full event sourcing would make rebuild `O(N)`, which is exactly the property
[workspace rebuild](architecture/workspace-rebuild.md) exists to avoid. The
compromise — append-only log, materialised graph, both durable — keeps
reconstructability without paying replay cost on every open.

### CRDTs for concurrent memory — *rejected*

Make the graph a conflict-free replicated data type so several agents can write
concurrently without coordination.

Rejected for this design: CRDTs resolve conflicts *automatically*, and this
architecture's whole position on conflict is that it must **not** be resolved
automatically. Two disagreeing claims are kept, marked `Contradicted`, and put in
front of the reasoner to adjudicate. A last-writer-wins register would silently
pick one, which is the failure mode `contradicts` edges exist to prevent.

Worth revisiting if multi-agent shared memory becomes a goal — but then the CRDT
should merge the *set of claims*, never the truth of them.

### Vector database as the primary store — *rejected*

Keep everything as embeddings; retrieve by similarity; treat text as a payload.

This is the design DCR argues against. Similarity finds material that *sounds*
relevant; it does not find the correction three turns later that made the
retrieved fact wrong, and it cannot follow "why did we decide this". The
benchmark makes the cost visible: plain top-k RAG over the same corpus scores
5/7 while DCR scores 7/7, missing exactly the probes that need supersession and
exact bytes.

Vectors are kept — as L2, one rung of the ladder and one of several retrieval
signals, not the substrate.

### Hierarchical / tiered memory (hot, warm, cold) — *rejected as redundant*

Move material between storage tiers by access frequency.

The representation ladder already does this along a better axis. Tiering asks
"how likely is this to be read?"; the ladder asks "what is the cheapest form
that still answers the question?" The second subsumes the first — a rarely-read
node simply never has its L1 materialised — and it does so without a background
migration process that can be interrupted halfway.

---

## Retrieval and planning

### Learned retrieval (train a retriever end to end) — *open*

Replace the hand-weighted `U(x)` with a model trained on which admissions
actually got cited.

Genuinely promising, and deliberately not done yet. The reason is stated in
[open question #7](open-questions.md): a wrong `U` fills the window with cheap
useless material *and looks efficient while doing it*. A hand-weighted sum with
every term named can be traced by `dcr plan --explain` to the term that caused a
bad admission; a learned scorer cannot, until there is enough labelled traffic
to evaluate it against.

The hybrid worth trying: keep the deterministic terms, learn only their weights,
and keep the per-term attribution.

### Query planning as search rather than knapsack — *rejected*

Treat context assembly as a search over candidate working sets (beam search,
MCTS) instead of a single constrained optimisation.

Rejected on cost-benefit. The knapsack is solved exactly, in milliseconds,
against a budget — and is verified against a brute-force optimum in the tests.
Search would buy the ability to model interactions between admissions (this span
is redundant *given* that fact), which the current cost model ignores. That is a
real limitation, but the fix is a better utility function, not a more expensive
optimiser over the same bad one.

### Two-stage retrieve-then-rerank — *rejected as already present*

Retrieve broadly, then rerank with a stronger model.

This is what seeding plus expansion plus the knapsack already is, with the
reranker being deterministic instead of a model call. Adding a model to the
reranking step would put a model call on the critical path of every turn, which
is the cost the two-system split exists to avoid.

### Parallel workspace assembly — *open*

Materialise candidate representations concurrently while the planner is still
deciding.

Attractive on paper: `bench --rebuild` shows materialisation is the term that
grows (8 ms on the span-heavy probe, against 0.14 ms when nothing needs
rebuilding). Not done because the runtime is deliberately synchronous and
single-threaded, and because speculation already covers the predictable half of
this — it prefetches what the *next* turn will likely need rather than
parallelising the current one.

The honest reason to wait: at 2 ms mean rebuild, parallelism optimises something
that is not yet the bottleneck.

---

## Consistency

### Full transactions over the memory graph — *rejected*

Wrap each turn in a transaction with rollback.

Snapshot isolation plus an interrupt gets the property that matters — a
background consolidation cannot silently change the meaning of an active
workspace — without a transaction manager. Rollback specifically is the wrong
primitive here: this store is append-only, so "undo" means *supersede*, which is
already how corrections work and leaves the audit log intact.

### Optimistic concurrency with retry — *adopted, in the narrow form*

Record the version a plan was made against; check it after the answer; re-plan
if anything in the working set was invalidated.

This is exactly what `Dcr::ask_with_consolidation` does. It is optimistic
concurrency, scoped to one turn, with the working set as the read set.

---

## Integrity

### Sign the whole store instead of a Merkle root — *rejected*

One signature over one serialised state.

Simpler, and it makes every incremental operation expensive: verifying one
object means reading everything, and repairing one object means re-signing
everything. The Merkle root gives `log n` inclusion proofs, which is what makes
the scrubber affordable enough to run in the background.

### Blockchain / external notarisation — *open, deferred*

Anchor checkpoint roots in an external append-only log so rollback is detectable
even by an attacker who controls the local disk.

This is the correct answer to the limitation stated in
[context integrity §9](architecture/context-integrity.md#9-what-this-does-not-defend-against):
a local high-water mark is only as trustworthy as the disk holding it. An
external witness — a notary, a transparency log, or simply a second machine —
removes that dependency.

Deferred rather than rejected: it adds a network dependency to a deliberately
offline library, and the format already records enough to add it later.

### Erasure coding instead of full replicas — *rejected for now*

Reed–Solomon across N shards for durability without N full copies.

Correct at scale and wrong here. Verified replicas cover the same need for a
research library, and hand-rolling Reed–Solomon next to hand-rolled SHA-256
compounds risk in a codebase whose selling point is that it can be read end to
end. Revisit when a store is large enough that replication cost actually hurts.

### Encrypt everything at rest by default — *rejected as a default*

Make the container encrypted unless told otherwise.

Rejected because a default that cannot be honoured is worse than an absent
feature. No AEAD is bundled ([and for good reason](architecture/context-integrity.md#4-signatures-and-their-states)),
so an "encrypted by default" flag would either be a lie or would force a
dependency on every user who does not need confidentiality. The trait is there;
the store records honestly that it is unencrypted.

---

## The pattern in these rejections

Three of them (CRDTs, vector-primary storage, encrypt-by-default) are rejected
for the same reason: they resolve something automatically that this architecture
believes should be surfaced. Conflicting facts, missing evidence, and absent
protection are all *findings*, and a design that smooths them over produces a
system that is easier to demo and harder to trust.
