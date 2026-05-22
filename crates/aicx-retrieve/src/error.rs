// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
use thiserror::Error;

/// Errors that protect retrieval manifests and query-time compatibility.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RetrieveError {
    #[error("dimension mismatch: expected {expected}, got {actual}")]
    DimMismatch { expected: usize, actual: usize },

    #[error("generation mismatch: lexical={lexical_gen}, dense={dense_gen}")]
    GenerationMismatch {
        lexical_gen: String,
        dense_gen: String,
    },

    #[error("dense count mismatch: expected {expected}, got {actual}")]
    DenseCountMismatch { expected: usize, actual: usize },

    #[error("lexical document count mismatch: expected {expected}, got {actual}")]
    LexicalDocCountMismatch { expected: usize, actual: usize },

    #[error("lexical commit mismatch: expected {expected}, got {actual}")]
    LexicalCommitMismatch { expected: String, actual: String },

    #[error("embedder model drift: manifest={manifest_model}, query={query_model}")]
    EmbedderModelDrift {
        manifest_model: String,
        query_model: String,
    },

    #[error("schema version unsupported: {0}")]
    SchemaVersionUnsupported(String),

    #[error("source hash drift: manifest={manifest_hash}, observed={observed_hash}")]
    SourceHashDrift {
        manifest_hash: String,
        observed_hash: String,
    },
}

/// Manifest validation uses the same typed error family as retrieval.
pub type ManifestError = RetrieveError;
