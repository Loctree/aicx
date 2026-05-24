//! Embedder foundation for AICX and the Vibecrafted framework.
//!
//! Re-exports the reusable [`aicx_embeddings`] crate so existing consumers
//! keep using `aicx::embedder::*` while `rust-memex` and other workspaces
//! depend on the provider crate directly.
//!
//! Two production backends compile in by default:
//! - **Cloud** (`cloud-embedder` feature) — HTTP POST against an
//!   OpenAI-compatible `/v1/embeddings`. Recommended VetCoders production
//!   default: zero-install, config-driven URL/model/api_key_env.
//! - **Native GGUF** (`native-embedder` feature) — local llama.cpp
//!   inference over an F2LLM/GGUF model resolved from `AICX_EMBEDDER_PATH`
//!   or the local HuggingFace cache. Release bundles stay slim and do not
//!   silently carry model payloads.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

#![cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]

pub use aicx_embeddings::{
    BackendPreference, CloudEmbeddingConfig, ConfigSource, EmbeddingConfig, EmbeddingEngine,
    EmbeddingModelInfo, EmbeddingProfile, EmbeddingProfileSpec, LocalEmbeddingProvider,
    NativeEmbeddingSource, ResolvedEmbeddingModel, config_search_paths,
    config_search_paths_with_source, effective_config_source, find_cached_model_file, l2_normalize,
    profile_spec, similarity,
};

#[cfg(feature = "cloud-embedder")]
pub use aicx_embeddings::CloudEmbeddingProvider;

#[cfg(feature = "cloud-embedder")]
pub use aicx_embeddings::cloud;

pub type EmbedderConfig = EmbeddingConfig;
pub type EmbedderEngine = EmbeddingEngine;

/// Build-time include_bytes embedding is intentionally off for the production
/// GGUF path. Keep this compatibility shim for old diagnostics/tests.
pub fn is_embedded_available() -> bool {
    false
}

/// GGUF release builds do not expose a static embedded dimension hint because
/// the model is hydrated at install/runtime.
pub fn embedded_dimension() -> Option<usize> {
    None
}
