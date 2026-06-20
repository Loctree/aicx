//! G-4: per-extractor diagnostic SUMMARY + `--verbose` per-file detail flag.
//!
//! These tests stand up a synthetic Claude corpus that triggers a known number
//! of per-file warnings (unparsable timestamps), then runs `aicx all` twice:
//! once at default verbosity (stderr must be quiet, summary ≤ 5 lines) and
//! once with `--verbose` (per-file warnings must reappear).

use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-diagnostics-summary-{name}-{}-{}",
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
        // Windows resolves the home dir from USERPROFILE, not HOME (dirs::home_dir).
        .env("USERPROFILE", home)
        .env("AICX_HOME", home.join(".aicx"))
        .env("AICX_ALLOW_TMP", "1")
        .output()
        .expect("run aicx")
}

/// Build a Claude session JSONL with N entries that all have UNPARSABLE
/// timestamps — exercises the per-file warning aggregation path in
/// `parse_claude_jsonl_with_diagnostics`.
fn write_claude_session_with_bad_timestamps(path: &Path, session_id: &str, cwd: &Path, n: usize) {
    let mut lines = Vec::with_capacity(n);
    for i in 0..n {
        lines.push(
            json!({
                "type": "user",
                "message": { "role": "user", "content": format!("msg {i}") },
                "timestamp": "not-a-valid-rfc3339-timestamp",
                "sessionId": session_id,
                "cwd": cwd.display().to_string(),
            })
            .to_string(),
        );
    }
    write_file(path, &lines.join("\n"));
}

fn setup_corpus(home: &Path, files: usize, bad_per_file: usize) {
    let claude_dir = home.join(".claude").join("projects").join("-tmp-corpus");
    fs::create_dir_all(&claude_dir).expect("create claude project dir");
    let cwd = home.join("tmp").join("corpus");
    for i in 0..files {
        let session_id = format!("session-{i:04}-aaaa-bbbb-cccc-dddddddddddd");
        let path = claude_dir.join(format!("{session_id}.jsonl"));
        write_claude_session_with_bad_timestamps(&path, &session_id, &cwd, bad_per_file);
    }
}

fn count_extractor_warning_lines(stderr: &str) -> usize {
    stderr
        .lines()
        .filter(|line| {
            line.contains("session warning:")
                || line.contains("content warning:")
                || line.contains("history warning:")
        })
        .count()
}

#[test]
fn default_verbosity_aggregates_per_file_warnings_into_summary() {
    let root = unique_test_dir("default");
    let home = root.join("home");
    setup_corpus(&home, 10, 5);

    let output = run_aicx(&home, &["all", "-H", "0", "--emit", "none"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let per_file = count_extractor_warning_lines(&stderr);
    assert_eq!(
        per_file, 0,
        "default mode must NOT emit per-file extractor warnings to stderr\nstderr:\n{stderr}"
    );

    let summary_lines: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("diagnostics:") || line.contains("Diagnostics detail:"))
        .collect();
    assert!(
        summary_lines.len() <= 5,
        "expected ≤5 diagnostics summary lines, got {}:\n{stderr}",
        summary_lines.len()
    );
    assert!(
        summary_lines
            .iter()
            .any(|l| l.contains("Claude diagnostics:")),
        "expected Claude diagnostics summary line\nstderr:\n{stderr}"
    );
    let claude_line = summary_lines
        .iter()
        .find(|l| l.contains("Claude diagnostics:"))
        .expect("claude line");
    assert!(
        claude_line.contains("unparsable timestamps"),
        "claude summary must mention unparsable timestamps\nline:\n{claude_line}"
    );

    // Structured run log was written under ~/.aicx/state/.
    let state_dir = home.join(".aicx").join("state");
    let logs: Vec<_> = fs::read_dir(&state_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|f| f.to_str())
                        .is_some_and(|f| f.starts_with("diagnostics-") && f.ends_with(".log"))
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !logs.is_empty(),
        "expected at least one diagnostics-*.log under {}",
        state_dir.display()
    );
    let log_body = fs::read_to_string(&logs[0]).expect("read diagnostics log");
    assert!(
        log_body.contains("Claude session warning"),
        "structured log must contain per-file detail; got:\n{log_body}"
    );
}

#[test]
fn verbose_flag_restores_per_file_stderr_detail() {
    let root = unique_test_dir("verbose");
    let home = root.join("home");
    setup_corpus(&home, 10, 5);

    let output = run_aicx(&home, &["--verbose", "all", "-H", "0", "--emit", "none"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let per_file = count_extractor_warning_lines(&stderr);
    assert!(
        per_file >= 10,
        "verbose mode must echo per-file warnings; got {per_file}\nstderr:\n{stderr}"
    );
}
