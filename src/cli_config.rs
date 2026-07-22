use anyhow::{Context, Result};
use clap::Subcommand;
use serde::Serialize;
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const INSPECTION_SCHEMA: &str = "aicx.runtime_inspection.v1";
const CONFIG_READ_LIMIT: u64 = 1024 * 1024;
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// Inspect the exact running binary, install shadows, config, MCP target,
    /// embedder identity, and published index generation without changing them.
    Inspect {
        /// Emit the stable machine-readable inspection contract.
        #[arg(short = 'j', long)]
        json: bool,

        /// Inspect an external MCP client/mux config for its configured AICX target.
        /// Repeat for multiple clients. Files are read only and never rewritten.
        #[arg(long = "mcp-config", value_name = "PATH")]
        mcp_configs: Vec<PathBuf>,
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
        ConfigAction::Inspect { json, mcp_configs } => run_runtime_inspection(json, &mcp_configs),
    }
}

#[derive(Debug, Clone, Serialize)]
struct BuildIdentity {
    version: &'static str,
    semver: &'static str,
    git_commit: &'static str,
    dirty: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeIdentity {
    executable_path: String,
    build: BuildIdentity,
}

#[derive(Debug, Clone, Serialize)]
struct PathIdentity {
    path: String,
    source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct ConfigInspection {
    canonical_path: String,
    effective_path: Option<String>,
    source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct BinaryInspection {
    path: String,
    resolved_path: Option<String>,
    channel: &'static str,
    version: Option<String>,
    status: &'static str,
    recommendation: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct InstallationInspection {
    aicx: Vec<BinaryInspection>,
    aicx_mcp: Vec<BinaryInspection>,
}

#[derive(Debug, Clone, Serialize)]
struct ConfiguredMcpTarget {
    config_path: String,
    key_path: String,
    command: Option<String>,
    resolved_path: Option<String>,
    version: Option<String>,
    status: &'static str,
    recommendation: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct McpInspection {
    path_winner: Option<String>,
    configured_targets: Vec<ConfiguredMcpTarget>,
}

#[derive(Debug, Clone, Serialize)]
struct SecretSourceInspection {
    source: String,
    present: bool,
}

#[derive(Debug, Clone, Serialize)]
struct EmbedderInspection {
    backend: String,
    profile: String,
    model: String,
    dimension: usize,
    endpoint_origin: Option<String>,
    api_key: SecretSourceInspection,
}

#[derive(Debug, Clone, Serialize)]
struct IndexGenerationInspection {
    scope: &'static str,
    hybrid_root: String,
    generation: Option<String>,
    manifest_path: String,
    manifest_embedder_model: Option<String>,
    manifest_embedder_url_hash: Option<String>,
    status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeInspection {
    schema: &'static str,
    status: &'static str,
    runtime: RuntimeIdentity,
    aicx_home: PathIdentity,
    config: ConfigInspection,
    installations: InstallationInspection,
    mcp: McpInspection,
    embedder: EmbedderInspection,
    index: IndexGenerationInspection,
    actions: Vec<String>,
}

fn run_runtime_inspection(json: bool, mcp_configs: &[PathBuf]) -> Result<()> {
    let inspection = inspect_runtime(mcp_configs)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&inspection)?);
    } else {
        eprintln!("aicx config inspect — {}", inspection.status);
        eprintln!("  executable: {}", inspection.runtime.executable_path);
        eprintln!("  build:      {}", inspection.runtime.build.version);
        eprintln!("  AICX_HOME:  {}", inspection.aicx_home.path);
        eprintln!("  config:     {}", inspection.config.canonical_path);
        eprintln!(
            "  generation: {}",
            inspection.index.generation.as_deref().unwrap_or("<none>")
        );
        for action in inspection.actions {
            eprintln!("  action: {action}");
        }
    }
    Ok(())
}

fn inspect_runtime(mcp_configs: &[PathBuf]) -> Result<RuntimeInspection> {
    let executable = std::env::current_exe().context("resolve running aicx executable")?;
    let home = aicx::store::resolve_aicx_home().context("resolve AICX_HOME")?;
    let home_source = if std::env::var_os("AICX_HOME").is_some_and(|value| !value.is_empty()) {
        "env"
    } else {
        "bootstrap_or_default"
    };
    let canonical_path = home.join("config.toml");
    let effective = effective_config_inspection(&canonical_path);
    let aicx_candidates = inspect_binary_candidates("aicx", Some(&executable));
    let mcp_candidates = inspect_binary_candidates("aicx-mcp", None);
    let configured_targets = inspect_mcp_configs(mcp_configs);
    let path_winner = first_path_candidate("aicx-mcp").map(|path| path.display().to_string());
    let embedder = inspect_embedder();
    let index = inspect_index_generation(&home);

    let mut actions = Vec::new();
    for candidate in aicx_candidates.iter().chain(&mcp_candidates) {
        if candidate.status == "drift" {
            actions.push(format!(
                "{} reports {}; expected {} — reinstall or move the matching channel earlier on PATH",
                candidate.path,
                candidate.version.as_deref().unwrap_or("unknown"),
                aicx::BUILD_VERSION
            ));
        }
    }
    for target in &configured_targets {
        if let Some(recommendation) = &target.recommendation {
            actions.push(recommendation.clone());
        }
    }
    actions.sort();
    actions.dedup();
    let status = if aicx_candidates
        .iter()
        .chain(&mcp_candidates)
        .any(|candidate| candidate.status == "drift")
        || configured_targets.iter().any(|target| {
            matches!(
                target.status,
                "drift" | "missing" | "unavailable" | "invalid"
            )
        }) {
        "drift"
    } else {
        "ok"
    };

    Ok(RuntimeInspection {
        schema: INSPECTION_SCHEMA,
        status,
        runtime: RuntimeIdentity {
            executable_path: executable.display().to_string(),
            build: BuildIdentity {
                version: aicx::BUILD_VERSION,
                semver: env!("CARGO_PKG_VERSION"),
                git_commit: aicx::GIT_COMMIT,
                dirty: aicx::GIT_DIRTY,
            },
        },
        aicx_home: PathIdentity {
            path: home.display().to_string(),
            source: home_source,
        },
        config: effective,
        installations: InstallationInspection {
            aicx: aicx_candidates,
            aicx_mcp: mcp_candidates,
        },
        mcp: McpInspection {
            path_winner,
            configured_targets,
        },
        embedder,
        index,
        actions,
    })
}

fn effective_config_inspection(canonical_path: &Path) -> ConfigInspection {
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    {
        let effective = aicx::embedder::effective_config_source();
        ConfigInspection {
            canonical_path: canonical_path.display().to_string(),
            effective_path: effective
                .as_ref()
                .map(|(path, _)| path.display().to_string()),
            source: effective
                .as_ref()
                .map(|(_, source)| source.as_str())
                .unwrap_or("defaults"),
        }
    }
    #[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
    ConfigInspection {
        canonical_path: canonical_path.display().to_string(),
        effective_path: canonical_path
            .exists()
            .then(|| canonical_path.display().to_string()),
        source: if canonical_path.exists() {
            "canonical"
        } else {
            "defaults"
        },
    }
}

fn inspect_binary_candidates(name: &str, current: Option<&Path>) -> Vec<BinaryInspection> {
    let mut paths = Vec::new();
    if let Some(path) = current {
        paths.push(path.to_path_buf());
    }
    paths.extend(path_candidates(name));
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".cargo/bin").join(name));
        paths.push(home.join(".local/bin").join(name));
    }
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| (path.is_file() || path.is_symlink()) && seen.insert(path.clone()))
        .take(32)
        .map(|path| {
            let is_current = current.is_some_and(|current| current == path);
            let version = if is_current {
                Some(aicx::BUILD_VERSION.to_string())
            } else {
                probe_binary(&path)
            };
            let status = match version.as_deref() {
                Some(version) if version == aicx::BUILD_VERSION => "match",
                Some(_) => "drift",
                None => "unavailable",
            };
            BinaryInspection {
                resolved_path: fs::canonicalize(&path)
                    .ok()
                    .map(|resolved| resolved.display().to_string()),
                channel: classify_install_channel(&path),
                recommendation: (status == "unavailable").then(|| {
                    format!(
                        "{} could not answer --version within {}s; verify or remove the shadow manually",
                        path.display(),
                        VERSION_PROBE_TIMEOUT.as_secs()
                    )
                }),
                path: path.display().to_string(),
                version,
                status,
            }
        })
        .collect()
}

