#![allow(unused_imports)]
use crate::sources::*;
use chrono::TimeZone;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use crate::timeline::FrameKind;

/// Codex history JSONL entry structure.
#[derive(Debug, Deserialize)]
pub(crate) struct CodexEntry {
    session_id: String,
    #[serde(default)]
    text: String,
    ts: i64,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// Extract timeline entries from a single Codex JSONL file by path.
///
/// Supports both:
/// - Codex history format (`~/.codex/history.jsonl`) — `CodexEntry` per line.
/// - Codex session format (`~/.codex/sessions/**/**/*.jsonl`) — `CodexSessionEvent` per line.
pub fn extract_codex_file(path: &Path, config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let (entries, warnings) = parse_codex_file_with_diagnostics(path, "codex", config)?;
    emit_codex_session_warnings(path, &warnings);
    Ok(entries)
}

pub(crate) fn parse_codex_file_with_diagnostics(
    path: &Path,
    agent_label: &str,
    config: &ExtractionConfig,
) -> Result<(Vec<TimelineEntry>, Vec<CodexSessionWarning>)> {
    let file = sanitize::open_file_validated(path)?;
    let (source_path, source_sha256) = source_path_and_sha256(path);
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

        let mut entries = build_codex_history_entries(
            &history_records,
            config,
            &mut warnings,
            &source_path,
            source_sha256.as_deref(),
        );
        if !session_events.is_empty() {
            let (mut session_entries, session_warnings) =
                parse_codex_session_events_with_diagnostics(
                    path,
                    agent_label,
                    &session_events,
                    config,
                );
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

pub fn extract_codex(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let codex_path = crate::os_user_home()
        .context("No home dir")?
        .join(".codex")
        .join("history.jsonl");

    if !codex_path.exists() {
        return Ok(vec![]);
    }

    let file = sanitize::open_file_validated(&codex_path)?;
    let (source_path, source_sha256) = source_path_and_sha256(&codex_path);
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
    let mut entries = build_codex_history_entries(
        &records,
        config,
        &mut warnings,
        &source_path,
        source_sha256.as_deref(),
    );
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

fn codex_response_item_message(payload: &serde_json::Value) -> String {
    payload
        .get("content")
        .map(codex_content_message)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| codex_payload_message(payload))
}

fn codex_content_message(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(codex_content_part_text)
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(_) => {
            codex_content_part_text(value).unwrap_or_else(|| render_json_inline(value))
        }
        _ => render_json_inline(value),
    }
}

fn codex_content_part_text(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }

    let obj = value.as_object()?;
    for key in ["text", "content", "message"] {
        if let Some(text) = obj.get(key).and_then(|v| v.as_str())
            && !text.trim().is_empty()
        {
            return Some(text.to_string());
        }
    }

    None
}

fn codex_timestamp_from_epoch_seconds(raw: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(raw, 0).single()
}

fn build_codex_history_entries(
    records: &[CodexEntry],
    config: &ExtractionConfig,
    warnings: &mut Vec<CodexSessionWarning>,
    source_path: &str,
    source_sha256: Option<&str>,
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
                    source_path: Some(source_path.to_string()),
                    source_sha256: source_sha256.map(str::to_string),
                    source_line_span: None,
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
    let sessions_dir = crate::os_user_home()
        .context("No home dir")?
        .join(".codex")
        .join("sessions");
    extract_codex_like_sessions_at(&sessions_dir, "codex", config)
}

/// Internal: walk a sessions root (date-nested or Grok-style <project>/<uuid> nested) for *.jsonl,
/// parse using the v1/responses session event format, and label everything with the given agent_label.
/// This is the reusable "clone" implementation.
pub(crate) fn extract_codex_like_sessions_at(
    sessions_root: &Path,
    agent_label: &str,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    if !sessions_root.exists() || !sessions_root.is_dir() {
        return Ok(vec![]);
    }

    let mut entries = Vec::new();
    let files = walk_jsonl_files(sessions_root);

    for path in &files {
        match parse_codex_session_file_with_diagnostics(path, agent_label, config) {
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

pub(crate) fn parse_codex_session_file_with_diagnostics(
    path: &Path,
    agent_label: &str,
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
        parse_codex_session_events_with_diagnostics(path, agent_label, &events, config);
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
    agent_label: &str,
    events: &[CodexSessionEvent],
    config: &ExtractionConfig,
) -> (Vec<TimelineEntry>, Vec<CodexSessionWarning>) {
    let (source_path, source_sha256) = source_path_and_sha256(path);
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

        if ev.event_type != "event_msg" && ev.event_type != "response_item" {
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

        let (role, message, frame_kind) = if ev.event_type == "response_item" {
            let item_type = ev
                .payload
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if item_type != "message" {
                continue;
            }

            let raw_role = ev
                .payload
                .get("role")
                .and_then(|v| v.as_str())
                .unwrap_or("system");
            let frame_kind = frame_kind_from_role(raw_role);
            let role = match frame_kind {
                Some(kind) => role_for_frame_kind(kind).to_string(),
                None => raw_role.to_string(),
            };
            (role, codex_response_item_message(&ev.payload), frame_kind)
        } else {
            let msg_type = ev
                .payload
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match msg_type {
                "user_message" => (
                    "user".to_string(),
                    ev.payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    Some(FrameKind::UserMsg),
                ),
                "agent_message" => (
                    "assistant".to_string(),
                    ev.payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    Some(FrameKind::AgentReply),
                ),
                "agent_reasoning" | "thinking" | "thinking_delta" => (
                    "reasoning".to_string(),
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
                    "tool".to_string(),
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
                    "system".to_string(),
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
                        role_for_frame_kind(frame_kind).to_string(),
                        codex_payload_message(&ev.payload),
                        Some(frame_kind),
                    )
                }
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
            agent_label,
            &session_id,
            &role,
            message,
            TimelineEntryMeta {
                branch: None,
                cwd: current_cwd.clone(),
                frame_kind,
                timestamp_source: None,
                source_path: Some(source_path.clone()),
                source_sha256: source_sha256.clone(),
                source_line_span: None,
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

/// Count unique sessions in the Codex history file.
pub(crate) fn count_codex_sessions(path: &std::path::Path) -> Result<usize> {
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
// Grok support (Codex v1/responses format 1:1 under ~/.grok/projects and ~/.grok/sessions)
// ============================================================================

/// Extract from a single Grok transcript file (e.g. chat_history.jsonl or events.jsonl
/// inside a ~/.grok/sessions/<project>/<session-uuid>/ dir).
///
/// The line format for conversation turns is 1:1 with Codex rollout JSONL (CodexSessionEvent).
/// We delegate to the codex parser; warnings/diagnostics are currently tagged "codex".
pub fn extract_grok_file(path: &Path, config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name == "chat_history.jsonl" {
        // Grok stores its main conversation transcript in this file (different envelope than pure codex rollouts).
        let (entries, warnings) = parse_grok_chat_history(path, config)?;
        emit_codex_session_warnings(path, &warnings);
        return Ok(entries);
    }
    // Delegate everything else (including any v1/responses rollout jsonl) to the codex parser.
    // The caller is responsible for passing a grok-flavored path.
    extract_codex_file(path, config)
}

/// Walk `~/.grok/sessions/` (and nested project-slug / uuid dirs) for *.jsonl and
/// parse using the Codex v1/responses session event parser.
pub fn extract_grok_sessions(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let sessions_root = crate::os_user_home()
        .context("No home dir")?
        .join(".grok")
        .join("sessions");

    // Use the shared "codex format" walker (now parameterized) for any pure v1 jsonl files.
    let mut entries = extract_codex_like_sessions_at(&sessions_root, "grok", config)?;

    // Additionally walk for Grok-specific chat_history.jsonl (the main readable transcript log).
    // These use a different top-level shape than pure Codex rollout events, so we need a mapper.
    // We still reuse the same sessions_root so only one place "points" at the raw location.
    let chat_files = walk_jsonl_files(&sessions_root)
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("chat_history.jsonl"))
        .collect::<Vec<_>>();

    for p in &chat_files {
        if let Ok((mut chat_entries, warnings)) = parse_grok_chat_history(p, config) {
            emit_codex_session_warnings(p, &warnings);
            if let Some(dir_id) = grok_session_id_from_path(p) {
                for e in &mut chat_entries {
                    if e.session_id.is_empty() || e.session_id == "chat_history" {
                        e.session_id = dir_id.clone();
                    }
                }
            }
            entries.extend(chat_entries);
        }
    }

    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

/// Extract Grok transcripts.
///
/// Prefers the session/rollout layout under ~/.grok/sessions (v1/responses).
/// If a top-level ~/.grok/history.jsonl in legacy CodexEntry format exists, it is also included.
pub fn extract_grok(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let grok_root = crate::os_user_home().context("No home dir")?.join(".grok");

    let mut entries = Vec::new();

    // Optional legacy-style history.jsonl at ~/.grok/history.jsonl (CodexEntry lines)
    let history_path = grok_root.join("history.jsonl");
    if history_path.exists()
        && let Ok(file) = sanitize::open_file_validated(&history_path)
    {
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
            if let Ok(entry) = serde_json::from_str::<CodexEntry>(&line) {
                records.push(entry);
            }
        }
        if oversized_count > 0 {
            warnings.push(CodexSessionWarning::OversizedLine {
                count: oversized_count,
                samples: oversized_samples,
            });
        }
        let (source_path, source_sha256) = source_path_and_sha256(&history_path);
        let mut hist = build_codex_history_entries(
            &records,
            config,
            &mut warnings,
            &source_path,
            source_sha256.as_deref(),
        );
        emit_codex_session_warnings(&history_path, &warnings);
        entries.append(&mut hist);
    }

    // Main source: session rollouts under ~/.grok/sessions (and projects layout may surface more via the walk)
    match extract_grok_sessions(config) {
        Ok(sess) => entries.extend(sess),
        Err(e) => eprintln!("Grok sessions extraction warning: {}", e),
    }

    entries.sort_by_key(|a| a.timestamp);
    Ok(entries)
}

/// Try to derive a session id from a Grok layout path.
/// Grok stores per-session data under ~/.grok/sessions/<encoded-project>/<uuid>/...
/// The <uuid> dir name (e.g. 019ecde7-...) is the canonical session identifier.
fn grok_session_id_from_path(path: &Path) -> Option<String> {
    // Walk up a few parents looking for a dir that looks like a Grok session id (the one passed to --session).
    let mut cur = path.parent();
    for _ in 0..4 {
        if let Some(dir) = cur {
            if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
                // Common Grok/this-harness ids start with 01 or are long hex/ULID-like with dashes.
                if name.len() >= 20
                    && (name.starts_with("01")
                        || name.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
                {
                    return Some(name.to_string());
                }
            }
            cur = dir.parent();
        } else {
            break;
        }
    }
    None
}

/// Minimal parser for Grok's chat_history.jsonl (the primary human-visible transcript).
/// Lines are not CodexSessionEvent shaped, but carry "type": "user" | "assistant" | "reasoning" | "tool_result" etc.
/// We force the session_id from the parent directory name when present.
/// Returns entries + content sanitization warnings so caller can emit them (no silencing).
fn parse_grok_chat_history(
    path: &Path,
    config: &ExtractionConfig,
) -> Result<(Vec<TimelineEntry>, Vec<CodexSessionWarning>)> {
    let file = sanitize::open_file_validated(path)?;
    let (source_path, source_sha256) = source_path_and_sha256(path);
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    let mut warnings: Vec<CodexSessionWarning> = Vec::new();
    let mut line_no = 0usize;

    let forced_session_id =
        grok_session_id_from_path(path).unwrap_or_else(|| file_stem_string(path));

    // Base timestamp: mtime of the file, or now.
    let base_ts = fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|st| {
            use std::time::SystemTime;
            let dur = st.duration_since(SystemTime::UNIX_EPOCH).ok()?;
            Some(
                chrono::Utc
                    .timestamp_opt(dur.as_secs() as i64, dur.subsec_nanos())
                    .single()
                    .unwrap_or_else(Utc::now),
            )
        })
        .unwrap_or_else(Utc::now);

    for line_res in reader.lines() {
        line_no += 1;
        let line = match line_res {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");

        let (role, message, frame_kind) = match typ {
            "user" => {
                let text = v
                    .get("content")
                    .and_then(|c| c.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|part| part.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                ("user".to_string(), text, Some(FrameKind::UserMsg))
            }
            "assistant" => {
                let mut text = v
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                if text.is_empty()
                    && let Some(calls) = v.get("tool_calls").and_then(|c| c.as_array())
                {
                    let names: Vec<String> = calls
                        .iter()
                        .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                        .map(|s| s.to_string())
                        .collect();
                    if !names.is_empty() {
                        text = format!("[tool calls: {}]", names.join(", "));
                    }
                }
                ("assistant".to_string(), text, Some(FrameKind::AgentReply))
            }
            "reasoning" => {
                let text = v
                    .get("summary")
                    .and_then(|s| s.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|p| p.get("text").and_then(|t| t.as_str()))
                    .unwrap_or("")
                    .to_string();
                (
                    "reasoning".to_string(),
                    text,
                    Some(FrameKind::InternalThought),
                )
            }
            "tool_result" => {
                let text = v
                    .get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or("")
                    .to_string();
                ("tool".to_string(), text, Some(FrameKind::ToolCall))
            }
            "text" | "summary_text" => {
                let text = v
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                ("assistant".to_string(), text, Some(FrameKind::AgentReply))
            }
            _ => continue,
        };

        if !should_keep_entry(frame_kind, config) {
            continue;
        }
        if message.trim().is_empty() {
            continue;
        }

        // Assign slightly offset timestamps to preserve order within the file.
        let ts = base_ts + chrono::Duration::milliseconds(line_no as i64);

        if ts < config.cutoff {
            continue;
        }

        entries.push(build_timeline_entry_with_content_warnings(
            ts,
            "grok",
            &forced_session_id,
            &role,
            message,
            TimelineEntryMeta {
                branch: None,
                cwd: None,
                frame_kind,
                timestamp_source: None,
                source_path: Some(source_path.clone()),
                source_sha256: source_sha256.clone(),
                source_line_span: Some((line_no as u64, line_no as u64)),
            },
            &mut warnings,
        ));
    }

    Ok((entries, warnings))
}
