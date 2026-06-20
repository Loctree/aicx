#![allow(unused_imports)]
use crate::sources::*;
use chrono::{Duration, TimeZone};
use serde::Deserialize;
use std::collections::HashMap;

use crate::timeline::FrameKind;

/// Gemini CLI session file (`~/.gemini/tmp/<project>/chats/session-*.json[l]`).
///
/// Gemini has used both a single JSON object with a `messages` array and a
/// JSONL stream where the first line carries session metadata and later lines
/// carry messages / state updates.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiSession {
    #[serde(default)]
    pub(crate) session_id: Option<String>,
    #[serde(default)]
    pub(crate) messages: Vec<GeminiMessage>,
}

/// Gemini CLI message within a session.
///
/// The `type` field uses: "user", "gemini", "error", "info".
/// Unknown fields (thoughts, tokens, model, toolCalls, id) are silently ignored.
#[derive(Debug, Deserialize)]
pub(crate) struct GeminiMessage {
    #[serde(default, rename = "type")]
    pub(crate) msg_type: Option<String>,
    #[serde(default)]
    pub(crate) role: Option<String>,
    #[serde(default)]
    pub(crate) content: Option<serde_json::Value>,
    #[serde(default, rename = "displayContent")]
    pub(crate) display_content: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) timestamp: Option<String>,
    /// Agent reasoning/thinking steps.
    #[serde(default)]
    pub(crate) thoughts: Vec<GeminiThought>,
}

/// A single thought/reasoning step from Gemini.
#[derive(Debug, Deserialize)]
pub(crate) struct GeminiThought {
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

pub(crate) fn render_gemini_message_content(message: &GeminiMessage) -> Option<String> {
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

pub(crate) fn render_gemini_content_value(value: &serde_json::Value) -> Option<String> {
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

    if let Some(home) = crate::os_user_home() {
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
            used_paths.push(path.to_path_buf());
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
        return crate::os_user_home()
            .map(|home| {
                home.join(trimmed.trim_start_matches("~/"))
                    .display()
                    .to_string()
            })
            .or_else(|| Some(trimmed.to_string()));
    }

    Some(trimmed.to_string())
}

pub fn extract_gemini(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let home = crate::os_user_home().context("No home dir")?;
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

pub(crate) fn parse_gemini_session_with_diagnostics(
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

pub(crate) fn is_gemini_session_file(path: &Path) -> bool {
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

pub(crate) fn parse_gemini_jsonl_session(
    content: &str,
) -> Result<(GeminiSession, Vec<GeminiSessionWarning>)> {
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
