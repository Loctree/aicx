//! Read-only catalog/live-session scanner with a legacy fallback.

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

use super::{
    DashboardPayload, DashboardRecord, DashboardScope, DashboardStats, ScanResult,
    parse_rfc3339_timestamp, project_matches_filter, sort_ts_matches_hours_scope,
};

const MAX_JSON_PARSE_BYTES: u64 = 8 * 1024 * 1024;
const SEARCH_READ_BYTES: u64 = 256 * 1024;
const MAX_SEARCH_TEXT_CHARS: usize = 12_000;
const MAX_DETAIL_CHARS: usize = 32_000;
/// Newest readable sessions kept on the dashboard list. The semantic search
/// still covers the whole catalog; opening the port must not wait on every file.
const DASHBOARD_LIST_CAP: usize = 400;
const DASHBOARD_READ_BUDGET: usize = 800;
type ConversationPreview = (Option<usize>, String, String, String, Option<i64>);

pub(super) fn scan_legacy_archive(
    aicx_home: &Path,
    preview_chars: usize,
    scope: &DashboardScope,
) -> Result<ScanResult> {
    let aicx_home = crate::sanitize::validate_dir_path(aicx_home)?;
    if crate::catalog::sessions_path_for(&aicx_home).is_file() {
        return scan_catalog_sessions(&aicx_home, preview_chars, scope);
    }
    let scope = scope.normalized();

    let mut stats = DashboardStats {
        search_backend: "raw-notes-fuzzy".to_string(),
        ..Default::default()
    };

    let mut assumptions = vec![
        "This compatibility view is read-only and scans legacy cards still present under ~/.aicx; catalog + extracts own live session truth.".to_string(),
        "Layout is intentionally simplified to Search -> List -> Content for archive inspection.".to_string(),
        "Legacy repo-scoped cards may remain under ~/.aicx/store/<org>/<repo>/<YYYY_MMDD>/<kind>/<agent>/...".to_string(),
        "Legacy non-repository cards may remain under ~/.aicx/non-repository-contexts/<YYYY_MMDD>/<kind>/<agent>/...".to_string(),
        "Archive fuzzy search uses normalized matching over file metadata and bounded content excerpts.".to_string(),
    ];

    let mut records = Vec::<DashboardRecord>::new();
    let mut projects = BTreeSet::<String>::new();
    let mut agents = BTreeSet::<String>::new();
    let mut kinds = BTreeSet::<String>::new();

    let index_path = aicx_home.join("index.json");
    let state_path = aicx_home.join("state.json");
    stats.index_loaded = index_path.exists();
    stats.state_loaded = state_path.exists();

    if !stats.index_loaded {
        assumptions.push(
            "index.json not found; per-project counters are derived from files only.".to_string(),
        );
    }
    if !stats.state_loaded {
        assumptions
            .push("state.json not found; dedup history is not surfaced in dashboard.".to_string());
    }

    if let Some(project) = scope.project.as_ref() {
        assumptions.push(format!(
            "Startup scope narrows the legacy archive view to project buckets containing: {}",
            project
        ));
    }
    if let Some(hours) = scope.hours {
        assumptions.push(format!(
            "Startup scope narrows dashboard payload to the last {} hour(s) using extracted event timestamps when available, falling back to canonical chunk dates.",
            hours
        ));
    }

    for stored_file in crate::legacy_archive::scan_context_files_at(&aicx_home)? {
        if !project_matches_filter(&stored_file.project, scope.project.as_deref()) {
            continue;
        }

        let file_path = stored_file.path.clone();
        let extension = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !supported_note_extension(&extension) {
            continue;
        }

        let metadata = match fs::metadata(&file_path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        let file_name = file_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown-file")
            .to_string();
        let (entry_count, preview, search_excerpt, detail_text, content_sort_ts) =
            read_preview_and_search_excerpt(&file_path, &extension, metadata.len(), preview_chars);

        let modified = metadata.modified().ok();
        let modified_utc = format_modified_utc(modified);
        let modified_sort_ts = modified.map(|mtime| DateTime::<Utc>::from(mtime).timestamp());
        let effective_sort_ts = content_sort_ts.or(modified_sort_ts);
        if !sort_ts_matches_hours_scope(effective_sort_ts, &stored_file.date_iso, scope.hours) {
            continue;
        }
        let sort_ts = effective_sort_ts.unwrap_or_default();
        let time = effective_sort_ts
            .and_then(|timestamp| Utc.timestamp_opt(timestamp, 0).single())
            .map(|datetime| datetime.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "00:00:00".to_string());
        let relative_path = file_path
            .strip_prefix(&aicx_home)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| file_path.display().to_string());

        let search_blob = trim_chars(
            &collapse_ws(&format!(
                "{} {} {} {} {} {}",
                stored_file.project,
                stored_file.agent,
                stored_file.date_iso,
                relative_path,
                stored_file.kind.dir_name(),
                search_excerpt
            ))
            .to_lowercase(),
            MAX_SEARCH_TEXT_CHARS,
        );

        stats.fuzzy_index_chars += search_blob.len();
        projects.insert(stored_file.project.clone());
        agents.insert(stored_file.agent.clone());
        kinds.insert(stored_file.kind.dir_name().to_string());

        let record = DashboardRecord {
            id: records.len() + 1,
            project: stored_file.project,
            agent: stored_file.agent,
            date: stored_file.date_iso,
            time,
            kind: stored_file.kind.dir_name().to_string(),
            extension,
            file_name,
            relative_path,
            absolute_path: file_path.display().to_string(),
            bytes: metadata.len(),
            size_human: human_size(metadata.len()),
            modified_utc,
            sort_ts,
            entry_count,
            preview,
            search_blob,
            detail_text,
        };

        stats.total_files += 1;
        stats.total_bytes += metadata.len();
        stats.total_entries_estimate += record.entry_count.unwrap_or(0);
        records.push(record);
    }

    records.sort_by(|a, b| {
        b.sort_ts
            .cmp(&a.sort_ts)
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });

    for (idx, rec) in records.iter_mut().enumerate() {
        rec.id = idx + 1;
    }

    stats.total_projects = projects.len();
    stats.total_days = records
        .iter()
        .map(|r| format!("{}:{}", r.project, r.date))
        .collect::<BTreeSet<_>>()
        .len();
    stats.agents_detected = agents.len();

    assumptions.push(format!(
        "Detected {} project(s), {} date bucket(s), and {} note file(s).",
        stats.total_projects, stats.total_days, stats.total_files
    ));
    assumptions.push(format!(
        "Fuzzy index stores ~{} normalized characters.",
        stats.fuzzy_index_chars
    ));

    if stats.malformed_session_files > 0 {
        assumptions.push(format!(
            "{} file(s) did not match expected session naming and were classified as raw-note files.",
            stats.malformed_session_files
        ));
    }

    let payload = DashboardPayload {
        generated_at: Utc::now().to_rfc3339(),
        aicx_home: aicx_home.display().to_string(),
        stats,
        assumptions,
        projects: projects.into_iter().collect(),
        agents: agents.into_iter().collect(),
        kinds: kinds.into_iter().collect(),
        records,
    };

    Ok(ScanResult { payload })
}

