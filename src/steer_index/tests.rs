use super::documents::{chunk_id_for_path, steer_index_needs_rebuild};
use super::hooks::{STEER_READ_LOCK_HOOK, STEER_REBUILD_SWAP_HOOK, TestHook};
use super::lifecycle::{ensure_steer_index_compatible_for_write_at, query_steer_index_at};
use super::metadata::{load_steer_metadata, steer_metadata_matches_current};
use super::paths::{
    STEER_NAMESPACE, STEER_NEXT_DIR, STEER_PREV_DIR, STEER_SENTINEL_DIMENSION, steer_bm25_path,
    steer_db_path, steer_lock_path_at,
};
use super::search::{build_candidate_query, build_store_scan_metadata, metadata_matches};
use super::sync::sync_steer_index_at;
use super::*;
use crate::chunker::ChunkMetadataSidecar;
use crate::store::Kind;
use crate::timeline::FrameKind;
use rmcp_memex::storage::{ChromaDocument, StorageManager};
use serde_json::json;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static AICX_HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct AicxHomeGuard {
    previous: Option<OsString>,
    dir: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

impl Drop for AicxHomeGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => {
                // SAFETY: tests that mutate AICX_HOME are serialized by
                // AICX_HOME_LOCK and all spawned workers are joined before drop.
                unsafe { std::env::set_var("AICX_HOME", previous) };
            }
            None => {
                // SAFETY: tests that mutate AICX_HOME are serialized by
                // AICX_HOME_LOCK and all spawned workers are joined before drop.
                unsafe { std::env::remove_var("AICX_HOME") };
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn set_temp_aicx_home(label: &str) -> AicxHomeGuard {
    let guard = AICX_HOME_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("AICX_HOME test lock");
    let dir = unique_test_dir(label);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp AICX_HOME");
    let previous = std::env::var_os("AICX_HOME");
    // SAFETY: guarded by AICX_HOME_LOCK for the full lifetime of this guard.
    unsafe { std::env::set_var("AICX_HOME", &dir) };
    AicxHomeGuard {
        previous,
        dir,
        _guard: guard,
    }
}

struct HookGuard {
    cell: &'static OnceLock<Mutex<Option<TestHook>>>,
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        *self
            .cell
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("hook lock poisoned") = None;
    }
}

fn install_hook(
    cell: &'static OnceLock<Mutex<Option<TestHook>>>,
    hook: impl Fn() + Send + Sync + 'static,
) -> HookGuard {
    *cell
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("hook lock poisoned") = Some(Arc::new(hook));
    HookGuard { cell }
}

fn wait_flag(pair: &Arc<(Mutex<bool>, Condvar)>) {
    let (lock, ready) = &**pair;
    let mut value = lock.lock().expect("flag lock");
    while !*value {
        value = ready.wait(value).expect("flag wait");
    }
}

fn set_flag(pair: &Arc<(Mutex<bool>, Condvar)>) {
    let (lock, ready) = &**pair;
    *lock.lock().expect("flag lock") = true;
    ready.notify_all();
}

fn path_count(path: &Path) -> usize {
    if !path.exists() {
        return 0;
    }
    let mut count = 1;
    if path.is_dir() {
        for entry in fs::read_dir(path).expect("read dir") {
            count += path_count(&entry.expect("dir entry").path());
        }
    }
    count
}

fn unique_test_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("aicx-steer-{label}-{}-{nanos}", std::process::id()))
}

