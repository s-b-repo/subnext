# Bad-pattern audit

`scripts/audit-bad-patterns.sh` is the regex matrix from
[rustsploit](https://github.com/s-b-repo/rustsploit), run against `src/`.

```bash
scripts/audit-bad-patterns.sh              # full report
scripts/audit-bad-patterns.sh --section A  # one section
scripts/audit-bad-patterns.sh --strict     # non-zero exit on any A/B/C/L/M/N/O hit
```

The matrix is calibrated for rustsploit: an **async, offensive-security tool
that parses hostile input mid-engagement**, where a panic or a silent
truncation is a live failure. DCR is a **synchronous, offline research
library** whose inputs are its own persisted state. Several sections therefore
flag idioms that are correct here. This file records the disposition of every
residual strict hit, so the non-zero count is a set of reviewed decisions rather
than an unread number.

## Fixed

| Where | Was | Now |
|---|---|---|
| `demo.rs` | three `.last().unwrap()` on graph lookups | `let … else` with a skip message — a demo must not panic on an edited transcript |
| `budget.rs` | `.expect("candidate with no options")` in `cheapest()` | returns `Option<Choice>`; the guarantee is now a type the caller handles, not a panic |
| `llm.rs` | `&scored[0]` after a length guard | `scored.first()` — the method carries no index that could panic if the guard moves |

After these, section A holds only `unwrap_or_default()` in library code, which
provides a value and cannot panic.

## The integrity layer (added Aug 2026)

`src/hash.rs`, `src/merkle.rs`, `src/context_store.rs`, `src/trust.rs`,
`src/scrub.rs` and `src/baselines.rs` are new since the rows above. Re-running
the matrix over them moved four counts; each is dispositioned here rather than
left to be rediscovered.

**Section L (crypto) still reports zero**, which is worth stating explicitly
now that the crate contains a hash implementation. The matrix's crypto patterns
look for weak primitives (MD5, SHA-1, ECB, hardcoded keys, `rand` for secrets)
and for none of them does SHA-256 with domain-separated, length-prefixed inputs
qualify. What the matrix cannot check is whether the implementation is
*correct*, which is why `src/hash.rs` is tested against the published NIST
vectors rather than against itself — and why no signature or AEAD is
hand-rolled at all.

**A — panicking forms now appear inside `#[cfg(test)]` modules (`unwrap`,
`expect`, `panic!`, `assert*`).** The matrix scans whole files, so unit tests
inside `src/` are counted alongside library code. A test that cannot panic
cannot fail, so these are correct where they are. The invariant still holds
where it matters: the only non-test panicking form under `src/` is the
pre-existing `.last().unwrap()` in the ablation benchmark.

**E — numeric casts (72 → 100).** The new hits are `as u64` / `as usize` in
length prefixes and hex conversion, and `as f64` in the report arithmetic.
Domain conversions in code that computes sizes it produced itself.

**F — `std::fs::read` / `write` (1 → 13).** The container is a directory of
files, so it reads and writes rather more than a single-file store did. Still
synchronous, still a deliberately offline library. Object writes go through
`write_atomic` (temp file plus rename), which is the property that matters here:
a partial write must never be mistakable for a whole object.

**J — `tokens: usize` matched as a secret.** The matrix's `[Tt]oken` pattern
looks for credentials in source; every token *count* field in this crate trips
it. There is no secret anywhere in `src/` — the only secret-shaped value is
`InsecureDevSigner`'s test key, which is now redacted in its `Debug` output
after review found the derive would have printed it. That was a genuine finding
from this matrix and the one real bug it has produced.

**B — `let _ = …` (11 → 14).** The new ones discard the result of removing a
sidecar during quarantine and of cleaning a scratch directory in the tamper
probe. In both, failure means a leftover file rather than a wrong result.

## Accepted, with reasons

**A — `unwrap_or_default()` (8).** Rendering and JSON-load paths: a missing
optional key becomes an empty string or zero. These provide a default; they do
not panic. In the persistence loader the default is deliberate
forward-compatibility — an older store missing a newer field loads rather than
failing.

**B — silent discards (11).**
- `let _ = write!(buf, …)` (5): writing to a `String`/`Vec` via `fmt::Write` is
  infallible; discarding the `Result` is the idiomatic form and clippy does not
  flag it.
- `let _ = stdin.write_all(prompt)` (`llm.rs`): if the child's stdin write
  fails, the child yields empty output, which the next line already handles.
- `let _ = runtime.explain(id)` (`bench.rs`): called for its telemetry side
  effect; the return value is intentionally unused.
- `if let Ok(meta) = entry.metadata()` (`main.rs`): a file whose metadata cannot
  be read falls back to `UNIX_EPOCH` in the mtime sort — deliberate graceful
  degradation during a directory walk.
- `map_err(|_| "…")` (`main.rs`): CLI argument errors become a user-facing
  message; the underlying `ParseIntError` ("invalid digit found in string")
  adds nothing over "--budget must be a number".

**D — string slices (4).** `&text[start..end]` on bounds the code computed
itself (from `.find()` or the store's own chunker), on ASCII delimiters, so the
slice is always on a valid char boundary. Self-derived indices, not offsets from
untrusted input.

**E — numeric casts (72).** `as f32` / `as f64` / `as usize` in the embedder,
the similarity and utility math, and the budget quantiser. These are
value-domain conversions in scoring code, not truncations of untrusted input.
`try_into()` here would obscure the arithmetic without removing a real failure
mode.

**F — `std::fs::write` (1).** `Dcr::save` is a synchronous file write in a
synchronous API. DCR is deliberately offline and single-threaded; the async
guidance the matrix encodes does not apply.

**J — `Box<dyn>` (5).** The `Reasoner` trait object, the derivation and
estimator closures, and the boxed error. Trait objects are the right tool for a
pluggable reasoner and a user-supplied token estimator.

**N — `Result<_, String>` (11).** Two sources:
- `json.rs`: the internal parser returns `Result<_, String>`, and every message
  is wrapped into the typed `DcrError::Parse(String)` at the public boundary
  (`parse(&text).map_err(DcrError::Parse)`). The public API is typed; the string
  is the message *content*, not a stringly-typed error surface.
- `main.rs`: CLI plumbing whose errors are printed to stderr. A typed error enum
  for argument parsing would be ceremony without a caller that discriminates on
  it.

## Not applicable

C (lint suppression), G (logging), H (HTTP), I, L (crypto), M (injection),
O (performance), P (API hygiene) report zero hits.

## Gating

`--strict` is **not** wired into CI, because a green result would require
waiving every accepted line above, and a gate that is mostly waivers tests
nothing. The useful invariant — no unreviewed panics in library code — is held
by library code being empty of panicking forms and by this file existing.
Re-run the audit after any change to `src/` and add a row here for anything new.
