# Motivation

## What RLM gets right

The Recursive Language Model idea treats context as a **variable in a REPL**: the model can
programmatically inspect it, decompose it, and recursively query sub-parts of it with further
model calls. That is the important primitive — context as a manipulable program object rather
than a flat prompt.

## What it does not do

It does not remove context rot. It relocates it.

```text
huge context
      ↓
RLM
      ↓
Python slicing
      ↓
sub-LLM calls
```

Every subcall still reconstructs what it needs from raw text. The system pays repeatedly:

- re-reading the same spans across turns
- re-deriving the same conclusions
- waiting on slicing/compression inside each recursion level
- keeping thousands of tokens that only explain *how* a settled fact was reached

## The claim

A runtime layer above the RLM primitive can be **faster than RLM while keeping its ability to
operate on effectively unbounded context**, because it:

1. stores context at multiple cost levels and picks the cheapest sufficient one
2. keeps state instead of re-reading source
3. caches settled facts as structured objects with provenance
4. retrieves along dependency edges instead of similarity
5. prefetches speculatively so subcalls don't stall

## The framing to build around

Give the transformer **unlimited external state** and a **small dynamic working set** —
not unlimited raw context.
