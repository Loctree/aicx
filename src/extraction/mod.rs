#![allow(unused_imports)]
use aicx_parser::engine::{
    DetectionProbe, EngineError, MIN_VISIBLE_TURNS, RefusalEvidence, RefusalReason,
};
pub(crate) use anyhow::Context;
use anyhow::Result;
pub(crate) use chrono::{DateTime, Utc};
use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};
pub(crate) use std::fs;
use std::io::BufReader;
pub(crate) use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

use crate::legacy_archive::project_filter_matches;
pub(crate) use crate::sanitize;
use crate::timeline::FrameKind;
pub use crate::timeline::{
    CollapseStubKind, ConversationMessage, ExtractionConfig, MessageKind, SourceInfo, TimelineEntry,
};

pub mod conversation;
pub mod files;
mod importer_support;
pub mod list;
pub mod project;
pub mod projection;

pub use conversation::{
    ConversationProjection, is_harness_injected_noise, to_conversation, to_conversation_with_stats,
};
pub(crate) use conversation::{IntentLineModality, intent_line_modality};
pub(crate) use files::{MAX_LINE_BYTES, walk_jsonl_files};
pub(crate) use importer_support::{
    TimelineEntryMeta, build_timeline_entry, source_path_and_sha256,
};
pub use list::list_available_sources;
pub(crate) use project::*;
pub use project::{
    decode_claude_project_path, detect_project_name, infer_repo_name_from_current_dir,
    repo_labels_from_entries, repo_name_from_cwd,
};

const UNPROTECTED_SOURCE_WARNING: &str = "unprotected source material; run `aicx sources protect --root <path> --backend git-local --apply` to opt in";

#[cfg(feature = "app")]
const IN_FLIGHT_GRACE: StdDuration = StdDuration::from_secs(5 * 60);

/// One source that was refused by the parser without poisoning its batch.
#[cfg(feature = "app")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSkip {
    pub source_path: PathBuf,
    pub session_id: String,
    pub reason: String,
    pub recover: String,
    pub in_flight: bool,
}

/// Fail-closed per-session extraction with batch-level resilience.
///
/// Entries flow to extract/report output only. Per-frame store cards and
/// canonical-projection materialization are retired (extracts-store cut).
#[cfg(feature = "app")]
#[derive(Debug, Default)]
pub struct SessionExtractionBatch {
    pub entries: Vec<TimelineEntry>,
    pub ingested_session_ids: BTreeSet<String>,
    pub selected_sessions: usize,
    pub ingested_sessions: usize,
    /// Sessions cut by the `-p/--project` discovery filter. Deliberately
    /// outside `selected_sessions`/`skipped`: an all-filtered batch is an
    /// empty result, not an all-skipped failure.
    pub filtered_out_sessions: usize,
    /// Extra physical sources whose logical session was already ingested in
    /// this batch (e.g. an archived copy of the same rollout file). Dropped,
    /// not skipped: they must not hold the watermark or trigger retries.
    pub duplicate_sources: usize,
    pub skipped: Vec<SessionSkip>,
}

#[cfg(feature = "app")]
impl SessionExtractionBatch {
    pub fn from_entries(entries: Vec<TimelineEntry>) -> Self {
        Self {
            ingested_sessions: usize::from(!entries.is_empty()),
            entries,
            ingested_session_ids: BTreeSet::new(),
            selected_sessions: 0,
            filtered_out_sessions: 0,
            duplicate_sources: 0,
            skipped: Vec::new(),
        }
    }
}

/// Freshest copy first: one logical session can be discoverable through more
/// than one physical path (e.g. a live rollout file plus an archived copy).
/// Parsing the most recently modified source first lets the per-batch
/// session-id guard drop older duplicates so extract/report sees one body.
#[cfg(feature = "app")]
fn order_sources_freshest_first(sources: &mut [crate::session_catalog::CatalogSource]) {
    // STABLE sort on purpose (NOT sort_unstable_by): archived copies made with
    // `cp -p` carry an identical mtime, and on a tie the catalog scan order
    // must decide the duplicate winner deterministically. An unstable sort
    // would make "which copy wins" run-to-run random for equal timestamps.
    sources.sort_by(|left, right| {
        right
            .fingerprint
            .modified_unix_nanos
            .cmp(&left.fingerprint.modified_unix_nanos)
    });
}

