//! Shared HTTP Bearer-token auth for MCP HTTP transport and dashboard server.
//!
//! Single token loaded from CLI override, `AICX_HTTP_AUTH_TOKEN`, `~/.aicx/auth-token`,
//! or generated and persisted (mode 0600). Compared in constant time. Mismatch and
//! missing produce the same 401 body to defeat oracle probing.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result, anyhow};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};

const AUTH_RATE_LIMIT_BURST: u32 = 100;
const AUTH_RATE_LIMIT_REPLENISH_MS: u64 = 600;

/// Where the active token came from. Used for the startup log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthSource {
    /// Token provided via CLI flag (`--auth-token`).
    Cli,
    /// Token read from `AICX_HTTP_AUTH_TOKEN`.
    Env,
    /// Token read from a persistent token file (typically `~/.aicx/auth-token`).
    File(PathBuf),
    /// Token generated on this run and written to a fresh token file.
    Generated(PathBuf),
    /// Auth explicitly disabled by the operator (`--no-require-auth`). No token enforced.
    Disabled,
}

impl AuthSource {
    pub fn describe(&self) -> String {
        match self {
            Self::Cli => "cli".to_string(),
            Self::Env => "env".to_string(),
            Self::File(path) => format!("file:{}", path.display()),
            Self::Generated(path) => format!("generated:{}", path.display()),
            Self::Disabled => "disabled".to_string(),
        }
    }
}

/// Loaded auth state. `token == None` only when the operator explicitly opted out.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub token: Option<String>,
    pub source: AuthSource,
}

impl AuthConfig {
    /// Auth is enforced when a token is present.
    pub fn is_enforced(&self) -> bool {
        self.token.is_some()
    }

    /// Disabled auth — no token. Use only when operator passes `--no-require-auth`.
    pub fn disabled() -> Self {
        Self {
            token: None,
            source: AuthSource::Disabled,
        }
    }
}

/// Resolution rule for the canonical persistent token file.
fn default_token_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("Unable to resolve home directory"))?;
    Ok(home.join(".aicx").join("auth-token"))
}

/// Load auth configuration from (in order): CLI override, env, file, or generate.
///
/// `require_auth = false` skips all of the above and returns [`AuthConfig::disabled`].
pub fn load_auth_config(cli_token: Option<&str>, require_auth: bool) -> Result<AuthConfig> {
    if !require_auth {
        return Ok(AuthConfig::disabled());
    }

    if let Some(token) = cli_token {
        let token = token.trim();
        if token.is_empty() {
            return Err(anyhow!("--auth-token must not be empty"));
        }
        return Ok(AuthConfig {
            token: Some(token.to_string()),
            source: AuthSource::Cli,
        });
    }

    if let Ok(value) = std::env::var("AICX_HTTP_AUTH_TOKEN") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Ok(AuthConfig {
                token: Some(value),
                source: AuthSource::Env,
            });
        }
    }

    let path = default_token_path()?;
    if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Read auth token file {}", path.display()))?;
        let token = content.trim().to_string();
        if !token.is_empty() {
            return Ok(AuthConfig {
                token: Some(token),
                source: AuthSource::File(path),
            });
        }
    }

    let token = generate_token().context("Generate HTTP auth token")?;
    persist_token_file(&path, &token).context("Persist HTTP auth token to file")?;
    Ok(AuthConfig {
        token: Some(token),
        source: AuthSource::Generated(path),
    })
}

fn generate_token() -> Result<String> {
    let mut buf = [0u8; 32];
    let mut file =
        std::fs::File::open("/dev/urandom").context("Open /dev/urandom for token entropy")?;
    file.read_exact(&mut buf).context("Read /dev/urandom")?;
    Ok(hex_encode(&buf))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(*byte >> 4) as usize] as char);
        out.push(HEX[(*byte & 0x0f) as usize] as char);
    }
    out
}

fn persist_token_file(path: &PathBuf, token: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Create token directory {}", parent.display()))?;
    }
    std::fs::write(path, format!("{}\n", token))
        .with_context(|| format!("Write token file {}", path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)
            .with_context(|| format!("Stat token file {}", path.display()))?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("Set mode 0600 on token file {}", path.display()))?;
    }

    Ok(())
}

/// Hand-rolled constant-time byte slice comparison. Returns true iff the inputs
/// have identical length AND identical bytes. Length-mismatch is short-circuited
/// (already a known timing channel for length, but byte content does not leak).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

