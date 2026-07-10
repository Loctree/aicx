//! Reusable local embedding providers for AICX and rust-memex.
//!
//! The first production backend is GGUF through llama.cpp. Models are resolved
//! from an explicit path or from the local HuggingFace cache; this crate never
//! performs network downloads on its own.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use std::fmt;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use serde::Deserialize;

pub mod cloud;
mod config;
pub mod hf_cache;

#[cfg(feature = "gguf")]
mod gguf;

#[cfg(feature = "cloud")]
pub use cloud::CloudEmbeddingProvider;
pub use cloud::{
    CloudEmbeddingConfig, DEFAULT_CLOUD_DIMENSION, DEFAULT_CLOUD_EMBED_BATCH, DEFAULT_TIMEOUT_SECS,
};

/// Maximum input byte length any embedder backend will accept (D-9). Inputs
/// over this size short-circuit with a structured error before the embedder
/// touches the tokenizer, HTTP body, or local context. 32 KiB is large
/// enough for any sane prefix slice yet small enough to keep cloud POSTs
/// snappy and prevent runaway local tokenization.
pub const MAX_EMBED_INPUT_BYTES: usize = 32 * 1024;

/// Trim `text` to `MAX_EMBED_INPUT_BYTES` and reject empty / whitespace-only
/// payloads. Centralized so cloud + gguf backends apply the same budget and
/// rejection message. Returns the (possibly-truncated) input on success.
pub fn enforce_embed_input_budget(text: &str) -> Result<&str> {
    if text.trim().is_empty() {
        return Err(anyhow!(
            "embedder input is empty or whitespace-only; supply a non-empty query"
        ));
    }
    if text.len() > MAX_EMBED_INPUT_BYTES {
        return Err(anyhow!(
            "embedder input {} bytes exceeds {} byte budget; truncate or rephrase before embedding",
            text.len(),
            MAX_EMBED_INPUT_BYTES
        ));
    }
    Ok(text)
}
pub use config::{
    ConfigSource, DEFAULT_BASE_FILENAME, DEFAULT_BASE_REPO, DEFAULT_DEV_FILENAME, DEFAULT_DEV_REPO,
    DEFAULT_PREMIUM_FILENAME, DEFAULT_PREMIUM_REPO, config_search_paths,
    config_search_paths_with_source, effective_config_source, find_cached_model_file, profile_spec,
};

/// Backend preference from config/env.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendPreference {
    /// Let the crate choose the strongest compiled local backend.
    Auto,
    /// Cloud HTTP embedder (OpenAI-compatible `/v1/embeddings`). The
    /// Vetcoders default for zero-install operator workflows; the actual
    /// `[embedder.cloud]` section in config supplies URL + model + key.
    Cloud,
    /// GGUF through llama.cpp. The first-choice local backend for
    /// offline / dev workstations.
    #[default]
    Gguf,
    /// Legacy Candle/BERT selector retained only for clear diagnostics.
    Candle,
}

impl BackendPreference {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" => None,
            "auto" => Some(Self::Auto),
            "cloud" | "http" | "openai" | "openai-compat" => Some(Self::Cloud),
            "gguf" | "llama" | "llama.cpp" | "llamacpp" => Some(Self::Gguf),
            "candle" | "bert" | "safetensors" => Some(Self::Candle),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cloud => "cloud",
            Self::Gguf => "gguf",
            Self::Candle => "candle",
        }
    }
}

impl fmt::Display for BackendPreference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Operator-facing profile for the local embedder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmbeddingProfile {
    /// Portable F2LLM 0.6B Q4_K_M, roughly 397 MB.
    #[default]
    Base,
    /// Workstation F2LLM 1.7B Q4_K_M, roughly 1.1 GB.
    Dev,
    /// Stronger 1.7B Q6_K, roughly 1.4 GB. Heavy retrieval should still live
    /// in rust-memex/Roost when operators need the full retrieval plane.
    Premium,
}

impl EmbeddingProfile {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" => None,
            "base" | "default" | "portable" => Some(Self::Base),
            "dev" | "workstation" => Some(Self::Dev),
            "premium" | "strong" | "heavy" => Some(Self::Premium),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Dev => "dev",
            Self::Premium => "premium",
        }
    }
}

impl fmt::Display for EmbeddingProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Resolved model preset metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingProfileSpec {
    pub profile: EmbeddingProfile,
    pub repo: &'static str,
    pub filename: &'static str,
    pub dimension_hint: usize,
    pub approx_size: &'static str,
    pub description: &'static str,
}

