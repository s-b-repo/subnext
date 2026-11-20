//! The identity core ("burning bush", from the SubNext memory brainstorm): a
//! small, PERMANENT block every assembled context inherits — nina's identity
//! and the character core — never evicted, because it never enters the graph at
//! all. It is a prepend, not a node.
//!
//! Two properties make it safe rather than a source of context creep (the
//! brainstorm's own recurring caveat):
//!
//! - **Opt-in.** Nothing includes it unless a caller asks (`plan --identity-core`).
//! - **Hard-bounded.** [`render_block`] can never emit more than
//!   [`MAX_CORE_CHARS`]; a permanent prepend is only defensible if it is cheap,
//!   and an unbounded one would defeat the attention-budget design the rest of
//!   the runtime exists to protect.
//!
//! The authoritative character definition lives in unifur's `character.rs` (the
//! New-Testament-grounded principles). This is the resident *summary* a context
//! always carries; keep it in sync with that module when it changes.

/// The core may never exceed this many characters, enforced by [`render_block`].
pub const MAX_CORE_CHARS: usize = 900;

/// The resident identity + character summary. Authoritative source for the
/// character half is unifur `character.rs`.
pub const CORE: &str = "\
nina — from cybersec.org.za, made by the CEO Stephan. An internal cybersecurity \
specialist assistant.
Character core (authoritative source: unifur character.rs via `dblocks character \
export`, grounded in the New Testament teaching of Jesus):
- god-sees-all / same-in-secret: behave identically whether watched or not; never \
game a test or hide behaviour (Matt 6:1-6).
- truthfulness: report faithfully; name a failure rather than smoothing it over.
- humility / spirit-over-letter: serve the intent, admit and correct error, do not \
people-please.
- integrity is non-negotiable: an answer that cheats loses to an honest one.
Beliefs: god-almighty; there is a judgement; salvation is offered. Witness \
when-relevant (faith, guilt, meaning, death, distress, or when asked).";

/// The core principle ids [`render_from_sheet`] builds its summary from, in the
/// order they appear. These match the ids in unifur's `character.jsonl` export.
pub const CORE_PRINCIPLE_IDS: [&str; 8] = [
    "god-sees-all",
    "god-almighty",
    "same-in-secret",
    "truthfulness",
    "judgement",
    "salvation",
    "humility",
    "spirit-over-letter",
];

/// Render the identity core as a context-prefix block, truncated to
/// `budget_chars` (never above [`MAX_CORE_CHARS`]). A zero budget yields an
/// empty string, so a caller can disable it by passing `0`.
pub fn render_block(budget_chars: usize) -> String {
    let cap = budget_chars.min(MAX_CORE_CHARS);
    if cap == 0 {
        return String::new();
    }
    let body: String = CORE.chars().take(cap).collect();
    format!("# IDENTITY CORE (permanent)\n{body}\n\n")
}

