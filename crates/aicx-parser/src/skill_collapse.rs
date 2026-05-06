//! Cross-entry skill-template + content-hash collapse.
//!
//! Detects repeated message bodies (e.g. `/loop` fire'ing the same skill
//! template, repeated tool outputs, fixture reposts) and collapses every
//! occurrence after the first into a stub reference. Operates message-level
//! across an entry sequence, complementing line-level [`crate::noise`].
//!
//! Originating signal: extracting a `/loop`-driven session yielded ~16k
//! lines, ~60% of which were verbatim re-pastes of the same skill prompt
//! (`vc-ownership` ~290 lines per fire × 6 fires). The line-level noise
//! filter could not catch this — the lines themselves are semantic prose,
//! they're just identical across messages. Collapse acts at message level
//! across the sequence: keep the first full body, replace later identical
//! bodies with a one-line stub pointing at the first occurrence.

use crate::timeline::TimelineEntry;
use std::collections::HashMap;

/// Minimum line count for a message to be eligible for collapse.
///
/// Short messages (`< 8` lines) repeat naturally in conversations — `"ok"`,
/// `"yes"`, brief acknowledgements, terse `assistant` replies — and must
/// never be collapsed. The threshold is empirical: skill templates and
/// tool boilerplates start at ~20+ lines, while the longest legitimate
/// short replies stay under 8.
pub const DEFAULT_THRESHOLD_LINES: usize = 8;

/// Aggregate statistics from a single [`collapse_repeats`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollapseStats {
    /// Number of messages that were replaced with a stub.
    pub messages_collapsed: usize,
    /// Total bytes saved across all collapsed messages
    /// (`sum(original_len - stub_len)`).
    pub bytes_saved: usize,
}

impl CollapseStats {
    /// Merge another stats record into `self` (additive).
    pub fn merge(&mut self, other: &Self) {
        self.messages_collapsed += other.messages_collapsed;
        self.bytes_saved += other.bytes_saved;
    }
}

/// Collapse repeated message bodies in-place across an entry sequence.
///
/// Strategy: first occurrence of each unique message body (with `>=
/// threshold_lines` lines) is kept verbatim; later identical bodies are
/// replaced with a single-line stub:
///
/// - `<skill-ref: <name> | first-seen: entry #<idx>>` when the body
///   matches a `Base directory for this skill: <path>` marker
///   (see [`detect_skill_marker`]).
/// - `<dedup-ref: hash:<8hex> | first-seen: entry #<idx>>` otherwise.
///
/// Returns the (possibly modified) entry vec and a [`CollapseStats`]
/// record for observability.
pub fn collapse_repeats(
    mut entries: Vec<TimelineEntry>,
    threshold_lines: usize,
) -> (Vec<TimelineEntry>, CollapseStats) {
    let mut seen: HashMap<u64, usize> = HashMap::new();
    let mut stats = CollapseStats::default();

    for (idx, entry) in entries.iter_mut().enumerate() {
        let line_count = entry.message.lines().count();
        if line_count < threshold_lines {
            continue;
        }
        let h = hash_message(&entry.message);
        if let Some(&first_idx) = seen.get(&h) {
            let original_len = entry.message.len();
            let stub = match detect_skill_marker(&entry.message) {
                Some(name) => format!("<skill-ref: {} | first-seen: entry #{}>", name, first_idx),
                None => format!(
                    "<dedup-ref: hash:{:08x} | first-seen: entry #{}>",
                    h as u32, first_idx
                ),
            };
            stats.messages_collapsed += 1;
            stats.bytes_saved += original_len.saturating_sub(stub.len());
            entry.message = stub;
        } else {
            seen.insert(h, idx);
        }
    }

    (entries, stats)
}

/// Detect a skill-template marker in the first ~8 lines of a message body.
///
/// Pattern: `Base directory for this skill: <path>` (optionally with a
/// `> ` quote prefix when the message was rendered into a markdown
/// blockquote). Returns the skill name — the path basename — when found.
///
/// Examples:
/// - `"Base directory for this skill: /Users/x/.claude/skills/vc-ownership"`
///   → `Some("vc-ownership")`
/// - `"> Base directory for this skill: /home/y/.claude/skills/vc-init"`
///   → `Some("vc-init")`
/// - `"Some other content"` → `None`
pub fn detect_skill_marker(message: &str) -> Option<String> {
    const MARKER: &str = "Base directory for this skill: ";
    message.lines().take(8).find_map(|line| {
        let trimmed = line.trim_start();
        let after_quote = trimmed.trim_start_matches('>').trim_start();
        let path = after_quote.strip_prefix(MARKER)?;
        std::path::Path::new(path.trim())
            .file_name()
            .and_then(|s| s.to_str())
            .map(String::from)
    })
}

