//! Sources / Extractors module for AI Contexters
//!
//! Standalone extraction logic for Claude Code, Codex, Gemini Code Assist,
//! and Gemini Antigravity direct extracts.
//! Improvements over the inline main.rs approach:
//! - Session-based Codex filtering (not per-message)
//! - Watermark support for incremental extraction
//! - Optional assistant message inclusion
//! - Gemini Code Assist support
//! - Gemini Antigravity conversation/decision recovery
//! - Proper deduplication
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sanitize;
use crate::store::project_filter_matches;
use crate::timeline::FrameKind;
pub use crate::timeline::{
    CollapseStubKind, ConversationMessage, ExtractionConfig, MessageKind, SourceInfo, TimelineEntry,
};

// During Faza 1 transition, pull shared logic from the new location
pub use super::shared::conversation::{
    ConversationProjection, to_conversation, to_conversation_with_stats,
};

const CODESCRIBE_AGENT: &str = "codescribe";
const CODESCRIBE_TRANSCRIPT_KIND: &str = "transcript";
const CODESCRIBE_NO_SPEECH_MARKERS: &[&str] = &[
    "no reliable speech detected",
    "no speech detected",
    "vad_no_speech_detected",
];
const OPERATOR_MD_AGENT: &str = "operator";
const OPERATOR_MD_KIND: &str = "operator-md";
/// Default discovery window applied when a caller does NOT supply its own cutoff.
///
/// Historically this acted as an unconditional ceiling, which silently capped
/// `aicx store --agent operator-md -H 0` (all-time backfill) at 30 days. It
/// is now a *default* honored only when `caller_cutoff` is `None` in
/// [`discover_operator_markdown_from`]. Callers that thread an
/// `ExtractionConfig::cutoff` through (e.g. the store pipeline) bypass this
/// default entirely, so explicit lookback flags are honored.
const OPERATOR_MD_RECENT_DAYS: i64 = 30;
const UNPROTECTED_SOURCE_WARNING: &str = "unprotected source material; run `aicx sources protect --root <path> --backend git-local --apply` to opt in";
// Constants moved to shared/conversation.rs during Faza 1
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

// Conversation projection logic has been moved to src/sources/shared/conversation.rs
// Re-exported here for backward compatibility during transition.

// Note: line reading goes through `aicx_parser::sanitize::read_line_capped`,
// which walks back past UTF-8 continuation bytes when an oversized line is
// truncated. The previously-private `read_line_limited` here truncated at
// the raw byte boundary and would surface `InvalidData` on any input where
// `max_bytes` landed inside a multi-byte codepoint — that follow-up was
// half-done (one call site already used the capped helper; the rest were
// still on the legacy truncator).

fn observe_oversized_line(count: &mut usize, samples: &mut Vec<String>, line_number: usize) {
    *count += 1;
    if samples.len() < 5 {
        samples.push(format!("line {line_number}"));
    }
}

fn parse_rfc3339_or_naive_utc(raw: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(raw) {
        return Ok(timestamp.with_timezone(&Utc));
    }

    NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f"))
        .map(|timestamp| DateTime::<Utc>::from_naive_utc_and_offset(timestamp, Utc))
}

fn short_path_hash(path: &Path) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}").chars().take(12).collect::<String>()
}

// classify_conversation_message moved to shared/conversation.rs during Faza 1

// ============================================================================
// Internal deserialization types
// ============================================================================

/// Claude Code JSONL entry structure.
#[derive(Debug, Deserialize)]
struct ClaudeEntry {
    #[serde(rename = "type")]
    entry_type: String,
    #[serde(default)]
    message: Option<serde_json::Value>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(rename = "gitBranch", default)]
    git_branch: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug)]
struct ClaudeRawEntry {
    entry: ClaudeEntry,
    line_number: usize,
}

/// Codex history JSONL entry structure.
#[derive(Debug, Deserialize)]
struct CodexEntry {
    session_id: String,
    #[serde(default)]
    text: String,
    ts: i64,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// Gemini CLI session file (`~/.gemini/tmp/<project>/chats/session-*.json[l]`).
///
/// Gemini has used both a single JSON object with a `messages` array and a
/// JSONL stream where the first line carries session metadata and later lines
/// carry messages / state updates.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiSession {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    messages: Vec<GeminiMessage>,
}

/// Gemini CLI message within a session.
///
/// The `type` field uses: "user", "gemini", "error", "info".
/// Unknown fields (thoughts, tokens, model, toolCalls, id) are silently ignored.
#[derive(Debug, Deserialize)]
struct GeminiMessage {
    #[serde(default, rename = "type")]
    msg_type: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default, rename = "displayContent")]
    display_content: Option<serde_json::Value>,
    #[serde(default)]
    timestamp: Option<String>,
    /// Agent reasoning/thinking steps.
    #[serde(default)]
    thoughts: Vec<GeminiThought>,
}

/// A single thought/reasoning step from Gemini.
#[derive(Debug, Deserialize)]
struct GeminiThought {
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
}

const JUNIE_SESSION_DIR_PREFIX: &str = "session-";
const JUNIE_REQUEST_ID_PREFIX: &str = "prompt-";
const JUNIE_EVENTS_FILENAME: &str = "events.jsonl";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeminiAntigravityRecoveryMode {
    ConversationArtifacts,
    StepOutputFallback,
}

impl GeminiAntigravityRecoveryMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::ConversationArtifacts => "conversation-artifacts",
            Self::StepOutputFallback => "step-output-fallback",
        }
    }

    fn note(self) -> &'static str {
        match self {
            Self::ConversationArtifacts => {
                "Recovered readable Antigravity conversation artifacts from brain state. Raw .pb was treated as opaque provenance, not parsed as plaintext."
            }
            Self::StepOutputFallback => {
                "No readable conversation artifact was found. This is a fallback decision stream from .system_generated/steps/*/output.txt, not a full conversation transcript."
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ClassifiedFrameBlock {
    role: String,
    frame_kind: FrameKind,
    message: String,
}

/// A discovered CodeScribe transcript under `$HOME/.codescribe/transcriptions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodescribeTranscript {
    pub path: PathBuf,
    pub date: NaiveDate,
}

/// A discovered operator-authored markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorMarkdown {
    pub path: PathBuf,
    pub modified: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct CodescribeSegment {
    start_ms: u64,
    duration_ms: Option<u64>,
    speaker: Option<String>,
    text: String,
}

#[derive(Debug, Clone, Default)]
struct CodescribeLexicon {
    entries: Vec<CodescribeLexiconEntry>,
}

#[derive(Debug, Clone)]
struct CodescribeLexiconEntry {
    speaker: Option<String>,
    keywords: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawCodescribeLexiconEntry {
    #[serde(default)]
    speaker: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    term: Option<String>,
    #[serde(default)]
    mispronunciations: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WhisperTranscript {
    #[serde(default)]
    segments: Vec<WhisperSegment>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WhisperSegment {
    #[serde(default)]
    start: Option<f64>,
    #[serde(default)]
    end: Option<f64>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    speaker: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct OperatorMarkdownFrontmatter {
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    author: Option<String>,
}

/// Optional trailing metadata for `build_timeline_entry` — bundled so the
/// constructor stays under the clippy argument ceiling without `#[allow]`.
#[derive(Debug, Default, Clone)]
struct TimelineEntryMeta {
    branch: Option<String>,
    cwd: Option<String>,
    frame_kind: Option<FrameKind>,
    timestamp_source: Option<String>,
}

trait PushContentSanitizationWarning {
    fn push_content_sanitization_warning(&mut self, warning: sanitize::ContentSanitizationWarning);
}

fn build_timeline_entry(
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

fn build_timeline_entry_with_content_warnings<W>(
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

fn build_timeline_entry_from_message(
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
    }
}

fn push_classified_block(
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

fn frame_kind_from_role(role: &str) -> Option<FrameKind> {
    match role.to_ascii_lowercase().as_str() {
        "user" => Some(FrameKind::UserMsg),
        "assistant" | "agent" => Some(FrameKind::AgentReply),
        "reasoning" | "thinking" => Some(FrameKind::InternalThought),
        "tool" | "tool_call" | "tool_result" | "function_call" => Some(FrameKind::ToolCall),
        "system" | "info" | "error" | "notification" | "system_note" => Some(FrameKind::SystemNote),
        _ => None,
    }
}

fn frame_kind_from_claude_type(entry_type: &str) -> Option<FrameKind> {
    match entry_type {
        "user" => Some(FrameKind::UserMsg),
        "assistant" => Some(FrameKind::AgentReply),
        "thinking" => Some(FrameKind::InternalThought),
        "tool_use" | "tool_result" => Some(FrameKind::ToolCall),
        _ => None,
    }
}

fn role_for_frame_kind(frame_kind: FrameKind) -> &'static str {
    match frame_kind {
        FrameKind::UserMsg => "user",
        FrameKind::AgentReply => "assistant",
        FrameKind::InternalThought => "reasoning",
        FrameKind::ToolCall => "tool",
        FrameKind::SystemNote => "system",
    }
}

fn should_keep_entry(frame_kind: Option<FrameKind>, config: &ExtractionConfig) -> bool {
    config.include_assistant || matches!(frame_kind, Some(FrameKind::UserMsg))
}

fn render_json_inline(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn render_claude_thinking_block(
    block: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    ["thinking", "text", "content", "summary"]
        .iter()
        .filter_map(|key| block.get(*key))
        .find_map(extract_text_from_json_value)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn render_claude_tool_block(block: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut parts = Vec::new();
    if let Some(name) = block.get("name").and_then(|value| value.as_str()) {
        parts.push(format!("name: {name}"));
    }
    if let Some(id) = block
        .get("id")
        .or_else(|| block.get("tool_use_id"))
        .and_then(|value| value.as_str())
    {
        parts.push(format!("id: {id}"));
    }
    if let Some(input) = block.get("input") {
        parts.push(format!("input: {}", render_json_inline(input)));
    }
    if let Some(content) = block.get("content").and_then(extract_text_from_json_value) {
        parts.push(format!("content: {content}"));
    }

    if parts.is_empty() {
        render_json_inline(&serde_json::Value::Object(block.clone()))
    } else {
        parts.join("\n")
    }
}

fn extract_claude_classified_blocks(
    message: &Option<serde_json::Value>,
    fallback_role: &str,
) -> Vec<ClassifiedFrameBlock> {
    fn from_content(
        content: &serde_json::Value,
        role: &str,
        blocks: &mut Vec<ClassifiedFrameBlock>,
    ) {
        match content {
            serde_json::Value::String(text) => {
                if let Some(frame_kind) = frame_kind_from_role(role) {
                    push_classified_block(blocks, role, frame_kind, text.clone());
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    from_content(item, role, blocks);
                }
            }
            serde_json::Value::Object(block) => {
                let block_type = block.get("type").and_then(|value| value.as_str());
                match block_type {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(|value| value.as_str())
                            && let Some(frame_kind) = frame_kind_from_role(role)
                        {
                            push_classified_block(blocks, role, frame_kind, text.to_string());
                        }
                    }
                    Some("thinking") => {
                        if let Some(thinking) = render_claude_thinking_block(block) {
                            push_classified_block(
                                blocks,
                                role_for_frame_kind(FrameKind::InternalThought),
                                FrameKind::InternalThought,
                                thinking,
                            );
                        }
                    }
                    Some("tool_use") | Some("tool_result") => {
                        push_classified_block(
                            blocks,
                            role_for_frame_kind(FrameKind::ToolCall),
                            FrameKind::ToolCall,
                            render_claude_tool_block(block),
                        );
                    }
                    _ => {
                        if let Some(content) = block.get("content") {
                            let nested_role = block
                                .get("role")
                                .and_then(|value| value.as_str())
                                .unwrap_or(role);
                            from_content(content, nested_role, blocks);
                        } else if block.get("thought").and_then(|value| value.as_bool())
                            == Some(true)
                        {
                            if let Some(thinking) = render_claude_thinking_block(block) {
                                push_classified_block(
                                    blocks,
                                    role_for_frame_kind(FrameKind::InternalThought),
                                    FrameKind::InternalThought,
                                    thinking,
                                );
                            }
                        } else if let Some(text) =
                            extract_text_from_json_value(&serde_json::Value::Object(block.clone()))
                            && let Some(frame_kind) = frame_kind_from_role(role)
                        {
                            push_classified_block(blocks, role, frame_kind, text);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut blocks = Vec::new();
    match message {
        Some(serde_json::Value::Object(object)) => {
            let role = object
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or(fallback_role);
            if let Some(content) = object.get("content") {
                from_content(content, role, &mut blocks);
            } else {
                from_content(
                    &serde_json::Value::Object(object.clone()),
                    role,
                    &mut blocks,
                );
            }
        }
        Some(value) => from_content(value, fallback_role, &mut blocks),
        None => {}
    }

    blocks
}

fn extract_claude_line_entries(
    entry: ClaudeEntry,
    timestamp: DateTime<Utc>,
    timestamp_source: Option<&str>,
    session_id: &str,
    config: &ExtractionConfig,
    warnings: &mut Vec<ClaudeSessionWarning>,
) -> Vec<TimelineEntry> {
    if timestamp < config.cutoff || config.watermark.is_some_and(|wm| timestamp < wm) {
        return Vec::new();
    }

    let mut entries = Vec::new();
    let fallback_role = if entry.entry_type == "tool_use" || entry.entry_type == "tool_result" {
        role_for_frame_kind(FrameKind::ToolCall)
    } else {
        entry.entry_type.as_str()
    };

    let classified = extract_claude_classified_blocks(&entry.message, fallback_role);
    if !classified.is_empty() {
        for block in classified {
            if !should_keep_entry(Some(block.frame_kind), config) {
                continue;
            }
            entries.push(build_timeline_entry_with_content_warnings(
                timestamp,
                "claude",
                session_id,
                &block.role,
                block.message,
                TimelineEntryMeta {
                    branch: entry.git_branch.clone(),
                    cwd: entry.cwd.clone(),
                    frame_kind: Some(block.frame_kind),
                    timestamp_source: timestamp_source.map(str::to_string),
                },
                warnings,
            ));
        }
        return entries;
    }

    let message = extract_message_text(&entry.message);
    if message.trim().is_empty() {
        return Vec::new();
    }

    let frame_kind = frame_kind_from_claude_type(&entry.entry_type)
        .or_else(|| frame_kind_from_role(fallback_role));
    if !should_keep_entry(frame_kind, config) {
        return Vec::new();
    }

    entries.push(build_timeline_entry_with_content_warnings(
        timestamp,
        "claude",
        session_id,
        fallback_role,
        message,
        TimelineEntryMeta {
            branch: entry.git_branch,
            cwd: entry.cwd,
            frame_kind,
            timestamp_source: timestamp_source.map(str::to_string),
        },
        warnings,
    ));
    entries
}

fn select_claude_session_id<'a>(
    entries: impl IntoIterator<Item = &'a ClaudeEntry>,
    fallback: &str,
    warnings: &mut Vec<ClaudeSessionWarning>,
) -> String {
    let mut first: Option<String> = None;
    let mut ignored = Vec::new();
    for id in entries
        .into_iter()
        .filter_map(|entry| entry.session_id.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        if let Some(existing) = &first {
            if existing != id && !ignored.iter().any(|seen| seen == id) {
                ignored.push(id.to_string());
            }
        } else {
            first = Some(id.to_string());
        }
    }

    match first {
        Some(first) => {
            if !ignored.is_empty() {
                warnings.push(ClaudeSessionWarning::SessionIdDrift {
                    first: first.clone(),
                    ignored,
                });
            }
            first
        }
        None => {
            warnings.push(ClaudeSessionWarning::MissingSessionId {
                fallback: fallback.to_string(),
            });
            fallback.to_string()
        }
    }
}

fn render_gemini_message_content(message: &GeminiMessage) -> Option<String> {
    message
        .content
        .as_ref()
        .and_then(render_gemini_content_value)
        .or_else(|| {
            message
                .display_content
                .as_ref()
                .and_then(render_gemini_content_value)
        })
}

fn truncate_gemini_large_data(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(inline_data) = map.get("inlineData") {
                let placeholder = render_gemini_inline_data_placeholder(inline_data);
                map.remove("inlineData");
                map.insert(
                    "inlineDataPlaceholder".to_string(),
                    serde_json::Value::String(placeholder),
                );
            }
            if let Some(file_data) = map.get("fileData") {
                let placeholder = render_gemini_file_data_placeholder(file_data);
                map.remove("fileData");
                map.insert(
                    "fileDataPlaceholder".to_string(),
                    serde_json::Value::String(placeholder),
                );
            }
            for v in map.values_mut() {
                truncate_gemini_large_data(v);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                truncate_gemini_large_data(v);
            }
        }
        _ => {}
    }
}

fn render_gemini_content_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(text) => {
            if text.trim().is_empty() {
                None
            } else {
                Some(text.clone())
            }
        }
        serde_json::Value::Array(arr) => {
            let mut cleaned = serde_json::Value::Array(arr.clone());
            truncate_gemini_large_data(&mut cleaned);
            if let Ok(json) = serde_json::to_string_pretty(&cleaned) {
                let trimmed = json.trim();
                if trimmed.is_empty() || trimmed == "[]" {
                    None
                } else {
                    Some(json)
                }
            } else {
                None
            }
        }
        serde_json::Value::Object(map) => {
            let mut cleaned = serde_json::Value::Object(map.clone());
            truncate_gemini_large_data(&mut cleaned);
            if let Ok(json) = serde_json::to_string_pretty(&cleaned) {
                let trimmed = json.trim();
                if trimmed.is_empty() || trimmed == "{}" {
                    None
                } else {
                    Some(json)
                }
            } else {
                None
            }
        }
        _ => Some(value.to_string()),
    }
}

fn render_gemini_inline_data_placeholder(value: &serde_json::Value) -> String {
    let mime_type = value
        .as_object()
        .and_then(|map| map.get("mimeType"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let data_chars = value
        .as_object()
        .and_then(|map| map.get("data"))
        .and_then(|value| value.as_str())
        .map(|data| data.len());

    match data_chars {
        Some(count) => {
            format!("[inlineData omitted: mimeType={mime_type}, data_chars={count}]")
        }
        None => format!("[inlineData omitted: mimeType={mime_type}]"),
    }
}

fn render_gemini_file_data_placeholder(value: &serde_json::Value) -> String {
    let mime_type = value
        .as_object()
        .and_then(|map| map.get("mimeType"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let uri = value
        .as_object()
        .and_then(|map| map.get("fileUri").or_else(|| map.get("uri")))
        .and_then(|value| value.as_str());

    match uri {
        Some(uri) if !uri.is_empty() => {
            format!("[fileData omitted: mimeType={mime_type}, uri={uri}]")
        }
        _ => format!("[fileData omitted: mimeType={mime_type}]"),
    }
}

fn infer_project_hint_from_gemini_message(message: &GeminiMessage) -> Option<String> {
    message
        .content
        .as_ref()
        .and_then(infer_project_hint_from_json_value)
        .or_else(|| {
            message
                .display_content
                .as_ref()
                .and_then(infer_project_hint_from_json_value)
        })
        .or_else(|| {
            render_gemini_message_content(message)
                .as_deref()
                .and_then(infer_project_hint_from_text)
        })
}

fn infer_gemini_project_hint_from_session_path(path: &Path) -> Option<String> {
    let chats_dir = path.parent()?;
    if chats_dir.file_name().and_then(|name| name.to_str()) != Some("chats") {
        return None;
    }

    let project_dir = chats_dir.parent()?;
    let project = project_dir.file_name()?.to_string_lossy();
    normalize_project_hint(&project)
}

fn project_filter_matches_identity(
    organization: &str,
    repository: &str,
    filters: &[String],
) -> bool {
    filters.is_empty()
        || filters
            .iter()
            .any(|filter| project_filter_matches(organization, repository, filter))
}

/// Detect Windows-style absolute paths so Tier 1 canonical resolution
/// also fires for `C:\Users\...` / `C:/Users/...` / UNC `\\server\share`
/// shapes. Without this, Windows local checkouts where the canonical
/// `(owner, repository)` lives only in `.git/config` (not in path
/// segments) silently fall through to Tier 3 segment-adjacency matching
/// and `-p owner/repo` filters cannot reach them.
///
/// Surfaced by `chatgpt-codex-connector` P1 on PR #8 at src/sources.rs:1069.
fn is_windows_absolute_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    // Drive-letter form: `C:\…` or `C:/…` (`X:` for any ASCII letter).
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    // UNC form: `\\server\share…`.
    s.starts_with("\\\\")
}

fn canonical_project_parts(value: &str) -> Option<(String, String)> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.contains(":\\")
    {
        return None;
    }

    let mut parts = trimmed.split(['/', '\\']);
    let organization = parts
        .next()
        .map(str::trim)
        .filter(|part| !part.is_empty())?;
    let repository = parts
        .next()
        .map(str::trim)
        .filter(|part| !part.is_empty())?;
    if parts.next().is_some() {
        return None;
    }

    Some((organization.to_string(), repository.to_string()))
}

/// Check if any project filter matches the given path or canonical identity.
///
/// Resolution tiers (in order):
///
/// 1. **Canonical identity from `aicx_parser::segmentation::infer_tiered_identity_from_cwd`**
///    — consults `cwd`'s `.git/config` remote URL, then known-layout
///    heuristics, then URL-shape inference. This is the **ground truth**
///    resolver, the same one `semantic_segments` uses to bucket entries.
/// 2. **Literal `owner/repo` canonical string** — for inputs that are
///    already a slash-shaped slug (not a path).
/// 3. **Strict adjacent path segments** — `owner/repo` requires both
///    segments to appear adjacent in the path. `owner/` matches any
///    occurrence of `owner`. `/repo` matches any occurrence of `repo`.
///    Bare `name` matches any segment equal to `name`.
///
/// The previous "tier 3 relax" (path's last segment equals filter's
/// repo, even when the owner is absent from the path) is **gone**.
/// It caused the cross-org leak flagged by `chatgpt-codex-connector` P1
/// on PR #8 (filter `Loctree/aicx` matched `/.../VetCoders/aicx`).
/// The original bug #14 workflow (`-p Loctree/aicx` against
/// `/Users/silver/Git/aicx`) now travels through Tier 1: when the local
/// path has a `.git/config` pointing at `github.com/Loctree/aicx`, the
/// canonical resolver returns `(Loctree, aicx)` and the filter matches
/// honestly — by ground truth, not by path-noise heuristic. For paths
/// with no resolvable canonical identity (no `.git`, no URL shape, no
/// known layout), `-p owner/repo` requires strict adjacency.
fn project_filter_matches_path(cwd: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }

    // Tier 1: ground-truth canonical resolver (git remote → known layout
    // → URL shape). Same machinery `semantic_segments` uses to bucket
    // entries, so the filter and the bucketer cannot disagree.
    //
    // We only invoke Tier 1 when the cwd LOOKS LIKE A REAL CWD — an
    // absolute path, a `~/...` home-relative path, or a URL shape. For
    // anything else (relative path strings, encoded Claude dir names,
    // bare segments) Tier 1's internal `discover_git_root` would walk
    // ancestors that resolve relative to the *test runner's own cwd*,
    // spuriously locking onto the running process's `.git` and
    // reporting THAT remote's identity. Guarding here is cheaper and
    // honester than patching `discover_git_root` at the parser layer.
    let cwd_trimmed = cwd.trim();
    // Windows-shape admission is gated behind `cfg!(windows)`. On
    // non-Windows runners, `Path::ancestors()` over a `C:\...` string
    // resolves the empty trailing component against the test process's
    // own cwd and spuriously locks onto the running git remote — the
    // same Tier-1 leak `resolvable_shape` was designed to prevent for
    // unanchored relative paths. Gating by compile-time platform keeps
    // Windows local-checkout matching (codex P1 src/sources.rs:1069)
    // honest on Windows runners while non-Windows runners cleanly fall
    // through to Tier 3 segment matching.
    let resolvable_shape = cwd_trimmed.starts_with('/')
        || cwd_trimmed.starts_with('~')
        || cwd_trimmed.starts_with("http://")
        || cwd_trimmed.starts_with("https://")
        || cwd_trimmed.starts_with("git@")
        || cwd_trimmed.starts_with("ssh://")
        || cwd_trimmed.starts_with("git://")
        || (cfg!(windows) && is_windows_absolute_path(cwd_trimmed));
    if resolvable_shape
        && let Some(tiered) = aicx_parser::segmentation::infer_tiered_identity_from_cwd(Some(cwd))
    {
        return project_filter_matches_identity(
            &tiered.identity.organization,
            &tiered.identity.repository,
            filters,
        );
    }

    // Tier 2: cwd is already a literal `owner/repo` slug (not a path).
    if let Some((organization, repository)) = canonical_project_parts(cwd) {
        return project_filter_matches_identity(&organization, &repository, filters);
    }

    // Tier 3: strict path-segment matching, no last-segment relax.
    let path_segments: Vec<String> = cwd
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_ascii_lowercase())
        .collect();
    if path_segments.is_empty() {
        return false;
    }
    filters.iter().any(|filter| {
        let filter = filter.trim().to_ascii_lowercase();
        if filter.is_empty() {
            return false;
        }

        if let Some(repo_only) = filter.strip_prefix('/') {
            return !repo_only.is_empty()
                && !repo_only.contains('/')
                && path_segments.iter().any(|segment| segment == repo_only);
        }

        if let Some(owner_only) = filter.strip_suffix('/') {
            return !owner_only.is_empty()
                && !owner_only.contains('/')
                && path_segments.iter().any(|segment| segment == owner_only);
        }

        if filter.contains('/') {
            let mut parts = filter.split('/');
            let Some(owner) = parts.next().filter(|part| !part.is_empty()) else {
                return false;
            };
            let Some(repo) = parts.next().filter(|part| !part.is_empty()) else {
                return false;
            };
            if parts.next().is_some() {
                return false;
            }
            // Strict adjacency — no last-segment relax. Cross-org leak
            // (filter `Loctree/aicx` matching `/.../VetCoders/aicx`)
            // is impossible in this branch. For a local checkout to
            // match by canonical identity, Tier 1 must resolve it via
            // `.git/config` remote URL.
            return path_segments
                .windows(2)
                .any(|pair| pair[0] == owner && pair[1] == repo);
        }

        path_segments.iter().any(|segment| segment == &filter)
    })
}

/// Token-aware "does this body mention `repo` as a whole word?" check.
///
/// Splits the (already-lowercased) body on runs of non-identifier
/// characters — alphanumerics, `-`, and `_` count as in-word, everything
/// else is a separator. Returns true iff some resulting token equals
/// `repo_lower` byte-for-byte.
///
/// This is used by codescribe project-hint inference (sources.rs ~4943):
/// the prior `lower_body.contains(&repo)` substring match was flagged in
/// the gemini-code-assist MEDIUM PR #8 review for re-introducing a
/// suffix-leak (`-p vista` matched bodies mentioning `vista-portal`).
/// Token-equality is consistent with the rest of pass-4's strict
/// identity matchers (e.g. `project_filter_matches`).
fn body_mentions_repo_token(body_lower: &str, repo_lower: &str) -> bool {
    if repo_lower.is_empty() {
        return false;
    }
    body_lower
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .any(|token| token == repo_lower)
}

/// Soft pre-filter on Claude project directory names.
///
/// The Claude on-disk encoding is the original cwd with `/` replaced by
/// `-` (optionally prefixed with a leading `-` for absolute paths). The
/// encoding is **inherently lossy** — given `-Users-silver-Git-vista` we
/// cannot recover whether the original was `/Users/silver/Git/vista`
/// (single-segment repo `vista`) or `/Users/silver/Git/nextra-docs-vista`
/// or `/Users-silver/Git/vista`, etc.
///
/// This function therefore acts as a **permissive pre-filter** at the
/// directory-listing stage. Three legal match shapes for a bare `repo`
/// filter:
///
///   1. Exact: `dir_name == filter` (single-component repo, no prefix).
///   2. Leading-dash exact: `dir_name == "-{filter}"` (single-component
///      repo encoded as absolute path).
///   3. Last-chunk: `dir_name.ends_with("-{filter}")` (filter looks like
///      a plausible last cwd segment).
///
/// For `owner/repo` filters (containing `/`), we defer to the strict
/// path-shaped matcher on a best-effort decoded form.
///
/// Case 3 reintroduces a controlled ambiguity — `-p vista` will match
/// both `-Users-silver-Git-vista` (correct) **and**
/// `-Users-silver-Git-nextra-docs-vista` (likely not what the operator
/// meant). That ambiguity is **inherent** to the Claude encoding and
/// cannot be resolved from the directory name alone. The strict
/// per-entry `cwd` filter inside `extract_claude` (sources.rs ~1587,
/// `entry.cwd.as_deref().is_some_and(|c| project_filter_matches_path(c, ...))`)
/// resolves it precisely when the entry carries a `cwd` field — the
/// common case. Sessions without `cwd` were previously dropped by the
/// over-restrictive equality check; this relaxation keeps them reachable
/// so the strict per-entry pass can decide.
///
/// History: the previous `decoded.eq_ignore_ascii_case(filter) ||
/// dir_name.eq_ignore_ascii_case(filter)` required the filter to be the
/// **whole** encoded dir name (e.g. `-p Users-silver-Git-aicx`) instead
/// of just the repo name. That broke every existing `-p reponame`
/// invocation against absolute-path Claude dirs and was the regression
/// flagged by gemini-code-assist HIGH + chatgpt-codex-connector P1
/// comments on PR #8.
fn claude_project_dir_matches_filter(dir_name: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }

    filters.iter().any(|filter| {
        let filter = filter.trim();
        if filter.is_empty() {
            return false;
        }

        if filter.contains('/') {
            // owner/repo or /repo: defer to the strict path matcher on a
            // best-effort decoded form. Decode is lossy (hyphens vs
            // slashes ambiguous) but the path matcher's adjacency
            // semantics tolerate that; the per-entry cwd filter
            // downstream catches the strict case.
            let decoded = decode_claude_project_path(dir_name);
            return project_filter_matches_path(&decoded, &[filter.to_string()]);
        }

        let needle_lower = filter.to_ascii_lowercase();
        let dir_lower = dir_name.to_ascii_lowercase();

        // Shape 1: exact (no leading dash, single-component).
        if dir_lower == needle_lower {
            return true;
        }
        // Shape 2: leading-dash exact (single-component as absolute).
        let dash_exact = format!("-{needle_lower}");
        if dir_lower == dash_exact {
            return true;
        }
        // Shape 3: last-`-`-chunk match (plausible last cwd segment).
        dir_lower.ends_with(&dash_exact)
    })
}

fn normalize_gemini_role(raw: &str) -> Option<&'static str> {
    match raw.to_ascii_lowercase().as_str() {
        "user" | "human" | "prompt" => Some("user"),
        "gemini" | "assistant" | "model" | "ai" => Some("assistant"),
        "thinking" | "reasoning" => Some("reasoning"),
        "tool" | "tool_call" | "tool_result" | "function_call" => Some("tool"),
        "system" | "info" | "error" => Some("system"),
        _ => None,
    }
}

fn gemini_base_role(message: &GeminiMessage) -> Option<&'static str> {
    message
        .role
        .as_deref()
        .and_then(normalize_gemini_role)
        .or_else(|| {
            message
                .msg_type
                .as_deref()
                .and_then(normalize_gemini_role)
                .or_else(|| match message.msg_type.as_deref().unwrap_or("user") {
                    "user" => Some("user"),
                    "gemini" => Some("assistant"),
                    _ => None,
                })
        })
}

fn render_gemini_function_call_part(part: &serde_json::Map<String, serde_json::Value>) -> String {
    part.get("functionCall")
        .map(render_json_inline)
        .unwrap_or_else(|| render_json_inline(&serde_json::Value::Object(part.clone())))
}

fn extract_gemini_classified_blocks(
    value: &serde_json::Value,
    base_role: &str,
) -> Vec<ClassifiedFrameBlock> {
    fn from_value(
        value: &serde_json::Value,
        base_role: &str,
        blocks: &mut Vec<ClassifiedFrameBlock>,
    ) {
        match value {
            serde_json::Value::Null => {}
            serde_json::Value::String(text) => {
                if let Some(frame_kind) = frame_kind_from_role(base_role) {
                    push_classified_block(blocks, base_role, frame_kind, text.clone());
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    from_value(item, base_role, blocks);
                }
            }
            serde_json::Value::Object(map) => {
                if let Some(parts) = map.get("parts").or_else(|| map.get("content")) {
                    from_value(parts, base_role, blocks);
                    return;
                }

                if map.contains_key("functionCall") {
                    push_classified_block(
                        blocks,
                        role_for_frame_kind(FrameKind::ToolCall),
                        FrameKind::ToolCall,
                        render_gemini_function_call_part(map),
                    );
                    return;
                }

                if map.get("thought").and_then(|value| value.as_bool()) == Some(true)
                    || map.contains_key("thoughtSignature")
                {
                    let thought_text = map
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            render_gemini_content_value(&serde_json::Value::Object(map.clone()))
                        })
                        .unwrap_or_else(|| {
                            render_json_inline(&serde_json::Value::Object(map.clone()))
                        });
                    push_classified_block(
                        blocks,
                        role_for_frame_kind(FrameKind::InternalThought),
                        FrameKind::InternalThought,
                        thought_text,
                    );
                    return;
                }

                if let Some(text) = map.get("text").and_then(|value| value.as_str())
                    && let Some(frame_kind) = frame_kind_from_role(base_role)
                {
                    push_classified_block(blocks, base_role, frame_kind, text.to_string());
                    return;
                }

                if let Some(frame_kind) = frame_kind_from_role(base_role)
                    && let Some(text) =
                        render_gemini_content_value(&serde_json::Value::Object(map.clone()))
                {
                    push_classified_block(blocks, base_role, frame_kind, text);
                }
            }
            _ => {
                if let Some(frame_kind) = frame_kind_from_role(base_role) {
                    push_classified_block(blocks, base_role, frame_kind, value.to_string());
                }
            }
        }
    }

    let mut blocks = Vec::new();
    from_value(value, base_role, &mut blocks);
    blocks
}

#[derive(Debug, Clone)]
struct GeminiAntigravityInput {
    conversation_id: String,
    input_path: PathBuf,
    brain_dir: PathBuf,
    raw_pb_path: Option<PathBuf>,
}

#[derive(Debug)]
struct GeminiAntigravityRecovery {
    entries: Vec<TimelineEntry>,
    used_paths: Vec<PathBuf>,
    mode: GeminiAntigravityRecoveryMode,
}

fn describe_content_sanitization_warning(warning: &sanitize::ContentSanitizationWarning) -> String {
    match warning {
        sanitize::ContentSanitizationWarning::NullByteStripped(offset) => {
            format!("stripped NUL byte at byte offset {offset}")
        }
        sanitize::ContentSanitizationWarning::BidiOverride(ch, offset) => format!(
            "preserved bidi override U+{:04X} at byte offset {}",
            *ch as u32, offset
        ),
        sanitize::ContentSanitizationWarning::ZeroWidth(ch, offset) => format!(
            "preserved zero-width character U+{:04X} at byte offset {}",
            *ch as u32, offset
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaudeSessionWarning {
    MissingSessionId {
        fallback: String,
    },
    SessionIdDrift {
        first: String,
        ignored: Vec<String>,
    },
    UnparsableTimestamp {
        count: usize,
        samples: Vec<String>,
    },
    FallbackTimestamp {
        count: usize,
        samples: Vec<String>,
    },
    InvalidEpochMillis {
        count: usize,
        samples: Vec<String>,
    },
    OversizedLine {
        count: usize,
        samples: Vec<String>,
    },
    ContentSanitization {
        warning: sanitize::ContentSanitizationWarning,
    },
}

impl ClaudeSessionWarning {
    fn describe(&self, path: &Path) -> String {
        match self {
            ClaudeSessionWarning::MissingSessionId { fallback } => format!(
                "Claude session warning: {} has no non-empty sessionId; using `{}` fallback",
                path.display(),
                fallback
            ),
            ClaudeSessionWarning::SessionIdDrift { first, ignored } => format!(
                "Claude session warning: {} has multiple sessionId values; using `{}` and ignoring {}",
                path.display(),
                first,
                ignored.join(", ")
            ),
            ClaudeSessionWarning::UnparsableTimestamp { count, samples } => format!(
                "Claude session warning: {} has {} unparsable timestamp(s); frames dropped. Sample(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            ClaudeSessionWarning::FallbackTimestamp { count, samples } => format!(
                "Claude session warning: {} has {} frames preserved with fallback timestamp; sample lines: {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            ClaudeSessionWarning::InvalidEpochMillis { count, samples } => format!(
                "Claude history warning: {} has {} invalid epoch millisecond timestamp(s); frames dropped. Sample(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            ClaudeSessionWarning::OversizedLine { count, samples } => format!(
                "Claude session warning: {} skipped {} oversized JSONL line(s) over {} bytes. Sample(s): {}",
                path.display(),
                count,
                MAX_LINE_BYTES,
                samples.join(", ")
            ),
            ClaudeSessionWarning::ContentSanitization { warning } => format!(
                "Claude content warning: {} {}",
                path.display(),
                describe_content_sanitization_warning(warning)
            ),
        }
    }
}

fn emit_claude_session_warnings(path: &Path, warnings: &[ClaudeSessionWarning]) {
    use crate::diagnostics::{self, DiagnosticKind};
    for warning in warnings {
        let line = warning.describe(path);
        diagnostics::log_describe(&line);
        match warning {
            ClaudeSessionWarning::FallbackTimestamp { count, .. } => {
                diagnostics::record("claude", DiagnosticKind::FallbackTimestamp, *count, path);
            }
            ClaudeSessionWarning::UnparsableTimestamp { count, .. } => {
                diagnostics::record("claude", DiagnosticKind::UnparsableTimestamp, *count, path);
            }
            ClaudeSessionWarning::InvalidEpochMillis { count, .. } => {
                diagnostics::record("claude", DiagnosticKind::InvalidEpochMillis, *count, path);
            }
            ClaudeSessionWarning::OversizedLine { count, .. } => {
                diagnostics::record("claude", DiagnosticKind::OversizedLine, *count, path);
            }
            ClaudeSessionWarning::MissingSessionId { .. } => {
                diagnostics::record("claude", DiagnosticKind::MissingSessionId, 1, path);
            }
            ClaudeSessionWarning::SessionIdDrift { .. } => {
                diagnostics::record("claude", DiagnosticKind::SessionIdDrift, 1, path);
            }
            ClaudeSessionWarning::ContentSanitization { warning } => {
                record_content_sanitization("claude", warning, path);
            }
        }
        if diagnostics::is_verbose() {
            eprintln!("{line}");
        }
    }
}

fn record_content_sanitization(
    extractor: &'static str,
    warning: &sanitize::ContentSanitizationWarning,
    path: &Path,
) {
    use crate::diagnostics::{self, DiagnosticKind};
    let kind = match warning {
        sanitize::ContentSanitizationWarning::BidiOverride(_, _) => DiagnosticKind::BidiOverride,
        sanitize::ContentSanitizationWarning::ZeroWidth(_, _) => DiagnosticKind::ZeroWidth,
        sanitize::ContentSanitizationWarning::NullByteStripped(_) => {
            DiagnosticKind::NullByteStripped
        }
    };
    diagnostics::record(extractor, kind, 1, path);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GeminiSessionWarning {
    MissingSessionId {
        fallback: String,
    },
    SessionIdDrift {
        first: String,
        ignored: Vec<String>,
    },
    UnparsableTimestamp {
        count: usize,
        samples: Vec<String>,
    },
    UnknownMsgType {
        count: usize,
        samples: Vec<String>,
    },
    ContentSanitization {
        warning: sanitize::ContentSanitizationWarning,
    },
}

impl GeminiSessionWarning {
    fn describe(&self, path: &Path) -> String {
        match self {
            GeminiSessionWarning::MissingSessionId { fallback } => format!(
                "Gemini session warning: {} has no non-empty sessionId; using `{}` fallback",
                path.display(),
                fallback
            ),
            GeminiSessionWarning::SessionIdDrift { first, ignored } => format!(
                "Gemini session warning: {} has multiple sessionId values; using `{}` and ignoring {}",
                path.display(),
                first,
                ignored.join(", ")
            ),
            GeminiSessionWarning::UnparsableTimestamp { count, samples } => format!(
                "Gemini session warning: {} has {} unparsable timestamp(s); frames dropped or fell back to parent timestamp. Sample(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            GeminiSessionWarning::UnknownMsgType { count, samples } => format!(
                "Gemini session warning: {} encountered {} message(s) with unrecognized type/role; preserved as system_note. Sample(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            GeminiSessionWarning::ContentSanitization { warning } => format!(
                "Gemini content warning: {} {}",
                path.display(),
                describe_content_sanitization_warning(warning)
            ),
        }
    }
}

fn emit_gemini_session_warnings(path: &Path, warnings: &[GeminiSessionWarning]) {
    use crate::diagnostics::{self, DiagnosticKind};
    for warning in warnings {
        let line = warning.describe(path);
        diagnostics::log_describe(&line);
        match warning {
            GeminiSessionWarning::UnparsableTimestamp { count, .. } => {
                diagnostics::record("gemini", DiagnosticKind::UnparsableTimestamp, *count, path);
            }
            GeminiSessionWarning::UnknownMsgType { count, .. } => {
                diagnostics::record("gemini", DiagnosticKind::UnknownMsgType, *count, path);
            }
            GeminiSessionWarning::MissingSessionId { .. } => {
                diagnostics::record("gemini", DiagnosticKind::MissingSessionId, 1, path);
            }
            GeminiSessionWarning::SessionIdDrift { .. } => {
                diagnostics::record("gemini", DiagnosticKind::SessionIdDrift, 1, path);
            }
            GeminiSessionWarning::ContentSanitization { warning } => {
                record_content_sanitization("gemini", warning, path);
            }
        }
        if diagnostics::is_verbose() {
            eprintln!("{line}");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JunieSessionWarning {
    JunieFallbackId {
        fallback: String,
    },
    OversizedLine {
        count: usize,
        samples: Vec<String>,
    },
    ContentSanitization {
        warning: sanitize::ContentSanitizationWarning,
    },
}

impl JunieSessionWarning {
    fn describe(&self, path: &Path) -> String {
        match self {
            JunieSessionWarning::JunieFallbackId { fallback } => format!(
                "Junie session warning: {} has no session-* ancestor; using `{}` fallback",
                path.display(),
                fallback
            ),
            JunieSessionWarning::OversizedLine { count, samples } => format!(
                "Junie session warning: {} skipped {} oversized JSONL line(s) over {} bytes. Sample(s): {}",
                path.display(),
                count,
                MAX_LINE_BYTES,
                samples.join(", ")
            ),
            JunieSessionWarning::ContentSanitization { warning } => format!(
                "Junie content warning: {} {}",
                path.display(),
                describe_content_sanitization_warning(warning)
            ),
        }
    }
}

fn emit_junie_session_warnings(path: &Path, warnings: &[JunieSessionWarning]) {
    use crate::diagnostics::{self, DiagnosticKind};
    for warning in warnings {
        let line = warning.describe(path);
        diagnostics::log_describe(&line);
        match warning {
            JunieSessionWarning::JunieFallbackId { .. } => {
                diagnostics::record("junie", DiagnosticKind::JunieFallbackId, 1, path);
            }
            JunieSessionWarning::OversizedLine { count, .. } => {
                diagnostics::record("junie", DiagnosticKind::OversizedLine, *count, path);
            }
            JunieSessionWarning::ContentSanitization { warning } => {
                record_content_sanitization("junie", warning, path);
            }
        }
        if diagnostics::is_verbose() {
            eprintln!("{line}");
        }
    }
}

// ============================================================================
// Claude Code extractor
// ============================================================================

/// Extract timeline entries from Claude Code session files.
///
/// Reads `~/.claude/projects/<project_dir>/<uuid>.jsonl` files.
/// Uses filename stem (UUID) as session_id for consistency.
pub fn extract_claude(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let claude_dir = dirs::home_dir()
        .context("No home dir")?
        .join(".claude")
        .join("projects");

    if !claude_dir.exists() {
        return Ok(vec![]);
    }

    let mut entries: Vec<TimelineEntry> = Vec::new();

    for dir_entry in fs::read_dir(&claude_dir)? {
        let dir_entry = dir_entry?;
        let dir_name = dir_entry.file_name().to_string_lossy().to_string();

        let project_dir = dir_entry.path();
        if !project_dir.is_dir() {
            continue;
        }

        // Determine if directory name inherently matches the filter
        let dir_matches = claude_project_dir_matches_filter(&dir_name, &config.project_filter);

        for file_entry in fs::read_dir(&project_dir)? {
            let file_entry = file_entry?;
            let path = file_entry.path();

            if path.extension().is_some_and(|e| e == "jsonl") {
                let session_id = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();

                let session_entries = parse_claude_jsonl(&path, &session_id, config)?;

                // If directory name matched, keep all entries.
                // Otherwise, check if ANY entry in this session matches the project filter.
                let keep_session = dir_matches
                    || (!config.project_filter.is_empty()
                        && session_entries.iter().any(|entry| {
                            entry.cwd.as_deref().is_some_and(|c| {
                                project_filter_matches_path(c, &config.project_filter)
                            })
                        }));

                if keep_session {
                    entries.extend(session_entries);
                }
            }
        }
    }

    // Merge claude history.jsonl entries
    match extract_claude_history(config) {
        Ok(hist) => entries.extend(hist),
        Err(e) => eprintln!("Claude history extraction warning: {}", e),
    }

    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

/// Parse a single Claude JSONL file into timeline entries.
fn parse_claude_jsonl(
    path: &std::path::Path,
    session_id: &str,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let (entries, warnings) = parse_claude_jsonl_with_diagnostics(path, session_id, config)?;
    emit_claude_session_warnings(path, &warnings);
    Ok(entries)
}

fn parse_claude_jsonl_with_diagnostics(
    path: &std::path::Path,
    default_session_id: &str,
    config: &ExtractionConfig,
) -> Result<(Vec<TimelineEntry>, Vec<ClaudeSessionWarning>)> {
    let file = sanitize::open_file_validated(path)?;
    let file_mtime = file
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(DateTime::<Utc>::from);
    let mut reader = BufReader::new(file);
    let mut raw_entries = Vec::new();
    let mut warnings = Vec::new();
    let mut oversized_count = 0usize;
    let mut oversized_samples = Vec::new();
    let mut unparsable_ts_count = 0usize;
    let mut unparsable_ts_samples = Vec::new();
    let mut fallback_ts_count = 0usize;
    let mut fallback_ts_samples = Vec::new();
    let mut line_number = 0usize;

    while let Some(limited) = sanitize::read_line_capped(&mut reader, MAX_LINE_BYTES)? {
        line_number += 1;
        if limited.exceeded {
            observe_oversized_line(&mut oversized_count, &mut oversized_samples, line_number);
            continue;
        }
        let line = limited.line;
        if line.trim().is_empty() {
            continue;
        }

        let entry: ClaudeEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !matches!(
            entry.entry_type.as_str(),
            "user" | "assistant" | "tool_use" | "tool_result"
        ) {
            continue;
        }
        raw_entries.push(ClaudeRawEntry { entry, line_number });
    }

    let effective_session_id = select_claude_session_id(
        raw_entries.iter().map(|raw| &raw.entry),
        default_session_id,
        &mut warnings,
    );
    let mut entries = Vec::new();
    let mut previous_timestamp = None;
    for raw in raw_entries {
        let (timestamp, timestamp_source) = match raw.entry.timestamp.as_deref() {
            Some(ts) => match parse_rfc3339_or_naive_utc(ts) {
                Ok(timestamp) => {
                    let timestamp = timestamp.with_timezone(&Utc);
                    previous_timestamp = Some(timestamp);
                    (timestamp, None)
                }
                Err(_) => {
                    unparsable_ts_count += 1;
                    push_unique_sample(&mut unparsable_ts_samples, ts.to_string(), 5);
                    continue;
                }
            },
            None => {
                let (timestamp, source) = if let Some(timestamp) = previous_timestamp {
                    (timestamp, "fallback_previous")
                } else if let Some(timestamp) = file_mtime {
                    (timestamp, "fallback_file_mtime")
                } else {
                    (Utc::now(), "fallback_now")
                };
                (timestamp, Some(source))
            }
        };

        let line_entries = extract_claude_line_entries(
            raw.entry,
            timestamp,
            timestamp_source,
            &effective_session_id,
            config,
            &mut warnings,
        );
        if !line_entries.is_empty()
            && let Some(source) = timestamp_source
        {
            fallback_ts_count += line_entries.len();
            if fallback_ts_samples.len() < 5 {
                fallback_ts_samples
                    .push(format!("line {}: <missing> -> {source}", raw.line_number));
            }
        }
        entries.extend(line_entries);
    }

    if oversized_count > 0 {
        warnings.push(ClaudeSessionWarning::OversizedLine {
            count: oversized_count,
            samples: oversized_samples,
        });
    }
    if fallback_ts_count > 0 {
        warnings.push(ClaudeSessionWarning::FallbackTimestamp {
            count: fallback_ts_count,
            samples: fallback_ts_samples,
        });
    }
    if unparsable_ts_count > 0 {
        warnings.push(ClaudeSessionWarning::UnparsableTimestamp {
            count: unparsable_ts_count,
            samples: unparsable_ts_samples,
        });
    }

    Ok((entries, warnings))
}

/// Extract timeline entries from a single Claude JSONL-like file by path.
///
/// This is intentionally a "direct file" extractor used by:
/// `aicx extract --format claude <path> -o <out.md>`
///
/// Unlike `extract_claude()`, this does not require the file to live under
/// `~/.claude/projects/**` nor to have a `.jsonl` extension (Claude task outputs
/// often end with `.output` but are still JSONL).
pub fn extract_claude_file(path: &Path, config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let default_session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let (mut entries, warnings) =
        parse_claude_jsonl_with_diagnostics(path, &default_session_id, config)?;
    emit_claude_session_warnings(path, &warnings);

    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

/// Extract timeline entries from a single Codex JSONL file by path.
///
/// Supports both:
/// - Codex history format (`~/.codex/history.jsonl`) — `CodexEntry` per line.
/// - Codex session format (`~/.codex/sessions/**/**/*.jsonl`) — `CodexSessionEvent` per line.
pub fn extract_codex_file(path: &Path, config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let (entries, warnings) = parse_codex_file_with_diagnostics(path, config)?;
    emit_codex_session_warnings(path, &warnings);
    Ok(entries)
}

fn parse_codex_file_with_diagnostics(
    path: &Path,
    config: &ExtractionConfig,
) -> Result<(Vec<TimelineEntry>, Vec<CodexSessionWarning>)> {
    let file = sanitize::open_file_validated(path)?;
    let mut reader = BufReader::new(file);
    let mut history_records = Vec::new();
    let mut session_events = Vec::new();
    let mut warnings = Vec::new();
    let mut first_non_empty: Option<String> = None;
    let mut oversized_count = 0usize;
    let mut oversized_samples = Vec::new();
    let mut line_number = 0usize;

    while let Some(limited) = sanitize::read_line_capped(&mut reader, MAX_LINE_BYTES)? {
        line_number += 1;
        if limited.exceeded {
            observe_oversized_line(&mut oversized_count, &mut oversized_samples, line_number);
            continue;
        }
        let line = limited.line;
        if line.trim().is_empty() {
            continue;
        }
        if first_non_empty.is_none() {
            first_non_empty = Some(line.clone());
        }
        match serde_json::from_str::<CodexEntry>(&line) {
            Ok(entry) => {
                history_records.push(entry);
                continue;
            }
            Err(_) => match serde_json::from_str::<CodexSessionEvent>(&line) {
                Ok(event) => session_events.push(event),
                Err(_) => warnings.push(codex_line_parse_error(line_number, &line)),
            },
        }
    }

    if oversized_count > 0 {
        warnings.push(CodexSessionWarning::OversizedLine {
            count: oversized_count,
            samples: oversized_samples,
        });
    }

    let Some(first_line) = first_non_empty else {
        return Ok((vec![], warnings));
    };

    if !history_records.is_empty() || !session_events.is_empty() {
        if !history_records.is_empty() && !session_events.is_empty() {
            let history_first = serde_json::from_str::<CodexEntry>(&first_line).is_ok();
            let minority_count = if history_first {
                session_events.len()
            } else {
                history_records.len()
            };
            warnings.push(CodexSessionWarning::MixedFormat {
                count: minority_count,
                samples: vec![if history_first {
                    "session records after history first line".to_string()
                } else {
                    "history records after session first line".to_string()
                }],
            });
        }

        let mut entries = build_codex_history_entries(&history_records, config, &mut warnings);
        if !session_events.is_empty() {
            let (mut session_entries, session_warnings) =
                parse_codex_session_events_with_diagnostics(path, &session_events, config);
            warnings.extend(session_warnings);
            entries.append(&mut session_entries);
        }
        entries.sort_by_key(|a| a.timestamp);
        return Ok((entries, warnings));
    }

    // Check for legacy JSON format ({"session": {...}, "items": [...]})
    // We read the full file because it's usually formatted JSON.
    if let Ok(content) = sanitize::read_to_string_validated(path)
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&content)
        && val.get("session").is_some()
        && val.get("items").is_some()
    {
        anyhow::bail!(
            "Legacy Codex JSON rollout format is unsupported (no cwd available): {}",
            path.display()
        );
    }

    Err(anyhow::anyhow!(
        "Unsupported codex file format: {}",
        path.display()
    ))
}

/// Extract timeline entries from a single Gemini CLI session file by path.
///
/// Gemini sessions live under:
/// `~/.gemini/tmp/<project>/chats/session-*.json[l]`
pub fn extract_gemini_file(path: &Path, config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let mut entries = parse_gemini_session(path, config)?;
    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

/// Extract timeline entries from a Gemini Antigravity conversation.
///
/// Supported inputs:
/// - `~/.gemini/antigravity/conversations/<uuid>.pb`
/// - `~/.gemini/antigravity/brain/<uuid>/`
///
/// The `.pb` file remains opaque provenance. Readable extraction happens from
/// the sibling `brain/<uuid>/` directory.
pub fn extract_gemini_antigravity_file(
    path: &Path,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let input = resolve_gemini_antigravity_input(path)?;

    let mut recovery = match extract_gemini_antigravity_conversation_artifacts(&input, config)? {
        Some(recovery) => recovery,
        None => extract_gemini_antigravity_step_outputs(&input, config)?,
    };

    let summary = build_gemini_antigravity_summary(&input, &recovery, &recovery.entries);
    let mut entries = std::mem::take(&mut recovery.entries);
    if entries.is_empty() {
        return Ok(entries);
    }

    entries.push(summary);
    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

fn resolve_gemini_antigravity_input(path: &Path) -> Result<GeminiAntigravityInput> {
    let input_path = sanitize::validate_read_path(path)?;

    if input_path.is_dir() {
        let brain_dir = sanitize::validate_dir_path(&input_path)?;
        let conversation_id = brain_dir
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .context("Gemini Antigravity brain path is missing a conversation id")?;

        return Ok(GeminiAntigravityInput {
            raw_pb_path: discover_antigravity_pb_for_brain(&brain_dir, &conversation_id),
            conversation_id,
            input_path,
            brain_dir,
        });
    }

    if input_path.extension().is_none_or(|ext| ext != "pb") {
        anyhow::bail!(
            "Gemini Antigravity input must be a conversations/<uuid>.pb file or brain/<uuid>/ directory: {}",
            input_path.display()
        );
    }

    let conversation_id = input_path
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .context("Gemini Antigravity .pb file is missing a conversation id")?;
    let candidate_paths = antigravity_brain_candidates(&input_path, &conversation_id);
    let brain_dir = candidate_paths
        .iter()
        .find(|candidate| candidate.exists() && candidate.is_dir())
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Gemini Antigravity .pb files are opaque/encrypted and require a readable sibling brain/{id}/ directory. Looked for: {}",
                candidate_paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                id = conversation_id
            )
        })?;

    Ok(GeminiAntigravityInput {
        conversation_id,
        raw_pb_path: Some(input_path.clone()),
        input_path,
        brain_dir: sanitize::validate_dir_path(&brain_dir)?,
    })
}

fn antigravity_brain_candidates(pb_path: &Path, conversation_id: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(conversations_dir) = pb_path.parent()
        && conversations_dir
            .file_name()
            .is_some_and(|name| name == "conversations")
        && let Some(antigravity_root) = conversations_dir.parent()
    {
        candidates.push(antigravity_root.join("brain").join(conversation_id));
    }

    if let Some(home) = dirs::home_dir() {
        let default = home
            .join(".gemini")
            .join("antigravity")
            .join("brain")
            .join(conversation_id);
        if !candidates.iter().any(|candidate| candidate == &default) {
            candidates.push(default);
        }
    }

    candidates
}

fn discover_antigravity_pb_for_brain(brain_dir: &Path, conversation_id: &str) -> Option<PathBuf> {
    let brain_parent = brain_dir.parent()?;
    if brain_parent.file_name().is_some_and(|name| name == "brain") {
        let candidate = brain_parent
            .parent()?
            .join("conversations")
            .join(format!("{conversation_id}.pb"));
        if candidate.exists() {
            return sanitize::validate_read_path(&candidate).ok();
        }
    }
    None
}

fn extract_gemini_antigravity_conversation_artifacts(
    input: &GeminiAntigravityInput,
    config: &ExtractionConfig,
) -> Result<Option<GeminiAntigravityRecovery>> {
    let step_outputs: HashSet<PathBuf> = antigravity_step_output_paths(&input.brain_dir)
        .into_iter()
        .collect();
    let mut used_paths = Vec::new();
    let mut entries = Vec::new();

    for path in walk_files(&input.brain_dir) {
        if step_outputs.contains(&path) || !is_antigravity_conversation_candidate(&path) {
            continue;
        }

        let content = match sanitize::read_to_string_validated(&path) {
            Ok(content) => content,
            Err(_) => continue,
        };

        let mut parsed = parse_antigravity_conversation_artifact(
            &path,
            &input.conversation_id,
            &content,
            config,
        );
        if !parsed.is_empty() {
            used_paths.push(path);
            entries.append(&mut parsed);
        }
    }

    if entries.is_empty() {
        return Ok(None);
    }

    apply_default_project_hint(&mut entries);
    entries.sort_by_key(|a| a.timestamp);

    Ok(Some(GeminiAntigravityRecovery {
        entries,
        used_paths,
        mode: GeminiAntigravityRecoveryMode::ConversationArtifacts,
    }))
}

fn extract_gemini_antigravity_step_outputs(
    input: &GeminiAntigravityInput,
    config: &ExtractionConfig,
) -> Result<GeminiAntigravityRecovery> {
    let step_output_paths = antigravity_step_output_paths(&input.brain_dir);
    if step_output_paths.is_empty() {
        anyhow::bail!(
            "No readable Gemini Antigravity artifacts found under {}. The raw .pb remains opaque and there were no .system_generated/steps/*/output.txt fallbacks.",
            input.brain_dir.display()
        );
    }

    let session_default_cwd = infer_default_project_hint_for_paths(&step_output_paths);
    let mut entries = Vec::new();

    for (index, path) in step_output_paths.iter().enumerate() {
        let content = match sanitize::read_to_string_validated(path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let trimmed = content.trim();
        if trimmed.is_empty() {
            continue;
        }

        let timestamp =
            file_timestamp(path).unwrap_or_else(|| Utc::now() + Duration::seconds(index as i64));
        if timestamp < config.cutoff {
            continue;
        }
        if config.watermark.is_some_and(|wm| timestamp < wm) {
            continue;
        }

        entries.push(build_timeline_entry(
            timestamp,
            "gemini-antigravity",
            &input.conversation_id,
            "artifact",
            format!(
                "Antigravity step output fallback\nsource: {}\nfull_transcript_available: false\n\n{}",
                path.display(),
                trimmed
            ),
            TimelineEntryMeta {
                branch: None,
                cwd: infer_project_hint_from_text(trimmed)
                    .or_else(|| session_default_cwd.clone()),
                frame_kind: None,
                timestamp_source: None,
            },
        ));
    }

    if entries.is_empty() {
        anyhow::bail!(
            "Gemini Antigravity fallback found step outputs under {}, but none produced usable timeline entries.",
            input.brain_dir.display()
        );
    }

    apply_default_project_hint(&mut entries);
    entries.sort_by_key(|a| a.timestamp);

    Ok(GeminiAntigravityRecovery {
        entries,
        used_paths: step_output_paths,
        mode: GeminiAntigravityRecoveryMode::StepOutputFallback,
    })
}

fn antigravity_step_output_paths(brain_dir: &Path) -> Vec<PathBuf> {
    let steps_dir = brain_dir.join(".system_generated").join("steps");
    if !steps_dir.exists() || !steps_dir.is_dir() {
        return Vec::new();
    }

    let mut step_outputs = Vec::new();
    if let Ok(read_dir) = fs::read_dir(&steps_dir) {
        for entry in read_dir.flatten() {
            let step_dir = entry.path();
            if !step_dir.is_dir() {
                continue;
            }

            let output_path = step_dir.join("output.txt");
            if output_path.exists()
                && output_path.is_file()
                && let Ok(validated) = sanitize::validate_read_path(&output_path)
            {
                step_outputs.push(validated);
            }
        }
    }

    step_outputs.sort_by_key(|path| antigravity_step_index(path));
    step_outputs
}

fn antigravity_step_index(path: &Path) -> usize {
    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .and_then(|name| name.parse::<usize>().ok())
        .unwrap_or(usize::MAX)
}

fn is_antigravity_conversation_candidate(path: &Path) -> bool {
    let file_name = match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => name.to_lowercase(),
        None => return false,
    };

    if file_name == ".ds_store" {
        return false;
    }

    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_lowercase());

    if matches!(
        extension.as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "img" | "pb" | "pdf" | "zip")
    ) {
        return false;
    }

    extension.is_some_and(|ext| {
        matches!(
            ext.as_str(),
            "json" | "jsonl" | "md" | "markdown" | "txt" | "log" | "yaml" | "yml"
        )
    }) || [
        "conversation",
        "transcript",
        "dialog",
        "messages",
        "turns",
        "chat",
    ]
    .iter()
    .any(|keyword| file_name.contains(keyword))
}

fn parse_antigravity_conversation_artifact(
    path: &Path,
    session_id: &str,
    content: &str,
    config: &ExtractionConfig,
) -> Vec<TimelineEntry> {
    let default_timestamp = file_timestamp(path).unwrap_or_else(Utc::now);

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(content) {
        let mut entries = collect_antigravity_json_entries(
            &value,
            session_id,
            infer_project_hint_from_json_value(&value).as_deref(),
            default_timestamp,
            config,
        );
        dedup_timeline_entries(&mut entries);
        if !entries.is_empty() {
            return entries;
        }
    }

    let mut entries = parse_antigravity_transcript_text(path, session_id, content, config);
    dedup_timeline_entries(&mut entries);
    entries
}

fn collect_antigravity_json_entries(
    value: &serde_json::Value,
    session_id: &str,
    default_cwd: Option<&str>,
    fallback_timestamp: DateTime<Utc>,
    config: &ExtractionConfig,
) -> Vec<TimelineEntry> {
    let mut entries = Vec::new();
    let mut counter = 0usize;
    collect_antigravity_json_entries_inner(
        value,
        session_id,
        default_cwd,
        fallback_timestamp,
        config,
        &mut counter,
        &mut entries,
    );
    entries
}

fn collect_antigravity_json_entries_inner(
    value: &serde_json::Value,
    session_id: &str,
    default_cwd: Option<&str>,
    fallback_timestamp: DateTime<Utc>,
    config: &ExtractionConfig,
    counter: &mut usize,
    entries: &mut Vec<TimelineEntry>,
) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_antigravity_json_entries_inner(
                    item,
                    session_id,
                    default_cwd,
                    fallback_timestamp,
                    config,
                    counter,
                    entries,
                );
            }
        }
        serde_json::Value::Object(map) => {
            let parsed = antigravity_json_message_to_entries(
                map,
                session_id,
                default_cwd,
                fallback_timestamp + Duration::seconds(*counter as i64),
                config,
            );
            if !parsed.is_empty() {
                *counter += parsed.len();
                entries.extend(parsed);
            }

            for child in map.values() {
                collect_antigravity_json_entries_inner(
                    child,
                    session_id,
                    default_cwd,
                    fallback_timestamp,
                    config,
                    counter,
                    entries,
                );
            }
        }
        _ => {}
    }
}

fn antigravity_json_message_to_entries(
    map: &serde_json::Map<String, serde_json::Value>,
    session_id: &str,
    default_cwd: Option<&str>,
    fallback_timestamp: DateTime<Utc>,
    config: &ExtractionConfig,
) -> Vec<TimelineEntry> {
    let Some(role) = antigravity_role_from_map(map) else {
        return Vec::new();
    };
    let timestamp = ["timestamp", "createdAt", "created_at", "time", "date"]
        .iter()
        .filter_map(|key| map.get(*key))
        .find_map(parse_json_timestamp)
        .unwrap_or(fallback_timestamp);

    if timestamp < config.cutoff {
        return Vec::new();
    }
    if config.watermark.is_some_and(|wm| timestamp < wm) {
        return Vec::new();
    }

    let cwd = infer_project_hint_from_map(map).or_else(|| default_cwd.map(ToOwned::to_owned));
    let content_value = ["content", "text", "message", "body", "value", "output"]
        .iter()
        .filter_map(|key| map.get(*key))
        .next();

    let mut entries = Vec::new();
    if let Some(value) = content_value {
        let mut classified = extract_gemini_classified_blocks(value, role);
        if classified.is_empty()
            && let Some(text) = extract_text_from_json_value(value)
            && let Some(frame_kind) = frame_kind_from_role(role)
        {
            classified.push(ClassifiedFrameBlock {
                role: role.to_string(),
                frame_kind,
                message: text,
            });
        }

        for block in classified {
            if !should_keep_entry(Some(block.frame_kind), config) {
                continue;
            }
            entries.push(build_timeline_entry(
                timestamp,
                "gemini-antigravity",
                session_id,
                &block.role,
                block.message,
                TimelineEntryMeta {
                    branch: None,
                    cwd: cwd.clone(),
                    frame_kind: Some(block.frame_kind),
                    timestamp_source: None,
                },
            ));
        }
    }

    entries
}

fn antigravity_role_from_map(
    map: &serde_json::Map<String, serde_json::Value>,
) -> Option<&'static str> {
    let raw_role = ["role", "speaker", "author", "type", "kind", "from"]
        .iter()
        .filter_map(|key| map.get(*key))
        .find_map(|value| value.as_str())?;

    let normalized = raw_role.to_lowercase();
    if normalized.contains("user") || normalized.contains("human") || normalized == "prompt" {
        Some("user")
    } else if normalized.contains("assistant")
        || normalized.contains("gemini")
        || normalized.contains("model")
        || normalized == "ai"
    {
        Some("assistant")
    } else if normalized.contains("system") {
        Some("system")
    } else {
        None
    }
}

fn extract_text_from_json_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        serde_json::Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(extract_text_from_json_value)
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        serde_json::Value::Object(map) => ["text", "content", "message", "body", "value"]
            .iter()
            .filter_map(|key| map.get(*key))
            .find_map(extract_text_from_json_value),
        _ => None,
    }
}

fn parse_json_timestamp(value: &serde_json::Value) -> Option<DateTime<Utc>> {
    match value {
        serde_json::Value::String(raw) => parse_rfc3339_or_naive_utc(raw).ok(),
        serde_json::Value::Number(number) => {
            let raw = number.as_i64()?;
            if raw > 10_000_000_000 {
                Utc.timestamp_millis_opt(raw).single()
            } else {
                Utc.timestamp_opt(raw, 0).single()
            }
        }
        _ => None,
    }
}

fn parse_antigravity_transcript_text(
    path: &Path,
    session_id: &str,
    content: &str,
    config: &ExtractionConfig,
) -> Vec<TimelineEntry> {
    let default_cwd = infer_project_hint_from_text(content);
    let base_timestamp = file_timestamp(path).unwrap_or_else(Utc::now);
    let mut entries = Vec::new();

    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let (role, message) = if let Some(rest) = trimmed.strip_prefix("User:") {
            ("user", rest.trim())
        } else if let Some(rest) = trimmed.strip_prefix("Assistant:") {
            ("assistant", rest.trim())
        } else if let Some(rest) = trimmed.strip_prefix("Gemini:") {
            ("assistant", rest.trim())
        } else if let Some(rest) = trimmed.strip_prefix("System:") {
            ("system", rest.trim())
        } else {
            continue;
        };

        if role == "assistant" && !config.include_assistant {
            continue;
        }
        if message.is_empty() {
            continue;
        }

        let timestamp = base_timestamp + Duration::seconds(index as i64);
        if timestamp < config.cutoff {
            continue;
        }
        if config.watermark.is_some_and(|wm| timestamp < wm) {
            continue;
        }

        entries.push(build_timeline_entry(
            timestamp,
            "gemini-antigravity",
            session_id,
            role,
            message.to_string(),
            TimelineEntryMeta {
                branch: None,
                cwd: default_cwd.clone(),
                frame_kind: frame_kind_from_role(role),
                timestamp_source: None,
            },
        ));
    }

    entries
}

