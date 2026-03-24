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
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::sanitize;

// ============================================================================
// Public types
// ============================================================================

/// Unified timeline entry from any AI agent source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub timestamp: DateTime<Utc>,
    pub agent: String,
    pub session_id: String,
    pub role: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Denoised conversation message — the canonical projection of a TimelineEntry
/// containing only user/assistant messages with repo-centric identity.
///
/// This is the primary unit for "recover the conversation" workflows.
/// Tool calls, tool results, reasoning/thoughts, system noise, and artifact
/// payloads are excluded. Artifact paths may appear as references only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub timestamp: DateTime<Utc>,
    pub agent: String,
    pub session_id: String,
    /// Only "user" or "assistant" — reasoning and system roles are excluded.
    pub role: String,
    /// Raw, untrimmed, untruncated message body.
    pub message: String,
    /// Canonical project/repo identity (derived from cwd + project filter).
    pub repo_project: String,
    /// Secondary provenance: source working directory path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// Git branch at time of message (when available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Project timeline entries into a denoised conversation stream.
///
/// Filters to only `user` and `assistant` roles, resolves repo/project identity
/// from `cwd` + project filter, and preserves provenance fields.
pub fn to_conversation(
    entries: &[TimelineEntry],
    project_filter: &[String],
) -> Vec<ConversationMessage> {
    entries
        .iter()
        .filter(|e| e.role == "user" || e.role == "assistant")
        .map(|e| ConversationMessage {
            timestamp: e.timestamp,
            agent: e.agent.clone(),
            session_id: e.session_id.clone(),
            role: e.role.clone(),
            message: e.message.clone(),
            repo_project: repo_name_from_cwd(e.cwd.as_deref(), project_filter),
            source_path: e.cwd.clone(),
            branch: e.branch.clone(),
        })
        .collect()
}

/// Configuration for extraction.
#[derive(Debug, Clone)]
pub struct ExtractionConfig {
    pub project_filter: Vec<String>,
    pub cutoff: DateTime<Utc>,
    pub include_assistant: bool,
    pub watermark: Option<DateTime<Utc>>,
}

/// Info about an available source directory/file.
#[derive(Debug, Clone, Serialize)]
pub struct SourceInfo {
    pub agent: String,
    pub path: PathBuf,
    pub sessions: usize,
    pub size_bytes: u64,
}

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
    #[allow(dead_code)] // Deserialized but we use filename stem as session_id instead
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(rename = "gitBranch", default)]
    git_branch: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
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

/// Gemini CLI session file (~/.gemini/tmp/<hash>/chats/session-*.json).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiSession {
    #[serde(default)]
    session_id: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    project_hash: Option<String>,
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
                map.insert("inlineDataPlaceholder".to_string(), serde_json::Value::String(placeholder));
            }
            if let Some(file_data) = map.get("fileData") {
                let placeholder = render_gemini_file_data_placeholder(file_data);
                map.remove("fileData");
                map.insert("fileDataPlaceholder".to_string(), serde_json::Value::String(placeholder));
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

fn gemini_message_matches_filter(message: &GeminiMessage, filters_lower: &[String]) -> bool {
    let content = render_gemini_message_content(message);
    let project_hint = infer_project_hint_from_gemini_message(message);

    filters_lower.iter().any(|filter| {
        content
            .as_ref()
            .is_some_and(|text| text.to_lowercase().contains(filter))
            || project_hint
                .as_ref()
                .is_some_and(|cwd| cwd.to_lowercase().contains(filter))
    })
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
        let dir_matches = if config.project_filter.is_empty() {
            true
        } else {
            let decoded = decode_claude_project_path(&dir_name);
            let decoded_lower = decoded.to_lowercase();
            let dir_lower = dir_name.to_lowercase();
            config.project_filter.iter().any(|f| {
                let fl = f.to_lowercase();
                decoded_lower.contains(&fl) || dir_lower.contains(&fl)
            })
        };

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
                let keep_session = dir_matches || {
                    if config.project_filter.is_empty() {
                        true
                    } else {
                        let filters_lower: Vec<String> = config
                            .project_filter
                            .iter()
                            .map(|f| f.to_lowercase())
                            .collect();

                        session_entries.iter().any(|entry| {
                            filters_lower.iter().any(|fl| {
                                entry.message.to_lowercase().contains(fl)
                                    || entry
                                        .cwd
                                        .as_ref()
                                        .is_some_and(|c| c.to_lowercase().contains(fl))
                            })
                        })
                    }
                };

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

    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(entries)
}

/// Parse a single Claude JSONL file into timeline entries.
fn parse_claude_jsonl(
    path: &std::path::Path,
    session_id: &str,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let file = sanitize::open_file_validated(path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let entry: ClaudeEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Only process user/assistant messages
        if entry.entry_type != "user" && entry.entry_type != "assistant" {
            continue;
        }

        // Skip assistant messages if not requested
        if !config.include_assistant && entry.entry_type == "assistant" {
            continue;
        }

        // Parse timestamp
        let timestamp = match &entry.timestamp {
            Some(ts) => match DateTime::parse_from_rfc3339(ts) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(_) => continue,
            },
            None => continue,
        };

        // Respect cutoff
        if timestamp < config.cutoff {
            continue;
        }

        // Respect watermark (skip already-processed entries)
        if config.watermark.is_some_and(|wm| timestamp <= wm) {
            continue;
        }

        // Extract message text
        let message = extract_message_text(&entry.message);
        if message.is_empty() {
            continue;
        }

        entries.push(TimelineEntry {
            timestamp,
            agent: "claude".to_string(),
            session_id: session_id.to_string(),
            role: entry.entry_type,
            message,
            branch: entry.git_branch,
            cwd: entry.cwd,
        });
    }

    Ok(entries)
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
    let file = sanitize::open_file_validated(path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    let default_session_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let entry: ClaudeEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Only process user/assistant messages
        if entry.entry_type != "user" && entry.entry_type != "assistant" {
            continue;
        }

        // Skip assistant messages if not requested
        if !config.include_assistant && entry.entry_type == "assistant" {
            continue;
        }

        // Parse timestamp
        let timestamp = match &entry.timestamp {
            Some(ts) => match DateTime::parse_from_rfc3339(ts) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(_) => continue,
            },
            None => continue,
        };

        // Respect cutoff
        if timestamp < config.cutoff {
            continue;
        }

        // Respect watermark (skip already-processed entries)
        if config.watermark.is_some_and(|wm| timestamp <= wm) {
            continue;
        }

        // Extract message text
        let message = extract_message_text(&entry.message);
        if message.is_empty() {
            continue;
        }

        // Prefer embedded sessionId when present (task outputs are often renamed).
        let session_id = entry
            .session_id
            .unwrap_or_else(|| default_session_id.clone());

        entries.push(TimelineEntry {
            timestamp,
            agent: "claude".to_string(),
            session_id,
            role: entry.entry_type,
            message,
            branch: entry.git_branch,
            cwd: entry.cwd,
        });
    }

    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(entries)
}

