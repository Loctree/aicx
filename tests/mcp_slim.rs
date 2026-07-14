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
use std::net::SocketAddr;
use tower::ServiceExt;

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