fn build_gemini_antigravity_summary(
    input: &GeminiAntigravityInput,
    recovery: &GeminiAntigravityRecovery,
    entries: &[TimelineEntry],
) -> TimelineEntry {
    let inferred_projects = repo_labels_from_entries(entries, &[]);
    let inferred_label = if inferred_projects.is_empty() {
        "unknown".to_string()
    } else {
        inferred_projects.join(", ")
    };
    let raw_pb = input
        .raw_pb_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "(not provided)".to_string());
    let used_paths = recovery
        .used_paths
        .iter()
        .map(|path| format!("- {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n");

    build_timeline_entry(
        entries
            .iter()
            .map(|entry| entry.timestamp)
            .min()
            .unwrap_or_else(Utc::now)
            - Duration::seconds(1),
        "gemini-antigravity",
        &input.conversation_id,
        "system",
        format!(
            "Gemini Antigravity recovery report\nmode: {}\nconversation_id: {}\ninput: {}\nbrain: {}\nraw_pb: {}\nreadable_entry_count: {}\ninferred_projects: {}\nrecovery_note: {}\nused_artifacts:\n{}",
            recovery.mode.as_str(),
            input.conversation_id,
            input.input_path.display(),
            input.brain_dir.display(),
            raw_pb,
            entries.len(),
            inferred_label,
            recovery.mode.note(),
            if used_paths.is_empty() {
                "- (none)".to_string()
            } else {
                used_paths
            }
        ),
        TimelineEntryMeta::default(),
    )
}

