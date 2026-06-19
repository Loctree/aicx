use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path};

pub const DEFAULT_SEARCH_QUALITY_SEED_TOML: &str =
    include_str!("../tests/retrieval_eval/search_quality_seed.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchQualityExpectation {
    InCorpus,
    OutOfCorpus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchQualityCaseType {
    Evidence,
    AskAnswer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchQualityAnchorsMatch {
    AnyOf,
    AllOf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchQualityTermsMatch {
    Any,
    All,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchQualitySeed {
    pub schema: String,
    pub corpus: String,
    #[serde(rename = "questions")]
    pub cases: Vec<SearchQualityCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchQualityCase {
    pub id: String,
    pub scope: String,
    #[serde(rename = "type")]
    pub case_type: SearchQualityCaseType,
    pub query: String,
    pub good_result: String,
    pub bad_result: String,
    #[serde(default = "default_anchors_match")]
    pub anchors_match: SearchQualityAnchorsMatch,
    #[serde(default = "default_expectation")]
    pub expectation: SearchQualityExpectation,
    pub anchors: Vec<SearchQualityAnchor>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchQualityAnchor {
    pub map_id: String,
    pub expected_terms: Vec<String>,
    #[serde(default = "default_terms_match")]
    pub terms_match: SearchQualityTermsMatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchQualityTopHit {
    pub rank: usize,
    pub evidence_score: Option<u64>,
    pub label: Option<String>,
    pub round_id: Option<String>,
    pub path: Option<String>,
    pub matched_terms: Vec<String>,
    pub matched_anchors: Vec<String>,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchQualityCaseEvaluation {
    pub id: String,
    pub scope: String,
    pub case_type: SearchQualityCaseType,
    pub query: String,
    pub projects: Vec<String>,
    pub expectation: SearchQualityExpectation,
    pub passed: bool,
    pub reason: String,
    pub matched_terms: Vec<String>,
    pub matched_anchors: Vec<String>,
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

pub fn load_search_quality_seed(path: Option<&Path>) -> Result<SearchQualitySeed> {
    let content = match path {
        Some(path) => fs::read_to_string(path)
            .with_context(|| format!("read search-quality seed {}", path.display()))?,
        None => DEFAULT_SEARCH_QUALITY_SEED_TOML.to_string(),
    };
    parse_search_quality_seed(&content)
}

pub fn parse_search_quality_seed(content: &str) -> Result<SearchQualitySeed> {
    let seed: SearchQualitySeed =
        toml::from_str(content).context("parse search-quality seed TOML")?;
    validate_search_quality_seed(&seed)?;
    Ok(seed)
}

pub fn select_search_quality_cases<'a>(
    seed: &'a SearchQualitySeed,
    ids: &[String],
) -> Result<Vec<&'a SearchQualityCase>> {
    if ids.is_empty() {
        return Ok(seed.cases.iter().collect());
    }

    let mut selected = Vec::new();
    for id in ids {
        let Some(case) = seed.cases.iter().find(|case| case.id == *id) else {
            bail!("unknown search-quality eval case: {id}");
        };
        selected.push(case);
    }
    Ok(selected)
}

pub fn discover_projects_for_cases(
    store_root: &Path,
    cases: &[&SearchQualityCase],
) -> Result<BTreeMap<String, Vec<String>>> {
    let store_dir = store_root.join("store");
    if !store_dir.is_dir() {
        bail!(
            "search-quality eval expected a canonical store at {}; set AICX_HOME to the seeded store",
            store_dir.display()
        );
    }

    let mut anchor_to_cases: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut projects_by_case: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for case in cases {
        projects_by_case.entry(case.id.clone()).or_default();
        for anchor in &case.anchors {
            anchor_to_cases
                .entry(anchor.map_id.clone())
                .or_default()
                .push(case.id.clone());
        }
    }

    scan_meta_projects(
        &store_dir,
        &store_dir,
        &anchor_to_cases,
        &mut projects_by_case,
    )?;

    Ok(projects_by_case
        .into_iter()
        .map(|(case_id, projects)| (case_id, projects.into_iter().collect()))
        .collect())
}

pub fn evaluate_evidence_payload(
    case: &SearchQualityCase,
    projects: Vec<String>,
    payload: &Value,
    top_n: usize,
) -> SearchQualityCaseEvaluation {
    let mut top_hits = Vec::new();
    let mut matched_terms = BTreeSet::new();
    let mut matched_anchors = BTreeSet::new();

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
        let identity = identity_text(item);
        let hit_matches = matched_expected_terms(&case_expected_terms(case), &searchable);
        let hit_anchor_matches = matched_anchors_for_hit(case, &identity, &searchable);
        matched_terms.extend(hit_matches.iter().cloned());
        matched_anchors.extend(hit_anchor_matches.iter().cloned());
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
            matched_anchors: hit_anchor_matches,
            excerpt: compact_excerpt(&display_text, 240),
        });
    }

    let matched_terms: Vec<String> = matched_terms.into_iter().collect();
    let matched_anchors: Vec<String> = matched_anchors.into_iter().collect();
    let supported_top_hits = top_hits
        .iter()
        .filter(|hit| hit.label.as_deref() == Some("supported"))
        .count();

    let (passed, reason) = match case.expectation {
        SearchQualityExpectation::InCorpus => {
            evaluate_in_corpus_result(case, top_hits.len(), &matched_terms, &matched_anchors)
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
        id: case.id.clone(),
        scope: case.scope.clone(),
        case_type: case.case_type,
        query: case.query.clone(),
        projects,
        expectation: case.expectation,
        passed,
        reason,
        matched_terms,
        matched_anchors,
        supported_top_hits,
        top_hits,
    }
}

pub fn command_error_evaluation(
    case: &SearchQualityCase,
    projects: Vec<String>,
    status: Option<i32>,
    stderr: &[u8],
) -> SearchQualityCaseEvaluation {
    failure_evaluation(
        case,
        projects,
        format!(
            "search command failed with status {:?}: {}",
            status,
            String::from_utf8_lossy(stderr).trim()
        ),
    )
}

pub fn invalid_json_evaluation(
    case: &SearchQualityCase,
    projects: Vec<String>,
    error: &serde_json::Error,
    stdout: &[u8],
) -> SearchQualityCaseEvaluation {
    failure_evaluation(
        case,
        projects,
        format!(
            "search command returned invalid JSON: {error}; stdout prefix: {}",
            compact_excerpt(&String::from_utf8_lossy(stdout), 360)
        ),
    )
}

pub fn project_resolution_error_evaluation(
    case: &SearchQualityCase,
    reason: String,
) -> SearchQualityCaseEvaluation {
    failure_evaluation(case, Vec::new(), reason)
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
            "- {} [{}] scope={} type={} query=\"{}\"\n  anchors: {}; terms: {}\n  good: {}\n",
            case.id,
            expectation_label(case.expectation),
            case.scope,
            case_type_label(case.case_type),
            case.query,
            case.anchors
                .iter()
                .map(|anchor| anchor.map_id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            case_expected_terms(case).join(", "),
            case.good_result
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
            "\n[{marker}] {} [{}]\nscope: {}\nproject(s): {}\nquery: {}\n{}\n",
            case.id,
            expectation_label(case.expectation),
            case.scope,
            if case.projects.is_empty() {
                "-".to_string()
            } else {
                case.projects.join(", ")
            },
            case.query,
            case.reason
        ));
        for hit in case.top_hits.iter().take(3) {
            output.push_str(&format!(
                "  #{} score={:?} label={} round={} anchors={} terms={}\n",
                hit.rank,
                hit.evidence_score,
                hit.label.as_deref().unwrap_or("-"),
                hit.round_id.as_deref().unwrap_or("-"),
                if hit.matched_anchors.is_empty() {
                    "-".to_string()
                } else {
                    hit.matched_anchors.join(", ")
                },
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

fn validate_search_quality_seed(seed: &SearchQualitySeed) -> Result<()> {
    if seed.schema.trim().is_empty() {
        bail!("search-quality seed schema cannot be empty");
    }
    if seed.cases.is_empty() {
        bail!("search-quality seed must contain at least one question");
    }

    let mut ids = BTreeSet::new();
    for case in &seed.cases {
        if case.id.trim().is_empty() {
            bail!("search-quality seed contains a case with an empty id");
        }
        if !ids.insert(case.id.clone()) {
            bail!("duplicate search-quality case id: {}", case.id);
        }
        if case.query.trim().is_empty() {
            bail!("search-quality case {} has an empty query", case.id);
        }
        if case.anchors.is_empty() && case.expectation == SearchQualityExpectation::InCorpus {
            bail!("search-quality case {} has no anchors", case.id);
        }
        for anchor in &case.anchors {
            if anchor.map_id.trim().is_empty() {
                bail!("search-quality case {} has an empty anchor map_id", case.id);
            }
            if anchor.expected_terms.is_empty()
                && case.expectation == SearchQualityExpectation::InCorpus
            {
                bail!(
                    "search-quality case {} anchor {} has no expected_terms",
                    case.id,
                    anchor.map_id
                );
            }
        }
    }
    Ok(())
}

fn scan_meta_projects(
    dir: &Path,
    store_dir: &Path,
    anchor_to_cases: &BTreeMap<String, Vec<String>>,
    projects_by_case: &mut BTreeMap<String, BTreeSet<String>>,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            scan_meta_projects(&path, store_dir, anchor_to_cases, projects_by_case)?;
            continue;
        }
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".meta.json"))
        {
            continue;
        }

        let content =
            fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let Some(project) = project_slug_from_meta_path(store_dir, &path) else {
            continue;
        };
        for (map_id, case_ids) in anchor_to_cases {
            if content.contains(map_id) {
                for case_id in case_ids {
                    projects_by_case
                        .entry(case_id.clone())
                        .or_default()
                        .insert(project.clone());
                }
            }
        }
    }
    Ok(())
}

fn project_slug_from_meta_path(store_dir: &Path, meta_path: &Path) -> Option<String> {
    let rel = meta_path.strip_prefix(store_dir).ok()?;
    let mut components = rel.components().filter_map(|component| match component {
        Component::Normal(value) => Some(value.to_string_lossy().to_string()),
        _ => None,
    });
    let owner = components.next()?;
    let repo = components.next()?;
    Some(format!("{owner}/{repo}"))
}

fn evaluate_in_corpus_result(
    case: &SearchQualityCase,
    top_hits: usize,
    matched_terms: &[String],
    matched_anchors: &[String],
) -> (bool, String) {
    if top_hits == 0 {
        return (false, "no evidence results returned".to_string());
    }

    let expected_anchors: BTreeSet<_> = case
        .anchors
        .iter()
        .map(|anchor| anchor.map_id.as_str())
        .collect();
    let matched_anchor_set: BTreeSet<_> = matched_anchors.iter().map(String::as_str).collect();
    let anchor_pass = match case.anchors_match {
        SearchQualityAnchorsMatch::AnyOf => !matched_anchor_set.is_empty(),
        SearchQualityAnchorsMatch::AllOf => expected_anchors
            .iter()
            .all(|anchor| matched_anchor_set.contains(anchor)),
    };

    if anchor_pass {
        return (
            true,
            format!(
                "matched anchored evidence in top {top_hits}: anchors={} terms={}",
                matched_anchors.join(", "),
                if matched_terms.is_empty() {
                    "-".to_string()
                } else {
                    matched_terms.join(", ")
                }
            ),
        );
    }

    if matched_terms.is_empty() {
        (
            false,
            format!(
                "no expected anchored evidence appeared in the top {top_hits} evidence results"
            ),
        )
    } else {
        (
            false,
            format!(
                "expected terms appeared in top {top_hits}, but not on the anchored session/chunk: {}",
                matched_terms.join(", ")
            ),
        )
    }
}

fn matched_anchors_for_hit(
    case: &SearchQualityCase,
    identity: &str,
    searchable: &str,
) -> Vec<String> {
    let identity = identity.to_lowercase();
    case.anchors
        .iter()
        .filter_map(|anchor| {
            if !identity.contains(&anchor.map_id.to_lowercase()) {
                return None;
            }
            let matched_terms = matched_expected_terms(&anchor.expected_terms, searchable);
            let terms_ok = match anchor.terms_match {
                SearchQualityTermsMatch::Any => !matched_terms.is_empty(),
                SearchQualityTermsMatch::All => matched_terms.len() == anchor.expected_terms.len(),
            };
            terms_ok.then(|| anchor.map_id.clone())
        })
        .collect()
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

fn identity_text(item: &Value) -> String {
    let mut parts = Vec::new();
    push_string(&mut parts, item.get("path"));
    push_string(&mut parts, item.get("id"));
    if let Some(metadata) = item.get("metadata") {
        push_string(&mut parts, metadata.get("id"));
        push_string(&mut parts, metadata.get("round_id"));
        push_string(&mut parts, metadata.get("source"));
    }
    parts.join("\n")
}

fn push_string(parts: &mut Vec<String>, value: Option<&Value>) {
    if let Some(text) = value.and_then(Value::as_str) {
        parts.push(text.to_string());
    }
}

fn matched_expected_terms(terms: &[String], searchable: &str) -> Vec<String> {
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

fn case_expected_terms(case: &SearchQualityCase) -> Vec<String> {
    let mut terms = BTreeSet::new();
    for anchor in &case.anchors {
        for term in &anchor.expected_terms {
            terms.insert(term.to_string());
        }
    }
    terms.into_iter().collect()
}

fn compact_excerpt(text: &str, max_chars: usize) -> String {
    let mut excerpt: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        excerpt.push_str("...");
    }
    excerpt
}

fn failure_evaluation(
    case: &SearchQualityCase,
    projects: Vec<String>,
    reason: String,
) -> SearchQualityCaseEvaluation {
    SearchQualityCaseEvaluation {
        id: case.id.clone(),
        scope: case.scope.clone(),
        case_type: case.case_type,
        query: case.query.clone(),
        projects,
        expectation: case.expectation,
        passed: false,
        reason,
        matched_terms: Vec::new(),
        matched_anchors: Vec::new(),
        supported_top_hits: 0,
        top_hits: Vec::new(),
    }
}

fn default_expectation() -> SearchQualityExpectation {
    SearchQualityExpectation::InCorpus
}

fn default_anchors_match() -> SearchQualityAnchorsMatch {
    SearchQualityAnchorsMatch::AnyOf
}

fn default_terms_match() -> SearchQualityTermsMatch {
    SearchQualityTermsMatch::Any
}

fn expectation_label(expectation: SearchQualityExpectation) -> &'static str {
    match expectation {
        SearchQualityExpectation::InCorpus => "in-corpus",
        SearchQualityExpectation::OutOfCorpus => "out-of-corpus",
    }
}

fn case_type_label(case_type: SearchQualityCaseType) -> &'static str {
    match case_type {
        SearchQualityCaseType::Evidence => "evidence",
        SearchQualityCaseType::AskAnswer => "ask-answer",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn default_seed_loads_curated_matrix() {
        let seed = load_search_quality_seed(None).expect("default seed loads");

        assert_eq!(seed.schema, "aicx.search_quality_seed.v1");
        assert_eq!(seed.cases.len(), 34);
        assert!(seed.cases.iter().any(|case| case.id == "aicx-all-bucket"));
    }

    #[test]
    fn seed_case_ids_are_unique() {
        let seed = load_search_quality_seed(None).expect("default seed loads");
        let ids: BTreeSet<_> = seed.cases.iter().map(|case| case.id.as_str()).collect();

        assert_eq!(ids.len(), seed.cases.len());
    }

    #[test]
    fn in_corpus_case_passes_when_anchor_and_terms_appear_in_evidence() {
        let case = fixture_case();
        let payload = json!({
            "items": [{
                "evidence_score": 89,
                "label": "supported",
                "path": "/tmp/aicx.md",
                "metadata": { "round_id": "codex__demo__2026-06-19__abc12345:round:0001" },
                "sections": {
                    "agent_answered": "Silver ma byc operatorski, a Sztudio trzyma embedding workload."
                }
            }]
        });

        let evaluation =
            evaluate_evidence_payload(&case, vec!["tb14d-anchor-v4/aicx".to_string()], &payload, 3);

        assert!(evaluation.passed, "{evaluation:#?}");
        assert_eq!(
            evaluation.matched_anchors,
            vec!["codex__demo__2026-06-19__abc12345"]
        );
    }

    #[test]
    fn in_corpus_case_fails_when_terms_appear_on_wrong_anchor() {
        let case = fixture_case();
        let payload = json!({
            "items": [{
                "evidence_score": 89,
                "label": "supported",
                "metadata": { "round_id": "codex__other__2026-06-19__abc12345:round:0001" },
                "sections": {
                    "agent_answered": "Silver ma byc operatorski, a Sztudio trzyma embedding workload."
                }
            }]
        });

        let evaluation =
            evaluate_evidence_payload(&case, vec!["tb14d-anchor-v4/aicx".to_string()], &payload, 3);

        assert!(!evaluation.passed, "{evaluation:#?}");
        assert!(evaluation.reason.contains("not on the anchored"));
    }

    #[test]
    fn out_of_corpus_case_fails_on_supported_false_positive() {
        let mut case = fixture_case();
        case.expectation = SearchQualityExpectation::OutOfCorpus;
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

        let evaluation = evaluate_evidence_payload(&case, Vec::new(), &payload, 3);

        assert!(!evaluation.passed, "{evaluation:#?}");
        assert_eq!(evaluation.supported_top_hits, 1);
    }

    #[test]
    fn selecting_unknown_case_returns_error() {
        let seed = load_search_quality_seed(None).expect("default seed loads");
        let error = select_search_quality_cases(&seed, &["missing-case".to_string()])
            .expect_err("unknown case should fail");

        assert!(error.to_string().contains("missing-case"));
    }

    #[test]
    fn discovers_projects_from_anchor_meta_sidecars() {
        let root = temp_root("project-discovery");
        let meta_dir = root
            .join("store")
            .join("tb14d-anchor-v4")
            .join("aicx")
            .join("2026_0619")
            .join("conversations")
            .join("codex");
        fs::create_dir_all(&meta_dir).expect("create meta dir");
        fs::write(
            meta_dir.join("chunk.meta.json"),
            r#"{"round_id":"codex__demo__2026-06-19__abc12345:round:0001"}"#,
        )
        .expect("write meta");
        let case = fixture_case();
        let cases = vec![&case];

        let projects = discover_projects_for_cases(&root, &cases).expect("discover projects");

        assert_eq!(
            projects.get("demo-case").cloned().unwrap_or_default(),
            vec!["tb14d-anchor-v4/aicx".to_string()]
        );

        let _ = fs::remove_dir_all(root);
    }

    fn fixture_case() -> SearchQualityCase {
        SearchQualityCase {
            id: "demo-case".to_string(),
            scope: "aicx".to_string(),
            case_type: SearchQualityCaseType::Evidence,
            query: "czemu sztudio".to_string(),
            good_result: "anchored answer".to_string(),
            bad_result: "wrong session".to_string(),
            anchors_match: SearchQualityAnchorsMatch::AnyOf,
            expectation: SearchQualityExpectation::InCorpus,
            anchors: vec![SearchQualityAnchor {
                map_id: "codex__demo__2026-06-19__abc12345".to_string(),
                expected_terms: vec!["sztudio".to_string(), "silver".to_string()],
                terms_match: SearchQualityTermsMatch::Any,
            }],
        }
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "aicx-search-quality-{label}-{}-{nanos}",
            std::process::id()
        ))
    }
}
