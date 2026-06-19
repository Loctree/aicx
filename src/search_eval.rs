use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchQualityExpectation {
    InCorpus,
    OutOfCorpus,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct SearchQualityCase {
    pub id: &'static str,
    pub project: &'static str,
    pub query: &'static str,
    pub expectation: SearchQualityExpectation,
    pub expected_terms: &'static [&'static str],
    pub notes: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchQualityTopHit {
    pub rank: usize,
    pub evidence_score: Option<u64>,
    pub label: Option<String>,
    pub round_id: Option<String>,
    pub path: Option<String>,
    pub matched_terms: Vec<String>,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchQualityCaseEvaluation {
    pub id: String,
    pub project: String,
    pub query: String,
    pub expectation: SearchQualityExpectation,
    pub passed: bool,
    pub reason: String,
    pub matched_terms: Vec<String>,
    pub supported_top_hits: usize,
    pub top_hits: Vec<SearchQualityTopHit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchQualityRunReport {
    pub mode: &'static str,
    pub store_root: String,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub cases: Vec<SearchQualityCaseEvaluation>,
}

const SEARCH_QUALITY_CASES: &[SearchQualityCase] = &[
    SearchQualityCase {
        id: "aicx_sztudio_reason",
        project: "tb14d-anchor-v4/aicx",
        query: "czemu przenieslismy embeddingi na Sztudio",
        expectation: SearchQualityExpectation::InCorpus,
        expected_terms: &["sztudio", "silver", "embedding", "_all"],
        notes: "Core AICX/Silver/Sztudio migration rationale.",
    },
    SearchQualityCase {
        id: "aicx_silver_model",
        project: "tb14d-anchor-v4/aicx",
        query: "po co Silverowi model embeddingowy",
        expectation: SearchQualityExpectation::InCorpus,
        expected_terms: &["silver", "model", "embedding", "sztudio"],
        notes: "Checks whether local operator-machine model discussion is retrievable.",
    },
    SearchQualityCase {
        id: "aicx_search_command",
        project: "tb14d-anchor-v4/aicx",
        query: "jaka komende odpalilam do searchowania embeddingow",
        expectation: SearchQualityExpectation::InCorpus,
        expected_terms: &["aicx search", "aicx_home", "tb14d", "--evidence"],
        notes: "Operational exception: command/meta content may be the answer.",
    },
    SearchQualityCase {
        id: "md_radar_marbles",
        project: "tb14d-anchor-v4/md-radar-marbles-exp",
        query: "md-radar Obczaij last marbles",
        expectation: SearchQualityExpectation::InCorpus,
        expected_terms: &["marbles", "vc-audit", "vc-polarize", "md-radar"],
        notes: "Cross-project named workflow query from the 14-day curated corpus.",
    },
    SearchQualityCase {
        id: "vista_memory_cleanup",
        project: "tb14d-anchor-v4/vista",
        query: "czemu nie konsolidujemy memory Visty",
        expectation: SearchQualityExpectation::InCorpus,
        expected_terms: &["memory", "vista", "konsolid", "aicx"],
        notes: "Vista case that is expected to exist in the 14-day curated corpus.",
    },
    SearchQualityCase {
        id: "vista_transcripts_old_history",
        project: "tb14d-anchor-v4/vista",
        query: "historia problemow z transkrypcjami w vista",
        expectation: SearchQualityExpectation::OutOfCorpus,
        expected_terms: &["transkrypc", "audio", "spaghetti"],
        notes: "Negative guard: old Vista transcript history should not be hallucinated from a 14-day corpus.",
    },
];

pub fn search_quality_seed_cases() -> &'static [SearchQualityCase] {
    SEARCH_QUALITY_CASES
}

pub fn select_search_quality_cases(ids: &[String]) -> Result<Vec<&'static SearchQualityCase>> {
    if ids.is_empty() {
        return Ok(SEARCH_QUALITY_CASES.iter().collect());
    }

    let mut selected = Vec::new();
    for id in ids {
        let Some(case) = SEARCH_QUALITY_CASES.iter().find(|case| case.id == id) else {
            bail!("unknown search-quality eval case: {id}");
        };
        selected.push(case);
    }
    Ok(selected)
}

pub fn evaluate_evidence_payload(
    case: &SearchQualityCase,
    payload: &Value,
    top_n: usize,
) -> SearchQualityCaseEvaluation {
    let mut top_hits = Vec::new();
    let mut matched_terms = BTreeSet::new();

    for (index, item) in payload
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(top_n)
        .enumerate()
    {
        let searchable = searchable_text(item);
        let display_text = display_excerpt_text(item);
        let hit_matches = matched_expected_terms(case.expected_terms, &searchable);
        matched_terms.extend(hit_matches.iter().cloned());
        top_hits.push(SearchQualityTopHit {
            rank: index + 1,
            evidence_score: item.get("evidence_score").and_then(Value::as_u64),
            label: item
                .get("label")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            round_id: item
                .get("metadata")
                .and_then(|metadata| metadata.get("round_id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            path: item
                .get("path")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            matched_terms: hit_matches,
            excerpt: compact_excerpt(&display_text, 240),
        });
    }

    let matched_terms: Vec<String> = matched_terms.into_iter().collect();
    let supported_top_hits = top_hits
        .iter()
        .filter(|hit| hit.label.as_deref() == Some("supported"))
        .count();

    let (passed, reason) = match case.expectation {
        SearchQualityExpectation::InCorpus => {
            if top_hits.is_empty() {
                (false, "no evidence results returned".to_string())
            } else if matched_terms.is_empty() {
                (
                    false,
                    format!(
                        "none of the expected terms appeared in the top {} evidence results",
                        top_hits.len()
                    ),
                )
            } else {
                (
                    true,
                    format!(
                        "matched expected terms in top {}: {}",
                        top_hits.len(),
                        matched_terms.join(", ")
                    ),
                )
            }
        }
        SearchQualityExpectation::OutOfCorpus => {
            if supported_top_hits == 0 {
                (
                    true,
                    format!(
                        "no supported evidence in top {}; acceptable out-of-corpus behavior",
                        top_hits.len()
                    ),
                )
            } else {
                (
                    false,
                    format!(
                        "top {} contains {supported_top_hits} supported result(s) for an out-of-corpus query",
                        top_hits.len()
                    ),
                )
            }
        }
    };

    SearchQualityCaseEvaluation {
        id: case.id.to_string(),
        project: case.project.to_string(),
        query: case.query.to_string(),
        expectation: case.expectation,
        passed,
        reason,
        matched_terms,
        supported_top_hits,
        top_hits,
    }
}

pub fn command_error_evaluation(
    case: &SearchQualityCase,
    status: Option<i32>,
    stderr: &[u8],
) -> SearchQualityCaseEvaluation {
    SearchQualityCaseEvaluation {
        id: case.id.to_string(),
        project: case.project.to_string(),
        query: case.query.to_string(),
        expectation: case.expectation,
        passed: false,
        reason: format!(
            "search command failed with status {:?}: {}",
            status,
            String::from_utf8_lossy(stderr).trim()
        ),
        matched_terms: Vec::new(),
        supported_top_hits: 0,
        top_hits: Vec::new(),
    }
}

pub fn invalid_json_evaluation(
    case: &SearchQualityCase,
    error: &serde_json::Error,
    stdout: &[u8],
) -> SearchQualityCaseEvaluation {
    SearchQualityCaseEvaluation {
        id: case.id.to_string(),
        project: case.project.to_string(),
        query: case.query.to_string(),
        expectation: case.expectation,
        passed: false,
        reason: format!(
            "search command returned invalid JSON: {error}; stdout prefix: {}",
            compact_excerpt(&String::from_utf8_lossy(stdout), 360)
        ),
        matched_terms: Vec::new(),
        supported_top_hits: 0,
        top_hits: Vec::new(),
    }
}

pub fn build_run_report(
    store_root: String,
    cases: Vec<SearchQualityCaseEvaluation>,
) -> SearchQualityRunReport {
    let passed = cases.iter().filter(|case| case.passed).count();
    let total = cases.len();
    SearchQualityRunReport {
        mode: "search_quality",
        store_root,
        total,
        passed,
        failed: total.saturating_sub(passed),
        cases,
    }
}

pub fn render_seed_cases_text(cases: &[&SearchQualityCase]) -> String {
    let mut output = String::new();
    output.push_str("Search quality seed matrix:\n");
    for case in cases {
        output.push_str(&format!(
            "- {} [{}] project={} query=\"{}\"\n  expects: {}; terms: {}\n  note: {}\n",
            case.id,
            expectation_label(case.expectation),
            case.project,
            case.query,
            expectation_label(case.expectation),
            case.expected_terms.join(", "),
            case.notes
        ));
    }
    output
}

pub fn render_run_report_text(report: &SearchQualityRunReport) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "Search quality eval: {}/{} passed (store: {})\n",
        report.passed, report.total, report.store_root
    ));

    for case in &report.cases {
        let marker = if case.passed { "PASS" } else { "FAIL" };
        output.push_str(&format!(
            "\n[{marker}] {} [{}]\nproject: {}\nquery: {}\n{}\n",
            case.id,
            expectation_label(case.expectation),
            case.project,
            case.query,
            case.reason
        ));
        for hit in case.top_hits.iter().take(3) {
            output.push_str(&format!(
                "  #{} score={:?} label={} round={} terms={}\n",
                hit.rank,
                hit.evidence_score,
                hit.label.as_deref().unwrap_or("-"),
                hit.round_id.as_deref().unwrap_or("-"),
                if hit.matched_terms.is_empty() {
                    "-".to_string()
                } else {
                    hit.matched_terms.join(", ")
                }
            ));
        }
    }
    output
}