fn file_timestamp(path: &Path) -> Option<DateTime<Utc>> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
}

fn infer_default_project_hint_for_paths(paths: &[PathBuf]) -> Option<String> {
    let mut hints = Vec::new();
    for path in paths {
        let content = match sanitize::read_to_string_validated(path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        if let Some(hint) = infer_project_hint_from_text(&content) {
            hints.push(hint);
        }
    }
    most_common_project_hint(&hints)
}

fn apply_default_project_hint(entries: &mut [TimelineEntry]) {
    let hints: Vec<String> = entries
        .iter()
        .filter_map(|entry| entry.cwd.clone())
        .collect();
    if let Some(default_hint) = most_common_project_hint(&hints) {
        for entry in entries {
            if entry.cwd.is_none() {
                entry.cwd = Some(default_hint.clone());
            }
        }
    }
}

fn most_common_project_hint(hints: &[String]) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for hint in hints {
        *counts.entry(hint.clone()).or_default() += 1;
    }

    counts
        .into_iter()
        .max_by(|(left_hint, left_count), (right_hint, right_count)| {
            left_count
                .cmp(right_count)
                .then_with(|| right_hint.len().cmp(&left_hint.len()))
                .then_with(|| right_hint.cmp(left_hint))
        })
        .map(|(hint, _)| hint)
}

fn infer_project_hint_from_text(text: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text)
        && let Some(hint) = infer_project_hint_from_json_value(&value)
    {
        return Some(hint);
    }

    let path_re = regex::Regex::new(r"(/[A-Za-z0-9._~\-]+(?:/[A-Za-z0-9._~\-]+)+)").ok()?;
    path_re
        .captures(text)
        .and_then(|captures| captures.get(1))
        .and_then(|capture| normalize_project_hint(capture.as_str()))
}

fn infer_project_hint_from_json_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => infer_project_hint_from_map(map)
            .or_else(|| map.values().find_map(infer_project_hint_from_json_value)),
        serde_json::Value::Array(items) => {
            items.iter().find_map(infer_project_hint_from_json_value)
        }
        _ => None,
    }
}

fn infer_project_hint_from_map(map: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    [
        "project",
        "projectRoot",
        "project_root",
        "cwd",
        "repo",
        "repository",
        "workspace",
        "root",
        "rootPath",
        "workingDirectory",
    ]
    .iter()
    .filter_map(|key| map.get(*key))
    .find_map(|value| value.as_str())
    .and_then(normalize_project_hint)
}

fn normalize_project_hint(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_matches('"');
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_lowercase();
    if matches!(
        lower.as_str(),
        "unknown" | "none" | "null" | "app" | "src" | "lib" | "tests" | "docs"
    ) {
        return None;
    }

    if trimmed.starts_with("~/") {
        return dirs::home_dir()
            .map(|home| {
                home.join(trimmed.trim_start_matches("~/"))
                    .display()
                    .to_string()
            })
            .or_else(|| Some(trimmed.to_string()));
    }

    Some(trimmed.to_string())
}

