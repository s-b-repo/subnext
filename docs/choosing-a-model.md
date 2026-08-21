# Choosing a model

This runtime is model-agnostic by construction. Two traits are the entire
surface a model touches:

```rust
pub trait Reasoner {
    fn complete(&mut self, prompt: &str, system: &str) -> String;
}

pub trait Embedder: std::fmt::Debug {
    fn embed(&self, text: &str) -> Vec<f32>;
    fn dim(&self) -> usize;
}
```

Everything below is about what to put behind them.

---

## Read this before the recommendations

**Nothing on this page is measured by this project, and that is deliberate.**

The benchmark harness uses a deterministic line-matching reasoner
(`LocalReasoner`) on purpose, so that every published figure measures *context
assembly* rather than model quality. That choice is what makes the numbers in
[RESULTS](../RESULTS.md) mean anything — and it is exactly what disqualifies this
repository from having an opinion, backed by evidence, about which model is best
at anything.

So treat this page as **selection criteria plus informed judgement**, not as a
result. It is the kind of page that in this project would normally carry a table
and a control; it carries neither, and the honest thing is to say so at the top
rather than to let the surrounding rigour rub off on it.

Two further limits worth stating plainly:

- **This reflects a knowledge cutoff.** Model families move faster than any
  document. Names and generations here should be verified against the vendor's
  current lineup before you act on them; the *criteria* age far better than the
  names.
- **Public leaderboards are conditioned samples too.** A model tuned on the
  benchmark you are reading is the same defect this project keeps finding in its
  own instruments. Run your own task, on your own data, before believing a rank.

---

## The two jobs, which have different requirements

The [two-system split](architecture/two-system-split.md) means there are two very
different model-shaped holes here, and filling both with your most expensive
model is usually wrong.

### Solution B — the indexer

Reads each incoming turn and emits structured state: nodes, values, typed edges,
supersession candidates. It runs **on every turn**, it produces structured output
rather than prose, and it never needs to be charming.

What it needs: **reliable structured output**, low latency, low cost per call,
and stability across runs. What it does not need: deep reasoning, long context
(it sees one turn), or broad world knowledge.

This is the single best place in the system to use a small, fast model. The
design's claim that B can be "a much smaller model plus deterministic data
structures" is [still an open question](open-questions.md#8-how-small-can-solution-bs-model-be)
at the quality cliff, but the direction is not in doubt — extraction is the cheap half.

### Solution A — the reasoner

Answers the query from the assembled working set. It runs once per turn, sees a
small bounded context by construction, and its output is what the user reads.

What it needs: reasoning quality, instruction-following, and honest refusal when
the context does not contain the answer. What it needs **less** of than you would
expect: a long context window. That is the entire point of the runtime — the
window is bounded at `B_attention`, so paying for a million-token context you
never fill is paying for the problem this design removes.

> A useful consequence: bounded attention makes *smaller* models more viable at
> Solution A than they would be on raw transcripts, because the hard part —
> finding the relevant material in a haystack — has already happened. This is
> untested here and is the most interesting thing on this page to test.

---

## By category

Categories overlap. Pick by the *dominant* constraint, not by the label.

### Coding

**What actually matters:** long-context code comprehension across many files,
diff-shaped editing without collateral damage, tool-use reliability over long
sessions, and instruction-following under a repository's conventions.

**Frontier tier** — refactors spanning many files, unfamiliar codebases,
architecture work, anything where a wrong answer is expensive to detect:
Claude Opus 5 (`claude-opus-5`); OpenAI's frontier reasoning line; Google's
Gemini Pro tier.

**Workhorse tier** — the bulk of day-to-day work, and the right default for
agentic coding loops where you will run thousands of calls:
Claude Sonnet 5 (`claude-sonnet-5`); the mid-tier equivalents from other vendors.
Sonnet-class models are usually the best cost/quality point for coding agents,
and the gap to frontier narrows sharply once the task is well-scoped.

**Fast tier** — lint-shaped work, commit messages, mechanical edits, classifying
which files matter before a bigger model reads them:
Claude Haiku 4.5 (`claude-haiku-4-5-20251001`).

**Open-weight** — Qwen's coder line and DeepSeek's coder line are the strongest
open options; Llama and Mistral families are viable and generally weaker at code
specifically. Choose these for cost, air-gap, or licence reasons rather than
expecting parity at the frontier.

### Security work

**Scope note.** This section is about authorized work: penetration testing under
contract, CTF competitions, vulnerability research, reverse engineering,
detection engineering, and defensive analysis. Model choice does not change the
authorization question, and no model on any list here makes unauthorized access
lawful.

**What actually matters, and it is not "how willing is the model":**

1. **Long-context code and binary reading.** Vulnerability research is mostly
   reading — large diffs, decompiler output, unfamiliar protocols. This is the
   binding constraint far more often than reasoning depth.
2. **Tool-use reliability.** Real work is driven through tooling. A model that
   silently malforms a tool call once in fifty is worse than a slower model that
   never does.
