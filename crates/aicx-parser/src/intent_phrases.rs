//! Phrase lists compiled from `intent_phrases.toml`.
//! The toml file is the list. This module only leaks it into `&'static [&str]`.

use serde::Deserialize;
use std::path::Path;
use std::sync::atomic::{AtomicPtr, Ordering};

#[derive(Debug, Deserialize)]
struct Raw {
    intent: List,
    task: Task,
    decision: Decision,
    requirement: List,
    outcome: Outcome,
    question: List,
    assumption: List,
    why: List,
    argue: List,
    insight: List,
    negation: Negation,
}

#[derive(Debug, Deserialize)]
struct List {
    keywords: Option<Vec<String>>,
    markers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct Task {
    directive: Vec<String>,
    action_heads: Vec<String>,
    commitment_heads: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Decision {
    policy: Vec<String>,
    output: Vec<String>,
    output_case_sensitive: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Outcome {
    result_keywords: Vec<String>,
    bare_affirmation: Vec<String>,
    result_strict: Vec<String>,
    result_soft: Vec<String>,
    result_shape: Vec<String>,
    completion: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Negation {
    pre: Vec<String>,
    post: Vec<String>,
}

pub struct Phrases {
    pub intent: &'static [&'static str],
    pub task_directive: &'static [&'static str],
    pub task_action_heads: &'static [&'static str],
    pub commitment_heads: &'static [&'static str],
    pub decision_policy: &'static [&'static str],
    pub decision_output: &'static [&'static str],
    pub decision_output_case_sensitive: &'static [&'static str],
    pub requirement: &'static [&'static str],
    pub result_keywords: &'static [&'static str],
    pub bare_affirmation: &'static [&'static str],
    /// Markers whose presence alone is enough to call a line a Result line. Each
    /// carries result-shape on its own (PASS/FAIL outcome, score readout, P-level
    /// count, command name that only appears in result-reporting contexts).
    pub result_strict: &'static [&'static str],
    /// Markers that look result-y but appear too often in meta-discussion (e.g.
    /// "we need to write tests for X", "this throws an error: should we…").
    /// These classify a line as Result only when the line also has result shape.
    pub result_soft: &'static [&'static str],
    pub result_shape: &'static [&'static str],
    pub completion: &'static [&'static str],
    pub question: &'static [&'static str],
    pub assumption: &'static [&'static str],
    pub why: &'static [&'static str],
    pub argue: &'static [&'static str],
    pub insight: &'static [&'static str],
    pub negation_pre: &'static [&'static str],
    pub negation_post: &'static [&'static str],
}

pub fn embedded_source() -> &'static str {
    include_str!("../intent_phrases.toml")
}

pub fn phrases() -> &'static Phrases {
    let current = CELL.load(Ordering::Acquire);
    if !current.is_null() {
        return unsafe { &*current };
    }
    let built = Box::leak(Box::new(
        parse_phrases(embedded_source()).expect("embedded intent_phrases.toml parses"),
    ));
    match CELL.compare_exchange(
        std::ptr::null_mut(),
        built,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => built,
        Err(existing) => unsafe { &*existing },
    }
}

/// Replace the live phrase table. The previous table is leaked for the process
/// lifetime so in-flight `&'static` borrows stay valid.
pub fn reload_from_str(text: &str) -> Result<(), String> {
    let built = Box::leak(Box::new(parse_phrases(text)?));
    CELL.store(built, Ordering::Release);
    Ok(())
}

/// Load persisted operator phrases before classification. Invalid or unreadable
/// overrides restore embedded defaults and return a diagnostic for the caller.
pub fn load_operator_or_embedded(path: &Path) -> Result<(), String> {
    let result = match std::fs::read_to_string(path) {
        Ok(text) => reload_from_str(&text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            reload_from_str(embedded_source())
        }
        Err(err) => Err(err.to_string()),
    };
    if result.is_err() {
        reload_from_str(embedded_source()).expect("embedded intent_phrases.toml parses");
    }
    result
}

pub fn read_operator_or_embedded(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|_| embedded_source().to_string())
}

fn parse_phrases(text: &str) -> Result<Phrases, String> {
    let raw: Raw = toml::from_str(text).map_err(|err| err.to_string())?;
    Ok(Phrases {
        intent: leak(&raw.intent.keywords.ok_or("intent.keywords")?),
        task_directive: leak(&raw.task.directive),
        task_action_heads: leak(&raw.task.action_heads),
        commitment_heads: leak(&raw.task.commitment_heads),
        decision_policy: leak(&raw.decision.policy),
        decision_output: leak(&raw.decision.output),
        decision_output_case_sensitive: leak(&raw.decision.output_case_sensitive),
        requirement: leak(&raw.requirement.markers.ok_or("requirement.markers")?),
        result_keywords: leak(&raw.outcome.result_keywords),
        bare_affirmation: leak(&raw.outcome.bare_affirmation),
        result_strict: leak(&raw.outcome.result_strict),
        result_soft: leak(&raw.outcome.result_soft),
        result_shape: leak(&raw.outcome.result_shape),
        completion: leak(&raw.outcome.completion),
        question: leak(&raw.question.markers.ok_or("question.markers")?),
        assumption: leak(&raw.assumption.markers.ok_or("assumption.markers")?),
        why: leak(&raw.why.markers.ok_or("why.markers")?),
        argue: leak(&raw.argue.markers.ok_or("argue.markers")?),
        insight: leak(&raw.insight.markers.ok_or("insight.markers")?),
        negation_pre: leak(&raw.negation.pre),
        negation_post: leak(&raw.negation.post),
    })
}

static CELL: AtomicPtr<Phrases> = AtomicPtr::new(std::ptr::null_mut());

fn leak(items: &[String]) -> &'static [&'static str] {
    let leaked: Vec<&'static str> = items
        .iter()
        .map(|item| Box::leak(item.clone().into_boxed_str()) as &'static str)
        .collect();
    Box::leak(leaked.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_phrases_are_validated_before_becoming_live() {
        let edited =
            embedded_source().replacen("keywords = [", "keywords = [\"operator-only-intent\",", 1);
        assert!(
            parse_phrases(&edited)
                .unwrap()
                .intent
                .contains(&"operator-only-intent")
        );
        assert!(parse_phrases("intent = [\"broken shape\"]").is_err());
        let missing = embedded_source().replace("[intent]", "[missing_intent]");
        assert!(parse_phrases(&missing).is_err());
    }

    #[test]
    fn startup_loads_persisted_phrases_and_falls_back_on_invalid_or_missing_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "intent-phrases-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("intent_phrases.toml");
        let edited =
            embedded_source().replacen("keywords = [", "keywords = [\"operator-only-intent\",", 1);
        std::fs::write(&path, edited).unwrap();
        load_operator_or_embedded(&path).unwrap();
        assert!(phrases().intent.contains(&"operator-only-intent"));
        std::fs::write(&path, "invalid = [").unwrap();
        assert!(load_operator_or_embedded(&path).is_err());
        assert!(!phrases().intent.contains(&"operator-only-intent"));
        std::fs::remove_file(&path).unwrap();
        load_operator_or_embedded(&path).unwrap();
        assert_eq!(
            phrases().intent,
            parse_phrases(embedded_source()).unwrap().intent
        );
        std::fs::remove_dir(&root).unwrap();
    }
}
