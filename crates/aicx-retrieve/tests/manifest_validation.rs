// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
use aicx_retrieve::{Manifest, RetrieveError};
use chrono::{TimeZone, Utc};

fn manifest() -> Manifest {
    let started = Utc.with_ymd_and_hms(2026, 5, 15, 1, 2, 3).unwrap();
    let completed = Utc.with_ymd_and_hms(2026, 5, 15, 1, 2, 8).unwrap();

    Manifest {
        schema_version: "2.0".to_string(),
        generation_id: "g-2026-05-15T01:02:03Z-deadbeef".to_string(),
        source_chunk_count: 3,
        source_hash_blake3: "blake3-source".to_string(),
        embedder_model: "text-embedding-3-small".to_string(),
        embedder_url_hash: "sha256-endpoint".to_string(),
        embedder_dim: 1536,
        embedder_distance: "cosine".to_string(),
        dense_count: 3,
        dense_kind: "brute_force_ndjson".to_string(),
        lexical_commit_id: "lex-commit-1".to_string(),
        lexical_doc_count: 3,
        build_started_at: started,
        build_completed_at: completed,
        build_wall_seconds: 5,
        fusion_algorithm: "rrf".to_string(),
        fusion_k: 60,
    }
}

#[test]
fn matching_manifests_validate() {
    let left = manifest();
    let right = manifest();

    assert_eq!(left.validate_against(&right), Ok(()));
}

#[test]
fn dim_mismatch_is_typed() {
    let left = manifest();
    let mut right = manifest();
    right.embedder_dim = 768;

    assert_eq!(
        left.validate_against(&right),
        Err(RetrieveError::DimMismatch {
            expected: 1536,
            actual: 768,
        })
    );
}

#[test]
fn embedder_model_drift_is_typed() {
    let left = manifest();
    let mut right = manifest();
    right.embedder_model = "other-model".to_string();

    assert_eq!(
        left.validate_against(&right),
        Err(RetrieveError::EmbedderModelDrift {
            manifest_model: "text-embedding-3-small".to_string(),
            query_model: "other-model".to_string(),
        })
    );
}

#[test]
fn source_hash_drift_is_typed() {
    let left = manifest();
    let mut right = manifest();
    right.source_hash_blake3 = "different-source".to_string();

    assert_eq!(
        left.validate_against(&right),
        Err(RetrieveError::SourceHashDrift {
            manifest_hash: "blake3-source".to_string(),
            observed_hash: "different-source".to_string(),
        })
    );
}

#[test]
fn lexical_commit_mismatch_is_typed() {
    let left = manifest();
    let mut right = manifest();
    right.lexical_commit_id = "lex-commit-2".to_string();

    assert_eq!(
        left.validate_against(&right),
        Err(RetrieveError::GenerationMismatch {
            lexical_gen: "lex-commit-1".to_string(),
            dense_gen: "lex-commit-2".to_string(),
        })
    );
}

#[test]
fn forward_schema_version_is_rejected() {
    let left = manifest();
    let mut right = manifest();
    right.schema_version = "3.0".to_string();

    assert_eq!(
        left.validate_against(&right),
        Err(RetrieveError::SchemaVersionUnsupported("3.0".to_string()))
    );
}