3. **Calibrated uncertainty.** "This looks exploitable" and "this is exploitable"
   are different claims, and a model that will not distinguish them wastes your
   time on false leads. This matters more here than in almost any other category.
4. **Engaging with the material.** A model that refuses to read a CVE writeup is
   useless for defensive work too. Frontier models from the major vendors handle
   authorized security context well; the friction is usually in how the request
   is framed, not in the model.

**Practical picks:** Claude Opus 5 for analysis and exploit-development reasoning
in an authorized engagement; Claude Sonnet 5 for the volume work of triage,
log analysis, and detection rules; Haiku 4.5 for high-volume classification, such
as first-pass alert triage where recall matters more than depth.

**Reverse engineering** deserves its own note: decompiler output is long, ugly and
low-signal-per-token, so context length and patience with noisy input dominate.
This is one of the few categories where the largest available context genuinely
earns its cost.

### Automation and long-running agents

**What actually matters:** consistency over hundreds of turns, cost per turn
(because there are many), tool-call correctness, and graceful degradation rather
than confident drift when state gets stale — which is the failure mode this whole
runtime exists to address.

**The dominant consideration is cost per turn, not peak capability.** An agent
loop that runs 500 turns at frontier prices is usually a design that has not been
decomposed. The common shape that works:

| role in the loop | model tier | why |
|---|---|---|
| planning / decomposition | frontier | run once, wrong answers are expensive |
| per-step execution | workhorse | run often, well-scoped |
| extraction / classification | fast | run constantly, structured output |
| final synthesis | frontier or workhorse | user-visible |

Claude Sonnet 5 is the usual default for the execution tier; Haiku 4.5 for the
extraction tier; Opus 5 reserved for planning and for the steps where being
wrong is not recoverable.

**Determinism note.** If you need reproducible runs — and this project's whole
evaluation posture depends on determinism — a hosted model is not deterministic
even at temperature zero. Where reproducibility is the requirement rather than
quality, a deterministic local model or a rule-based component beats any API.

### Extraction and structured output

The Solution B role, and a category in its own right because so many pipelines
need it. **Native structured-output or tool-calling support matters far more than
model size.** A small model with enforced schema output beats a large model
asked politely for JSON, because the failure mode of the latter is a parse error
at 3am rather than a slightly worse answer.

Haiku 4.5 and the small tiers from other vendors are the right default. Escalate
only when extraction quality is *measurably* the bottleneck — measured, not
assumed, because "the model is not smart enough" is the most common wrong
diagnosis for what is actually a prompt or schema problem.

### Embeddings

Not a chat model, and worth separating. The bundled embedder here is a
256-dimensional hashing embedding — it matches shared vocabulary, not meaning,
and [use-cases](use-cases.md) is explicit that paraphrase is a poor fit.

Swapping in a learned embedder is a constructor argument, not a patch — see
[`examples/custom_embedder.rs`](https://github.com/s-b-repo/subnext/blob/main/examples/custom_embedder.rs).
Any provider's text-embedding endpoint, or a local sentence-transformer model,
plugs into the `Embedder` trait directly.

**Read the cost before you do it.** A learned embedder makes this runtime stop
being offline and deterministic, and every figure in [RESULTS](../RESULTS.md)
would need re-measuring against the new channel rather than inherited. Vectors
from one embedder are never comparable with another's — `dim()` is checked rather
than assumed for exactly this reason — so changing embedder invalidates a stored
index, not just future queries.

### Local and air-gapped

Choose by what fits in memory after quantisation, then by task:

- **General + coding:** Qwen and DeepSeek families are currently the strongest
  open-weight options; Llama is the safest default for ecosystem support;
  Mistral for the small-and-fast end.
- **Extraction:** almost any modern 7–14B model with enforced schema output is
  adequate, and this is where local models are most obviously good enough.
- **Embeddings:** local sentence-transformer models are mature and are the least
  compromised part of a local stack.

Quantisation is usually a better trade than dropping a model tier: a larger model
at 4-bit typically beats a smaller model at 8-bit for the same memory. Verify on
your task rather than trusting that as a rule.

---

## Selection criteria, condensed

If you remember one thing from this page, make it this table rather than any
model name — the names age, the questions do not.

| ask this | if the answer is yes |
|---|---|
| Does it run on every turn? | drop a tier; cost per call dominates |
| Does it emit structured output? | prioritise schema enforcement over size |
| Does a wrong answer get caught immediately? | drop a tier; cheap errors are affordable |
| Does a wrong answer surface days later? | pay for the frontier tier |
| Do you need byte-identical reruns? | no hosted API qualifies, at any temperature |
| Is the input long, ugly and low-signal? | context length beats reasoning depth |
| Is the task well-scoped and repetitive? | the workhorse tier is almost certainly enough |
| Are you sure the model is the bottleneck? | measure it — usually it is the prompt or the retrieval |

That last row is the one this project would insist on. The recurring defect
documented throughout this repository is a claim that outran its evidence, and
"we need a bigger model" is that claim in its most expensive form.
