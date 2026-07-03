#![allow(unused_imports)]
use super::*;
use sha2::{Digest, Sha256};
use std::io::Read;

#[derive(Debug, Clone)]
pub(crate) struct ClassifiedFrameBlock {
    pub(crate) role: String,
    pub(crate) frame_kind: FrameKind,
    pub(crate) message: String,
}

/// Optional trailing metadata for `build_timeline_entry` — bundled so the
/// constructor stays under the clippy argument ceiling without `#[allow]`.
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

pub(crate) trait PushContentSanitizationWarning {
    fn push_content_sanitization_warning(&mut self, warning: sanitize::ContentSanitizationWarning);
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
    build_timeline_entry_from_message(
        timestamp,
        agent,
        session_id,
        role,
        sanitized.text.into_owned(),
        meta,
    )
}

pub(crate) fn build_timeline_entry_with_content_warnings<W>(
    timestamp: DateTime<Utc>,
    agent: &str,
    session_id: &str,
    role: &str,
    message: String,
    meta: TimelineEntryMeta,
    warnings: &mut W,
) -> TimelineEntry
where
    W: PushContentSanitizationWarning,
{
    let sanitized = sanitize::sanitize_chunk_content(&message);
    for warning in sanitized.warnings {
        warnings.push_content_sanitization_warning(warning);
    }
    build_timeline_entry_from_message(
        timestamp,
        agent,
        session_id,
        role,
        sanitized.text.into_owned(),
        meta,
    )
}

pub(crate) fn build_timeline_entry_from_message(
    timestamp: DateTime<Utc>,
    agent: &str,
    session_id: &str,
    role: &str,
    message: String,
    meta: TimelineEntryMeta,
) -> TimelineEntry {
    TimelineEntry {
        timestamp,
        agent: agent.to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        message,
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

pub(crate) fn file_sha256_hex(path: &Path) -> Result<String> {
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

pub(crate) fn push_classified_block(
    blocks: &mut Vec<ClassifiedFrameBlock>,
    role: &str,
    frame_kind: FrameKind,
    message: String,
) {
    let message = message.trim();
    if message.is_empty() {
        return;
    }

    if let Some(last) = blocks.last_mut()
        && last.role == role
        && last.frame_kind == frame_kind
    {
        last.message.push('\n');
        last.message.push_str(message);
        return;
    }

    blocks.push(ClassifiedFrameBlock {
        role: role.to_string(),
        frame_kind,
        message: message.to_string(),
    });
}

pub(crate) fn frame_kind_from_role(role: &str) -> Option<FrameKind> {
    match role.to_ascii_lowercase().as_str() {
        "user" => Some(FrameKind::UserMsg),
        "assistant" | "agent" => Some(FrameKind::AgentReply),
        "reasoning" | "thinking" => Some(FrameKind::InternalThought),
        "tool" | "tool_call" | "tool_result" | "function_call" => Some(FrameKind::ToolCall),
        "system" | "info" | "error" | "notification" | "system_note" | "developer"
        | "instructions" => Some(FrameKind::SystemNote),
        _ => None,
    }
}

pub(crate) fn role_for_frame_kind(frame_kind: FrameKind) -> &'static str {
    match frame_kind {
        FrameKind::UserMsg => "user",
        FrameKind::AgentReply => "assistant",
        FrameKind::InternalThought => "reasoning",
        FrameKind::ToolCall => "tool",
        FrameKind::SystemNote => "system",
    }
}

pub(crate) fn should_keep_entry(frame_kind: Option<FrameKind>, config: &ExtractionConfig) -> bool {
    config.include_assistant || matches!(frame_kind, Some(FrameKind::UserMsg))
}

pub(crate) fn dedup_timeline_entries(entries: &mut Vec<TimelineEntry>) {
    let mut seen = HashSet::new();
    entries.retain(|entry| {
        seen.insert((
            entry.timestamp,
            entry.role.clone(),
            entry.frame_kind,
            entry.message.clone(),
            entry.cwd.clone(),
        ))
    });
}

// ============================================================================
// Claude history.jsonl extractor
// ============================================================================
