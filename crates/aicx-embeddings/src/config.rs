use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::path::PathBuf;

use tracing::{debug, warn};

use crate::{
    BackendPreference, EmbeddingConfig, EmbeddingProfile, EmbeddingProfileSpec,
    NativeEmbedderConfigFile, ResolvedEmbeddingModel,
};

pub const DEFAULT_BASE_REPO: &str = "mradermacher/F2LLM-v2-0.6B-GGUF";
pub const DEFAULT_BASE_FILENAME: &str = "F2LLM-v2-0.6B.Q4_K_M.gguf";
pub const DEFAULT_DEV_REPO: &str = "mradermacher/F2LLM-v2-1.7B-GGUF";
pub const DEFAULT_DEV_FILENAME: &str = "F2LLM-v2-1.7B.Q4_K_M.gguf";
pub const DEFAULT_PREMIUM_REPO: &str = "mradermacher/F2LLM-v2-1.7B-GGUF";
pub const DEFAULT_PREMIUM_FILENAME: &str = "F2LLM-v2-1.7B.Q6_K.gguf";

const LEGACY_MINILM_REPO: &str = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2";
const LEGACY_HARRIER_REPO: &str = "microsoft/harrier-oss-v1-0.6b";
const LEGACY_F2_REPO: &str = "codefuse-ai/F2LLM-v2-1.7B";

pub fn profile_spec(profile: EmbeddingProfile) -> EmbeddingProfileSpec {
    match profile {
        EmbeddingProfile::Base => EmbeddingProfileSpec {
            profile,
            repo: DEFAULT_BASE_REPO,
            filename: DEFAULT_BASE_FILENAME,
            dimension_hint: 1024,
            approx_size: "~397 MB",
            description: "portable F2LLM 0.6B Q4_K_M",
        },
        EmbeddingProfile::Dev => EmbeddingProfileSpec {
            profile,
            repo: DEFAULT_DEV_REPO,
            filename: DEFAULT_DEV_FILENAME,
            dimension_hint: 2048,
            approx_size: "~1.1 GB",
            description: "workstation F2LLM 1.7B Q4_K_M",
        },
        EmbeddingProfile::Premium => EmbeddingProfileSpec {
            profile,
            repo: DEFAULT_PREMIUM_REPO,
            filename: DEFAULT_PREMIUM_FILENAME,
            dimension_hint: 2048,
            approx_size: "~1.4 GB",
            description: "stronger F2LLM 1.7B Q6_K",
        },
    }
}

pub fn load_from_env() -> EmbeddingConfig {
    let mut cfg = EmbeddingConfig::default();

    if let Some(file_cfg) = load_config_file() {
        apply_section(&mut cfg, file_cfg);
    }

    if let Some(backend) =
        env_string("AICX_EMBEDDER_BACKEND").and_then(|raw| BackendPreference::parse(&raw))
    {
        cfg.backend = backend;
    }
    if let Some(profile) = env_string("AICX_EMBEDDER_PROFILE")
        .or_else(|| env_string("AICX_RUNTIME_PROFILE"))
        .and_then(|raw| EmbeddingProfile::parse(&raw))
    {
        cfg.profile = profile;
    }
    if let Some(repo) = env_string("AICX_EMBEDDER_REPO") {
        cfg.repo = Some(repo);
    }
    if let Some(filename) =
        env_string("AICX_EMBEDDER_FILENAME").or_else(|| env_string("AICX_EMBEDDER_FILE"))
    {
        cfg.filename = Some(filename);
    }
    if let Some(path) = env_string("AICX_EMBEDDER_PATH") {
        cfg.model_path = Some(PathBuf::from(path));
    }
    if let Some(max_length) = env_usize("AICX_EMBEDDER_MAX_LENGTH") {
        cfg.max_length = Some(max_length);
    }
    if let Some(threads) = env_i32("AICX_EMBEDDER_THREADS") {
        cfg.threads = Some(threads);
    }
    if let Some(gpu_layers) = env_u32("AICX_EMBEDDER_GPU_LAYERS") {
        cfg.gpu_layers = Some(gpu_layers);
    }
    if let Some(prefer_embedded) = env_bool("AICX_EMBEDDER_PREFER_EMBEDDED") {
        cfg.prefer_embedded = prefer_embedded;
    }

    cfg
}

