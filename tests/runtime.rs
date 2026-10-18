//! End-to-end behaviour: memoisation, escalation, consistency, persistence.

use std::sync::{Arc, Mutex};

use dcr::graph::DcrError;
use dcr::llm::{LocalReasoner, Reasoner};
use dcr::nodes::{Kind, Status};
use dcr::runtime::Dcr;

fn build(budget: usize, noise: usize) -> Dcr {
    let mut rt = Dcr::new(budget);
    rt.ingest("Goal: restore checkout by 09:00 UTC.", Some("t1"))
        .unwrap();
    rt.ingest(
        "The error was \"connection refused\" when talking to the inventory host.",
        Some("t2"),
    )
    .unwrap();
    rt.ingest(
        "The server ip is 10.0.4.12 and the port is 8080.",
        Some("t3"),
    )
    .unwrap();
    rt.ingest("The hourly rate is 180 USD.", Some("t4"))
        .unwrap();
    rt.ingest(
        "The engineer count is 3 and the incident hours are 4.",
        Some("t5"),
    )
    .unwrap();
    for i in 0..noise {
        rt.ingest(
            &format!("Chatter {i}: dashboards refreshed, queue at {i} items, nothing to do."),
            Some(&format!("n{i}")),
        )
        .unwrap();
    }
    rt.ingest(
        "Correction: actually the server ip is 10.0.9.7, we misread the dashboard.",
        Some("t6"),
    )
    .unwrap();
    rt
}

fn counting_cost(calls: Arc<Mutex<u32>>) -> impl Fn(&dcr::execute::Inputs) -> f64 + Send {
    move |inputs| {
        *calls.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        let get = |name: &str| {
            inputs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| *v)
                .unwrap_or(0.0)
        };
        get("rate") * get("hours")
    }
}

// -- execution ------------------------------------------------------------

#[test]
fn memoised_derivation_runs_once() {
    let mut rt = build(600, 60);
    let calls = Arc::new(Mutex::new(0u32));
    rt.register("incident_cost", counting_cost(calls.clone()));
    let rate = *rt.graph.by_key("hourly.rate", true).last().unwrap();
    let deps = vec![rt.graph.node(rate).id.clone()];
    let inputs = vec![("rate".to_string(), 180.0), ("hours".to_string(), 4.0)];
    let mut node = 0;
    for _ in 0..3 {
        node = rt
            .compute(
                "incident_cost",
                inputs.clone(),
                deps.clone(),
                Some("incident.cost"),
            )
            .unwrap();
    }
    assert_eq!(rt.graph.node(node).value, "720");
    assert_eq!(*calls.lock().unwrap_or_else(|p| p.into_inner()), 1);
    assert_eq!(rt.execution.memo_hits, 2);
}

#[test]
fn changed_input_invalidates_and_recomputes() {
    let mut rt = build(600, 60);
    let calls = Arc::new(Mutex::new(0u32));
    rt.register("incident_cost", counting_cost(calls.clone()));
    let rate = *rt.graph.by_key("hourly.rate", true).last().unwrap();
    let deps = vec![rt.graph.node(rate).id.clone()];
    let node = rt
        .compute(
            "incident_cost",
            vec![("rate".into(), 180.0), ("hours".into(), 4.0)],
            deps.clone(),
            Some("incident.cost"),
        )
        .unwrap();
    rt.ingest("Correction: the hourly rate is 210 USD.", Some("t7"))
        .unwrap();
    assert_eq!(rt.graph.node(node).status, Status::Stale);
    let again = rt
        .compute(
            "incident_cost",
            vec![("rate".into(), 210.0), ("hours".into(), 4.0)],
            deps,
            Some("incident.cost"),
        )
        .unwrap();
    assert_eq!(rt.graph.node(again).value, "840");
    assert_eq!(
        *calls.lock().unwrap_or_else(|p| p.into_inner()),
        2,
        "a changed dependency must not hit the memo"
    );
}

// -- answering ------------------------------------------------------------

#[test]
fn answers_with_the_corrected_fact_in_a_tiny_window() {
    let mut rt = build(600, 60);
    let answer = rt.ask("what is the server ip?", None);
    assert!(answer.text.contains("10.0.9.7"));
    assert!(!answer.text.contains("10.0.4.12"));
    assert!(answer.tokens < 400);
    assert!(answer.tokens < rt.telemetry.history_tokens / 4);
}

#[test]
fn escalation_promotes_a_node_to_raw() {
    let mut rt = build(600, 60);
    rt.ingest(
        "Deploy log: the rollout moved through canary and 10 percent of the fleet without \
         incident, then readiness probes began failing against the inventory host. The retry \
         budget was exhausted after 7 attempts and the final failure code was \
         ERR_CONN_REFUSED_37, at which point the controller paged on-call.",
        Some("log"),
    )
    .unwrap();
    let answer = rt.ask(
        "how many retry attempts were made before the failure?",
        None,
    );
    assert_eq!(answer.escalations, 1);
    assert!(answer.text.contains("7 attempts"));
    assert_eq!(rt.telemetry.report().escalation_rate, Some(1.0));
}