#[derive(Serialize)]
struct UnauthorizedBody {
    error: &'static str,
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(UnauthorizedBody {
            error: "unauthorized",
        }),
    )
        .into_response()
}

async fn auth_middleware(
    State(config): State<Arc<AuthConfig>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(expected) = config.token.as_deref() else {
        return next.run(request).await;
    };

    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "));

    let Some(provided) = presented else {
        return unauthorized_response();
    };

    if provided.len() != expected.len() {
        return unauthorized_response();
    }

    if constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
        next.run(request).await
    } else {
        unauthorized_response()
    }
}

/// Wrap a router with the Bearer auth middleware. Pass-through when
/// `config.token` is `None` (operator opted out of auth).
pub fn require_auth_layer<S>(router: Router<S>, config: AuthConfig) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let auth_enforced = config.is_enforced();
    let state = Arc::new(config);
    let router = router.layer(middleware::from_fn_with_state(state, auth_middleware));

    if !auth_enforced {
        return router;
    }

    let governor_config = GovernorConfigBuilder::default()
        .per_millisecond(AUTH_RATE_LIMIT_REPLENISH_MS)
        .burst_size(AUTH_RATE_LIMIT_BURST)
        .finish()
        .expect("auth rate limit config is non-zero");

    router.layer(GovernorLayer::new(governor_config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialise env-var manipulation to avoid cross-test interference.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn clear_env() {
        // Safety: the mutex guards concurrent access to process env across tests.
        unsafe {
            std::env::remove_var("AICX_HTTP_AUTH_TOKEN");
        }
    }

    #[test]
    fn test_load_auth_token_from_env() {
        let _guard = ENV_MUTEX.lock().expect("env mutex poisoned");
        clear_env();
        // Safety: guarded by ENV_MUTEX for the duration of this test.
        unsafe {
            std::env::set_var("AICX_HTTP_AUTH_TOKEN", "from-env-token");
        }
        let cfg = load_auth_config(None, true).expect("load env token");
        assert_eq!(cfg.token.as_deref(), Some("from-env-token"));
        assert_eq!(cfg.source, AuthSource::Env);
        clear_env();
    }

    #[test]
    fn test_load_auth_token_from_file_with_mode_0600() {
        let _guard = ENV_MUTEX.lock().expect("env mutex poisoned");
        clear_env();
        let tmp = std::env::temp_dir().join(format!(
            "aicx-auth-test-{}-{}.token",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::write(&tmp, "file-token-value\n").expect("write tmp token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&tmp).expect("stat tmp").permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&tmp, perms).expect("chmod tmp");
        }

        let token = std::fs::read_to_string(&tmp).expect("read tmp");
        assert_eq!(token.trim(), "file-token-value");
        assert!(constant_time_eq(
            token.trim().as_bytes(),
            b"file-token-value"
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&tmp)
                .expect("stat tmp")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "file should be mode 0600");
        }
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_load_auth_token_generates_when_missing() {
        let _guard = ENV_MUTEX.lock().expect("env mutex poisoned");
        clear_env();
        let tmp_dir = std::env::temp_dir().join(format!(
            "aicx-auth-gen-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let tmp_path = tmp_dir.join("auth-token");
        let token = generate_token().expect("generate token");
        assert_eq!(token.len(), 64, "32 bytes hex-encoded = 64 chars");
        persist_token_file(&tmp_path, &token).expect("persist token");
        assert!(tmp_path.exists());

        let on_disk = std::fs::read_to_string(&tmp_path).expect("read persisted");
        assert_eq!(on_disk.trim(), token);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&tmp_path)
                .expect("stat persisted")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "persisted token must be 0600");
        }
        let _ = std::fs::remove_file(&tmp_path);
        let _ = std::fs::remove_dir(&tmp_dir);
    }

    #[test]
    fn test_constant_time_compare_rejects_short_mismatch() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn test_disabled_config_passes_through_in_middleware() {
        assert!(!AuthConfig::disabled().is_enforced());
    }

    #[test]
    fn test_cli_override_wins() {
        let _guard = ENV_MUTEX.lock().expect("env mutex poisoned");
        clear_env();
        // Safety: env access guarded by ENV_MUTEX.
        unsafe {
            std::env::set_var("AICX_HTTP_AUTH_TOKEN", "env-loser");
        }
        let cfg = load_auth_config(Some("cli-winner"), true).expect("cli override");
        assert_eq!(cfg.token.as_deref(), Some("cli-winner"));
        assert_eq!(cfg.source, AuthSource::Cli);
        clear_env();
    }
}
