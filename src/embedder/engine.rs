//! BERT-style embedder engine powered by Candle.
//!
//! Accepts either embedded bytes (build-time) or a HuggingFace cache snapshot
//! (runtime). Produces L2-normalised, mean-pooled sentence embeddings.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig};
use serde::Deserialize;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};
use tracing::{debug, info};

use super::embedded;
use crate::hf_cache;

const DEFAULT_MAX_LENGTH: usize = 512;
const DEFAULT_FALLBACK_REPO: &str = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2";
const DEV_FALLBACK_REPO: &str = "harrier-oss/harrier-oss-0.6b";
const PREMIUM_FALLBACK_REPO: &str = "F2-LLM/F2-LLM-v2-1.7b";

/// Where the live embedder weights came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeEmbeddingSource {
    /// Weights were shipped inside the binary via `include_bytes!`.
    Embedded { repo: String },
    /// Weights were loaded from a local HF cache snapshot.
    HfCache { repo: String, path: PathBuf },
    /// Explicit operator-specified directory override.
    ExplicitPath(PathBuf),
}

impl NativeEmbeddingSource {
    pub fn repo(&self) -> &str {
        match self {
            Self::Embedded { repo } => repo,
            Self::HfCache { repo, .. } => repo,
            Self::ExplicitPath(_) => "<explicit-path>",
        }
    }
}

/// Runtime configuration for the native embedder.
#[derive(Debug, Clone, Default)]
pub struct EmbedderConfig {
    /// Preferred HuggingFace repo (overrides env). Falls back to
    /// `AICX_EMBEDDER_REPO`, then to the bundled MiniLM default.
    pub repo: Option<String>,
    /// Explicit path to a model directory (bypasses HF cache lookup).
    pub model_path: Option<PathBuf>,
    /// Cap on input tokens per request (default from the model config).
    pub max_length: Option<usize>,
    /// Whether to prefer embedded bytes when both are available (default: true).
    pub prefer_embedded: bool,
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
    profile: Option<String>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    prefer_embedded: Option<bool>,
    #[serde(default)]
    max_length: Option<usize>,
}

impl NativeEmbedderConfigSection {
    fn merge_from(&mut self, other: Self) {
        if other.profile.is_some() {
            self.profile = other.profile;
        }
        if other.repo.is_some() {
            self.repo = other.repo;
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
    }
}

fn profile_repo(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "base" => Some(DEFAULT_FALLBACK_REPO),
        "dev" => Some(DEV_FALLBACK_REPO),
        "premium" => Some(PREMIUM_FALLBACK_REPO),
        _ => None,
    }
}

fn config_search_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(path) = std::env::var("AICX_EMBEDDER_CONFIG") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            out.push(PathBuf::from(trimmed));
        }
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".aicx").join("embedder.toml"));
        out.push(home.join(".aicx").join("config.toml"));
    }
    out
}