/// Extract timeline entries from a single Codex JSONL file by path.
///
/// Supports both:
/// - Codex history format (`~/.codex/history.jsonl`) — `CodexEntry` per line.
/// - Codex session format (`~/.codex/sessions/**/**/*.jsonl`) — `CodexSessionEvent` per line.
pub fn extract_codex_file(path: &Path, config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let file = sanitize::open_file_validated(path)?;
    let reader = BufReader::new(file);

    // Detect file format from the first non-empty line.
    let mut first_line: Option<String> = None;
    for line in reader.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            first_line = Some(line);
            break;
        }
    }

    let Some(first_line) = first_line else {
        return Ok(vec![]);
    };

    // History file: parse as CodexEntry (per line).
    if serde_json::from_str::<CodexEntry>(&first_line).is_ok() {
        let file = sanitize::open_file_validated(path)?;
        let reader = BufReader::new(file);

        // First pass: group by session_id (same behavior as extract_codex()).
        let mut sessions: HashMap<String, Vec<CodexEntry>> = HashMap::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let entry: CodexEntry = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(_) => continue,
            };

            sessions
                .entry(entry.session_id.clone())
                .or_default()
                .push(entry);
        }

        // Second pass: determine matching sessions (if filter provided).
        let matching_sessions: HashSet<String> = if !config.project_filter.is_empty() {
            let filters_lower: Vec<String> = config
                .project_filter
                .iter()
                .map(|f| f.to_lowercase())
                .collect();
            sessions
                .iter()
                .filter(|(_id, msgs)| {
                    filters_lower.iter().any(|fl| {
                        msgs.iter().any(|m| {
                            m.text.to_lowercase().contains(fl)
                                || m.cwd
                                    .as_ref()
                                    .is_some_and(|c| c.to_lowercase().contains(fl))
                        })
                    })
                })
                .map(|(id, _)| id.clone())
                .collect()
        } else {
            sessions.keys().cloned().collect()
        };

        // Third pass: build timeline entries from matching sessions.
        let mut entries: Vec<TimelineEntry> = Vec::new();

        for (session_id, msgs) in &sessions {
            if !matching_sessions.contains(session_id) {
                continue;
            }

            for msg in msgs {
                let timestamp = match Utc.timestamp_opt(msg.ts, 0).single() {
                    Some(ts) => ts,
                    None => continue,
                };

                // Respect cutoff
                if timestamp < config.cutoff {
                    continue;
                }

                // Respect watermark
                if config.watermark.is_some_and(|wm| timestamp <= wm) {
                    continue;
                }

                let role = msg.role.as_deref().unwrap_or("user").to_string();

                // Skip assistant messages if not requested
                if !config.include_assistant && role == "assistant" {
                    continue;
                }

                if msg.text.is_empty() {
                    continue;
                }

                entries.push(TimelineEntry {
                    timestamp,
                    agent: "codex".to_string(),
                    session_id: session_id.clone(),
                    role,
                    message: msg.text.clone(),
                    branch: None,
                    cwd: msg.cwd.clone(),
                });
            }
        }

        entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        return Ok(entries);
    }

    // Session file: parse as CodexSessionEvent (delegate to existing parser).
    if serde_json::from_str::<CodexSessionEvent>(&first_line).is_ok() {
        let mut entries = parse_codex_session_file(path, config)?;
        entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        return Ok(entries);
    }

    // Check for legacy JSON format ({"session": {...}, "items": [...]})
    // We read the full file because it's usually formatted JSON.
    if let Ok(content) = fs::read_to_string(path) {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
            if val.get("session").is_some() && val.get("items").is_some() {
                anyhow::bail!(
                    "Legacy Codex JSON rollout format is unsupported (no cwd available): {}",
                    path.display()
                );
            }
        }
    }

    Err(anyhow::anyhow!(
        "Unsupported codex file format: {}",
        path.display()
    ))
}

