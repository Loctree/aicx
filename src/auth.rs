//! Shared HTTP Bearer-token auth for MCP HTTP transport and dashboard server.
//!
//! Single token loaded from CLI override, `AICX_HTTP_AUTH_TOKEN`, `~/.aicx/auth-token`,
//! or generated and persisted on Unix (mode 0600). Compared in constant time.
//! Mismatch and missing produce the same 401 body to defeat oracle probing.
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
use std::path::{Path, PathBuf};
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
///
/// Lives at `<AICX_HOME>/auth-token` — i.e. honors `$AICX_HOME` when set,
/// falls back to `~/.aicx/auth-token` otherwise. Routes through the
/// canonical resolver so an operator pinning `AICX_HOME` does not end
/// up with an auth token stranded in the default `~/.aicx` while
/// everything else moves.
fn default_token_path() -> Result<PathBuf> {
    Ok(crate::store::resolve_aicx_home()?.join("auth-token"))
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
    match persist_token_file(&path, &token).context("Persist HTTP auth token to file")? {
        TokenPersistOutcome::Created | TokenPersistOutcome::Overwrote => Ok(AuthConfig {
            token: Some(token),
            source: AuthSource::Generated(path),
        }),
        TokenPersistOutcome::AdoptedExisting(existing) => Ok(AuthConfig {
            // Startup race: another process created a usable token file
            // between our existence check and our create_new attempt.
            // Adopt theirs instead of failing the entire auth init.
            token: Some(existing),
            source: AuthSource::File(path),
        }),
    }
}

/// Outcome of [`persist_token_file`]. Distinguishes between the
/// happy-path create, recovering from a startup race against another
/// process, and atomically replacing a truncated / empty existing file.
#[derive(Debug)]
#[cfg_attr(not(unix), allow(dead_code))]
enum TokenPersistOutcome {
    /// We won the create race and wrote our token to disk.
    Created,
    /// The file already existed and held a non-empty token; the caller
    /// should adopt that token instead of the one it just generated.
    AdoptedExisting(String),
    /// The file existed but was empty / whitespace-only (truncated or
    /// manually edited); we atomically replaced it with our token while
    /// preserving mode 0600.
    Overwrote,
}

fn generate_token() -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf)
        .map_err(|err| anyhow!("Generate random bytes for auth token: {err}"))?;
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

fn persist_token_file(path: &Path, token: &str) -> Result<TokenPersistOutcome> {
    #[cfg(windows)]
    {
        let _ = token;
        Err(anyhow!(
            "Refusing to persist aicx auth token file {} on Windows because this build does not configure restricted file ACLs. Run aicx auth on Linux/macOS, or pass --auth-token <token> explicitly so the token file is never written.",
            path.display()
        ))
    }

    #[cfg(unix)]
    {
        use std::io::{ErrorKind, Write};
        use std::os::unix::fs::OpenOptionsExt;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Create token directory {}", parent.display()))?;
        }

        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(mut file) => {
                file.write_all(format!("{}\n", token).as_bytes())
                    .with_context(|| format!("Write token file {}", path.display()))?;
                file.flush()
                    .with_context(|| format!("Flush token file {}", path.display()))?;
                Ok(TokenPersistOutcome::Created)
            }
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                // Two cases collapse here:
                //   1. Startup race — another process won create_new between
                //      load_auth_config's `path.exists()` check and ours.
                //   2. Empty-file recovery — the file exists but was
                //      truncated or hand-edited to whitespace, so
                //      load_auth_config fell through to generate+persist.
                // Reading the file once tells us which case we're in and
                // lets us avoid aborting the entire auth init on either.
                let existing = std::fs::read_to_string(path).with_context(|| {
                    format!(
                        "Re-read existing token file after AlreadyExists: {}",
                        path.display()
                    )
                })?;
                let trimmed = existing.trim();
                if !trimmed.is_empty() {
                    return Ok(TokenPersistOutcome::AdoptedExisting(trimmed.to_string()));
                }
                atomic_replace_token_file(path, token)
                    .with_context(|| format!("Replace empty token file {}", path.display()))?;
                Ok(TokenPersistOutcome::Overwrote)
            }
            Err(err) => Err(err).with_context(|| {
                format!(
                    "Create token file {} atomically with mode 0600",
                    path.display()
                )
            }),
        }
    }

    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = token;
        Err(anyhow!(
            "Refusing to persist aicx auth token file {} because this platform does not expose Unix mode 0600 or Windows restricted ACL handling. Pass --auth-token <token> explicitly so the token file is never written.",
            path.display()
        ))
    }
}