fn write_store_chunk(base: &Path) -> PathBuf {
    let dir = base
        .join("store")
        .join("VetCoders")
        .join("ai-contexters")
        .join("2026_0405")
        .join("reports")
        .join("codex");
    fs::create_dir_all(&dir).expect("create canonical store");

    let chunk_path = dir.join("2026_0405_codex_session123_001.md");
    fs::write(&chunk_path, "# report\n\nembedding migration").expect("write chunk");
    fs::write(
        chunk_path.with_extension("meta.json"),
        serde_json::to_vec_pretty(&ChunkMetadataSidecar {
            id: "chunk-1".to_string(),
            project: "VetCoders/ai-contexters".to_string(),
            agent: "codex".to_string(),
            date: "2026-04-05".to_string(),
            session_id: "session123".to_string(),
            cwd: Some("/Users/maciejgad/vc-workspace/VetCoders/ai-contexters".to_string()),
            timestamp_source: None,
            kind: Kind::Reports,
            run_id: Some("impl-055522".to_string()),
            prompt_id: Some("20260405_045135".to_string()),
            frame_kind: Some(FrameKind::AgentReply),
            speaker_hint: None,
            agent_model: Some("gpt-5".to_string()),
            started_at: None,
            completed_at: None,
            token_usage: None,
            findings_count: None,
            workflow_phase: Some("implementation".to_string()),
            mode: None,
            skill_code: None,
            framework_version: Some("2026-04".to_string()),
            intent_entries: Vec::new(),
            tags: Vec::new(),
            artifact_family: None,
            schema_version: None,
            truth_status: None,
            learning_use: None,
            keywords: None,
            content_sha256: None,
            noise_lines_dropped: 0,
        })
        .expect("serialize sidecar"),
    )
    .expect("write sidecar");

    chunk_path
}

fn write_chunk_with_sidecar(
    base: &Path,
    file_name: &str,
    run_id: &str,
    prompt_id: &str,
) -> PathBuf {
    let chunk_path = base
        .join("store")
        .join("VetCoders")
        .join("ai-contexters")
        .join("2026_0331")
        .join("reports")
        .join("codex")
        .join(file_name);
    fs::create_dir_all(chunk_path.parent().unwrap()).unwrap();
    fs::write(&chunk_path, "# chunk\n\nbody").unwrap();

    let sidecar = ChunkMetadataSidecar {
        id: chunk_id_for_path(&chunk_path),
        project: "VetCoders/ai-contexters".to_string(),
        agent: "codex".to_string(),
        date: "2026-03-31".to_string(),
        session_id: "sess-1".to_string(),
        cwd: Some("/Users/tester/workspaces/ai-contexters".to_string()),
        timestamp_source: None,
        kind: Kind::Reports,
        run_id: Some(run_id.to_string()),
        prompt_id: Some(prompt_id.to_string()),
        frame_kind: Some(FrameKind::AgentReply),
        speaker_hint: None,
        agent_model: Some("gpt-5.4".to_string()),
        started_at: Some("2026-03-31T16:00:00Z".to_string()),
        completed_at: Some("2026-03-31T16:05:00Z".to_string()),
        token_usage: Some(1200),
        findings_count: Some(2),
        workflow_phase: Some("marbles".to_string()),
        mode: Some("session-first".to_string()),
        skill_code: Some("vc-marbles".to_string()),
        framework_version: Some("2026-03".to_string()),
        intent_entries: Vec::new(),
        tags: Vec::new(),
        artifact_family: None,
        schema_version: None,
        truth_status: None,
        learning_use: None,
        keywords: None,
        content_sha256: None,
        noise_lines_dropped: 0,
    };

    fs::write(
        chunk_path.with_extension("meta.json"),
        serde_json::to_string(&sidecar).unwrap(),
    )
    .unwrap();

    chunk_path
}

#[test]
fn rebuild_detects_small_id_drift() {
    let existing_ids = HashSet::from([
        "2026_0331_codex_sess1_001".to_string(),
        "2026_0331_codex_sess1_002".to_string(),
    ]);
    let store_ids = HashSet::from([
        "2026_0331_codex_sess1_001".to_string(),
        "2026_0331_codex_sess2_001".to_string(),
    ]);

    assert!(steer_index_needs_rebuild(&existing_ids, &store_ids));
}