/// Extract timeline entries from a single Gemini CLI session JSON file by path.
///
/// Gemini sessions are JSON (not JSONL) and live under:
/// `~/.gemini/tmp/<hash>/chats/session-*.json`
pub fn extract_gemini_file(path: &Path, config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let mut entries = parse_gemini_session(path, config)?;
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
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
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
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
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

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
        if config.watermark.is_some_and(|wm| timestamp <= wm) {
            continue;
        }

        entries.push(TimelineEntry {
            timestamp,
            agent: "gemini-antigravity".to_string(),
            session_id: input.conversation_id.clone(),
            role: "artifact".to_string(),
            message: format!(
                "Antigravity step output fallback\nsource: {}\nfull_transcript_available: false\n\n{}",
                path.display(),
                trimmed
            ),
            branch: None,
            cwd: infer_project_hint_from_text(trimmed).or_else(|| session_default_cwd.clone()),
        });
    }

    if entries.is_empty() {
        anyhow::bail!(
            "Gemini Antigravity fallback found step outputs under {}, but none produced usable timeline entries.",
            input.brain_dir.display()
        );
    }

    apply_default_project_hint(&mut entries);
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

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
            if let Some(entry) = antigravity_json_message_to_entry(
                map,
                session_id,
                default_cwd,
                fallback_timestamp + Duration::seconds(*counter as i64),
                config,
            ) {
                entries.push(entry);
                *counter += 1;
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

fn antigravity_json_message_to_entry(
    map: &serde_json::Map<String, serde_json::Value>,
    session_id: &str,
    default_cwd: Option<&str>,
    fallback_timestamp: DateTime<Utc>,
    config: &ExtractionConfig,
) -> Option<TimelineEntry> {
    let role = antigravity_role_from_map(map)?;
    if role == "assistant" && !config.include_assistant {
        return None;
    }

    let message = ["content", "text", "message", "body", "value", "output"]
        .iter()
        .filter_map(|key| map.get(*key))
        .find_map(extract_text_from_json_value)?;
    if message.trim().is_empty() {
        return None;
    }

    let timestamp = ["timestamp", "createdAt", "created_at", "time", "date"]
        .iter()
        .filter_map(|key| map.get(*key))
        .find_map(parse_json_timestamp)
        .unwrap_or(fallback_timestamp);

    if timestamp < config.cutoff {
        return None;
    }
    if config.watermark.is_some_and(|wm| timestamp <= wm) {
        return None;
    }

    Some(TimelineEntry {
        timestamp,
        agent: "gemini-antigravity".to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        message,
        branch: None,
        cwd: infer_project_hint_from_map(map).or_else(|| default_cwd.map(ToOwned::to_owned)),
    })
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
        serde_json::Value::String(raw) => DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc)),
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
        if config.watermark.is_some_and(|wm| timestamp <= wm) {
            continue;
        }

        entries.push(TimelineEntry {
            timestamp,
            agent: "gemini-antigravity".to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            message: message.to_string(),
            branch: None,
            cwd: default_cwd.clone(),
        });
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

    TimelineEntry {
        timestamp: entries
            .iter()
            .map(|entry| entry.timestamp)
            .min()
            .unwrap_or_else(Utc::now)
            - Duration::seconds(1),
        agent: "gemini-antigravity".to_string(),
        session_id: input.conversation_id.clone(),
        role: "system".to_string(),
        message: format!(
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
        branch: None,
        cwd: None,
    }
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
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line in reader.lines() {
        let line = line?;
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
            let matches = entry.project.as_ref().is_some_and(|p| {
                let pl = p.to_lowercase();
                config
                    .project_filter
                    .iter()
                    .any(|f| pl.contains(&f.to_lowercase()))
            });
            if !matches {
                continue;
            }
        }

        // timestamp is ms epoch
        let ts_secs = entry.timestamp / 1000;
        let ts_nanos = ((entry.timestamp % 1000) * 1_000_000) as u32;
        let timestamp = match Utc.timestamp_opt(ts_secs, ts_nanos).single() {
            Some(ts) => ts,
            None => continue,
        };

        if timestamp < config.cutoff {
            continue;
        }
        if config.watermark.is_some_and(|wm| timestamp <= wm) {
            continue;
        }

        entries.push(TimelineEntry {
            timestamp,
            agent: "claude".to_string(),
            session_id: entry.session_id.unwrap_or_else(|| "history".to_string()),
            role: "user".to_string(),
            message,
            branch: None,
            cwd: entry.project,
        });
    }

    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
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
    let reader = BufReader::new(file);

    // First pass: read all entries, group by session
    let mut sessions: HashMap<String, Vec<CodexEntry>> = HashMap::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let entry: CodexEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        sessions
            .entry(entry.session_id.clone())
            .or_default()
            .push(entry);
    }

    // Second pass: determine which sessions match the filter
    let matching_sessions: HashSet<String> = if !config.project_filter.is_empty() {
        let filters_lower: Vec<String> = config
            .project_filter
            .iter()
            .map(|f| f.to_lowercase())
            .collect();
        sessions
            .iter()
            .filter(|(_id, msgs)| {
                filters_lower.iter().any(|fl| {
                    msgs.iter().any(|m| {
                        m.text.to_lowercase().contains(fl)
                            || m.cwd
                                .as_ref()
                                .is_some_and(|c| c.to_lowercase().contains(fl))
                    })
                })
            })
            .map(|(id, _)| id.clone())
            .collect()
    } else {
        sessions.keys().cloned().collect()
    };

    // Third pass: build timeline entries from matching sessions
    let mut entries: Vec<TimelineEntry> = Vec::new();

    for (session_id, msgs) in &sessions {
        if !matching_sessions.contains(session_id) {
            continue;
        }

        for msg in msgs {
            let timestamp = match Utc.timestamp_opt(msg.ts, 0).single() {
                Some(ts) => ts,
                None => continue,
            };

            // Respect cutoff
            if timestamp < config.cutoff {
                continue;
            }

            // Respect watermark
            if config.watermark.is_some_and(|wm| timestamp <= wm) {
                continue;
            }

            let role = msg.role.as_deref().unwrap_or("user").to_string();

            // Skip assistant messages if not requested
            if !config.include_assistant && role == "assistant" {
                continue;
            }

            if msg.text.is_empty() {
                continue;
            }

            entries.push(TimelineEntry {
                timestamp,
                agent: "codex".to_string(),
                session_id: session_id.clone(),
                role,
                message: msg.text.clone(),
                branch: None,
                cwd: msg.cwd.clone(),
            });
        }
    }

    // Merge codex sessions entries
    match extract_codex_sessions(config) {
        Ok(sess) => entries.extend(sess),
        Err(e) => eprintln!("Codex sessions extraction warning: {}", e),
    }

    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
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
        match parse_codex_session_file(path, config) {
            Ok(se) => entries.extend(se),
            Err(_) => continue,
        }
    }

    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(entries)
}

/// Parse a single Codex session JSONL file.
fn parse_codex_session_file(path: &Path, config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let file = sanitize::open_file_validated(path)?;
    let reader = BufReader::new(file);

    let mut events: Vec<CodexSessionEvent> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<CodexSessionEvent>(&line) {
            events.push(ev);
        }
    }

    // Extract global session metadata (like session_id) and the initial cwd
    let mut session_id: Option<String> = None;
    let mut initial_cwd: Option<String> = None;

    for ev in &events {
        if ev.event_type == "session_meta" {
            if session_id.is_none() {
                session_id = ev
                    .payload
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
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

    // Fallback session_id from filename stem
    let session_id = session_id.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    // Collect event_msg entries (user_message + agent_message)
    let mut entries = Vec::new();
    let mut current_cwd = initial_cwd;

    for ev in &events {
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
            let matches = current_cwd.as_ref().is_some_and(|cwd| {
                let cwd_lower = cwd.to_lowercase();
                config
                    .project_filter
                    .iter()
                    .any(|f| cwd_lower.contains(&f.to_lowercase()))
            });
            if !matches {
                continue;
            }
        }

        let msg_type = ev
            .payload
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let (role, message) = match msg_type {
            "user_message" => (
                "user",
                ev.payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            "agent_message" => (
                "assistant",
                ev.payload
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            "agent_reasoning" => (
                "reasoning",
                ev.payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ),
            _ => continue,
        };

        // Skip assistant/reasoning if not requested
        if !config.include_assistant && (role == "assistant" || role == "reasoning") {
            continue;
        }

        if message.is_empty() {
            continue;
        }

        let timestamp = match DateTime::parse_from_rfc3339(&ev.timestamp) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => continue,
        };

        if timestamp < config.cutoff {
            continue;
        }
        if config.watermark.is_some_and(|wm| timestamp <= wm) {
            continue;
        }

        entries.push(TimelineEntry {
            timestamp,
            agent: "codex".to_string(),
            session_id: session_id.clone(),
            role: role.to_string(),
            message,
            branch: None,
            cwd: current_cwd.clone(),
        });
    }

    Ok(entries)
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
/// Reads `~/.gemini/tmp/<projectHash>/chats/session-*.json` files.
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

            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }

            match parse_gemini_session(&path, config) {
                Ok(se) => entries.extend(se),
                Err(_) => continue,
            }
        }
    }

    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(entries)
}

