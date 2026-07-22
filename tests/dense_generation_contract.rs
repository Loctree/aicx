// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
//! W2-03 contract: one dense payload per hybrid generation.
//!
//! A hybrid generation materializes vectors exactly once, into the versioned
//! mmap dense artifact (`aicx.dense.exact_mmap.v1`). The generation directory
//! is written completely before the manifest, and the manifest is written
//! before the `CURRENT` pointer flip — so an interrupted build can never
//! become the current generation, and manifest validation rejects every
//! drift axis between artifacts that claim the same generation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use aicx::vector_index::{
    IndexEntry, IndexHeader, materialize_hybrid_generation, observed_source_hash_for_index_path,
    resolve_hybrid_generation_dir,
};
use aicx_retrieve::{
    Distance, EmbedderFingerprint, MMAP_DENSE_KIND, MMAP_DENSE_PAYLOAD_FILE_NAME, Manifest,
    MmapDenseAdapter, RetrieveError, source_hash_bytes,
};
use chrono::{TimeZone, Utc};

static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn fixture_root(tag: &str) -> PathBuf {
    let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "aicx-dense-generation-{tag}-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create fixture root");
    root
}

fn make_entry(root: &Path, id: &str, embedding: Vec<f32>) -> IndexEntry {
    let chunk_path = root.join("chunks").join(format!("{id}.md"));
    std::fs::create_dir_all(chunk_path.parent().expect("chunk parent")).expect("chunk dir");
    std::fs::write(&chunk_path, format!("# chunk {id}\ncontent for {id}"))
        .expect("write chunk source");
    IndexEntry {
        id: id.to_string(),
        project: "vetcoders/example-app".to_string(),
        agent: "claude".to_string(),
        date: "20260722".to_string(),
        path: chunk_path,
        kind: "conversations".to_string(),
        session_id: format!("session-{id}"),
        frame_kind: Some("agent_reply".to_string()),
        cwd: None,
        embedding,
    }
}

fn write_committed_index(path: &Path, entries: &[IndexEntry], generated_at: &str) {
    let header = IndexHeader {
        schema_version: "1.0".to_string(),
        model_id: "test-model".to_string(),
        model_profile: "base".to_string(),
        dimension: 2,
        generated_at: generated_at.to_string(),
        entry_count: entries.len(),
    };
    let mut body = serde_json::to_string(&header).expect("serialize header");
    body.push('\n');
    for entry in entries {
        body.push_str(&serde_json::to_string(entry).expect("serialize entry"));
        body.push('\n');
    }
    std::fs::create_dir_all(path.parent().expect("index parent")).expect("index dir");
    std::fs::write(path, body).expect("write committed fixture index");
}

fn fingerprint() -> EmbedderFingerprint {
    EmbedderFingerprint::new("test-model", "http://example.invalid/embed", 2, "cosine")
}

