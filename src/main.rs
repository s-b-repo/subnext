//! Command line interface.
//!
//! ```text
//! dcr demo
//! dcr ingest notes/            index a file or a directory
//! dcr ask "what is the server ip?" --budget 600
//! dcr plan "why did we roll back?" --explain
//! dcr explain <node_id>
//! dcr stats
//! dcr bench [--turns N] [--window N] [--scaling]
//! ```
//!
//! Globs are left to the shell, which already does them better than a
//! hand-rolled matcher would.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dcr::bench::{run_ablation, run_benchmark, run_mutation_probe, run_scaling, run_sweep};
use dcr::demo::run_demo;
use dcr::graph::DcrError;
use dcr::runtime::Dcr;

const DEFAULT_STORE: &str = ".dcr.json";
const USAGE: &str = "\
dcr — Dynamic Context Runtime

usage: dcr [--store PATH] [--budget N] <command> [options]

commands:
  ingest <path>...            index files or directories into the memory runtime
  ask <query> [--show-context]  plan a working set and answer a question
  plan <query> [--explain]    show the active context without calling a model
  explain <node_id>           audit path from a node down to raw spans
  stats                       telemetry report
  demo                        worked example through all four ladder levels
  bench [--turns N] [--window N]
                              DCR vs full context vs sliding window
        --scaling             does k stay flat while history grows?
        --ablate              which mechanism carries which probe?
        --mutate              is a correction served once the original has dependents?
        --sweep               correctness and cost against B_attention

options:
  --store PATH                persisted memory (default: .dcr.json)
  --budget N                  B_attention, in tokens (default: 1200)
";

struct Args {
    store: PathBuf,
    budget: usize,
    command: String,
    positional: Vec<String>,
    flags: Vec<String>,
    turns: usize,
    window: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut store = PathBuf::from(DEFAULT_STORE);
    let mut budget = 1200usize;
    let mut turns = 300usize;
    let mut window = 8000usize;
    let mut command = String::new();
    let mut positional = Vec::new();
    let mut flags = Vec::new();

    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--store" => store = PathBuf::from(argv.next().ok_or("--store needs a path")?),
            "--budget" => {
                budget = argv
                    .next()
                    .ok_or("--budget needs a number")?
                    .parse()
                    .map_err(|_| "--budget must be a number")?
            }
            "--turns" => {
                turns = argv
                    .next()
                    .ok_or("--turns needs a number")?
                    .parse()
                    .map_err(|_| "--turns must be a number")?
            }
            "--window" => {
                window = argv
                    .next()
                    .ok_or("--window needs a number")?
                    .parse()
                    .map_err(|_| "--window must be a number")?
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            flag if flag.starts_with("--") => flags.push(flag.to_string()),
            value if command.is_empty() => command = value.to_string(),
            value => positional.push(value.to_string()),
        }
    }
    if command.is_empty() {
        return Err(USAGE.to_string());
    }
    Ok(Args {
        store,
        budget,
        command,
        positional,
        flags,
        turns,
        window,
    })
}

fn open(store: &Path, budget: usize) -> Result<Dcr, DcrError> {
    if store.exists() {
        let mut runtime = Dcr::load(store, budget)?;
        runtime.set_budget(budget);
        Ok(runtime)
    } else {
        Ok(Dcr::new(budget))
    }
}

/// A file, or every file under a directory — modification-time ordered.
///
/// Ingest order is revision order, so a directory listed by name would let
/// `a.md` supersede `z.md`.
fn expand(pattern: &str) -> Vec<PathBuf> {
    let path = Path::new(pattern);
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    if path.is_dir() {
        let mut found: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(meta) = entry.metadata() {
                    let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                    found.push((modified, path));
                }
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        return found.into_iter().map(|(_, p)| p).collect();
    }
    Vec::new()
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let to_err = |e: DcrError| e.to_string();

    match args.command.as_str() {
        "ingest" => {
            let mut runtime = open(&args.store, args.budget).map_err(to_err)?;
            let mut total = 0usize;
            for pattern in &args.positional {
                let matches = expand(pattern);
                if matches.is_empty() {
                    eprintln!("no match: {pattern}");
                }
                for path in matches {
                    let result = runtime.ingest_file(&path).map_err(to_err)?;
                    total += result.nodes.len();
                    println!("{}: {}", path.display(), result.summary());
                    for (old, new) in &result.contradictions {
                        println!("  contradiction: {old} superseded by {new}");
                    }
                }
            }
            runtime.save(&args.store).map_err(to_err)?;
            println!("\n{total} state nodes; store: {}", args.store.display());
        }
        "ask" => {
            let query = args.positional.first().ok_or("ask needs a query")?;
            let mut runtime = open(&args.store, args.budget).map_err(to_err)?;
            let answer = runtime.ask(query, None);
            println!("{}", answer.text);
            println!(
                "\n-- {}/{} tokens, {} nodes, {} escalation(s), cited {}",
                answer.tokens,
                args.budget,
                answer.context.entries.len(),
                answer.escalations,
                if answer.cited.is_empty() {
                    "nothing".to_string()
                } else {
                    answer.cited.join(", ")
                }
            );
            if args.flags.iter().any(|f| f == "--show-context") {
                println!("\n{}", answer.context.render());
            }
            runtime.save(&args.store).map_err(to_err)?;
        }
        "plan" => {
            let query = args.positional.first().ok_or("plan needs a query")?;
            let mut runtime = open(&args.store, args.budget).map_err(to_err)?;
            let context = runtime.plan(query, None);
            println!("{}", context.render());
            if args.flags.iter().any(|f| f == "--explain") {
                println!("\n{}", runtime.explain_plan(&context));
            }
        }
        "explain" => {
            let node_id = args.positional.first().ok_or("explain needs a node id")?;
            let mut runtime = open(&args.store, args.budget).map_err(to_err)?;
            println!("{}", runtime.explain(node_id).map_err(to_err)?);
        }
        "stats" => {
            let runtime = open(&args.store, args.budget).map_err(to_err)?;
            print!("{}", runtime.report());
        }
        "demo" => {
            run_demo(args.budget).map_err(to_err)?;
        }
        "bench" => {
            let has = |name: &str| args.flags.iter().any(|f| f == name);
            if has("--scaling") {
                run_scaling(&[100, 300, 1000, 3000], args.budget).map_err(to_err)?;
            } else if has("--mutate") {
                run_mutation_probe(args.turns, args.budget).map_err(to_err)?;
            } else if has("--ablate") {
                run_ablation(args.turns, args.budget).map_err(to_err)?;
            } else if has("--sweep") {
                run_sweep(args.turns, &[120, 200, 300, 500, 800, 1200, 2000, 4000])
                    .map_err(to_err)?;
            } else {
                run_benchmark(args.turns, args.budget, args.window).map_err(to_err)?;
            }
        }
        other => return Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
