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

/// Discover identities with the catalog and parse every selected session once.
///
/// App-only: session discovery (`session_catalog`), parser dispatch, and
/// timeline projection all live behind `feature = "app"`; the slim
/// loctree-consumer profile reads the canonical store instead of raw sources.
#[cfg(feature = "app")]
pub fn extract_agent_sessions(
    agent: crate::session_catalog::AgentKind,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let home = crate::os_user_home().context("No home dir")?;
    let root = match agent {
        crate::session_catalog::AgentKind::Claude => home.join(".claude").join("projects"),
        crate::session_catalog::AgentKind::Codex => home.join(".codex").join("sessions"),
        crate::session_catalog::AgentKind::Gemini => home.join(".gemini").join("tmp"),
        crate::session_catalog::AgentKind::Grok => home.join(".grok"),
        crate::session_catalog::AgentKind::Junie => home.join(".junie").join("sessions"),
    };
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let scan = crate::session_catalog::SessionCatalog::new(agent, &root)?.scan_with_stats();
    let parser_agent = parser_agent(agent);
    let mut entries = Vec::new();
    for source in scan.result? {
        let session = crate::parser_dispatch::parse_file(
            parser_agent,
            &source.source_id,
            source.logical_session_id,
            &source.path,
        )?;
        entries.extend(crate::output::timeline_entries_from_model(session.model()));
    }
    entries.retain(|entry| {
        entry.timestamp >= config.cutoff
            && (config.include_assistant || entry.role == "user")
            && config
                .watermark
                .is_none_or(|watermark| entry.timestamp > watermark)
    });
    Ok(entries)
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
