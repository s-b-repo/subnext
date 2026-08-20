# Contributing

This repo is a specification wiki (`docs/`) plus a reference implementation of it (`src/`). The
wiki is the design; the code exists to make the design falsifiable. Changes to either are welcome,
but they answer to different standards — see the rules below.

## What is welcome

- **Critiques.** Attack the design. [docs/open-questions.md](docs/open-questions.md) is the
  priority surface.
- **Worked examples.** Trace a real long-context task through the ladder / graph and show where it
  breaks.
- **Prior art.** Papers or systems that already do part of this, with a note on what they solved.
- **Clarity fixes.** Wrong, vague, or hand-wavy prose.
- **Code that closes a gap between the wiki and `src/`**, or a benchmark probe that the current
  implementation fails. A failing probe is worth more than a passing feature.

## Rules

1. **No unsupported performance claims.** `O(k + r)` is a design target. Don't write it as a
   measured result. If you claim a speedup, cite a benchmark.
2. **Code changes carry tests.** `cargo test` must pass, and anything
   the wiki states as an invariant ("nothing is deleted", "a stale node never reaches the model")
   belongs in a test rather than a comment.
3. **Don't let the code and the wiki drift.** If an implementation choice resolves something the
   wiki calls open, say so on the relevant page — and if it does not resolve it, don't imply that
   shipping code settled the question.
4. **Keep diagrams ASCII** in fenced `text` blocks so they render everywhere.
5. **One idea per page.** Link between pages instead of duplicating.
6. **Update the index.** New pages go into [docs/index.md](docs/index.md) and the README table.
7. **Terms go in the [glossary](docs/glossary.md).** Don't introduce a new term without defining it.

## Style

- Plain prose, short sentences, no filler.
- Prefer a table over a paragraph when comparing things.
- Mark uncertainty explicitly ("unclear", "unresolved") rather than implying confidence.

## Process

Open an issue first for anything structural (new concept page, schema change). Small fixes can go
straight to a PR.
