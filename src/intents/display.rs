use anyhow::{Context, Result};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::extraction::is_harness_injected_noise;
use crate::oracle::{ClaimHonesty, OracleEnvelope, OracleStatus};

use super::{
    IntentKind, IntentRecord, IntentsCompleteness, cmp_dates_flexible, parse_flexible_utc,
};

/// Sort order for `apply_display_filters`. Mirrors the CLI's `SortOrder`
/// without importing main.rs types into the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentSortOrder {
    Newest,
    Oldest,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
    Default,
)]
#[serde(rename_all = "lowercase")]
pub enum UnresolvedMode {
    #[default]
    Session,
    Intent,
}

/// Display-time filters applied AFTER `extract_intents`.
///
/// Promoted from `main.rs::run_intents` so that both the CLI and the MCP
/// `aicx_intents` tool can reuse the same post-processing pipeline. Without
/// this, MCP would silently lose `unresolved`/`collapse_session`/`agent`/
/// date-range/sort/limit semantics.
#[derive(Debug, Clone, Default)]
pub struct IntentDisplayFilters {
    pub unresolved: bool,
    pub unresolved_mode: UnresolvedMode,
    pub collapse_session: bool,
    pub agent: Option<String>,
    pub date_lo: Option<String>,
    pub date_hi: Option<String>,
    pub sort: Option<IntentSortOrder>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct IntentDisplayResult {
    pub records: Vec<IntentRecord>,
    pub available_before_limit: usize,
    pub requested_limit: Option<usize>,
}

fn clean_to_significant_words(text: &str) -> HashSet<String> {
    const STOP_WORDS: &[&str] = &[
        // English verbs/helpers
        "fix",
        "fixed",
        "fixing",
        "ship",
        "shipped",
        "shipping",
        "implement",
        "implemented",
        "implementing",
        "add",
        "added",
        "adding",
        "test",
        "tests",
        "tested",
        "testing",
        "run",
        "running",
        "done",
        "completed",
        "complete",
        "finished",
        "finish",
        "working",
        "works",
        "success",
        "successfully",
        "fail",
        "failed",
        "with",
        "for",
        "and",
        "the",
        "a",
        "an",
        "in",
        "on",
        "at",
        "to",
        "of",
        "from",
        "by",
        "that",
        "this",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "have",
        "has",
        "had",
        "do",
        "does",
        "did",
        "as",
        "or",
        // Polish verbs/helpers
        "napraw",
        "naprawione",
        "naprawiłem",
        "wdrożone",
        "wdrozone",
        "wdrożyłem",
        "dodaj",
        "dodane",
        "dodałem",
        "zrobione",
        "zrobiłem",
        "gotowe",
        "dziala",
        "działa",
        "test",
        "testy",
        "przetestowane",
        "uruchom",
        "uruchomione",
        "sukces",
        "porażka",
        "błąd",
        "bledu",
        "bledy",
        "błędy",
        "z",
        "do",
        "na",
        "w",
        "o",
        "i",
        "a",
        "lub",
        "dla",
        "przez",
        "pod",
        "po",
        "działające",
    ];

    let mut words = HashSet::new();
    let cleaned = text.to_lowercase();
    for token in cleaned.split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_') {
        let trimmed = token.trim();
        if trimmed.len() > 1 && !STOP_WORDS.contains(&trimmed) {
            words.insert(trimmed.to_string());
        }
    }
    words
}