/// Atomically replace an existing (empty / whitespace-only) token file
/// with a fresh token while preserving mode 0600. Implemented as
/// "write tempfile sibling with create_new + 0600, then rename" so a
/// crash mid-write never truncates the destination. Called only from
/// the recovery branch of [`persist_token_file`].
#[cfg(unix)]
fn atomic_replace_token_file(path: &Path, token: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Token file path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("Token file path has no filename: {}", path.display()))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut rand = [0u8; 8];
    getrandom::fill(&mut rand)
        .map_err(|err| anyhow!("Generate random tmp suffix for token replace: {err}"))?;
    let tmp_path = parent.join(format!(
        ".{}.tmp.{}.{}.{}",
        file_name.to_string_lossy(),
        std::process::id(),
        nanos,
        hex_encode(&rand),
    ));

    let res = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp_path)
            .with_context(|| format!("Create tmp token file {}", tmp_path.display()))?;
        file.write_all(format!("{}\n", token).as_bytes())
            .with_context(|| format!("Write tmp token file {}", tmp_path.display()))?;
        file.flush()
            .with_context(|| format!("Flush tmp token file {}", tmp_path.display()))?;
        // Drop the handle so the rename is unambiguous on platforms that
        // care about an open writer crossing a rename boundary.
        drop(file);
        std::fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "Rename tmp token file {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })
    })();

    if res.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    res
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
///
/// **Rate-limit contract (local-first, NOT proxy-aware).** The governor
/// uses `tower_governor`'s default key extractor, which buckets requests
/// by the peer socket address. AICX is local-first: in the loopback /
/// Tailscale / direct-bind case that maps one bucket per actual client
/// and behaves as intended.
///
/// Behind a reverse proxy (nginx, Caddy, Cloudflare), every request
/// arrives from a small number of proxy IPs, so all proxied users share
/// a single bucket — one noisy client can starve the rest with `429`s.
/// The bind path emits an operator warning when auth is enforced and the
/// server is bound to a non-loopback address; resolving that into a
/// trusted-header proxy mode is tracked as a follow-up (Option B in the
/// PR #6 review). Do not market this layer as multi-user / proxy-safe
/// until that work lands.
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

    // Peer-IP / local-first bucket. See the function doc-comment for the
    // proxy contract this intentionally does not provide.
    let governor_config = GovernorConfigBuilder::default()
        .per_millisecond(AUTH_RATE_LIMIT_REPLENISH_MS)
        .burst_size(AUTH_RATE_LIMIT_BURST)
        .finish()
        .expect("auth rate limit config is non-zero");

    router.layer(GovernorLayer::new(governor_config))
}

