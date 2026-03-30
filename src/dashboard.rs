//! AI Contexters dashboard generator.
//!
//! Builds a static HTML dashboard for daily browsing of raw extracted notes
//! from the ai-contexters store (`~/.ai-contexters` by default).
//!
//! Layout: Search -> List -> Content
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(test)]
use regex::Regex;

const MAX_JSON_PARSE_BYTES: u64 = 8 * 1024 * 1024;
const SEARCH_READ_BYTES: u64 = 256 * 1024;
const MAX_SEARCH_TEXT_CHARS: usize = 12_000;
const MAX_DETAIL_CHARS: usize = 32_000;

/// Configuration for dashboard generation.
#[derive(Debug, Clone)]
pub struct DashboardConfig {
    /// Store root directory (`~/.ai-contexters`).
    pub store_root: PathBuf,
    /// HTML document title.
    pub title: String,
    /// Max characters in per-record preview.
    pub preview_chars: usize,
}

/// Dashboard generation output.
#[derive(Debug, Clone)]
pub struct DashboardArtifact {
    /// Rendered HTML page.
    pub html: String,
    /// Aggregate stats shown in CLI output.
    pub stats: DashboardStats,
    /// Assumptions detected/labeled during scan.
    pub assumptions: Vec<String>,
}

/// Aggregate stats for dashboard payload.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DashboardStats {
    pub total_projects: usize,
    pub total_days: usize,
    pub total_files: usize,
    pub total_bytes: u64,
    pub total_entries_estimate: usize,
    pub agents_detected: usize,
    pub malformed_session_files: usize,
    pub ignored_non_date_dirs: usize,
    pub ignored_non_store_projects: usize,
    pub index_loaded: bool,
    pub state_loaded: bool,
    pub fuzzy_index_chars: usize,
    pub search_backend: String,
}

