# Results

Measured output of the reference implementation, reproducible from this repo:

```bash
python -m dcr --budget 1200 bench            # the context-rot table
python -m dcr --budget 800 bench --scaling   # the flat-k table
python -m unittest discover -s tests         # 72 tests, ~1s
```

Both benchmarks are deterministic and offline. No API key, no network.

---

## Context rot

300-turn incident transcript, 300 documents, 27,362 tokens of history,
`B_attention = 1200`, sliding window = 8,000 tokens.

| | full context | sliding window (8k) | DCR |
|---|---:|---:|---:|
| correct | 5/7 | 2/7 | **7/7** |
| tokens/query | 27,362 | 7,968 | **457** |
| attention vs full history | 1x | 3.4x | **60x** |

Per probe:

| probe | full | window | DCR | DCR tokens |
|---|:---:|:---:|:---:|---:|
| corrected fact (mid-history) | ok | MISS | ok | 162 |
| corrected fact (late) | ok | ok | ok | 447 |
| old fact, never repeated | MISS | MISS | ok | 393 |
| exact quote | MISS | MISS | ok | 963 |
| justification / multi-hop | ok | MISS | ok | 189 |
| detail buried in a long span | ok | MISS | ok | 908 |
| corrected fact (very late) | ok | ok | ok | 134 |

Telemetry for the DCR column:

```
escalation_rate         : 0.143
stale_fact_read_rate    : 0.0
tokens_per_query_mean   : 456.6
tokens_per_query_max    : 963
budget_overflows        : 0
demotions               : 3
audit_path_completeness : 1.0
compression_ratio       : 59.8
```

Escalations are counted and charged: a probe that needed to drop to L0 costs
more, and that cost is inside the DCR token column, not hidden beside it.

### How to read this table

**The benchmark is deliberately unflattering to DCR.** The same dumb
deterministic line-matcher reasons for all three systems, so this compares
*context assemblies*, not models. Full-context accuracy here is a **floor, not
a ceiling** — a real model reading the whole transcript would beat 5/7. Do not
read this as "DCR is more accurate than a long-context model."

The two load-bearing results are:

1. **The token counts.** 457 vs 27,362 is a property of what gets assembled,
   and no amount of model quality changes it.
2. **The window's misses.** They are structural. A fact that fell outside an
   8k window is unrecoverable at any model quality — that is the point of the
   `old fact, never repeated` and `exact quote` rows.

---

## k stays flat while history grows

Same probes, same `B_attention = 800`, history scaled 33x:

| turns | history | nodes | mean k | max k | correct | ingest | query |
|---:|---:|---:|---:|---:|:---:|---:|---:|
| 100 | 8,482 | 121 | 418.3 | 764 | 7/7 | 0.08s | 5.9ms |
| 300 | 27,362 | 295 | 413.9 | 786 | 7/7 | 0.27s | 12.5ms |
| 1,000 | 93,442 | 908 | 418.7 | 793 | 7/7 | 1.23s | 33.1ms |
| 3,000 | 283,253 | 2,658 | 411.6 | 785 | 7/7 | 5.73s | 77.5ms |

History grew **33x**; active context grew **0.98x**, and accuracy held at 7/7 at
every size. That is the `O(k + r)` shape from the cost model, measured rather
than asserted.

Storage still grows `O(N)` — 121 → 2,658 nodes. The claim is bounded
*attention*, not bounded storage.

---

## Known gap: retrieval is not sub-linear

**Query latency is not flat: 5.9ms → 77.5ms across the scaling run.** Vector
search in `index.py` is a linear scan over state nodes, so retrieval cost grows
with the graph even though the assembled context does not.

The cost model in [`docs/concepts/cost-model.md`](docs/concepts/cost-model.md)
requires sub-linear retrieval for the `O(k + r)` claim to hold at real scale, so
this is the next real piece of work. It is a two-method swap behind `index.py`
(an ANN index), not a redesign — but until it lands, the flat-k table above
should be read as "flat attention, linear retrieval."

---

## Spec-to-code mapping

Every module maps to a page in [`docs/`](docs/):

| module | page |
|---|---|
| `spans.py` | L0 of the [representation ladder](docs/concepts/representation-ladder.md) |
| `ladder.py` | [Representation ladder](docs/concepts/representation-ladder.md) |
| `graph.py` | [Memory graph](docs/architecture/memory-graph.md) + [provenance](docs/architecture/provenance.md) |
| `budget.py` | [Attention budget](docs/concepts/attention-budget.md) — the knapsack |
| `planner.py` | [Relevance planner](docs/architecture/relevance-planner.md) |
| `speculation.py` | [Speculative context](docs/concepts/speculative-context.md) — prefetch |
| `indexer.py` | [State indexer](docs/architecture/state-indexer.md) |
| `policy.py` | [Decision policy](docs/architecture/decision-policy.md) |
| `telemetry.py` | Phase-5 metrics in the [roadmap](docs/roadmap.md) |

Invariants the wiki states as prose are enforced in code and covered by tests:

- an unsourced fact raises `ProvenanceError` — a claim cannot exist without a
  path down to raw spans (`graph.py`, `test_graph.py`)
- nothing is ever deleted; superseded nodes are marked, not removed
- stale nodes never reach the model (`stale_fact_read_rate: 0.0`)
- demotion-beats-eviction falls out of the knapsack, verified against a brute-force
  optimum in the tests rather than hand-asserted

`audit_path_completeness: 1.0` means every answer above resolves to raw source
spans via `rt.explain(...)`.

---

## Scope note

[`docs/roadmap.md`](docs/roadmap.md) listed "shipping a library from this repo"
as a non-goal, and this implementation crosses that line. It is framed as a
**reference implementation that makes the spec falsifiable** — a way to check
whether the invariants hold when they meet real code — not as production
infrastructure. Easy to revert if you disagree with that call.