fn path_candidates(name: &str) -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .filter(|path| path.is_file() || path.is_symlink())
        .collect()
}

fn first_path_candidate(name: &str) -> Option<PathBuf> {
    path_candidates(name).into_iter().next()
}

fn classify_install_channel(path: &Path) -> &'static str {
    let text = path.to_string_lossy();
    let resolved = fs::canonicalize(path)
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    if text.contains("node_modules")
        || text.contains("/npm/")
        || resolved.contains("node_modules")
        || resolved.contains("/npm/")
    {
        "npm"
    } else if text.contains("/.cargo/bin/") {
        "cargo"
    } else if text.contains("/.local/bin/") {
        "local"
    } else if text.contains("/target/debug/") || text.contains("/target/release/") {
        "checkout"
    } else {
        "path"
    }
}

fn probe_binary(path: &Path) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    loop {
        match child.try_wait().ok()? {
            Some(status) => {
                let output = child.wait_with_output().ok()?;
                if !status.success() {
                    return None;
                }
                return version_token(&String::from_utf8_lossy(&output.stdout));
            }
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn version_token(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|token| {
            token.len() <= 128
                && token.chars().next().is_some_and(|ch| ch.is_ascii_digit())
                && token
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '+' | '-'))
        })
        .map(ToOwned::to_owned)
}

