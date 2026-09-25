//! Operator surfaces on the dashboard: phrase file and index trigger.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    DashboardServerState, REGENERATE_HEADER_NAME, REGENERATE_HEADER_VALUE, forbidden_response,
};
use crate::parser::intent_phrases;

pub(super) fn phrases_path(state: &DashboardServerState) -> std::path::PathBuf {
    state.config.aicx_home.join("intent_phrases.toml")
}

pub(super) async fn get_phrases(State(state): State<Arc<DashboardServerState>>) -> Response {
    let path = phrases_path(&state);
    let body = intent_phrases::read_operator_or_embedded(&path);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

pub(super) async fn put_phrases(
    State(state): State<Arc<DashboardServerState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Some(response) = reject_mutation(&state, &headers) {
        return response;
    }
    if let Err(err) = intent_phrases::reload_from_str(&body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(JsonError {
                error: "invalid_phrases",
                detail: err,
            }),
        )
            .into_response();
    }
    let path = phrases_path(&state);
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "write_failed",
                detail: err.to_string(),
            }),
        )
            .into_response();
    }
    if let Err(err) = std::fs::write(&path, body) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(JsonError {
                error: "write_failed",
                detail: err.to_string(),
            }),
        )
            .into_response();
    }
    (
        StatusCode::OK,
        Json(JsonOk {
            ok: true,
            path: path.display().to_string(),
        }),
    )
        .into_response()
}

pub(super) async fn post_index(
    State(state): State<Arc<DashboardServerState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = reject_mutation(&state, &headers) {
        return response;
    }
    let home = state.config.aicx_home.clone();
    tokio::spawn(async move {
        let _ = tokio::task::spawn_blocking(move || {
            let reporter = crate::progress::select_reporter(true);
            crate::source_index::build_with_reporter(&home, &[], false, false, true, reporter)
        })
        .await;
    });
    (
        StatusCode::ACCEPTED,
        Json(JsonOk {
            ok: true,
            path: "index".to_string(),
        }),
    )
        .into_response()
}

pub(super) async fn get_auth_page() -> Html<String> {
    Html(AUTH_HTML.replace("<!--mark-->", crate::dashboard::AICX_MARK_SVG))
}

pub(super) async fn auth_tailscale(headers: HeaderMap) -> Response {
    let login = headers
        .get("tailscale-user-login")
        .and_then(|value| value.to_str().ok());
    let Some(identity) = login.and_then(tailscale_identity) else {
        return (
            StatusCode::UNAUTHORIZED,
            Html(
                "<p>Tailscale Serve did not present Tailscale-User-Login. Publish this dashboard with <code>tailscale serve</code>. There is no separate Tailscale OAuth app.</p>",
            ),
        )
            .into_response();
    };
    session_redirect(&identity)
}

pub(super) async fn auth_google_start(headers: HeaderMap) -> Response {
    oauth_start(
        "google",
        "https://accounts.google.com/o/oauth2/v2/auth",
        "openid email",
        "AICX_GOOGLE_CLIENT_ID",
        &headers,
    )
}

pub(super) async fn auth_github_start(headers: HeaderMap) -> Response {
    oauth_start(
        "github",
        "https://github.com/login/oauth/authorize",
        "read:user user:email",
        "AICX_GITHUB_CLIENT_ID",
        &headers,
    )
}

struct OauthProvider {
    name: &'static str,
    token_url: &'static str,
    identity_url: &'static str,
    client_env: &'static str,
    secret_env: &'static str,
    identity_of: fn(&str) -> Option<String>,
}

const GOOGLE_PROVIDER: OauthProvider = OauthProvider {
    name: "google",
    token_url: "https://oauth2.googleapis.com/token",
    identity_url: "https://openidconnect.googleapis.com/v1/userinfo",
    client_env: "AICX_GOOGLE_CLIENT_ID",
    secret_env: "AICX_GOOGLE_CLIENT_SECRET",
    identity_of: google_identity,
};

const GITHUB_PROVIDER: OauthProvider = OauthProvider {
    name: "github",
    token_url: "https://github.com/login/oauth/access_token",
    identity_url: "https://api.github.com/user",
    client_env: "AICX_GITHUB_CLIENT_ID",
    secret_env: "AICX_GITHUB_CLIENT_SECRET",
    identity_of: github_identity,
};