pub fn resolve_model(cfg: &EmbeddingConfig) -> ResolvedEmbeddingModel {
    let spec = profile_spec(cfg.profile);
    let raw_repo = cfg.repo.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let raw_filename = cfg
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let legacy = raw_repo.is_some_and(is_legacy_non_gguf_repo) && raw_filename.is_none();

    if legacy {
        return ResolvedEmbeddingModel {
            profile: cfg.profile,
            repo: spec.repo.to_string(),
            filename: spec.filename.to_string(),
            dimension_hint: spec.dimension_hint,
            approx_size: spec.approx_size.to_string(),
            from_legacy_repo: true,
        };
    }

    ResolvedEmbeddingModel {
        profile: cfg.profile,
        repo: raw_repo.unwrap_or(spec.repo).to_string(),
        filename: raw_filename.unwrap_or(spec.filename).to_string(),
        dimension_hint: spec.dimension_hint,
        approx_size: spec.approx_size.to_string(),
        from_legacy_repo: false,
    }
}

/// Which precedence branch a config path came from. Lets the CLI
/// (`aicx config show`) name the winning branch without grepping the
/// resolved path back through the precedence list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    /// `$AICX_EMBEDDER_CONFIG` env override.
    Env,
    /// Canonical `<aicx_home>/config.toml`.
    Canonical,
    /// Legacy `<aicx_home>/embedder.toml`.
    Legacy,
    /// Bootstrap `$HOME/.aicx/config.toml` used to discover `[storage].home`.
    Bootstrap,
}

impl ConfigSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConfigSource::Env => "env",
            ConfigSource::Canonical => "canonical",
            ConfigSource::Legacy => "legacy",
            ConfigSource::Bootstrap => "bootstrap",
        }
    }
}

pub fn config_search_paths() -> Vec<PathBuf> {
    config_search_paths_with_source()
        .into_iter()
        .map(|(p, _)| p)
        .collect()
}

/// Same precedence as [`config_search_paths`] but each candidate is
/// tagged with the branch it came from. Order = priority; the first
/// existing file wins.
///
/// Root resolution mirrors `aicx::store::resolve_aicx_home`: `$AICX_HOME`
/// wins when set+non-empty, otherwise bootstrap `$HOME/.aicx/config.toml`
/// may provide `[storage].home`, otherwise `~/.aicx`. Duplicated locally
/// because aicx-embeddings is a leaf crate (the main `aicx` crate
/// depends on it, not the other way around).
pub fn config_search_paths_with_source() -> Vec<(PathBuf, ConfigSource)> {
    let env_home = std::env::var_os("AICX_HOME");
    let default_home = dirs::home_dir().map(|home| home.join(".aicx"));
    let effective_home = aicx_home_root_from(env_home.clone(), default_home.as_deref());
    build_search_paths(
        env_string("AICX_EMBEDDER_CONFIG").as_deref(),
        effective_home.as_deref(),
        default_home
            .as_deref()
            .filter(|_| should_include_bootstrap_home(&env_home)),
    )
}

/// Return the first candidate from [`config_search_paths_with_source`]
/// that actually exists on disk. `None` means no file was found and the
/// embedder runs on built-in defaults.
pub fn effective_config_source() -> Option<(PathBuf, ConfigSource)> {
    config_search_paths_with_source()
        .into_iter()
        .find(|(path, _)| path.exists())
}