#[derive(Debug, Clone, Serialize)]
struct DashboardPayload {
    generated_at: String,
    store_root: String,
    stats: DashboardStats,
    assumptions: Vec<String>,
    projects: Vec<String>,
    agents: Vec<String>,
    kinds: Vec<String>,
    records: Vec<DashboardRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct DashboardRecord {
    id: usize,
    project: String,
    agent: String,
    date: String,
    time: String,
    kind: String,
    extension: String,
    file_name: String,
    relative_path: String,
    absolute_path: String,
    bytes: u64,
    size_human: String,
    modified_utc: String,
    sort_ts: i64,
    entry_count: Option<usize>,
    preview: String,
    search_blob: String,
    detail_text: String,
}

#[derive(Debug, Clone)]
struct ScanResult {
    payload: DashboardPayload,
}

/// Build a complete HTML dashboard from store data.
pub fn build_dashboard(config: &DashboardConfig) -> Result<DashboardArtifact> {
    let scan = scan_store(&config.store_root, config.preview_chars)?;
    let html = render_dashboard_html(&scan.payload, &config.title)?;

    Ok(DashboardArtifact {
        html,
        stats: scan.payload.stats.clone(),
        assumptions: scan.payload.assumptions.clone(),
    })
}

fn scan_store(store_root: &Path, preview_chars: usize) -> Result<ScanResult> {
    let store_root = crate::sanitize::validate_dir_path(store_root)?;

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

    for stored_file in crate::store::scan_context_files_at(&store_root)? {
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
        let (entry_count, preview, search_excerpt, detail_text) =
            read_preview_and_search_excerpt(&file_path, &extension, metadata.len(), preview_chars);

        let modified = metadata.modified().ok();
        let modified_utc = format_modified_utc(modified);
        let time = modified
            .map(DateTime::<Utc>::from)
            .map(|datetime| datetime.format("%H:%M:%S").to_string())
            .unwrap_or_else(|| "00:00:00".to_string());
        let sort_ts = modified
            .map(|mtime| DateTime::<Utc>::from(mtime).timestamp())
            .unwrap_or_default();
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
fn classify_extension_kind_ref(ext: &str) -> &'static str {
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
) -> (Option<usize>, String, String, String) {
    if extension == "json" {
        return read_json_preview_and_search(path, size, preview_chars);
    }

    let raw = read_text_limited(path, SEARCH_READ_BYTES);
    if raw.is_empty() {
        return (None, "".to_string(), "".to_string(), "".to_string());
    }

    let detail = trim_chars(&sanitize_detail_text(&raw), MAX_DETAIL_CHARS);
    let collapsed = collapse_ws(&raw);
    let preview = trim_chars(&collapsed, preview_chars);
    let search_excerpt = trim_chars(&collapsed, MAX_SEARCH_TEXT_CHARS);

    (None, preview, search_excerpt, detail)
}

fn read_json_preview_and_search(
    path: &Path,
    size: u64,
    max_preview_chars: usize,
) -> (Option<usize>, String, String, String) {
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

    (entry_count, preview, search_excerpt, detail)
}

fn collect_json_strings(
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

fn render_dashboard_html(payload: &DashboardPayload, title: &str) -> Result<String> {
    let payload_json =
        serde_json::to_string(payload).context("Failed to serialize dashboard payload")?;
    let payload_json = payload_json
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");

    Ok(format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{}</title>
  <style>{}</style>
</head>
<body>
  <div class="app-shell">
    <header class="app-header">
      <div>
        <h1>AI Context Browser</h1>
        <p class="meta">Search -> List -> Content | {}</p>
        <p class="meta">Generated {}</p>
      </div>
      <div class="header-stats">
        <div class="stat"><strong>{}</strong><span>files</span></div>
        <div class="stat"><strong>{}</strong><span>projects</span></div>
        <div class="stat"><strong>{}</strong><span>days</span></div>
      </div>
    </header>

    <section class="controls">
      <div class="search-row">
        <input id="ctx-search" type="search" placeholder="Fuzzy search… (Enter or pause to trigger)" autocomplete="off" />
        <label class="live-toggle" title="Live search (search while typing)">
          <input id="ctx-live" type="checkbox" /> <span>Live</span>
        </label>
      </div>
      <div class="filter-row">
        <select id="ctx-project"><option value="">All projects</option></select>
        <select id="ctx-agent"><option value="">All agents/sources</option></select>
        <select id="ctx-kind"><option value="">All kinds</option></select>
      </div>
    </section>

    <section class="layout" id="ctx-layout">
      <aside class="list-pane">
        <div id="ctx-summary" class="summary"></div>
        <div id="ctx-list" class="result-list"></div>
      </aside>

      <div class="resize-handle" id="ctx-resize-handle" title="Drag to resize panels"></div>

      <article class="detail-pane">
        <div class="detail-head">
          <div>
            <h2 id="ctx-detail-title">Select a result</h2>
            <p id="ctx-detail-meta" class="detail-meta"></p>
          </div>
          <button id="ctx-copy-path" type="button">Copy Path</button>
        </div>

        <p id="ctx-detail-path" class="detail-path"></p>
        <p id="ctx-detail-preview" class="detail-preview"></p>
        <pre id="ctx-detail-content" class="detail-content"></pre>

        <details class="assumptions" open>
          <summary>Assumptions</summary>
          <ul id="ctx-assumptions"></ul>
        </details>
      </article>
    </section>
  </div>

  <script id="ctx-data" type="application/json">{}</script>
  <script>{}</script>
</body>
</html>
"#,
        html_escape(title),
        DASHBOARD_CSS,
        html_escape(&payload.store_root),
        html_escape(&payload.generated_at),
        payload.stats.total_files,
        payload.stats.total_projects,
        payload.stats.total_days,
        payload_json,
        DASHBOARD_SCRIPT
    ))
}

fn format_modified_utc(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "unknown".to_string();
    };

    let dt: DateTime<Utc> = modified.into();
    dt.to_rfc3339()
}

#[cfg(test)]
fn parse_session_filename(file_name: &str, re: &Regex) -> Option<(String, String, String)> {
    let caps = re.captures(file_name)?;

    let time = caps.name("time")?.as_str().to_string();
    let agent = caps
        .name("agent")
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let suffix = caps
        .name("suffix")
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    let ext = caps
        .name("ext")
        .map(|m| m.as_str().to_ascii_lowercase())
        .unwrap_or_default();

    let kind = if suffix == "context" && ext == "json" {
        "context-json"
    } else if suffix == "context" {
        "context-note"
    } else if suffix.chars().all(|c| c.is_ascii_digit()) {
        "chunk"
    } else {
        classify_extension_kind_ref(&ext)
    }
    .to_string();

    Some((time, agent, kind))
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

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const DASHBOARD_CSS: &str = r#"
:root {
  color-scheme: dark;
  --bg: #0a0f19;
  --panel: #111827;
  --panel-2: #0f172a;
  --line: #1f2937;
  --text: #e5e7eb;
  --muted: #9ca3af;
  --accent: #38bdf8;
  --accent-2: #22d3ee;
}

* { box-sizing: border-box; }
body {
  margin: 0;
  font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif;
  background: radial-gradient(1200px 700px at 20% -10%, #13233f 0%, var(--bg) 52%);
  color: var(--text);
}

.app-shell {
  max-width: 1500px;
  margin: 0 auto;
  padding: 18px;
}

.app-header {
  display: flex;
  justify-content: space-between;
  gap: 16px;
  align-items: flex-start;
  padding: 10px 2px 16px;
}

.app-header h1 {
  margin: 0;
  font-size: 1.45rem;
}

.meta {
  margin: 4px 0 0;
  color: var(--muted);
  font-size: 0.9rem;
}

.header-stats {
  display: grid;
  grid-template-columns: repeat(3, minmax(90px, 1fr));
  gap: 8px;
}

.stat {
  border: 1px solid var(--line);
  background: var(--panel);
  border-radius: 10px;
  padding: 8px 10px;
  text-align: right;
}

.stat strong {
  display: block;
  font-size: 1.1rem;
}

.stat span {
  color: var(--muted);
  font-size: 0.75rem;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}

.controls {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-bottom: 12px;
}

.search-row {
  display: flex;
  gap: 10px;
  align-items: center;
}

.search-row input[type="search"] {
  flex: 1;
  min-width: 0;
}

.filter-row {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 10px;
}

.live-toggle {
  display: flex;
  align-items: center;
  gap: 6px;
  cursor: pointer;
  font-size: 0.82rem;
  color: var(--muted);
  white-space: nowrap;
  user-select: none;
  min-width: 60px;
  padding: 8px 12px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--panel);
  flex-shrink: 0;
}

.live-toggle input:checked + span {
  color: var(--accent, #4fc3f7);
  font-weight: 600;
}

.controls input[type="search"],
.controls select {
  width: 100%;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--panel);
  color: var(--text);
  padding: 11px 12px;
  font-size: 0.98rem;
  transition: border-color 0.15s;
}

.controls input[type="search"]:focus,
.controls select:focus {
  outline: none;
  border-color: var(--accent);
  box-shadow: 0 0 0 2px rgba(56, 189, 248, 0.15);
}

.layout {
  display: grid;
  grid-template-columns: minmax(250px, 0.95fr) 6px minmax(300px, 1.45fr);
  gap: 0;
  min-height: calc(100vh - 210px);
}

.resize-handle {
  width: 6px;
  cursor: col-resize;
  position: relative;
  z-index: 10;
  background: transparent;
  transition: background 0.15s;
}

.resize-handle::after {
  content: '';
  position: absolute;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  width: 2px;
  height: 48px;
  border-radius: 2px;
  background: var(--line);
  transition: background 0.15s, height 0.15s;
}

.resize-handle:hover,
.resize-handle.dragging {
  background: rgba(56, 189, 248, 0.06);
}

.resize-handle:hover::after,
.resize-handle.dragging::after {
  background: var(--accent);
  height: 72px;
}

.list-pane,
.detail-pane {
  border: 1px solid var(--line);
  border-radius: 12px;
  background: linear-gradient(180deg, var(--panel), var(--panel-2));
  overflow: hidden;
  min-width: 0;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.2);
}

.list-pane {
  margin-right: 3px;
}

.detail-pane {
  margin-left: 3px;
}

.summary {
  padding: 12px 14px;
  color: var(--muted);
  border-bottom: 1px solid var(--line);
}

.result-list {
  max-height: calc(100vh - 320px);
  overflow: auto;
}

.result-item {
  width: 100%;
  text-align: left;
  border: 0;
  border-bottom: 1px solid rgba(255, 255, 255, 0.04);
  background: transparent;
  color: inherit;
  padding: 11px 13px;
  cursor: pointer;
  transition: background 0.12s;
}

.result-item:hover {
  background: rgba(56, 189, 248, 0.08);
}

.result-item.active {
  background: rgba(34, 211, 238, 0.14);
}

.result-top {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}

.badge {
  border: 1px solid var(--line);
  border-radius: 999px;
  padding: 2px 8px;
  font-size: 0.72rem;
  color: var(--muted);
}

.result-name {
  margin-top: 6px;
  font-size: 0.88rem;
}

.detail-pane {
  display: flex;
  flex-direction: column;
}

.detail-head {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  align-items: flex-start;
  padding: 13px 14px;
  border-bottom: 1px solid var(--line);
}

.detail-head h2 {
  margin: 0;
  font-size: 1.08rem;
}

.detail-meta {
  margin: 5px 0 0;
  color: var(--muted);
  font-size: 0.86rem;
}

.detail-head button {
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--panel);
  color: var(--text);
  padding: 8px 10px;
  cursor: pointer;
}

