//! Regression: B-P1-08 — family-wide failure-as-state at the CLI boundary.
//!
//! Every CLI-boundary error should emit the structured `StructuredFailure`
//! envelope (Wave B §1.2 text / §1.3 JSON) instead of clap's default
//! "the following required arguments were not provided" / "missing
//! subcommand" / bare `anyhow!` strings. This test pins the contract for
//! the five entrypoints covered by the D2 uniformity pack:
//!
//! - `aicx ingest` (no `--source`)
//! - `aicx conversations` (no `--out-dir`)
//! - `aicx sources` (no subcommand)
//! - `aicx extract` (mode mismatch: no `--format`)
//! - `aicx ingest --source loct-context-pack` (no `<PACK_DIR>`)

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

fn assert_structured_text(
    stderr: &str,
    cmd: &str,
    kind: &str,
    reason_fragment: &str,
    rec_fragment: &str,
) {
    assert!(
        stderr.contains(&format!("{cmd} failed.")),
        "stderr should open with canonical failure header for {cmd}; got:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("kind:           {kind}")),
        "stderr should carry stable kind={kind}; got:\n{stderr}"
    );
    assert!(
        stderr.contains(reason_fragment),
        "stderr should mention reason fragment '{reason_fragment}'; got:\n{stderr}"
    );
    assert!(
        stderr.contains(rec_fragment),
        "stderr should mention recommendation fragment '{rec_fragment}'; got:\n{stderr}"
    );
}

#[test]
fn ingest_no_source_emits_structured_failure() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .arg("ingest")
        .output()
        .expect("run aicx ingest");
    assert_eq!(
        output.status.code(),
        Some(2),
        "exit code 2; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_structured_text(
        &stderr,
        "aicx ingest",
        "missing_required_arg",
        "--source <SOURCE> is required",
        "rerun with --source",
    );
    assert!(
        stderr.contains("fallback:"),
        "ingest no-arg failure should include fallback command; got:\n{stderr}"
    );
}

#[test]
fn ingest_no_source_json_envelope() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .env("AICX_JSON", "1")
        .arg("ingest")
        .output()
        .expect("run aicx ingest with AICX_JSON=1");
    assert_eq!(output.status.code(), Some(2));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let payload: serde_json::Value =
        serde_json::from_str(&stdout).expect("AICX_JSON=1 must produce parseable JSON on stdout");
    assert_eq!(payload["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        payload["kind"],
        serde_json::Value::String("missing_required_arg".to_string())
    );
    assert_eq!(
        payload["error"],
        serde_json::Value::String("aicx ingest failed".to_string())
    );
    assert!(payload["reason"].is_string());
    assert!(payload["recommendation"].is_string());
    assert!(payload["fallback"]["available"].as_bool().unwrap_or(false));
}

#[test]
fn conversations_no_out_dir_emits_structured_failure() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .arg("conversations")
        .output()
        .expect("run aicx conversations");
    assert_eq!(
        output.status.code(),
        Some(2),
        "exit code 2; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_structured_text(
        &stderr,
        "aicx conversations",
        "missing_required_arg",
        "--out-dir <DIR> is required",
        "rerun with --out-dir",
    );
}

#[test]
fn sources_no_subcommand_emits_structured_failure() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .arg("sources")
        .output()
        .expect("run aicx sources");
    assert_eq!(
        output.status.code(),
        Some(2),
        "exit code 2; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_structured_text(
        &stderr,
        "aicx sources",
        "missing_subcommand",
        "requires a subcommand",
        "aicx sources protect",
    );
}

#[test]
fn sources_protect_path_still_works() {
    // Intercept must NOT eat the legitimate `aicx sources protect` path
    // (we just check help — `protect` itself requires --root, which is
    // a separate clap surface we deliberately leave alone in D2).
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["sources", "protect", "--help"])
        .output()
        .expect("run aicx sources protect --help");
    assert!(
        output.status.success(),
        "aicx sources protect --help must keep working; status: {}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn extract_without_agent_subcommand_emits_structured_failure() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .arg("extract")
        .output()
        .expect("run aicx extract");
    assert_eq!(
        output.status.code(),
        Some(2),
        "exit code 2; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_structured_text(
        &stderr,
        "aicx extract",
        "missing_agent_subcommand",
        "extract requires an agent subcommand",
        "aicx extract codex --session <id> --conversation",
    );
}

#[test]
fn extract_legacy_format_flag_emits_structured_migration_hint() {
    // `--agent` is an accepted deprecated alias since 2026-07-16; the hard
    // rejection path is exercised by the still-removed `--format` grammar.
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["extract", "--format", "codex", "--session", "abc12345"])
        .output()
        .expect("run legacy extract flag grammar rejection case");
    assert_eq!(
        output.status.code(),
        Some(2),
        "exit code 2; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_structured_text(
        &stderr,
        "aicx extract",
        "legacy_flag_grammar",
        "the agent is a required subcommand",
        "aicx extract codex --session <id> --conversation",
    );
}

#[test]
fn extract_without_agent_subcommand_json_envelope() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .env("AICX_JSON", "1")
        .arg("extract")
        .output()
        .expect("run aicx extract with AICX_JSON=1");
    assert_eq!(output.status.code(), Some(2));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let payload: serde_json::Value =
        serde_json::from_str(&stdout).expect("AICX_JSON=1 must produce parseable JSON on stdout");
    assert_eq!(
        payload["kind"],
        serde_json::Value::String("missing_agent_subcommand".to_string())
    );
    assert_eq!(
        payload["error"],
        serde_json::Value::String("aicx extract failed".to_string())
    );
}

#[test]
fn ingest_loct_context_pack_no_dir_emits_structured_failure() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["ingest", "--source", "loct-context-pack"])
        .output()
        .expect("run aicx ingest --source loct-context-pack");
    assert_eq!(
        output.status.code(),
        Some(2),
        "exit code 2; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_structured_text(
        &stderr,
        "aicx ingest",
        "input_path_required",
        "loct-context-pack requires <PACK_DIR>",
        "append the pack directory path",
    );
}

#[test]
fn intercept_does_not_swallow_help_flag() {
    // `--help` and `-h` must keep clap's native help rendering — the
    // pre-parse interceptor must short-circuit on those.
    let bin = ensure_aicx_binary_exists();
    for args in [
        vec!["ingest", "--help"],
        vec!["conversations", "--help"],
        vec!["sources", "--help"],
    ] {
        let output = Command::new(&bin)
            .args(&args)
            .output()
            .unwrap_or_else(|e| panic!("run aicx {args:?}: {e}"));
        assert!(
            output.status.success(),
            "aicx {args:?} should print help and exit 0; got status: {}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.to_ascii_lowercase().contains("usage:"),
            "aicx {args:?} help should include `Usage:`; got:\n{stdout}"
        );
    }
}
