//! AI Contexters dashboard generator.
//!
//! Builds a static HTML dashboard for daily browsing of raw extracted notes
//! from the AICX store (`~/.aicx` by default).
//!
//! Layout: Search -> List -> Content
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use std::path::{Path, PathBuf};

mod assets;
mod scan;
#[cfg(test)]
mod tests;

/// Optional dataset scope applied before dashboard generation or server startup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DashboardScope {
    /// Case-insensitive substring match against canonical project/store bucket.
    pub project: Option<String>,
    /// Relative lookback window in hours; `None` means all time.
    pub hours: Option<u64>,
}

impl DashboardScope {
    /// Normalize empty project filters and treat `0` hours as "all time".
    pub fn normalized(&self) -> Self {
        Self {
            project: self
                .project
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            hours: self.hours.filter(|hours| *hours > 0),
        }
    }

    pub fn cutoff_date(&self) -> Option<String> {
        hours_scope_cutoff(self.normalized().hours)
            .map(|cutoff| cutoff.format("%Y-%m-%d").to_string())
    }
}

/// Strict project filter shared with `aicx::store::project_filter_matches`.
///
/// Splits the canonical project slug into `<organization>/<repository>` (or
/// `("", bucket)` when the slug is a single segment) and routes the user
/// filter through the canonical helper. Substring matching is intentionally
/// gone: `-p vista` no longer matches `vista-portal`. An empty / None filter
/// keeps the legacy "no filter applied" behavior.
pub fn project_matches_filter(project: &str, filter: Option<&str>) -> bool {
    let Some(needle) = filter else {
        return true;
    };
    let needle = needle.trim();
    if needle.is_empty() {
        return true;
    }
    let (organization, repository) = project.split_once('/').unwrap_or(("", project));
    crate::store::project_filter_matches(organization, repository, needle)
}

pub fn date_matches_hours_scope(date_iso: &str, hours: Option<u64>) -> bool {
    sort_ts_matches_hours_scope(None, date_iso, hours)
}

pub fn timestamp_matches_hours_scope(
    timestamp: Option<&str>,
    date_iso: &str,
    hours: Option<u64>,
) -> bool {
    timestamp_matches_hours_scope_at(timestamp, date_iso, hours, Utc::now())
}

pub fn sort_ts_matches_hours_scope(
    sort_ts: Option<i64>,
    date_iso: &str,
    hours: Option<u64>,
) -> bool {
    sort_ts_matches_hours_scope_at(sort_ts, date_iso, hours, Utc::now())
}

fn timestamp_matches_hours_scope_at(
    timestamp: Option<&str>,
    date_iso: &str,
    hours: Option<u64>,
    now: DateTime<Utc>,
) -> bool {
    let sort_ts = timestamp
        .and_then(parse_rfc3339_timestamp)
        .map(|parsed| parsed.timestamp());
    sort_ts_matches_hours_scope_at(sort_ts, date_iso, hours, now)
}

fn sort_ts_matches_hours_scope_at(
    sort_ts: Option<i64>,
    date_iso: &str,
    hours: Option<u64>,
    now: DateTime<Utc>,
) -> bool {
    let Some(cutoff) = hours_scope_cutoff_at(now, hours) else {
        return true;
    };

    if let Some(timestamp) = sort_ts.and_then(|ts| Utc.timestamp_opt(ts, 0).single()) {
        return timestamp >= cutoff;
    }

    date_iso >= cutoff.format("%Y-%m-%d").to_string().as_str()
}

fn hours_scope_cutoff(hours: Option<u64>) -> Option<DateTime<Utc>> {
    hours_scope_cutoff_at(Utc::now(), hours)
}

fn hours_scope_cutoff_at(now: DateTime<Utc>, hours: Option<u64>) -> Option<DateTime<Utc>> {
    hours
        .filter(|hours| *hours > 0)
        .map(|hours| now - chrono::Duration::hours(hours as i64))
}