/// Stable hash of a message body for content-equality lookup.
///
/// Uses [`std::hash::DefaultHasher`] for portability. Content-equality
/// is enforced by the source string itself; the hash is only an index
/// into the seen-set, so collision risk stays negligible at session
/// scale (typically `<= O(10^4)` messages).
fn hash_message(message: &str) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    message.hash(&mut h);
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(role: &str, message: &str) -> TimelineEntry {
        TimelineEntry {
            timestamp: Utc::now(),
            agent: "claude".into(),
            session_id: "test-session".into(),
            role: role.into(),
            message: message.into(),
            frame_kind: None,
            branch: None,
            cwd: None,
        }
    }

    fn long_skill_body(skill: &str, suffix: &str) -> String {
        let mut body = format!("Base directory for this skill: /home/u/.claude/skills/{skill}\n");
        body.push_str("# SkillName\n## Purpose\n");
        for i in 0..30 {
            body.push_str(&format!("- bullet line {i}\n"));
        }
        body.push_str(suffix);
        body
    }

    #[test]
    fn collapses_repeated_skill_template() {
        let body = long_skill_body("vc-ownership", "");
        let entries = vec![
            entry("user", &body),
            entry("user", &body),
            entry("user", &body),
        ];
        let (out, stats) = collapse_repeats(entries, DEFAULT_THRESHOLD_LINES);

        assert_eq!(stats.messages_collapsed, 2);
        assert!(stats.bytes_saved > 0);
        assert_eq!(out[0].message, body, "first occurrence kept verbatim");
        assert!(
            out[1].message.starts_with("<skill-ref: vc-ownership"),
            "second occurrence collapsed with skill-ref stub: {}",
            out[1].message
        );
        assert!(out[2].message.contains("first-seen: entry #0"));
    }

    #[test]
    fn skill_marker_naming_extracted() {
        let body = long_skill_body("vc-init", "");
        let entries = vec![entry("user", &body), entry("user", &body)];
        let (out, _) = collapse_repeats(entries, DEFAULT_THRESHOLD_LINES);
        assert!(out[1].message.contains("vc-init"));
    }

    #[test]
    fn dedup_ref_used_when_no_skill_marker() {
        let body = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let entries = vec![entry("assistant", &body), entry("assistant", &body)];
        let (out, stats) = collapse_repeats(entries, DEFAULT_THRESHOLD_LINES);
        assert_eq!(stats.messages_collapsed, 1);
        assert!(out[1].message.starts_with("<dedup-ref: hash:"));
        assert!(out[1].message.contains("first-seen: entry #0"));
    }

    #[test]
    fn short_messages_not_collapsed() {
        // Two-line bodies are way under threshold even when identical.
        let entries = vec![
            entry("user", "ok\nthanks"),
            entry("user", "ok\nthanks"),
            entry("user", "ok\nthanks"),
        ];
        let (out, stats) = collapse_repeats(entries, DEFAULT_THRESHOLD_LINES);
        assert_eq!(stats.messages_collapsed, 0);
        assert_eq!(out[1].message, "ok\nthanks");
        assert_eq!(out[2].message, "ok\nthanks");
    }

    #[test]
    fn different_long_messages_not_collapsed() {
        let a = long_skill_body("vc-ownership", "different-tail-A");
        let b = long_skill_body("vc-ownership", "different-tail-B");
        let entries = vec![entry("user", &a), entry("user", &b)];
        let (out, stats) = collapse_repeats(entries, DEFAULT_THRESHOLD_LINES);
        assert_eq!(stats.messages_collapsed, 0);
        assert_eq!(out[0].message, a);
        assert_eq!(out[1].message, b);
    }

    #[test]
    fn first_occurrence_index_is_stable() {
        let body = long_skill_body("vc-init", "");
        let entries = vec![
            entry("user", "short start"),
            entry("user", &body),
            entry("user", "short middle"),
            entry("user", &body),
        ];
        let (out, _) = collapse_repeats(entries, DEFAULT_THRESHOLD_LINES);
        assert!(
            out[3].message.contains("first-seen: entry #1"),
            "stub should reference index 1 (the first long occurrence): {}",
            out[3].message
        );
    }

    #[test]
    fn detect_skill_marker_plain() {
        let msg = "Base directory for this skill: /home/u/.claude/skills/vc-init\n# Init\n";
        assert_eq!(detect_skill_marker(msg).as_deref(), Some("vc-init"));
    }

    #[test]
    fn detect_skill_marker_quote_prefix() {
        let msg = "> Base directory for this skill: /home/u/.claude/skills/vc-ownership\n";
        assert_eq!(detect_skill_marker(msg).as_deref(), Some("vc-ownership"));
    }

    #[test]
    fn detect_skill_marker_indented_quote() {
        let msg = "   >> Base directory for this skill: /a/b/skills/vc-marbles\n";
        assert_eq!(detect_skill_marker(msg).as_deref(), Some("vc-marbles"));
    }

    #[test]
    fn detect_skill_marker_returns_none_for_plain_text() {
        assert_eq!(detect_skill_marker("Just a regular message"), None);
        assert_eq!(detect_skill_marker(""), None);
    }

    #[test]
    fn detect_skill_marker_skipped_after_first_8_lines() {
        let mut msg = String::new();
        for _ in 0..10 {
            msg.push_str("filler\n");
        }
        msg.push_str("Base directory for this skill: /a/b/c/vc-late\n");
        // The marker is past the 8-line scan window — must not match.
        assert_eq!(detect_skill_marker(&msg), None);
    }

    #[test]
    fn merge_stats_is_additive() {
        let mut a = CollapseStats {
            messages_collapsed: 2,
            bytes_saved: 1000,
        };
        let b = CollapseStats {
            messages_collapsed: 3,
            bytes_saved: 500,
        };
        a.merge(&b);
        assert_eq!(a.messages_collapsed, 5);
        assert_eq!(a.bytes_saved, 1500);
    }

    #[test]
    fn empty_input_returns_empty() {
        let (out, stats) = collapse_repeats(Vec::new(), DEFAULT_THRESHOLD_LINES);
        assert!(out.is_empty());
        assert_eq!(stats, CollapseStats::default());
    }
}
