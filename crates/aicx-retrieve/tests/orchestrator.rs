// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
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
        Some(source_hash),
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
fn commit_before_build_hybrid_returns_error() {
    let dim = 4;
    let temp = TempDir::new().unwrap();
    let mut index = HybridIndex::new(
        boxed_lexical(temp.path()),
        boxed_dense(dim),
        Box::new(ReciprocalRankFusion::default()),
        temp.path(),
        fingerprint(dim),
    );

    let err = index
        .commit()
        .expect_err("commit without manifest should fail");
    assert!(
        err.to_string()
            .contains("cannot commit hybrid index before build_hybrid")
    );
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
        Some(SOURCE_HASH),
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
fn manifest_dense_count_mismatch_fails_fast() {
    let dim = 4;
    let (temp, _, _, _) = build_committed_index(12, dim);
    let manifest_path = temp.path().join("manifest.json");
    let mut manifest = Manifest::read_from_path(&manifest_path).unwrap();
    manifest.dense_count = 99;
    manifest.write_to_path(&manifest_path).unwrap();

    let err = expect_load_err(load_index(temp.path(), dim, SOURCE_HASH));
    assert_retrieve_error(
        err,
        RetrieveError::DenseCountMismatch {
            expected: 99,
            actual: 12,
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
        Some(SOURCE_HASH),
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
fn qwen3_8b_provider_migration_queries_existing_vectors_without_rewriting_them() {
    let dim = 4096;
    let temp = TempDir::new().unwrap();
    let (chunks, dense_chunks) = corpus(3, dim);
    let old_fingerprint =
        EmbedderFingerprint::new("qwen3-embedding:8b", "http://old-host:11434", dim, "cosine");
    let mut index = HybridIndex::new(
        boxed_lexical(temp.path()),
        boxed_dense(dim),
        Box::new(ReciprocalRankFusion::default()),
        temp.path(),
        old_fingerprint,
    );
    index
        .build_hybrid(&chunks, &dense_chunks, SOURCE_HASH)
        .unwrap();
    index.commit().unwrap();
    persist_dense(temp.path(), dim, &dense_chunks);
    drop(index);
    let manifest_path = temp.path().join("manifest.json");
    let vectors_path = default_ndjson_path(temp.path());
    let manifest_before = fs::read(&manifest_path).unwrap();
    let vectors_before = fs::read(&vectors_path).unwrap();
    let new_fingerprint = EmbedderFingerprint::new(
        "mlx-community/Qwen3-Embedding-8B-4bit-DWQ",
        "http://new-host:12345/v1/embeddings",
        dim,
        "cosine",
    );
    for model in [
        "mlx-community/Qwen3-Embedding-8B-4bit-DWQ",
        "/opt/models/Qwen3-Embedding-8B-Q4_K_M.gguf",
        "Qwen/Qwen3-Embedding-8B-GGUF/Qwen3-Embedding-8B.Q8_0.gguf",
        r"C:\models\Qwen3-Embedding-8B.BF16.gguf",
    ] {
        let loaded = HybridIndex::load_from_manifest(
            boxed_lexical(temp.path()),
            loaded_dense(temp.path(), dim),
            Box::new(ReciprocalRankFusion::default()),
            temp.path(),
            EmbedderFingerprint::new(model, "different-deployment", dim, "cosine"),
            Some(SOURCE_HASH),
        )
        .expect("the same Qwen3 8B embedding space must survive provider migration");
        let hits = loaded
            .query_hybrid(HybridQueryInput {
                query_text: "needle",
                query_embedding: &query_embedding(dim),
                filters: FilterSet::default(),
                limit: 3,
            })
            .unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].chunk_id, "chunk-000");
    }
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(fs::read(&vectors_path).unwrap(), vectors_before);

    // Query compatibility does not authorize mixing new vectors into a
    // generation written with a different model/input contract.
    let manifest = Manifest::read_from_path(&manifest_path).unwrap();
    let error = aicx_retrieve::validate_live_bindings_for_refresh(
        &manifest,
        boxed_lexical(temp.path()).as_ref(),
        loaded_dense(temp.path(), dim).as_ref(),
        &ReciprocalRankFusion::default(),
        &new_fingerprint,
    )
    .expect_err("write admission must keep its exact fingerprint contract");
    assert!(matches!(error, RetrieveError::EmbedderModelDrift { .. }));

    for model in [
        "Qwen/Qwen3-Embedding-4B",
        "custom/Qwen3-Embedding-8B-finetuned",
    ] {
        let error = expect_load_err(HybridIndex::load_from_manifest(
            boxed_lexical(temp.path()),
            loaded_dense(temp.path(), dim),
            Box::new(ReciprocalRankFusion::default()),
            temp.path(),
            EmbedderFingerprint::new(model, "http://new-host:12345", dim, "cosine"),
            Some(SOURCE_HASH),
        ));
        assert!(matches!(
            error.downcast_ref::<RetrieveError>(),
            Some(RetrieveError::EmbedderModelDrift { .. })
        ));
    }
}

#[test]
fn embedding_space_is_portable_symmetric_and_keeps_model_dimension_and_distance_separate() {
    let deployments = [
        "qwen3-embedding:8b",
        "qwen3-embedding:8b-q4_k_m",
        "mlx-community/Qwen3-Embedding-8B-4bit-DWQ",
        "/opt/models/Qwen3-Embedding-8B.Q8_0.gguf",
    ];
    for a in deployments {
        for b in deployments {
            let a = EmbedderFingerprint::new(a, "host-a", 4096, "cosine");
            let b = EmbedderFingerprint::new(b, "host-b", 4096, "cosine");
            assert!(a.shares_embedding_space(&b));
            assert!(b.shares_embedding_space(&a));
        }
    }
    let base = EmbedderFingerprint::new(deployments[0], "host-a", 4096, "cosine");
    for model in [
        "qwen3-embedding:0.6b",
        "Qwen3-Embedding-8B-finetuned-Q4_K_M.gguf",
        "Qwen3-Embedding-8B-plus",
        "other-4096-model",
    ] {
        assert!(
            !base.shares_embedding_space(&EmbedderFingerprint::new(
                model, "host-b", 4096, "cosine",
            ))
        );
    }
    for (dim, distance) in [(2048, "cosine"), (4096, "dot")] {
        assert!(!base.shares_embedding_space(&EmbedderFingerprint::new(
            deployments[1],
            "host-b",
            dim,
            distance,
        )));
    }
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
        RetrieveError::LexicalCommitMismatch {
            expected: "edited-lexical-commit".to_string(),
            actual: original,
        },
    );
}

#[test]
fn manifest_lexical_doc_count_mismatch_fails_fast() {
    let dim = 4;
    let (temp, _, _, _) = build_committed_index(12, dim);
    let manifest_path = temp.path().join("manifest.json");
    let mut manifest = Manifest::read_from_path(&manifest_path).unwrap();
    manifest.lexical_doc_count = 99;
    manifest.write_to_path(&manifest_path).unwrap();

    let err = expect_load_err(load_index(temp.path(), dim, SOURCE_HASH));
    assert_retrieve_error(
        err,
        RetrieveError::LexicalDocCountMismatch {
            expected: 99,
            actual: 12,
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