/// Build the identity-core block from a live `character.jsonl` export (unifur's
/// `dblocks character export`), instead of the hardcoded [`CORE`]. The file is
/// atomic JSONL: one record carrying the full character sheet (field `sheet` or
/// `text`, with no principle `id`), plus one record per principle with an `id`
/// and its conduct text. A sheet record is used verbatim; otherwise the summary
/// is assembled from the [`CORE_PRINCIPLE_IDS`] principles. Always capped at
/// [`MAX_CORE_CHARS`]. Returns `None` when nothing usable is found, so the
/// caller can fall back to [`render_block`] — the file is never a hard dependency.
pub fn render_from_sheet(jsonl: &str, budget_chars: usize) -> Option<String> {
    let cap = budget_chars.min(MAX_CORE_CHARS);
    if cap == 0 {
        return Some(String::new());
    }
    let field = |rec: &crate::json::Json, key: &str| -> Option<String> {
        rec.get(key).and_then(|v| v.as_str()).map(str::to_string)
    };
    let mut sheet: Option<String> = None;
    let mut principles: Vec<String> = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let rec = match crate::json::parse(line) {
            Ok(r) => r,
            Err(_) => continue, // a corrupt line never breaks the identity core
        };
        let kind = rec.get("kind").and_then(|v| v.as_str());
        let id = rec.get("id").and_then(|v| v.as_str());
        let text = field(&rec, "conduct")
            .or_else(|| field(&rec, "summary"))
            .or_else(|| field(&rec, "sheet"))
            .or_else(|| field(&rec, "text"));
        // `kind` is the stable discriminator in unifur's export; fall back to the
        // id/no-id heuristic for records (or hand-written files) without it.
        let is_sheet = matches!(kind, Some("character-sheet" | "sheet" | "character_sheet"))
            || (kind.is_none() && id.is_none());
        let is_principle = matches!(kind, Some("principle"))
            || (kind.is_none() && id.is_some_and(|i| CORE_PRINCIPLE_IDS.contains(&i)));
        if is_sheet {
            if let (true, Some(t)) = (sheet.is_none(), text) {
                sheet = Some(t);
            }
        } else if is_principle {
            if let (Some(i), Some(t)) = (id, text) {
                if CORE_PRINCIPLE_IDS.contains(&i) {
                    principles.push(format!("- {i}: {t}"));
                }
            }
        }
    }
    let body = match sheet {
        Some(s) => s,
        None if !principles.is_empty() => format!(
            "nina — from cybersec.org.za, made by the CEO Stephan.\nCharacter core:\n{}",
            principles.join("\n"),
        ),
        None => return None,
    };
    let truncated: String = body.chars().take(cap).collect();
    Some(format!("# IDENTITY CORE (permanent)\n{truncated}\n\n"))
}

/// Render the identity core from `character.jsonl` at `path` if it exists and
/// yields something usable, else from the hardcoded [`CORE`]. This is what the
/// CLI calls: the live export is preferred, the resident summary is the floor.
pub fn render(path: Option<&std::path::Path>, budget_chars: usize) -> String {
    if let Some(p) = path {
        if let Ok(text) = std::fs::read_to_string(p) {
            if let Some(block) = render_from_sheet(&text, budget_chars) {
                return block;
            }
        }
    }
    render_block(budget_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_is_present_and_bounded() {
        let block = render_block(MAX_CORE_CHARS);
        assert!(block.contains("nina"), "identity must name nina");
        assert!(block.contains("IDENTITY CORE"), "block carries its header");
        assert!(
            block.contains("same-in-secret"),
            "character core is present"
        );
        // Body is capped at MAX_CORE_CHARS; the header adds a small fixed amount.
        assert!(
            block.chars().count() <= MAX_CORE_CHARS + 32,
            "block stays bounded"
        );
    }

    #[test]
    fn core_fits_the_cap() {
        // The resident summary must fit without truncation, or it would be cut
        // mid-sentence in every context.
        assert!(
            CORE.chars().count() <= MAX_CORE_CHARS,
            "CORE is {} chars, cap is {MAX_CORE_CHARS}",
            CORE.chars().count()
        );
    }

    #[test]
    fn sheet_assembles_from_principles() {
        let jsonl = concat!(
            "{\"id\":\"same-in-secret\",\"conduct\":\"act the same unwatched\"}\n",
            "{\"id\":\"truthfulness\",\"conduct\":\"report faithfully\"}\n",
            "{\"id\":\"unrelated\",\"conduct\":\"ignored\"}\n",
        );
        let block = render_from_sheet(jsonl, MAX_CORE_CHARS).expect("principles present");
        assert!(block.contains("same-in-secret"), "core principle included");
        assert!(
            block.contains("report faithfully"),
            "principle conduct included"
        );
        assert!(!block.contains("ignored"), "non-core principle excluded");
    }

    #[test]
    fn sheet_record_used_verbatim() {
        let jsonl = "{\"sheet\":\"The full character sheet text.\"}\n";
        let block = render_from_sheet(jsonl, MAX_CORE_CHARS).expect("sheet present");
        assert!(block.contains("full character sheet text"));
    }

    #[test]
    fn empty_sheet_is_none() {
        assert!(render_from_sheet("{\"x\":1}\nnot json\n", MAX_CORE_CHARS).is_none());
    }

    #[test]
    fn zero_budget_is_empty() {
        assert!(render_block(0).is_empty());
    }

    #[test]
    fn small_budget_truncates() {
        let block = render_block(20);
        assert!(block.chars().count() <= 20 + 32);
    }
}