fn inspect_mcp_configs(paths: &[PathBuf]) -> Vec<ConfiguredMcpTarget> {
    let mut targets = Vec::new();
    for path in paths {
        let config_path = path.display().to_string();
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => {
                targets.push(ConfiguredMcpTarget {
                    config_path,
                    key_path: "<config>".to_string(),
                    command: None,
                    resolved_path: None,
                    version: None,
                    status: "unavailable",
                    recommendation: Some(format!(
                        "MCP config {} is unavailable ({:?}); fix access or pass a readable config path",
                        path.display(),
                        error.kind()
                    )),
                });
                continue;
            }
        };
        if metadata.len() > CONFIG_READ_LIMIT {
            targets.push(ConfiguredMcpTarget {
                config_path,
                key_path: "<config>".to_string(),
                command: None,
                resolved_path: None,
                version: None,
                status: "invalid",
                recommendation: Some(format!(
                    "MCP config {} exceeds the {} byte inspection limit",
                    path.display(),
                    CONFIG_READ_LIMIT
                )),
            });
            continue;
        }
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) => {
                targets.push(ConfiguredMcpTarget {
                    config_path,
                    key_path: "<config>".to_string(),
                    command: None,
                    resolved_path: None,
                    version: None,
                    status: "unavailable",
                    recommendation: Some(format!(
                        "MCP config {} could not be read ({:?}); inspection made no changes",
                        path.display(),
                        error.kind()
                    )),
                });
                continue;
            }
        };
        let value = serde_json::from_str::<serde_json::Value>(&raw)
            .ok()
            .or_else(|| {
                toml::from_str::<toml::Value>(&raw)
                    .ok()
                    .and_then(|value| serde_json::to_value(value).ok())
            });
        let Some(value) = value else {
            targets.push(ConfiguredMcpTarget {
                config_path,
                key_path: "<config>".to_string(),
                command: None,
                resolved_path: None,
                version: None,
                status: "invalid",
                recommendation: Some(format!(
                    "MCP config {} is neither valid JSON nor TOML",
                    path.display()
                )),
            });
            continue;
        };
        let mut commands = Vec::new();
        collect_mcp_commands(&value, &mut Vec::new(), &mut commands);
        if commands.is_empty() {
            targets.push(ConfiguredMcpTarget {
                config_path,
                key_path: "<config>".to_string(),
                command: None,
                resolved_path: None,
                version: None,
                status: "unavailable",
                recommendation: Some(format!(
                    "MCP config {} contains no identifiable AICX command target",
                    path.display()
                )),
            });
        } else {
            targets.extend(
                commands.into_iter().map(|(key_path, command)| {
                    inspect_configured_mcp_target(path, key_path, command)
                }),
            );
        }
    }
    targets
}

