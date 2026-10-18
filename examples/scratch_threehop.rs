//! Scratch probe: ingest the adversarial corpus docs by hand and print the
//! rendered three-hop context, to locate what my render-order change moved.

use dcr::bench::{build_adversarial_corpus, Corpus};
use dcr::llm::LocalReasoner;

fn main() -> Result<(), dcr::DcrError> {
    let corpus: Corpus = build_adversarial_corpus(300);
    let mut rt = dcr::Dcr::new(1200);
    for (id, text) in &corpus.docs {
        rt.ingest(text, Some(id.as_str()))?;
    }
    let mut reasoner = LocalReasoner::new();
    for probe in &corpus.probes {
        let answer = rt.ask_with(probe.query, None, &mut reasoner);
        if probe.label.contains("three-hop") {
            println!(
                "pass={} expected={:?}\n\n{}",
                probe.scores(&answer.context.render()),
                probe.expected,
                answer.context.render()
            );
        }
    }
    Ok(())
}