/// Parse a single Gemini CLI session JSON file.
fn parse_gemini_session(
    path: &std::path::Path,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let content = sanitize::read_to_string_validated(path)?;
    let session: GeminiSession = serde_json::from_str(&content)?;

    let session_id = session
        .session_id
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_default();

    let session_default_cwd = session
        .messages
        .iter()
        .find_map(infer_project_hint_from_gemini_message);

    // Check project filter against message content
    let session_matches_filter = if !config.project_filter.is_empty() {
        let filters_lower: Vec<String> = config
            .project_filter
            .iter()
            .map(|f| f.to_lowercase())
            .collect();
        session
            .messages
            .iter()
            .any(|message| gemini_message_matches_filter(message, &filters_lower))
    } else {
        true
    };

    if !session_matches_filter {
        return Ok(vec![]);
    }

    let mut entries = Vec::new();

    for msg in &session.messages {
        let msg_type = msg.msg_type.as_deref().unwrap_or("user");

        // Skip system messages (errors, info)
        let role = match msg_type {
            "user" => "user".to_string(),
            "gemini" => "assistant".to_string(),
            _ => continue, // skip "error", "info", etc.
        };

        // Skip assistant messages if not requested
        if !config.include_assistant && role == "assistant" {
            continue;
        }

        // Parse timestamp (always RFC3339 in Gemini CLI)
        let timestamp = msg.timestamp.as_ref().and_then(|ts| {
            DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        });

        let timestamp = match timestamp {
            Some(ts) => ts,
            None => continue,
        };

        // Respect cutoff
        if timestamp < config.cutoff {
            continue;
        }

        // Respect watermark
        if config.watermark.is_some_and(|wm| timestamp <= wm) {
            continue;
        }

        let Some(text) = render_gemini_message_content(msg) else {
            continue;
        };

        let inferred_cwd =
            infer_project_hint_from_gemini_message(msg).or_else(|| session_default_cwd.clone());

        entries.push(TimelineEntry {
            timestamp,
            agent: "gemini".to_string(),
            session_id: session_id.clone(),
            role,
            message: text,
            branch: None,
            cwd: inferred_cwd.clone(),
        });

        // Extract thoughts as reasoning entries (only when include_assistant)
        if config.include_assistant && !msg.thoughts.is_empty() {
            for thought in &msg.thoughts {
                let thought_ts = thought
                    .timestamp
                    .as_ref()
                    .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or(timestamp);

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

                entries.push(TimelineEntry {
                    timestamp: thought_ts,
                    agent: "gemini".to_string(),
                    session_id: session_id.clone(),
                    role: "reasoning".to_string(),
                    message: text,
                    branch: None,
                    cwd: inferred_cwd.clone(),
                });
            }
        }
    }

    Ok(entries)
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
    all.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    // Dedup: same timestamp + same first 100 chars of message -> keep first
    let mut seen: HashSet<(i64, String)> = HashSet::new();
    all.retain(|entry| {
        let key_msg: String = entry.message.chars().take(100).collect();
        let key = (entry.timestamp.timestamp(), key_msg);
        seen.insert(key)
    });

    Ok(all)
}

// ============================================================================
// List helper
// ============================================================================

/// List available sources with session counts and sizes.
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
                sources.push(SourceInfo {
                    agent: "claude".to_string(),
                    path,
                    sessions: session_count,
                    size_bytes: total_size,
                });
            }
        }
    }

    // Claude history.jsonl
    let claude_history = home.join(".claude").join("history.jsonl");
    if claude_history.exists() {
        let size = fs::metadata(&claude_history).map(|m| m.len()).unwrap_or(0);
        sources.push(SourceInfo {
            agent: "claude-history".to_string(),
            path: claude_history,
            sessions: 1,
            size_bytes: size,
        });
    }

    // Codex
    let codex_path = home.join(".codex").join("history.jsonl");
    if codex_path.exists() {
        let size = fs::metadata(&codex_path).map(|m| m.len()).unwrap_or(0);
        let sessions = count_codex_sessions(&codex_path).unwrap_or(0);
        sources.push(SourceInfo {
            agent: "codex".to_string(),
            path: codex_path,
            sessions,
            size_bytes: size,
        });
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
            sources.push(SourceInfo {
                agent: "codex-sessions".to_string(),
                path: codex_sessions_dir,
                sessions: files.len(),
                size_bytes: total_size,
            });
        }
    }

    // Gemini CLI: ~/.gemini/tmp/<projectHash>/chats/session-*.json
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
                if fp.extension().is_some_and(|e| e == "json") {
                    session_count += 1;
                    if let Ok(meta) = fs::metadata(&fp) {
                        total_size += meta.len();
                    }
                }
            }

            if session_count > 0 {
                sources.push(SourceInfo {
                    agent: "gemini".to_string(),
                    path: project_path,
                    sessions: session_count,
                    size_bytes: total_size,
                });
            }
        }
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
            return project_filter[0].clone();
        } else if let Some(c) = cwd {
            for p in project_filter {
                if c.contains(p) {
                    return p.clone();
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
            return name.to_string_lossy().to_string();
        }
        current = p.parent();
    }

    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
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
            return name.to_string_lossy().to_string();
        }
    }

    // Fallback: cwd dirname
    if let Ok(cwd) = std::env::current_dir()
        && let Some(name) = cwd.file_name()
    {
        return name.to_string_lossy().to_string();
    }

    "unknown".to_string()
}

/// Count unique sessions in the Codex history file.
fn count_codex_sessions(path: &std::path::Path) -> Result<usize> {
    let file = sanitize::open_file_validated(path)?;
    let reader = BufReader::new(file);
    let mut sessions: HashSet<String> = HashSet::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<CodexEntry>(&line) {
            sessions.insert(entry.session_id);
        }
    }

    Ok(sessions.len())
}

// ============================================================================
// Utilities
// ============================================================================

