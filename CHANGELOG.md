# Changelog

All notable changes to this project.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Dates are the date of the change, not of a release. Nothing has been released
yet; everything below is on `main` at `0.1.0`.

---

## [Unreleased]

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
- **Python implementation** (`dcr/`, 4,350 lines, standard library only). Kept
  as a second implementation for reference. Marked `linguist-vendored` so it
  does not out-vote the Rust in GitHub's language statistics — excluded from the
  badge rather than deleted.
- **`.context` container** (`context_store`): objects, checkpoints, and a hash
  chain, with `verify`, `checkpoint`, and `quarantine` commands.
- **Bit-rot detection and repair** (`scrub`): `scrub [--repair]` detects
  corruption and repairs only from a replica that verifies.

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

### Tests

134 Rust tests across 15 binaries; 78 Python tests. They encode the invariants as
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

### Known gaps

Tracked in [`TODO.md`](TODO.md), stated here because they bound what the numbers
mean.

- **Retrieval is not sub-linear.** Query latency is 5.9ms → 77.5ms across a 33×
  history growth, because the vector search is a linear scan over state nodes.
  Attention is flat; retrieval is not. The cost model needs an ANN index before
  the `O(k + r)` claim holds at scale.
- **Graph expansion may not be earning its place.** Turning it off makes the
  runtime 46% cheaper (467.3 → 252.1 tokens) and loses nothing on this corpus.
  Reference linking has no effect at all. Either the corpus is too easy or the
  mechanisms are not load-bearing, and the current corpus cannot separate those.
- **Nothing is benchmarked under concurrency.** Every number comes from a
  single-threaded read path against a static store.
- **The extractor's general case is untouched.** `"X has moved to Y"`,
  `"X was replaced by Y"`, and `"we switched X to Y"` will still extract badly.

---

## [0.1.0] — 2026-08-18

### Added

- Initial specification: concepts, architecture, comparison, open questions, and
  roadmap, published as a documentation site.
