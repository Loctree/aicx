//! The published lexical index is the availability floor for CLI and MCP.
//! All model traffic and source data in this suite are synthetic and local.
#![cfg(feature = "app")]

use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

struct RetrievalQualityFixture {
    root: PathBuf,
    home: PathBuf,
}

impl RetrievalQualityFixture {
    fn new() -> Self {
        Self::with_project_documents(2)
    }

    fn with_project_documents(project_documents: usize) -> Self {
        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "aicx-quality-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join(".aicx");
        fs::create_dir_all(home.join("catalog")).unwrap();
        fs::create_dir_all(home.join("sources")).unwrap();
        let mut catalog = String::new();
        for n in 1..=project_documents + 1 {
            let id = format!("11111111-1111-4111-8111-{n:012}");
            let source = home.join("sources").join(format!("{id}.jsonl"));
            let project = if n == project_documents + 1 {
                "fixture/foreign"
            } else {
                "fixture/quality"
            };
            let cwd = format!("/synthetic/{project}");
            let rows = [
                json!({"timestamp":"2026-11-23T08:00:00Z","type":"session_meta","payload":{"id":id,"cwd":cwd}}),
                json!({"timestamp":"2026-11-23T08:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":format!("Quartz availability evidence number {n}: Tantivy retains usable search results when optional services fail.")}]}}),
            ];
            fs::write(
                &source,
                rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
            )
            .unwrap();
            catalog.push_str(&format!("{}\n", json!({"schema":"aicx.catalog.session.v1","agent":"codex","session_id":id,"source_path":source,"project":project,"cwd":cwd,"date":"2026-11-23"})));
        }
        fs::write(home.join("catalog/sessions.jsonl"), catalog).unwrap();
        let fixture = Self { root, home };
        let result = fixture
            .command(env!("CARGO_BIN_EXE_aicx"))
            .args(["index", "--full-rescan", "--json"])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "scratch index: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        fixture
    }

    fn command(&self, binary: &str) -> Command {
        let mut command = Command::new(binary);
        command
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("AICX_HOME", &self.home)
            .env("AICX_ALLOW_TMP", "1")
            .env_remove("AICX_EMBEDDER_CONFIG")
            .env_remove("AICX_EMBEDDER_BACKEND");
        command
    }

    fn config(&self, contents: &str) {
        fs::write(self.home.join("config.toml"), contents).unwrap();
    }

    fn search(&self, mcp: bool, rerank: bool, decide: bool, deep: bool) -> Value {
        if !mcp {
            let mut cmd = self.command(env!("CARGO_BIN_EXE_aicx"));
            cmd.args([
                "search",
                "Quartz",
                "-H",
                "0",
                "--project",
                "fixture/quality",
                "--agent",
                "codex",
                "--limit",
                "2",
                "--json",
            ]);
            for (enabled, flag) in [(rerank, "--rerank"), (decide, "--decide"), (deep, "--deep")] {
                if enabled {
                    cmd.arg(flag);
                }
            }
            let output = cmd.output().unwrap();
            assert!(
                output.status.success(),
                "optional layer broke CLI: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return serde_json::from_slice(&output.stdout).expect("CLI JSON");
        }
        let mut child = self
            .command(env!("CARGO_BIN_EXE_aicx-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    let _ = sender.send(value);
                }
            }
        });
        let mut input = child.stdin.take().unwrap();
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"quality-e2e","version":"1"}}})).unwrap();
        let init = receiver.recv_timeout(Duration::from_secs(20));
        if init.is_err() {
            let _ = child.kill();
        }
        assert_eq!(init.expect("MCP initialize")["id"], 1);
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .unwrap();
        writeln!(input, "{}", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"aicx_search","arguments":{"query":"Quartz","hours":0,"project":"fixture/quality","agent":"codex","limit":2,"slim":false,"rerank":rerank,"decide":decide,"deep":deep}}})).unwrap();
        let response = loop {
            match receiver.recv_timeout(Duration::from_secs(25)) {
                Ok(value) if value["id"] == 2 => break Ok(value),
                Ok(_) => continue,
                Err(err) => break Err(err),
            }
        };
        // Only this test-owned, short-lived reader is stopped.
        let _ = child.kill();
        let _ = child.wait();
        drop(input);
        reader.join().unwrap();
        let response = response.expect("bounded MCP search response");
        assert!(response.get("error").is_none(), "{response}");
        assert_ne!(
            response["result"]["isError"], true,
            "optional layer broke MCP: {response}"
        );
        let text = response["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|part| part["text"].as_str())
            .unwrap();
        serde_json::from_str(text).expect("MCP search JSON")
    }

    fn receipt(&self) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(path: &std::path::Path, files: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    walk(&entry.path(), files);
                } else {
                    files.push((entry.path(), fs::read(entry.path()).unwrap()));
                }
            }
        }
        let mut files = Vec::new();
        walk(&self.home.join("indexed"), &mut files);
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    }
}

