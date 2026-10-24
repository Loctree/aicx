#![cfg(feature = "app")]
//! Expand on the dashboard returns the whole session, not the bounded scan detail.
use aicx::{
    auth::{AuthConfig, AuthSource},
    catalog::{CATALOG_SCHEMA, CatalogEntry},
    dashboard::DashboardScope,
    dashboard_server::{DashboardCorsPolicy, DashboardServerConfig, dashboard_router},
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
    sync::{Mutex, MutexGuard},
};
use tower::ServiceExt;

static HOME_LOCK: Mutex<()> = Mutex::new(());
const TURNS: usize = 120;

struct Fixture {
    root: PathBuf,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _home_lock: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let guard = HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "aicx-dashboard-detail-full-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let id = "22222222-3333-4444-5555-000000000001";
        let cwd = "/fixtures/Loctree/aicx";
        let dir = root.join(".claude/projects/-fixtures-Loctree-aicx");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{id}.jsonl"));
        let mut lines = String::new();
        for n in 0..TURNS {
            let (minute, second) = (n / 60, n % 60);
            let user = json!({
                "type": "user",
                "uuid": format!("u-{n}"),
                "sessionId": id,
                "cwd": cwd,
                "timestamp": format!("2026-10-02T12:{minute:02}:{second:02}Z"),
                "message": {"role": "user", "content": format!("<user_query>marker-turn-{n:03}: keep every turn reachable.</user_query>")}
            });
            let reply = json!({
                "type": "assistant",
                "uuid": format!("a-{n}"),
                "sessionId": id,
                "cwd": cwd,
                "timestamp": format!("2026-10-02T13:{minute:02}:{second:02}Z"),
                "message": {"role": "assistant", "content": [{"type": "text", "text": format!("reply-{n:03} acknowledged.")}]}
            });
            lines.push_str(&format!("{user}\n{reply}\n"));
        }
        fs::write(&path, lines).unwrap();
        let entry = CatalogEntry {
            schema: CATALOG_SCHEMA.into(),
            session_id: id.into(),
            agent: "claude".into(),
            project: Some("Loctree/aicx".into()),
            date: Some("2026-10-02".into()),
            cwd: Some(cwd.into()),
            source_path: path.to_string_lossy().into(),
            source_len: None,
            source_mtime_ns: None,
            source_bundle_fingerprint: None,
            title: None,
            machine: None,
            logical_session_id: None,
            session_kind: None,
        };
        let catalog = aicx::catalog::sessions_path_for(&root);
        fs::create_dir_all(catalog.parent().unwrap()).unwrap();
        fs::write(
            catalog,
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();
        let previous = ["HOME", "AICX_ALLOW_TMP"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        // Every test in this binary holds HOME_LOCK; the scan resolves agent
        // sources against HOME and must never read the real user's sessions.
        unsafe {
            std::env::set_var("HOME", &root);
            std::env::set_var("AICX_ALLOW_TMP", "1");
        }
        Self {
            root,
            previous,
            _home_lock: guard,
        }
    }

    async fn router(&self) -> Router {
        dashboard_router(
            DashboardServerConfig {
                aicx_home: self.root.clone(),
                scope: DashboardScope::default(),
                title: "Detail test".into(),
                preview_chars: 320,
                artifact_path: self.root.join("dashboard.html"),
                cors_policy: DashboardCorsPolicy::Local,
                host: "127.0.0.1".parse().unwrap(),
                port: 8047,
                auth: AuthConfig {
                    token: Some("fixture-token".into()),
                    source: AuthSource::Cli,
                },
                allow_no_origin: true,
            },
            true,
            false,
        )
        .await
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            for (key, value) in &self.previous {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn get(router: Router, uri: &str) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .uri(uri)
        .header("authorization", "Bearer fixture-token")
        .body(Body::empty())
        .unwrap();
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
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn expand_rereads_the_whole_session_past_the_scan_caps() {
    let fixture = Fixture::new();
    let router = fixture.router().await;

    let (status, bounded) = get(router.clone(), "/api/detail?id=1").await;
    assert_eq!(status, StatusCode::OK, "bounded detail: {bounded}");
    let bounded = bounded["detail_text"].as_str().unwrap_or_default();
    assert!(
        bounded.contains("marker-turn-000"),
        "scan detail starts the conversation: {bounded:.200}"
    );
    assert!(!bounded.contains("<user_query>"));
    assert!(
        !bounded.contains("marker-turn-119"),
        "fixture must exceed the scan's turn cap, or this test proves nothing"
    );

    let (status, full) = get(router, "/api/detail?id=1&full=1").await;
    assert_eq!(status, StatusCode::OK, "full detail: {full}");
    let full = full["detail_text"].as_str().unwrap_or_default();
    for n in [0, 79, 80, TURNS - 1] {
        let marker = format!("marker-turn-{n:03}");
        assert!(full.contains(&marker), "Expand lost {marker}");
    }
    assert!(full.contains("reply-119 acknowledged."));
    assert!(!full.contains("<user_query>"));
    assert!(full.len() > bounded.len());
}
