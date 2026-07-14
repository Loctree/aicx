//! Minimal support for non-session document importers.
//!
//! CodeScribe and operator Markdown are intentionally not agent-session
//! parsers. Their shared TimelineEntry constructor and provenance hashing live
//! here so removing the legacy session engine does not amputate document ingest.

use anyhow::Result;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

use crate::sanitize;
use crate::timeline::{FrameKind, TimelineEntry};

#[derive(Debug, Default, Clone)]
pub(crate) struct TimelineEntryMeta {
    pub(crate) branch: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) frame_kind: Option<FrameKind>,
    pub(crate) timestamp_source: Option<String>,
    pub(crate) source_path: Option<String>,
    pub(crate) source_sha256: Option<String>,
    pub(crate) source_line_span: Option<(u64, u64)>,
}

pub(crate) fn build_timeline_entry(
    timestamp: DateTime<Utc>,
    agent: &str,
    session_id: &str,
    role: &str,
    message: String,
    meta: TimelineEntryMeta,
) -> TimelineEntry {
    let sanitized = sanitize::sanitize_chunk_content(&message);
    TimelineEntry {
        timestamp,
        agent: agent.to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        message: sanitized.text.into_owned(),
        frame_kind: meta.frame_kind,
        branch: meta.branch,
        cwd: meta.cwd,
        timestamp_source: meta.timestamp_source,
        source_path: meta.source_path,
        source_sha256: meta.source_sha256,
        source_line_span: meta.source_line_span,
    }
}

pub(crate) fn source_path_and_sha256(path: &Path) -> (String, Option<String>) {
    (path.display().to_string(), file_sha256_hex(path).ok())
}

fn file_sha256_hex(path: &Path) -> Result<String> {
    let mut file = sanitize::open_file_validated(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
