//! Memex integration — the optional semantic index behind `aicx memex-sync` and `--memex`.
//!
//! This module is the boundary between the aicx orchestrator and the published
//! `rmcp-memex` 0.5.0 library. Live ai-contexters flows stay inside that
//! library boundary:
//!
//! - Config discovery, embedding resolution, and content hashing
//! - Canonical chunk materialization via published storage + BM25 APIs
//! - Read-only BM25 search + LanceDB document lookups without subprocesses
//! - Explicit embedding-dimension/reindex mismatch detection at the boundary
//!
//! This module does not shell out to an `rmcp-memex` binary. The only CLI-shaped
//! behavior here is the human-facing rebuild command string embedded in
//! compatibility errors so operators know which `aicx memex-sync --reindex`
//! command to run.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use rmcp_memex::{
    BM25Config, ChromaDocument, DEFAULT_REQUIRED_DIMENSION, EmbeddingClient,
    EmbeddingConfig as RmcpEmbeddingConfig, MlxConfig, MlxMergeOptions,
    ProviderConfig as RmcpProviderConfig, RerankerConfig as RmcpRerankerConfig, StorageManager,
    compute_content_hash, search::BM25Index,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sanitize;

// ============================================================================
// Configuration
// ============================================================================

const DEFAULT_MEMEX_NAMESPACE: &str = "ai-contexts";
const SEMANTIC_INDEX_METADATA_VERSION: u32 = 1;
/// aicx-owned config paths. No fallback to memex server config.
const AICX_CONFIG_SEARCH_PATHS: &[&str] = &["~/.aicx/memex/config.toml", "~/.aicx/config.toml"];
const AICX_RUNTIME_PROFILE_ENV: &str = "AICX_RUNTIME_PROFILE";
const LEGACY_MLX_ENV_VARS: &[&str] = &[
    "DISABLE_MLX",
    "EMBEDDER_PORT",
    "DRAGON_BASE_URL",
    "DRAGON_EMBEDDER_PORT",
    "RERANKER_PORT",
    "EMBEDDER_MODEL",
    "RERANKER_MODEL",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum MemexRuntimeProfile {
    Base,
    Dev,
    Premium,
}

impl Default for MemexRuntimeProfile {
    fn default() -> Self {
        Self::Base
    }
}

impl fmt::Display for MemexRuntimeProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Base => "base",
            Self::Dev => "dev",
            Self::Premium => "premium",
        })
    }
}