/// Every regular file under `dir`, relative-path-sorted, for payload census.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(read_dir) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn fresh_generation_build_creates_exactly_one_dense_payload() {
    let root = fixture_root("fresh");
    let committed = root
        .join("indexed")
        .join("bucket")
        .join("embeddings.ndjson");
    let hybrid_root = root.join("indexed").join("bucket").join("hybrid");
    let entries = [
        make_entry(&root, "a", vec![1.0, 0.0]),
        make_entry(&root, "b", vec![0.0, 1.0]),
        make_entry(&root, "c", vec![0.6, 0.4]),
    ];
    write_committed_index(&committed, &entries, "2026-07-22T07:00:00Z");

    let manifest = materialize_hybrid_generation(&committed, &hybrid_root, &fingerprint())
        .expect("fresh generation build");

    assert_eq!(manifest.dense_kind, MMAP_DENSE_KIND);
    assert_eq!(manifest.dense_count, 3);
    assert_eq!(manifest.lexical_doc_count, 3);
    assert_eq!(manifest.source_chunk_count, 3);

    let generation_dir = resolve_hybrid_generation_dir(&hybrid_root);
    assert_ne!(
        generation_dir, hybrid_root,
        "published build must resolve to a generation directory, not the legacy root"
    );

    let all_files = files_under(&hybrid_root);
    let dense_ndjson_twins: Vec<_> = all_files
        .iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|n| n == "dense_brute_force.ndjson")
        })
        .collect();
    assert!(
        dense_ndjson_twins.is_empty(),
        "fresh build must not write the legacy NDJSON dense twin: {dense_ndjson_twins:?}"
    );
    let mmap_payloads: Vec<_> = all_files
        .iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|n| n == MMAP_DENSE_PAYLOAD_FILE_NAME)
        })
        .collect();
    assert_eq!(
        mmap_payloads.len(),
        1,
        "exactly one dense vector payload per generation: {mmap_payloads:?}"
    );
    assert!(
        mmap_payloads[0].starts_with(&generation_dir),
        "the dense payload lives inside the current generation directory"
    );

    // The manifest is the generation's authority and binds the payload by
    // source hash: opening the payload with the manifest-recorded hash works,
    // any other identity fails closed.
    let persisted =
        Manifest::read_from_path(&generation_dir.join("manifest.json")).expect("read manifest");
    assert_eq!(persisted.generation_id, manifest.generation_id);
    let raw_source_hash =
        observed_source_hash_for_index_path(&committed).expect("hash committed index");
    let expected_bytes = source_hash_bytes(&raw_source_hash);
    assert_eq!(
        persisted.source_hash_blake3,
        hex::encode(expected_bytes),
        "manifest source hash and mmap-embedded source hash share one derivation"
    );
    let dense = MmapDenseAdapter::open(
        &generation_dir.join(MMAP_DENSE_PAYLOAD_FILE_NAME),
        2,
        Distance::Cosine,
        Some(expected_bytes),
    )
    .expect("open dense payload bound to the manifest source hash");
    assert_eq!(aicx_retrieve::DenseIndex::count(&dense), 3);
    drop(dense);
    assert!(
        MmapDenseAdapter::open(
            &generation_dir.join(MMAP_DENSE_PAYLOAD_FILE_NAME),
            2,
            Distance::Cosine,
            Some([0xAB; 32]),
        )
        .is_err(),
        "a different corpus identity must be refused at open"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn interrupted_builds_never_become_current() {
    let root = fixture_root("interrupt");
    let committed = root
        .join("indexed")
        .join("bucket")
        .join("embeddings.ndjson");
    let hybrid_root = root.join("indexed").join("bucket").join("hybrid");
    let entries = [
        make_entry(&root, "a", vec![1.0, 0.0]),
        make_entry(&root, "b", vec![0.0, 1.0]),
    ];
    write_committed_index(&committed, &entries, "2026-07-22T07:00:00Z");

    let first = materialize_hybrid_generation(&committed, &hybrid_root, &fingerprint())
        .expect("first published generation");
    let published_dir = resolve_hybrid_generation_dir(&hybrid_root);
    assert_ne!(published_dir, hybrid_root);

    // Boundary: killed mid-payload-write — generation dir exists, dense tmp
    // present, no manifest yet.
    let partial_payload = hybrid_root.join("generations").join("g-partial-payload");
    std::fs::create_dir_all(&partial_payload).expect("partial generation dir");
    std::fs::write(
        partial_payload.join(format!("{MMAP_DENSE_PAYLOAD_FILE_NAME}.tmp")),
        b"partial bytes",
    )
    .expect("partial dense tmp");
    assert_eq!(
        resolve_hybrid_generation_dir(&hybrid_root),
        published_dir,
        "a payload-stage interruption must not alter current-generation resolution"
    );

    // Boundary: killed after the manifest fsync/rename but before the
    // CURRENT pointer flip — complete generation, still unreferenced.
    let unpublished = hybrid_root
        .join("generations")
        .join("g-complete-unpublished");
    std::fs::create_dir_all(&unpublished).expect("unpublished generation dir");
    std::fs::copy(
        published_dir.join("manifest.json"),
        unpublished.join("manifest.json"),
    )
    .expect("copy manifest into unpublished generation");
    assert_eq!(
        resolve_hybrid_generation_dir(&hybrid_root),
        published_dir,
        "a manifest-complete but unpublished generation must stay unreferenced"
    );

    // Boundary: killed mid-pointer-write — stray pointer tmp never counts.
    std::fs::write(hybrid_root.join("CURRENT.tmp"), "g-complete-unpublished\n")
        .expect("stray pointer tmp");
    assert_eq!(
        resolve_hybrid_generation_dir(&hybrid_root),
        published_dir,
        "an unrenamed pointer tmp must not redirect readers"
    );

    // Corrupt pointer states fail closed to the legacy root, never to an
    // attacker-controlled or missing directory.
    let pointer = hybrid_root.join("CURRENT");
    let healthy_pointer = std::fs::read_to_string(&pointer).expect("read healthy pointer");
    for corrupt in ["", "../../evil", "g-does-not-exist", "a/b"] {
        std::fs::write(&pointer, corrupt).expect("write corrupt pointer");
        assert_eq!(
            resolve_hybrid_generation_dir(&hybrid_root),
            hybrid_root,
            "corrupt pointer {corrupt:?} must fail closed to the legacy root"
        );
    }
    std::fs::write(&pointer, &healthy_pointer).expect("restore pointer");
    assert_eq!(resolve_hybrid_generation_dir(&hybrid_root), published_dir);

    // A completed second build atomically flips the pointer; the previous
    // generation stays on disk (no deletion in this cut).
    let refreshed = materialize_hybrid_generation(&committed, &hybrid_root, &fingerprint())
        .expect("second published generation");
    assert_ne!(refreshed.generation_id, first.generation_id);
    let second_dir = resolve_hybrid_generation_dir(&hybrid_root);
    assert_ne!(second_dir, published_dir);
    assert!(
        published_dir.join(MMAP_DENSE_PAYLOAD_FILE_NAME).exists(),
        "previous generation remains quarantinable on disk"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn embedder_identity_change_with_same_dimension_is_refused() {
    let root = fixture_root("identity");
    let committed = root
        .join("indexed")
        .join("bucket")
        .join("embeddings.ndjson");
    let hybrid_root = root.join("indexed").join("bucket").join("hybrid");
    let entries = [
        make_entry(&root, "a", vec![1.0, 0.0]),
        make_entry(&root, "b", vec![0.0, 1.0]),
    ];
    write_committed_index(&committed, &entries, "2026-07-22T07:00:00Z");

    let manifest = materialize_hybrid_generation(&committed, &hybrid_root, &fingerprint())
        .expect("published generation");

    // Same dimension, different model identity: the generation must refuse
    // reuse instead of silently serving vectors from another embedder.
    let mut observed = manifest.clone();
    observed.embedder_model = "other-model-same-dim".to_string();
    assert_eq!(
        manifest.validate_against(&observed),
        Err(RetrieveError::EmbedderModelDrift {
            manifest_model: "test-model".to_string(),
            query_model: "other-model-same-dim".to_string(),
        })
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn manifest_validation_rejects_every_drift_axis() {
    let started = Utc.with_ymd_and_hms(2026, 7, 22, 7, 0, 0).unwrap();
    let completed = Utc.with_ymd_and_hms(2026, 7, 22, 7, 0, 5).unwrap();
    let base = Manifest {
        schema_version: "2.0".to_string(),
        generation_id: "g-2026-07-22T07:00:00Z-deadbeef".to_string(),
        source_chunk_count: 3,
        source_hash_blake3: "blake3-source".to_string(),
        embedder_model: "test-model".to_string(),
        embedder_url_hash: "sha256-endpoint".to_string(),
        embedder_dim: 2,
        embedder_distance: "cosine".to_string(),
        dense_count: 3,
        dense_kind: MMAP_DENSE_KIND.to_string(),
        lexical_commit_id: "tantivy_lexical_v2_fast_body:seg-1".to_string(),
        lexical_doc_count: 3,
        build_started_at: started,
        build_completed_at: completed,
        build_wall_seconds: 5,
        fusion_algorithm: "rrf".to_string(),
        fusion_k: 60,
    };

    // source drift
    let mut other = base.clone();
    other.source_hash_blake3 = "blake3-other".to_string();
    assert!(matches!(
        base.validate_against(&other),
        Err(RetrieveError::SourceHashDrift { .. })
    ));

    // model drift
    let mut other = base.clone();
    other.embedder_model = "other-model".to_string();
    assert!(matches!(
        base.validate_against(&other),
        Err(RetrieveError::EmbedderModelDrift { .. })
    ));

    // dimension drift
    let mut other = base.clone();
    other.embedder_dim = 4;
    assert_eq!(
        base.validate_against(&other),
        Err(RetrieveError::DimMismatch {
            expected: 2,
            actual: 4,
        })
    );

    // distance drift (same model, same dimension)
    let mut other = base.clone();
    other.embedder_distance = "dot".to_string();
    assert!(
        base.validate_against(&other).is_err(),
        "distance drift must be rejected"
    );

    // lexical generation drift
    let mut other = base.clone();
    other.lexical_commit_id = "tantivy_lexical_v2_fast_body:seg-2".to_string();
    assert!(matches!(
        base.validate_against(&other),
        Err(RetrieveError::LexicalCommitMismatch { .. })
    ));

    // partial-build drift: dense payload row count diverges from the claim
    let mut other = base.clone();
    other.dense_count = 2;
    assert_eq!(
        base.validate_against(&other),
        Err(RetrieveError::DenseCountMismatch {
            expected: 3,
            actual: 2,
        })
    );

    // partial-build drift: lexical doc count diverges from the claim
    let mut other = base.clone();
    other.lexical_doc_count = 2;
    assert_eq!(
        base.validate_against(&other),
        Err(RetrieveError::LexicalDocCountMismatch {
            expected: 3,
            actual: 2,
        })
    );

    // partial-build drift: dense payload kind diverges (legacy twin vs mmap)
    let mut other = base.clone();
    other.dense_kind = "brute_force_ndjson".to_string();
    assert!(matches!(
        base.validate_against(&other),
        Err(RetrieveError::GenerationMismatch { .. })
    ));
}

#[test]
fn legacy_layout_without_pointer_resolves_to_root_for_migration_reads() {
    let root = fixture_root("legacy");
    let hybrid_root = root.join("indexed").join("bucket").join("hybrid");
    std::fs::create_dir_all(&hybrid_root).expect("legacy hybrid root");
    std::fs::write(hybrid_root.join("manifest.json"), "{}").expect("legacy manifest");
    std::fs::write(hybrid_root.join("dense_brute_force.ndjson"), "")
        .expect("legacy dense twin stays readable as migration input");

    assert_eq!(
        resolve_hybrid_generation_dir(&hybrid_root),
        hybrid_root,
        "legacy dual-file layout resolves to the root so migration reads keep working"
    );

    let _ = std::fs::remove_dir_all(&root);
}
