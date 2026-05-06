use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow};
use llama_cpp_2::{
    context::params::{LlamaAttentionType, LlamaContextParams, LlamaPoolingType},
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::params::LlamaModelParams,
    model::{AddBos, LlamaModel},
};
use tracing::{info, warn};

use crate::config::{find_cached_model_file, resolve_explicit_model_path};
use crate::{
    EmbeddingConfig, EmbeddingModelInfo, LocalEmbeddingProvider, NativeEmbeddingSource,
    ResolvedEmbeddingModel, l2_normalize,
};

static LLAMA_BACKEND: OnceLock<std::result::Result<LlamaBackend, String>> = OnceLock::new();

pub struct GgufEmbeddingProvider {
    model: LlamaModel,
    info: EmbeddingModelInfo,
    max_length: usize,
    threads: Option<i32>,
}

impl GgufEmbeddingProvider {
    pub fn with_config(config: EmbeddingConfig) -> Result<Self> {
        let resolved = config.resolved_model();
        let model_path = resolve_model_path(&config, &resolved)?;
        let backend = global_backend()?;

        let mut model_params = LlamaModelParams::default().with_use_mmap(true);
        if let Some(gpu_layers) = config.gpu_layers.filter(|layers| *layers > 0) {
            model_params = model_params.with_n_gpu_layers(gpu_layers);
        }

        let model = LlamaModel::load_from_file(backend, &model_path, &model_params)
            .with_context(|| format!("failed to load GGUF embedder {}", model_path.display()))?;
        let dimension = usize::try_from(model.n_embd())
            .map_err(|_| anyhow!("GGUF model reported a negative embedding dimension"))?;
        let source = source_for_path(&config, &resolved, model_path);
        let model_id = match &source {
            NativeEmbeddingSource::HfCache { repo, filename, .. } => {
                format!("{repo}/{filename}")
            }
            NativeEmbeddingSource::ExplicitPath(path) => path.display().to_string(),
        };

        if resolved.from_legacy_repo {
            warn!(
                target: "aicx_embeddings::gguf",
                "legacy non-GGUF embedder repo found in config; using {} instead",
                model_id
            );
        }

        info!(
            target: "aicx_embeddings::gguf",
            "GGUF embedder loaded: {} (dim={}, profile={})",
            model_id,
            dimension,
            resolved.profile
        );

        Ok(Self {
            model,
            info: EmbeddingModelInfo {
                model_id,
                dimension,
                backend: "gguf".to_string(),
                profile: resolved.profile,
                source,
            },
            max_length: config.max_length.unwrap_or(512).max(1),
            threads: config.threads,
        })
    }
}

impl LocalEmbeddingProvider for GgufEmbeddingProvider {
    fn info(&self) -> &EmbeddingModelInfo {
        &self.info
    }

    fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut tokenized = Vec::with_capacity(texts.len());
        let mut max_seq_len = 1usize;
        let mut total_tokens = 0usize;

        for text in texts {
            let mut tokens = self
                .model
                .str_to_token(text, AddBos::Always)
                .context("failed to tokenize text for GGUF embeddings")?;
            if tokens.len() > self.max_length {
                tokens.truncate(self.max_length);
            }
            if tokens.is_empty() {
                return Err(anyhow!("tokenizer returned an empty sequence"));
            }
            max_seq_len = max_seq_len.max(tokens.len());
            total_tokens += tokens.len();
            tokenized.push(tokens);
        }

        let n_ctx = NonZeroU32::new(u32::try_from(max_seq_len)?)
            .ok_or_else(|| anyhow!("GGUF context length must be non-zero"))?;
        let n_batch = u32::try_from(total_tokens)?;
        let n_seq_max = u32::try_from(texts.len())?;
        let n_seq_batch = i32::try_from(texts.len())?;

        let mut params = LlamaContextParams::default()
            .with_embeddings(true)
            .with_pooling_type(LlamaPoolingType::Mean)
            .with_attention_type(LlamaAttentionType::NonCausal)
            .with_n_ctx(Some(n_ctx))
            .with_n_batch(n_batch)
            .with_n_ubatch(n_batch)
            .with_n_seq_max(n_seq_max);
        if let Some(threads) = self.threads.filter(|threads| *threads > 0) {
            params = params.with_n_threads(threads).with_n_threads_batch(threads);
        }

        let backend = global_backend()?;
        let mut context = self
            .model
            .new_context(backend, params)
            .context("failed to create GGUF embedding context")?;
        let mut batch = LlamaBatch::new(total_tokens, n_seq_batch);

        for (seq_id, tokens) in tokenized.iter().enumerate() {
            batch
                .add_sequence(tokens, i32::try_from(seq_id)?, true)
                .context("failed to add token sequence to llama batch")?;
        }

        context
            .decode(&mut batch)
            .context("llama.cpp failed to decode embedding batch")?;

        let mut out = Vec::with_capacity(texts.len());
        for seq_id in 0..texts.len() {
            let mut embedding = context
                .embeddings_seq_ith(i32::try_from(seq_id)?)
                .context("llama.cpp did not return a pooled sequence embedding")?
                .to_vec();
            l2_normalize(&mut embedding);
            out.push(embedding);
        }
        Ok(out)
    }
}

fn global_backend() -> Result<&'static LlamaBackend> {
    let result = LLAMA_BACKEND.get_or_init(|| {
        let mut backend = LlamaBackend::init().map_err(|err| err.to_string())?;
        if std::env::var("AICX_LLAMA_LOGS")
            .map(|value| value != "0")
            .unwrap_or(false)
        {
            // Keep llama.cpp logs visible only when explicitly requested.
        } else {
            backend.void_logs();
        }
        Ok(backend)
    });
    result
        .as_ref()
        .map_err(|err| anyhow!("failed to initialise llama.cpp backend: {err}"))
}

fn resolve_model_path(
    config: &EmbeddingConfig,
    resolved: &ResolvedEmbeddingModel,
) -> Result<PathBuf> {
    if let Some(path) = config.model_path.as_ref() {
        if let Some(model_path) = resolve_explicit_model_path(path, Some(&resolved.filename)) {
            return Ok(model_path);
        }
        return Err(anyhow!(
            "explicit AICX embedder path does not contain a GGUF model: {}",
            path.display()
        ));
    }

    find_cached_model_file(&resolved.repo, &resolved.filename).ok_or_else(|| {
        anyhow!(
            "AICX GGUF embedder model is not hydrated. Expected {file} in HF cache for {repo}. \
             Run `hf download {repo} {file}` or set AICX_EMBEDDER_PATH to a local .gguf file.",
            repo = resolved.repo,
            file = resolved.filename
        )
    })
}

fn source_for_path(
    config: &EmbeddingConfig,
    resolved: &ResolvedEmbeddingModel,
    model_path: PathBuf,
) -> NativeEmbeddingSource {
    if config.model_path.is_some() {
        NativeEmbeddingSource::ExplicitPath(model_path)
    } else {
        NativeEmbeddingSource::HfCache {
            repo: resolved.repo.clone(),
            filename: resolved.filename.clone(),
            path: model_path,
        }
    }
}

#[allow(dead_code)]
fn is_gguf_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
}