/// Pure builder behind the env-reading public helpers. Kept separate so
/// tests can exercise all four precedence branches without mutating
/// process-global env vars.
fn build_search_paths(
    env_override: Option<&str>,
    aicx_home: Option<&Path>,
    bootstrap_home: Option<&Path>,
) -> Vec<(PathBuf, ConfigSource)> {
    let mut out = Vec::new();
    if let Some(path) = env_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        out.push((PathBuf::from(path), ConfigSource::Env));
    }
    if let Some(root) = aicx_home {
        out.push((root.join("config.toml"), ConfigSource::Canonical));
        out.push((root.join("embedder.toml"), ConfigSource::Legacy));
    }
    if let Some(root) = bootstrap_home {
        let bootstrap = root.join("config.toml");
        if !out.iter().any(|(path, _)| path == &bootstrap) {
            out.push((bootstrap, ConfigSource::Bootstrap));
        }
        let legacy = root.join("embedder.toml");
        if !out.iter().any(|(path, _)| path == &legacy) {
            out.push((legacy, ConfigSource::Legacy));
        }
    }
    out
}

fn should_include_bootstrap_home(env_home: &Option<std::ffi::OsString>) -> bool {
    env_home.as_ref().is_none_or(|value| value.is_empty())
}

/// Local mirror of `aicx::store::resolve_aicx_home`.
pub(crate) fn aicx_home_root() -> Option<PathBuf> {
    let default_home = dirs::home_dir().map(|home| home.join(".aicx"));
    aicx_home_root_from(std::env::var_os("AICX_HOME"), default_home.as_deref())
}

fn aicx_home_root_from(
    env_value: Option<std::ffi::OsString>,
    default_home: Option<&Path>,
) -> Option<PathBuf> {
    match env_value {
        Some(value) if !value.is_empty() => Some(PathBuf::from(value)),
        _ => {
            let default_home = default_home?;
            configured_home_from_bootstrap_config(default_home)
                .or_else(|| Some(default_home.to_path_buf()))
        }
    }
}

fn configured_home_from_bootstrap_config(default_home: &Path) -> Option<PathBuf> {
    let config_path = default_home.join("config.toml");
    if !config_path.exists() {
        return None;
    }
    let raw = match read_config_file_capped(&config_path, CONFIG_FILE_MAX_BYTES) {
        Ok(raw) => raw,
        Err(err) => {
            debug!(
                target: "aicx_embeddings::config",
                "failed to read bootstrap config {} for [storage].home: {}",
                config_path.display(),
                err
            );
            return None;
        }
    };
    let parsed: toml::Value = match toml::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            debug!(
                target: "aicx_embeddings::config",
                "failed to parse bootstrap config {} for [storage].home: {}",
                config_path.display(),
                err
            );
            return None;
        }
    };
    let value = parsed.get("storage")?.get("home")?.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    let home_dir = default_home.parent()?;
    let path = if value == "~" {
        home_dir.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home_dir.join(rest)
    } else {
        PathBuf::from(value)
    };
    path.is_absolute().then_some(path)
}

/// Hard upper bound for the embedder config file size. Realistic
/// `config.toml` files are < 4 KiB; 1 MiB is ~250× that and still small
/// enough that an accidental / hostile giant blob (e.g. a log rotated
/// onto the config path, a binary mistakenly renamed) cannot OOM the
/// embedder during startup. Bug #39.
pub(crate) const CONFIG_FILE_MAX_BYTES: u64 = 1024 * 1024;

