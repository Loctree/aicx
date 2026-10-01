//! Real source parse -> HTTP embeddings -> CURRENT publication, isolated from
//! the production catalog and index. No downloaded model is needed.
#![cfg(all(feature = "app", feature = "cloud-embedder"))]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
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

struct Embedder {
    address: std::net::SocketAddr,
    bad: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Embedder {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let bad = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (server_bad, server_calls, server_stop) = (bad.clone(), calls.clone(), stop.clone());
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
                    let mut buf = [0u8; 1024];
                    let count = stream.read(&mut buf).unwrap();
                    assert!(count > 0, "request closed before HTTP headers");
                    bytes.extend_from_slice(&buf[..count]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                while bytes.len() < header_end + length {
                    let mut buf = [0u8; 1024];
                    let count = stream.read(&mut buf).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buf[..count]);
                }
                let body: serde_json::Value =
                    serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
                server_calls.fetch_add(1, Ordering::SeqCst);
                let data: Vec<_> = body["input"].as_array().unwrap().iter().enumerate().map(|(i, _)| {
                    json!({"index": i, "embedding": if server_bad.load(Ordering::SeqCst) { vec![0.0, 0.0] } else { vec![1.0, 0.5] }})
                }).collect();
                let reply = json!({"data": data}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
            }
        });
        Self {
            address,
            bad,
            calls,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Embedder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        self.handle.take().unwrap().join().unwrap();
    }
}

#[test]
fn semantic_progress_model_drift_and_failed_rebuild_preserve_current() {
    let root = std::env::temp_dir().join(format!(
        "aicx-semantic-progress-{}-{}",
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
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut catalog = String::new();
    for n in 1..=2 {
        let sid = format!("11111111-1111-4111-8111-{n:012}");
        let path = root.join(format!("{sid}.jsonl"));
        let rows = [
            json!({"timestamp":"2026-10-01T10:00:00Z","type":"session_meta","payload":{"id":sid,"cwd":cwd,"timestamp":"2026-10-01T10:00:00Z"}}),
            json!({"timestamp":"2026-10-01T10:00:01Z","type":"turn_context","payload":{"turn_id":"t1","cwd":cwd}}),
            json!({"timestamp":"2026-10-01T10:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Please implement truthful source indexing progress and retain the last valid generation."}}),
            json!({"timestamp":"2026-10-01T10:00:03Z","type":"event_msg","payload":{"type":"agent_message","message":"The source indexing implementation is ready for review."}}),
        ];
        std::fs::write(
            &path,
            rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        catalog.push_str(&format!(
            "{}\n",
            json!({"schema":"aicx.catalog.session.v1","agent":"codex","session_id":sid,
            "source_path":path,"project":"loctree/aicx","cwd":cwd,"date":"2026-10-01"})
        ));
    }
    std::fs::write(root.join("catalog/sessions.jsonl"), catalog).unwrap();
    let embedder = Embedder::new();
    let write_config = |model: &str| {
        std::fs::write(&config, format!("[embedder]\nbackend = \"cloud\"\n[embedder.cloud]\nurl = \"http://{}/v1/embeddings\"\nmodel = \"{}\"\ndimension = 2\nbatch_size = 1\n", embedder.address, model)).unwrap();
    };
    write_config("fixture-v1");
    let capture = Arc::new(Capture::default());
    let build =
        || aicx::source_index::build_with_reporter(&root, &[], false, false, true, capture.clone());
    let first = build().unwrap();
    assert_eq!(first.dense_docs, 2);
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 2);
    let events = capture.0.lock().unwrap().clone();
    let embed: Vec<_> = events
        .iter()
        .filter(|(name, _, _)| name == "index_embed")
        .collect();
    assert_eq!(
        embed
            .iter()
            .map(|(_, event, count)| (event.as_str(), *count))
            .collect::<Vec<_>>(),
        vec![("start", 2), ("tick", 1), ("tick", 2), ("ok", 0)]
    );
    assert!(build().unwrap().unchanged);
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 2);
    write_config("fixture-v2");
    assert!(
        !build().unwrap().unchanged,
        "same dimension cannot hide a model change"
    );
    assert_eq!(embedder.calls.load(Ordering::SeqCst), 4);
    let hybrid = root.join("indexed/_all/hybrid");
    let dir = aicx::vector_index::resolve_hybrid_generation_dir(&hybrid);
    std::fs::write(
        dir.join(aicx_retrieve::MMAP_DENSE_PAYLOAD_FILE_NAME),
        b"interrupted payload",
    )
    .unwrap();
    assert!(aicx::vector_index::current_dense_not_built().unwrap());
    let status = aicx::api::index_status_at(&root, None).unwrap();
    assert_eq!(status.dense_status, "invalid");
    let pointer = std::fs::read(hybrid.join("CURRENT")).unwrap();
    embedder.bad.store(true, Ordering::SeqCst);
    assert!(
        build().is_err(),
        "zero vectors must not publish a generation"
    );
    assert_eq!(std::fs::read(hybrid.join("CURRENT")).unwrap(), pointer);
    assert!(
        capture
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|(name, event, _)| name == "index_embed" && event == "failed")
    );
    drop(embedder);
    drop(fixture);
}
