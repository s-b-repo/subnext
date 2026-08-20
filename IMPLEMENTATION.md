# DCR — implementation

A working implementation of the specification in [`docs/`](docs/): unlimited
external state plus a small, planned working set.

Rust, **zero dependencies** — no crates at all. Everything the runtime needs
(hashing, text scanning, JSON persistence, argument parsing) is small enough to
own, and a specification's reference implementation should be readable end to
end.

That claim has one qualification worth stating up front: SHA-256 is implemented
in-tree and checked against the published NIST vectors, because a hash is the
one cryptographic primitive that can be verified against known output.
Signatures and authenticated encryption are **traits with no bundled
implementation** — hand-rolling Ed25519 or an AEAD would trade a dependency for
a liability. See [context integrity](docs/architecture/context-integrity.md).

```bash
cargo run --release -- demo            # worked example through all four ladder levels
cargo run --release -- bench           # DCR vs full context vs sliding window
cargo run --release -- bench --scaling --budget 800   # does k stay flat while N grows?
cargo run --release -- bench --mutate  # is a correction served once the original has dependents?
cargo test                             # 151 tests
```

```rust
use dcr::Dcr;

let mut rt = Dcr::new(800);
rt.ingest(&std::fs::read_to_string("incident.log")?, None)?;

let answer = rt.ask("what is the server ip?", None);
println!("{} ({} tokens)", answer.text, answer.tokens);
println!("{}", rt.explain(&answer.cited[0])?);   // audit path down to raw spans
```

> **The Python implementation has been removed.** An earlier implementation of
> this same specification lived in `dcr/`. It is gone as of the commit that
> added this note; `git log -- dcr/` still has all of it.
>
> It was removed rather than maintained because a second implementation that
> disagrees with the first is worse than no second implementation. It had fallen
> seven features behind — the planner's superseded-evidence guard, the
> approximate index, pair coverage, the adversarial mutation set, the multi-hop
> probe, the recency prefilter and the consolidation harness — and it still
> served superseded values on a shape the Rust had been fixed for, so anyone
> running it got quieter and wrong answers from a repo whose report says
> otherwise.
>
> That hazard was not hypothetical. A write-up of this work quoted the Python's
> figures while the report quoted the Rust's, and a reviewer reading the repo
> reasonably concluded the ablation did not exist. One implementation, one set
> of numbers.

---

## What it does

The runtime never shows the model the history. Each turn it assembles a small
active context by solving a knapsack over (node, ladder level) pairs, and every
line it admits is traceable back to an immutable source span.

```text
ingest ──► L0 spans (immutable, addressed)
            │
            ├─► node extraction ──► typed state graph ──► contradiction / supersession
            │                              │
            └─► lexical index              ├─► L1 summaries   (lazy)
                                           ├─► L2 state objects + vectors
                                           └─► L3 derivations (memoised)
                                                       │
query ──► classify ──► seed ──► expand edges ──► level-assign ──► knapsack(B_attention)
                                                                        │
                                                          tiny active context ──► model
                                                                        │
                                                          escalate L2→L0 if insufficient
```

## Module map

| Spec page | Module | Notes |
|---|---|---|
| [state indexer](docs/architecture/state-indexer.md) | `src/spans.rs`, `src/indexer.rs` | eager L0 addressing, lazy everything else |
| [representation ladder](docs/concepts/representation-ladder.md) | `src/ladder.rs` | L0/L1/L2/L3 build, cache, cost |
| [memory graph](docs/architecture/memory-graph.md) | `src/graph.rs`, `src/nodes.rs` | typed nodes, `upsert/neighbors/explain/invalidate` |
| [provenance](docs/architecture/provenance.md) | `src/graph.rs` | grounding enforced at `upsert`, not documented and hoped for |
| [fact cache](docs/concepts/fact-cache.md) | `src/ladder.rs` (L2 payload) | `key = value · conf · spans` |
| [context graph](docs/concepts/context-graph.md) | `src/indexer.rs` reference linking | edges from value mentions, not similarity |
| [attention budget](docs/concepts/attention-budget.md) | `src/budget.rs` | exact multiple-choice knapsack |
| [relevance planner](docs/architecture/relevance-planner.md) | `src/planner.rs` | seed → expand → level-assign → bound → speculate |
| [decision policy](docs/architecture/decision-policy.md) | `src/policy.rs` | query-type routing, escalation, de-escalation |
| [speculative context](docs/concepts/speculative-context.md) | `src/speculation.rs` | online predictor, separate budget, feedback |
| [cost model](docs/concepts/cost-model.md) | `src/telemetry.rs`, `src/bench.rs` | measured, not asserted |
| [two-system split](docs/architecture/two-system-split.md) | `src/runtime.rs`, `src/llm.rs` | Reasoner sees only `k`; Memory Runtime needs no model |
| [context integrity](docs/architecture/context-integrity.md) | `src/hash.rs`, `src/merkle.rs`, `src/context_store.rs` | SHA-256, Merkle root, chained generations, anti-rollback |
| [context integrity](docs/architecture/context-integrity.md) | `src/trust.rs`, `src/scrub.rs` | the admission gateway; bit-rot repair from verified replicas |
| [failure modes](docs/architecture/failure-modes.md) | across the crate | detection / containment / recovery per mode |
| [alternatives](docs/alternatives.md) | `src/baselines.rs` | the assemblies DCR is measured against |