fn searchable_text(item: &Value) -> String {
    let mut parts = Vec::new();
    push_string(&mut parts, item.get("excerpt"));

    if let Some(sections) = item.get("sections") {
        push_string(&mut parts, sections.get("user_intent"));
        push_string(&mut parts, sections.get("agent_answered"));
        push_string(&mut parts, sections.get("evidence"));
        push_string(&mut parts, sections.get("full_text"));
    }

    if let Some(matches) = item.get("matches").and_then(Value::as_array) {
        for search_match in matches {
            push_string(&mut parts, search_match.get("text"));
            push_string(&mut parts, search_match.get("excerpt"));
        }
    }

    parts.join("\n").to_lowercase()
}

fn display_excerpt_text(item: &Value) -> String {
    let mut parts = Vec::new();
    push_string(&mut parts, item.get("excerpt"));

    if let Some(sections) = item.get("sections") {
        push_string(&mut parts, sections.get("user_intent"));
        push_string(&mut parts, sections.get("agent_answered"));
        push_string(&mut parts, sections.get("evidence"));
        push_string(&mut parts, sections.get("full_text"));
    }

    if let Some(matches) = item.get("matches").and_then(Value::as_array) {
        for search_match in matches {
            push_string(&mut parts, search_match.get("text"));
            push_string(&mut parts, search_match.get("excerpt"));
        }
    }

    if parts.is_empty() {
        push_string(&mut parts, item.get("path"));
    }

    parts.join("\n")
}

