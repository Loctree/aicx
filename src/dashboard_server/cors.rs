use anyhow::{Context, Result, anyhow};
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::Response,
};
use std::{net::IpAddr, sync::Arc};

use crate::auth::AuthConfig;

use super::{DashboardServerState, forbidden_response};

const LOCALHOST_ORIGINS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];
const TAILSCALE_MAGICDNS_SUFFIX: &str = ".ts.net";
const TAILSCALE_RANGE_BASE: u32 = u32::from_be_bytes([100, 64, 0, 0]);
const TAILSCALE_RANGE_END: u32 = u32::from_be_bytes([100, 127, 255, 255]);

/// CORS policy for dashboard HTTP serving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashboardCorsPolicy {
    /// Default developer-local mode; accepts localhost + loopback browser origins.
    Local,
    /// Accept origins served from Tailscale CGNAT IPs or MagicDNS hostnames.
    Tailscale,
    /// Wildcard CORS, intended for trusted reverse-proxy or lab setups.
    All,
    /// Exact origin match such as <https://dashboard.example.com>.
    Exact(String),
}

impl DashboardCorsPolicy {
    pub fn from_cli(raw: Option<&str>) -> Result<Self> {
        let Some(raw_value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(Self::Local);
        };

        match raw_value.to_ascii_lowercase().as_str() {
            "local" => Ok(Self::Local),
            "tailscale" => Ok(Self::Tailscale),
            "all" => Ok(Self::All),
            _ => {
                normalize_origin(raw_value)?;
                Ok(Self::Exact(raw_value.to_string()))
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Local => "local".to_string(),
            Self::Tailscale => "tailscale".to_string(),
            Self::All => "all".to_string(),
            Self::Exact(origin) => origin.clone(),
        }
    }

    pub(super) fn allows_origin(&self, origin: &str) -> bool {
        match self {
            Self::Local => matches_local_origin(origin),
            Self::Tailscale => matches_tailscale_origin(origin),
            Self::All => true,
            Self::Exact(allowed) => allowed.eq_ignore_ascii_case(origin),
        }
    }

    fn response_allow_origin(&self, origin: &str) -> Option<HeaderValue> {
        match self {
            // CORS spec: `*` is incompatible with `Access-Control-Allow-Credentials: true`,
            // and the browser refuses cross-origin credentialed requests when wildcard is
            // returned. Reflecting the request origin here would defeat the protection
            // by upgrading a wide-open allowlist into an attacker-controlled echo.
            Self::All => Some(HeaderValue::from_static("*")),
            _ if self.allows_origin(origin) => HeaderValue::from_str(origin).ok(),
            _ => None,
        }
    }
}

pub fn validate_dashboard_host_policy(
    host: IpAddr,
    cors_policy: &DashboardCorsPolicy,
    cors_policy_was_explicit: bool,
    auth: &AuthConfig,
) -> Result<()> {
    if host.is_loopback() {
        return Ok(());
    }

    if !cors_policy_was_explicit || matches!(cors_policy, DashboardCorsPolicy::Local) {
        return Err(anyhow!(
            "Binding dashboard server to non-loopback address '{}' requires an explicit non-local CORS policy. Re-run with `--allow-cors-origins tailscale`, `--allow-cors-origins all`, or an explicit URL.",
            host
        ));
    }

    if !auth.is_enforced() {
        return Err(anyhow!(
            "Binding dashboard server to non-loopback address '{}' requires HTTP Bearer auth. Re-run without `--no-require-auth` or provide `--auth-token <TOKEN>`.",
            host
        ));
    }

    Ok(())
}

pub(super) async fn dashboard_cors_middleware(
    State(state): State<Arc<DashboardServerState>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let is_preflight = request.method() == axum::http::Method::OPTIONS;

    if is_preflight {
        return match origin.as_deref() {
            Some(origin) => {
                if let Some(allow_origin) = state.config.cors_policy.response_allow_origin(origin) {
                    let mut response = Response::new(Body::empty());
                    *response.status_mut() = StatusCode::NO_CONTENT;
                    apply_cors_headers(response.headers_mut(), allow_origin, true);
                    response
                } else {
                    forbidden_response(
                        "cors_preflight_origin_rejected",
                        format!(
                            "origin={origin}; policy={}",
                            state.config.cors_policy.label()
                        ),
                    )
                }
            }
            None => next.run(request).await,
        };
    }

    let mut response = next.run(request).await;
    if let Some(origin) = origin.as_deref()
        && let Some(allow_origin) = state.config.cors_policy.response_allow_origin(origin)
    {
        apply_cors_headers(response.headers_mut(), allow_origin, false);
    }
    response
}

fn apply_cors_headers(headers: &mut HeaderMap, allow_origin: HeaderValue, preflight: bool) {
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, allow_origin);
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type, x-ai-contexters-action"),
    );
    headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    if preflight {
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
    }
}

