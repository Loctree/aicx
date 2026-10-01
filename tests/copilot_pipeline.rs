#![cfg(feature = "app")]

//! Public CLI and MCP paths over synthetic GitHub Copilot CLI session files.
//! Every subprocess gets a scratch HOME/AICX_HOME; no live source is read.

use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SESSION_ID: &str = "11111111-2222-4333-8444-555555555555";
const FIRST_TOKEN: &str = "copilotpipelineinitial91dbea";
const APPEND_TOKEN: &str = "copilotpipelineappended47fdae";
const TOOL_TOKEN: &str = "copilottoolexclusive893fed";
const EVENT_TIME: &str = "2026-09-29T12:00:00Z";
static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Git hooks export repository-local variables. Fixtures must never inherit
/// those coordinates, even for the git processes spawned by the CLI itself.
fn isolated_test_command(binary: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(binary);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
}

struct CopilotFixture {
    home: PathBuf,
    copilot_home: PathBuf,
    events: PathBuf,
}

impl CopilotFixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!(
            "aicx-copilot-pipeline-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let cwd = home.join("work").join("copilot-pipeline");
        fs::create_dir_all(&cwd).unwrap();
        assert!(
            isolated_test_command("git")
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .args(["init", "--quiet"])
                .arg(&cwd)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            isolated_test_command("git")
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .arg("-C")
                .arg(&cwd)
                .args([
                    "config",
                    "remote.origin.url",
                    "https://github.com/example/copilot-pipeline.git"
                ])
                .status()
                .unwrap()
                .success()
        );
        let copilot_home = home.join(".copilot");
        let dir = copilot_home.join("session-state").join(SESSION_ID);
        fs::create_dir_all(&dir).unwrap();
        let events = dir.join("events.jsonl");
        let records = [
            event(
                "start",
                "session.start",
                json!({
                    "sessionId": SESSION_ID, "startTime": EVENT_TIME,
                    "context": {"cwd": cwd, "gitRoot": cwd, "branch": "main"},
                    "copilotVersion": "synthetic", "selectedModel": "copilot-test"
                }),
            ),
            event(
                "user1",
                "user.message",
                json!({
                    "parentAgentTaskId": "synthetic-parent-task",
                    "messageId": "user1", "content": format!(
                        "Decision: use lexical search for {FIRST_TOKEN}. Please implement the session pipeline."
                    )
                }),
            ),
            event(
                "assistant1",
                "assistant.message",
                json!({
                    "messageId": "assistant1", "content": format!("I will implement {FIRST_TOKEN}."),
                    "toolRequests": [{"toolCallId": "shell1", "name": "bash", "arguments": {"command": "cargo test --workspace", "cwd": cwd}}]
                }),
            ),
            event(
                "toolstart",
                "tool.execution_start",
                json!({
                    "toolCallId": "shell1", "toolName": "bash",
                    "arguments": {"command": "cargo test --workspace", "cwd": cwd}
                }),
            ),
            event(
                "toolend",
                "tool.execution_complete",
                json!({
                    "toolCallId": "shell1", "success": true,
                    "result": {"content": format!("test result: ok. 3 passed; 0 failed; {TOOL_TOKEN}")}
                }),
            ),
            event(
                "assistant2",
                "assistant.message",
                json!({
                    "messageId": "assistant2", "content": format!("Implemented {FIRST_TOKEN}; all fixture checks passed.")
                }),
            ),
            event("idle", "session.idle", json!({})),
        ];
        fs::write(
            &events,
            records
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        fs::write(dir.join("workspace.yaml"), format!(
            "id: {SESSION_ID}\ncwd: {}\nname: Synthetic Copilot pipeline\ncreated_at: {EVENT_TIME}\nupdated_at: {EVENT_TIME}\n",
            cwd.display()
        )).unwrap();
        // Global telemetry and checkpoint copies are not independent sessions.
        fs::write(copilot_home.join("events.jsonl"), records[0].to_string()).unwrap();
        fs::create_dir_all(dir.join("checkpoints")).unwrap();
        fs::write(dir.join("checkpoints/events.jsonl"), records[0].to_string()).unwrap();
        Self {
            home,
            copilot_home,
            events,
        }
    }

    fn command(&self, binary: &str) -> Command {
        let mut command = isolated_test_command(binary);
        command
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("COPILOT_HOME", &self.copilot_home)
            .env("AICX_HOME", self.home.join(".aicx"))
            .env("AICX_ALLOW_TMP", "1")
            .env("AICX_NO_MUTATION_WARN", "1");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_aicx"))
            .args(args)
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert_success(&output);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid JSON: {error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    fn append(&self) {
        let mut source = OpenOptions::new().append(true).open(&self.events).unwrap();
        for row in [
            event(
                "user2",
                "user.message",
                json!({
                    "messageId": "user2", "content": format!("Decision: retain {APPEND_TOKEN} in the same session.")
                }),
            ),
            event(
                "assistant3",
                "assistant.message",
                json!({
                    "messageId": "assistant3", "content": format!("Retained {APPEND_TOKEN}.")
                }),
            ),
        ] {
            // Equal event timestamps deliberately exercise append-safe state.
            writeln!(source, "{row}").unwrap();
        }
    }
}

impl Drop for CopilotFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn event(id: &str, kind: &str, data: Value) -> Value {
    json!({"id": id, "type": kind, "timestamp": EVENT_TIME, "parentId": null, "data": data})
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn extract(fixture: &CopilotFixture, name: &str, flags: &[&str]) -> String {
    let path = fixture.home.join(name);
    let mut args = vec![
        "extract",
        "copilot",
        "--session",
        SESSION_ID,
        "--output",
        path.to_str().unwrap(),
    ];
    args.extend_from_slice(flags);
    assert_success(&fixture.run(&args));
    fs::read_to_string(path).unwrap()
}

fn search(fixture: &CopilotFixture, token: &str, agent: &str) -> Value {
    fixture.json(&["search", token, "--json", "--hours", "0", "--agent", agent])
}

/// Real stdio MCP transport, kept alive until each response is received.
/// Bounded receives and Drop prevent a failing contract from leaving a server.
struct CopilotMcp {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<Value>,
    next_id: u64,
}

impl CopilotMcp {
    fn new(fixture: &CopilotFixture) -> Self {
        let stderr = fs::File::create(fixture.home.join("mcp.stderr.log")).unwrap();
        let mut child = fixture
            .command(env!("CARGO_BIN_EXE_aicx-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if sender.send(value).is_err() {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            responses,
            next_id: 1,
        };
        let initialized = client.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05", "capabilities": {},
                "clientInfo": {"name": "copilot-pipeline-test", "version": "1"}
            }),
        );
        assert!(
            initialized["serverInfo"]["name"].is_string(),
            "{initialized}"
        );
        client.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        client
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let response = self
                .responses
                .recv_timeout(Duration::from_secs(30))
                .expect("MCP response within 30 seconds");
            if response["id"].as_u64() == Some(id) {
                assert!(response.get("error").is_none(), "MCP error: {response}");
                return response["result"].clone();
            }
        }
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        assert_ne!(result["isError"], true, "tool failed: {result}");
        let text = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }
}