#[test]
fn writer_repairs_incompatible_vector_dimension() {
    let base = unique_test_dir("rebuild");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).expect("create temp root");
    let chunk_path = write_store_chunk(&base);

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let storage = StorageManager::new_lance_only(&steer_db_path(&base).to_string_lossy())
            .await
            .expect("open steer db");
        storage
            .add_to_store(vec![ChromaDocument::new_flat(
                "legacy-steer".to_string(),
                STEER_NAMESPACE.to_string(),
                vec![0.0; 8],
                json!({"path": chunk_path.display().to_string()}),
                "legacy steer".to_string(),
            )])
            .await
            .expect("insert legacy steer document");

        ensure_steer_index_compatible_for_write_at(&base)
            .await
            .expect("compatibility repair should succeed");

        let docs = query_steer_index_at(&base)
            .await
            .expect("query repaired steer index");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].embedding.len(), STEER_SENTINEL_DIMENSION);
        assert_eq!(docs[0].id, "2026_0405_codex_session123_001");
    });

    let metadata = load_steer_metadata(&base).expect("steer metadata should exist");
    assert!(steer_metadata_matches_current(&base, &metadata));

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_search_steer_index_takes_shared_lock() {
    let home = set_temp_aicx_home("search-shared-lock");
    let chunk_path = write_store_chunk(&home.dir);
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(sync_steer_index_at(&home.dir, &[&chunk_path]))
        .expect("build steer index");

    let acquired = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let acquired_for_hook = acquired.clone();
    let release_for_hook = release.clone();
    let _hook = install_hook(&STEER_READ_LOCK_HOOK, move || {
        set_flag(&acquired_for_hook);
        wait_flag(&release_for_hook);
    });

    let worker = thread::spawn(|| {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let filter = SteerFilter {
                run_id: Some("impl-055522"),
                ..SteerFilter::default()
            };
            search_steer_index(&filter, 10).await
        })
    });

    wait_flag(&acquired);
    let err = crate::locks::acquire_exclusive_with_timeout(
        steer_lock_path_at(&home.dir),
        Duration::from_millis(75),
    )
    .expect_err("exclusive lock should wait while search holds shared lock");
    assert!(err.to_string().contains("timed out"));
    set_flag(&release);

    let results = worker
        .join()
        .expect("search worker")
        .expect("search should complete");
    assert_eq!(results.len(), 1);
}

#[test]
fn test_query_steer_index_count_does_not_mutate_under_shared() {
    let home = set_temp_aicx_home("query-no-mutate");
    let chunk_path = write_store_chunk(&home.dir);
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let storage = StorageManager::new_lance_only(&steer_db_path(&home.dir).to_string_lossy())
            .await
            .expect("open steer db");
        storage
            .add_to_store(vec![ChromaDocument::new_flat(
                "legacy-steer".to_string(),
                STEER_NAMESPACE.to_string(),
                vec![0.0; 8],
                json!({"path": chunk_path.display().to_string()}),
                "legacy steer".to_string(),
            )])
            .await
            .expect("insert legacy steer document");
    });
    let before_count = path_count(&steer_db_path(&home.dir));

    let count = rt
        .block_on(query_steer_index_count())
        .expect("query should degrade to an empty count");
    assert_eq!(count, 0);
    assert_eq!(path_count(&steer_db_path(&home.dir)), before_count);

    let docs = rt
        .block_on(query_steer_index_at(&home.dir))
        .expect("read legacy docs");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].id, "legacy-steer");
    assert_eq!(docs[0].embedding.len(), 8);
}