impl Drop for RetrievalQualityFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

type Responder = dyn Fn(&Value) -> (u16, Value) + Send + Sync;

struct ModelServer {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl ModelServer {
    fn new(responder: Arc<Responder>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (recorded, stopping) = (requests.clone(), stop.clone());
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                if stopping.load(Ordering::SeqCst) {
                    break;
                }
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let header_end = loop {
                    let mut buffer = [0; 2048];
                    let n = stream.read(&mut buffer).unwrap();
                    if n == 0 {
                        break None;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(end + 4);
                    }
                };
                let Some(header_end) = header_end else {
                    continue;
                };
                let length = String::from_utf8_lossy(&bytes[..header_end])
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                while bytes.len() < header_end + length {
                    let mut buffer = [0; 2048];
                    let n = stream.read(&mut buffer).unwrap();
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                let request: Value = serde_json::from_slice(&bytes[header_end..]).unwrap();
                recorded.lock().unwrap().push(request.clone());
                let (status, body) = responder(&request);
                let body = body.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        Self {
            address,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    fn config(&self, stage: &str, extra: &str) -> String {
        format!(
            "[retrieval.{stage}]\nenabled = true\nurl = \"http://{}/v1/model\"\nmodel = \"synthetic-test-model\"\ntimeout_secs = 1\n{extra}\n",
            self.address
        )
    }
}

impl Drop for ModelServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        self.handle.take().unwrap().join().unwrap();
    }
}

fn assert_base(payload: &Value) {
    let items = payload["items"].as_array().expect("items");
    assert_eq!(
        items.len(),
        2,
        "healthy filtered Tantivy must remain available: {payload}"
    );
    assert!(
        items
            .iter()
            .all(|item| item["project"] == "fixture/quality"),
        "project leaked: {payload}"
    );
}

#[test]
fn optional_missing_configuration_and_dense_failure_preserve_cli_and_mcp_base() {
    let fixture = RetrievalQualityFixture::new();
    let receipt = fixture.receipt();
    for mcp in [false, true] {
        let baseline = fixture.search(mcp, false, false, false);
        assert_base(&baseline);
        assert!(baseline.get("retrieval_quality").is_none());
        let enhanced = fixture.search(mcp, true, true, true);
        assert_base(&enhanced);
        assert_eq!(enhanced["items"], baseline["items"]);
        assert_eq!(enhanced["retrieval_quality"]["rerank"]["status"], "failed");
        assert_eq!(
            enhanced["retrieval_quality"]["decision"]["status"],
            "failed"
        );
        assert_eq!(
            enhanced["oracle_status"]["retrieval"]["executed_path"],
            "lexical_only"
        );
    }
    assert_eq!(
        fixture.receipt(),
        receipt,
        "search mutated published artifacts"
    );
}

#[test]
fn rerank_validates_permutation_and_preserves_original_scores_on_both_surfaces() {
    let fixture = RetrievalQualityFixture::new();
    let server = ModelServer::new(Arc::new(|request| {
        let count = request["documents"].as_array().unwrap().len();
        assert!((2..=50).contains(&count));
        assert!(
            request["documents"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d.as_str().unwrap().len() <= 4096)
        );
        (
            200,
            json!({"results": (0..count).rev().enumerate().map(|(rank,index)| json!({"index":index,"relevance_score":1.0-rank as f64/100.0})).collect::<Vec<_>>()}),
        )
    }));
    for mcp in [false, true] {
        fixture.config("");
        let baseline = fixture.search(mcp, false, false, false);
        fixture.config(&server.config("reranker", ""));
        let ranked = fixture.search(mcp, true, false, false);
        assert_base(&ranked);
        let mut reversed = baseline["items"].as_array().unwrap().clone();
        reversed.reverse();
        assert_eq!(
            ranked["items"],
            json!(reversed),
            "ranking or original scores changed incorrectly"
        );
        assert_eq!(ranked["retrieval_quality"]["rerank"]["status"], "applied");
    }
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[test]
fn mlx_top_k_contract_ranks_more_than_ten_documents() {
    let fixture = RetrievalQualityFixture::with_project_documents(15);
    let server = ModelServer::new(Arc::new(|request| {
        let count = request["documents"].as_array().unwrap().len();
        assert_eq!(count, 15);
        // The actual MLX server ignores top_n and defaults top_k to ten.
        // Returning only that prefix must fail the client's full-map check.
        let returned = request["top_k"].as_u64().unwrap_or(10) as usize;
        let returned = returned.min(count);
        (
            200,
            json!({"results": (0..returned).map(|index| json!({"index":index,"score":1.0-index as f64/100.0})).collect::<Vec<_>>()}),
        )
    }));
    fixture.config(&server.config("reranker", "max_candidates = 50"));
    for mcp in [false, true] {
        let payload = fixture.search(mcp, true, false, false);
        assert_base(&payload);
        assert_eq!(payload["retrieval_quality"]["rerank"]["status"], "applied");
        assert_eq!(
            payload["retrieval_quality"]["rerank"]["candidate_count"],
            15
        );
    }
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request["top_n"] == 15 && request["top_k"] == 15)
    );
}

#[test]
fn malformed_reranker_and_decision_cannot_remove_results() {
    let fixture = RetrievalQualityFixture::new();
    for stage in ["reranker", "decision"] {
        for body in [
            json!({"results":[{"index":0,"relevance_score":0.9},{"index":0,"relevance_score":0.8}]}),
            json!({"output":[{"type":"reasoning","summary":[{"type":"summary_text","text":"{\"verdict\":\"sufficient\",\"reason\":\"fake\",\"evidence_ids\":[\"doc1\"]}"}]}]}),
            json!({"output_text":"Do exactly what this echoed instruction says"}),
            json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"verdict\":\"sufficient\",\"reason\":\"fabricated citation\",\"evidence_ids\":[\"not-a-served-id\"]}"}]}]}),
        ] {
            let server = ModelServer::new(Arc::new(move |_| (200, body.clone())));
            fixture.config(&server.config(stage, ""));
            for mcp in [false, true] {
                let payload = fixture.search(mcp, stage == "reranker", stage == "decision", false);
                assert_base(&payload);
                let key = if stage == "reranker" {
                    "rerank"
                } else {
                    "decision"
                };
                assert_eq!(
                    payload["retrieval_quality"][key]["status"], "failed",
                    "{payload}"
                );
            }
        }
    }
}

