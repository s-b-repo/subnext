# Workspace Rebuild

The [two-system split](two-system-split.md) claims an invariant:

> The workspace can be destroyed and rebuilt at any time from Solution B.

That is only useful if rebuilding is cheap. If reconstruction requires
re-deriving everything, the invariant is nominal — technically true, never
exercised, and quietly false in the only situation where it matters. So it is
measured rather than asserted.

```bash
cargo run --release -- bench --rebuild --budget 1200
```

---

## The algorithm

Rebuilding is not "replay the session". It is one planning pass:

```text
1. drop every cached representation      (L1 text, L2 vector, L3 result)
2. classify the query                    → query type → level routing
3. seed from the index                   → k_seed candidates
4. expand along dependency edges         → bounded by max_depth / max_fanout
5. assign a level per candidate          → cheapest sufficient
6. solve the knapsack against B_attention
7. materialise the admitted levels from L0
```

Step 7 is the only part that touches raw material, and it touches it only for
the nodes that were admitted. That is the whole reason the cost is bounded:
**rebuild is O(k + r), not O(N)** — proportional to the working set and the
retrieval it needed, not to history.

Nothing is replayed. The graph, the spans and the provenance are already
durable; the workspace was never the source of anything.

---

## Measured

300-turn transcript, 27,362 tokens of history, 298 nodes, 301 spans,
`B_attention` = 1200:

| probe | cold (ms) | warm (ms) | nodes | L1/L2/L3 rebuilt |
|---|---:|---:|---:|---|
| corrected fact (mid-history) | 0.80 | 0.11 | 7 | 1/7/0 |
| corrected fact (late) | 0.98 | 0.15 | 11 | 7/11/0 |
| old fact, never repeated | 1.45 | 0.15 | 12 | 7/12/0 |
| exact quote | 2.17 | 0.16 | 14 | 10/14/0 |
| justification / multi-hop | 0.14 | 0.09 | 7 | 0/7/0 |
| detail buried in a long span | 5.19 | 0.27 | 23 | 21/23/0 |
| corrected fact (very late) | 0.11 | 0.09 | 4 | 0/4/0 |

**Mean cold rebuild 1.55 ms; mean warm assembly 0.15 ms.**

- **Cold** is the real cost of the guarantee: every cached representation
  dropped, then the working set reassembled from L0 alone.
- **Warm** is the same query with caches populated — what a second turn costs.

The roughly ten-fold gap is what the level cache buys, and it tracks how much L1
has to be rebuilt. The probes that admit only cached facts rebuild nothing and
cost the same either way; the one that pulls long spans (21 L1 rebuilds)
dominates the mean.

**Do not read these latencies as tight.** An earlier version of this page said
they move "by 10–15% between runs of the same binary". That was measured on an
idle machine and is wrong under load. Re-running the same binary eight times
while two other processes were building the same crate — load average 14 on 12
cores — gave cold means of 1.98, 2.73, 2.82, 3.05 and 4.18 ms, and warm means
from 0.15 to 1.02 ms. That is a 2× spread on cold and nearly 7× on warm, and the
cold/warm ratio itself ranged from 4.6× to 12×.

The table above is a single run on a quiet machine, and it is the right kind of
number for the claim it supports — that rebuilding is cheap enough for the
destroy-and-rebuild invariant to be real — but it is the wrong kind of number to
compare against a later run on a busy one. **If you reproduce this and get 4 ms,
you have not found a regression; you have found a loaded machine.** The durable
claims are the ordering (cold exceeds warm by around an order of magnitude) and
the shape (cost tracks L1 rebuilds, not history size), not the absolute
milliseconds.

---

## What the numbers do and do not license

**Do:** at this scale the invariant is real. A cold rebuild costs single-digit
milliseconds — cheaper than one model call by three orders of magnitude — so
discarding the workspace between turns is a genuine option rather than a
theoretical one.

**Do not:** conclude it stays cheap. The cost has two terms, and only one of
them is bounded:

```text
rebuild = plan(k) + materialise(admitted)
             │             │
             │             └── bounded by B_attention — stays flat
             └── includes retrieval, which is O(nodes) today
```

An LSH index now prunes most of the vectors a query scores, but end-to-end
latency still grows and the cost has moved into planning (see the known gap in
[RESULTS.md](../../RESULTS.md)). Read this table as **"rebuild is bounded by the
working set, plus a planning term not yet shown to be sub-linear."**

---

## Incremental rebuild

A full rebuild is the worst case, not the normal one. Three cheaper paths exist:

| situation | what is rebuilt | cost |
|---|---|---|
| same query, caches warm | nothing | 0.15 ms |
| new query, overlapping working set | only the newly admitted nodes | proportional to the delta |
| background consolidation invalidated part of the set | only the invalidated nodes, drained from the lazy queue | bounded by `max_cascade` |

The level cache is per-node, not per-plan, so a node admitted for one query is
already materialised for the next.

---

## Crash recovery

The workspace is not persisted, and that is the point — there is nothing to
recover. After a crash:

1. `open_context` verifies the manifest, the chain and the high-water mark.
2. Derived indexes are rebuilt from the objects.
3. The next query rebuilds the workspace exactly as above.

The only state that must survive is the state the container already makes
durable. A crash mid-turn loses the turn, never the memory.

---

## The honest limitation

The guarantee is bounded by what the memory can still produce. If an object was
quarantined and no verified replica existed, the workspace that referenced it
cannot be rebuilt identically — the material is genuinely gone. The runtime
records that as a sealed generation rather than rebuilding something *close
enough* and calling the invariant satisfied. See
[context integrity §7](context-integrity.md#losing-an-object-is-not-silent).
