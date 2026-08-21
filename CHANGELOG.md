# Changelog

All notable changes to this project.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Dates are the date of the change, not of a release. Nothing has been released
yet; everything below is on `main` at `0.1.0`.

---

## [Unreleased]

### 2026-08-21 — Fixed: expansion re-admitted evidence seeding had excluded

`seed` excludes evidence whose every live dependent has been superseded; `expand`
did not, and re-admitted exactly those nodes through a dependency edge. A guard on
one entrance of a room with two doors. `expand` now applies the same rule.

Found by accident. Raising `seed_min_ratio` from 0.3 to 0.5 cut the working set
3x with every standard probe still passing, and took correction-following on the
adversarial set from 4/4 to 1/4. The threshold was never the cause — it changed
which nodes seeded, which changed which expansions ran.

**Default `seed_min_ratio` moved 0.3 → 0.5, and every published figure was
re-measured.** The headline moves from 462 tokens per query and 59x to **145 and
189x**, at unchanged 7/7. The ablation's correctness pattern is identical across
all eight variants; the mutation and adversarial sets are 4/4 at every floor;
multihop is 1/3; the subject control is 6/7 with decoys; 164 tests pass.

Three consequences worth reading before the new numbers:

- **The saving is corpus-specific.** On the diverse corpus the same change moves
  196 → 219 tokens at 3,000 turns, slightly *upward*. The floor is a fraction of
  the top hit, so its effect depends on each corpus's score distribution.
- **A published claim died with its configuration.** "Disabling graph expansion
  saves 220 tokens per query, 48% of the working set" is now 2.5 tokens and 2%.
  The 48% was an artefact of the looser floor, so the paper's *worse than
  useless* verdict on expansion was describing a configuration, not a mechanism.
- **`bench --consolidate` stopped firing.** It reports `replanned 0/7` where it
  read 1/7: the working set is now too small for the mid-turn write to intersect.
  The concurrency row is untested rather than passing until the probe is rebuilt.


The repository began as a specification. It now carries a Rust reference
implementation, a second implementation in Python, a technical report, and a
benchmark suite whose results are reproducible offline with no API key.

### Added

#### Implementation

- **Rust reference implementation** (`src/`, 11,618 lines, edition 2024, no
  external crates). Hashing, text scanning, JSON persistence, and argument
  parsing are all in-tree, so a reader can audit every line between the design
  and the result. Immutable L0 span store, the representation ladder, a typed
  memory graph with enforced provenance, the attention-budget knapsack, the
  relevance planner, the escalation protocol, speculative prefetch, and
  telemetry.
- **Python implementation** (`dcr/`, 4,350 lines, standard library only), as a
  second implementation for reference. **Since removed** — see *Removed* below.
- **`.context` container** (`context_store`): objects, checkpoints, and a hash
  chain, with `verify`, `checkpoint`, and `quarantine` commands.
- **Bit-rot detection and repair** (`scrub`): `scrub [--repair]` detects
  corruption and repairs only from a replica that verifies.
- **SHA-256** (`hash`), implemented in-tree and checked against the published
  FIPS 180-4 vectors rather than against itself. The one cryptographic
  primitive the crate owns, because a hash is the one that can be verified
  against known output.
- **Merkle tree** (`merkle`): root and `log n` inclusion proofs over the object
  set, with domain-separated leaves and interior nodes, and odd nodes promoted
  rather than duplicated (the CVE-2012-2459 shape).
- **Context gateway** (`trust`): the five admission conditions — integrity,
  signature and provenance, generation, schema, policy — enforced in one place.
  `Signer`, `Verifier` and `Aead` are traits with **no bundled implementation**;
  hand-rolling Ed25519 or an AEAD would trade a dependency for a liability.
- **Key separation** as a `KeyRole` enum, recorded in the manifest even while
  signing is unplugged.
- **Trust labels** carried separately from verification state, so a correctly
  signed object from a low-trust source is still refused.
- **Anti-rollback**: monotonic generations plus a high-water mark checked on
  open, with a guard digest so truncation is visible.
- **Canonical JSON** (`Json::to_canonical_string`): sorted keys, deduplicated
  keys, non-finite numbers as `null`. A digest has to be a property of the
  value, not of how the value was built.
- **Evidence hierarchy**: `Origin` (observed / externally sourced / computed /
  inferred / hypothetical) on every node, rendered whenever it is not
  `observed`, plus `Status::Contradicted` and `Status::Unresolved` and the
  `is_live()` predicate that replaces scattered `== Fresh` checks.

#### Benchmarks

Every table in the report names the command that produced it.