fn scan_catalog_sessions(
    aicx_home: &Path,
    preview_chars: usize,
    scope: &DashboardScope,
) -> Result<ScanResult> {
    let scope = scope.normalized();
    let mut by_key: BTreeMap<(String, String), crate::catalog::CatalogEntry> =
        crate::catalog::read_entries_at(aicx_home)?
            .into_iter()
            .map(|entry| ((entry.agent.clone(), entry.session_id.clone()), entry))
            .collect();
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let source_allow = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home);
    let cutoff_ns = scope
        .hours
        .map(|hours| Utc::now() - chrono::Duration::hours(hours as i64))
        .and_then(|cutoff| cutoff.timestamp_nanos_opt())
        .map(|value| value.max(0) as u128)
        .unwrap_or(0);
    let live_delta = crate::catalog::live_delta(aicx_home, &user_home, cutoff_ns).ok();
    if let Some(delta) = &live_delta {
        for entry in &delta.unadmitted {
            by_key.insert(
                (entry.agent.clone(), entry.session_id.clone()),
                entry.clone(),
            );
        }
    }

    let mut stats = DashboardStats {
        search_backend: "catalog-live-source".to_string(),
        state_loaded: true,
        ..Default::default()
    };
    let mut assumptions = vec![
        "Primary dataset: durable session catalog plus the bounded live delta; retired per-frame cards are not scanned.".to_string(),
        "Session content is read directly from allowlisted agent sources and bounded for preview/detail.".to_string(),
        "Project identity is the catalog owner/repo bucket; hot refresh reattributes resolvable cwd values from git origin.".to_string(),
    ];
    if let Some(delta) = &live_delta {
        assumptions.push(format!(
            "Live window saw {} candidate session(s), including {} unadmitted source(s), in {} ms.",
            delta.live_sessions,
            delta.unadmitted.len(),
            delta.wall_ms
        ));
    }

    let mut records = Vec::new();
    let mut projects = BTreeSet::new();
    let mut agents = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    kinds.insert("session".to_string());

    let mut candidates: Vec<_> = by_key.into_values().collect();
    let catalog_sessions = candidates.len();
    candidates.sort_by(|left, right| {
        right
            .source_mtime_ns
            .unwrap_or(0)
            .cmp(&left.source_mtime_ns.unwrap_or(0))
            .then_with(|| right.date.cmp(&left.date))
    });
    let mut attempts = 0usize;
    for entry in candidates {
        if records.len() >= DASHBOARD_LIST_CAP || attempts >= DASHBOARD_READ_BUDGET {
            break;
        }
        let project = entry
            .project
            .clone()
            .unwrap_or_else(|| "_unknown".to_string());
        if !project_matches_filter(&project, scope.project.as_deref()) {
            continue;
        }
        let Ok(path) = source_allow.resolve_file(entry.source_path.as_str()) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let modified = metadata.modified().ok();
        let modified_sort_ts = modified.map(|mtime| DateTime::<Utc>::from(mtime).timestamp());
        let date = entry.date.clone().unwrap_or_else(|| {
            modified_sort_ts
                .and_then(|timestamp| Utc.timestamp_opt(timestamp, 0).single())
                .map(|datetime| datetime.format("%Y-%m-%d").to_string())
                .unwrap_or_default()
        });
        if !sort_ts_matches_hours_scope(modified_sort_ts, &date, scope.hours) {
            continue;
        }
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("log")
            .to_ascii_lowercase();
        // A catalog row is a session. If the conversation reader cannot produce
        // a readable turn, leave the raw jsonl out of the default list.
        attempts += 1;
        let Some((entry_count, preview, search_excerpt, detail_text, content_sort_ts)) =
            read_catalog_conversation_preview(aicx_home, &entry, preview_chars)
        else {
            continue;
        };
        // Source modification time is the durable signal that a live session
        // changed. Some providers preserve stale or malformed frame timestamps,
        // so letting content time win can bury the hottest session.
        let sort_ts = modified_sort_ts.or(content_sort_ts).unwrap_or_default();
        let time = Utc
            .timestamp_opt(sort_ts, 0)
            .single()
            .map(|datetime| datetime.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "00:00:00".to_string());
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("session")
            .to_string();
        let search_blob = trim_chars(
            &collapse_ws(&format!(
                "{} {} {} {} {} {}",
                project,
                entry.agent,
                date,
                entry.session_id,
                entry.cwd.as_deref().unwrap_or(""),
                search_excerpt
            ))
            .to_lowercase(),
            MAX_SEARCH_TEXT_CHARS,
        );
        projects.insert(project.clone());
        agents.insert(entry.agent.clone());
        stats.fuzzy_index_chars += search_blob.len();
        stats.total_bytes += metadata.len();
        stats.total_entries_estimate += entry_count.unwrap_or(0);
        records.push(DashboardRecord {
            id: 0,
            project,
            agent: entry.agent,
            date,
            time,
            kind: "session".to_string(),
            extension,
            file_name,
            relative_path: entry.source_path.clone(),
            absolute_path: path.display().to_string(),
            bytes: metadata.len(),
            size_human: human_size(metadata.len()),
            modified_utc: format_modified_utc(modified),
            sort_ts,
            entry_count,
            preview,
            search_blob,
            detail_text,
        });
    }

    records.sort_by(|left, right| {
        right
            .sort_ts
            .cmp(&left.sort_ts)
            .then_with(|| left.absolute_path.cmp(&right.absolute_path))
    });
    for (index, record) in records.iter_mut().enumerate() {
        record.id = index + 1;
    }
    stats.total_files = records.len();
    stats.total_projects = projects.len();
    stats.total_days = records
        .iter()
        .map(|record| format!("{}:{}", record.project, record.date))
        .collect::<BTreeSet<_>>()
        .len();
    stats.agents_detected = agents.len();
    stats.index_loaded = aicx_home
        .join("indexed")
        .join("_all")
        .join("hybrid")
        .is_dir();
    assumptions.push(format!(
        "Showing the newest {} readable session(s) from {} catalog session(s) across {} project(s). Search covers the whole corpus. Rules, tool payloads, and empty epoch stamps stay out of the list.",
        stats.total_files, catalog_sessions, stats.total_projects
    ));

    Ok(ScanResult {
        payload: DashboardPayload {
            generated_at: Utc::now().to_rfc3339(),
            aicx_home: aicx_home.display().to_string(),
            stats,
            assumptions,
            projects: projects.into_iter().collect(),
            agents: agents.into_iter().collect(),
            kinds: kinds.into_iter().collect(),
            records,
        },
    })
}

