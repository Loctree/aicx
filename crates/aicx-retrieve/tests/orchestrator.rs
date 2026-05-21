// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use aicx_retrieve::{
    BruteForceAdapter, ChunkRef, DenseChunkRef, DenseIndex, EmbedderFingerprint, FilterSet,
    HybridIndex, HybridQueryInput, Manifest, ReciprocalRankFusion, RetrieveError, TantivyAdapter,
    default_ndjson_path,
};
use serde_json::json;
use tempfile::TempDir;

const SOURCE_HASH: &str = "synthetic-source-v1";

fn chunk(id: usize) -> ChunkRef {
    let agent = if id.is_multiple_of(2) {
        "claude"
    } else {
        "codex"
    };
    ChunkRef {
        id: format!("chunk-{id:03}"),
        source_path: format!("/tmp/source-{id:03}.md"),
        text: format!("needle hybrid retrieval body {id}"),
        metadata: json!({
            "agent": agent,
            "date": "20260515",
            "project": "aicx",
        }),
    }
}

fn dense_chunk(chunk: ChunkRef, dim: usize) -> DenseChunkRef {
    let id_num = chunk
        .id
        .strip_prefix("chunk-")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let mut embedding = vec![0.0; dim];
    embedding[id_num % dim] = 1.0;
    DenseChunkRef { chunk, embedding }
}

fn corpus(count: usize, dim: usize) -> (Vec<ChunkRef>, Vec<DenseChunkRef>) {
    let chunks: Vec<_> = (0..count).map(chunk).collect();
    let dense_chunks = chunks
        .iter()
        .cloned()
        .map(|chunk| dense_chunk(chunk, dim))
        .collect();
    (chunks, dense_chunks)
}

fn fingerprint(dim: usize) -> EmbedderFingerprint {
    EmbedderFingerprint::new("test-embedder", "https://embed.example/v1", dim, "cosine")
}

fn query_embedding(dim: usize) -> Vec<f32> {
    let mut embedding = vec![0.0; dim];
    embedding[0] = 1.0;
    embedding
}

fn boxed_lexical(dir: &Path) -> Box<TantivyAdapter> {
    Box::new(TantivyAdapter::new(dir.to_path_buf()).unwrap())
}

fn boxed_dense(dim: usize) -> Box<BruteForceAdapter> {
    Box::new(BruteForceAdapter::new(dim))
}

fn build_committed_index(
    count: usize,
    dim: usize,
) -> (TempDir, Vec<ChunkRef>, Vec<DenseChunkRef>, Vec<f32>) {
    let temp = TempDir::new().unwrap();
    let (chunks, dense_chunks) = corpus(count, dim);
    let query = query_embedding(dim);
    let mut index = HybridIndex::new(
        boxed_lexical(temp.path()),
        boxed_dense(dim),
        Box::new(ReciprocalRankFusion::default()),
        temp.path(),
        fingerprint(dim),
    );
    index
        .build_hybrid(&chunks, &dense_chunks, SOURCE_HASH)
        .expect("build hybrid");
    index.commit().expect("commit manifest");
    persist_dense(temp.path(), dim, &dense_chunks);
    (temp, chunks, dense_chunks, query)
}

fn persist_dense(dir: &Path, dim: usize, chunks: &[DenseChunkRef]) {
    let mut dense = BruteForceAdapter::new(dim);
    dense.build(chunks).unwrap();
    dense.persist_ndjson(&default_ndjson_path(dir)).unwrap();
}

fn loaded_dense(dir: &Path, dim: usize) -> Box<BruteForceAdapter> {
    let mut dense = BruteForceAdapter::new(dim);
    dense.load_ndjson(&default_ndjson_path(dir)).unwrap();
    Box::new(dense)
}

fn load_index(dir: &Path, dim: usize, source_hash: &str) -> anyhow::Result<HybridIndex> {
    HybridIndex::load_from_manifest(
        boxed_lexical(dir),
        loaded_dense(dir, dim),
        Box::new(ReciprocalRankFusion::default()),
        dir,
        fingerprint(dim),
        source_hash,
    )
}

fn assert_retrieve_error(err: anyhow::Error, expected: RetrieveError) {
    assert_eq!(err.downcast_ref::<RetrieveError>(), Some(&expected));
}

fn expect_load_err(result: anyhow::Result<HybridIndex>) -> anyhow::Error {
    match result {
        Ok(_) => panic!("expected load_from_manifest to fail"),
        Err(err) => err,
    }
}