fn dedup_timeline_entries(entries: &mut Vec<TimelineEntry>) {
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

/// Claude `~/.claude/history.jsonl` entry — user prompts with project context.
#[derive(Debug, Deserialize)]
struct ClaudeHistoryEntry {
    display: String,
    timestamp: i64, // milliseconds epoch
    #[serde(default)]
    project: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    /// Pasted text content keyed by paste ID. The `display` field shows
    /// "[Pasted text #N +X lines]" placeholder; actual content lives here.
    #[serde(rename = "pastedContents", default)]
    pasted_contents: HashMap<String, serde_json::Value>,
}

/// Extract timeline entries from `~/.claude/history.jsonl`.
///
/// Contains user prompts with `project` (=cwd), `display` (text), `timestamp` (ms epoch).
/// Skips slash commands (`/init`, `/status`, `/model`, etc.).
pub fn extract_claude_history(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let history_path = dirs::home_dir()
        .context("No home dir")?
        .join(".claude")
        .join("history.jsonl");

    if !history_path.exists() {
        return Ok(vec![]);
    }

    let file = sanitize::open_file_validated(&history_path)?;
    let mut reader = BufReader::new(file);
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    let mut oversized_count = 0usize;
    let mut oversized_samples = Vec::new();
    let mut invalid_epoch_count = 0usize;
    let mut invalid_epoch_samples = Vec::new();
    let mut line_number = 0usize;

    while let Some(limited) = sanitize::read_line_capped(&mut reader, MAX_LINE_BYTES)? {
        line_number += 1;
        if limited.exceeded {
            observe_oversized_line(&mut oversized_count, &mut oversized_samples, line_number);
            continue;
        }
        let line = limited.line;
        if line.trim().is_empty() {
            continue;
        }

        let entry: ClaudeHistoryEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Skip slash commands
        if entry.display.starts_with('/') {
            continue;
        }

        // Expand pastedContents into the message text
        let message = if entry.pasted_contents.is_empty() {
            entry.display.clone()
        } else {
            let mut text = entry.display.clone();
            // Sort by key to get deterministic order
            let mut paste_keys: Vec<&String> = entry.pasted_contents.keys().collect();
            paste_keys.sort();
            for key in paste_keys {
                if let Some(obj) = entry.pasted_contents[key].as_object()
                    && let Some(content) = obj.get("content").and_then(|v| v.as_str())
                {
                    text.push_str("\n\n");
                    text.push_str(content);
                }
            }
            text
        };

        if message.trim().is_empty() {
            continue;
        }

        // Project filter
        if !config.project_filter.is_empty() {
            let matches = entry
                .project
                .as_deref()
                .is_some_and(|p| project_filter_matches_path(p, &config.project_filter));
            if !matches {
                continue;
            }
        }

        let timestamp = match DateTime::<Utc>::from_timestamp_millis(entry.timestamp) {
            Some(ts) => ts,
            None => {
                invalid_epoch_count += 1;
                push_unique_sample(&mut invalid_epoch_samples, entry.timestamp.to_string(), 5);
                continue;
            }
        };

        if timestamp < config.cutoff {
            continue;
        }
        if config.watermark.is_some_and(|wm| timestamp < wm) {
            continue;
        }

        entries.push(build_timeline_entry_with_content_warnings(
            timestamp,
            "claude",
            entry.session_id.as_deref().unwrap_or("history"),
            "user",
            message,
            TimelineEntryMeta {
                branch: None,
                cwd: entry.project,
                frame_kind: Some(FrameKind::UserMsg),
                timestamp_source: None,
            },
            &mut warnings,
        ));
    }

    if oversized_count > 0 {
        warnings.push(ClaudeSessionWarning::OversizedLine {
            count: oversized_count,
            samples: oversized_samples,
        });
    }
    if invalid_epoch_count > 0 {
        warnings.push(ClaudeSessionWarning::InvalidEpochMillis {
            count: invalid_epoch_count,
            samples: invalid_epoch_samples,
        });
    }
    emit_claude_session_warnings(&history_path, &warnings);

    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

// ============================================================================
// Codex extractor
// ============================================================================

/// Extract timeline entries from Codex history.
///
/// Improved approach: filters by session context, not per-message content.
/// If ANY message in a session mentions the project filter, ALL messages
/// from that session are included.
pub fn extract_codex(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let codex_path = dirs::home_dir()
        .context("No home dir")?
        .join(".codex")
        .join("history.jsonl");

    if !codex_path.exists() {
        return Ok(vec![]);
    }

    let file = sanitize::open_file_validated(&codex_path)?;
    let mut reader = BufReader::new(file);

    let mut records = Vec::new();
    let mut warnings = Vec::new();
    let mut oversized_count = 0usize;
    let mut oversized_samples = Vec::new();
    let mut line_number = 0usize;

    while let Some(limited) = sanitize::read_line_capped(&mut reader, MAX_LINE_BYTES)? {
        line_number += 1;
        if limited.exceeded {
            observe_oversized_line(&mut oversized_count, &mut oversized_samples, line_number);
            continue;
        }
        let line = limited.line;
        if line.trim().is_empty() {
            continue;
        }

        let entry: CodexEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        records.push(entry);
    }

    if oversized_count > 0 {
        warnings.push(CodexSessionWarning::OversizedLine {
            count: oversized_count,
            samples: oversized_samples,
        });
    }
    let mut entries = build_codex_history_entries(&records, config, &mut warnings);
    emit_codex_session_warnings(&codex_path, &warnings);

    // Merge codex sessions entries
    match extract_codex_sessions(config) {
        Ok(sess) => entries.extend(sess),
        Err(e) => eprintln!("Codex sessions extraction warning: {}", e),
    }

    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

// ============================================================================
// Codex sessions extractor
// ============================================================================

/// Codex session event from `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`.
#[derive(Debug, Deserialize)]
struct CodexSessionEvent {
    timestamp: String, // ISO 8601
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexSessionWarning {
    MissingSessionMeta {
        fallback: String,
    },
    DuplicateSessionMeta {
        first: String,
        ignored: Vec<String>,
    },
    FilenameMismatch {
        meta_id: String,
        filename_stem: String,
    },
    UnparsableTimestamp {
        count: usize,
        samples: Vec<String>,
    },
    UnknownMsgType {
        count: usize,
        samples: Vec<String>,
    },
    MixedFormat {
        count: usize,
        samples: Vec<String>,
    },
    OversizedLine {
        count: usize,
        samples: Vec<String>,
    },
    LineParseError {
        line_number: usize,
        snippet: String,
    },
    ContentSanitization {
        warning: sanitize::ContentSanitizationWarning,
    },
}

impl CodexSessionWarning {
    fn describe(&self, path: &Path) -> String {
        match self {
            CodexSessionWarning::MissingSessionMeta { fallback } => format!(
                "Codex session warning: {} has no session_meta.payload.id; using `{}` from filename",
                path.display(),
                fallback
            ),
            CodexSessionWarning::DuplicateSessionMeta { first, ignored } => format!(
                "Codex session warning: {} has multiple session_meta.payload.id values; using `{}` and ignoring {}",
                path.display(),
                first,
                ignored.join(", ")
            ),
            CodexSessionWarning::FilenameMismatch {
                meta_id,
                filename_stem,
            } => format!(
                "Codex session warning: {} session_meta.payload.id `{}` does not match filename UUID suffix in `{}`",
                path.display(),
                meta_id,
                filename_stem
            ),
            CodexSessionWarning::UnparsableTimestamp { count, samples } => format!(
                "Codex warning: {} has {} unparsable timestamp(s); frames dropped. Sample(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            CodexSessionWarning::UnknownMsgType { count, samples } => format!(
                "Codex session warning: {} encountered {} event_msg(s) with unrecognized payload.type; content preserved via fallback. Sample type(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            CodexSessionWarning::MixedFormat { count, samples } => format!(
                "Codex file warning: {} contains mixed history/session JSONL records ({} minority record(s)); content was parsed by both parsers where possible. Sample(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            CodexSessionWarning::OversizedLine { count, samples } => format!(
                "Codex warning: {} skipped {} oversized JSONL line(s) over {} bytes. Sample(s): {}",
                path.display(),
                count,
                MAX_LINE_BYTES,
                samples.join(", ")
            ),
            CodexSessionWarning::LineParseError {
                line_number,
                snippet,
            } => format!(
                "Codex warning: {} skipped malformed JSONL line {}; snippet: {}",
                path.display(),
                line_number,
                snippet
            ),
            CodexSessionWarning::ContentSanitization { warning } => format!(
                "Codex content warning: {} {}",
                path.display(),
                describe_content_sanitization_warning(warning)
            ),
        }
    }
}

impl PushContentSanitizationWarning for Vec<ClaudeSessionWarning> {
    fn push_content_sanitization_warning(&mut self, warning: sanitize::ContentSanitizationWarning) {
        self.push(ClaudeSessionWarning::ContentSanitization { warning });
    }
}

impl PushContentSanitizationWarning for Vec<GeminiSessionWarning> {
    fn push_content_sanitization_warning(&mut self, warning: sanitize::ContentSanitizationWarning) {
        self.push(GeminiSessionWarning::ContentSanitization { warning });
    }
}

impl PushContentSanitizationWarning for Vec<JunieSessionWarning> {
    fn push_content_sanitization_warning(&mut self, warning: sanitize::ContentSanitizationWarning) {
        self.push(JunieSessionWarning::ContentSanitization { warning });
    }
}

impl PushContentSanitizationWarning for Vec<CodexSessionWarning> {
    fn push_content_sanitization_warning(&mut self, warning: sanitize::ContentSanitizationWarning) {
        self.push(CodexSessionWarning::ContentSanitization { warning });
    }
}

fn emit_codex_session_warnings(path: &Path, warnings: &[CodexSessionWarning]) {
    use crate::diagnostics::{self, DiagnosticKind};
    for warning in warnings {
        let line = warning.describe(path);
        diagnostics::log_describe(&line);
        match warning {
            CodexSessionWarning::MissingSessionMeta { .. } => {
                diagnostics::record("codex", DiagnosticKind::MissingSessionMeta, 1, path);
            }
            CodexSessionWarning::DuplicateSessionMeta { .. } => {
                diagnostics::record("codex", DiagnosticKind::DuplicateSessionMeta, 1, path);
            }
            CodexSessionWarning::FilenameMismatch { .. } => {
                diagnostics::record("codex", DiagnosticKind::FilenameMismatch, 1, path);
            }
            CodexSessionWarning::UnparsableTimestamp { count, .. } => {
                diagnostics::record("codex", DiagnosticKind::UnparsableTimestamp, *count, path);
            }
            CodexSessionWarning::UnknownMsgType { count, .. } => {
                diagnostics::record("codex", DiagnosticKind::UnknownMsgType, *count, path);
            }
            CodexSessionWarning::MixedFormat { count, .. } => {
                diagnostics::record("codex", DiagnosticKind::MixedFormat, *count, path);
            }
            CodexSessionWarning::OversizedLine { count, .. } => {
                diagnostics::record("codex", DiagnosticKind::OversizedLine, *count, path);
            }
            CodexSessionWarning::LineParseError { .. } => {
                diagnostics::record("codex", DiagnosticKind::LineParseError, 1, path);
            }
            CodexSessionWarning::ContentSanitization { warning } => {
                record_content_sanitization("codex", warning, path);
            }
        }
        if diagnostics::is_verbose() {
            eprintln!("{line}");
        }
    }
}

// Legacy per-extractor aggregator kept for test coverage only. Production
// extractors now route per-file warnings through `emit_codex_session_warnings`
// which records into the shared `crate::diagnostics` aggregator and emits a
// single per-run SUMMARY (G-4).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct CodexSessionDiagnostics {
    pub(crate) missing: usize,
    pub(crate) duplicate: usize,
    pub(crate) mismatch: usize,
    pub(crate) unparsable_ts: usize,
    pub(crate) unknown_msg_type: usize,
    pub(crate) mixed_format: usize,
    pub(crate) oversized_line: usize,
    pub(crate) line_parse_error: usize,
    pub(crate) content_sanitization: usize,
}

#[cfg(test)]
impl CodexSessionDiagnostics {
    pub(crate) fn observe(&mut self, warnings: &[CodexSessionWarning]) {
        for warning in warnings {
            match warning {
                CodexSessionWarning::MissingSessionMeta { .. } => self.missing += 1,
                CodexSessionWarning::DuplicateSessionMeta { .. } => self.duplicate += 1,
                CodexSessionWarning::FilenameMismatch { .. } => self.mismatch += 1,
                CodexSessionWarning::UnparsableTimestamp { count, .. } => {
                    self.unparsable_ts += count;
                }
                CodexSessionWarning::UnknownMsgType { count, .. } => {
                    self.unknown_msg_type += count;
                }
                CodexSessionWarning::MixedFormat { count, .. } => {
                    self.mixed_format += count;
                }
                CodexSessionWarning::OversizedLine { count, .. } => {
                    self.oversized_line += count;
                }
                CodexSessionWarning::LineParseError { .. } => {
                    self.line_parse_error += 1;
                }
                CodexSessionWarning::ContentSanitization { .. } => {
                    self.content_sanitization += 1;
                }
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.missing == 0
            && self.duplicate == 0
            && self.mismatch == 0
            && self.unparsable_ts == 0
            && self.unknown_msg_type == 0
            && self.mixed_format == 0
            && self.oversized_line == 0
            && self.line_parse_error == 0
            && self.content_sanitization == 0
    }
}

fn file_stem_string(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn is_uuid_like(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(idx, byte)| {
            if matches!(idx, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn uuid_suffix_from_stem(stem: &str) -> Option<&str> {
    // Codex rollout filenames currently end in strict UUIDv4/v7-style hex IDs.
    // Non-UUID session ids skip filename-mismatch diagnostics.
    let start = stem.len().checked_sub(36)?;
    let suffix = &stem[start..];
    is_uuid_like(suffix).then_some(suffix)
}

fn push_unique_sample(samples: &mut Vec<String>, sample: String, max: usize) {
    if samples.len() < max && !samples.iter().any(|existing| existing == &sample) {
        samples.push(sample);
    }
}

fn codex_payload_message(payload: &serde_json::Value) -> String {
    for key in ["message", "text", "content", "error", "query", "result"] {
        if let Some(value) = payload.get(key) {
            if let Some(text) = value.as_str() {
                if !text.trim().is_empty() {
                    return text.to_string();
                }
            } else if !value.is_null() {
                return render_json_inline(value);
            }
        }
    }
    render_json_inline(payload)
}

fn codex_timestamp_from_epoch_seconds(raw: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(raw, 0).single()
}

fn build_codex_history_entries(
    records: &[CodexEntry],
    config: &ExtractionConfig,
    warnings: &mut Vec<CodexSessionWarning>,
) -> Vec<TimelineEntry> {
    let mut sessions: HashMap<String, Vec<&CodexEntry>> = HashMap::new();
    for entry in records {
        sessions
            .entry(entry.session_id.clone())
            .or_default()
            .push(entry);
    }

    let matching_sessions: HashSet<String> = if !config.project_filter.is_empty() {
        sessions
            .iter()
            .filter(|(_id, msgs)| {
                msgs.iter().any(|m| {
                    m.cwd
                        .as_deref()
                        .is_some_and(|c| project_filter_matches_path(c, &config.project_filter))
                })
            })
            .map(|(id, _)| id.clone())
            .collect()
    } else {
        sessions.keys().cloned().collect()
    };

    let mut entries = Vec::new();
    let mut unparsable_ts_count = 0usize;
    let mut unparsable_ts_samples = Vec::new();

    for (session_id, msgs) in &sessions {
        if !matching_sessions.contains(session_id) {
            continue;
        }

        for msg in msgs {
            let timestamp = match codex_timestamp_from_epoch_seconds(msg.ts) {
                Some(ts) => ts,
                None => {
                    unparsable_ts_count += 1;
                    push_unique_sample(
                        &mut unparsable_ts_samples,
                        format!("{}:{}", msg.session_id, msg.ts),
                        5,
                    );
                    continue;
                }
            };

            if timestamp < config.cutoff {
                continue;
            }
            if config.watermark.is_some_and(|wm| timestamp < wm) {
                continue;
            }

            let role = msg.role.as_deref().unwrap_or("user").to_string();
            let frame_kind = frame_kind_from_role(&role);
            if !should_keep_entry(frame_kind, config) {
                continue;
            }

            if msg.text.is_empty() {
                continue;
            }

            entries.push(build_timeline_entry_with_content_warnings(
                timestamp,
                "codex",
                session_id,
                &role,
                msg.text.clone(),
                TimelineEntryMeta {
                    branch: None,
                    cwd: msg.cwd.clone(),
                    frame_kind,
                    timestamp_source: None,
                },
                warnings,
            ));
        }
    }

    if unparsable_ts_count > 0 {
        warnings.push(CodexSessionWarning::UnparsableTimestamp {
            count: unparsable_ts_count,
            samples: unparsable_ts_samples,
        });
    }

    entries
}

/// Extract timeline entries from Codex session files (`~/.codex/sessions/`).
///
/// Walks `~/.codex/sessions/` recursively for `*.jsonl` files.
/// Two-pass per file: extract session metadata, then collect user/agent messages.
pub fn extract_codex_sessions(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let sessions_dir = dirs::home_dir()
        .context("No home dir")?
        .join(".codex")
        .join("sessions");

    if !sessions_dir.exists() || !sessions_dir.is_dir() {
        return Ok(vec![]);
    }

    let mut entries = Vec::new();
    let files = walk_jsonl_files(&sessions_dir);

    for path in &files {
        match parse_codex_session_file_with_diagnostics(path, config) {
            Ok((se, warnings)) => {
                emit_codex_session_warnings(path, &warnings);
                entries.extend(se);
            }
            Err(_) => continue,
        }
    }

    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

fn parse_codex_session_file_with_diagnostics(
    path: &Path,
    config: &ExtractionConfig,
) -> Result<(Vec<TimelineEntry>, Vec<CodexSessionWarning>)> {
    let file = sanitize::open_file_validated(path)?;
    let mut reader = BufReader::new(file);

    let mut events: Vec<CodexSessionEvent> = Vec::new();
    let mut warnings = Vec::new();
    let mut oversized_count = 0usize;
    let mut oversized_samples = Vec::new();
    let mut line_number = 0usize;
    while let Some(limited) = sanitize::read_line_capped(&mut reader, MAX_LINE_BYTES)? {
        line_number += 1;
        if limited.exceeded {
            observe_oversized_line(&mut oversized_count, &mut oversized_samples, line_number);
            continue;
        }
        let line = limited.line;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<CodexSessionEvent>(&line) {
            Ok(ev) => events.push(ev),
            Err(_) => warnings.push(codex_line_parse_error(line_number, &line)),
        }
    }

    if oversized_count > 0 {
        warnings.push(CodexSessionWarning::OversizedLine {
            count: oversized_count,
            samples: oversized_samples,
        });
    }

    let (entries, event_warnings) =
        parse_codex_session_events_with_diagnostics(path, &events, config);
    warnings.extend(event_warnings);
    Ok((entries, warnings))
}

fn codex_line_parse_error(line_number: usize, line: &str) -> CodexSessionWarning {
    CodexSessionWarning::LineParseError {
        line_number,
        snippet: line.chars().take(200).collect(),
    }
}

fn parse_codex_session_events_with_diagnostics(
    path: &Path,
    events: &[CodexSessionEvent],
    config: &ExtractionConfig,
) -> (Vec<TimelineEntry>, Vec<CodexSessionWarning>) {
    // Extract global session metadata (like session_id) and the initial cwd
    let mut session_id: Option<String> = None;
    let mut initial_cwd: Option<String> = None;
    let mut duplicate_meta_ids: Vec<String> = Vec::new();

    for ev in events {
        if ev.event_type == "session_meta" {
            if let Some(meta_id) = ev
                .payload
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|id| !id.trim().is_empty())
                .map(|id| id.trim().to_string())
            {
                if let Some(first) = &session_id {
                    if first != &meta_id && !duplicate_meta_ids.contains(&meta_id) {
                        duplicate_meta_ids.push(meta_id);
                    }
                } else {
                    session_id = Some(meta_id);
                }
            }
            if initial_cwd.is_none() {
                initial_cwd = ev
                    .payload
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
        }
    }

    let filename_stem = file_stem_string(path);
    let mut warnings: Vec<CodexSessionWarning> = Vec::new();
    if let Some(first) = &session_id {
        if !duplicate_meta_ids.is_empty() {
            warnings.push(CodexSessionWarning::DuplicateSessionMeta {
                first: first.clone(),
                ignored: duplicate_meta_ids,
            });
        }
        if let Some(filename_uuid) = uuid_suffix_from_stem(&filename_stem)
            && first != filename_uuid
        {
            warnings.push(CodexSessionWarning::FilenameMismatch {
                meta_id: first.clone(),
                filename_stem: filename_stem.clone(),
            });
        }
    }

    // Fallback session_id from filename stem
    let session_id = session_id.unwrap_or_else(|| {
        warnings.push(CodexSessionWarning::MissingSessionMeta {
            fallback: filename_stem.clone(),
        });
        filename_stem
    });

    // Collect event_msg entries (user_message + agent_message)
    let mut entries = Vec::new();
    let mut current_cwd = initial_cwd;
    let mut unparsable_ts_count: usize = 0;
    let mut unparsable_ts_samples: Vec<String> = Vec::new();
    let mut unknown_msg_type_count: usize = 0;
    let mut unknown_msg_type_samples: Vec<String> = Vec::new();

    for ev in events {
        // Update current context per-turn
        if ev.event_type == "turn_context" {
            if let Some(cwd) = ev
                .payload
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(String::from)
            {
                current_cwd = Some(cwd);
            }
            continue;
        }

        if ev.event_type != "event_msg" {
            continue;
        }

        // Project filter: check if the current turn's cwd matches
        if !config.project_filter.is_empty() {
            let matches = current_cwd
                .as_deref()
                .is_some_and(|cwd| project_filter_matches_path(cwd, &config.project_filter));
            if !matches {
                continue;
            }
        }

        let msg_type = ev
            .payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let (role, message, frame_kind) = match msg_type {
            "user_message" => (
                "user",
                ev.payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                Some(FrameKind::UserMsg),
            ),
            "agent_message" => (
                "assistant",
                ev.payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                Some(FrameKind::AgentReply),
            ),
            "agent_reasoning" | "thinking" | "thinking_delta" => (
                "reasoning",
                ev.payload
                    .get("text")
                    .or_else(|| ev.payload.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                Some(FrameKind::InternalThought),
            ),
            "function_call"
            | "tool_call"
            | "tool_result"
            | "mcp_tool_call"
            | "mcp_tool_call_response" => (
                "tool",
                ev.payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| render_json_inline(&ev.payload)),
                Some(FrameKind::ToolCall),
            ),
            "task_started"
            | "task_complete"
            | "error"
            | "notification"
            | "web_search"
            | "web_search_complete" => (
                "system",
                codex_payload_message(&ev.payload),
                Some(FrameKind::SystemNote),
            ),
            other => {
                let sample = if other.is_empty() {
                    "<missing>".to_string()
                } else {
                    other.to_string()
                };
                unknown_msg_type_count += 1;
                push_unique_sample(&mut unknown_msg_type_samples, sample, 5);

                let role = ev
                    .payload
                    .get("role")
                    .and_then(|value| value.as_str())
                    .unwrap_or("system");
                let frame_kind = frame_kind_from_role(role).unwrap_or(FrameKind::SystemNote);
                (
                    role_for_frame_kind(frame_kind),
                    codex_payload_message(&ev.payload),
                    Some(frame_kind),
                )
            }
        };

        if !should_keep_entry(frame_kind, config) {
            continue;
        }

        if message.is_empty() {
            continue;
        }

        let timestamp = match parse_rfc3339_or_naive_utc(&ev.timestamp) {
            Ok(dt) => dt,
            Err(_) => {
                unparsable_ts_count += 1;
                if unparsable_ts_samples.len() < 3
                    && !unparsable_ts_samples.iter().any(|s| s == &ev.timestamp)
                {
                    unparsable_ts_samples.push(ev.timestamp.clone());
                }
                continue;
            }
        };

        if timestamp < config.cutoff {
            continue;
        }
        if config.watermark.is_some_and(|wm| timestamp < wm) {
            continue;
        }

        entries.push(build_timeline_entry_with_content_warnings(
            timestamp,
            "codex",
            &session_id,
            role,
            message,
            TimelineEntryMeta {
                branch: None,
                cwd: current_cwd.clone(),
                frame_kind,
                timestamp_source: None,
            },
            &mut warnings,
        ));
    }

    if unparsable_ts_count > 0 {
        warnings.push(CodexSessionWarning::UnparsableTimestamp {
            count: unparsable_ts_count,
            samples: unparsable_ts_samples,
        });
    }
    if unknown_msg_type_count > 0 {
        warnings.push(CodexSessionWarning::UnknownMsgType {
            count: unknown_msg_type_count,
            samples: unknown_msg_type_samples,
        });
    }

    (entries, warnings)
}

/// Recursively walk a directory for `*.jsonl` files.
fn walk_jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walk_jsonl_files(&path));
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                files.push(path);
            }
        }
    }
    files
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walk_files(&path));
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    files
}

// ============================================================================
// Gemini extractor
// ============================================================================

/// Extract timeline entries from Gemini CLI sessions.
///
/// Reads `~/.gemini/tmp/<projectHash>/chats/session-*.json[l]` files.
/// Returns Ok(vec![]) silently if the directory doesn't exist.
pub fn extract_gemini(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let home = dirs::home_dir().context("No home dir")?;
    let gemini_tmp = home.join(".gemini").join("tmp");

    if !gemini_tmp.exists() || !gemini_tmp.is_dir() {
        return Ok(vec![]);
    }

    let mut entries: Vec<TimelineEntry> = Vec::new();

    // Walk each project hash directory
    for project_entry in fs::read_dir(&gemini_tmp)? {
        let project_entry = project_entry?;
        let project_path = project_entry.path();

        if !project_path.is_dir() {
            continue;
        }

        let chats_dir = project_path.join("chats");
        if !chats_dir.exists() || !chats_dir.is_dir() {
            continue;
        }

        for file_entry in fs::read_dir(&chats_dir)? {
            let file_entry = file_entry?;
            let path = file_entry.path();

            if !is_gemini_session_file(&path) {
                continue;
            }

            match parse_gemini_session(&path, config) {
                Ok(se) => entries.extend(se),
                Err(_) => continue,
            }
        }
    }

    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

/// Parse a single Gemini CLI session file.
fn parse_gemini_session(
    path: &std::path::Path,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let (entries, warnings) = parse_gemini_session_with_diagnostics(path, config)?;
    emit_gemini_session_warnings(path, &warnings);
    Ok(entries)
}

fn parse_gemini_session_with_diagnostics(
    path: &std::path::Path,
    config: &ExtractionConfig,
) -> Result<(Vec<TimelineEntry>, Vec<GeminiSessionWarning>)> {
    let content = sanitize::read_to_string_validated(path)?;
    let (mut session, mut warnings) = parse_gemini_session_content(path, &content)?;

    let session_id = match session.session_id.take() {
        Some(id) if !id.trim().is_empty() => id,
        _ => {
            let fallback = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            warnings.push(GeminiSessionWarning::MissingSessionId {
                fallback: fallback.clone(),
            });
            fallback
        }
    };

    let path_default_cwd = infer_gemini_project_hint_from_session_path(path);
    let session_default_cwd = path_default_cwd.clone().or_else(|| {
        session
            .messages
            .iter()
            .find_map(infer_project_hint_from_gemini_message)
    });

    let content_default_cwd = session
        .messages
        .iter()
        .find_map(infer_project_hint_from_gemini_message);

    // Gemini CLI stores sessions under ~/.gemini/tmp/<project>/chats/.
    // That path is the session ownership signal; message content may contain
    // referenced files/screenshots from unrelated projects.
    let session_matches_filter = if !config.project_filter.is_empty() {
        path_default_cwd
            .as_deref()
            .or(content_default_cwd.as_deref())
            .is_some_and(|cwd| project_filter_matches_path(cwd, &config.project_filter))
    } else {
        true
    };

    if !session_matches_filter {
        return Ok((vec![], vec![]));
    }

    let mut entries = Vec::new();
    let mut unparsable_ts_count: usize = 0;
    let mut unparsable_ts_samples: Vec<String> = Vec::new();
    let mut unknown_msg_type_count: usize = 0;
    let mut unknown_msg_type_samples: Vec<String> = Vec::new();

    for (idx, msg) in session.messages.iter().enumerate() {
        let base_role = match gemini_base_role(msg) {
            Some(role) => role,
            None => {
                unknown_msg_type_count += 1;
                push_unique_sample(
                    &mut unknown_msg_type_samples,
                    msg.msg_type
                        .as_deref()
                        .or(msg.role.as_deref())
                        .unwrap_or("<missing>")
                        .to_string(),
                    5,
                );
                "system"
            }
        };

        // Parse timestamp (always RFC3339 in Gemini CLI)
        let timestamp = match msg.timestamp.as_deref() {
            Some(ts) => match parse_rfc3339_or_naive_utc(ts) {
                Ok(timestamp) => timestamp,
                Err(_) => {
                    unparsable_ts_count += 1;
                    push_unique_sample(
                        &mut unparsable_ts_samples,
                        format!("message {}: {}", idx + 1, ts),
                        5,
                    );
                    continue;
                }
            },
            None => {
                unparsable_ts_count += 1;
                push_unique_sample(
                    &mut unparsable_ts_samples,
                    format!("message {}: <missing>", idx + 1),
                    5,
                );
                continue;
            }
        };

        // Respect cutoff
        if timestamp < config.cutoff {
            continue;
        }

        // Respect watermark
        if config.watermark.is_some_and(|wm| timestamp < wm) {
            continue;
        }

        let inferred_cwd = session_default_cwd
            .clone()
            .or_else(|| infer_project_hint_from_gemini_message(msg));
        let mut classified = msg
            .content
            .as_ref()
            .map(|value| extract_gemini_classified_blocks(value, base_role))
            .filter(|blocks| !blocks.is_empty())
            .unwrap_or_default();
        if classified.is_empty()
            && let Some(value) = msg.display_content.as_ref()
        {
            classified = extract_gemini_classified_blocks(value, base_role);
        }
        if classified.is_empty()
            && let Some(text) = render_gemini_message_content(msg)
            && let Some(frame_kind) = frame_kind_from_role(base_role)
        {
            classified.push(ClassifiedFrameBlock {
                role: base_role.to_string(),
                frame_kind,
                message: text,
            });
        }

        for block in classified {
            if !should_keep_entry(Some(block.frame_kind), config) {
                continue;
            }
            entries.push(build_timeline_entry_with_content_warnings(
                timestamp,
                "gemini",
                &session_id,
                &block.role,
                block.message,
                TimelineEntryMeta {
                    branch: None,
                    cwd: inferred_cwd.clone(),
                    frame_kind: Some(block.frame_kind),
                    timestamp_source: None,
                },
                &mut warnings,
            ));
        }

        // Extract thoughts as reasoning entries (only when include_assistant)
        if config.include_assistant && !msg.thoughts.is_empty() {
            for thought in &msg.thoughts {
                let thought_ts = match thought.timestamp.as_deref() {
                    Some(ts) => match parse_rfc3339_or_naive_utc(ts) {
                        Ok(timestamp) => timestamp,
                        Err(_) => {
                            unparsable_ts_count += 1;
                            push_unique_sample(
                                &mut unparsable_ts_samples,
                                format!("thought {}: {}", idx + 1, ts),
                                5,
                            );
                            timestamp
                        }
                    },
                    None => timestamp,
                };

                let desc = thought.description.as_deref().unwrap_or("");
                let subj = thought.subject.as_deref().unwrap_or("");
                if desc.is_empty() && subj.is_empty() {
                    continue;
                }

                let text = if subj.is_empty() {
                    desc.to_string()
                } else if desc.is_empty() {
                    subj.to_string()
                } else {
                    format!("**{}**: {}", subj, desc)
                };

                entries.push(build_timeline_entry_with_content_warnings(
                    thought_ts,
                    "gemini",
                    &session_id,
                    "reasoning",
                    text,
                    TimelineEntryMeta {
                        branch: None,
                        cwd: inferred_cwd.clone(),
                        frame_kind: Some(FrameKind::InternalThought),
                        timestamp_source: None,
                    },
                    &mut warnings,
                ));
            }
        }
    }

    if unparsable_ts_count > 0 {
        warnings.push(GeminiSessionWarning::UnparsableTimestamp {
            count: unparsable_ts_count,
            samples: unparsable_ts_samples,
        });
    }
    if unknown_msg_type_count > 0 {
        warnings.push(GeminiSessionWarning::UnknownMsgType {
            count: unknown_msg_type_count,
            samples: unknown_msg_type_samples,
        });
    }

    Ok((entries, warnings))
}

fn is_gemini_session_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "json" | "jsonl"))
}

fn parse_gemini_session_content(
    path: &Path,
    content: &str,
) -> Result<(GeminiSession, Vec<GeminiSessionWarning>)> {
    if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
        parse_gemini_jsonl_session(content)
            .with_context(|| format!("Failed to parse Gemini JSONL session {}", path.display()))
    } else {
        serde_json::from_str(content)
            .map(|session| (session, Vec::new()))
            .with_context(|| format!("Failed to parse Gemini JSON session {}", path.display()))
    }
}

fn parse_gemini_jsonl_session(content: &str) -> Result<(GeminiSession, Vec<GeminiSessionWarning>)> {
    let mut session_id = None;
    let mut messages = Vec::new();
    let mut distinct_session_ids = Vec::new();

    for (idx, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let value: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("invalid JSONL record at line {}", idx + 1))?;

        if let Some(candidate) = value
            .get("sessionId")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            if session_id.is_none() {
                session_id = Some(candidate.to_string());
            }
            if !distinct_session_ids.iter().any(|id| id == candidate) {
                distinct_session_ids.push(candidate.to_string());
            }
        }

        let looks_like_message = value.get("timestamp").is_some()
            && (value.get("type").is_some()
                || value.get("role").is_some()
                || value.get("content").is_some()
                || value.get("displayContent").is_some()
                || value.get("thoughts").is_some());

        if looks_like_message {
            let message: GeminiMessage = serde_json::from_value(value)
                .with_context(|| format!("invalid Gemini message at line {}", idx + 1))?;
            messages.push(message);
        }
    }

    let mut warnings = Vec::new();
    if let Some(first) = distinct_session_ids.first()
        && distinct_session_ids.len() > 1
    {
        warnings.push(GeminiSessionWarning::SessionIdDrift {
            first: first.clone(),
            ignored: distinct_session_ids[1..].to_vec(),
        });
    }

    Ok((
        GeminiSession {
            session_id,
            messages,
        },
        warnings,
    ))
}

