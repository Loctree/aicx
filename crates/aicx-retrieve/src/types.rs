// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Opaque lexical commit identifier produced by a lexical index.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LexicalCommitId(pub String);

/// Minimal source chunk reference consumed by lexical indexes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub id: String,
    pub source_path: String,
    pub text: String,
    pub metadata: serde_json::Value,
}

/// Source chunk plus embedding consumed by dense indexes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DenseChunkRef {
    pub chunk: ChunkRef,
    pub embedding: Vec<f32>,
}

/// Unified retrieval hit emitted by lexical and dense adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub chunk_id: String,
    pub score: f32,
    pub rank: usize,
    pub source: String,
    pub metadata: serde_json::Value,
}

/// Lexical query request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexicalQuery {
    pub text: String,
    pub limit: usize,
    pub filters: FilterSet,
}

/// Structured filters shared across lexical and dense retrieval paths.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FilterSet {
    pub values: BTreeMap<String, serde_json::Value>,
}

/// Dense-vector distance semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Distance {
    Cosine,
    Euclidean,
    Dot,
}