/// Decode a Claude project path from the encoded directory name.
///
/// Claude encodes project paths by replacing `/` with `-` in directory names.
/// Leading dash (from the root `/`) is stripped.
///
/// Example: `-Users-maciejgad-hosted-VetCoders-CodeScribe`
///       -> `Users/maciejgad/hosted/VetCoders/CodeScribe`
pub fn decode_claude_project_path(encoded: &str) -> String {
    let stripped = encoded.strip_prefix('-').unwrap_or(encoded);
    stripped.replace('-', "/")
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
mod tests {
    use super::*;
    use filetime::{FileTime, set_file_mtime};
    use std::fs;
    use std::path::PathBuf;

    fn unique_test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ai-contexters-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn set_mtime(path: &Path, unix_seconds: i64) {
        set_file_mtime(path, FileTime::from_unix_time(unix_seconds, 0)).unwrap();
    }

    #[test]
    fn test_repo_name_from_cwd() {
        // Fallback behavior
        assert_eq!(
            repo_name_from_cwd(Some("/Users/polyversai/Libraxis/lbrx-services"), &[]),
            "lbrx-services"
        );
        assert_eq!(
            repo_name_from_cwd(Some("/Users/polyversai/Libraxis/mlx-batch-runner"), &[]),
            "mlx-batch-runner"
        );
        assert_eq!(repo_name_from_cwd(None, &[]), "unknown");
        assert_eq!(repo_name_from_cwd(Some("/"), &[]), "unknown");
        assert_eq!(repo_name_from_cwd(Some(""), &[]), "unknown");

        // Single project filter
        assert_eq!(
            repo_name_from_cwd(
                Some("/Users/polyversai/Libraxis/lbrx-services/subfolder"),
                &["lbrx".to_string()]
            ),
            "lbrx"
        );

        // Multiple project filters
        let filters = vec!["lbrx-services".to_string(), "foo".to_string()];
        assert_eq!(
            repo_name_from_cwd(
                Some("/Users/polyversai/Libraxis/lbrx-services/subfolder"),
                &filters
            ),
            "lbrx-services"
        );
    }

    #[test]
    fn test_decode_claude_project_path_with_leading_dash() {
        let encoded = "-Users-maciejgad-hosted-VetCoders-CodeScribe";
        let decoded = decode_claude_project_path(encoded);
        assert_eq!(decoded, "Users/maciejgad/hosted/VetCoders/CodeScribe");
    }

    #[test]
    fn test_decode_claude_project_path_without_leading_dash() {
        let encoded = "Users-maciejgad-projects-foo";
        let decoded = decode_claude_project_path(encoded);
        assert_eq!(decoded, "Users/maciejgad/projects/foo");
    }

    #[test]
    fn test_decode_claude_project_path_single_segment() {
        let encoded = "-home";
        let decoded = decode_claude_project_path(encoded);
        assert_eq!(decoded, "home");
    }

    #[test]
    fn test_decode_claude_project_path_empty() {
        let decoded = decode_claude_project_path("");
        assert_eq!(decoded, "");
    }

    #[test]
    fn test_decode_claude_project_path_deep_nesting() {
        let encoded = "-a-b-c-d-e-f";
        let decoded = decode_claude_project_path(encoded);
        assert_eq!(decoded, "a/b/c/d/e/f");
    }

    #[test]
    fn test_extract_claude_file_parses_text_only_blocks() {
        let tmp = std::env::temp_dir().join("ai-ctx-claude-direct.jsonl");
        let _ = fs::remove_file(&tmp);

        let content = r#"{"type":"user","message":{"role":"user","content":"Hello"},"timestamp":"2026-02-09T22:03:06.765Z","sessionId":"sess123","gitBranch":"main","cwd":"/tmp"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi"}]},"timestamp":"2026-02-09T22:03:07.765Z","sessionId":"sess123"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo hi"}}]},"timestamp":"2026-02-09T22:03:08.765Z","sessionId":"sess123"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]},"timestamp":"2026-02-09T22:03:09.765Z","sessionId":"sess123"}"#;
        fs::write(&tmp, content).unwrap();

        let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff,
            include_assistant: true,
            watermark: None,
        };

        let entries = extract_claude_file(&tmp, &config).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[0].message, "Hello");
        assert_eq!(entries[1].role, "assistant");
        assert_eq!(entries[1].message, "Hi");

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn test_extract_codex_file_history_format() {
        let tmp = std::env::temp_dir().join("ai-ctx-codex-direct-history.jsonl");
        let _ = fs::remove_file(&tmp);

        let content = r#"{"session_id":"s1","text":"hello","ts":1000,"role":"user","cwd":"/tmp/a"}
{"session_id":"s1","text":"hi back","ts":1001,"role":"assistant","cwd":"/tmp/a"}
{"session_id":"s2","text":"unrelated","ts":2000,"role":"user","cwd":"/tmp/b"}"#;
        fs::write(&tmp, content).unwrap();

        let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff,
            include_assistant: true,
            watermark: None,
        };

        let entries = extract_codex_file(&tmp, &config).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].agent, "codex");
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[1].role, "assistant");

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn test_extract_codex_file_session_format_detects() {
        let tmp = std::env::temp_dir().join("ai-ctx-codex-direct-session.jsonl");
        let _ = fs::remove_file(&tmp);

        // Minimal session file (no event_msg) should parse and yield 0 entries.
        let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"sess","cwd":"/tmp/x"}}"#;
        fs::write(&tmp, content).unwrap();

        let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff,
            include_assistant: true,
            watermark: None,
        };

        let entries = extract_codex_file(&tmp, &config).unwrap();
        assert!(entries.is_empty());

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn test_extract_gemini_file_session_json() {
        let tmp = std::env::temp_dir().join("ai-ctx-gemini-direct.json");
        let _ = fs::remove_file(&tmp);

        let content = r#"{
  "sessionId": "sess-1",
  "projectHash": "hash-1",
  "messages": [
    {"type":"user","content":"hi","timestamp":"2026-02-01T00:00:00Z","thoughts":[]},
    {"type":"gemini","content":"hello","timestamp":"2026-02-01T00:00:01Z","thoughts":[]},
    {"type":"info","content":"skip me","timestamp":"2026-02-01T00:00:02Z","thoughts":[]}
  ]
}"#;
        fs::write(&tmp, content).unwrap();

        let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff,
            include_assistant: true,
            watermark: None,
        };

        let entries = extract_gemini_file(&tmp, &config).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].agent, "gemini");
        assert_eq!(entries[0].role, "user");
        assert_eq!(entries[1].role, "assistant");

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn test_extract_gemini_antigravity_prefers_conversation_artifacts_for_brain_input() {
        let root = unique_test_dir("gemini-antigravity-brain");
        let brain = root.join("brain").join("conv-1");
        let conversation_artifact = brain.join("conversation.json");
        let step_output = brain
            .join(".system_generated")
            .join("steps")
            .join("001")
            .join("output.txt");

        write_file(
            &conversation_artifact,
            r#"{
  "projectRoot": "/Users/tester/workspace/RepoAlpha",
  "messages": [
    {"role":"user","content":"Map the architecture","timestamp":"2026-02-01T00:00:00Z"},
    {"role":"assistant","content":"We should split extraction and reporting.","timestamp":"2026-02-01T00:00:01Z"}
  ]
}"#,
        );
        write_file(
            &step_output,
            r#"{"project":"/Users/tester/workspace/RepoIgnored","decision":"fallback should stay unused"}"#,
        );
        set_mtime(&conversation_artifact, 1_706_745_600);
        set_mtime(&step_output, 1_706_745_660);

        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
            include_assistant: true,
            watermark: None,
        };

        let entries = extract_gemini_antigravity_file(&brain, &config).unwrap();
        assert_eq!(entries[0].role, "system");
        assert!(entries[0].message.contains("mode: conversation-artifacts"));
        assert!(entries[0].message.contains("RepoAlpha"));
        assert!(
            entries[0]
                .message
                .contains(&conversation_artifact.display().to_string())
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.message.contains("step output fallback"))
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.role == "user" || entry.role == "assistant")
                .count(),
            2
        );
        assert!(
            entries
                .iter()
                .filter(|entry| entry.role != "system")
                .all(|entry| entry.cwd.as_deref() == Some("/Users/tester/workspace/RepoAlpha"))
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_extract_gemini_antigravity_pb_input_resolves_brain_and_falls_back_to_steps() {
        let root = unique_test_dir("gemini-antigravity-pb");
        let pb = root.join("conversations").join("conv-2.pb");
        let step_output = root
            .join("brain")
            .join("conv-2")
            .join(".system_generated")
            .join("steps")
            .join("007")
            .join("output.txt");

        write_file(&pb, "opaque");
        write_file(
            &step_output,
            r#"{"project":"/Users/tester/workspace/RepoBeta","decision":"Ship the extraction in additive mode."}"#,
        );
        set_mtime(&step_output, 1_706_745_720);

        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
            include_assistant: true,
            watermark: None,
        };

        let entries = extract_gemini_antigravity_file(&pb, &config).unwrap();
        assert_eq!(entries[0].role, "system");
        assert!(entries[0].message.contains("mode: step-output-fallback"));
        assert!(
            entries[0]
                .message
                .contains("not a full conversation transcript")
        );
        assert!(entries[0].message.contains(&pb.display().to_string()));
        assert_eq!(entries[1].role, "artifact");
        assert!(entries[1].message.contains("step output fallback"));
        assert!(
            entries[1]
                .cwd
                .as_deref()
                .is_some_and(|cwd| cwd.ends_with("RepoBeta"))
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_extract_gemini_antigravity_missing_brain_errors_honestly() {
        let root = unique_test_dir("gemini-antigravity-missing-brain");
        let pb = root.join("conversations").join("conv-3.pb");
        write_file(&pb, "opaque");

        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
            include_assistant: true,
            watermark: None,
        };

        let err = extract_gemini_antigravity_file(&pb, &config).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("opaque/encrypted"));
        assert!(message.contains("brain/conv-3/"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_extract_gemini_antigravity_brain_input_falls_back_explicitly() {
        let root = unique_test_dir("gemini-antigravity-brain-fallback");
        let brain = root.join("brain").join("conv-4");
        let step_a = brain
            .join(".system_generated")
            .join("steps")
            .join("002")
            .join("output.txt");
        let step_b = brain
            .join(".system_generated")
            .join("steps")
            .join("009")
            .join("output.txt");

        write_file(
            &step_a,
            r#"{"project":"RepoGamma","decision":"Prefer readable artifacts first."}"#,
        );
        write_file(
            &step_b,
            r#"{"decision":"Degrade to step outputs when chat artifacts are absent."}"#,
        );
        set_mtime(&step_a, 1_706_745_780);
        set_mtime(&step_b, 1_706_745_840);

        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
            include_assistant: true,
            watermark: None,
        };

        let entries = extract_gemini_antigravity_file(&brain, &config).unwrap();
        assert!(entries[0].message.contains("mode: step-output-fallback"));
        assert!(entries[1].message.contains(&step_a.display().to_string()));
        assert!(entries[2].message.contains(&step_b.display().to_string()));
        assert_eq!(entries[1].cwd.as_deref(), Some("RepoGamma"));
        assert_eq!(entries[2].cwd.as_deref(), Some("RepoGamma"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_codex_session_filtering_includes_all_messages() {
        // Simulate the session-based filtering logic:
        // If any message in a session mentions the filter, all messages are included.

        let session_a_msgs = [
            ("s1", "work on CodeScribe refactoring", 1000i64),
            ("s1", "fix the bug in controller", 1001),
            ("s1", "done with changes", 1002),
        ];
        let session_b_msgs = [
            ("s2", "unrelated project work", 2000),
            ("s2", "more unrelated stuff", 2001),
        ];

        // Build sessions map
        let mut sessions: HashMap<String, Vec<(String, i64)>> = HashMap::new();
        for (sid, text, ts) in session_a_msgs.iter().chain(session_b_msgs.iter()) {
            sessions
                .entry(sid.to_string())
                .or_default()
                .push((text.to_string(), *ts));
        }

        let filter = "CodeScribe";
        let filter_lower = filter.to_lowercase();

        // Determine matching sessions
        let matching: HashSet<String> = sessions
            .iter()
            .filter(|(_id, msgs)| {
                msgs.iter()
                    .any(|(text, _)| text.to_lowercase().contains(&filter_lower))
            })
            .map(|(id, _)| id.clone())
            .collect();

        // Session s1 should match (has "CodeScribe" in first message)
        assert!(matching.contains("s1"));
        // Session s2 should NOT match
        assert!(!matching.contains("s2"));

        // All 3 messages from s1 should be included, not just the one mentioning CodeScribe
        let included_count: usize = sessions
            .iter()
            .filter(|(id, _)| matching.contains(id.as_str()))
            .map(|(_, msgs)| msgs.len())
            .sum();
        assert_eq!(included_count, 3);
    }

    #[test]
    fn test_codex_session_filtering_no_filter_includes_all() {
        let sessions: HashMap<String, Vec<(String, i64)>> = HashMap::from([
            (
                "s1".to_string(),
                vec![("msg1".to_string(), 1000), ("msg2".to_string(), 1001)],
            ),
            ("s2".to_string(), vec![("msg3".to_string(), 2000)]),
        ]);

        // No filter -> all sessions match
        let matching: HashSet<String> = sessions.keys().cloned().collect();
        assert_eq!(matching.len(), 2);
    }

    #[test]
    fn test_codex_session_filtering_cwd_match() {
        // Simulate cwd-based matching
        let session_msgs: Vec<(Option<String>, String)> = vec![
            (
                Some("/Users/maciejgad/hosted/VetCoders/CodeScribe".to_string()),
                "run tests".to_string(),
            ),
            (None, "looks good".to_string()),
        ];

        let filter = "CodeScribe";
        let filter_lower = filter.to_lowercase();

        let session_matches = session_msgs.iter().any(|(cwd, text)| {
            text.to_lowercase().contains(&filter_lower)
                || cwd
                    .as_ref()
                    .is_some_and(|c| c.to_lowercase().contains(&filter_lower))
        });

        assert!(session_matches);
    }

    #[test]
    fn test_extract_message_text_plain_string() {
        let msg = Some(serde_json::Value::String("hello world".to_string()));
        assert_eq!(extract_message_text(&msg), "hello world");
    }

    #[test]
    fn test_extract_message_text_content_blocks() {
        let msg = Some(serde_json::json!([
            {"type": "text", "text": "first"},
            {"type": "image", "url": "..."},
            {"type": "text", "text": "second"}
        ]));
        assert_eq!(extract_message_text(&msg), "first\nsecond");
    }

    #[test]
    fn test_extract_message_text_object_with_content_string() {
        let msg = Some(serde_json::json!({
            "role": "user",
            "content": "direct content"
        }));
        assert_eq!(extract_message_text(&msg), "direct content");
    }

    #[test]
    fn test_extract_message_text_object_with_content_array() {
        let msg = Some(serde_json::json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "response part 1"},
                {"type": "tool_use", "id": "abc"},
                {"type": "text", "text": "response part 2"}
            ]
        }));
        assert_eq!(
            extract_message_text(&msg),
            "response part 1\nresponse part 2"
        );
    }

    #[test]
    fn test_extract_message_text_none() {
        assert_eq!(extract_message_text(&None), "");
    }

    #[test]
    fn test_dedup_logic() {
        let entries = vec![
            TimelineEntry {
                timestamp: Utc.timestamp_opt(1000, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "s1".to_string(),
                role: "user".to_string(),
                message: "same message here".to_string(),
                branch: None,
                cwd: None,
            },
            TimelineEntry {
                timestamp: Utc.timestamp_opt(1000, 0).unwrap(),
                agent: "codex".to_string(),
                session_id: "s2".to_string(),
                role: "user".to_string(),
                message: "same message here".to_string(),
                branch: None,
                cwd: None,
            },
            TimelineEntry {
                timestamp: Utc.timestamp_opt(1001, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "s1".to_string(),
                role: "user".to_string(),
                message: "different".to_string(),
                branch: None,
                cwd: None,
            },
        ];

        let mut result = entries;
        let mut seen: HashSet<(i64, String)> = HashSet::new();
        result.retain(|entry| {
            let key_msg: String = entry.message.chars().take(100).collect();
            let key = (entry.timestamp.timestamp(), key_msg);
            seen.insert(key)
        });

        // First two have same timestamp + same message -> deduped to 1
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].agent, "claude"); // first one kept
        assert_eq!(result[1].message, "different");
    }

    #[test]
    fn test_gemini_message_with_content() {
        let msg = GeminiMessage {
            msg_type: Some("user".to_string()),
            content: Some(serde_json::Value::String("hello from gemini".to_string())),
            display_content: None,
            timestamp: Some("2026-01-20T19:50:45.683Z".to_string()),
            thoughts: vec![],
        };
        assert_eq!(
            render_gemini_message_content(&msg).as_deref(),
            Some("hello from gemini")
        );
        assert_eq!(msg.msg_type.as_deref().unwrap(), "user");
    }

    #[test]
    fn test_gemini_message_type_mapping() {
        // "gemini" type maps to "assistant" role
        let msg = GeminiMessage {
            msg_type: Some("gemini".to_string()),
            content: Some(serde_json::Value::String("response text".to_string())),
            display_content: None,
            timestamp: Some("2026-01-20T19:50:51.778Z".to_string()),
            thoughts: vec![],
        };
        let role = match msg.msg_type.as_deref().unwrap_or("user") {
            "gemini" => "assistant",
            "user" => "user",
            _ => "skip",
        };
        assert_eq!(role, "assistant");
    }

    #[test]
    fn test_gemini_message_skip_error_info() {
        // "error" and "info" types should be skipped
        for msg_type in &["error", "info"] {
            let msg = GeminiMessage {
                msg_type: Some(msg_type.to_string()),
                content: Some(serde_json::Value::String("some system message".to_string())),
                display_content: None,
                timestamp: Some("2026-01-20T19:16:15.218Z".to_string()),
                thoughts: vec![],
            };
            let role = match msg.msg_type.as_deref().unwrap_or("user") {
                "user" => Some("user"),
                "gemini" => Some("assistant"),
                _ => None, // skip
            };
            assert_eq!(role, None);
        }
    }

    #[test]
    fn test_gemini_session_deserialization() {
        // Full round-trip: JSON with unknown fields (id, model, thoughts, tokens)
        // must deserialize without errors — serde ignores unknown fields by default.
        let json = r#"{
            "sessionId": "a45ff16f-2a8c-4a45-b690-2c2aaf631b71",
            "projectHash": "fef6ad02174d592d21e7f8a6143564388027ec0c38bbb44dec26e99f9cd9140f",
            "startTime": "2026-01-20T19:50:45.683Z",
            "lastUpdated": "2026-01-20T19:54:06.680Z",
            "messages": [
                {
                    "id": "772f4448-0cda-4256-8d89-121dc68776b7",
                    "timestamp": "2026-01-20T19:50:45.683Z",
                    "type": "user",
                    "content": "siemka!"
                },
                {
                    "id": "64b73173-3b0f-4838-9121-5dfd1f1bb5e1",
                    "timestamp": "2026-01-20T19:50:51.778Z",
                    "type": "gemini",
                    "content": "Cześć Maciej.",
                    "model": "gemini-3-flash-preview",
                    "thoughts": [{"subject": "test", "description": "ignored"}],
                    "tokens": {"input": 100, "output": 25}
                }
            ]
        }"#;

        let session: GeminiSession = serde_json::from_str(json).unwrap();
        assert_eq!(
            session.session_id.as_deref(),
            Some("a45ff16f-2a8c-4a45-b690-2c2aaf631b71")
        );
        assert_eq!(
            session.project_hash.as_deref(),
            Some("fef6ad02174d592d21e7f8a6143564388027ec0c38bbb44dec26e99f9cd9140f")
        );
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[0].msg_type.as_deref(), Some("user"));
        assert_eq!(
            session.messages[0].content.as_ref(),
            Some(&serde_json::Value::String("siemka!".to_string()))
        );
        assert_eq!(session.messages[1].msg_type.as_deref(), Some("gemini"));
        assert_eq!(
            session.messages[1].content.as_ref(),
            Some(&serde_json::Value::String("Cześć Maciej.".to_string()))
        );
    }

    #[test]
    fn test_render_gemini_content_value_preserves_structured_blocks() {
        let value = serde_json::json!([
            {"text": "co to jest reachy mini? @../../../.gemini/tmp/codescribe/images/clipboard-1773858428029.png"},
            {"text": "\n--- Content from referenced files ---"},
            {"inlineData": {"mimeType": "image/png", "data": "abc123"}},
            {"text": "\n--- End of content ---"}
        ]);

        let rendered = render_gemini_content_value(&value).unwrap();
        assert!(rendered.contains("co to jest reachy mini?"));
        assert!(rendered.contains("--- Content from referenced files ---"));
        assert!(rendered.contains("[inlineData omitted: mimeType=image/png, data_chars=6]"));
        assert!(rendered.contains("--- End of content ---"));
    }

    #[test]
    fn test_render_gemini_content_value_supports_object_shapes() {
        let value = serde_json::json!({
            "content": [
                {"text": "first line"},
                {"fileData": {"mimeType": "text/plain", "fileUri": "file:///tmp/note.txt"}}
            ]
        });

        let rendered = render_gemini_content_value(&value).unwrap();
        assert!(rendered.contains("first line"));
        assert!(rendered.contains("file:///tmp/note.txt"));
        assert!(rendered.contains("mimeType=text/plain"));
    }

    #[test]
    fn test_extract_gemini_file_preserves_user_array_content() {
        let tmp = std::env::temp_dir().join("ai-ctx-gemini-array-user.json");
        let _ = fs::remove_file(&tmp);

        let content = r##"{
  "sessionId": "sess-array",
  "messages": [
    {
      "type":"user",
      "content":[
        {"text":"# Task: Gemini truth repair"},
        {"text":"- preserve user arrays honestly"}
      ],
      "timestamp":"2026-02-01T00:00:00Z"
    },
    {
      "type":"gemini",
      "content":"working on it",
      "timestamp":"2026-02-01T00:00:01Z"
    }
  ]
}"##;
        fs::write(&tmp, content).unwrap();

        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
            include_assistant: true,
            watermark: None,
        };

        let entries = extract_gemini_file(&tmp, &config).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].role, "user");
        assert_eq!(
            entries[0].message,
            "[\n  {\n    \"text\": \"# Task: Gemini truth repair\"\n  },\n  {\n    \"text\": \"- preserve user arrays honestly\"\n  }\n]"
        );
        assert_eq!(entries[1].role, "assistant");

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn test_extract_gemini_file_keeps_inline_data_as_explicit_placeholder() {
        let tmp = std::env::temp_dir().join("ai-ctx-gemini-inline-data.json");
        let _ = fs::remove_file(&tmp);

        let content = r#"{
  "sessionId": "sess-inline",
  "messages": [
    {
      "type":"user",
      "timestamp":"2026-02-01T00:00:00Z",
      "content":[
        {"text":"co to jest reachy mini? @../../../.gemini/tmp/codescribe/images/clipboard-1773858428029.png"},
        {"text":"\n--- Content from referenced files ---"},
        {"inlineData":{"mimeType":"image/png","data":"abc123"}},
        {"text":"\n--- End of content ---"}
      ],
      "displayContent":[
        {"text":"co to jest reachy mini? @../../../.gemini/tmp/codescribe/images/clipboard-1773858428029.png"}
      ]
    },
    {
      "type":"gemini",
      "timestamp":"2026-02-01T00:00:01Z",
      "content":"To jest humanoidalny robot."
    }
  ]
}"#;
        fs::write(&tmp, content).unwrap();

        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
            include_assistant: true,
            watermark: None,
        };

        let entries = extract_gemini_file(&tmp, &config).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].message.contains("co to jest reachy mini?"));
        assert!(
            entries[0]
                .message
                .contains("[inlineData omitted: mimeType=image/png, data_chars=6]")
        );
        assert!(entries[1].message.contains("humanoidalny robot"));

        let _ = fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod conversation_tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_conversation_first_excludes_reasoning() {
        let entries = vec![
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "sess1".to_string(),
                role: "user".to_string(),
                message: "Fix the auth middleware".to_string(),
                branch: Some("main".to_string()),
                cwd: Some("/home/user/myrepo".to_string()),
            },
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, 30).unwrap(),
                agent: "claude".to_string(),
                session_id: "sess1".to_string(),
                role: "assistant".to_string(),
                message: "I'll refactor the auth module to use JWT tokens.".to_string(),
                branch: Some("main".to_string()),
                cwd: Some("/home/user/myrepo".to_string()),
            },
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 1, 0).unwrap(),
                agent: "codex".to_string(),
                session_id: "sess2".to_string(),
                role: "reasoning".to_string(),
                message: "Thinking about the best approach...".to_string(),
                branch: None,
                cwd: Some("/home/user/myrepo".to_string()),
            },
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 1, 30).unwrap(),
                agent: "gemini".to_string(),
                session_id: "sess3".to_string(),
                role: "reasoning".to_string(),
                message: "**Analysis**: Checking dependencies".to_string(),
                branch: None,
                cwd: Some("/home/user/myrepo".to_string()),
            },
        ];

        let conv = to_conversation(&entries, &[]);
        assert_eq!(conv.len(), 2);
        assert_eq!(conv[0].role, "user");
        assert_eq!(conv[0].message, "Fix the auth middleware");
        assert_eq!(conv[1].role, "assistant");
        assert_eq!(
            conv[1].message,
            "I'll refactor the auth module to use JWT tokens."
        );
        assert!(conv.iter().all(|m| m.role != "reasoning"));
    }

    #[test]
    fn test_conversation_first_preserves_full_messages() {
        let long_msg = "A".repeat(50_000);
        let entries = vec![TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "sess1".to_string(),
            role: "user".to_string(),
            message: long_msg.clone(),
            branch: None,
            cwd: None,
        }];

        let conv = to_conversation(&entries, &[]);
        assert_eq!(conv.len(), 1);
        assert_eq!(conv[0].message.len(), 50_000);
        assert_eq!(conv[0].message, long_msg);
    }

    #[test]
    fn test_conversation_first_repo_project_identity() {
        let entries = vec![
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "sess1".to_string(),
                role: "user".to_string(),
                message: "hello".to_string(),
                branch: None,
                cwd: Some("/Users/maciejgad/hosted/VetCoders/ai-contexters".to_string()),
            },
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 1, 0).unwrap(),
                agent: "codex".to_string(),
                session_id: "sess2".to_string(),
                role: "assistant".to_string(),
                message: "world".to_string(),
                branch: None,
                cwd: None,
            },
        ];

        let conv = to_conversation(&entries, &["ai-contexters".to_string()]);
        assert_eq!(conv[0].repo_project, "ai-contexters");
        assert_eq!(conv[1].repo_project, "ai-contexters");
        assert_eq!(
            conv[0].source_path.as_deref(),
            Some("/Users/maciejgad/hosted/VetCoders/ai-contexters")
        );
        assert!(conv[1].source_path.is_none());
    }

    #[test]
    fn test_conversation_first_preserves_provenance() {
        let entries = vec![TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 14, 30, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "abc12345-6789-session-uuid".to_string(),
            role: "user".to_string(),
            message: "Deploy to production".to_string(),
            branch: Some("release/v2".to_string()),
            cwd: Some("/home/user/project".to_string()),
        }];

        let conv = to_conversation(&entries, &[]);
        assert_eq!(conv.len(), 1);
        let msg = &conv[0];
        assert_eq!(msg.session_id, "abc12345-6789-session-uuid");
        assert_eq!(msg.agent, "claude");
        assert_eq!(msg.branch.as_deref(), Some("release/v2"));
        assert_eq!(
            msg.timestamp,
            Utc.with_ymd_and_hms(2026, 3, 21, 14, 30, 0).unwrap()
        );
    }

    #[test]
    fn test_extract_claude_excludes_tool_blocks_then_conversation_clean() {
        use std::fs;
        let tmp = std::env::temp_dir().join("ai-ctx-conv-tool-blocks.jsonl");
        let _ = fs::remove_file(&tmp);

        let content = concat!(
            r#"{"type":"user","message":{"role":"user","content":"Hello agent"},"timestamp":"2026-03-21T10:00:00.000Z","sessionId":"s1","cwd":"/tmp"}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}},{"type":"text","text":"Here are the files."}]},"timestamp":"2026-03-21T10:00:01.000Z","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]},"timestamp":"2026-03-21T10:00:02.000Z","sessionId":"s1"}"#
        );
        fs::write(&tmp, content).unwrap();

        let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
        let config = ExtractionConfig {
            project_filter: vec![],
            cutoff,
            include_assistant: true,
            watermark: None,
        };

        let entries = extract_claude_file(&tmp, &config).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].message, "Hello agent");
        assert_eq!(entries[1].message, "Let me check.\nHere are the files.");
        assert!(!entries[1].message.contains("tool_use"));
        assert!(!entries[1].message.contains("Bash"));

        let conv = to_conversation(&entries, &[]);
        assert_eq!(conv.len(), 2);
        assert_eq!(conv[0].message, "Hello agent");
        assert_eq!(conv[1].message, "Let me check.\nHere are the files.");

        let _ = fs::remove_file(&tmp);
    }
}
