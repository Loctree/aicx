//! AI Contexters dashboard HTTP server runtime.
//!
//! Serves the generated dashboard artifact and supports on-demand regeneration.

use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    extract::{Query, State, rejection::QueryRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::RwLock;

use crate::dashboard::{self, DashboardConfig, DashboardStats};
use crate::rank;
use crate::store;

/// Guard that prevents concurrent `aicx store` child-process spawns from dashboard search.
static DASHBOARD_RESCAN_RUNNING: AtomicBool = AtomicBool::new(false);

const REGENERATE_HEADER_NAME: &str = "x-ai-contexters-action";
const REGENERATE_HEADER_VALUE: &str = "regenerate";

/// Runtime configuration for dashboard server mode.
#[derive(Debug, Clone)]
pub struct DashboardServerConfig {
    pub store_root: PathBuf,
    pub title: String,
    pub preview_chars: usize,
    pub artifact_path: PathBuf,
    pub host: IpAddr,
    pub port: u16,
}

#[derive(Debug, Clone)]
struct DashboardSnapshot {
    html: String,
    generated_at: DateTime<Utc>,
    stats: DashboardStats,
    assumptions: Vec<String>,
    build_count: u64,
    last_error: Option<String>,
}

impl DashboardSnapshot {
    fn from_build(build: BuildOutput) -> Self {
        Self {
            html: build.artifact.html,
            generated_at: build.generated_at,
            stats: build.artifact.stats,
            assumptions: build.artifact.assumptions,
            build_count: 1,
            last_error: None,
        }
    }
}

#[derive(Debug)]
struct DashboardServerState {
    config: DashboardServerConfig,
    snapshot: RwLock<DashboardSnapshot>,
    rebuilding: AtomicBool,
}

#[derive(Debug)]
struct BuildOutput {
    artifact: dashboard::DashboardArtifact,
    generated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct DashboardStatusResponse {
    ok: bool,
    rebuilding: bool,
    generated_at: String,
    build_count: u64,
    store_root: String,
    artifact_path: String,
    title: String,
    preview_chars: usize,
    stats: DashboardStats,
    assumptions: Vec<String>,
    last_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct DashboardRegenerateResponse {
    ok: bool,
    regenerated_at: String,
    build_count: u64,
    artifact_path: String,
    stats: DashboardStats,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    ok: bool,
    error: String,
}

// ============================================================================
// Search types
// ============================================================================

#[derive(Debug, Deserialize)]
struct FuzzySearchParams {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
    /// Optional project filter (case-insensitive substring match)
    project: Option<String>,
}

fn default_search_limit() -> usize {
    20
}

#[derive(Debug, Deserialize)]
struct SemanticSearchParams {
    q: String,
    #[serde(default = "default_namespace")]
    ns: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
    #[serde(default = "default_search_mode")]
    mode: String,
}

fn default_namespace() -> String {
    "ai-contexts".to_string()
}

fn default_search_mode() -> String {
    "hybrid".to_string()
}

#[derive(Debug, Deserialize)]
struct CrossSearchParams {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
    #[serde(default = "default_search_mode")]
    mode: String,
}

#[derive(Debug, Serialize)]
struct FuzzySearchResult {
    file: String,
    path: String,
    project: String,
    kind: String,
    agent: String,
    date: String,
    score: u8,
    label: String,
    signal_density: f32,
    matched_lines: Vec<String>,
    excerpt: String,
}

#[derive(Debug, Serialize)]
struct FuzzySearchResponse {
    ok: bool,
    query: String,
    results: Vec<FuzzySearchResult>,
    total_scanned: usize,
}

#[derive(Debug, Serialize)]
struct MemexSearchResponse {
    ok: bool,
    query: String,
    source: String,
    results: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct SteerSearchParams {
    /// Filter by run_id (exact)
    run_id: Option<String>,
    /// Filter by prompt_id (exact)
    prompt_id: Option<String>,
    /// Filter by agent name
    agent: Option<String>,
    /// Filter by kind
    kind: Option<String>,
    /// Filter by project (case-insensitive substring)
    project: Option<String>,
    /// Filter by date (YYYY-MM-DD or range)
    date: Option<String>,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

#[derive(Debug, Serialize)]
struct SteerSearchResult {
    path: String,
    project: String,
    agent: String,
    kind: String,
    date: String,
    session_id: String,
    run_id: Option<String>,
    prompt_id: Option<String>,
    agent_model: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    token_usage: Option<u64>,
    findings_count: Option<u32>,
}

#[derive(Debug, Serialize)]
struct SteerSearchResponse {
    ok: bool,
    scanned: usize,
    matched: usize,
    items: Vec<SteerSearchResult>,
}

struct RebuildFlagGuard<'a> {
    flag: &'a AtomicBool,
}

impl<'a> RebuildFlagGuard<'a> {
    fn new(flag: &'a AtomicBool) -> Self {
        Self { flag }
    }
}

impl Drop for RebuildFlagGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

/// Run dashboard server and block until process is terminated.
pub async fn run_dashboard_server(config: DashboardServerConfig) -> Result<()> {
    ensure_loopback_host(config.host)?;

    let initial = rebuild_dashboard(&config).context("Initial dashboard build failed")?;
    let state = Arc::new(DashboardServerState {
        config: config.clone(),
        snapshot: RwLock::new(DashboardSnapshot::from_build(initial)),
        rebuilding: AtomicBool::new(false),
    });

    let app = Router::new()
        .route("/", get(get_dashboard_html))
        .route("/api/health", get(get_health))
        .route("/health", get(get_health))
        .route("/api/status", get(get_status))
        .route("/api/regenerate", post(regenerate_dashboard))
        .route("/api/search/fuzzy", get(fuzzy_search))
        .route("/api/search/semantic", get(semantic_search))
        .route("/api/search/cross", get(cross_search))
        .route("/api/search/steer", get(steer_search))
        .with_state(state);

    let addr = SocketAddr::new(config.host, config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind dashboard server on http://{addr}"))?;

    eprintln!("✓ Dashboard server started");
    eprintln!("  URL: http://{addr}");
    eprintln!("  Status:    GET  http://{addr}/api/status");
    eprintln!("  Regenerate: POST http://{addr}/api/regenerate");
    eprintln!("  Fuzzy:     GET  http://{addr}/api/search/fuzzy?q=<query>");
    eprintln!("  Semantic:  GET  http://{addr}/api/search/semantic?q=<query>&ns=<namespace>");
    eprintln!("  Cross:     GET  http://{addr}/api/search/cross?q=<query>");
    eprintln!("  Steer:     GET  http://{addr}/api/search/steer?run_id=<id>&project=<p>");
    eprintln!(
        "  Required header: {}: {}",
        REGENERATE_HEADER_NAME, REGENERATE_HEADER_VALUE
    );
    eprintln!("  Artifact: {}", config.artifact_path.display());
    eprintln!("  Store: {}", config.store_root.display());

    axum::serve(listener, app)
        .await
        .context("Dashboard server runtime terminated unexpectedly")
}

fn ensure_loopback_host(host: IpAddr) -> Result<()> {
    if host.is_loopback() {
        Ok(())
    } else {
        Err(anyhow!(
            "Refusing to bind dashboard server to non-loopback address '{}'. Use --host 127.0.0.1 or ::1.",
            host
        ))
    }
}

async fn get_dashboard_html(State(state): State<Arc<DashboardServerState>>) -> impl IntoResponse {
    let snapshot = state.snapshot.read().await;
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (headers, Html(snapshot.html.clone()))
}

async fn get_health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "ok": true,
        "service": "aicx-dashboard",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn get_status(
    State(state): State<Arc<DashboardServerState>>,
) -> Json<DashboardStatusResponse> {
    let snapshot = state.snapshot.read().await;
    Json(DashboardStatusResponse {
        ok: true,
        rebuilding: state.rebuilding.load(Ordering::SeqCst),
        generated_at: snapshot.generated_at.to_rfc3339(),
        build_count: snapshot.build_count,
        store_root: state.config.store_root.display().to_string(),
        artifact_path: state.config.artifact_path.display().to_string(),
        title: state.config.title.clone(),
        preview_chars: state.config.preview_chars,
        stats: snapshot.stats.clone(),
        assumptions: snapshot.assumptions.clone(),
        last_error: snapshot.last_error.clone(),
    })
}

async fn regenerate_dashboard(
    State(state): State<Arc<DashboardServerState>>,
    headers: HeaderMap,
) -> Response {
    let header_ok = headers
        .get(REGENERATE_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case(REGENERATE_HEADER_VALUE));

    if !header_ok {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                ok: false,
                error: format!(
                    "Missing required header: {}: {}",
                    REGENERATE_HEADER_NAME, REGENERATE_HEADER_VALUE
                ),
            }),
        )
            .into_response();
    }

    if state.rebuilding.swap(true, Ordering::SeqCst) {
        return (
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                ok: false,
                error: "Dashboard regeneration already in progress.".to_string(),
            }),
        )
            .into_response();
    }
    let _flag_guard = RebuildFlagGuard::new(&state.rebuilding);

    let config = state.config.clone();
    let rebuilt = tokio::task::spawn_blocking(move || rebuild_dashboard(&config)).await;

    match rebuilt {
        Ok(Ok(build)) => {
            let mut snapshot = state.snapshot.write().await;
            snapshot.html = build.artifact.html;
            snapshot.generated_at = build.generated_at;
            snapshot.stats = build.artifact.stats.clone();
            snapshot.assumptions = build.artifact.assumptions;
            snapshot.build_count = snapshot.build_count.saturating_add(1);
            snapshot.last_error = None;

            let response = DashboardRegenerateResponse {
                ok: true,
                regenerated_at: snapshot.generated_at.to_rfc3339(),
                build_count: snapshot.build_count,
                artifact_path: state.config.artifact_path.display().to_string(),
                stats: snapshot.stats.clone(),
            };

            (StatusCode::OK, Json(response)).into_response()
        }
        Ok(Err(err)) => {
            let err_msg = format!("{err:#}");
            let mut snapshot = state.snapshot.write().await;
            snapshot.last_error = Some(err_msg.clone());

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    ok: false,
                    error: err_msg,
                }),
            )
                .into_response()
        }
        Err(err) => {
            let err_msg = format!("Regeneration task join failure: {err}");
            let mut snapshot = state.snapshot.write().await;
            snapshot.last_error = Some(err_msg.clone());

            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    ok: false,
                    error: err_msg,
                }),
            )
                .into_response()
        }
    }
}

