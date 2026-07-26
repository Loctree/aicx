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
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use aicx::vector_index::{
    BatchEmbedder, DenseBuildOptions, DenseBuildProgress, IndexEntry, IndexHeader,
    build_source_hybrid_generation_resumable, materialize_hybrid_generation,
    observed_source_hash_for_index_path, publish_source_hybrid_generation,
    resolve_current_generation_dir, resolve_hybrid_generation_dir, source_dense_checkpoint_path,
};
use aicx_retrieve::{
    ChunkRef, Distance, EmbedderFingerprint, FilterSet, LexicalQuery, MMAP_DENSE_KIND,
    MMAP_DENSE_PAYLOAD_FILE_NAME, Manifest, MmapDenseAdapter, RetrieveError, TantivyAdapter,
    source_hash_bytes,
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

fn source_chunks() -> Vec<ChunkRef> {
    ["a", "b", "c"]
        .into_iter()
        .map(|id| ChunkRef {
            id: id.to_string(),
            source_path: format!("/catalog/{id}.jsonl"),
            text: format!("source-driven content for {id}"),
            metadata: serde_json::json!({
                "project": "vetcoders/example-app",
                "agent": "codex",
                "session_id": format!("session-{id}"),
            }),
        })
        .collect()
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

#[derive(Default)]
struct CountingEmbedder {
    dimension: usize,
    fail_after_successful_batches: Option<usize>,
    successful_batches: usize,
    batch_sizes: Vec<usize>,
    embedded_texts: Vec<String>,
}

impl CountingEmbedder {
    fn new(dimension: usize) -> Self {
        Self {
            dimension,
            ..Self::default()
        }
    }

    fn interrupt_after(dimension: usize, successful_batches: usize) -> Self {
        Self {
            dimension,
            fail_after_successful_batches: Some(successful_batches),
            ..Self::default()
        }
    }
}

impl BatchEmbedder for CountingEmbedder {
    fn embed_batch(&mut self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if self
            .fail_after_successful_batches
            .is_some_and(|limit| self.successful_batches >= limit)
        {
            anyhow::bail!("injected dense batch interruption");
        }
        self.successful_batches += 1;
        self.batch_sizes.push(texts.len());
        self.embedded_texts.extend(texts.iter().cloned());
        Ok(texts
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let mut vector = vec![0.0; self.dimension];
                vector[index % self.dimension] = 1.0;
                vector
            })
            .collect())
    }

    fn embed_one(&mut self, text: &str) -> anyhow::Result<Vec<f32>> {
        if self
            .fail_after_successful_batches
            .is_some_and(|limit| self.successful_batches >= limit)
        {
            anyhow::bail!("injected dense item interruption");
        }
        self.successful_batches += 1;
        self.batch_sizes.push(1);
        self.embedded_texts.push(text.to_string());
        let mut vector = vec![0.0; self.dimension];
        vector[0] = 1.0;
        Ok(vector)
    }
}

