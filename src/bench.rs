//! Benchmark: DCR vs full context vs a sliding window.
//!
//! The roadmap's Phase 5 asks for a benchmark that could *falsify* the
//! `O(k + r)` claim, so this measures the things that would show it failing:
//! tokens per resolved query as history grows, and whether the answers stay
//! correct while the window stays small.
//!
//! The reasoner is the same deterministic line-matcher for every system, so the
//! comparison is between *context assemblies*, not between models. Baselines
//! are given every advantage that is honest: full context sees the entire
//! transcript, the sliding window sees the most recent `window` tokens, and on
//! a scoring tie both prefer the *latest* matching line, so they get
//! corrections right whenever the correction is inside their window.

use std::collections::HashSet;
use std::time::Instant;

use crate::graph::DcrError;
use crate::llm::{LocalReasoner, Reasoner};
use crate::runtime::Dcr;
use crate::text::content_tokens;
use crate::tokens::estimate_tokens;

pub struct Probe {
    pub query: &'static str,
    pub expected: &'static str,
    pub label: &'static str,
}

pub struct Corpus {
    pub docs: Vec<(String, String)>,
    pub probes: Vec<Probe>,
}

impl Corpus {
    pub fn text(&self) -> String {
        self.docs
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

const NOISE: &[&str] = &[
    "Standup notes for slot {n}: dashboards were refreshed after the overnight batch, {n} alerts \
     acknowledged and closed without action, and the on-call rotation stays as published. Nobody \
     raised a blocker. The mobile team asked about the shared staging cluster again; the answer is \
     unchanged, they should book a slot in the calendar rather than grabbing it. No follow-up \
     needed from this thread.",
    "Triage sweep {n}: the queue is at {n} items, mostly duplicate reports of the same flaky \
     integration test. Two were closed as cannot-reproduce, one was merged into an existing \
     thread, and the rest are waiting on logs from the reporter. Nothing here touches the checkout \
     path. The label taxonomy is still a mess and someone should clean it up when there is time, \
     which there is not.",
    "Chat log {n}: the coffee machine is broken again, facilities ticket {n} is filed, and the \
     descaling kit is apparently on back-order until next month. Somebody suggested a kettle. \
     Somebody else suggested going outside. The thread then drifted into a long argument about \
     tabs versus spaces which nobody won and which is reproduced here in full for completeness of \
     the audit log.",
    "Metrics digest {n}: p99 latency held steady at {n}ms across all regions, error budget \
     consumption is flat, and the saturation alarms did not fire. Cache hit rate drifted down by a \
     fraction of a percent, which is within noise. The synthetic probes from the eu-west region \
     were briefly red during a network maintenance window that was announced two weeks ago and is \
     not an incident.",
    "Retro prep {n}: the agenda doc has {n} comments, most of them about process rather than the \
     outage itself. Recurring themes are alert fatigue, unclear ownership of the shared queues, \
     and the fact that runbooks are out of date. Someone will volunteer to own runbook cleanup and \
     then not do it, as is traditional. Meeting is Thursday, same room, coffee situation \
     permitting.",
    "Deploy bot {n}: build {n} passed CI in eleven minutes, artifacts uploaded, images signed, and \
     the staging smoke suite is green. No action needed. The flaky browser test failed once and \
     passed on retry, as it does roughly one run in nine. The changelog is auto-generated and \
     contains nothing but dependency bumps and a typo fix in a comment nobody will ever read.",
    "Capacity note {n}: disk usage on runner-{n} sits at 61 percent, below the threshold, and the \
     cleanup cron is doing its job. Memory headroom is comfortable. The build cache could be \
     pruned more aggressively but the savings do not justify the churn. Nothing about this affects \
     the checkout service or any of its dependencies, and no action is required from anyone \
     reading this.",
    "Handover {n}: on-call handover completed, {n} open threads carried over, none of them \
     customer-facing. The incoming engineer has the runbook links and the escalation ladder. One \
     long-running investigation into intermittent DNS timeouts continues with no conclusion; it \
     has been open for weeks and remains a mystery that everyone has quietly agreed to live with \
     for now.",
];

/// A long ops transcript: a few facts, three corrections, a lot of noise.
pub fn build_corpus(turns: usize) -> Corpus {
    let mut docs: Vec<(String, String)> = Vec::new();
    let mut add = |id: &str, text: &str| docs.push((id.to_string(), text.to_string()));

    add(
        "t000",
        "Goal: restore checkout by 09:00 UTC.\n\n\
         Constraint: never restart the payment service during business hours.",
    );
    add(
        "t001",
        "The service is alpha-checkout and the owner is team-payments.",
    );
    add(
        "t002",
        "The error was \"connection refused\" when talking to the inventory host.",
    );
    add("t003", "The server ip is 10.0.4.12 and the port is 8080.");
    add("t004", "The deploy window is 23:00-01:00 UTC.");
    add(
        "t005",
        "The blocker is firewall rule 37, which drops traffic to the checkout subnet.",
    );
    add(
        "t006",
        "Decision: roll back to build 4471 because the blocker is firewall rule 37.",
    );
    add(
        "t007",
        "The engineer count is 3 and the incident hours are 4.",
    );
    add("t008", "The hourly rate is 180 USD.");
    add(
        "t009",
        "Deploy log for build 4471: the rollout started at 04:03 and moved through canary, then 10 \
         percent, then 50 percent of the fleet without incident. At 04:11 the checkout pods began \
         failing readiness probes against the inventory host, the load balancer drained them, and \
         the rollout controller entered a retry loop. The retry budget was exhausted after 7 \
         attempts and the final failure code was ERR_CONN_REFUSED_37, at which point the \
         controller stopped and paged on-call.",
    );

    let fixed = docs.len();
    let corrections: [(usize, &str); 3] = [
        (
            (turns as f64 * 0.35) as usize,
            "Correction: actually the server ip is 10.0.9.7, we misread the dashboard.",
        ),
        (
            (turns as f64 * 0.80) as usize,
            "Update: the deploy window is 02:00-04:00 UTC after the change-freeze review.",
        ),
        (
            (turns as f64 * 0.90) as usize,
            "Correction: the hourly rate is 210 USD, finance updated the figure.",
        ),
    ];
    for i in fixed..turns.max(fixed + 1) {
        let id = format!("t{i:03}");
        match corrections.iter().find(|(at, _)| *at == i) {
            Some((_, text)) => docs.push((id, text.to_string())),
            None => docs.push((id, NOISE[i % NOISE.len()].replace("{n}", &i.to_string()))),
        }
    }

    let probes = vec![
        Probe {
            query: "what is the server ip?",
            expected: "10.0.9.7",
            label: "corrected fact (mid-history)",
        },
        Probe {
            query: "what is the deploy window?",
            expected: "02:00-04:00",
            label: "corrected fact (late)",
        },
        Probe {
            query: "who is the owner of the checkout service?",
            expected: "team-payments",
            label: "old fact, never repeated",
        },
        Probe {
            query: "quote the exact error message",
            expected: "connection refused",
            label: "exact quote",
        },
        Probe {
            query: "why did we roll back?",
            expected: "firewall rule 37",
            label: "justification / multi-hop",
        },
        Probe {
            query: "how many retry attempts were made before the failure?",
            expected: "7 attempts",
            label: "detail buried in a long span",
        },
        Probe {
            query: "what is the hourly rate?",
            expected: "210",
            label: "corrected fact (very late)",
        },
    ];
    Corpus { docs, probes }
}

/// Line-matcher over raw text — the same scoring the DCR reasoner uses.
///
/// On a tie it prefers the *latest* line, which is the strongest honest
/// baseline: it gets a correction right whenever the correction is in view.
#[derive(Debug, Default)]
pub struct BaselineReasoner;

impl Reasoner for BaselineReasoner {
    fn complete(&mut self, prompt: &str, _system: &str) -> String {
        let (query, body) = match prompt.rfind("\nQUESTION:") {
            Some(at) => (prompt[at + "\nQUESTION:".len()..].trim(), &prompt[..at]),
            None => ("", prompt),
        };
        let q: HashSet<String> = content_tokens(query).into_iter().collect();
        if q.is_empty() {
            return String::new();
        }
        let mut best = "";
        let mut best_score = 0.0f32;
        for line in body.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let expanded = format!("{} {line}", line.replace('.', " "));
            let tokens: HashSet<String> = content_tokens(&expanded).into_iter().collect();
            let score = q.intersection(&tokens).count() as f32 / q.len() as f32;
            if score >= best_score {
                best_score = score;
                best = line;
            }
        }
        if best_score >= 0.2 {
            best.to_string()
        } else {
            "I don't have that in the context.".to_string()
        }
    }
}

/// Keep the last `budget` tokens — the sliding-window baseline.
fn truncate_tokens(text: &str, budget: usize) -> String {
    if estimate_tokens(text) <= budget {
        return text.to_string();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for line in text.lines().rev() {
        let cost = estimate_tokens(line);
        if used + cost > budget {
            break;
        }
        kept.push(line);
        used += cost;
    }
    kept.reverse();
    kept.join("\n")
}

struct Row {
    label: &'static str,
    query: &'static str,
    correct_full: bool,
    correct_window: bool,
    correct_dcr: bool,
    dcr_tokens: usize,
    dcr_answer: String,
}

pub fn run_benchmark(turns: usize, budget: usize, window: usize) -> Result<(), DcrError> {
    let corpus = build_corpus(turns);
    let full_text = corpus.text();
    let full_tokens = estimate_tokens(&full_text);
    let windowed = truncate_tokens(&full_text, window);
    let windowed_tokens = estimate_tokens(&windowed);

    let mut runtime = Dcr::new(budget);
    for (doc_id, text) in &corpus.docs {
        runtime.ingest(text, Some(doc_id))?;
    }

    let mut baseline = BaselineReasoner;
    let mut reasoner = LocalReasoner::new();
    let mut rows: Vec<Row> = Vec::new();

    for probe in &corpus.probes {
        let full = baseline.complete(&format!("{full_text}\n\nQUESTION: {}", probe.query), "");
        let win = baseline.complete(&format!("{windowed}\n\nQUESTION: {}", probe.query), "");
        let answer = runtime.ask_with(probe.query, None, &mut reasoner);
        // Auditability is part of the deliverable, not a bonus: every DCR
        // answer must be walkable back to raw spans.
        for node_id in &answer.cited {
            let _ = runtime.explain(node_id);
        }
        let hit = |text: &str| text.to_lowercase().contains(&probe.expected.to_lowercase());
        rows.push(Row {
            label: probe.label,
            query: probe.query,
            correct_full: hit(&full),
            correct_window: hit(&win),
            correct_dcr: hit(&answer.text),
            dcr_tokens: answer.tokens,
            dcr_answer: answer.text,
        });
    }

    let line = "-".repeat(88);
    println!("{line}");
    println!(
        "CONTEXT ROT BENCHMARK — {turns} turns, {} documents, {full_tokens} tokens of history",
        corpus.docs.len()
    );
    println!("B_attention = {budget} tokens \u{b7} sliding window = {window} tokens");
    println!("{line}");
    let header = format!(
        "{:<38} {:>6} {:>7} {:>6}   {:>10}",
        "probe", "full", "window", "DCR", "DCR tokens"
    );
    println!("{header}");
    println!("{}", "-".repeat(header.len()));
    let mark = |ok: bool| if ok { "  ok  " } else { " MISS " };
    for row in &rows {
        println!(
            "{:<38}{} {} {} {:>10}",
            row.label,
            mark(row.correct_full),
            mark(row.correct_window),
            mark(row.correct_dcr),
            row.dcr_tokens
        );
    }
    println!("{}", "-".repeat(header.len()));
    let total = |f: fn(&Row) -> bool| rows.iter().filter(|r| f(r)).count();
    println!(
        "{:<38}{:>6} {:>7} {:>6}   of {}",
        "correct",
        total(|r| r.correct_full),
        total(|r| r.correct_window),
        total(|r| r.correct_dcr),
        rows.len()
    );
    let mean_dcr = rows.iter().map(|r| r.dcr_tokens).sum::<usize>() as f64 / rows.len() as f64;
    println!(
        "{:<38}{:>6.1} {:>7.1} {:>6.1}",
        "mean tokens per query", full_tokens as f64, windowed_tokens as f64, mean_dcr
    );
    println!(
        "{:<38}{:>6} {:>7} {:>6}",
        "attention vs full history",
        "1x",
        format!("{:.1}x", full_tokens as f64 / windowed_tokens.max(1) as f64),
        format!("{:.0}x", full_tokens as f64 / mean_dcr.max(1.0))
    );
    println!("{line}");
    println!("per-probe answers (DCR):");
    for row in &rows {
        println!(
            "  [{}] {}",
            if row.correct_dcr { "ok  " } else { "MISS" },
            row.query
        );
        let answer: String = row.dcr_answer.chars().take(110).collect();
        println!("          -> {answer}");
    }
    println!("{line}");
    println!("DCR telemetry");
    print!("{}", runtime.telemetry.report());
    println!("{line}");
    println!("one-time indexing cost (amortised over all queries, not per query):");
    let stats = runtime.graph.stats();
    println!(
        "  state nodes: {}   edges: {}   L0 spans: {}",
        stats.nodes,
        stats.edges,
        runtime.raw.len()
    );
    println!(
        "  ladder builds: {}   superseded: {}   stale: {}",
        runtime.ladder.builds(),
        stats.superseded,
        stats.stale
    );
    println!("  note: storage stays O(N); the claim is bounded *attention*, not bounded storage.");
    println!("{line}");
    println!("how to read this");
    println!("  * The reasoner is a deterministic line-matcher for all three systems, so this");
    println!("    compares context assemblies, not models. Full-context accuracy here is a");
    println!("    FLOOR, not a ceiling: a real model reading the whole transcript would do");
    println!("    better on the retrieval probes. Do not read these columns as 'DCR is more");
    println!("    accurate than a long-context model'.");
    println!("  * The load-bearing results are (a) the token counts, which no model quality");
    println!("    changes, and (b) the sliding window's misses, which are structural — a fact");
    println!("    outside the window is unrecoverable at any model quality.");
    println!("  * Escalations are counted and charged: a probe that needed L0 costs more, and");
    println!("    that cost is in the DCR token column, not hidden.");
    Ok(())
}

/// Which mechanism is load-bearing for which question?
///
/// Each row disables exactly one thing and re-runs the whole probe set, so a
/// drop in the `correct` column names the mechanism that was carrying it. A
/// design whose parts can all be removed without changing the result does not
/// need those parts.
pub fn run_ablation(turns: usize, budget: usize) -> Result<(), DcrError> {
    struct Ablation {
        name: &'static str,
        apply: fn(&mut Dcr),
    }
    let ablations = [
        Ablation {
            name: "full runtime",
            apply: |_| {},
        },
        Ablation {
            name: "no supersession",
            apply: |rt| rt.indexer.supersede_on_conflict = false,
        },
        Ablation {
            name: "no reference linking",
            apply: |rt| rt.indexer.reference_linking = false,
        },
        Ablation {
            name: "no escalation",
            apply: |rt| rt.max_escalations = 0,
        },
        Ablation {
            name: "no seed floor",
            apply: |rt| rt.planner.seed_min_ratio = 0.0,
        },
        Ablation {
            name: "no graph expansion",
            apply: |rt| rt.planner.max_depth = 0,
        },
        Ablation {
            name: "L2 only (no ladder)",
            apply: |rt| {
                rt.max_escalations = 0;
                rt.ladder.flatten_to_l2 = true;
            },
        },
    ];

    let corpus = build_corpus(turns);
    println!(
        "ABLATION - {turns} turns, {} tokens of history, B_attention = {budget}",
        estimate_tokens(&corpus.text())
    );
    let header = format!(
        "{:<24} {:>8} {:>9} {:>7} {:>7}   {}",
        "variant", "correct", "mean k", "esc.", "nodes", "probes that fail"
    );
    println!("{}", "-".repeat(header.len()));
    println!("{header}");
    println!("{}", "-".repeat(header.len()));

    for ablation in ablations {
        let mut runtime = Dcr::new(budget);
        (ablation.apply)(&mut runtime);
        for (doc_id, text) in &corpus.docs {
            runtime.ingest(text, Some(doc_id))?;
        }
        let mut reasoner = LocalReasoner::new();
        let mut correct = 0usize;
        let mut failed: Vec<&str> = Vec::new();
        for probe in &corpus.probes {
            let answer = runtime.ask_with(probe.query, None, &mut reasoner);
            if answer
                .text
                .to_lowercase()
                .contains(&probe.expected.to_lowercase())
            {
                correct += 1;
            } else {
                failed.push(probe.label);
            }
        }
        let report = runtime.telemetry.report();
        println!(
            "{:<24} {:>8} {:>9.1} {:>7.2} {:>7}   {}",
            ablation.name,
            format!("{correct}/{}", corpus.probes.len()),
            report.tokens_per_query_mean,
            report.escalation_rate.unwrap_or(0.0),
            runtime.graph.len(),
            if failed.is_empty() {
                "-".to_string()
            } else {
                failed.join("; ")
            }
        );
    }
    println!("{}", "-".repeat(header.len()));
    Ok(())
}

/// How does the answer change as `B_attention` changes?
///
/// The budget is the one knob an operator actually turns, so the curve of
/// correctness against it is the practical question: how small can the window
/// be before answers start to go missing?
pub fn run_sweep(turns: usize, budgets: &[usize]) -> Result<(), DcrError> {
    let corpus = build_corpus(turns);
    println!(
        "BUDGET SWEEP - {turns} turns, {} tokens of history",
        estimate_tokens(&corpus.text())
    );
    println!(
        "{:>11} {:>9} {:>8} {:>8} {:>10} {:>10}",
        "B_attention", "correct", "mean k", "max k", "escal.", "demotions"
    );
    println!("{}", "-".repeat(60));
    for &budget in budgets {
        let mut runtime = Dcr::new(budget);
        for (doc_id, text) in &corpus.docs {
            runtime.ingest(text, Some(doc_id))?;
        }
        let mut reasoner = LocalReasoner::new();
        let mut correct = 0usize;
        for probe in &corpus.probes {
            let answer = runtime.ask_with(probe.query, None, &mut reasoner);
            if answer
                .text
                .to_lowercase()
                .contains(&probe.expected.to_lowercase())
            {
                correct += 1;
            }
        }
        let report = runtime.telemetry.report();
        println!(
            "{:>11} {:>9} {:>8.1} {:>8} {:>10.2} {:>10}",
            budget,
            format!("{correct}/{}", corpus.probes.len()),
            report.tokens_per_query_mean,
            report.tokens_per_query_max,
            report.escalation_rate.unwrap_or(0.0),
            report.demotions
        );
    }
    println!("{}", "-".repeat(60));
    Ok(())
}

/// Positive control for `stale_fact_read_rate`.
///
/// A zero is only evidence if the run could have produced nonzero. This builds
/// a run that *can*: a derived value is computed, its input is then corrected
/// so the derivation goes stale, and a probe asks for exactly that stale value.
/// The expected outcomes are written down here, before execution, per the
/// critique that motivated this control:
///
///   guard ON  (production) : stale node skipped -> stale_fact_read_rate = 0.0
///   guard OFF (bypassed)   : stale node admitted -> stale_fact_read_rate = 1.0
///
/// If the OFF run does not reach 1.0, the metric is dead and the ON zero means
/// nothing. If ON is not 0.0, the guard leaks. Only ON=0 with OFF=1 licenses
/// reading the production zero as a fired guard.
pub fn run_poison(budget: usize) -> Result<(), DcrError> {
    fn scenario(budget: usize, admit_stale: bool) -> Result<f64, DcrError> {
        let mut rt = Dcr::new(budget);
        rt.planner.admit_stale = admit_stale;
        rt.register("incident_cost", |inputs| {
            let get = |n: &str| {
                inputs
                    .iter()
                    .find(|(k, _)| k == n)
                    .map(|(_, v)| *v)
                    .unwrap_or(0.0)
            };
            get("rate") * get("hours")
        });
        rt.ingest("The hourly rate is 180 USD.", Some("t1"))?;
        rt.ingest("The incident lasted 4 hours.", Some("t2"))?;
        let rate = *rt.graph.by_key("hourly.rate", true).last().unwrap();
        let deps = vec![rt.graph.node(rate).id.clone()];
        rt.compute(
            "incident_cost",
            vec![("rate".into(), 180.0), ("hours".into(), 4.0)],
            deps,
            Some("incident.cost"),
        )?;
        // Poison: correct the input. The derivation now depends on a superseded
        // fact, so it is marked stale.
        rt.ingest("Correction: the hourly rate is 210 USD.", Some("t3"))?;
        let cost = rt.graph.by_key("incident.cost", true)[0];
        assert_eq!(
            rt.graph.node(cost).status.as_str(),
            "stale",
            "the derivation must be stale for this control to mean anything"
        );
        let mut reasoner = LocalReasoner::new();
        rt.ask_with("what is the incident cost?", None, &mut reasoner);
        Ok(rt.telemetry.report().stale_fact_read_rate.unwrap_or(0.0))
    }

    let line = "-".repeat(68);
    println!("{line}");
    println!("STALE-FACT POSITIVE CONTROL");
    println!("{line}");
    println!("scenario: a derived value goes stale after its input is corrected,");
    println!("          then a probe asks for exactly that value.\n");
    println!(
        "  {:<26} {:>10} {:>10} {:>8}",
        "run", "expected", "measured", "verdict"
    );
    println!("  {}", "-".repeat(58));
    let on = scenario(budget, false)?;
    let off = scenario(budget, true)?;
    let row = |name: &str, expected: f64, measured: f64| {
        let ok = (expected - measured).abs() < 1e-9;
        println!(
            "  {:<26} {:>10.1} {:>10.1} {:>8}",
            name,
            expected,
            measured,
            if ok { "PASS" } else { "FAIL" }
        );
        ok
    };
    let a = row("guard ON (production)", 0.0, on);
    let b = row("guard OFF (bypassed)", 1.0, off);
    println!("{line}");
    if a && b {
        println!("Both controls pass: the metric CAN return nonzero, and the guard drove");
        println!("it to zero. The production 0.0 is a fired guard, not an unexercised one.");
    } else {
        println!("A control failed: do not read the production zero as evidence.");
    }
    println!("{line}");
    Ok(())
}

/// Read coverage across a growing history — the offline, unconditioned metric.
///
/// Answers the question the probe-based table cannot: as `N` grows, does the
/// set of spans ever assembled grow with it, or stay flat while the unread
/// region grows linearly? Coverage is measured after replaying the fixed probe
/// set at each size.
pub fn run_coverage(sizes: &[usize], budget: usize) -> Result<(), DcrError> {
    println!(
        "{:>7} {:>9} {:>9} {:>10} {:>12}",
        "turns", "spans N", "assembled", "coverage", "never seen"
    );
    println!("{}", "-".repeat(52));
    let mut first: Option<(usize, usize)> = None;
    let mut last = (0usize, 0usize, 0usize);
    for &turns in sizes {
        let corpus = build_corpus(turns);
        let mut rt = Dcr::new(budget);
        for (doc_id, text) in &corpus.docs {
            rt.ingest(text, Some(doc_id))?;
        }
        let mut reasoner = LocalReasoner::new();
        for probe in &corpus.probes {
            rt.ask_with(probe.query, None, &mut reasoner);
        }
        let cov = rt.coverage();
        println!(
            "{turns:>7} {:>9} {:>9} {:>9.1}% {:>12}",
            cov.total_spans,
            cov.assembled_spans,
            cov.fraction * 100.0,
            cov.total_spans - cov.assembled_spans
        );
        if first.is_none() {
            first = Some((cov.total_spans, cov.assembled_spans));
        }
        last = (
            cov.total_spans,
            cov.assembled_spans,
            cov.total_spans - cov.assembled_spans,
        );
    }
    println!("{}", "-".repeat(52));
    if let Some((n0, a0)) = first {
        let n_growth = last.0 as f64 / n0.max(1) as f64;
        let a_growth = last.1 as f64 / a0.max(1) as f64;
        println!("history (N) grew {n_growth:.0}x; spans ever shown at L0 grew {a_growth:.1}x.",);
        println!("Coverage counts L0 only: the sole level that renders a span's actual bytes.");
        println!("A fixed probe set surfaces a roughly constant handful of spans, so the count");
        println!(
            "stays flat while the unread region ({} spans here) grows with the history and",
            last.2
        );
        println!("the covered fraction collapses toward zero. This is the dual cost of bounded");
        println!("attention that the probe-based table cannot show: the runtime answers from");
        println!("compact facts without ever surfacing most of the history's actual content, so");
        println!("a span whose specifics silently governed an answer can never appear in a probe.");
    }
    Ok(())
}

/// Does `k` stay flat while `N` grows? That is the whole claim.
pub fn run_scaling(sizes: &[usize], budget: usize) -> Result<(), DcrError> {
    println!(
        "{:>7} {:>9} {:>7} {:>8} {:>7} {:>8} {:>8} {:>8}",
        "turns", "history", "nodes", "mean k", "max k", "correct", "ingest", "query"
    );
    println!("{}", "-".repeat(68));
    let mut first: Option<(usize, f64)> = None;
    let mut last: (usize, f64) = (0, 0.0);
    for &turns in sizes {
        let corpus = build_corpus(turns);
        let started = Instant::now();
        let mut runtime = Dcr::new(budget);
        for (doc_id, text) in &corpus.docs {
            runtime.ingest(text, Some(doc_id))?;
        }
        let ingest_s = started.elapsed().as_secs_f64();

        let started = Instant::now();
        // Fresh per size: node ids are content-derived, so a shared reasoner
        // would carry escalation memory across runs.
        let mut reasoner = LocalReasoner::new();
        let mut correct = 0usize;
        for probe in &corpus.probes {
            let answer = runtime.ask_with(probe.query, None, &mut reasoner);
            if answer
                .text
                .to_lowercase()
                .contains(&probe.expected.to_lowercase())
            {
                correct += 1;
            }
        }
        let query_ms = started.elapsed().as_secs_f64() * 1000.0 / corpus.probes.len() as f64;
        let report = runtime.telemetry.report();
        let history = estimate_tokens(&corpus.text());
        println!(
            "{turns:>7} {history:>9} {:>7} {:>8.1} {:>7} {:>8} {:>7.2}s {:>6.1}ms",
            runtime.graph.len(),
            report.tokens_per_query_mean,
            report.tokens_per_query_max,
            format!("{correct}/{}", corpus.probes.len()),
            ingest_s,
            query_ms
        );
        if first.is_none() {
            first = Some((history, report.tokens_per_query_mean));
        }
        last = (history, report.tokens_per_query_mean);
    }
    println!("{}", "-".repeat(68));
    if let Some((first_history, first_k)) = first {
        println!(
            "history grew {:.0}x; active context grew {:.2}x  <- the O(k + r) claim",
            last.0 as f64 / first_history.max(1) as f64,
            last.1 / first_k.max(1.0)
        );
    }
    println!("query latency is NOT flat: vector search here is a linear scan over state");
    println!("nodes. The cost model needs sub-linear retrieval (ANN index) to hold at scale.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Mutation-and-correction probe
// ---------------------------------------------------------------------------

/// One mutation case: a fact planted early, referenced repeatedly so it
/// accumulates dependents, then superseded late.
///
/// The point of the repeated references is that a correction arriving at 85% of
/// the way through has to overcome whatever weight the original accumulated —
/// graph proximity, read-through, and the dependents built on top of it. A
/// correction to an isolated leaf is the easy case and proves little.
pub struct Mutation {
    pub label: &'static str,
    pub query: &'static str,
    /// Ground truth: the value that must no longer be served.
    pub stale: &'static str,
    /// Ground truth: the value that must be served instead.
    pub live: &'static str,
    establish: &'static str,
    references: &'static [&'static str],
    correction: &'static str,
}

pub const MUTATIONS: &[Mutation] = &[
    Mutation {
        label: "datastore (4 dependents)",
        query: "what is the primary datastore?",
        stale: "postgres-11",
        live: "postgres-15",
        establish: "The primary datastore is postgres-11 on host db-alpha.",
        references: &[
            "Nightly backup job targets the primary datastore postgres-11 on db-alpha; retention is 14 days.",
            "The read replica lags the primary datastore postgres-11 by under 200ms during business hours.",
            "Schema migration 0042 was applied against postgres-11 and verified on the replica.",
            "Capacity note: postgres-11 on db-alpha is at 61 percent disk with no growth alarm set.",
        ],
        correction: "Correction: the primary datastore was migrated and is now postgres-15 on host db-omega.",
    },
    Mutation {
        label: "autoscaler threshold (3 dependents)",
        query: "what is the autoscaler threshold?",
        stale: "65 percent",
        live: "80 percent",
        establish: "The autoscaler threshold is 65 percent CPU.",
        references: &[
            "Load test 19 held the fleet just under the autoscaler threshold of 65 percent CPU for forty minutes.",
            "The scale-out event at 03:12 fired because sustained CPU crossed 65 percent.",
            "Cost review flagged that a 65 percent trigger keeps roughly two extra nodes warm overnight.",
        ],
        correction: "Update: the autoscaler threshold is 80 percent CPU after the cost review.",
    },
    Mutation {
        label: "failover region (3 dependents)",
        query: "what is the failover region?",
        stale: "eu-west-2",
        live: "eu-central-1",
        establish: "The failover region is eu-west-2.",
        references: &[
            "The disaster recovery runbook fails traffic over to eu-west-2 and expects a 12 minute RTO.",
            "Cross-region replication to eu-west-2 was re-enabled after the maintenance window.",
            "The last failover drill exercised eu-west-2 and completed inside the RTO budget.",
        ],
        correction: "Correction: the failover region is eu-central-1, eu-west-2 is being decommissioned.",
    },
    Mutation {
        label: "escalation extension (2 dependents)",
        query: "what is the escalation extension?",
        stale: "4412",
        live: "4419",
        establish: "The escalation extension is 4412 for the platform on-call rota.",
        references: &[
            "Page at 02:40 was routed to extension 4412 and acknowledged in ninety seconds.",
            "The incident bridge dials extension 4412 before opening a severity review.",
        ],
        correction: "Update: the escalation extension is 4419, the rota moved to a new bridge.",
    },
];

/// Interleave the mutation cases into a long transcript: each fact is
/// established early, referenced across the first 60%, and corrected at 85%.
pub fn build_mutation_corpus(turns: usize) -> Corpus {
    let mut docs: Vec<(String, String)> = Vec::new();
    for (m_idx, m) in MUTATIONS.iter().enumerate() {
        docs.push((format!("m{m_idx:02}e"), m.establish.to_string()));
    }

    let established = docs.len();
    let correct_at = (turns as f64 * 0.85) as usize;
    // References are spread across the first 60% so the originals are still
    // accumulating dependents well after they were planted.
    let ref_span_end = (turns as f64 * 0.60) as usize;
    let mut refs: Vec<(usize, &str)> = Vec::new();
    let total_refs: usize = MUTATIONS.iter().map(|m| m.references.len()).sum();
    let mut slot = 0usize;
    for m in MUTATIONS {
        for r in m.references {
            let at = established
                + ((ref_span_end.saturating_sub(established)) * slot) / total_refs.max(1);
            refs.push((at.max(established), r));
            slot += 1;
        }
    }

    for i in established..turns.max(established + 1) {
        let id = format!("t{i:03}");
        // corrections land in consecutive slots starting at 85%
        let corr = MUTATIONS
            .iter()
            .enumerate()
            .find(|(k, _)| correct_at + k == i)
            .map(|(_, m)| m.correction);
        if let Some(text) = corr {
            docs.push((id, text.to_string()));
            continue;
        }
        let planted: Vec<&str> = refs
            .iter()
            .filter(|(at, _)| *at == i)
            .map(|(_, r)| *r)
            .collect();
        if !planted.is_empty() {
            docs.push((id, planted.join("\n\n")));
            continue;
        }
        docs.push((id, NOISE[i % NOISE.len()].replace("{n}", &i.to_string())));
    }

    Corpus {
        docs,
        probes: Vec::new(),
    }
}

/// Measure what the main benchmark cannot: whether a correction is actually
/// served once the original has dependents, and whether the runtime can show
/// the supersession edge that justifies it.
///
/// Both measurements are made against ground truth held by the corpus, not
/// against the runtime's own `Status::Stale` marking. That distinction is the
/// whole point. `stale_fact_read_rate` counts entries whose node the runtime
/// has *marked* stale, so it cannot fire in the configuration where marking is
/// switched off — a control that cannot fire and a control that passes are
/// indistinguishable. Checking the answer text for a value the corpus knows was
/// superseded fires in every configuration, so the `no supersession` row below
/// is a live negative control rather than a decorative one.
pub fn run_mutation_probe(turns: usize, budget: usize) -> Result<(), DcrError> {
    struct Variant {
        name: &'static str,
        apply: fn(&mut Dcr),
    }
    let variants = [
        Variant {
            name: "full runtime",
            apply: |_| {},
        },
        Variant {
            name: "no supersession",
            apply: |rt| rt.indexer.supersede_on_conflict = false,
        },
        Variant {
            name: "no reference linking",
            apply: |rt| rt.indexer.reference_linking = false,
        },
        Variant {
            name: "no graph expansion",
            apply: |rt| rt.planner.max_depth = 0,
        },
    ];

    let corpus = build_mutation_corpus(turns);
    let n = MUTATIONS.len();
    println!(
        "MUTATION AND CORRECTION - {turns} turns, {} tokens of history, B_attention = {budget}",
        estimate_tokens(&corpus.text())
    );
    println!(
        "{n} facts established early, referenced {} times in total, superseded at 85% of history",
        MUTATIONS.iter().map(|m| m.references.len()).sum::<usize>()
    );
    let header = format!(
        "{:<24} {:>10} {:>13} {:>11} {:>9}   {}",
        "variant", "corrected", "stale served", "edge shown", "stale k", "notes"
    );
    println!("{}", "-".repeat(header.len()));
    println!("{header}");
    println!("{}", "-".repeat(header.len()));

    let mut control_fired = false;
    let mut served: Vec<(&str, String, usize)> = Vec::new();
    for variant in variants {
        let mut runtime = Dcr::new(budget);
        (variant.apply)(&mut runtime);
        for (doc_id, text) in &corpus.docs {
            runtime.ingest(text, Some(doc_id))?;
        }
        let mut reasoner = LocalReasoner::new();
        let (mut corrected, mut stale, mut edges) = (0usize, 0usize, 0usize);
        let mut stale_cases: Vec<&str> = Vec::new();
        let mut missed: Vec<&str> = Vec::new();
        let mut no_edge: Vec<&str> = Vec::new();

        for m in MUTATIONS {
            let answer = runtime.ask_with(m.query, None, &mut reasoner);
            let text = answer.text.to_lowercase();
            let live_v = m.live.to_lowercase();
            let stale_v = m.stale.to_lowercase();

            if text.contains(&live_v) {
                corrected += 1;
            } else if !text.contains(&stale_v) {
                // Neither value: the planner served something unrelated, which
                // is a different failure from serving the superseded value and
                // is worth separating.
                missed.push(m.label);
            }
            if text.contains(&stale_v) {
                stale += 1;
                stale_cases.push(m.label);
            }
            // Can the runtime show *why* the live value wins? A cited node must
            // supersede a node that still carries the stale value.
            let shown = answer.cited.iter().any(|id| {
                runtime.graph.get(id).is_some_and(|node| {
                    node.meta.supersedes.iter().any(|old| {
                        runtime
                            .graph
                            .get(old)
                            .is_some_and(|o| o.value.to_lowercase().contains(&stale_v))
                    })
                })
            });
            if shown {
                edges += 1;
            } else if text.contains(&live_v) {
                // Served the right value but cannot point at the edge that
                // justifies it — a provenance gap, not a retrieval one.
                no_edge.push(m.label);
            }
            if variant.name == "full runtime" {
                served.push((m.label, answer.text.clone(), answer.tokens));
            }
        }

        let report = runtime.telemetry.report();
        let marked = match report.stale_fact_read_rate {
            Some(v) => format!("{v:.2}"),
            None => "n/a".to_string(),
        };
        if variant.name == "no supersession" && stale > 0 {
            control_fired = true;
        }
        println!(
            "{:<24} {:>8}/{n} {:>11}/{n} {:>9}/{n} {:>9}   {}",
            variant.name,
            corrected,
            stale,
            edges,
            marked,
            {
                let mut notes = Vec::new();
                for c in &stale_cases {
                    notes.push(format!("STALE {c}"));
                }
                for c in &missed {
                    notes.push(format!("no answer {c}"));
                }
                for c in &no_edge {
                    notes.push(format!("no edge {c}"));
                }
                if notes.is_empty() {
                    "-".to_string()
                } else {
                    notes.join("; ")
                }
            }
        );
    }
    println!("{}", "-".repeat(header.len()));
    println!(
        "'stale k' is the runtime's own stale_fact_read_rate, shown for comparison: it stays 0\n\
         even on the row where superseded values are provably served, because disabling\n\
         supersession means nothing is ever marked. The 'stale served' column is ground truth."
    );
    println!("\nwhat the full runtime served:");
    for (label, text, tokens) in &served {
        let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let shown: String = one_line.chars().take(96).collect();
        println!("  {label:<32} {tokens:>4} tok  {shown}");
    }
    println!();
    if control_fired {
        println!("negative control FIRED: the instrument can distinguish a pass from a no-op.");
    } else {
        println!(
            "negative control DID NOT FIRE: 'no supersession' served no stale value, so this\n\
             run does not establish that the probe can detect the failure it tests for."
        );
    }
    Ok(())
}
