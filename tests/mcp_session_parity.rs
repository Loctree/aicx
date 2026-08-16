// App-only integration surface.
#![cfg(feature = "app")]

//! CLI `aicx sessions list --json` and MCP `aicx_sessions` share discovery.
//! This test pins that parity on an isolated HOME and checks the loud-empty
//! project signal the MCP surface adds on top.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_home(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-mcp-session-parity-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

fn write_claude(home: &Path, encoded_cwd: &str, session_id: &str, cwd: &str, user: &str) {
    let dir = home.join(".claude").join("projects").join(encoded_cwd);
    fs::create_dir_all(&dir).expect("claude project");
    fs::write(
        dir.join(format!("{session_id}.jsonl")),
        format!(
            concat!(
                r#"{{"type":"user","cwd":"{cwd}","sessionId":"{sid}","message":{{"role":"user","content":"{user}"}},"timestamp":"2026-08-16T12:00:00.000Z"}}"#,
                "\n",
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"ok"}}]}},"timestamp":"2026-08-16T12:00:01.000Z"}}"#,
                "\n"
            ),
            cwd = cwd,
            sid = session_id,
            user = user
        ),
    )
    .expect("write session");
}

#[test]
fn mcp_list_without_project_matches_cli_session_ids() {
    let home = unique_home("cli");
    write_claude(
        &home,
        "-Volumes-vc-workspace-Loctree-aicx",
        "dddddddd-1111-2222-3333-444444444444",
        "/Volumes/vc-workspace/Loctree/aicx",
        "parity",
    );
    let cli = Command::new(env!("CARGO_BIN_EXE_aicx"))
        .env("HOME", &home)
        .env("AICX_NO_MUTATION_WARN", "1")
        .args(["sessions", "list", "--json", "--all"])
        .output()
        .expect("run aicx sessions list");
    assert!(
        cli.status.success(),
        "cli failed: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli_json: Value = serde_json::from_slice(&cli.stdout).expect("cli json");
    let cli_ids: Vec<&str> = cli_json
        .as_array()
        .expect("cli array")
        .iter()
        .filter_map(|row| row.get("session_id").and_then(Value::as_str))
        .collect();

    let aicx_home = home.join(".aicx");
    fs::create_dir_all(&aicx_home).expect("aicx home");
    let mcp = aicx::mcp_session::list_sessions(aicx::mcp_session::ListSessionsRequest {
        user_home: &home,
        aicx_home: &aicx_home,
        project: None,
        projects: &[],
        project_match: aicx::legacy_archive::ProjectMatchMode::Exact,
        agent: None,
        hours: 0,
        since: None,
        limit: 0,
    })
    .expect("mcp list");
    let mcp_ids: Vec<&str> = mcp
        .sessions
        .iter()
        .map(|session| session.session_id.as_str())
        .collect();
    assert_eq!(cli_ids, mcp_ids);
    let _ = fs::remove_dir_all(home);
}