impl Drop for CopilotMcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn copilot_public_pipeline_and_incremental_append() {
    let fixture = CopilotFixture::new();
    let catalog = fixture.json(&["catalog", "rebuild", "--json"]);
    assert_eq!(
        catalog["agents"]["copilot"], 1,
        "telemetry/checkpoints are excluded: {catalog}"
    );
    assert_eq!(catalog["total_sessions"], 1);
    let resolved = fixture.json(&["catalog", "resolve", SESSION_ID, "--json"]);
    assert_eq!(resolved["agent"], "copilot");
    assert_eq!(resolved["session_id"], SESSION_ID);
    assert!(
        resolved["source_path"]
            .as_str()
            .unwrap()
            .ends_with("events.jsonl")
    );

    let sessions = fixture.json(&["sessions", "list", "--all", "--agent", "copilot", "--json"]);
    assert_eq!(sessions.as_array().unwrap().len(), 1);
    assert_eq!(sessions[0]["session_id"], SESSION_ID);
    assert_eq!(sessions[0]["agent"], "copilot");

    let conversation = extract(&fixture, "conversation.md", &["--conversation"]);
    assert!(conversation.contains(FIRST_TOKEN));
    assert!(conversation.contains("Decision: use lexical search"));
    let user = extract(&fixture, "user.md", &["--conversation", "--user-only"]);
    assert!(user.contains(FIRST_TOKEN));
    assert!(!user.contains("I will implement"));
    let commands = extract(
        &fixture,
        "commands.md",
        &["--agent-commands", "--result", "full"],
    );
    assert!(commands.contains("cargo test --workspace"), "{commands}");
    assert!(
        commands.contains(TOOL_TOKEN),
        "retained shell result: {commands}"
    );
    let brief = extract(&fixture, "brief.md", &["--brief"]);
    assert!(
        brief.contains("cargo test --workspace"),
        "shell gate appears in distilled brief: {brief}"
    );

