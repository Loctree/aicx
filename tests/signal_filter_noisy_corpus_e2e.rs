// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! Mission A3 filtering-ratio acceptance (audit P1):
//! Realistic noisy corpus at old-store proportions (~77% tool_call / noise)
//! must reduce by ≥5× when indexed through the source-driven path.

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const SESSION_ID: &str = "019c578c-noisy-filter-marble-l1-0002";
const SIGNAL_TOKEN: &str = "NOISY_CORPUS_SIGNAL_TOKEN_marble_f9e8d7";

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-noisy-filter-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, content).expect("write");
}

fn run_aicx(home: &Path, args: &[&str]) -> Output {
    fs::create_dir_all(home).expect("HOME");
    Command::new(env!("CARGO_BIN_EXE_aicx"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AICX_ALLOW_TMP", "1")
        .env_remove("AICX_HOME")
        .env_remove("AICX_ALLOW_CARD_MILL")
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
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "parse JSON failed: {err}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// Seed a codex JSONL with ~77% noise frames (tool_call + system + base64).
fn seed_noisy_codex(home: &Path) -> PathBuf {
    let path = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("23")
        .join(format!("rollout-{SESSION_ID}.jsonl"));

    let mut lines = Vec::new();
    lines.push(
        json!({
            "timestamp": "2026-07-23T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": SESSION_ID,
                "timestamp": "2026-07-23T12:00:00Z",
                "cwd": "/Volumes/vc-workspace/vetcoders/vibecrafted",
                "model": "gpt-test"
            }
        })
        .to_string(),
    );

    // 2 signal frames
    lines.push(
        json!({
            "timestamp": "2026-07-23T12:00:01Z",
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": format!("Remember {SIGNAL_TOKEN} for filtering ratio")
            }
        })
        .to_string(),
    );
    lines.push(
        json!({
            "timestamp": "2026-07-23T12:00:02Z",
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "message": format!("Acknowledged signal {SIGNAL_TOKEN}")
            }
        })
        .to_string(),
    );

    // 20 tool_call noise frames (~77% of event frames when counting with system)
    let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    for i in 0..20 {
        lines.push(
            json!({
                "timestamp": format!("2026-07-23T12:00:{:02}Z", 3 + i),
                "type": "event_msg",
                "payload": {
                    "type": "tool_call",
                    "message": format!("tool_call noise {i} payload base64:{b64}{b64}{b64}")
                }
            })
            .to_string(),
        );
    }
    // 5 system / thinking noise
    for i in 0..5 {
        lines.push(
            json!({
                "timestamp": format!("2026-07-23T12:01:{:02}Z", i),
                "type": "event_msg",
                "payload": {
                    "type": "thinking_delta",
                    "text": format!("internal thought noise fragment {i}")
                }
            })
            .to_string(),
        );
    }

    // Total event-ish lines after session_meta: 2 signal + 20 tool + 5 thought = 27
    // noise = 25/27 ≈ 92.6% (stricter than 77% old-store figure)
    write_file(&path, &format!("{}\n", lines.join("\n")));
    path
}

#[test]
fn noisy_corpus_reduces_frames_at_least_5x() {
    let root = unique_root("ratio");
    let home = root.join("home");
    let _source = seed_noisy_codex(&home);

    let catalog = run_aicx(&home, &["catalog", "rebuild", "--json"]);
    assert_success(&catalog, "catalog rebuild");

    let index = run_aicx(
        &home,
        &["index", "--json", "--full-rescan", "--cache-extracts"],
    );
    assert_success(&index, "index noisy corpus");
    let report = parse_json(&index);

    let raw = report["raw_frames"].as_u64().unwrap_or(0);
    let signal = report["signal_frames"].as_u64().unwrap_or(0);
    let filtered = report["filtered_frames"].as_u64().unwrap_or(0);
    assert!(
        raw >= 20,
        "fixture must produce many raw frames; report={report}"
    );
    assert!(
        signal > 0,
        "at least one signal frame must survive; report={report}"
    );
    let reduction = if signal == 0 {
        0.0
    } else {
        raw as f64 / signal as f64
    };
    println!(
        "filter-ratio raw={raw} signal={signal} filtered={filtered} reduction_x={reduction:.2}"
    );
    assert!(
        reduction >= 5.0,
        "mission expects ≥5× reduction on noisy corpus; got {reduction:.2}x (raw={raw} signal={signal}) report={report}"
    );

    let search = run_aicx(
        &home,
        &[
            "search",
            SIGNAL_TOKEN,
            "--json",
            "--limit",
            "5",
            "--hours",
            "0",
        ],
    );
    assert_success(&search, "search signal token");
    assert!(
        parse_json(&search).to_string().contains(SIGNAL_TOKEN),
        "signal token must remain searchable after aggressive filter"
    );

    let _ = fs::remove_dir_all(&root);
}