fn normalize_alphanumeric(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

fn outcome_matches_intent(outcome: &IntentRecord, intent: &IntentRecord) -> bool {
    if outcome.project != intent.project {
        return false;
    }

    let outcome_words = clean_to_significant_words(&outcome.summary);
    let intent_words = clean_to_significant_words(&intent.summary);

    if outcome_words.is_empty() || intent_words.is_empty() {
        let norm_outcome = normalize_alphanumeric(&outcome.summary);
        let norm_intent = normalize_alphanumeric(&intent.summary);
        if norm_outcome.is_empty() || norm_intent.is_empty() {
            return false;
        }
        return norm_outcome.contains(&norm_intent) || norm_intent.contains(&norm_outcome);
    }

    let intersection: HashSet<_> = outcome_words.intersection(&intent_words).collect();
    if intersection.is_empty() {
        return false;
    }

    let min_len = outcome_words.len().min(intent_words.len());
    let overlap_ratio = intersection.len() as f32 / min_len as f32;

    if min_len <= 2 {
        overlap_ratio >= 0.99
    } else {
        overlap_ratio >= 0.5
    }
}

/// Apply display-time filters to intent records.
///
/// Order matters: `unresolved` and `collapse_session` are session-scoped
/// transformations and must run before `agent`/date filters or the count
/// aggregation in `collapse_session` becomes inconsistent with the
/// downstream filters.
pub fn apply_display_filters(
    records: Vec<IntentRecord>,
    filters: &IntentDisplayFilters,
) -> Vec<IntentRecord> {
    apply_display_filters_with_completeness(records, filters).records
}

pub fn apply_display_filters_with_completeness(
    mut records: Vec<IntentRecord>,
    filters: &IntentDisplayFilters,
) -> IntentDisplayResult {
    if filters.unresolved {
        match filters.unresolved_mode {
            UnresolvedMode::Session => {
                let mut resolved_sessions = HashSet::new();
                for rec in &records {
                    if rec.kind == IntentKind::Outcome {
                        resolved_sessions.insert(rec.session_id.clone());
                    }
                }
                records.retain(|r| {
                    r.kind != IntentKind::Intent || !resolved_sessions.contains(&r.session_id)
                });
            }
            UnresolvedMode::Intent => {
                let outcomes: Vec<IntentRecord> = records
                    .iter()
                    .filter(|r| r.kind == IntentKind::Outcome)
                    .cloned()
                    .collect();
                records.retain(|r| {
                    if r.kind != IntentKind::Intent {
                        return true;
                    }
                    let has_match = outcomes.iter().any(|o| outcome_matches_intent(o, r));
                    !has_match
                });
            }
        }
    }

    if filters.collapse_session {
        records = collapse_exact_daily_duplicates(records);
        let mut map: HashMap<(String, String), IntentRecord> = HashMap::new();
        for rec in records {
            let key = (rec.project.clone(), rec.session_id.clone());
            match map.entry(key) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let mut rec = rec;
                    rec.count = Some(rec.count.unwrap_or(1));
                    entry.insert(rec);
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    merge_collapsed_record(entry.get_mut(), rec);
                }
            }
        }
        for record in map.values_mut() {
            record.evidence.sort();
            record.evidence.dedup();
            canonicalize_source_chunks(&mut record.source_chunk);
        }
        records = map.into_values().collect();
    }

    if let Some(agent_filter) = &filters.agent {
        records.retain(|r| &r.agent == agent_filter);
    }

    if filters.date_lo.is_some() || filters.date_hi.is_some() {
        // Compare on the typed UTC axis when both sides parse (bare dates
        // are midnight UTC, RFC3339 offsets are normalized); fall back to
        // the legacy lexicographic comparison for unparsable input.
        let within_bound = |date: &str, bound: &str, keep_low: bool| -> bool {
            match (parse_flexible_utc(date), parse_flexible_utc(bound)) {
                (Some(d), Some(b)) => {
                    if keep_low {
                        d >= b
                    } else {
                        d <= b
                    }
                }
                _ => {
                    if keep_low {
                        date >= bound
                    } else {
                        date <= bound
                    }
                }
            }
        };
        records.retain(|r| {
            filters
                .date_lo
                .as_ref()
                .is_none_or(|lo| within_bound(&r.date, lo, true))
                && filters
                    .date_hi
                    .as_ref()
                    .is_none_or(|hi| within_bound(&r.date, hi, false))
        });
    }

    // The documented default is newest-first. Always materialize that order
    // before applying the limit so equal timestamps cannot inherit filesystem,
    // HashMap, or caller iteration order. Project/session/chunk identity is the
    // shared CLI+MCP tie-break contract; kind/summary close the order for the
    // multiple intent records that can legitimately originate in one chunk.
    let sort_order = filters.sort.unwrap_or(IntentSortOrder::Newest);
    records.sort_by(|left, right| compare_intent_records(left, right, sort_order));

    let available_before_limit = records.len();
    if let Some(limit) = filters.limit {
        records.truncate(limit);
    }

    IntentDisplayResult {
        records,
        available_before_limit,
        requested_limit: filters.limit,
    }
}