fn push_string(parts: &mut Vec<String>, value: Option<&Value>) {
    if let Some(text) = value.and_then(Value::as_str) {
        parts.push(text.to_string());
    }
}

fn matched_expected_terms(terms: &[&str], searchable: &str) -> Vec<String> {
    terms
        .iter()
        .filter_map(|term| {
            let normalized = term.to_lowercase();
            searchable
                .contains(&normalized)
                .then(|| normalized.to_string())
        })
        .collect()
}

fn compact_excerpt(text: &str, max_chars: usize) -> String {
    let mut excerpt: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        excerpt.push_str("...");
    }
    excerpt
}

fn expectation_label(expectation: SearchQualityExpectation) -> &'static str {
    match expectation {
        SearchQualityExpectation::InCorpus => "in-corpus",
        SearchQualityExpectation::OutOfCorpus => "out-of-corpus",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeSet;

    #[test]
    fn seed_case_ids_are_unique() {
        let ids: BTreeSet<_> = search_quality_seed_cases()
            .iter()
            .map(|case| case.id)
            .collect();

        assert_eq!(ids.len(), search_quality_seed_cases().len());
    }

    #[test]
    fn in_corpus_case_passes_when_expected_terms_appear_in_evidence() {
        let case = search_quality_seed_cases()
            .iter()
            .find(|case| case.id == "aicx_sztudio_reason")
            .expect("seed case");
        let payload = json!({
            "items": [{
                "evidence_score": 89,
                "label": "supported",
                "path": "/tmp/aicx.md",
                "metadata": { "round_id": "round-1" },
                "sections": {
                    "user_intent": "czemu przenieslismy embeddingi na Sztudio",
                    "agent_answered": "Silver ma byc operatorski, a Sztudio trzyma embedding workload oraz _all."
                }
            }]
        });

        let evaluation = evaluate_evidence_payload(case, &payload, 3);

        assert!(evaluation.passed, "{evaluation:#?}");
        assert!(
            evaluation
                .matched_terms
                .iter()
                .any(|term| term == "sztudio")
        );
    }

    #[test]
    fn out_of_corpus_case_fails_on_supported_false_positive() {
        let case = search_quality_seed_cases()
            .iter()
            .find(|case| case.id == "vista_transcripts_old_history")
            .expect("seed case");
        let payload = json!({
            "items": [{
                "evidence_score": 91,
                "label": "supported",
                "metadata": { "round_id": "round-2" },
                "sections": {
                    "agent_answered": "To jest tylko cleanup memory Visty, bez historii transkrypcji."
                }
            }]
        });

        let evaluation = evaluate_evidence_payload(case, &payload, 3);

        assert!(!evaluation.passed, "{evaluation:#?}");
        assert_eq!(evaluation.supported_top_hits, 1);
    }

    #[test]
    fn selecting_unknown_case_returns_error() {
        let error = select_search_quality_cases(&["missing-case".to_string()])
            .expect_err("unknown case should fail");

        assert!(error.to_string().contains("missing-case"));
    }
}