/// Extract timeline entries from a single Junie session event log.
///
/// Junie stores very noisy UI/runtime session traces under:
/// `~/.junie/sessions/session-<YYMMDD>-<HHMMSS>-<id>/events.jsonl`
///
/// This extractor keeps the full conversation truth: user prompts, agent
/// replies, the agent's internal thoughts, and the tool / terminal / MCP /
/// file-change actions the agent took. Status churn (`AgentCurrentStatusUpdatedEvent`,
/// env updates, plan ticks) and intermediate streaming snapshots are dropped
/// via per-`stepId` content dedup so we keep the final state of each block.
///
/// Mapping:
/// - `UserPromptEvent.prompt`              -> `user` / `UserMsg`
/// - `UserResponseEvent.prompt`            -> `user` / `UserMsg`
/// - `ResultBlockUpdatedEvent.result`      -> `assistant` / `AgentReply`
/// - `AgentThoughtBlockUpdatedEvent.text`  -> `reasoning` / `InternalThought`
/// - `TerminalBlockUpdatedEvent`           -> `tool` / `ToolCall` (`$ {cmd}\n{output}`)
/// - `McpBlockUpdatedEvent`                -> `tool` / `ToolCall` (`{toolName}: {input}` + details)
/// - `ToolBlockUpdatedEvent`               -> `tool` / `ToolCall` (text + details)
/// - `FileChangesBlockUpdatedEvent`        -> `tool` / `ToolCall` (paths changed)
/// - `ViewFilesBlockUpdatedEvent`          -> `tool` / `ToolCall` (paths viewed)
///
/// Pre-COMPLETED block snapshots are dropped automatically because rendered
/// text dedups by `stepId`. Status churn (`AgentCurrentStatusUpdatedEvent`,
/// `EnvironmentVariablesUpdatedEvent`, plan updates) is ignored.
pub fn extract_junie_file(path: &Path, config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let path = sanitize::validate_read_path(path)?;
    let file = sanitize::open_file_validated(&path)?;
    let mut reader = BufReader::new(file);
    let (session_id, fallback_warning) = junie_session_id_from_path_with_warning(&path);
    let mut warnings = Vec::new();
    if let Some(warning) = fallback_warning {
        warnings.push(warning);
    }
    let session_anchor = infer_junie_session_anchor(&path)
        .or_else(|| infer_junie_file_anchor(&path))
        .unwrap_or_else(Utc::now);

    let mut entries = Vec::new();
    let mut current_cwd: Option<String> = None;
    let mut cursor = session_anchor;
    // Streaming dedup: each Junie block kind emits multiple updates per stepId
    // (IN_PROGRESS -> COMPLETED, sometimes COMPLETED twice). Track the last
    // rendered text per (stepId, kind) to drop snapshots that didn't change.
    let mut last_block_render: HashMap<(String, &'static str), String> = HashMap::new();
    let mut oversized_count = 0usize;
    let mut oversized_samples = Vec::new();
    let mut line_number = 0usize;

    while let Some(limited) = sanitize::read_line_capped(&mut reader, MAX_LINE_BYTES)? {
        line_number += 1;
        if limited.exceeded {
            observe_oversized_line(&mut oversized_count, &mut oversized_samples, line_number);
            continue;
        }
        let line = limited.line;

        if line.trim().is_empty() {
            continue;
        }

        let interesting_kind = detect_junie_interesting_kind(&line);
        if interesting_kind.is_none() {
            continue;
        }

        let raw: serde_json::Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        match interesting_kind {
            Some("UserPromptEvent") => {
                let message = raw
                    .get("prompt")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToOwned::to_owned);
                let Some(message) = message else {
                    continue;
                };

                let candidate = raw
                    .get("requestId")
                    .and_then(|value| value.as_str())
                    .and_then(parse_junie_request_timestamp);
                let timestamp = next_junie_timestamp(&mut cursor, candidate);
                if junie_timestamp_in_window(timestamp, config) {
                    entries.push(build_timeline_entry_with_content_warnings(
                        timestamp,
                        "junie",
                        &session_id,
                        "user",
                        message,
                        TimelineEntryMeta {
                            branch: None,
                            cwd: current_cwd.clone(),
                            frame_kind: Some(FrameKind::UserMsg),
                            timestamp_source: None,
                        },
                        &mut warnings,
                    ));
                }
            }
            Some("UserResponseEvent") => {
                let message = raw
                    .get("prompt")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToOwned::to_owned);
                let Some(message) = message else {
                    continue;
                };

                let timestamp = next_junie_timestamp(&mut cursor, None);
                if junie_timestamp_in_window(timestamp, config) {
                    entries.push(build_timeline_entry_with_content_warnings(
                        timestamp,
                        "junie",
                        &session_id,
                        "user",
                        message,
                        TimelineEntryMeta {
                            branch: None,
                            cwd: current_cwd.clone(),
                            frame_kind: Some(FrameKind::UserMsg),
                            timestamp_source: None,
                        },
                        &mut warnings,
                    ));
                }
            }
            Some("CurrentDirectoryUpdatedEvent") => {
                current_cwd = raw
                    .get("event")
                    .and_then(|value| value.get("agentEvent"))
                    .and_then(|value| value.get("currentDirectory"))
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToOwned::to_owned);
            }
            Some(
                block_kind @ ("ResultBlockUpdatedEvent"
                | "AgentThoughtBlockUpdatedEvent"
                | "TerminalBlockUpdatedEvent"
                | "McpBlockUpdatedEvent"
                | "ToolBlockUpdatedEvent"
                | "FileChangesBlockUpdatedEvent"
                | "ViewFilesBlockUpdatedEvent"),
            ) => {
                if !config.include_assistant {
                    continue;
                }

                let agent_event = raw.get("event").and_then(|value| value.get("agentEvent"));
                let Some(agent_event) = agent_event else {
                    continue;
                };

                // Streaming block events (terminal/tool/mcp/view/changes) emit
                // a noisy IN_PROGRESS snapshot before COMPLETED. Skip pre-final
                // states so the corpus only sees the settled rendering.
                // `ResultBlockUpdatedEvent` and `AgentThoughtBlockUpdatedEvent`
                // do not carry a `status` field — let them through unconditionally.
                let has_streaming_status = matches!(
                    block_kind,
                    "TerminalBlockUpdatedEvent"
                        | "McpBlockUpdatedEvent"
                        | "ToolBlockUpdatedEvent"
                        | "FileChangesBlockUpdatedEvent"
                        | "ViewFilesBlockUpdatedEvent"
                );
                if has_streaming_status {
                    let status = agent_event
                        .get("status")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default();
                    if !status.eq_ignore_ascii_case("COMPLETED") {
                        continue;
                    }
                }

                let step_id = agent_event
                    .get("stepId")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| format!("{session_id}:{block_kind}"));

                let Some(BlockProjection {
                    message,
                    role,
                    frame_kind,
                }) = project_junie_block(block_kind, agent_event)
                else {
                    continue;
                };

                let dedup_key = (step_id, block_kind);
                if last_block_render
                    .get(&dedup_key)
                    .is_some_and(|previous| previous == &message)
                {
                    continue;
                }
                last_block_render.insert(dedup_key, message.clone());

                let timestamp = next_junie_timestamp(&mut cursor, None);
                if junie_timestamp_in_window(timestamp, config) {
                    entries.push(build_timeline_entry_with_content_warnings(
                        timestamp,
                        "junie",
                        &session_id,
                        role,
                        message,
                        TimelineEntryMeta {
                            branch: None,
                            cwd: current_cwd.clone(),
                            frame_kind: Some(frame_kind),
                            timestamp_source: None,
                        },
                        &mut warnings,
                    ));
                }
            }
            _ => {}
        }
    }

    if oversized_count > 0 {
        warnings.push(JunieSessionWarning::OversizedLine {
            count: oversized_count,
            samples: oversized_samples,
        });
    }
    emit_junie_session_warnings(&path, &warnings);

    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

struct BlockProjection {
    message: String,
    role: &'static str,
    frame_kind: FrameKind,
}

/// Render a Junie `agentEvent` payload into a timeline-shaped (role, frame, text) triple.
///
/// Returns `None` when the block has no extractable content yet (empty streaming
/// snapshot, missing required field) or when the block kind isn't supported.
fn project_junie_block(kind: &str, agent_event: &serde_json::Value) -> Option<BlockProjection> {
    match kind {
        "ResultBlockUpdatedEvent" => agent_event
            .get("result")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| BlockProjection {
                message: text.to_string(),
                role: "assistant",
                frame_kind: FrameKind::AgentReply,
            }),
        "AgentThoughtBlockUpdatedEvent" => agent_event
            .get("text")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(|text| BlockProjection {
                message: text.to_string(),
                role: "reasoning",
                frame_kind: FrameKind::InternalThought,
            }),
        "TerminalBlockUpdatedEvent" => {
            let command = agent_event
                .get("command")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .unwrap_or_default();
            if command.is_empty() {
                return None;
            }
            let output = agent_event
                .get("presentableOutput")
                .or_else(|| agent_event.get("output"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .unwrap_or_default();
            let message = if output.is_empty() {
                format!("$ {command}")
            } else {
                format!("$ {command}\n{output}")
            };
            Some(BlockProjection {
                message,
                role: "tool",
                frame_kind: FrameKind::ToolCall,
            })
        }
        "McpBlockUpdatedEvent" => {
            let tool_name = agent_event
                .get("toolName")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("mcp");
            let input = agent_event
                .get("input")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .unwrap_or_default();
            let details = agent_event
                .get("details")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .unwrap_or_default();
            // Skip pure status churn (no input, no details).
            if input.is_empty() && details.is_empty() {
                return None;
            }
            let mut message = format!("{tool_name}: {input}");
            if !details.is_empty() && details != input {
                message.push_str("\n→ ");
                message.push_str(details);
            }
            Some(BlockProjection {
                message,
                role: "tool",
                frame_kind: FrameKind::ToolCall,
            })
        }
        "ToolBlockUpdatedEvent" => {
            let text = agent_event
                .get("text")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .unwrap_or_default();
            let details = agent_event
                .get("details")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .unwrap_or_default();
            let output = agent_event
                .get("output")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .unwrap_or_default();
            if text.is_empty() && details.is_empty() && output.is_empty() {
                return None;
            }
            let mut message = text.to_string();
            for extra in [details, output] {
                if extra.is_empty() || extra == text {
                    continue;
                }
                if !message.is_empty() {
                    message.push('\n');
                }
                message.push_str(extra);
            }
            if message.is_empty() {
                return None;
            }
            Some(BlockProjection {
                message,
                role: "tool",
                frame_kind: FrameKind::ToolCall,
            })
        }
        "ViewFilesBlockUpdatedEvent" => {
            let files = agent_event
                .get("files")
                .and_then(|value| value.as_array())?;
            let mut paths = Vec::new();
            for file in files {
                let Some(rel) = file
                    .get("relativePath")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                else {
                    continue;
                };
                let from = file.get("lineFrom").and_then(serde_json::Value::as_i64);
                let to = file.get("lineTo").and_then(serde_json::Value::as_i64);
                let span = match (from, to) {
                    (Some(start), Some(end)) => format!("{rel}:{start}-{end}"),
                    (Some(start), None) => format!("{rel}:{start}"),
                    _ => rel.to_string(),
                };
                paths.push(span);
            }
            if paths.is_empty() {
                return None;
            }
            Some(BlockProjection {
                message: format!("viewed: {}", paths.join(", ")),
                role: "tool",
                frame_kind: FrameKind::ToolCall,
            })
        }
        "FileChangesBlockUpdatedEvent" => {
            let changes = agent_event
                .get("changes")
                .and_then(|value| value.as_array())?;
            let mut paths = Vec::new();
            for change in changes {
                if let Some(rel) = change
                    .get("relativePath")
                    .or_else(|| change.get("path"))
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                {
                    paths.push(rel.to_string());
                }
            }
            if paths.is_empty() {
                return None;
            }
            Some(BlockProjection {
                message: format!("edited: {}", paths.join(", ")),
                role: "tool",
                frame_kind: FrameKind::ToolCall,
            })
        }
        _ => None,
    }
}

/// Extract timeline entries from all Junie session logs under `~/.junie/sessions/`.
pub fn extract_junie(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let sessions_dir = dirs::home_dir()
        .context("No home dir")?
        .join(".junie")
        .join("sessions");

    if !sessions_dir.exists() || !sessions_dir.is_dir() {
        return Ok(vec![]);
    }

    let mut entries = Vec::new();
    for path in walk_jsonl_files(&sessions_dir) {
        if path.file_name().and_then(|name| name.to_str()) != Some(JUNIE_EVENTS_FILENAME) {
            continue;
        }

        if let Some(modified) = infer_junie_file_anchor(&path)
            && modified < config.cutoff
            && config
                .watermark
                .is_none_or(|watermark| modified < watermark)
        {
            continue;
        }

        match extract_junie_file(&path, config) {
            Ok(mut file_entries) => entries.append(&mut file_entries),
            Err(_) => continue,
        }
    }

    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

fn detect_junie_interesting_kind(line: &str) -> Option<&'static str> {
    // Order matters only for ambiguity: each kind is a unique JSON literal
    // (`"kind":"X"` substring). We bail on the first match.
    const KINDS: &[&str] = &[
        "UserPromptEvent",
        "UserResponseEvent",
        "CurrentDirectoryUpdatedEvent",
        "ResultBlockUpdatedEvent",
        "AgentThoughtBlockUpdatedEvent",
        "TerminalBlockUpdatedEvent",
        "McpBlockUpdatedEvent",
        "ToolBlockUpdatedEvent",
        "FileChangesBlockUpdatedEvent",
        "ViewFilesBlockUpdatedEvent",
    ];
    for kind in KINDS {
        // Match the JSON literal form to avoid false positives from prose
        // ("...mentioned UserPromptEvent in docs..." -> would never match
        // because we require the surrounding quotes).
        let mut needle = String::with_capacity(kind.len() + 2);
        needle.push('"');
        needle.push_str(kind);
        needle.push('"');
        if line.contains(&needle) {
            return Some(*kind);
        }
    }
    None
}

fn junie_session_id_from_path_with_warning(path: &Path) -> (String, Option<JunieSessionWarning>) {
    for ancestor in path.ancestors() {
        if let Some(raw) = ancestor.file_name().and_then(|segment| segment.to_str())
            && let Some(id) = raw
                .strip_prefix(JUNIE_SESSION_DIR_PREFIX)
                .map(str::trim)
                .filter(|id| !id.is_empty())
        {
            return (id.to_string(), None);
        }
    }

    let fallback = format!("unknown-{}", short_path_hash(path));
    (
        fallback.clone(),
        Some(JunieSessionWarning::JunieFallbackId { fallback }),
    )
}

fn infer_junie_session_anchor(path: &Path) -> Option<DateTime<Utc>> {
    let session_dir = path.parent()?.file_name()?.to_str()?;
    let suffix = session_dir.strip_prefix(JUNIE_SESSION_DIR_PREFIX)?;
    let mut parts = suffix.split('-');
    let compact_date = parts.next()?;
    let compact_time = parts.next()?;
    parse_compact_junie_timestamp(compact_date, compact_time)
}

fn infer_junie_file_anchor(path: &Path) -> Option<DateTime<Utc>> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    Some(DateTime::<Utc>::from(modified))
}

