//! AI Contexters dashboard HTTP server runtime.
//!
//! Serves the generated dashboard artifact and supports on-demand regeneration.

use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::{
    fs,
    io::Write,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::RwLock;

use crate::dashboard::{self, DashboardConfig, DashboardStats};

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
        .route("/api/status", get(get_status))
        .route("/api/regenerate", post(regenerate_dashboard))
        .with_state(state);

    let addr = SocketAddr::new(config.host, config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind dashboard server on http://{addr}"))?;

    eprintln!("✓ Dashboard server started");
    eprintln!("  URL: http://{addr}");
    eprintln!("  Status endpoint: http://{addr}/api/status");
    eprintln!("  Regenerate endpoint: POST http://{addr}/api/regenerate");
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

async fn get_status(State(state): State<Arc<DashboardServerState>>) -> Json<DashboardStatusResponse> {
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

fn rebuild_dashboard(config: &DashboardServerConfig) -> Result<BuildOutput> {
    let artifact = dashboard::build_dashboard(&DashboardConfig {
        store_root: config.store_root.clone(),
        title: config.title.clone(),
        preview_chars: config.preview_chars,
        include_absolute_paths: true,
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
                    format!("Failed to create temporary artifact: {}", tmp_path.display())
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
    use crate::test_util::mk_tmp_dir;

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

        let response = runtime.block_on(regenerate_dashboard(State(state.clone()), HeaderMap::new()));
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