/// Operator-facing description of the rate-limit / proxy contract.
///
/// Returned as `Some(message)` when the operator binds to a non-loopback
/// address with auth enabled — exactly the configuration where a
/// reverse proxy is plausible and where the peer-IP bucket could be
/// silently shared across many real users. Returned as `None` when the
/// bind is loopback (rate-limit semantics match operator expectations).
pub fn proxy_rate_limit_warning(host: std::net::IpAddr) -> Option<&'static str> {
    if host.is_loopback() {
        None
    } else {
        Some(
            "Rate limit on /api/* is peer-IP / local-first and NOT proxy-aware. \
             Behind a reverse proxy every user shares the proxy's bucket, so a single \
             noisy client can starve others with 429. Proxy-aware key extraction \
             (trusted-header opt-in) is tracked as a follow-up.",
        )
    }
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
    fn test_proxy_rate_limit_warning_is_silent_on_loopback() {
        // PR #6 follow-up regression for the rate-limit/proxy contract:
        // local-first binds (the AICX default) must NOT emit the
        // proxy-shared-bucket warning — that would be noise and would
        // train operators to ignore it.
        let v4 = std::net::IpAddr::from([127u8, 0, 0, 1]);
        assert!(super::proxy_rate_limit_warning(v4).is_none());
        let v6 = std::net::IpAddr::from([0u16, 0, 0, 0, 0, 0, 0, 1]);
        assert!(super::proxy_rate_limit_warning(v6).is_none());
    }

    #[test]
    fn test_proxy_rate_limit_warning_fires_for_non_loopback_bind() {
        // The same path that gates non-loopback bind (must have auth +
        // explicit CORS) must surface the peer-IP / proxy limitation so
        // the operator does not assume multi-user safety behind a
        // reverse proxy.
        let v4 = std::net::IpAddr::from([0u8, 0, 0, 0]);
        let msg = super::proxy_rate_limit_warning(v4)
            .expect("non-loopback bind must emit proxy rate-limit warning");
        assert!(
            msg.contains("peer-IP") && msg.contains("proxy"),
            "warning must reference peer-IP / proxy contract: {msg}"
        );

        let tailscale = std::net::IpAddr::from([100u8, 75, 30, 90]);
        assert!(super::proxy_rate_limit_warning(tailscale).is_some());
    }

    #[test]
    fn test_generate_token_shape_and_uniqueness_sanity() {
        let first = generate_token().expect("generate first token");
        let second = generate_token().expect("generate second token");

        assert_eq!(first.len(), 64, "32 bytes hex-encoded = 64 chars");
        assert_eq!(second.len(), 64, "32 bytes hex-encoded = 64 chars");
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(second.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second, "two CSPRNG tokens should differ");
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

    #[cfg(unix)]
    #[test]
    fn test_persist_token_file_adopts_existing_valid_token() {
        // PR #6 follow-up regression: simulate the startup race where
        // another process wrote a valid token between our existence
        // check and our `create_new(true)` attempt. The recovery path
        // MUST re-read the file and return `AdoptedExisting` instead of
        // aborting auth initialisation with `AlreadyExists`.
        let tmp_dir = std::env::temp_dir().join(format!(
            "aicx-auth-existing-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let tmp_path = tmp_dir.join("auth-token");
        std::fs::create_dir_all(&tmp_dir).expect("create tmp dir");
        std::fs::write(&tmp_path, "existing-token\n").expect("write existing token");

        let outcome = persist_token_file(&tmp_path, "replacement-token")
            .expect("AlreadyExists with valid content must recover via adoption");
        match outcome {
            TokenPersistOutcome::AdoptedExisting(token) => {
                assert_eq!(token, "existing-token");
            }
            other => panic!("expected AdoptedExisting, got {other:?}"),
        }
        // Existing content must not be clobbered when we adopt it.
        assert_eq!(
            std::fs::read_to_string(&tmp_path).expect("read existing token"),
            "existing-token\n"
        );

        let _ = std::fs::remove_file(&tmp_path);
        let _ = std::fs::remove_dir(&tmp_dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_persist_token_file_overwrites_empty_existing_file() {
        // PR #6 follow-up regression: an empty / whitespace-only token
        // file is treated as unusable by load_auth_config, so it falls
        // through to generate + persist. The recovery path MUST
        // atomically replace the empty file with the fresh token (mode
        // 0600 preserved) and signal `Overwrote` so the caller surfaces
        // `AuthSource::Generated`.
        use std::os::unix::fs::PermissionsExt;

        let tmp_dir = std::env::temp_dir().join(format!(
            "aicx-auth-empty-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let tmp_path = tmp_dir.join("auth-token");
        std::fs::create_dir_all(&tmp_dir).expect("create tmp dir");
        // Whitespace-only is exactly the shape load_auth_config rejects.
        std::fs::write(&tmp_path, "   \n").expect("write empty token");

        let fresh = generate_token().expect("generate replacement token");
        let outcome = persist_token_file(&tmp_path, &fresh)
            .expect("empty token file must be atomically replaced");
        match outcome {
            TokenPersistOutcome::Overwrote => {}
            other => panic!("expected Overwrote, got {other:?}"),
        }

        let on_disk = std::fs::read_to_string(&tmp_path).expect("read replaced token");
        assert_eq!(on_disk.trim(), fresh);
        let mode = std::fs::metadata(&tmp_path)
            .expect("stat replaced token")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "atomic replace must preserve mode 0600 on the destination"
        );

        // Sibling tempfiles must be cleaned up (no `.auth-token.tmp.*` left).
        let leftovers: Vec<_> = std::fs::read_dir(&tmp_dir)
            .expect("read tmp dir")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".auth-token.tmp.")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "atomic replace left tempfiles behind: {leftovers:?}"
        );

        let _ = std::fs::remove_file(&tmp_path);
        let _ = std::fs::remove_dir(&tmp_dir);
    }

    #[cfg(unix)]
    #[test]
    fn test_persist_token_file_first_writer_returns_created() {
        // Happy path: target does not exist, we win create_new, outcome
        // is `Created`. Reaffirms that the recovery branch did not
        // silently take over the normal create path.
        let tmp_dir = std::env::temp_dir().join(format!(
            "aicx-auth-firstwriter-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let tmp_path = tmp_dir.join("auth-token");
        let fresh = generate_token().expect("generate token");
        let outcome = persist_token_file(&tmp_path, &fresh).expect("persist new token");
        match outcome {
            TokenPersistOutcome::Created => {}
            other => panic!("expected Created, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&tmp_path)
                .expect("read persisted")
                .trim(),
            fresh
        );
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
