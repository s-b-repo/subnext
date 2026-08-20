# DCR — implementation

A working implementation of the specification in [`docs/`](docs/): unlimited
external state plus a small, planned working set.

Pure Python, standard library only, runs offline. The Anthropic SDK is an
optional extra used only when you want a real Reasoner instead of the
deterministic evaluation harness.

```bash
python -m dcr demo          # worked example through all four ladder levels
python -m dcr bench         # DCR vs full context vs sliding window
python -m dcr --budget 800 bench --scaling   # does k stay flat while N grows?
python -m unittest discover -s tests         # 72 tests, ~1s
```

```python
from dcr import DCR

rt = DCR(budget=800)
rt.ingest(open("incident.log").read())
answer = rt.ask("what is the server ip?")
print(answer.text, answer.tokens)
print(rt.explain(answer.cited[0]))          # audit path down to raw spans
```

---

## What it does

The runtime never shows the model the history. Each turn it assembles a small
active context by solving a knapsack over (node, ladder level) pairs, and every
line it admits is traceable back to an immutable source span.

```
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
| [state indexer](docs/architecture/state-indexer.md) | `dcr/spans.py`, `dcr/indexer.py` | eager L0 addressing, lazy everything else |
| [representation ladder](docs/concepts/representation-ladder.md) | `dcr/ladder.py` | L0/L1/L2/L3 build, cache, cost |
| [memory graph](docs/architecture/memory-graph.md) | `dcr/graph.py`, `dcr/nodes.py` | typed nodes, `upsert/neighbors/explain/invalidate` |
| [provenance](docs/architecture/provenance.md) | `dcr/graph.py` | grounding enforced at `upsert`, not documented and hoped for |
| [fact cache](docs/concepts/fact-cache.md) | `dcr/ladder.py` (L2 payload) | `key = value · conf · spans` |
| [context graph](docs/concepts/context-graph.md) | `dcr/indexer.py` reference linking | edges from value mentions, not similarity |
| [attention budget](docs/concepts/attention-budget.md) | `dcr/budget.py` | exact multiple-choice knapsack |
| [relevance planner](docs/architecture/relevance-planner.md) | `dcr/planner.py` | seed → expand → level-assign → bound → speculate |
| [decision policy](docs/architecture/decision-policy.md) | `dcr/policy.py` | query-type routing, escalation, de-escalation |
| [speculative context](docs/concepts/speculative-context.md) | `dcr/speculation.py` | online predictor, separate budget, feedback |
| [cost model](docs/concepts/cost-model.md) | `dcr/telemetry.py`, `dcr/bench.py` | measured, not asserted |
| [two-system split](docs/architecture/two-system-split.md) | `dcr/runtime.py`, `dcr/llm.py` | Reasoner sees only `k`; Memory Runtime needs no model |

## Results

Full writeup with the scaling table and caveats: [`RESULTS.md`](RESULTS.md).

`python -m dcr bench` — 300 turns, 27,362 tokens of history, `B_attention` =
1200, sliding window = 8000. Same deterministic reasoner for all three systems,
so this compares *context assemblies*, not models.

```
probe                                    full  window    DCR   DCR tokens
corrected fact (mid-history)            ok    MISS    ok          162
corrected fact (late)                   ok     ok     ok          447
old fact, never repeated               MISS   MISS    ok          393
exact quote                            MISS   MISS    ok          963
justification / multi-hop               ok    MISS    ok          189
detail buried in a long span            ok    MISS    ok          908
corrected fact (very late)              ok     ok     ok          134
correct                                    5       2      7   of 7
mean tokens per query                 27362.0  7968.0  456.6
attention vs full history                 1x    3.4x    60x
```

`python -m dcr --budget 800 bench --scaling` — the `O(k + r)` claim:

```
  turns   history   nodes   mean k   max k  correct   ingest    query
    100      8482     121    418.3     764      7/7    0.07s     5.2ms
    300     27362     295    413.9     786      7/7    0.22s     9.4ms
   1000     93442     908    418.7     793      7/7    0.86s    23.8ms
   3000    283253    2658    411.6     785      7/7    4.11s    65.8ms