#[test]
fn resume_reuses_completed_vectors_and_keeps_current_readable() {
    let root = fixture_root("resume");
    let hybrid_root = root.join("indexed").join("_all").join("hybrid");
    let chunks = source_chunks();
    let source_fingerprint = "catalog-v1:resume";
    let options = DenseBuildOptions { batch_size: 2 };

    let initial_embeddings = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.6, 0.4]];
    publish_source_hybrid_generation(
        &chunks,
        &initial_embeddings,
        "catalog-v1:prior-current",
        &hybrid_root,
        &fingerprint(),
    )
    .expect("publish prior current generation");
    let prior_current =
        std::fs::read_to_string(hybrid_root.join("CURRENT")).expect("read prior CURRENT");

    let mut interrupted = CountingEmbedder::interrupt_after(2, 1);
    build_source_hybrid_generation_resumable(
        &chunks,
        source_fingerprint,
        &hybrid_root,
        &fingerprint(),
        &mut interrupted,
        options,
        &|_| {},
    )
    .expect_err("second batch interruption must preserve checkpoint");
    assert_eq!(
        interrupted.embedded_texts.len(),
        2,
        "the first completed batch is the durable resume unit"
    );
    assert_eq!(
        std::fs::read_to_string(hybrid_root.join("CURRENT")).expect("read CURRENT after failure"),
        prior_current,
        "an interrupted dense build must keep the prior generation readable"
    );

    let checkpoint = source_dense_checkpoint_path(&hybrid_root, source_fingerprint, &fingerprint());
    assert!(checkpoint.is_file(), "interruption must leave a checkpoint");
    let checkpoint_body = std::fs::read_to_string(&checkpoint).expect("read checkpoint");
    let mut checkpoint_lines = checkpoint_body.lines();
    let header: serde_json::Value =
        serde_json::from_str(checkpoint_lines.next().expect("checkpoint header"))
            .expect("parse checkpoint header");
    assert_eq!(header["source_fingerprint"], source_fingerprint);
    assert_eq!(header["embedder_model"], fingerprint().model);
    assert_eq!(header["embedder_url_hash"], fingerprint().url_hash);
    assert_eq!(header["dimension"], fingerprint().dim);
    assert_eq!(header["distance"], fingerprint().distance);
    let first_row: serde_json::Value =
        serde_json::from_str(checkpoint_lines.next().expect("checkpoint vector row"))
            .expect("parse checkpoint vector row");
    assert_eq!(first_row["chunk_id"], chunks[0].id);
    assert!(
        first_row["content_hash_blake3"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        "checkpoint row binds reuse to chunk content"
    );
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&checkpoint)
            .expect("open checkpoint for partial-row simulation");
        file.write_all(b"{\"chunk_id\":\"partial")
            .expect("append partial checkpoint row");
        file.sync_all().expect("sync partial checkpoint row");
    }

    let unrelated = checkpoint
        .parent()
        .expect("checkpoint parent")
        .join("unrelated.ndjson");
    std::fs::write(&unrelated, "unrelated checkpoint").expect("write unrelated checkpoint");
    let progress = Mutex::new(Vec::<DenseBuildProgress>::new());
    let mut resumed = CountingEmbedder::new(2);
    let manifest = build_source_hybrid_generation_resumable(
        &chunks,
        source_fingerprint,
        &hybrid_root,
        &fingerprint(),
        &mut resumed,
        options,
        &|event| progress.lock().expect("progress lock").push(event),
    )
    .expect("resume and publish source-driven generation");
    assert_eq!(
        resumed.embedded_texts.len(),
        1,
        "two vectors must be reused"
    );
    assert_eq!(manifest.dense_count, chunks.len());
    assert!(
        resumed.batch_sizes.iter().all(|size| *size <= 2),
        "embedder batches must stay bounded: {:?}",
        resumed.batch_sizes
    );
    assert!(
        progress
            .lock()
            .expect("progress lock")
            .iter()
            .any(|event| event.reused == 2 && event.completed == chunks.len()),
        "progress must expose reused and completed counts"
    );
    assert!(
        !checkpoint.exists(),
        "successful publication removes only its consumed checkpoint"
    );
    assert!(
        unrelated.exists(),
        "successful publication must not delete unrelated checkpoints"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn checkpoint_reuse_rejects_every_compatibility_axis() {
    let base_chunks = source_chunks();
    let base_source = "catalog-v1:checkpoint-identity";
    let base_fingerprint = fingerprint();

    for axis in ["content", "source", "model", "dimension", "distance"] {
        let root = fixture_root(&format!("checkpoint-{axis}"));
        let hybrid_root = root.join("indexed").join("_all").join("hybrid");
        let mut interrupted = CountingEmbedder::interrupt_after(2, 1);
        build_source_hybrid_generation_resumable(
            &base_chunks,
            base_source,
            &hybrid_root,
            &base_fingerprint,
            &mut interrupted,
            DenseBuildOptions { batch_size: 2 },
            &|_| {},
        )
        .expect_err("seed partial checkpoint");

        let mut chunks = base_chunks.clone();
        let mut source = base_source.to_string();
        let mut fp = base_fingerprint.clone();
        match axis {
            "content" => chunks[0].text.push_str(" changed"),
            "source" => source.push_str("-changed"),
            "model" => {
                fp = EmbedderFingerprint::new(
                    "other-model",
                    "http://example.invalid/embed",
                    2,
                    "cosine",
                )
            }
            "dimension" => {
                fp = EmbedderFingerprint::new(
                    "test-model",
                    "http://example.invalid/embed",
                    3,
                    "cosine",
                )
            }
            "distance" => {
                fp =
                    EmbedderFingerprint::new("test-model", "http://example.invalid/embed", 2, "dot")
            }
            _ => unreachable!(),
        }
        let mut fresh = CountingEmbedder::new(fp.dim);
        build_source_hybrid_generation_resumable(
            &chunks,
            &source,
            &hybrid_root,
            &fp,
            &mut fresh,
            DenseBuildOptions { batch_size: 2 },
            &|_| {},
        )
        .expect("incompatible axis starts a safe workset");
        let expected_new = if axis == "content" { 2 } else { chunks.len() };
        assert_eq!(
            fresh.embedded_texts.len(),
            expected_new,
            "{axis} compatibility drift reused the wrong vectors"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
fn source_driven_generation_builds_aligned_lexical_and_dense_artifacts() {
    let root = fixture_root("source-driven");
    let hybrid_root = root.join("indexed").join("_all").join("hybrid");
    let chunks = source_chunks();
    let embeddings = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.6, 0.4]];
    let source_fingerprint = "catalog-v1:source-snapshot";

    let manifest = publish_source_hybrid_generation(
        &chunks,
        &embeddings,
        source_fingerprint,
        &hybrid_root,
        &fingerprint(),
    )
    .expect("publish source-driven hybrid generation");

    assert_eq!(manifest.source_chunk_count, chunks.len());
    assert_eq!(manifest.lexical_doc_count, chunks.len());
    assert_eq!(manifest.dense_count, chunks.len());
    assert_eq!(manifest.dense_kind, MMAP_DENSE_KIND);
    assert!(
        aicx::vector_index::source_generation_matches_at(&hybrid_root, source_fingerprint, None,)
            .expect("lexical no-op match"),
        "a default lexical index run must preserve an already-complete dense CURRENT"
    );
    assert!(
        aicx::vector_index::source_generation_matches_at(
            &hybrid_root,
            source_fingerprint,
            Some(&fingerprint()),
        )
        .expect("dense no-op match"),
        "the exact source/embedder identity should be a dense no-op"
    );
    assert_eq!(
        manifest.source_hash_blake3,
        hex::encode(source_hash_bytes(source_fingerprint))
    );

    let generation_dir = resolve_hybrid_generation_dir(&hybrid_root);
    assert!(generation_dir.join("manifest.json").is_file());
    assert!(generation_dir.join(MMAP_DENSE_PAYLOAD_FILE_NAME).is_file());
    assert!(
        files_under(&hybrid_root).iter().all(|path| {
            path.file_name()
                .is_none_or(|name| name != "embeddings.ndjson")
        }),
        "source-driven publication must not create the legacy semantic index"
    );

    let dense = MmapDenseAdapter::open(
        generation_dir.join(MMAP_DENSE_PAYLOAD_FILE_NAME),
        fingerprint().dim,
        Distance::Cosine,
        Some(source_hash_bytes(source_fingerprint)),
    )
    .expect("open source-driven dense payload");
    assert_eq!(aicx_retrieve::DenseIndex::count(&dense), chunks.len());
    let dense_ids =
        aicx_retrieve::DenseIndex::query(&dense, &[1.0, 0.0], chunks.len(), &FilterSet::default())
            .expect("query source-driven dense payload")
            .into_iter()
            .map(|hit| hit.chunk_id)
            .collect::<std::collections::BTreeSet<_>>();
    let lexical = TantivyAdapter::new(generation_dir.clone()).expect("open lexical index");
    let lexical_ids = aicx_retrieve::LexicalIndex::query(
        &lexical,
        &LexicalQuery {
            text: "source-driven content".to_string(),
            limit: chunks.len(),
            filters: FilterSet::default(),
        },
    )
    .expect("query source-driven lexical index")
    .into_iter()
    .map(|hit| hit.chunk_id)
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        lexical_ids, dense_ids,
        "lexical and dense legs must expose the same canonical chunk identifiers"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn source_driven_failure_keeps_previous_current_generation() {
    let root = fixture_root("source-driven-failure");
    let hybrid_root = root.join("indexed").join("_all").join("hybrid");
    let chunks = source_chunks();
    let embeddings = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.6, 0.4]];

    publish_source_hybrid_generation(
        &chunks,
        &embeddings,
        "catalog-v1:healthy",
        &hybrid_root,
        &fingerprint(),
    )
    .expect("publish healthy source-driven generation");
    let prior_current =
        std::fs::read_to_string(hybrid_root.join("CURRENT")).expect("read prior CURRENT");

    let wrong_dimension = vec![vec![1.0], vec![0.0], vec![0.5]];
    let error = publish_source_hybrid_generation(
        &chunks,
        &wrong_dimension,
        "catalog-v1:broken",
        &hybrid_root,
        &fingerprint(),
    )
    .expect_err("wrong embedding dimension must fail");
    assert!(
        error.to_string().contains("dim"),
        "unexpected embedding error: {error:#}"
    );
    assert_eq!(
        std::fs::read_to_string(hybrid_root.join("CURRENT")).expect("read CURRENT after failure"),
        prior_current,
        "failed source-driven generation must not replace CURRENT"
    );

    let _ = std::fs::remove_dir_all(&root);
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
        generation_dir.join(MMAP_DENSE_PAYLOAD_FILE_NAME),
        2,
        Distance::Cosine,
        Some(expected_bytes),
    )
    .expect("open dense payload bound to the manifest source hash");
    assert_eq!(aicx_retrieve::DenseIndex::count(&dense), 3);
    drop(dense);
    assert!(
        MmapDenseAdapter::open(
            generation_dir.join(MMAP_DENSE_PAYLOAD_FILE_NAME),
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
    assert!(
        resolve_current_generation_dir(&hybrid_root).is_err(),
        "canonical readers must reject the same root layout without CURRENT"
    );

    let _ = std::fs::remove_dir_all(&root);
}
