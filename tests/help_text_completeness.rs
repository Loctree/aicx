//! Regression: B-P1-11 — shared retrieval grammar help bodies.
//!
//! `RetrievalFilters` is the single struct flattened into `aicx search`,
//! `aicx steer`, `aicx intents`, and `aicx tail`. Pre-Wave-C, four of
//! its five canonical fields (`--score`, `--agent`, `--since`,
//! `--until`, `--frame-kind`) shipped without `help` doc-comments, so
//! `--help` rendered just the flag name and value placeholder. Adding
//! the docs is a single-PR leverage move — one struct, four commands
//! upgraded.
//!
//! This test pins the help text shape so a future refactor cannot
//! silently regress the shared grammar back to bare flags.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

fn current_profile_dir() -> PathBuf {
    let test_exe = std::env::current_exe().expect("resolve current test executable");
    test_exe
        .parent()
        .and_then(std::path::Path::parent)
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
                "fallback cargo build --bin aicx failed\nstatus: {}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );

            fallback
        })
        .clone()
}

/// Per Wave C §3.6 the help body for `--<flag>` must include the
/// `expected_fragment` so operators can infer accepted values from
/// `aicx <cmd> --help` alone.
fn assert_help_contains(stdout: &str, cmd: &str, flag: &str, expected_fragment: &str) {
    // clap renders the help body indented under the flag line. Find
    // the `--flag` line then scan forward until the next blank or
    // less-indented line.
    let needle = format!("--{flag} ");
    let needle_eq = format!("--{flag}<"); // value-required form: `--flag <FLAG>`
    let mut iter = stdout.lines().peekable();
    while let Some(line) = iter.next() {
        if line.trim_start().starts_with(&needle) || line.trim_start().starts_with(&needle_eq) {
            // Grab the next ~8 indented lines for the body.
            let mut body = String::new();
            for _ in 0..8 {
                if let Some(next) = iter.peek() {
                    let trimmed = next.trim_start();
                    if trimmed.is_empty() || next.trim_start().starts_with("--") {
                        break;
                    }
                    body.push_str(next);
                    body.push('\n');
                    iter.next();
                } else {
                    break;
                }
            }
            assert!(
                body.contains(expected_fragment),
                "aicx {cmd} --help: --{flag} body should contain '{expected_fragment}'; got:\n{body}"
            );
            return;
        }
    }
    panic!("aicx {cmd} --help: --{flag} flag not found in help output");
}

fn help_for(cmd: &str) -> String {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args([cmd, "--help"])
        .output()
        .unwrap_or_else(|e| panic!("run aicx {cmd} --help: {e}"));
    assert!(
        output.status.success(),
        "aicx {cmd} --help must exit 0; got status: {}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("aicx --help should emit UTF-8")
}

/// One subtest per consumer of `RetrievalFilters`: search, steer,
/// intents, tail. Each must surface the five shared grammar fields
/// with the canonical help bodies (see `struct RetrievalFilters` in
/// `src/main.rs`).
fn assert_retrieval_grammar(cmd: &str, stdout: &str) {
    assert_help_contains(stdout, cmd, "score", "0-100");
    assert_help_contains(stdout, cmd, "agent", "claude | codex | gemini | junie");
    assert_help_contains(stdout, cmd, "since", "YYYY-MM-DD");
    assert_help_contains(stdout, cmd, "until", "YYYY-MM-DD");
    assert_help_contains(stdout, cmd, "frame-kind", "user_msg | agent_reply");
}

#[test]
fn search_help_carries_retrieval_grammar() {
    assert_retrieval_grammar("search", &help_for("search"));
}

#[test]
fn intents_help_carries_retrieval_grammar() {
    assert_retrieval_grammar("intents", &help_for("intents"));
}

#[test]
fn tail_help_carries_retrieval_grammar() {
    assert_retrieval_grammar("tail", &help_for("tail"));
}

#[test]
fn steer_help_carries_retrieval_grammar() {
    // `steer` is feature-gated behind the `lance` build feature. When
    // built without it, the subcommand is still parsed (so `--help`
    // works) but flagged in the about text. We still want the shared
    // grammar bodies to render.
    assert_retrieval_grammar("steer", &help_for("steer"));
}

#[test]
fn doctor_oracle_help_documents_severity_mapping() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["doctor", "--help"])
        .output()
        .expect("run aicx doctor --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Severity vocabulary mapping"),
        "doctor --help should expose the severity mapping table; got:\n{stdout}"
    );
    assert!(
        stdout.contains("ready"),
        "doctor --help oracle section should mention `ready`"
    );
    assert!(
        stdout.contains("unsafe_for_loctree_scope"),
        "doctor --help oracle section should mention `unsafe_for_loctree_scope`"
    );
    assert!(
        stdout.contains("TitleCase"),
        "doctor --help should call out the TitleCase / lowercase split"
    );
}