/// Discover identities with the catalog and parse every selected session once.
///
/// App-only: session discovery (`session_catalog`), parser dispatch, and
/// timeline projection all live behind `feature = "app"`; the slim
/// The loctree-consumer profile can read explicit legacy archive fixtures
/// instead of discovering raw sources.
#[cfg(feature = "app")]
pub fn extract_agent_sessions(
    agent: crate::session_catalog::AgentKind,
    config: &ExtractionConfig,
) -> Result<SessionExtractionBatch> {
    let home = crate::os_user_home().context("No home dir")?;
    let root = match agent {
        crate::session_catalog::AgentKind::Claude => home.join(".claude").join("projects"),
        crate::session_catalog::AgentKind::Codex => home.join(".codex").join("sessions"),
        crate::session_catalog::AgentKind::Cursor => home.join(".cursor").join("projects"),
        crate::session_catalog::AgentKind::Gemini => home.join(".gemini").join("tmp"),
        crate::session_catalog::AgentKind::Grok => home.join(".grok"),
        crate::session_catalog::AgentKind::Junie => home.join(".junie").join("sessions"),
    };
    if !root.is_dir() {
        // No session root means this agent has never written a session on
        // this host: zero sources, not a failed adapter claim. Typed refusals
        // are reserved for sources that exist and were understood.
        crate::diagnostics::log_describe(&format!(
            "session_root_missing agent={} root={}",
            agent,
            root.display()
        ));
        return Ok(SessionExtractionBatch::default());
    }
    let scan = crate::session_catalog::SessionCatalog::new(agent, &root)?.scan_with_stats();
    let parser_agent_kind = parser_agent(agent);
    let mut batch = SessionExtractionBatch::default();
    let mut first_refusal = None;
    let mut projection_basis = None;
    let mut sources: Vec<_> = scan
        .result?
        .into_iter()
        .filter(|source| source_is_selected(source.fingerprint.modified_unix_nanos, config))
        .collect();
    order_sources_freshest_first(&mut sources);
    for source in sources {
        let parsed = crate::parser_dispatch::parse_file(
            parser_agent_kind,
            &source.source_id,
            source.logical_session_id.clone(),
            &source.path,
        );
        match parsed {
            Ok(session) => {
                projection_basis.get_or_insert_with(|| {
                    (
                        session.model().session_id.clone(),
                        session.model().coverage.clone(),
                        session.model().turns.len() as u64,
                    )
                });
                if !session_matches_project_filter(session.model(), &config.project_filter) {
                    batch.filtered_out_sessions += 1;
                    crate::diagnostics::log_describe(&format!(
                        "session_filtered_out agent={} session_id={} path={}",
                        agent,
                        session.model().session_id,
                        source.path.display()
                    ));
                    continue;
                }
                batch.selected_sessions += 1;
                if batch
                    .ingested_session_ids
                    .contains(&session.model().session_id)
                {
                    batch.duplicate_sources += 1;
                    continue;
                }
                // Store-era canonical card projection is retired. A successful
                // parse is enough to emit timeline entries / extract bodies.
                batch.ingested_sessions += 1;
                batch
                    .ingested_session_ids
                    .insert(session.model().session_id.clone());
                let mut session_entries =
                    crate::output::timeline_entries_from_model(session.model());
                // Transports with no in-band wall clock (cursor without a
                // harness <timestamp> wrapper) project UNIX_EPOCH entries,
                // which the cutoff/watermark retain below would delete in
                // full — a "CompleteVisible parse, zero output" lie. Fall
                // back to the source file's mtime and say so.
                if let Some(mtime) =
                    datetime_from_unix_nanos(source.fingerprint.modified_unix_nanos)
                {
                    for entry in &mut session_entries {
                        if entry.timestamp == DateTime::<Utc>::UNIX_EPOCH
                            && entry.timestamp_source.as_deref() == Some("unknown")
                        {
                            entry.timestamp = mtime;
                            entry.timestamp_source = Some("file_mtime".to_string());
                        }
                    }
                }
                batch.entries.extend(session_entries);
            }
            Err(error) => {
                if first_refusal.is_none() {
                    first_refusal =
                        error
                            .downcast_ref::<EngineError>()
                            .and_then(|engine| match engine {
                                EngineError::Validation(validation) => {
                                    validation.refusal.as_deref().cloned()
                                }
                                _ => None,
                            });
                }
                // A parse failure hides the session's cwd, so an active
                // `-p` filter cannot prove the session is out of scope.
                // Keep it selected + skipped: hiding parse failures inside
                // the filtered project would be worse than surfacing a
                // neighbour's broken session.
                batch.selected_sessions += 1;
                batch.skipped.push(session_skip(
                    agent,
                    &source.path,
                    &source.source_id,
                    source.logical_session_id.as_deref(),
                    source.fingerprint.modified_unix_nanos,
                    &error.to_string(),
                ));
            }
        }
    }
    batch.entries.retain(|entry| {
        entry.timestamp >= config.cutoff
            && (config.include_assistant || entry.role == "user")
            && config
                .watermark
                .is_none_or(|watermark| entry.timestamp > watermark)
    });
    if batch.entries.is_empty() {
        if batch.ingested_sessions == 0
            && let Some(refusal) = first_refusal
        {
            return Err(anyhow::Error::new(refusal));
        }
        if let Some((session_id, _coverage, turns_before_filter)) = projection_basis {
            // A batch filter (`-p`, cutoff, watermark) that removes every
            // entry is a legitimate zero for `aicx <agent>` / `aicx all`
            // (incremental runs and project cuts hit it routinely). Say so on
            // the diagnostics channel; the typed `ProjectionFilteredAll`
            // refusal belongs to single-session seams (`extract`, MCP).
            crate::diagnostics::log_describe(&format!(
                "projection_filtered_all agent={} session_id={} turns_before_filter={} cutoff={} include_assistant={} watermark={:?} project_filter={:?}",
                agent,
                session_id,
                turns_before_filter,
                config.cutoff,
                config.include_assistant,
                config.watermark,
                config.project_filter
            ));
        }
        // Nothing was selected inside the extract window: an empty batch is
        // the honest answer (dry-runs, watermark catch-ups and `-H` windows
        // legitimately see zero sessions). Detection refusals are for sources
        // the adapter looked at and could not claim.
    }
    Ok(batch)
}