// ============================================================================
// Search handlers
// ============================================================================

/// Fuzzy text search across all stored chunks.
///
/// Reads chunk files from the store, matches lines against the query,
/// and scores each match using the rank module.
async fn fuzzy_search(
    State(state): State<Arc<DashboardServerState>>,
    params: Result<Query<FuzzySearchParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(q) => q,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    ok: false,
                    error: format!("Invalid query parameters: {rejection}"),
                }),
            )
                .into_response();
        }
    };

    let query = params.q.trim().to_string();
    if query.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                ok: false,
                error: "Query parameter 'q' is required".to_string(),
            }),
        )
            .into_response();
    }

    let limit = params.limit.min(100);
    let store_root = state.config.store_root.clone();
    let project_filter = params.project;
    let query_clone = query.clone();

    let result = tokio::task::spawn_blocking(move || {
        run_fuzzy_search(&store_root, &query_clone, limit, project_filter.as_deref())
    })
    .await;

    match result {
        Ok(Ok((results, total_scanned))) => (
            StatusCode::OK,
            Json(FuzzySearchResponse {
                ok: true,
                query,
                results,
                total_scanned,
            }),
        )
            .into_response(),
        Ok(Err(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("{err:#}"),
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("Search task failed: {err}"),
            }),
        )
            .into_response(),
    }
}