fn parse_junie_request_timestamp(request_id: &str) -> Option<DateTime<Utc>> {
    let suffix = request_id.strip_prefix(JUNIE_REQUEST_ID_PREFIX)?;
    let mut parts = suffix.split('-');
    let compact_date = parts.next()?;
    let compact_time = parts.next()?;
    parse_compact_junie_timestamp(compact_date, compact_time)
}

fn parse_compact_junie_timestamp(compact_date: &str, compact_time: &str) -> Option<DateTime<Utc>> {
    if compact_date.len() != 6 || compact_time.len() != 6 {
        return None;
    }

    let year = 2000 + compact_date[0..2].parse::<i32>().ok()?;
    let month = compact_date[2..4].parse::<u32>().ok()?;
    let day = compact_date[4..6].parse::<u32>().ok()?;
    let hour = compact_time[0..2].parse::<u32>().ok()?;
    let minute = compact_time[2..4].parse::<u32>().ok()?;
    let second = compact_time[4..6].parse::<u32>().ok()?;

    let naive = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

fn next_junie_timestamp(
    cursor: &mut DateTime<Utc>,
    candidate: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    let next = match candidate {
        Some(ts) if ts > *cursor => ts,
        _ => *cursor + Duration::milliseconds(1),
    };
    *cursor = next;
    next
}

fn junie_timestamp_in_window(timestamp: DateTime<Utc>, config: &ExtractionConfig) -> bool {
    if timestamp < config.cutoff {
        return false;
    }

    if config
        .watermark
        .is_some_and(|watermark| timestamp < watermark)
    {
        return false;
    }

    true
}

// ============================================================================
// CodeScribe transcript extractor
// ============================================================================

/// Discover CodeScribe transcript files under `$HOME/.codescribe/transcriptions`.
pub fn discover_codescribe_transcripts(home: &Path) -> Vec<CodescribeTranscript> {
    discover_codescribe_transcripts_at(&home.join(".codescribe").join("transcriptions"))
}

/// Discover CodeScribe transcript files under an explicit transcriptions root.
pub fn discover_codescribe_transcripts_at(root: &Path) -> Vec<CodescribeTranscript> {
    if !root.is_dir() {
        return Vec::new();
    }

    let mut entries = Vec::new();
    let Ok(day_dirs) = fs::read_dir(root) else {
        return entries;
    };

    for day_dir in day_dirs.flatten() {
        let day_path = day_dir.path();
        if !day_path.is_dir() {
            continue;
        }
        let Some(date) = day_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(parse_codescribe_day)
        else {
            continue;
        };
        let Ok(files) = fs::read_dir(&day_path) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if is_codescribe_transcript_file(&path) {
                entries.push(CodescribeTranscript { path, date });
            }
        }
    }

    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries
}

fn parse_codescribe_day(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

fn is_codescribe_transcript_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    if !matches!(ext, "txt" | "md" | "json") {
        return false;
    }

    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    !name.ends_with(".truth.json")
}

fn codescribe_base_time(path: &Path, date: NaiveDate) -> DateTime<Utc> {
    let time = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.get(0..6))
        .and_then(|prefix| NaiveTime::parse_from_str(prefix, "%H%M%S").ok())
        .unwrap_or(NaiveTime::MIN);
    Utc.from_utc_datetime(&date.and_time(time))
}

fn codescribe_timestamp(path: &Path, date: NaiveDate, start_ms: u64) -> DateTime<Utc> {
    codescribe_base_time(path, date) + Duration::milliseconds(start_ms.min(i64::MAX as u64) as i64)
}

fn load_codescribe_lexicon(home: &Path) -> CodescribeLexicon {
    let path = home.join(".codescribe").join("lexicon.custom.jsonl");
    let Ok(file) = sanitize::open_file_validated(&path) else {
        return CodescribeLexicon::default();
    };

    let mut entries = Vec::new();
    let mut reader = BufReader::new(file);
    while let Ok(Some(line)) = sanitize::read_line_capped(&mut reader, MAX_LINE_BYTES) {
        if line.exceeded {
            continue;
        }
        let line = line.line;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(raw) = serde_json::from_str::<RawCodescribeLexiconEntry>(&line) else {
            continue;
        };
        let mut keywords = raw.keywords;
        if let Some(term) = raw.term {
            keywords.push(term);
        }
        keywords.extend(raw.mispronunciations);
        keywords.retain(|keyword| !keyword.trim().is_empty());
        if !keywords.is_empty() {
            entries.push(CodescribeLexiconEntry {
                speaker: raw.speaker,
                keywords,
            });
        }
    }

    CodescribeLexicon { entries }
}

impl CodescribeLexicon {
    fn speaker_hint(&self, explicit: Option<&str>, text: &str) -> String {
        if let Some(speaker) = explicit.and_then(normalize_speaker_hint) {
            return speaker;
        }

        let text = text.to_lowercase();
        let mut scores: HashMap<String, usize> = HashMap::new();
        for entry in &self.entries {
            let Some(speaker) = entry.speaker.as_deref().and_then(normalize_speaker_hint) else {
                continue;
            };
            for keyword in &entry.keywords {
                if text.contains(&keyword.to_lowercase()) {
                    *scores.entry(speaker.clone()).or_default() += 1;
                }
            }
        }

        scores
            .into_iter()
            .max_by_key(|(_, score)| *score)
            .map(|(speaker, _)| speaker)
            .unwrap_or_else(|| "unknown".to_string())
    }
}

fn normalize_speaker_hint(value: &str) -> Option<String> {
    let normalized = value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>();
    (!normalized.is_empty()).then_some(normalized)
}

fn parse_plain_codescribe_text(content: &str) -> Vec<CodescribeSegment> {
    let text = content.trim();
    if text.is_empty() || is_codescribe_no_speech(text) {
        return Vec::new();
    }

    vec![CodescribeSegment {
        start_ms: 0,
        duration_ms: None,
        speaker: None,
        text: text.to_string(),
    }]
}

fn parse_codescribe_markdown(content: &str) -> Vec<CodescribeSegment> {
    let mut segments = Vec::new();
    let mut current_speaker: Option<String> = None;
    let mut current = String::new();

    for line in content.lines() {
        if let Some(speaker) = parse_markdown_speaker_heading(line) {
            push_codescribe_markdown_segment(&mut segments, current_speaker.take(), &mut current);
            current_speaker = Some(speaker);
            continue;
        }

        current.push_str(line);
        current.push('\n');
    }

    push_codescribe_markdown_segment(&mut segments, current_speaker, &mut current);
    if segments.is_empty() {
        return parse_plain_codescribe_text(content);
    }
    segments
}

fn parse_markdown_speaker_heading(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let heading = trimmed.strip_prefix('#')?.trim_start_matches('#').trim();
    let lower = heading.to_lowercase();
    if !(lower.starts_with("speaker") || lower.starts_with("maciej") || lower.starts_with("monika"))
    {
        return None;
    }
    Some(
        heading
            .trim_end_matches(':')
            .split(':')
            .next()
            .unwrap_or(heading)
            .trim()
            .to_string(),
    )
}

fn push_codescribe_markdown_segment(
    segments: &mut Vec<CodescribeSegment>,
    speaker: Option<String>,
    current: &mut String,
) {
    let text = current.trim();
    if !text.is_empty() && !is_codescribe_no_speech(text) {
        segments.push(CodescribeSegment {
            start_ms: 0,
            duration_ms: None,
            speaker,
            text: text.to_string(),
        });
    }
    current.clear();
}

fn parse_codescribe_json(content: &str) -> Result<Vec<CodescribeSegment>> {
    let transcript: WhisperTranscript = serde_json::from_str(content)?;
    let mut segments = Vec::new();

    for segment in transcript.segments {
        let text = segment.text.unwrap_or_default().trim().to_string();
        if text.is_empty() || is_codescribe_no_speech(&text) {
            continue;
        }
        let start_ms = seconds_to_ms(segment.start.unwrap_or_default());
        let duration_ms = match (segment.start, segment.end) {
            (Some(start), Some(end)) if end > start => Some(seconds_to_ms(end - start)),
            _ => None,
        };
        segments.push(CodescribeSegment {
            start_ms,
            duration_ms,
            speaker: segment.speaker,
            text,
        });
    }

    if segments.is_empty()
        && let Some(text) = transcript.text
    {
        segments = parse_plain_codescribe_text(&text);
    }

    Ok(segments)
}

fn seconds_to_ms(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0).round() as u64
    } else {
        0
    }
}

fn is_codescribe_no_speech(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    CODESCRIBE_NO_SPEECH_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Parse one CodeScribe transcript file into timeline entries.
pub fn parse_codescribe_transcript(
    path: &Path,
    date: NaiveDate,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let home = dirs::home_dir().context("No home dir")?;
    let lexicon = load_codescribe_lexicon(&home);
    parse_codescribe_transcript_with_lexicon(path, date, config, &lexicon, &home)
}

fn parse_codescribe_transcript_with_lexicon(
    path: &Path,
    date: NaiveDate,
    config: &ExtractionConfig,
    lexicon: &CodescribeLexicon,
    home: &Path,
) -> Result<Vec<TimelineEntry>> {
    let content = sanitize::read_to_string_validated(path)?;

    let mut project_hint = None;
    let (frontmatter, body) = split_operator_frontmatter(&content);
    if let Some(project) = frontmatter
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        project_hint = Some(project.trim().to_string());
    } else {
        let lower_body = body.to_ascii_lowercase();
        for filter in &config.project_filter {
            let repo = filter
                .split_once('/')
                .map(|(_, r)| r)
                .unwrap_or(filter)
                .to_ascii_lowercase();
            // Use token-equality (not raw substring) so `-p vista` does
            // not falsely attribute a transcript that mentions
            // `vista-portal`. This addresses the gemini-code-assist
            // MEDIUM comment on PR #8: substring `lower_body.contains`
            // re-introduced the suffix-leak shape that the strict
            // identity matchers in this PR were specifically removing.
            if body_mentions_repo_token(&lower_body, &repo) {
                project_hint = Some(filter.clone());
                break;
            }
        }
    }

    if project_hint.is_none() {
        // TODO: codescribe transcript has no embedded identity in frontmatter or content.
        // Keeping the global filter as a fallback for that path-only case.
        if config.project_filter.len() == 1 {
            project_hint = config.project_filter.first().cloned();
        }
    }

    let cwd_hint_owned = resolve_codescribe_cwd_hint(home, project_hint.as_deref());
    let cwd_hint = cwd_hint_owned.as_deref();
    let segments = match path.extension().and_then(|ext| ext.to_str()) {
        Some("json") => parse_codescribe_json(&content)?,
        Some("md") => parse_codescribe_markdown(&content),
        _ => parse_plain_codescribe_text(&content),
    };

    let session_id = format!(
        "{}-{}-codescribe-{}",
        codescribe_path_fingerprint(path),
        path.file_stem()
            .map(|stem| stem.to_string_lossy())
            .unwrap_or_else(|| "unknown".into()),
        date.format("%Y-%m-%d")
    );
    let source_file = path.display();

    let mut entries = Vec::new();
    for segment in segments {
        let timestamp = codescribe_timestamp(path, date, segment.start_ms);
        if timestamp < config.cutoff || config.watermark.is_some_and(|w| timestamp < w) {
            continue;
        }

        let speaker_hint = lexicon.speaker_hint(segment.speaker.as_deref(), &segment.text);
        let duration = segment
            .duration_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let message = format!(
            "kind: {CODESCRIBE_TRANSCRIPT_KIND}\nspeaker_hint: {speaker_hint}\nsource_file: {source_file}\naudio_offset_ms: {}\nduration_ms: {duration}\n\n{}",
            segment.start_ms, segment.text
        );

        entries.push(build_timeline_entry(
            timestamp,
            CODESCRIBE_AGENT,
            &session_id,
            "user",
            message,
            TimelineEntryMeta {
                cwd: cwd_hint.map(ToOwned::to_owned),
                frame_kind: Some(FrameKind::UserMsg),
                ..TimelineEntryMeta::default()
            },
        ));
    }

    Ok(entries)
}

fn codescribe_path_fingerprint(path: &Path) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Extract CodeScribe transcript entries from `$HOME/.codescribe/transcriptions`.
pub fn extract_codescribe(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let home = dirs::home_dir().context("No home dir")?;
    extract_codescribe_from_home(&home, config)
}

/// Extract CodeScribe transcript entries using an explicit home directory.
pub fn extract_codescribe_from_home(
    home: &Path,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let lexicon = load_codescribe_lexicon(home);
    let mut entries = Vec::new();

    for transcript in discover_codescribe_transcripts(home) {
        match parse_codescribe_transcript_with_lexicon(
            &transcript.path,
            transcript.date,
            config,
            &lexicon,
            home,
        ) {
            Ok(mut parsed) => entries.append(&mut parsed),
            Err(e) => eprintln!(
                "CodeScribe transcript extraction warning ({}): {}",
                transcript.path.display(),
                e
            ),
        }
    }

    if !config.project_filter.is_empty() {
        entries.retain(|entry| {
            if let Some(cwd) = &entry.cwd {
                project_filter_matches_path(cwd, &config.project_filter)
            } else {
                false
            }
        });
    }

    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

fn resolve_codescribe_cwd_hint(home: &Path, project_hint: Option<&str>) -> Option<String> {
    let project = project_hint?.trim();
    if project.is_empty() {
        return None;
    }

    let (org, repo) = project.split_once('/').unwrap_or(("", project));

    let candidates = if !org.is_empty() {
        vec![
            home.join(org).join(repo),
            home.join("Libraxis").join(org).join(repo),
            home.join("Libraxis")
                .join("01_deployed_libraxis_vm")
                .join(org)
                .join(repo),
            home.join("Libraxis")
                .join("vc-runtime")
                .join(org)
                .join(repo),
            home.join("hosted").join(org).join(repo),
            home.join("vc-workspace").join(org).join(repo),
        ]
    } else {
        vec![
            home.join(repo),
            home.join("Libraxis").join(repo),
            home.join("Libraxis")
                .join("01_deployed_libraxis_vm")
                .join(repo),
            home.join("Libraxis").join("vc-runtime").join(repo),
            home.join("hosted").join("VetCoders").join(repo),
            home.join("vc-workspace").join("VetCoders").join(repo),
        ]
    };

    candidates
        .into_iter()
        .find(|candidate| candidate.is_dir())
        .map(|candidate| candidate.display().to_string())
}

/// Discover recent operator markdown files from the standard operator inboxes.
pub fn discover_operator_markdown(home: &Path) -> Vec<OperatorMarkdown> {
    discover_operator_markdown_from(home, None, None)
}

/// Discover operator markdown files, optionally including `<repo>/docs/operator`.
///
/// `caller_cutoff` is the earliest file-mtime the caller is interested in:
/// - `None` falls back to a 30-day default window
///   ([`OPERATOR_MD_RECENT_DAYS`]). This is the legacy convenience for source
///   enumeration paths that have no [`ExtractionConfig`] to hand in.
/// - `Some(t)` honors `t` directly. `t = UNIX epoch` therefore means
///   "all time", which is what `aicx store --agent operator-md -H 0` needs.
pub fn discover_operator_markdown_from(
    home: &Path,
    repo_root: Option<&Path>,
    caller_cutoff: Option<DateTime<Utc>>,
) -> Vec<OperatorMarkdown> {
    let mut dirs = vec![
        home.join("Downloads"),
        home.join(".vibecrafted").join("inbox"),
    ];
    if let Some(repo_root) = repo_root {
        dirs.push(repo_root.join("docs").join("operator"));
    }

    let cutoff =
        caller_cutoff.unwrap_or_else(|| Utc::now() - Duration::days(OPERATOR_MD_RECENT_DAYS));
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    for dir in dirs {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let modified = DateTime::<Utc>::from(modified);
            if modified < cutoff || !seen.insert(path.clone()) {
                continue;
            }
            entries.push(OperatorMarkdown { path, modified });
        }
    }

    entries.sort_by_key(|entry| (entry.modified, entry.path.clone()));
    entries
}

/// Extract operator-authored markdown from Downloads, the Vibecrafted inbox,
/// and the current repo's `docs/operator` directory when present.
pub fn extract_operator_markdown(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let home = dirs::home_dir().context("No home dir")?;
    let repo_root = std::env::current_dir()
        .ok()
        .and_then(|cwd| discover_git_root_from_path(&cwd));
    extract_operator_markdown_from_home_and_repo(&home, repo_root.as_deref(), config)
}

/// Extract operator-authored markdown using an explicit home directory.
pub fn extract_operator_markdown_from_home(
    home: &Path,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    extract_operator_markdown_from_home_and_repo(home, None, config)
}

/// Extract operator-authored markdown using explicit home and repo roots.
pub fn extract_operator_markdown_from_home_and_repo(
    home: &Path,
    repo_root: Option<&Path>,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let mut entries = Vec::new();

    for document in discover_operator_markdown_from(home, repo_root, Some(config.cutoff)) {
        match parse_operator_markdown_document(home, &document, config) {
            Ok(mut parsed) => entries.append(&mut parsed),
            Err(e) => eprintln!(
                "Operator markdown extraction warning ({}): {}",
                document.path.display(),
                e
            ),
        }
    }

    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

fn parse_operator_markdown_document(
    home: &Path,
    document: &OperatorMarkdown,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let content = sanitize::read_to_string_validated(&document.path)?;
    let (frontmatter, body) = split_operator_frontmatter(&content);
    let project_hint = infer_operator_project_hint(&frontmatter, &body, &document.path, config);
    let cwd_hint = resolve_operator_cwd_hint(home, &document.path, project_hint.as_deref());
    let base_timestamp = frontmatter
        .date
        .as_deref()
        .and_then(parse_operator_timestamp)
        .unwrap_or(document.modified);
    let session_id = format!(
        "{}-{}",
        operator_path_fingerprint(&document.path),
        document
            .path
            .file_stem()
            .map(|stem| stem.to_string_lossy())
            .unwrap_or_else(|| "operator-md".into())
    );

    let mut entries = Vec::new();
    let mut heading: Option<String> = None;
    let mut sequence = 0i64;

    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(next_heading) = parse_markdown_heading(line) {
            heading = Some(next_heading);
            continue;
        }

        let parsed = if let Some((done, task)) = parse_operator_checklist_task(line) {
            if done {
                None
            } else {
                Some(OperatorMarkdownSignal {
                    kind: "task",
                    severity: None,
                    display_line: format!("- [ ] {task}"),
                    text: task,
                })
            }
        } else if let Some(decision) = strip_operator_prefix(line, "Decision:") {
            Some(OperatorMarkdownSignal {
                kind: "decision",
                severity: None,
                text: decision.to_string(),
                display_line: format!("Decision: {}", decision.trim()),
            })
        } else if let Some(outcome) = strip_operator_prefix(line, "Outcome:") {
            Some(OperatorMarkdownSignal {
                kind: "outcome",
                severity: None,
                text: outcome.to_string(),
                display_line: format!("Outcome: {}", outcome.trim()),
            })
        } else {
            operator_severity_marker(line).map(|severity| {
                let text = strip_operator_severity_prefix(line, severity);
                OperatorMarkdownSignal {
                    kind: "intent",
                    severity: Some(severity),
                    text: text.to_string(),
                    display_line: format!("Intent: [{severity}] {}", text.trim()),
                }
            })
        };

        let Some(signal) = parsed else {
            continue;
        };
        let timestamp = base_timestamp + Duration::seconds(sequence);
        sequence += 1;
        if timestamp < config.cutoff || config.watermark.is_some_and(|w| timestamp < w) {
            continue;
        }

        entries.push(build_timeline_entry(
            timestamp,
            OPERATOR_MD_AGENT,
            &session_id,
            "user",
            format_operator_markdown_message(
                &document.path,
                &frontmatter,
                heading.as_deref(),
                &signal,
            ),
            TimelineEntryMeta {
                cwd: cwd_hint.clone(),
                frame_kind: Some(FrameKind::UserMsg),
                ..TimelineEntryMeta::default()
            },
        ));
    }

    Ok(entries)
}

