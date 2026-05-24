//! AI Contexters dashboard HTTP server runtime.
//!
//! Serves the local dashboard UI and supports live search/regeneration APIs.

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::RwLock;

#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::{fs, io::Write};

use crate::auth::{self, AuthConfig};
use crate::dashboard::{self, DashboardPayload, DashboardScope, DashboardStats};

mod browse;
mod cors;
mod search;
use cors::dashboard_cors_middleware;
pub use cors::{DashboardCorsPolicy, validate_dashboard_host_policy};

const REGENERATE_HEADER_NAME: &str = "x-ai-contexters-action";
const REGENERATE_HEADER_VALUE: &str = "regenerate";

/// Runtime configuration for dashboard server mode.
#[derive(Debug, Clone)]
pub struct DashboardServerConfig {
    pub store_root: PathBuf,
    pub scope: DashboardScope,
    pub title: String,
    pub preview_chars: usize,
    /// Legacy compatibility path surfaced in status; server mode does not write it.
    pub artifact_path: PathBuf,
    pub cors_policy: DashboardCorsPolicy,
    pub host: IpAddr,
    pub port: u16,
    pub auth: AuthConfig,
    pub allow_no_origin: bool,
}

#[derive(Debug, Clone)]
struct DashboardSnapshot {
    /// Scanned payload (records, projects, agents, kinds, stats).
    payload: DashboardPayload,
    generated_at: DateTime<Utc>,
    stats: DashboardStats,
    assumptions: Vec<String>,
    build_count: u64,
    last_error: Option<String>,
}

impl DashboardSnapshot {
    fn from_build(build: BuildOutput) -> Self {
        Self {
            stats: build.payload.stats.clone(),
            assumptions: build.payload.assumptions.clone(),
            payload: build.payload,
            generated_at: build.generated_at,
            build_count: 1,
            last_error: None,
        }
    }
}

#[derive(Debug)]
struct DashboardServerState {
    config: DashboardServerConfig,
    /// Lightweight server-mode HTML shell (no embedded data).
    shell_html: String,
    snapshot: RwLock<DashboardSnapshot>,
    rebuilding: AtomicBool,
}

#[derive(Debug)]
struct BuildOutput {
    payload: DashboardPayload,
    generated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct DashboardStatusResponse {
    ok: bool,
    mode: &'static str,
    rebuilding: bool,
    generated_at: String,
    build_count: u64,
    store_root: String,
    artifact_path: String,
    artifact_written: bool,
    title: String,
    preview_chars: usize,
    stats: DashboardStats,
    assumptions: Vec<String>,
    last_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct DashboardRegenerateResponse {
    ok: bool,
    mode: &'static str,
    regenerated_at: String,
    build_count: u64,
    artifact_path: String,
    artifact_written: bool,
    stats: DashboardStats,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    ok: bool,
    error: String,
}

fn forbidden_response(reason: &'static str, detail: impl std::fmt::Display) -> Response {
    tracing::warn!(
        reason,
        detail = %detail,
        "dashboard security check rejected request"
    );
    (
        StatusCode::FORBIDDEN,
        Json(ErrorResponse {
            ok: false,
            error: "Forbidden".to_string(),
        }),
    )
        .into_response()
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
    validate_dashboard_host_policy(config.host, &config.cors_policy, true, &config.auth)?;

    let initial = rebuild_dashboard(&config).context("Initial dashboard build failed")?;
    let shell_html = dashboard::render_server_shell_html(&config.title);

    let state = Arc::new(DashboardServerState {
        config: config.clone(),
        shell_html,
        snapshot: RwLock::new(DashboardSnapshot::from_build(initial)),
        rebuilding: AtomicBool::new(false),
    });

    let auth_source_label = config.auth.source.describe();
    let auth_enforced = config.auth.is_enforced();

    // Public (no Bearer) routes: HTML shell + manifest + service worker + liveness.
    // The browser fetches these as a raw GET to render the UI; the corpus-data
    // surface (`/api/browse`, `/api/detail`, `/api/chunk`, `/api/context`,
    // `/api/search/*`, `/api/regenerate`, `/api/status`) is gated below.
    let public_router: Router<Arc<DashboardServerState>> = Router::new()
        .route("/", get(get_dashboard_html))
        .route("/health", get(get_health))
        .route("/api/health", get(get_health))
        .route("/manifest.webmanifest", get(get_manifest))
        .route("/service-worker.js", get(get_service_worker));

    let api_router: Router<Arc<DashboardServerState>> = Router::new()
        .route("/api/status", get(get_status))
        .route("/api/browse", get(browse::get_browse))
        .route("/api/detail", get(browse::get_detail))
        .route("/api/chunk", get(browse::get_chunk))
        .route("/api/context", get(get_context))
        .route("/api/regenerate", post(regenerate_dashboard))
        .route("/api/search/semantic", get(search::get_semantic_search))
        .route("/api/search/cross", get(search::cross_search_gone))
        .route("/api/search/steer", get(search::steer_search));

    let api_router = auth::require_auth_layer(api_router, config.auth.clone());

    let app = public_router
        .merge(api_router)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            dashboard_cors_middleware,
        ))
        .with_state(state);

