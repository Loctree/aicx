use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};

use crate::oracle::{ClaimHonesty, OracleEnvelope, OracleStatus};

use super::{IntentKind, IntentRecord, cmp_dates_flexible, parse_flexible_utc};

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
    mut records: Vec<IntentRecord>,
    filters: &IntentDisplayFilters,
) -> Vec<IntentRecord> {
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
        let mut map: HashMap<String, IntentRecord> = HashMap::new();
        let mut order = Vec::new();
        for rec in records {
            let key = rec.session_id.clone();
            match map.entry(key.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    order.push(key);
                    let mut clone = rec.clone();
                    clone.count = Some(rec.count.unwrap_or(1));
                    entry.insert(clone);
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    *existing.count.get_or_insert(0) += rec.count.unwrap_or(1);
                    if !existing.evidence.contains(&rec.summary) {
                        existing.evidence.push(rec.summary);
                    }
                    append_unique_source_chunk(&mut existing.source_chunk, &rec.source_chunk);
                }
            }
        }
        records = order.into_iter().filter_map(|k| map.remove(&k)).collect();
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

    if let Some(sort_order) = filters.sort {
        records.sort_by(|a, b| {
            let t_a = a.timestamp.as_deref().unwrap_or(a.date.as_str());
            let t_b = b.timestamp.as_deref().unwrap_or(b.date.as_str());
            let ord = cmp_dates_flexible(t_a, t_b);
            match sort_order {
                IntentSortOrder::Newest => ord.reverse(),
                IntentSortOrder::Oldest => ord,
            }
        });
    }

    if let Some(limit) = filters.limit {
        records.truncate(limit);
    }

    records
}

fn collapse_exact_daily_duplicates(records: Vec<IntentRecord>) -> Vec<IntentRecord> {
    let mut map: HashMap<(IntentKind, String, String, String), IntentRecord> = HashMap::new();
    let mut order = Vec::new();

    for rec in records {
        let key = (
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

fn normalize_display_key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn append_unique_source_chunk(target: &mut String, source_chunk: &str) {
    if target.split(", ").any(|existing| existing == source_chunk) {
        return;
    }
    if target.is_empty() {
        target.push_str(source_chunk);
    } else {
        target.push_str(", ");
        target.push_str(source_chunk);
    }
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
