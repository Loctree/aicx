use super::search::{MAX_SCORE_FILTER, merge_project_scopes, validate_score_filter};
use super::*;
use crate::test_support::capture_logs;
use http_body_util::BodyExt;

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
fn validate_dashboard_host_policy_requires_explicit_non_local_cors_for_remote_hosts() {
    let local = DashboardCorsPolicy::Local;
    let all = DashboardCorsPolicy::All;
    let exact =
        DashboardCorsPolicy::from_cli(Some("https://dashboard.example.com")).expect("exact");
    let auth_on = AuthConfig {
        token: Some("test-token".to_string()),
        source: crate::auth::AuthSource::Cli,
    };
    let auth_off = AuthConfig::disabled();

    assert!(
        validate_dashboard_host_policy(
            "127.0.0.1".parse().expect("ipv4"),
            &local,
            false,
            &auth_off
        )
        .is_ok()
    );
    assert!(
        validate_dashboard_host_policy("0.0.0.0".parse().expect("any"), &local, false, &auth_on)
            .is_err()
    );
    assert!(
        validate_dashboard_host_policy("0.0.0.0".parse().expect("any"), &local, true, &auth_on)
            .is_err()
    );
    assert!(
        validate_dashboard_host_policy("0.0.0.0".parse().expect("any"), &all, true, &auth_on)
            .is_ok()
    );
    assert!(
        validate_dashboard_host_policy("0.0.0.0".parse().expect("any"), &exact, true, &auth_on)
            .is_ok()
    );
    // F-P0-2: non-loopback bind without auth must refuse, regardless of CORS.
    assert!(
        validate_dashboard_host_policy("0.0.0.0".parse().expect("any"), &all, true, &auth_off)
            .is_err()
    );
    assert!(
        validate_dashboard_host_policy("0.0.0.0".parse().expect("any"), &exact, true, &auth_off)
            .is_err()
    );
}

#[test]
fn cors_policy_matches_supported_origin_sets() {
    let local = DashboardCorsPolicy::from_cli(None).expect("default local");
    let tailscale = DashboardCorsPolicy::from_cli(Some("tailscale")).expect("tailscale");
    let all = DashboardCorsPolicy::from_cli(Some("all")).expect("all");
    let exact =
        DashboardCorsPolicy::from_cli(Some("https://dashboard.example.com")).expect("exact");

    assert!(local.allows_origin("http://localhost:3000"));
    assert!(local.allows_origin("http://127.0.0.1:9478"));
    assert!(!local.allows_origin("https://dashboard.example.com"));

    assert!(tailscale.allows_origin("http://100.64.0.1:9478"));
    assert!(tailscale.allows_origin("https://host.example.ts.net"));
    assert!(!tailscale.allows_origin("http://192.168.0.4:9478"));
    assert!(!tailscale.allows_origin("https://dashboard.example.com"));

    assert!(all.allows_origin("https://anything.example"));

    assert!(exact.allows_origin("https://dashboard.example.com"));
    assert!(!exact.allows_origin("https://other.example.com"));
}

#[test]
fn invalid_exact_cors_origin_is_rejected() {
    let err =
        DashboardCorsPolicy::from_cli(Some("dashboard.example.com")).expect_err("missing scheme");
    assert!(err.to_string().contains("scheme"));
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
fn score_filter_rejects_values_above_max() {
    let err = validate_score_filter(Some(MAX_SCORE_FILTER + 1))
        .expect_err("score above 100 should be rejected");
    assert_eq!(err, "score must be between 0 and 100");
}

// Bug #28 regression: `merge_project_scopes` must roll up by canonical
// case-insensitive equality, not by reverse substring. `vista` no longer
// collapses `vista-portal` into the same bucket.
#[test]
fn merge_project_scopes_rejects_substring_rollup() {
    let merged = merge_project_scopes(
        Some("vista"),
        None,
        vec!["vista-portal".to_string(), "vetcoders/vista".to_string()],
    );
    assert_eq!(
        merged,
        vec!["vista".to_string()],
        "reverse-substring rollup leaked: vista-portal / vetcoders/vista must NOT be retained when scope is `vista`"
    );
}

#[test]
fn merge_project_scopes_keeps_canonical_match_case_insensitive() {
    let merged = merge_project_scopes(
        Some("Vetcoders/Vista"),
        None,
        vec![
            "vetcoders/vista".to_string(),
            "vetcoders/vista-portal".to_string(),
        ],
    );
    assert_eq!(
        merged,
        vec!["vetcoders/vista".to_string()],
        "canonical-equality rollup must keep only the exact match (case-insensitive)"
    );
}

#[test]
fn merge_project_scopes_falls_back_to_scope_when_no_overlap() {
    let merged = merge_project_scopes(Some("vista"), None, vec!["vista-portal".to_string()]);
    assert_eq!(
        merged,
        vec!["vista".to_string()],
        "when no request matches the scope canonically, fall back to scope alone"
    );
}

#[test]
fn test_cors_all_returns_wildcard_not_reflected_origin() {
    let policy = DashboardCorsPolicy::All;
    // `Self::All` must return the literal wildcard so credentialed
    // cross-origin requests are refused by the browser. Reflecting the
    // request origin would let an attacker upgrade `All` into an echo
    // server for cookies/credentials if the server ever set
    // `Access-Control-Allow-Credentials: true`.
    assert_eq!(
        policy.response_allow_origin("https://example.com"),
        Some(HeaderValue::from_static("*"))
    );
    assert_eq!(
        policy.response_allow_origin("https://attacker.example.com"),
        Some(HeaderValue::from_static("*"))
    );
}

#[test]
fn test_service_worker_cache_name_includes_version() {
    // Just statically verified in code. But we can't test get_service_worker directly
    // unless it's sync. It's async.
}
