//! Learned retrieval over a derived index of source passages and live state.
//!
//! The encoder returns ranks and source coordinates. All prompted text is
//! resolved against the runtime's verified store.

use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::index::Namespace;
use crate::json::{Json, parse};
use crate::nodes::{Kind, NodeIdx};
use crate::planner::ActiveContext;
use crate::policy::QueryType;
use crate::runtime::Dcr;
use crate::spans::SpanIdx;
use crate::text::sentence_ranges;

const ENCODER: &str = include_str!("../scripts/semantic_encoder.py");
const MINIMUM_SIMILARITY: f32 = 0.22;
const SEARCH_LIMIT: usize = 32;
const LEXICAL_LIMIT: usize = 8;
const WINDOW_BYTES: usize = 448;
const WINDOW_OVERLAP: usize = 112;

#[derive(Debug, Default)]
pub(crate) struct Matches {
    pub(crate) nodes: Vec<(NodeIdx, f32)>,
    pub(crate) spans: Vec<Passage>,
    pub(crate) answer_checked: bool,
}

impl Matches {
    pub(crate) fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.spans.is_empty()
    }
}

#[derive(Debug)]
pub(crate) struct Passage {
    pub(crate) span: SpanIdx,
    pub(crate) range: Range<usize>,
    pub(crate) similarity: f32,
    pub(crate) focus: Option<Range<usize>>,
}

/// Give confident semantic matches room to compete for a tight token budget.
pub(crate) fn relevance(similarity: f32) -> f32 {
    0.95 + 1.6 * (similarity - MINIMUM_SIMILARITY) / (1.0 - MINIMUM_SIMILARITY)
}

fn index_path(store: &Path) -> Result<PathBuf, String> {
    let store = store
        .canonicalize()
        .map_err(|e| format!("resolve store: {e}"))?;
    let mut name = store
        .file_name()
        .ok_or("store needs a file name")?
        .to_os_string();
    name.push(".semantic.npz");
    Ok(store.with_file_name(name))
}

/// Index one immutable snapshot. Rebuilding replaces the sidecar atomically.
pub fn build(runtime: &Dcr, store: &Path, model: &Path) -> Result<(PathBuf, usize), String> {
    let path = index_path(store)?;
    let model = model
        .canonicalize()
        .map_err(|e| format!("resolve semantic model: {e}"))?;
    let count = build_at(runtime, &path, Some(&model))?;
    Ok((path, count))
}

/// Refresh a configured store after ingestion, reusing the unchanged vectors.
pub fn refresh(runtime: &Dcr, store: &Path) -> Result<(), String> {
    let path = index_path(store)?;
    if path
        .try_exists()
        .map_err(|e| format!("check semantic index: {e}"))?
    {
        build_at(runtime, &path, None)?;
    }
    Ok(())
}

fn build_at(runtime: &Dcr, path: &Path, model: Option<&Path>) -> Result<usize, String> {
    let rows = corpus(runtime);
    let count = rows.len();
    let snapshot = fingerprint(&rows);
    let request = Json::obj(vec![
        ("snapshot", Json::str(snapshot)),
        ("rows", Json::Arr(rows)),
    ]);
    let operation = if model.is_some() { "build" } else { "refresh" };
    let result = run(operation, path, model, request)?;
    if result.as_usize() != Some(count) {
        return Err("semantic encoder returned an invalid record count".into());
    }
    Ok(count)
}

/// Discover the index next to the store. A configured index must match the
/// current corpus fingerprint before its rankings can be used.
pub fn plan(
    runtime: &mut Dcr,
    store: &Path,
    query: &str,
    encoded_bytes: Option<usize>,
) -> Result<ActiveContext, String> {
    let matches = if encoded_bytes.is_some_and(|bytes| bytes < 4) {
        Matches::default()
    } else {
        search(runtime, store, query)?
    };
    runtime
        .plan_evidence_with(query, None, encoded_bytes, &matches)
        .map_err(|e| e.to_string())
}

