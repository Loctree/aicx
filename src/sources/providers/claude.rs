#![allow(unused_imports)]
use crate::sources::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::BufReader;

use crate::timeline::FrameKind;

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
    /// Harness-injected/meta marker. When `true`, the row is synthetic context
    /// (hook output, system reminder, injected card) rather than real
    /// conversation, so every block it produces is reclassified to
    /// `FrameKind::SystemNote`.
    #[serde(rename = "isMeta", default)]
    is_meta: Option<bool>,
}

#[derive(Debug)]
struct ClaudeRawEntry {
    entry: ClaudeEntry,
    line_number: usize,
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
    // A harness-injected/meta row is synthetic context, not real conversation:
    // collapse every block it produces to SystemNote so the conversation
    // projection and the user-only/intent lane (both keyed on frame_kind) drop
    // it structurally, regardless of its surface role.
    let force_system = entry.is_meta == Some(true);
    let fallback_role = if entry.entry_type == "tool_use" || entry.entry_type == "tool_result" {
        role_for_frame_kind(FrameKind::ToolCall)
    } else {
        entry.entry_type.as_str()
    };

    let classified = extract_claude_classified_blocks(&entry.message, fallback_role);
    if !classified.is_empty() {
        for block in classified {
            let frame_kind = if force_system {
                FrameKind::SystemNote
            } else {
                block.frame_kind
            };
            let role = if force_system {
                role_for_frame_kind(FrameKind::SystemNote)
            } else {
                block.role.as_str()
            };
            if !should_keep_entry(Some(frame_kind), config) {
                continue;
            }
            entries.push(build_timeline_entry_with_content_warnings(
                timestamp,
                "claude",
                session_id,
                role,
                block.message,
                TimelineEntryMeta {
                    branch: entry.git_branch.clone(),
                    cwd: entry.cwd.clone(),
                    frame_kind: Some(frame_kind),
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

    let frame_kind = if force_system {
        Some(FrameKind::SystemNote)
    } else {
        frame_kind_from_claude_type(&entry.entry_type)
            .or_else(|| frame_kind_from_role(fallback_role))
    };
    if !should_keep_entry(frame_kind, config) {
        return Vec::new();
    }
    let role = if force_system {
        role_for_frame_kind(FrameKind::SystemNote)
    } else {
        fallback_role
    };

    entries.push(build_timeline_entry_with_content_warnings(
        timestamp,
        "claude",
        session_id,
        role,
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

/// Extract timeline entries from Claude Code session files.
///
/// Reads `~/.claude/projects/<project_dir>/<uuid>.jsonl` files.
/// Uses filename stem (UUID) as session_id for consistency.
pub fn extract_claude(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let claude_dir = crate::os_user_home()
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

pub(crate) fn parse_claude_jsonl_with_diagnostics(
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
    let history_path = crate::os_user_home()
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
