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

**E — numeric casts (72 → 118 lines / 151 matches).** The new hits are
`as u64` / `as usize` in length prefixes and hex conversion, and `as f64` in the
report arithmetic. Domain conversions in code that computes sizes it produced
itself. The fixed tool now separates distinct hit *lines* from raw *matches*,
which is why this number looks like a jump: one line casting twice was
previously counted once.

**F — `std::fs::read` / `write` (1 → 13).** The container is a directory of
files, so it reads and writes rather more than a single-file store did. Still
synchronous, still a deliberately offline library. Object writes go through
`write_atomic` (temp file plus rename), which is the property that matters here:
a partial write must never be mistakable for a whole object.

**P — `Debug` over a struct containing a token (6).** Section P was dead until
the tool was fixed: three of its four regexes never matched anything. The live
one looks for a struct that derives `Debug` and holds a field whose name
contains `token` — a credential printed by `{:?}`.

All six are token *counts*, not secrets — `Allocation` (`budget.rs:82`),
`ActiveContext` (`planner.rs:170`), `Answer` (`runtime.rs:39`), `RebuildReport`
(`runtime.rs:1137`), `Turn` (`telemetry.rs:15`) and `Report`
(`telemetry.rs:105`). This crate counts tokens in the language-model sense on
nearly every struct that crosses the planner, so the pattern will keep firing
here and the count will rise rather than fall. Confirm the list with
`scripts/audit-bad-patterns.sh --section P --show` rather than by grepping for
token-named fields: more structs have one than the section reports, because the
pattern requires the field to sit inside the struct that derives `Debug`.

**The pattern is still worth its false positives.** Before the fix it was dead,
and while it was dead `InsecureDevSigner` shipped with a derived `Debug` over
its `secret: String` — so formatting the signer would have printed the signing
key. That is the one real bug this matrix has produced, and it was found the
first time the section ran. The type exists to be conspicuously unsafe for
protection, not to leak; it now has a manual `Debug` rendering the secret as
`<redacted>`.

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

C (lint suppression), G (logging), H (HTTP), I, L (crypto), M (injection) and
O (performance) report zero hits. P no longer does — see above; it was reporting
zero because its regexes were broken, not because the code was clean.

## Re-baselined at `3821cc5`

The audit tool itself was rewritten after it was found to pass silently on an
empty `src/`, drop filenames containing spaces, and carry three section-P
regexes that could never match. Counts below therefore come from a different
instrument than the ones above them, and two moved for tool reasons rather than
code reasons: **N 11 → 10** (two overlapping `Result<_, String>` patterns were
double-counting one line) and **E**, which now reports lines and matches
separately.

Current totals: 274 distinct hit lines, 311 raw matches, 106 in gating sections,
across 30 files and 13,539 lines. Unchanged from the `c17abf5` baseline except
that one pattern stopped matching: the crate's only `debug_assert_eq!` became a
real `assert_eq!`, because it guarded Table 4's correctness-equality claim and
every benchmark runs `--release`, where debug assertions are compiled out. It
was a control that could not fire, sitting inside the table that reported the
result it was meant to guard — the third instance of that defect this suite has
caught.

A tool that silently passes on an empty directory is worth more scrutiny than
the code it audits — it had been reporting success for a case where it had
examined nothing.

## Gating

`--strict` is **not** wired into CI, because a green result would require
waiving every accepted line above, and a gate that is mostly waivers tests
nothing. The useful invariant — no unreviewed panics in library code — is held
by library code being empty of panicking forms and by this file existing.
Re-run the audit after any change to `src/` and add a row here for anything new.