fn collect_mcp_commands(
    value: &serde_json::Value,
    path: &mut Vec<String>,
    commands: &mut Vec<(String, String)>,
) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                path.push(key.clone());
                if key == "command"
                    && let Some(command) = child.as_str()
                    && (command.to_ascii_lowercase().contains("aicx-mcp")
                        || path
                            .iter()
                            .any(|part| part.to_ascii_lowercase().contains("aicx")))
                {
                    commands.push((path.join("."), command.to_string()));
                } else {
                    collect_mcp_commands(child, path, commands);
                }
                path.pop();
            }
        }
        serde_json::Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                path.push(index.to_string());
                collect_mcp_commands(child, path, commands);
                path.pop();
            }
        }
        _ => {}
    }
}

fn inspect_configured_mcp_target(
    config_path: &Path,
    key_path: String,
    command: String,
) -> ConfiguredMcpTarget {
    let command_path = PathBuf::from(&command);
    let command_label = safe_command_label(&command);
    let is_direct_aicx_mcp = command_path
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| matches!(name, "aicx-mcp" | "aicx-mcp.exe"));
    if !is_direct_aicx_mcp {
        let resolved = first_path_candidate(&command);
        return ConfiguredMcpTarget {
            config_path: config_path.display().to_string(),
            key_path,
            command: Some(command_label.clone()),
            resolved_path: resolved.map(|path| path.display().to_string()),
            version: None,
            status: "unavailable",
            recommendation: Some(format!(
                "MCP config {} uses wrapper `{command_label}`; inspect that mux/proxy route manually because this read-only probe will not execute wrappers or guess their backing aicx-mcp",
                config_path.display()
            )),
        };
    }
    let resolved = if command_path.is_absolute() || command.contains(std::path::MAIN_SEPARATOR) {
        Some(command_path)
    } else {
        first_path_candidate(&command)
    };
    let Some(path) = resolved else {
        return ConfiguredMcpTarget {
            config_path: config_path.display().to_string(),
            key_path,
            command: Some(command_label.clone()),
            resolved_path: None,
            version: None,
            status: "missing",
            recommendation: Some(format!(
                "MCP config {} points at `{command_label}`, which is not executable on PATH; update it to the expected aicx-mcp {}",
                config_path.display(),
                aicx::BUILD_VERSION
            )),
        };
    };
    if !path.is_file() {
        return ConfiguredMcpTarget {
            config_path: config_path.display().to_string(),
            key_path,
            command: Some(command_label.clone()),
            resolved_path: Some(path.display().to_string()),
            version: None,
            status: "missing",
            recommendation: Some(format!(
                "MCP config {} points at missing executable {}; update the config manually to a matching aicx-mcp",
                config_path.display(),
                path.display()
            )),
        };
    }
    let version = probe_binary(&path);
    let status = match version.as_deref() {
        Some(version) if version == aicx::BUILD_VERSION => "match",
        Some(_) => "drift",
        None => "unavailable",
    };
    ConfiguredMcpTarget {
        config_path: config_path.display().to_string(),
        key_path,
        command: Some(command_label),
        resolved_path: Some(path.display().to_string()),
        recommendation: (status != "match").then(|| {
            format!(
                "MCP config {} resolves to {} ({}) but expected {}; update external config manually",
                config_path.display(),
                path.display(),
                version.as_deref().unwrap_or("version unavailable"),
                aicx::BUILD_VERSION
            )
        }),
        version,
        status,
    }
}

