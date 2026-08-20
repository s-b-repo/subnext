"""L3 — the execution layer and `C_t`.

Derivations are registered Python callables. Results are memoised on a key
built from the derivation name, its literal inputs, *and* the fingerprints of
the nodes it depends on — so when an upstream fact is corrected the key
changes, the memo misses, and the value is recomputed rather than silently
served stale. That is the whole point of keying on provenance instead of on
the question text.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Callable

from .ids import digest
from .nodes import CALCULATION, Node, new_node

Derivation = Callable[..., Any]


class UnknownDerivation(KeyError):
    pass


@dataclass
class MemoEntry:
    key: str
    value: Any
    inputs: dict
    deps: tuple[str, ...]
    hits: int = 0


@dataclass
class ExecutionLayer:
    graph: Any = None
    derivations: dict[str, Derivation] = field(default_factory=dict)
    memo: dict[str, MemoEntry] = field(default_factory=dict)
    calls: int = 0
    hits: int = 0

    def register(self, name: str, fn: Derivation) -> Derivation:
        self.derivations[name] = fn
        return fn

    def derivation(self, name: str):
        def wrap(fn: Derivation) -> Derivation:
            self.register(name, fn)
            return fn
        return wrap

    # -- keying ------------------------------------------------------------
    def key_for(self, name: str, inputs: dict, deps: tuple[str, ...]) -> str:
        fingerprints = []
        for dep in sorted(deps):
            node = self.graph.get(dep) if self.graph else None
            fingerprints.append(node.fingerprint() if node else dep)
        return digest(name, sorted(inputs.items()), fingerprints, length=16)

    # -- execution ---------------------------------------------------------
    def call(self, name: str, inputs: dict | None = None, deps: tuple[str, ...] = ()) -> Any:
        if name not in self.derivations:
            raise UnknownDerivation(name)
        inputs = dict(inputs or {})
        key = self.key_for(name, inputs, tuple(deps))
        entry = self.memo.get(key)
        if entry is not None:
            entry.hits += 1
            self.hits += 1
            return entry.value
        value = self.derivations[name](**inputs)
        self.memo[key] = MemoEntry(key, value, inputs, tuple(deps))
        self.calls += 1
        return value

    def run(self, node: Node) -> Any:
        """Execute the derivation attached to a node (used by Ladder for L3)."""
        derivation = node.meta.get("derivation")
        if not derivation:
            return None
        value = self.call(derivation["name"], derivation.get("inputs", {}), tuple(node.dependencies))
        node.meta.setdefault("last_result", None)
        node.meta["last_result"] = value
        return value

    def compute_node(
        self,
        name: str,
        inputs: dict | None = None,
        deps: tuple[str, ...] | list[str] = (),
        *,
        key: str | None = None,
        source_spans: tuple[str, ...] | list[str] = (),
        confidence: float = 1.0,
    ) -> Node:
        """Run a derivation and materialise the result as a calculation node."""
        deps = tuple(deps)
        value = self.call(name, inputs, deps)
        node = new_node(
            CALCULATION,
            str(value),
            dependencies=deps,
            source_spans=tuple(source_spans),
            confidence=confidence,
            key=key or name,
            meta={"derivation": {"name": name, "inputs": dict(inputs or {})}},
        )
        node.level_cache["L3"] = value
        return node

    def stats(self) -> dict:
        return {
            "derivations": sorted(self.derivations),
            "memo_entries": len(self.memo),
            "executions": self.calls,
            "memo_hits": self.hits,
        }