fn run_fuzzy_search(
    store_root: &Path,
    query: &str,
    limit: usize,
    project_filter: Option<&str>,
) -> Result<(Vec<FuzzySearchResult>, usize)> {
    // Non-blocking auto-rescan with rate-limit guard.
    if !DASHBOARD_RESCAN_RUNNING.swap(true, Ordering::SeqCst) {
        match std::process::Command::new("aicx")
            .args(["store", "-H", "24", "--incremental"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => {
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(30));
                    DASHBOARD_RESCAN_RUNNING.store(false, Ordering::SeqCst);
                });
            }
            Err(_) => {
                DASHBOARD_RESCAN_RUNNING.store(false, Ordering::SeqCst);
            }
        }
    }

    let (results, total_scanned) =
        rank::fuzzy_search_store(store_root, query, limit, project_filter)?;
    let results = results
        .into_iter()
        .map(|result| {
            let excerpt = result.matched_lines.join(" ... ");
            FuzzySearchResult {
                file: result.file,
                path: result.path,
                project: result.project,
                kind: result.kind,
                agent: result.agent,
                date: result.date,
                score: result.score,
                label: result.label,
                signal_density: result.density,
                matched_lines: result.matched_lines,
                excerpt,
            }
        })
        .collect();

    Ok((results, total_scanned))
}

