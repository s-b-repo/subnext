//! The mutation-and-correction probe, and the property that makes it worth
//! having: its negative control can actually fail.
//!
//! `stale_fact_read_rate` counts context entries whose node carries
//! `Status::Stale`, and `supersede_on_conflict` gates whether that status is
//! ever set — so the metric reads 0.00 both when the runtime correctly excludes
//! stale nodes and when it never marked any. These tests pin the ground-truth
//! measurement that distinguishes those two cases.

use dcr::bench::{MUTATIONS, build_mutation_corpus};
use dcr::llm::LocalReasoner;
use dcr::runtime::Dcr;

/// Ingest the mutation corpus and return, per mutation, whether the answer
/// carried the live value and whether it carried the superseded one.
fn serve(supersede: bool) -> Vec<(bool, bool)> {
    let corpus = build_mutation_corpus(300);
    let mut runtime = Dcr::new(1200);
    runtime.indexer.supersede_on_conflict = supersede;
    for (doc_id, text) in &corpus.docs {
        runtime.ingest(text, Some(doc_id)).expect("ingest");
    }
    let mut reasoner = LocalReasoner::new();
    MUTATIONS
        .iter()
        .map(|m| {
            let text = runtime
                .ask_with(m.query, None, &mut reasoner)
                .text
                .to_lowercase();
            (
                text.contains(&m.live.to_lowercase()),
                text.contains(&m.stale.to_lowercase()),
            )
        })
        .collect()
}

#[test]
fn corrections_are_served_once_the_original_has_dependents() {
    let results = serve(true);
    let stale: Vec<&str> = results
        .iter()
        .zip(MUTATIONS)
        .filter(|((_, served_stale), _)| *served_stale)
        .map(|(_, m)| m.label)
        .collect();
    assert!(
        stale.is_empty(),
        "the full runtime served superseded values for: {stale:?}"
    );
}

#[test]
fn the_negative_control_can_fail() {
    // The whole point of the probe. If disabling supersession does not produce
    // a single stale read, the probe is not measuring what it claims to and the
    // "0 stale reads" result in the full runtime is unfalsifiable.
    let without = serve(false);
    let stale_reads = without.iter().filter(|(_, s)| *s).count();
    assert!(
        stale_reads > 0,
        "disabling supersession served no superseded value; the control cannot \
         fire, so a passing run proves nothing"
    );
}

#[test]
fn supersession_strictly_improves_correction_delivery() {
    let with = serve(true).iter().filter(|(live, _)| *live).count();
    let without = serve(false).iter().filter(|(live, _)| *live).count();
    assert!(
        with > without,
        "supersession did not improve correction delivery ({with} vs {without}); \
         either the mechanism is not load-bearing on this corpus or the probe \
         is not exercising it"
    );
}

#[test]
fn ground_truth_values_are_distinguishable() {
    // A case whose stale and live values are substrings of one another would
    // make every measurement above meaningless.
    for m in MUTATIONS {
        let (s, l) = (m.stale.to_lowercase(), m.live.to_lowercase());
        assert!(
            !s.contains(&l) && !l.contains(&s),
            "{}: stale {:?} and live {:?} are not distinguishable by substring",
            m.label,
            m.stale,
            m.live
        );
    }
}