.detail-head button:hover {
  border-color: var(--accent);
}

.detail-path,
.detail-preview {
  padding: 0 14px;
  margin: 10px 0 0;
  color: var(--muted);
  font-size: 0.86rem;
}

.detail-preview {
  color: var(--text);
}

.detail-content {
  margin: 10px 14px 12px;
  border: 1px solid var(--line);
  background: #0b1220;
  border-radius: 10px;
  padding: 12px;
  overflow: auto;
  white-space: pre-wrap;
  line-height: 1.35;
  font-size: 0.86rem;
  flex: 1;
  min-height: 280px;
}

.assumptions {
  margin: 0 14px 14px;
  color: var(--muted);
}

.assumptions ul {
  margin: 8px 0 0;
  padding-left: 18px;
}

.empty {
  padding: 16px;
  color: var(--muted);
}

@media (max-width: 1020px) {
  .filter-row {
    grid-template-columns: 1fr;
  }

  .layout {
    grid-template-columns: 1fr !important;
    min-height: 0;
  }

  .resize-handle {
    display: none;
  }

  .list-pane,
  .detail-pane {
    margin: 0;
  }

  .result-list {
    max-height: 360px;
  }

  .detail-content {
    min-height: 220px;
  }
}
"#;

const DASHBOARD_SCRIPT: &str = r#"
(() => {
  const dataNode = document.getElementById('ctx-data');
  if (!dataNode) return;

  let payload = null;
  try {
    payload = JSON.parse(dataNode.textContent || '{}');
  } catch (_err) {
    return;
  }

  const records = Array.isArray(payload.records) ? payload.records : [];
  const ui = {
    search: document.getElementById('ctx-search'),
    project: document.getElementById('ctx-project'),
    agent: document.getElementById('ctx-agent'),
    kind: document.getElementById('ctx-kind'),
    summary: document.getElementById('ctx-summary'),
    list: document.getElementById('ctx-list'),
    detailTitle: document.getElementById('ctx-detail-title'),
    detailMeta: document.getElementById('ctx-detail-meta'),
    detailPath: document.getElementById('ctx-detail-path'),
    detailPreview: document.getElementById('ctx-detail-preview'),
    detailContent: document.getElementById('ctx-detail-content'),
    assumptions: document.getElementById('ctx-assumptions'),
    copyPath: document.getElementById('ctx-copy-path'),
  };

  if (!ui.search || !ui.project || !ui.agent || !ui.kind || !ui.summary || !ui.list || !ui.detailTitle || !ui.detailMeta || !ui.detailPath || !ui.detailPreview || !ui.detailContent || !ui.assumptions || !ui.copyPath) {
    return;
  }

  const hooks = {
    beforeRender: [],
    afterRender: [],
    onSelect: [],
  };

  const state = {
    query: '',
    project: '',
    agent: '',
    kind: '',
    limit: 350,
    selectedId: null,
    rows: [],
    selectedRecord: null,
  };

  const normalize = (value) =>
    (value || '')
      .toString()
      .toLowerCase()
      .normalize('NFKD')
      .replace(/[\u0300-\u036f]/g, '')
      .replace(/\s+/g, ' ')
      .trim();

  const fillSelect = (node, values) => {
    values.forEach((value) => {
      const option = document.createElement('option');
      option.value = value;
      option.textContent = value;
      node.appendChild(option);
    });
  };

  fillSelect(ui.project, Array.isArray(payload.projects) ? payload.projects : []);
  fillSelect(ui.agent, Array.isArray(payload.agents) ? payload.agents : []);
  fillSelect(ui.kind, Array.isArray(payload.kinds) ? payload.kinds : []);

  (Array.isArray(payload.assumptions) ? payload.assumptions : []).forEach((item) => {
    const li = document.createElement('li');
    li.textContent = item;
    ui.assumptions.appendChild(li);
  });

  const uniqueChars = (text) => {
    const set = new Set();
    for (const ch of text) set.add(ch);
    return set;
  };

  const charJaccard = (a, b) => {
    if (!a || !b) return 0;
    const sa = uniqueChars(a);
    const sb = uniqueChars(b);
    let inter = 0;
    for (const ch of sa) {
      if (sb.has(ch)) inter += 1;
    }
    const union = sa.size + sb.size - inter;
    return union > 0 ? inter / union : 0;
  };

  const subsequenceScore = (needle, haystack) => {
    if (!needle || !haystack) return 0;
    let i = 0;
    let j = 0;
    while (i < needle.length && j < haystack.length) {
      if (needle[i] === haystack[j]) i += 1;
      j += 1;
    }
    return i / needle.length;
  };

  const tokenScore = (token, field, weight) => {
    if (!token || !field) return 0;

    if (field.includes(token)) {
      return weight * (1 + Math.min(token.length / 12, 1));
    }

    const subseq = subsequenceScore(token, field);
    if (subseq < 0.7) return 0;

    const jac = charJaccard(token, field);
    return weight * (0.35 * subseq + 0.15 * jac);
  };

  const fieldsForRecord = (record) => ({
    project: normalize(record.project),
    agent: normalize(record.agent),
    fileName: normalize(record.file_name),
    relPath: normalize(record.relative_path),
    preview: normalize(record.preview),
    blob: normalize(record.search_blob),
  });

  const scoreRecord = (record, tokens) => {
    if (!tokens.length) return 1;

    const fields = fieldsForRecord(record);
    let total = 0;

    for (const token of tokens) {
      const best = Math.max(
        tokenScore(token, fields.project, 2.3),
        tokenScore(token, fields.agent, 2.0),
        tokenScore(token, fields.fileName, 1.9),
        tokenScore(token, fields.relPath, 1.7),
        tokenScore(token, fields.preview, 1.2),
        tokenScore(token, fields.blob, 1.0),
      );
      total += best;
    }

    const threshold = Math.max(0.22 * tokens.length, 0.35);
    return total >= threshold ? total : 0;
  };

  const runHooks = (name, value) => {
    const list = hooks[name] || [];
    return list.reduce((acc, fn) => {
      try {
        const maybe = fn(acc, payload, state);
        return maybe === undefined ? acc : maybe;
      } catch (_err) {
        return acc;
      }
    }, value);
  };

  const renderDetail = (record, score) => {
    state.selectedRecord = record || null;

    if (!record) {
      ui.detailTitle.textContent = 'No result selected';
      ui.detailMeta.textContent = '';
      ui.detailPath.textContent = '';
      ui.detailPreview.textContent = '';
      ui.detailContent.textContent = 'Use search or filters to pick a note.';
      return;
    }

    ui.detailTitle.textContent = record.file_name || '(unnamed file)';
    ui.detailMeta.textContent = `${record.project || 'unknown'} | ${record.agent || 'unknown'} | ${record.kind || 'unknown'} | score ${Math.round(Number(score || 0) * 100)}/100`;
    ui.detailPath.textContent = record.absolute_path || record.relative_path || '';
    ui.detailPreview.textContent = record.preview || '';
    ui.detailContent.textContent = record.detail_text || record.preview || '(no content)';
  };

  const renderList = (rows) => {
    ui.list.innerHTML = '';

    if (!rows.length) {
      const empty = document.createElement('div');
      empty.className = 'empty';
      empty.textContent = 'No records match current query/filters.';
      ui.list.appendChild(empty);
      renderDetail(null, 0);
      return;
    }

    const visible = rows.slice(0, state.limit);

    if (!state.selectedId || !visible.some((r) => r.record.id === state.selectedId)) {
      state.selectedId = visible[0].record.id;
    }

    visible.forEach(({ record, score }) => {
      const item = document.createElement('button');
      item.type = 'button';
      item.className = 'result-item' + (record.id === state.selectedId ? ' active' : '');

      const top = document.createElement('div');
      top.className = 'result-top';

      const mkBadge = (txt) => {
        const node = document.createElement('span');
        node.className = 'badge';
        node.textContent = txt;
        return node;
      };

      top.appendChild(mkBadge(record.project || 'project'));
      top.appendChild(mkBadge(record.agent || 'agent'));
      top.appendChild(mkBadge(record.kind || 'kind'));
      top.appendChild(mkBadge(record.date || 'date'));
      top.appendChild(mkBadge(`${Math.round(Number(score) * 100)}/100`));

      const name = document.createElement('div');
      name.className = 'result-name';
      name.textContent = `${record.file_name || '(unnamed)'} • ${record.size_human || ''}`;

      item.appendChild(top);
      item.appendChild(name);

      item.addEventListener('click', () => {
        state.selectedId = record.id;
        renderList(state.rows);
        renderDetail(record, score);
        runHooks('onSelect', record);
      });

      ui.list.appendChild(item);
    });

    const selected = visible.find((r) => r.record.id === state.selectedId) || visible[0];
    if (selected) {
      renderDetail(selected.record, selected.score);
    }
  };

  const refresh = () => {
    state.query = normalize(ui.search.value);
    state.project = ui.project.value;
    state.agent = ui.agent.value;
    state.kind = ui.kind.value;

    const tokens = state.query.split(' ').filter(Boolean);

    let rows = records
      .filter((record) => {
        if (state.project && record.project !== state.project) return false;
        if (state.agent && record.agent !== state.agent) return false;
        if (state.kind && record.kind !== state.kind) return false;
        return true;
      })
      .map((record) => ({
        record,
        score: scoreRecord(record, tokens),
      }))
      .filter((row) => row.score > 0)
      .sort((a, b) => {
        if (b.score !== a.score) return b.score - a.score;
        return (b.record.sort_ts || 0) - (a.record.sort_ts || 0);
      });

    rows = runHooks('beforeRender', rows);
    state.rows = rows;

    ui.summary.textContent = `${rows.length} fuzzy match(es) | showing up to ${state.limit} | total files: ${records.length}`;

    renderList(rows);
    runHooks('afterRender', rows);
  };

  /* --- debounced search ------------------------------------------------- */
  const DEBOUNCE_MS = 800;
  let debounceTimer = null;
  const liveCheckbox = document.getElementById('ctx-live');

  const scheduleRefresh = () => {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(refresh, DEBOUNCE_MS);
  };

  ui.search.addEventListener('input', () => {
    if (liveCheckbox.checked) {
      scheduleRefresh();              // live mode: 800 ms debounce
    }
    // non-live: wait for Enter or space (handled below)
  });

  ui.search.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') {
      clearTimeout(debounceTimer);
      refresh();
    }
  });

  // dropdowns always refresh immediately
  ['input', 'change'].forEach((eventName) => {
    ui.project.addEventListener(eventName, refresh);
    ui.agent.addEventListener(eventName, refresh);
    ui.kind.addEventListener(eventName, refresh);
  });

  liveCheckbox.addEventListener('change', () => {
    if (liveCheckbox.checked) scheduleRefresh();
  });

  ui.copyPath.addEventListener('click', async () => {
    const path = state.selectedRecord?.absolute_path || state.selectedRecord?.relative_path || '';
    if (!path || !navigator.clipboard) return;
    try {
      await navigator.clipboard.writeText(path);
    } catch (_err) {
      // no-op
    }
  });

  /* --- resizable panels ------------------------------------------------ */
  const resizeHandle = document.getElementById('ctx-resize-handle');
  const layoutEl = document.getElementById('ctx-layout');

  if (resizeHandle && layoutEl) {
    const STORAGE_KEY = 'aicx-split-ratio';
    const MIN_LIST = 250;
    const MIN_DETAIL = 300;

    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved) {
      const ratio = parseFloat(saved);
      if (ratio > 0 && ratio < 1) {
        layoutEl.style.gridTemplateColumns = `${ratio}fr 6px ${1 - ratio}fr`;
      }
    }

    let dragging = false;

    resizeHandle.addEventListener('mousedown', (e) => {
      e.preventDefault();
      dragging = true;
      resizeHandle.classList.add('dragging');
      document.body.style.cursor = 'col-resize';
      document.body.style.userSelect = 'none';
    });

    document.addEventListener('mousemove', (e) => {
      if (!dragging) return;
      const rect = layoutEl.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const total = rect.width - 6;
      const listW = Math.max(MIN_LIST, Math.min(x, total - MIN_DETAIL));
      const ratio = listW / total;
      layoutEl.style.gridTemplateColumns = `${ratio}fr 6px ${1 - ratio}fr`;
      localStorage.setItem(STORAGE_KEY, ratio.toFixed(4));
    });

    document.addEventListener('mouseup', () => {
      if (!dragging) return;
      dragging = false;
      resizeHandle.classList.remove('dragging');
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    });
  }

  window.AIContextersDashboard = {
    version: '4.0.0',
    payload,
    state,
    registerHook(name, fn) {
      if (!hooks[name] || typeof fn !== 'function') return false;
      hooks[name].push(fn);
      return true;
    },
    refresh,
  };

  refresh();
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::current_dir()
            .expect("cwd")
            .join("target")
            .join("test-tmp")
            .join(format!("{}_{}", name, Utc::now().timestamp_micros()));
        fs::create_dir_all(&dir).expect("create dir");
        dir
    }

    #[test]
    fn parses_session_filename_variants() {
        let re = Regex::new(
            r"^(?P<time>\d{6})_(?P<agent>[A-Za-z0-9][A-Za-z0-9_-]*?)(?:-(?P<suffix>context|\d{3}|[A-Za-z0-9_-]+))?\.(?P<ext>md|json|txt|markdown)$",
        )
        .expect("regex");

        let a = parse_session_filename("034519_claude-context.json", &re).expect("a");
        assert_eq!(a.0, "034519");
        assert_eq!(a.1, "claude");
        assert_eq!(a.2, "context-json");

        let b = parse_session_filename("185442_codex-003.md", &re).expect("b");
        assert_eq!(b.1, "codex");
        assert_eq!(b.2, "chunk");
    }

    #[test]
    fn scans_store_and_builds_payload() {
        let root = mk_tmp_dir("ai_ctx_dashboard_scan");
        let proj = root
            .join("store")
            .join("local")
            .join("demo-project")
            .join("2026_0224")
            .join("conversations")
            .join("codex");
        fs::create_dir_all(&proj).expect("proj");

        fs::write(
            proj.join("2026_0224_codex_dashjson001_001.json"),
            r#"[
                {"timestamp":"2026-02-24T10:11:12Z","agent":"codex","role":"user","message":"hello world"}
            ]"#,
        )
        .expect("json");
        fs::write(
            proj.join("2026_0224_codex_dashmd001_001.md"),
            "# demo\n\n### 2026-02-24 10:11:12 UTC | user\n> hello world\n",
        )
        .expect("md");

        fs::write(
            root.join("index.json"),
            r#"{"projects":{},"last_updated":"2026-02-24T00:00:00Z"}"#,
        )
        .expect("index");
        fs::write(
            root.join("state.json"),
            r#"{"last_processed":{},"seen_hashes":{},"runs":[]}"#,
        )
        .expect("state");

        let scan = scan_store(&root, 120).expect("scan");
        assert_eq!(scan.payload.stats.total_projects, 1);
        assert_eq!(scan.payload.stats.total_files, 2);
        assert_eq!(scan.payload.stats.search_backend, "raw-notes-fuzzy");
        assert!(
            scan.payload
                .records
                .iter()
                .any(|r| r.kind == "conversations")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn builds_dashboard_html_with_simple_layout() {
        let root = mk_tmp_dir("ai_ctx_dashboard_html");
        let proj = root
            .join("store")
            .join("local")
            .join("demo")
            .join("2026_0224")
            .join("conversations")
            .join("claude");
        fs::create_dir_all(&proj).expect("proj");
        fs::write(
            proj.join("2026_0224_claude_dashhtml001_001.md"),
            "# demo | claude | 2026-02-24\n\n### 2026-02-24 12:00:00 UTC | user\n> hi\n",
        )
        .expect("md");

        let cfg = DashboardConfig {
            store_root: root.clone(),
            title: "AI Context Dashboard".to_string(),
            preview_chars: 100,
        };

        let artifact = build_dashboard(&cfg).expect("dashboard");
        assert!(artifact.html.contains("AI Context Browser"));
        assert!(
            artifact.html.contains("Search -&gt; List -&gt; Content")
                || artifact.html.contains("Search -> List -> Content")
        );
        assert!(artifact.html.contains("ctx-data"));
        assert!(artifact.html.contains("AIContextersDashboard"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn extract_json_search_collects_strings() {
        let value: Value = serde_json::json!({
            "a": "hello",
            "b": ["world", {"c": "notes"}],
            "n": 123
        });

        let mut out = Vec::new();
        let mut chars = 0usize;
        collect_json_strings(&value, &mut out, &mut chars, 50, 1000);
        let joined = out.join(" ");
        assert!(joined.contains("hello"));
        assert!(joined.contains("world"));
        assert!(joined.contains("notes"));
    }

    #[cfg(unix)]
    #[test]
    fn scan_skips_symlinked_files() {
        let root = mk_tmp_dir("ai_ctx_dashboard_symlink_root");
        let proj = root
            .join("store")
            .join("local")
            .join("demo")
            .join("2026_0224")
            .join("conversations")
            .join("codex");
        fs::create_dir_all(&proj).expect("proj");

        let outside = mk_tmp_dir("ai_ctx_dashboard_symlink_outside");
        let outside_file = outside.join("2026_0224_codex_outside001_001.md");
        fs::write(
            &outside_file,
            "outside file that should not be scanned via symlink",
        )
        .expect("outside");

        fs::write(
            proj.join("2026_0224_codex_inside001_001.md"),
            "inside file that should be scanned",
        )
        .expect("inside");

        let symlink_path = proj.join("2026_0224_codex_symlink001_001.md");
        std::os::unix::fs::symlink(&outside_file, &symlink_path).expect("symlink");

        let scan = scan_store(&root, 120).expect("scan");
        assert_eq!(scan.payload.stats.total_files, 1);
        assert!(
            scan.payload
                .records
                .iter()
                .all(|r| r.file_name != "2026_0224_codex_symlink001_001.md")
        );

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
}
