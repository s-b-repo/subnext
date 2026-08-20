# Authenticated Context Integrity

Everything else in this specification assumes the memory runtime returns what it
stored. That assumption is doing a lot of work. A `.dcr.json` can be edited in
any text editor and reloaded without anything noticing; a disk can flip a bit in
a span nobody has read for a year; a process killed mid-write can leave half an
object. None of those are exotic, and all of them end the same way — the
reasoner is handed something that is not what the runtime meant to store.

So context is treated as **content-addressed, tamper-evident state** rather than
as a file.

```text
                    ┌─────────────────────────────┐
                    │       Context Gateway       │
                    │  verify → inspect → admit   │
                    └──────────────┬──────────────┘
                                   │
             ┌─────────────────────┴─────────────────────┐
             │                                           │
     ┌───────▼────────┐                        ┌─────────▼────────┐
     │ Context Index  │                        │ Context Objects  │
     │ Merkle root    │                        │ documents/spans  │
     │ chained gens   │                        │ nodes/edges      │
     │ signatures     │                        │ immutable        │
     └───────┬────────┘                        └─────────┬────────┘
             │                                           │
             └──────────────────┬────────────────────────┘
                                ▼
                       ┌────────────────┐
                       │   Reasoner     │
                       │ bounded active │
                       │ attention      │
                       └────────────────┘
```

Implemented in [`src/hash.rs`](../../src/hash.rs), [`src/merkle.rs`](../../src/merkle.rs),
[`src/context_store.rs`](../../src/context_store.rs), [`src/trust.rs`](../../src/trust.rs)
and [`src/scrub.rs`](../../src/scrub.rs).

---

## 1. Two kinds of identity

A span id (`s_1f2e3d4c5b6a`) is a **name**. It is FNV-1a, twelve hex characters,
and it exists so the same material ingested twice lands on the same span instead
of duplicating. It is fast, it is not cryptographic, and it is not trying to be.

An object id is an **address**: the full SHA-256 of the object's canonical
encoding. Modifying an object changes its address, so there is no such thing as
an edited object — only a different one.

```text
name     s_1f2e3d4c5b6a          stable across ingests, cheap, non-cryptographic
address  9dc38d71…c05c67 (64)    SHA-256 of the canonical bytes
```

Keeping both is deliberate. Making the name cryptographic would churn every
persisted store and every node id in the system to buy a property that only the
storage layer needs.

### Canonical encoding

A digest must be a property of the value, not of how the value was built. The
canonical form sorts object keys, drops repeated keys, and writes non-finite
numbers as `null`. Re-serialising a loaded object must reproduce its address —
`ContextStore::get` checks exactly that, so an encoding drift shows up as a
verification failure rather than as silent corruption years later.

---

## 2. Merkle root over the object set

One signature over one enormous file proves only that the file as a whole is
intact. A Merkle root proves the same thing *and* lets any single object be
verified with `log n` hashes, without reading the rest of the store — which is
what makes scrubbing and repair affordable.

```text
                    ROOT
                  /      \
               H01        H02
              /  \       /  \
            H1   H2    H3    H4
            │    │     │     │
          obj1 obj2  obj3  obj4
```

Two rules the construction depends on:

- **Leaves are sorted by object id**, so the root is a function of the set, not
  of the write order.
- **Leaves and interior nodes are domain-separated** (`tagged("dcr:merkle:leaf")`
  vs `"dcr:merkle:node"`). Without that, an attacker who controls object bytes
  could submit a crafted "object" whose digest is really an interior node and
  forge an inclusion proof for material that was never stored.

An odd node at any level is **promoted**, never duplicated — duplicating the
last leaf is the CVE-2012-2459 shape, where two different leaf sets share a root.

---

## 3. Append-only generations

Nothing is overwritten. Each checkpoint chains to its parent:

```text
chain_n = H(chain_{n-1} ‖ canonical(checkpoint_n))

C0 → C1 → C2 → C3
```

The chain covers the **whole checkpoint body**, not a selected few fields. An
earlier iteration chained only the root and the delta, which left the timestamp,
the object count and the policy hash editable without breaking anything; the
`bench --tamper` probe caught it, which is the reason that probe exists.

