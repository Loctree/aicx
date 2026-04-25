//! Reusable local embedding providers for AICX and rust-memex.
//!
//! The first production backend is GGUF through llama.cpp. Models are resolved
//! from an explicit path or from the local HuggingFace cache; this crate never
//! performs network downloads on its own.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::fmt;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use serde::Deserialize;

mod config;
mod hf_cache;

#[cfg(feature = "gguf")]
mod gguf;

pub use config::{
    DEFAULT_BASE_FILENAME, DEFAULT_BASE_REPO, DEFAULT_DEV_FILENAME, DEFAULT_DEV_REPO,
    DEFAULT_PREMIUM_FILENAME, DEFAULT_PREMIUM_REPO, config_search_paths, find_cached_model_file,
    profile_spec,
};

/// Backend preference from config/env.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendPreference {
    /// Let the crate choose the strongest compiled local backend.
    Auto,
    /// GGUF through llama.cpp. This is the production first choice.
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
            "gguf" | "llama" | "llama.cpp" | "llamacpp" => Some(Self::Gguf),
            "candle" | "bert" | "safetensors" => Some(Self::Candle),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
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
}

impl NativeEmbeddingSource {
    pub fn repo(&self) -> &str {
        match self {
            Self::HfCache { repo, .. } => repo,
            Self::ExplicitPath(_) => "<explicit-path>",
        }
    }

    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::HfCache { path, .. } => path,
            Self::ExplicitPath(path) => path,
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

/// Stateful local embedding engine. This hides the concrete backend.
pub struct EmbeddingEngine {
    inner: Box<dyn LocalEmbeddingProvider>,
}

impl EmbeddingEngine {
    pub fn new() -> Result<Self> {
        Self::with_config(EmbeddingConfig::from_env())
    }

    pub fn with_config(config: EmbeddingConfig) -> Result<Self> {
        match config.backend {
            BackendPreference::Auto | BackendPreference::Gguf => Self::with_gguf(config),
            BackendPreference::Candle => Err(anyhow!(
                "Candle/BERT native embedding is no longer the first-choice AICX backend. \
                 Set backend=\"gguf\" and hydrate an F2LLM GGUF model, or use an older build \
                 that explicitly enabled the legacy Candle path."
            )),
        }
    }

    #[cfg(feature = "gguf")]
    fn with_gguf(config: EmbeddingConfig) -> Result<Self> {
        Ok(Self {
            inner: Box::new(gguf::GgufEmbeddingProvider::with_config(config)?),
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
    fn similarity_length_mismatch_is_zero() {
        assert_eq!(similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
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
}