/// Semantic search via rmcp-memex vector DB.
///
/// Shells out to `rmcp-memex search --json` for vector similarity.
async fn semantic_search(params: Result<Query<SemanticSearchParams>, QueryRejection>) -> Response {
    let Query(params) = match params {
        Ok(q) => q,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    ok: false,
                    error: format!("Invalid query parameters: {rejection}"),
                }),
            )
                .into_response();
        }
    };

    let query = params.q.trim().to_string();
    if query.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                ok: false,
                error: "Query parameter 'q' is required".to_string(),
            }),
        )
            .into_response();
    }

    let ns = params.ns;
    let limit = params.limit;
    let mode = params.mode;

    let result =
        tokio::task::spawn_blocking(move || run_memex_search(&query, &ns, limit, &mode)).await;

    match result {
        Ok(Ok(response)) => (StatusCode::OK, Json(response)).into_response(),
        Ok(Err(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("{err:#}"),
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("Search task failed: {err}"),
            }),
        )
            .into_response(),
    }
}

fn run_memex_search(
    query: &str,
    namespace: &str,
    limit: usize,
    mode: &str,
) -> Result<MemexSearchResponse> {
    let output = Command::new("rmcp-memex")
        .arg("search")
        .arg("-n")
        .arg(namespace)
        .arg("-q")
        .arg(query)
        .arg("-l")
        .arg(limit.to_string())
        .arg("-m")
        .arg(mode)
        .arg("--json")
        .output()
        .context("Failed to run rmcp-memex search. Is rmcp-memex installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("rmcp-memex search failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let results: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or(serde_json::Value::String(stdout.to_string()));

    Ok(MemexSearchResponse {
        ok: true,
        query: query.to_string(),
        source: format!("rmcp-memex search -n {} --mode {}", namespace, mode),
        results,
    })
}

/// Cross-namespace semantic search via rmcp-memex.
///
/// Searches all namespaces at once, merging and ranking results.
async fn cross_search(params: Result<Query<CrossSearchParams>, QueryRejection>) -> Response {
    let Query(params) = match params {
        Ok(q) => q,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    ok: false,
                    error: format!("Invalid query parameters: {rejection}"),
                }),
            )
                .into_response();
        }
    };
    let query = params.q.trim().to_string();
    if query.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                ok: false,
                error: "Query parameter 'q' is required".to_string(),
            }),
        )
            .into_response();
    }

    let limit = params.limit;
    let mode = params.mode;

    let result =
        tokio::task::spawn_blocking(move || run_memex_cross_search(&query, limit, &mode)).await;

    match result {
        Ok(Ok(response)) => (StatusCode::OK, Json(response)).into_response(),
        Ok(Err(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("{err:#}"),
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("Search task failed: {err}"),
            }),
        )
            .into_response(),
    }
}

