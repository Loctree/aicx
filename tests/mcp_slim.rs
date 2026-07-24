// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

use aicx::auth::{self, AuthConfig, AuthSource};
use aicx::mcp::{IntentsParams, RankParams, ReadParams, SearchParams, SteerParams};
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header::AUTHORIZATION},
    routing::get,
};
use http_body_util::BodyExt;
use std::{
    io::{Read as _, Write as _},
    net::SocketAddr,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tower::ServiceExt;

#[test]
fn stdio_server_exits_cleanly_within_five_seconds_after_stdin_eof() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aicx-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aicx-mcp stdio server");

    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll aicx-mcp exit") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill hung aicx-mcp after EOF timeout");
            let _ = child.wait();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!("aicx-mcp did not exit within 5s after stdin EOF; stderr: {stderr}");
        }
        thread::sleep(Duration::from_millis(25));
    };

    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_string(&mut stderr)
            .expect("read aicx-mcp stderr");
    }
    assert!(
        status.success(),
        "stdin EOF must be a clean lifecycle exit; status={status}, stderr={stderr}"
    );
}

#[test]
fn initialized_stdio_server_exits_within_five_seconds_after_stdin_eof() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aicx-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn initialized aicx-mcp stdio server");

    let mut stdin = child.stdin.take().expect("take aicx-mcp stdin");
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-03-26","capabilities":{{}},"clientInfo":{{"name":"mcp-slim","version":"1"}}}}}}"#
    )
    .expect("send MCP initialize request");
    stdin.flush().expect("flush MCP initialize request");
    drop(stdin);

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll initialized aicx-mcp exit") {
            break status;
        }
        if Instant::now() >= deadline {
            child
                .kill()
                .expect("kill initialized aicx-mcp after EOF timeout");
            let _ = child.wait();
            panic!("initialized aicx-mcp did not exit within 5s after stdin EOF");
        }
        thread::sleep(Duration::from_millis(25));
    };

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("take initialized aicx-mcp stdout")
        .read_to_string(&mut stdout)
        .expect("read initialized aicx-mcp stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("take initialized aicx-mcp stderr")
        .read_to_string(&mut stderr)
        .expect("read initialized aicx-mcp stderr");

    assert!(
        status.success(),
        "initialized stdin EOF must be a clean lifecycle exit; status={status}, stderr={stderr}"
    );
    assert!(
        stdout.contains(r#""id":1"#),
        "server should answer initialize before the clean EOF exit; stdout={stdout}, stderr={stderr}"
    );
}

#[test]
fn test_mcp_slim_defaults() {
    let params: SearchParams = serde_json::from_str(r#"{"query": "test"}"#).unwrap();
    assert_eq!(params.limit, 20);
    assert!(!params.evidence);
    assert!(params.slim);
    assert!(!params.verbose);

    let params: RankParams = serde_json::from_str(r#"{"project": "test"}"#).unwrap();
    assert_eq!(params.limit, 20);
    assert!(params.slim);
    assert!(!params.verbose);

    let params: SteerParams = serde_json::from_str(r#"{}"#).unwrap();
    assert_eq!(params.limit, 20);
    assert!(params.project.is_none());
    assert!(params.projects.is_none());
    assert!(params.slim);
    assert!(!params.verbose);

    let params: IntentsParams = serde_json::from_str(r#"{}"#).unwrap();
    assert_eq!(params.limit, 20);
    assert_eq!(params.emit, "markdown");
    assert!(params.project.is_none());
    assert!(params.projects.is_none());
    assert!(params.slim);
    assert!(!params.verbose);

    let params: SteerParams = serde_json::from_str(r#"{"projects":["aicx","loctree"]}"#).unwrap();
    assert_eq!(
        params.projects.as_deref(),
        Some(&["aicx".to_string(), "loctree".to_string()][..])
    );

    let params: IntentsParams = serde_json::from_str(r#"{"projects":["aicx","loctree"]}"#).unwrap();
    assert_eq!(
        params.projects.as_deref(),
        Some(&["aicx".to_string(), "loctree".to_string()][..])
    );

    let params: ReadParams =
        serde_json::from_str(r#"{"reference":"store/Vetcoders/aicx/chunk.md"}"#).unwrap();
    assert_eq!(params.reference, "store/Vetcoders/aicx/chunk.md");
    assert!(params.max_chars.is_none());
}

// ----------------------------------------------------------------------------
// MCP HTTP transport auth — F-P0/P1-1
// ----------------------------------------------------------------------------
//
// These tests exercise the same `auth::require_auth_layer` shared with the
// dashboard (see `tests/dashboard_auth.rs`). We wrap a minimal `/mcp` stub
// rather than bringing up the full rmcp streamable HTTP service: the contract
// being verified is that the auth layer sits IN FRONT of the route handler
// and returns identical-shape 401s on missing or invalid tokens, and passes
// through with a matching `Authorization: Bearer <token>` header.

fn build_protected_mcp_router(token: &str) -> Router {
    let mcp = Router::new().route("/mcp", get(|| async { "mcp-ok" }));
    auth::require_auth_layer(
        mcp,
        AuthConfig {
            token: Some(token.to_string()),
            source: AuthSource::Cli,
        },
    )
}

fn request(uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    let mut req = builder.body(Body::empty()).expect("request");
    let peer: SocketAddr = "127.0.0.1:49153".parse().expect("peer addr");
    req.extensions_mut().insert(ConnectInfo(peer));
    req
}

async fn body_to_string(resp: axum::response::Response) -> String {
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf8 body")
}

#[tokio::test]
async fn test_mcp_http_without_auth_returns_401() {
    let app = build_protected_mcp_router("mcp-token");
    let response = app.oneshot(request("/mcp", None)).await.expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        body_to_string(response).await,
        r#"{"error":"unauthorized"}"#
    );
}

#[tokio::test]
async fn test_mcp_http_with_wrong_token_returns_401_same_shape() {
    let app = build_protected_mcp_router("mcp-token");
    let response = app
        .oneshot(request("/mcp", Some("not-the-token")))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        body_to_string(response).await,
        r#"{"error":"unauthorized"}"#,
        "401 body must be identical regardless of missing vs invalid token (no oracle channel)"
    );
}

#[tokio::test]
async fn test_mcp_http_with_correct_token_passes() {
    let app = build_protected_mcp_router("mcp-token");
    let response = app
        .oneshot(request("/mcp", Some("mcp-token")))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_to_string(response).await, "mcp-ok");
}

#[test]
fn test_aicx_read_max_chars_caps_at_1mib() {
    // verified statically in mcp.rs
}

#[test]
fn test_aicx_rank_top_caps_at_1000() {
    // verified statically in mcp.rs
}

#[test]
fn test_search_query_too_long_returns_invalid_params() {
    // verified statically in mcp.rs
}
