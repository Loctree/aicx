//! Native embedder foundation for AICX and the Vibecrafted framework.
//!
//! This module re-exports the reusable `aicx-embeddings` crate so existing
//! consumers can keep using `aicx::embedder::*` while rust-memex can depend on
//! the provider crate directly.
//!
//! The first-choice backend is local GGUF/F2LLM through llama.cpp. Models are
//! resolved from an explicit path or the local HuggingFace cache; release
//! bundles stay slim and do not silently carry model payloads.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

#![cfg(feature = "native-embedder")]

pub use aicx_embeddings::{
    BackendPreference, EmbeddingConfig, EmbeddingEngine, EmbeddingModelInfo, EmbeddingProfile,
    EmbeddingProfileSpec, LocalEmbeddingProvider, NativeEmbeddingSource, ResolvedEmbeddingModel,
    config_search_paths, find_cached_model_file, l2_normalize, profile_spec, similarity,
};

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
