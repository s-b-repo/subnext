# Failure Modes

A design that only describes its happy path is a design that has not been
thought through. This is the catalogue: for each way the runtime can fail, how
it is **detected**, how it is **contained**, and how it **recovers**.

Rows marked *specified* have a defined behaviour but no implementation yet;
everything else names the code that handles it.

---

## Storage and integrity

### Corrupted memory (bit rot, bad sector, truncated write)

| | |
|---|---|
| **Detect** | Object bytes no longer hash to their address. Caught on read (`ContextStore::get`) and on demand (`dcr scrub`). |
| **Contain** | The object never reaches the graph — `get` returns `Corrupt`, and `open_context` fails rather than loading a partial memory. |
| **Recover** | Repair from a replica that independently verifies; otherwise quarantine and seal a new generation recording the loss. |
| **Code** | [`src/scrub.rs`](../../src/scrub.rs), [`src/context_store.rs`](../../src/context_store.rs) |

A partially admitted memory is worse than one that refuses to open, because the
gaps are invisible and the reasoner will confidently answer from what survived.

### Missing objects

| | |
|---|---|
| **Detect** | The object is in the index and not on disk. `read_object_bytes` returns `Missing`; the scrubber treats it exactly as corruption. |
| **Contain** | Same path as corruption: refused, not skipped. |
| **Recover** | Replica, or quarantine plus a sealed generation. |

### Partial writes

| | |
|---|---|
| **Detect** | Cannot occur at the object level: every write is to a temporary file followed by a rename, so a reader sees the old bytes or the new ones. A leftover `.tmp` is visible and ignored by the object walker. |
| **Contain** | Rename is the closest the filesystem offers to atomic. |
| **Recover** | Re-put the object; the address is unchanged, so the write is idempotent. |
| **Test** | `writes_leave_no_partial_files_behind` |

### Interrupted commit

| | |
|---|---|
| **Detect** | The write order is: objects, then the checkpoint, then the manifest, then the high-water mark. A crash between any two leaves a store that opens at the *older* generation. |
| **Contain** | The manifest never points at a checkpoint that does not exist. |
| **Recover** | Re-commit. The objects are already there and content-addressed, so nothing is written twice. |

### Conflicting versions

| | |
|---|---|
| **Detect** | A checkpoint whose parent pointers or chain digest do not match the previous generation. |
| **Contain** | `ContextStore::open` fails with `ChainBroken`, naming the generation. |
| **Recover** | Roll forward from the last intact generation; the objects below it are still addressable. |
| **Test** | `editing_history_invalidates_everything_after_it`, `any_edit_to_a_checkpoint_breaks_the_chain` |

### Replay / rollback