/// Bounded replacement for `fs::read_to_string` on the embedder config
/// path. Reads up to `max_bytes + 1` so cap-hits are observable without
/// allocating the rest of the file. On cap-hit returns
/// `io::ErrorKind::FileTooLarge` so the caller can log at a higher level
/// than generic IO errors. Bug #39.
///
/// `path` comes from `config_search_paths()` — an internal resolver that
/// returns `$AICX_EMBEDDER_CONFIG`, effective-root config files, and the
/// bootstrap `~/.aicx/config.toml` fallback. It is not user input in the
/// path-traversal sense, but we still open via explicit
/// `OpenOptions::new().read(true)` to match the codebase's
/// pass-3 hardening pattern (see commits `9682007` and `095c988`).
pub(crate) fn read_config_file_capped(path: &Path, max_bytes: u64) -> io::Result<String> {
    let file = fs::OpenOptions::new().read(true).open(path)?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!(
                "embedder config '{}' exceeds {}-byte cap (read ≥ {} bytes; refusing to load)",
                path.display(),
                max_bytes,
                bytes.len()
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn load_config_file() -> Option<crate::NativeEmbedderConfigSection> {
    for path in config_search_paths() {
        if !path.exists() {
            continue;
        }
        let raw = match read_config_file_capped(&path, CONFIG_FILE_MAX_BYTES) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::FileTooLarge => {
                warn!(
                    target: "aicx_embeddings::config",
                    "refusing oversized embedder config {}: {}",
                    path.display(),
                    err
                );
                continue;
            }
            Err(err) => {
                debug!(
                    target: "aicx_embeddings::config",
                    "failed to read embedder config {}: {}",
                    path.display(),
                    err
                );
                continue;
            }
        };
        let parsed: NativeEmbedderConfigFile = match toml::from_str(&raw) {
            Ok(parsed) => parsed,
            Err(err) => {
                debug!(
                    target: "aicx_embeddings::config",
                    "failed to parse embedder config {}: {}",
                    path.display(),
                    err
                );
                continue;
            }
        };
        let mut merged = parsed.top_level;
        if let Some(section) = parsed.embedder {
            merged.merge_from(section);
        }
        if let Some(section) = parsed.native_embedder {
            merged.merge_from(section);
        }
        return Some(merged);
    }
    None
}

fn apply_section(cfg: &mut EmbeddingConfig, section: crate::NativeEmbedderConfigSection) {
    if let Some(backend) = section
        .backend
        .as_deref()
        .and_then(BackendPreference::parse)
    {
        cfg.backend = backend;
    }
    if let Some(profile) = section.profile.as_deref().and_then(EmbeddingProfile::parse) {
        cfg.profile = profile;
    }
    if let Some(repo) = non_empty(section.repo) {
        cfg.repo = Some(repo);
    }
    if let Some(filename) = non_empty(section.filename).or_else(|| non_empty(section.file)) {
        cfg.filename = Some(filename);
    }
    if section.path.is_some() {
        cfg.model_path = section.path;
    }
    if section.prefer_embedded.is_some() {
        cfg.prefer_embedded = section.prefer_embedded.unwrap_or(false);
    }
    if section.max_length.is_some() {
        cfg.max_length = section.max_length;
    }
    if section.threads.is_some() {
        cfg.threads = section.threads;
    }
    if section.gpu_layers.is_some() {
        cfg.gpu_layers = section.gpu_layers;
    }
    if section.cloud.is_some() {
        cfg.cloud = section.cloud;
    }
}

pub fn find_cached_model_file(repo: &str, filename: &str) -> Option<PathBuf> {
    crate::hf_cache::find_snapshot_with_file(repo, filename).map(|snapshot| snapshot.join(filename))
}

#[cfg(feature = "gguf")]
pub fn resolve_explicit_model_path(path: &Path, filename: Option<&str>) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if !path.is_dir() {
        return None;
    }
    if let Some(filename) = filename {
        let candidate = path.join(filename);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let mut candidates = fs::read_dir(path)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.into_iter().next()
}

fn is_legacy_non_gguf_repo(repo: &str) -> bool {
    let repo = repo.trim();
    repo.eq_ignore_ascii_case(LEGACY_MINILM_REPO)
        || repo.eq_ignore_ascii_case(LEGACY_HARRIER_REPO)
        || repo.eq_ignore_ascii_case(LEGACY_F2_REPO)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_string(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_usize(name: &str) -> Option<usize> {
    env_string(name).and_then(|value| value.parse().ok())
}

fn env_i32(name: &str) -> Option<i32> {
    env_string(name).and_then(|value| value.parse().ok())
}

fn env_u32(name: &str) -> Option<u32> {
    env_string(name).and_then(|value| value.parse().ok())
}

fn env_bool(name: &str) -> Option<bool> {
    env_string(name).and_then(|value| match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static UNIQ: AtomicUsize = AtomicUsize::new(0);

    fn tmp_dir(label: &str) -> PathBuf {
        let pid = std::process::id();
        let seq = UNIQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("aicx-cfg-test-{label}-{pid}-{seq}"));
        fs::create_dir_all(&dir).expect("create test temp dir");
        dir
    }

    fn first_existing(paths: Vec<(PathBuf, ConfigSource)>) -> Option<(PathBuf, ConfigSource)> {
        paths.into_iter().find(|(p, _)| p.exists())
    }

    #[test]
    fn env_branch_wins_when_override_file_exists() {
        let home = tmp_dir("env-branch-home");
        fs::write(home.join("config.toml"), "").unwrap();
        fs::write(home.join("embedder.toml"), "").unwrap();
        let override_file = home.join("override.toml");
        fs::write(&override_file, "").unwrap();

        let paths = build_search_paths(Some(override_file.to_str().unwrap()), Some(&home), None);
        let effective = first_existing(paths).expect("env override should win");
        assert_eq!(effective.1, ConfigSource::Env);
        assert_eq!(effective.0, override_file);
        assert_eq!(effective.1.as_str(), "env");
    }

    #[test]
    fn canonical_branch_wins_when_no_env_no_legacy_present() {
        let home = tmp_dir("canonical-branch-home");
        let canonical = home.join("config.toml");
        fs::write(&canonical, "").unwrap();

        let paths = build_search_paths(None, Some(&home), None);
        let effective = first_existing(paths).expect("canonical should win");
        assert_eq!(effective.1, ConfigSource::Canonical);
        assert_eq!(effective.0, canonical);
        assert_eq!(effective.1.as_str(), "canonical");
    }

    #[test]
    fn legacy_branch_wins_when_only_embedder_toml_exists() {
        let home = tmp_dir("legacy-branch-home");
        let legacy = home.join("embedder.toml");
        fs::write(&legacy, "").unwrap();

        let paths = build_search_paths(None, Some(&home), None);
        let effective = first_existing(paths).expect("legacy should win");
        assert_eq!(effective.1, ConfigSource::Legacy);
        assert_eq!(effective.0, legacy);
        assert_eq!(effective.1.as_str(), "legacy");
    }

    #[test]
    fn defaults_branch_when_no_file_present() {
        let home = tmp_dir("defaults-branch-home");

        let paths = build_search_paths(None, Some(&home), None);
        assert!(
            first_existing(paths).is_none(),
            "no file should match — embedder must fall back to built-in defaults"
        );
    }

    #[test]
    fn empty_env_override_is_ignored() {
        let home = tmp_dir("empty-env-home");
        fs::write(home.join("config.toml"), "").unwrap();

        let paths = build_search_paths(Some("   "), Some(&home), None);
        let effective = first_existing(paths).expect("canonical should still win");
        assert_eq!(effective.1, ConfigSource::Canonical);
    }

    #[test]
    fn canonical_outranks_legacy_when_both_present() {
        let home = tmp_dir("precedence-home");
        let canonical = home.join("config.toml");
        let legacy = home.join("embedder.toml");
        fs::write(&canonical, "").unwrap();
        fs::write(&legacy, "").unwrap();

        let paths = build_search_paths(None, Some(&home), None);
        let effective = first_existing(paths).expect("canonical should win");
        assert_eq!(effective.1, ConfigSource::Canonical);
        assert_eq!(effective.0, canonical);
    }

    #[test]
    fn bounded_read_rejects_oversized_config_with_path_and_cap_in_error() {
        let home = tmp_dir("bounded-oversize");
        let path = home.join("config.toml");
        // Cap small to keep the test fast; payload = cap + 1024 bytes.
        let cap: u64 = 2048;
        let payload = vec![b'x'; (cap as usize) + 1024];
        fs::write(&path, &payload).unwrap();

        let err = read_config_file_capped(&path, cap)
            .expect_err("oversized file must error under bounded read");
        assert_eq!(err.kind(), io::ErrorKind::FileTooLarge);
        let msg = err.to_string();
        assert!(
            msg.contains(&path.display().to_string()),
            "error must name the file path: {msg}",
        );
        assert!(
            msg.contains(&cap.to_string()),
            "error must name the cap value: {msg}",
        );
    }

    #[test]
    fn bounded_read_happy_path_loads_realistic_config() {
        let home = tmp_dir("bounded-happy");
        let path = home.join("config.toml");
        let body = "[embedder]\nbackend = \"native\"\nprofile = \"base\"\n";
        fs::write(&path, body).unwrap();

        let raw = read_config_file_capped(&path, CONFIG_FILE_MAX_BYTES)
            .expect("realistic config must load under 1 MiB cap");
        assert_eq!(raw, body);
    }

    #[test]
    fn bounded_read_empty_file_returns_empty_string() {
        let home = tmp_dir("bounded-empty");
        let path = home.join("config.toml");
        fs::write(&path, "").unwrap();

        let raw = read_config_file_capped(&path, CONFIG_FILE_MAX_BYTES)
            .expect("empty config must load cleanly");
        assert!(raw.is_empty(), "empty file must produce empty string");
        // toml parser must still accept an empty document — preserves the
        // pre-bounded-read behavior where an empty config = all defaults.
        let parsed: Result<NativeEmbedderConfigFile, _> = toml::from_str(&raw);
        assert!(parsed.is_ok(), "toml must accept empty document");
    }

    #[test]
    fn config_search_paths_back_compat_drops_source() {
        let home = tmp_dir("backcompat-home");
        let paths = build_search_paths(Some("/tmp/x.toml"), Some(&home), None);
        let plain: Vec<PathBuf> = paths.into_iter().map(|(p, _)| p).collect();
        assert_eq!(plain.len(), 3);
        assert_eq!(plain[0], PathBuf::from("/tmp/x.toml"));
        assert_eq!(plain[1], home.join("config.toml"));
        assert_eq!(plain[2], home.join("embedder.toml"));
    }

    #[test]
    fn bootstrap_config_can_relocate_aicx_home_and_remain_embedder_fallback() {
        let user_home = tmp_dir("bootstrap-user-home");
        let bootstrap_home = user_home.join(".aicx");
        let relocated = user_home.join("relocated-aicx");
        fs::create_dir_all(&bootstrap_home).unwrap();
        fs::write(
            bootstrap_home.join("config.toml"),
            format!(
                "[storage]\nhome = \"{}\"\n\n[embedder]\nbackend = \"cloud\"\n",
                relocated.display()
            ),
        )
        .unwrap();

        let effective = aicx_home_root_from(None, Some(&bootstrap_home))
            .expect("bootstrap storage.home should resolve");
        assert_eq!(effective, relocated);

        let paths = build_search_paths(None, Some(&effective), Some(&bootstrap_home));
        assert_eq!(
            paths[0],
            (relocated.join("config.toml"), ConfigSource::Canonical)
        );
        assert_eq!(
            paths[1],
            (relocated.join("embedder.toml"), ConfigSource::Legacy)
        );
        assert_eq!(
            paths[2],
            (bootstrap_home.join("config.toml"), ConfigSource::Bootstrap)
        );
    }

    #[test]
    fn explicit_aicx_home_env_wins_over_bootstrap_storage_home() {
        let user_home = tmp_dir("bootstrap-env-wins");
        let bootstrap_home = user_home.join(".aicx");
        let relocated = user_home.join("relocated-aicx");
        let pinned = user_home.join("env-pinned");
        fs::create_dir_all(&bootstrap_home).unwrap();
        fs::write(
            bootstrap_home.join("config.toml"),
            format!("[storage]\nhome = \"{}\"\n", relocated.display()),
        )
        .unwrap();

        let effective =
            aicx_home_root_from(Some(pinned.clone().into_os_string()), Some(&bootstrap_home))
                .expect("env-pinned root should resolve");
        assert_eq!(effective, pinned);
    }

    #[test]
    fn empty_aicx_home_env_still_allows_bootstrap_config() {
        assert!(should_include_bootstrap_home(&None));
        assert!(should_include_bootstrap_home(&Some(
            std::ffi::OsString::new()
        )));
        assert!(!should_include_bootstrap_home(&Some(
            std::ffi::OsString::from("/tmp/aicx")
        )));
    }
}
