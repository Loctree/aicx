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
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockEmbedder {
    fn new(fail_after_count: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let fail_after = Arc::new(AtomicUsize::new(fail_after_count));
        let calls = Arc::new(AtomicUsize::new(0));
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
    assert!(lines[0].contains("aicx.source_dense_checkpoint.v1"));

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

    // Upon successful publication, checkpoint file must be removed!
    assert!(
        !checkpoint_files[0].path().exists(),
        "checkpoint file must be deleted upon successful publication"
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
