use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-frame-kind-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, content).expect("write file");
}

fn write_codex_session(path: &Path, cwd: &Path) {
    let now = Utc::now();
    let lines = [
        json!({
            "timestamp": (now - chrono::Duration::seconds(4)).to_rfc3339_opts(SecondsFormat::Secs, true),
            "type": "session_meta",
            "payload": {
                "id": "frame-kind-contract",
                "cwd": cwd.display().to_string(),
            }
        })
        .to_string(),
        json!({
            "timestamp": (now - chrono::Duration::seconds(4)).to_rfc3339_opts(SecondsFormat::Secs, true),
            "type": "turn_context",
            "payload": {
                "cwd": cwd.display().to_string(),
            }
        })
        .to_string(),
        json!({
            "timestamp": (now - chrono::Duration::seconds(3)).to_rfc3339_opts(SecondsFormat::Secs, true),
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": "User asks for frame separation",
            }
        })
        .to_string(),
        json!({
            "timestamp": (now - chrono::Duration::seconds(2)).to_rfc3339_opts(SecondsFormat::Secs, true),
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "message": "Visible assistant reply",
            }
        })
        .to_string(),
        json!({
            "timestamp": (now - chrono::Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
            "type": "event_msg",
            "payload": {
                "type": "thinking_delta",
                "text": "Hidden chain of thought",
            }
        })
        .to_string(),
        json!({
            "timestamp": now.to_rfc3339_opts(SecondsFormat::Secs, true),
            "type": "event_msg",
            "payload": {
                "type": "tool_call",
                "message": "searchDocs({\"query\":\"frame_kind\"})",
            }
        })
        .to_string(),
    ];

    write_file(path, &lines.join("\n"));
}

fn write_claude_session_with_empty_signature(path: &Path, cwd: &Path) {
    let now = Utc::now();
    let lines = [
        json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": "Please answer visibly",
            },
            "timestamp": (now - chrono::Duration::seconds(2)).to_rfc3339_opts(SecondsFormat::Secs, true),
            "sessionId": "claude-signature-contract",
            "gitBranch": "main",
            "cwd": cwd.display().to_string(),
        })
        .to_string(),
        json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "thinking",
                        "thinking": "",
                        "signature": "abc123",
                    },
                    {
                        "type": "text",
                        "text": "Visible Claude answer",
                    }
                ],
            },
            "timestamp": (now - chrono::Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
            "sessionId": "claude-signature-contract",
            "gitBranch": "main",
            "cwd": cwd.display().to_string(),
        })
        .to_string(),
    ];

    write_file(path, &lines.join("\n"));
}

fn current_profile_dir() -> PathBuf {
    let test_exe = std::env::current_exe().expect("resolve current test executable");
    test_exe
        .parent()
        .and_then(Path::parent)
        .expect("resolve cargo profile dir")
        .to_path_buf()
}