Supporting modules with no spec page of their own: `src/text.rs` (tokenisation
and the scanners that replace a regex engine), `src/embed.rs` (hashing
embeddings), `src/summarize.rs` (extractive L1), `src/index.rs` (BM25 + vector),
`src/execute.rs` (`C_t`), `src/json.rs` (persistence), `src/ids.rs`,
`src/tokens.rs`.

## Results

`cargo run --release -- bench` — 300 turns, 27,362 tokens of history,
`B_attention` = 1200, sliding window = 8000. Same deterministic reasoner for all
three systems, so this compares *context assemblies*, not models.

```text
probe                                    full  window    DCR   DCR tokens
corrected fact (mid-history)            ok    MISS    ok          196
corrected fact (late)                   ok     ok     ok          419
old fact, never repeated               MISS   MISS    ok          399
exact quote                            MISS   MISS    ok          963
justification / multi-hop               ok    MISS    ok          192
detail buried in a long span            ok    MISS    ok          962
corrected fact (very late)              ok     ok     ok          102
correct                                    5       2      7   of 7
mean tokens per query                 27362.0  7968.0  461.9
attention vs full history                 1x    3.4x    59x
```

`cargo run --release -- bench --scaling --budget 800` — the `O(k + r)` claim:

```text
  turns   history   nodes   mean k   max k  correct   ingest    query
  turns   history   nodes   mean k   max k  correct   ingest     query ann query    ann k
    100      8482     124    416.7     764      7/7    0.02s     0.6ms     0.6ms    416.7
    300     27362     298    411.6     787      7/7    0.06s     1.6ms     1.3ms    451.7
   1000     93442     911    398.6     793      7/7    0.22s     3.5ms     2.9ms    355.6
   3000    283253    2661    413.3     795      7/7    1.04s    14.0ms     6.4ms    346.9

history grew 33x; active context grew 0.99x
```

**Read these honestly.** The reasoner is a line-matcher, so the full-context
column is a floor, not a ceiling — a real model reading the whole transcript
would beat it on the retrieval probes. The load-bearing results are the ones no
model quality changes: the token counts, the flat `k`, and the sliding window's
misses, which are structural (a fact outside the window is unrecoverable at any
model quality).

## Persistence: two formats

```bash
dcr ingest notes/ --store .dcr.json    # a file extension → plain JSON store
dcr ingest notes/ --store .context     # no extension    → tamper-evident container
```

The JSON store is unchanged and still the default. The container adds what a
plaintext file cannot have: objects addressed by the SHA-256 of their canonical
form, a Merkle root over the set, checkpoints chained so editing history
invalidates everything after it, and a generation high-water mark that refuses a
rollback. An edited `.dcr.json` reloads silently; an edited `.context` does not
load at all.

```bash
dcr verify --store .context                 # objects, chain, root, signatures
dcr scrub --repair --store .context --replica /mnt/backup
dcr checkpoint --store .context             # seal the current state
dcr quarantine --store .context             # what failed, and why
```