impl MemexRuntimeProfile {
    fn label(self) -> &'static str {
        match self {
            Self::Base => "portable 1024-dim Qwen 0.6B preset",
            Self::Dev => "legacy 2560-dim Qwen 4B preset",
            Self::Premium => "heavy 4096-dim Qwen 8B preset",
        }
    }

    fn required_dimension(self) -> usize {
        match self {
            Self::Base => 1024,
            Self::Dev => 2560,
            Self::Premium => 4096,
        }
    }

    fn local_model(self) -> &'static str {
        match self {
            Self::Base => "qwen3-embedding:0.6b",
            Self::Dev => "qwen3-embedding:4b",
            Self::Premium => "qwen3-embedding:8b",
        }
    }

    fn dragon_model(self) -> &'static str {
        match self {
            Self::Base => "Qwen/Qwen3-Embedding-0.6B",
            Self::Dev => "Qwen/Qwen3-Embedding-4B",
            Self::Premium => "Qwen/Qwen3-Embedding-8B",
        }
    }

    fn matches_runtime(self, model: &str, dimension: usize) -> bool {
        dimension == self.required_dimension()
            && matches!(
                model,
                value if value.eq_ignore_ascii_case(self.local_model())
                    || value.eq_ignore_ascii_case(self.dragon_model())
            )
    }

    fn detect(model: &str, dimension: usize) -> Option<Self> {
        [Self::Base, Self::Dev, Self::Premium]
            .into_iter()
            .find(|profile| profile.matches_runtime(model, dimension))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeProfileSource {
    Default,
    Config,
    Env,
    Cli,
}

/// Resolved rmcp-memex runtime truth as seen by ai-contexters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemexRuntimeTruth {
    pub db_path: PathBuf,
    pub bm25_path: PathBuf,
    pub embedding_model: String,
    pub embedding_dimension: usize,
    pub config_path: Option<PathBuf>,
    pub runtime_profile: Option<MemexRuntimeProfile>,
    pub runtime_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SemanticIndexMetadata {
    format_version: u32,
    namespace: String,
    db_path: String,
    bm25_path: String,
    embedding_model: String,
    embedding_dimension: usize,
    updated_at: DateTime<Utc>,
}

#[derive(Debug)]
struct MemexCompatibilityError {
    message: String,
}

impl fmt::Display for MemexCompatibilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MemexCompatibilityError {}

#[derive(Debug, Default, Deserialize)]
struct MemexFileConfig {
    db_path: Option<String>,
    #[serde(default)]
    runtime: Option<MemexRuntimeFileConfig>,
    #[serde(default)]
    embeddings: Option<MemexEmbeddingsFileConfig>,
    #[serde(default)]
    mlx: Option<MemexMlxFileConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct MemexRuntimeFileConfig {
    profile: Option<MemexRuntimeProfile>,
}

#[derive(Debug, Clone, Deserialize)]
struct MemexEmbeddingsFileConfig {
    #[serde(default = "default_required_dimension")]
    required_dimension: usize,
    #[serde(default)]
    providers: Vec<MemexProviderFileConfig>,
    #[serde(default)]
    reranker: Option<MemexRerankerFileConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct MemexProviderFileConfig {
    name: String,
    base_url: String,
    model: String,
    #[serde(default = "default_provider_priority")]
    priority: u8,
    #[serde(default = "default_provider_endpoint")]
    endpoint: String,
}

impl MemexProviderFileConfig {
    fn to_rmcp_provider_config(&self) -> RmcpProviderConfig {
        RmcpProviderConfig {
            name: self.name.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            priority: self.priority,
            endpoint: self.endpoint.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct MemexRerankerFileConfig {
    base_url: String,
    model: String,
    #[serde(default = "default_reranker_endpoint")]
    endpoint: String,
}

impl MemexRerankerFileConfig {
    fn to_rmcp_reranker_config(&self) -> RmcpRerankerConfig {
        RmcpRerankerConfig {
            base_url: Some(self.base_url.clone()),
            model: Some(self.model.clone()),
            endpoint: self.endpoint.clone(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct MemexMlxFileConfig {
    #[serde(default)]
    disabled: bool,
    local_port: Option<u16>,
    dragon_url: Option<String>,
    dragon_port: Option<u16>,
    embedder_model: Option<String>,
    reranker_model: Option<String>,
    reranker_port_offset: Option<u16>,
}

impl MemexMlxFileConfig {
    fn to_mlx_config(&self) -> MlxConfig {
        let mut config = MlxConfig::from_env();
        config.merge_file_config(MlxMergeOptions {
            disabled: Some(self.disabled),
            local_port: self.local_port,
            dragon_url: self.dragon_url.clone(),
            dragon_port: self.dragon_port,
            embedder_model: self.embedder_model.clone(),
            reranker_model: self.reranker_model.clone(),
            reranker_port_offset: self.reranker_port_offset,
        });
        config
    }
}

/// Configuration for memex integration.
#[derive(Debug, Clone)]
pub struct MemexConfig {
    /// Namespace in vector store (default: "ai-contexts")
    pub namespace: String,
    /// Override LanceDB path if needed
    pub db_path: Option<PathBuf>,
    /// Use batched library-backed stores (true) or per-chunk library writes (false).
    pub batch_mode: bool,
    /// Compatibility flag retained for older callers and CLI surface stability.
    ///
    /// The published `rmcp-memex` library boundary does not consume this value,
    /// so live ai-contexters sync paths ignore it.
    pub preprocess: bool,
    /// Optional runtime profile override for this sync.
    pub runtime_profile: Option<MemexRuntimeProfile>,
}

impl Default for MemexConfig {
    fn default() -> Self {
        Self {
            namespace: DEFAULT_MEMEX_NAMESPACE.to_string(),
            db_path: None,
            batch_mode: true,
            preprocess: true,
            runtime_profile: None,
        }
    }
}

fn memex_state_dir() -> Result<PathBuf> {
    let dir = crate::store::store_base_dir()?.join("memex");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn expand_home_path(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }

    if let Some(stripped) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }

    PathBuf::from(raw)
}

fn default_memex_db_path() -> PathBuf {
    expand_home_path("~/.aicx/lancedb")
}

fn default_memex_bm25_path() -> PathBuf {
    expand_home_path("~/.aicx/bm25")
}

fn default_required_dimension() -> usize {
    DEFAULT_REQUIRED_DIMENSION
}

fn default_provider_priority() -> u8 {
    RmcpProviderConfig::default().priority
}

fn default_provider_endpoint() -> String {
    RmcpProviderConfig::default().endpoint
}

fn default_reranker_endpoint() -> String {
    "/v1/rerank".to_string()
}

fn discover_memex_config_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("RMCP_MEMEX_CONFIG") {
        let expanded = expand_home_path(&path);
        if expanded.exists() {
            return Some(expanded);
        }
    }

    AICX_CONFIG_SEARCH_PATHS
        .iter()
        .map(|path| expand_home_path(path))
        .find(|path| path.exists())
}

fn load_memex_file_config(config_path: Option<&Path>) -> Result<Option<MemexFileConfig>> {
    let Some(config_path) = config_path else {
        return Ok(None);
    };

    let raw = fs::read_to_string(config_path).with_context(|| {
        format!(
            "Failed to read rmcp-memex config at {}",
            config_path.display()
        )
    })?;

    toml::from_str(&raw)
        .with_context(|| {
            format!(
                "Failed to parse rmcp-memex config at {}",
                config_path.display()
            )
        })
        .map(Some)
}

fn resolve_embedding_config(file_cfg: &MemexFileConfig) -> RmcpEmbeddingConfig {
    if let Some(embeddings) = file_cfg.embeddings.as_ref() {
        let mut config = if embeddings.providers.is_empty() {
            file_cfg.mlx.as_ref().map_or_else(
                || MlxConfig::from_env().to_embedding_config(),
                |mlx| mlx.to_mlx_config().to_embedding_config(),
            )
        } else {
            RmcpEmbeddingConfig::default()
        };

        config.required_dimension = embeddings.required_dimension;

        if !embeddings.providers.is_empty() {
            config.providers = embeddings
                .providers
                .iter()
                .map(MemexProviderFileConfig::to_rmcp_provider_config)
                .collect();
        }

        if let Some(reranker) = embeddings.reranker.as_ref() {
            config.reranker = reranker.to_rmcp_reranker_config();
        }

        return config;
    }

    file_cfg.mlx.as_ref().map_or_else(
        || MlxConfig::from_env().to_embedding_config(),
        |mlx| mlx.to_mlx_config().to_embedding_config(),
    )
}

fn legacy_mlx_env_override_present() -> bool {
    LEGACY_MLX_ENV_VARS
        .iter()
        .any(|key| std::env::var_os(key).is_some())
}

fn runtime_profile_from_env() -> Result<Option<MemexRuntimeProfile>> {
    let Some(raw) = std::env::var_os(AICX_RUNTIME_PROFILE_ENV) else {
        return Ok(None);
    };

    let raw = raw.to_string_lossy();
    MemexRuntimeProfile::from_str(raw.trim(), true).map(Some).map_err(|_| {
        anyhow::anyhow!(
            "Unsupported {AICX_RUNTIME_PROFILE_ENV}='{raw}'. Expected one of: base, dev, premium."
        )
    })
}

fn merged_mlx_config(file_cfg: Option<&MemexMlxFileConfig>) -> MlxConfig {
    let mut config = MlxConfig::from_env();
    if let Some(mlx) = file_cfg {
        config.merge_file_config(MlxMergeOptions {
            disabled: Some(mlx.disabled),
            local_port: mlx.local_port,
            dragon_url: mlx.dragon_url.clone(),
            dragon_port: mlx.dragon_port,
            embedder_model: mlx.embedder_model.clone(),
            reranker_model: mlx.reranker_model.clone(),
            reranker_port_offset: mlx.reranker_port_offset,
        });
    }
    config
}

fn runtime_profile_embedding_config(
    profile: MemexRuntimeProfile,
    mlx: &MlxConfig,
) -> RmcpEmbeddingConfig {
    let reranker_port = mlx.local_port + mlx.reranker_port_offset;

    RmcpEmbeddingConfig {
        required_dimension: profile.required_dimension(),
        max_batch_chars: mlx.max_batch_chars,
        max_batch_items: mlx.max_batch_items,
        providers: vec![
            RmcpProviderConfig {
                name: "local".to_string(),
                base_url: format!("http://localhost:{}", mlx.local_port),
                model: profile.local_model().to_string(),
                priority: 1,
                endpoint: default_provider_endpoint(),
            },
            RmcpProviderConfig {
                name: "dragon".to_string(),
                base_url: format!("{}:{}", mlx.dragon_url, mlx.dragon_port),
                model: profile.dragon_model().to_string(),
                priority: 2,
                endpoint: default_provider_endpoint(),
            },
        ],
        reranker: RmcpRerankerConfig {
            base_url: Some(format!("{}:{}", mlx.dragon_url, reranker_port)),
            model: Some(mlx.reranker_model.clone()),
            endpoint: default_reranker_endpoint(),
        },
    }
}

fn runtime_profile_summary(
    profile: MemexRuntimeProfile,
    source: RuntimeProfileSource,
    config_path: Option<&Path>,
) -> String {
    match source {
        RuntimeProfileSource::Default => {
            format!("aicx runtime profile '{profile}' ({})", profile.label())
        }
        RuntimeProfileSource::Env => format!(
            "aicx runtime profile '{profile}' ({}) via {AICX_RUNTIME_PROFILE_ENV}",
            profile.label()
        ),
        RuntimeProfileSource::Cli => format!(
            "aicx runtime profile '{profile}' ({}) via --profile",
            profile.label()
        ),
        RuntimeProfileSource::Config => match config_path {
            Some(path) => format!(
                "aicx runtime profile '{profile}' ({}) from [runtime].profile in {}",
                profile.label(),
                path.display()
            ),
            None => format!(
                "aicx runtime profile '{profile}' ({}) from [runtime].profile",
                profile.label()
            ),
        },
    }
}

fn explicit_embeddings_summary(config_path: Option<&Path>) -> String {
    match config_path {
        Some(path) => format!("explicit [embeddings] config from {}", path.display()),
        None => "explicit [embeddings] config".to_string(),
    }
}

fn explicit_legacy_mlx_summary(config_path: Option<&Path>) -> String {
    match config_path {
        Some(path) => format!("explicit [mlx] config from {}", path.display()),
        None => "explicit [mlx] config".to_string(),
    }
}

fn explicit_legacy_env_summary() -> String {
    "legacy MLX environment overrides (EMBEDDER_MODEL / EMBEDDER_PORT / DRAGON_BASE_URL / RERANKER_MODEL)"
        .to_string()
}

fn resolve_runtime_boundary_from_config(
    db_path_override: Option<&Path>,
    config_path: Option<&Path>,
    profile_override: Option<MemexRuntimeProfile>,
) -> Result<(MemexRuntimeTruth, RmcpEmbeddingConfig)> {
    let file_cfg = load_memex_file_config(config_path)?;
    let mut db_path = db_path_override
        .map(Path::to_path_buf)
        .unwrap_or_else(default_memex_db_path);
    let mlx_config = merged_mlx_config(file_cfg.as_ref().and_then(|cfg| cfg.mlx.as_ref()));
    let profile_from_file = file_cfg
        .as_ref()
        .and_then(|cfg| cfg.runtime.as_ref())
        .and_then(|runtime| runtime.profile);
    let selected_profile = if let Some(profile) = profile_override {
        Some((profile, RuntimeProfileSource::Cli))
    } else if let Some(profile) = runtime_profile_from_env()? {
        Some((profile, RuntimeProfileSource::Env))
    } else {
        profile_from_file.map(|profile| (profile, RuntimeProfileSource::Config))
    };

    let (embedding_config, runtime_profile, runtime_summary) =
        if let Some(file_cfg) = file_cfg.as_ref().filter(|cfg| cfg.embeddings.is_some()) {
            (
                resolve_embedding_config(file_cfg),
                None,
                explicit_embeddings_summary(config_path),
            )
        } else if let Some((profile, source)) = selected_profile {
            (
                runtime_profile_embedding_config(profile, &mlx_config),
                Some(profile),
                runtime_profile_summary(profile, source, config_path),
            )
        } else if file_cfg.as_ref().and_then(|cfg| cfg.mlx.as_ref()).is_some() {
            (
                mlx_config.to_embedding_config(),
                None,
                explicit_legacy_mlx_summary(config_path),
            )
        } else if legacy_mlx_env_override_present() {
            (
                MlxConfig::from_env().to_embedding_config(),
                None,
                explicit_legacy_env_summary(),
            )
        } else {
            let profile = MemexRuntimeProfile::default();
            (
                runtime_profile_embedding_config(profile, &mlx_config),
                Some(profile),
                runtime_profile_summary(profile, RuntimeProfileSource::Default, config_path),
            )
        };

    if db_path_override.is_none()
        && let Some(path) = file_cfg.as_ref().and_then(|cfg| cfg.db_path.as_deref())
    {
        db_path = expand_home_path(path);
    }

    Ok((
        MemexRuntimeTruth {
            db_path,
            bm25_path: default_memex_bm25_path(),
            embedding_model: embedding_config.model_name(),
            embedding_dimension: embedding_config.dimension(),
            config_path: config_path.map(Path::to_path_buf),
            runtime_profile,
            runtime_summary,
        },
        embedding_config,
    ))
}

fn resolve_runtime_truth_from_config(
    db_path_override: Option<&Path>,
    config_path: Option<&Path>,
    profile_override: Option<MemexRuntimeProfile>,
) -> Result<MemexRuntimeTruth> {
    resolve_runtime_boundary_from_config(db_path_override, config_path, profile_override)
        .map(|(truth, _)| truth)
}

/// Resolve the current rmcp-memex runtime truth from config + defaults.
pub fn resolve_runtime_truth(db_path_override: Option<&Path>) -> Result<MemexRuntimeTruth> {
    resolve_runtime_truth_with_profile(db_path_override, None)
}

/// Resolve the current rmcp-memex runtime truth from config + defaults, with an optional profile override.
pub fn resolve_runtime_truth_with_profile(
    db_path_override: Option<&Path>,
    profile_override: Option<MemexRuntimeProfile>,
) -> Result<MemexRuntimeTruth> {
    let config_path = discover_memex_config_path();
    resolve_runtime_truth_from_config(db_path_override, config_path.as_deref(), profile_override)
}

// ============================================================================
// Per-namespace semantic index metadata + compatibility checks
// ============================================================================

fn semantic_index_metadata_path(namespace: &str) -> Result<PathBuf> {
    Ok(memex_state_dir()?.join(format!(
        "semantic-index-{}.json",
        namespace
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>()
    )))
}

fn cli_display_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn load_semantic_index_metadata(namespace: &str) -> Option<SemanticIndexMetadata> {
    let path = semantic_index_metadata_path(namespace).ok()?;
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn save_semantic_index_metadata(namespace: &str, truth: &MemexRuntimeTruth) -> Result<()> {
    let path = semantic_index_metadata_path(namespace)?;
    let metadata = SemanticIndexMetadata {
        format_version: SEMANTIC_INDEX_METADATA_VERSION,
        namespace: namespace.to_string(),
        db_path: truth.db_path.display().to_string(),
        bm25_path: truth.bm25_path.display().to_string(),
        embedding_model: truth.embedding_model.clone(),
        embedding_dimension: truth.embedding_dimension,
        updated_at: Utc::now(),
    };

    fs::write(path, serde_json::to_vec_pretty(&metadata)?)?;
    Ok(())
}

fn semantic_reindex_command(namespace: &str, truth: &MemexRuntimeTruth) -> String {
    let mut command = vec![
        "aicx".to_string(),
        "memex-sync".to_string(),
        "--reindex".to_string(),
    ];

    if namespace != DEFAULT_MEMEX_NAMESPACE {
        command.push("--namespace".to_string());
        command.push(cli_display_arg(namespace));
    }

    if truth.db_path != default_memex_db_path() {
        command.push("--db-path".to_string());
        command.push(cli_display_arg(&truth.db_path.to_string_lossy()));
    }

    if truth
        .runtime_profile
        .is_some_and(|profile| profile != MemexRuntimeProfile::Base)
    {
        command.push("--profile".to_string());
        command.push(
            truth
                .runtime_profile
                .expect("runtime profile should exist")
                .to_string(),
        );
    }

    command.join(" ")
}

fn runtime_truth_source_message(truth: &MemexRuntimeTruth) -> String {
    format!(" Runtime selection uses {}.", truth.runtime_summary)
}

fn compatibility_profile_note(
    truth: &MemexRuntimeTruth,
    metadata: Option<&SemanticIndexMetadata>,
    actual_dimension: Option<usize>,
) -> Option<String> {
    let metadata_profile = metadata.and_then(|entry| {
        MemexRuntimeProfile::detect(&entry.embedding_model, entry.embedding_dimension)
    });
    let actual_profile = match (metadata, actual_dimension) {
        (Some(entry), Some(dimension)) => {
            MemexRuntimeProfile::detect(&entry.embedding_model, dimension)
        }
        _ => None,
    };

    let previous_profile = metadata_profile.or(actual_profile);
    let current_profile = truth.runtime_profile;

    match (previous_profile, current_profile) {
        (Some(previous), Some(current)) if previous != current => Some(format!(
            " To keep the previous heavier preset instead of rebuilding right now, rerun with `AICX_RUNTIME_PROFILE={previous} aicx memex-sync` or set `[runtime].profile = \"{previous}\"`."
        )),
        (Some(previous), None) => Some(format!(
            " If you want to keep the recorded preset, rerun with `AICX_RUNTIME_PROFILE={previous} aicx memex-sync` or set `[runtime].profile = \"{previous}\"`."
        )),
        _ => None,
    }
}

fn semantic_metadata_mismatch_fields(
    namespace: &str,
    truth: &MemexRuntimeTruth,
    metadata: &SemanticIndexMetadata,
) -> Vec<&'static str> {
    let truth_db_path = truth.db_path.to_string_lossy();
    let truth_bm25_path = truth.bm25_path.to_string_lossy();
    let mut fields = Vec::new();

    if metadata.format_version != SEMANTIC_INDEX_METADATA_VERSION {
        fields.push("format_version");
    }
    if metadata.namespace != namespace {
        fields.push("namespace");
    }
    if metadata.db_path != truth_db_path.as_ref() {
        fields.push("db_path");
    }
    if metadata.bm25_path != truth_bm25_path.as_ref() {
        fields.push("bm25_path");
    }
    if metadata.embedding_model != truth.embedding_model {
        fields.push("embedding_model");
    }
    if metadata.embedding_dimension != truth.embedding_dimension {
        fields.push("embedding_dimension");
    }

    fields
}

fn semantic_metadata_mismatch(
    namespace: &str,
    truth: &MemexRuntimeTruth,
    metadata: Option<&SemanticIndexMetadata>,
) -> bool {
    metadata.is_some_and(|metadata| {
        !semantic_metadata_mismatch_fields(namespace, truth, metadata).is_empty()
    })
}

fn semantic_compatibility_error(
    namespace: &str,
    truth: &MemexRuntimeTruth,
    metadata: Option<&SemanticIndexMetadata>,
    actual_dimension: Option<usize>,
) -> anyhow::Error {
    let mut message = format!(
        "Semantic index mismatch for namespace '{namespace}'. rmcp-memex currently expects model '{}' ({} dims) at {}.",
        truth.embedding_model,
        truth.embedding_dimension,
        truth.db_path.display()
    );
    message.push_str(&runtime_truth_source_message(truth));

    if let Some(actual_dimension) = actual_dimension {
        message.push_str(&format!(
            " Existing namespace data uses {actual_dimension} dims."
        ));
    }

    if let Some(metadata) = metadata {
        message.push_str(&format!(
            " ai-contexters metadata (format v{}) still points at model '{}' ({} dims) recorded from {} with BM25 at {}.",
            metadata.format_version,
            metadata.embedding_model,
            metadata.embedding_dimension,
            metadata.db_path,
            metadata.bm25_path
        ));

        let mismatched_fields = semantic_metadata_mismatch_fields(namespace, truth, metadata);
        if !mismatched_fields.is_empty() {
            message.push_str(&format!(
                " Diverged fields: {}.",
                mismatched_fields.join(", ")
            ));
        }
    }

    message.push_str(&format!(
        " Run `{}` to rebuild this namespace for the new embedding truth. Other namespaces are not touched — aicx only drops per-namespace documents, keeping sibling namespaces intact.",
        semantic_reindex_command(namespace, truth)
    ));
    if let Some(note) = compatibility_profile_note(truth, metadata, actual_dimension) {
        message.push_str(&note);
    }
    message.push_str(
        " This stays explicit because Lance vector schemas are shared across the whole store, so silent reuse would corrupt search semantics.",
    );

    MemexCompatibilityError { message }.into()
}

async fn semantic_store_dimension(
    truth: &MemexRuntimeTruth,
    namespace: &str,
) -> Result<Option<usize>> {
    let storage = StorageManager::new_lance_only(&truth.db_path.to_string_lossy())
        .await
        .with_context(|| {
            format!(
                "Failed to open rmcp-memex LanceDB at {}",
                truth.db_path.display()
            )
        })?;

    Ok(storage
        .all_documents(Some(namespace), 1)
        .await?
        .into_iter()
        .next()
        .map(|doc| doc.embedding.len()))
}

async fn validate_semantic_index_compatibility_truth(
    namespace: &str,
    truth: &MemexRuntimeTruth,
) -> Result<()> {
    let actual_dimension = semantic_store_dimension(truth, namespace).await?;
    let metadata = load_semantic_index_metadata(namespace);
    let metadata_mismatch = semantic_metadata_mismatch(namespace, truth, metadata.as_ref());

    if metadata_mismatch {
        return Err(semantic_compatibility_error(
            namespace,
            truth,
            metadata.as_ref(),
            actual_dimension,
        ));
    }

    if let Some(actual_dimension) = actual_dimension {
        if actual_dimension != truth.embedding_dimension {
            return Err(semantic_compatibility_error(
                namespace,
                truth,
                metadata.as_ref(),
                Some(actual_dimension),
            ));
        }

        save_semantic_index_metadata(namespace, truth)?;
    }

    Ok(())
}

/// Returns true when the error came from explicit memex compatibility checks.
pub fn is_compatibility_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.downcast_ref::<MemexCompatibilityError>().is_some())
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    if path.is_dir() {
        fs::remove_dir_all(path)
            .with_context(|| format!("Failed to remove directory {}", path.display()))?;
    } else {
        fs::remove_file(path)
            .with_context(|| format!("Failed to remove file {}", path.display()))?;
    }

    Ok(())
}

/// Explicitly drop all documents for a single namespace so it can be rebuilt for a new embedding truth.
///
/// Non-destructive by design: only the target namespace's documents are removed from the
/// shared LanceDB store. Sibling namespaces with different embedding dimensions stay intact.
/// When the namespace is the only one in the store (or was missing entirely), the directory
/// tree is cleaned up so the next sync starts from a clean slate.
pub fn reset_semantic_index(
    namespace: &str,
    db_path_override: Option<&Path>,
) -> Result<MemexRuntimeTruth> {
    reset_semantic_index_with_profile(namespace, db_path_override, None)
}

pub fn reset_semantic_index_with_profile(
    namespace: &str,
    db_path_override: Option<&Path>,
    profile_override: Option<MemexRuntimeProfile>,
) -> Result<MemexRuntimeTruth> {
    let truth = resolve_runtime_truth_with_profile(db_path_override, profile_override)?;
    let runtime = tokio::runtime::Runtime::new()
        .context("Failed to start Tokio runtime for semantic-index reset")?;

    let remaining_namespaces = runtime.block_on(async {
        if !truth.db_path.exists() {
            return Ok::<usize, anyhow::Error>(0);
        }

        let storage = StorageManager::new_lance_only(&truth.db_path.to_string_lossy())
            .await
            .with_context(|| {
                format!(
                    "Failed to open rmcp-memex LanceDB for reset at {}",
                    truth.db_path.display()
                )
            })?;

        // Best-effort: drop only documents for the requested namespace.
        let _ = storage.delete_namespace_documents(namespace).await?;

        let namespaces = storage
            .list_namespaces()
            .await
            .context("Failed to list namespaces after per-namespace reset")?;
        Ok(namespaces.len())
    })?;

    if remaining_namespaces == 0 {
        // Nothing else lives in the store — safe to drop the full directories so the
        // next sync starts from a clean slate.
        remove_path_if_exists(&truth.db_path)?;
        remove_path_if_exists(&truth.bm25_path)?;
        remove_path_if_exists(&sync_state_path()?)?;
    } else {
        // BM25 index is keyed by namespace column, so we leave it intact for siblings.
        // Sync state tracks chunk IDs, which are globally unique; purge the file to force
        // a full re-scan on the next sync (other namespaces will no-op on re-ingest).
        remove_path_if_exists(&sync_state_path()?)?;
    }

    remove_path_if_exists(&semantic_index_metadata_path(namespace)?)?;

    Ok(truth)
}

// ============================================================================
// Sync state
// ============================================================================

/// Persistent state tracking what's been synced to memex.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemexSyncState {
    /// Last time a sync was performed.
    pub last_synced: Option<DateTime<Utc>>,
    /// Set of chunk IDs already materialized into memex.
    pub synced_chunks: HashSet<String>,
    /// Total number of chunks materialized across all syncs.
    #[serde(alias = "total_pushes")]
    pub total_materialized: usize,
}

/// Result of a sync operation.
#[derive(Debug, Default)]
pub struct SyncResult {
    /// Number of chunks successfully materialized.
    pub chunks_materialized: usize,
    /// Number of chunks skipped (already synced or dedup).
    pub chunks_skipped: usize,
    /// Number of chunks excluded by `.aicxignore`.
    pub chunks_ignored: usize,
    /// Errors encountered during sync.
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncProgressPhase {
    Discovering,
    Embedding,
    Writing,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncProgress {
    pub phase: SyncProgressPhase,
    pub done: usize,
    pub total: usize,
    pub detail: String,
}

const EMBEDDING_BATCH_SIZE: usize = 64;

#[derive(Debug, Serialize)]
struct MemexRecord {
    id: String,
    text: String,
    metadata: serde_json::Value,
    content_hash: String,
}

#[derive(Debug)]
struct PendingSyncRecord {
    chunk_path: PathBuf,
    id: String,
    text: String,
    metadata: serde_json::Value,
    content_hash: String,
}

// ============================================================================
// Sync state persistence
// ============================================================================

/// Path to sync state file: `~/.aicx/memex/sync_state.json`
fn sync_state_path() -> Result<PathBuf> {
    Ok(memex_state_dir()?.join("sync_state.json"))
}

/// Load sync state from disk. Returns default if missing or unparseable.
pub fn load_sync_state() -> MemexSyncState {
    let path = match sync_state_path() {
        Ok(p) => p,
        Err(_) => return MemexSyncState::default(),
    };

    if !path.exists() {
        return MemexSyncState::default();
    }

    let contents = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return MemexSyncState::default(),
    };

    serde_json::from_str(&contents).unwrap_or_default()
}

/// Persist sync state to disk.
pub fn save_sync_state(state: &MemexSyncState) -> Result<()> {
    let path = sync_state_path()?;
    let json = serde_json::to_string_pretty(state).context("Failed to serialize sync state")?;
    fs::write(&path, json)?;
    Ok(())
}

// ============================================================================
// Library-backed sync methods
// ============================================================================

async fn sync_chunk_library(
    storage: &StorageManager,
    bm25: &BM25Index,
    embedding_client: &mut EmbeddingClient,
    namespace: &str,
    record: PendingSyncRecord,
) -> Result<()> {
    let embedding = embedding_client.embed(&record.text).await?;
    let bm25_docs = vec![(
        record.id.clone(),
        namespace.to_string(),
        record.text.clone(),
    )];
    let doc = ChromaDocument::new_flat_with_hash(
        record.id,
        namespace.to_string(),
        embedding,
        record.metadata,
        record.text,
        record.content_hash,
    );

    storage.add_to_store(vec![doc]).await?;
    bm25.add_documents(&bm25_docs).await?;
    Ok(())
}

async fn sync_chunks_library<F>(
    chunk_paths: &[PathBuf],
    namespace: &str,
    truth: &MemexRuntimeTruth,
    embedding_config: &RmcpEmbeddingConfig,
    batch_mode: bool,
    mut progress: F,
) -> Result<(SyncResult, Vec<PathBuf>)>
where
    F: FnMut(SyncProgress),
{
    if chunk_paths.is_empty() {
        return Ok((SyncResult::default(), Vec::new()));
    }

    let storage = StorageManager::new_lance_only(&truth.db_path.to_string_lossy())
        .await
        .with_context(|| {
            format!(
                "Failed to open rmcp-memex LanceDB for sync at {}",
                truth.db_path.display()
            )
        })?;
    storage.ensure_collection().await?;

    let bm25_config =
        BM25Config::default().with_path(truth.bm25_path.to_string_lossy().to_string());
    let bm25 = BM25Index::new(&bm25_config).context("Failed to open BM25 index for sync")?;
    let mut embedding_client = EmbeddingClient::new(embedding_config)
        .await
        .context("Failed to initialize rmcp-memex embedding client for sync")?;

    let mut result = SyncResult::default();
    let mut completed_paths = Vec::new();
    let mut seen_hashes = HashSet::new();
    let mut pending = Vec::new();

    for chunk_path in chunk_paths {
        let validated_path = sanitize::validate_read_path(chunk_path)?;
        let id = validated_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let text = sanitize::read_to_string_validated(&validated_path)?;
        let record = chunk_memex_record(&validated_path, &id, &text);

        if !seen_hashes.insert(record.content_hash.clone())
            || storage
                .has_content_hash(namespace, &record.content_hash)
                .await?
        {
            result.chunks_skipped += 1;
            completed_paths.push(chunk_path.clone());
            continue;
        }

        pending.push(PendingSyncRecord {
            chunk_path: chunk_path.clone(),
            id: record.id,
            text: record.text,
            metadata: record.metadata,
            content_hash: record.content_hash,
        });
    }

    if pending.is_empty() {
        return Ok((result, completed_paths));
    }

    if batch_mode {
        let pending_total = pending.len();

        for (batch_index, chunk_batch) in pending.chunks(EMBEDDING_BATCH_SIZE).enumerate() {
            let batch_start = batch_index * EMBEDDING_BATCH_SIZE;
            let batch_end = batch_start + chunk_batch.len();

            emit_sync_progress(
                &mut progress,
                SyncProgressPhase::Embedding,
                batch_end,
                pending_total,
                format!(
                    "Embedding batch {} ({}-{} of {})",
                    batch_index + 1,
                    batch_start + 1,
                    batch_end,
                    pending_total
                ),
            );

            let texts: Vec<String> = chunk_batch
                .iter()
                .map(|record| record.text.clone())
                .collect();
            let embeddings = embedding_client.embed_batch(&texts).await?;
            let mut bm25_docs = Vec::with_capacity(chunk_batch.len());
            let mut docs = Vec::with_capacity(chunk_batch.len());

            for (record, embedding) in chunk_batch.iter().zip(embeddings.into_iter()) {
                bm25_docs.push((
                    record.id.clone(),
                    namespace.to_string(),
                    record.text.clone(),
                ));
                docs.push(ChromaDocument::new_flat_with_hash(
                    record.id.clone(),
                    namespace.to_string(),
                    embedding,
                    record.metadata.clone(),
                    record.text.clone(),
                    record.content_hash.clone(),
                ));
            }

            storage.add_to_store(docs).await?;
            bm25.add_documents(&bm25_docs).await?;

            result.chunks_materialized += chunk_batch.len();
            completed_paths.extend(chunk_batch.iter().map(|record| record.chunk_path.clone()));

            emit_sync_progress(
                &mut progress,
                SyncProgressPhase::Writing,
                result.chunks_materialized,
                pending_total,
                format!(
                    "Indexed {} of {} chunks",
                    result.chunks_materialized, pending_total
                ),
            );
        }

        return Ok((result, completed_paths));
    }

    let pending_total = pending.len();
    for (idx, record) in pending.into_iter().enumerate() {
        let chunk_path = record.chunk_path.clone();
        let record_id = record.id.clone();

        emit_sync_progress(
            &mut progress,
            SyncProgressPhase::Embedding,
            idx + 1,
            pending_total,
            format!("Embedding {}", chunk_path.display()),
        );

        match sync_chunk_library(&storage, &bm25, &mut embedding_client, namespace, record).await {
            Ok(()) => {
                result.chunks_materialized += 1;
                completed_paths.push(chunk_path);
                emit_sync_progress(
                    &mut progress,
                    SyncProgressPhase::Writing,
                    result.chunks_materialized,
                    pending_total,
                    format!(
                        "Indexed {} of {} chunks",
                        result.chunks_materialized, pending_total
                    ),
                );
            }
            Err(err) => result.errors.push(format!("{record_id}: {err}")),
        }
    }

    Ok((result, completed_paths))
}

#[cfg(test)]
fn chunk_sidecar_path(chunk_path: &Path) -> PathBuf {
    chunk_path.with_extension("meta.json")
}

fn insert_optional_string(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<String>,
) {
    if let Some(value) = value {
        metadata.insert(key.to_string(), serde_json::Value::String(value));
    }
}

fn insert_optional_u64(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<u64>,
) {
    if let Some(value) = value {
        metadata.insert(key.to_string(), serde_json::Value::from(value));
    }
}

fn insert_optional_u32(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<u32>,
) {
    if let Some(value) = value {
        metadata.insert(key.to_string(), serde_json::Value::from(value));
    }
}

fn chunk_metadata_from_header(text: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut metadata = serde_json::Map::new();
    let Some(first_line) = text.lines().next() else {
        return metadata;
    };

    if !first_line.starts_with('[') || !first_line.ends_with(']') {
        return metadata;
    }

    let inner = &first_line[1..first_line.len() - 1];
    for part in inner.split('|') {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        metadata.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    metadata
}

fn chunk_metadata_for_memex(chunk_path: &Path, chunk_id: &str, text: &str) -> serde_json::Value {
    let content_hash = compute_content_hash(text);
    let mut metadata = serde_json::Map::from_iter([
        (
            "source".to_string(),
            serde_json::Value::String("ai-contexters".to_string()),
        ),
        (
            "chunk_id".to_string(),
            serde_json::Value::String(chunk_id.to_string()),
        ),
        (
            "path".to_string(),
            serde_json::Value::String(chunk_path.to_string_lossy().to_string()),
        ),
        (
            "content_hash".to_string(),
            serde_json::Value::String(content_hash),
        ),
    ]);

    let sidecar = crate::store::load_sidecar(chunk_path);

    if let Some(sidecar) = sidecar {
        metadata.insert(
            "project".to_string(),
            serde_json::Value::String(sidecar.project),
        );
        metadata.insert(
            "agent".to_string(),
            serde_json::Value::String(sidecar.agent),
        );
        metadata.insert("date".to_string(), serde_json::Value::String(sidecar.date));
        metadata.insert(
            "session_id".to_string(),
            serde_json::Value::String(sidecar.session_id),
        );
        metadata.insert(
            "kind".to_string(),
            serde_json::Value::String(sidecar.kind.dir_name().to_string()),
        );
        insert_optional_string(&mut metadata, "cwd", sidecar.cwd);
        insert_optional_string(&mut metadata, "run_id", sidecar.run_id);
        insert_optional_string(&mut metadata, "prompt_id", sidecar.prompt_id);
        insert_optional_string(&mut metadata, "agent_model", sidecar.agent_model);
        insert_optional_string(&mut metadata, "started_at", sidecar.started_at);
        insert_optional_string(&mut metadata, "completed_at", sidecar.completed_at);
        insert_optional_u64(&mut metadata, "token_usage", sidecar.token_usage);
        insert_optional_u32(&mut metadata, "findings_count", sidecar.findings_count);
        insert_optional_string(&mut metadata, "workflow_phase", sidecar.workflow_phase);
        insert_optional_string(&mut metadata, "mode", sidecar.mode);
        insert_optional_string(&mut metadata, "skill_code", sidecar.skill_code);
        insert_optional_string(
            &mut metadata,
            "frame_kind",
            sidecar.frame_kind.map(|kind| kind.to_string()),
        );
        insert_optional_string(
            &mut metadata,
            "framework_version",
            sidecar.framework_version,
        );
    } else {
        metadata.extend(chunk_metadata_from_header(text));
    }

    serde_json::Value::Object(metadata)
}

fn chunk_memex_record(chunk_path: &Path, chunk_id: &str, text: &str) -> MemexRecord {
    MemexRecord {
        id: chunk_id.to_string(),
        text: text.to_string(),
        metadata: chunk_metadata_for_memex(chunk_path, chunk_id, text),
        content_hash: compute_content_hash(text),
    }
}

// ============================================================================
// High-level sync
// ============================================================================

fn emit_sync_progress<F>(
    progress: &mut F,
    phase: SyncProgressPhase,
    done: usize,
    total: usize,
    detail: impl Into<String>,
) where
    F: FnMut(SyncProgress),
{
    progress(SyncProgress {
        phase,
        done,
        total,
        detail: detail.into(),
    });
}

/// Sync only new chunks (not previously synced) to memex.
///
/// Loads sync state, determines which chunk files are new,
/// syncs them through the published rmcp-memex library boundary, and updates
/// state plus semantic-index metadata.
pub fn sync_new_chunk_paths(chunk_paths: &[PathBuf], config: &MemexConfig) -> Result<SyncResult> {
    sync_new_chunk_paths_with_progress(chunk_paths, config, |_| {})
}

pub fn sync_new_chunk_paths_with_progress<F>(
    chunk_paths: &[PathBuf],
    config: &MemexConfig,
    progress: F,
) -> Result<SyncResult>
where
    F: FnMut(SyncProgress),
{
    let rt =
        tokio::runtime::Runtime::new().context("Failed to start Tokio runtime for memex sync")?;
    rt.block_on(sync_new_chunk_paths_async(chunk_paths, config, progress))
}

async fn sync_new_chunk_paths_async<F>(
    chunk_paths: &[PathBuf],
    config: &MemexConfig,
    mut progress: F,
) -> Result<SyncResult>
where
    F: FnMut(SyncProgress),
{
    let mut state = load_sync_state();
    let store_base = crate::store::store_base_dir()?;
    let (filtered_paths, ignored_count) =
        crate::store::filter_ignored_paths_at(&store_base, chunk_paths)?;

    let all_files: Vec<PathBuf> = filtered_paths
        .iter()
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "md" || ext == "txt")
        })
        .cloned()
        .collect();

    let total_candidates = all_files.len();
    if total_candidates == 0 {
        emit_sync_progress(
            &mut progress,
            SyncProgressPhase::Completed,
            ignored_count,
            ignored_count,
            format!(
                "Completed: 0 materialized, 0 skipped, {} ignored",
                ignored_count
            ),
        );
        return Ok(SyncResult {
            chunks_ignored: ignored_count,
            ..SyncResult::default()
        });
    }

    let new_files: Vec<PathBuf> = all_files
        .iter()
        .enumerate()
        .filter_map(|(idx, p)| {
            let id = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            emit_sync_progress(
                &mut progress,
                SyncProgressPhase::Discovering,
                idx + 1,
                total_candidates,
                format!("Scanning {}", p.display()),
            );
            (!state.synced_chunks.contains(&id)).then_some(p.clone())
        })
        .collect();

    let config_path = discover_memex_config_path();
    let (truth, embedding_config) = resolve_runtime_boundary_from_config(
        config.db_path.as_deref(),
        config_path.as_deref(),
        config.runtime_profile,
    )?;
    validate_semantic_index_compatibility_truth(&config.namespace, &truth).await?;
    if new_files.is_empty() {
        emit_sync_progress(
            &mut progress,
            SyncProgressPhase::Completed,
            all_files.len() + ignored_count,
            all_files.len() + ignored_count,
            format!(
                "Completed: 0 materialized, {} skipped, {} ignored",
                all_files.len(),
                ignored_count
            ),
        );
        return Ok(SyncResult {
            chunks_materialized: 0,
            chunks_skipped: all_files.len(),
            chunks_ignored: ignored_count,
            errors: vec![],
        });
    }

    let (mut result, synced_files) = sync_chunks_library(
        &new_files,
        &config.namespace,
        &truth,
        &embedding_config,
        config.batch_mode,
        &mut progress,
    )
    .await?;

    result.chunks_skipped += all_files.len().saturating_sub(new_files.len());
    result.chunks_ignored = ignored_count;

    let path_refs: Vec<&PathBuf> = new_files.iter().collect();
    if let Err(e) = crate::steer_index::sync_steer_index(&path_refs).await {
        tracing::warn!("Failed to sync steer index: {}", e);
    }

    for file in &synced_files {
        let id = file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        state.synced_chunks.insert(id);
    }
    state.last_synced = Some(Utc::now());
    state.total_materialized += result.chunks_materialized;
    save_sync_state(&state)?;

    emit_sync_progress(
        &mut progress,
        SyncProgressPhase::Completed,
        result.chunks_materialized + result.chunks_skipped + result.chunks_ignored,
        total_candidates + ignored_count,
        format!(
            "Completed: {} materialized, {} skipped, {} ignored",
            result.chunks_materialized, result.chunks_skipped, result.chunks_ignored
        ),
    );

    Ok(result)
}

pub fn sync_new_chunks(chunks_dir: &Path, config: &MemexConfig) -> Result<SyncResult> {
    if !chunks_dir.exists() {
        return Ok(SyncResult::default());
    }

    let validated_dir = sanitize::validate_dir_path(chunks_dir)?;

    // SECURITY: dir sanitized via validate_dir_path (traversal + canonicalize + allowlist)
    let all_files: Vec<PathBuf> = fs::read_dir(&validated_dir)? // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            ext == "txt" || ext == "md"
        })
        .collect();
    sync_new_chunk_paths(&all_files, config)
}

use crate::rank::{FuzzyResult, score_chunk_content};

// ============================================================================
// High-level fast search via rmcp-memex library
// ============================================================================

/// Fast keyword-first search using `rmcp_memex`'s published BM25 index and
/// LanceDB document lookups.
///
/// This path stays fully library-backed and validates the embedding/runtime
/// boundary up front, but it does not execute vector similarity search.
pub async fn fast_memex_search(
    query: &str,
    limit: usize,
    project_filter: Option<&str>,
    frame_kind_filter: Option<crate::types::FrameKind>,
) -> Result<(Vec<FuzzyResult>, usize)> {
    let truth = resolve_runtime_truth(None)?;
    let config = BM25Config::default()
        .with_path(truth.bm25_path.to_string_lossy().to_string())
        .with_read_only(true);
    let index = BM25Index::new(&config).context("Failed to load BM25 index")?;

    let raw_results = index.search(query, None, limit * 5)?;
    let total_scanned = raw_results.len(); // Approximate

    let storage = StorageManager::new_lance_only(&truth.db_path.to_string_lossy())
        .await
        .context("Failed to open LanceDB")?;

    let mut results = Vec::new();
    let project_lower = project_filter.map(|s| s.to_lowercase());

    for (id, hit_namespace, score) in raw_results {
        if results.len() >= limit {
            break;
        }

        if let Ok(Some(doc)) = storage.get_document(&hit_namespace, &id).await {
            // Apply project filter if any
            let doc_project = doc
                .metadata
                .get("project")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let doc_frame_kind = doc
                .metadata
                .get("frame_kind")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            if let Some(ref pf) = project_lower
                && !doc_project.to_lowercase().contains(pf)
            {
                continue;
            }
            if let Some(expected) = frame_kind_filter
                && doc_frame_kind.as_deref() != Some(expected.as_str())
            {
                continue;
            }

            let kind = doc
                .metadata
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let agent = doc
                .metadata
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let date = doc
                .metadata
                .get("date")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let session_id = doc
                .metadata
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let cwd = doc
                .metadata
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // Score content to get density and matched lines
            let chunk_score = score_chunk_content(&doc.document);

            // Extract matching lines
            let query_terms: Vec<&str> = query.split_whitespace().collect();
            let matched_lines: Vec<String> = doc
                .document
                .lines()
                .filter(|line| {
                    let lower = line.to_lowercase();
                    query_terms
                        .iter()
                        .any(|&term| lower.contains(&term.to_lowercase()))
                })
                .take(5)
                .map(|s| s.trim().to_string())
                .collect();

            // Calculate final score using BM25 score and signal density
            // BM25 score usually > 0. The higher the better.
            let final_score = ((chunk_score.score as f32 * 5.0 + score * 10.0) as u8).min(100);

            let timestamp = doc
                .metadata
                .get("started_at")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| {
                    doc.metadata
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                });

            results.push(FuzzyResult {
                file: format!("{}.md", id),
                path: doc
                    .metadata
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&format!("{}.md", id))
                    .to_string(),
                project: doc_project,
                kind,
                frame_kind: doc_frame_kind,
                agent,
                date,
                timestamp,
                score: final_score,
                label: if final_score >= 80 {
                    "HIGH".to_string()
                } else if final_score >= 60 {
                    "MEDIUM".to_string()
                } else {
                    "LOW".to_string()
                },
                density: chunk_score.density,
                matched_lines,
                session_id,
                cwd,
            });
        }
    }

    // Sort by score
    results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));

    Ok((results, total_scanned))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("aicx-memex-{label}-{}-{nanos}", std::process::id()))
    }

    fn unique_test_namespace(label: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        format!("aicx-test-{label}-{}-{nanos}", std::process::id())
    }

    #[test]
    fn test_memex_config_default() {
        let config = MemexConfig::default();
        assert_eq!(config.namespace, DEFAULT_MEMEX_NAMESPACE);
        assert!(config.db_path.is_none());
        assert!(config.batch_mode);
        assert!(config.preprocess);
    }

    #[test]
    fn test_resolve_runtime_truth_from_explicit_config() {
        let root = unique_test_dir("runtime-truth");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");

        let db_path = root.join("memex-db");
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
db_path = "{}"

[embeddings]
required_dimension = 2560

[[embeddings.providers]]
name = "ollama-local"
base_url = "http://localhost:11434"
model = "qwen3-embedding:4b"
"#,
                db_path.display()
            ),
        )
        .expect("write config");

        let truth = resolve_runtime_truth_from_config(None, Some(&config_path), None)
            .expect("resolve truth");
        assert_eq!(truth.db_path, db_path);
        assert_eq!(truth.embedding_model, "qwen3-embedding:4b");
        assert_eq!(truth.embedding_dimension, 2560);
        assert_eq!(truth.config_path.as_deref(), Some(config_path.as_path()));
        assert!(truth.runtime_profile.is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_resolve_runtime_truth_without_config_defaults_to_base_profile() {
        let truth = resolve_runtime_truth_from_config(None, None, None).expect("resolve truth");
        assert_eq!(truth.db_path, default_memex_db_path());
        assert_eq!(truth.embedding_model, "qwen3-embedding:0.6b");
        assert_eq!(truth.embedding_dimension, 1024);
        assert!(truth.config_path.is_none());
        assert_eq!(truth.runtime_profile, Some(MemexRuntimeProfile::Base));
        assert!(truth.runtime_summary.contains("runtime profile 'base'"));
    }

    #[test]
    fn test_resolve_runtime_truth_from_embeddings_config_keeps_rmcp_memex_dimension_default() {
        let root = unique_test_dir("runtime-truth-embeddings-default");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");

        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[embeddings]

[[embeddings.providers]]
name = "alt"
base_url = "http://localhost:11434"
model = "nomic-embed-text"
"#,
        )
        .expect("write config");

        let truth = resolve_runtime_truth_from_config(None, Some(&config_path), None)
            .expect("resolve truth");

        assert_eq!(truth.embedding_model, "nomic-embed-text");
        assert_eq!(truth.embedding_dimension, DEFAULT_REQUIRED_DIMENSION);
        assert!(truth.runtime_profile.is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_resolve_runtime_boundary_from_embeddings_config_keeps_reranker_truth() {
        let root = unique_test_dir("runtime-boundary-reranker");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");

        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[embeddings]
required_dimension = 2560

[[embeddings.providers]]
name = "alt"
base_url = "http://localhost:11434"
model = "nomic-embed-text"

[embeddings.reranker]
base_url = "http://localhost:11435"
model = "bge-reranker-v2"
"#,
        )
        .expect("write config");

        let (_truth, embedding_config) =
            resolve_runtime_boundary_from_config(None, Some(&config_path), None)
                .expect("resolve boundary");

        assert_eq!(
            embedding_config.reranker.base_url.as_deref(),
            Some("http://localhost:11435")
        );
        assert_eq!(
            embedding_config.reranker.model.as_deref(),
            Some("bge-reranker-v2")
        );
        assert_eq!(embedding_config.reranker.endpoint, "/v1/rerank");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_resolve_runtime_truth_rejects_partial_provider_config() {
        let root = unique_test_dir("runtime-truth-invalid-provider");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");

        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[embeddings]

[[embeddings.providers]]
name = "alt"
model = "nomic-embed-text"
"#,
        )
        .expect("write config");

        let err = resolve_runtime_truth_from_config(None, Some(&config_path), None)
            .expect_err("partial provider config should be rejected");

        assert!(
            err.to_string()
                .contains("Failed to parse rmcp-memex config")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_resolve_runtime_truth_from_legacy_mlx_config() {
        let root = unique_test_dir("runtime-truth-mlx");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");

        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[mlx]
embedder_model = "nomic-embed-text"
"#,
        )
        .expect("write config");

        let truth = resolve_runtime_truth_from_config(None, Some(&config_path), None)
            .expect("resolve truth");
        let mut mlx_config = MlxConfig::from_env();
        mlx_config.merge_file_config(MlxMergeOptions {
            disabled: Some(false),
            local_port: None,
            dragon_url: None,
            dragon_port: None,
            embedder_model: Some("nomic-embed-text".to_string()),
            reranker_model: None,
            reranker_port_offset: None,
        });
        let embedding_config = mlx_config.to_embedding_config();

        assert_eq!(truth.embedding_model, embedding_config.model_name());
        assert_eq!(truth.embedding_dimension, embedding_config.dimension());
        assert!(truth.runtime_profile.is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_resolve_runtime_truth_from_profile_override_uses_dev_preset() {
        let truth = resolve_runtime_truth_from_config(None, None, Some(MemexRuntimeProfile::Dev))
            .expect("resolve truth");

        assert_eq!(truth.embedding_model, "qwen3-embedding:4b");
        assert_eq!(truth.embedding_dimension, 2560);
        assert_eq!(truth.runtime_profile, Some(MemexRuntimeProfile::Dev));
        assert!(truth.runtime_summary.contains("via --profile"));
    }

    #[test]
    fn test_runtime_profile_config_is_used_when_embeddings_are_absent() {
        let root = unique_test_dir("runtime-profile-config");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");

        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            r#"
[runtime]
profile = "premium"
"#,
        )
        .expect("write config");

        let truth = resolve_runtime_truth_from_config(None, Some(&config_path), None)
            .expect("resolve truth");

        assert_eq!(truth.embedding_model, "qwen3-embedding:8b");
        assert_eq!(truth.embedding_dimension, 4096);
        assert_eq!(truth.runtime_profile, Some(MemexRuntimeProfile::Premium));
        assert!(truth.runtime_summary.contains("[runtime].profile"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_validate_semantic_index_compatibility_rejects_stale_metadata_without_documents() {
        let root = unique_test_dir("metadata-mismatch");
        let namespace = unique_test_namespace("metadata-mismatch");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");

        let metadata_path =
            semantic_index_metadata_path(&namespace).expect("metadata path should resolve");
        let _ = remove_path_if_exists(&metadata_path);

        let db_path = root.join("memex-db");
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                r#"
db_path = "{}"

[embeddings]
required_dimension = 2560

[[embeddings.providers]]
name = "ollama-local"
base_url = "http://localhost:11434"
model = "qwen3-embedding:4b"
"#,
                db_path.display()
            ),
        )
        .expect("write config");

        let stale_truth = MemexRuntimeTruth {
            db_path: root.join("legacy-db"),
            bm25_path: root.join("legacy-bm25"),
            embedding_model: "legacy-embedding".to_string(),
            embedding_dimension: 4096,
            config_path: None,
            runtime_profile: None,
            runtime_summary: "legacy truth".to_string(),
        };
        save_semantic_index_metadata(&namespace, &stale_truth).expect("save stale metadata");

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let truth = resolve_runtime_truth_from_config(
            Some(db_path.as_path()),
            Some(config_path.as_path()),
            None,
        )
        .expect("resolve truth for test");
        let err = runtime
            .block_on(validate_semantic_index_compatibility_truth(
                &namespace, &truth,
            ))
            .expect_err("stale metadata should force an explicit compatibility error");

        assert!(is_compatibility_error(&err));
        let message = err.to_string();
        assert!(message.contains("qwen3-embedding:4b"));
        assert!(message.contains("legacy-embedding"));
        assert!(message.contains("Diverged fields:"));
        assert!(message.contains("db_path"));
        assert!(message.contains("bm25_path"));
        assert!(message.contains("embedding_model"));
        assert!(message.contains("embedding_dimension"));
        assert!(message.contains(&config_path.to_string_lossy().to_string()));
        assert!(message.contains("aicx memex-sync --reindex"));

        let _ = remove_path_if_exists(&metadata_path);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_sync_state_serialization() {
        let mut synced_chunks = HashSet::new();
        synced_chunks.insert("chunk_001".to_string());
        synced_chunks.insert("chunk_002".to_string());

        let state = MemexSyncState {
            last_synced: Some(Utc::now()),
            synced_chunks,
            total_materialized: 42,
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let restored: MemexSyncState = serde_json::from_str(&json).unwrap();

        assert!(restored.last_synced.is_some());
        assert_eq!(restored.synced_chunks.len(), 2);
        assert!(restored.synced_chunks.contains("chunk_001"));
        assert!(restored.synced_chunks.contains("chunk_002"));
        assert_eq!(restored.total_materialized, 42);
    }

    #[test]
    fn test_sync_state_deserializes_legacy_total_pushes() {
        let restored: MemexSyncState = serde_json::from_str(
            r#"{
                "last_synced": null,
                "synced_chunks": ["chunk_001"],
                "total_pushes": 7
            }"#,
        )
        .unwrap();

        assert_eq!(restored.total_materialized, 7);
        assert!(restored.synced_chunks.contains("chunk_001"));
    }

    #[test]
    fn test_sync_state_tracks_chunks() {
        let mut state = MemexSyncState::default();
        assert!(state.synced_chunks.is_empty());

        state.synced_chunks.insert("a".to_string());
        assert!(state.synced_chunks.contains("a"));
        assert!(!state.synced_chunks.contains("b"));

        state.synced_chunks.insert("b".to_string());
        assert_eq!(state.synced_chunks.len(), 2);
    }

    #[test]
    fn test_sync_result_default() {
        let result = SyncResult::default();
        assert_eq!(result.chunks_materialized, 0);
        assert_eq!(result.chunks_skipped, 0);
        assert_eq!(result.chunks_ignored, 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_chunk_metadata_from_header() {
        let text = "[project: prview-rs | agent: claude | date: 2026-03-24]\n\nhello";
        let metadata = chunk_metadata_from_header(text);

        assert_eq!(metadata.get("project").unwrap(), "prview-rs");
        assert_eq!(metadata.get("agent").unwrap(), "claude");
        assert_eq!(metadata.get("date").unwrap(), "2026-03-24");
    }

    #[test]
    fn test_chunk_metadata_for_memex_prefers_sidecar() {
        let tmp = std::env::temp_dir().join(format!("ai-ctx-memex-sidecar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let chunk_path = tmp.join("chunk.txt");
        fs::write(
            &chunk_path,
            "[project: wrong | agent: wrong | date: 2026-01-01]\n\nbody",
        )
        .unwrap();
        fs::write(
            chunk_sidecar_path(&chunk_path),
            serde_json::to_vec_pretty(&crate::chunker::ChunkMetadataSidecar {
                id: "chunk".to_string(),
                project: "prview-rs".to_string(),
                agent: "claude".to_string(),
                date: "2026-03-24".to_string(),
                session_id: "sess-1".to_string(),
                cwd: Some("/Users/tester/workspaces/prview-rs".to_string()),
                kind: crate::store::Kind::Conversations,
                run_id: Some("mrbl-001".to_string()),
                prompt_id: Some("api-redesign_20260327".to_string()),
                frame_kind: Some(crate::types::FrameKind::AgentReply),
                agent_model: Some("gpt-5.4".to_string()),
                started_at: Some("2026-03-27T10:00:00Z".to_string()),
                completed_at: Some("2026-03-27T10:01:00Z".to_string()),
                token_usage: Some(1234),
                findings_count: Some(4),
                workflow_phase: Some("implement".to_string()),
                mode: Some("session-first".to_string()),
                skill_code: Some("vc-workflow".to_string()),
                framework_version: Some("2026-03".to_string()),
                intent_entries: Vec::new(),
            })
            .unwrap(),
        )
        .unwrap();

        let metadata = chunk_metadata_for_memex(&chunk_path, "chunk", "body");
        let object = metadata.as_object().unwrap();

        assert_eq!(object.get("project").unwrap(), "prview-rs");
        assert_eq!(object.get("agent").unwrap(), "claude");
        assert_eq!(object.get("date").unwrap(), "2026-03-24");
        assert_eq!(object.get("session_id").unwrap(), "sess-1");
        assert_eq!(object.get("kind").unwrap(), "conversations");
        assert_eq!(
            object.get("cwd").unwrap(),
            "/Users/tester/workspaces/prview-rs"
        );
        assert_eq!(object.get("run_id").unwrap(), "mrbl-001");
        assert_eq!(object.get("prompt_id").unwrap(), "api-redesign_20260327");
        assert_eq!(object.get("agent_model").unwrap(), "gpt-5.4");
        assert_eq!(object.get("started_at").unwrap(), "2026-03-27T10:00:00Z");
        assert_eq!(object.get("completed_at").unwrap(), "2026-03-27T10:01:00Z");
        assert_eq!(object.get("token_usage").unwrap(), 1234);
        assert_eq!(object.get("findings_count").unwrap(), 4);
        assert_eq!(object.get("workflow_phase").unwrap(), "implement");
        assert_eq!(object.get("mode").unwrap(), "session-first");
        assert_eq!(object.get("skill_code").unwrap(), "vc-workflow");
        assert_eq!(object.get("framework_version").unwrap(), "2026-03");
        assert_eq!(
            object.get("path").unwrap(),
            &serde_json::Value::String(chunk_path.to_string_lossy().to_string())
        );
        assert_eq!(
            object.get("content_hash").unwrap(),
            &serde_json::Value::String(compute_content_hash("body"))
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_chunk_memex_record_includes_content_hash() {
        let chunk_path = PathBuf::from("/tmp/ctx/chunk.md");
        let record = chunk_memex_record(&chunk_path, "chunk", "body");

        assert_eq!(record.id, "chunk");
        assert_eq!(record.content_hash, compute_content_hash("body"));
    }
}