fn fallback_aicx_path() -> PathBuf {
    let mut path = current_profile_dir().join("aicx");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

fn ensure_aicx_binary_exists() -> PathBuf {
    static BIN_PATH: OnceLock<PathBuf> = OnceLock::new();

    BIN_PATH
        .get_or_init(|| {
            if let Some(env_path) = std::env::var_os("CARGO_BIN_EXE_aicx").map(PathBuf::from)
                && env_path.is_file()
            {
                return env_path;
            }

            let env_path = PathBuf::from(env!("CARGO_BIN_EXE_aicx"));
            if env_path.is_file() {
                return env_path;
            }

            let fallback = fallback_aicx_path();
            if fallback.is_file() {
                return fallback;
            }

            let cargo = std::env::var_os("CARGO")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("cargo"));
            let output = Command::new(&cargo)
                .args(["build", "--locked", "--bin", "aicx"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .expect("build fallback aicx binary");

            assert!(
                output.status.success(),
                "fallback cargo build --bin aicx failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                fallback.is_file(),
                "fallback cargo build succeeded but binary missing at {}",
                fallback.display()
            );

            fallback
        })
        .clone()
}

fn run_aicx(home: &Path, args: &[&str]) -> Output {
    fs::create_dir_all(home).expect("create temp HOME");
    Command::new(ensure_aicx_binary_exists())
        .args(args)
        .env("HOME", home)
        .output()
        .expect("run aicx")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn parse_stdout_json(output: &Output) -> Value {
    assert_success(output);
    serde_json::from_slice(&output.stdout).expect("parse stdout json")
}

fn json_paths(value: &Value, key: &str) -> Vec<PathBuf> {
    value[key]
        .as_array()
        .expect("json array")
        .iter()
        .map(|path| {
            PathBuf::from(
                path.as_str()
                    .expect("string path in json payload")
                    .to_string(),
            )
        })
        .collect()
}

#[test]
fn codex_store_round_trips_frame_kind_filters() {
    let root = unique_test_dir("round-trip");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("VetCoders").join("ai-contexters");
    let history_path = home.join(".codex").join("history.jsonl");
    let session_path = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("04")
        .join("14")
        .join("rollout-frame-kind.jsonl");

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_file(&history_path, "");
    write_codex_session(&session_path, &repo_root);

    let store_output = run_aicx(&home, &["codex", "-H", "24", "--emit", "json"]);
    let payload = parse_stdout_json(&store_output);
    let store_paths = json_paths(&payload, "store_paths");
    assert_eq!(store_paths.len(), 4);

    let mut paths_by_frame = BTreeMap::new();
    for path in &store_paths {
        let sidecar: Value = serde_json::from_slice(
            &fs::read(path.with_extension("meta.json")).expect("read sidecar"),
        )
        .expect("parse sidecar json");
        let frame_kind = sidecar["frame_kind"]
            .as_str()
            .expect("frame kind in sidecar")
            .to_string();
        paths_by_frame.insert(frame_kind, path.clone());
    }

    assert_eq!(
        paths_by_frame.keys().cloned().collect::<Vec<_>>(),
        vec![
            "agent_reply".to_string(),
            "internal_thought".to_string(),
            "tool_call".to_string(),
            "user_msg".to_string(),
        ]
    );

    let search_output = run_aicx(
        &home,
        &[
            "search",
            "Hidden chain of thought",
            "--frame-kind",
            "internal_thought",
            "--json",
            "-p",
            "ai-contexters",
            // Test asserts the lexical frame-kind filter contract; the
            // hermetic test env has no embedder, so `--no-semantic`
            // explicitly opts into the fuzzy path. Production search
            // fails fast on missing embedder; this test does not.
            "--no-semantic",
        ],
    );
    let search_payload = parse_stdout_json(&search_output);
    assert_eq!(search_payload["results"].as_u64(), Some(1));
    assert_eq!(
        search_payload["items"][0]["frame_kind"].as_str(),
        Some("internal_thought")
    );
    let expected_thought_file = paths_by_frame["internal_thought"]
        .file_name()
        .expect("thought chunk filename")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        Path::new(
            search_payload["items"][0]["path"]
                .as_str()
                .expect("search result path"),
        )
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .as_deref(),
        Some(expected_thought_file.as_str())
    );

    #[cfg(feature = "lance")]
    {
        let steer_output = run_aicx(
            &home,
            &["steer", "-p", "ai-contexters", "--frame-kind", "user_msg"],
        );
        assert_success(&steer_output);
        let steer_stdout = String::from_utf8_lossy(&steer_output.stdout);

        let expected_user_file = paths_by_frame["user_msg"]
            .file_name()
            .expect("user chunk filename")
            .to_string_lossy()
            .into_owned();
        assert!(steer_stdout.contains(&expected_user_file));
        for unexpected in ["agent_reply", "internal_thought", "tool_call"] {
            let unexpected_path = paths_by_frame[unexpected]
                .file_name()
                .expect("unexpected chunk filename")
                .to_string_lossy()
                .into_owned();
            assert!(
                !steer_stdout.contains(&unexpected_path),
                "steer output leaked {unexpected} path: {steer_stdout}"
            );
        }
    }

    #[cfg(not(feature = "lance"))]
    {
        let steer_output = run_aicx(
            &home,
            &["steer", "-p", "ai-contexters", "--frame-kind", "user_msg"],
        );
        assert!(
            !steer_output.status.success(),
            "steer should fail when lance is disabled"
        );
        let steer_stderr = String::from_utf8_lossy(&steer_output.stderr);
        assert!(steer_stderr.contains("not enabled in this aicx build"));
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn claude_store_does_not_emit_empty_thinking_signature() {
    let root = unique_test_dir("claude-signature-store");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("VetCoders").join("aicx");
    let session_path = home
        .join(".claude")
        .join("projects")
        .join("-Users-test-hosted-VetCoders-aicx")
        .join("claude-signature-contract.jsonl");

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_claude_session_with_empty_signature(&session_path, &repo_root);

    let store_output = run_aicx(
        &home,
        &["store", "--agent", "claude", "-H", "24", "--emit", "json"],
    );
    let payload = parse_stdout_json(&store_output);
    let store_paths = json_paths(&payload, "store_paths");
    assert_eq!(store_paths.len(), 2);

    let mut saw_agent_reply = false;
    for path in &store_paths {
        let chunk = fs::read_to_string(path).expect("read stored chunk");
        assert!(!chunk.contains("signature"));
        assert!(!chunk.contains("abc123"));
        assert!(!chunk.contains(r#""type":"thinking""#));

        let sidecar: Value = serde_json::from_slice(
            &fs::read(path.with_extension("meta.json")).expect("read sidecar"),
        )
        .expect("parse sidecar");
        if sidecar["frame_kind"].as_str() == Some("agent_reply") {
            saw_agent_reply = true;
            assert!(chunk.contains("Visible Claude answer"));
        }
    }
    assert!(saw_agent_reply, "expected an agent_reply chunk");

    let input_arg = session_path.display().to_string();
    let conversation_path = root.join("conversation.md");
    let output_arg = conversation_path.display().to_string();
    let conversation_output = run_aicx(
        &home,
        &[
            "extract",
            "--format",
            "claude",
            &input_arg,
            "-o",
            &output_arg,
            "--conversation",
        ],
    );
    assert_success(&conversation_output);
    let conversation = fs::read_to_string(conversation_path).expect("read conversation markdown");
    assert!(conversation.contains("Visible Claude answer"));
    assert!(!conversation.contains(r#""signature""#));
    assert!(!conversation.contains("abc123"));

    let _ = fs::remove_dir_all(&root);
}