history grew 33x; active context grew 0.98x
```

**Read these honestly.** The reasoner is a line-matcher, so the full-context
column is a floor, not a ceiling — a real model reading the whole transcript
would beat it on the retrieval probes. The load-bearing results are the ones no
model quality changes: the token counts, the flat `k`, and the sliding window's
misses, which are structural (a fact outside the window is unrecoverable at any
model quality).

## Design decisions the spec left open

**`sufficient(L, q)`** (open question #1) is a query-type router plus *measured*
escalation, exactly as the decision-policy page proposes. `dcr/policy.py` maps
query shape → cheapest level; when the model replies `#ESCALATE <node_id>` the
runtime re-plans with that node pinned at L0 and the telemetry counts it.
Escalation rate is the metric that catches a bad router, so it is reported by
default rather than buried.

**What L2 means for a prompt.** The wiki calls L2 "semantic/state vectors", and
a vector is not something a model reads. Here the vector is L2's *index key* and
the structured state object is its *payload*: `server.ip = 10.0.9.7 · conf=0.90
· spans=s_83…`. That is the level the fact cache lives at, and the reason 27k
tokens of transcript collapse into ~450.

**`U(x)`** (open question #8) is a small weighted sum with every term named —
similarity, graph proximity, `explain()` membership, confidence, historical
read-through, recency, kind prior, contradiction bonus, staleness penalty. It is
deliberately inspectable rather than learned, because a wrong `U` fills the
window with cheap useless material *and looks efficient while doing it*.
`planner.explain_plan(ctx)` prints the per-node arithmetic so a bad answer can
be traced to the term that caused it.

**Invalidation cascades** (open question #5) are bounded: `max_cascade` nodes
eagerly, the tail deferred to a lazy queue drained before the next plan. One
small correction cannot stall a turn by invalidating a whole subgraph.

**Reasoner/Memory consistency** (open question #9) is snapshot isolation plus an
interrupt. Every plan records `graph.version`; after the model answers, the
runtime checks whether anything in the working set was invalidated mid-turn and
rebuilds rather than returning an answer grounded in state that no longer holds.

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
annotated `CONTRADICTS=… — adjudicate, do not pick blindly`. Two conflicting
facts in a window *without* that marker would be worse than either alone. The
CLI ingests directories in modification-time order for the same reason.

**Repetition is corroboration.** Restating a fact merges into the existing node,
adds the span and raises confidence, instead of creating a near-duplicate line
that competes for the window.

## Known limitations

* **Retrieval is O(nodes).** The vector index is a linear scan. The cost model
  needs sub-linear retrieval to hold at scale (see the query-latency column
  above: flat `k`, growing latency). Swap in an ANN index behind
  `dcr/index.py`'s `VectorIndex` — the interface is two methods.
* **Extraction is conservative and regex-based by default.** It proposes state
  only for unambiguous shapes (`key = value`, `Decision: …`). Prose-heavy
  material stays as L0 and is found by search instead. `LLMExtractor` in
  `dcr/llm.py` swaps in a model, with the wrong-fact risk that open question #2
  describes — mandatory spans and confidence thresholds mitigate it, they do not
  solve it.
* **L1 is extractive.** Every character of a summary exists verbatim in L0, so
  compression cannot introduce a claim. `LLMSummarizer` is available and gives
  that property up.
* **Token counts are a character heuristic** (`len/4`) unless you pass a real
  estimator; `dcr/tokens.py` ships `make_count_tokens_estimator` for the
  Anthropic token-counting endpoint.
* **Storage is O(N) and unbounded, by design.** The claim is bounded
  *attention*, not bounded storage.
* **The benchmark corpus is synthetic.** It exercises correction, staleness,
  exact quoting, multi-hop justification and buried detail — it is not a
  substitute for a real long-horizon agent trace.

## Using a real Reasoner

```python
from dcr import DCR
from dcr.llm import AnthropicLLM          # pip install anthropic

rt = DCR(budget=1200, reasoner=AnthropicLLM())   # Claude Opus 5, adaptive thinking
print(rt.ask("why did we roll back?").text)
```

The Reasoner is handed the rendered active context and the escalation protocol
in its system prompt; it never sees history. The Memory Runtime — addressing,
indexing, traversal, memoisation, invalidation — makes no model calls at all,
which is the asymmetry the two-system split argues for.

## CLI

```
dcr ingest notes/*.md                    index files into the memory runtime
dcr ask "what is the server ip?"         plan a working set and answer
dcr plan "why did we roll back?" --explain    show the window and the arithmetic
dcr explain <node_id>                    audit path down to raw spans
dcr stats                                telemetry report
dcr demo | dcr bench [--scaling]         worked example / measurements
```

State persists to `--store` (default `.dcr.json`) as raw spans + graph; the
retrieval index is derived and rebuilt on load.