fn search(runtime: &Dcr, store: &Path, query: &str) -> Result<Matches, String> {
    let path = index_path(store)?;
    if !path
        .try_exists()
        .map_err(|e| format!("check semantic index: {e}"))?
    {
        return Ok(Matches::default());
    }
    let rows = corpus(runtime);
    let count = rows.len();
    let snapshot = fingerprint(&rows);
    let lookup = is_lookup(query, runtime.policy.classify(query));
    let request = Json::obj(vec![
        ("snapshot", Json::str(snapshot.clone())),
        ("count", Json::num(count as f64)),
        ("query", Json::str(query)),
        ("limit", Json::num((SEARCH_LIMIT * 2) as f64)),
    ]);
    let initial = run("search", &path, None, request)?;
    let ranked = ranked_rows(&rows, &initial, false)?;
    let candidates = shortlist(runtime, query, &rows, &ranked);
    if candidates.is_empty() {
        return Ok(Matches {
            answer_checked: lookup,
            ..Matches::default()
        });
    }
    let response = run(
        "rerank",
        &path,
        None,
        Json::obj(vec![
            ("snapshot", Json::str(snapshot)),
            ("query", Json::str(query)),
            ("lookup", Json::Bool(lookup)),
            ("candidates", Json::Arr(candidates)),
        ]),
    )?;
    let mut matches = decode(runtime, &rows, &response)?;
    matches.answer_checked = lookup;
    Ok(matches)
}

fn is_lookup(query: &str, qtype: QueryType) -> bool {
    if qtype == QueryType::ValueLookup {
        return true;
    }
    if !matches!(qtype, QueryType::Open | QueryType::Recompute) {
        return false;
    }
    query.trim_end().ends_with('?')
        || query.split_whitespace().next().is_some_and(|first| {
            ["who", "what", "when", "where", "which", "how"]
                .iter()
                .any(|word| word.eq_ignore_ascii_case(first))
        })
}

