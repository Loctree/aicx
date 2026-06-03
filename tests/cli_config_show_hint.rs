//! Regression: B-P1-12
//!
//! `aicx config --show` is a common discoverability mistake: `--show`
//! is a positional subcommand (`aicx config show`), not a flag. Pre-Wave-C
//! the CLI surfaced clap's default `error: unexpected argument '--show'`
//! which gave the user no idea what to type next. This test pins the
//! structured hint that replaces it (kind / reason / recommendation /
//! fallback envelope, identical shape to D2's StructuredFailure module).

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

#[test]
fn config_show_flag_emits_structured_hint() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["config", "--show"])
        .output()
        .expect("run aicx config --show");

    // Hint goes to stderr. Exit code is 2 (clap-style parse error).
    assert_eq!(
        output.status.code(),
        Some(2),
        "exit code must be 2; got status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("aicx config failed."),
        "stderr should open with the canonical failure header; got:\n{stderr}"
    );
    assert!(
        stderr.contains("kind:           flag_not_recognized"),
        "stderr should carry stable `kind` token; got:\n{stderr}"
    );
    assert!(
        stderr.contains("reason:"),
        "stderr should carry `reason` line; got:\n{stderr}"
    );
    assert!(
        stderr.contains("recommendation: use the subcommand form: aicx config show"),
        "stderr should recommend `aicx config show`; got:\n{stderr}"
    );
    assert!(
        stderr.contains("fallback:       aicx config show"),
        "stderr should expose paste-ready fallback command; got:\n{stderr}"
    );
}

#[test]
fn config_show_subcommand_still_works() {
    // The intercept must NOT eat the legitimate `aicx config show` path.
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["config", "show", "--help"])
        .output()
        .expect("run aicx config show --help");
    assert!(
        output.status.success(),
        "aicx config show --help must keep working; got status: {}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    assert!(
        stdout.contains("display") || stdout.contains("embedder") || stdout.contains("show"),
        "config show --help should document the show subcommand; got:\n{stdout}"
    );
}

#[test]
fn config_show_flag_with_global_verbose_still_intercepted() {
    // `aicx --verbose config --show` is the same mistake — top-level
    // flags must not bypass the intercept.
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["--verbose", "config", "--show"])
        .output()
        .expect("run aicx --verbose config --show");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("aicx config failed."),
        "intercept must fire even with leading global flags; stderr:\n{stderr}"
    );
}
