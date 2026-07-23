use super::*;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);
static AICX_HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn cosine_orthogonal_vectors_are_zero() {
    let a = [1.0f32, 0.0, 0.0];
    let b = [0.0f32, 1.0, 0.0];
    assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
}

#[test]
fn cosine_identical_vectors_are_one() {
    let a = [0.5f32, 0.3, 0.8];
    let b = [0.5f32, 0.3, 0.8];
    let s = cosine_similarity(&a, &b);
    assert!((s - 1.0).abs() < 1e-6, "expected ~1.0, got {}", s);
}

#[test]
fn cosine_zero_vector_is_safely_zero() {
    let a = [0.0f32, 0.0, 0.0];
    let b = [1.0f32, 2.0, 3.0];
    assert_eq!(cosine_similarity(&a, &b), 0.0);
}

#[test]
fn cosine_dimension_mismatch_returns_zero() {
    let a = [1.0f32, 2.0];
    let b = [1.0f32, 2.0, 3.0];
    assert_eq!(cosine_similarity(&a, &b), 0.0);
}

#[test]
fn chunk_id_strips_md_extension() {
    let path = Path::new("/tmp/store/foo/bar/baz_001.md");
    assert_eq!(chunk_id_from_path(path), "baz_001");
}

#[test]
fn index_path_collapses_slashes_to_underscores() {
    let dir = tempdir_for_test();
    let path = index_path_for(&dir, Some("vetcoders/aicx"));
    let path_str = path.to_string_lossy();
    assert!(
        path_str.contains("vetcoders_aicx"),
        "expected slash collapsed to underscore in {path_str}"
    );
    assert!(
        path_str.ends_with("embeddings.ndjson"),
        "expected NDJSON filename in {path_str}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn partition_incremental_files_keeps_only_unembedded_and_fresh() {
    // G-3 partition logic on a synthetic corpus. The committed
    // baseline knows about ids `a` and `b`; chunk `c` is brand new.
    // `a` is older than cutoff AND committed -> skip; `b` is newer
    // than cutoff but already committed -> skip (crash-recovery
    // guard); `c` is the only survivor (new id wins regardless of
    // mtime, plus its mtime is fresh here anyway).
    use std::collections::HashSet;

    let dir = tempdir_for_test();
    let cutoff_dt = chrono::DateTime::parse_from_rfc3339("2026-05-15T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let chunks = [
        ("a", "2026-05-14T00:00:00Z"), // older than cutoff
        ("b", "2026-05-16T00:00:00Z"), // newer than cutoff
        ("c", "2026-05-16T00:00:00Z"), // newer than cutoff
    ];
    let mut files: Vec<crate::store::StoredContextFile> = Vec::new();
    for (id, mtime_rfc) in chunks {
        let path = dir.join(format!("{id}.md"));
        std::fs::write(&path, format!("# chunk {id}")).unwrap();
        let ts: std::time::SystemTime = chrono::DateTime::parse_from_rfc3339(mtime_rfc)
            .unwrap()
            .with_timezone(&chrono::Utc)
            .into();
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(ts)).unwrap();
        files.push(crate::store::StoredContextFile {
            path,
            project: "test".into(),
            repo: None,
            date_compact: "20260516".into(),
            date_iso: "2026-05-16".into(),
            kind: crate::timeline::Kind::Other,
            agent: "claude".into(),
            session_id: id.into(),
            chunk: 0,
        });
    }

    let mut embedded_ids = HashSet::new();
    embedded_ids.insert("a".to_string());
    embedded_ids.insert("b".to_string());
    let baseline = IncrementalBaseline {
        cutoff: cutoff_dt.into(),
        embedded_ids,
        source_chunk_count: 2,
        source_hash_blake3: "baseline".to_string(),
    };

    let to_embed = partition_incremental_files(&files, &baseline);
    let ids: Vec<String> = to_embed
        .iter()
        .map(|stored| chunk_id_from_path(&stored.path))
        .collect();
    assert_eq!(ids, vec!["c".to_string()]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn partition_incremental_files_reembeds_missing_id_with_old_mtime() {
    // Regression for PR #6 follow-up: a chunk restored from backup /
    // rsync / quarantine restore may have an mtime OLDER than the
    // committed `header.generated_at`, but if its id is not in the
    // committed body the incremental walk MUST still pick it up.
    // Otherwise Layer 2 semantic search silently drifts incomplete
    // and operators are forced into `--full-rescan` to recover.
    use std::collections::HashSet;

    let dir = tempdir_for_test();
    let cutoff_dt = chrono::DateTime::parse_from_rfc3339("2026-05-15T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    // Chunk `restored` has an old mtime (way before the cutoff) and
    // no entry in the committed embedded_ids set — exactly the
    // backup-restore shape we want to cover.
    let restored_path = dir.join("restored.md");
    std::fs::write(&restored_path, "# restored chunk").unwrap();
    let old_ts: std::time::SystemTime =
        chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
            .into();
    filetime::set_file_mtime(&restored_path, filetime::FileTime::from_system_time(old_ts)).unwrap();

    let files = vec![crate::store::StoredContextFile {
        path: restored_path,
        project: "test".into(),
        repo: None,
        date_compact: "20260101".into(),
        date_iso: "2026-01-01".into(),
        kind: crate::timeline::Kind::Other,
        agent: "claude".into(),
        session_id: "restored".into(),
        chunk: 0,
    }];

    // Committed baseline mentions other ids, never `restored`.
    let mut embedded_ids = HashSet::new();
    embedded_ids.insert("a".to_string());
    embedded_ids.insert("b".to_string());
    let baseline = IncrementalBaseline {
        cutoff: cutoff_dt.into(),
        embedded_ids,
        source_chunk_count: 2,
        source_hash_blake3: "baseline".to_string(),
    };

    let to_embed = partition_incremental_files(&files, &baseline);
    let ids: Vec<String> = to_embed
        .iter()
        .map(|stored| chunk_id_from_path(&stored.path))
        .collect();
    assert_eq!(
        ids,
        vec!["restored".to_string()],
        "missing-id chunk with old mtime must be re-embedded"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn copy_committed_body_into_streams_data_rows_only() {
    // G-3 incremental seed: existing committed index has 5 data rows
    // plus a header. The seed helper must write exactly those 5 rows
    // into the tmp writer (header dropped because the caller writes a
    // fresh placeholder of its own).
    let dir = tempdir_for_test();
    let target = dir.join("embeddings.ndjson");
    let tmp = dir.join("embeddings.ndjson.tmp");

    let header = IndexHeader {
        schema_version: "v0-test".into(),
        model_id: "test-model".into(),
        model_profile: "base".into(),
        dimension: 4,
        generated_at: "2026-05-15T12:00:00Z".into(),
        entry_count: 5,
    };
    let mut body = serde_json::to_string(&header).unwrap();
    body.push('\n');
    for i in 0..5 {
        body.push_str(&format!(
            r#"{{"id":"row-{i}","embedding":[0.1,0.2,0.3,0.4]}}"#
        ));
        body.push('\n');
    }
    std::fs::write(&target, &body).unwrap();

    {
        let mut writer = std::io::BufWriter::new(std::fs::File::create(&tmp).unwrap());
        use std::io::Write;
        // Caller writes its own placeholder first.
        writeln!(writer, "{{\"placeholder\":true}}").unwrap();
        let rows = copy_committed_body_into(&mut writer, &target).unwrap();
        assert_eq!(rows, 5, "seed must surface row count for D-2 math");
    }

    let content = std::fs::read_to_string(&tmp).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 6, "placeholder header + 5 data rows");
    assert_eq!(lines[0], "{\"placeholder\":true}");
    for (i, expected_id) in (0..5).map(|i| format!("row-{i}")).enumerate() {
        assert!(
            lines[i + 1].contains(&expected_id),
            "row {} preserved verbatim",
            i
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn incremental_round_trip_appends_only_new_rows() {
    // End-to-end shape: first committed index has 3 rows. Operator
    // adds 5 more chunks. Incremental walk + seed + rewrite produces
    // a final file with exactly 8 rows and a truthful entry_count.
    let dir = tempdir_for_test();
    let target = dir.join("embeddings.ndjson");
    let tmp = dir.join("embeddings.ndjson.tmp");
    let commit_tmp = dir.join("embeddings.ndjson.commit-tmp");

    // 1. Seed the canonical committed file with 3 rows.
    let old_header = IndexHeader {
        schema_version: INDEX_SCHEMA_VERSION.into(),
        model_id: "test-model".into(),
        model_profile: "base".into(),
        dimension: 4,
        generated_at: "2026-05-15T12:00:00Z".into(),
        entry_count: 3,
    };
    let mut body = serde_json::to_string(&old_header).unwrap();
    body.push('\n');
    for i in 0..3 {
        body.push_str(&format!(
            r#"{{"id":"old-{i}","project":"test","agent":"claude","date":"20260515","path":"/tmp/old-{i}.md","kind":"other","session_id":"old-{i}","frame_kind":null,"cwd":null,"embedding":[0.1,0.2,0.3,0.4]}}"#
        ));
        body.push('\n');
    }
    std::fs::write(&target, &body).unwrap();

    // 2. Simulate incremental write: tmp = placeholder + copy of old
    //    body + 5 brand-new rows. Mirrors the production sequencing
    //    inside `write_index_with_options` without invoking the
    //    embedder (the unit-of-work the test is asserting is the
    //    file-level math, not the embed step itself).
    {
        let mut writer = std::io::BufWriter::new(std::fs::File::create(&tmp).unwrap());
        use std::io::Write;
        let placeholder = IndexHeader {
            entry_count: 0,
            generated_at: "2026-05-16T12:00:00Z".into(),
            ..old_header.clone()
        };
        writeln!(writer, "{}", serde_json::to_string(&placeholder).unwrap()).unwrap();
        copy_committed_body_into(&mut writer, &target).unwrap();
        for i in 0..5 {
            writeln!(
                writer,
                r#"{{"id":"new-{i}","project":"test","agent":"claude","date":"20260516","path":"/tmp/new-{i}.md","kind":"other","session_id":"new-{i}","frame_kind":null,"cwd":null,"embedding":[0.5,0.6,0.7,0.8]}}"#
            )
            .unwrap();
        }
    }

    // 3. Truthful header rewrite (D-2 contract) + atomic rename
    //    onto the canonical target.
    let truthful = IndexHeader {
        entry_count: 8,
        generated_at: "2026-05-16T12:00:00Z".into(),
        ..old_header
    };
    rewrite_index_with_truthful_header(&tmp, &commit_tmp, &truthful).unwrap();
    let _ = std::fs::remove_file(&tmp);
    std::fs::rename(&commit_tmp, &target).unwrap();

    // 4. Assertions: header.entry_count == 8, body has 3 old + 5 new
    //    rows, no full re-embed of the originals.
    let final_content = std::fs::read_to_string(&target).unwrap();
    let lines: Vec<&str> = final_content.lines().collect();
    assert_eq!(lines.len(), 9, "header + 3 old + 5 new = 9 lines");
    let final_header: IndexHeader = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(
        final_header.entry_count, 8,
        "D-2 entry_count truthful after incremental append"
    );
    let old_count = (1..=3).filter(|i| lines[*i].contains("old-")).count();
    let new_count = (4..=8).filter(|i| lines[*i].contains("new-")).count();
    assert_eq!(old_count, 3, "3 original rows preserved verbatim");
    assert_eq!(new_count, 5, "exactly 5 new rows appended");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Build an `EmbeddingModelInfo` matching the headers written by the
/// resume-checkpoint tests below (profile `base`, dimension `dim`).
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn test_model_info(dim: usize) -> crate::embedder::EmbeddingModelInfo {
    crate::embedder::EmbeddingModelInfo {
        model_id: "test-model".into(),
        dimension: dim,
        backend: "test".into(),
        profile: crate::embedder::EmbeddingProfile::Base,
        source: crate::embedder::NativeEmbeddingSource::ExplicitPath(std::path::PathBuf::from("")),
    }
}

/// Serialize a well-formed resume checkpoint (header + `count` id rows)
/// into `path`. Ids are `existing-{i}`. When `trailing_newline` is false
/// the final row is written without a terminating `\n`, mirroring a build
/// killed mid-write.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn write_resume_checkpoint(path: &Path, dim: usize, count: usize, trailing_newline: bool) {
    let header = IndexHeader {
        schema_version: INDEX_SCHEMA_VERSION.into(),
        model_id: "test-model".into(),
        model_profile: "base".into(),
        dimension: dim,
        generated_at: "2026-05-15T12:00:00Z".into(),
        entry_count: 0,
    };
    let embedding: Vec<f32> = (0..dim).map(|_| 0.1_f32).collect();
    let mut body = serde_json::to_string(&header).unwrap();
    body.push('\n');
    for i in 0..count {
        body.push_str(&make_entry_line(
            &format!("existing-{i}"),
            embedding.clone(),
        ));
        body.push('\n');
    }
    if !trailing_newline {
        body.pop();
    }
    std::fs::write(path, &body).unwrap();
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn load_resume_tmp_index_reads_full_checkpoint_and_flags_newline() {
    // Resume precedence input: an interrupted full build leaves a `.tmp`
    // checkpoint. The loader must surface every embedded id and the exact
    // row count so the classifier can skip them — and must report whether
    // the file needs a repair newline before append. If this returned
    // `None`/short counts the resume path would silently re-embed.
    let dir = tempdir_for_test();
    let tmp = dir.join("embeddings.ndjson.tmp");
    let info = test_model_info(4);

    // Well-formed checkpoint (ends with `\n`).
    write_resume_checkpoint(&tmp, 4, 6, true);
    let state = load_resume_tmp_index(&tmp, &info)
        .unwrap()
        .expect("checkpoint must be resumable");
    assert_eq!(state.rows, 6, "all 6 embedded rows counted");
    assert_eq!(state.ids.len(), 6, "all 6 ids captured for skip set");
    for i in 0..6 {
        assert!(
            state.ids.contains(&format!("existing-{i}")),
            "id existing-{i} must be in the resume skip set"
        );
    }
    assert!(
        !state.needs_newline,
        "trailing newline present -> no repair"
    );

    // Checkpoint killed mid-write (no trailing newline) must flag repair
    // so the appended rows do not glue onto a partial last line.
    write_resume_checkpoint(&tmp, 4, 6, false);
    let state = load_resume_tmp_index(&tmp, &info).unwrap().unwrap();
    assert_eq!(
        state.rows, 6,
        "partial-newline checkpoint still counts rows"
    );
    assert!(
        state.needs_newline,
        "missing trailing newline -> repair before append"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn resume_appends_new_rows_without_truncating_checkpoint() {
    // Regression: the resume path must APPEND onto the surviving `.tmp`
    // checkpoint, never truncate + re-seed it. Repro shape: a full build
    // is interrupted with K embedded rows; resuming classifies the first
    // K chunks as already-done (skip) and only embeds the M new ones, so
    // the file ends with K + M rows and the original K are byte-preserved.
    let dir = tempdir_for_test();
    let tmp = dir.join("embeddings.ndjson.tmp");
    let info = test_model_info(4);

    const K: usize = 5;
    const M: usize = 3;

    // Interrupted full build: K embedded rows in the checkpoint.
    write_resume_checkpoint(&tmp, 4, K, true);
    let before = std::fs::read_to_string(&tmp).unwrap();
    let original_rows: Vec<String> = before.lines().skip(1).map(str::to_string).collect();
    assert_eq!(original_rows.len(), K, "checkpoint seeded with K rows");

    // Resume reads the checkpoint -> skip set of the K embedded ids.
    let state = load_resume_tmp_index(&tmp, &info).unwrap().unwrap();
    let resumed_ids: HashSet<String> = state.ids.clone();

    // Phase-1 classification (production predicate): the capped window is
    // the K already-embedded chunks followed by M fresh ones. Ids come
    // from `chunk_id_from_path`, exactly as the embed loop derives them.
    let mut to_embed = Vec::new();
    let mut skipped = 0usize;
    let existing_paths: Vec<std::path::PathBuf> = (0..K)
        .map(|i| dir.join(format!("existing-{i}.md")))
        .collect();
    let fresh_paths: Vec<std::path::PathBuf> =
        (0..M).map(|i| dir.join(format!("fresh-{i}.md"))).collect();
    for path in existing_paths.iter().chain(fresh_paths.iter()) {
        let entry_id = chunk_id_from_path(path);
        if resumed_ids.contains(&entry_id) {
            skipped += 1;
        } else {
            to_embed.push(entry_id);
        }
    }
    assert_eq!(skipped, K, "the K embedded chunks are resume-skipped");
    assert_eq!(
        to_embed.len(),
        M,
        "only the M new chunks flow to the embedder"
    );
    assert!(
        to_embed.iter().all(|id| id.starts_with("fresh-")),
        "zero re-embed of already-committed chunks"
    );

    // Resume writer (production sequencing): open with append(true), never
    // create/truncate, then write the M new rows.
    {
        use std::fs::OpenOptions;
        use std::io::{BufWriter, Write};
        let mut writer = BufWriter::new(OpenOptions::new().append(true).open(&tmp).unwrap());
        let embedding: Vec<f32> = vec![0.5, 0.6, 0.7, 0.8];
        for id in &to_embed {
            writeln!(writer, "{}", make_entry_line(id, embedding.clone())).unwrap();
        }
    }

    // File ends with K + M rows; the original K are byte-identical.
    let after = std::fs::read_to_string(&tmp).unwrap();
    let data: Vec<&str> = after.lines().skip(1).collect();
    assert_eq!(
        data.len(),
        K + M,
        "checkpoint grew to K + M rows (no truncation)"
    );
    for (i, original) in original_rows.iter().enumerate() {
        assert_eq!(data[i], original, "original row {i} preserved verbatim");
    }
    for row in data.iter().skip(K) {
        assert!(row.contains("fresh-"), "appended rows are the new chunks");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn rewrite_index_with_truthful_header_replaces_placeholder_and_preserves_entries() {
    // D-2: rewrite swap-and-rename helper produces a file whose first
    // line carries the truthful entry_count and whose data lines are
    // byte-for-byte identical to the placeholder tmp.
    let dir = tempdir_for_test();
    let tmp_path = dir.join("test.ndjson.tmp");
    let final_tmp = dir.join("test.ndjson.commit-tmp");

    let placeholder = IndexHeader {
        schema_version: "v0-test".to_string(),
        model_id: "test-model".to_string(),
        model_profile: "base".to_string(),
        dimension: 4,
        generated_at: "2026-01-01T00:00:00Z".to_string(),
        entry_count: 0,
    };
    let entries = [
        r#"{"id":"a","embedding":[0.1,0.2,0.3,0.4]}"#,
        r#"{"id":"b","embedding":[0.5,0.6,0.7,0.8]}"#,
        r#"{"id":"c","embedding":[0.9,1.0,1.1,1.2]}"#,
    ];
    let mut tmp_bytes = serde_json::to_string(&placeholder).unwrap();
    tmp_bytes.push('\n');
    for entry in &entries {
        tmp_bytes.push_str(entry);
        tmp_bytes.push('\n');
    }
    std::fs::write(&tmp_path, &tmp_bytes).unwrap();

    let truthful = IndexHeader {
        entry_count: entries.len(),
        ..placeholder.clone()
    };
    rewrite_index_with_truthful_header(&tmp_path, &final_tmp, &truthful)
        .expect("rewrite must succeed");

    let lines: Vec<String> = std::fs::read_to_string(&final_tmp)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let header: IndexHeader = serde_json::from_str(&lines[0]).expect("header parses");
    assert_eq!(
        header.entry_count,
        entries.len(),
        "rewritten header must carry truthful entry_count"
    );
    assert_eq!(&lines[1..], entries);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn index_path_uses_all_bucket_for_no_project_filter() {
    let dir = tempdir_for_test();
    let path = index_path_for(&dir, None);
    assert!(
        path.to_string_lossy().contains("_all"),
        "expected _all bucket in {}",
        path.display()
    );
    assert_eq!(path, dir.join("indexed").join("_all").join(INDEX_FILE_NAME));
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn derive_project_index_from_all_streams_matching_rows_only() {
    let home = tempdir_for_test();
    let _guard = ScopedAicxHome::set(&home);
    std::fs::create_dir_all(home.join("locks")).expect("create locks dir");

    let all_index = index_path(None).expect("all index path");
    std::fs::create_dir_all(all_index.parent().unwrap()).expect("create all index dir");

    let header = IndexHeader {
        schema_version: INDEX_SCHEMA_VERSION.to_string(),
        model_id: "test-model".to_string(),
        model_profile: "base".to_string(),
        dimension: 2,
        generated_at: "2026-06-01T00:00:00Z".to_string(),
        entry_count: 3,
    };
    let mk_entry = |id: &str, project: &str| IndexEntry {
        id: id.to_string(),
        project: project.to_string(),
        agent: "codex".to_string(),
        date: "20260601".to_string(),
        path: home.join("store").join(format!("{id}.md")),
        kind: "conversations".to_string(),
        session_id: format!("session-{id}"),
        frame_kind: Some("user_msg".to_string()),
        cwd: None,
        embedding: vec![1.0, 0.0],
    };
    let rows = [
        mk_entry("example-app-1", "vetcoders/example-app"),
        mk_entry("other-1", "vetcoders/Other"),
        mk_entry("example-app-2", "vetcoders/example-app"),
    ];
    let mut body = serde_json::to_string(&header).unwrap();
    body.push('\n');
    for row in rows {
        body.push_str(&serde_json::to_string(&row).unwrap());
        body.push('\n');
    }
    std::fs::write(&all_index, body).expect("write synthetic all index");

    let stats =
        derive_project_index_from_all("vetcoders/example-app").expect("derive project index");
    assert_eq!(stats.entries_written, 2);

    let project_index = index_path(Some("vetcoders/example-app")).expect("project index path");
    assert_eq!(stats.index_path, project_index);
    let (derived_header, derived_entries) =
        read_committed_index_entries(&project_index).expect("read derived project index");
    assert_eq!(derived_header.entry_count, 2);
    assert_eq!(derived_entries.len(), 2);
    assert!(
        derived_entries
            .iter()
            .all(|entry| entry.project == "vetcoders/example-app"),
        "derived bucket must not include other projects: {derived_entries:?}"
    );

    let _ = std::fs::remove_dir_all(home);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn derive_project_indexes_from_all_materializes_every_project_in_one_call() {
    let home = tempdir_for_test();
    let _guard = ScopedAicxHome::set(&home);
    std::fs::create_dir_all(home.join("locks")).expect("create locks dir");

    let all_index = index_path(None).expect("all index path");
    std::fs::create_dir_all(all_index.parent().unwrap()).expect("create all index dir");

    let header = IndexHeader {
        schema_version: INDEX_SCHEMA_VERSION.to_string(),
        model_id: "test-model".to_string(),
        model_profile: "base".to_string(),
        dimension: 2,
        generated_at: "2026-06-01T00:00:00Z".to_string(),
        entry_count: 4,
    };
    let mk_entry = |id: &str, project: &str| IndexEntry {
        id: id.to_string(),
        project: project.to_string(),
        agent: "codex".to_string(),
        date: "20260601".to_string(),
        path: home.join("store").join(format!("{id}.md")),
        kind: "conversations".to_string(),
        session_id: format!("session-{id}"),
        frame_kind: Some("user_msg".to_string()),
        cwd: None,
        embedding: vec![1.0, 0.0],
    };
    let rows = [
        mk_entry("example-app-1", "vetcoders/example-app"),
        mk_entry("blackbox-1", "m-szymanska/agent-blackbox"),
        mk_entry("example-app-2", "vetcoders/example-app"),
        mk_entry("blackbox-2", "m-szymanska/agent-blackbox"),
    ];
    let mut body = serde_json::to_string(&header).unwrap();
    body.push('\n');
    for row in rows {
        body.push_str(&serde_json::to_string(&row).unwrap());
        body.push('\n');
    }
    std::fs::write(&all_index, body).expect("write synthetic all index");

    let stats = derive_project_indexes_from_all(&[]).expect("derive every project");
    let projects: HashSet<_> = stats.iter().map(|stat| stat.project.as_str()).collect();
    assert_eq!(
        projects,
        HashSet::from(["vetcoders/example-app", "m-szymanska/agent-blackbox"])
    );
    assert!(stats.iter().all(|stat| stat.entries_written == 2));

    for project in ["vetcoders/example-app", "m-szymanska/agent-blackbox"] {
        let project_index = index_path(Some(project)).expect("project index path");
        let (derived_header, derived_entries) =
            read_committed_index_entries(&project_index).expect("read derived project index");
        assert_eq!(derived_header.entry_count, 2);
        assert_eq!(derived_entries.len(), 2);
        assert!(
            derived_entries.iter().all(|entry| entry.project == project),
            "derived bucket for {project} must not include other projects: {derived_entries:?}"
        );
    }

    let _ = std::fs::remove_dir_all(home);
}

fn tempdir_for_test() -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    let n = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    p.push(format!(
        "aicx-vector-index-test-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// RAII guard that scopes `AICX_HOME` to a test tempdir so that paths
/// derived from `store::store_base_dir()` (lock paths, store paths) stay
/// isolated from any concurrently running aicx process on the host.
///
/// On drop, restores the previous value of `AICX_HOME` (or unsets it).
/// Required for tests that exercise code paths inside
/// `query_index_with_embedding` etc., which acquire the canonical
/// `lance.lock` derived from `AICX_HOME`.
struct ScopedAicxHome {
    previous: Option<std::ffi::OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl ScopedAicxHome {
    fn set(home: &std::path::Path) -> Self {
        let guard = AICX_HOME_ENV_LOCK.lock().expect("AICX_HOME test lock");
        let previous = std::env::var_os("AICX_HOME");
        // SAFETY: env mutation is single-threaded within this test scope;
        // the RAII guard restores prior state on drop.
        unsafe {
            std::env::set_var("AICX_HOME", home);
        }
        Self {
            previous,
            _guard: guard,
        }
    }
}

impl Drop for ScopedAicxHome {
    fn drop(&mut self) {
        // SAFETY: env restore is single-threaded within the test scope.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var("AICX_HOME", value),
                None => std::env::remove_var("AICX_HOME"),
            }
        }
    }
}

/// Build a synthetic NDJSON data-line for an `IndexEntry`. Mirrors the
/// real `write_index` row shape without going through filesystem.
fn make_entry_line(id: &str, embedding: Vec<f32>) -> String {
    let entry = IndexEntry {
        id: id.to_string(),
        project: "test".to_string(),
        agent: "claude".to_string(),
        date: "20260515".to_string(),
        path: std::path::PathBuf::from(format!("/tmp/aicx-test/{id}.md")),
        kind: "session".to_string(),
        session_id: id.to_string(),
        frame_kind: None,
        cwd: None,
        embedding,
    };
    serde_json::to_string(&entry).expect("serialize synthetic entry")
}

fn ok_lines(
    lines: impl IntoIterator<Item = String>,
) -> impl Iterator<Item = std::io::Result<String>> {
    lines.into_iter().map(Ok::<_, std::io::Error>)
}

#[test]
fn capped_index_lines_error_on_oversized_and_advance_to_next_line() {
    let next = make_entry_line("after-oversized", vec![1.0]);
    let mut input = "x".repeat(crate::sanitize::MAX_VALIDATED_BYTES + 1);
    input.push('\n');
    input.push_str(&next);
    input.push('\n');

    let reader = std::io::BufReader::new(std::io::Cursor::new(input.into_bytes()));
    let mut lines = capped_index_lines(
        reader,
        Path::new("/tmp/aicx-vector-index-oversized.ndjson"),
        2,
        "test index data",
    );
    let err = lines
        .next()
        .expect("first oversized line is observed")
        .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("exceeds"));
    let second = lines
        .next()
        .expect("reader advances to following line")
        .expect("second line is valid");
    assert_eq!(second, next);
}

#[test]
fn scan_index_entries_no_corrupt_returns_all_hits() {
    let q = vec![1.0f32, 0.0, 0.0];
    let lines = vec![
        make_entry_line("a", vec![1.0, 0.0, 0.0]),
        make_entry_line("b", vec![0.0, 1.0, 0.0]),
        make_entry_line("c", vec![0.5, 0.5, 0.0]),
    ];
    let scan = scan_index_entries(ok_lines(lines), &q, None, None).expect("scan");
    assert_eq!(scan.total_data_lines, 3);
    assert_eq!(scan.corrupt_count, 0);
    assert_eq!(scan.hits.len(), 3);
}

#[test]
fn scan_index_entries_counts_corrupt_lines_below_threshold() {
    // 1 corrupt out of 10 = 10% — above the 5% threshold ratio, but
    // policy lives in `query_index`. The helper itself only reports.
    let q = vec![1.0f32, 0.0];
    let mut lines: Vec<String> = (0..9)
        .map(|i| make_entry_line(&format!("ok-{i}"), vec![1.0, 0.0]))
        .collect();
    lines.push("{not valid json".to_string());

    let scan = scan_index_entries(ok_lines(lines), &q, None, None).expect("scan");
    assert_eq!(scan.total_data_lines, 10);
    assert_eq!(scan.corrupt_count, 1);
    assert_eq!(scan.hits.len(), 9, "valid entries still parsed and scored");
}

fn make_hit(id: &str, score: f32) -> QueryHit {
    QueryHit {
        id: id.to_string(),
        project: "test".to_string(),
        agent: "claude".to_string(),
        date: "20260524".to_string(),
        path: std::path::PathBuf::from(format!("/tmp/aicx-test/{id}.md")),
        kind: "session".to_string(),
        session_id: id.to_string(),
        frame_kind: None,
        cwd: None,
        score,
    }
}

#[test]
fn finalize_query_hits_truncates_to_requested_limit() {
    // Bug #32 regression. The legacy `query_index` accepted a `limit`
    // arg but did not honor it — the parameter was prefixed `_limit`
    // and the post-scan tail returned every hit. This locks the
    // contract that the returned vec has `len() <= limit`.
    let hits: Vec<QueryHit> = (0..50)
        .map(|i| make_hit(&format!("h-{i}"), (50 - i) as f32 / 50.0))
        .collect();
    let out = finalize_query_hits(hits, 10);
    assert_eq!(out.len(), 10, "limit honored: returns exactly 10 hits");
    // Top score is the highest (1.0); confirm score-desc sort holds
    // after truncate so the kept 10 are the BEST 10, not a random
    // slice.
    assert!(
        out.windows(2).all(|w| w[0].score >= w[1].score),
        "hits remain sorted score-desc after truncate"
    );
    assert_eq!(out[0].id, "h-0", "highest-scoring hit retained at head");
}

#[test]
fn finalize_query_hits_returns_full_pool_when_limit_exceeds_pool() {
    // Pool shorter than limit ⇒ return everything (no padding, no
    // panic). Documents the "fewer if pool exhausted" half of the
    // bug #32 contract.
    let hits: Vec<QueryHit> = (0..3)
        .map(|i| make_hit(&format!("h-{i}"), i as f32 / 3.0))
        .collect();
    let out = finalize_query_hits(hits, 100);
    assert_eq!(out.len(), 3);
}

#[test]
fn finalize_query_hits_zero_limit_returns_empty() {
    // `limit == 0` is a legal request for "no hits, just confirm the
    // scan ran". The legacy code ignored `_limit` entirely; the fix
    // honors it strictly, including the degenerate case.
    let hits: Vec<QueryHit> = (0..5)
        .map(|i| make_hit(&format!("h-{i}"), i as f32))
        .collect();
    let out = finalize_query_hits(hits, 0);
    assert!(out.is_empty(), "limit=0 returns empty vec");
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn query_index_recovery_hint_uses_full_rescan_not_fresh() {
    // Bug #33 regression. Exercise the same post-embedder query helper
    // that opens the on-disk index, scans NDJSON data rows, and surfaces
    // the operator-facing recovery hint when corruption exceeds policy.
    let root = tempdir_for_test();
    let path = index_path_for(&root, Some("recovery-hint"));
    std::fs::create_dir_all(path.parent().expect("index parent")).unwrap();

    let header = IndexHeader {
        schema_version: INDEX_SCHEMA_VERSION.to_string(),
        model_id: "test-model".to_string(),
        model_profile: "base".to_string(),
        dimension: 2,
        generated_at: "2026-05-24T18:13:11Z".to_string(),
        entry_count: 20,
    };
    let mut body = serde_json::to_string(&header).expect("serialize header");
    body.push('\n');
    for i in 0..18 {
        body.push_str(&make_entry_line(&format!("ok-{i}"), vec![1.0, 0.0]));
        body.push('\n');
    }
    body.push_str("{not valid json\n");
    body.push_str("{still not valid json\n");
    std::fs::write(&path, body).expect("write corrupt fixture index");

    // Isolate lance.lock to the test tempdir so the assertion exercises the
    // recovery-hint path even when a real `aicx index` is running on the
    // host machine. Without this scope, the test contends on
    // `~/.aicx/locks/lance.lock` against any active indexer and surfaces a
    // lock-timeout error rather than the integrity-failure recovery hint
    // that the regression guard cares about.
    let _aicx_home_guard = ScopedAicxHome::set(&root);
    let err = query_index_with_embedding(&path, &[1.0, 0.0], 10, None, None)
        .expect_err("corrupt fixture should fail-fast");
    let message = format!("{err:#}");
    assert!(
        message.contains("--full-rescan"),
        "query_index recovery hint must reference the canonical rescan flag"
    );
    let stale_flag = format!("--{}", "fresh");
    assert!(
        !message.contains(&stale_flag),
        "stale rescan flag hint must not appear in the recovery message"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn incremental_materialize_hybrid_refreshes_persisted_artifacts() {
    use aicx_retrieve::{Distance, HybridIndex, ReciprocalRankFusion, TantivyAdapter};

    let root = tempdir_for_test();
    let _aicx_home_guard = ScopedAicxHome::set(&root);
    let project = "vetcoders/example-app";
    let semantic_index = index_path_for(&root, Some(project));
    std::fs::create_dir_all(semantic_index.parent().expect("semantic parent")).unwrap();

    let chunks_dir = root.join("chunks");
    std::fs::create_dir_all(&chunks_dir).unwrap();
    let chunk_a_path = chunks_dir.join("a.md");
    let chunk_b_path = chunks_dir.join("b.md");
    let chunk_c_path = chunks_dir.join("c.md");
    std::fs::write(&chunk_a_path, "alpha").unwrap();
    std::fs::write(&chunk_b_path, "bravo").unwrap();
    std::fs::write(&chunk_c_path, "charlie").unwrap();

    let make_entry = |id: &str, path: &std::path::Path, embedding: Vec<f32>| IndexEntry {
        id: id.to_string(),
        project: "vetcoders/example-app".to_string(),
        agent: "claude".to_string(),
        date: "20260603".to_string(),
        path: path.to_path_buf(),
        kind: "conversations".to_string(),
        session_id: format!("session-{id}"),
        frame_kind: Some("agent_reply".to_string()),
        cwd: Some("/Users/tester/Git/example-app".to_string()),
        embedding,
    };
    let write_semantic_index = |entries: &[IndexEntry], generated_at: &str| {
        let header = IndexHeader {
            schema_version: INDEX_SCHEMA_VERSION.to_string(),
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
        std::fs::write(&semantic_index, body).expect("write semantic fixture");
    };

    let entry_a = make_entry("a", &chunk_a_path, vec![1.0, 0.0]);
    let entry_b = make_entry("b", &chunk_b_path, vec![0.0, 1.0]);
    let entry_c = make_entry("c", &chunk_c_path, vec![0.6, 0.4]);
    write_semantic_index(&[entry_a.clone(), entry_b.clone()], "2026-06-03T18:00:00Z");

    let info = crate::embedder::EmbeddingModelInfo {
        model_id: "test-model".to_string(),
        dimension: 2,
        backend: "gguf".to_string(),
        profile: crate::embedder::EmbeddingProfile::Base,
        source: crate::embedder::NativeEmbeddingSource::ExplicitPath(
            root.join("fixtures").join("test-model.gguf"),
        ),
    };

    let initial = materialize_hybrid_index(&semantic_index, Some(project), &info)
        .expect("initial hybrid build");
    assert_eq!(initial.dense_count, 2);
    assert_eq!(initial.lexical_doc_count, 2);

    write_semantic_index(
        &[entry_a.clone(), entry_b.clone(), entry_c.clone()],
        "2026-06-03T18:05:00Z",
    );
    let source_hash =
        observed_source_hash_for_index_path(&semantic_index).expect("semantic source hash");
    let delta = aicx_retrieve::DenseChunkRef {
        chunk: aicx_retrieve::ChunkRef {
            id: entry_c.id.clone(),
            source_path: entry_c.path.to_string_lossy().to_string(),
            text: std::fs::read_to_string(&entry_c.path).expect("read delta chunk"),
            metadata: index_entry_metadata_json(&entry_c),
        },
        embedding: entry_c.embedding.clone(),
    };

    let refreshed = incremental_materialize_hybrid(
        Some(project),
        &info,
        &[delta],
        3,
        &source_hash,
        &semantic_index,
    )
    .expect("incremental hybrid refresh");
    assert_eq!(refreshed.source_chunk_count, 3);
    assert_eq!(refreshed.dense_count, 3);
    assert_eq!(refreshed.lexical_doc_count, 3);
    assert_ne!(refreshed.generation_id, initial.generation_id);

    let persisted = aicx_retrieve::Manifest::read_from_path(
        &hybrid_manifest_path(Some(project)).expect("manifest path"),
    )
    .expect("persisted manifest");
    assert_eq!(persisted.source_chunk_count, 3);
    assert_eq!(persisted.dense_count, 3);
    assert_eq!(persisted.lexical_doc_count, 3);

    // The refreshed generation still holds exactly one dense payload, bound
    // to the refreshed manifest's source hash.
    let manifest_dir = hybrid_index_dir(Some(project)).expect("manifest dir");
    assert!(
        !hybrid_dense_path(Some(project))
            .expect("legacy dense path")
            .exists(),
        "incremental refresh must not write the legacy NDJSON dense twin"
    );
    let lexical = Box::new(TantivyAdapter::new(manifest_dir.clone()).expect("fresh lexical"));
    let dense = Box::new(
        aicx_retrieve::MmapDenseAdapter::open(
            hybrid_dense_mmap_path(Some(project)).expect("dense payload path"),
            info.dimension,
            Distance::Cosine,
            Some(aicx_retrieve::decode_source_hash_blake3(&persisted.source_hash_blake3).unwrap()),
        )
        .expect("fresh dense"),
    );
    let fusion = Box::new(ReciprocalRankFusion::default());
    let reloaded = HybridIndex::load_from_manifest(
        lexical,
        dense,
        fusion,
        manifest_dir,
        hybrid_embedder_fingerprint(&info),
        Some(source_hash.as_str()),
    )
    .expect("fresh reload after incremental refresh");
    let manifest = reloaded.manifest().expect("reloaded manifest");
    assert_eq!(manifest.source_chunk_count, 3);
    assert_eq!(manifest.dense_count, 3);
    assert_eq!(manifest.lexical_doc_count, 3);

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn materialize_hybrid_index_skips_missing_source_rows() {
    let root = tempdir_for_test();
    let _aicx_home_guard = ScopedAicxHome::set(&root);
    let project = "vetcoders/example-app";
    let semantic_index = index_path_for(&root, Some(project));
    std::fs::create_dir_all(semantic_index.parent().expect("semantic parent")).unwrap();

    let chunks_dir = root.join("chunks");
    std::fs::create_dir_all(&chunks_dir).unwrap();
    let live_chunk_path = chunks_dir.join("live.md");
    let missing_chunk_path = chunks_dir.join("missing.md");
    std::fs::write(&live_chunk_path, "live chunk").unwrap();

    let make_entry = |id: &str, path: &std::path::Path, embedding: Vec<f32>| IndexEntry {
        id: id.to_string(),
        project: "vetcoders/example-app".to_string(),
        agent: "codex".to_string(),
        date: "20260614".to_string(),
        path: path.to_path_buf(),
        kind: "conversations".to_string(),
        session_id: format!("session-{id}"),
        frame_kind: Some("agent_reply".to_string()),
        cwd: Some("/Users/tester/vc-workspace/vetcoders/aicx".to_string()),
        embedding,
    };
    let header = IndexHeader {
        schema_version: INDEX_SCHEMA_VERSION.to_string(),
        model_id: "test-model".to_string(),
        model_profile: "base".to_string(),
        dimension: 2,
        generated_at: "2026-06-14T05:32:00Z".to_string(),
        entry_count: 2,
    };
    let entries = [
        make_entry("live", &live_chunk_path, vec![1.0, 0.0]),
        make_entry("missing", &missing_chunk_path, vec![0.0, 1.0]),
    ];
    let mut body = serde_json::to_string(&header).expect("serialize header");
    body.push('\n');
    for entry in &entries {
        body.push_str(&serde_json::to_string(entry).expect("serialize entry"));
        body.push('\n');
    }
    std::fs::write(&semantic_index, body).expect("write semantic fixture");

    let info = crate::embedder::EmbeddingModelInfo {
        model_id: "test-model".to_string(),
        dimension: 2,
        backend: "gguf".to_string(),
        profile: crate::embedder::EmbeddingProfile::Base,
        source: crate::embedder::NativeEmbeddingSource::ExplicitPath(
            root.join("fixtures").join("test-model.gguf"),
        ),
    };

    let manifest = materialize_hybrid_index(&semantic_index, Some(project), &info)
        .expect("hybrid build should tolerate stale source rows");
    assert_eq!(manifest.source_chunk_count, 1);
    assert_eq!(manifest.dense_count, 1);
    assert_eq!(manifest.lexical_doc_count, 1);

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn incremental_baseline_detects_hybrid_manifest_stale_against_committed_source() {
    let root = tempdir_for_test();
    let _aicx_home_guard = ScopedAicxHome::set(&root);
    let project = "vetcoders/example-app";
    let semantic_index = index_path_for(&root, Some(project));
    std::fs::create_dir_all(semantic_index.parent().expect("semantic parent")).unwrap();

    let chunks_dir = root.join("chunks");
    std::fs::create_dir_all(&chunks_dir).unwrap();
    let chunk_a_path = chunks_dir.join("a.md");
    let chunk_b_path = chunks_dir.join("b.md");
    std::fs::write(&chunk_a_path, "alpha").unwrap();
    std::fs::write(&chunk_b_path, "bravo").unwrap();

    let make_entry = |id: &str, path: &std::path::Path, embedding: Vec<f32>| IndexEntry {
        id: id.to_string(),
        project: "vetcoders/example-app".to_string(),
        agent: "claude".to_string(),
        date: "20260603".to_string(),
        path: path.to_path_buf(),
        kind: "conversations".to_string(),
        session_id: format!("session-{id}"),
        frame_kind: Some("agent_reply".to_string()),
        cwd: Some("/Users/tester/Git/example-app".to_string()),
        embedding,
    };
    let write_semantic_index = |entries: &[IndexEntry], generated_at: &str| {
        let header = IndexHeader {
            schema_version: INDEX_SCHEMA_VERSION.to_string(),
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
        std::fs::write(&semantic_index, body).expect("write semantic fixture");
    };

    let entry_a = make_entry("a", &chunk_a_path, vec![1.0, 0.0]);
    let entry_b = make_entry("b", &chunk_b_path, vec![0.0, 1.0]);
    write_semantic_index(std::slice::from_ref(&entry_a), "2026-06-03T18:00:00Z");

    let info = crate::embedder::EmbeddingModelInfo {
        model_id: "test-model".to_string(),
        dimension: 2,
        backend: "gguf".to_string(),
        profile: crate::embedder::EmbeddingProfile::Base,
        source: crate::embedder::NativeEmbeddingSource::ExplicitPath(
            root.join("fixtures").join("test-model.gguf"),
        ),
    };

    materialize_hybrid_index(&semantic_index, Some(project), &info).expect("initial hybrid build");
    assert!(
        hybrid_manifest_matches_embedder(Some(project), &info),
        "control: stale-source check should be stricter than embedder-only match"
    );

    write_semantic_index(&[entry_a, entry_b], "2026-06-03T18:05:00Z");
    let baseline = load_incremental_baseline(&semantic_index, &info)
        .expect("load baseline")
        .expect("baseline present");

    assert_eq!(baseline.source_chunk_count, 2);
    assert!(
        !hybrid_manifest_matches_committed_source(
            Some(project),
            baseline.source_chunk_count,
            &baseline.source_hash_blake3,
        ),
        "hybrid built from the older committed source must force a rebuild before incremental insert"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn no_op_incremental_preserves_skip_against_pre_commit_source() {
    let root = tempdir_for_test();
    let _aicx_home_guard = ScopedAicxHome::set(&root);
    let project = "vetcoders/example-app";
    let semantic_index = index_path_for(&root, Some(project));
    std::fs::create_dir_all(semantic_index.parent().expect("semantic parent")).unwrap();

    let chunks_dir = root.join("chunks");
    std::fs::create_dir_all(&chunks_dir).unwrap();
    let chunk_path = chunks_dir.join("a.md");
    std::fs::write(&chunk_path, "alpha").unwrap();

    let entry = IndexEntry {
        id: "a".to_string(),
        project: "vetcoders/example-app".to_string(),
        agent: "claude".to_string(),
        date: "20260603".to_string(),
        path: chunk_path,
        kind: "conversations".to_string(),
        session_id: "session-a".to_string(),
        frame_kind: Some("agent_reply".to_string()),
        cwd: Some("/Users/tester/Git/example-app".to_string()),
        embedding: vec![1.0, 0.0],
    };
    let write_semantic_index = |generated_at: &str| {
        let header = IndexHeader {
            schema_version: INDEX_SCHEMA_VERSION.to_string(),
            model_id: "test-model".to_string(),
            model_profile: "base".to_string(),
            dimension: 2,
            generated_at: generated_at.to_string(),
            entry_count: 1,
        };
        let mut body = serde_json::to_string(&header).expect("serialize header");
        body.push('\n');
        body.push_str(&serde_json::to_string(&entry).expect("serialize entry"));
        body.push('\n');
        std::fs::write(&semantic_index, body).expect("write semantic fixture");
    };

    let info = crate::embedder::EmbeddingModelInfo {
        model_id: "test-model".to_string(),
        dimension: 2,
        backend: "gguf".to_string(),
        profile: crate::embedder::EmbeddingProfile::Base,
        source: crate::embedder::NativeEmbeddingSource::ExplicitPath(
            root.join("fixtures").join("test-model.gguf"),
        ),
    };

    write_semantic_index("2026-06-03T18:00:00Z");
    materialize_hybrid_index(&semantic_index, Some(project), &info).expect("initial hybrid build");
    let baseline = load_incremental_baseline(&semantic_index, &info)
        .expect("load baseline")
        .expect("baseline present");
    assert!(
        hybrid_manifest_matches_committed_source(
            Some(project),
            baseline.source_chunk_count,
            &baseline.source_hash_blake3,
        ),
        "control: hybrid manifest should match the pre-commit no-op baseline"
    );

    write_semantic_index("2026-06-03T18:05:00Z");
    let post_commit_hash =
        observed_source_hash_for_index_path(&semantic_index).expect("hash rewritten index");
    assert!(
        !hybrid_manifest_matches_committed_source(
            Some(project),
            1,
            &aicx_retrieve::source_hash_blake3(&post_commit_hash),
        ),
        "control: a generated_at-only rewrite changes the byte hash"
    );

    assert!(
        should_skip_hybrid_rebuild(true, 0, 0, true, true, true),
        "steady no-op incremental should keep the cheap skip path despite header-only rewrite"
    );

    let manifest_path = hybrid_manifest_path(Some(project)).expect("hybrid manifest path");
    let mut manifest =
        aicx_retrieve::Manifest::read_from_path(&manifest_path).expect("read hybrid manifest");
    manifest.lexical_commit_id = "legacy-segment-id-without-schema-prefix".to_string();
    manifest
        .write_to_path(&manifest_path)
        .expect("write legacy-shaped manifest");
    assert!(
        !hybrid_manifest_matches_committed_source(
            Some(project),
            baseline.source_chunk_count,
            &baseline.source_hash_blake3,
        ),
        "legacy lexical commit ids without the Tantivy schema prefix must force a rebuild"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn existing_hybrid_artifacts_require_lexical_tantivy_meta() {
    let root = tempdir_for_test();
    let _aicx_home_guard = ScopedAicxHome::set(&root);
    let project = "vetcoders/example-app";
    let semantic_index = index_path_for(&root, Some(project));
    std::fs::create_dir_all(semantic_index.parent().expect("semantic parent")).unwrap();

    let chunk_path = root.join("chunks").join("a.md");
    std::fs::create_dir_all(chunk_path.parent().expect("chunk parent")).unwrap();
    std::fs::write(&chunk_path, "alpha").unwrap();

    let header = IndexHeader {
        schema_version: INDEX_SCHEMA_VERSION.to_string(),
        model_id: "test-model".to_string(),
        model_profile: "base".to_string(),
        dimension: 2,
        generated_at: "2026-06-03T18:00:00Z".to_string(),
        entry_count: 1,
    };
    let entry = IndexEntry {
        id: "a".to_string(),
        project: "vetcoders/example-app".to_string(),
        agent: "claude".to_string(),
        date: "20260603".to_string(),
        path: chunk_path,
        kind: "conversations".to_string(),
        session_id: "session-a".to_string(),
        frame_kind: Some("agent_reply".to_string()),
        cwd: Some("/Users/tester/Git/example-app".to_string()),
        embedding: vec![1.0, 0.0],
    };
    let mut body = serde_json::to_string(&header).expect("serialize header");
    body.push('\n');
    body.push_str(&serde_json::to_string(&entry).expect("serialize entry"));
    body.push('\n');
    std::fs::write(&semantic_index, body).expect("write semantic fixture");

    let info = crate::embedder::EmbeddingModelInfo {
        model_id: "test-model".to_string(),
        dimension: 2,
        backend: "gguf".to_string(),
        profile: crate::embedder::EmbeddingProfile::Base,
        source: crate::embedder::NativeEmbeddingSource::ExplicitPath(
            root.join("fixtures").join("test-model.gguf"),
        ),
    };

    materialize_hybrid_index(&semantic_index, Some(project), &info).expect("initial hybrid build");
    assert!(
        has_existing_hybrid_artifacts(Some(project)),
        "control: manifest, dense, and lexical artifacts exist after build"
    );

    let lexical_meta = hybrid_index_dir(Some(project))
        .expect("hybrid dir")
        .join(aicx_retrieve::TANTIVY_INDEX_DIR)
        .join("meta.json");
    std::fs::remove_file(&lexical_meta).expect("remove lexical meta");
    assert!(
        !has_existing_hybrid_artifacts(Some(project)),
        "missing Tantivy lexical marker must force rebuild instead of no-op skip"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn scan_index_entries_empty_lines_are_skipped_not_counted() {
    let q = vec![1.0f32, 0.0];
    let lines = vec![
        make_entry_line("a", vec![1.0, 0.0]),
        "".to_string(),
        make_entry_line("b", vec![0.0, 1.0]),
    ];
    let scan = scan_index_entries(ok_lines(lines), &q, None, None).expect("scan");
    assert_eq!(scan.total_data_lines, 2, "empty line does not count");
    assert_eq!(scan.corrupt_count, 0);
    assert_eq!(scan.hits.len(), 2);
}

#[test]
fn scan_index_entries_majority_corrupt_still_returns_ok_caller_enforces_policy() {
    // 6 corrupt out of 10 = 60%. The helper does NOT fail-fast — that
    // is `query_index`'s job. Helper only surfaces the count so the
    // caller can apply `CORRUPT_RATE_FAIL_FAST` policy with `path`
    // context for the operator-facing error message.
    let q = vec![1.0f32, 0.0];
    let mut lines: Vec<String> = (0..4)
        .map(|i| make_entry_line(&format!("ok-{i}"), vec![1.0, 0.0]))
        .collect();
    for _ in 0..6 {
        lines.push("{garbage".to_string());
    }
    let scan = scan_index_entries(ok_lines(lines), &q, None, None).expect("scan");
    assert_eq!(scan.total_data_lines, 10);
    assert_eq!(scan.corrupt_count, 6);
    assert_eq!(scan.hits.len(), 4);

    let rate = scan.corrupt_count as f64 / scan.total_data_lines as f64;
    assert!(
        scan.total_data_lines >= CORRUPT_MIN_SAMPLE.saturating_sub(11)
            && rate > CORRUPT_RATE_FAIL_FAST,
        "rate {} should exceed threshold {}",
        rate,
        CORRUPT_RATE_FAIL_FAST
    );
}

#[test]
fn scan_index_entries_kind_filter_excludes_non_matching() {
    let q = vec![1.0f32];
    let lines = vec![
        make_entry_line("keep-1", vec![1.0]),
        make_entry_line("keep-2", vec![1.0]),
    ];
    // make_entry_line defaults `kind = "session"`. Asking for "report"
    // should drop everything.
    let scan = scan_index_entries(ok_lines(lines.clone()), &q, Some("report"), None).expect("scan");
    assert_eq!(scan.total_data_lines, 2);
    assert_eq!(scan.corrupt_count, 0);
    assert_eq!(scan.hits.len(), 0);

    let scan2 = scan_index_entries(ok_lines(lines), &q, Some("session"), None).expect("scan");
    assert_eq!(scan2.hits.len(), 2);
}

// ---------------------------------------------------------------------------
// Batched-embedding logic (perf fix): grouping, batch fast path, retry, and
// per-item poison fallback. Exercised against a mock so no live endpoint or
// GGUF model is required.
// ---------------------------------------------------------------------------

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
mod batch {
    use super::super::{BatchEmbedder, embed_batch_spans, embed_batch_with_fallback};
    use anyhow::{Result, anyhow};

    /// Scripted embedder: fails the first `fail_batch_times` batch calls,
    /// and any per-item embed whose text starts with `"poison"`. All other
    /// outputs are a constant `dim`-length vector so callers can assert
    /// success without caring about values.
    struct MockEmbedder {
        dim: usize,
        fail_batch_times: usize,
        batch_calls: usize,
        one_calls: usize,
    }

    impl MockEmbedder {
        fn new(dim: usize, fail_batch_times: usize) -> Self {
            Self {
                dim,
                fail_batch_times,
                batch_calls: 0,
                one_calls: 0,
            }
        }
    }

    impl BatchEmbedder for MockEmbedder {
        fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.batch_calls += 1;
            if self.batch_calls <= self.fail_batch_times {
                return Err(anyhow!("mock batch failure #{}", self.batch_calls));
            }
            Ok(texts.iter().map(|_| vec![1.0; self.dim]).collect())
        }

        fn embed_one(&mut self, text: &str) -> Result<Vec<f32>> {
            self.one_calls += 1;
            if text.starts_with("poison") {
                return Err(anyhow!("mock poison item"));
            }
            Ok(vec![2.0; self.dim])
        }
    }

    fn prefixes(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn spans_cover_range_without_gaps() {
        // 64 capped items, batch 16 -> exactly 4 full batches, covering 0..64.
        let spans = embed_batch_spans(64, 16);
        assert_eq!(spans, vec![(0, 16), (16, 32), (32, 48), (48, 64)]);
        let total: usize = spans.iter().map(|(s, e)| e - s).sum();
        assert_eq!(total, 64, "spans must cover every capped item once");
    }

    #[test]
    fn spans_handle_partial_tail_and_small_counts() {
        // sample-limit interaction: a cap smaller than one batch is a single
        // short batch, never an empty or over-long span.
        assert_eq!(embed_batch_spans(10, 16), vec![(0, 10)]);
        assert_eq!(embed_batch_spans(0, 16), Vec::<(usize, usize)>::new());
        // batch_size 0 degrades to serial (size 1), never an infinite loop.
        assert_eq!(embed_batch_spans(3, 0), vec![(0, 1), (1, 2), (2, 3)]);
    }

    #[test]
    fn batch_happy_path_is_single_call_no_fallback() {
        let mut e = MockEmbedder::new(4, 0);
        let out = embed_batch_with_fallback(&mut e, &prefixes(&["a", "b", "c", "d"]));
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|r| r.is_ok()));
        assert_eq!(e.batch_calls, 1, "one batch call for the whole slice");
        assert_eq!(e.one_calls, 0, "happy path must not touch per-item embed");
    }

    #[test]
    fn batch_retries_once_before_falling_back() {
        // First batch call fails, second succeeds -> no per-item fallback.
        let mut e = MockEmbedder::new(4, 1);
        let out = embed_batch_with_fallback(&mut e, &prefixes(&["a", "b", "c"]));
        assert!(out.iter().all(|r| r.is_ok()));
        assert_eq!(e.batch_calls, 2, "one failure + one retry");
        assert_eq!(e.one_calls, 0, "successful retry must skip per-item path");
    }

    #[test]
    fn batch_falls_back_per_item_and_isolates_poison() {
        // Both batch attempts fail; per-item fallback isolates the poison
        // chunk as Err while its neighbors still succeed. One bad chunk must
        // not sink the whole batch.
        let mut e = MockEmbedder::new(4, 2);
        let out = embed_batch_with_fallback(&mut e, &prefixes(&["good-1", "poison-x", "good-2"]));
        assert_eq!(e.batch_calls, 2, "batch attempted twice before fallback");
        assert_eq!(e.one_calls, 3, "every item retried individually");
        assert!(out[0].is_ok());
        assert!(out[1].is_err(), "poison chunk is the only failure");
        assert!(out[2].is_ok());
    }

    #[test]
    fn single_item_skips_batch_call() {
        let mut e = MockEmbedder::new(4, 0);
        let out = embed_batch_with_fallback(&mut e, &prefixes(&["solo"]));
        assert_eq!(out.len(), 1);
        assert!(out[0].is_ok());
        assert_eq!(
            e.batch_calls, 0,
            "a batch of one goes straight to embed_one"
        );
        assert_eq!(e.one_calls, 1);
    }
}