fn collapse_exact_daily_duplicates(records: Vec<IntentRecord>) -> Vec<IntentRecord> {
    let mut map: HashMap<(String, String, IntentKind, String, String, String), IntentRecord> =
        HashMap::new();
    let mut order = Vec::new();

    for rec in records {
        let key = (
            rec.project.clone(),
            rec.session_id.clone(),
            rec.kind,
            rec.agent.clone(),
            rec.date.clone(),
            normalize_display_key(&rec.summary),
        );
        match map.entry(key.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                order.push(key);
                entry.insert(rec);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let existing = entry.get_mut();
                let incoming_count = rec.count.unwrap_or(1);
                let existing_count = existing.count.get_or_insert(1);
                *existing_count += incoming_count;
                append_unique_source_chunk(&mut existing.source_chunk, &rec.source_chunk);
                for evidence in rec.evidence {
                    if !existing.evidence.contains(&evidence) {
                        existing.evidence.push(evidence);
                    }
                }
            }
        }
    }

    order
        .into_iter()
        .filter_map(|key| map.remove(&key))
        .collect()
}

fn compare_intent_records(
    left: &IntentRecord,
    right: &IntentRecord,
    sort_order: IntentSortOrder,
) -> Ordering {
    let left_time = left.timestamp.as_deref().unwrap_or(left.date.as_str());
    let right_time = right.timestamp.as_deref().unwrap_or(right.date.as_str());
    let time_order = cmp_dates_flexible(left_time, right_time);
    let time_order = match sort_order {
        IntentSortOrder::Newest => time_order.reverse(),
        IntentSortOrder::Oldest => time_order,
    };

    time_order
        .then_with(|| left.project.cmp(&right.project))
        .then_with(|| left.session_id.cmp(&right.session_id))
        .then_with(|| left.source_chunk.cmp(&right.source_chunk))
        .then_with(|| left.kind.sort_rank().cmp(&right.kind.sort_rank()))
        .then_with(|| left.summary.cmp(&right.summary))
}

fn merge_collapsed_record(existing: &mut IntentRecord, incoming: IntentRecord) {
    let total_count = existing.count.unwrap_or(1) + incoming.count.unwrap_or(1);
    let existing_summary = existing.summary.clone();
    let incoming_summary = incoming.summary.clone();
    let mut combined_evidence = existing.evidence.clone();
    append_unique_values(&mut combined_evidence, incoming.evidence.iter().cloned());
    let mut combined_source_chunk = existing.source_chunk.clone();
    append_unique_source_chunk(&mut combined_source_chunk, &incoming.source_chunk);

    if representative_preference(&incoming, existing).is_gt() {
        *existing = incoming;
        if existing.summary != existing_summary {
            append_unique_values(&mut combined_evidence, [existing_summary]);
        }
    } else if existing.summary != incoming_summary {
        append_unique_values(&mut combined_evidence, [incoming_summary]);
    }

    existing.count = Some(total_count);
    existing.evidence = combined_evidence;
    existing.source_chunk = combined_source_chunk;
}

