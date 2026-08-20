"""Reasoner adapters — "Solution A".

Two implementations, for two different purposes:

* `LocalReasoner` is deterministic, offline and deliberately literal. It can
  only answer from the text of the active context it is handed, and it signals
  insufficiency with `#ESCALATE <node_id>` instead of guessing. That makes it
  the right instrument for evaluating the *runtime*: if the answer is wrong,
  the planner put the wrong thing in the window, and no model quality can be
  blamed for it.

* `AnthropicLLM` is the real Reasoner, via the official SDK.

The asymmetry the wiki argues for shows up here: the Memory Runtime (indexing,
addressing, traversal, memoisation, invalidation) needs no model at all, and
the Reasoner never sees history — only `k`.
"""

from __future__ import annotations

import os
import re
from typing import Protocol

from .embed import content_tokens

ESCALATE_RE = re.compile(r"#ESCALATE\s+([A-Za-z0-9_]+)")
DEFAULT_MODEL = "claude-opus-5"

REASONER_SYSTEM = """You are the Reasoner in a Dynamic Context Runtime.

You never see the full history. You see a small ACTIVE CONTEXT assembled by a
memory runtime: cached facts at L2, summaries at L1, exact source spans at L0,
and computed results at L3. Every line starts with the node id it came from.

Rules:
- Answer only from the active context. Do not invent facts.
- Cite the node ids you used, in the form [node_id].
- If the context is insufficient — you need the exact wording of something you
  were given only as a summary or a fact — reply with exactly:
      #ESCALATE <node_id>
  and nothing else. The runtime will re-plan and give you that node at L0.
- A fact marked status=stale must not be used; escalate instead.
"""


class LLM(Protocol):
    def complete(self, prompt: str, system: str | None = None, max_tokens: int = 1024) -> str: ...


class LocalReasoner:
    """Offline, deterministic reasoner over the rendered active context."""

    LINE = re.compile(r"^\[(?P<node>\w+) (?P<level>L[0-3])[^\]]*\]\s*(?P<body>.*)$")

    def __init__(self, threshold: float = 0.25, escalate_below: float = 0.6) -> None:
        self.threshold = threshold
        self.escalate_below = escalate_below
        """Between `threshold` and this, the best match is plausible but thin —
        the compact form probably dropped the detail the question turns on.
        That is precisely the "model signals insufficiency" case the escalation
        protocol exists for."""
        self.escalated: set[str] = set()
        self._current_query: str | None = None

    def complete(self, prompt: str, system: str | None = None, max_tokens: int = 1024) -> str:
        query = self._query_of(prompt)
        if query != self._current_query:
            # Escalation memory is per question, not per session: refusing to
            # escalate a node twice across unrelated questions would silently
            # answer a later one from a summary that already proved too thin.
            self._current_query = query
            self.escalated.clear()
        lines = self._lines(prompt)
        if not lines:
            return "I don't have that in the active context."
        wants_quote = bool(re.search(r"\b(quote|exact|verbatim|word for word)\b", query, re.I))
        scored = sorted(
            ((self._score(query, line), line) for line in lines), key=lambda x: -x[0]
        )
        best_score, best = scored[0]
        escalatable = best["level"] != "L0" and best["node"] not in self.escalated
        if best_score < self.threshold:
            # Nothing usable. If the best candidate is a compressed form with
            # *some* overlap, the detail may be in its raw span — escalate
            # rather than answer "I don't know" from a summary.
            if best_score >= 0.1 and escalatable:
                self.escalated.add(best["node"])
                return f"#ESCALATE {best['node']}"
            return "I don't have that in the active context."
        weak = best_score < self.escalate_below
        if (wants_quote or weak) and escalatable:
            # Either we were asked to quote something we only have compressed,
            # or the match is too thin to trust the compression. Escalate
            # instead of guessing.
            self.escalated.add(best["node"])
            return f"#ESCALATE {best['node']}"
        body = best["body"]
        if best["level"] == "L0":
            body = body.strip().strip('"')
            return f"{body} [{best['node']}]"
        value = body.split("·")[0].strip()
        if "=" in value and not wants_quote:
            value = value.split("=", 1)[1].strip()
        return f"{value} [{best['node']}]"

    # -- helpers -----------------------------------------------------------
    @staticmethod
    def _query_of(prompt: str) -> str:
        match = re.search(r"(?:^|\n)QUESTION:\s*(.+)", prompt)
        return match.group(1).strip() if match else prompt.strip().splitlines()[-1]

    def _lines(self, prompt: str) -> list[dict]:
        out = []
        for raw in prompt.splitlines():
            match = self.LINE.match(raw.strip())
            if match:
                out.append(match.groupdict())
        return out

    def _score(self, query: str, line: dict) -> float:
        q = set(content_tokens(query))
        if not q:
            return 0.0
        body = line["body"]
        # `server.ip = 10.0.9.7` should match "server ip", so split dotted keys.
        text = body.replace(".", " ") + " " + body
        tokens = set(content_tokens(text))
        overlap = len(q & tokens) / len(q)
        # A question is about the *subject*, so key matches count double.
        head = body.split("=")[0]
        head_tokens = set(content_tokens(head.replace(".", " ")))
        if q & head_tokens:
            overlap += 0.5 * len(q & head_tokens) / len(q)
        return overlap


