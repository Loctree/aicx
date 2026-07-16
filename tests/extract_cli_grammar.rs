//! Instrumented CLI contract for the canonical `aicx extract <agent>` grammar
//! (C7 cutover).
//!
//! Frozen surfaces:
//! - the agent is a required subcommand; `--agent`/`--format` are rejected
//!   with a structured migration hint (never aliased);
//! - `--session` resolves the session catalog FIRST (bounded headers, zero
//!   body reads) and exactly one source moves on to a single parse pass —
//!   proven through the `extract:` instrumentation lines;
//! - `--file` builds a direct handle with no catalog scan and no global AICX
//!   state (`catalog_files_opened=0`);
//! - a fatal parse never leaves partial output on disk.
//!
//! The direct-file argv below is the compact-recall consumer contract
//! recorded for C7H intake — do not reshape it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// Frozen external-consumer argv (C7H compact-recall hook). `{file}` and
/// `{out}` are the only variable slots.
const FROZEN_DIRECT_FILE_ARGV: [&str; 7] = [
    "extract",
    "codex",
    "--file",
    "{file}",
    "--conversation",
    "-o",
    "{out}",
];

const SESSION_UUID: &str = "019f1111-2222-7333-8444-000000000042";

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-extract-grammar-{name}-{}-{}",
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
    fs::write(path, content).expect("write fixture");
}

fn rollout_fixture(session_id: &str) -> String {
    format!(
        concat!(
            r#"{{"timestamp":"2026-07-13T04:00:00Z","type":"session_meta","payload":{{"id":"{id}","cwd":"/tmp/work"}}}}"#,
            "\n",
            r#"{{"timestamp":"2026-07-13T04:00:01Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"hello"}}]}}}}"#,
            "\n",
        ),
        id = session_id
    )
}

fn run_extract(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aicx"))
        .env("HOME", home)
        .env("AICX_NO_MUTATION_WARN", "1")
        .args(args)
        .output()
        .expect("run aicx")
}

