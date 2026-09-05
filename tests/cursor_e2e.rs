// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! Production E2E for the Cursor lane: a synthetic transcript must travel the
//! real user path — discovery finds it, the agent alias matches it, and
//! extraction emits conversation output — including the transport's worst
//! case: no harness `<timestamp>` wrapper anywhere (no in-band wall clock).
//! Regression for the "CompleteVisible parse, zero output" class: cutoff and
//! watermark used to delete every UNIX_EPOCH-stamped entry.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const SESSION_ID: &str = "e2e00001-1111-4111-8111-aaaaaaaaaaaa";

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-cursor-e2e-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
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

/// Transcript WITHOUT any `<timestamp>` wrapper: the only wall clock the
/// pipeline can use is the file mtime.
const WRAPPERLESS_TRANSCRIPT: &str = concat!(
    r#"{"role":"user","message":{"content":[{"type":"text","text":"Ship the cursor e2e lane."}]}}"#,
    "\n",
    r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Shipping the cursor e2e lane."},{"type":"tool_use","name":"Shell","input":{"command":"echo e2e-tool-lane"}}]}}"#,
    "\n",
    r#"{"type":"turn_ended","status":"success"}"#,
    "\n",
);

fn write_session(home: &Path) -> PathBuf {
    let session_dir = home
        .join(".cursor")
        .join("projects")
        .join("users-user-example-project")
        .join("agent-transcripts")
        .join(SESSION_ID);
    fs::create_dir_all(&session_dir).expect("session dir");
    let path = session_dir.join(format!("{SESSION_ID}.jsonl"));
    fs::write(&path, WRAPPERLESS_TRANSCRIPT).expect("write transcript");
    path
}

#[test]
fn cursor_session_is_discovered_via_alias_and_extracts_real_output() {
    let home = unique_root("home");
    let transcript = write_session(&home);

    // Discovery + canonical alias: `cursor-agent` must surface the session.
    let listed = run_aicx(
        &home,
        &["sessions", "list", "--agent", "cursor-agent", "--all"],
    );
    let listed_stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed.status.success(),
        "sessions list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(
        listed_stdout.contains(SESSION_ID) || listed_stdout.contains(&SESSION_ID[..8]),
        "cursor-agent alias listing must include the session, got:\n{listed_stdout}"
    );
    // Canonical spelling agrees with the alias.
    let listed_canonical = run_aicx(&home, &["sessions", "list", "--agent", "cursor", "--all"]);
    let canonical_stdout = String::from_utf8_lossy(&listed_canonical.stdout);
    assert!(
        canonical_stdout.contains(SESSION_ID) || canonical_stdout.contains(&SESSION_ID[..8]),
        "canonical cursor listing must include the session, got:\n{canonical_stdout}"
    );

    // Project filter on a HYPHENATED repo name: the cursor slug is lossy
    // (`users-user-example-project` decodes to `/users/user/example/project`),
    // so this only works through ENCODED-space matching.
    let filtered = run_aicx(
        &home,
        &[
            "sessions",
            "list",
            "--agent",
            "cursor",
            "--project",
            "/example-project",
            "--all",
        ],
    );
    let filtered_stdout = String::from_utf8_lossy(&filtered.stdout);
    assert!(
        filtered_stdout.contains(SESSION_ID) || filtered_stdout.contains(&SESSION_ID[..8]),
        "hyphenated --project filter must match the cursor session, got:\nstdout={filtered_stdout}\nstderr={}",
        String::from_utf8_lossy(&filtered.stderr)
    );

    // Extraction through the production path: wrapperless transcript must
    // still produce conversation output (file mtime is the fallback clock).
    let out_path = home.join("out").join("cursor_e2e_conversation.md");
    fs::create_dir_all(out_path.parent().unwrap()).expect("out dir");
    let transcript_str = transcript.to_string_lossy().to_string();
    let out_str = out_path.to_string_lossy().to_string();
    let extracted = run_aicx(
        &home,
        &[
            "extract",
            "cursor",
            "--file",
            &transcript_str,
            "-o",
            &out_str,
            "--conversation",
        ],
    );
    assert!(
        extracted.status.success(),
        "extract cursor failed: stdout={} stderr={}",
        String::from_utf8_lossy(&extracted.stdout),
        String::from_utf8_lossy(&extracted.stderr)
    );
    let body = fs::read_to_string(&out_path).expect("conversation output written");
    assert!(
        body.contains("Ship the cursor e2e lane."),
        "user lane missing from output:\n{body}"
    );
    assert!(
        body.contains("Shipping the cursor e2e lane."),
        "assistant lane missing from output:\n{body}"
    );

    // Batch path (`aicx all`): the cutoff/watermark retain runs here. A
    // wrapperless transcript has no in-band wall clock, so without the
    // file-mtime fallback every entry was UNIX_EPOCH and the default cutoff
    // deleted 100% of them — parse "complete", store empty. The store must
    // actually receive this session's content.
    let batch = run_aicx(&home, &["all"]);
    assert!(
        batch.status.success(),
        "aicx all failed: stdout={} stderr={}",
        String::from_utf8_lossy(&batch.stdout),
        String::from_utf8_lossy(&batch.stderr)
    );
    // `aicx all` reports per-agent entry counts on stdout (the card store is
    // retired; entries stay in memory for reports). The wrapperless session
    // must survive the cutoff/watermark retain as non-zero entries.
    let batch_stdout = format!(
        "{}{}",
        String::from_utf8_lossy(&batch.stdout),
        String::from_utf8_lossy(&batch.stderr)
    );
    let cursor_line = batch_stdout
        .lines()
        .find(|line| line.contains("[cursor]") && line.contains("entries"))
        .unwrap_or_else(|| panic!("no [cursor] entries line in aicx all output:\n{batch_stdout}"));
    assert!(
        !cursor_line.contains(" 0 entries"),
        "wrapperless cursor session was filtered to zero entries by cutoff/watermark: {cursor_line}"
    );
    assert!(
        batch_stdout.contains("ingested=1"),
        "cursor session was not ingested:\n{batch_stdout}"
    );

    let _ = fs::remove_dir_all(&home);
}
