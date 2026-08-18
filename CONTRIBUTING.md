# Contributing

This repo is **docs-only**. Pull requests that add implementation code will be closed with a
pointer to [docs/roadmap.md](docs/roadmap.md) non-goals.

## What is welcome

- **Critiques.** Attack the design. [docs/open-questions.md](docs/open-questions.md) is the
  priority surface.
- **Worked examples.** Trace a real long-context task through the ladder / graph and show where it
  breaks.
- **Prior art.** Papers or systems that already do part of this, with a note on what they solved.
- **Clarity fixes.** Wrong, vague, or hand-wavy prose.

## Rules

1. **No unsupported performance claims.** `O(k + r)` is a design target. Don't write it as a
   measured result. If you claim a speedup, cite a benchmark.
2. **Keep diagrams ASCII** in fenced `text` blocks so they render everywhere.
3. **One idea per page.** Link between pages instead of duplicating.
4. **Update the index.** New pages go into [docs/index.md](docs/index.md) and the README table.
5. **Terms go in the [glossary](docs/glossary.md).** Don't introduce a new term without defining it.

## Style

- Plain prose, short sentences, no filler.
- Prefer a table over a paragraph when comparing things.
- Mark uncertainty explicitly ("unclear", "unresolved") rather than implying confidence.

## Process

Open an issue first for anything structural (new concept page, schema change). Small fixes can go
straight to a PR.