fn parse_rfc3339_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// Configuration for dashboard generation.
#[derive(Debug, Clone)]
pub struct DashboardConfig {
    /// Store root directory (`~/.aicx`).
    pub store_root: PathBuf,
    /// HTML document title.
    pub title: String,
    /// Max characters in per-record preview.
    pub preview_chars: usize,
    /// Optional pre-filter applied before the payload is built.
    pub scope: DashboardScope,
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
pub struct DashboardPayload {
    pub generated_at: String,
    pub store_root: String,
    pub stats: DashboardStats,
    pub assumptions: Vec<String>,
    pub projects: Vec<String>,
    pub agents: Vec<String>,
    pub kinds: Vec<String>,
    pub records: Vec<DashboardRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardRecord {
    pub id: usize,
    pub project: String,
    pub agent: String,
    pub date: String,
    pub time: String,
    pub kind: String,
    pub extension: String,
    pub file_name: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub bytes: u64,
    pub size_human: String,
    pub modified_utc: String,
    pub sort_ts: i64,
    pub entry_count: Option<usize>,
    pub preview: String,
    pub search_blob: String,
    pub detail_text: String,
}

#[derive(Debug, Clone)]
struct ScanResult {
    payload: DashboardPayload,
}

/// Build a complete HTML dashboard from store data.
pub fn build_dashboard(config: &DashboardConfig) -> Result<DashboardArtifact> {
    let scan = scan::scan_store(&config.store_root, config.preview_chars, &config.scope)?;
    let html = render_dashboard_html(&scan.payload, &config.title)?;

    Ok(DashboardArtifact {
        html,
        stats: scan.payload.stats.clone(),
        assumptions: scan.payload.assumptions.clone(),
    })
}

/// Scan the store and return the raw payload (for server mode).
pub fn scan_store_payload(store_root: &Path, preview_chars: usize) -> Result<DashboardPayload> {
    let scan = scan::scan_store(store_root, preview_chars, &DashboardScope::default())?;
    Ok(scan.payload)
}

/// Scan the store with an explicit scope and return the raw payload (for server mode).
pub fn scan_store_payload_scoped(
    store_root: &Path,
    preview_chars: usize,
    scope: &DashboardScope,
) -> Result<DashboardPayload> {
    let scan = scan::scan_store(store_root, preview_chars, scope)?;
    Ok(scan.payload)
}

/// Build a static HTML artifact from an already-scanned payload.
///
/// Reuses `payload` instead of scanning the store again — designed for server
/// mode where `scan_store_payload` has already run.
pub fn build_dashboard_from_payload(
    payload: &DashboardPayload,
    title: &str,
) -> Result<DashboardArtifact> {
    let html = render_dashboard_html(payload, title)?;
    Ok(DashboardArtifact {
        html,
        stats: payload.stats.clone(),
        assumptions: payload.assumptions.clone(),
    })
}

/// Render a lightweight HTML shell for server mode.
///
/// No data is embedded — the JavaScript fetches everything through API endpoints.
/// PWA-ready: includes manifest link and service worker registration.
pub fn render_server_shell_html(title: &str) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <meta name="theme-color" content="#0a0f19" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'; form-action 'none';">
  <link rel="manifest" href="/manifest.webmanifest" />
  <title>{}</title>
  <style>{}
.regen-btn {{ background: var(--panel); border: 1px solid var(--line); color: var(--accent); border-radius: 8px; padding: 4px 10px; font-size: 1.1rem; cursor: pointer; min-width: 36px; }}
.regen-btn:hover {{ background: var(--panel-2); }}
.regen-btn:disabled {{ opacity: 0.5; cursor: wait; }}
.time-row {{ display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }}
.time-btn {{ background: var(--panel); border: 1px solid var(--line); color: var(--muted); border-radius: 8px; padding: 6px 12px; font-size: 0.82rem; cursor: pointer; transition: border-color 0.15s, color 0.15s; }}
.time-btn:hover {{ border-color: var(--accent); color: var(--text); }}
.time-btn.active {{ border-color: var(--accent); color: var(--accent); font-weight: 600; }}
.sort-select {{ background: var(--panel); border: 1px solid var(--line); color: var(--text); border-radius: 8px; padding: 6px 10px; font-size: 0.82rem; }}
.score-group {{ display: flex; align-items: center; gap: 6px; margin-left: auto; }}
.score-group input[type="range"] {{ width: 100px; accent-color: var(--accent); }}
.score-group span {{ color: var(--muted); font-size: 0.82rem; min-width: 28px; }}
.md-rendered {{ font-size: 0.88rem; line-height: 1.55; }}
.md-rendered h1,.md-rendered h2,.md-rendered h3,.md-rendered h4 {{ margin: 0.8em 0 0.3em; color: var(--accent); }}
.md-rendered h1 {{ font-size: 1.2em; }} .md-rendered h2 {{ font-size: 1.1em; }} .md-rendered h3 {{ font-size: 1.0em; }}
.md-rendered pre {{ background: #0b1220; border: 1px solid var(--line); border-radius: 8px; padding: 10px; overflow-x: auto; }}
.md-rendered code {{ background: rgba(56,189,248,0.1); padding: 1px 4px; border-radius: 3px; font-size: 0.9em; }}
.md-rendered pre code {{ background: none; padding: 0; }}
.md-rendered blockquote {{ border-left: 3px solid var(--accent); margin: 0.5em 0; padding: 0.3em 1em; color: var(--muted); }}
.md-rendered ul,.md-rendered ol {{ padding-left: 1.5em; }}
.md-rendered hr {{ border: none; border-top: 1px solid var(--line); margin: 1em 0; }}
.md-rendered a {{ color: var(--accent-2); text-decoration: none; }}
.md-rendered a:hover {{ text-decoration: underline; }}
.detail-actions {{ display: flex; gap: 6px; }}
.detail-actions button {{ border: 1px solid var(--line); border-radius: 8px; background: var(--panel); color: var(--text); padding: 6px 10px; cursor: pointer; font-size: 0.82rem; }}
.detail-actions button:hover {{ border-color: var(--accent); }}
.detail-content {{ margin: 0; border: 0; background: transparent; border-radius: 0; padding: 14px; overflow: auto; flex: 1; min-height: 280px; font-size: 0.86rem; line-height: 1.35; }}
.filter-row {{ display: grid; grid-template-columns: repeat(3, 1fr) auto; gap: 10px; }}
  </style>
</head>
<body>
  <div class="app-shell">
    <header class="app-header">
      <div>
        <h1>aicx</h1>
        <p class="meta">Context Browser | PWA shell</p>
        <p class="meta" id="ctx-gen-info">Loading…</p>
      </div>
      <div class="header-stats">
        <div class="stat"><strong id="ctx-stat-files">-</strong><span>files</span></div>
        <div class="stat"><strong id="ctx-stat-projects">-</strong><span>projects</span></div>
        <div class="stat"><strong id="ctx-stat-days">-</strong><span>days</span></div>
      </div>
    </header>

    <section class="controls">
      <div class="search-row">
        <input id="ctx-search" type="search" placeholder="Fuzzy search… (Enter or pause to trigger)" autocomplete="off" />
        <label class="live-toggle" title="Live search (search while typing)">
          <input id="ctx-live" type="checkbox" /> <span>Live</span>
        </label>
        <button id="ctx-regenerate" type="button" class="regen-btn" title="Regenerate dashboard data">&#8635;</button>
      </div>
      <div class="filter-row">
        <select id="ctx-project"><option value="">All projects</option></select>
        <select id="ctx-agent"><option value="">All agents/sources</option></select>
        <select id="ctx-kind"><option value="">All kinds</option></select>
        <select id="ctx-sort" class="sort-select">
          <option value="newest">Newest</option>
          <option value="oldest">Oldest</option>
          <option value="score">Score</option>
        </select>
      </div>
      <div class="time-row">
        <button class="time-btn" data-since="1h">1h</button>
        <button class="time-btn" data-since="4h">4h</button>
        <button class="time-btn" data-since="24h">24h</button>
        <button class="time-btn" data-since="7d">7d</button>
        <button class="time-btn" data-since="30d">30d</button>
        <button class="time-btn active" data-since="">All</button>
        <div class="score-group">
          <span>Score</span>
          <input type="range" id="ctx-score" min="0" max="100" value="0" />
          <span id="ctx-score-label">0</span>
        </div>
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
          <div class="detail-actions">
            <button id="ctx-expand" type="button" title="Expand full content">Expand</button>
            <button id="ctx-copy-path" type="button">Copy Path</button>
          </div>
        </div>

        <div id="ctx-detail-content" class="detail-content">Use search or filters to pick a note.</div>

        <details class="assumptions">
          <summary>Assumptions</summary>
          <ul id="ctx-assumptions"></ul>
        </details>
      </article>
    </section>
  </div>

  <script>{}</script>
  <script>if('serviceWorker' in navigator)navigator.serviceWorker.register('/service-worker.js');</script>
</body>
</html>
"##,
        html_escape(title),
        assets::DASHBOARD_CSS,
        format_args!(
            "{}\n{}",
            assets::DASHBOARD_INLINE_MARKDOWN_SCRIPT,
            assets::DASHBOARD_SERVER_SCRIPT
        )
    )
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
        assets::DASHBOARD_CSS,
        html_escape(&payload.store_root),
        html_escape(&payload.generated_at),
        payload.stats.total_files,
        payload.stats.total_projects,
        payload.stats.total_days,
        payload_json,
        assets::DASHBOARD_SCRIPT
    ))
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