fn run_memex_cross_search(query: &str, limit: usize, mode: &str) -> Result<MemexSearchResponse> {
    let output = Command::new("rmcp-memex")
        .arg("cross-search")
        .arg(query)
        .arg("--limit")
        .arg(limit.to_string())
        .arg("--mode")
        .arg(mode)
        .arg("--json")
        .output()
        .context("Failed to run rmcp-memex cross-search. Is rmcp-memex installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("rmcp-memex cross-search failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let results: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or(serde_json::Value::String(stdout.to_string()));

    Ok(MemexSearchResponse {
        ok: true,
        query: query.to_string(),
        source: format!("rmcp-memex cross-search --mode {}", mode),
        results,
    })
}

/// Steering-metadata search across stored chunks.
///
/// Filters by sidecar metadata (run_id, prompt_id, agent, kind, project, date)
/// using canonical chunk dates instead of filesystem mtime.
async fn steer_search(params: Result<Query<SteerSearchParams>, QueryRejection>) -> Response {
    let Query(params) = match params {
        Ok(q) => q,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    ok: false,
                    error: format!("Invalid query parameters: {rejection}"),
                }),
            )
                .into_response();
        }
    };

    let limit = params.limit.min(100);

    let result = tokio::task::spawn_blocking(move || run_steer_search(params, limit)).await;

    match result {
        Ok(Ok(response)) => (StatusCode::OK, Json(response)).into_response(),
        Ok(Err(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("{err:#}"),
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("Steer search task failed: {err}"),
            }),
        )
            .into_response(),
    }
}

fn parse_date_bounds(date: &str) -> (Option<String>, Option<String>) {
    if let Some((lo, hi)) = date.split_once("..") {
        let lo = if lo.is_empty() {
            None
        } else {
            Some(lo.to_string())
        };
        let hi = if hi.is_empty() {
            None
        } else {
            Some(hi.to_string())
        };
        (lo, hi)
    } else {
        (Some(date.to_string()), Some(date.to_string()))
    }
}

