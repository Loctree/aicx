#![cfg(all(feature = "app", feature = "cloud-embedder"))]

use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const TOKEN: &str = "SOURCE_DENSE_CLI_E2E_7c0a8f";
const SESSION_ID: &str = "019f8e3b-daee-7bbb-8ccc-111111111111";

fn unique_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-source-dense-cli-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, content).expect("write fixture");
}

fn aicx_binary() -> PathBuf {
    std::env::var_os("AICX_E2E_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_aicx")))
}

fn run_aicx(home: &Path, aicx_home: &Path, args: &[&str]) -> Output {
    Command::new(aicx_binary())
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AICX_HOME", aicx_home)
        .env("AICX_ALLOW_TMP", "1")
        .output()
        .expect("run aicx")
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn parse_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "parse JSON failed: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn seed_grok_session(home: &Path) {
    let session_dir = home
        .join(".grok")
        .join("sessions")
        .join("%2FVolumes%2Fvc-workspace%2Fvetcoders%2Faicx")
        .join(SESSION_ID);
    write_file(
        &session_dir.join("chat_history.jsonl"),
        &[
            json!({
                "type": "user",
                "content": [{"type": "text", "text": format!("Find {TOKEN}")}]
            })
            .to_string(),
            json!({
                "type": "assistant",
                "content": format!("{TOKEN} is covered by the explicit dense CLI proof.")
            })
            .to_string(),
        ]
        .join("\n"),
    );
    write_file(
        &session_dir.join("summary.json"),
        &json!({
            "info": {"id": SESSION_ID, "cwd": "/Volumes/vc-workspace/vetcoders/aicx"},
            "session_summary": TOKEN,
            "created_at": "2026-07-26T00:00:00Z",
            "updated_at": "2026-07-26T00:00:01Z",
            "agent_name": "grok"
        })
        .to_string(),
    );
}

fn serve_embedding_request(stream: TcpStream) {
    let mut reader = BufReader::new(stream);
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read request header");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(value) = line
            .strip_prefix("content-length:")
            .or_else(|| line.strip_prefix("Content-Length:"))
        {
            content_length = value.trim().parse().expect("content length");
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).expect("read request body");
    let payload: Value = serde_json::from_slice(&body).expect("embedding request JSON");
    let input_count = payload["input"]
        .as_array()
        .expect("embedding input array")
        .len();
    let response = json!({
        "data": (0..input_count)
            .map(|index| json!({"embedding": [1.0_f32, index as f32 + 0.5]}))
            .collect::<Vec<_>>()
    })
    .to_string();
    let stream = reader.get_mut();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.len(),
        response
    )
    .expect("write embedding response");
    stream.flush().expect("flush embedding response");
}