    let intents = fixture.json(&[
        "intents",
        "--hours",
        "0",
        "--agent",
        "copilot",
        "--no-live",
        "--emit",
        "json",
    ]);
    let intent_items = intents["items"].as_array().unwrap();
    assert!(
        intent_items
            .iter()
            .any(|item| item.to_string().contains(FIRST_TOKEN)),
        "human intent evidence: {intents}"
    );
    assert!(
        intent_items
            .iter()
            .all(|item| item["session_id"] == SESSION_ID && item["agent"] == "copilot"),
        "intent provenance: {intents}"
    );
    assert!(
        !intent_items
            .iter()
            .any(|item| item.to_string().contains(TOOL_TOKEN)),
        "shell output is not human intent: {intents}"
    );

    let bulk = fixture.json(&["extract", "all", "--json"]);
    assert_eq!(
        bulk["totals"]["extracted"], 1,
        "default provider registry includes Copilot: {bulk}"
    );
    let warm_bulk = fixture.json(&["extract", "all", "--json"]);
    assert_eq!(warm_bulk["totals"]["unchanged"], 1, "{warm_bulk}");

    let out = fixture.home.join("conversations");
    assert_success(&fixture.run(&[
        "conversations",
        "--agent",
        "copilot",
        "--hours",
        "0",
        "--out-dir",
        out.to_str().unwrap(),
    ]));
    let exported: Value = serde_json::from_slice(
        &fs::read(out.join("copilot").join(format!("{SESSION_ID}.json"))).unwrap(),
    )
    .unwrap();
    assert!(exported.to_string().contains(FIRST_TOKEN));