fn load_config_file() -> Option<NativeEmbedderConfigSection> {
    for path in config_search_paths() {
        if !path.exists() {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) => {
                debug!(
                    target: "aicx::embedder",
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
                    target: "aicx::embedder",
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

impl EmbedderConfig {
    pub fn from_env() -> Self {
        let file_cfg = load_config_file().unwrap_or_default();
        Self {
            repo: std::env::var("AICX_EMBEDDER_REPO")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            model_path: std::env::var("AICX_EMBEDDER_PATH")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or(file_cfg.path),
            max_length: file_cfg.max_length,
            prefer_embedded: file_cfg.prefer_embedded.unwrap_or(true),
        }
        .with_profile_fallback(file_cfg.profile.as_deref())
    }

    pub fn with_max_length(mut self, max_length: usize) -> Self {
        self.max_length = Some(max_length);
        self
    }

    fn with_profile_fallback(mut self, profile: Option<&str>) -> Self {
        if self.repo.is_none()
            && let Some(profile) = profile
            && let Some(repo) = profile_repo(profile)
        {
            self.repo = Some(repo.to_string());
        }
        self
    }
}

/// Stateful text embedder. `embed` / `embed_batch` reuse the loaded weights.
pub struct EmbedderEngine {
    model: BertModel,
    tokenizer: Tokenizer,
    config: BertConfig,
    device: Device,
    source: NativeEmbeddingSource,
}

impl EmbedderEngine {
    /// Create a new embedder using environment overrides and default preferences.
    pub fn new() -> Result<Self> {
        Self::with_config(EmbedderConfig::from_env())
    }

    /// Create a new embedder from an explicit config.
    pub fn with_config(config: EmbedderConfig) -> Result<Self> {
        let device = Device::new_metal(0).unwrap_or(Device::Cpu);
        debug!(target: "aicx::embedder", "native embedder device: {:?}", device);

        if let Some(explicit) = config.model_path.as_ref() {
            return Self::from_path(explicit, device, config.max_length);
        }

        if config.prefer_embedded
            && let Some(model) = embedded::get_embedded()
        {
            return Self::from_embedded(&model, device, config.max_length);
        }

        let repo = config
            .repo
            .clone()
            .unwrap_or_else(|| DEFAULT_FALLBACK_REPO.to_string());
        let snapshot = hf_cache::find_snapshot_with_any(
            &repo,
            &["config.json", "tokenizer.json"],
            &[
                "model.safetensors",
                "pytorch_model.safetensors",
                "weights.safetensors",
            ],
        )
        .ok_or_else(|| {
            anyhow!(
                "Native embedder model not available. Embedded bytes absent and HuggingFace cache \
                 has no snapshot for '{repo}'. Either set AICX_EMBEDDER_PATH, prime the HF cache \
                 (e.g. `hf download {repo}`), or rebuild with the `native-embedder` feature after \
                 downloading the model."
            )
        })?;

        Self::from_hf_snapshot(&repo, &snapshot, device, config.max_length)
    }

    fn from_embedded(
        model: &embedded::EmbeddedModel,
        device: Device,
        max_length: Option<usize>,
    ) -> Result<Self> {
        let bert_config: BertConfig = serde_json::from_slice(model.config)
            .context("Failed to parse embedded embedder config.json")?;
        let tokenizer = Tokenizer::from_bytes(model.tokenizer)
            .map_err(|e| anyhow!("Failed to parse embedded tokenizer: {e}"))?;
        let tokenizer = prepare_tokenizer(tokenizer, &bert_config, max_length)?;

        let dtype = device.bf16_default_to_f32();
        let tensors = candle_core::safetensors::load_buffer(model.weights, &Device::Cpu)
            .context("Failed to deserialize embedded embedder weights")?;
        let tensors = move_tensors_to_device(tensors, &device, dtype)?;
        let vb = VarBuilder::from_tensors(tensors, dtype, &device);
        let bert =
            BertModel::load(vb, &bert_config).context("Failed to build embedded embedder model")?;

        info!(
            target: "aicx::embedder",
            "native embedder initialised from embedded bytes (device={:?}, dim={})",
            device, bert_config.hidden_size
        );

        Ok(Self {
            model: bert,
            tokenizer,
            config: bert_config,
            device,
            source: NativeEmbeddingSource::Embedded {
                repo: model.repo.to_string(),
            },
        })
    }

    fn from_hf_snapshot(
        repo: &str,
        model_path: &Path,
        device: Device,
        max_length: Option<usize>,
    ) -> Result<Self> {
        let this = Self::from_path(model_path, device, max_length)?;
        Ok(Self {
            source: NativeEmbeddingSource::HfCache {
                repo: repo.to_string(),
                path: model_path.to_path_buf(),
            },
            ..this
        })
    }

    fn from_path(model_path: &Path, device: Device, max_length: Option<usize>) -> Result<Self> {
        let config_path = model_path.join("config.json");
        let tokenizer_path = model_path.join("tokenizer.json");
        let weights_path = resolve_weights_path(model_path).ok_or_else(|| {
            anyhow!(
                "No safetensors file found in {}. Expected model.safetensors / pytorch_model.safetensors / weights.safetensors.",
                model_path.display()
            )
        })?;

        let config_raw = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;
        let config: BertConfig =
            serde_json::from_str(&config_raw).context("Failed to parse embedder config.json")?;

        let tokenizer_raw = std::fs::read_to_string(&tokenizer_path)
            .with_context(|| format!("Failed to read {}", tokenizer_path.display()))?;
        let tokenizer: Tokenizer = tokenizer_raw
            .parse()
            .map_err(|e| anyhow!("Failed to parse tokenizer.json: {e}"))?;
        let tokenizer = prepare_tokenizer(tokenizer, &config, max_length)?;

        let dtype = device.bf16_default_to_f32();
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[&weights_path], dtype, &device)
                .context("Failed to mmap embedder weights")?
        };
        let model =
            BertModel::load(vb, &config).context("Failed to build embedder model from path")?;

        info!(
            target: "aicx::embedder",
            "native embedder initialised from path {} (device={:?}, dim={})",
            model_path.display(),
            device,
            config.hidden_size
        );

        Ok(Self {
            model,
            tokenizer,
            config,
            device,
            source: NativeEmbeddingSource::ExplicitPath(model_path.to_path_buf()),
        })
    }

    /// Embedding dimension (matches the BERT `hidden_size`).
    pub fn dimension(&self) -> usize {
        self.config.hidden_size
    }

    /// Source description — useful for diagnostics and tests.
    pub fn source(&self) -> &NativeEmbeddingSource {
        &self.source
    }

    /// Embed a single string.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        self.embed_batch(&[text])?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("no embedding generated"))
    }

    /// Embed a batch of strings in a single forward pass.
    pub fn embed_batch(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let owned: Vec<String> = texts.iter().map(|t| (*t).to_string()).collect();
        let (input_ids, token_type_ids, attention_mask) = encode_batch(
            &self.tokenizer,
            &owned,
            self.config.pad_token_id as u32,
            self.device.clone(),
        )?;

        let outputs = self
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))?;
        let pooled = mean_pool(&outputs, &attention_mask)?;
        let normalised = l2_normalize(&pooled)?
            .to_dtype(DType::F32)?
            .to_device(&Device::Cpu)?;
        normalised
            .to_vec2::<f32>()
            .context("Failed to extract embeddings into Vec")
    }

    /// Cosine similarity helper for downstream consumers.
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
}

