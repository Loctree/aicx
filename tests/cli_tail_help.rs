//! Regression: B-P3-24
//!
//! `aicx tail --help` previously read "Stream newly-arriving intents/chunks
//! in a follow-like mode", which described the `--follow` mode as if it
//! were the default. Tail's real default is the snapshot mode (print
//! recent entries and exit); streaming requires `--follow`. The help text
//! must reflect both behaviors so operators don't expect a hang.

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
fn tail_help_covers_snapshot_and_follow() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["tail", "--help"])
        .output()
        .expect("run aicx tail --help");

    assert!(
        output.status.success(),
        "aicx tail --help exited non-zero\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    // Snapshot mode + follow mode must both be discoverable from the help text.
    assert!(
        stdout.contains("snapshot"),
        "aicx tail --help should mention the snapshot default mode; got:\n{}",
        stdout
    );
    assert!(
        stdout.contains("--follow") || stdout.contains("follow"),
        "aicx tail --help should mention --follow streaming mode; got:\n{}",
        stdout
    );
}

#[test]
fn steer_help_mentions_lance_feature_requirement() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["steer", "--help"])
        .output()
        .expect("run aicx steer --help");

    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    assert!(
        stdout.contains("lance") || stdout.contains("features"),
        "aicx steer --help should mention the lance feature requirement; got:\n{}",
        stdout
    );
}

#[test]
fn top_level_help_drops_layer_1_jargon_and_store_command() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["--help"])
        .output()
        .expect("run aicx --help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Top-level help shows the short description for each subcommand. The
    // current extraction/catalog set must not show the bare
    // "(layer 1)" jargon — they should use a plain or "canonical corpus
    // extraction" phrasing instead.
    //
    // We grep line-by-line for the subcommand row and assert no "(layer 1)"
    // suffix is present on it.
    for cmd in ["all", "claude", "codex", "catalog"] {
        let line = stdout
            .lines()
            .find(|line| {
                line.trim_start().starts_with(&format!("{cmd} "))
                    || line.trim_start() == cmd
                    || line.trim_start().starts_with(&format!("{cmd}\t"))
            })
            .unwrap_or_else(|| {
                panic!(
                    "could not find row for subcommand `{cmd}` in `aicx --help` output:\n{stdout}"
                )
            });
        assert!(
            !line.to_ascii_lowercase().contains("(layer 1)"),
            "subcommand `{cmd}` row still mentions `(layer 1)` jargon: {line}"
        );
    }
    assert!(
        !stdout
            .lines()
            .any(|line| line.trim_start().starts_with("store ")),
        "retired `store` command must not return to primary help"
    );
}
