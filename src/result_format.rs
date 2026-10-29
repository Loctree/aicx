//! One preview shape for intent results and the web viewer.
//!
//! The osnowa is the first [`RESULT_BODY_PREVIEW_CHARS`] Unicode scalar values,
//! cut on a sentence boundary when one sits in the tail window. A longer source
//! keeps an explicit remainder marker. Expand surfaces pass `max_chars = 0` and
//! receive the original text.

/// Default osnowa length for intents, dashboard detail, and `/api/browse`.
pub const RESULT_BODY_PREVIEW_CHARS: usize = 3000;

/// Shorter list-card snippet. Injected into the viewer; not a second body cap.
pub const RESULT_LIST_SNIPPET_CHARS: usize = 240;

const TAIL_LOOKBACK_CHARS: usize = 80;

/// Display cut of one result body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultBody {
    pub body: String,
    pub body_chars_total: usize,
    pub body_truncated: bool,
}

/// Cut `text` to `max_chars` Unicode scalars.
///
/// `max_chars == 0` returns the original text and reports `body_truncated = false`.
/// A cut prefers a `.`, `!`, or `?` inside the last 80 scalars, then `,`, `;`,
/// or `:`, then whitespace. The remainder marker is not part of the 3000.
pub fn cut_result_body(text: &str, max_chars: usize) -> ResultBody {
    let total = text.chars().count();
    if max_chars == 0 || total <= max_chars {
        return ResultBody {
            body: text.to_string(),
            body_chars_total: total,
            body_truncated: false,
        };
    }

    let mut end = max_chars;
    let look_start = end.saturating_sub(TAIL_LOOKBACK_CHARS);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let window = &chars[look_start..end];

    if let Some((byte_idx, ch)) = window
        .iter()
        .rev()
        .find(|(_, ch)| matches!(ch, '.' | '!' | '?'))
    {
        end = chars
            .iter()
            .position(|(idx, _)| idx == byte_idx)
            .unwrap_or(end)
            + 1;
        let _ = ch;
    } else if let Some((byte_idx, _)) = window
        .iter()
        .rev()
        .find(|(_, ch)| matches!(ch, ',' | ';' | ':'))
    {
        end = chars
            .iter()
            .position(|(idx, _)| idx == byte_idx)
            .unwrap_or(end);
    } else if let Some((byte_idx, _)) = window.iter().rev().find(|(_, ch)| ch.is_whitespace()) {
        end = chars
            .iter()
            .position(|(idx, _)| idx == byte_idx)
            .unwrap_or(end);
    }

    let byte_end = chars.get(end).map(|(idx, _)| *idx).unwrap_or(text.len());
    let stem = text[..byte_end].trim_end();
    let kept = stem.chars().count();
    let omitted = total.saturating_sub(kept);
    let mut body = stem.to_string();
    body.push_str(" …[+");
    body.push_str(&group_thousands(omitted));
    body.push_str(" znaków]");
    ResultBody {
        body,
        body_chars_total: total,
        body_truncated: true,
    }
}

/// `projekt · agent · rodzaj · data · [score] · sesja`. Empty parts are skipped.
pub fn result_header(
    project: &str,
    agent: &str,
    kind: &str,
    date: &str,
    score: Option<f64>,
    session_id: &str,
) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(6);
    let score_owned: String;
    for value in [project, agent, kind, date] {
        let value = value.trim();
        if !value.is_empty() {
            parts.push(value);
        }
    }
    if let Some(score) = score.filter(|score| *score > 0.0) {
        score_owned = format_score(score);
        parts.push(&score_owned);
    }
    let short = short_session_id(session_id);
    if !short.is_empty() {
        parts.push(short);
    }
    parts.join(" · ")
}

pub fn short_session_id(session_id: &str) -> &str {
    let session_id = session_id.trim();
    match session_id.char_indices().nth(8) {
        Some((idx, _)) => &session_id[..idx],
        None => session_id,
    }
}

fn format_score(score: f64) -> String {
    if (score - score.round()).abs() < f64::EPSILON {
        format!("{}", score.round() as i64)
    } else {
        format!("{score:.1}")
    }
}