fn resolve_weights_path(model_path: &Path) -> Option<PathBuf> {
    for candidate in [
        "model.safetensors",
        "pytorch_model.safetensors",
        "weights.safetensors",
    ] {
        let p = model_path.join(candidate);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn prepare_tokenizer(
    tokenizer: Tokenizer,
    config: &BertConfig,
    max_length_override: Option<usize>,
) -> Result<Tokenizer> {
    let max_len = max_length_override
        .unwrap_or(config.max_position_embeddings)
        .min(DEFAULT_MAX_LENGTH);

    let pad_id = config.pad_token_id as u32;
    let pad_token = tokenizer
        .id_to_token(pad_id)
        .unwrap_or_else(|| "[PAD]".to_string());

    let mut tokenizer = tokenizer;
    tokenizer.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::BatchLongest,
        pad_id,
        pad_token,
        ..Default::default()
    }));
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: max_len,
            ..Default::default()
        }))
        .map_err(anyhow::Error::msg)?;

    Ok(tokenizer)
}

fn encode_batch(
    tokenizer: &Tokenizer,
    inputs: &[String],
    pad_id: u32,
    device: Device,
) -> Result<(Tensor, Tensor, Tensor)> {
    let encodings = tokenizer
        .encode_batch(inputs.to_vec(), true)
        .map_err(|e| anyhow!("Tokenization failed: {e}"))?;

    let max_len = encodings.iter().map(|e| e.len()).max().unwrap_or(0);

    let mut input_ids = Vec::with_capacity(encodings.len() * max_len);
    let mut token_type_ids = Vec::with_capacity(encodings.len() * max_len);
    let mut attention_mask = Vec::with_capacity(encodings.len() * max_len);

    for enc in encodings {
        let ids = enc.get_ids();
        let types = enc.get_type_ids();
        let mask = enc.get_attention_mask();

        let mut ids_vec = ids.to_vec();
        let mut type_vec = if types.is_empty() {
            vec![0u32; ids.len()]
        } else {
            types.to_vec()
        };
        let mut mask_vec = mask.to_vec();

        pad_to(&mut ids_vec, max_len, pad_id);
        pad_to(&mut type_vec, max_len, 0);
        pad_to(&mut mask_vec, max_len, 0);

        input_ids.extend_from_slice(&ids_vec);
        token_type_ids.extend_from_slice(&type_vec);
        attention_mask.extend_from_slice(&mask_vec);
    }

    let batch = inputs.len();
    let input_ids = Tensor::from_vec(input_ids, (batch, max_len), &device)?.to_dtype(DType::I64)?;
    let token_type_ids =
        Tensor::from_vec(token_type_ids, (batch, max_len), &device)?.to_dtype(DType::I64)?;
    let attention_mask =
        Tensor::from_vec(attention_mask, (batch, max_len), &device)?.to_dtype(DType::F32)?;

    Ok((input_ids, token_type_ids, attention_mask))
}

