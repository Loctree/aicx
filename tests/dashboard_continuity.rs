#![cfg(feature = "app")]
//! Continuity through the authenticated dashboard router and the real CLI.
use aicx::{
    auth::{AuthConfig, AuthSource},
    catalog::{CATALOG_SCHEMA, CatalogEntry},
    dashboard::DashboardScope,
    dashboard_server::{DashboardCorsPolicy, DashboardServerConfig, dashboard_router_deferred},
};
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{
    fs,
    net::SocketAddr,
    path::PathBuf,
    process::Command,
    sync::{Mutex, MutexGuard},
};
use tower::ServiceExt;

static HOME_LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    root: PathBuf,
    previous_home: Option<std::ffi::OsString>,
    _home_lock: MutexGuard<'static, ()>,
}
impl Fixture {
    fn new() -> Self {
        let guard = HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "aicx-dashboard-continuity-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(root.join("synthetic")).unwrap();
        let mut entries = Vec::new();
        for (n, project) in ["Loctree/aicx", "other/aicx"].iter().enumerate() {
            let id = format!("11111111-2222-3333-4444-{n:012}");
            let path = root.join(format!("synthetic/{id}.jsonl"));
            let cwd = format!("/fixtures/{project}");
            let source = json!({"type":"user","sessionId":id,"cwd":cwd,"timestamp":"2026-10-02T12:00:00Z","message":{"role":"user","content":"Decision: preserve the browser cold start contract."}});
            fs::write(&path, format!("{source}\n")).unwrap();
            entries.push(CatalogEntry {
                schema: CATALOG_SCHEMA.into(),
                session_id: id,
                agent: "claude".into(),
                project: Some((*project).into()),
                date: Some("2026-10-02".into()),
                cwd: Some(cwd),
                source_path: path.to_string_lossy().into(),
                source_len: None,
                source_mtime_ns: None,
                source_bundle_fingerprint: None,
                title: None,
                machine: None,
                logical_session_id: None,
                session_kind: None,
            });
        }
        let catalog = aicx::catalog::sessions_path_for(&root);
        fs::create_dir_all(catalog.parent().unwrap()).unwrap();
        fs::write(
            catalog,
            entries
                .iter()
                .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
                .collect::<String>(),
        )
        .unwrap();
        let previous_home = std::env::var_os("HOME");
        // Every test in this binary holds HOME_LOCK, including its blocking
        // parser tasks. Addressed speech must never scan the real user's bus.
        unsafe {
            std::env::set_var("HOME", &root);
        }
        Self {
            root,
            previous_home,
            _home_lock: guard,
        }
    }
    async fn router(&self) -> Router {
        dashboard_router_deferred(
            DashboardServerConfig {
                aicx_home: self.root.clone(),
                scope: DashboardScope::default(),
                title: "Continuity test".into(),
                preview_chars: 320,
                artifact_path: self.root.join("dashboard.html"),
                cors_policy: DashboardCorsPolicy::Local,
                host: "127.0.0.1".parse().unwrap(),
                port: 8045,
                auth: AuthConfig {
                    token: Some("fixture-token".into()),
                    source: AuthSource::Cli,
                },
                allow_no_origin: true,
            },
            true,
        )
        .await
        .unwrap()
    }
    fn cli(&self, project: &str, inject: bool, addressee: Option<&str>) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aicx"));
        command.args([
            "continuity",
            "show",
            "-p",
            project,
            "-H",
            "72",
            "--until",
            "2026-10-03T04:47:00Z",
            "--no-refresh",
        ]);
        if inject {
            command.arg("--for-inject");
        }
        if let Some(name) = addressee {
            command.args(["--as", name]);
        }
        command
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("AICX_HOME", &self.root)
            .env("AICX_ALLOW_TMP", "1")
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            match &self.previous_home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn call(router: Router, uri: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut req = Request::builder().uri(uri);
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let mut req = req.body(Body::empty()).unwrap();
    req.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:49152".parse::<SocketAddr>().unwrap(),
    ));
    let response = router.oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&bytes)})),
    )
}

