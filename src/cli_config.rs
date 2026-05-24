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
/// Set up to advertise cloud-embedder as the recommended VetCoders
/// production default with concrete provider examples; the native GGUF
/// section ships fully-commented so operators can flip backends without
/// hunting for the schema.
const DEFAULT_CONFIG_TOML: &str = r#"# aicx — Vibecrafted with AI Agents (c)2026 VetCoders
#
# Canonical AICX configuration. Loaded by `aicx` (CLI), `aicx-mcp`,
# and any in-process consumer of the embedder. Field precedence
# (highest first):
#   1. AICX_EMBEDDER_CONFIG env var  (explicit path override)
#   2. ~/.aicx/embedder.toml          (legacy, native fields only)
#   3. ~/.aicx/config.toml            (this file — canonical)
#   4. AICX_EMBEDDER_*                (per-field env overrides)
#
# Edit and re-save. No restart needed; aicx reloads on every invocation.

[embedder]
# Recommended VetCoders default: cloud HTTP embedder, zero-install,
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
            },
            "cloud": cfg.cloud.as_ref().map(|c| serde_json::json!({
                "url": c.url,
                "model": c.model,
                "api_key_env": c.api_key_env,
                "dimension": c.effective_dimension(),
                "timeout_secs": c.effective_timeout_secs(),
            })),
            "config_path": canonical_config_path().ok().map(|p| p.display().to_string()),
            "cloud_section_present": cloud_set,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    let path_display = canonical_config_path()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unresolved>".to_string());

    eprintln!("aicx config show — resolved embedder configuration");
    eprintln!("  config_path: {path_display}");
    eprintln!("  backend:     {}", cfg.backend.as_str());
    eprintln!("  profile:     {}", cfg.profile.as_str());
    eprintln!("  native.repo:           {}", resolved.repo);
    eprintln!("  native.filename:       {}", resolved.filename);
    eprintln!("  native.dimension_hint: {}", resolved.dimension_hint);
    eprintln!("  native.approx_size:    {}", resolved.approx_size);
    if resolved.from_legacy_repo {
        eprintln!("  native.from_legacy_repo: true (auto-mapped to F2LLM GGUF)");
    }
    if let Some(cloud) = &cfg.cloud {
        eprintln!("  cloud.url:           {}", cloud.url);
        eprintln!("  cloud.model:         {}", cloud.model);
        eprintln!(
            "  cloud.api_key_env:   {}",
            cloud.api_key_env.as_deref().unwrap_or("<unset>")
        );
        eprintln!("  cloud.dimension:     {}", cloud.effective_dimension());
        eprintln!("  cloud.timeout_secs:  {}", cloud.effective_timeout_secs());
    } else {
        eprintln!("  cloud:               <not configured> (run `aicx config init` to bootstrap)");
    }
    Ok(())
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
