//! Dashboard module regression tests.

use super::scan::{
    classify_extension_kind_ref, collect_json_strings, extract_latest_timestamp_from_json,
    extract_latest_timestamp_from_text, scan_store,
};
use super::*;
use chrono::{TimeZone, Utc};
use regex::Regex;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

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

fn mk_tmp_dir(name: &str) -> PathBuf {
    let dir = std::env::current_dir()
        .expect("cwd")
        .join("target")
        .join("test-tmp")
        .join(format!("{}_{}", name, Utc::now().timestamp_micros()));
    fs::create_dir_all(&dir).expect("create dir");
    dir
}

/// Run the inline-markdown JS module via `node`. Returns `None` if Node.js
/// is not on PATH (gracefully skip — `cargo test` stays runnable on
/// minimal Rust-only environments). Caller short-circuits the test.
fn inline_markdown_via_node(markdown: &str) -> Option<String> {
    let module_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/dashboard_inline_markdown.js"
    );
    let output = Command::new("node")
        .arg("-e")
        .arg(
            "const { pathToFileURL } = require('url'); (async () => { await import(pathToFileURL(process.argv[1])); const md = globalThis.AicxMarkdown; process.stdout.write(md.inlineMarkdown(process.argv[2])); })().catch((err) => { console.error(err); process.exit(1); });",
        )
        .arg(module_path)
        .arg(markdown)
        .output()
        .ok()?;
    assert!(
        output.status.success(),
        "node inlineMarkdown failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8(output.stdout).expect("node output is utf8"))
}