fn shortlist(runtime: &Dcr, query: &str, rows: &[Json], ranked: &[RankedRow<'_>]) -> Vec<Json> {
    let mut lexical_ids = std::collections::HashSet::new();
    for (idx, _) in runtime
        .index
        .lexical_matches(Namespace::Node, query, LEXICAL_LIMIT)
    {
        lexical_ids.insert(runtime.graph.node(idx).id.as_str());
    }
    for (idx, _) in runtime
        .index
        .lexical_matches(Namespace::Span, query, LEXICAL_LIMIT)
    {
        lexical_ids.insert(runtime.raw.span(idx).id.as_str());
    }
    let mut lexical = Vec::new();
    for row in rows.iter().filter_map(Json::as_array) {
        if row[0].as_str().is_some_and(|id| lexical_ids.contains(id)) {
            let text = row[3].as_str().unwrap_or_default();
            let overlap = crate::policy::overlap(query, text);
            if overlap > 0.0 {
                lexical.push((row, overlap));
            }
        }
    }
    lexical.sort_by(|a, b| {
        b.1.total_cmp(&a.1).then_with(|| {
            a.0[3]
                .as_str()
                .map(str::len)
                .cmp(&b.0[3].as_str().map(str::len))
        })
    });
    let mut seen = std::collections::HashSet::new();
    // Reserve space for both lanes. Identical passages compete once even if
    // they occur in multiple documents or in overlapping source ranges.
    let mut candidates = Vec::new();
    for hit in ranked {
        let Some(text) = hit.row[3].as_str() else {
            continue;
        };
        if seen.insert(text) {
            candidates.push(rank_candidate(hit.row, hit.score));
        }
        if candidates.len() == SEARCH_LIMIT - LEXICAL_LIMIT {
            break;
        }
    }
    for (row, _) in lexical {
        let Some(text) = row[3].as_str() else {
            continue;
        };
        if seen.insert(text) {
            let score = ranked
                .iter()
                .find(|hit| hit.row == row)
                .map_or(0.0, |hit| hit.score);
            candidates.push(rank_candidate(row, score));
        }
        if candidates.len() == SEARCH_LIMIT {
            break;
        }
    }
    for hit in ranked {
        if candidates.len() == SEARCH_LIMIT {
            break;
        }
        if hit.row[3].as_str().is_some_and(|text| seen.insert(text)) {
            candidates.push(rank_candidate(hit.row, hit.score));
        }
    }
    candidates
}

fn rank_candidate(row: &[Json], score: f32) -> Json {
    let mut candidate = row.to_vec();
    candidate.insert(3, Json::num(f64::from(score)));
    Json::Arr(candidate)
}

fn fingerprint(rows: &[Json]) -> String {
    // Rows are ordered by stable ids and byte ranges in both store formats.
    // Usage counters and container generations do not change the corpus.
    let encoded = Json::Arr(rows.to_vec()).to_json_string();
    crate::hash::sha256(encoded.as_bytes()).short(64)
}

fn corpus(runtime: &Dcr) -> Vec<Json> {
    let mut rows = Vec::new();
    let mut spans: Vec<_> = runtime.raw.spans().iter().collect();
    spans.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    for span in spans {
        if runtime
            .indexer
            .evidence_by_span
            .get(&span.id)
            .and_then(|id| runtime.graph.get(id))
            .is_some_and(|node| node.meta.superseded_source || !node.meta.corrected_by.is_empty())
        {
            continue;
        }
        let raw = runtime.raw.text_of(&span.id);
        let ranges = sentence_ranges(raw);
        if ranges.len() > 1 && raw.len() <= 1024 {
            rows.push(row(&span.id, 0..raw.len(), raw));
        }
        for (start, end) in ranges {
            for window in sentence_windows(&raw[start..end]) {
                let range = start + window.start..start + window.end;
                rows.push(row(&span.id, range.clone(), &raw[range]));
            }
        }
    }
    let mut nodes: Vec<_> = runtime
        .graph
        .nodes()
        .iter()
        .filter(|node| node.status.is_live() && !matches!(node.kind, Kind::Evidence | Kind::Skill))
        .collect();
    nodes.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    for node in nodes {
        let text = match &node.key {
            Some(key) => format!("{} is {}", key.replace('.', " "), node.value),
            None => node.value.clone(),
        };
        if !text.trim().is_empty() {
            rows.push(row(&node.id, 0..0, &text));
        }
    }
    rows
}

// A stored span can contain a sentence longer than the models' input. Index
// overlapping pieces so the end of the sentence is searchable as well.
fn sentence_windows(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut cursor = 0;
    std::iter::from_fn(move || {
        let tail = &text[cursor..];
        let remaining = tail.trim_start();
        cursor += tail.len() - remaining.len();
        if remaining.is_empty() {
            return None;
        }
        let mut end = remaining.len().min(WINDOW_BYTES);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        let advance = if end < remaining.len() {
            if let Some(space) = remaining[..end]
                .rfind(char::is_whitespace)
                .filter(|&space| space >= WINDOW_BYTES - WINDOW_OVERLAP)
            {
                end = space;
            }
            let mut overlap = end - WINDOW_OVERLAP;
            while !remaining.is_char_boundary(overlap) {
                overlap += 1;
            }
            remaining[overlap..end]
                .find(char::is_whitespace)
                .map_or(overlap, |space| overlap + space)
        } else {
            end
        };
        let range = cursor..cursor + remaining[..end].trim_end().len();
        cursor += advance;
        Some(range)
    })
}

fn row(id: &str, range: Range<usize>, text: &str) -> Json {
    Json::Arr(vec![
        Json::str(id),
        Json::num(range.start as f64),
        Json::num(range.end as f64),
        Json::str(text),
    ])
}

fn coordinates(hit: &[Json]) -> Option<(&str, usize, usize)> {
    let [id, start, end, ..] = hit else {
        return None;
    };
    Some((id.as_str()?, start.as_usize()?, end.as_usize()?))
}

struct RankedRow<'a> {
    row: &'a [Json],
    score: f32,
    focus: Option<Range<usize>>,
}