/// Records every prompt the reasoner is sent, so a test can measure what the
/// prompt cache would actually see.
struct RecordingReasoner {
    inner: LocalReasoner,
    prompts: Rc<RefCell<Vec<String>>>,
}

impl Reasoner for RecordingReasoner {
    fn complete(&mut self, prompt: &str, system: &str) -> String {
        self.prompts.borrow_mut().push(prompt.to_string());
        self.inner.complete(prompt, system)
    }
}

fn shared_prefix_pct(a: &str, b: &str) -> f64 {
    let shared = a
        .as_bytes()
        .iter()
        .zip(b.as_bytes())
        .take_while(|(x, y)| x == y)
        .count();
    shared as f64 / a.len().max(1) as f64 * 100.0
}

#[test]
fn escalation_append_preserves_the_prompt_prefix() {
    // The before/after measurement for escalation-as-append, on one turn.
    //
    // BEFORE (full re-render): the escalated re-plan rendered the whole
    // working set again; what the prompt cache would see is
    // shared_prefix(first prompt, prompt_for(final context)) — whatever
    // stable ordering survives the membership change.
    //
    // AFTER (append): the final prompt is the first prompt plus an
    // `## ESCALATED` block, so the shared prefix is 100.0 by construction.
    let mut rt = build(600, 60);
    rt.ingest(
        "Deploy log: the rollout moved through canary and 10 percent of the fleet without \
         incident, then readiness probes began failing against the inventory host. The retry \
         budget was exhausted after 7 attempts and the final failure code was \
         ERR_CONN_REFUSED_37, at which point the controller paged on-call.",
        Some("log"),
    )
    .unwrap();
    let prompts = Rc::new(RefCell::new(Vec::new()));
    let mut reasoner = RecordingReasoner {
        inner: LocalReasoner::new(),
        prompts: Rc::clone(&prompts),
    };
    let query = "how many retry attempts were made before the failure?";
    let answer = rt.ask_with(query, None, &mut reasoner);

    assert_eq!(answer.escalations, 1, "the scenario needs a mid-turn escalation");
    let sent = prompts.borrow();
    assert!(sent.len() >= 2, "an escalation re-plans, so two prompts");
    let first = &sent[0];
    let last = sent.last().unwrap();

    // AFTER — what the mechanism actually produces.
    let after = shared_prefix_pct(first, last);
    assert!(
        answer.escalated_prompt,
        "the final prompt must be the append form"
    );
    assert!(
        last.starts_with(first.as_str()),
        "the append form makes the first prompt a byte-prefix of the final one"
    );
    assert!(last.contains("## ESCALATED"), "the delta block is present");
    assert_eq!(answer.prompt_prefix_pct, 100.0);

    // BEFORE — the same turn under the old strategy: the final plan's full
    // re-render. Membership grew (the escalated node joined at L0), so the
    // kind groups shifted and the prefix breaks at the first moved line.
    let before = shared_prefix_pct(first, &rt.prompt_for(&answer.context));
    println!(
        "escalation prompt prefix: full re-render {before:.1}% -> append {after:.1}%"
    );
    assert!(
        before < after,
        "append ({after:.1}%) must beat full re-render ({before:.1}%) on the same turn"
    );
    assert!(
        answer.text.contains("7 attempts"),
        "the answer must still be correct from the appended prompt"
    );
}


#[test]
fn reactivation_pruning_spares_the_hot_old_candidates() {
    // The eviction-items integration: the planner's age prefilter is a real
    // eviction decision, and reactivation awareness changes it. On the
    // standard corpus every aged candidate is hot (read repeatedly by later
    // probes), so plain recency drops them and the predictor spares them —
    // at equal correctness, and without the span-fallback cost the pure
    // recency path pays.
    let mut pure = Dcr::new(600);
    pure.planner.recency_cutoff = 0.5;
    let corpus = dcr::bench::build_corpus(200);
    for (doc_id, text) in &corpus.docs {
        pure.ingest(text, Some(doc_id)).unwrap();
    }
    let mut reasoner = LocalReasoner::new();
    let mut pure_correct = 0;
    for probe in &corpus.probes {
        let a = pure.ask_with(probe.query, None, &mut reasoner);
        if a.text.to_lowercase().contains(&probe.expected.to_lowercase()) {
            pure_correct += 1;
        }
    }
    assert!(
        pure.planning.seed_dropped_recency > 0,
        "pure recency must actually drop candidates at this cutoff"
    );

    let mut reactivation = Dcr::new(600);
    reactivation.planner.recency_cutoff = 0.5;
    reactivation.planner.reactivation_prune = true;
    for (doc_id, text) in &corpus.docs {
        reactivation.ingest(text, Some(doc_id)).unwrap();
    }
    let mut reasoner = LocalReasoner::new();
    let mut reactivation_correct = 0;
    for probe in &corpus.probes {
        let a = reactivation.ask_with(probe.query, None, &mut reasoner);
        if a.text.to_lowercase().contains(&probe.expected.to_lowercase()) {
            reactivation_correct += 1;
        }
    }
    assert!(
        reactivation.planning.seed_spared_reactivation > 0,
        "the predictor must spare the hot aged candidates"
    );
    assert_eq!(
        reactivation.planning.seed_dropped_recency, 0,
        "nothing on this corpus is cold enough to drop"
    );
    assert_eq!(reactivation_correct, pure_correct, "no correctness regression");
    assert_eq!(reactivation_correct, corpus.probes.len());
}

