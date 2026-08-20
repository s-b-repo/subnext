"""Telemetry — the metrics the evaluation design asks for.

Phase 5 of the roadmap names the numbers that could falsify the `O(k + r)`
claim, so the runtime records them rather than asserting them:

* **escalation rate** — how often the cheap level was insufficient. The
  router's honesty check; a rising rate means `sufficient(L, q)` is wrong.
* **stale-fact read rate** — how often a stale node reached the model. Should
  be zero; anything else means invalidation is leaking.
* **prefetch hit rate** — whether speculation pays for its waste.
* **tokens per resolved query** — `k + r`, measured, versus what the whole
  history would have cost.
* **audit-path completeness** — the fraction of `explain()` walks that reach
  evidence without truncating.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from statistics import mean


@dataclass
class Turn:
    query: str
    qtype: str
    tokens: int
    budget: int
    admitted: int
    considered: int
    escalations: int = 0
    stale_reads: int = 0
    demotions: int = 0
    overflow: bool = False
    answered: bool = True


@dataclass
class Telemetry:
    turns: list[Turn] = field(default_factory=list)
    escalations: int = 0
    stale_reads: int = 0
    explain_calls: int = 0
    explain_complete: int = 0
    history_tokens: int = 0
    """What the naive baseline would have paid: every token ever ingested."""

    def record_turn(self, turn: Turn) -> Turn:
        self.turns.append(turn)
        self.escalations += turn.escalations
        self.stale_reads += turn.stale_reads
        return turn

    def record_explain(self, complete: bool) -> None:
        self.explain_calls += 1
        self.explain_complete += int(complete)

    def report(self, extra: dict | None = None) -> dict:
        turns = len(self.turns)
        tokens = [t.tokens for t in self.turns] or [0]
        report = {
            "turns": turns,
            "escalation_rate": round(self.escalations / turns, 3) if turns else None,
            "stale_fact_read_rate": round(self.stale_reads / turns, 3) if turns else None,
            "tokens_per_query_mean": round(mean(tokens), 1),
            "tokens_per_query_max": max(tokens),
            "budget_overflows": sum(1 for t in self.turns if t.overflow),
            "demotions": sum(t.demotions for t in self.turns),
            "audit_path_completeness": (
                round(self.explain_complete / self.explain_calls, 3) if self.explain_calls else None
            ),
            "history_tokens": self.history_tokens,
            "compression_ratio": (
                round(self.history_tokens / mean(tokens), 1) if turns and mean(tokens) else None
            ),
        }
        if extra:
            report.update(extra)
        return report

    def format_report(self, extra: dict | None = None) -> str:
        report = self.report(extra)
        width = max(len(k) for k in report) if report else 0
        return "\n".join(f"{k:<{width}} : {v}" for k, v in report.items())