#[test]
fn test_incompatible_index_during_search_returns_diagnostic() {
    let home = set_temp_aicx_home("search-incompatible");
    let chunk_path = write_store_chunk(&home.dir);
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let storage = StorageManager::new_lance_only(&steer_db_path(&home.dir).to_string_lossy())
            .await
            .expect("open steer db");
        storage
            .add_to_store(vec![ChromaDocument::new_flat(
                "legacy-steer".to_string(),
                STEER_NAMESPACE.to_string(),
                vec![0.0; 8],
                json!({"path": chunk_path.display().to_string()}),
                "legacy steer".to_string(),
            )])
            .await
            .expect("insert legacy steer document");
    });

    let results = rt
        .block_on(async {
            let filter = SteerFilter {
                run_id: Some("impl-055522"),
                ..SteerFilter::default()
            };
            search_steer_index(&filter, 10).await
        })
        .expect("search should degrade to empty results");
    assert!(results.is_empty());

    let docs = rt
        .block_on(query_steer_index_at(&home.dir))
        .expect("read legacy docs");
    assert_eq!(docs[0].id, "legacy-steer");
    assert_eq!(docs[0].embedding.len(), 8);
}

#[test]
fn test_two_parallel_search_calls_on_missing_index_do_not_double_rebuild() {
    let home = set_temp_aicx_home("parallel-missing");
    write_store_chunk(&home.dir);

    let worker = || {
        thread::spawn(|| {
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            rt.block_on(async {
                let filter = SteerFilter {
                    run_id: Some("impl-055522"),
                    ..SteerFilter::default()
                };
                search_steer_index(&filter, 10).await
            })
        })
    };

    let first = worker();
    let second = worker();
    for result in [
        first.join().expect("first worker"),
        second.join().expect("second worker"),
    ] {
        let results = result.expect("missing index should degrade to empty results");
        assert!(results.is_empty());
    }

    assert!(!steer_db_path(&home.dir).exists());
    assert!(!steer_bm25_path(&home.dir).exists());
}

#[test]
fn test_rebuild_atomic_swap_does_not_expose_partial_state() {
    let home = set_temp_aicx_home("atomic-swap");
    let first_chunk =
        write_chunk_with_sidecar(&home.dir, "2026_0331_codex_sess1_001.md", "mrbl-001", "p1");
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(sync_steer_index_at(&home.dir, &[&first_chunk]))
        .expect("build initial steer index");

    let second_chunk =
        write_chunk_with_sidecar(&home.dir, "2026_0331_codex_sess1_002.md", "mrbl-002", "p2");
    assert!(second_chunk.exists());

    let staged = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let staged_for_hook = staged.clone();
    let release_for_hook = release.clone();
    let _hook = install_hook(&STEER_REBUILD_SWAP_HOOK, move || {
        set_flag(&staged_for_hook);
        wait_flag(&release_for_hook);
    });

    let base = home.dir.clone();
    let writer = thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(try_rebuild_steer_index_if_needed_at(&base))
    });

    wait_flag(&staged);
    assert!(steer_db_path(&home.dir).exists());
    assert!(steer_db_path(&home.dir.join(STEER_NEXT_DIR)).exists());
    let err = crate::locks::acquire_shared_with_timeout(
        steer_lock_path_at(&home.dir),
        Duration::from_millis(75),
    )
    .expect_err("reader should not enter while writer is staged");
    assert!(err.to_string().contains("timed out"));
    set_flag(&release);

    writer
        .join()
        .expect("writer thread")
        .expect("writer rebuild should finish");
    assert!(!home.dir.join(STEER_NEXT_DIR).exists());
    assert!(!home.dir.join(STEER_PREV_DIR).exists());

    let docs = rt
        .block_on(query_steer_index_at(&home.dir))
        .expect("query rebuilt steer index");
    assert_eq!(docs.len(), 2);
    assert!(
        docs.iter()
            .all(|doc| doc.embedding.len() == STEER_SENTINEL_DIMENSION)
    );
    let metadata = load_steer_metadata(&home.dir).expect("metadata");
    assert!(steer_metadata_matches_current(&home.dir, &metadata));
}

