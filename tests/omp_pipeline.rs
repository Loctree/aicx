#![cfg(feature = "app")]

//! Public CLI and MCP paths over synthetic oh-my-pi / pi session files.
//! Every subprocess gets a scratch HOME/AICX_HOME; no live source is read.

use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const OMP_ID: &str = "01999999-bbbb-7666-8666-111111111111";
const PI_ID: &str = "01999999-bbbb-7666-8666-222222222222";
const FIRST_TOKEN: &str = "omppipelineinitial91dbea";
const PI_TOKEN: &str = "omppipelinepiroot47fdae";
const APPEND_TOKEN: &str = "omppipelineappended47fdae";
const TOOL_TOKEN: &str = "omptoolonly893fed";
const EVENT_TIME: &str = "2026-09-29T12:00:00.000Z";
static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn isolated_test_command(binary: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(binary);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
}

struct OmpFixture {
    home: PathBuf,
    events: PathBuf,
}

impl OmpFixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!(
            "aicx-omp-pipeline-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let cwd = home.join("work").join("omp-pipeline");
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
                    "https://github.com/example/omp-pipeline.git"
                ])
                .status()
                .unwrap()
                .success()
        );
        let events = write_session(
            &home.join(".omp").join("agent").join("sessions"),
            OMP_ID,
            &cwd,
            &format!("Decision: keep lexical search for {FIRST_TOKEN}."),
            TOOL_TOKEN,
            true,
        );
        write_session(
            &home.join(".pi").join("agent").join("sessions"),
            PI_ID,
            &cwd,
            &format!("Decision: keep the upstream pi root for {PI_TOKEN}."),
            "pi-tool-not-indexed",
            false,
        );
        Self { home, events }
    }

    fn command(&self, binary: &str) -> Command {
        let mut command = isolated_test_command(binary);
        command
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
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
        let row = json!({
            "type": "message",
            "id": "u-append",
            "parentId": "a2",
            "timestamp": EVENT_TIME,
            "message": {
                "role": "user",
                "attribution": "user",
                "content": [{"type": "text", "text": format!("Decision: retain {APPEND_TOKEN} in the same session.")}],
                "timestamp": EVENT_TIME
            }
        });
        writeln!(source, "{row}").unwrap();
    }
}