#[tokio::test]
async fn continuity_requires_the_same_bearer_as_status() {
    let fixture = Fixture::new();
    let router = fixture.router().await;
    for token in [None, Some("wrong-token")] {
        let status = call(router.clone(), "/api/status", token).await;
        let continuity = call(
            router.clone(),
            "/api/continuity?project=Loctree/aicx",
            token,
        )
        .await;
        assert_eq!(continuity, status);
        assert_eq!(
            continuity,
            (StatusCode::UNAUTHORIZED, json!({"error":"unauthorized"}))
        );
    }
}

#[tokio::test]
async fn continuity_pack_counts_and_command_reproduce_the_cli() {
    let fixture = Fixture::new();
    let router = fixture.router().await;
    for inject in [false, true] {
        let uri = format!(
            "/api/continuity?project=Loctree/aicx&as=Andrzej&hours=72&inject={inject}&until=2026-10-03T04:47:00Z"
        );
        let (status, data) = call(router.clone(), &uri, Some("fixture-token")).await;
        assert_eq!(status, StatusCode::OK, "{data}");
        let cli = fixture.cli("Loctree/aicx", inject, Some("Andrzej"));
        assert!(
            cli.status.success(),
            "{}",
            String::from_utf8_lossy(&cli.stderr)
        );
        assert_eq!(data["markdown"], String::from_utf8(cli.stdout).unwrap());
        let markdown = data["markdown"].as_str().unwrap();
        let sections = data["sections"].as_array().unwrap();
        assert!(sections.iter().any(|s| s["title"] == "NOW"));
        assert!(
            sections
                .iter()
                .any(|s| s["title"] == "ADDRESSED TO ANDRZEJ")
        );
        assert_eq!(
            sections
                .iter()
                .map(|s| s["lines"].as_u64().unwrap())
                .sum::<u64>(),
            markdown.lines().count() as u64
        );
        assert_eq!(
            sections
                .iter()
                .map(|s| s["markdown"].as_str().unwrap())
                .collect::<String>(),
            markdown
        );
        for section in sections {
            assert_eq!(
                section["lines"].as_u64().unwrap(),
                section["markdown"].as_str().unwrap().lines().count() as u64
            );
        }
        let command = data["cli_command"].as_str().unwrap();
        let args = shlex::split(command).unwrap();
        assert_eq!(args[0], "aicx");
        let replay = Command::new(env!("CARGO_BIN_EXE_aicx"))
            .args(&args[1..])
            .env("HOME", &fixture.root)
            .env("USERPROFILE", &fixture.root)
            .env("AICX_HOME", &fixture.root)
            .env("AICX_ALLOW_TMP", "1")
            .output()
            .unwrap();
        assert!(
            replay.status.success(),
            "{}",
            String::from_utf8_lossy(&replay.stderr)
        );
        assert_eq!(String::from_utf8(replay.stdout).unwrap(), markdown);
        assert_eq!(args.contains(&"--for-inject".to_string()), inject);
    }
}

#[tokio::test]
async fn unknown_and_ambiguous_projects_return_cli_error_as_json() {
    let fixture = Fixture::new();
    let router = fixture.router().await;
    for project in ["missing", "unknown/repo", "aicx"] {
        let cli = fixture.cli(project, false, None);
        assert!(!cli.status.success());
        let (status, data) = call(
            router.clone(),
            &format!("/api/continuity?project={project}"),
            Some("fixture-token"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{data}");
        let stderr = String::from_utf8(cli.stderr).unwrap();
        assert!(
            stderr.contains(data["error"].as_str().unwrap()),
            "CLI: {stderr}; API: {data}"
        );
        assert!(!data["error"].as_str().unwrap().is_empty());
        assert!(data.get("markdown").is_none());
    }
}

#[tokio::test]
async fn invalid_parameters_fail_as_json() {
    let fixture = Fixture::new();
    for query in [
        "",
        "project=",
        "project=Loctree/aicx&hours=nope",
        "project=Loctree/aicx&inject=yes",
        "project=Loctree/aicx&until=yesterday",
    ] {
        let (status, data) = call(
            fixture.router().await,
            &format!("/api/continuity?{query}"),
            Some("fixture-token"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{data}");
        assert_eq!(data["ok"], false);
        assert!(data["error"].is_string());
    }
}
