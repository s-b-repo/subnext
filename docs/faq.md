# FAQ

**Is this just RAG with extra steps?**
No. RAG retrieves documents by similarity. DCR retrieves whichever *representation* is cheapest
and sufficient — which may be an exact span, a vector, a cached fact, or a recomputation — and it
traverses dependency edges, not just similarity.

**Is this a replacement for RLM?**
No. It builds on RLM's primitive (context as a manipulable program object with recursive model
calls) and adds a stateful runtime above it.

**Why not just use a longer context window?**
Longer windows increase `O(N²)` attention pressure and don't stop lost-in-the-middle behavior.
The goal is unlimited *history* with a small *attention* footprint: unlimited history ≠ unlimited
attention.

**Doesn't summarizing lose information?**
Yes — which is why L0 is immutable and always available, and why every cached fact carries source
spans. Loss is recoverable; see [open questions](open-questions.md) #3 for the unresolved part.

**Is there code?**
Yes — `dcr/` is a dependency-free reference implementation of this specification, and
[implementation](implementation.md) explains what it enforces and what it measures. The wiki
remains the design; the code exists to make the design falsifiable rather than to be depended on.

**How would you know it's actually faster than RLM?**
You wouldn't, yet. Phase 5 of the roadmap defines the benchmark that could falsify the claim.
`python -m dcr bench --scaling` measures the part that is measurable without a competing system:
attention cost per query stays flat (~415 tokens) while history grows 33x. That supports the
`O(k + r)` shape for the active context; it is not a latency comparison against RLM, and the
implementation's own retrieval is still a linear scan, which the cost model says must be
sub-linear before the claim holds at scale.

**Why two systems instead of one model?**
Because the two jobs want opposite optimizations. Reasoning wants low latency, small context, high
quality. Memory wants huge storage, exact retrieval, indexing, background consolidation. Solution B
can be a much smaller model plus deterministic data structures. See
[the two-system split](architecture/two-system-split.md).

**Isn't the memory system just a database?**
Largely, yes — and that is the point. Most of Solution B's work is deterministic. The novelty is
that it returns *state* at a chosen representation level under an attention budget, not documents.

**What's the single most fragile assumption?**
That extracted state is correct. A confidently cached wrong claim is worse than a bloated prompt.
Mandatory provenance is the mitigation, not a solution.