| | |
|---|---|
| **Detect** | `manifest.highest_generation < generation.hwm`. |
| **Contain** | Refused on open with `Rollback { found, floor }`. |
| **Recover** | Operator decision — this is an attack signature, not an accident. |
| **Limit** | The mark is only as trustworthy as where it is stored. See [context integrity §9](context-integrity.md#9-what-this-does-not-defend-against). |

---

## Retrieval and planning

### Stale indexes

| | |
|---|---|
| **Detect** | Not detected by checksum, because indexes are **derived**. They are rebuilt from the objects on every load and are never authoritative. |
| **Contain** | An index that disagrees with the graph cannot outlive a process. |
| **Recover** | Automatic: `open_context` reindexes every span and node. |

Making indexes derived rather than persisted is the containment. A persisted
index is a second source of truth, and a second source of truth is a bug that
has not happened yet.

### Retrieval failure (nothing relevant found)

| | |
|---|---|
| **Detect** | Seeds fall below `seed_min_ratio`, or the state search returns fewer than two candidates. |
| **Contain** | Fall back to the raw lexical index over L0 — material may be ingested but not yet extracted into state. |
| **Recover** | The answer is `I don't have that in the context`, which is the correct answer rather than a failure to suppress. |
| **Code** | [`src/planner.rs`](../../src/planner.rs) |

### Planner failure (nothing fits the budget)

| | |
|---|---|
| **Detect** | The knapsack cannot fit even the pinned candidates. |
| **Contain** | Overflow is *reported*, never silently exceeded: `budget_overflows` in the telemetry, and demotion is tried before eviction. |
| **Recover** | Escalate to a smaller working set at a cheaper level, or return an explicit insufficiency. |
| **Test** | `overflow_is_reported_not_silently_exceeded`, `never_exceeds_the_budget` |

### Over-retrieval

| | |
|---|---|
| **Detect** | `tokens_per_query_mean` climbing while accuracy does not. |
| **Contain** | Expansion caps (`max_depth`, `max_fanout`, `max_candidates`) bound traversal; the budget bounds the result. |
| **Recover** | Tune `U(x)`; `dcr plan --explain` attributes each admission to the term that caused it. |

### Recursive retrieval explosion

| | |
|---|---|
| **Detect** | Candidate count hits `max_candidates`. |
| **Contain** | Hard caps, not heuristics — the traversal is breadth-bounded at every level. |
| **Recover** | n/a; the cap is the recovery. |

---

## State correctness

### Model hallucinated memory

| | |
|---|---|
| **Detect** | A node arrives with no `source_spans` and no `dependencies`. |
| **Contain** | Refused at `upsert` with `ProvenanceError`, and refused again by the store on write. A cached fact with no spans is a hallucination with a database row. |
| **Recover** | The claim never enters the graph; the raw material remains at L0 and is found by search. |
| **Test** | `derived_objects_must_name_their_sources`, `tests/graph.rs` |

### Summary drift

| | |
|---|---|
| **Detect** | Structurally prevented: L1 is **extractive**, so every character of a summary exists verbatim in L0. |
| **Contain** | Compression cannot introduce a claim. |
| **Recover** | Escalate to L0 for exact bytes. |
| **Test** | `l1_is_extractive` |

### Contradictory evidence

| | |
|---|---|
| **Detect** | Two live nodes sharing a key and disagreeing on value. |
| **Contain** | Both are retained with a `contradicts` edge, both move to `Status::Contradicted`, and both enter the window annotated `CONTRADICTS=… — adjudicate, do not pick blindly`. |
| **Recover** | Supersession resolves it and settles the survivor back to `Fresh`. |
| **Test** | `contradiction_keeps_both_sides`, `resolving_a_contradiction_settles_the_survivor` |

### Invalidation cascade

| | |
|---|---|
| **Detect** | One correction marks a large transitive subgraph stale. |
| **Contain** | Bounded: `max_cascade` nodes eagerly, the tail deferred to a lazy queue drained before the next plan. |
| **Recover** | Revalidation on read. |

### Crashed background consolidation

| | |
|---|---|
| **Detect** | The graph version advanced but the pending-invalidation queue is non-empty. |
| **Contain** | The queue is persisted with the graph; a half-finished consolidation leaves nodes marked stale, which is the safe direction — stale nodes are never admitted. |
| **Recover** | The queue drains before the next plan. |

### Time-of-check / time-of-use races

| | |
|---|---|
| **Detect** | Every plan records `graph.version`; after the model answers, the runtime checks whether anything in the working set was invalidated mid-turn. |
| **Contain** | Snapshot isolation with an interrupt — the answer is not returned if it rests on state that no longer holds. |
| **Recover** | Re-plan and re-answer against the new state. |
| **Code** | `Dcr::ask_with_consolidation` |

### Clock / ordering errors

| | |
|---|---|
| **Detect** | Time is a **logical counter**, not wall-clock, so a machine whose clock jumps cannot reorder history. |
| **Contain** | Ingest order is revision order. Because that is only approximately chronology, an explicitly corrective statement is protected: later plain material records a `contradicts` edge but does not supersede it. |
| **Recover** | Both sides enter the window annotated for adjudication. |

---

## Adversarial input

### Malicious input (prompt injection in ingested material)

| | |
|---|---|
| **Detect** | Not detectable by integrity checking — the bytes are exactly what was stored. This is the case the [integrity/trust split](context-integrity.md#9-what-this-does-not-defend-against) exists for. |
| **Contain** | `TrustLabel` travels with the object; the admission policy decides which labels may enter trusted reasoning. Low-trust material is refused *after* being shown intact and authentic. |
| **Recover** | Relabel the source, or narrow the policy — both change `policy_hash`, so the change is visible in the next checkpoint rather than silent. |
| **Test** | `a_valid_signature_does_not_make_content_trustworthy` |

### Malicious tool output

| | |
|---|---|
| **Detect** | Same shape as malicious input, arriving through a different door. |
| **Contain** | Tool results are `Origin::ExternallySourced` at best, never `Observed`, and the window labels them. Tool authorisation is a separate key role from context signing. |
| **Recover** | *Specified.* Per-tool trust labels are defined; the ingest path does not yet stamp them automatically. |

### Forged signature

| | |
|---|---|
| **Detect** | Signature verification against the declared key. |
| **Contain** | `Verification::Invalid`; the object does not enter trusted memory. |
| **Recover** | Restore from a generation whose signature verifies. |
| **Test** | `a_forged_signature_does_not_verify` |

### Revoked key

| | |
|---|---|
| **Detect** | `Verifier::is_revoked`, checked *before* the signature itself. |
| **Contain** | A signature that still checks out arithmetically is refused anyway. |
| **Recover** | Re-sign from a current key; the objects are unchanged. |
| **Test** | `revocation_invalidates_a_signature_that_still_checks_out` |

---

## What has no answer yet

Honest gaps, not oversights:

- **A wrong-but-grounded extraction.** A claim with real spans that misreads
  them passes every check here. Confidence thresholds and `contradicts` edges
  reduce the blast radius; nothing detects it. This is
  [open question #2](../open-questions.md).
- **Coordinated rewrite of a whole container.** Objects, chain, manifest and
  high-water mark rewritten together verify cleanly without a signer. Stated
  plainly in [context integrity §9](context-integrity.md#9-what-this-does-not-defend-against).
- **A compromised signing key.** Revocation stops future acceptance; it does not
  tell you which past states were forged. An external witness or notarisation
  would, and neither is implemented.