    let index = fixture.json(&["index", "--json", "--full-rescan", "--cache-extracts"]);
    assert_eq!(index["sources_parsed"], 1, "{index}");
    assert!(index["lexical_docs"].as_u64().unwrap() > 0);
    let hits = search(&fixture, FIRST_TOKEN, "copilot");
    let items = hits["items"].as_array().unwrap();
    assert!(!items.is_empty(), "source lexical search: {hits}");
    assert!(
        items
            .iter()
            .any(|item| item.to_string().contains(FIRST_TOKEN)),
        "{hits}"
    );
    assert!(
        items
            .iter()
            .all(|item| item["agent"] == "copilot" && item["session_id"] == SESSION_ID),
        "search provenance: {hits}"
    );
    assert!(
        search(&fixture, FIRST_TOKEN, "claude")["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        search(&fixture, TOOL_TOKEN, "copilot")["items"]
            .as_array()
            .unwrap()
            .is_empty(),
        "tool-only payload stays out of signal index"
    );

    {
        let mut mcp = CopilotMcp::new(&fixture);
        let listed = mcp.tool("aicx_sessions", json!({"agent": "copilot", "hours": 0}));
        assert_eq!(listed["matched"], 1, "{listed}");
        assert_eq!(listed["sessions"][0]["session_id"], SESSION_ID);
        let shown = mcp.tool(
            "aicx_session",
            json!({"agent": "copilot", "session": SESSION_ID, "conversation": true}),
        );
        assert!(shown.to_string().contains(FIRST_TOKEN), "{shown}");
        let searched = mcp.tool(
            "aicx_search",
            json!({"query": FIRST_TOKEN, "agent": "copilot", "hours": 0, "slim": false}),
        );
        assert!(
            searched["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.to_string().contains(FIRST_TOKEN)),
            "{searched}"
        );
        let reference = items[0]["path"]
            .as_str()
            .or_else(|| items[0]["reference"].as_str())
            .expect("search result readable reference");
        let read = mcp.tool("aicx_read", json!({"reference": reference}));
        assert!(read.to_string().contains(FIRST_TOKEN), "{read}");
        assert_eq!(read["agent"], "copilot");
        assert_eq!(read["session_id"], SESSION_ID);
        let native_read = mcp.tool("aicx_read", json!({"reference": fixture.events}));
        assert!(
            native_read["content"]
                .as_str()
                .unwrap()
                .contains(FIRST_TOKEN),
            "catalog-admitted native source is readable: {native_read}"
        );
        assert!(
            !native_read["content"]
                .as_str()
                .unwrap()
                .contains(TOOL_TOKEN),
            "native read projects conversational signal: {native_read}"
        );
        assert_eq!(native_read["agent"], "copilot");
        assert_eq!(native_read["session_id"], SESSION_ID);

        // Corrupt only the existing derived cache. The API must verify its
        // hash and recover content through the catalog's live source reader.
        const POISON: &str = "poisonedcachetoken91aefd";
        fs::write(reference, POISON).unwrap();
        let recovered = mcp.tool("aicx_read", json!({"reference": reference}));
        assert!(
            recovered["content"].as_str().unwrap().contains(FIRST_TOKEN),
            "tampered cache falls back to verified source: {recovered}"
        );
        assert!(
            !recovered["content"].as_str().unwrap().contains(POISON),
            "derived cache cannot override source truth: {recovered}"
        );
        assert!(!recovered["content"].as_str().unwrap().contains(TOOL_TOKEN));
        let truncated = mcp.tool(
            "aicx_read",
            json!({"reference": reference, "max_chars": 32}),
        );
        assert_eq!(truncated["truncated"], true, "{truncated}");
        assert_eq!(truncated["content"].as_str().unwrap().chars().count(), 32);
        assert_eq!(truncated["session_id"], SESSION_ID);
        let mcp_intents = mcp.tool(
            "aicx_intents",
            json!({
                "agent": "copilot", "hours": 0, "emit": "json", "slim": false, "limit": 100
            }),
        );
        let intent_items = mcp_intents["items"].as_array().unwrap();
        assert!(
            intent_items
                .iter()
                .any(|item| item.to_string().contains(FIRST_TOKEN)),
            "human evidence survives parent task provenance: {mcp_intents}"
        );
        assert!(
            intent_items
                .iter()
                .all(|item| item["agent"] == "copilot" && item["session_id"] == SESSION_ID),
            "MCP intent provenance: {mcp_intents}"
        );
        assert!(
            !intent_items
                .iter()
                .any(|item| item.to_string().contains(TOOL_TOKEN)),
            "tool output is not human intent: {mcp_intents}"
        );
        let continuity = mcp.tool(
            "aicx_continuity",
            json!({
                "project": resolved["project"], "hours": 720
            }),
        );
        assert_eq!(continuity["ok"], true, "{continuity}");
        assert!(
            continuity["markdown"]
                .as_str()
                .unwrap()
                .contains(FIRST_TOKEN),
            "Copilot human decisions enter the continuity pack: {continuity}"
        );
    }

    fixture.append();
    let refreshed = fixture.json(&["index", "--json", "--cache-extracts"]);
    assert_eq!(
        refreshed["sources_parsed"], 1,
        "live append re-parses the changed source: {refreshed}"
    );
    assert_ne!(refreshed["unchanged"], true);
    let refreshed_extract = fs::read_to_string(items[0]["path"].as_str().unwrap()).unwrap();
    assert!(
        refreshed_extract.contains(APPEND_TOKEN),
        "refreshed source-index extract must retain append; index: {refreshed}\nextract:\n{refreshed_extract}"
    );
    let appended_search = search(&fixture, APPEND_TOKEN, "copilot");
    assert!(
        appended_search["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.to_string().contains(APPEND_TOKEN)),
        "same-timestamp append must be searchable: {appended_search}"
    );
    let appended_bulk = fixture.json(&["extract", "all", "--json"]);
    assert_eq!(
        appended_bulk["totals"]["extracted"], 1,
        "same-timestamp append cannot be skipped: {appended_bulk}"
    );
    let resolved_after = fixture.json(&["catalog", "resolve", SESSION_ID, "--json"]);
    assert_eq!(resolved_after["session_id"], resolved["session_id"]);
    assert_eq!(resolved_after["source_path"], resolved["source_path"]);
    let appended_intents = fixture.json(&[
        "intents",
        "--hours",
        "0",
        "--agent",
        "copilot",
        "--no-live",
        "--emit",
        "json",
    ]);
    assert!(
        appended_intents.to_string().contains(APPEND_TOKEN),
        "appended human intent is retained: {appended_intents}"
    );
    // A future event mtime must not hide same-size metadata changes, even
    // when the workspace editor preserves the sidecar's original mtime.
    let workspace = fixture.events.with_file_name("workspace.yaml");
    let metadata_time = filetime::FileTime::from_unix_time(1_800_000_000, 0);
    filetime::set_file_mtime(
        &fixture.events,
        filetime::FileTime::from_unix_time(2_000_000_000, 0),
    )
    .unwrap();
    filetime::set_file_mtime(&workspace, metadata_time).unwrap();
    let baseline_bulk = fixture.json(&["extract", "all", "--json"]);
    assert_eq!(baseline_bulk["totals"]["extracted"], 1, "{baseline_bulk}");
    let baseline_index = fixture.json(&["index", "--json", "--cache-extracts"]);
    assert_eq!(baseline_index["sources_parsed"], 1, "{baseline_index}");
    let old_yaml = fs::read_to_string(&workspace).unwrap();
    let new_yaml = old_yaml.replace("Synthetic Copilot pipeline", "Synthetic Copilot metadata");
    assert_eq!(old_yaml.len(), new_yaml.len());
    fs::write(&workspace, new_yaml).unwrap();
    filetime::set_file_mtime(&workspace, metadata_time).unwrap();
    let metadata_bulk = fixture.json(&["extract", "all", "--json"]);
    assert_eq!(
        metadata_bulk["totals"]["extracted"], 1,
        "sidecar edit invalidates bulk reuse: {metadata_bulk}"
    );
    let metadata_index = fixture.json(&["index", "--json", "--cache-extracts"]);
    assert_eq!(
        metadata_index["sources_parsed"], 1,
        "sidecar edit invalidates index reuse: {metadata_index}"
    );
    let resolved_metadata = fixture.json(&["catalog", "resolve", SESSION_ID, "--json"]);
    assert_eq!(
        resolved_metadata["title"], "Synthetic Copilot metadata",
        "{resolved_metadata}"
    );
    let warm_metadata_bulk = fixture.json(&["extract", "all", "--json"]);
    assert_eq!(
        warm_metadata_bulk["totals"]["unchanged"], 1,
        "{warm_metadata_bulk}"
    );
    assert!(
        !fixture.home.join(".aicx/store").exists(),
        "source pipeline never recreates per-frame cards"
    );
}

#[test]
fn copilot_custom_home_is_discovered_extracted_and_indexed() {
    const NAMED_SESSION: &str = "user-123-task-456";
    let mut fixture = CopilotFixture::new();
    let custom_home = fixture.home.join("custom-copilot");
    fs::rename(&fixture.copilot_home, &custom_home).unwrap();
    fixture.copilot_home = custom_home;
    let source_dir = fixture
        .copilot_home
        .join("session-state")
        .join(NAMED_SESSION);
    fs::rename(
        fixture.copilot_home.join("session-state").join(SESSION_ID),
        &source_dir,
    )
    .unwrap();
    fixture.events = source_dir.join("events.jsonl");
    for path in [&fixture.events, &source_dir.join("workspace.yaml")] {
        let body = fs::read_to_string(path)
            .unwrap()
            .replace(SESSION_ID, NAMED_SESSION);
        fs::write(path, body).unwrap();
    }
    assert!(!fixture.home.join(".copilot").exists());

    let catalog = fixture.json(&["catalog", "rebuild", "--json"]);
    assert_eq!(
        catalog["agents"]["copilot"], 1,
        "custom source root: {catalog}"
    );
    let resolved = fixture.json(&["catalog", "resolve", NAMED_SESSION, "--json"]);
    assert_eq!(resolved["agent"], "copilot");
    assert_eq!(resolved["session_id"], NAMED_SESSION);
    assert!(
        PathBuf::from(resolved["source_path"].as_str().unwrap()).starts_with(&fixture.copilot_home)
    );
    let output = fixture.home.join("custom-conversation.md");
    assert_success(&fixture.run(&[
        "extract",
        "copilot",
        "--session",
        NAMED_SESSION,
        "--conversation",
        "--output",
        output.to_str().unwrap(),
    ]));
    assert!(fs::read_to_string(output).unwrap().contains(FIRST_TOKEN));
    let listed = fixture.json(&[
        "sessions",
        "list",
        "--agent",
        "copilot-cli",
        "--all",
        "--json",
    ]);
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["agent"], "copilot");
    assert_eq!(listed[0]["session_id"], NAMED_SESSION);
    let index = fixture.json(&["index", "--json", "--cache-extracts"]);
    assert_eq!(
        index["sources_parsed"], 1,
        "allowlisted custom root: {index}"
    );
    let hits = search(&fixture, FIRST_TOKEN, "github-copilot-cli");
    assert!(
        hits["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["agent"] == "copilot"
                && item["session_id"] == NAMED_SESSION
                && item.to_string().contains(FIRST_TOKEN)),
        "canonical alias search: {hits}"
    );
}

#[test]
fn legacy_all_watermark_does_not_skip_the_new_copilot_provider() {
    let fixture = CopilotFixture::new();
    let aicx_home = fixture.home.join(".aicx");
    fs::create_dir_all(&aicx_home).unwrap();
    let older_provider_key = "claude+codescribe+codex+cursor+gemini+grok+junie+kimi:all";
    let old_watermark = chrono::DateTime::parse_from_rfc3339("2026-09-30T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut state = aicx::state::StateManager::default();
    state.update_watermark(older_provider_key, old_watermark);
    fs::write(
        aicx_home.join("state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();

    // All Copilot events predate this watermark, which covers incumbent
    // providers only. Upgrade must ingest Copilot without --full-rescan.
    let ingested = fixture.json(&["all", "-H", "0", "--emit", "json"]);
    let entries = ingested["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|entry| entry["message"]
            .as_str()
            .is_some_and(|message| message.contains(FIRST_TOKEN))),
        "new provider cannot inherit an incumbent watermark: {ingested}"
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry["agent"] == "copilot" && entry["session_id"] == SESSION_ID),
        "legacy all retains provider/session provenance: {ingested}"
    );
    let migrated: Value =
        serde_json::from_slice(&fs::read(aicx_home.join("state.json")).unwrap()).unwrap();
    assert_eq!(
        migrated["last_processed"]["claude+codescribe+codex+copilot+cursor+gemini+grok+junie+kimi:all"],
        "2026-09-30T00:00:00Z",
        "incumbent watermark does not move backwards: {migrated}"
    );
    assert!(
        migrated["runs"].as_array().unwrap().last().unwrap()["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source == "copilot")
    );

    let warm = fixture.json(&["all", "-H", "0", "--emit", "json"]);
    assert_eq!(
        warm["total_entries"], 0,
        "unchanged Copilot content is deduplicated: {warm}"
    );
    assert!(warm["entries"].as_array().unwrap().is_empty());

    fixture.append();
    let appended = fixture.json(&["all", "-H", "0", "--emit", "json"]);
    let entries = appended["entries"].as_array().unwrap();
    assert_eq!(
        entries.len(),
        2,
        "same-timestamp append returns only its human/assistant pair: {appended}"
    );
    assert!(
        entries.iter().any(|entry| entry["message"]
            .as_str()
            .is_some_and(|message| message.contains(APPEND_TOKEN))),
        "content dedup admits unseen events even behind the watermark: {appended}"
    );
    assert!(
        !entries.iter().any(|entry| entry["message"]
            .as_str()
            .is_some_and(|message| message.contains(FIRST_TOKEN))),
        "old source content is not duplicated: {appended}"
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry["agent"] == "copilot" && entry["session_id"] == SESSION_ID)
    );
}
