fn decision_keywords() -> &'static [&'static str] {
    crate::parser::intent_phrases::phrases().decision_output
}

fn decision_keywords_case_sensitive() -> &'static [&'static str] {
    crate::parser::intent_phrases::phrases().decision_output_case_sensitive
}

pub(crate) fn is_decision_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    decision_keywords()
        .iter()
        .any(|kw| lower.contains(&kw.to_lowercase()))
        || decision_keywords_case_sensitive()
            .iter()
            .any(|kw| message.contains(kw))
}