fn group_thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_under_the_limit_is_unchanged() {
        let text = "Keep the parser flat";
        let cut = cut_result_body(text, RESULT_BODY_PREVIEW_CHARS);
        assert_eq!(cut.body, text);
        assert_eq!(cut.body_chars_total, text.chars().count());
        assert!(!cut.body_truncated);
    }

    #[test]
    fn polish_scalars_count_as_one_character() {
        let text = "łódź";
        assert_eq!(text.chars().count(), 4);
        assert!(text.len() > 4);
        let cut = cut_result_body(text, 2);
        assert!(cut.body.starts_with("łó"));
        assert!(cut.body_truncated);
        assert_eq!(cut.body_chars_total, 4);
        assert!(cut.body.contains("znaków"));
    }

    #[test]
    fn emoji_is_one_scalar_and_stays_intact() {
        let text = "😀ab";
        assert_eq!(text.chars().count(), 3);
        let cut = cut_result_body(text, 1);
        let stem = cut.body.split(" …[+").next().expect("stem");
        assert_eq!(stem, "😀");
        assert!(cut.body_truncated);
        assert_eq!(cut.body_chars_total, 3);
    }

    #[test]
    fn exactly_3000_chars_has_no_marker() {
        let text = "a".repeat(RESULT_BODY_PREVIEW_CHARS);
        let cut = cut_result_body(&text, RESULT_BODY_PREVIEW_CHARS);
        assert_eq!(cut.body, text);
        assert!(!cut.body_truncated);
        assert_eq!(cut.body_chars_total, 3000);
    }

    #[test]
    fn three_thousand_one_without_a_break_hard_cuts() {
        let text = "b".repeat(RESULT_BODY_PREVIEW_CHARS + 1);
        let cut = cut_result_body(&text, RESULT_BODY_PREVIEW_CHARS);
        assert!(cut.body_truncated);
        assert_eq!(cut.body_chars_total, 3001);
        assert!(cut.body.ends_with(" …[+1 znaków]"));
        let stem = cut.body.split(" …[+").next().expect("stem");
        assert_eq!(stem.chars().count(), 3000);
    }

    #[test]
    fn sentence_terminator_in_the_tail_window_is_kept() {
        let prefix = "x".repeat(RESULT_BODY_PREVIEW_CHARS - 40);
        let text = format!("{prefix}This sentence ends here. And then extra words follow on.");
        assert!(text.chars().count() > RESULT_BODY_PREVIEW_CHARS);
        let cut = cut_result_body(&text, RESULT_BODY_PREVIEW_CHARS);
        let stem = cut.body.split(" …[+").next().expect("stem");
        assert!(stem.ends_with('.'));
        assert!(stem.contains("This sentence ends here."));
        assert!(!stem.contains("extra words"));
        assert!(cut.body.contains("znaków"));
        assert!(cut.body_truncated);
    }

    #[test]
    fn omitted_count_uses_a_space_thousands_separator() {
        let text = "x".repeat(RESULT_BODY_PREVIEW_CHARS + 4812);
        let cut = cut_result_body(&text, RESULT_BODY_PREVIEW_CHARS);
        assert!(cut.body.ends_with(" …[+4 812 znaków]"));
        assert_eq!(cut.body_chars_total, 3000 + 4812);
    }

    #[test]
    fn zero_cap_returns_the_whole_text() {
        let text = "ę".repeat(20);
        let cut = cut_result_body(&text, 0);
        assert_eq!(cut.body, text);
        assert!(!cut.body_truncated);
        assert_eq!(cut.body_chars_total, 20);
    }

    #[test]
    fn header_skips_empty_fields_and_keeps_order() {
        assert_eq!(
            result_header(
                "Loctree/aicx",
                "claude",
                "INTENT",
                "2026-07-02",
                None,
                "sess-b2-long"
            ),
            "Loctree/aicx · claude · INTENT · 2026-07-02 · sess-b2-"
        );
        assert_eq!(
            result_header("Loctree/aicx", "", "INTENT", "", Some(0.0), ""),
            "Loctree/aicx · INTENT"
        );
        assert_eq!(
            result_header("P", "A", "K", "D", Some(87.0), "abcdef123456"),
            "P · A · K · D · 87 · abcdef12"
        );
    }
}