#[test]
fn build_commit_load_query_round_trip() {
    let dim = 8;
    let temp = TempDir::new().unwrap();
    let (chunks, dense_chunks) = corpus(50, dim);
    let query = query_embedding(dim);
    let mut index = HybridIndex::new(
        boxed_lexical(temp.path()),
        boxed_dense(dim),
        Box::new(ReciprocalRankFusion::default()),
        temp.path(),
        fingerprint(dim),
    );
    index
        .build_hybrid(&chunks, &dense_chunks, SOURCE_HASH)
        .expect("build hybrid");
    let before = index
        .query_hybrid(HybridQueryInput {
            query_text: "needle",
            query_embedding: &query,
            filters: FilterSet::default(),
            limit: 10,
        })
        .expect("pre-commit query");
    index.commit().expect("commit");
    persist_dense(temp.path(), dim, &dense_chunks);

    let loaded = load_index(temp.path(), dim, SOURCE_HASH).expect("load");
    let after = loaded
        .query_hybrid(HybridQueryInput {
            query_text: "needle",
            query_embedding: &query,
            filters: FilterSet::default(),
            limit: 10,
        })
        .expect("post-load query");

    assert_eq!(
        before.iter().map(|hit| &hit.chunk_id).collect::<Vec<_>>(),
        after.iter().map(|hit| &hit.chunk_id).collect::<Vec<_>>()
    );
    assert!(loaded.generation_id().is_some());
}

#[test]
fn manifest_dim_mismatch_fails_fast() {
    let dim = 4;
    let (temp, _, _, _) = build_committed_index(12, dim);
    let err = expect_load_err(HybridIndex::load_from_manifest(
        boxed_lexical(temp.path()),
        loaded_dense(temp.path(), dim),
        Box::new(ReciprocalRankFusion::default()),
        temp.path(),
        fingerprint(dim + 1),
        SOURCE_HASH,
    ));
    assert_retrieve_error(
        err,
        RetrieveError::DimMismatch {
            expected: dim,
            actual: dim + 1,
        },
    );
}

#[test]
fn manifest_embedder_drift_fails_fast() {
    let dim = 4;
    let (temp, _, _, _) = build_committed_index(12, dim);
    let err = expect_load_err(HybridIndex::load_from_manifest(
        boxed_lexical(temp.path()),
        loaded_dense(temp.path(), dim),
        Box::new(ReciprocalRankFusion::default()),
        temp.path(),
        EmbedderFingerprint::new("other-embedder", "https://embed.example/v1", dim, "cosine"),
        SOURCE_HASH,
    ));
    assert_retrieve_error(
        err,
        RetrieveError::EmbedderModelDrift {
            manifest_model: "test-embedder".to_string(),
            query_model: "other-embedder".to_string(),
        },
    );
}

#[test]
fn manifest_source_hash_drift_fails_fast() {
    let dim = 4;
    let (temp, _, _, _) = build_committed_index(12, dim);
    let err = expect_load_err(load_index(temp.path(), dim, "synthetic-source-v2"));
    assert!(matches!(
        err.downcast_ref::<RetrieveError>(),
        Some(RetrieveError::SourceHashDrift { .. })
    ));
}

#[test]
fn manifest_lexical_commit_mismatch_fails_fast() {
    let dim = 4;
    let (temp, _, _, _) = build_committed_index(12, dim);
    let manifest_path = temp.path().join("manifest.json");
    let mut manifest = Manifest::read_from_path(&manifest_path).unwrap();
    let original = manifest.lexical_commit_id.clone();
    manifest.lexical_commit_id = "edited-lexical-commit".to_string();
    manifest.write_to_path(&manifest_path).unwrap();

    let err = expect_load_err(load_index(temp.path(), dim, SOURCE_HASH));
    assert_retrieve_error(
        err,
        RetrieveError::GenerationMismatch {
            lexical_gen: "edited-lexical-commit".to_string(),
            dense_gen: original,
        },
    );
}

#[test]
fn filter_pre_pass_works_through_orchestrator() {
    let dim = 4;
    let (temp, _, _, query) = build_committed_index(20, dim);
    let loaded = load_index(temp.path(), dim, SOURCE_HASH).expect("load");
    let mut values = BTreeMap::new();
    values.insert("agent".to_string(), json!("claude"));

    let hits = loaded
        .query_hybrid(HybridQueryInput {
            query_text: "needle",
            query_embedding: &query,
            filters: FilterSet { values },
            limit: 10,
        })
        .expect("filtered query");

    assert!(!hits.is_empty());
    assert!(
        hits.iter()
            .all(|hit| hit.metadata.get("agent") == Some(&json!("claude")))
    );

    let _ = fs::remove_dir_all(temp.path());
}
