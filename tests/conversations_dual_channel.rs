//! Regression: B-P0-04 — `aicx conversations --dry-run` dual-channel.
//!
//! Pre-D1: dry-run wrote `key=value` to stderr only, exit 0 with empty
//! stdout. Pipelines like `aicx conversations --dry-run | jq .` got an
//! empty stream — the same anti-pattern the rest of the family already
//! healed via `migrate-intent-schema --dry-run` (JSON on stdout, human
//! banner on stderr).
//!
//! Wave D Cut D1 task 4 promotes that gold pattern to `conversations`:
//! - stdout carries a JSON envelope with `dry_run`, `sessions_discovered`,
//!   `by_kind`, `by_agent`, `filters_applied`, `output_dir`, …
//! - stderr carries the human banner `=== Conversations Dry-Run ===` plus
//!   aligned key:value lines so operators still see a friendly summary.
//!
//! Tests pin both channels via separate Output captures.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI

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
            assert!(output.status.success(), "fallback cargo build failed");
            fallback
        })
        .clone()
}

fn unique_dir(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aicx-convo-dual-{label}-{}-{suffix}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn conversations_dry_run_emits_json_on_stdout() {
    // Pin the GOLD pattern: stdout carries a JSON envelope that downstream
    // tools (`jq`, scripts, vibecrafted-mcp wrappers) can parse cleanly.
    // Operator-isolated source roots (empty CLAUDE_CONFIG_DIR/AICX_HOME)
    // keep the discovery histograms at zero so the test stays
    // deterministic — we are pinning the envelope shape, not its values.
    let bin = ensure_aicx_binary_exists();
    let home = unique_dir("home");
    let claude_cfg = unique_dir("claude-cfg");
    let out_dir = unique_dir("out");

    let output = Command::new(&bin)
        .arg("conversations")
        .arg("--dry-run")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg("--hours")
        .arg("1")
        .env("AICX_HOME", &home)
        .env("CLAUDE_CONFIG_DIR", &claude_cfg)
        .env("AICX_NO_MUTATION_WARN", "1")
        .output()
        .expect("run aicx conversations --dry-run");

    assert!(
        output.status.success(),
        "exit must be 0 in dry-run; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|err| {
        panic!(
            "stdout must parse as JSON for pipe-friendly consumption; \
             err: {err:?}; stdout:\n{stdout}"
        )
    });

    // Pin envelope shape.
    assert_eq!(
        parsed["dry_run"],
        serde_json::Value::Bool(true),
        "dry_run flag must be set in envelope; got:\n{parsed}"
    );
    assert!(
        parsed.get("sessions_discovered").is_some(),
        "envelope must carry sessions_discovered field; got:\n{parsed}"
    );
    assert!(
        parsed.get("by_kind").is_some(),
        "envelope must carry by_kind histogram; got:\n{parsed}"
    );
    assert!(
        parsed.get("by_agent").is_some(),
        "envelope must carry by_agent histogram; got:\n{parsed}"
    );
    assert!(
        parsed.get("filters_applied").is_some(),
        "envelope must carry filters_applied object; got:\n{parsed}"
    );
    let filters = &parsed["filters_applied"];
    assert!(
        filters.get("hours").is_some(),
        "filters_applied must include hours; got:\n{filters}"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&claude_cfg);
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn conversations_dry_run_preserves_human_summary_on_stderr() {
    // Back-compat: the styled banner must still appear on stderr so
    // operators reading the terminal see the same human surface they
    // had before D1. The flat `sessions_discovered=N` lines on stderr
    // are intentionally not pinned anymore — that contract has shifted
    // to the JSON channel.
    let bin = ensure_aicx_binary_exists();
    let home = unique_dir("home");
    let claude_cfg = unique_dir("claude-cfg");
    let out_dir = unique_dir("out");

    let output = Command::new(&bin)
        .arg("conversations")
        .arg("--dry-run")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg("--hours")
        .arg("1")
        .env("AICX_HOME", &home)
        .env("CLAUDE_CONFIG_DIR", &claude_cfg)
        .env("AICX_NO_MUTATION_WARN", "1")
        .output()
        .expect("run aicx conversations --dry-run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("=== Conversations Dry-Run ==="),
        "stderr must include the styled banner; got:\n{stderr}"
    );
    assert!(
        stderr.contains("Sessions discovered:"),
        "stderr must include the human-readable session count; got:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&claude_cfg);
    let _ = std::fs::remove_dir_all(&out_dir);
}

#[test]
fn conversations_dry_run_stdout_does_not_leak_human_banner() {
    // The split-channel contract is bidirectional: stdout must NOT carry
    // the human banner. Otherwise `| jq .` would fail because the JSON
    // would be polluted by banner text. We already verify stdout parses
    // as JSON above; this test makes the bidirectional contract
    // self-documenting.
    let bin = ensure_aicx_binary_exists();
    let home = unique_dir("home");
    let claude_cfg = unique_dir("claude-cfg");
    let out_dir = unique_dir("out");

    let output = Command::new(&bin)
        .arg("conversations")
        .arg("--dry-run")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg("--hours")
        .arg("1")
        .env("AICX_HOME", &home)
        .env("CLAUDE_CONFIG_DIR", &claude_cfg)
        .env("AICX_NO_MUTATION_WARN", "1")
        .output()
        .expect("run aicx conversations --dry-run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("=== Conversations Dry-Run ==="),
        "stdout must NOT include the human banner; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("Sessions discovered:"),
        "stdout must NOT include the human key:value lines; got:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&claude_cfg);
    let _ = std::fs::remove_dir_all(&out_dir);
}