/// Runtime configuration for local embeddings.
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// Backend selection. Defaults to GGUF as the first-choice local path.
    pub backend: BackendPreference,
    /// Profile used when repo/filename are not explicitly pinned.
    pub profile: EmbeddingProfile,
    /// HuggingFace repo override. For GGUF this should point at a GGUF repo.
    pub repo: Option<String>,
    /// Exact model file inside the repo/snapshot, e.g. `*.Q4_K_M.gguf`.
    pub filename: Option<String>,
    /// Explicit model file or directory. A file wins over repo lookup.
    pub model_path: Option<PathBuf>,
    /// Max tokens submitted per text.
    pub max_length: Option<usize>,
    /// llama.cpp context thread count.
    pub threads: Option<i32>,
    /// Number of layers to offload when the build/backend supports it.
    pub gpu_layers: Option<u32>,
    /// Compatibility knob for older configs; GGUF builds do not embed by default.
    pub prefer_embedded: bool,
    /// Cloud HTTP embedder configuration. Populated from
    /// `[embedder.cloud]` in `~/.aicx/config.toml`. Only consulted when
    /// `backend == Cloud` (or `Auto` without a hydrated GGUF model).
    pub cloud: Option<CloudEmbeddingConfig>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            backend: BackendPreference::Gguf,
            profile: EmbeddingProfile::Base,
            repo: None,
            filename: None,
            model_path: None,
            max_length: Some(512),
            threads: None,
            gpu_layers: None,
            prefer_embedded: false,
            cloud: None,
        }
    }
}

impl EmbeddingConfig {
    /// Load config from `~/.aicx/embedder.toml` / env with env taking priority.
    pub fn from_env() -> Self {
        config::load_from_env()
    }

    pub fn with_profile(mut self, profile: EmbeddingProfile) -> Self {
        self.profile = profile;
        self
    }

