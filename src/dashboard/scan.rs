//! Store scanner and dashboard payload extraction.

use anyhow::Result;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde_json::Value;
use std::collections::BTreeSet;
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

pub(super) fn scan_store(
    store_root: &Path,
    preview_chars: usize,
    scope: &DashboardScope,
) -> Result<ScanResult> {
    let store_root = crate::sanitize::validate_dir_path(store_root)?;
    let scope = scope.normalized();

    let mut stats = DashboardStats {
        search_backend: "raw-notes-fuzzy".to_string(),
        ..Default::default()
    };

    let mut assumptions = vec![
        "Data source is canonical files from ~/.aicx with repo and non-repository roots.".to_string(),
        "Layout is intentionally simplified to Search -> List -> Content for daily browsing.".to_string(),
        "Repo-scoped files are scanned from ~/.aicx/store/<org>/<repo>/<YYYY_MMDD>/<kind>/<agent>/...".to_string(),
        "Non-repository fallbacks are scanned from ~/.aicx/non-repository-contexts/<YYYY_MMDD>/<kind>/<agent>/...".to_string(),
        "Fuzzy search index uses normalized matching over file metadata and bounded raw-note content excerpts.".to_string(),
    ];

    let mut records = Vec::<DashboardRecord>::new();
    let mut projects = BTreeSet::<String>::new();
    let mut agents = BTreeSet::<String>::new();
    let mut kinds = BTreeSet::<String>::new();

    let index_path = store_root.join("index.json");
    let state_path = store_root.join("state.json");
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
            "Startup scope narrows dashboard payload to project/store buckets containing: {}",
            project
        ));
    }
    if let Some(hours) = scope.hours {
        assumptions.push(format!(
            "Startup scope narrows dashboard payload to the last {} hour(s) using extracted event timestamps when available, falling back to canonical chunk dates.",
            hours
        ));
    }

    for stored_file in crate::store::scan_context_files_at(&store_root)? {
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
            .strip_prefix(&store_root)
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
        store_root: store_root.display().to_string(),
        stats,
        assumptions,
        projects: projects.into_iter().collect(),
        agents: agents.into_iter().collect(),
        kinds: kinds.into_iter().collect(),
        records,
    };

    Ok(ScanResult { payload })
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