/// Tags whose bodies are harness boilerplate, not something an operator reads.
const DASHBOARD_POLLUTION_TAGS: &[&str] = &[
    "rules",
    "always_applied_workspace_rules",
    "always_applied_workspace_rule",
    "user_rules",
    "agent_requestable_workspace_rules",
    "agent_requestable_workspace_rule",
    "user_info",
    "git_status",
    "agent_skills",
    "agent_skill",
    "system-reminder",
    "open_and_recently_viewed_files",
    "communication",
    "mcp_instructions",
    "user_query",
];

/// Readable default when `--preview-chars 0` would otherwise dump the whole turn.
const READABLE_PREVIEW_CHARS: usize = 1_600;

/// A stamp at or before the first Unix day is an empty epoch, not a session time.
fn is_empty_epoch(timestamp: DateTime<Utc>) -> bool {
    timestamp.timestamp() < 86_400
}

pub(super) fn strip_dashboard_pollution(message: &str) -> String {
    let mut cleaned = message.to_string();
    for tag in DASHBOARD_POLLUTION_TAGS {
        cleaned = strip_tag_blocks(&cleaned, tag);
    }
    cleaned.trim().to_string()
}

fn strip_tag_blocks(input: &str, tag: &str) -> String {
    let open_prefix = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    loop {
        let Some(start) = rest.find(&open_prefix) else {
            out.push_str(rest);
            break;
        };
        let after = &rest[start + open_prefix.len()..];
        let is_tag = after.starts_with('>')
            || after.starts_with('/')
            || after.starts_with(|ch: char| ch.is_whitespace());
        if !is_tag {
            out.push_str(&rest[..start + 1]);
            rest = &rest[start + 1..];
            continue;
        }
        out.push_str(&rest[..start]);
        if let Some(end_rel) = rest[start..].find(&close) {
            rest = &rest[start + end_rel + close.len()..];
        } else {
            break;
        }
    }
    out
}