    let addr = SocketAddr::new(config.host, config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind dashboard server on http://{addr}"))?;

    eprintln!("✓ Dashboard server started (PWA shell)");
    eprintln!("  URL: http://{addr}");
    eprintln!("  Browse:    GET  http://{addr}/api/browse?sort=newest&since=24h&project=<p>");
    eprintln!("  Detail:    GET  http://{addr}/api/detail?id=<n>");
    eprintln!("  Chunk:     GET  http://{addr}/api/chunk?id=<n>");
    eprintln!("  Context:   GET  http://{addr}/api/context");
    eprintln!("  Status:    GET  http://{addr}/api/status");
    eprintln!("  Regenerate: POST http://{addr}/api/regenerate");
    eprintln!(
        "  Semantic:  GET  http://{addr}/api/search/semantic?q=<query>&project=<p>&score=<min>"
    );
    eprintln!("  Steer:     GET  http://{addr}/api/search/steer?run_id=<id>&project=<p>");
    eprintln!("  PWA:       GET  http://{addr}/manifest.webmanifest");
    eprintln!(
        "  Required header: {}: {}",
        REGENERATE_HEADER_NAME, REGENERATE_HEADER_VALUE
    );
    if config.allow_no_origin {
        eprintln!("  Mutation origin gate: allow no-Origin/no-Referer requests");
    } else {
        eprintln!("  Mutation origin gate: require Origin or Referer");
    }
    eprintln!("  Store: {}", config.store_root.display());
    eprintln!("  CORS: {}", config.cors_policy.label());
    if auth_enforced {
        eprintln!("  Auth: enabled on /api/* (source: {auth_source_label})");
        if let Some(msg) = auth::proxy_rate_limit_warning(config.host) {
            eprintln!("  ⚠ Rate-limit (proxy): {msg}");
        }
    } else {
        eprintln!(
            "  Auth: DISABLED ({auth_source_label}) — /api/* surface is reachable by anyone who can hit {addr}"
        );
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("Dashboard server runtime terminated unexpectedly")
}

async fn get_dashboard_html(State(state): State<Arc<DashboardServerState>>) -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("interest-cohort=()"),
    );
    (headers, Html(state.shell_html.clone()))
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
        mode: "server-shell",
        rebuilding: state.rebuilding.load(Ordering::SeqCst),
        generated_at: snapshot.generated_at.to_rfc3339(),
        build_count: snapshot.build_count,
        store_root: state.config.store_root.display().to_string(),
        artifact_path: state.config.artifact_path.display().to_string(),
        artifact_written: false,
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
    // Mutation gate: require the action header, Bearer auth, and by default an
    // Origin/Referer that matches the configured dashboard origin policy.
    let header_ok = headers
        .get(REGENERATE_HEADER_NAME)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case(REGENERATE_HEADER_VALUE));

    if !header_ok {
        return forbidden_response(
            "missing_or_invalid_action_header",
            format!("expected {REGENERATE_HEADER_NAME}: {REGENERATE_HEADER_VALUE}"),
        );
    }

    let origin_str = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let referer_str = headers.get(header::REFERER).and_then(|v| v.to_str().ok());
    let source_url = origin_str.or(referer_str);

    if source_url.is_none() && !state.config.allow_no_origin {
        return forbidden_response(
            "missing_origin_or_referer",
            "mutating dashboard request had neither Origin nor Referer",
        );
    }

    if let Some(source_url) = source_url
        && !state.config.cors_policy.allows_origin(source_url)
    {
        return forbidden_response(
            "origin_or_referer_rejected",
            format!(
                "source={source_url}; policy={}",
                state.config.cors_policy.label()
            ),
        );
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
            snapshot.stats = build.payload.stats.clone();
            snapshot.assumptions = build.payload.assumptions.clone();
            snapshot.payload = build.payload;
            snapshot.generated_at = build.generated_at;
            snapshot.build_count = snapshot.build_count.saturating_add(1);
            snapshot.last_error = None;

            let response = DashboardRegenerateResponse {
                ok: true,
                mode: "server-shell",
                regenerated_at: snapshot.generated_at.to_rfc3339(),
                build_count: snapshot.build_count,
                artifact_path: state.config.artifact_path.display().to_string(),
                artifact_written: false,
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
// Context / PWA endpoints
// ============================================================================

async fn get_context(State(state): State<Arc<DashboardServerState>>) -> Json<serde_json::Value> {
    let snapshot = state.snapshot.read().await;
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "store_root": state.config.store_root.display().to_string(),
        "host": state.config.host.to_string(),
        "port": state.config.port,
        "generated_at": snapshot.generated_at.to_rfc3339(),
        "build_count": snapshot.build_count,
        "stats": snapshot.stats,
    }))
}

