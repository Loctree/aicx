//! Sources / Extractors module for AI Contexters
//!
//! Standalone extraction logic for Claude Code, Codex, and Gemini Code Assist.
//! Improvements over the inline main.rs approach:
//! - Session-based Codex filtering (not per-message)
//! - Watermark support for incremental extraction
//! - Optional assistant message inclusion
//! - Gemini Code Assist support
//! - Proper deduplication
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
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
    content: Option<String>,
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

        // Filter by project if specified
        if !config.project_filter.is_empty() {
            let decoded = decode_claude_project_path(&dir_name);
            let decoded_lower = decoded.to_lowercase();
            let dir_lower = dir_name.to_lowercase();
            let matches = config.project_filter.iter().any(|f| {
                let fl = f.to_lowercase();
                decoded_lower.contains(&fl) || dir_lower.contains(&fl)
            });
            if !matches {
                continue;
            }
        }

        let project_dir = dir_entry.path();
        if !project_dir.is_dir() {
            continue;
        }

        for file_entry in fs::read_dir(&project_dir)? {
            let file_entry = file_entry?;
            let path = file_entry.path();

            if path.extension().is_some_and(|e| e == "jsonl") {
                let session_id = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();

                let session_entries = parse_claude_jsonl(&path, &session_id, config)?;
                entries.extend(session_entries);
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
    let validated = sanitize::validate_read_path(path)?;
    // SECURITY: path sanitized via validate_read_path (traversal + canonicalize + allowlist)
    let file = File::open(&validated)?; // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
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

    let file = File::open(&history_path)?;
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

    let file = File::open(&codex_path)?;
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
fn parse_codex_session_file(
    path: &Path,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let validated = sanitize::validate_read_path(path)?;
    // SECURITY: path sanitized via validate_read_path (traversal + canonicalize + allowlist)
    let file = File::open(&validated)?; // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
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

    // Extract session metadata
    let mut session_cwd: Option<String> = None;
    let mut session_id: Option<String> = None;

    for ev in &events {
        if ev.event_type == "session_meta" {
            if session_cwd.is_none() {
                session_cwd = ev.payload.get("cwd").and_then(|v| v.as_str()).map(String::from);
            }
            if session_id.is_none() {
                session_id = ev.payload.get("id").and_then(|v| v.as_str()).map(String::from);
            }
        }
        if ev.event_type == "turn_context" && session_cwd.is_none() {
            session_cwd = ev.payload.get("cwd").and_then(|v| v.as_str()).map(String::from);
        }
    }

    // Fallback session_id from filename stem
    let session_id = session_id.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    });

    // Project filter: check if session cwd matches
    if !config.project_filter.is_empty() {
        let matches = session_cwd.as_ref().is_some_and(|cwd| {
            let cwd_lower = cwd.to_lowercase();
            config
                .project_filter
                .iter()
                .any(|f| cwd_lower.contains(&f.to_lowercase()))
        });
        if !matches {
            return Ok(vec![]);
        }
    }

    // Collect event_msg entries (user_message + agent_message)
    let mut entries = Vec::new();

    for ev in &events {
        if ev.event_type != "event_msg" {
            continue;
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
            cwd: session_cwd.clone(),
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
    let content = fs::read_to_string(path)?;
    let session: GeminiSession = serde_json::from_str(&content)?;

    let session_id = session
        .session_id
        .or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()))
        .unwrap_or_default();

    // Use projectHash as a pseudo-cwd for filtering
    let project_hash = session.project_hash.clone();

    // Check project filter against message content
    let session_matches_filter = if !config.project_filter.is_empty() {
        let filters_lower: Vec<String> = config
            .project_filter
            .iter()
            .map(|f| f.to_lowercase())
            .collect();
        filters_lower.iter().any(|fl| {
            session.messages.iter().any(|m| {
                m.content
                    .as_ref()
                    .is_some_and(|c| c.to_lowercase().contains(fl))
            })
        })
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

        let text = msg.content.as_deref().unwrap_or("").to_string();
        if text.is_empty() {
            continue;
        }

        entries.push(TimelineEntry {
            timestamp,
            agent: "gemini".to_string(),
            session_id: session_id.clone(),
            role,
            message: text,
            branch: None,
            cwd: project_hash.clone(),
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
                    cwd: project_hash.clone(),
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

/// Extract repo name from a `cwd` path (last path component).
///
/// Example: `"/Users/polyversai/Libraxis/lbrx-services"` → `"lbrx-services"`
pub fn repo_name_from_cwd(cwd: Option<&str>) -> String {
    cwd.and_then(|c| std::path::Path::new(c).file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
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
    let validated = sanitize::validate_read_path(path)?;
    // SECURITY: path sanitized via validate_read_path (traversal + canonicalize + allowlist)
    let file = File::open(&validated)?; // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
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

    #[test]
    fn test_repo_name_from_cwd() {
        assert_eq!(
            repo_name_from_cwd(Some("/Users/polyversai/Libraxis/lbrx-services")),
            "lbrx-services"
        );
        assert_eq!(
            repo_name_from_cwd(Some("/Users/polyversai/Libraxis/mlx-batch-runner")),
            "mlx-batch-runner"
        );
        assert_eq!(repo_name_from_cwd(None), "unknown");
        assert_eq!(repo_name_from_cwd(Some("/")), "unknown");
        assert_eq!(repo_name_from_cwd(Some("")), "unknown");
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
            content: Some("hello from gemini".to_string()),
            timestamp: Some("2026-01-20T19:50:45.683Z".to_string()),
            thoughts: vec![],
        };
        assert_eq!(msg.content.as_deref().unwrap_or(""), "hello from gemini");
        assert_eq!(msg.msg_type.as_deref().unwrap(), "user");
    }

    #[test]
    fn test_gemini_message_type_mapping() {
        // "gemini" type maps to "assistant" role
        let msg = GeminiMessage {
            msg_type: Some("gemini".to_string()),
            content: Some("response text".to_string()),
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
                content: Some("some system message".to_string()),
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
        assert_eq!(session.messages[0].content.as_deref(), Some("siemka!"));
        assert_eq!(session.messages[1].msg_type.as_deref(), Some("gemini"));
        assert_eq!(
            session.messages[1].content.as_deref(),
            Some("Cześć Maciej.")
        );
    }
}
