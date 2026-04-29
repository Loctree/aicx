//! Noise pattern filter: strips structural scaffolding from chunk content
//! before it reaches the semantic layer.
//!
//! Three classes of noise are removed line-by-line:
//!
//! 1. **Line-numbered grep matches** — `^\s*\d+[ \t]+` (e.g. `60 Passed:`,
//!    `7 status: completed`). Prefixed digit + whitespace without a trailing
//!    dot, so ordered lists (`1. First item`) are preserved.
//! 2. **Tool-call echoes** — `^\s*input:\s*\{` (e.g.
//!    `input: {"command":"for run in agnt-..."`).
//! 3. **YAML frontmatter delimiters** — `^---\s*$` standalone separator lines
//!    that escape the top-of-file frontmatter strip.
//!
//! Lines that match any rule are dropped; the remainder is rejoined with `\n`.
//! Filtering is line-local so semantic paragraphs survive even when adjacent
//! lines are noisy.

use regex::Regex;
use std::sync::OnceLock;

fn line_number_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*\d+[ \t]+(?:[^.\d]|$)").expect("line_number_pattern regex must compile")
    })
}

fn tool_call_echo_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*input:\s*\{").expect("tool_call_echo_pattern regex must compile")
    })
}

fn yaml_delimiter_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^---\s*$").expect("yaml_delimiter_pattern regex must compile"))
}

/// Returns `true` when a single line is structural noise that should be
/// elided from chunk text.
pub fn is_noise_line(line: &str) -> bool {
    if line_number_pattern().is_match(line) {
        return true;
    }
    if tool_call_echo_pattern().is_match(line) {
        return true;
    }
    if yaml_delimiter_pattern().is_match(line) {
        return true;
    }
    false
}

/// Drops scaffolding noise lines while preserving semantic content.
///
/// Returns a `String` with the same line ordering as the input, minus the
/// noise. Trailing newlines from the input are preserved on a best-effort
/// basis (single trailing newline if the original ended with one).
pub fn filter_noise_lines(text: &str) -> String {
    filter_noise_lines_with_count(text).0
}

/// Like [`filter_noise_lines`] but additionally returns the number of lines
/// that were dropped, for observability counters in chunker sidecars.
pub fn filter_noise_lines_with_count(text: &str) -> (String, usize) {
    if text.is_empty() {
        return (String::new(), 0);
    }

    let trailing_newline = text.ends_with('\n');
    let mut out = String::with_capacity(text.len());
    let mut wrote_any = false;
    let mut dropped = 0usize;

    for line in text.lines() {
        if is_noise_line(line) {
            dropped += 1;
            continue;
        }
        if wrote_any {
            out.push('\n');
        }
        out.push_str(line);
        wrote_any = true;
    }

    if trailing_newline && wrote_any {
        out.push('\n');
    }

    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_numbered_grep_matches_dropped() {
        let input = "60 Passed:\nReal content\n7 status: completed";
        let out = filter_noise_lines(input);
        assert_eq!(out, "Real content");
    }

    #[test]
    fn tool_call_echo_dropped() {
        let input = "input: {\"command\":\"for run in agnt-...\"}\nReal follow-up";
        let out = filter_noise_lines(input);
        assert_eq!(out, "Real follow-up");
    }

    #[test]
    fn yaml_delimiter_dropped() {
        let input = "---\ntitle: foo\n---\nReal content";
        // Two `---` delimiters and a key:value yaml line; only delimiters drop,
        // because frontmatter values are policy-side and not in scope here.
        let out = filter_noise_lines(input);
        assert_eq!(out, "title: foo\nReal content");
    }

    #[test]
    fn ordered_list_preserved() {
        let input = "1. First item\n2. Second item\n3. Third item";
        let out = filter_noise_lines(input);
        assert_eq!(out, input);
    }

    #[test]
    fn semantic_paragraph_preserved_amid_noise() {
        let input = "60 Passed:\nThe pipeline shipped despite the gates being noisy.\nResult: 15 suites passed.\ninput: {\"echo\":\"x\"}\nDecision: keep retry.";
        let out = filter_noise_lines(input);
        assert_eq!(
            out,
            "The pipeline shipped despite the gates being noisy.\nResult: 15 suites passed.\nDecision: keep retry."
        );
    }

    #[test]
    fn noise_only_input_returns_empty() {
        let input = "60 Passed:\n7 status: completed\ninput: {\"x\":1}\n---";
        let out = filter_noise_lines(input);
        assert_eq!(out, "");
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(filter_noise_lines(""), "");
    }

    #[test]
    fn trailing_newline_preserved() {
        let input = "Real content\n60 Passed:\n";
        let out = filter_noise_lines(input);
        assert_eq!(out, "Real content\n");
    }

    #[test]
    fn leading_whitespace_in_line_number_match_handled() {
        let input = "  132 of `Chunking... done/total`\nReal content";
        let out = filter_noise_lines(input);
        assert_eq!(out, "Real content");
    }

    #[test]
    fn yaml_value_line_with_space_separator_dropped_as_line_number() {
        // User audit example: "7 status: completed" — line number prefix.
        let input = "7 status: completed\nReal content";
        let out = filter_noise_lines(input);
        assert_eq!(out, "Real content");
    }

    #[test]
    fn header_brackets_preserved() {
        // Format used in chunker headers: starts with `[`, never digits.
        let input = "[project: aicx | agent: claude | date: 2026-04-29]\nbody";
        let out = filter_noise_lines(input);
        assert_eq!(out, input);
    }

    #[test]
    fn timestamped_role_line_preserved() {
        // Chunker emits `[14:32:01] user: ...` — must survive.
        let input = "[14:32:01] user: actual message body\nfollow-up line";
        let out = filter_noise_lines(input);
        assert_eq!(out, input);
    }

    #[test]
    fn count_helper_reports_dropped_lines() {
        let input = "60 Passed:\nReal\ninput: {\"k\":1}\nMore\n---";
        let (out, dropped) = filter_noise_lines_with_count(input);
        assert_eq!(out, "Real\nMore");
        assert_eq!(dropped, 3);
    }

    #[test]
    fn count_helper_zero_for_clean_input() {
        let input = "Real\nContent\nHere";
        let (out, dropped) = filter_noise_lines_with_count(input);
        assert_eq!(out, input);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn is_noise_line_direct_checks() {
        assert!(is_noise_line("60 Passed:"));
        assert!(is_noise_line("  7 status: completed"));
        assert!(is_noise_line("input: {\"k\":\"v\"}"));
        assert!(is_noise_line("---"));
        assert!(is_noise_line("---   "));
        assert!(!is_noise_line("Real content"));
        assert!(!is_noise_line("1. Ordered list"));
        assert!(!is_noise_line("[14:32] user: msg"));
        assert!(!is_noise_line(""));
    }
}
