/// Keywords that signal an important decision or architectural note.
const DECISION_KEYWORDS: &[&str] = &[
    "decision:",
    "plan:",
    "architecture",
    "BREAKING",
    "TODO:",
    "FIXME:",
];

/// Case-sensitive keywords (checked without lowercasing).
const DECISION_KEYWORDS_CASE_SENSITIVE: &[&str] = &["WAŻNE", "KEY"];

pub(crate) fn is_decision_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    DECISION_KEYWORDS
        .iter()
        .any(|kw| lower.contains(&kw.to_lowercase()))
        || DECISION_KEYWORDS_CASE_SENSITIVE
            .iter()
            .any(|kw| message.contains(kw))
}