pub(super) async fn auth_google_callback(
    headers: HeaderMap,
    Query(query): Query<OauthQuery>,
) -> Response {
    finish_oauth(&GOOGLE_PROVIDER, &headers, &query).await
}

pub(super) async fn auth_github_callback(
    headers: HeaderMap,
    Query(query): Query<OauthQuery>,
) -> Response {
    finish_oauth(&GITHUB_PROVIDER, &headers, &query).await
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct OauthQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

pub(crate) fn oauth_authorize_url(
    base: &str,
    client_id: &str,
    redirect: &str,
    scope: &str,
    state: &str,
) -> String {
    format!(
        "{base}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}",
        urlencoding_min(client_id),
        urlencoding_min(redirect),
        urlencoding_min(scope),
        urlencoding_min(state)
    )
}

pub(crate) fn tailscale_identity(login: &str) -> Option<String> {
    let login = login.trim();
    if login.is_empty() || login.len() > 200 {
        return None;
    }
    if !login.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'_' | b'-' | b'+')
    }) {
        return None;
    }
    Some(format!("tailscale:{login}"))
}

pub(crate) fn google_identity(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let id = value
        .get("email")
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())
        .or_else(|| value.get("sub").and_then(|item| item.as_str()))?;
    Some(format!("google:{id}"))
}

pub(crate) fn github_identity(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let login = value
        .get("login")
        .and_then(|item| item.as_str())
        .filter(|item| !item.is_empty())?;
    Some(format!("github:{login}"))
}

fn oauth_start(
    provider: &str,
    base: &str,
    scope: &str,
    client_env: &str,
    headers: &HeaderMap,
) -> Response {
    let Some(client_id) = std::env::var(client_env)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Html(format!(
                "<p>{provider} is not configured. Set {client_env}.</p>"
            )),
        )
            .into_response();
    };
    let origin = public_origin(headers);
    let redirect = format!("{origin}/auth/{provider}/callback");
    let state = random_hex(16);
    let url = oauth_authorize_url(base, &client_id, &redirect, scope, &state);
    let mut response_headers = HeaderMap::new();
    if let Ok(location) = HeaderValue::from_str(&url) {
        response_headers.insert(header::LOCATION, location);
    }
    append_cookie(
        &mut response_headers,
        &format!("aicx_oauth_state={state}; HttpOnly; SameSite=Lax; Path=/; Max-Age=600"),
    );
    append_cookie(
        &mut response_headers,
        &format!(
            "aicx_oauth_redirect={}; HttpOnly; SameSite=Lax; Path=/; Max-Age=600",
            hex::encode(redirect.as_bytes())
        ),
    );
    (StatusCode::FOUND, response_headers).into_response()
}

async fn finish_oauth(
    provider: &OauthProvider,
    headers: &HeaderMap,
    query: &OauthQuery,
) -> Response {
    if query
        .error
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        return html_status(
            StatusCode::BAD_REQUEST,
            "<p>The provider refused the sign-in.</p>",
        );
    }
    let Some(code) = query.code.as_deref().filter(|value| !value.is_empty()) else {
        return html_status(
            StatusCode::BAD_REQUEST,
            "<p>Missing authorization code.</p>",
        );
    };
    let Some(state) = query.state.as_deref().filter(|value| !value.is_empty()) else {
        return html_status(StatusCode::BAD_REQUEST, "<p>Missing OAuth state.</p>");
    };
    let Some(state_cookie) = cookie_value(headers, "aicx_oauth_state") else {
        return html_status(
            StatusCode::BAD_REQUEST,
            "<p>Missing OAuth state cookie.</p>",
        );
    };
    if !eq_secret(state, &state_cookie) {
        return html_status(StatusCode::BAD_REQUEST, "<p>OAuth state did not match.</p>");
    }
    let Some(redirect) = cookie_value(headers, "aicx_oauth_redirect")
        .and_then(|value| hex::decode(value).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
    else {
        return html_status(StatusCode::BAD_REQUEST, "<p>Missing OAuth redirect.</p>");
    };
    let Some(client_id) = std::env::var(provider.client_env)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return html_status(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!(
                "<p>{} is not configured. Set {}.</p>",
                provider.name, provider.client_env
            ),
        );
    };
    let Some(secret) = std::env::var(provider.secret_env)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return html_status(
            StatusCode::SERVICE_UNAVAILABLE,
            &format!(
                "<p>{} callback needs {} before a code can be exchanged.</p>",
                provider.name, provider.secret_env
            ),
        );
    };
    let identity = exchange_identity(
        provider.token_url,
        provider.identity_url,
        &client_id,
        &secret,
        code,
        &redirect,
        provider.identity_of,
    )
    .await;
    session_or_refuse(identity)
}