#[cfg(feature = "app")]
fn session_skip(
    agent: crate::session_catalog::AgentKind,
    source_path: &Path,
    source_id: &str,
    logical_session_id: Option<&str>,
    modified_unix_nanos: u128,
    error: &str,
) -> SessionSkip {
    let in_flight = is_in_flight_failure(modified_unix_nanos, error);
    let reason = if in_flight {
        "in-flight: source is still being written; parser completeness is not final".to_owned()
    } else {
        one_line_error(error)
    };
    let recover = format!(
        "aicx extract {} --file '{}' --conversation -o <output>",
        agent,
        source_path.display()
    );
    let skip = SessionSkip {
        source_path: source_path.to_path_buf(),
        session_id: logical_session_id.unwrap_or(source_id).to_owned(),
        reason,
        recover,
        in_flight,
    };
    crate::diagnostics::log_describe(&format!(
        "session_skip agent={} session_id={} path={} reason={} recover={}",
        agent,
        skip.session_id,
        skip.source_path.display(),
        skip.reason,
        skip.recover
    ));
    skip
}

/// Session-level `-p/--project` discovery filter (O1, problem-log
/// 2026-07-17 15:12 UTC): a session belongs to the requested project when
/// ANY of its known working directories (session provenance or per-segment
/// cwd) matches the filter, using the same word-boundary path semantics as
/// per-entry segmentation (`project_filter_matches_path`). Fail-closed: with
/// an active filter, a session with no known cwd cannot be shown to belong
/// to the requested project and is filtered out (visible in `filtered_out`).
#[cfg(feature = "app")]
fn session_matches_project_filter(
    model: &aicx_parser::engine::SessionModel,
    filters: &[String],
) -> bool {
    if filters.is_empty() {
        return true;
    }
    let provenance_cwd = match model.provenance.cwd.as_ref() {
        aicx_parser::engine::Known::Value(cwd) => Some(cwd.as_str()),
        aicx_parser::engine::Known::Unknown(_) => None,
    };
    let segment_cwds = model
        .segments
        .iter()
        .filter_map(|segment| match segment.cwd.as_ref() {
            aicx_parser::engine::Known::Value(cwd) => Some(cwd.as_str()),
            aicx_parser::engine::Known::Unknown(_) => None,
        });
    provenance_cwd
        .into_iter()
        .chain(segment_cwds)
        .any(|cwd| project_filter_matches_path(cwd, filters))
}

/// Source-file mtime as a UTC instant; `None` for a zero/overflowed stamp.
fn datetime_from_unix_nanos(nanos: u128) -> Option<DateTime<Utc>> {
    if nanos == 0 {
        return None;
    }
    let secs = i64::try_from(nanos / 1_000_000_000).ok()?;
    let subsec = (nanos % 1_000_000_000) as u32;
    DateTime::<Utc>::from_timestamp(secs, subsec)
}

#[cfg(feature = "app")]
fn source_is_selected(modified_unix_nanos: u128, config: &ExtractionConfig) -> bool {
    let lower_bound = config.watermark.unwrap_or(config.cutoff);
    let lower_bound_nanos = lower_bound
        .timestamp_nanos_opt()
        .map_or(0, |value| value.max(0) as u128);
    modified_unix_nanos > lower_bound_nanos
}

#[cfg(feature = "app")]
fn is_in_flight_failure(modified_unix_nanos: u128, error: &str) -> bool {
    if !error.contains("Fatal completeness") {
        return false;
    }
    let modified = UNIX_EPOCH
        .checked_add(StdDuration::from_nanos(
            modified_unix_nanos.min(u64::MAX as u128) as u64,
        ))
        .unwrap_or(UNIX_EPOCH);
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age <= IN_FLIGHT_GRACE)
}

#[cfg(feature = "app")]
fn one_line_error(error: &str) -> String {
    error.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(feature = "app")]
const fn parser_agent(agent: crate::session_catalog::AgentKind) -> aicx_parser::engine::AgentKind {
    match agent {
        crate::session_catalog::AgentKind::Claude => aicx_parser::engine::AgentKind::Claude,
        crate::session_catalog::AgentKind::Codex => aicx_parser::engine::AgentKind::Codex,
        crate::session_catalog::AgentKind::Cursor => aicx_parser::engine::AgentKind::Cursor,
        crate::session_catalog::AgentKind::Gemini => aicx_parser::engine::AgentKind::Gemini,
        crate::session_catalog::AgentKind::Grok => aicx_parser::engine::AgentKind::Grok,
        crate::session_catalog::AgentKind::Junie => aicx_parser::engine::AgentKind::Junie,
    }
}

#[cfg(test)]
mod tests;