fn ranked_rows<'a>(
    rows: &'a [Json],
    response: &Json,
    reranked: bool,
) -> Result<Vec<RankedRow<'a>>, String> {
    let known: std::collections::HashMap<_, _> = rows
        .iter()
        .filter_map(|row| {
            let row = row.as_array()?;
            Some((coordinates(row)?, row))
        })
        .collect();
    let hits = response
        .as_array()
        .ok_or("expected semantic index results")?;
    let limit = if reranked {
        SEARCH_LIMIT
    } else {
        SEARCH_LIMIT * 2
    };
    if hits.len() > limit {
        return Err("semantic index returned too many matches".into());
    }
    hits.iter()
        .map(|hit| {
            let hit = hit.as_array().ok_or("invalid semantic hit")?;
            let expected = if reranked { 6 } else { 4 };
            if hit.len() != expected {
                return Err("semantic index returned a malformed hit".into());
            }
            let source = coordinates(hit).ok_or("invalid semantic source coordinates")?;
            let row = known
                .get(&source)
                .ok_or("semantic index returned a match outside the current corpus")?;
            let score = hit[3].as_f64().ok_or("invalid semantic similarity")? as f32;
            let floor = if reranked { MINIMUM_SIMILARITY } else { -1.0 };
            if !(floor..=1.0).contains(&score) {
                return Err("semantic index returned a score outside its range".into());
            }
            let focus = match hit.get(4..) {
                Some([Json::Null, Json::Null]) | Some([]) => None,
                Some([start, end]) => {
                    let (Some(start), Some(end)) = (start.as_usize(), end.as_usize()) else {
                        return Err("invalid semantic focus range".into());
                    };
                    if start >= end
                        || row[3]
                            .as_str()
                            .and_then(|text| text.get(start..end))
                            .is_none()
                    {
                        return Err("semantic focus is outside its source passage".into());
                    }
                    Some(start..end)
                }
                _ => return Err("malformed semantic focus range".into()),
            };
            Ok(RankedRow { row, score, focus })
        })
        .collect()
}

fn decode(runtime: &Dcr, rows: &[Json], response: &Json) -> Result<Matches, String> {
    let mut matches = Matches::default();
    for RankedRow { row, score, focus } in ranked_rows(rows, response, true)? {
        let (id, start, end) = coordinates(row).ok_or("invalid semantic source coordinates")?;
        if start == end {
            let idx = runtime
                .graph
                .idx_of(id)
                .ok_or("unknown semantic graph node")?;
            matches.nodes.push((idx, score));
        } else {
            let span = runtime
                .raw
                .span_idx(id)
                .ok_or("unknown semantic source span")?;
            matches.spans.push(Passage {
                span,
                range: start..end,
                similarity: score,
                focus,
            });
        }
    }
    Ok(matches)
}

