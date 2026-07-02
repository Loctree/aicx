use anyhow::{Context, Result};
use clap::Subcommand;
use std::path::PathBuf;

/// Subcommands for `aicx config`.
#[derive(Debug, Clone, Subcommand)]
pub(crate) enum ConfigAction {
    /// Write a default `~/.aicx/config.toml` with cloud-embedder
    /// pre-selected. Bails if the file exists unless `--force`.
    Init {
        /// Overwrite the existing config file if present.
        #[arg(long)]
        force: bool,

        /// Write to a custom path instead of `~/.aicx/config.toml`.
        /// Useful for shared / repo-local config snapshots.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Display the resolved embedder configuration after merging env,
    /// `embedder.toml`, `config.toml`, and built-in defaults.
    Show {
        /// Emit JSON instead of human-readable text.
        #[arg(short = 'j', long)]
        json: bool,
    },
}

/// Default canonical config template written by `aicx config init`.
///
/// Set up to advertise cloud-embedder as the recommended Vetcoders
/// production default with concrete provider examples; the native GGUF
/// section ships fully-commented so operators can flip backends without
/// hunting for the schema.
const DEFAULT_CONFIG_TOML: &str = r#"# aicx — Vibecrafted with AI Agents (c)2026 Vetcoders
#
# Canonical AICX configuration. Loaded by `aicx` (CLI), `aicx-mcp`,
# and any in-process consumer of the embedder. Config file search order:
#   1. AICX_EMBEDDER_CONFIG env var  (explicit path override)
#   2. <effective-aicx-home>/config.toml   (canonical)
#   3. <effective-aicx-home>/embedder.toml (legacy, native fields only)
# Per-field AICX_EMBEDDER_* env vars override values loaded from the file.
#
# Edit and re-save. No restart needed; aicx reloads on every invocation.

[storage]
# Optional persistent AICX root override. `AICX_HOME` env still wins for
# one-shot commands; when env is unset this value moves the whole runtime
# root (`store/`, `indexed/`, `state/`, `embeddings/`) away from ~/.aicx.
# Use an absolute path or ~/...
# home = "~/aicx"

[embedder]
# Recommended Vetcoders default: cloud HTTP embedder, zero-install,
# config-driven URL/model/API key. Switch to "gguf" for offline / dev
# workstations with native llama.cpp inference. Use "auto" to let the
# binary pick the strongest compiled-in backend.
backend = "cloud"

# Native GGUF profile (only consulted when backend = "gguf" or "auto"):
#   "base"    — F2LLM 0.6B Q4_K_M  (~397 MB, 1024 dim)
#   "dev"     — F2LLM 1.7B Q4_K_M  (~1.1 GB, 2048 dim)
#   "premium" — F2LLM 1.7B Q6_K    (~1.4 GB, 2048 dim)
profile = "base"

[embedder.cloud]
# OpenAI-compatible /v1/embeddings endpoint. Replace with your provider.
#   OpenAI:           https://api.openai.com/v1/embeddings
#   Voyage AI:        https://api.voyageai.com/v1/embeddings
#   Together AI:      https://api.together.xyz/v1/embeddings
#   OpenRouter:       https://openrouter.ai/api/v1/embeddings
#   Ollama local:     http://localhost:11434/v1/embeddings
#   Local LM Studio:  http://localhost:1234/v1/embeddings
#
# Local provider caveat: Ollama measured ~38s first-call coldstart
# from idle on 2026-05-06, then warm calls are much faster. Local
# providers are excellent for batched `aicx index` workflows where
# startup amortizes over many chunks. For one-shot CLI search, remote
# cloud providers usually feel faster. Run `aicx warmup` after idle to
# pre-load local daemons before an interactive search session.
url = "https://api.openai.com/v1/embeddings"

# Model identifier as accepted by the provider:
#   OpenAI:    text-embedding-3-small (1536 dim) | text-embedding-3-large (3072 dim)
#   Voyage:    voyage-3 (1024 dim) | voyage-large-2 (1536 dim)
#   Together:  BAAI/bge-large-en-v1.5 (1024 dim)
model = "text-embedding-3-small"

# Env var name holding the API key. Resolved at call time so secrets
# never sit in config files. Set the env var before running aicx:
#   export OPENAI_API_KEY=sk-...
api_key_env = "OPENAI_API_KEY"

# Output dimension (informational; some providers do not echo it).
dimension = 1536

# Request timeout in seconds.
timeout_secs = 30

# Optional extra headers (rarely needed; uncomment to use):
# [embedder.cloud.headers]
# "X-Trace-Id" = "vetcoders-aicx"
"#;

fn canonical_config_path() -> Result<PathBuf> {
    Ok(aicx::store::resolve_aicx_home()
        .context("cannot resolve AICX home for config.toml")?
        .join("config.toml"))
}

/// Dispatch `aicx config <action>`.
pub(crate) fn run_config(action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Init { force, path } => run_config_init(force, path),
        ConfigAction::Show { json } => run_config_show(json),
    }
}

