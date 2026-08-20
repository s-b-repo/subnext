"""The attention budget: context assembly as constrained optimisation.

    maximize   U(S)
    over       S ⊆ memory objects
    subject to Σ cost(x) ≤ B_attention

Because the same node can be admitted at several ladder levels, this is a
*multiple-choice* knapsack: pick at most one (node, level) option per node.
Solved exactly by DP over quantised token costs — candidate counts here are in
the hundreds, so exactness is affordable and removes a whole class of "why did
it drop that" debugging.

Demotion beats eviction: dropping a node to a cheaper level keeps some utility,
evicting keeps none. That falls out of the formulation for free — a cheaper
option with non-zero value dominates the skip option — which is exactly why
this is an optimiser and not a heuristic sort.
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class Option:
    level: str
    cost: int
    utility: float


@dataclass
class Candidate:
    node_id: str
    options: list[Option]
    pinned: bool = False
    """Pinned candidates must be admitted at some level (their cheapest, if
    the budget is tight). Used for goals, constraints and `explain()` paths
    the query explicitly demands."""

    def cheapest(self) -> Option:
        return min(self.options, key=lambda o: o.cost)


@dataclass
class Allocation:
    chosen: dict[str, Option] = field(default_factory=dict)
    tokens: int = 0
    utility: float = 0.0
    budget: int = 0
    dropped: list[str] = field(default_factory=list)
    demoted: list[tuple[str, str, str]] = field(default_factory=list)
    """(node_id, wanted_level, admitted_level) — the demotions the budget forced."""
    overflow: bool = False
    """True when even the pinned set did not fit; the caller must shrink the
    plan rather than silently blow the budget."""


MAX_DP_BUCKETS = 400
"""Cost quantisation is chosen so the DP stays this wide regardless of budget:
small budgets get exact token resolution, large ones a few tokens of slack.
Costs always round *up*, so the solution can never exceed `B_attention`."""


def solve(
    candidates: list[Candidate],
    budget: int,
    quantum: int | None = None,
    preferred: dict[str, str] | None = None,
) -> Allocation:
    """Exact multiple-choice knapsack over quantised costs."""
    preferred = preferred or {}
    alloc = Allocation(budget=budget)
    if budget <= 0 or not candidates:
        alloc.dropped = [c.node_id for c in candidates]
        alloc.overflow = bool(candidates and any(c.pinned for c in candidates))
        return alloc

    # Pinned candidates get their cheapest option reserved off the top so a
    # flood of cheap high-utility trivia can never squeeze out a constraint.
    reserved = 0
    forced: dict[str, Option] = {}
    for cand in candidates:
        if cand.pinned and cand.options:
            option = cand.cheapest()
            forced[cand.node_id] = option
            reserved += option.cost
    if reserved > budget:
        # Not even the mandatory set fits: admit in utility order until full.
        ordered = sorted(forced.items(), key=lambda kv: -kv[1].utility)
        for node_id, option in ordered:
            if alloc.tokens + option.cost <= budget:
                alloc.chosen[node_id] = option
                alloc.tokens += option.cost
                alloc.utility += option.utility
            else:
                alloc.dropped.append(node_id)
        alloc.overflow = True
        alloc.dropped = [
            c.node_id for c in candidates if c.node_id not in alloc.chosen
        ]
        return alloc

    free_budget = budget - reserved
    if quantum is None:
        quantum = max(1, _ceil_div(free_budget, MAX_DP_BUCKETS))
    buckets = max(1, free_budget // quantum)

    # For a pinned node the DP optimises the *upgrade* from its reserved
    # cheapest option; for everyone else it optimises admission from nothing.
    items: list[tuple[str, list[tuple[int, float, Option | None]]]] = []
    for cand in candidates:
        if not cand.options:
            continue
        base = forced.get(cand.node_id)
        choices: list[tuple[int, float, Option | None]] = []
        if base is None:
            choices.append((0, 0.0, None))  # skip
            for option in cand.options:
                choices.append((_ceil_div(option.cost, quantum), option.utility, option))
        else:
            choices.append((0, 0.0, base))  # keep the reserved cheapest
            for option in cand.options:
                if option.cost <= base.cost:
                    continue
                choices.append(
                    (_ceil_div(option.cost - base.cost, quantum),
                     option.utility - base.utility, option)
                )
        items.append((cand.node_id, choices))

    NEG = float("-inf")
    dp = [0.0] * (buckets + 1)
    back: list[list[int]] = []
    for _node_id, choices in items:
        new_dp = [NEG] * (buckets + 1)
        picks = [-1] * (buckets + 1)
        for capacity in range(buckets + 1):
            best_value, best_choice = NEG, -1
            for ci, (cost_b, value, _opt) in enumerate(choices):
                if cost_b > capacity:
                    continue
                prev = dp[capacity - cost_b]
                if prev == NEG:
                    continue
                total = prev + value
                if total > best_value:
                    best_value, best_choice = total, ci
            new_dp[capacity] = best_value
            picks[capacity] = best_choice
        dp = new_dp
        back.append(picks)

    capacity = max(range(buckets + 1), key=lambda c: dp[c])
    selections: dict[str, Option | None] = {}
    for i in range(len(items) - 1, -1, -1):
        node_id, choices = items[i]
        ci = back[i][capacity]
        if ci < 0:
            selections[node_id] = forced.get(node_id)
            continue
        cost_b, _value, option = choices[ci]
        selections[node_id] = option
        capacity -= cost_b

    for cand in candidates:
        option = selections.get(cand.node_id) or forced.get(cand.node_id)
        if option is None:
            alloc.dropped.append(cand.node_id)
            continue
        alloc.chosen[cand.node_id] = option
        alloc.tokens += option.cost
        alloc.utility += option.utility
        wanted = preferred.get(cand.node_id)
        if wanted and wanted != option.level:
            alloc.demoted.append((cand.node_id, wanted, option.level))
    return alloc


def _ceil_div(a: int, b: int) -> int:
    return -(-a // b)