fn run(operation: &str, path: &Path, model: Option<&Path>, request: Json) -> Result<Json, String> {
    let python = std::env::var_os("DCR_SEMANTIC_PYTHON").unwrap_or_else(|| "python3".into());
    let mut command = Command::new(python);
    command
        .args(["-I", "-B", "-c", ENCODER, operation])
        .arg(path)
        .env("OPENBLAS_NUM_THREADS", "1")
        .env("TOKENIZERS_PARALLELISM", "false");
    if let Some(model) = model {
        command.arg(model);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("start semantic encoder: {e}"))?;
    let written = child
        .stdin
        .take()
        .ok_or("missing semantic encoder input")?
        .write_all(request.to_json_string().as_bytes());
    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait for semantic encoder: {e}"))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        let detail = error.lines().last().unwrap_or("no diagnostic");
        return Err(format!("semantic encoder: {detail}"));
    }
    written.map_err(|e| format!("write semantic encoder input: {e}"))?;
    parse(std::str::from_utf8(&output.stdout).map_err(|e| e.to_string())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(id: &str, range: Range<usize>) -> Json {
        Json::Arr(vec![Json::Arr(vec![
            Json::str(id),
            Json::num(range.start as f64),
            Json::num(range.end as f64),
            Json::num(0.66),
            Json::Null,
            Json::Null,
        ])])
    }

    #[test]
    fn learned_passage_supplies_a_verbatim_sentence_without_shared_words() {
        let quote = "The feline rested atop the sofa.";
        let other = "An unrelated paragraph about the train schedule. ".repeat(5);
        let mut runtime = Dcr::new(8);
        runtime
            .ingest(&format!("{other}{quote}"), Some("notes"))
            .unwrap();
        let query = "Quote the exact wording about where the cat snoozed.";
        assert_eq!(crate::policy::overlap(query, quote), 0.0);
        assert!(
            runtime
                .plan_evidence(query, None)
                .unwrap()
                .entries
                .is_empty()
        );

        let id = &runtime.raw.span(0).id;
        let matches = decode(
            &runtime,
            &corpus(&runtime),
            &response(id, other.len()..other.len() + quote.len()),
        )
        .unwrap();
        let plan = runtime
            .plan_evidence_with(query, None, None, &matches)
            .unwrap();
        assert!(plan.tokens <= 8);
        assert_eq!(plan.entries.len(), 1);
        assert_eq!(plan.entries[0].rendered, quote);
        assert_eq!(
            runtime.graph.node(plan.entries[0].node).source_spans,
            [runtime.raw.span(0).id.clone()]
        );
    }

    #[test]
    fn the_semantic_index_cannot_select_replaced_sources_or_invent_ranges() {
        let old = "The parcel code is OLD-54.";
        let mut runtime = Dcr::new(24);
        runtime.ingest(old, Some("original")).unwrap();
        let earlier = fingerprint(&corpus(&runtime));
        runtime
            .ingest("Correction: the parcel code is ZX-17.", Some("revised"))
            .unwrap();
        let rows = corpus(&runtime);
        assert_ne!(earlier, fingerprint(&rows));
        assert!(
            decode(
                &runtime,
                &rows,
                &response(&runtime.raw.span(0).id, 0..old.len())
            )
            .is_err()
        );
        assert!(decode(&runtime, &rows, &response(&runtime.raw.span(1).id, 1..5)).is_err());
    }

    #[test]
    fn compact_excerpt_keeps_a_utf8_answer_under_the_runner_limit() {
        let mut runtime = Dcr::new(24);
        let decoy = "Shoes fixed. ";
        let passage = "For everyone needing a patient repair of footwear or a fitting from a specialist, the cobbler works from a recently renovated workshop on Rówan Street beside the railway station.";
        let source = format!("{decoy}{passage}");
        runtime.ingest(&source, Some("notice")).unwrap();
        let span = runtime.raw.span(0);
        let start = passage.find("Rówan Street").unwrap();
        let end = start + "Rówan Street".len();
        let response = Json::Arr(vec![Json::Arr(vec![
            Json::str(&span.id),
            Json::num(decoy.len() as f64),
            Json::num(source.len() as f64),
            Json::num(0.8),
            Json::num(start as f64),
            Json::num(end as f64),
        ])]);
        let mut matches = decode(&runtime, &corpus(&runtime), &response).unwrap();
        matches.answer_checked = true;
        let plan = runtime
            .plan_evidence_with("Where can I get my shoes fixed?", None, Some(25), &matches)
            .unwrap();
        assert_eq!(plan.entries.len(), 1);
        let text = &plan.entries[0].rendered;
        assert!(text.contains("Rówan Street") && source.contains(text));
        assert!(crate::json::evidence_line_bytes(text) <= 25);
        let too_small = crate::json::evidence_line_bytes("Rówan Street") - 1;
        let plan = runtime
            .plan_evidence_with(
                "Where can I get my shoes fixed?",
                None,
                Some(too_small),
                &matches,
            )
            .unwrap();
        assert!(plan.entries.is_empty());
    }

    #[test]
    fn an_answerability_rejection_cannot_fall_back_to_a_lexical_decoy() {
        let mut runtime = Dcr::new(24);
        runtime
            .ingest("The laptop repair desk accepts checks.", Some("notice"))
            .unwrap();
        let query = "How much does laptop repair cost?";
        assert!(
            !runtime
                .plan_evidence(query, None)
                .unwrap()
                .entries
                .is_empty()
        );
        let matches = Matches {
            answer_checked: true,
            ..Matches::default()
        };
        assert!(
            runtime
                .plan_evidence_with(query, None, None, &matches)
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn long_sentences_keep_overlapping_utf8_source_ranges() {
        let words = "café observers record the ordinary details; ".repeat(35);
        let unbroken = "à東".repeat(115);
        let ending = " the cobbler repairs footwear on Rówan Street.";
        let text = format!("{words}{unbroken}{ending}");
        let mut runtime = Dcr::new(24);
        runtime.ingest(&text, Some("notice")).unwrap();

        let rows = corpus(&runtime);
        let windows: Vec<_> = rows.iter().filter_map(Json::as_array).collect();
        assert!(windows.len() > 2);
        assert_eq!(windows[0][1].as_usize(), Some(0));
        for window in &windows {
            let (_, start, end) = coordinates(window).unwrap();
            assert!(end - start <= WINDOW_BYTES);
            assert_eq!(text.get(start..end), window[3].as_str());
        }
        for pair in windows.windows(2) {
            assert!(pair[0][2].as_usize().unwrap() > pair[1][1].as_usize().unwrap());
        }
        let last = windows.last().unwrap();
        assert_eq!(last[2].as_usize(), Some(text.len()));
        assert!(last[3].as_str().unwrap().ends_with(ending));
    }
}
