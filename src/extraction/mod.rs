#![allow(unused_imports)]
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

pub(crate) use crate::sanitize;
use crate::store::project_filter_matches;
use crate::timeline::FrameKind;
pub use crate::timeline::{
    CollapseStubKind, ConversationMessage, ExtractionConfig, MessageKind, SourceInfo, TimelineEntry,
};

pub mod conversation;
pub mod files;
mod importer_support;
pub mod list;
pub mod project;

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
#[cfg(feature = "app")]
#[derive(Debug, Default)]
pub struct SessionExtractionBatch {
    pub entries: Vec<TimelineEntry>,
    pub canonical_cards: Vec<aicx_parser::projections::CanonicalCard>,
    pub ingested_session_ids: BTreeSet<String>,
    pub selected_sessions: usize,
    pub ingested_sessions: usize,
    pub skipped: Vec<SessionSkip>,
}

#[cfg(feature = "app")]
impl SessionExtractionBatch {
    pub fn from_entries(entries: Vec<TimelineEntry>) -> Self {
        Self {
            ingested_sessions: usize::from(!entries.is_empty()),
            entries,
            canonical_cards: Vec::new(),
            ingested_session_ids: BTreeSet::new(),
            selected_sessions: 0,
            skipped: Vec::new(),
        }
    }
}

/// Discover identities with the catalog and parse every selected session once.
///
/// App-only: session discovery (`session_catalog`), parser dispatch, and
/// timeline projection all live behind `feature = "app"`; the slim
/// loctree-consumer profile reads the canonical store instead of raw sources.
#[cfg(feature = "app")]
pub fn extract_agent_sessions(
    agent: crate::session_catalog::AgentKind,
    config: &ExtractionConfig,
) -> Result<SessionExtractionBatch> {
    let home = crate::os_user_home().context("No home dir")?;
    let root = match agent {
        crate::session_catalog::AgentKind::Claude => home.join(".claude").join("projects"),
        crate::session_catalog::AgentKind::Codex => home.join(".codex").join("sessions"),
        crate::session_catalog::AgentKind::Gemini => home.join(".gemini").join("tmp"),
        crate::session_catalog::AgentKind::Grok => home.join(".grok"),
        crate::session_catalog::AgentKind::Junie => home.join(".junie").join("sessions"),
    };
    if !root.is_dir() {
        return Ok(SessionExtractionBatch::default());
    }
    let scan = crate::session_catalog::SessionCatalog::new(agent, &root)?.scan_with_stats();
    let parser_agent = parser_agent(agent);
    let mut batch = SessionExtractionBatch::default();
    for source in scan
        .result?
        .into_iter()
        .filter(|source| source_is_selected(source.fingerprint.modified_unix_nanos, config))
    {
        batch.selected_sessions += 1;
        let parsed = crate::parser_dispatch::parse_file(
            parser_agent,
            &source.source_id,
            source.logical_session_id.clone(),
            &source.path,
        );
        match parsed {
            Ok(session) => {
                let projection = match aicx_parser::projections::project_validated_session(
                    &session,
                    &canonical_projection_config(),
                ) {
                    Ok(projection) => projection,
                    Err(error) => {
                        batch.skipped.push(session_skip(
                            agent,
                            &source.path,
                            &source.source_id,
                            source.logical_session_id.as_deref(),
                            source.fingerprint.modified_unix_nanos,
                            &format!("canonical projection failed: {error}"),
                        ));
                        continue;
                    }
                };
                batch.ingested_sessions += 1;
                batch
                    .ingested_session_ids
                    .insert(session.model().session_id.clone());
                batch.canonical_cards.extend(projection.cards);
                batch
                    .entries
                    .extend(crate::output::timeline_entries_from_model(session.model()));
            }
            Err(error) => {
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
    Ok(batch)
}

#[cfg(feature = "app")]
pub fn canonical_projection_config() -> aicx_parser::projections::ProjectionConfig {
    aicx_parser::projections::ProjectionConfig {
        extraction_schema: aicx_parser::engine::SESSION_MODEL_SCHEMA.to_owned(),
        producer_version: format!("aicx-parser@{}", env!("CARGO_PKG_VERSION")),
        attribution_version: "project-bucket-v1".to_owned(),
        project_override: None,
    }
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
        crate::session_catalog::AgentKind::Gemini => aicx_parser::engine::AgentKind::Gemini,
        crate::session_catalog::AgentKind::Grok => aicx_parser::engine::AgentKind::Grok,
        crate::session_catalog::AgentKind::Junie => aicx_parser::engine::AgentKind::Junie,
    }
}

#[cfg(test)]
mod tests;