/// Write the canonical config.toml template, refusing to overwrite
/// without `--force` so an operator never loses hand-tuned settings to
/// a stray init.
fn run_config_init(force: bool, path: Option<PathBuf>) -> Result<()> {
    let target = match path {
        Some(p) => p,
        None => canonical_config_path()?,
    };

    if target.exists() && !force {
        anyhow::bail!(
            "config file already exists at {}; pass --force to overwrite, or edit it directly",
            target.display()
        );
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create config directory at {}", parent.display())
        })?;
    }

    std::fs::write(&target, DEFAULT_CONFIG_TOML)
        .with_context(|| format!("failed to write config to {}", target.display()))?;

    eprintln!("aicx config init -> wrote {}", target.display());
    eprintln!("Edit it to set your endpoint / model / API key env var, then:");
    eprintln!("  export OPENAI_API_KEY=sk-...   # or your provider equivalent");
    eprintln!("  aicx search 'your query'");

    Ok(())
}

/// Print the resolved [`aicx_parser`]-compatible embedder config so the
/// operator can verify what backend / model / dimension will actually
/// run for the next `aicx search`.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn run_config_show(json: bool) -> Result<()> {
    let cfg = aicx::embedder::EmbeddingConfig::from_env();
    let resolved = cfg.resolved_model();
    let cloud_set = cfg.cloud.is_some();

    let canonical_path = canonical_config_path().ok();
    let resolved_aicx_home = aicx::store::resolve_aicx_home().ok();
    let effective = aicx::embedder::effective_config_source();
    let (effective_path_display, effective_branch, marker_line) =
        describe_effective_config(&effective);

    // HF cache probing: surface whether the configured profile is hydrated
    // and which other profiles (if any) have a usable snapshot already.
    // Lets operators recover from the "runtime default base 0.6B missing,
    // premium 1.7B already cached" situation without grep-debugging.
    let current_cache_path = aicx::embedder::hf_cache::snapshot_path_for_profile(cfg.profile);
    let cache_present = current_cache_path.is_some();
    let cached_profiles = aicx::embedder::hf_cache::detect_cached_profiles();
    let suggested_profile = if !cache_present {
        cached_profiles.iter().find(|&&p| p != cfg.profile).copied()
    } else {
        None
    };

    if json {
        let payload = serde_json::json!({
            "backend": cfg.backend.as_str(),
            "profile": cfg.profile.as_str(),
            "resolved_native": {
                "repo": resolved.repo,
                "filename": resolved.filename,
                "dimension_hint": resolved.dimension_hint,
                "approx_size": resolved.approx_size,
                "from_legacy_repo": resolved.from_legacy_repo,
                "cache_present": cache_present,
                "cache_path": current_cache_path.as_ref().map(|p| p.display().to_string()),
            },
            "cloud": cfg.cloud.as_ref().map(|c| serde_json::json!({
                "url": c.url,
                "model": c.model,
                "api_key_env": c.api_key_env,
                "dimension": c.effective_dimension(),
                "timeout_secs": c.effective_timeout_secs(),
            })),
            "canonical_config_path": canonical_path.as_ref().map(|p| p.display().to_string()),
            "resolved_aicx_home": resolved_aicx_home.as_ref().map(|p| p.display().to_string()),
            "effective_config_path": effective.as_ref().map(|(p, _)| p.display().to_string()),
            "effective_branch": effective_branch,
            "config_path": canonical_path.as_ref().map(|p| p.display().to_string()),
            "cloud_section_present": cloud_set,
            "available_cached_profiles": cached_profiles.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
            "suggested_profile": suggested_profile.map(|p| p.as_str()),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    let canonical_display = canonical_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unresolved>".to_string());

    eprintln!("aicx config show — resolved embedder configuration");
    if let Some(path) = &resolved_aicx_home {
        eprintln!("  resolved_aicx_home:    {}", path.display());
    }
    eprintln!("  canonical_config_path: {canonical_display}");
    eprintln!("  effective_config_path: {effective_path_display}");
    eprintln!("  effective_branch:      {effective_branch}");
    eprintln!("  {marker_line}");
    eprintln!("  backend:     {}", cfg.backend.as_str());
    eprintln!("  profile:     {}", cfg.profile.as_str());
    eprintln!("  native.repo:           {}", resolved.repo);
    eprintln!("  native.filename:       {}", resolved.filename);
    eprintln!("  native.dimension_hint: {}", resolved.dimension_hint);
    eprintln!("  native.approx_size:    {}", resolved.approx_size);
    if resolved.from_legacy_repo {
        eprintln!("  native.from_legacy_repo: true (auto-mapped to F2LLM GGUF)");
    }
    eprintln!("  native.cache_present:  {cache_present}");
    if let Some(path) = &current_cache_path {
        eprintln!("  native.cache_path:     {}", path.display());
    }
    if !cached_profiles.is_empty() {
        let names: Vec<&str> = cached_profiles.iter().map(|p| p.as_str()).collect();
        eprintln!("  available_cached_profiles: {}", names.join(", "));
    } else {
        eprintln!("  available_cached_profiles: <none — run `hf download` first>");
    }
    if let Some(sug) = suggested_profile {
        let sug_name = sug.as_str();
        eprintln!(
            "  suggested_profile:     {sug_name} (HF cache has it hydrated — set \
             `AICX_EMBEDDER_PROFILE={sug_name}` or `profile = \"{sug_name}\"` in config.toml)"
        );
    }
    if let Some(cloud) = &cfg.cloud {
        eprintln!("  cloud.url:           {}", cloud.url);
        eprintln!("  cloud.model:         {}", cloud.model);
        eprintln!(
            "  cloud.api_key_env:   {}",
            cloud.api_key_env.as_deref().unwrap_or("<none>")
        );
        eprintln!("  cloud.dimension:     {}", cloud.effective_dimension());
        eprintln!("  cloud.timeout_secs:  {}", cloud.effective_timeout_secs());
    } else {
        eprintln!("  cloud:               <none> (run `aicx config init` to bootstrap)");
    }
    Ok(())
}

/// Render the human-readable `(effective_path, branch_name, marker_line)`
/// triple used by both the plain-text and JSON paths of `aicx config show`.
/// `None` means no config file was found and the embedder runs on built-in
/// defaults — the marker then nudges `aicx config init`.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub(crate) fn describe_effective_config(
    effective: &Option<(PathBuf, aicx::embedder::ConfigSource)>,
) -> (String, &'static str, String) {
    use aicx::embedder::ConfigSource;
    match effective {
        Some((path, ConfigSource::Env)) => (
            path.display().to_string(),
            "env",
            format!(
                "(loaded from: env $AICX_EMBEDDER_CONFIG -> {})",
                path.display()
            ),
        ),
        Some((path, ConfigSource::Canonical)) => (
            path.display().to_string(),
            "canonical",
            format!("(loaded from: canonical -> {})", path.display()),
        ),
        Some((path, ConfigSource::Legacy)) => (
            path.display().to_string(),
            "legacy",
            format!(
                "(loaded from: legacy embedder.toml -> {} — run `aicx config init` to migrate to canonical config.toml)",
                path.display()
            ),
        ),
        Some((path, ConfigSource::Bootstrap)) => (
            path.display().to_string(),
            "bootstrap",
            format!(
                "(loaded from: bootstrap ~/.aicx/config.toml -> {}; use [storage].home to relocate the runtime root)",
                path.display()
            ),
        ),
        None => (
            "<built-in defaults>".to_string(),
            "defaults",
            "(no config file found; using built-in defaults — run `aicx config init` to materialize one)"
                .to_string(),
        ),
    }
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
fn run_config_show(_json: bool) -> Result<()> {
    eprintln!(
        "aicx was built without any embedder feature. \
         Install a pre-built release (e.g., `npm install -g @loctree/aicx`), \
         or rebuild with `cargo install --features cloud-embedder` (recommended) \
         or `--features native-embedder` (offline GGUF)."
    );
    Ok(())
}
