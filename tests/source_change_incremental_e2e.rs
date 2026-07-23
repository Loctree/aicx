// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! Audit P0 regression (audi-260723-115708-81000):
//!
//! Incremental index must re-parse when an EXISTING session source changes
//! (append a unique frame), not only when a new session id appears.
//!
//! Flow: publish → append unique user frame → catalog rebuild → second index
//! parses exactly that one source → token is searchable.

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CODEX_SESSION_ID: &str = "019c578c-source-change-marble-l1-0001";
const INITIAL_TOKEN: &str = "SOURCE_CHANGE_INITIAL_TOKEN_marble_a1b2c3";
const APPENDED_TOKEN: &str = "AUDIT_CHANGED_SESSION_ONLY_7f4d9c31_marble_l1";

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-source-change-{label}-{}-{}",
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
    fs::write(path, content).expect("write fixture");
}

fn run_aicx(home: &Path, args: &[&str]) -> Output {
    fs::create_dir_all(home).expect("create HOME");
    Command::new(env!("CARGO_BIN_EXE_aicx"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AICX_ALLOW_TMP", "1")
        .env_remove("AICX_HOME")
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

/// Real Codex layout under `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`.
fn seed_codex_session(home: &Path) -> PathBuf {
    let path = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("23")
        .join(format!("rollout-{CODEX_SESSION_ID}.jsonl"));
    let body = [
        json!({
            "timestamp": "2026-07-23T10:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": CODEX_SESSION_ID,
                "timestamp": "2026-07-23T10:00:00Z",
                "cwd": "/Volumes/vc-workspace/vetcoders/vibecrafted",
                "model": "gpt-test"
            }
        })
        .to_string(),
        json!({
            "timestamp": "2026-07-23T10:00:01Z",
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": format!("Initial context with {INITIAL_TOKEN}")
            }
        })
        .to_string(),
        json!({
            "timestamp": "2026-07-23T10:00:02Z",
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "message": format!("Acknowledged {INITIAL_TOKEN} baseline.")
            }
        })
        .to_string(),
    ]
    .join("\n");
    write_file(&path, &format!("{body}\n"));
    path
}

fn append_codex_user_frame(path: &Path, token: &str) {
    // Ensure mtime moves even on filesystems with coarse resolution.
    thread::sleep(Duration::from_millis(20));
    let line = json!({
        "timestamp": "2026-07-23T11:00:00Z",
        "type": "event_msg",
        "payload": {
            "type": "user_message",
            "message": format!("Appended unique frame {token}")
        }
    })
    .to_string();
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open codex session for append");
    writeln!(file, "{line}").expect("append frame");
    file.sync_all().expect("sync append");
}