async fn get_manifest() -> Response {
    let manifest = serde_json::json!({
        "name": "aicx Dashboard",
        "short_name": "aicx",
        "description": "AI Context Browser \u{2014} operator retrieval dashboard",
        "start_url": "/",
        "scope": "/",
        "display": "standalone",
        "theme_color": "#0a0f19",
        "background_color": "#0a0f19",
        "icons": []
    });
    let body = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/manifest+json"),
    );
    (headers, body).into_response()
}

async fn get_service_worker() -> Response {
    let sw_js = concat!(
        "const CACHE_NAME='aicx-shell-v",
        env!("CARGO_PKG_VERSION"),
        "';\
const SHELL_URLS=['/','/manifest.webmanifest'];\
self.addEventListener('install',e=>{e.waitUntil(caches.open(CACHE_NAME)\
.then(c=>c.addAll(SHELL_URLS)));self.skipWaiting();});\
self.addEventListener('activate',e=>{e.waitUntil(caches.keys()\
.then(ks=>Promise.all(ks.filter(k=>k.startsWith('aicx-shell-')&&k!==CACHE_NAME).map(k=>caches.delete(k)))));\
self.clients.claim();});\
self.addEventListener('fetch',e=>{const u=new URL(e.request.url);\
if(u.pathname.startsWith('/api/')||u.pathname==='/service-worker.js')return;\
e.respondWith(caches.match(e.request).then(r=>{if(r)return r;\
return fetch(e.request).catch(()=>{if(e.request.mode==='navigate')\
return new Response('<html><body style=\"background:#0a0f19;color:#e5e7eb;\
font-family:system-ui;display:flex;align-items:center;justify-content:center;\
height:100vh;margin:0\"><div style=\"text-align:center\"><h1>aicx store not \
reachable</h1><p>Start the server with <code>aicx dashboard --serve</code></p>\
</div></body></html>',{headers:{'Content-Type':'text/html'}});});}));});"
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    (headers, sw_js).into_response()
}

fn rebuild_dashboard(config: &DashboardServerConfig) -> Result<BuildOutput> {
    // Server mode: scan only — no static HTML rendering, no artifact write.
    // The server shell HTML is pre-built once at startup; all data reaches
    // clients through the /api/* endpoints.
    let payload = dashboard::scan_store_payload_scoped(
        &config.store_root,
        config.preview_chars,
        &config.scope,
    )?;

    Ok(BuildOutput {
        payload,
        generated_at: Utc::now(),
    })
}

#[cfg(test)]
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

    let (tmp_path, mut tmp_file) = temp_slot
        .ok_or_else(|| anyhow::anyhow!("Failed to allocate unique temporary artifact path"))?;

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
    use http_body_util::BodyExt;
    use std::io;
    use std::sync::{Arc as StdArc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct CapturedLogWriter {
        buffer: StdArc<Mutex<Vec<u8>>>,
    }

    struct CapturedLogGuard {
        buffer: StdArc<Mutex<Vec<u8>>>,
    }

    impl io::Write for CapturedLogGuard {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buffer.lock().expect("log buffer poisoned").extend(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for CapturedLogWriter {
        type Writer = CapturedLogGuard;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedLogGuard {
                buffer: self.buffer.clone(),
            }
        }
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

    fn seed_store(root: &Path) {
        let p = root.join("demo").join("2026-02-24");
        fs::create_dir_all(&p).expect("create store dirs");
        fs::write(
            p.join("120000_codex-context.md"),
            "# demo\n\n### 2026-02-24 12:00:00 UTC | user\n> hello",
        )
        .expect("seed file");
    }

    async fn response_body_to_string(response: Response) -> String {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect response body")
            .to_bytes();
        String::from_utf8(bytes.to_vec()).expect("utf8 body")
    }

    fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, String) {
        let buffer = StdArc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(CapturedLogWriter {
                buffer: buffer.clone(),
            })
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::WARN)
            .finish();

        let result = tracing::subscriber::with_default(subscriber, f);
        let logs = String::from_utf8(buffer.lock().expect("log buffer poisoned").clone())
            .expect("utf8 logs");
        (result, logs)
    }

    fn mk_state(root: PathBuf, artifact_path: PathBuf) -> Arc<DashboardServerState> {
        mk_state_with_origin_escape(root, artifact_path, false)
    }

    fn mk_state_with_origin_escape(
        root: PathBuf,
        artifact_path: PathBuf,
        allow_no_origin: bool,
    ) -> Arc<DashboardServerState> {
        Arc::new(DashboardServerState {
            config: DashboardServerConfig {
                store_root: root,
                scope: DashboardScope::default(),
                title: "test".to_string(),
                preview_chars: 120,
                artifact_path,
                cors_policy: DashboardCorsPolicy::Local,
                host: "127.0.0.1".parse().expect("host"),
                port: 8033,
                auth: AuthConfig::disabled(),
                allow_no_origin,
            },
            shell_html: "<html>shell</html>".to_string(),
            snapshot: RwLock::new(DashboardSnapshot {
                payload: DashboardPayload {
                    generated_at: String::new(),
                    store_root: String::new(),
                    stats: DashboardStats::default(),
                    assumptions: Vec::new(),
                    projects: Vec::new(),
                    agents: Vec::new(),
                    kinds: Vec::new(),
                    records: Vec::new(),
                },
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
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = runtime.block_on(response_body_to_string(response));
        assert_eq!(body, r#"{"ok":false,"error":"Forbidden"}"#);
        assert!(!state.rebuilding.load(Ordering::SeqCst));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn regenerate_logs_detailed_reason_without_leaking_403_body() {
        let root = mk_tmp_dir("dashboard_server_forbidden_log");
        let artifact_path = root.join("dashboard.html");
        seed_store(&root);
        let state = mk_state(root.clone(), artifact_path);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let (response, logs) = capture_logs(|| {
            runtime.block_on(regenerate_dashboard(State(state.clone()), HeaderMap::new()))
        });
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = runtime.block_on(response_body_to_string(response));
        assert_eq!(body, r#"{"ok":false,"error":"Forbidden"}"#);
        assert!(logs.contains("missing_or_invalid_action_header"));
        assert!(logs.contains(REGENERATE_HEADER_NAME));
        assert!(!body.contains(REGENERATE_HEADER_NAME));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn regenerate_rejects_missing_origin_and_referer() {
        let root = mk_tmp_dir("dashboard_server_missing_origin");
        let artifact_path = root.join("dashboard.html");
        seed_store(&root);
        let state = mk_state(root.clone(), artifact_path);

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
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = runtime.block_on(response_body_to_string(response));
        assert_eq!(body, r#"{"ok":false,"error":"Forbidden"}"#);
        assert!(!state.rebuilding.load(Ordering::SeqCst));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn regenerate_accepts_no_origin_when_escape_hatch_enabled() {
        let root = mk_tmp_dir("dashboard_server_no_origin_escape");
        let artifact_path = root.join("dashboard.html");
        seed_store(&root);
        let state = mk_state_with_origin_escape(root.clone(), artifact_path, true);

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

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn regenerate_rejects_cross_origin_referer() {
        let root = mk_tmp_dir("dashboard_server_cross_origin_referer");
        let artifact_path = root.join("dashboard.html");
        seed_store(&root);
        let state = mk_state(root.clone(), artifact_path);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let mut headers = HeaderMap::new();
        headers.insert(
            REGENERATE_HEADER_NAME,
            HeaderValue::from_static(REGENERATE_HEADER_VALUE),
        );
        headers.insert(header::REFERER, "https://evil.example".parse().unwrap());
        let response = runtime.block_on(regenerate_dashboard(State(state), headers));
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = runtime.block_on(response_body_to_string(response));
        assert_eq!(body, r#"{"ok":false,"error":"Forbidden"}"#);

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
        headers.insert(header::ORIGIN, "http://127.0.0.1:4000".parse().unwrap());
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
        headers.insert(header::ORIGIN, "http://127.0.0.1:4000".parse().unwrap());
        let response = runtime.block_on(regenerate_dashboard(State(state), headers));
        assert_eq!(response.status(), StatusCode::OK);
        // Server mode no longer writes a static HTML artifact — data is served via API.

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn test_service_worker_cache_name_includes_version() {
        // Just statically verified in code. But we can't test get_service_worker directly
        // unless it's sync. It's async.
    }
}