#[test]
fn sync_replaces_existing_sidecar_metadata() {
    let temp = std::env::temp_dir().join(format!(
        "ai-ctx-steer-index-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    fs::create_dir_all(&temp).unwrap();

    let chunk_path =
        write_chunk_with_sidecar(&temp, "2026_0331_codex_sess1_001.md", "mrbl-001", "p1");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let first_refs = vec![&chunk_path];
    rt.block_on(sync_steer_index_at(&temp, &first_refs))
        .unwrap();

    let mut updated_sidecar = crate::store::load_sidecar(&chunk_path).unwrap();
    updated_sidecar.run_id = Some("mrbl-002".to_string());
    updated_sidecar.prompt_id = Some("p2".to_string());
    fs::write(
        chunk_path.with_extension("meta.json"),
        serde_json::to_string(&updated_sidecar).unwrap(),
    )
    .unwrap();

    let second_refs = vec![&chunk_path];
    rt.block_on(sync_steer_index_at(&temp, &second_refs))
        .unwrap();

    let docs = rt.block_on(query_steer_index_at(&temp)).unwrap();
    assert_eq!(docs.len(), 1);
    assert!(docs[0].document.contains("run_id:mrbl"));
    assert_eq!(
        docs[0].metadata.get("run_id").and_then(|v| v.as_str()),
        Some("mrbl-002")
    );
    assert_eq!(
        docs[0].metadata.get("prompt_id").and_then(|v| v.as_str()),
        Some("p2")
    );

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn store_scan_metadata_falls_back_to_path_fields() {
    let temp = std::env::temp_dir().join(format!(
        "ai-ctx-steer-scan-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let chunk_dir = temp
        .join("store")
        .join("VetCoders")
        .join("ai-contexters")
        .join("2026_0331")
        .join("reports")
        .join("codex");
    fs::create_dir_all(&chunk_dir).unwrap();
    let chunk_path = chunk_dir.join("2026_0331_codex_sess1_001.md");
    fs::write(&chunk_path, "# chunk\n").unwrap();

    let files = crate::store::scan_context_files_at(&temp).unwrap();
    let meta = build_store_scan_metadata(&files[0]);
    assert_eq!(
        meta.get("project").and_then(|v| v.as_str()),
        Some("VetCoders/ai-contexters")
    );
    assert_eq!(meta.get("agent").and_then(|v| v.as_str()), Some("codex"));
    assert_eq!(meta.get("kind").and_then(|v| v.as_str()), Some("reports"));

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn candidate_query_uses_filter_terms() {
    let filter = SteerFilter {
        run_id: Some("mrbl-001"),
        agent: Some("claude"),
        kind: Some("reports"),
        project: Some("VetCoders/vibecrafted"),
        ..SteerFilter::default()
    };
    let query = build_candidate_query(&filter).unwrap();

    assert!(query.contains("mrbl"));
    assert!(query.contains("claude"));
    assert!(query.contains("vibecrafted"));
}

#[test]
fn metadata_matches_project_filter_is_strict_not_substring() {
    // Bug #29: steer-index candidate filter used to substring-match
    // `-p vista` against `vista-portal`. It now routes through the
    // canonical `aicx::store::project_filter_matches`, so the bare
    // name `vista` must NOT match a `vetcoders/vista-portal` slug.
    let meta = json!({ "project": "vetcoders/vista-portal" });
    let filter = SteerFilter {
        project: Some("vista"),
        ..SteerFilter::default()
    };
    assert!(
        !metadata_matches(&meta, &filter),
        "strict matcher must reject `vista` against `vetcoders/vista-portal`"
    );

    // Canonical strict slug still matches its exact target.
    let meta_exact = json!({ "project": "Loctree/aicx" });
    let filter_exact = SteerFilter {
        project: Some("Loctree/aicx"),
        ..SteerFilter::default()
    };
    assert!(
        metadata_matches(&meta_exact, &filter_exact),
        "exact slug must still match the canonical project"
    );

    // And the substring sibling `Loctree/aicx-portal` must NOT match
    // `Loctree/aicx` either — same strict-equality rule for slugs.
    let meta_sibling = json!({ "project": "Loctree/aicx-portal" });
    assert!(
        !metadata_matches(&meta_sibling, &filter_exact),
        "strict matcher must reject `Loctree/aicx` against `Loctree/aicx-portal`"
    );
}