#[test]
fn citations_are_recorded_as_read_through() {
    let mut rt = build(600, 60);
    let answer = rt.ask("what is the server ip?", None);
    assert!(!answer.cited.is_empty());
    for node_id in &answer.cited {
        assert!(rt.graph.get(node_id).unwrap().reads.get() > 0);
    }
}

#[test]
fn unanswerable_query_does_not_invent() {
    let mut rt = build(600, 60);
    let answer = rt.ask("what is the airspeed velocity of an unladen swallow?", None);
    assert!(
        answer.text.contains("don't have that"),
        "got {}",
        answer.text
    );
}

#[test]
fn telemetry_reports_the_evaluation_metrics() {
    let mut rt = build(600, 60);
    rt.ask("what is the server ip?", None);
    let report = rt.telemetry.report();
    assert_eq!(report.turns, 1);
    assert_eq!(report.stale_fact_read_rate, Some(0.0));
    assert!(report.compression_ratio.unwrap() > 1.0);
}

// -- consistency ----------------------------------------------------------

#[test]
fn mid_turn_invalidation_forces_a_replan() {
    let mut rt = build(600, 60);
    let target = *rt.graph.by_key("server.ip", true).last().unwrap();
    let mut fired = false;
    let mut reasoner = LocalReasoner::new();
    // Solution B consolidates while Solution A is thinking, and invalidates
    // the very fact A was handed.
    let answer = rt.ask_with_consolidation(
        "what is the server ip?",
        None,
        &mut reasoner,
        &mut |graph| {
            if !fired {
                fired = true;
                graph.invalidate(target, true);
            }
        },
    );
    assert!(
        answer.replanned,
        "an invalidated working set must not be answered from"
    );
    assert!(
        !answer.context.nodes().contains(&target),
        "the rebuilt workspace must exclude the invalidated node"
    );
}

#[test]
fn commit_fact_requires_provenance() {
    let mut rt = build(600, 10);
    let result = rt.commit_fact(
        "something I made up",
        Some("invented"),
        &[],
        &[],
        0.9,
        Kind::Claim,
    );
    assert!(matches!(result, Err(DcrError::Provenance(_))));
}

#[test]
fn workspace_can_be_destroyed_and_rebuilt() {
    let mut rt = build(600, 60);
    let before = rt.plan("what is the server ip?", None);
    let before_ids: Vec<String> = before.node_ids().iter().map(|s| s.to_string()).collect();
    let stats = rt.rebuild_workspace("what is the server ip?", None);
    let after = rt.plan("what is the server ip?", None);
    let after_ids: Vec<String> = after.node_ids().iter().map(|s| s.to_string()).collect();
    assert!(stats.cleared_level_cache_entries > 0);
    assert_eq!(before_ids, after_ids);
    assert_eq!(before.tokens, after.tokens);
}

// -- persistence ----------------------------------------------------------

#[test]
fn round_trip_preserves_state_and_answers() {
    let mut rt = build(600, 60);
    let expected = rt.ask("what is the server ip?", None).text;
    let dir = std::env::temp_dir().join(format!("dcr-test-{}", std::process::id()));
    let path = dir.join("store.json");
    rt.save(&path).unwrap();
    let mut restored = Dcr::load(&path, 600).unwrap();
    assert_eq!(restored.graph.len(), rt.graph.len());
    assert_eq!(restored.raw.len(), rt.raw.len());
    assert_eq!(restored.ask("what is the server ip?", None).text, expected);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_local_reasoner_only_answers_from_the_window() {
    let mut reasoner = LocalReasoner::new();
    let prompt = "# ACTIVE CONTEXT\n\n## FACTS (cached state)\n\
                  [clai_aaaaaaaaaaaa L2] server.ip = 10.0.9.7 \u{b7} conf=0.90\n\n\
                  QUESTION: what is the server ip?\n";
    assert_eq!(
        reasoner.complete(prompt, ""),
        "10.0.9.7 [clai_aaaaaaaaaaaa]"
    );
}

/// The prefix-reuse metric slices the prompt at the shared byte prefix of two
/// turns. 'é' (C3 A9) and 'è' (C3 A8) share a lead byte, so when consecutive
/// queries differ only in an accented character the shared run ends *inside*
/// a codepoint — slicing there panics. The metric must floor to a boundary.
#[test]
fn prefix_reuse_does_not_panic_on_multibyte_prompts() {
    let mut rt = build(600, 0);
    rt.ask("café?", None);
    rt.ask("cafè?", None);
    assert!(
        rt.telemetry.report().prefix_reuse_mean.is_some(),
        "two turns in, the metric must exist — and the asks must not panic"
    );
}
