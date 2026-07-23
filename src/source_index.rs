//! Source-driven lexical index.
//!
//! The durable corpus is the session catalog plus live source files. This
//! module parses each cataloged source once, keeps only user/assistant signal,
//! writes at most one readable extract per session, and publishes Tantivy
//! directly. It never reads or writes per-frame store cards or embedding
//! NDJSON intermediates.

use std::collections::BTreeMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::SecondsFormat;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::catalog::CatalogEntry;
use crate::timeline::{FrameKind, TimelineEntry};

const MAX_MESSAGE_CHARS: usize = 256 * 1024;
const MAX_EXTRACT_CHARS: usize = 4 * 1024 * 1024;
const MAX_UNBROKEN_TOKEN_CHARS: usize = 4096;
const MAX_FULL_PARSE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSONL_RECORD_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct SourceIndexReport {
    pub catalog_path: String,
    pub sources_total: usize,
    pub sources_parsed: usize,
    pub sources_skipped: usize,
    pub raw_frames: usize,
    pub signal_frames: usize,
    pub filtered_frames: usize,
    pub extracts_written: usize,
    pub lexical_docs: usize,
    pub unchanged: bool,
    pub wall_ms: u64,
    pub manifest_path: Option<String>,
    pub skipped_by_agent: BTreeMap<String, usize>,
}

/// Build or preview the global lexical index from the durable catalog.
///
/// Incremental truth is bounded by the durable catalog snapshot. When that
/// snapshot is unchanged, CURRENT is reused without parsing session bodies.
/// A catalog rebuild explicitly admits new or changed live-source state.
pub fn build(
    aicx_home: &Path,
    project_filters: &[String],
    dry_run: bool,
    full_rescan: bool,
    cache_extracts: bool,
) -> Result<SourceIndexReport> {
    let started = Instant::now();
    if !dry_run && !project_filters.is_empty() {
        anyhow::bail!(
            "project-scoped index publishing is retired; run `aicx index` once for the global \
             catalog, then filter queries with `aicx search -p <project>` (use `aicx index -p \
             <project> --dry-run` only to inspect a project slice)"
        );
    }
    let catalog_path = crate::catalog::sessions_path_for(aicx_home);
    let entries = crate::catalog::read_entries_at(aicx_home)?;
    if entries.is_empty() {
        anyhow::bail!(
            "session catalog is empty at {}; run `aicx catalog rebuild` first",
            catalog_path.display()
        );
    }

    let selected: Vec<CatalogEntry> = entries
        .into_iter()
        .filter(|entry| project_selected(entry.project.as_deref(), project_filters))
        .collect();
    let source_fingerprint = source_fingerprint(&catalog_path, &selected)?;
    // Incremental short-circuit applies to both publish and dry-run. A matching
    // catalog fingerprint means CURRENT already reflects this snapshot — re-parsing
    // ~10k sources on every `index --dry-run` recreated the mill latency the
    // extracts-store cut was meant to kill. Use `--full-rescan` to force a walk.
    if !full_rescan
        && project_filters.is_empty()
        && crate::vector_index::source_lexical_generation_matches(&source_fingerprint)?
    {
        return Ok(SourceIndexReport {
            catalog_path: catalog_path.display().to_string(),
            sources_total: selected.len(),
            sources_parsed: 0,
            sources_skipped: 0,
            raw_frames: 0,
            signal_frames: 0,
            filtered_frames: 0,
            extracts_written: 0,
            lexical_docs: crate::vector_index::current_lexical_doc_count()?.unwrap_or(0),
            unchanged: true,
            wall_ms: started.elapsed().as_millis() as u64,
            manifest_path: crate::vector_index::hybrid_manifest_path(None)
                .ok()
                .map(|path| path.display().to_string()),
            skipped_by_agent: BTreeMap::new(),
        });
    }

    let mut chunks = Vec::with_capacity(selected.len());
    let mut sources_parsed = 0usize;
    let mut sources_skipped = 0usize;
    let mut raw_frames = 0usize;
    let mut signal_frames = 0usize;
    let mut filtered_frames = 0usize;
    let mut extracts_written = 0usize;
    let mut skipped_by_agent = BTreeMap::new();

    for entry in &selected {
        let source_path = Path::new(&entry.source_path);
        let mut frames = match parse_catalog_source(entry, source_path) {
            Ok(frames) => frames,
            Err(error) => {
                crate::diagnostics::log_describe(&format!(
                    "source_index_skip agent={} session_id={} path={} error={error:#}",
                    entry.agent,
                    entry.session_id,
                    source_path.display()
                ));
                sources_skipped += 1;
                *skipped_by_agent.entry(entry.agent.clone()).or_default() += 1;
                continue;
            }
        };
        sources_parsed += 1;
        raw_frames += frames.len();
        frames.sort_by_key(|frame| frame.timestamp);
        let before = frames.len();
        frames.retain(is_signal_frame);
        for frame in &mut frames {
            frame.message = clean_message(&frame.message);
        }
        frames.retain(|frame| !frame.message.trim().is_empty());
        signal_frames += frames.len();
        filtered_frames += before.saturating_sub(frames.len());
        if frames.is_empty() {
            continue;
        }

        let extract = render_extract(entry, &frames);
        if extract.trim().is_empty() {
            continue;
        }
        let extract_path = extract_path_for(aicx_home, &entry.agent, &entry.session_id);
        if !dry_run && cache_extracts && write_if_changed(&extract_path, extract.as_bytes())? {
            extracts_written += 1;
        }
        let indexed_path = if !dry_run && cache_extracts {
            extract_path
        } else {
            source_path.to_path_buf()
        };
        let date = frames
            .last()
            .map(|frame| frame.timestamp.format("%Y-%m-%d").to_string())
            .or_else(|| entry.date.clone())
            .unwrap_or_default();
        let project = entry
            .project
            .clone()
            .unwrap_or_else(|| "_unknown".to_string());
        let metadata = serde_json::json!({
            "source_path": indexed_path.to_string_lossy(),
            "project": project,
            "agent": entry.agent,
            "date": date,
            "kind": "conversations",
            "session_id": entry.session_id,
            "frame_kind": "conversation",
            "cwd": entry.cwd,
            "source_catalog_path": entry.source_path,
            "preview_lines": extract_preview_lines(&frames),
        });
        chunks.push(aicx_retrieve::ChunkRef {
            id: format!("{}:{}", entry.agent, entry.session_id),
            source_path: indexed_path.display().to_string(),
            text: extract,
            metadata,
        });
    }

    if chunks.is_empty() {
        anyhow::bail!(
            "source-driven index produced zero signal extracts from {} cataloged source(s)",
            selected.len()
        );
    }

    let manifest_path = if dry_run {
        None
    } else {
        let manifest =
            crate::vector_index::publish_source_lexical_generation(&chunks, &source_fingerprint)?;
        Some(
            crate::vector_index::hybrid_manifest_path(None)?
                .display()
                .to_string(),
        )
        .filter(|_| manifest.lexical_doc_count == chunks.len())
    };

    Ok(SourceIndexReport {
        catalog_path: catalog_path.display().to_string(),
        sources_total: selected.len(),
        sources_parsed,
        sources_skipped,
        raw_frames,
        signal_frames,
        filtered_frames,
        extracts_written,
        lexical_docs: chunks.len(),
        unchanged: false,
        wall_ms: started.elapsed().as_millis() as u64,
        manifest_path,
        skipped_by_agent,
    })
}

