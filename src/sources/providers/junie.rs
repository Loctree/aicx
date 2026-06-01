#![allow(unused_imports)]
use crate::sources::*;
use chrono::{Duration, NaiveDate};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::BufReader;

use crate::timeline::FrameKind;

const JUNIE_SESSION_DIR_PREFIX: &str = "session-";
const JUNIE_REQUEST_ID_PREFIX: &str = "prompt-";
pub(crate) const JUNIE_EVENTS_FILENAME: &str = "events.jsonl";

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
    let sessions_dir = resolve_source_home()?.join(".junie").join("sessions");

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

pub(crate) fn junie_session_id_from_path_with_warning(
    path: &Path,
) -> (String, Option<JunieSessionWarning>) {
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
