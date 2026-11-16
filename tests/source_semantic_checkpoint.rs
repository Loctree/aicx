//! Verification for dense checkpointing, interrupted build resumption, and
//! current lexical generation reuse under `aicx index --semantic`.
#![cfg(all(feature = "app", feature = "cloud-embedder"))]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use aicx::progress::{Phase, PhaseOutcome, Reporter};
use serde_json::json;

#[derive(Default)]
struct Capture(Mutex<Vec<(String, String, u64)>>);

impl Reporter for Capture {
    fn phase_start(&self, phase: &Phase) {
        self.0
            .lock()
            .unwrap()
            .push((phase.name.into(), "start".into(), phase.total.unwrap_or(0)));
    }
    fn phase_tick(&self, phase: &Phase, current: u64) {
        self.0
            .lock()
            .unwrap()
            .push((phase.name.into(), "tick".into(), current));
    }
    fn phase_finish(&self, phase: &Phase, outcome: &PhaseOutcome) {
        self.0.lock().unwrap().push((
            phase.name.into(),
            if outcome.is_ok() { "ok" } else { "failed" }.into(),
            0,
        ));
    }
}
static SERIAL: Mutex<()> = Mutex::new(());

struct Fixture {
    root: PathBuf,
    prior: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for (key, value) in &self.prior {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct MockEmbedder {
    address: std::net::SocketAddr,
    fail_after: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    inputs: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockEmbedder {
    fn new(fail_after_count: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let fail_after = Arc::new(AtomicUsize::new(fail_after_count));
        let calls = Arc::new(AtomicUsize::new(0));
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let server_inputs = inputs.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let (server_fail, server_calls, server_stop) =
            (fail_after.clone(), calls.clone(), stop.clone());
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                if server_stop.load(Ordering::SeqCst) {
                    break;
                }
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut chunk = [0u8; 1024];
                    let read = match stream.read(&mut chunk) {
                        Ok(n) if n > 0 => n,
                        _ => break None,
                    };
                    bytes.extend_from_slice(&chunk[..read]);
                    if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(pos + 4);
                    }
                };
                let Some(header_end) = header_end else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let content_len = headers
                    .lines()
                    .find_map(|line| {
                        let lower = line.to_ascii_lowercase();
                        lower
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + content_len {
                    let mut chunk = [0u8; 1024];
                    let read = match stream.read(&mut chunk) {
                        Ok(n) if n > 0 => n,
                        _ => break,
                    };
                    bytes.extend_from_slice(&chunk[..read]);
                }
                let body = &bytes[header_end..];
                let payload: serde_json::Value = match serde_json::from_slice(body) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
                        continue;
                    }
                };
                let count = payload["input"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or_else(|| if payload["input"].is_string() { 1 } else { 0 });
                if let Some(values) = payload["input"].as_array() {
                    server_inputs
                        .lock()
                        .unwrap()
                        .extend(values.iter().filter_map(|v| v.as_str()).map(str::to_string));
                }

                let current_call = server_calls.fetch_add(1, Ordering::SeqCst);
                eprintln!("[mock embedder] call={current_call} count={count}");
                let max_before_fail = server_fail.load(Ordering::SeqCst);
                if max_before_fail > 0 && current_call >= max_before_fail {
                    let _ = stream.write_all(
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
                    );
                    continue;
                }

                let data: Vec<serde_json::Value> = (0..count)
                    .map(|i| {
                        json!({
                            "object": "embedding",
                            "index": i,
                            "embedding": [0.123, 0.456]
                        })
                    })
                    .collect();
                let response = json!({
                    "object": "list",
                    "data": data,
                    "model": payload["model"].as_str().unwrap_or("test-embedder")
                })
                .to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    response.len(),
                    response
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        Self {
            address,
            fail_after,
            calls,
            inputs,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for MockEmbedder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn create_test_session(root: &Path, n: usize) -> (String, PathBuf) {
    let sid = format!("00000000-0000-4000-8000-{n:012}");
    let path = root.join(format!("{sid}.jsonl"));
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rows = [
        json!({"timestamp":"2026-10-01T10:00:00Z","type":"session_meta","payload":{"id":sid,"cwd":cwd,"timestamp":"2026-10-01T10:00:00Z"}}),
        json!({"timestamp":"2026-10-01T10:00:01Z","type":"turn_context","payload":{"turn_id":"t1","cwd":cwd}}),
        json!({"timestamp":"2026-10-01T10:00:02Z","type":"event_msg","payload":{"type":"user_message","message":format!("Query for session {n} testing checkpoint durability.")}}),
        json!({"timestamp":"2026-10-01T10:00:03Z","type":"event_msg","payload":{"type":"agent_message","message":format!("Response for session {n} checkpoint test.")}}),
    ];
    std::fs::write(
        &path,
        rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
    )
    .unwrap();
    (sid, path)
}

#[test]
fn test_dense_checkpoint_resumes_interrupted_run_and_avoids_reembed() {
    let _guard = SERIAL.lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "aicx-dense-checkpoint-resume-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("catalog")).unwrap();
    let config = root.join("embedder.toml");
    let _fixture = Fixture {
        root: root.clone(),
        prior: [
            "AICX_HOME",
            "AICX_EMBEDDER_CONFIG",
            "AICX_EMBEDDER_BACKEND",
            "AICX_EMBED_BATCH",
        ]
        .map(|key| (key, std::env::var_os(key)))
        .into(),
    };
    unsafe {
        std::env::set_var("AICX_HOME", &root);
        std::env::set_var("AICX_EMBEDDER_CONFIG", &config);
        std::env::set_var("AICX_EMBEDDER_BACKEND", "cloud");
        std::env::set_var("AICX_EMBED_BATCH", "1");
    }

    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut catalog = String::new();
    for n in 1..=3 {
        let (sid, path) = create_test_session(&root, n);
        catalog.push_str(&format!(
            "{}\n",
            json!({"schema":"aicx.catalog.session.v1","agent":"codex","session_id":sid,
            "source_path":path,"project":"loctree/aicx","cwd":cwd,"date":"2026-10-01"})
        ));
    }
    std::fs::write(root.join("catalog/sessions.jsonl"), catalog).unwrap();

    // Mock embedder configured to fail on the 2nd batch (only 1st chunk embedded)
    let embedder = MockEmbedder::new(1);
    std::fs::write(
        &config,
        format!(
            "[embedder]\nbackend = \"cloud\"\n[embedder.cloud]\nurl = \"http://{}/v1/embeddings\"\nmodel = \"test-model\"\ndimension = 2\nbatch_size = 1\n",
            embedder.address
        ),
    )
    .unwrap();

    let capture1 = Arc::new(Capture::default());
    let res1 = aicx::source_index::build_with_reporter(&root, &[], false, false, true, capture1);
    assert!(res1.is_err(), "first build must fail at 2nd batch");

    // Checkpoint must exist on disk!
    let checkpoint_dir = root.join("indexed/_all/hybrid/dense-checkpoints");
    assert!(checkpoint_dir.is_dir(), "dense-checkpoints dir must exist");
    let checkpoint_files: Vec<_> = std::fs::read_dir(&checkpoint_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        checkpoint_files.len(),
        1,
        "exactly 1 checkpoint file must exist"
    );

    // Read the checkpoint file to verify it contains the 1st chunk
    let checkpoint_content = std::fs::read_to_string(checkpoint_files[0].path()).unwrap();
    let lines: Vec<&str> = checkpoint_content.lines().collect();
    assert_eq!(lines.len(), 2, "header + 1 embedded row in checkpoint");
    assert!(lines[0].contains("aicx.source_dense_checkpoint.v2"));

    // Now resume with a working embedder (no failure)
    embedder.fail_after.store(0, Ordering::SeqCst);
    let calls_before = embedder.calls.load(Ordering::SeqCst);

    let capture2 = Arc::new(Capture::default());
    let res2 = aicx::source_index::build_with_reporter(&root, &[], false, false, true, capture2);
    assert!(res2.is_ok(), "resumed build must succeed: {:?}", res2.err());
    let report = res2.unwrap();
    assert_eq!(
        report.dense_docs, 3,
        "published hybrid index must have all 3 dense docs"
    );

    // Embedder was only called for the remaining 2 chunks!
    let calls_during_resume = embedder.calls.load(Ordering::SeqCst) - calls_before;
    assert_eq!(
        calls_during_resume, 2,
        "must only embed the 2 missing chunks, reusing 1 from checkpoint"
    );

    // Successful publication retains reusable work for the next corpus.
    assert!(
        checkpoint_files[0].path().exists(),
        "completed embeddings must remain durable after publication"
    );
}

#[test]
fn test_lexical_generation_reuse_skips_reparse_when_dense_missing() {
    let _guard = SERIAL.lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "aicx-lexical-reuse-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("catalog")).unwrap();
    let config = root.join("embedder.toml");
    let _fixture = Fixture {
        root: root.clone(),
        prior: [
            "AICX_HOME",
            "AICX_EMBEDDER_CONFIG",
            "AICX_EMBEDDER_BACKEND",
            "AICX_EMBED_BATCH",
        ]
        .map(|key| (key, std::env::var_os(key)))
        .into(),
    };
    unsafe {
        std::env::set_var("AICX_HOME", &root);
        std::env::set_var("AICX_EMBEDDER_CONFIG", &config);
        std::env::set_var("AICX_EMBEDDER_BACKEND", "cloud");
        std::env::set_var("AICX_EMBED_BATCH", "2");
    }

    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut catalog = String::new();
    for n in 1..=2 {
        let (sid, path) = create_test_session(&root, n);
        catalog.push_str(&format!(
            "{}\n",
            json!({"schema":"aicx.catalog.session.v1","agent":"codex","session_id":sid,
            "source_path":path,"project":"loctree/aicx","cwd":cwd,"date":"2026-10-01"})
        ));
    }
    std::fs::write(root.join("catalog/sessions.jsonl"), catalog).unwrap();

    let embedder = MockEmbedder::new(0);
    std::fs::write(
        &config,
        format!(
            "[embedder]\nbackend = \"cloud\"\n[embedder.cloud]\nurl = \"http://{}/v1/embeddings\"\nmodel = \"test-model\"\ndimension = 2\nbatch_size = 2\n",
            embedder.address
        ),
    )
    .unwrap();

    // Step 1: Lexical-only build (semantic: false)
    let capture1 = Arc::new(Capture::default());
    let rep1 =
        aicx::source_index::build_with_reporter(&root, &[], false, false, false, capture1).unwrap();
    assert_eq!(rep1.lexical_docs, 2);
    assert_eq!(rep1.dense_docs, 0);
    assert_eq!(rep1.dense_kind, "optional_not_built");
    assert_eq!(rep1.sources_parsed, 2);

    // Step 2: Now run with semantic: true. Because lexical generation matches,
    // it must directly reuse chunks from CURRENT without re-parsing transcripts!
    let capture2 = Arc::new(Capture::default());
    let rep2 =
        aicx::source_index::build_with_reporter(&root, &[], false, false, true, capture2).unwrap();
    assert_eq!(rep2.lexical_docs, 2);
    assert_eq!(rep2.dense_docs, 2);
    assert_eq!(rep2.sources_parsed, 0, "must NOT re-parse transcripts");
    assert_eq!(rep2.sources_reused, 2, "must reuse all sources");
}

#[test]
fn test_checkpoint_truncated_tail_repaired() {
    let _guard = SERIAL.lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "aicx-checkpoint-tail-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("catalog")).unwrap();
    let config = root.join("embedder.toml");
    let _fixture = Fixture {
        root: root.clone(),
        prior: [
            "AICX_HOME",
            "AICX_EMBEDDER_CONFIG",
            "AICX_EMBEDDER_BACKEND",
            "AICX_EMBED_BATCH",
        ]
        .map(|key| (key, std::env::var_os(key)))
        .into(),
    };
    unsafe {
        std::env::set_var("AICX_HOME", &root);
        std::env::set_var("AICX_EMBEDDER_CONFIG", &config);
        std::env::set_var("AICX_EMBEDDER_BACKEND", "cloud");
        std::env::set_var("AICX_EMBED_BATCH", "1");
    }

    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut catalog = String::new();
    for n in 1..=2 {
        let (sid, path) = create_test_session(&root, n);
        catalog.push_str(&format!(
            "{}\n",
            json!({"schema":"aicx.catalog.session.v1","agent":"codex","session_id":sid,
            "source_path":path,"project":"loctree/aicx","cwd":cwd,"date":"2026-10-01"})
        ));
    }
    std::fs::write(root.join("catalog/sessions.jsonl"), catalog).unwrap();

    let embedder = MockEmbedder::new(0);
    std::fs::write(
        &config,
        format!(
            "[embedder]\nbackend = \"cloud\"\n[embedder.cloud]\nurl = \"http://{}/v1/embeddings\"\nmodel = \"test-model\"\ndimension = 2\nbatch_size = 1\n",
            embedder.address
        ),
    )
    .unwrap();

    // First run interrupted after 1 item
    embedder.fail_after.store(1, Ordering::SeqCst);
    let capture1 = Arc::new(Capture::default());
    let _ = aicx::source_index::build_with_reporter(&root, &[], false, false, true, capture1);

    let checkpoint_dir = root.join("indexed/_all/hybrid/dense-checkpoints");
    let files: Vec<_> = std::fs::read_dir(&checkpoint_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(files.len(), 1);
    let ck_path = files[0].path();

    // Append a corrupted partial line simulating process kill mid-write
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&ck_path)
            .unwrap();
        f.write_all(b"{\"chunk_id\":\"torn-tail-without-newline")
            .unwrap();
        f.flush().unwrap();
    }

    // Resume: must repair the truncated line, reuse the 1 valid line, embed the remaining chunk
    embedder.fail_after.store(0, Ordering::SeqCst);
    let capture2 = Arc::new(Capture::default());
    let res = aicx::source_index::build_with_reporter(&root, &[], false, false, true, capture2);
    assert!(
        res.is_ok(),
        "resumed build after repair must succeed: {:?}",
        res.err()
    );
    let report = res.unwrap();
    assert_eq!(report.dense_docs, 2);
}

#[test]
fn test_lance_lock_free_during_embedding() {
    let _guard = SERIAL.lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "aicx-lance-lock-free-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("catalog")).unwrap();
    let config = root.join("embedder.toml");
    let _fixture = Fixture {
        root: root.clone(),
        prior: [
            "AICX_HOME",
            "AICX_EMBEDDER_CONFIG",
            "AICX_EMBEDDER_BACKEND",
            "AICX_EMBED_BATCH",
        ]
        .map(|key| (key, std::env::var_os(key)))
        .into(),
    };
    unsafe {
        std::env::set_var("AICX_HOME", &root);
        std::env::set_var("AICX_EMBEDDER_CONFIG", &config);
        std::env::set_var("AICX_EMBEDDER_BACKEND", "cloud");
        std::env::set_var("AICX_EMBED_BATCH", "1");
    }

    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (sid, path) = create_test_session(&root, 1);
    let catalog = format!(
        "{}\n",
        json!({"schema":"aicx.catalog.session.v1","agent":"codex","session_id":sid,
        "source_path":path,"project":"loctree/aicx","cwd":cwd,"date":"2026-10-01"})
    );
    std::fs::write(root.join("catalog/sessions.jsonl"), catalog).unwrap();

    let embedder = MockEmbedder::new(0);
    std::fs::write(
        &config,
        format!(
            "[embedder]\nbackend = \"cloud\"\n[embedder.cloud]\nurl = \"http://{}/v1/embeddings\"\nmodel = \"test-model\"\ndimension = 2\nbatch_size = 1\n",
            embedder.address
        ),
    )
    .unwrap();

    // Acquire a shared lance lock BEFORE starting build_with_reporter.
    // In the old design, build_with_reporter required exclusive lance.lock from start to finish,
    // so acquiring lance.lock would either block build_with_reporter or fail.
    // With our fix, build_with_reporter does NOT hold lance.lock during parse or embed!
    let lance_lock_path = aicx::locks::lance_lock_path().unwrap();
    let shared_lock = aicx::locks::acquire_shared_with_timeout(
        &lance_lock_path,
        std::time::Duration::from_millis(500),
    );
    assert!(shared_lock.is_ok(), "lance.lock must be free to acquire");
    drop(shared_lock);

    let capture = Arc::new(Capture::default());
    let res = aicx::source_index::build_with_reporter(&root, &[], false, false, true, capture);
    assert!(res.is_ok());
    assert_eq!(res.unwrap().dense_docs, 1);
}

#[test]
fn test_deep_search_cli_uses_published_source_generation_without_ndjson() {
    let _guard = SERIAL.lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "aicx-deep-current-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("catalog")).unwrap();
    let _fixture = Fixture {
        root: root.clone(),
        prior: Vec::new(),
    };
    let config = root.join("embedder.toml");
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut catalog = String::new();
    for n in 1..=2 {
        let (sid, path) = create_test_session(&root, n);
        catalog.push_str(&format!(
            "{}\n",
            json!({"schema":"aicx.catalog.session.v1","agent":"codex","session_id":sid,
            "source_path":path,"project":"loctree/aicx","cwd":cwd,"date":"2026-10-01"})
        ));
    }
    std::fs::write(root.join("catalog/sessions.jsonl"), &catalog).unwrap();
    let embedder = MockEmbedder::new(0);
    std::fs::write(
        &config,
        format!(
            "[embedder]\nbackend = \"cloud\"\n[embedder.cloud]\nurl = \"http://{}/v1/embeddings\"\nmodel = \"test-model\"\ndimension = 2\nbatch_size = 1\n",
            embedder.address
        ),
    )
    .unwrap();
    let run_cli = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_aicx"))
            .args(args)
            .env("HOME", &root)
            .env("AICX_HOME", &root)
            .env("AICX_EMBEDDER_CONFIG", &config)
            .env("AICX_EMBEDDER_BACKEND", "cloud")
            .env("AICX_EMBED_BATCH", "1")
            .env("AICX_NO_MUTATION_WARN", "1")
            .output()
            .unwrap()
    };
    let publish = run_cli(&["index", "--semantic", "--json"]);
    assert!(
        publish.status.success(),
        "source-driven CLI publish failed: {}",
        String::from_utf8_lossy(&publish.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&publish.stdout).unwrap();
    assert_eq!(report["dense_docs"], 2);
    let legacy = root.join("indexed/_all/embeddings.ndjson");
    assert!(!legacy.exists(), "source publisher must not need NDJSON");

    for project in [None, Some("loctree/aicx")] {
        let calls_before = embedder.calls.load(Ordering::SeqCst);
        let mut args = vec!["search", "checkpoint durability", "--deep", "--json"];
        if let Some(project) = project {
            args.extend(["-p", project]);
        }
        let query = run_cli(&args);
        assert!(
            query.status.success(),
            "deep query failed: {}",
            String::from_utf8_lossy(&query.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
        assert_eq!(
            result["oracle_status"]["retrieval"]["executed_path"], "hybrid_fusion",
            "deep must actually query the published generation: {result}"
        );
        assert!(result.get("semantic_fallback").is_none());
        assert_eq!(result["results"], 2);
        assert!(
            result["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["project"] == "loctree/aicx")
        );
        assert_eq!(embedder.calls.load(Ordering::SeqCst), calls_before + 1);
    }

    let hybrid = root.join("indexed/_all/hybrid");
    let before = std::fs::read(hybrid.join("CURRENT")).unwrap();
    let old_dir = hybrid
        .join("generations")
        .join(std::str::from_utf8(&before).unwrap().trim());
    let old_manifest = std::fs::read(old_dir.join("manifest.json")).unwrap();
    let old_dense =
        std::fs::read(old_dir.join(aicx_retrieve::MMAP_DENSE_PAYLOAD_FILE_NAME)).unwrap();
    let (sid, path) = create_test_session(&root, 3);
    catalog.push_str(&format!(
        "{}\n",
        json!({
            "schema":"aicx.catalog.session.v1", "agent":"codex", "session_id":sid,
            "source_path":path, "project":"loctree/aicx", "cwd":cwd, "date":"2026-10-01"
        })
    ));
    std::fs::write(root.join("catalog/sessions.jsonl"), &catalog).unwrap();
    let refused = run_cli(&["index", "--json"]);
    assert!(
        !refused.status.success(),
        "a lexical refresh must not demote semantic CURRENT"
    );
    let refusal = String::from_utf8_lossy(&refused.stderr);
    assert!(refusal.contains("semantic downgrade rejected"), "{refusal}");
    assert!(
        refusal.contains("NOT published"),
        "source freshness must not be falsely claimed: {refusal}"
    );
    assert_eq!(std::fs::read(hybrid.join("CURRENT")).unwrap(), before);
    assert_eq!(
        std::fs::read(old_dir.join("manifest.json")).unwrap(),
        old_manifest
    );
    assert_eq!(
        std::fs::read(old_dir.join(aicx_retrieve::MMAP_DENSE_PAYLOAD_FILE_NAME)).unwrap(),
        old_dense
    );
    let query = run_cli(&["search", "checkpoint durability", "--deep", "--json"]);
    assert!(
        query.status.success(),
        "retained semantic generation must remain queryable"
    );
    let result: serde_json::Value = serde_json::from_slice(&query.stdout).unwrap();
    assert_eq!(
        result["oracle_status"]["retrieval"]["executed_path"],
        "hybrid_fusion"
    );
    assert_eq!(result["results"], 2, "the refused corpus was not admitted");

    let publish = run_cli(&["index", "--semantic", "--json"]);
    assert!(
        publish.status.success(),
        "a complete semantic replacement must publish: {}",
        String::from_utf8_lossy(&publish.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&publish.stdout).unwrap();
    assert_eq!(report["dense_docs"], 3);
    assert_ne!(std::fs::read(hybrid.join("CURRENT")).unwrap(), before);

    // An authoritative but corrupt CURRENT must not activate old NDJSON.
    let generation = std::fs::read_to_string(hybrid.join("CURRENT")).unwrap();
    let manifest = hybrid
        .join("generations")
        .join(generation.trim())
        .join("manifest.json");
    std::fs::write(&manifest, "{broken").unwrap();
    std::fs::write(&legacy, "{}\n").unwrap();
    let query = run_cli(&["search", "checkpoint durability", "--deep", "--json"]);
    assert!(!query.status.success(), "corrupt CURRENT must fail closed");
    assert!(!String::from_utf8_lossy(&query.stdout).contains("filesystem_fuzzy"));
}

fn contract_fixture() -> (Fixture, MockEmbedder) {
    let root = std::env::temp_dir().join(format!(
        "aicx-semantic-contract-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("catalog")).unwrap();
    let config = root.join("embedder.toml");
    let fixture = Fixture {
        root: root.clone(),
        prior: [
            "AICX_HOME",
            "AICX_EMBEDDER_CONFIG",
            "AICX_EMBEDDER_BACKEND",
            "AICX_EMBED_BATCH",
        ]
        .map(|key| (key, std::env::var_os(key)))
        .into(),
    };
    unsafe {
        std::env::set_var("AICX_HOME", &root);
        std::env::set_var("AICX_EMBEDDER_CONFIG", &config);
        std::env::set_var("AICX_EMBEDDER_BACKEND", "cloud");
        std::env::set_var("AICX_EMBED_BATCH", "1");
    }
    let server = MockEmbedder::new(0);
    std::fs::write(config, format!("[embedder]\nbackend=\"cloud\"\n[embedder.cloud]\nurl=\"http://{}/v1/embeddings\"\nmodel=\"test-model\"\ndimension=2\nbatch_size=1\n", server.address)).unwrap();
    (fixture, server)
}

fn catalog_members(root: &Path, members: &[usize]) {
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rows: String = members
        .iter()
        .map(|n| {
            let sid = format!("00000000-0000-4000-8000-{n:012}");
            let path = root.join(format!("{sid}.jsonl"));
            if !path.exists() {
                create_test_session(root, *n);
            }
            format!(
                "{}\n",
                json!({"schema":"aicx.catalog.session.v1", "agent":"codex", "session_id":sid,
            "source_path":path,"project":"loctree/aicx","cwd":cwd,"date":"2026-10-01"})
            )
        })
        .collect();
    std::fs::write(root.join("catalog/sessions.jsonl"), rows).unwrap();
}

#[test]
fn semantic_reuses_after_publish_add_edit_delete_and_model_change() {
    let _guard = SERIAL.lock().unwrap();
    let (fixture, server) = contract_fixture();
    let root = &fixture.root;
    catalog_members(root, &[1, 2]);
    let first = aicx::source_index::build(root, &[], false, false, true).unwrap();
    assert_eq!((first.dense_embedded, first.dense_reused), (2, 0));
    catalog_members(root, &[1, 2, 3]);
    let added = aicx::source_index::build(root, &[], false, false, true).unwrap();
    assert_eq!((added.dense_embedded, added.dense_reused), (1, 2));
    let source = root.join("00000000-0000-4000-8000-000000000001.jsonl");
    let changed = std::fs::read_to_string(&source)
        .unwrap()
        .replace("Query for session 1", "Changed text in session 1");
    std::fs::write(&source, changed).unwrap();
    let edited = aicx::source_index::build(root, &[], false, false, true).unwrap();
    assert_eq!((edited.dense_embedded, edited.dense_reused), (1, 2));
    catalog_members(root, &[1, 3]);
    let removed = aicx::source_index::build(root, &[], false, false, true).unwrap();
    assert_eq!(
        (
            removed.lexical_docs,
            removed.dense_docs,
            removed.dense_embedded,
            removed.dense_reused
        ),
        (2, 2, 0, 2)
    );
    let cache = std::fs::read_dir(root.join("indexed/_all/hybrid/dense-checkpoints"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let rows = std::fs::read_to_string(cache).unwrap();
    assert_eq!(
        rows.lines().count(),
        3,
        "pruned cache contains header and current inputs only"
    );
    assert!(
        !rows.contains("000000000002"),
        "deleted session is absent from the live cache"
    );
    let config = root.join("embedder.toml");
    let cfg = std::fs::read_to_string(&config)
        .unwrap()
        .replace("test-model", "different-model");
    std::fs::write(config, cfg).unwrap();
    let model = aicx::source_index::build(root, &[], false, false, true).unwrap();
    assert_eq!((model.dense_embedded, model.dense_reused), (2, 0));
    assert_eq!(server.calls.load(Ordering::SeqCst), 6);
}

#[test]
fn interrupted_semantic_work_survives_catalog_drift() {
    let _guard = SERIAL.lock().unwrap();
    let (fixture, server) = contract_fixture();
    let root = &fixture.root;
    catalog_members(root, &[1, 2]);
    server.fail_after.store(1, Ordering::SeqCst);
    assert!(aicx::source_index::build(root, &[], false, false, true).is_err());
    catalog_members(root, &[1, 2, 3]);
    server.fail_after.store(0, Ordering::SeqCst);
    let report = aicx::source_index::build(root, &[], false, false, true).unwrap();
    assert_eq!((report.dense_reused, report.dense_embedded), (1, 2));
}

#[test]
fn semantic_embeds_tail_after_8192_and_detects_cache_vector_mutation() {
    let _guard = SERIAL.lock().unwrap();
    let (fixture, server) = contract_fixture();
    let root = &fixture.root;
    catalog_members(root, &[1]);
    let source = root.join("00000000-0000-4000-8000-000000000001.jsonl");
    let mut rows = std::fs::read_to_string(&source).unwrap();
    for n in 0..90 {
        rows.push_str(&format!("{}\n", json!({"timestamp":"2026-10-01T10:10:00Z","type":"event_msg",
        "payload":{"type":"user_message","message":format!("Discussion {n}: complete coverage of our conversation and the chosen implementation. {}", "Signal text. ".repeat(12))}})));
    }
    rows.push_str(&format!("{}\n", json!({"timestamp":"2026-10-01T10:20:00Z","type":"event_msg","payload":{"type":"agent_message","message":"TAIL_ACCEPTANCE: the final decision is saffron retention."}})));
    std::fs::write(source, rows).unwrap();
    let report = aicx::source_index::build(root, &[], false, false, true).unwrap();
    assert!(report.dense_docs > report.lexical_docs);
    let inputs = server.inputs.lock().unwrap().clone();
    assert!(
        inputs.iter().any(|s| s.contains("TAIL_ACCEPTANCE")),
        "tail must reach the provider"
    );
    assert!(
        inputs.iter().all(|s| s.len() <= 4096),
        "each complete passage fits the input budget"
    );
    let cache = std::fs::read_dir(root.join("indexed/_all/hybrid/dense-checkpoints"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut lines: Vec<_> = std::fs::read_to_string(&cache)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let mut row: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    row["embedding"] = json!([0.7, 0.8]);
    lines[1] = row.to_string();
    std::fs::write(cache, lines.join("\n") + "\n").unwrap();
    let repaired = aicx::source_index::build(root, &[], false, true, true).unwrap();
    assert_eq!(
        repaired.dense_embedded, 1,
        "modified vector checksum cannot be reused"
    );
    assert_eq!(repaired.dense_reused, report.dense_docs - 1);
}

#[test]
fn privacy_change_refuses_old_evidence_then_publishes_only_visible_sessions() {
    let _guard = SERIAL.lock().unwrap();
    let (fixture, server) = contract_fixture();
    let root = &fixture.root;
    catalog_members(root, &[1, 2]);
    let private = root.join("private-repo");
    std::fs::create_dir_all(private.join(".git")).unwrap();
    let source = root.join("00000000-0000-4000-8000-000000000001.jsonl");
    let old = env!("CARGO_MANIFEST_DIR");
    let body = std::fs::read_to_string(&source)
        .unwrap()
        .replace(old, private.to_str().unwrap());
    std::fs::write(&source, body).unwrap();
    let catalog_path = root.join("catalog/sessions.jsonl");
    let mut rows: Vec<serde_json::Value> = std::fs::read_to_string(&catalog_path)
        .unwrap()
        .lines()
        .map(|r| serde_json::from_str(r).unwrap())
        .collect();
    rows[0]["cwd"] = json!(private);
    rows[0]["project"] = json!("private-repo");
    std::fs::write(
        catalog_path,
        rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
    )
    .unwrap();
    aicx::source_index::build(root, &[], false, false, true).unwrap();
    let calls = server.calls.load(Ordering::SeqCst);
    std::fs::write(root.join(".aicxignore"), format!("{}\n", private.display())).unwrap();
    let refused =
        aicx::search_engine::try_semantic_search(root, "checkpoint", 10, &[None], None, None)
            .unwrap_err();
    assert_eq!(refused.kind(), "retrieval_manifest_stale");
    assert!(refused.reason().contains("privacy policy changed"));
    let report = aicx::source_index::build(root, &[], false, false, true).unwrap();
    assert_eq!(
        (
            report.lexical_docs,
            report.dense_docs,
            report.dense_reused,
            report.dense_embedded
        ),
        (1, 1, 1, 0)
    );
    assert_eq!(server.calls.load(Ordering::SeqCst), calls);
    let found =
        aicx::search_engine::try_semantic_search(root, "checkpoint", 10, &[None], None, None)
            .unwrap();
    assert!(
        found
            .results
            .iter()
            .all(|r| r.session_id.as_deref() == Some("00000000-0000-4000-8000-000000000002"))
    );
}

struct MutateAfterEmbedding {
    path: PathBuf,
    text: String,
}
impl Reporter for MutateAfterEmbedding {
    fn phase_start(&self, _: &Phase) {}
    fn phase_tick(&self, _: &Phase, _: u64) {}
    fn phase_finish(&self, phase: &Phase, outcome: &PhaseOutcome) {
        if phase.name == "index_embed" && outcome.is_ok() {
            std::fs::write(&self.path, &self.text).unwrap();
        }
    }
}

#[test]
fn source_drift_during_embedding_retains_current_and_completed_work() {
    let _guard = SERIAL.lock().unwrap();
    let (fixture, _server) = contract_fixture();
    let root = &fixture.root;
    catalog_members(root, &[1, 2]);
    aicx::source_index::build(root, &[], false, false, true).unwrap();
    let pointer = root.join("indexed/_all/hybrid/CURRENT");
    let before = std::fs::read(&pointer).unwrap();
    catalog_members(root, &[1, 2, 3]);
    let path = root.join("00000000-0000-4000-8000-000000000001.jsonl");
    let text = std::fs::read_to_string(&path).unwrap().replace(
        "Query for session 1",
        "Changed while provider was running in session 1",
    );
    let error = aicx::source_index::build_with_reporter(
        root,
        &[],
        false,
        false,
        true,
        Arc::new(MutateAfterEmbedding { path, text }),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("changed during embedding"));
    assert_eq!(std::fs::read(pointer).unwrap(), before);
    let resumed = aicx::source_index::build(root, &[], false, false, true).unwrap();
    assert_eq!((resumed.dense_reused, resumed.dense_embedded), (2, 1));
}

#[test]
fn admitted_dense_mutation_is_rejected_and_cache_restores_without_reembedding() {
    use std::io::{Seek, SeekFrom};
    let _guard = SERIAL.lock().unwrap();
    let (fixture, server) = contract_fixture();
    let root = &fixture.root;
    catalog_members(root, &[1]);
    aicx::source_index::build(root, &[], false, false, true).unwrap();
    let manifest_path = aicx::vector_index::hybrid_manifest_path(None).unwrap();
    let manifest = aicx_retrieve::Manifest::read_from_path(&manifest_path).unwrap();
    assert!(manifest.dense_payload_blake3.is_some());
    let payload = manifest_path
        .parent()
        .unwrap()
        .join(aicx_retrieve::MMAP_DENSE_PAYLOAD_FILE_NAME);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(payload)
        .unwrap();
    let mut header = [0u8; 128];
    file.read_exact(&mut header).unwrap();
    let offset = u64::from_le_bytes(header[80..88].try_into().unwrap());
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&0.9f32.to_le_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let filters = aicx::search_engine::SemanticSearchFilters {
        deep: true,
        ..Default::default()
    };
    let error = aicx::search_engine::try_semantic_search_filtered(
        root,
        "checkpoint",
        5,
        &[None],
        None,
        None,
        &filters,
    )
    .unwrap_err();
    assert!(error.reason().contains("checksum mismatch"), "{error}");
    assert!(!error.allows_filesystem_fallback());
    let before = server.calls.load(Ordering::SeqCst);
    let restored = aicx::source_index::build(root, &[], false, false, true).unwrap();
    assert_eq!((restored.dense_reused, restored.dense_embedded), (1, 0));
    assert_eq!(server.calls.load(Ordering::SeqCst), before);
    let path = aicx::vector_index::hybrid_manifest_path(None).unwrap();
    aicx_retrieve::Manifest::read_from_path(&path)
        .unwrap()
        .verify_dense_payload_checksum(path.parent().unwrap())
        .unwrap();
}

#[test]
fn future_semantic_contract_cannot_be_overwritten_by_lexical_writer() {
    let _guard = SERIAL.lock().unwrap();
    let (fixture, _server) = contract_fixture();
    let root = &fixture.root;
    catalog_members(root, &[1]);
    aicx::source_index::build(root, &[], false, false, true).unwrap();
    let path = aicx::vector_index::hybrid_manifest_path(None).unwrap();
    let mut manifest = aicx_retrieve::Manifest::read_from_path(&path).unwrap();
    assert_eq!(manifest.schema_version, "3.0");
    manifest.schema_version = "99.0".into();
    manifest.write_to_path(&path).unwrap();
    let pointer = root.join("indexed/_all/hybrid/CURRENT");
    let before = std::fs::read(&pointer).unwrap();
    catalog_members(root, &[1, 2]);
    let error = aicx::source_index::build(root, &[], false, false, false).unwrap_err();
    assert!(format!("{error:#}").contains("unsupported generation contract"));
    assert_eq!(std::fs::read(pointer).unwrap(), before);
}

#[test]
fn lexical_admission_cannot_stamp_a_new_policy_over_old_content() {
    let _guard = SERIAL.lock().unwrap();
    let (fixture, _server) = contract_fixture();
    let root = &fixture.root;
    catalog_members(root, &[1, 2]);
    aicx::source_index::build(root, &[], false, false, false).unwrap();
    let pointer = root.join("indexed/_all/hybrid/CURRENT");
    let before = std::fs::read(&pointer).unwrap();
    let manifest = aicx_retrieve::Manifest::read_from_path(
        &aicx::vector_index::hybrid_manifest_path(None).unwrap(),
    )
    .unwrap();
    let chunks = aicx::vector_index::current_lexical_chunks().unwrap();
    std::fs::write(
        root.join(".aicxignore"),
        "/private/newly-withheld-repository\n",
    )
    .unwrap();
    let error = aicx::vector_index::publish_source_lexical_generation_with_policy(
        &chunks,
        "old-source-snapshot",
        manifest.source_policy_fingerprint.as_deref(),
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("changed before lexical generation admission"),
        "{error:#}"
    );
    assert_eq!(std::fs::read(&pointer).unwrap(), before);
}
