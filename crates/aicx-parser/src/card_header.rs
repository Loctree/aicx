//! Header-agnostic card header parsing.
//!
//! Canonical-store cards carry identity metadata in one of two header forms:
//! the legacy single-line bracket header emitted by the v1 writer
//! (`[project: … | agent: … | date: … | frame_kind: …]`) and the card schema
//! v2 YAML frontmatter block (`---\nproject: …\n---`). Every in-repo reader
//! goes through this module so neither form leaks bespoke parsing into
//! consumers. The `.meta.json` sidecar stays authoritative — readers call
//! `store::load_sidecar` first and fall back to [`parse_card_header`].

use crate::timeline::FrameKind;

const BRACKET_PREFIX: &str = "[project:";

/// Card identity fields recovered from a chunk header (either form).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardHeader {
    pub project: Option<String>,
    pub agent: Option<String>,
    pub date: Option<String>,
    pub frame_kind: Option<FrameKind>,
}

enum HeaderForm<'a> {
    Bracket { fields: &'a str, body: &'a str },
    Frontmatter { header: CardHeader, body: &'a str },
}

/// Single-line guard for the legacy bracket header. Line-stream filters
/// (search previews, evidence excerpts, noise detection) use this so the
/// bracket prefix has exactly one reader-side home.
pub fn is_bracket_header_line(line: &str) -> bool {
    line.trim_start().starts_with(BRACKET_PREFIX)
}

/// Parse the card header at the start of `text`, accepting both the bracket
/// and the YAML-frontmatter form. Returns `None` when `text` carries no
/// recognizable card header.
pub fn parse_card_header(text: &str) -> Option<CardHeader> {
    match header_form(text)? {
        HeaderForm::Bracket { fields, .. } => Some(parse_bracket_fields(fields)),
        HeaderForm::Frontmatter { header, .. } => Some(header),
    }
}

/// Body of a card after its header (either form), with leading newlines
/// trimmed. Text without a recognizable card header is returned unchanged.
pub fn card_body(text: &str) -> &str {
    match header_form(text) {
        Some(HeaderForm::Bracket { body, .. }) => body,
        Some(HeaderForm::Frontmatter { body, .. }) => body,
        None => text,
    }
}

fn header_form(text: &str) -> Option<HeaderForm<'_>> {
    if let Some(rest) = text.strip_prefix(BRACKET_PREFIX) {
        // Body semantics must stay byte-identical to the historical
        // `chunk_body_after_header`: everything after the first line, with
        // leading CR/LF trimmed; a header-only card has an empty body.
        let (fields, body) = match rest.split_once('\n') {
            Some((fields, body)) => (fields, body.trim_start_matches(['\r', '\n'])),
            None => (rest, ""),
        };
        return Some(HeaderForm::Bracket { fields, body });
    }
    frontmatter_form(text)
}

fn parse_bracket_fields(fields: &str) -> CardHeader {
    let inner = fields.trim_end();
    let inner = inner.strip_suffix(']').unwrap_or(inner);

    let mut header = CardHeader::default();
    for (idx, segment) in inner.split('|').enumerate() {
        let segment = segment.trim();
        if idx == 0 {
            // `[project:` is already stripped; the first segment is the value.
            header.project = non_empty(segment);
            continue;
        }
        let Some((key, value)) = segment.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "agent" => header.agent = non_empty(value),
            "date" => header.date = non_empty(value),
            "frame_kind" => header.frame_kind = FrameKind::parse(value),
            _ => {}
        }
    }
    header
}

fn frontmatter_form(text: &str) -> Option<HeaderForm<'_>> {
    // The v2 writer anchors the delimiter at byte 0. This is stricter than
    // the report-frontmatter parser on purpose: a card body must never be
    // chopped because it merely opens with a horizontal rule.
    let after_open = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let end = after_open.find("\n---")?;
    let block = &after_open[..end];
    let rest = &after_open[end + 4..];
    let rest = rest.strip_prefix('\r').unwrap_or(rest);
    // The closing delimiter must terminate its own line (`----` is content).
    if !(rest.is_empty() || rest.starts_with('\n')) {
        return None;
    }

    // Key gate: only a block carrying at least one card identity field is a
    // card header; anything else stays part of the body untouched.
    let header = parse_frontmatter_card_fields(block)?;
    Some(HeaderForm::Frontmatter {
        header,
        body: rest.trim_start_matches(['\r', '\n']),
    })
}