fn parse_catalog_source(entry: &CatalogEntry, path: &Path) -> Result<Vec<TimelineEntry>> {
    if entry.agent == "vibecrafted" {
        let body = crate::sanitize::read_to_string_validated(path)
            .with_context(|| format!("read runtime transcript {}", path.display()))?;
        // Token-stream runtime_runs logs interleave thought fragments with
        // visible text. Indexing the raw body made search surface
        // `{"type":"thought","data":"The"}` spam over real operator answers.
        let message = vibecrafted_signal_body(&body);
        if message.trim().is_empty() {
            return Ok(Vec::new());
        }
        let timestamp = fs::metadata(path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(chrono::DateTime::<chrono::Utc>::from)
            .unwrap_or_else(chrono::Utc::now);
        return Ok(vec![TimelineEntry {
            timestamp,
            agent: entry.agent.clone(),
            session_id: entry.session_id.clone(),
            role: "assistant".to_string(),
            message,
            frame_kind: Some(FrameKind::AgentReply),
            branch: None,
            cwd: entry.cwd.clone(),
            timestamp_source: Some("source_mtime".to_string()),
            source_path: Some(entry.source_path.clone()),
            source_sha256: None,
            source_line_span: None,
        }]);
    }

    let source_bytes = fs::metadata(path)
        .with_context(|| format!("stat source {}", path.display()))?
        .len();
    if entry.agent == "codex" && source_bytes > MAX_FULL_PARSE_BYTES {
        return parse_large_codex_signal(entry, path);
    }
    if source_bytes > MAX_FULL_PARSE_BYTES {
        anyhow::bail!(
            "source is {} bytes (bounded full-parser limit is {} bytes)",
            source_bytes,
            MAX_FULL_PARSE_BYTES
        );
    }

    let agent = match entry.agent.as_str() {
        "claude" => aicx_parser::engine::AgentKind::Claude,
        "codex" => aicx_parser::engine::AgentKind::Codex,
        "gemini" => aicx_parser::engine::AgentKind::Gemini,
        "grok" => aicx_parser::engine::AgentKind::Grok,
        "junie" => aicx_parser::engine::AgentKind::Junie,
        other => anyhow::bail!("unsupported catalog agent `{other}`"),
    };
    let parsed = crate::parser_dispatch::parse_file(
        agent,
        &entry.session_id,
        entry.logical_session_id.clone(),
        path,
    )?;
    Ok(crate::output::timeline_entries_from_model(parsed.model()))
}

/// Bounded signal-only reader for oversized Codex rollouts.
///
/// Historical rollouts can exceed hundreds of MB because tool results and
/// pasted artifacts share the JSONL. The full canonical projection pays for
/// all of that noise. This path drains over-cap records without allocating
/// them and deserializes only bounded message records.
fn parse_large_codex_signal(entry: &CatalogEntry, path: &Path) -> Result<Vec<TimelineEntry>> {
    // `path` comes from cataloged local session sources (operator home), not HTTP.
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let file = fs::File::open(path).with_context(|| format!("open source {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut frames = Vec::new();
    let mut line_no = 0u64;
    while let Some(record) = crate::sanitize::read_line_capped(&mut reader, MAX_JSONL_RECORD_BYTES)?
    {
        line_no += 1;
        if record.exceeded || record.line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&record.line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(serde_json::Value::as_str) != Some("message") {
            continue;
        }
        let Some(role) = payload.get("role").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let frame_kind = match role {
            "user" => FrameKind::UserMsg,
            "assistant" => FrameKind::AgentReply,
            _ => continue,
        };
        let message = payload
            .get("content")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let kind = item.get("type")?.as_str()?;
                if !matches!(kind, "input_text" | "output_text" | "text") {
                    return None;
                }
                item.get("text")?.as_str()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if message.trim().is_empty() {
            continue;
        }
        let timestamp = value
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);
        frames.push(TimelineEntry {
            timestamp,
            agent: entry.agent.clone(),
            session_id: entry.session_id.clone(),
            role: role.to_string(),
            message,
            frame_kind: Some(frame_kind),
            branch: None,
            cwd: entry.cwd.clone(),
            timestamp_source: Some("record".to_string()),
            source_path: Some(entry.source_path.clone()),
            source_sha256: None,
            source_line_span: Some((line_no, line_no)),
        });
    }
    Ok(frames)
}

fn is_signal_frame(frame: &TimelineEntry) -> bool {
    let signal_kind = match frame.frame_kind {
        Some(FrameKind::UserMsg | FrameKind::AgentReply) => true,
        Some(FrameKind::ToolCall | FrameKind::InternalThought | FrameKind::SystemNote) => false,
        None => matches!(frame.role.as_str(), "user" | "assistant"),
    };
    signal_kind
        && !crate::extraction::is_harness_injected_noise(&frame.role, &frame.message)
        && !looks_like_binary_payload(&frame.message)
}

/// Collapse a vibecrafted `runtime_runs/*/transcript.log` into indexable text.
///
/// Keeps visible `text` tokens and nested `agent_message` bodies; drops pure
/// `thought` token streams. Non-JSON lines (plain markdown transcripts) pass
/// through unchanged.
fn vibecrafted_signal_body(body: &str) -> String {
    let mut out = String::new();
    let mut saw_json_line = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            if !saw_json_line {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        };
        saw_json_line = true;
        let Some(ty) = value.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        match ty {
            "thought" => continue,
            "text" => {
                if let Some(data) = value.get("data").and_then(serde_json::Value::as_str) {
                    out.push_str(data);
                }
            }
            "item.completed" => {
                if let Some(item) = value.get("item") {
                    let item_ty = item.get("type").and_then(serde_json::Value::as_str);
                    if matches!(item_ty, Some("agent_message") | Some("message"))
                        && let Some(text) = item.get("text").and_then(serde_json::Value::as_str)
                    {
                        if !out.is_empty() && !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push_str(text);
                        out.push('\n');
                    }
                }
            }
            "agent_message" | "message" => {
                if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(text);
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    clean_message(&out)
}

fn looks_like_binary_payload(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    if lower.contains("data:image/") && lower.contains(";base64,") {
        return true;
    }
    message
        .split_whitespace()
        .any(|token| token.chars().count() > MAX_UNBROKEN_TOKEN_CHARS)
}

fn clean_message(message: &str) -> String {
    let mut cleaned = String::new();
    for line in message.lines() {
        if line.chars().count() > MAX_UNBROKEN_TOKEN_CHARS
            || (line.to_ascii_lowercase().contains("base64")
                && line.chars().count() > MAX_UNBROKEN_TOKEN_CHARS / 2)
        {
            continue;
        }
        if cleaned.chars().count() + line.chars().count() + 1 > MAX_MESSAGE_CHARS {
            cleaned.push_str("\n[message truncated by source index]\n");
            break;
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }
    cleaned.trim().to_string()
}

fn render_extract(entry: &CatalogEntry, frames: &[TimelineEntry]) -> String {
    let mut out = format!(
        "# AICX session extract\n\n- session: `{}`\n- agent: `{}`\n- project: `{}`\n- source: `{}`\n\n",
        entry.session_id,
        entry.agent,
        entry.project.as_deref().unwrap_or("_unknown"),
        entry.source_path
    );
    for frame in frames {
        let role = if frame.role == "user" {
            "user"
        } else {
            "assistant"
        };
        let header = format!(
            "## {} · {}\n\n",
            frame.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
            role
        );
        if out.chars().count() + header.chars().count() + frame.message.chars().count()
            > MAX_EXTRACT_CHARS
        {
            out.push_str("\n[session extract truncated by source index]\n");
            break;
        }
        out.push_str(&header);
        out.push_str(frame.message.trim());
        out.push_str("\n\n");
    }
    out
}

fn extract_preview_lines(frames: &[TimelineEntry]) -> Vec<String> {
    frames
        .iter()
        .flat_map(|frame| frame.message.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(6)
        .map(|line| line.chars().take(240).collect())
        .collect()
}

fn project_selected(project: Option<&str>, filters: &[String]) -> bool {
    filters.is_empty()
        || project.is_some_and(|project| {
            filters
                .iter()
                .any(|filter| project.eq_ignore_ascii_case(filter))
        })
}

fn source_fingerprint(catalog_path: &Path, entries: &[CatalogEntry]) -> Result<String> {
    let mut hasher = Sha256::new();
    // The catalog is the explicit snapshot boundary. Do not mix live source
    // mtimes into this digest: an active Vibecrafted transcript grows while
    // indexing and would make every immediate second run look dirty. A
    // subsequent `aicx catalog rebuild` changes the catalog bytes and admits
    // the new source snapshot deterministically.
    // `catalog_path` is the operator AICX catalog file under aicx_home.
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    hasher.update(
        fs::read(catalog_path)
            .with_context(|| format!("read catalog {}", catalog_path.display()))?,
    );
    for entry in entries {
        hasher.update(entry.agent.as_bytes());
        hasher.update([0]);
        hasher.update(entry.session_id.as_bytes());
        hasher.update([0]);
        hasher.update(entry.source_path.as_bytes());
    }
    Ok(hex::encode(hasher.finalize()))
}

fn extract_path_for(aicx_home: &Path, agent: &str, session_id: &str) -> PathBuf {
    let mut safe: String = session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    safe = safe.trim_matches(['.', '_']).to_string();
    if safe.is_empty() || safe.len() > 180 {
        let digest = Sha256::digest(session_id.as_bytes());
        safe = format!("session-{}", &hex::encode(digest)[..16]);
    }
    aicx_home
        .join("extracts")
        .join(agent)
        .join(format!("{safe}_conversation.md"))
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<bool> {
    if fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(false);
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("extract path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create extract dir {}", parent.display()))?;
    let tmp = path.with_extension("md.tmp");
    fs::write(&tmp, bytes).with_context(|| format!("write extract tmp {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("publish extract {} -> {}", tmp.display(), path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_project_slice_cannot_replace_the_global_generation() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-project-safety-{}",
            std::process::id()
        ));
        let error = build(
            &root,
            &["vetcoders/vibecrafted".to_string()],
            false,
            false,
            false,
        )
        .expect_err("project-scoped publish must fail before touching the index");

        assert!(
            error
                .to_string()
                .contains("project-scoped index publishing is retired")
        );
    }

    #[test]
    fn vibecrafted_signal_body_drops_thought_tokens_and_keeps_visible_text() {
        let raw = r#"{"type":"thought","data":"The"}
{"type":"thought","data":" user"}
{"type":"text","data":"I'll"}
{"type":"text","data":" start"}
{"type":"text","data":" with"}
{"type":"text","data":" catalog"}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"Routing strzałek is W2-B-4c."}}
"#;
        let cleaned = vibecrafted_signal_body(raw);
        assert!(
            cleaned.contains("I'll start with catalog"),
            "visible text tokens must reassemble; got {cleaned:?}"
        );
        assert!(
            cleaned.contains("Routing strzałek is W2-B-4c."),
            "agent_message bodies must survive; got {cleaned:?}"
        );
        assert!(
            !cleaned.contains("thought") && !cleaned.contains("\"data\":\"The\""),
            "thought token streams must not enter the index; got {cleaned:?}"
        );
    }

    #[test]
    fn vibecrafted_signal_body_keeps_plain_markdown_transcripts() {
        let raw = "# implement report\n\nRouting strzałek taby landed in W2-B-4c.\n";
        let cleaned = vibecrafted_signal_body(raw);
        assert!(cleaned.contains("Routing strzałek taby landed in W2-B-4c."));
    }
}
