use super::assets::{REPORTS_EXTRACTOR_CSS, REPORTS_EXTRACTOR_SCRIPT};
use super::types::ReportsExplorerPayload;
use anyhow::{Context, Result};

pub(super) fn render_reports_html(payload: &ReportsExplorerPayload, title: &str) -> Result<String> {
    let payload_json =
        serde_json::to_string(payload).context("Failed to serialize reports explorer payload")?;
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
    <header class="hero">
      <div>
        <h1>Workflow Report Explorer</h1>
        <p class="meta">Embedded browse + synthesis + import for Vibecrafted artifacts</p>
        <p class="meta">Repo: {} / {} | Generated {}</p>
      </div>
      <div class="hero-stats">
        <div class="stat-card"><strong>{}</strong><span>records</span></div>
        <div class="stat-card"><strong>{}</strong><span>days</span></div>
        <div class="stat-card"><strong>{}</strong><span>workflows</span></div>
      </div>
    </header>

    <section class="tool-row">
      <div class="search-wrap">
        <input id="rx-search" type="search" placeholder="Search titles, bodies, run IDs, headings…" autocomplete="off" />
      </div>
      <button id="rx-import-trigger" type="button">Import JSON Bundle</button>
      <button id="rx-download-bundle" type="button">Download Current Bundle</button>
      <button id="rx-reset-data" type="button">Reset Embedded Data</button>
      <input id="rx-import-file" type="file" accept=".json,application/json" hidden />
    </section>

    <section class="filters">
      <select id="rx-workflow"><option value="">All workflows</option></select>
      <select id="rx-lane"><option value="">All lanes</option></select>
      <select id="rx-agent"><option value="">All agents</option></select>
      <select id="rx-status"><option value="">All statuses</option></select>
      <select id="rx-day"><option value="">All days</option></select>
    </section>

    <section class="cards" id="rx-cards"></section>

    <section class="layout">
      <aside class="list-pane">
        <div id="rx-summary" class="summary"></div>
        <div id="rx-list" class="result-list"></div>
      </aside>

      <article class="detail-pane">
        <div class="detail-head">
          <div>
            <h2 id="rx-detail-title">Select a record</h2>
            <p id="rx-detail-meta" class="detail-meta"></p>
          </div>
          <button id="rx-copy-path" type="button">Copy Path</button>
        </div>

        <div class="detail-grid" id="rx-detail-grid"></div>
        <div id="rx-detail-headings" class="chip-row"></div>
        <p id="rx-detail-preview" class="detail-preview"></p>
        <pre id="rx-detail-content" class="detail-content">Use search or filters to inspect a workflow artifact.</pre>

        <details class="assumptions" open>
          <summary>Assumptions & provenance</summary>
          <ul id="rx-assumptions"></ul>
        </details>
      </article>
    </section>
  </div>

  <script id="rx-data" type="application/json">{}</script>
  <script>{}</script>
</body>
</html>
"#,
        html_escape(title),
        REPORTS_EXTRACTOR_CSS,
        html_escape(&payload.resolved_org),
        html_escape(&payload.resolved_repo),
        html_escape(&payload.generated_at),
        payload.stats.total_records,
        payload.stats.total_days,
        payload.stats.total_workflows,
        payload_json,
        REPORTS_EXTRACTOR_SCRIPT
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
