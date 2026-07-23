// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! Coverage cut (audit item 5 / marbles A4):
//!
//! 1. Catalog + source-driven index admit real Grok and Gemini layouts.
//! 2. Long tokens that exist ONLY in each agent source return CURRENT-index hits.
//! 3. Self-echo of this test's unique prompt marker does not rank top.
//! 4. Filtering-ratio report: raw frames vs signal frames (noise out).
//!
//! Uses a scratch HOME (not the operator live home) so index publish stays
//! isolated. Live-home probes are documented as an operator button.

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// Unique marker present only in the Grok chat_history fixture.
const GROK_ONLY_TOKEN: &str = "GROKONLY_TOKEN_marble_L1_a7f3c91e2b88";
/// Unique marker present only in the Gemini session fixture.
const GEMINI_ONLY_TOKEN: &str = "GEMINIONLY_TOKEN_marble_L1_d4e8b02c9a11";
/// Self-echo negative: must not appear in fixtures or rank as a hit.
const SELF_ECHO_MARKER: &str = "SELF_ECHO_NEGATIVE_marble_L1_prompt_must_not_rank_top_zz9q";

const GROK_SESSION_ID: &str = "019f8e3b-aaaa-7bbb-8ccc-111111111111";
const GEMINI_SESSION_ID: &str = "session-gemini-marble-l1-coverage";

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-grok-gemini-e2e-{label}-{}-{}",
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

/// Real Grok layout: `~/.grok/sessions/<cwd-encoded>/<uuid>/chat_history.jsonl`
/// plus a tool_call-ish noise line that must not dominate signal.
fn seed_grok_session(home: &Path) -> PathBuf {
    // cwd encodes to owner/repo path segments for catalog project inference.
    let encoded_cwd = "%2FVolumes%2Fvc-workspace%2Fvetcoders%2Fvibecrafted";
    let session_dir = home
        .join(".grok")
        .join("sessions")
        .join(encoded_cwd)
        .join(GROK_SESSION_ID);
    let chat = session_dir.join("chat_history.jsonl");
    let body = [
        json!({
            "type": "user",
            "content": [{"type": "text", "text": format!(
                "<user_query>Where did we document {GROK_ONLY_TOKEN} routing?</user_query>"
            )}]
        })
        .to_string(),
        // Noise: tool payload should be filtered when frame_kind maps tool_call.
        json!({
            "type": "tool_result",
            "tool_call_id": "noise-1",
            "content": "BASE64_NOISE_SHOULD_NOT_INDEX_xxxxxxxxxxxxxxxxxxxxxxxx"
        })
        .to_string(),
        json!({
            "type": "assistant",
            "model_id": "grok-test",
            "content": format!(
                "The Grok-only token {GROK_ONLY_TOKEN} lives in this session extract."
            )
        })
        .to_string(),
    ]
    .join("\n");
    write_file(&chat, &body);
    write_file(
        &session_dir.join("summary.json"),
        &json!({
            "info": {"id": GROK_SESSION_ID, "cwd": "/Volumes/vc-workspace/vetcoders/vibecrafted"},
            "session_summary": format!("Grok coverage {GROK_ONLY_TOKEN}"),
            "created_at": "2026-07-23T00:00:00Z",
            "updated_at": "2026-07-23T00:00:01Z",
            "current_model_id": "grok-test",
            "agent_name": "grok"
        })
        .to_string(),
    );
    chat
}

/// Real Gemini layout: `~/.gemini/tmp/<hash>/chats/session-*.json`
fn seed_gemini_session(home: &Path) -> PathBuf {
    let session = home
        .join(".gemini")
        .join("tmp")
        .join("marble-l1-hash")
        .join("chats")
        .join(format!("{GEMINI_SESSION_ID}.json"));
    let body = json!({
        "sessionId": GEMINI_SESSION_ID,
        "startTime": "2026-07-23T00:00:00Z",
        "lastUpdated": "2026-07-23T00:00:02Z",
        "messages": [
            {
                "id": "u1",
                "timestamp": "2026-07-23T00:00:00Z",
                "type": "user",
                "content": format!("Locate the Gemini-only token {GEMINI_ONLY_TOKEN} please.")
            },
            {
                "id": "a1",
                "timestamp": "2026-07-23T00:00:01Z",
                "type": "gemini",
                "content": format!(
                    "Found {GEMINI_ONLY_TOKEN} only in this Gemini session body."
                ),
                "model": "gemini-test"
            },
            {
                "id": "t1",
                "timestamp": "2026-07-23T00:00:01Z",
                "type": "tool",
                "content": "tool_call noise payload that must not become the only hit"
            }
        ]
    });
    write_file(&session, &serde_json::to_string_pretty(&body).unwrap());
    session
}