Editing any historical generation invalidates every later one. That is
tamper-*evidence*: the store cannot be quietly rewritten, though see
[§9](#9-what-this-does-not-defend-against) for what it still cannot stop.

---

## 4. Signatures, and their states

A checkpoint may carry a detached signature. Verification is a state, not a
boolean, because "nobody signed this" and "the signature did not check out" call
for different responses:

```text
UNSIGNED     no signature offered, and the store declares no signing key
SIGNED       a signature is present, not yet checked
VERIFIED     checked against the declared key and passed
INVALID      checked and failed, or the bytes do not match their digest
REVOKED      the key has been revoked; past signatures no longer count
QUARANTINED  failed, and moved out of the object store
```

Only `VERIFIED` (or `UNSIGNED`, in a store that declares no signing key) enters
trusted memory. A store that *was* signed refuses unsigned objects — otherwise
turning signing off would be an undetectable downgrade.

**No cryptographic signer is bundled.** `Signer`, `Verifier` and `Aead` are
traits. This crate hand-writes SHA-256 because a hash can be checked against
published vectors; that argument does not extend to Ed25519 or an AEAD, where
the failure modes are constant-time behaviour and nonce discipline rather than a
wrong digest. `InsecureDevSigner` exists to exercise the state machine and says
so: `is_cryptographic()` returns `false`, and the manifest records that, so a
development signature reads as unprotected in an audit rather than as protection.

---

## 5. Key separation

One key for everything means one compromise loses everything. Roles are an enum
rather than a convention:

```text
context-signing        checkpoints
context-encryption     objects at rest
agent-identity         who this runtime claims to be
tool-authorization     what it may invoke
backup-signing         archived state
```

The manifest records which role produced which artefact even while signing is
unplugged, so a later integrator cannot quietly reuse the context-signing key to
authorise a tool call. For a long-lived installation the signing key should be
hardware-backed; the format is built to accept that, and this crate is not it.

---

## 6. Encryption is separate from integrity

Confidentiality is a different property from integrity, and conflating them is
how ciphertext ends up movable between object ids. The `Aead` trait takes the
object's metadata as authenticated data:

```text
C = Seal_K(plaintext, nonce, AAD)

AAD = object_id ‖ generation ‖ parent_root ‖ agent_id ‖ schema_version
```

so a valid ciphertext cannot be relocated to a different address. No
implementation is bundled; `encryption_key_id` stays `null` and the store says
it is unencrypted rather than implying otherwise.

---

## 7. Bit rot, and repair

Signatures detect modification. They repair nothing, and they do not distinguish
a malicious edit from a sector that decayed. The scrubber is the second half:

```text
read object → recompute hash → compare
                                 │
                       ┌─────────┴─────────┐
                    matches            mismatch
                       │                   │
                    healthy      find a replica copy
                                           │
                                 verify the replica ON ITS OWN
                                 (digest == address, and it proves
                                  against the committed Merkle root)
                                           │
                             ┌─────────────┴─────────────┐
                         verified                   unverified
                             │                           │
                        repair primary              quarantine
```

**Never repair from an unverified replica.** A copy may fix another copy only
once its own bytes hash to the address being repaired *and* that pairing proves
against the committed root. Otherwise a scrubber is a mechanism for spreading
corruption evenly.

Each object carries a sidecar — hash, size, replica count, locations,
verification state — kept outside the hashed record because it describes where
the bytes live, which changes without the object changing.

### Losing an object is not silent

With no replicas configured there is nothing to repair *from*: detection still
works, and the only available action is quarantine. Quarantining changes the
object set, so the store can no longer reproduce the root its last checkpoint
committed to. That is not papered over — the scrubber seals a new generation
recording the loss, because a store that quietly claims a root it cannot rebuild
is worse than one that admits what it dropped.

**Erasure coding is deferred.** Verified replicas cover the same durability need
at this scale, and hand-rolling Reed–Solomon next to hand-rolled SHA-256 is more
risk than the design earns.

---

## 8. Quarantine, and who decides

An object that fails verification is moved to `quarantine/` with a reason
record. The reasoner is never asked to adjudicate:

```text
Object 9dc38d71 failed integrity verification.
Trusted replacement available: object 9dc38d71@gen17.
```

The security decision belongs to the runtime. Handing suspect bytes to a model
and asking whether they look trustworthy is not a security control.

---

## 9. What this does *not* defend against

Cryptographic verification answers **"was this modified since it was written?"**
It does not answer **"was this true when it was written?"**, and it never
answers **"should this be believed?"**

Those are separate layers, kept separate in the code:

```text
integrity          the bytes are what the store recorded         ContextStore
authenticity       a named key vouched for them                  Signer/Verifier
confidentiality    only key-holders can read them                Aead
semantic trust     the content may be acted on                   TrustLabel + policy
```

A perfectly signed instruction from a low-trust source is still refused. The
gateway checks trust *last*, after an object has been shown to be intact,
authentic and current — which is the point: a valid signature does not make
content trustworthy.

And plainly: **without a signer this container is tamper-evident, not
tamper-proof.** An attacker with write access to the directory can rewrite the
objects, the chain, the manifest and the high-water mark together and produce a
store that verifies. No amount of hashing prevents that. Signatures raise the
bar; real anti-rollback needs the high-water mark held somewhere the attacker
cannot write — a TPM, a secure element, a remote witness. The format is built to
accept that hardware. This crate does not pretend to be it.

---

## 10. Anti-rollback

An attacker should not be able to replace generation 100 with an older,
correctly signed generation 37. Every state carries a monotonic generation, and
the highest ever accepted is persisted separately:

```text
v_new > v_trusted            enforced on write
manifest.highest_generation ≥ generation.hwm      enforced on open
```

`generation.hwm` carries a guard digest so truncation is visible. This is secure
boot logic applied to memory state, with the same dependency: the mark is only
as trustworthy as the place it is stored.

---

## 11. Provenance is part of admission

Every derived object names the objects it came from. A derived object with no
sources is refused at write time — that is a fact with no path back to raw
material, which the graph already rejects in memory and the store now rejects
again on the way to disk.

Combined with the [evidence hierarchy](../concepts/representation-ladder.md)
and [`Origin`](provenance.md), this preserves the distinction between

```text
observed → externally sourced → computed → inferred → hypothetical
```

so an agent cannot silently turn an inference into a fact.

---

## 12. The container format

```text
.context/
├── manifest              format_version, agent_id, root_hash, highest_generation,
│                         encryption_key_id, signing_key_id, schema_hash, created_at
├── generation.hwm        anti-rollback high-water mark
├── objects/ab/cdef…      immutable, content-addressed
├── objects/ab/cdef….m    sidecar: hash, size, replicas, locations, verification
├── checkpoints/000001    generation, merkle root, parent root, chain, policy hash
├── signatures/000001     detached signature over the checkpoint
├── indexes/              derived; rebuilt on load, never authoritative
└── quarantine/           failed objects, plus why
```

Indexes are **derived**. They are rebuilt on load and never authoritative,
because an index that can disagree with the objects is a second source of truth.

Usage counters (reads, admits) live in one `usage` object per generation rather
than inside node objects. Folding a read counter into an address would mint a
new copy of every admitted node on every query — an append-only store that grows
with *reads* rather than with knowledge.

---

## 13. The invariant

> **No context enters trusted reasoning unless**
> integrity is verified, the signature and provenance are valid, the generation
> is acceptable, the schema is valid, and policy allows it.

> **Every trusted context state must be reproducible from its signed history.**

Both are enforced in one place — `ContextGateway::admit` — rather than checked
in five places and forgotten in a sixth.

---

## Trying it

```bash
dcr ingest notes/ --store .context     # a directory is a container
dcr verify --store .context            # objects, chain, root, signatures
dcr scrub --repair --store .context --replica /mnt/backup
dcr checkpoint --store .context
dcr quarantine --store .context
cargo run --release -- bench --tamper  # can it actually detect tampering?
```

`bench --tamper` is the falsification: it corrupts an object, rewrites a
historical checkpoint, and attempts a rollback, then asserts each is caught. A
security property that cannot fail a test is not a property, it is a claim.
