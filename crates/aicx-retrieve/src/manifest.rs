// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ManifestError;

/// Retrieval build manifest for split lexical + dense index artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: String,
    pub generation_id: String,
    pub source_chunk_count: usize,
    pub source_hash_blake3: String,
    pub embedder_model: String,
    pub embedder_url_hash: String,
    pub embedder_dim: usize,
    pub embedder_distance: String,
    pub dense_count: usize,
    pub dense_kind: String,
    pub lexical_commit_id: String,
    pub lexical_doc_count: usize,
    pub build_started_at: DateTime<Utc>,
    pub build_completed_at: DateTime<Utc>,
    pub build_wall_seconds: u64,
    pub fusion_algorithm: String,
    pub fusion_k: u32,
}

impl Manifest {
    /// Validate that two retrieval artifacts belong to the same generation.
    pub fn validate_against(&self, other: &Manifest) -> Result<(), ManifestError> {
        const SUPPORTED_SCHEMA_VERSION: &str = "2.0";

        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(ManifestError::SchemaVersionUnsupported(
                self.schema_version.clone(),
            ));
        }

        if other.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(ManifestError::SchemaVersionUnsupported(
                other.schema_version.clone(),
            ));
        }

        if self.embedder_dim != other.embedder_dim {
            return Err(ManifestError::DimMismatch {
                expected: self.embedder_dim,
                actual: other.embedder_dim,
            });
        }

        if self.embedder_model != other.embedder_model {
            return Err(ManifestError::EmbedderModelDrift {
                manifest_model: self.embedder_model.clone(),
                query_model: other.embedder_model.clone(),
            });
        }

        if self.source_hash_blake3 != other.source_hash_blake3 {
            return Err(ManifestError::SourceHashDrift {
                manifest_hash: self.source_hash_blake3.clone(),
                observed_hash: other.source_hash_blake3.clone(),
            });
        }

        if self.lexical_commit_id != other.lexical_commit_id {
            return Err(ManifestError::GenerationMismatch {
                lexical_gen: self.lexical_commit_id.clone(),
                dense_gen: other.lexical_commit_id.clone(),
            });
        }

        Ok(())
    }
}