fn session_or_refuse(identity: Result<String, String>) -> Response {
    match identity {
        Ok(identity) => session_redirect(&identity),
        Err(_) => html_status(
            StatusCode::BAD_GATEWAY,
            "<p>The provider did not return an identity. No session was created.</p>",
        ),
    }
}

async fn exchange_identity(
    token_url: &str,
    identity_url: &str,
    client_id: &str,
    secret: &str,
    code: &str,
    redirect: &str,
    identity_of: fn(&str) -> Option<String>,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "oauth client unavailable".to_string())?;
    let body = format!(
        "code={}&client_id={}&client_secret={}&redirect_uri={}&grant_type=authorization_code",
        urlencoding_min(code),
        urlencoding_min(client_id),
        urlencoding_min(secret),
        urlencoding_min(redirect)
    );
    let token_text = client
        .post(token_url)
        .header(header::ACCEPT, "application/json")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|_| "token exchange failed".to_string())?
        .error_for_status()
        .map_err(|_| "token exchange refused".to_string())?
        .text()
        .await
        .map_err(|_| "token response was empty".to_string())?;
    let token: serde_json::Value =
        serde_json::from_str(&token_text).map_err(|_| "token response was not json".to_string())?;
    let access = token
        .get("access_token")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "no access token".to_string())?;
    let profile = client
        .get(identity_url)
        .header(header::AUTHORIZATION, format!("Bearer {access}"))
        .header(header::USER_AGENT, "aicx")
        .header(header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|_| "identity request failed".to_string())?
        .error_for_status()
        .map_err(|_| "identity request refused".to_string())?
        .text()
        .await
        .map_err(|_| "identity response was empty".to_string())?;
    identity_of(&profile).ok_or_else(|| "provider returned no identity".to_string())
}

fn session_redirect(identity: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::LOCATION, HeaderValue::from_static("/"));
    append_cookie(
        &mut headers,
        &format!(
            "aicx_session={}; HttpOnly; SameSite=Lax; Path=/",
            sign_session(identity)
        ),
    );
    (StatusCode::FOUND, headers).into_response()
}

fn sign_session(identity: &str) -> String {
    let mac = hmac_sha256(session_key(), identity.as_bytes());
    format!(
        "v1.{}.{}",
        hex::encode(identity.as_bytes()),
        hex::encode(mac)
    )
}

#[cfg(test)]
fn open_session(token: &str) -> Option<String> {
    let rest = token.strip_prefix("v1.")?;
    let (ident_hex, mac_hex) = rest.split_once('.')?;
    let ident = hex::decode(ident_hex).ok()?;
    let mac = hex::decode(mac_hex).ok()?;
    let expected = hmac_sha256(session_key(), &ident);
    if mac.len() != expected.len() {
        return None;
    }
    let mut diff = 0u8;
    for (left, right) in mac.iter().zip(expected.iter()) {
        diff |= left ^ right;
    }
    if diff != 0 {
        return None;
    }
    String::from_utf8(ident).ok()
}

fn session_key() -> &'static [u8; 32] {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    KEY.get_or_init(|| {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).expect("session key");
        buf
    })
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        padded[..32].copy_from_slice(&digest);
    } else {
        padded[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        ipad[index] ^= padded[index];
        opad[index] ^= padded[index];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    let digest = outer.finalize();
    let mut mac = [0u8; 32];
    mac.copy_from_slice(&digest);
    mac
}

fn public_origin(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("127.0.0.1");
    let https = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("https"));
    let scheme = if https { "https" } else { "http" };
    format!("{scheme}://{host}")
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("oauth state");
    hex::encode(buf)
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookie| {
            cookie.split(';').find_map(|part| {
                let part = part.trim();
                part.strip_prefix(&prefix).map(str::to_string)
            })
        })
}

fn append_cookie(headers: &mut HeaderMap, cookie: &str) {
    if let Ok(value) = HeaderValue::from_str(cookie) {
        headers.append(header::SET_COOKIE, value);
    }
}

