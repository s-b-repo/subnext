"""DCR — Dynamic Context Runtime.

An implementation of the specification in `docs/`: unlimited external state
plus a small, planned working set.

    from dcr import DCR

    rt = DCR(budget=800)
    rt.ingest(open("incident.log").read())
    print(rt.ask("what is the server ip?").text)

The pieces map one-to-one onto the wiki:

    dcr.spans       L0 immutable raw store           (R_t)
    dcr.ladder      representation ladder L0-L3
    dcr.nodes       typed state schema
    dcr.graph       dynamic memory graph             (S_t, E_t)
    dcr.execute     execution layer + memoisation    (C_t)
    dcr.indexer     state indexer / node extraction
    dcr.index       hybrid retrieval over text *and* state
    dcr.policy      runtime decision policy
    dcr.budget      attention-budget knapsack        (B_attention)
    dcr.planner     relevance planner                (k + r)
    dcr.speculation speculative context              (tau, prefetch)
    dcr.telemetry   escalation / staleness / prefetch metrics
    dcr.runtime     the facade tying it together
"""

from .budget import Allocation, Candidate, Option, solve
from .execute import ExecutionLayer
from .graph import ExplainPath, MemoryGraph, ProvenanceError
from .index import HybridIndex
from .indexer import HeuristicExtractor, IndexResult, StateIndexer
from .ladder import L0, L1, L2, L3, Ladder
from .llm import AnthropicLLM, LLMExtractor, LocalReasoner, default_reasoner
from .nodes import Edge, Node, new_node
from .planner import ActiveContext, RelevancePlanner, Weights
from .policy import DecisionPolicy
from .runtime import Answer, ConsistencyInterrupt, DCR
from .spans import RawStore, Span
from .speculation import Predictor, Speculator
from .telemetry import Telemetry

__version__ = "0.1.0"

__all__ = [
    "DCR", "Answer", "ConsistencyInterrupt",
    "RawStore", "Span", "Ladder", "L0", "L1", "L2", "L3",
    "MemoryGraph", "ExplainPath", "ProvenanceError", "Node", "Edge", "new_node",
    "StateIndexer", "HeuristicExtractor", "IndexResult", "HybridIndex",
    "DecisionPolicy", "RelevancePlanner", "ActiveContext", "Weights",
    "Candidate", "Option", "Allocation", "solve",
    "ExecutionLayer", "Predictor", "Speculator", "Telemetry",
    "LocalReasoner", "AnthropicLLM", "LLMExtractor", "default_reasoner",
    "__version__",
]
