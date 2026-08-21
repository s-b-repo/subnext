//! The seed floor is load-bearing, and the standard probe set cannot see it.
//!
//! Raising `seed_min_ratio` from 0.3 to 0.7 cuts the working set from 461.9
//! tokens to 145.1 with all seven standard probes still answering correctly.
//! That reads like a 3x saving for nothing. It is not: on the adversarial
//! mutation set, where the superseded value is the closer lexical match, the
//! same change takes correction-following from 4/4 to 0/4 and serves the stale
//! value every time.
//!
//! Both directions are asserted here on purpose. A test that only pinned the
//! good configuration would pass just as happily if the floor stopped mattering
//! at all, which is the failure mode this whole file exists to catch.

use dcr::bench::{ADVERSARIAL, build_mutation_corpus_from};
use dcr::index::{Fusion, Namespace};
use dcr::llm::LocalReasoner;
use dcr::runtime::Dcr;

/// (corrected, stale served) over the adversarial mutation set.
fn adversarial(floor: f32, fusion: Fusion) -> (usize, usize) {
    let corpus = build_mutation_corpus_from(300, ADVERSARIAL);
    let mut rt = Dcr::new(1200);
    rt.planner.seed_min_ratio = floor;
    rt.index.fusion = fusion;
    for (id, t) in &corpus.docs {
        rt.ingest(t, Some(id)).expect("ingest");
    }
    let mut reasoner = LocalReasoner::new();
    let (mut corrected, mut stale) = (0, 0);
    for m in ADVERSARIAL {
        let text = rt.ask_with(m.query, None, &mut reasoner).text.to_lowercase();
        if text.contains(&m.live.to_lowercase()) {
            corrected += 1;
        }
        if text.contains(&m.stale.to_lowercase()) {
            stale += 1;
        }
    }
    (corrected, stale)
}

#[test]
fn default_seed_floor_follows_every_correction() {
    let (corrected, stale) = adversarial(0.3, Fusion::Linear);
    assert_eq!(corrected, ADVERSARIAL.len(), "default floor must correct all");
    assert_eq!(stale, 0, "default floor must never serve the stale value");
}

#[test]
fn raising_the_seed_floor_breaks_correction_following() {
    // The control for the test above. If this ever passes, the floor has
    // stopped being the thing that carries corrections and the cheap
    // configuration is no longer being rejected for a measured reason.
    let (corrected, stale) = adversarial(0.7, Fusion::Linear);
    assert!(
        corrected < ADVERSARIAL.len(),
        "a floor of 0.7 is supposed to lose corrections; it corrected {corrected}"
    );
    assert!(
        stale > 0,
        "a floor of 0.7 is supposed to serve superseded values; it served none"
    );
}

#[test]
fn rank_fusion_is_off_by_default() {
    // Every published figure was measured on the linear blend.
    assert_eq!(Dcr::new(1200).index.fusion, Fusion::Linear);
}

#[test]
fn rank_fusion_changes_the_ranking_it_is_offered_to_change() {
    // An option that cannot be observed to do anything is not an option, and
    // `bench --fusion` would be comparing a configuration against itself.
    let corpus = build_mutation_corpus_from(300, ADVERSARIAL);
    let mut rt = Dcr::new(1200);
    for (id, t) in &corpus.docs {
        rt.ingest(t, Some(id)).expect("ingest");
    }
    let query = ADVERSARIAL[0].query;
    let qv = dcr::embed::hashing_embed(query, dcr::embed::DIM);
    let linear = rt.index.search(Namespace::Node, query, &qv, 12);
    rt.index.fusion = Fusion::Rrf { k0: 60.0 };
    let rrf = rt.index.search(Namespace::Node, query, &qv, 12);
    assert_ne!(
        linear.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        rrf.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        "rank fusion returned the same order as the linear blend"
    );
}

#[test]
fn rank_fusion_scores_stay_on_the_linear_scale() {
    // RRF's raw scores are ~1/60 of the linear path's. Anything downstream that
    // reads a score as a magnitude — the seed floor is one — would change
    // behaviour for a reason unrelated to ranking, so the fused list is
    // rescaled to top = 1.0. This pins that.
    let corpus = build_mutation_corpus_from(300, ADVERSARIAL);
    let mut rt = Dcr::new(1200);
    rt.index.fusion = Fusion::Rrf { k0: 60.0 };
    for (id, t) in &corpus.docs {
        rt.ingest(t, Some(id)).expect("ingest");
    }
    let query = ADVERSARIAL[0].query;
    let qv = dcr::embed::hashing_embed(query, dcr::embed::DIM);
    let top = rt.index.search(Namespace::Node, query, &qv, 12);
    let top = top.first().expect("a hit").1;
    assert!(
        (top - 1.0).abs() < 1e-5,
        "fused top score should be rescaled to 1.0, got {top}"
    );
}