fn is_dashboard_noise_turn(message: &str) -> bool {
    let head = message.trim_start();
    if head.is_empty() {
        return true;
    }
    if head.starts_with("Briefly inform the user about the task result") {
        return true;
    }
    // Tool payloads that survived the signal filter still read as raw logs.
    head.starts_with('{') && (head.contains("\"type\"") || head.contains("\"tool"))
}

fn read_catalog_conversation_preview(
    aicx_home: &Path,
    entry: &crate::catalog::CatalogEntry,
    preview_chars: usize,
) -> Option<ConversationPreview> {
    let (_, frames) = crate::source_index::read_catalog_conversation_at(aicx_home, entry).ok()?;
    if frames.is_empty() {
        return None;
    }

    let mut turns: Vec<String> = Vec::new();
    for frame in &frames {
        let message = strip_dashboard_pollution(&frame.message);
        if is_dashboard_noise_turn(&message) {
            continue;
        }
        let role = if frame.role == "user" {
            "user"
        } else {
            "assistant"
        };
        let stamp = if is_empty_epoch(frame.timestamp) {
            String::new()
        } else {
            format!("[{}] ", frame.timestamp.format("%Y-%m-%d %H:%M:%S"))
        };
        turns.push(format!("{stamp}{role}: {message}"));
        if turns.len() >= 80 {
            break;
        }
    }
    if turns.is_empty() {
        return None;
    }

    let mut conversation = String::new();
    for line in &turns {
        if conversation.chars().count() >= MAX_DETAIL_CHARS {
            break;
        }
        conversation.push_str(line.trim());
        conversation.push_str("\n\n");
    }
    let detail = trim_chars(conversation.trim(), MAX_DETAIL_CHARS);
    if detail.trim().is_empty() {
        return None;
    }

    let preview_cap = if preview_chars == 0 {
        READABLE_PREVIEW_CHARS
    } else {
        preview_chars
    };
    let preview = trim_chars(conversation.trim(), preview_cap);
    let search_excerpt = trim_chars(&collapse_ws(&conversation), MAX_SEARCH_TEXT_CHARS);
    let sort_ts = frames
        .iter()
        .rev()
        .find(|frame| !is_empty_epoch(frame.timestamp))
        .or_else(|| frames.last())
        .map(|frame| frame.timestamp.timestamp());
    Some((Some(turns.len()), preview, search_excerpt, detail, sort_ts))
}

