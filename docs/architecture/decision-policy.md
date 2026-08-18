# Runtime Decision Policy

For every piece of context, every turn, the runtime automatically decides:

```text
keep raw?
summarize?
vectorize?
execute?
cache?
discard from active window?
prefetch?
recompute?
```

based on the current task.

## Decision table (starting heuristics)

| Decision | Trigger |
|---|---|
| keep raw (L0) | query needs exact wording, quotes, code, identifiers, or legal/audit fidelity |
| summarize (L1) | node is referenced for gist/value only, and is large |
| vectorize (L2) | node must be findable by meaning; state nodes as well as text chunks |
| execute (L3) | answer is a function of known inputs and is cheaper to compute than to read |
| cache | a derivation or fact is settled, reusable, and groundable to spans |
| discard from active | node not referenced by current plan and not above prefetch threshold |
| prefetch | `P(m_i | q_t, S_t) > τ` and budget available |
| recompute | node is `stale`, or dependencies changed since `timestamp` |

## Cost-based selection

Choose the minimum-cost level `L` such that `sufficient(L, q_t)` holds. `sufficient` is the hard
part and is deliberately left open — see [open questions](../open-questions.md). Practical proxy:
route by query type, then verify by checking whether the model requested escalation to a lower
level (L1 → L0). Escalation rate is the metric to minimize.

## Escalation and de-escalation

- **Escalate** (cheaper → richer) when the model signals insufficiency or confidence is low.
- **De-escalate** when a node has been read at L0 repeatedly with no new information extracted —
  collapse it into a cached fact instead.
