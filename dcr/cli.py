"""Command line interface.

    python -m dcr demo
    python -m dcr ingest notes/*.md --store .dcr.json
    python -m dcr ask "what is the server ip?" --store .dcr.json --budget 600
    python -m dcr plan "why did we roll back?" --store .dcr.json --explain
    python -m dcr explain <node_id> --store .dcr.json
    python -m dcr stats --store .dcr.json
    python -m dcr bench
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from .llm import LocalReasoner, default_reasoner
from .runtime import DCR

DEFAULT_STORE = ".dcr.json"


def _open(store: str, budget: int) -> DCR:
    path = Path(store)
    if path.exists():
        runtime = DCR.load(path, budget=budget)
        runtime.budget = budget
        runtime.planner.budget = budget
        return runtime
    return DCR(budget=budget)


def _expand(pattern: str) -> list[Path]:
    """Accept a file, a directory, or a glob — absolute or relative."""
    path = Path(pattern)
    if path.is_file():
        return [path]
    if path.is_dir():
        # Chronological, not alphabetical: ingest order is revision order, so a
        # directory listed by name would let `a.md` supersede `z.md`.
        return sorted((p for p in path.rglob("*") if p.is_file()),
                      key=lambda p: (p.stat().st_mtime, p.name))
    if path.is_absolute():
        anchor = Path(path.anchor)
        return sorted(anchor.glob(str(path.relative_to(anchor))))
    return sorted(Path().glob(pattern))


def cmd_ingest(args) -> int:
    runtime = _open(args.store, args.budget)
    total = 0
    for pattern in args.paths:
        matches = _expand(pattern)
        if not matches:
            print(f"no match: {pattern}", file=sys.stderr)
        for path in matches:
            if not path.is_file():
                continue
            result = runtime.ingest_file(path)
            total += len(result.nodes)
            print(f"{path}: {result.summary()}")
            for old, new in result.contradictions:
                print(f"  contradiction: {old} superseded by {new}")
    runtime.save(args.store)
    print(f"\n{total} state nodes; store: {args.store}")
    return 0


def cmd_ask(args) -> int:
    runtime = _open(args.store, args.budget)
    reasoner = LocalReasoner() if args.local else default_reasoner()
    answer = runtime.ask(args.query, reasoner=reasoner)
    print(answer.text)
    print(
        f"\n-- {answer.tokens}/{args.budget} tokens, {len(answer.context.entries)} nodes, "
        f"{answer.escalations} escalation(s), cited {', '.join(answer.cited) or 'nothing'}"
    )
    if args.show_context:
        print("\n" + answer.context.render())
    runtime.save(args.store)
    return 0


def cmd_plan(args) -> int:
    runtime = _open(args.store, args.budget)
    ctx = runtime.plan(args.query)
    print(ctx.render())
    if args.explain:
        print("\n" + runtime.planner.explain_plan(ctx))
    return 0


def cmd_explain(args) -> int:
    runtime = _open(args.store, args.budget)
    print(runtime.explain(args.node_id))
    return 0


def cmd_stats(args) -> int:
    runtime = _open(args.store, args.budget)
    print(runtime.report())
    return 0


def cmd_demo(args) -> int:
    from .demo import run_demo

    run_demo(budget=args.budget)
    return 0


def cmd_bench(args) -> int:
    from .bench import run_benchmark, run_mutation_probe, run_scaling

    if args.scaling:
        run_scaling(budget=args.budget)
        return 0
    if args.mutate:
        run_mutation_probe(turns=args.turns, budget=args.budget)
        return 0
    run_benchmark(turns=args.turns, budget=args.budget, window=args.window)
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="dcr", description="Dynamic Context Runtime")
    parser.add_argument("--store", default=DEFAULT_STORE, help="path to the persisted memory")
    parser.add_argument("--budget", type=int, default=1200, help="B_attention, in tokens")
    sub = parser.add_subparsers(dest="command", required=True)

    p = sub.add_parser("ingest", help="index files into the memory runtime")
    p.add_argument("paths", nargs="+")
    p.set_defaults(func=cmd_ingest)

    p = sub.add_parser("ask", help="plan a working set and answer a question")
    p.add_argument("query")
    p.add_argument("--local", action="store_true", help="force the offline reasoner")
    p.add_argument("--show-context", action="store_true")
    p.set_defaults(func=cmd_ask)

    p = sub.add_parser("plan", help="show the active context without calling a model")
    p.add_argument("query")
    p.add_argument("--explain", action="store_true", help="show per-node utility arithmetic")
    p.set_defaults(func=cmd_plan)

    p = sub.add_parser("explain", help="audit path from a node down to evidence")
    p.add_argument("node_id")
    p.set_defaults(func=cmd_explain)

    p = sub.add_parser("stats", help="telemetry report")
    p.set_defaults(func=cmd_stats)

    p = sub.add_parser("demo", help="worked example: correction, quote, justify, recompute")
    p.set_defaults(func=cmd_demo)

    p = sub.add_parser("bench", help="DCR vs full-context vs sliding window")
    p.add_argument("--turns", type=int, default=300)
    p.add_argument("--window", type=int, default=8000)
    p.add_argument("--scaling", action="store_true",
                   help="does k stay flat while history grows?")
    p.add_argument("--mutate", action="store_true",
                   help="is a correction served once the original has dependents?")
    p.set_defaults(func=cmd_bench)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