fn normalize_origin(origin: &str) -> Result<()> {
    let uri = origin
        .parse::<axum::http::Uri>()
        .with_context(|| format!("Invalid CORS origin URL '{}'", origin))?;
    let scheme = uri
        .scheme_str()
        .ok_or_else(|| anyhow!("CORS origin '{}' is missing a scheme", origin))?;
    if scheme != "http" && scheme != "https" {
        return Err(anyhow!(
            "CORS origin '{}' must use http:// or https://",
            origin
        ));
    }
    if uri.authority().is_none() {
        return Err(anyhow!("CORS origin '{}' is missing a host", origin));
    }
    Ok(())
}

fn matches_local_origin(origin: &str) -> bool {
    origin_host(origin)
        .map(|host| {
            LOCALHOST_ORIGINS
                .iter()
                .any(|allowed| host.eq_ignore_ascii_case(allowed))
        })
        .unwrap_or(false)
}

fn matches_tailscale_origin(origin: &str) -> bool {
    origin_host(origin)
        .is_some_and(|host| matches_tailscale_magicdns_host(&host) || matches_tailscale_ip(&host))
}

fn matches_tailscale_magicdns_host(host: &str) -> bool {
    host.to_ascii_lowercase()
        .ends_with(TAILSCALE_MAGICDNS_SUFFIX)
}

fn matches_tailscale_ip(host: &str) -> bool {
    host.parse::<IpAddr>()
        .ok()
        .and_then(|ip| match ip {
            IpAddr::V4(ipv4) => Some(u32::from_be_bytes(ipv4.octets())),
            IpAddr::V6(_) => None,
        })
        .is_some_and(|addr| (TAILSCALE_RANGE_BASE..=TAILSCALE_RANGE_END).contains(&addr))
}

fn origin_host(origin: &str) -> Option<String> {
    let uri = origin.parse::<axum::http::Uri>().ok()?;
    let authority = uri.authority()?;
    let host = authority.host().trim_matches(['[', ']']);
    Some(host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            validate_dashboard_host_policy(
                "0.0.0.0".parse().expect("any"),
                &local,
                false,
                &auth_on
            )
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
            validate_dashboard_host_policy(
                "0.0.0.0".parse().expect("any"),
                &exact,
                true,
                &auth_off
            )
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

        assert!(tailscale.allows_origin("http://100.96.12.4:9478"));
        assert!(tailscale.allows_origin("https://vetcoders-mbp.tail2c9f.ts.net"));
        assert!(!tailscale.allows_origin("http://192.168.0.4:9478"));
        assert!(!tailscale.allows_origin("https://dashboard.example.com"));

        assert!(all.allows_origin("https://anything.example"));

        assert!(exact.allows_origin("https://dashboard.example.com"));
        assert!(!exact.allows_origin("https://other.example.com"));
    }

    #[test]
    fn invalid_exact_cors_origin_is_rejected() {
        let err = DashboardCorsPolicy::from_cli(Some("dashboard.example.com"))
            .expect_err("missing scheme");
        assert!(err.to_string().contains("scheme"));
    }

    #[test]
    fn cors_all_returns_wildcard_not_reflected_origin() {
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
}
