// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
use aicx_retrieve::{Manifest, RetrieveError};
use chrono::{TimeZone, Utc};

fn manifest() -> Manifest {
    let started = Utc.with_ymd_and_hms(2026, 5, 15, 1, 2, 3).unwrap();
    let completed = Utc.with_ymd_and_hms(2026, 5, 15, 1, 2, 8).unwrap();

    Manifest {
        schema_version: "2.0".to_string(),
        generation_id: "g-2026-05-15T01:02:03Z-deadbeef".to_string(),
        writer_version: "0.12.1".to_string(),
        build_id: "0.12.1+gtestbead".to_string(),
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
        Err(RetrieveError::LexicalCommitMismatch {
            expected: "lex-commit-1".to_string(),
            actual: "lex-commit-2".to_string(),
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

#[test]
fn lexical_schema_generation_parses_versioned_tags() {
    use aicx_retrieve::lexical_schema_generation;
    assert_eq!(
        lexical_schema_generation("tantivy_lexical_v3_folded_dictation"),
        Some(3)
    );
    assert_eq!(
        lexical_schema_generation("tantivy_lexical_v2_fast_body:de262a7d"),
        Some(2)
    );
    assert_eq!(lexical_schema_generation("tantivy_lexical_v12_x"), Some(12));
    assert_eq!(lexical_schema_generation("lex-commit-1"), None);
    assert_eq!(lexical_schema_generation("tantivy_lexical_vNaN"), None);
    assert_eq!(lexical_schema_generation(""), None);
}

#[test]
fn pre_provenance_manifest_json_parses_with_empty_writer_identity() {
    // Manifests written before 2026-08-10 carry no writer fields; they must
    // stay readable and self-describe as an unknown writer.
    let json = serde_json::to_value(manifest()).unwrap();
    let mut stripped = json.as_object().unwrap().clone();
    stripped.remove("writer_version");
    stripped.remove("build_id");
    let parsed: Manifest = serde_json::from_value(serde_json::Value::Object(stripped)).unwrap();
    assert_eq!(parsed.writer_version, "");
    assert_eq!(parsed.build_id, "");
    assert_eq!(parsed.writer_label(), "unknown (pre-provenance writer)");
}

#[test]
fn writer_label_prefers_full_identity() {
    let m = manifest();
    assert_eq!(m.writer_label(), "0.12.1 (0.12.1+gtestbead)");
}

#[test]
fn ensure_lexical_schema_supported_rejects_downgrade_with_writer_identity() {
    let mut m = manifest();
    m.lexical_commit_id = "tantivy_lexical_v2_fast_body:seg".to_string();
    let err = m
        .ensure_lexical_schema_supported("tantivy_lexical_v3_folded_dictation")
        .unwrap_err();
    assert_eq!(
        err,
        RetrieveError::LexicalSchemaDowngrade {
            current: "tantivy_lexical_v3_folded_dictation".to_string(),
            incoming: "tantivy_lexical_v2_fast_body:seg".to_string(),
            writer: "0.12.1 (0.12.1+gtestbead)".to_string(),
        }
    );
}

#[test]
fn ensure_lexical_schema_supported_rejects_future_schema() {
    let mut m = manifest();
    m.lexical_commit_id = "tantivy_lexical_v4_next:seg".to_string();
    let err = m
        .ensure_lexical_schema_supported("tantivy_lexical_v3_folded_dictation")
        .unwrap_err();
    assert!(matches!(err, RetrieveError::LexicalSchemaTooNew { .. }));
}

#[test]
fn ensure_lexical_schema_supported_passes_matching_and_unversioned_ids() {
    let mut m = manifest();
    m.lexical_commit_id = "tantivy_lexical_v3_folded_dictation:seg".to_string();
    assert_eq!(
        m.ensure_lexical_schema_supported("tantivy_lexical_v3_folded_dictation"),
        Ok(())
    );
    // Legacy/synthetic ids carry no comparable generation — the gate makes
    // no claim and defers to the adapter's own fail-closed field checks.
    m.lexical_commit_id = "lex-commit-1".to_string();
    assert_eq!(
        m.ensure_lexical_schema_supported("tantivy_lexical_v3_folded_dictation"),
        Ok(())
    );
}