#[derive(Debug, Clone)]
struct OperatorMarkdownSignal {
    kind: &'static str,
    severity: Option<&'static str>,
    text: String,
    display_line: String,
}

fn format_operator_markdown_message(
    path: &Path,
    frontmatter: &OperatorMarkdownFrontmatter,
    heading: Option<&str>,
    signal: &OperatorMarkdownSignal,
) -> String {
    let mut message = format!(
        "source: {OPERATOR_MD_KIND}\nkind: {}\nsource_file: {}",
        signal.kind,
        path.display()
    );
    if let Some(severity) = signal.severity {
        message.push_str(&format!("\nseverity: {severity}"));
    }
    if let Some(project) = frontmatter
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        message.push_str(&format!("\nproject: {}", project.trim()));
    }
    if let Some(author) = frontmatter
        .author
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        message.push_str(&format!("\nauthor: {}", author.trim()));
    }
    if let Some(heading) = heading.filter(|value| !value.trim().is_empty()) {
        message.push_str(&format!("\nheading: {}", heading.trim()));
    }
    message.push_str("\n\n");
    message.push_str(signal.display_line.trim());
    if !signal.text.trim().is_empty() && !signal.display_line.contains(signal.text.trim()) {
        message.push_str(&format!("\n{}", signal.text.trim()));
    }
    message
}

fn split_operator_frontmatter(content: &str) -> (OperatorMarkdownFrontmatter, String) {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (OperatorMarkdownFrontmatter::default(), content.to_string());
    }

    let mut yaml = Vec::new();
    let mut body = Vec::new();
    let mut in_yaml = true;
    for line in lines {
        if in_yaml && line.trim() == "---" {
            in_yaml = false;
            continue;
        }
        if in_yaml {
            yaml.push(line);
        } else {
            body.push(line);
        }
    }

    if in_yaml {
        return (OperatorMarkdownFrontmatter::default(), content.to_string());
    }

    let frontmatter =
        serde_yaml::from_str::<OperatorMarkdownFrontmatter>(&yaml.join("\n")).unwrap_or_default();
    (frontmatter, body.join("\n"))
}

fn parse_operator_timestamp(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Some(timestamp.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%d", "%Y_%m%d"] {
        if let Ok(date) = NaiveDate::parse_from_str(value, format)
            && let Some(time) = NaiveTime::from_hms_opt(0, 0, 0)
        {
            return Some(Utc.from_utc_datetime(&date.and_time(time)));
        }
    }
    None
}

fn parse_markdown_heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let text = trimmed.get(level..)?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn parse_operator_checklist_task(line: &str) -> Option<(bool, String)> {
    let line = line.trim_start();
    let mut chars = line.chars();
    if !matches!(chars.next()?, '-' | '*' | '+') {
        return None;
    }
    let rest = chars.as_str().trim_start().strip_prefix('[')?;
    let mut chars = rest.chars();
    let state = chars.next()?;
    let rest = chars.as_str().strip_prefix(']')?;
    let task = rest.trim_start();
    if task.is_empty() {
        return None;
    }
    match state {
        ' ' => Some((false, task.to_string())),
        'x' | 'X' => Some((true, task.to_string())),
        _ => None,
    }
}

fn strip_operator_prefix<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = strip_operator_bullet(line);
    if trimmed.len() < prefix.len() {
        return None;
    }
    let candidate = trimmed.get(..prefix.len())?;
    candidate
        .eq_ignore_ascii_case(prefix)
        .then(|| trimmed.get(prefix.len()..).unwrap_or("").trim())
        .filter(|value| !value.is_empty())
}

fn strip_operator_bullet(line: &str) -> &str {
    line.trim().trim_start_matches(['-', '*', '+']).trim_start()
}

fn operator_severity_marker(line: &str) -> Option<&'static str> {
    let upper = line.to_ascii_uppercase();
    let has_marker = |marker: &str| {
        upper
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|token| token == marker)
    };
    ["P0", "P1", "P2"]
        .into_iter()
        .find(|marker| has_marker(marker))
}

fn strip_operator_severity_prefix<'a>(line: &'a str, severity: &str) -> &'a str {
    let stripped = strip_operator_bullet(line);
    let Some(rest) = stripped.get(severity.len()..) else {
        return stripped.trim();
    };
    if stripped
        .get(..severity.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(severity))
    {
        rest.trim_start_matches([' ', '-', ':', ']']).trim()
    } else {
        stripped.trim()
    }
}

fn infer_operator_project_hint(
    frontmatter: &OperatorMarkdownFrontmatter,
    body: &str,
    path: &Path,
    config: &ExtractionConfig,
) -> Option<String> {
    if let Some(project) = frontmatter
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return Some(project.trim().to_string());
    }
    if config.project_filter.len() == 1 {
        return config.project_filter.first().cloned();
    }

    let lower_path = path.to_string_lossy().to_ascii_lowercase();
    let lower_body = body.to_ascii_lowercase();
    for candidate in ["rust-memex", "aicx", "loctree", "vc-context-engine"] {
        if lower_path.contains(candidate) || lower_body.contains(candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

fn resolve_operator_cwd_hint(
    home: &Path,
    path: &Path,
    project_hint: Option<&str>,
) -> Option<String> {
    if path
        .components()
        .any(|component| component.as_os_str().to_string_lossy() == "docs")
        && path
            .components()
            .any(|component| component.as_os_str().to_string_lossy() == "operator")
        && let Some(root) = discover_git_root_from_path(path)
    {
        return Some(root.display().to_string());
    }

    let project = project_hint?.trim();
    if project.is_empty() {
        return None;
    }
    let (org, repo) = project.split_once('/').unwrap_or(("", project));

    let candidates = if !org.is_empty() {
        vec![
            home.join(org).join(repo),
            home.join("Libraxis").join(org).join(repo),
            home.join("Libraxis")
                .join("vc-runtime")
                .join(org)
                .join(repo),
            home.join("Libraxis")
                .join("01_deployed_libraxis_vm")
                .join(org)
                .join(repo),
            home.join("hosted").join(org).join(repo),
            home.join("vc-workspace").join(org).join(repo),
        ]
    } else {
        vec![
            home.join(repo),
            home.join("Libraxis").join(repo),
            home.join("Libraxis").join("vc-runtime").join(repo),
            home.join("Libraxis")
                .join("01_deployed_libraxis_vm")
                .join(repo),
            home.join("hosted").join("VetCoders").join(repo),
            home.join("vc-workspace").join("VetCoders").join(repo),
        ]
    };

    candidates
        .into_iter()
        .find(|candidate| candidate.is_dir())
        .map(|candidate| candidate.display().to_string())
}

fn discover_git_root_from_path(path: &Path) -> Option<PathBuf> {
    let seed = if path.is_file() { path.parent()? } else { path };
    seed.ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
}

fn operator_path_fingerprint(path: &Path) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

// ============================================================================
// Combined extractor
// ============================================================================

/// Extract from all sources, merge, sort, and deduplicate.
pub fn extract_all(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let mut all: Vec<TimelineEntry> = Vec::new();

    // Claude
    match extract_claude(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Claude extraction warning: {}", e),
    }

    // Codex
    match extract_codex(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Codex extraction warning: {}", e),
    }

    // Gemini
    match extract_gemini(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Gemini extraction warning: {}", e),
    }

    // Junie
    match extract_junie(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Junie extraction warning: {}", e),
    }

    // CodeScribe
    match extract_codescribe(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("CodeScribe extraction warning: {}", e),
    }

    // Operator markdown
    match extract_operator_markdown(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Operator markdown extraction warning: {}", e),
    }

    // Claude history.jsonl
    match extract_claude_history(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Claude history extraction warning: {}", e),
    }

    // Codex sessions
    match extract_codex_sessions(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Codex sessions extraction warning: {}", e),
    }

    // Sort by timestamp
    all.sort_by_key(|a| a.timestamp);

    // Dedup: same timestamp + same first 100 chars of message -> keep first
    let mut seen: HashSet<(i64, String)> = HashSet::new();
    all.retain(|entry| {
        let key_msg: String = format!(
            "{}:{}",
            entry.frame_kind.map(FrameKind::as_str).unwrap_or("unknown"),
            entry.message.chars().take(100).collect::<String>()
        );
        let key = (entry.timestamp.timestamp(), key_msg);
        seen.insert(key)
    });

    Ok(all)
}

// ============================================================================
// List helper
// ============================================================================

fn source_root_for_protection(path: &Path) -> PathBuf {
    if path.is_file() {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn discover_protecting_git_root(path: &Path, home: &Path) -> Option<PathBuf> {
    let source_root = source_root_for_protection(path);
    let mut current = Some(source_root.as_path());

    while let Some(candidate) = current {
        if candidate.join(".git").is_dir() {
            return Some(candidate.to_path_buf());
        }
        if candidate == home {
            break;
        }
        current = candidate.parent();
    }

    None
}

fn git_remote_lines(root: &Path) -> Vec<String> {
    let Ok(output) = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["remote", "-v"])
        .output()
    else {
        return Vec::new();
    };

    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn source_info(
    home: &Path,
    agent: impl Into<String>,
    path: PathBuf,
    sessions: usize,
    size_bytes: u64,
) -> SourceInfo {
    let protection_root = discover_protecting_git_root(&path, home);
    let git_remotes = protection_root
        .as_deref()
        .map(git_remote_lines)
        .unwrap_or_default();
    let protected_by_git = protection_root.is_some();

    SourceInfo {
        agent: agent.into(),
        path,
        sessions,
        size_bytes,
        protected_by_git,
        protection_backend: if protected_by_git {
            "git-local".to_string()
        } else {
            "none".to_string()
        },
        protection_root,
        git_remote_count: git_remotes.len(),
        git_remotes,
        protection_warning: (!protected_by_git).then(|| UNPROTECTED_SOURCE_WARNING.to_string()),
    }
}

/// List available sources with session counts, sizes, and read-only protection status.
pub fn list_available_sources() -> Result<Vec<SourceInfo>> {
    let home = dirs::home_dir().context("No home dir")?;
    let mut sources: Vec<SourceInfo> = Vec::new();

    // Claude
    let claude_dir = home.join(".claude").join("projects");
    if claude_dir.exists() && claude_dir.is_dir() {
        for dir_entry in fs::read_dir(&claude_dir)? {
            let dir_entry = dir_entry?;
            let path = dir_entry.path();
            if !path.is_dir() {
                continue;
            }

            let mut session_count = 0usize;
            let mut total_size = 0u64;

            for file_entry in fs::read_dir(&path)? {
                let file_entry = file_entry?;
                let fp = file_entry.path();
                if fp.extension().is_some_and(|e| e == "jsonl") {
                    session_count += 1;
                    if let Ok(meta) = fs::metadata(&fp) {
                        total_size += meta.len();
                    }
                }
            }

            if session_count > 0 {
                sources.push(source_info(
                    &home,
                    "claude",
                    path,
                    session_count,
                    total_size,
                ));
            }
        }
    }

    // Claude history.jsonl
    let claude_history = home.join(".claude").join("history.jsonl");
    if claude_history.exists() {
        let size = fs::metadata(&claude_history).map(|m| m.len()).unwrap_or(0);
        sources.push(source_info(
            &home,
            "claude-history",
            claude_history,
            1,
            size,
        ));
    }

    // Codex
    let codex_path = home.join(".codex").join("history.jsonl");
    if codex_path.exists() {
        let size = fs::metadata(&codex_path).map(|m| m.len()).unwrap_or(0);
        let sessions = count_codex_sessions(&codex_path).unwrap_or(0);
        sources.push(source_info(&home, "codex", codex_path, sessions, size));
    }

    // Codex sessions: ~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl
    let codex_sessions_dir = home.join(".codex").join("sessions");
    if codex_sessions_dir.exists() && codex_sessions_dir.is_dir() {
        let files = walk_jsonl_files(&codex_sessions_dir);
        let total_size: u64 = files
            .iter()
            .filter_map(|f| fs::metadata(f).ok())
            .map(|m| m.len())
            .sum();
        if !files.is_empty() {
            sources.push(source_info(
                &home,
                "codex-sessions",
                codex_sessions_dir,
                files.len(),
                total_size,
            ));
        }
    }

    // Gemini CLI: ~/.gemini/tmp/<projectHash>/chats/session-*.json[l]
    let gemini_tmp = home.join(".gemini").join("tmp");
    if gemini_tmp.exists() && gemini_tmp.is_dir() {
        for project_entry in fs::read_dir(&gemini_tmp)? {
            let project_entry = project_entry?;
            let project_path = project_entry.path();

            if !project_path.is_dir() {
                continue;
            }

            let chats_dir = project_path.join("chats");
            if !chats_dir.exists() || !chats_dir.is_dir() {
                continue;
            }

            let mut session_count = 0usize;
            let mut total_size = 0u64;

            for file_entry in fs::read_dir(&chats_dir)? {
                let file_entry = file_entry?;
                let fp = file_entry.path();
                if is_gemini_session_file(&fp) {
                    session_count += 1;
                    if let Ok(meta) = fs::metadata(&fp) {
                        total_size += meta.len();
                    }
                }
            }

            if session_count > 0 {
                sources.push(source_info(
                    &home,
                    "gemini",
                    project_path,
                    session_count,
                    total_size,
                ));
            }
        }
    }

    // Junie sessions: ~/.junie/sessions/session-*/events.jsonl
    let junie_sessions = home.join(".junie").join("sessions");
    if junie_sessions.exists() && junie_sessions.is_dir() {
        let files: Vec<PathBuf> = walk_jsonl_files(&junie_sessions)
            .into_iter()
            .filter(|path| {
                path.file_name().and_then(|name| name.to_str()) == Some(JUNIE_EVENTS_FILENAME)
            })
            .collect();
        let total_size: u64 = files
            .iter()
            .filter_map(|file| fs::metadata(file).ok())
            .map(|metadata| metadata.len())
            .sum();
        if !files.is_empty() {
            sources.push(source_info(
                &home,
                "junie",
                junie_sessions,
                files.len(),
                total_size,
            ));
        }
    }

    // CodeScribe transcripts: ~/.codescribe/transcriptions/YYYY-MM-DD/*.{txt,md,json}
    let codescribe_transcripts = discover_codescribe_transcripts(&home);
    if !codescribe_transcripts.is_empty() {
        let total_size: u64 = codescribe_transcripts
            .iter()
            .filter_map(|transcript| fs::metadata(&transcript.path).ok())
            .map(|metadata| metadata.len())
            .sum();
        sources.push(source_info(
            &home,
            CODESCRIBE_AGENT,
            home.join(".codescribe").join("transcriptions"),
            codescribe_transcripts.len(),
            total_size,
        ));
    }

    // Operator markdown: ~/Downloads/*.md and ~/.vibecrafted/inbox/*.md
    let operator_markdown = discover_operator_markdown(&home);
    if !operator_markdown.is_empty() {
        let total_size: u64 = operator_markdown
            .iter()
            .filter_map(|document| fs::metadata(&document.path).ok())
            .map(|metadata| metadata.len())
            .sum();
        sources.push(source_info(
            &home,
            "operator-md",
            home.join("Downloads"),
            operator_markdown.len(),
            total_size,
        ));
    }

    Ok(sources)
}

/// Determine the project/repo name for a given entry.
///
/// 1. If a single project filter is active, it unconditionally becomes the project name.
/// 2. If multiple filters are active, uses the first one matching the `cwd`.
/// 3. Otherwise, tries to walk up the `cwd` path to find a `.git` root.
/// 4. Fallback: last path component of `cwd`.
pub fn repo_name_from_cwd(cwd: Option<&str>, project_filter: &[String]) -> String {
    if !project_filter.is_empty() {
        if project_filter.len() == 1 {
            return canonical_repo_label(&project_filter[0]);
        } else if let Some(c) = cwd {
            for p in project_filter {
                // Use the same word-boundary path match as the project
                // filter itself, so a label cannot be picked via a raw
                // substring (`--project test` against `/tmp/fastest-project`)
                // even though the filter step rejects that match.
                if project_filter_matches_path(c, std::slice::from_ref(p)) {
                    return canonical_repo_label(p);
                }
            }
        }
    }

    let cwd_str = match cwd {
        Some(c) if !c.is_empty() => c,
        _ => return "unknown".to_string(),
    };

    let path = std::path::Path::new(cwd_str);
    let mut current = Some(path);

    while let Some(p) = current {
        if !p.as_os_str().is_empty()
            && p.join(".git").is_dir()
            && let Some(name) = p.file_name()
        {
            return canonical_repo_label(&name.to_string_lossy());
        }
        current = p.parent();
    }

    path.file_name()
        .map(|n| canonical_repo_label(&n.to_string_lossy()))
        .unwrap_or_else(|| "unknown".to_string())
}

fn canonical_repo_label(value: &str) -> String {
    value
        .split('/')
        .map(|segment| segment.trim().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

/// Derive canonical repo labels from extracted entries.
pub fn repo_labels_from_entries(
    entries: &[TimelineEntry],
    project_filter: &[String],
) -> Vec<String> {
    let mut labels = BTreeSet::new();

    for entry in entries {
        let repo = repo_name_from_cwd(entry.cwd.as_deref(), project_filter);
        if repo != "unknown" {
            labels.insert(repo);
        }
    }

    labels.into_iter().collect()
}

/// Detect project name from current working directory.
///
/// Strategy: git repo root dirname → cwd dirname → "unknown".
pub fn detect_project_name() -> String {
    // Try git repo root
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        && output.status.success()
    {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(name) = std::path::Path::new(&s).file_name() {
            return canonical_repo_label(&name.to_string_lossy());
        }
    }

    // Fallback: cwd dirname
    if let Ok(cwd) = std::env::current_dir()
        && let Some(name) = cwd.file_name()
    {
        return canonical_repo_label(&name.to_string_lossy());
    }

    "unknown".to_string()
}

/// Count unique sessions in the Codex history file.
fn count_codex_sessions(path: &std::path::Path) -> Result<usize> {
    let file = sanitize::open_file_validated(path)?;
    let mut reader = BufReader::new(file);
    let mut sessions: HashSet<String> = HashSet::new();
    let mut oversized_count = 0usize;
    let mut oversized_samples = Vec::new();
    let mut line_number = 0usize;

    while let Some(limited) = sanitize::read_line_capped(&mut reader, MAX_LINE_BYTES)? {
        line_number += 1;
        if limited.exceeded {
            observe_oversized_line(&mut oversized_count, &mut oversized_samples, line_number);
            continue;
        }
        let line = limited.line;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<CodexEntry>(&line) {
            sessions.insert(entry.session_id);
        }
    }

    if oversized_count > 0 {
        emit_codex_session_warnings(
            path,
            &[CodexSessionWarning::OversizedLine {
                count: oversized_count,
                samples: oversized_samples,
            }],
        );
    }

    Ok(sessions.len())
}

// ============================================================================
// Utilities
// ============================================================================

/// Decode a Claude project path from the encoded directory name.
///
/// Claude's project-dir encoding is lossy: `-` may be either a literal
/// repository-name character or an encoded `/`. Replacing every dash with `/`
/// turns hyphenated repos into fake path segments, so this helper preserves the
/// stored name and only removes the leading absolute-path sentinel.
///
/// Example: `-Users-maciejgad-hosted-VetCoders-CodeScribe`
///       -> `Users-maciejgad-hosted-VetCoders-CodeScribe`
pub fn decode_claude_project_path(encoded: &str) -> String {
    let stripped = encoded.strip_prefix('-').unwrap_or(encoded);
    stripped.to_string()
}

/// Extract text content from a Claude message value.
///
/// Handles the various formats Claude uses:
/// - Plain string
/// - Array of content blocks with type "text"
/// - Object with "content" field (string or array of blocks)
/// - Object with direct "text" field
fn extract_message_text(message: &Option<serde_json::Value>) -> String {
    match message {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|item| {
                if let Some(obj) = item.as_object()
                    && obj.get("type").and_then(|t| t.as_str()) == Some("text")
                {
                    return obj.get("text").and_then(|t| t.as_str()).map(String::from);
                }
                None
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(serde_json::Value::Object(obj)) => {
            if let Some(content) = obj.get("content") {
                match content {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(arr) => arr
                        .iter()
                        .filter_map(|item| {
                            if let Some(block) = item.as_object()
                                && block.get("type").and_then(|t| t.as_str()) == Some("text")
                            {
                                return block
                                    .get("text")
                                    .and_then(|t| t.as_str())
                                    .map(String::from);
                            }
                            None
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                }
            } else if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                text.to_string()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests;

#[cfg(test)]
mod conversation_tests;