#[test]
fn source_dense_cli_build_status_and_deep_search() {
    let root = unique_root();
    let home = root.join("home");
    let aicx_home = root.join("aicx-home");
    fs::create_dir_all(&aicx_home).expect("create AICX home");
    seed_grok_session(&home);

    let binary = aicx_binary();
    assert!(binary.is_absolute(), "E2E binary must be an absolute path");
    let version = Command::new(&binary)
        .arg("--version")
        .output()
        .expect("run release candidate --version");
    assert_success(&version, "release candidate version");
    eprintln!(
        "E2E binary={} version={}",
        binary.display(),
        String::from_utf8_lossy(&version.stdout).trim()
    );

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local embedding mock");
    let endpoint = format!(
        "http://{}/v1/embeddings",
        listener.local_addr().expect("mock address")
    );
    let request_count = Arc::new(AtomicUsize::new(0));
    let server_count = Arc::clone(&request_count);
    let server = std::thread::spawn(move || {
        for stream in listener.incoming().take(3) {
            server_count.fetch_add(1, Ordering::SeqCst);
            serve_embedding_request(stream.expect("accept embedding request"));
        }
    });
    write_file(
        &aicx_home.join("config.toml"),
        &format!(
            "[embedder]\nbackend = \"cloud\"\n\n[embedder.cloud]\nurl = \"{endpoint}\"\nmodel = \"mock-dense-v1\"\ndimension = 2\nbatch_size = 16\ntimeout_secs = 5\n"
        ),
    );

    let catalog = run_aicx(&home, &aicx_home, &["catalog", "rebuild", "--json"]);
    assert_success(&catalog, "catalog rebuild");

    let lexical = run_aicx(&home, &aicx_home, &["index", "--json", "--full-rescan"]);
    assert_success(&lexical, "default lexical index");
    let lexical_report = parse_json(&lexical);
    assert_eq!(lexical_report["dense_requested"], false);
    assert_eq!(request_count.load(Ordering::SeqCst), 0);

    let lexical_status = run_aicx(&home, &aicx_home, &["index", "status", "--json"]);
    assert_success(&lexical_status, "lexical status");
    let lexical_status = parse_json(&lexical_status);
    assert_eq!(
        lexical_status[0]["status"]["dense_state"],
        "optional_not_built"
    );
    assert_eq!(lexical_status[0]["status"]["readiness"], "ready");

    let dense = run_aicx(&home, &aicx_home, &["index", "--dense", "--json"]);
    assert_success(&dense, "explicit dense index");
    let dense_report = parse_json(&dense);
    assert_eq!(dense_report["dense_requested"], true);
    assert_eq!(dense_report["dense_count"], 1);
    assert_eq!(dense_report["dense_newly_embedded"], 1);
    assert_eq!(request_count.load(Ordering::SeqCst), 1);

    let dense_status = run_aicx(&home, &aicx_home, &["index", "status", "--json"]);
    assert_success(&dense_status, "dense status");
    let dense_status = parse_json(&dense_status);
    assert_eq!(dense_status[0]["status"]["readiness"], "ready");
    assert_eq!(dense_status[0]["status"]["dense_state"], "ready");
    assert_eq!(dense_status[0]["status"]["dense_missing_count"], 0);
    assert_eq!(dense_status[0]["status"]["pending_chunks"], 0);

    let hybrid_root = aicx_home.join("indexed/_all/hybrid");
    let current_name =
        fs::read_to_string(hybrid_root.join("CURRENT")).expect("read CURRENT generation");
    let generation_dir = hybrid_root.join("generations").join(current_name.trim());
    let manifest_path = generation_dir.join("manifest.json");
    let manifest_bytes = fs::read(&manifest_path).expect("read CURRENT manifest");
    let manifest =
        aicx_retrieve::Manifest::read_from_path(&manifest_path).expect("parse CURRENT manifest");
    assert_eq!(manifest.dense_kind, aicx_retrieve::MMAP_DENSE_KIND);
    assert!(manifest.dense_count > 0);
    assert_eq!(manifest.dense_count, manifest.lexical_doc_count);
    let mmap_payload_count = fs::read_dir(&generation_dir)
        .expect("read CURRENT generation")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_name() == std::ffi::OsStr::new(aicx_retrieve::MMAP_DENSE_PAYLOAD_FILE_NAME)
        })
        .count();
    assert_eq!(
        mmap_payload_count, 1,
        "CURRENT must contain one mmap payload"
    );
    eprintln!(
        "CURRENT={} manifest_blake3={} dense_kind={} dense_count={} lexical_doc_count={} mmap_payloads={}",
        manifest_path.display(),
        blake3::hash(&manifest_bytes).to_hex(),
        manifest.dense_kind,
        manifest.dense_count,
        manifest.lexical_doc_count,
        mmap_payload_count
    );

    let lexical_noop = run_aicx(&home, &aicx_home, &["index", "--json"]);
    assert_success(&lexical_noop, "lexical no-op after dense");
    assert_eq!(parse_json(&lexical_noop)["unchanged"], true);
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        1,
        "default index must neither initialize the embedder nor replace dense CURRENT"
    );

    let deep = run_aicx(
        &home,
        &aicx_home,
        &["search", TOKEN, "--deep", "--json", "--hours", "0"],
    );
    assert_success(&deep, "deep hybrid search");
    assert!(
        String::from_utf8_lossy(&deep.stdout).contains(TOKEN),
        "deep search must read the published CURRENT corpus\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&deep.stdout),
        String::from_utf8_lossy(&deep.stderr)
    );
    assert!(
        String::from_utf8_lossy(&deep.stdout).contains("hybrid_rrf"),
        "deep search must report hybrid/RRF execution, not lexical or fuzzy fallback\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&deep.stdout),
        String::from_utf8_lossy(&deep.stderr)
    );
    let dense_payload = hybrid_root
        .join("generations")
        .join(current_name.trim())
        .join(aicx_retrieve::MMAP_DENSE_PAYLOAD_FILE_NAME);
    let hidden_dense_payload = dense_payload.with_extension("bin.hidden");
    fs::rename(&dense_payload, &hidden_dense_payload).expect("hide dense mmap payload");
    let missing_dense = run_aicx(
        &home,
        &aicx_home,
        &["search", TOKEN, "--deep", "--json", "--hours", "0"],
    );
    assert!(
        !missing_dense.status.success(),
        "missing dense payload must be a typed failure, never false hybrid success\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&missing_dense.stdout),
        String::from_utf8_lossy(&missing_dense.stderr)
    );
    let missing_dense_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&missing_dense.stdout),
        String::from_utf8_lossy(&missing_dense.stderr)
    );
    assert!(
        missing_dense_output.contains("dense mmap artifact is missing"),
        "negative control must identify the missing CURRENT payload\n{missing_dense_output}"
    );
    assert!(
        !missing_dense_output.contains("backend=hybrid_rrf"),
        "missing payload must not claim healthy hybrid retrieval\n{missing_dense_output}"
    );
    fs::rename(&hidden_dense_payload, &dense_payload).expect("restore dense mmap payload");

    server.join().expect("embedding mock server");
    assert_eq!(request_count.load(Ordering::SeqCst), 3);

    // A stale primary NDJSON must never be an implicit recovery path. Hide
    // CURRENT while leaving the old filename present: canonical --deep must
    // fail at CURRENT resolution before embedder startup and report the
    // source-driven contract, not resurrect the legacy reader.
    fs::copy(
        hybrid_root
            .join("generations")
            .join(current_name.trim())
            .join("manifest.json"),
        hybrid_root.join("manifest.json"),
    )
    .expect("seed retired root-layout manifest");
    fs::rename(
        hybrid_root.join("CURRENT"),
        hybrid_root.join("CURRENT.hidden"),
    )
    .expect("hide CURRENT");
    write_file(
        &aicx_home.join("indexed/_all/embeddings.ndjson"),
        "{\"schema_version\":\"1\",\"entry_count\":0}\n",
    );
    let missing_current = run_aicx(
        &home,
        &aicx_home,
        &["search", TOKEN, "--deep", "--json", "--hours", "0"],
    );
    assert_success(
        &missing_current,
        "deep search may degrade to fuzzy when CURRENT is absent",
    );
    let missing_current_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&missing_current.stdout),
        String::from_utf8_lossy(&missing_current.stderr)
    );
    assert!(
        missing_current_output.contains("source-driven dense CURRENT"),
        "missing CURRENT must name the canonical contract\n{missing_current_output}"
    );
    assert!(
        !missing_current_output.contains("indexed/_all/embeddings.ndjson"),
        "canonical --deep must not inspect or recommend the retired path\n{missing_current_output}"
    );
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        3,
        "missing CURRENT must fail before embedder startup"
    );

    let _ = fs::remove_dir_all(root);
}