fn supported_note_extension(ext: &str) -> bool {
    matches!(ext, "md" | "markdown" | "txt" | "json")
}

#[cfg(test)]
pub(super) fn classify_extension_kind_ref(ext: &str) -> &'static str {
    match ext {
        "json" => "raw-json",
        "txt" => "raw-text",
        "markdown" => "raw-markdown",
        _ => "raw-note",
    }
}

fn read_preview_and_search_excerpt(
    path: &Path,
    extension: &str,
    size: u64,
    preview_chars: usize,
) -> (Option<usize>, String, String, String, Option<i64>) {
    if extension == "json" {
        return read_json_preview_and_search(path, size, preview_chars);
    }

    let raw = read_text_limited(path, SEARCH_READ_BYTES);
    if raw.is_empty() {
        return (None, "".to_string(), "".to_string(), "".to_string(), None);
    }

    let detail = trim_chars(&sanitize_detail_text(&raw), MAX_DETAIL_CHARS);
    let collapsed = collapse_ws(&raw);
    let preview = trim_chars(&collapsed, preview_chars);
    let search_excerpt = trim_chars(&collapsed, MAX_SEARCH_TEXT_CHARS);
    let sort_ts = extract_latest_timestamp_from_text(&raw);

    (None, preview, search_excerpt, detail, sort_ts)
}

fn read_json_preview_and_search(
    path: &Path,
    size: u64,
    max_preview_chars: usize,
) -> (Option<usize>, String, String, String, Option<i64>) {
    if size > MAX_JSON_PARSE_BYTES {
        let raw = read_text_limited(path, SEARCH_READ_BYTES);
        let collapsed = collapse_ws(&raw);
        let preview = trim_chars(
            &format!(
                "JSON file too large to parse structurally; using raw excerpt ({}). {}",
                human_size(size),
                trim_chars(&collapsed, max_preview_chars)
            ),
            max_preview_chars,
        );
        let detail = trim_chars(&sanitize_detail_text(&raw), MAX_DETAIL_CHARS);
        return (
            None,
            preview,
            trim_chars(&collapsed, MAX_SEARCH_TEXT_CHARS),
            detail,
            None,
        );
    }

    let bytes = match fs::read(path) {
        Ok(v) => v,
        Err(_) => {
            return (
                None,
                "Failed to read JSON preview.".to_string(),
                "".to_string(),
                "".to_string(),
                None,
            );
        }
    };

    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            let raw = String::from_utf8_lossy(&bytes).to_string();
            let collapsed = collapse_ws(&raw);
            return (
                None,
                trim_chars(&collapsed, max_preview_chars),
                trim_chars(&collapsed, MAX_SEARCH_TEXT_CHARS),
                trim_chars(&sanitize_detail_text(&raw), MAX_DETAIL_CHARS),
                None,
            );
        }
    };

    let entry_count = value.as_array().map(|a| a.len());

    let mut strings = Vec::new();
    let mut total_chars = 0usize;
    collect_json_strings(
        &value,
        &mut strings,
        &mut total_chars,
        300,
        MAX_SEARCH_TEXT_CHARS * 2,
    );

    let collapsed = collapse_ws(&strings.join(" | "));
    let preview = if collapsed.is_empty() {
        trim_chars(
            "JSON payload parsed but no string fields were found.",
            max_preview_chars,
        )
    } else {
        trim_chars(&collapsed, max_preview_chars)
    };
    let search_excerpt = trim_chars(&collapsed, MAX_SEARCH_TEXT_CHARS);

    let pretty = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    let detail = trim_chars(&sanitize_detail_text(&pretty), MAX_DETAIL_CHARS);
    let sort_ts = extract_latest_timestamp_from_json(&value);

    (entry_count, preview, search_excerpt, detail, sort_ts)
}