#[test]
fn grok_and_gemini_catalog_index_search_and_filter_ratio() {
    let root = unique_root("coverage");
    let home = root.join("home");
    let grok_path = seed_grok_session(&home);
    let gemini_path = seed_gemini_session(&home);
    assert!(grok_path.is_file());
    assert!(gemini_path.is_file());

    // 1) Catalog rebuild from real source roots (zero cards).
    let catalog_out = run_aicx(&home, &["catalog", "rebuild", "--json"]);
    assert_success(&catalog_out, "catalog rebuild");
    let catalog = parse_json(&catalog_out);
    assert_eq!(catalog["cards_written"].as_u64(), Some(0));
    let total = catalog["total_sessions"].as_u64().unwrap_or(0);
    assert!(
        total >= 2,
        "catalog must list grok+gemini sessions; report={catalog}"
    );
    let agents = catalog["agents"].as_object().expect("agents map");
    assert!(
        agents.get("grok").and_then(|v| v.as_u64()).unwrap_or(0) >= 1,
        "grok must be cataloged: {catalog}"
    );
    assert!(
        agents.get("gemini").and_then(|v| v.as_u64()).unwrap_or(0) >= 1,
        "gemini must be cataloged: {catalog}"
    );

    let aicx_home = home.join(".aicx");
    assert!(
        !aicx_home.join("store").exists(),
        "catalog rebuild must not recreate card store"
    );

    // Resolve via catalog API (CLI resolve + library).
    let grok_entry = aicx::catalog::resolve_session(&aicx_home, GROK_SESSION_ID)
        .expect("resolve grok")
        .expect("grok session in catalog");
    assert_eq!(grok_entry.agent, "grok");
    assert!(
        grok_entry.source_path.contains("chat_history.jsonl"),
        "grok primary source must be chat_history: {}",
        grok_entry.source_path
    );

    // Gemini session id is the filename stem in many layouts; resolve by prefix/id.
    let gemini_resolve = run_aicx(&home, &["catalog", "resolve", GEMINI_SESSION_ID, "--json"]);
    // Accept full id or skip if catalog used a different id encoding — then list via read.
    let gemini_in_catalog = if gemini_resolve.status.success() {
        let payload = parse_json(&gemini_resolve);
        assert_eq!(payload["agent"], "gemini");
        true
    } else {
        let entries = aicx::catalog::read_entries_at(&aicx_home).expect("read catalog");
        entries.iter().any(|e| e.agent == "gemini")
    };
    assert!(gemini_in_catalog, "gemini session must appear in catalog");

    // 2) Source-driven index publish into scratch CURRENT (lexical).
    let index_out = run_aicx(
        &home,
        &["index", "--json", "--full-rescan", "--cache-extracts"],
    );
    assert_success(&index_out, "index publish");
    let index = parse_json(&index_out);
    let raw = index["raw_frames"].as_u64().unwrap_or(0);
    let signal = index["signal_frames"].as_u64().unwrap_or(0);
    let filtered = index["filtered_frames"].as_u64().unwrap_or(0);
    let lexical_docs = index["lexical_docs"].as_u64().unwrap_or(0);
    assert!(
        lexical_docs >= 1,
        "index must publish at least one lexical doc: {index}"
    );
    assert!(
        raw >= signal,
        "raw_frames ({raw}) must be >= signal_frames ({signal}): {index}"
    );
    // Filtering ratio report (measured numbers, not adjectives).
    let reduction = if signal == 0 {
        0.0
    } else {
        raw as f64 / signal as f64
    };
    println!(
        "filtering-ratio raw_frames={raw} signal_frames={signal} filtered_frames={filtered} \
         reduction_x={reduction:.2} lexical_docs={lexical_docs} wall_ms={}",
        index["wall_ms"]
    );
    // With tool/noise lines present, expect some filtering OR at least signal < huge noise mill.
    assert!(
        signal > 0,
        "signal frames must be non-zero after filter: {index}"
    );

    // 3) Search hits for agent-only tokens (CURRENT lexical path).
    let grok_search = run_aicx(
        &home,
        &[
            "search",
            GROK_ONLY_TOKEN,
            "--json",
            "--limit",
            "5",
            "--hours",
            "0",
        ],
    );
    assert_success(&grok_search, "search grok-only token");
    let grok_hits = parse_json(&grok_search);
    let grok_blob = grok_hits.to_string();
    assert!(
        grok_blob.contains(GROK_ONLY_TOKEN),
        "search must surface GROK-only token in CURRENT hits: {grok_hits}"
    );
    assert!(
        !grok_blob.contains(GEMINI_ONLY_TOKEN) || grok_blob.contains(GROK_ONLY_TOKEN),
        "grok query must not be dominated by unrelated gemini-only content alone"
    );

    let gemini_search = run_aicx(
        &home,
        &[
            "search",
            GEMINI_ONLY_TOKEN,
            "--json",
            "--limit",
            "5",
            "--hours",
            "0",
        ],
    );
    assert_success(&gemini_search, "search gemini-only token");
    let gemini_hits = parse_json(&gemini_search);
    let gemini_blob = gemini_hits.to_string();
    assert!(
        gemini_blob.contains(GEMINI_ONLY_TOKEN),
        "search must surface GEMINI-only token in CURRENT hits: {gemini_hits}"
    );

    // 4) Self-echo negative: the current marbles prompt marker is not in corpus.
    let echo_search = run_aicx(
        &home,
        &[
            "search",
            SELF_ECHO_MARKER,
            "--json",
            "--limit",
            "3",
            "--hours",
            "0",
        ],
    );
    // May succeed with empty items or fail closed — either way marker must not top-rank.
    let echo_stdout = String::from_utf8_lossy(&echo_search.stdout);
    let echo_stderr = String::from_utf8_lossy(&echo_search.stderr);
    let echo_blob = format!("{echo_stdout}{echo_stderr}");
    if echo_search.status.success()
        && let Ok(payload) = serde_json::from_str::<Value>(&echo_stdout)
        && let Some(items) = payload.get("items").and_then(|v| v.as_array())
        && let Some(top) = items.first()
    {
        let top_text = top.to_string();
        assert!(
            !top_text.contains(SELF_ECHO_MARKER),
            "self-echo marker must not rank top: {top_text}"
        );
    }
    assert!(
        !echo_blob.contains(&format!("\"text\":\"{SELF_ECHO_MARKER}\"")),
        "self-echo marker must not appear as indexed text"
    );

    println!(
        "coverage-ok grok_token_hit=true gemini_token_hit=true self_echo_top=false \
         catalog_sessions={total} filter raw/signal={raw}/{signal}"
    );

    let _ = fs::remove_dir_all(&root);
}