fn run_steer_search(params: SteerSearchParams, limit: usize) -> Result<SteerSearchResponse> {
    let files = store::scan_context_files()?;
    let total = files.len();

    let (date_lo, date_hi) = if let Some(ref d) = params.date {
        parse_date_bounds(d)
    } else {
        (None, None)
    };

    let project_lower = params.project.as_deref().map(str::to_ascii_lowercase);
    let agent_lower = params.agent.as_deref().map(str::to_ascii_lowercase);
    let kind_lower = params.kind.as_deref().map(str::to_ascii_lowercase);

    let mut items = Vec::new();

    for file in &files {
        if items.len() >= limit {
            break;
        }

        if let Some(ref needle) = project_lower {
            if !file.project.to_ascii_lowercase().contains(needle) {
                continue;
            }
        }
        if let Some(ref needle) = agent_lower {
            if file.agent.to_ascii_lowercase() != *needle {
                continue;
            }
        }
        if let Some(ref needle) = kind_lower {
            if file.kind.dir_name() != *needle {
                continue;
            }
        }
        if let Some(ref lo) = date_lo {
            if file.date_iso.as_str() < lo.as_str() {
                continue;
            }
        }
        if let Some(ref hi) = date_hi {
            if file.date_iso.as_str() > hi.as_str() {
                continue;
            }
        }

        if params.run_id.is_some() || params.prompt_id.is_some() {
            let sidecar = store::load_sidecar(&file.path);
            if let Some(ref sc) = sidecar {
                if let Some(ref wanted) = params.run_id {
                    if sc.run_id.as_deref() != Some(wanted.as_str()) {
                        continue;
                    }
                }
                if let Some(ref wanted) = params.prompt_id {
                    if sc.prompt_id.as_deref() != Some(wanted.as_str()) {
                        continue;
                    }
                }
                items.push(SteerSearchResult {
                    path: file.path.display().to_string(),
                    project: file.project.clone(),
                    agent: file.agent.clone(),
                    kind: file.kind.dir_name().to_string(),
                    date: file.date_iso.clone(),
                    session_id: file.session_id.clone(),
                    run_id: sc.run_id.clone(),
                    prompt_id: sc.prompt_id.clone(),
                    agent_model: sc.agent_model.clone(),
                    started_at: sc.started_at.clone(),
                    completed_at: sc.completed_at.clone(),
                    token_usage: sc.token_usage,
                    findings_count: sc.findings_count,
                });
            }
        } else {
            let sidecar = store::load_sidecar(&file.path);
            items.push(SteerSearchResult {
                path: file.path.display().to_string(),
                project: file.project.clone(),
                agent: file.agent.clone(),
                kind: file.kind.dir_name().to_string(),
                date: file.date_iso.clone(),
                session_id: file.session_id.clone(),
                run_id: sidecar.as_ref().and_then(|s| s.run_id.clone()),
                prompt_id: sidecar.as_ref().and_then(|s| s.prompt_id.clone()),
                agent_model: sidecar.as_ref().and_then(|s| s.agent_model.clone()),
                started_at: sidecar.as_ref().and_then(|s| s.started_at.clone()),
                completed_at: sidecar.as_ref().and_then(|s| s.completed_at.clone()),
                token_usage: sidecar.as_ref().and_then(|s| s.token_usage),
                findings_count: sidecar.as_ref().and_then(|s| s.findings_count),
            });
        }
    }

    Ok(SteerSearchResponse {
        ok: true,
        scanned: total,
        matched: items.len(),
        items,
    })
}

fn rebuild_dashboard(config: &DashboardServerConfig) -> Result<BuildOutput> {
    let artifact = dashboard::build_dashboard(&DashboardConfig {
        store_root: config.store_root.clone(),
        title: config.title.clone(),
        preview_chars: config.preview_chars,
    })?;
    write_dashboard_artifact(&config.artifact_path, &artifact.html)?;

    Ok(BuildOutput {
        artifact,
        generated_at: Utc::now(),
    })
}