fn safe_command_label(command: &str) -> String {
    let lower = command.to_ascii_lowercase();
    if command.len() > 4096
        || command.chars().any(char::is_control)
        || command.contains("://")
        || command.contains(['?', '#', '='])
        || lower.contains("token")
        || lower.contains("secret")
    {
        "<redacted-command>".to_string()
    } else {
        command.to_string()
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn inspect_embedder() -> EmbedderInspection {
    let config = aicx::embedder::EmbeddingConfig::from_env();
    let resolved = config.resolved_model();
    let cloud = config.cloud.as_ref();
    let model = cloud
        .filter(|_| config.backend.as_str() == "cloud")
        .map(|cloud| cloud.model.clone())
        .unwrap_or_else(|| format!("{}/{}", resolved.repo, resolved.filename));
    let dimension = cloud
        .filter(|_| config.backend.as_str() == "cloud")
        .map(|cloud| cloud.effective_dimension())
        .unwrap_or(resolved.dimension_hint);
    let key_env = cloud.and_then(|cloud| cloud.api_key_env.as_deref());
    let safe_key_env = key_env.filter(|name| valid_env_name(name));
    EmbedderInspection {
        backend: config.backend.as_str().to_string(),
        profile: config.profile.as_str().to_string(),
        model,
        dimension,
        endpoint_origin: cloud.and_then(|cloud| sanitize_endpoint_origin(&cloud.url)),
        api_key: SecretSourceInspection {
            source: safe_key_env
                .map(|name| format!("env:{name}"))
                .unwrap_or_else(|| {
                    if key_env.is_some() {
                        "invalid_env_name".to_string()
                    } else {
                        "none".to_string()
                    }
                }),
            present: safe_key_env
                .is_some_and(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty())),
        },
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 128
        && chars
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
fn inspect_embedder() -> EmbedderInspection {
    EmbedderInspection {
        backend: "not_compiled".to_string(),
        profile: "unavailable".to_string(),
        model: "unavailable".to_string(),
        dimension: 0,
        endpoint_origin: None,
        api_key: SecretSourceInspection {
            source: "none".to_string(),
            present: false,
        },
    }
}

fn sanitize_endpoint_origin(url: &str) -> Option<String> {
    let (scheme, remainder) = url.trim().split_once("://")?;
    if !scheme
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
    {
        return None;
    }
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(authority);
    (!host.is_empty()).then(|| format!("{}://{}", scheme.to_ascii_lowercase(), host))
}

fn inspect_index_generation(home: &Path) -> IndexGenerationInspection {
    let hybrid_root = home.join("indexed/_all/hybrid");
    let current = hybrid_root.join("CURRENT");
    let generation = fs::metadata(&current)
        .ok()
        .filter(|metadata| metadata.len() <= 129)
        .and_then(|_| fs::read_to_string(&current).ok())
        .map(|value| value.trim().to_string())
        .filter(|name| valid_generation_name(name));
    let generation_dir = generation
        .as_ref()
        .map(|name| hybrid_root.join("generations").join(name))
        .filter(|path| path.is_dir());
    let manifest_path = generation_dir
        .as_ref()
        .unwrap_or(&hybrid_root)
        .join("manifest.json");
    let manifest = read_json_capped(&manifest_path, CONFIG_READ_LIMIT);
    let status = if generation.is_some() && generation_dir.is_some() && manifest.is_some() {
        "active"
    } else if manifest.is_some() {
        "legacy"
    } else {
        "missing"
    };
    IndexGenerationInspection {
        scope: "_all",
        hybrid_root: hybrid_root.display().to_string(),
        generation,
        manifest_path: manifest_path.display().to_string(),
        manifest_embedder_model: manifest
            .as_ref()
            .and_then(|value| value.get("embedder_model"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        manifest_embedder_url_hash: manifest
            .as_ref()
            .and_then(|value| value.get("embedder_url_hash"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        status,
    }
}

fn valid_generation_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn read_json_capped(path: &Path, limit: u64) -> Option<serde_json::Value> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > limit {
        return None;
    }
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
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