class AnthropicLLM:
    """The real Reasoner, through the official Anthropic SDK.

    Defaults follow current API guidance: Claude Opus 5, adaptive thinking, and
    server-side refusal fallbacks enabled. Install with `pip install anthropic`.
    """

    def __init__(
        self,
        model: str = DEFAULT_MODEL,
        client=None,
        *,
        max_tokens: int = 16000,
        effort: str = "high",
        fallbacks: bool = True,
    ) -> None:
        self.model = model
        self.max_tokens = max_tokens
        self.effort = effort
        self.fallbacks = fallbacks
        if client is not None:
            self.client = client
        else:
            try:
                import anthropic
            except ImportError as exc:  # pragma: no cover - environment dependent
                raise ImportError(
                    "AnthropicLLM needs the official SDK: pip install anthropic"
                ) from exc
            self.client = anthropic.Anthropic()

    def complete(self, prompt: str, system: str | None = None, max_tokens: int | None = None) -> str:
        params = dict(
            model=self.model,
            max_tokens=max_tokens or self.max_tokens,
            system=system or REASONER_SYSTEM,
            thinking={"type": "adaptive"},
            output_config={"effort": self.effort},
            messages=[{"role": "user", "content": prompt}],
        )
        response = self._create(params)
        if getattr(response, "stop_reason", None) == "refusal":
            details = getattr(response, "stop_details", None)
            category = getattr(details, "category", None)
            return f"[refused: {category}]"
        return "".join(
            block.text for block in response.content if getattr(block, "type", "") == "text"
        ).strip()

    def _create(self, params: dict):
        if self.fallbacks:
            try:
                return self.client.beta.messages.create(
                    betas=["server-side-fallback-2026-07-01"], fallbacks="default", **params
                )
            except Exception:
                # Older SDK, or a surface that rejects the parameter (Bedrock,
                # Vertex, Foundry). Fall through to the plain call.
                self.fallbacks = False
        return self.client.messages.create(**params)

    def extract(self, text: str, schema: dict) -> dict:
        """Structured extraction — used by `LLMExtractor`."""
        import json

        response = self.client.messages.create(
            model=self.model,
            max_tokens=4096,
            messages=[{"role": "user", "content": text}],
            output_config={"format": {"type": "json_schema", "schema": schema}},
        )
        if getattr(response, "stop_reason", None) == "refusal":
            return {}
        payload = next(b.text for b in response.content if b.type == "text")
        return json.loads(payload)


NODE_SCHEMA = {
    "type": "object",
    "properties": {
        "nodes": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["claim", "decision", "goal", "constraint", "open_question"],
                    },
                    "key": {"type": "string"},
                    "value": {"type": "string"},
                    "confidence": {"type": "number"},
                },
                "required": ["kind", "key", "value", "confidence"],
                "additionalProperties": False,
            },
        }
    },
    "required": ["nodes"],
    "additionalProperties": False,
}

EXTRACT_PROMPT = """Extract durable state from the passage below.

Emit only facts that are stated in the passage. Use a stable dotted `key` for
the subject (e.g. server.ip, deploy.window, owner). For non-claims use the kind
and leave `key` as a short slug. Set confidence below 0.7 if the passage hedges.
A wrong fact cached with high confidence is worse than no fact at all.

PASSAGE:
"""


class LLMExtractor:
    """Node extraction by a model — the reasoning-shaped part of Solution B.

    Open question #2: this is where a wrong `Claim` can enter the cache with
    high confidence. Mitigations kept here: every node still gets its span,
    confidence is thresholded, and contradictions are detected downstream
    rather than trusted away.
    """

    def __init__(self, llm: AnthropicLLM, min_confidence: float = 0.6) -> None:
        self.llm = llm
        self.min_confidence = min_confidence

    def __call__(self, text: str, span_id: str, span_text: str) -> list[dict]:
        if len(span_text.strip()) < 24:
            return []
        data = self.llm.extract(EXTRACT_PROMPT + span_text, NODE_SCHEMA)
        out = []
        for node in data.get("nodes", []):
            if node.get("confidence", 0) < self.min_confidence:
                continue
            out.append(
                {
                    "kind": node["kind"],
                    "value": node["value"].strip(),
                    "key": node.get("key") or None,
                    "span_id": span_id,
                    "confidence": float(node["confidence"]),
                    "corrective": False,
                }
            )
        return out


def default_reasoner() -> LLM:
    """Anthropic when credentials and the SDK are present, local otherwise."""
    try:
        import anthropic  # noqa: F401
    except ImportError:
        return LocalReasoner()
    if not (os.environ.get("ANTHROPIC_API_KEY") or os.environ.get("ANTHROPIC_AUTH_TOKEN")):
        return LocalReasoner()
    return AnthropicLLM()