pub(super) fn collect_json_strings(
    value: &Value,
    out: &mut Vec<String>,
    total_chars: &mut usize,
    max_items: usize,
    max_total_chars: usize,
) {
    if out.len() >= max_items || *total_chars >= max_total_chars {
        return;
    }

    match value {
        Value::String(s) => {
            let s = collapse_ws(s);
            if s.is_empty() {
                return;
            }
            let remaining = max_total_chars.saturating_sub(*total_chars);
            if remaining == 0 {
                return;
            }
            let clipped = trim_chars(&s, remaining);
            *total_chars += clipped.len();
            out.push(clipped);
        }
        Value::Array(items) => {
            for item in items {
                collect_json_strings(item, out, total_chars, max_items, max_total_chars);
                if out.len() >= max_items || *total_chars >= max_total_chars {
                    break;
                }
            }
        }
        Value::Object(map) => {
            for (_, v) in map {
                collect_json_strings(v, out, total_chars, max_items, max_total_chars);
                if out.len() >= max_items || *total_chars >= max_total_chars {
                    break;
                }
            }
        }
        _ => {}
    }
}

pub(super) fn extract_latest_timestamp_from_text(raw: &str) -> Option<i64> {
    let mut latest: Option<i64> = None;

    for line in raw.lines() {
        let trimmed = line.trim();

        if let Some(value) = trimmed.strip_prefix("### ")
            && let Some(timestamp) = value.split(" UTC |").next()
            && let Ok(parsed) = NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S")
        {
            latest = Some(latest.map_or(parsed.and_utc().timestamp(), |current| {
                current.max(parsed.and_utc().timestamp())
            }));
            continue;
        }

        for prefix in ["timestamp:", "started_at:", "completed_at:"] {
            if let Some(value) = trimmed.strip_prefix(prefix)
                && let Some(parsed) = parse_rfc3339_timestamp(value.trim())
            {
                latest = Some(latest.map_or(parsed.timestamp(), |current| {
                    current.max(parsed.timestamp())
                }));
            }
        }
    }

    latest
}

pub(super) fn extract_latest_timestamp_from_json(value: &Value) -> Option<i64> {
    let mut latest: Option<i64> = None;
    collect_json_timestamps(value, &mut latest);
    latest
}

fn collect_json_timestamps(value: &Value, latest: &mut Option<i64>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if matches!(
                    key.as_str(),
                    "timestamp" | "started_at" | "completed_at" | "ts"
                ) {
                    let parsed = match child {
                        Value::String(text) => {
                            parse_rfc3339_timestamp(text).map(|dt| dt.timestamp())
                        }
                        Value::Number(number) => number.as_i64(),
                        _ => None,
                    };
                    if let Some(parsed) = parsed {
                        *latest = Some(latest.map_or(parsed, |current| current.max(parsed)));
                    }
                }
                collect_json_timestamps(child, latest);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_json_timestamps(item, latest);
            }
        }
        _ => {}
    }
}

fn read_text_limited(path: &Path, max_bytes: u64) -> String {
    let mut file = match fs::File::open(path) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };

    let mut buf = Vec::new();
    if file.by_ref().take(max_bytes).read_to_end(&mut buf).is_err() {
        return String::new();
    }

    String::from_utf8_lossy(&buf).to_string()
}

fn sanitize_detail_text(input: &str) -> String {
    input.replace('\0', "").replace("\r\n", "\n")
}
fn format_modified_utc(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "unknown".to_string();
    };

    let dt: DateTime<Utc> = modified.into();
    dt.to_rfc3339()
}
fn trim_chars(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return s.to_string();
    }

    let mut out = String::new();
    for (idx, ch) in s.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut was_ws = false;

    for ch in s.chars() {
        if ch.is_whitespace() {
            if !was_ws {
                out.push(' ');
            }
            was_ws = true;
        } else {
            out.push(ch);
            was_ws = false;
        }
    }

    out.trim().to_string()
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{} B", bytes)
    }
}