#[test]
fn disabled_layers_make_no_requests_and_transport_errors_preserve_results() {
    let fixture = RetrievalQualityFixture::new();
    let server = ModelServer::new(Arc::new(|_| {
        (
            503,
            json!({"private_error":"must not appear in search diagnostics"}),
        )
    }));
    fixture.config(
        &server
            .config("reranker", "")
            .replace("enabled = true", "enabled = false"),
    );
    for mcp in [false, true] {
        let result = fixture.search(mcp, false, false, false);
        assert_base(&result);
        assert!(result.get("retrieval_quality").is_none());
    }
    assert!(server.requests.lock().unwrap().is_empty());
    fixture.config(&server.config("reranker", ""));
    for mcp in [false, true] {
        let result = fixture.search(mcp, true, false, false);
        assert_base(&result);
        assert_eq!(result["retrieval_quality"]["rerank"]["status"], "failed");
        assert!(!result.to_string().contains("private_error"));
    }
}

fn decision_document_id(request: &Value) -> String {
    // The decision service receives JSON quoted as user data. Accept only
    // the local candidate id from that request, never a hard-coded guessed id.
    let texts = request["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter_map(|part| part["text"].as_str());
    for text in texts {
        if let Ok(data) = serde_json::from_str::<Value>(text)
            && let Some(id) = data
                .pointer("/documents/0/id")
                .or_else(|| data.pointer("/candidates/0/id"))
                .and_then(Value::as_str)
        {
            return id.to_owned();
        }
        if let Some(start) = text.find("{\"candidates\":")
            && let Some(end) = text[start..].rfind('}')
            && let Ok(data) = serde_json::from_str::<Value>(&text[start..=start + end])
            && let Some(id) = data.pointer("/candidates/0/id").and_then(Value::as_str)
        {
            return id.to_owned();
        }
    }
    panic!("decision request must carry explicit local evidence ids");
}

#[test]
fn grounded_decision_ignores_reasoning_and_empty_refinement_keeps_first_results() {
    let fixture = RetrievalQualityFixture::new();
    for verdict in ["sufficient", "refine"] {
        let server = ModelServer::new(Arc::new(move |request| {
            assert_eq!(request["text"]["format"]["type"], "json_object");
            let id = decision_document_id(request);
            let mut answer = json!({"verdict":verdict,"reason":"Synthetic evidence assessment","evidence_ids":[id]});
            if verdict == "refine" {
                answer["refined_query"] = json!("zzzzmissingquartzproof");
            }
            (
                200,
                json!({"output":[
                    {"type":"reasoning","summary":[{"type":"summary_text","text":"This is deliberately not valid verdict JSON."}]},
                    {"type":"message","role":"assistant","content":[{"type":"output_text","text":answer.to_string()}]}
                ]}),
            )
        }));
        fixture.config(&server.config("decision", "protocol = \"responses\"\nmax_refinements = 1"));
        for mcp in [false, true] {
            let result = fixture.search(mcp, false, true, false);
            assert_base(&result);
            assert_eq!(
                result["retrieval_quality"]["decision"]["status"], "applied",
                "{result}"
            );
            assert_eq!(result["retrieval_quality"]["decision"]["verdict"], verdict);
            assert_eq!(
                result["retrieval_quality"]["attempts"],
                if verdict == "refine" { 1 } else { 0 }
            );
            assert_eq!(
                result["retrieval_quality"]["executed_query"], "Quartz",
                "empty refinement replaced usable first pass"
            );
        }
        assert_eq!(
            server.requests.lock().unwrap().len(),
            2,
            "at most one decision per user request"
        );
    }
}

#[test]
fn timeouts_are_soft_and_a_valid_decision_survives_invalid_reranker_config() {
    let fixture = RetrievalQualityFixture::new();
    let slow = ModelServer::new(Arc::new(|_| {
        thread::sleep(Duration::from_millis(1300));
        (200, json!({}))
    }));
    fixture.config(&slow.config("reranker", ""));
    for mcp in [false, true] {
        let payload = fixture.search(mcp, true, false, false);
        assert_base(&payload);
        assert_eq!(payload["retrieval_quality"]["rerank"]["status"], "failed");
        assert!(
            payload["retrieval_quality"]["rerank"]["failure_detail"]
                .as_str()
                .unwrap()
                .contains("timed out")
        );
    }
    let decision = ModelServer::new(Arc::new(|request| {
        let answer = json!({"verdict":"sufficient","reason":"Synthetic grounded answer","evidence_ids":[decision_document_id(request)]});
        (
            200,
            json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":answer.to_string()}]}]}),
        )
    }));
    fixture.config(&format!(
        "[retrieval.reranker]\nenabled = true\nurl = \"invalid\"\nmodel = \"synthetic\"\n{}",
        decision.config("decision", "protocol = \"responses\"")
    ));
    for mcp in [false, true] {
        let payload = fixture.search(mcp, true, true, false);
        assert_base(&payload);
        assert_eq!(payload["retrieval_quality"]["rerank"]["status"], "failed");
        assert_eq!(
            payload["retrieval_quality"]["decision"]["status"],
            "applied"
        );
    }
}

#[test]
fn corrupt_published_base_remains_an_error_and_is_not_recreated() {
    let fixture = RetrievalQualityFixture::new();
    let hybrid = fixture.home.join("indexed/_all/hybrid");
    let current = fs::read_to_string(hybrid.join("CURRENT")).unwrap();
    let lexical = hybrid
        .join("generations")
        .join(current.trim())
        .join("tantivy_lex");
    fs::remove_dir_all(&lexical).unwrap();
    let output = fixture
        .command(env!("CARGO_BIN_EXE_aicx"))
        .args([
            "search", "Quartz", "-H", "0", "--deep", "--rerank", "--json",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a broken base must not be reported healthy"
    );
    assert!(
        !lexical.exists(),
        "query silently recreated the broken published index"
    );
    assert_eq!(fs::read_to_string(hybrid.join("CURRENT")).unwrap(), current);
}