    pub fn with_model_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.model_path = Some(path.into());
        self
    }

    pub fn with_max_length(mut self, max_length: usize) -> Self {
        self.max_length = Some(max_length);
        self
    }

    pub fn resolved_model(&self) -> ResolvedEmbeddingModel {
        config::resolve_model(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEmbeddingModel {
    pub profile: EmbeddingProfile,
    pub repo: String,
    pub filename: String,
    pub dimension_hint: usize,
    pub approx_size: String,
    pub from_legacy_repo: bool,
}

/// Where the live embedder weights came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeEmbeddingSource {
    /// Weights were loaded from a local HF cache snapshot.
    HfCache {
        repo: String,
        filename: String,
        path: PathBuf,
    },
    /// Explicit operator-specified model file.
    ExplicitPath(PathBuf),
    /// Remote or local HTTP endpoint for cloud-compatible embeddings.
    CloudEndpoint(String),
}

impl NativeEmbeddingSource {
    pub fn repo(&self) -> &str {
        match self {
            Self::HfCache { repo, .. } => repo,
            Self::ExplicitPath(_) => "<explicit-path>",
            Self::CloudEndpoint(_) => "<cloud-endpoint>",
        }
    }

    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::HfCache { path, .. } => path,
            Self::ExplicitPath(path) => path,
            Self::CloudEndpoint(_) => std::path::Path::new(""),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingModelInfo {
    pub model_id: String,
    pub dimension: usize,
    pub backend: String,
    pub profile: EmbeddingProfile,
    pub source: NativeEmbeddingSource,
}

/// Minimal provider surface rust-memex can adapt without inheriting AICX.
pub trait LocalEmbeddingProvider: Send {
    fn info(&self) -> &EmbeddingModelInfo;

    fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        self.embed_batch(&[text.to_string()])?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no embedding generated"))
    }
}

/// Resolve the per-request embedding batch size from config + env.
///
/// Precedence (highest wins): `AICX_EMBED_BATCH` env override →
/// `[embedder.cloud] batch_size` (cloud backend only) →
/// [`DEFAULT_CLOUD_EMBED_BATCH`] for cloud / `1` for every other backend.
/// The result is clamped to `>= 1` so a bogus `0` degrades to serial
/// embedding instead of an empty-batch loop.
///
/// GGUF defaults to `1` on purpose: its `embed_batch` sizes a llama.cpp
/// context to the batch's total token count, so a large default batch
/// would inflate local memory on workstation builds. Operators who want
/// GGUF batching opt in explicitly via `AICX_EMBED_BATCH`.
fn resolve_embed_batch_size(config: &EmbeddingConfig, resolved_backend: &str) -> usize {
    let default = if resolved_backend == "cloud" {
        config
            .cloud
            .as_ref()
            .and_then(|cloud| cloud.batch_size)
            .unwrap_or(DEFAULT_CLOUD_EMBED_BATCH)
    } else {
        1
    };
    std::env::var("AICX_EMBED_BATCH")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(default)
        .max(1)
}

/// Stateful local embedding engine. This hides the concrete backend.
pub struct EmbeddingEngine {
    inner: Box<dyn LocalEmbeddingProvider>,
    /// Chunk texts embedded per `embed_batch` call the index builder
    /// should target. Resolved once at construction from config + env
    /// (see [`resolve_embed_batch_size`]); `1` means serial embedding.
    batch_size: usize,
}

impl EmbeddingEngine {
    pub fn new() -> Result<Self> {
        Self::with_config(EmbeddingConfig::from_env())
    }

    pub fn with_config(config: EmbeddingConfig) -> Result<Self> {
        let mut engine = Self::build_backend(config.clone())?;
        let backend = engine.info().backend.clone();
        engine.batch_size = resolve_embed_batch_size(&config, &backend);
        Ok(engine)
    }

    /// Recommended number of chunk texts to hand a single `embed_batch`
    /// call during an index build. `1` for serial (GGUF default, or a
    /// cloud config that pins it); higher collapses per-request latency.
    pub fn embed_batch_size(&self) -> usize {
        self.batch_size
    }

    fn build_backend(config: EmbeddingConfig) -> Result<Self> {
        match config.backend {
            BackendPreference::Cloud => Self::with_cloud(config),
            BackendPreference::Gguf => Self::with_gguf(config),
            // D-7: `backend = "auto"` prefers the local GGUF backend; on
            // failure (model not hydrated, feature missing, …) falls back
            // to the cloud backend if a `[embedder.cloud]` section is
            // configured. Operators get usable retrieval on a fresh machine
            // before they finish downloading the local weights.
            BackendPreference::Auto => {
                let cloud_available = config.cloud.is_some();
                let cloud_fallback = config.clone();
                match Self::with_gguf(config) {
                    Ok(engine) => Ok(engine),
                    Err(gguf_err) if cloud_available => match Self::with_cloud(cloud_fallback) {
                        Ok(engine) => Ok(engine),
                        Err(cloud_err) => Err(anyhow!(
                            "embedder=auto: failed GGUF ({gguf_err}); cloud fallback also failed: {cloud_err}"
                        )),
                    },
                    Err(gguf_err) => Err(anyhow!(
                        "embedder=auto: failed GGUF ({gguf_err}); no [embedder.cloud] section configured for cloud fallback"
                    )),
                }
            }
            BackendPreference::Candle => Err(anyhow!(
                "Candle/BERT native embedding is no longer the first-choice AICX backend. \
                 Set backend=\"gguf\" or backend=\"cloud\", or use an older build that \
                 explicitly enabled the legacy Candle path."
            )),
        }
    }

    #[cfg(feature = "cloud")]
    fn with_cloud(config: EmbeddingConfig) -> Result<Self> {
        let cloud_cfg = config.cloud.clone().ok_or_else(|| {
            anyhow!(
                "backend=\"cloud\" but no [embedder.cloud] section in config; \
                 add url + model + api_key_env to ~/.aicx/config.toml"
            )
        })?;
        Ok(Self {
            inner: Box::new(cloud::CloudEmbeddingProvider::new(cloud_cfg)?),
            // Overwritten by `with_config` once the backend is known.
            batch_size: 1,
        })
    }

    #[cfg(not(feature = "cloud"))]
    fn with_cloud(_config: EmbeddingConfig) -> Result<Self> {
        Err(anyhow!(
            "AICX cloud embedder is not compiled in. Rebuild with feature `cloud` \
             (or AICX feature `cloud-embedder`)."
        ))
    }

    #[cfg(feature = "gguf")]
    fn with_gguf(config: EmbeddingConfig) -> Result<Self> {
        Ok(Self {
            inner: Box::new(gguf::GgufEmbeddingProvider::with_config(config)?),
            // Overwritten by `with_config` once the backend is known.
            batch_size: 1,
        })
    }

    #[cfg(not(feature = "gguf"))]
    fn with_gguf(_config: EmbeddingConfig) -> Result<Self> {
        Err(anyhow!(
            "AICX local GGUF embedder is not compiled in. Rebuild with feature `gguf` \
             (or AICX feature `native-embedder`)."
        ))
    }

    pub fn info(&self) -> &EmbeddingModelInfo {
        self.inner.info()
    }

    pub fn dimension(&self) -> usize {
        self.info().dimension
    }

    pub fn source(&self) -> &NativeEmbeddingSource {
        &self.info().source
    }

    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        self.inner.embed(text)
    }

    pub fn embed_batch<T: AsRef<str>>(&mut self, texts: &[T]) -> Result<Vec<Vec<f32>>> {
        let owned: Vec<String> = texts.iter().map(|text| text.as_ref().to_string()).collect();
        self.inner.embed_batch(&owned)
    }

    pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
        similarity(a, b)
    }
}

pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in v {
            *value /= norm;
        }
    }
}

#[derive(Debug, Default, Deserialize, Clone)]
struct NativeEmbedderConfigFile {
    #[serde(default)]
    native_embedder: Option<NativeEmbedderConfigSection>,
    #[serde(default)]
    embedder: Option<NativeEmbedderConfigSection>,
    #[serde(flatten)]
    top_level: NativeEmbedderConfigSection,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct NativeEmbedderConfigSection {
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    prefer_embedded: Option<bool>,
    #[serde(default)]
    max_length: Option<usize>,
    #[serde(default)]
    threads: Option<i32>,
    #[serde(default)]
    gpu_layers: Option<u32>,
    /// Nested `[embedder.cloud]` (or `[native_embedder.cloud]`) section.
    /// Populated when the operator selects the cloud backend.
    #[serde(default)]
    cloud: Option<CloudEmbeddingConfig>,
}

impl NativeEmbedderConfigSection {
    fn merge_from(&mut self, other: Self) {
        if other.backend.is_some() {
            self.backend = other.backend;
        }
        if other.profile.is_some() {
            self.profile = other.profile;
        }
        if other.repo.is_some() {
            self.repo = other.repo;
        }
        if other.filename.is_some() {
            self.filename = other.filename;
        }
        if other.file.is_some() {
            self.file = other.file;
        }
        if other.path.is_some() {
            self.path = other.path;
        }
        if other.prefer_embedded.is_some() {
            self.prefer_embedded = other.prefer_embedded;
        }
        if other.max_length.is_some() {
            self.max_length = other.max_length;
        }
        if other.threads.is_some() {
            self.threads = other.threads;
        }
        if other.gpu_layers.is_some() {
            self.gpu_layers = other.gpu_layers;
        }
        if other.cloud.is_some() {
            self.cloud = other.cloud;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_identical_vectors() {
        let v = vec![1.0f32, 0.0, 0.0];
        assert!((similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn enforce_embed_input_budget_rejects_empty_and_whitespace() {
        assert!(enforce_embed_input_budget("").is_err());
        assert!(enforce_embed_input_budget("   \n\t  ").is_err());
    }

    #[test]
    fn enforce_embed_input_budget_rejects_inputs_over_32kib() {
        let big = "x".repeat(MAX_EMBED_INPUT_BYTES + 1);
        let err = enforce_embed_input_budget(&big)
            .expect_err("input above the 32 KiB cap must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("exceeds"),
            "error must reference budget breach: {msg}"
        );
    }

    #[test]
    fn enforce_embed_input_budget_accepts_normal_query_and_exact_limit() {
        assert_eq!(
            enforce_embed_input_budget("how does the noise filter work").unwrap(),
            "how does the noise filter work"
        );
        let exact = "x".repeat(MAX_EMBED_INPUT_BYTES);
        assert!(
            enforce_embed_input_budget(&exact).is_ok(),
            "input exactly at the cap must be accepted"
        );
    }

    #[test]
    fn similarity_length_mismatch_is_zero() {
        assert_eq!(similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
    }

    // `AICX_EMBED_BATCH` is process-global; serialize the resolver tests
    // that mutate it so they stay correct under `cargo test` parallelism
    // (mirrors the AICX_HOME_ENV_LOCK pattern used elsewhere in the tree).
    static EMBED_BATCH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard: set (or clear) `AICX_EMBED_BATCH` for the test body and
    /// restore the prior value on drop so tests never leak env state into
    /// each other.
    struct BatchEnvGuard {
        prior: Option<String>,
    }

    impl BatchEnvGuard {
        fn set(value: Option<&str>) -> Self {
            let prior = std::env::var("AICX_EMBED_BATCH").ok();
            match value {
                Some(v) => unsafe { std::env::set_var("AICX_EMBED_BATCH", v) },
                None => unsafe { std::env::remove_var("AICX_EMBED_BATCH") },
            }
            Self { prior }
        }
    }

    impl Drop for BatchEnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => unsafe { std::env::set_var("AICX_EMBED_BATCH", v) },
                None => unsafe { std::env::remove_var("AICX_EMBED_BATCH") },
            }
        }
    }

    fn cloud_config_with_batch(batch: Option<usize>) -> EmbeddingConfig {
        EmbeddingConfig {
            backend: BackendPreference::Cloud,
            cloud: Some(CloudEmbeddingConfig {
                url: "http://127.0.0.1:11434/v1/embeddings".to_string(),
                model: "qwen3-embedding".to_string(),
                batch_size: batch,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn embed_batch_size_cloud_defaults_to_sixteen() {
        let _lock = EMBED_BATCH_ENV_LOCK.lock().expect("env lock");
        let _env = BatchEnvGuard::set(None);
        let cfg = cloud_config_with_batch(None);
        assert_eq!(
            resolve_embed_batch_size(&cfg, "cloud"),
            DEFAULT_CLOUD_EMBED_BATCH
        );
    }

    #[test]
    fn embed_batch_size_cloud_config_value_wins_over_default() {
        let _lock = EMBED_BATCH_ENV_LOCK.lock().expect("env lock");
        let _env = BatchEnvGuard::set(None);
        let cfg = cloud_config_with_batch(Some(8));
        assert_eq!(resolve_embed_batch_size(&cfg, "cloud"), 8);
    }

    #[test]
    fn embed_batch_size_env_overrides_config() {
        let _lock = EMBED_BATCH_ENV_LOCK.lock().expect("env lock");
        let _env = BatchEnvGuard::set(Some("32"));
        let cfg = cloud_config_with_batch(Some(8));
        assert_eq!(resolve_embed_batch_size(&cfg, "cloud"), 32);
    }

    #[test]
    fn embed_batch_size_gguf_defaults_to_serial_but_honors_env() {
        let _lock = EMBED_BATCH_ENV_LOCK.lock().expect("env lock");
        let cfg = EmbeddingConfig::default();

        let _off = BatchEnvGuard::set(None);
        assert_eq!(
            resolve_embed_batch_size(&cfg, "gguf"),
            1,
            "gguf must default to serial embedding"
        );
        drop(_off);

        let _on = BatchEnvGuard::set(Some("4"));
        assert_eq!(
            resolve_embed_batch_size(&cfg, "gguf"),
            4,
            "explicit env opt-in must batch gguf too"
        );
    }

    #[test]
    fn embed_batch_size_clamps_zero_and_ignores_garbage_env() {
        let _lock = EMBED_BATCH_ENV_LOCK.lock().expect("env lock");
        let cfg = cloud_config_with_batch(Some(0));

        let _zero_env = BatchEnvGuard::set(Some("0"));
        assert_eq!(
            resolve_embed_batch_size(&cfg, "cloud"),
            1,
            "a zero batch size must clamp to serial, never an empty batch"
        );
        drop(_zero_env);

        let _garbage = BatchEnvGuard::set(Some("not-a-number"));
        // Garbage env is ignored; falls back to the (clamped) config value.
        assert_eq!(resolve_embed_batch_size(&cfg, "cloud"), 1);
    }

    #[test]
    fn default_profile_resolves_to_base_gguf() {
        let cfg = EmbeddingConfig::default();
        let resolved = cfg.resolved_model();
        assert_eq!(resolved.profile, EmbeddingProfile::Base);
        assert_eq!(resolved.repo, DEFAULT_BASE_REPO);
        assert_eq!(resolved.filename, DEFAULT_BASE_FILENAME);
        assert_eq!(resolved.dimension_hint, 1024);
    }

    #[test]
    fn legacy_non_gguf_repo_uses_profile_spec() {
        let cfg = EmbeddingConfig {
            profile: EmbeddingProfile::Dev,
            repo: Some("microsoft/harrier-oss-v1-0.6b".to_string()),
            ..Default::default()
        };
        let resolved = cfg.resolved_model();
        assert!(resolved.from_legacy_repo);
        assert_eq!(resolved.repo, DEFAULT_DEV_REPO);
        assert_eq!(resolved.filename, DEFAULT_DEV_FILENAME);
    }

    #[cfg(not(any(feature = "gguf", feature = "cloud")))]
    #[test]
    fn auto_with_cloud_config_attempts_cloud_fallback_after_gguf() {
        let cfg = EmbeddingConfig {
            backend: BackendPreference::Auto,
            cloud: Some(CloudEmbeddingConfig {
                url: "http://127.0.0.1:65535/v1/embeddings".to_string(),
                model: "test-model".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let err = match EmbeddingEngine::with_config(cfg) {
            Ok(_) => panic!("auto without compiled backends should not construct an engine"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("failed GGUF"));
        assert!(err.contains("cloud fallback"));
    }
}
