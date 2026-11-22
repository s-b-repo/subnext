//! The fit gate: when the whole store fits in the window, the runtime must
//! get out of the way — and when it does not, the gate must not fire.

use dcr::runtime::Dcr;
use dcr::tokens::{Estimator, estimate_tokens};
use dcr::{Fit, Reasoner};

/// A reasoner that reads raw text — what the bypass is actually for. The
/// offline line-matcher cannot, which is why the gate is opt-in.
struct RawReader;
impl Reasoner for RawReader {
    fn complete(&mut self, prompt: &str, _system: &str) -> String {
        if prompt.contains("10.0.4.12") {
            "10.0.4.12 [raw]".to_string()
        } else {
            "not in the history".to_string()
        }
    }
}

#[test]
fn small_store_bypasses_the_planner_when_the_gate_is_on() {
    let mut rt = Dcr::new(800);
    rt.fit_gate = true;
    rt.ingest("The server ip is 10.0.4.12.", Some("ops")).unwrap();
    assert_eq!(rt.fit(), Fit::Bypass);

    let answer = rt.ask_with("what is the server ip?", None, &mut RawReader);
    assert!(answer.bypassed, "the gate did not fire");
    assert!(answer.text.contains("10.0.4.12"), "{:?}", answer.text);
    assert_eq!(rt.plans, 0, "a bypassed turn must not plan");
    assert_eq!(rt.telemetry.bypassed, 1);
    assert_eq!(rt.telemetry.report().bypass_rate, Some(1.0));
}

#[test]
fn the_gate_stays_closed_when_history_exceeds_the_window() {
    let mut rt = Dcr::new(40);
    rt.fit_gate = true;
    rt.ingest(
        "The server ip is 10.0.4.12 and the port is 8080, and this sentence \
         keeps going and going, with more operational detail than any small \
         window could hold: rack numbers, vlan tags, failover notes, ticket \
         references, and the rest of the runbook appended for good measure.",
        Some("ops"),
    )
    .unwrap();
    assert_eq!(rt.fit(), Fit::Plan);

    let answer = rt.ask_with("what is the server ip?", None, &mut RawReader);
    assert!(!answer.bypassed, "the gate fired over a too-big store");
    assert!(rt.plans > 0, "a planned turn must leave planner telemetry");
}

#[test]
fn the_gate_is_off_by_default() {
    let mut rt = Dcr::new(800);
    rt.ingest("The server ip is 10.0.4.12.", Some("ops")).unwrap();
    let answer = rt.ask_with("what is the server ip?", None, &mut RawReader);
    assert!(!answer.bypassed);
    assert!(rt.plans > 0);
    assert_eq!(rt.telemetry.bypassed, 0);
}

#[test]
fn a_store_with_skills_never_bypasses() {
    let mut rt = Dcr::new(800);
    rt.fit_gate = true;
    rt.add_skill("code-review", "reviews code diffs", "sha256:abc").unwrap();
    // The store is otherwise empty and tiny — but admission is what loads a
    // skill, and bypassing the planner would leave it dormant.
    assert_eq!(rt.fit(), Fit::Plan);
}

#[test]
fn the_offline_reasoner_receives_the_format_it_can_read() {
    let mut rt = Dcr::new(800);
    rt.fit_gate = true;
    rt.ingest("The parcel code is ZX-17.", Some("parcel"))
        .unwrap();
    let answer = rt.ask("what is the parcel code?", None);
    assert!(!answer.bypassed);
    assert!(answer.text.contains("ZX-17"));
}

#[test]
fn the_gate_counts_the_prompt_wrapper_against_the_budget() {
    // The history uses 85 tokens. The query and wrapper push it over 100.
    let mut rt = Dcr::new(100);
    rt.fit_gate = true;
    let doc = "operational detail, ticket references, runbook notes. ".repeat(8);
    rt.ingest(&doc[..340], Some("ops")).unwrap();
    assert_eq!(rt.telemetry.history_tokens, 85);

    let query = "summarise every ticket reference and runbook note above please";
    let answer = rt.ask_with(query, None, &mut RawReader);
    assert!(
        !answer.bypassed,
        "history fits but the full prompt does not — the gate must not fire"
    );
}

#[test]
fn the_gate_uses_the_rendered_cost_with_a_custom_tokenizer() {
    let mut rt = Dcr::new(80).with_estimator(Estimator::Custom(Box::new(|text| {
        if text.starts_with("# HISTORY\n") {
            81
        } else {
            estimate_tokens(text)
        }
    })));
    rt.fit_gate = true;
    rt.ingest("The server ip is 10.0.4.12.", Some("ops"))
        .unwrap();

    assert!(
        !rt.ask_with("what is the server ip?", None, &mut RawReader)
            .bypassed
    );
    let answer = rt.ask_with("what is the server ip?", Some(81), &mut RawReader);
    assert!(answer.bypassed);
    assert_eq!(answer.tokens, 81);
}

#[test]
fn the_gate_preserves_corrections_in_a_short_history() {
    let mut rt = Dcr::new(800);
    rt.fit_gate = true;
    rt.ingest("The parcel code is ZX-17.", Some("original"))
        .unwrap();
    rt.ingest("Correction: the parcel code is CHECK-2.", Some("revised"))
        .unwrap();

    assert_eq!(rt.fit(), Fit::Plan);
    let answer = rt.ask("what is the parcel code?", None);
    assert!(!answer.bypassed);
    assert!(answer.text.contains("CHECK-2"));
    assert!(!answer.context.render().contains("ZX-17"));
}

/// A bypassed turn is still a turn: it must feed the prefix-reuse metric
/// (otherwise reuse would be compared *across* bypasses) and leave no stale
/// membership for the stickiness weight to pay toward.
#[test]
fn a_bypassed_turn_keeps_cross_turn_state_honest() {
    let mut rt = Dcr::new(800);
    rt.fit_gate = true;
    rt.ingest("The server ip is 10.0.4.12.", Some("ops")).unwrap();
    rt.ask_with("what is the server ip?", None, &mut RawReader);
    assert_eq!(rt.telemetry.report().prefix_reuse_mean, None);
    rt.ask_with("what is the server ip?", None, &mut RawReader);
    // Identical bypass prompts: full reuse, recorded rather than skipped.
    assert_eq!(rt.telemetry.report().prefix_reuse_mean, Some(1.0));
    assert!(
        rt.last_assembly.is_empty(),
        "a bypassed turn must clear the planned membership it replaces"
    );
}