#[test]
fn source_append_invalidates_incremental_and_indexes_new_token() {
    let root = unique_root("append");
    let home = root.join("home");
    let source_path = seed_codex_session(&home);

    // 1) Catalog + first publish with extract cache (builds parse ledger).
    let catalog1 = run_aicx(&home, &["catalog", "rebuild", "--json"]);
    assert_success(&catalog1, "catalog rebuild initial");
    let cat1 = parse_json(&catalog1);
    assert_eq!(cat1["cards_written"].as_u64(), Some(0));
    assert!(
        cat1["total_sessions"].as_u64().unwrap_or(0) >= 1,
        "catalog must list codex session: {cat1}"
    );

    let index1 = run_aicx(
        &home,
        &["index", "--json", "--full-rescan", "--cache-extracts"],
    );
    assert_success(&index1, "index initial publish");
    let rep1 = parse_json(&index1);
    assert_eq!(rep1["unchanged"].as_bool(), Some(false));
    assert!(
        rep1["sources_parsed"].as_u64().unwrap_or(0) >= 1,
        "initial publish must parse: {rep1}"
    );

    let search_initial = run_aicx(
        &home,
        &[
            "search",
            INITIAL_TOKEN,
            "--json",
            "--limit",
            "5",
            "--hours",
            "0",
        ],
    );
    assert_success(&search_initial, "search initial token");
    assert!(
        parse_json(&search_initial)
            .to_string()
            .contains(INITIAL_TOKEN),
        "initial token must be findable"
    );

    // Immediate no-op: same catalog + same live fingerprints → unchanged.
    let noop = run_aicx(&home, &["index", "--json", "--cache-extracts"]);
    assert_success(&noop, "index no-op");
    let noop_rep = parse_json(&noop);
    assert_eq!(
        noop_rep["unchanged"].as_bool(),
        Some(true),
        "no-op must short-circuit: {noop_rep}"
    );
    assert_eq!(noop_rep["sources_parsed"].as_u64(), Some(0));

    // 2) Append unique frame to the SAME source file.
    append_codex_user_frame(&source_path, APPENDED_TOKEN);

    // 3) Catalog rebuild must re-stat and change the row fingerprint.
    let catalog2 = run_aicx(&home, &["catalog", "rebuild", "--json"]);
    assert_success(&catalog2, "catalog rebuild after append");

    // 4) Second index must re-parse the changed source (not short-circuit).
    let index2 = run_aicx(&home, &["index", "--json", "--cache-extracts"]);
    assert_success(&index2, "index after source change");
    let rep2 = parse_json(&index2);
    assert_eq!(
        rep2["unchanged"].as_bool(),
        Some(false),
        "source-change must not report unchanged: {rep2}"
    );
    assert_eq!(
        rep2["sources_parsed"].as_u64(),
        Some(1),
        "exactly one changed source must reparse: {rep2}"
    );

    // 5) Appended token must be searchable in CURRENT.
    let search_appended = run_aicx(
        &home,
        &[
            "search",
            APPENDED_TOKEN,
            "--json",
            "--limit",
            "5",
            "--hours",
            "0",
        ],
    );
    assert_success(&search_appended, "search appended token");
    let hits = parse_json(&search_appended);
    assert!(
        hits.to_string().contains(APPENDED_TOKEN),
        "appended unique token must surface in CURRENT search: {hits}"
    );

    println!(
        "source-change-ok initial_parsed={} after_parsed={} noop_unchanged=true token_hit=true",
        rep1["sources_parsed"], rep2["sources_parsed"]
    );

    let _ = fs::remove_dir_all(&root);
}

/// Live-fingerprint path: append without catalog rebuild must still reparse
/// and surface the new token. Catalog lag must not hide source-change.
#[test]
fn source_append_without_catalog_rebuild_still_indexes_token() {
    let root = unique_root("live-only");
    let home = root.join("home");
    let source_path = seed_codex_session(&home);

    let catalog1 = run_aicx(&home, &["catalog", "rebuild", "--json"]);
    assert_success(&catalog1, "catalog rebuild initial");
    let index1 = run_aicx(
        &home,
        &["index", "--json", "--full-rescan", "--cache-extracts"],
    );
    assert_success(&index1, "index initial publish");

    // Append only — do NOT rebuild catalog (stale catalog fingerprints).
    const LIVE_TOKEN: &str = "LIVE_FP_NO_CATALOG_REBUILD_9e2a1b40_marble_l2";
    append_codex_user_frame(&source_path, LIVE_TOKEN);

    let index2 = run_aicx(&home, &["index", "--json", "--cache-extracts"]);
    assert_success(&index2, "index after live append without catalog rebuild");
    let rep2 = parse_json(&index2);
    assert_eq!(
        rep2["unchanged"].as_bool(),
        Some(false),
        "live append must not short-circuit: {rep2}"
    );
    assert_eq!(
        rep2["sources_parsed"].as_u64(),
        Some(1),
        "exactly the changed source must reparse: {rep2}"
    );

    let search = run_aicx(
        &home,
        &[
            "search", LIVE_TOKEN, "--json", "--limit", "5", "--hours", "0",
        ],
    );
    assert_success(&search, "search live-only token");
    assert!(
        parse_json(&search).to_string().contains(LIVE_TOKEN),
        "token from live-only append must be searchable"
    );

    // Second pass with no further append must short-circuit on live digest.
    let noop = run_aicx(&home, &["index", "--json", "--cache-extracts"]);
    assert_success(&noop, "index no-op after live reparse");
    let noop_rep = parse_json(&noop);
    assert_eq!(
        noop_rep["unchanged"].as_bool(),
        Some(true),
        "no-op after live reparse: {noop_rep}"
    );

    println!(
        "live-only-ok after_parsed={} noop_unchanged=true token_hit=true",
        rep2["sources_parsed"]
    );

    let _ = fs::remove_dir_all(&root);
}
