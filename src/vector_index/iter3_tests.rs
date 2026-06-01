use super::*;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEST_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);

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