Read the [limitations](docs/architecture/context-integrity.md#9-what-this-does-not-defend-against)
before relying on it: without a signer this is tamper-*evident*, not
tamper-proof.

## Design decisions the spec left open

**`sufficient(L, q)`** (open question #1) is a query-type router plus *measured*
escalation, exactly as the decision-policy page proposes. `src/policy.rs` maps
query shape → cheapest level; when the model replies `#ESCALATE <node_id>` the
runtime re-plans with that node pinned at L0 and the telemetry counts it.
Escalation rate is the metric that catches a bad router, so it is reported by
default rather than buried.

**What L2 means for a prompt.** The wiki calls L2 "semantic/state vectors", and
a vector is not something a model reads. Here the vector is L2's *index key* and
the structured state object is its *payload*: `server.ip = 10.0.9.7 · conf=0.90
· spans=s_83…`. That is the level the fact cache lives at, and the reason 27k
tokens of transcript collapse into ~450.

**`U(x)`** (open question #7) is a small weighted sum with every term named —
similarity, graph proximity, `explain()` membership, confidence, historical
read-through, recency, kind prior, contradiction bonus, staleness penalty,
superseded-source penalty. It is deliberately inspectable rather than learned,
because a wrong `U` fills the window with cheap useless material *and looks
efficient while doing it*. `dcr plan <query> --explain` prints the per-node
arithmetic so a bad answer can be traced to the term that caused it.

**Invalidation cascades** (formerly open question #5, now resolved) are bounded: `max_cascade` nodes
eagerly, the tail deferred to a lazy queue drained before the next plan. One
small correction cannot stall a turn by invalidating a whole subgraph.

**Reasoner/Memory consistency** (formerly open question #9, now resolved) is
snapshot isolation plus an
interrupt. Every plan records `graph.version`; after the model answers, the
runtime checks whether anything in the working set was invalidated mid-turn and
rebuilds rather than returning an answer grounded in state that no longer holds.
`Dcr::ask_with_consolidation` takes the background pass explicitly, so the race
is testable rather than hypothetical.

**Corrections keep their history.** A new claim that disagrees with a live claim
on the same key produces a `contradicts` edge *and* a `supersedes` edge; the old
node stays in the graph, marked, and never re-enters the window. Evidence spans
that sourced a superseded value are annotated (`NOTE=contains a value corrected
later; current value in …`) rather than hidden — deleting them would break the
audit log, and admitting them unmarked is how a settled contradiction walks back
in.

**Ingest order is event order.** Revision is decided by the order material
arrives, which is the only ordering the runtime actually has. Because that is
only approximately chronology — files read from a directory, transcripts merged
from two sources — an explicitly corrective statement ("Correction: …",
"actually …") is protected: plain material arriving afterwards records a
`contradicts` edge but does not supersede it, and both sides enter the window
annotated `CONTRADICTS=… — adjudicate, do not pick blindly`. The CLI ingests
directories in modification-time order for the same reason.

**Object identity is content, and nothing else.** The generation an object was
written in lives in its sidecar, not in the hashed body, and read counters live
in one `usage` object per generation rather than inside node objects. Both were
bugs first: stamping the generation into the body gave unchanged material a new
address on every commit, and folding read counters into nodes minted a fresh
copy of every admitted node on every query. An append-only store must grow with
knowledge, not with reads.

**Repetition is corroboration.** Restating a fact merges into the existing node,
adds the span and raises confidence, instead of creating a near-duplicate line
that competes for the window.

## Notes on the Rust

* **No regex engine.** The extractor is a hand-written scanner
  (`src/indexer.rs`): find a separator (`=`, `:`, `is/was/are/were`), walk left
  for a one-to-three-word subject behind a valid boundary, walk right to the
  clause end. That is what the original pattern meant, and as a parser it can be
  read and stepped through.
* **Indices, not reference cycles.** The graph is `Vec<Node>` plus
  `HashMap<String, NodeIdx>`, with edges as index pairs. No `Rc<RefCell<…>>`
  ownership graph.
* **Interior mutability exactly where caching lives.** `Node::level_cache` is a
  `RefCell` and the read counters are `Cell`, because materialising a cheaper
  representation is logically a *read* — forcing `&mut` there would stop the
  planner from holding the graph while pricing what is in it.
* **Deterministic iteration.** Every ranking breaks ties on the item's own index
  and every persisted object keeps insertion order, so a plan never depends on
  which way a `HashMap` happened to iterate.
* **Typed metadata.** `NodeMeta` is a struct, not a string-keyed bag: the
  contradiction pointers, the corrective flag, the derivation and the L0 read
  counters are all fields the compiler checks.

## Auditing

`scripts/audit-bad-patterns.sh` runs the [rustsploit](https://github.com/s-b-repo/rustsploit)
bad-pattern regex matrix against `src/`. That matrix is calibrated for an async
offensive-security tool that parses hostile input, so several of its rules do
not apply to a synchronous offline library; `AUDIT.md` records the disposition
of every residual strict hit rather than leaving the count unexplained. The
genuine findings it surfaced — three panicking `unwrap`s in the demo, an
`expect` in the budget solver, and an indexed access in the reasoner — are
fixed; section A now holds only value-providing `unwrap_or_default`.

## Known limitations

* **End-to-end latency still grows**, though retrieval is no longer why. The cost model
  needs sub-linear retrieval to hold at scale (see the query-latency column
  above: flat `k`, growing latency). Swap in an ANN index behind
  `src/index.rs`'s `VectorIndex` — the interface is three methods.
* **Extraction is conservative and scanner-based.** It proposes state only for
  unambiguous shapes (`key = value`, `Decision: …`). Prose-heavy material stays
  as L0 and is found by search instead. A model-driven extractor would find
  more, with the wrong-fact risk open question #2 describes.
* **L1 is extractive.** Every character of a summary exists verbatim in L0, so
  compression cannot introduce a claim.
* **Token counts are a character heuristic** (`len / 4`) unless you supply
  `Estimator::Custom`.
* **Derivation inputs and results are `f64`.** The execution layer covers
  numeric derivations; anything else needs a wider value type.
* **Storage is O(N) and unbounded, by design.** The claim is bounded
  *attention*, not bounded storage.
* **The benchmark corpus is synthetic.** It exercises correction, staleness,
  exact quoting, multi-hop justification and buried detail — it is not a
  substitute for a real long-horizon agent trace.
* **The container is tamper-evident, not tamper-proof.** No signer ships with
  the crate, so an attacker with write access to the directory can rewrite the
  objects, the chain, the manifest and the high-water mark together and produce
  a store that verifies. Hashing cannot prevent that; signatures and an
  out-of-band high-water mark are what raise the bar.
* **RAG ties full context on this corpus.** DCR wins the benchmark 7/7 to 5/7,
  but plain top-k retrieval also scores 5/7 at 2.5x the tokens. The architecture
  earns its keep on two probes — a fact never repeated, and an exact quote — not
  across the board.

## Using a real Reasoner

Rust has no official Anthropic SDK, and hand-rolling TLS to avoid a dependency
would be a bad trade. `CommandReasoner` pipes the prompt to a program of your
choosing instead:

```rust
use dcr::{Dcr, CommandReasoner};

let mut rt = Dcr::new(1200).with_reasoner(Box::new(CommandReasoner::new("./ask-claude.sh", &[])));
println!("{}", rt.ask("why did we roll back?", None).text);
```

```bash
#!/usr/bin/env bash
# ask-claude.sh — prompt on stdin, answer on stdout
jq -Rs '{model:"claude-opus-5", max_tokens:16000, thinking:{type:"adaptive"},
         system:$ENV.DCR_SYSTEM, messages:[{role:"user", content:.}]}' \
  | curl -s https://api.anthropic.com/v1/messages \
      -H "content-type: application/json" \
      -H "x-api-key: $ANTHROPIC_API_KEY" \
      -H "anthropic-version: 2023-06-01" -d @- \
  | jq -r '.content[] | select(.type=="text") | .text'
```

The Reasoner is handed the rendered active context and the escalation protocol
in `DCR_SYSTEM`; it never sees history. The Memory Runtime — addressing,
indexing, traversal, memoisation, invalidation — makes no model calls at all,
which is the asymmetry the two-system split argues for.

## CLI

```text
dcr ingest notes/                        index a file or directory (mtime order)
dcr ask "what is the server ip?"         plan a working set and answer
dcr plan "why did we roll back?" --explain    show the window and the arithmetic
dcr explain <node_id>                    audit path down to raw spans
dcr stats                                telemetry report
dcr demo | dcr bench [--scaling]         worked example / measurements
```

State persists to `--store` (default `.dcr.json`) as raw spans + graph; the
retrieval index is derived and rebuilt on load.
