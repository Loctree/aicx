//! Integration tests for shared HTTP Bearer auth on the dashboard `/api/*` surface
//! and on MCP HTTP transport. Both servers share `aicx::auth::require_auth_layer`,
//! so these tests exercise the same contract: identical-shape 401 on missing or
//! invalid token, pass-through on a matching constant-time compare, and
//! refusal-to-bind for non-loopback hosts without a token.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use aicx::auth::{self, AuthConfig, AuthSource};
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

fn auth_on(token: &str) -> AuthConfig {
    AuthConfig {
        token: Some(token.to_string()),
        source: AuthSource::Cli,
    }
}

fn build_protected_api_router(token: &str) -> Router {
    let api = Router::new()
        .route("/api/browse", get(|| async { "browse-ok" }))
        .route("/api/chunk", get(|| async { "chunk-ok" }));
    auth::require_auth_layer(api, auth_on(token))
}

fn request(uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = bearer {
        builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    let mut req = builder.body(Body::empty()).expect("request");
    let peer: SocketAddr = "127.0.0.1:49152".parse().expect("peer addr");
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
async fn test_dashboard_api_browse_without_token_401() {
    let app = build_protected_api_router("right-token");
    let response = app
        .oneshot(request("/api/browse", None))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = body_to_string(response).await;
    assert_eq!(body, r#"{"error":"unauthorized"}"#);
}

#[tokio::test]
async fn test_dashboard_api_browse_with_wrong_token_returns_401_same_shape() {
    let app = build_protected_api_router("right-token");
    let response = app
        .oneshot(request("/api/browse", Some("wrong-token")))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = body_to_string(response).await;
    assert_eq!(
        body, r#"{"error":"unauthorized"}"#,
        "401 body must be identical regardless of missing vs invalid token (no oracle channel)"
    );
}

#[tokio::test]
async fn test_dashboard_api_browse_with_wrong_length_token_returns_401_same_shape() {
    let app = build_protected_api_router("right-token");
    let response = app
        .oneshot(request("/api/browse", Some("short")))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = body_to_string(response).await;
    assert_eq!(body, r#"{"error":"unauthorized"}"#);
}

#[tokio::test]
async fn test_invalid_bearer_burst_rate_limited_after_threshold() {
    let app = build_protected_api_router("right-token");

    for _ in 0..100 {
        let response = app
            .clone()
            .oneshot(request("/api/browse", Some("wrong-token")))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let response = app
        .oneshot(request("/api/browse", Some("wrong-token")))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn test_dashboard_correct_token_passes() {
    let app = build_protected_api_router("right-token");
    let response = app
        .oneshot(request("/api/browse", Some("right-token")))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_to_string(response).await;
    assert_eq!(body, "browse-ok");
}

#[tokio::test]
async fn test_disabled_auth_does_not_gate_requests() {
    let app = auth::require_auth_layer(
        Router::new().route("/api/browse", get(|| async { "open-ok" })),
        AuthConfig::disabled(),
    );
    let response = app
        .oneshot(request("/api/browse", None))
        .await
        .expect("oneshot");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_to_string(response).await, "open-ok");
}

#[test]
fn test_dashboard_non_loopback_without_token_refuses_bind() {
    use aicx::dashboard_server::{DashboardCorsPolicy, validate_dashboard_host_policy};

    let auth_off = AuthConfig::disabled();
    let auth_on = AuthConfig {
        token: Some("seven-of-nine".to_string()),
        source: AuthSource::Cli,
    };

    // Loopback bind: always allowed even without auth.
    assert!(
        validate_dashboard_host_policy(
            "127.0.0.1".parse().expect("loopback"),
            &DashboardCorsPolicy::Local,
            false,
            &auth_off,
        )
        .is_ok()
    );

    // Non-loopback + explicit CORS + auth_off -> refuse bind.
    assert!(
        validate_dashboard_host_policy(
            "0.0.0.0".parse().expect("any"),
            &DashboardCorsPolicy::All,
            true,
            &auth_off,
        )
        .is_err(),
        "non-loopback bind must refuse when no auth token is configured",
    );

    // Non-loopback + explicit CORS + auth_on -> allow.
    assert!(
        validate_dashboard_host_policy(
            "0.0.0.0".parse().expect("any"),
            &DashboardCorsPolicy::All,
            true,
            &auth_on,
        )
        .is_ok(),
        "non-loopback bind must be allowed once an auth token is configured",
    );
}
