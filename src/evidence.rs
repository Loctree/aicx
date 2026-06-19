//! Evidence packet scoring and rendering for search results.
//!
//! This module intentionally sits after semantic/hybrid retrieval. It does
//! not synthesize answers and it does not replace the retrieval engine; it
//! re-ranks a bounded candidate pool into operator-readable evidence.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;

use crate::oracle::OracleStatus;
use crate::rank::FuzzyResult;
use crate::sanitize::{normalize_query, read_to_string_validated};
use crate::store;

const EVIDENCE_SECTION_MAX_CHARS: usize = 900;
const EVIDENCE_MATCH_MAX_CHARS: usize = 220;
const SUPPRESSED_LIMIT: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceQueryClass {
    Explanation,
    Incident,
    Operational,
    Generic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLabel {
    Supported,
    Weak,
    Meta,
    Dump,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EvidenceMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_part_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_part_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_anchor_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_evidence_chars: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct EvidenceSections {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_evidence_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_answered: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceItem {
    pub evidence_score: u8,
    pub base_score: u8,
    pub label: EvidenceLabel,
    pub reasons: Vec<String>,
    pub project: String,
    pub kind: String,
    pub agent: String,
    pub date: String,
    pub timestamp: Option<String>,
    pub frame_kind: Option<String>,
    pub session: String,
    pub session_id: String,
    pub cwd: String,
    pub matches: Vec<String>,
    pub path: String,
    pub metadata: EvidenceMetadata,
    pub sections: EvidenceSections,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceSuppressed {
    pub path: String,
    pub label: EvidenceLabel,
    pub evidence_score: u8,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceReport {
    pub mode: &'static str,
    pub query: String,
    pub query_class: EvidenceQueryClass,
    pub candidates_examined: usize,
    pub results: usize,
    pub items: Vec<EvidenceItem>,
    pub suppressed: Vec<EvidenceSuppressed>,
}

#[derive(Debug, Serialize)]
struct EvidenceJsonResponse<'a> {
    mode: &'static str,
    query: &'a str,
    query_class: EvidenceQueryClass,
    oracle_status: OracleStatus,
    results: usize,
    candidates_examined: usize,
    scanned: usize,
    items: &'a [EvidenceItem],
    suppressed: &'a [EvidenceSuppressed],
}

#[derive(Debug)]
struct EvidenceCandidate {
    item: EvidenceItem,
    duplicate_key: String,
}

pub fn classify_evidence_query(query: &str) -> EvidenceQueryClass {
    let normalized = normalize_query(query);
    let has_any = |needles: &[&str]| needles.iter().any(|needle| normalized.contains(needle));

    if has_any(&[
        "czemu",
        "dlaczego",
        "po co",
        "czymu",
        "why",
        "reason",
        "powod",
        "przyczyna",
    ]) {
        return EvidenceQueryClass::Explanation;
    }

    if has_any(&[
        "o co chodzi",
        "co sie stalo",
        "co bylo",
        "historia",
        "problemow",
        "incydent",
        "root cause",
        "jak doszlo",
    ]) {
        return EvidenceQueryClass::Incident;
    }

    if has_any(&[
        "komende",
        "komenda",
        "jak searchowac",
        "jak uzyc",
        "status",
        "czy dziala",
        "ile miejsca",
        "jak tam",
        "co teraz",
        "next step",
        "smoke",
    ]) {
        return EvidenceQueryClass::Operational;
    }

    EvidenceQueryClass::Generic
}

pub fn build_evidence_report(
    query: &str,
    candidates: Vec<FuzzyResult>,
    limit: usize,
) -> EvidenceReport {
    let query_class = classify_evidence_query(query);
    let query_terms = evidence_query_terms(query);
    let candidates_examined = candidates.len();
    let mut scored = candidates
        .into_iter()
        .map(|result| score_candidate(query_class, &query_terms, result))
        .collect::<Vec<_>>();

    scored.sort_by(|a, b| {
        b.item
            .evidence_score
            .cmp(&a.item.evidence_score)
            .then_with(|| b.item.base_score.cmp(&a.item.base_score))
            .then_with(|| b.item.date.cmp(&a.item.date))
    });

    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut suppressed = Vec::new();
    let limit = limit.max(1);

    for candidate in scored {
        if !seen.insert(candidate.duplicate_key.clone()) {
            push_suppressed(
                &mut suppressed,
                &candidate.item,
                "dedupe_round_or_path_best_result_kept".to_string(),
            );
            continue;
        }

        if items.len() < limit {
            items.push(candidate.item);
        } else {
            push_suppressed(
                &mut suppressed,
                &candidate.item,
                "outside_evidence_limit_not_label_diagnostic".to_string(),
            );
        }
    }

    EvidenceReport {
        mode: "evidence",
        query: query.to_string(),
        query_class,
        candidates_examined,
        results: items.len(),
        items,
        suppressed,
    }
}

pub fn render_evidence_json(
    report: &EvidenceReport,
    scanned: usize,
    oracle_status: OracleStatus,
) -> serde_json::Result<String> {
    serde_json::to_string(&EvidenceJsonResponse {
        mode: report.mode,
        query: &report.query,
        query_class: report.query_class,
        oracle_status,
        results: report.results,
        candidates_examined: report.candidates_examined,
        scanned,
        items: &report.items,
        suppressed: &report.suppressed,
    })
}

pub fn render_evidence_text(report: &EvidenceReport, color: bool) -> String {
    let mut out = String::new();

    for item in &report.items {
        let label = evidence_label_str(item.label);
        if color {
            let score_color = match item.label {
                EvidenceLabel::Supported => "\x1b[1;32m",
                EvidenceLabel::Weak => "\x1b[1;33m",
                EvidenceLabel::Meta => "\x1b[1;35m",
                EvidenceLabel::Dump => "\x1b[1;31m",
            };
            let _ = writeln!(
                out,
                "{score_color}[{}/100 evidence:{} base:{}]\x1b[0m \x1b[1;36m{}\x1b[0m | \x1b[35m{}\x1b[0m | \x1b[90m{}\x1b[0m",
                item.evidence_score, label, item.base_score, item.project, item.agent, item.date
            );
            render_evidence_item_body(&mut out, item, true);
        } else {
            let _ = writeln!(
                out,
                "[{}/100 evidence:{} base:{}] {} | {} | {}",
                item.evidence_score, label, item.base_score, item.project, item.agent, item.date
            );
            render_evidence_item_body(&mut out, item, false);
        }
        let _ = writeln!(out);
    }

    if !report.suppressed.is_empty() {
        if color {
            let _ = writeln!(
                out,
                "\x1b[90msuppressed: {} (dedupe/limit only; not a full label diagnostic)\x1b[0m",
                report.suppressed.len()
            );
        } else {
            let _ = writeln!(
                out,
                "suppressed: {} (dedupe/limit only; not a full label diagnostic)",
                report.suppressed.len()
            );
        }
    }

    out
}

pub fn evidence_source_paths(report: &EvidenceReport) -> impl Iterator<Item = &Path> {
    report
        .items
        .iter()
        .map(|item| Path::new(item.path.as_str()))
}

fn render_evidence_item_body(out: &mut String, item: &EvidenceItem, color: bool) {
    let session_str = item.session_id.as_str();
    let cwd_str = item.cwd.as_str();
    let frame_str = item.frame_kind.as_deref().unwrap_or("-");
    let reason = item.reasons.join("; ");
    let round = item
        .metadata
        .round_id
        .as_deref()
        .map(|round_id| {
            let part = match (
                item.metadata.round_part_index,
                item.metadata.round_part_count,
            ) {
                (Some(idx), Some(count)) if count > 1 => format!(" part {idx}/{count}"),
                _ => String::new(),
            };
            format!("{round_id}{part}")
        })
        .unwrap_or_else(|| "-".to_string());
    let anchor = item.metadata.user_anchor_kind.as_deref().unwrap_or("-");

    if color {
        let _ = writeln!(out, "session(s): \x1b[90m{session_str}\x1b[0m");
        let _ = writeln!(out, "cwd: \x1b[90m{cwd_str}\x1b[0m");
        let _ = writeln!(out, "frame_kind: \x1b[90m{frame_str}\x1b[0m");
        let _ = writeln!(out, "round: \x1b[90m{round}\x1b[0m");
        let _ = writeln!(out, "anchor: \x1b[90m{anchor}\x1b[0m");
        let _ = writeln!(out, "reason: \x1b[90m{reason}\x1b[0m");
        render_named_section(
            out,
            "user intent",
            item.sections.user_intent.as_deref(),
            true,
        );
        render_named_section(
            out,
            "user evidence",
            item.sections.user_evidence_excerpt.as_deref(),
            true,
        );
        render_named_section(
            out,
            "agent answered",
            item.sections.agent_answered.as_deref(),
            true,
        );
        if item.sections.agent_answered.is_none() && !item.matches.is_empty() {
            let _ = writeln!(out, "matches:");
            for line in &item.matches {
                let _ = writeln!(out, "  \x1b[90m>\x1b[0m \x1b[90m{}\x1b[0m", line);
            }
        }
        let _ = writeln!(out, "source file(s):");
        let _ = writeln!(out, "\x1b[90;4m{}\x1b[0m", item.path);
    } else {
        let _ = writeln!(out, "session(s): {session_str}");
        let _ = writeln!(out, "cwd: {cwd_str}");
        let _ = writeln!(out, "frame_kind: {frame_str}");
        let _ = writeln!(out, "round: {round}");
        let _ = writeln!(out, "anchor: {anchor}");
        let _ = writeln!(out, "reason: {reason}");
        render_named_section(
            out,
            "user intent",
            item.sections.user_intent.as_deref(),
            false,
        );
        render_named_section(
            out,
            "user evidence",
            item.sections.user_evidence_excerpt.as_deref(),
            false,
        );
        render_named_section(
            out,
            "agent answered",
            item.sections.agent_answered.as_deref(),
            false,
        );
        if item.sections.agent_answered.is_none() && !item.matches.is_empty() {
            let _ = writeln!(out, "matches:");
            for line in &item.matches {
                let _ = writeln!(out, "  > {}", line);
            }
        }
        let _ = writeln!(out, "source file(s):");
        let _ = writeln!(out, "{}", item.path);
    }
}

fn render_named_section(out: &mut String, name: &str, value: Option<&str>, color: bool) {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return;
    };
    let _ = writeln!(out, "{name}:");
    let line = truncate_compact(value, EVIDENCE_MATCH_MAX_CHARS);
    if color {
        let _ = writeln!(out, "  \x1b[90m>\x1b[0m \x1b[90m{}\x1b[0m", line);
    } else {
        let _ = writeln!(out, "  > {}", line);
    }
}

fn score_candidate(
    query_class: EvidenceQueryClass,
    query_terms: &[String],
    result: FuzzyResult,
) -> EvidenceCandidate {
    let body = load_result_body(&result);
    let sections = body
        .as_deref()
        .map(parse_evidence_sections)
        .unwrap_or_default();
    let metadata = load_result_metadata(&result);
    let matches = clean_matches(&result);

    let answer_norm = normalize_query(sections.agent_answered.as_deref().unwrap_or_default());
    let intent_norm = normalize_query(sections.user_intent.as_deref().unwrap_or_default());
    let evidence_norm = normalize_query(
        sections
            .user_evidence_excerpt
            .as_deref()
            .unwrap_or_default(),
    );
    let body_norm = normalize_query(body.as_deref().unwrap_or_default());
    let matches_norm = normalize_query(&matches.join(" "));
    let mut score = 10i16 + (result.score as i16 * 30 / 100);
    let mut reasons = Vec::new();
    let expects_answer = matches!(
        query_class,
        EvidenceQueryClass::Explanation | EvidenceQueryClass::Incident
    );

    if metadata.artifact_family.as_deref() == Some("tb-spotlight-rounds") {
        score += 4;
        reasons.push("tb_spotlight_round".to_string());
    }

    if sections.user_intent.is_some() {
        score += 4;
        reasons.push("has_user_intent".to_string());
    }

    if sections.agent_answered.is_some() {
        score += 8;
        reasons.push("has_agent_answer".to_string());
    } else {
        score -= 12;
        reasons.push("no_agent_answer_section".to_string());
    }

    match metadata.user_anchor_kind.as_deref() {
        Some("request") => {
            score += 5;
            reasons.push("request_anchor".to_string());
        }
        Some("mixed") => {
            score += 2;
            reasons.push("mixed_anchor".to_string());
        }
        Some("evidence_dump") if expects_answer => {
            score -= 18;
            reasons.push("evidence_dump_anchor_for_answer_query".to_string());
        }
        Some("evidence_dump") => {
            score -= 2;
            reasons.push("evidence_dump_anchor".to_string());
        }
        Some(other) => reasons.push(format!("anchor:{other}")),
        None => {}
    }

    let answer_overlap = overlap_count(query_terms, &answer_norm);
    let intent_overlap = overlap_count(query_terms, &intent_norm);
    let evidence_overlap = overlap_count(query_terms, &evidence_norm);
    let total_overlap =
        overlap_count(query_terms, &body_norm).max(overlap_count(query_terms, &matches_norm));

    if answer_overlap >= 2 {
        score += 14;
        reasons.push(format!("answer_overlap:{answer_overlap}"));
    } else if answer_overlap == 1 {
        score += 6;
        reasons.push("answer_overlap:1".to_string());
    }

    if total_overlap == 0 && !query_terms.is_empty() {
        score -= 18;
        reasons.push("no_query_term_overlap".to_string());
    }

    if expects_answer && answer_overlap == 0 && (intent_overlap + evidence_overlap) > 0 {
        score -= 10;
        reasons.push("query_matches_prompt_more_than_answer".to_string());
    }

    let answer_has_marker = has_answer_marker(&answer_norm);
    if expects_answer && answer_has_marker {
        score += 14;
        reasons.push("decision_or_reason_marker".to_string());
    }
    let answer_supports_query = answer_has_marker || answer_overlap > 0;

    let meta_noise =
        has_operational_meta_noise(&body_norm) || has_operational_meta_noise(&matches_norm);
    if meta_noise && expects_answer && !answer_has_marker {
        score -= 28;
        reasons.push("meta_search_or_status_round".to_string());
    } else if meta_noise && query_class == EvidenceQueryClass::Operational {
        score += 12;
        reasons.push("operational_trace_matches_query".to_string());
    }

    if sections.user_intent.is_none() && sections.agent_answered.is_none() && matches.len() <= 1 {
        score -= 10;
        reasons.push("thin_evidence_surface".to_string());
    }

    let raw_score = score.clamp(0, 100) as u8;
    let label = evidence_label(
        raw_score,
        expects_answer,
        meta_noise,
        answer_supports_query,
        metadata.user_anchor_kind.as_deref(),
    );
    let evidence_score = capped_evidence_score(raw_score, label, expects_answer, answer_has_marker);
    let duplicate_key = metadata
        .round_id
        .clone()
        .filter(|round_id| !round_id.trim().is_empty())
        .unwrap_or_else(|| result.path.clone());

    EvidenceCandidate {
        item: EvidenceItem {
            evidence_score,
            base_score: result.score,
            label,
            reasons,
            project: result.project,
            kind: result.kind,
            agent: result.agent,
            date: result.date,
            timestamp: result.timestamp,
            frame_kind: result.frame_kind,
            session: result.session_id.clone().unwrap_or_else(|| "-".to_string()),
            session_id: result.session_id.unwrap_or_else(|| "-".to_string()),
            cwd: result.cwd.unwrap_or_else(|| "-".to_string()),
            matches,
            path: result.path,
            metadata,
            sections,
        },
        duplicate_key,
    }
}

fn evidence_label(
    score: u8,
    expects_answer: bool,
    meta_noise: bool,
    answer_supports_query: bool,
    anchor_kind: Option<&str>,
) -> EvidenceLabel {
    if expects_answer && anchor_kind == Some("evidence_dump") && score < 70 {
        return EvidenceLabel::Dump;
    }
    if expects_answer && meta_noise && !answer_supports_query {
        return EvidenceLabel::Meta;
    }
    if score >= 78 {
        EvidenceLabel::Supported
    } else {
        EvidenceLabel::Weak
    }
}

fn capped_evidence_score(
    score: u8,
    label: EvidenceLabel,
    expects_answer: bool,
    answer_has_marker: bool,
) -> u8 {
    match label {
        EvidenceLabel::Supported if expects_answer && !answer_has_marker => score.min(88),
        EvidenceLabel::Supported => score,
        EvidenceLabel::Weak => score.min(77),
        EvidenceLabel::Meta => score.min(69),
        EvidenceLabel::Dump => score.min(59),
    }
}

fn load_result_body(result: &FuzzyResult) -> Option<String> {
    read_to_string_validated(Path::new(&result.path)).ok()
}

fn load_result_metadata(result: &FuzzyResult) -> EvidenceMetadata {
    let sidecar_path = store::sidecar_path_for_chunk(Path::new(&result.path));
    let Ok(content) = read_to_string_validated(&sidecar_path) else {
        return EvidenceMetadata::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return EvidenceMetadata::default();
    };

    EvidenceMetadata {
        artifact_family: json_string(&value, "artifact_family"),
        schema_version: json_string(&value, "schema_version"),
        round_id: json_string(&value, "round_id"),
        round_index: json_u64(&value, "round_index"),
        round_part_index: json_u64(&value, "round_part_index"),
        round_part_count: json_u64(&value, "round_part_count"),
        round_status: json_string(&value, "round_status"),
        user_anchor_kind: json_string(&value, "user_anchor_kind"),
        user_evidence_chars: json_u64(&value, "user_evidence_chars"),
    }
}

fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn json_u64(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|value| value.as_u64())
}

#[derive(Clone, Copy)]
enum SectionKind {
    UserIntent,
    UserEvidence,
    AgentAnswered,
}

fn parse_evidence_sections(body: &str) -> EvidenceSections {
    let mut current = None;
    let mut user_intent = String::new();
    let mut user_evidence = String::new();
    let mut agent_answered = String::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if is_source_metadata_line(trimmed) {
            continue;
        }
        if let Some(section) = section_label(trimmed) {
            current = Some(section);
            continue;
        }

        match current {
            Some(SectionKind::UserIntent) => push_section_line(&mut user_intent, line),
            Some(SectionKind::UserEvidence) => push_section_line(&mut user_evidence, line),
            Some(SectionKind::AgentAnswered) => push_section_line(&mut agent_answered, line),
            None => {}
        }
    }

    EvidenceSections {
        user_intent: section_value(user_intent),
        user_evidence_excerpt: section_value(user_evidence),
        agent_answered: section_value(agent_answered),
    }
}

fn section_label(trimmed: &str) -> Option<SectionKind> {
    let lower = trimmed.to_ascii_lowercase();
    match lower.as_str() {
        "user intent:" => Some(SectionKind::UserIntent),
        "user provided evidence:" | "user evidence excerpt:" => Some(SectionKind::UserEvidence),
        _ if lower.starts_with("agent answered") && lower.ends_with(':') => {
            Some(SectionKind::AgentAnswered)
        }
        _ => None,
    }
}

fn push_section_line(target: &mut String, line: &str) {
    if target.len() >= EVIDENCE_SECTION_MAX_CHARS {
        return;
    }
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(line.trim());
}

fn section_value(value: String) -> Option<String> {
    let compact = compact_whitespace(&value);
    (!compact.is_empty()).then(|| truncate_compact(&compact, EVIDENCE_SECTION_MAX_CHARS))
}

fn clean_matches(result: &FuzzyResult) -> Vec<String> {
    let mut lines = result
        .matched_lines
        .iter()
        .filter(|line| !is_source_metadata_line(line.trim()))
        .map(|line| truncate_compact(line, EVIDENCE_MATCH_MAX_CHARS))
        .collect::<Vec<_>>();

    if lines.is_empty() {
        lines = result
            .matched_lines
            .iter()
            .map(|line| truncate_compact(line, EVIDENCE_MATCH_MAX_CHARS))
            .collect();
    }

    lines
}

fn is_source_metadata_line(trimmed: &str) -> bool {
    let lower = trimmed.to_ascii_lowercase();
    trimmed.starts_with("[project:")
        || trimmed.starts_with("[metadata]")
        || lower.starts_with("[frame_kind:")
        || lower.starts_with("source project:")
        || lower.starts_with("canonical test project:")
        || lower.starts_with("tb artifact:")
        || lower.starts_with("round status:")
}

fn truncate_compact(text: &str, max_chars: usize) -> String {
    let compact = compact_whitespace(text);
    let char_count = compact.chars().count();
    if char_count <= max_chars {
        return compact;
    }
    let mut truncated = compact.chars().take(max_chars).collect::<String>();
    truncated.push_str(" ...");
    truncated
}

fn compact_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn evidence_query_terms(query: &str) -> Vec<String> {
    let normalized = normalize_query(query);
    let mut seen = HashSet::new();
    normalized
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
        .map(|term| term.trim_matches('-'))
        .filter(|term| term.len() >= 3)
        .filter(|term| !is_query_stopword(term))
        .filter_map(|term| {
            let term = term.to_string();
            seen.insert(term.clone()).then_some(term)
        })
        .collect()
}

fn is_query_stopword(term: &str) -> bool {
    matches!(
        term,
        "czy"
            | "dla"
            | "jak"
            | "jaki"
            | "jakie"
            | "jest"
            | "mamy"
            | "nie"
            | "oraz"
            | "pod"
            | "sie"
            | "ten"
            | "tez"
            | "tym"
            | "the"
            | "and"
            | "for"
            | "with"
            | "why"
            | "what"
            | "when"
            | "where"
            | "czemu"
            | "dlaczego"
            | "historia"
            | "problemow"
    )
}

fn overlap_count(terms: &[String], text: &str) -> usize {
    terms
        .iter()
        .filter(|term| text.contains(term.as_str()))
        .count()
}

fn has_answer_marker(text: &str) -> bool {
    [
        "bo ",
        "dlatego",
        "poniewaz",
        "przyczyna",
        "powod",
        "decyz",
        "ustalen",
        "wniosek",
        "root cause",
        "because",
        "reason",
        "decision",
        "conclusion",
        "diagnosis",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn has_operational_meta_noise(text: &str) -> bool {
    [
        "aicx search",
        "--no-semantic",
        "oracle_status",
        "backend=",
        "hybrid_rrf",
        "dense_only",
        "daj mi komende",
        "komende",
        "jak searchowac",
        "puszczam",
        "odpal",
        "smoke",
        "testowo",
        "sprawdzam",
        "stopka",
        "candidate chunks",
        "result(s) from",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn evidence_label_str(label: EvidenceLabel) -> &'static str {
    match label {
        EvidenceLabel::Supported => "supported",
        EvidenceLabel::Weak => "weak",
        EvidenceLabel::Meta => "meta",
        EvidenceLabel::Dump => "dump",
    }
}

fn push_suppressed(suppressed: &mut Vec<EvidenceSuppressed>, item: &EvidenceItem, reason: String) {
    if suppressed.len() >= SUPPRESSED_LIMIT {
        return;
    }
    suppressed.push(EvidenceSuppressed {
        path: item.path.clone(),
        label: item.label,
        evidence_score: item.evidence_score,
        reason,
        round_id: item.metadata.round_id.clone(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_result(path: &Path, score: u8, matched_lines: Vec<&str>) -> FuzzyResult {
        FuzzyResult {
            file: path.file_name().unwrap().to_string_lossy().to_string(),
            path: path.display().to_string(),
            project: "tb14d-anchor-v4/aicx".to_string(),
            kind: "conversations".to_string(),
            frame_kind: Some("agent_reply".to_string()),
            agent: "codex".to_string(),
            date: "2026-06-19".to_string(),
            timestamp: None,
            score,
            label: "hybrid_rrf".to_string(),
            density: 0.8,
            matched_lines: matched_lines.into_iter().map(str::to_string).collect(),
            session_id: Some("sess".to_string()),
            cwd: Some("/repo".to_string()),
        }
    }

    fn write_fixture(name: &str, body: &str, meta: serde_json::Value) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("aicx-evidence-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        let path = dir.join("chunk.md");
        std::fs::write(&path, body).expect("write body");
        std::fs::write(
            path.with_extension("meta.json"),
            serde_json::to_string(&meta).expect("serialize meta"),
        )
        .expect("write meta");
        path
    }

    #[test]
    fn evidence_query_classifier_detects_explanation() {
        assert_eq!(
            classify_evidence_query("czemu przenieslismy embeddingi na Sztudio"),
            EvidenceQueryClass::Explanation
        );
        assert_eq!(
            classify_evidence_query("daj mi komende jak searchowac"),
            EvidenceQueryClass::Operational
        );
    }

    #[test]
    fn evidence_rerank_prefers_reasoned_answer_over_meta_search_round() {
        let supported_path = write_fixture(
            "supported",
            "[project: x]\nUser intent:\nczemu przenieslismy embeddingi na Sztudio?\n\nAgent answered:\nPrzenieslismy embeddingi na Sztudio, bo indeksowanie store jest ciezkim workloadem. Decyzja: Silver zostaje maszyna operatorska, a Sztudio wykonawcza.\n",
            serde_json::json!({
                "artifact_family": "tb-spotlight-rounds",
                "round_id": "round-supported",
                "round_part_index": 1,
                "round_part_count": 1,
                "user_anchor_kind": "request"
            }),
        );
        let meta_path = write_fixture(
            "meta",
            "[project: x]\nUser intent:\ndaj mi komende jak searchowac\n\nAgent answered:\nAICX_HOME=/tmp/aicx aicx search -p aicx \"status\". Stopka pokaze oracle_status backend=hybrid_rrf.\n",
            serde_json::json!({
                "artifact_family": "tb-spotlight-rounds",
                "round_id": "round-meta",
                "round_part_index": 1,
                "round_part_count": 1,
                "user_anchor_kind": "request"
            }),
        );

        let report = build_evidence_report(
            "czemu przenieslismy embeddingi na Sztudio",
            vec![
                fake_result(&meta_path, 99, vec!["aicx search embeddingi Sztudio"]),
                fake_result(&supported_path, 78, vec!["Decyzja: Sztudio wykonawcza"]),
            ],
            2,
        );

        assert_eq!(
            report.items[0].metadata.round_id.as_deref(),
            Some("round-supported")
        );
        assert_eq!(report.items[0].label, EvidenceLabel::Supported);
        assert_eq!(report.items[1].label, EvidenceLabel::Meta);

        let _ = std::fs::remove_dir_all(supported_path.parent().unwrap());
        let _ = std::fs::remove_dir_all(meta_path.parent().unwrap());
    }

    #[test]
    fn evidence_does_not_label_answer_overlap_with_search_trace_as_meta() {
        let path = write_fixture(
            "overlap-with-trace",
            "[project: x]\nUser intent:\nczemu przenieslismy embeddingi na Sztudio?\n\nAgent answered:\nAICX_HOME=/tmp/aicx aicx search -p aicx \"embeddingi Sztudio\". Smoke pokazal backend=hybrid_rrf.\n",
            serde_json::json!({
                "artifact_family": "tb-spotlight-rounds",
                "round_id": "round-overlap-with-trace",
                "round_part_index": 1,
                "round_part_count": 1,
                "user_anchor_kind": "request"
            }),
        );

        let report = build_evidence_report(
            "czemu przenieslismy embeddingi na Sztudio",
            vec![fake_result(&path, 92, vec!["backend=hybrid_rrf"])],
            1,
        );

        assert_ne!(report.items[0].label, EvidenceLabel::Meta);
        assert_eq!(report.items[0].label, EvidenceLabel::Weak);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn evidence_does_not_label_decision_with_search_trace_as_meta() {
        let path = write_fixture(
            "decision-with-trace",
            "[project: x]\nUser intent:\nczemu przenieslismy embeddingi na Sztudio?\n\nAgent answered:\nDecyzja: przenieslismy embeddingi na Sztudio, bo indeksowanie store jest ciezkim workloadem. Smoke przez aicx search pokazal backend=hybrid_rrf, wiec trace jest dowodem wykonania, a nie meta-only odpowiedzia.\n",
            serde_json::json!({
                "artifact_family": "tb-spotlight-rounds",
                "round_id": "round-decision-with-trace",
                "round_part_index": 1,
                "round_part_count": 1,
                "user_anchor_kind": "request"
            }),
        );

        let report = build_evidence_report(
            "czemu przenieslismy embeddingi na Sztudio",
            vec![fake_result(&path, 92, vec!["backend=hybrid_rrf"])],
            1,
        );

        assert_eq!(report.items[0].label, EvidenceLabel::Supported);
        assert!(
            report.items[0].evidence_score > 69,
            "decision evidence with trace should not be capped as meta"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn evidence_labels_dump_anchor_for_answer_query() {
        let path = write_fixture(
            "dump",
            "[project: x]\nUser provided evidence:\n[terminal/log/context dump; no direct request text extracted]\n\nUser evidence excerpt:\nAICX output and logs\n\nAgent answered:\nTask complete.\n",
            serde_json::json!({
                "artifact_family": "tb-spotlight-rounds",
                "round_id": "round-dump",
                "user_anchor_kind": "evidence_dump",
                "user_evidence_chars": 2000
            }),
        );

        let report = build_evidence_report(
            "dlaczego zmienilismy ranking searcha",
            vec![fake_result(&path, 90, vec!["ranking searcha"])],
            1,
        );

        assert_eq!(report.items[0].label, EvidenceLabel::Dump);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn evidence_operational_query_can_surface_command_trace() {
        let path = write_fixture(
            "operational-command",
            "[project: x]\nUser intent:\njaką komendę odpaliłam do searchowania embeddingów?\n\nAgent answered:\nKomende: AICX_HOME=/tmp/aicx aicx search --evidence -p tb14d-anchor-v4/aicx \"embeddingow\". Smoke pokazal oracle_status backend=hybrid_rrf.\n",
            serde_json::json!({
                "artifact_family": "tb-spotlight-rounds",
                "round_id": "round-command",
                "round_part_index": 1,
                "round_part_count": 1,
                "user_anchor_kind": "request"
            }),
        );

        let report = build_evidence_report(
            "jaką komendę odpaliłam do searchowania embeddingów",
            vec![fake_result(
                &path,
                100,
                vec!["AICX_HOME=/tmp/aicx aicx search --evidence"],
            )],
            1,
        );

        assert_eq!(report.query_class, EvidenceQueryClass::Operational);
        assert_ne!(report.items[0].label, EvidenceLabel::Meta);
        assert_eq!(report.items[0].label, EvidenceLabel::Supported);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn evidence_dedupes_round_parts() {
        let path_a = write_fixture(
            "part-a",
            "User intent:\nwhy\n\nAgent answered (part 1/2):\nDecision because A.\n",
            serde_json::json!({
                "artifact_family": "tb-spotlight-rounds",
                "round_id": "same-round",
                "round_part_index": 1,
                "round_part_count": 2,
                "user_anchor_kind": "request"
            }),
        );
        let path_b = write_fixture(
            "part-b",
            "User intent:\nwhy\n\nAgent answered (part 2/2):\nDecision because B.\n",
            serde_json::json!({
                "artifact_family": "tb-spotlight-rounds",
                "round_id": "same-round",
                "round_part_index": 2,
                "round_part_count": 2,
                "user_anchor_kind": "request"
            }),
        );

        let report = build_evidence_report(
            "why decision",
            vec![
                fake_result(&path_a, 88, vec!["Decision because A"]),
                fake_result(&path_b, 80, vec!["Decision because B"]),
            ],
            10,
        );

        assert_eq!(report.items.len(), 1);
        assert_eq!(report.suppressed.len(), 1);
        assert_eq!(
            report.suppressed[0].reason,
            "dedupe_round_or_path_best_result_kept"
        );

        let _ = std::fs::remove_dir_all(path_a.parent().unwrap());
        let _ = std::fs::remove_dir_all(path_b.parent().unwrap());
    }
}
