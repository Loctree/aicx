use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};

use crate::oracle::{OracleEnvelope, OracleStatus};

use super::{IntentKind, IntentRecord, cmp_dates_flexible, parse_flexible_utc};

/// Sort order for `apply_display_filters`. Mirrors the CLI's `SortOrder`
/// without importing main.rs types into the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentSortOrder {
    Newest,
    Oldest,
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
    pub collapse_session: bool,
    pub agent: Option<String>,
    pub date_lo: Option<String>,
    pub date_hi: Option<String>,
    pub sort: Option<IntentSortOrder>,
    pub limit: Option<usize>,
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
        let mut resolved_sessions = HashSet::new();
        for rec in &records {
            if rec.kind == IntentKind::Outcome {
                resolved_sessions.insert(rec.session_id.clone());
            }
        }
        records
            .retain(|r| r.kind != IntentKind::Intent || !resolved_sessions.contains(&r.session_id));
    }

    if filters.collapse_session {
        let mut map: HashMap<String, IntentRecord> = HashMap::new();
        let mut order = Vec::new();
        for rec in records {
            let key = rec.session_id.clone();
            match map.entry(key.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    order.push(key);
                    let mut clone = rec.clone();
                    clone.count = Some(1);
                    entry.insert(clone);
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    *existing.count.as_mut().unwrap() += 1;
                    if !existing.evidence.contains(&rec.summary) {
                        existing.evidence.push(rec.summary);
                    }
                    if !existing.source_chunk.contains(&rec.source_chunk) {
                        existing.source_chunk =
                            format!("{}, {}", existing.source_chunk, rec.source_chunk);
                    }
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

pub fn format_intents_markdown(records: &[IntentRecord]) -> String {
    if records.is_empty() {
        return String::new();
    }

    let mut out = String::from("# Intent Timeline\n\n");
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
        results: records.len(),
        items: records,
    })
    .context("Failed to serialize intents oracle JSON")
}