/// Helper: emit a skip notice + return early when `node` isn't installed.
macro_rules! skip_if_no_node {
    ($call:expr) => {
        match $call {
            Some(v) => v,
            None => {
                eprintln!("[skip] dashboard inline-markdown behavior test: `node` not on PATH");
                return;
            }
        }
    };
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

    let scan = scan_store(&root, 120, &DashboardScope::default()).expect("scan");
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
        scope: DashboardScope::default(),
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
fn server_shell_includes_highlight_styles_and_wiring() {
    let html = render_server_shell_html("AI Context Browser");

    assert!(html.contains("mark.hl"));
    assert!(html.contains("mark.hl-fuzzy"));
    assert!(html.contains("const escapeRegex = (s) =>"));
    assert!(html.contains("const highlightTerms = (text, query) => {"));
    assert!(html.contains("ui.detailTitle.innerHTML = highlightTerms(title, state.query);"));
    assert!(html.contains("ui.detailMeta.innerHTML = highlightTerms(meta, state.query);"));
    assert!(html.contains("n.innerHTML = highlightTerms(String(txt || ''), state.query);"));
    assert!(html.contains("name.innerHTML = highlightTerms(fname, state.query);"));
    assert!(html.contains(".result-preview {"));
    assert!(html.contains("preview.className = 'result-preview';"));
    assert!(html.contains("preview.innerHTML = highlightTerms(truncated, state.query);"));
}

#[test]
fn static_dashboard_includes_highlight_styles_and_wiring() {
    let payload = DashboardPayload {
        generated_at: "2026-04-02T17:43:00Z".to_string(),
        store_root: "/tmp/aicx".to_string(),
        records: Vec::new(),
        stats: DashboardStats::default(),
        assumptions: Vec::new(),
        projects: Vec::new(),
        agents: Vec::new(),
        kinds: Vec::new(),
    };

    let html = render_dashboard_html(&payload, "AI Context Browser").expect("static html");

    assert!(html.contains("mark.hl"));
    assert!(html.contains("mark.hl-fuzzy"));
    assert!(html.contains("const escapeRegex = (s) =>"));
    assert!(html.contains("const highlightTerms = (text, query) => {"));
    assert!(html.contains("queryRaw: ''"));
    assert!(html.contains("const highlightQuery = () => state.queryRaw || state.query;"));
    assert!(
        html.contains("ui.detailTitle.innerHTML = highlightTerms(detailTitle, highlightQuery());")
    );
    assert!(
        html.contains("ui.detailMeta.innerHTML = highlightTerms(detailMeta, highlightQuery());")
    );
    assert!(
        html.contains("ui.detailPath.innerHTML = highlightTerms(detailPath, highlightQuery());")
    );
    assert!(html.contains("node.innerHTML = highlightTerms(String(txt || ''), highlightQuery());"));
    assert!(html.contains("name.innerHTML = highlightTerms(nameText, highlightQuery());"));
    assert!(html.contains(".result-preview {"));
    assert!(html.contains("preview.className = 'result-preview';"));
    assert!(html.contains("preview.innerHTML = highlightTerms(record.preview, highlightQuery());"));
    assert!(html.contains("state.queryRaw = ui.search.value || '';"));
}

#[test]
fn static_dashboard_includes_polish_normalization_map_for_l_stroke() {
    let payload = DashboardPayload {
        generated_at: "2026-04-02T17:43:00Z".to_string(),
        store_root: "/tmp/aicx".to_string(),
        records: Vec::new(),
        stats: DashboardStats::default(),
        assumptions: Vec::new(),
        projects: Vec::new(),
        agents: Vec::new(),
        kinds: Vec::new(),
    };

    let html = render_dashboard_html(&payload, "AI Context Browser").expect("static html");

    assert!(html.contains("const normalizeText = (text) => {"));
    assert!(html.contains("'\\u0141':'L','\\u0142':'l'"));
    assert!(html.contains("normalizeText(value)"));
    assert!(html.contains("const normalizedText = normalizeText(text);"));
    assert!(html.contains("terms.map(normalizeText).filter(Boolean).forEach((term) => {"));
    assert!(!html.contains("const normalizedText = normalize(text);"));
}

#[test]
fn server_shell_includes_polish_normalization_map_for_l_stroke() {
    let html = render_server_shell_html("AI Context Browser");

    assert!(html.contains("const normalizeText = (text) => {"));
    assert!(html.contains("'\\u0141':'L','\\u0142':'l'"));
    assert!(html.contains("const normalizedText = normalizeText(text);"));
    assert!(html.contains("terms.map(normalizeText).filter(Boolean).forEach(function(term) {"));
    assert!(!html.contains("const normalizedText = normalize(text);"));
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

    let scan = scan_store(&root, 120, &DashboardScope::default()).expect("scan");
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

#[test]
fn scan_store_scope_filters_by_project_and_hours() {
    let root = mk_tmp_dir("ai_ctx_dashboard_scope");
    let recent = Utc::now() - chrono::Duration::hours(1);
    let alpha_date = recent.format("%Y_%m%d").to_string();
    let alpha_timestamp = recent.format("%Y-%m-%d %H:%M:%S").to_string();
    let stale = Utc::now() - chrono::Duration::days(30);
    let beta_date = stale.format("%Y_%m%d").to_string();
    let beta_timestamp = stale.format("%Y-%m-%d %H:%M:%S").to_string();

    let alpha = root
        .join("store")
        .join("local")
        .join("alpha-project")
        .join(&alpha_date)
        .join("conversations")
        .join("codex");
    fs::create_dir_all(&alpha).expect("alpha dirs");
    fs::write(
        alpha.join(format!("{alpha_date}_codex_scopealpha001_001.md")),
        format!("# alpha\n\n### {alpha_timestamp} UTC | user\n> alpha kept\n"),
    )
    .expect("alpha file");

    let beta = root
        .join("store")
        .join("local")
        .join("beta-project")
        .join(&beta_date)
        .join("conversations")
        .join("claude");
    fs::create_dir_all(&beta).expect("beta dirs");
    fs::write(
        beta.join(format!("{beta_date}_claude_scopebeta001_001.md")),
        format!("# beta\n\n### {beta_timestamp} UTC | user\n> beta excluded\n"),
    )
    .expect("beta file");

    // Bug #27/#28 regression: the startup scope filter is now strict
    // (routes through `aicx::store::project_filter_matches`). The old
    // assertion used `Some("alpha")` and relied on substring matching
    // against canonical slug `local/alpha-project` — that was the
    // very leak the strict filter is designed to kill. The strict
    // matcher accepts `alpha-project` (cross-org repo-name match),
    // `local/alpha-project` (exact slug), `local/` (org wildcard),
    // or `/alpha-project` (repo wildcard).
    let scoped = scan_store(
        &root,
        120,
        &DashboardScope {
            project: Some("alpha-project".to_string()),
            hours: Some(72),
        },
    )
    .expect("scoped scan");

    assert_eq!(scoped.payload.records.len(), 1);
    assert_eq!(scoped.payload.records[0].project, "local/alpha-project");
    assert!(
        scoped
            .payload
            .assumptions
            .iter()
            .any(|line| line.contains("project/store buckets containing: alpha-project"))
    );
    assert!(
        scoped
            .payload
            .assumptions
            .iter()
            .any(|line| line.contains("last 72 hour(s)"))
    );

    // Bug #27 positive guard: the substring-only filter `alpha` MUST
    // NOT match canonical `local/alpha-project` under strict
    // semantics. The dashboard layer used to silently leak this.
    let substring_leak = scan_store(
        &root,
        120,
        &DashboardScope {
            project: Some("alpha".to_string()),
            hours: Some(72),
        },
    )
    .expect("substring-leak scoped scan");
    assert!(
        substring_leak.payload.records.is_empty(),
        "strict filter must NOT match `alpha` against `local/alpha-project`; got {} records",
        substring_leak.payload.records.len()
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn hours_scope_uses_precise_same_day_timestamps_when_available() {
    let now = Utc.with_ymd_and_hms(2026, 4, 17, 12, 0, 0).unwrap();
    let recent = Utc.with_ymd_and_hms(2026, 4, 17, 11, 30, 0).unwrap();
    let stale_same_day = Utc.with_ymd_and_hms(2026, 4, 17, 7, 0, 0).unwrap();

    assert!(sort_ts_matches_hours_scope_at(
        Some(recent.timestamp()),
        "2026-04-17",
        Some(2),
        now,
    ));
    assert!(!sort_ts_matches_hours_scope_at(
        Some(stale_same_day.timestamp()),
        "2026-04-17",
        Some(2),
        now,
    ));
    assert!(!timestamp_matches_hours_scope_at(
        Some("2026-04-17T07:00:00Z"),
        "2026-04-17",
        Some(2),
        now,
    ));
}

#[test]
fn extract_latest_timestamp_helpers_prefer_newest_event_time() {
    let json_ts = Utc
        .with_ymd_and_hms(2026, 4, 17, 9, 0, 0)
        .unwrap()
        .timestamp();
    let markdown = "\
# sample

### 2026-04-17 09:15:00 UTC | user
> hello

### 2026-04-17 11:45:00 UTC | assistant
> updated
";
    assert_eq!(
        extract_latest_timestamp_from_text(markdown),
        Some(
            Utc.with_ymd_and_hms(2026, 4, 17, 11, 45, 0)
                .unwrap()
                .timestamp()
        )
    );

    let value = serde_json::json!({
        "items": [
            {"timestamp": "2026-04-17T08:00:00Z"},
            {"ts": json_ts}
        ],
        "completed_at": "2026-04-17T10:30:00Z"
    });
    assert_eq!(
        extract_latest_timestamp_from_json(&value),
        Some(
            Utc.with_ymd_and_hms(2026, 4, 17, 10, 30, 0)
                .unwrap()
                .timestamp()
        )
    );
}

#[test]
fn test_inline_markdown_javascript_scheme_renders_as_text() {
    let html = skip_if_no_node!(inline_markdown_via_node("[x](javascript:alert(1))"));
    assert_eq!(html, "[x](javascript:alert(1))");
    assert!(!html.contains("<a "));
}

#[test]
fn test_inline_markdown_data_scheme_renders_as_text() {
    let html = skip_if_no_node!(inline_markdown_via_node("[x](data:text/html,boom)"));
    assert_eq!(html, "[x](data:text/html,boom)");
    assert!(!html.contains("<a "));
}

#[test]
fn test_inline_markdown_quote_break_attempt_does_not_inject_attribute() {
    let html = skip_if_no_node!(inline_markdown_via_node(
        "[x](https://example.com/\" onclick=\"alert(1))"
    ));
    assert!(html.contains("<a href=\"https://example.com/&quot; onclick=&quot;alert(1\""));
    assert!(!html.contains("\" onclick=\""));
}

#[test]
fn test_render_server_shell_html_contains_csp_meta() {
    let html = render_server_shell_html("test");
    assert!(html.contains("<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'; form-action 'none';\">"));
}