fn parse_frontmatter_card_fields(block: &str) -> Option<CardHeader> {
    let mut header = CardHeader::default();
    let mut saw_card_field = false;

    for raw_line in block.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(|ch| matches!(ch, '"' | '\''));
        match key.trim() {
            "project" => {
                header.project = non_empty(value);
                saw_card_field = true;
            }
            "agent" => {
                header.agent = non_empty(value);
                saw_card_field = true;
            }
            "date" => {
                header.date = non_empty(value);
                saw_card_field = true;
            }
            "frame_kind" => {
                header.frame_kind = FrameKind::parse(value);
                saw_card_field = true;
            }
            _ => {}
        }
    }

    saw_card_field.then_some(header)
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracket_and_frontmatter_forms_parse_to_same_card_header() {
        let bracket = "[project: Loctree/aicx | agent: claude | date: 2026-07-02 | frame_kind: user_msg]\n\nbody";
        let frontmatter = "---\nproject: Loctree/aicx\nagent: claude\ndate: 2026-07-02\nframe_kind: user_msg\n---\n\nbody";

        let from_bracket = parse_card_header(bracket).expect("bracket header parses");
        let from_frontmatter = parse_card_header(frontmatter).expect("frontmatter header parses");

        assert_eq!(from_bracket, from_frontmatter);
        assert_eq!(from_bracket.project.as_deref(), Some("Loctree/aicx"));
        assert_eq!(from_bracket.agent.as_deref(), Some("claude"));
        assert_eq!(from_bracket.date.as_deref(), Some("2026-07-02"));
        assert_eq!(from_bracket.frame_kind, Some(FrameKind::UserMsg));
        assert_eq!(card_body(bracket), card_body(frontmatter));
        assert_eq!(card_body(bracket), "body");
    }

    #[test]
    fn bracket_minimal_project_only_header_parses() {
        let header = parse_card_header("[project: x]\nbody").expect("degenerate header parses");
        assert_eq!(header.project.as_deref(), Some("x"));
        assert_eq!(header.agent, None);
        assert_eq!(header.date, None);
        assert_eq!(header.frame_kind, None);
    }

    #[test]
    fn card_body_strips_bracket_header_and_preserves_signals_block() {
        let text = "[project: demo | agent: codex | date: 2026-03-15]\n\n[signals]\nDecision:\n- [decision] keep\n[/signals]\n\n[12:00:00] user: hello\n";
        let body = card_body(text);
        assert!(body.starts_with("[signals]"));
        assert!(body.contains("[12:00:00] user: hello"));
    }

    #[test]
    fn card_body_of_header_only_bracket_card_is_empty() {
        assert_eq!(
            card_body("[project: demo | agent: codex | date: 2026-03-15]\n\n"),
            ""
        );
        assert_eq!(card_body("[project: demo]"), "");
    }

    #[test]
    fn card_body_strips_frontmatter_block_and_preserves_body() {
        let text = "---\nproject: demo\nagent: claude\ndate: 2026-07-02\n---\n\n[signals]\nDecision:\n- [decision] keep\n[/signals]\n\nreal content\n";
        let body = card_body(text);
        assert!(body.starts_with("[signals]"));
        assert!(body.ends_with("real content\n"));
    }

    #[test]
    fn non_card_frontmatter_and_plain_text_stay_untouched() {
        // A leading horizontal-rule block without card identity keys is body
        // content, not a header — chopping it would corrupt empty-body checks.
        let rule_block = "---\nsome: yaml\nunrelated: field\n---\nreal body";
        assert_eq!(card_body(rule_block), rule_block);
        assert_eq!(parse_card_header(rule_block), None);

        let plain = "no header here\njust text";
        assert_eq!(card_body(plain), plain);
        assert_eq!(parse_card_header(plain), None);

        // `----` after the block is content, not a closing delimiter.
        let dashes = "---\nproject: demo\n----\nbody";
        assert_eq!(card_body(dashes), dashes);
    }

    #[test]
    fn frontmatter_frame_kind_disagreeing_with_nothing_else_still_parses() {
        let text = "---\nframe_kind: agent_reply\n---\nbody";
        let header = parse_card_header(text).expect("frame_kind alone is a card field");
        assert_eq!(header.frame_kind, Some(FrameKind::AgentReply));
        assert_eq!(card_body(text), "body");
    }

    #[test]
    fn is_bracket_header_line_matches_only_bracket_form() {
        assert!(is_bracket_header_line(
            "[project: demo | agent: claude | date: 2026-07-02]"
        ));
        assert!(is_bracket_header_line("  [project: demo]"));
        assert!(!is_bracket_header_line("project: demo"));
        assert!(!is_bracket_header_line("---"));
        assert!(!is_bracket_header_line("[metadata] project: demo"));
    }
}