fn pad_to(vec: &mut Vec<u32>, target_len: usize, pad: u32) {
    if vec.len() < target_len {
        vec.extend(std::iter::repeat_n(pad, target_len - vec.len()));
    }
}

fn mean_pool(hidden: &Tensor, mask: &Tensor) -> Result<Tensor> {
    let dtype = hidden.dtype();
    let mask = mask.to_dtype(dtype)?;
    let mask = mask.unsqueeze(2)?;
    let masked = hidden.broadcast_mul(&mask)?;
    let sum = masked.sum(1)?;
    let counts = mask.sum(1)?;
    let eps = Tensor::from_vec(vec![1e-9f32], (1,), hidden.device())?.to_dtype(dtype)?;
    let counts = counts.broadcast_add(&eps)?;
    Ok(sum.broadcast_div(&counts)?)
}

fn l2_normalize(t: &Tensor) -> Result<Tensor> {
    let dtype = t.dtype();
    let squared = t.sqr()?;
    let sum = squared.sum(1)?.unsqueeze(1)?;
    let norm = sum.sqrt()?;
    let eps = Tensor::from_vec(vec![1e-9f32], (1,), t.device())?.to_dtype(dtype)?;
    let norm = norm.broadcast_add(&eps)?;
    Ok(t.broadcast_div(&norm)?)
}

fn move_tensors_to_device(
    tensors: HashMap<String, Tensor>,
    device: &Device,
    dtype: DType,
) -> Result<HashMap<String, Tensor>> {
    let mut result = HashMap::with_capacity(tensors.len());
    for (name, tensor) in tensors {
        let mut t = tensor;
        if t.dtype() != dtype {
            t = t.to_dtype(dtype)?;
        }
        t = t.to_device(device)?;
        result.insert(name, t);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_identical_vectors() {
        let v = vec![1.0f32, 0.0, 0.0];
        assert!((EmbedderEngine::similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn similarity_orthogonal_vectors() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        assert!(EmbedderEngine::similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn similarity_length_mismatch_is_zero() {
        assert_eq!(
            EmbedderEngine::similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]),
            0.0
        );
    }

    #[test]
    fn config_from_env_respects_repo_override() {
        // SAFETY: a cargo test process has no concurrent readers of AICX_EMBEDDER_REPO.
        unsafe {
            std::env::set_var("AICX_EMBEDDER_REPO", "harrier-oss/harrier-oss-0.6b");
        }
        let cfg = EmbedderConfig::from_env();
        assert_eq!(cfg.repo.as_deref(), Some("harrier-oss/harrier-oss-0.6b"));
        unsafe {
            std::env::remove_var("AICX_EMBEDDER_REPO");
        }
    }

    #[test]
    fn config_from_file_uses_profile_when_repo_missing() {
        let temp_dir =
            std::env::temp_dir().join(format!("aicx-embedder-config-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let config_path = temp_dir.join("embedder.toml");
        std::fs::write(
            &config_path,
            "[native_embedder]\nprofile = \"premium\"\nprefer_embedded = false\n",
        )
        .expect("write config");

        // SAFETY: tests are single-process for this env access pattern.
        unsafe {
            std::env::remove_var("AICX_EMBEDDER_REPO");
            std::env::remove_var("AICX_EMBEDDER_PATH");
            std::env::set_var("AICX_EMBEDDER_CONFIG", &config_path);
        }

        let cfg = EmbedderConfig::from_env();
        assert_eq!(cfg.repo.as_deref(), Some(PREMIUM_FALLBACK_REPO));
        assert!(!cfg.prefer_embedded);

        // SAFETY: tests are single-process for this env access pattern.
        unsafe {
            std::env::remove_var("AICX_EMBEDDER_CONFIG");
        }
        let _ = std::fs::remove_file(&config_path);
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