impl Drop for OmpFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn write_session(
    root: &Path,
    id: &str,
    cwd: &Path,
    human: &str,
    tool_token: &str,
    with_decoys: bool,
) -> PathBuf {
    let encoded = root.join("encoded");
    fs::create_dir_all(&encoded).unwrap();
    let stem = format!("2026-09-29T12-00-00-000Z_{id}");
    let path = encoded.join(format!("{stem}.jsonl"));
    let cwd_text = cwd.display().to_string();
    let records = [
        json!({"type":"title","v":1,"title":"Omp pipeline","source":"user","updatedAt":EVENT_TIME,"pad":"pad"}),
        json!({"type":"session","version":3,"id":id,"timestamp":EVENT_TIME,"cwd":cwd_text}),
        json!({
            "type":"message","id":"u1","parentId":Value::Null,"timestamp":EVENT_TIME,
            "message":{"role":"user","attribution":"user","content":[{"type":"text","text":human}],"timestamp":EVENT_TIME}
        }),
        json!({
            "type":"message","id":"a1","parentId":"u1","timestamp":EVENT_TIME,
            "message":{
                "role":"assistant","model":"fixture-model",
                "content":[
                    {"type":"text","text":format!("I will implement {human}.")},
                    {"type":"toolCall","id":"call-bash","name":"bash","arguments":{"command":"cargo test -p aicx-parser","cwd":cwd_text}}
                ],
                "timestamp":EVENT_TIME
            }
        }),
        json!({
            "type":"message","id":"r1","parentId":"a1","timestamp":EVENT_TIME,
            "message":{
                "role":"toolResult","toolCallId":"call-bash","toolName":"bash","isError":false,
                "content":[{"type":"text","text":format!("test result: ok {tool_token}")}],
                "details":{"exitCode":0},
                "timestamp":EVENT_TIME
            }
        }),
        json!({
            "type":"custom_message","id":"cm1","parentId":"r1","timestamp":EVENT_TIME,
            "customType":"fleet-loct-context-card","content":"Injected context card.",
            "display":false,"attribution":"user"
        }),
        json!({
            "type":"message","id":"a2","parentId":"cm1","timestamp":EVENT_TIME,
            "message":{"role":"assistant","content":[{"type":"text","text":format!("Implemented {human}.")}],"timestamp":EVENT_TIME}
        }),
    ];
    fs::write(
        &path,
        records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .unwrap();
    if with_decoys {
        let nest = encoded.join(&stem);
        fs::create_dir_all(&nest).unwrap();
        fs::write(nest.join("1.bash.log"), "tool log, not a session\n").unwrap();
        fs::write(
            nest.join("nested.jsonl"),
            "{\"type\":\"session\",\"id\":\"not-a-session\"}\n",
        )
        .unwrap();
        fs::write(
            root.join(format!("{stem}.jsonl")),
            "{\"type\":\"session\",\"id\":\"loose\"}\n",
        )
        .unwrap();
    }
    path
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

fn extract(fixture: &OmpFixture, name: &str, agent: &str, flags: &[&str]) -> String {
    let path = fixture.home.join(name);
    let mut args = vec![
        "extract",
        agent,
        "--session",
        OMP_ID,
        "--output",
        path.to_str().unwrap(),
    ];
    args.extend_from_slice(flags);
    assert_success(&fixture.run(&args));
    fs::read_to_string(path).unwrap()
}

fn search(fixture: &OmpFixture, token: &str, agent: &str) -> Value {
    fixture.json(&["search", token, "--json", "--hours", "0", "--agent", agent])
}

struct OmpMcp {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<Value>,
    next_id: u64,
}

impl OmpMcp {
    fn new(fixture: &OmpFixture) -> Self {
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
                "clientInfo": {"name": "omp-pipeline-test", "version": "1"}
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

impl Drop for OmpMcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn omp_public_pipeline_discovers_both_roots_and_ignores_tool_logs() {
    let fixture = OmpFixture::new();
    let catalog = fixture.json(&["catalog", "rebuild", "--json"]);
    assert_eq!(
        catalog["agents"]["omp"], 2,
        "nested tool logs and a loose sessions/*.jsonl are not sessions: {catalog}"
    );
    assert_eq!(catalog["total_sessions"], 2, "{catalog}");

    let resolved = fixture.json(&["catalog", "resolve", OMP_ID, "--json"]);
    assert_eq!(resolved["agent"], "omp");
    assert_eq!(resolved["session_id"], OMP_ID);
    // Windows prints `\` separators; compare the store shape, not the platform.
    let source_path = |value: &Value| value["source_path"].as_str().unwrap().replace('\\', "/");
    assert!(source_path(&resolved).contains("/.omp/agent/sessions/"));
    let pi = fixture.json(&["catalog", "resolve", PI_ID, "--json"]);
    assert!(source_path(&pi).contains("/.pi/agent/sessions/"));

    for agent in ["omp", "pi", "oh-my-pi"] {
        let sessions = fixture.json(&["sessions", "list", "--all", "--agent", agent, "--json"]);
        let rows = sessions.as_array().expect("sessions list json is an array");
        assert_eq!(rows.len(), 2, "{agent}: {sessions}");
        assert!(rows.iter().all(|row| row["agent"] == "omp"));
        assert!(rows.iter().any(|row| row["session_id"] == OMP_ID));
        assert!(rows.iter().any(|row| row["session_id"] == PI_ID));
    }

    let conversation = extract(&fixture, "conversation.md", "omp", &["--conversation"]);
    assert!(conversation.contains(FIRST_TOKEN));
    let user = extract(
        &fixture,
        "user.md",
        "pi",
        &["--conversation", "--user-only"],
    );
    assert!(user.contains(FIRST_TOKEN));
    assert!(!user.contains("I will implement"), "{user}");
    assert!(!user.contains("Injected context card"), "{user}");
    assert!(!user.contains(TOOL_TOKEN), "{user}");
    let commands = extract(
        &fixture,
        "commands.md",
        "oh-my-pi",
        &["--agent-commands", "--result", "full"],
    );
    assert!(commands.contains("cargo test -p aicx-parser"), "{commands}");

    let intents = fixture.json(&[
        "intents",
        "--hours",
        "0",
        "--agent",
        "omp",
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
            .any(|item| item.to_string().contains(PI_TOKEN)),
        "pi root is the same agent: {intents}"
    );
    assert!(
        intent_items.iter().all(|item| item["agent"] == "omp"),
        "intent provenance: {intents}"
    );
    assert!(
        !intent_items
            .iter()
            .any(|item| item.to_string().contains(TOOL_TOKEN)),
        "shell output is not human intent: {intents}"
    );
    assert!(
        !intent_items
            .iter()
            .any(|item| item.to_string().contains("Injected context card")),
        "harness injection is not human intent: {intents}"
    );

    let bulk = fixture.json(&["extract", "all", "--json"]);
    assert_eq!(
        bulk["totals"]["extracted"], 2,
        "default provider registry includes omp: {bulk}"
    );

    let index = fixture.json(&["index", "--json", "--full-rescan", "--cache-extracts"]);
    assert_eq!(index["sources_parsed"], 2, "{index}");
    let hits = search(&fixture, FIRST_TOKEN, "omp");
    let items = hits["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["agent"] == "omp" && item["session_id"] == OMP_ID),
        "source lexical search: {hits}"
    );
    let alias_hits = search(&fixture, PI_TOKEN, "pi");
    assert!(
        alias_hits["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["agent"] == "omp" && item["session_id"] == PI_ID),
        "pi alias searches omp: {alias_hits}"
    );
    assert!(
        search(&fixture, TOOL_TOKEN, "oh-my-pi")["items"]
            .as_array()
            .unwrap()
            .is_empty(),
        "tool-only payload stays out of the signal index"
    );

    {
        let mut mcp = OmpMcp::new(&fixture);
        let listed = mcp.tool("aicx_sessions", json!({"agent": "pi", "hours": 0}));
        assert_eq!(listed["matched"], 2, "{listed}");
        let shown = mcp.tool(
            "aicx_session",
            json!({"agent": "omp", "session": OMP_ID, "conversation": true}),
        );
        assert!(shown.to_string().contains(FIRST_TOKEN), "{shown}");
        let searched = mcp.tool(
            "aicx_search",
            json!({"query": FIRST_TOKEN, "agent": "oh-my-pi", "hours": 0, "slim": false}),
        );
        assert!(
            searched["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["session_id"] == OMP_ID),
            "{searched}"
        );
        let reference = items[0]["path"]
            .as_str()
            .or_else(|| items[0]["reference"].as_str())
            .expect("search result readable reference");
        let read = mcp.tool("aicx_read", json!({"reference": reference}));
        assert!(read.to_string().contains(FIRST_TOKEN), "{read}");
        assert_eq!(read["agent"], "omp");
    }

    fixture.append();
    let refreshed = fixture.json(&["index", "--json", "--cache-extracts"]);
    assert_eq!(refreshed["sources_parsed"], 1, "{refreshed}");
    let appended = search(&fixture, APPEND_TOKEN, "omp");
    assert!(
        appended["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["session_id"] == OMP_ID),
        "same-timestamp append must be searchable: {appended}"
    );
}

#[test]
fn legacy_all_watermark_does_not_skip_the_new_omp_provider() {
    let fixture = OmpFixture::new();
    let aicx_home = fixture.home.join(".aicx");
    fs::create_dir_all(&aicx_home).unwrap();
    let older_provider_key = "claude+codescribe+codex+copilot+cursor+gemini+grok+junie+kimi:all";
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

    let ingested = fixture.json(&["all", "-H", "0", "--emit", "json"]);
    let entries = ingested["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|entry| entry["message"]
            .as_str()
            .is_some_and(|message| message.contains(FIRST_TOKEN))),
        "new provider cannot inherit an incumbent watermark: {ingested}"
    );
    assert!(
        entries.iter().all(|entry| entry["agent"] == "omp"),
        "legacy all retains provider provenance: {ingested}"
    );
    let migrated: Value =
        serde_json::from_slice(&fs::read(aicx_home.join("state.json")).unwrap()).unwrap();
    assert_eq!(
        migrated["last_processed"]["agy+claude+codescribe+codex+copilot+cursor+gemini+grok+junie+kimi+omp:all"],
        "2026-09-30T00:00:00Z",
        "incumbent watermark does not move backwards: {migrated}"
    );
    assert!(
        migrated["runs"].as_array().unwrap().last().unwrap()["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source == "omp")
    );
}