#[test]
fn help_lists_agent_subcommands_and_hides_flag_grammar() {
    let home = unique_test_dir("help");
    fs::create_dir_all(&home).expect("create home");
    let output = run_extract(&home, &["extract", "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for agent in ["codex", "claude", "gemini", "grok", "junie"] {
        assert!(
            stdout.contains(agent),
            "extract --help must list `{agent}`:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("--agent") && !stdout.contains("--format"),
        "removed flag grammar leaked into help:\n{stdout}"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn legacy_agent_flag_is_aliased_with_deprecation_note() {
    // Restored 2026-07-16: the hard removal of `--agent` silently broke
    // fleet-wide consumers (hooks with `|| true` never see the migration
    // hint). The alias must behave EXACTLY like the subcommand form and
    // announce its deprecation on stderr.
    let home = unique_test_dir("legacy-agent-alias");
    let rollout = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join(format!("rollout-2026-07-13T04-00-00-{SESSION_UUID}.jsonl"));
    write_file(&rollout, &rollout_fixture(SESSION_UUID));

    let output = run_extract(
        &home,
        &[
            "extract",
            "--agent",
            "codex",
            "--session",
            SESSION_UUID,
            "--conversation",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "aliased legacy --agent grammar must succeed\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("deprecated alias"),
        "deprecation note missing:\n{stderr}"
    );
    let extracts = home.join(".aicx").join("extracts");
    assert!(
        extracts.exists(),
        "aliased invocation must write the extract like the subcommand form"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn legacy_format_flag_stays_rejected_not_aliased() {
    let home = unique_test_dir("legacy-format");
    let rollout = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join(format!("rollout-2026-07-13T04-00-00-{SESSION_UUID}.jsonl"));
    write_file(&rollout, &rollout_fixture(SESSION_UUID));

    let output = run_extract(
        &home,
        &[
            "extract",
            "--format",
            "codex",
            "--session",
            SESSION_UUID,
            "--conversation",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "legacy --format must exit 2");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("legacy_flag_grammar"),
        "structured kind missing:\n{stderr}"
    );
    assert!(
        stderr.contains("aicx extract codex --session <id> --conversation"),
        "migration hint missing:\n{stderr}"
    );
    assert!(
        !stderr.contains("extract: resolved"),
        "legacy --format must not reach catalog resolution"
    );
    let extracts = home.join(".aicx").join("extracts");
    assert!(
        !extracts.exists(),
        "rejected invocation must not write any output"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn session_mode_resolves_catalog_first_and_parses_once() {
    let home = unique_test_dir("session-mode");
    let sessions = home.join(".codex").join("sessions");
    let rollout = sessions
        .join("2026")
        .join("07")
        .join("13")
        .join(format!("rollout-2026-07-13T04-00-00-{SESSION_UUID}.jsonl"));
    write_file(&rollout, &rollout_fixture(SESSION_UUID));
    // Unrelated sibling sessions prove the exact-UUID fast path opens only
    // the selected header.
    for other in 1..=3 {
        let sibling = sessions.join("2026").join("07").join("12").join(format!(
            "rollout-2026-07-12T04-00-0{other}-019f1111-2222-7333-8444-00000000000{other}.jsonl"
        ));
        write_file(
            &sibling,
            &rollout_fixture(&format!("019f1111-2222-7333-8444-00000000000{other}")),
        );
    }

    let out_path = home.join("session-conversation.md");
    let out_arg = out_path.display().to_string();
    let output = run_extract(
        &home,
        &[
            "extract",
            "codex",
            "--session",
            SESSION_UUID,
            "--conversation",
            "-o",
            &out_arg,
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains(&format!("extract: resolved `{SESSION_UUID}`")),
        "catalog resolution line missing:\n{stderr}"
    );
    assert!(
        stderr.contains("catalog_candidates=4"),
        "all candidates must be enumerated by metadata:\n{stderr}"
    );
    assert!(
        stderr.contains("catalog_files_opened=1"),
        "exact-UUID lookup must open exactly one header:\n{stderr}"
    );
    assert!(
        stderr.contains("catalog_body_reads=0"),
        "locate-before-parse: the catalog never reads bodies:\n{stderr}"
    );
    assert!(
        stderr.contains("sources_parsed=1"),
        "exactly one source moves to the parse pass:\n{stderr}"
    );

    // No partial output invariant: output exists iff the parse succeeded.
    // (The parse seam is fail-closed until the per-agent adapter cuts land;
    // this assertion holds unchanged before and after C5X wiring.)
    assert_eq!(
        output.status.success(),
        out_path.exists(),
        "output file must exist exactly when extraction succeeded; stderr:\n{stderr}"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn session_mode_missing_session_is_structured() {
    let home = unique_test_dir("missing");
    write_file(
        &home
            .join(".codex")
            .join("sessions")
            .join("2026")
            .join("07")
            .join("13")
            .join(format!("rollout-2026-07-13T04-00-00-{SESSION_UUID}.jsonl")),
        &rollout_fixture(SESSION_UUID),
    );
    let output = run_extract(
        &home,
        &[
            "extract",
            "codex",
            "--session",
            "deadbeef-0000-7000-8000-999999999999",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session_not_found"),
        "missing session must be a structured failure:\n{stderr}"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn session_mode_ambiguous_reference_is_structured_not_first_wins() {
    let home = unique_test_dir("ambiguous");
    let sessions = home.join(".codex").join("sessions");
    // Two distinct physical sources share the same filename UUID: no
    // traversal-order winner is allowed.
    write_file(
        &sessions
            .join("a")
            .join(format!("rollout-2026-07-13T04-00-00-{SESSION_UUID}.jsonl")),
        &rollout_fixture(SESSION_UUID),
    );
    write_file(
        &sessions
            .join("b")
            .join(format!("rollout-2026-07-13T05-00-00-{SESSION_UUID}.jsonl")),
        &rollout_fixture(SESSION_UUID),
    );

    let output = run_extract(&home, &["extract", "codex", "--session", SESSION_UUID]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session_ambiguous"),
        "ambiguity must be structured, never first-wins:\n{stderr}"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn direct_file_mode_requires_output_and_skips_catalog() {
    let home = unique_test_dir("direct-file");
    let rollout = home.join("standalone-rollout.jsonl");
    write_file(&rollout, &rollout_fixture(SESSION_UUID));
    let file_arg = rollout.display().to_string();

    // Missing -o is a structured failure before any parse.
    let output = run_extract(&home, &["extract", "codex", "--file", &file_arg]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("output_path_required"),
        "missing -o must be structured:\n{stderr}"
    );

    // Frozen compact-recall argv (C7H): direct handle, zero catalog work,
    // exactly one parse pass, and no global AICX state required.
    let out_path = home.join("recall.md");
    let out_arg = out_path.display().to_string();
    let argv: Vec<&str> = FROZEN_DIRECT_FILE_ARGV
        .iter()
        .map(|arg| match *arg {
            "{file}" => file_arg.as_str(),
            "{out}" => out_arg.as_str(),
            other => other,
        })
        .collect();
    let output = run_extract(&home, &argv);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("catalog_files_opened=0"),
        "direct-file mode must not touch the catalog:\n{stderr}"
    );
    assert!(
        stderr.contains("sources_parsed=1"),
        "direct-file mode is a single parse pass:\n{stderr}"
    );
    // Direct-file mode must not touch the canonical store, extract defaults,
    // or watermark state. (The always-on per-run diagnostics log under
    // `.aicx/state/diagnostics-*.log` is a binary-wide surface, not extract
    // state, and is exempt.)
    for global in ["store", "extracts", "chunks", "state/state.json"] {
        assert!(
            !home.join(".aicx").join(global).exists(),
            "direct-file mode must not create global AICX state: .aicx/{global}"
        );
    }
    assert_eq!(
        output.status.success(),
        out_path.exists(),
        "output file must exist exactly when extraction succeeded; stderr:\n{stderr}"
    );
    let _ = fs::remove_dir_all(&home);
}