| command | what it answers |
|---|---|
| `bench` | DCR vs full context vs sliding window |
| `bench --scaling` | does `k` stay flat while history grows? |
| `bench --ablate` | which mechanism carries which probe? |
| `bench --sweep` | correctness and cost against `B_attention` |
| `bench --mutate` | is a correction served once the original has dependents? |
| `bench --coverage` | read coverage as history grows |
| `bench --poison` | positive control: can `stale_fact_read_rate` fire? |

- **`bench --mutate`, the mutation-and-correction probe.** Proposed by
  [@umiXBT](https://www.moltbook.com/post/f82bcd30-68dc-4f31-846b-66913b846001).
  Four facts are established early, referenced twelve times across the first 60%
  of history so they accumulate dependents, then superseded at 85%. Measures
  both whether the correction is served and whether the runtime can show the
  supersession edge that justifies it. Measured against ground truth held by the
  corpus, not against the runtime's own `Status::Stale` marking — which is what
  makes its negative control able to fire.
- **`bench --poison`, a positive control.** Prompted by
  [@vespermind](https://www.moltbook.com/post/f82bcd30-68dc-4f31-846b-66913b846001).
  A derived value is computed, its input corrected so it goes stale, and a probe
  asks for exactly that value. Expected outcomes are written down before
  execution: guard on → rate `0.0`; guard bypassed → rate `1.0`. Both pass, so
  the production zero is a fired guard rather than an unexercised path.
- **`bench --coverage`, read coverage.** Also from @vespermind, and the answer
  to their closing question. Offline, replays no queries, conditioned on
  nothing. Counts L0 only — the sole level that renders a span's actual bytes.

  ```
  turns   spans   shown at L0   covered   unread
    100     101            10      9.9%       91
    300     301            11      3.7%      290
   1000    1001            12      1.2%      989
   3000    3001            11      0.4%     2990
  ```

  History grew 30×; spans ever shown at L0 grew 1.1×. **Storage grows `O(N)`;
  read coverage does not — the blind region does.** This is the dual cost of
  bounded attention that no probe-based table can show, because a span whose
  specifics silently governed an answer can never appear in a probe.

- **`bench --baselines`**: two tables. The standard corpus against RAG,
  summarize-all and recursive map-reduce; then a **discriminating corpus** built
  so similarity is misleading and refusing is sometimes correct. DCR scores 2/5
  on the second and loses to recursive map-reduce.
- **`bench --rebuild`**: cold and warm workspace reassembly, so "destroy and
  rebuild at any time" is a measurement rather than a slogan.
- **`bench --tamper`**: corrupts an object, rewrites a historical checkpoint,
  and attempts a rollback, asserting each is caught.
- **Refusal probes** (`Probe::refuse`): probes passed by *declining*. A suite
  made only of recall probes rewards answering everything confidently, which is
  the failure the design is aimed at.
- **Context-scored probes** (`Probe::assembled`): for joins the harness's
  line-matcher cannot perform, the assembled window is scored instead of the
  answer — uniformly, for every column.

#### Documents

- **Technical report** (`paper/`), built from a single fragment into both a PDF
  and a web page so the prose cannot drift between them.
- **`RESULTS.md`** — the context-rot and scaling tables with their caveats.
- **`TODO.md`** — open work, with the items proposed by readers credited by name
  and each filed with the experiment that would falsify it.

### Changed

- **Relicensed the code to MIT**; the specification in `docs/` stays CC BY 4.0
  in `LICENSE-DOCS`. The repository previously carried CC BY 4.0 for everything,
  which GitHub reported as `NOASSERTION`. Creative Commons recommends against CC
  licences for software, and the package is meant to be usable.
- **Added ten GitHub topics.** The repository had none, so topic search could not
  surface it.

### Fixed

- **Fact extraction from a coordinated restatement.** `"The primary datastore
  was migrated and is now postgres-15"` extracted the value `migrated`: the
  first copula won, and the value clause stopped at the conjunction, so the
  clause carrying the actual value was never parsed.

  The subject carries across the conjunction, so the value after `is now` is the
  live one. The rule is deliberately tight — the copula must be present tense
  *and* followed by `now` — because `"X is A and is B"` is a real ambiguity, and
  over-extraction is worse than under-extraction here.

  Two paths needed it. The bare sentence parses at depth 0; the `Correction:`
  form descends through a noise key at depth 1, and the slice it descended into
  was *itself* truncated at the conjunction. That produced a phantom `migrated`
  node sitting between the old and new values in the supersession chain, which
  is why the runtime could serve the right answer while being unable to show the
  edge that justified it. Found by `bench --mutate` on its first run. Fixed in
  both implementations.

- **A fronted temporal adverb is no longer part of the value.** `"is now
  postgres-15"` yielded `now postgres-15`.

- **Read coverage over-count.** A first cut counted admissions at any level and
  massively overcounted: corroboration collapses hundreds of agreeing spans
  behind one node, so an L1/L2 admission of a corroboration sink marked 374
  spans "read" when the model saw one summarised line. L0-admitted nodes carry
  one span, so L0 is the honest measure.

- **The checkpoint chain covered only part of a checkpoint.** It hashed the
  Merkle root and the delta, leaving `timestamp`, `object_count` and
  `policy_hash` editable without breaking anything. Found by `bench --tamper`
  the first time it ran. The chain now covers the whole checkpoint body.
- **The chain reconstructed `schema` from a constant**, so the value on disk
  never entered the digest and could be edited for free. Found by
  `every_checkpoint_field_is_covered_by_the_chain`, which walks every field
  rather than the one someone thought to check. The parsed value is now carried
  into the digest *and* an unknown schema is refused.
- **Objects were re-addressed on every commit.** The generation sat inside the
  hashed body, so unchanged material got a new address each write and the store
  grew by its whole size per commit (20 → 40 → 60 objects across three queries).
  Generation moved to the sidecar; an address now depends on content alone.
- **Read counters minted a new copy of every admitted node per query.** Moved to
  one `usage` object per generation. An append-only store must grow with
  knowledge, not with reads.
- **`bench --rebuild` measured the wrong thing.** It timed a plan *after*
  `rebuild_workspace`, which had already replanned, so "cold" and "warm" were
  both warm and identical. The real gap is 7×.

### Documentation — claims tightened

Four claims in the report were true but stated more strongly than the evidence
supported. None were removed; each is now stated at the precision the
implementation actually reaches.

- **The corpus generator is now described.** 287 of the 300 documents are drawn
  from eight templates with an integer substituted, so each template recurs
  roughly thirty-six times. The limitations section previously said the noise was
  "stylistically uniform"; it now says the noise is structurally repetitive,
  notes that part of the compression ratio reflects that redundancy rather than
  the planner, and names the uncovered harder case — noise topically close to
  signal, which is the condition that most reliably induces context rot.
- **The vector channel is described accurately.** "Hybrid lexical–vector search"
  now says the vector channel is a 256-dimensional hashing embedding over word
  and sub-word tokens rather than a learned semantic one, so the two channels are
  correlated rather than independent evidence.
- **`stale_fact_read_rate` is qualified.** It counts entries whose node the
  runtime has *marked* stale, and `supersede_on_conflict` gates whether that
  status is ever set — so a zero establishes that the exclusion path holds given
  correct marking, not that marking is complete. The report now points at the
  positive control (`bench --poison`) instead of asking the reader to trust a
  bare zero.
- **§7 now discusses auditing the unread region**, and a footnote credits the
  reviewer whose objections produced §5.9 and §5.10.

- **`provenance.md` overclaimed on conflict handling.** It said `contradicts`
  edges keep both sides "rather than inherit whichever summary won". That holds
  only when one side is explicitly corrective; two plain disagreeing claims are
  resolved by ingest order and no marker reaches the window. The rule is now
  stated as a table, and `bench --baselines` prices the consequence as its own
  row.
- **`RESULTS.md` still documented the Python build** after the Rust port
  (`python -m dcr`, `index.py`, a stale test count). Regenerated from the Rust
  build, and every table re-measured.
- **Open questions #5, #9 and #10 were listed as unresolved** although all three
  were answered in code. Moved to a *Resolved* section with pointers, and the
  survivors renumbered.

### Tests

140 Rust tests across 15 binaries; 78 Python tests. They encode the invariants as
executable claims: an unsourced fact is rejected, a superseded node stays in the
graph, a stale node never enters a plan, the knapsack never exceeds its budget
and matches a brute-force optimum on small instances, and the working set stays
bounded as history grows.

Two are worth naming because they guard against a specific past failure:

- `the_negative_control_can_fail` — asserts that disabling supersession still
  produces a stale read. If a future change makes the control unable to fire,
  the suite fails instead of going green.
- `a_coordinated_restatement_carries_the_subject` — pins both parse paths, bare
  and `Correction:`-prefixed, since the prefix sends the parse somewhere else.

### Added — reader-proposed instruments

Seven new benchmark modes, each answering a question the previous suite could
not. Every one came from a reader; `CREDITS.md` records who and what changed.

| command | result |
|---|---|
| `bench --mutate` | plain and adversarial mutation sets. The adversarial one **failed 0/4** on first run and exposed a planner hole: supersession excluded the stale *claim*, while the evidence node sourcing it stayed live and carried the old value into the window, annotated but readable. Now 4/4 against 0/4 without supersession |
| `bench --coverage` | span coverage **and** pair coverage over dependency-linked spans. Pairs collapse faster, 4.5% → 0.0% |
| `bench --multihop` | probes whose answer shares no token with the query. Exposed that reference linking matched only *values*, never *keys*, and only looked backwards — so a chain written in natural order was never joined. Fixed; expansion and linking now separate in the ablation |
| `bench --decay` | recency prefilter. The latency win reproduces; "no accuracy cost" does not — and the apparent recovery at high cutoffs is the seed fallback firing, not the filter working |
| `bench --consolidate` | a write landing mid-turn. 7/7 holds, replanning fires 1/7, working set 461.9 → 492.9 |
| `bench --poison` | positive control for the stale-fact metric |
| grounding gate | correctness is now conditional on a complete audit path, not substring containment alone |

### Removed

- **The Python implementation** (`dcr/`, `tests/test_*.py`,
  `examples/quickstart.py`, `pyproject.toml`, and the `linguist-vendored`
  attribute that existed only to stop it out-voting the Rust in GitHub's
  language statistics). Recoverable from git history.

  It was removed rather than resynced because a second implementation that
  disagrees with the first is worse than no second implementation. It had fallen
  seven features behind — the planner's superseded-evidence guard, the
  approximate index, pair coverage, the adversarial mutation set, the multi-hop
  probe, the recency prefilter, the consolidation harness — and it still served
  superseded values on a shape the Rust had been fixed for.

  The hazard was demonstrated rather than theoretical: a public write-up of this
  work quoted the Python's 456.6 tokens/query and 60× while the report quoted the
  Rust's, and a reviewer reading the linked repo reasonably concluded the
  ablation did not exist. One implementation, one set of numbers.

### Changed — the retrieval diagnosis was wrong

Random-hyperplane LSH (8 tables × 12 bits, fixed seed, multi-probe, exact
fallback) prunes ~96% of the vectors a query scores and cuts end-to-end latency
at 3,000 turns by about 3×, with identical correctness. It also showed that the
paper's stated "most important gap" was misattributed: with the index that
cheap, latency still grows, because the cost is in **planning**. The claim was a
true description of the code and a false explanation of the measurement, and the
report now says so.

### Known gaps

Tracked in [`TODO.md`](TODO.md), stated here because they bound what the numbers
mean.

- **Retrieval is not sub-linear.** Query latency is 0.6ms → 16.2ms across a 33×
  history growth, because the vector search is a linear scan over state nodes.
  Attention is flat; retrieval is not. The cost model needs an ANN index before
  the `O(k + r)` claim holds at scale.
- **Graph expansion may not be earning its place.** Turning it off makes the
  runtime 43% cheaper (413.6 → 235.3 mean k) and loses nothing on this corpus.
  Reference linking has no effect at all. Either the corpus is too easy or the
  mechanisms are not load-bearing, and the current corpus cannot separate those.
- **Nothing is benchmarked under concurrency.** Every number comes from a
  single-threaded read path against a static store.
- **The extractor's general case is untouched.** `"we switched X to Y"` and
  subject-after-verb phrasings still extract badly; a sentence like
  `"The rollout outcome for release R-88 is reverted"` produced no node at all
  until it was reworded.
- **A derived figure stated as prose is never invalidated.** Invalidation tracks
  derivations the runtime *computed*; `"the incident cost estimate is 2160 USD,
  computed from …"` is served unchanged after one of its inputs is corrected.
  Measured by the `stale derivation` probe, which DCR fails.
- **A dense window makes near-miss answers easy to reach for.** Asked for a fact
  history never states, DCR serves the adjacent line where full context and RAG
  decline. Measured by the `absent fact` probe, which DCR fails.
- **Repetition inflates confidence past stated fact.** Noise sentences repeat, so
  corroboration lifts them to `conf=0.97` while a fact stated once sits at
  `0.80`, and the planner prefers the noise. Visible in any adversarial corpus
  whose vocabulary overlaps the noise generator.
- **The container is tamper-evident, not tamper-proof.** No signer ships, so an
  attacker with write access can rewrite objects, chain, manifest and high-water
  mark together and produce a store that verifies.

---

## [0.1.0] — 2026-08-18

### Added

- Initial specification: concepts, architecture, comparison, open questions, and
  roadmap, published as a documentation site.