fn write_dashboard_artifact(path: &Path, html: &str) -> Result<()> {
    let mut output_path = crate::sanitize::validate_write_path(path)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output directory: {}", parent.display()))?;
    }
    output_path = crate::sanitize::validate_write_path(&output_path)?;

    let base_name = output_path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("dashboard-artifact");

    let mut temp_slot = None;
    for attempt in 0..32u32 {
        let stamp = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let tmp_path = output_path.with_file_name(format!(
            ".{}.{}.{}.tmp",
            base_name,
            std::process::id(),
            stamp.saturating_add(i64::from(attempt))
        ));

        crate::sanitize::validate_write_path(&tmp_path).with_context(|| {
            format!(
                "Temporary artifact path failed validation: {}",
                tmp_path.display()
            )
        })?;

        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(file) => {
                temp_slot = Some((tmp_path, file));
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "Failed to create temporary artifact: {}",
                        tmp_path.display()
                    )
                });
            }
        }
    }

    let (tmp_path, mut tmp_file) =
        temp_slot.ok_or_else(|| anyhow!("Failed to allocate unique temporary artifact path"))?;

    tmp_file
        .write_all(html.as_bytes())
        .with_context(|| format!("Failed to write temporary artifact: {}", tmp_path.display()))?;
    tmp_file
        .sync_all()
        .with_context(|| format!("Failed to sync temporary artifact: {}", tmp_path.display()))?;
    drop(tmp_file);

    if let Err(rename_err) = fs::rename(&tmp_path, &output_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(rename_err).with_context(|| {
            format!(
                "Failed to atomically replace dashboard artifact: {}",
                output_path.display()
            )
        });
    }

    Ok(())
}

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

    fn seed_store(root: &Path) {
        let p = root.join("demo").join("2026-02-24");
        fs::create_dir_all(&p).expect("create store dirs");
        fs::write(
            p.join("120000_codex-context.md"),
            "# demo\n\n### 2026-02-24 12:00:00 UTC | user\n> hello",
        )
        .expect("seed file");
    }

    fn mk_state(root: PathBuf, artifact_path: PathBuf) -> Arc<DashboardServerState> {
        Arc::new(DashboardServerState {
            config: DashboardServerConfig {
                store_root: root,
                title: "test".to_string(),
                preview_chars: 120,
                artifact_path,
                host: "127.0.0.1".parse().expect("host"),
                port: 8033,
            },
            snapshot: RwLock::new(DashboardSnapshot {
                html: "<html></html>".to_string(),
                generated_at: Utc::now(),
                stats: DashboardStats::default(),
                assumptions: Vec::new(),
                build_count: 1,
                last_error: None,
            }),
            rebuilding: AtomicBool::new(false),
        })
    }

    #[test]
    fn ensure_loopback_host_accepts_loopback_only() {
        assert!(ensure_loopback_host("127.0.0.1".parse().expect("ipv4")).is_ok());
        assert!(ensure_loopback_host("::1".parse().expect("ipv6")).is_ok());
        assert!(ensure_loopback_host("0.0.0.0".parse().expect("any")).is_err());
    }

    #[test]
    fn write_dashboard_artifact_writes_atomically() {
        let dir = mk_tmp_dir("dashboard_server_write");
        let output = dir.join("dashboard.html");

        write_dashboard_artifact(&output, "<h1>first</h1>").expect("first write");
        assert_eq!(
            fs::read_to_string(&output).expect("read first"),
            "<h1>first</h1>"
        );

        write_dashboard_artifact(&output, "<h1>second</h1>").expect("second write");
        assert_eq!(
            fs::read_to_string(&output).expect("read second"),
            "<h1>second</h1>"
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn regenerate_rejects_missing_header() {
        let root = mk_tmp_dir("dashboard_server_missing_header");
        let artifact_path = root.join("dashboard.html");
        seed_store(&root);
        let state = mk_state(root.clone(), artifact_path);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let response =
            runtime.block_on(regenerate_dashboard(State(state.clone()), HeaderMap::new()));
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!state.rebuilding.load(Ordering::SeqCst));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn regenerate_rejects_when_rebuild_in_progress() {
        let root = mk_tmp_dir("dashboard_server_rebuild_conflict");
        let artifact_path = root.join("dashboard.html");
        seed_store(&root);
        let state = mk_state(root.clone(), artifact_path);
        state.rebuilding.store(true, Ordering::SeqCst);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let mut headers = HeaderMap::new();
        headers.insert(
            REGENERATE_HEADER_NAME,
            HeaderValue::from_static(REGENERATE_HEADER_VALUE),
        );
        let response = runtime.block_on(regenerate_dashboard(State(state.clone()), headers));
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(state.rebuilding.load(Ordering::SeqCst));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn regenerate_accepts_required_header() {
        let root = mk_tmp_dir("dashboard_server_header_ok");
        let artifact_path = root.join("dashboard.html");
        seed_store(&root);
        let state = mk_state(root.clone(), artifact_path.clone());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let mut headers = HeaderMap::new();
        headers.insert(
            REGENERATE_HEADER_NAME,
            HeaderValue::from_static(REGENERATE_HEADER_VALUE),
        );
        let response = runtime.block_on(regenerate_dashboard(State(state), headers));
        assert_eq!(response.status(), StatusCode::OK);
        assert!(artifact_path.exists());

        let _ = fs::remove_dir_all(root);
    }
}
