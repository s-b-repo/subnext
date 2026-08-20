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

After these, section A holds only `unwrap_or_default()`, which provides a value
and cannot panic.

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
by section A being empty of panicking forms and by this file existing. Re-run
the audit after any change to `src/` and add a row here for anything new.