fn html_status(status: StatusCode, body: &str) -> Response {
    (status, Html(body.to_string())).into_response()
}

fn eq_secret(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.bytes().zip(right.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn urlencoding_min(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn reject_mutation(state: &DashboardServerState, headers: &HeaderMap) -> Option<Response> {
    let header_ok = headers
        .get(REGENERATE_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case(REGENERATE_HEADER_VALUE));
    if !header_ok {
        return Some(forbidden_response(
            "missing_or_invalid_action_header",
            format!("expected {REGENERATE_HEADER_NAME}: {REGENERATE_HEADER_VALUE}"),
        ));
    }
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    let referer = headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok());
    let source = origin.or(referer);
    if source.is_none() && !state.config.allow_no_origin {
        return Some(forbidden_response(
            "missing_origin_or_referer",
            "mutating dashboard request had neither Origin nor Referer",
        ));
    }
    if let Some(source) = source
        && !state.config.cors_policy.allows_origin(source)
    {
        return Some(forbidden_response(
            "origin_or_referer_rejected",
            format!("source={source}"),
        ));
    }
    None
}

#[derive(Serialize)]
struct JsonError {
    error: &'static str,
    detail: String,
}

#[derive(Serialize)]
struct JsonOk {
    ok: bool,
    path: String,
}

const AUTH_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>aicx auth</title>
<style>
body{margin:0;background:#0e0e0e;color:#f5f1e7;font-family:Inter,system-ui,sans-serif}
main{max-width:22rem;margin:4rem auto;padding:1.5rem;text-align:center}
h1{font-family:"Instrument Serif","Iowan Old Style",Palatino,Georgia,serif;font-weight:400;font-size:2rem;margin:0 0 .5rem}
p{margin:.4rem 0 1rem;line-height:1.4}
#aicx-mark{display:block;width:28px;height:28px;margin:0 auto 1rem;color:#f5f1e7;background:transparent}
.pills{display:flex;flex-direction:column;align-items:center;gap:.45rem}
a{display:inline-flex;align-items:center;justify-content:center;gap:.55rem;width:14.5rem;margin:0;padding:.45rem .7rem;border:1px solid rgba(255,255,255,.16);border-radius:999px;color:#f5f1e7;text-decoration:none;font-size:.84rem}
a svg{width:14px;height:14px;flex:none}
a:hover{border-color:#3d7a72}
.note{margin:.15rem 0 .35rem;color:rgba(245,241,231,.64);font-size:.75rem}
</style></head>
<body><main>
<!--mark-->
<h1>Sign in</h1>
<p>Search stays on this machine. Pick the credential that already knows you.</p>
<div class="pills">
<a href="/auth/tailscale"><svg class="auth-mark" viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M24 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0zm-9 9a3 3 0 1 1-6 0 3 3 0 0 1 6 0zm0-9a3 3 0 1 1-6 0 3 3 0 0 1 6 0zm6-6a3 3 0 1 1 0-6 3 3 0 0 1 0 6zm0-.5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5zM3 24a3 3 0 1 1 0-6 3 3 0 0 1 0 6zm0-.5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5zm18 .5a3 3 0 1 1 0-6 3 3 0 0 1 0 6zm0-.5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5zM6 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0zm9-9a3 3 0 1 1-6 0 3 3 0 0 1 6 0zm-3 2.5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5zM6 3a3 3 0 1 1-6 0 3 3 0 0 1 6 0zM3 5.5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5z"/></svg>Continue with Tailscale</a>
<p class="note">Tailscale Serve sends Tailscale-User-Login. No separate Tailscale app.</p>
<a href="/auth/google"><svg class="auth-mark" viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M12.24 10.29V14.4h6.81c-.28 1.76-2.06 5.17-6.81 5.17-4.1 0-7.44-3.39-7.44-7.57S8.14 4.43 12.24 4.43c2.33 0 3.89.99 4.79 1.85l3.25-3.14C18.19 1.19 15.48 0 12.24 0 5.48 0 0 5.48 0 12.24S5.48 24.48 12.24 24.48c7.06 0 11.75-4.96 11.75-11.96 0-.8-.09-1.41-.19-2.23H12.24z"/></svg>Continue with Google</a>
<a href="/auth/github"><svg class="auth-mark" viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M12 .3C5.37.3 0 5.67 0 12.3c0 5.3 3.44 9.8 8.21 11.39.6.11.82-.26.82-.58 0-.28-.01-1.04-.02-2.04-3.34.72-4.04-1.61-4.04-1.61-.55-1.39-1.33-1.76-1.33-1.76-1.09-.74.08-.73.08-.73 1.2.08 1.84 1.24 1.84 1.24 1.07 1.83 2.81 1.3 3.5 1 .1-.78.42-1.3.76-1.6-2.67-.3-5.47-1.33-5.47-5.93 0-1.31.47-2.38 1.24-3.22-.13-.3-.54-1.52.1-3.18 0 0 1.01-.32 3.3 1.23a11.5 11.5 0 0 1 6 0c2.28-1.55 3.29-1.23 3.29-1.23.64 1.66.23 2.88.12 3.18.77.84 1.23 1.91 1.23 3.22 0 4.61-2.81 5.62-5.48 5.92.43.37.81 1.1.81 2.22 0 1.61-.01 2.9-.01 3.29 0 .32.22.69.83.57A12.3 12.3 0 0 0 24 12.3C24 5.67 18.63.3 12 .3z"/></svg>Continue with GitHub</a>
</div>
</main></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use tower::ServiceExt;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    fn auth_router() -> Router {
        Router::new()
            .route("/auth", get(get_auth_page))
            .route("/auth/tailscale", get(auth_tailscale))
            .route("/auth/google", get(auth_google_start))
            .route("/auth/github", get(auth_github_start))
            .route("/auth/google/callback", get(auth_google_callback))
            .route("/auth/github/callback", get(auth_github_callback))
    }

    async fn text_of(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        String::from_utf8(bytes.to_vec()).expect("utf8")
    }

    fn session_cookie(response: &Response) -> Option<String> {
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with("aicx_session="))
            .map(|value| {
                value
                    .trim_start_matches("aicx_session=")
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
    }

    #[test]
    fn auth_routes_respond() {
        runtime().block_on(async {
            let app = auth_router();
            let page = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/auth")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(page.status(), StatusCode::OK);
            let html = text_of(page).await;
            assert!(html.contains("id=\"aicx-mark\""));
            assert!(html.contains("aria-label=\"Loctree\""));
            assert!(html.contains("/auth/tailscale"));
            assert!(html.contains("/auth/google"));
            assert!(html.contains("/auth/github"));
            assert!(html.contains("Tailscale-User-Login"));
            assert_eq!(html.matches("auth-mark").count(), 3);
            assert!(html.contains("M24 12a3 3 0"));
            assert!(html.contains("background:transparent"));
            assert!(html.contains("text-align:center"));

            let missing = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/auth/tailscale")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
            assert!(session_cookie(&missing).is_none());

            let signed = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/auth/tailscale")
                        .header("Tailscale-User-Login", "ada@example.com")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(signed.status(), StatusCode::FOUND);
            let token = session_cookie(&signed).expect("signed session");
            assert_eq!(
                open_session(&token).as_deref(),
                Some("tailscale:ada@example.com")
            );

            for path in ["/auth/google/callback", "/auth/github/callback"] {
                let refused = app
                    .clone()
                    .oneshot(
                        axum::http::Request::builder()
                            .uri(path)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
                assert!(session_cookie(&refused).is_none());
            }
        });
    }

    #[test]
    fn failed_exchange_does_not_mint_a_session() {
        let response = session_or_refuse(Err("no identity".to_string()));
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(session_cookie(&response).is_none());
    }

    #[test]
    fn provider_identity_parsers_and_signed_cookie() {
        assert_eq!(
            google_identity(r#"{"email":"ada@example.com"}"#).as_deref(),
            Some("google:ada@example.com")
        );
        assert_eq!(
            github_identity(r#"{"login":"ada"}"#).as_deref(),
            Some("github:ada")
        );
        assert!(google_identity("{}").is_none());
        assert!(tailscale_identity("").is_none());
        let token = sign_session("google:ada@example.com");
        assert_eq!(
            open_session(&token).as_deref(),
            Some("google:ada@example.com")
        );
        assert!(open_session("v1.00.00").is_none());
        let url = oauth_authorize_url(
            "https://accounts.google.com/o/oauth2/v2/auth",
            "client",
            "https://host/auth/google/callback",
            "openid email",
            "state",
        );
        assert!(url.contains("client_id=client"));
        assert!(url.contains("scope=openid%20email"));
    }
}