/// Compare two candidates for the single representative of a collapsed
/// `(project_identity, session_id)` group. This intentionally reuses signals
/// already owned by the intents pipeline instead of inventing another ranker:
/// conversation harness-noise detection, IntentKind's established strength,
/// then the longest substantive frame. Remaining fields are deterministic
/// tie-breakers only.
fn representative_preference(left: &IntentRecord, right: &IntentRecord) -> Ordering {
    let left_is_noise = is_harness_injected_noise("user", &left.summary);
    let right_is_noise = is_harness_injected_noise("user", &right.summary);
    let left_time = left.timestamp.as_deref().unwrap_or(left.date.as_str());
    let right_time = right.timestamp.as_deref().unwrap_or(right.date.as_str());

    right_is_noise
        .cmp(&left_is_noise)
        .then_with(|| right.kind.sort_rank().cmp(&left.kind.sort_rank()))
        .then_with(|| {
            left.summary
                .chars()
                .count()
                .cmp(&right.summary.chars().count())
        })
        .then_with(|| cmp_dates_flexible(left_time, right_time))
        .then_with(|| right.source_chunk.cmp(&left.source_chunk))
        .then_with(|| right.summary.cmp(&left.summary))
}

fn append_unique_values(target: &mut Vec<String>, values: impl IntoIterator<Item = String>) {
    for value in values {
        if !target.contains(&value) {
            target.push(value);
        }
    }
}

fn normalize_display_key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn append_unique_source_chunk(target: &mut String, source_chunk: &str) {
    if !source_chunk.is_empty() {
        if !target.is_empty() {
            target.push_str(", ");
        }
        target.push_str(source_chunk);
    }
    canonicalize_source_chunks(target);
}

fn canonicalize_source_chunks(target: &mut String) {
    let mut chunks = target
        .split(", ")
        .filter(|chunk| !chunk.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    chunks.sort();
    chunks.dedup();
    *target = chunks.join(", ");
}

pub fn format_intents_markdown(records: &[IntentRecord]) -> String {
    if records.is_empty() {
        return String::new();
    }

    let mut out = String::from("# Intent Timeline\n\n");
    // Honesty frame for the whole surface: every record below is a historical
    // claim bound to its session close, never runtime-verified by aicx.
    out.push_str(&format!(
        "_{}_\n\n",
        ClaimHonesty::canonical().display_line()
    ));
    let mut last_date: Option<&str> = None;

    for record in records {
        if last_date != Some(record.date.as_str()) {
            if last_date.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("## {}\n\n", record.date));
            last_date = Some(record.date.as_str());
        }

        let voice_marker = if record.source.as_deref() == Some("voice_transcript") {
            " [voice]"
        } else {
            ""
        };
        out.push_str(&format!(
            "### {}{} | {}\n",
            record.kind.heading(),
            voice_marker,
            record.agent
        ));
        out.push_str(&format!("{}: {}\n", record.kind.heading(), record.summary));
        out.push_str(&format!(
            "WHY: {}\n",
            record.context.as_deref().unwrap_or("not captured")
        ));
        out.push_str("EVIDENCE:\n");
        out.push_str(&format!("- source_chunk: {}\n", record.source_chunk));
        for evidence in &record.evidence {
            out.push_str(&format!("- {}\n", evidence));
        }
        out.push('\n');
    }

    out
}

pub fn format_intents_json(records: &[IntentRecord]) -> Result<String> {
    serde_json::to_string_pretty(records).context("Failed to serialize intents to JSON")
}

pub fn format_intents_oracle_json(
    records: &[IntentRecord],
    oracle_status: OracleStatus,
) -> Result<String> {
    serde_json::to_string_pretty(&OracleEnvelope {
        oracle_status,
        claim_honesty: ClaimHonesty::canonical(),
        results: records.len(),
        items: records,
    })
    .context("Failed to serialize intents oracle JSON")
}

pub fn format_intents_oracle_json_with_completeness(
    records: &[IntentRecord],
    oracle_status: OracleStatus,
    completeness: IntentsCompleteness,
) -> Result<String> {
    #[derive(serde::Serialize)]
    struct IntentsOracleEnvelope<'a> {
        oracle_status: OracleStatus,
        claim_honesty: ClaimHonesty,
        completeness: IntentsCompleteness,
        results: usize,
        items: &'a [IntentRecord],
    }

    serde_json::to_string_pretty(&IntentsOracleEnvelope {
        oracle_status,
        claim_honesty: ClaimHonesty::canonical(),
        completeness,
        results: records.len(),
        items: records,
    })
    .context("Failed to serialize intents oracle JSON")
}
