//! Regression: B-P0-03 — non-blocking mutation warning on bare no-arg
//! invocations of write commands.
//!
//! Wave D Cut D1 (2026-05-25) adds a one-line stderr note before any of
//! the seven mutation subcommands (`all`, `claude`, `codex`, `store`,
//! `migrate`, `migrate-intent-schema`, `index`) start writing to
//! the resolved AICX home. The note is informational only — operators can still
//! proceed; shipped scripts and automations can opt out via the
//! `AICX_NO_MUTATION_WARN=1` env var.
//!
//! Tests pin:
//! - warning text is emitted on bare invocations
//! - warning is suppressed when `AICX_NO_MUTATION_WARN=1`
//! - the configurable delay env var is honoured (default 3s; we exercise
//!   `AICX_MUTATION_WARN_DELAY_SECONDS=0` so the test stays fast)
//! - dry-run modes of `migrate` / `migrate-intent-schema` / `index` skip
//!   the warning (no mutation is about to happen)
//!
//! All tests use a per-invocation `AICX_HOME` so they never touch the
//! real operator store and never race against each other.
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

fn unique_aicx_home(label: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "aicx-mutwarn-{label}-{}-{suffix}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp aicx home");
    dir
}

fn warn_text_fragment_for(home: &std::path::Path) -> String {
    format!("about to write to {}", home.display())
}

#[test]
fn migrate_emits_mutation_warning_on_bare_invocation() {
    // `aicx migrate` (no `--dry-run`) is one of the seven mutation
    // subcommands per Wave A1 / Wave B B-P0-03. We pass `--legacy-root` to
    // an empty dir so the migration completes quickly (no legacy contents
    // to copy) — the warning fires before the work begins regardless.
    let bin = ensure_aicx_binary_exists();
    let home = unique_aicx_home("migrate-bare");
    let legacy = unique_aicx_home("migrate-legacy-src");

    let output = Command::new(&bin)
        .arg("migrate")
        .arg("--legacy-root")
        .arg(&legacy)
        .arg("--store-root")
        .arg(&home)
        .env("AICX_HOME", &home)
        .env("AICX_MUTATION_WARN_DELAY_SECONDS", "0")
        .env_remove("AICX_NO_MUTATION_WARN")
        .output()
        .expect("run aicx migrate");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("aicx migrate: note:"),
        "stderr should include mutation warning header; got:\n{stderr}"
    );
    assert!(
        stderr.contains(&warn_text_fragment_for(&home)),
        "stderr should mention resolved aicx home {}; got:\n{stderr}",
        home.display()
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&legacy);
}

#[test]
fn migrate_dry_run_skips_mutation_warning() {
    // Dry-run mode does not actually mutate `~/.aicx/`, so the operator
    // does not need the confirmation pause.
    let bin = ensure_aicx_binary_exists();
    let home = unique_aicx_home("migrate-dry");
    let legacy = unique_aicx_home("migrate-dry-legacy");

    let output = Command::new(&bin)
        .arg("migrate")
        .arg("--dry-run")
        .arg("--legacy-root")
        .arg(&legacy)
        .arg("--store-root")
        .arg(&home)
        .env("AICX_HOME", &home)
        .env("AICX_MUTATION_WARN_DELAY_SECONDS", "0")
        .env_remove("AICX_NO_MUTATION_WARN")
        .output()
        .expect("run aicx migrate --dry-run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("aicx migrate: note:"),
        "dry-run must NOT emit the mutation warning; got:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&legacy);
}

#[test]
fn aicx_no_mutation_warn_env_suppresses_warning() {
    // Shipped automation (vc-init, vibecrafted-mcp, install.sh) sets
    // `AICX_NO_MUTATION_WARN=1` so the pause never blocks scripted runs.
    let bin = ensure_aicx_binary_exists();
    let home = unique_aicx_home("noenv");
    let legacy = unique_aicx_home("noenv-legacy");

    let output = Command::new(&bin)
        .arg("migrate")
        .arg("--legacy-root")
        .arg(&legacy)
        .arg("--store-root")
        .arg(&home)
        .env("AICX_HOME", &home)
        .env("AICX_NO_MUTATION_WARN", "1")
        // delay env should be ignored when suppression is active
        .env("AICX_MUTATION_WARN_DELAY_SECONDS", "0")
        .output()
        .expect("run aicx migrate with AICX_NO_MUTATION_WARN=1");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("aicx migrate: note:"),
        "AICX_NO_MUTATION_WARN=1 must suppress the mutation warning; got:\n{stderr}"
    );
    assert!(
        !stderr.contains(&warn_text_fragment_for(&home)),
        "suppression env must also suppress the body fragment; got:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&legacy);
}

#[test]
fn mutation_warning_uses_bootstrap_storage_home_when_env_unset() {
    // Mirrors the installer path: config stays at $HOME/.aicx/config.toml,
    // while [storage].home points runtime writes at a custom root.
    let bin = ensure_aicx_binary_exists();
    let bootstrap_home = unique_aicx_home("bootstrap-home");
    let storage_root = unique_aicx_home("bootstrap-storage-root");
    let legacy = storage_root.join("legacy-src");
    std::fs::create_dir_all(&legacy).expect("create bootstrap legacy root");
    let bootstrap_config_dir = bootstrap_home.join(".aicx");
    std::fs::create_dir_all(&bootstrap_config_dir).expect("create bootstrap config dir");
    std::fs::write(
        bootstrap_config_dir.join("config.toml"),
        // A Windows `C:\Users\…` value embeds `\U`, which a TOML *basic*
        // (double-quoted) string parses as a unicode escape and rejects. Use a
        // TOML literal (single-quoted) string, which performs no escaping, so
        // the backslash path round-trips verbatim into the resolved-root note
        // the assertion below checks.
        format!("[storage]\nhome = '{}'\n", storage_root.display()),
    )
    .expect("write bootstrap storage config");

    let output = Command::new(&bin)
        .arg("migrate")
        .arg("--legacy-root")
        .arg(&legacy)
        .arg("--store-root")
        .arg(&storage_root)
        .env("HOME", &bootstrap_home)
        // Windows resolves the home dir from USERPROFILE, not HOME (dirs::home_dir).
        .env("USERPROFILE", &bootstrap_home)
        .env_remove("AICX_HOME")
        .env("AICX_ALLOW_TMP", "1")
        .env("AICX_MUTATION_WARN_DELAY_SECONDS", "0")
        .env_remove("AICX_NO_MUTATION_WARN")
        .output()
        .expect("run aicx migrate with bootstrap storage.home");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "migrate should succeed against isolated bootstrap storage root; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("aicx migrate: note:"),
        "stderr should include mutation warning header; got:\n{stderr}"
    );
    assert!(
        stderr.contains(&warn_text_fragment_for(&storage_root)),
        "stderr should mention resolved storage root {}; got:\n{stderr}",
        storage_root.display()
    );
    assert!(
        !stderr.contains(&warn_text_fragment_for(&bootstrap_config_dir)),
        "warning must not describe the bootstrap config dir as the storage root; got:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&bootstrap_home);
    let _ = std::fs::remove_dir_all(&storage_root);
    let _ = std::fs::remove_dir_all(&legacy);
}

#[test]
fn aicx_mutation_warn_delay_zero_skips_sleep_but_keeps_warning() {
    // Delay=0 is a safe shorthand: keep the note (still informational
    // value) but skip the sleep entirely. Bounded test runtime depends
    // on this.
    let bin = ensure_aicx_binary_exists();
    let home = unique_aicx_home("delay0");
    let legacy = unique_aicx_home("delay0-legacy");

    let start = std::time::Instant::now();
    let output = Command::new(&bin)
        .arg("migrate")
        .arg("--legacy-root")
        .arg(&legacy)
        .arg("--store-root")
        .arg(&home)
        .env("AICX_HOME", &home)
        .env("AICX_MUTATION_WARN_DELAY_SECONDS", "0")
        .env_remove("AICX_NO_MUTATION_WARN")
        .output()
        .expect("run aicx migrate delay=0");
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 3,
        "delay=0 must skip the 3s sleep (elapsed: {:?})",
        elapsed
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("aicx migrate: note:"),
        "delay=0 must still emit the warning text; got:\n{stderr}"
    );
    // The variant with delay 0 omits the "Ctrl-C within Ns" prompt fragment
    // to keep the note honest about what was skipped.
    assert!(
        !stderr.contains("Ctrl-C within 0s"),
        "delay=0 must not advertise a Ctrl-C window of 0 seconds; got:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&legacy);
}

#[test]
fn migrate_intent_schema_dry_run_skips_mutation_warning() {
    // Per the dry-run-aware wiring on the three migrate-family commands,
    // `--dry-run` must keep the operator surface noiseless.
    let bin = ensure_aicx_binary_exists();
    let home = unique_aicx_home("mis-dry");

    let output = Command::new(&bin)
        .arg("migrate-intent-schema")
        .arg("--dry-run")
        .arg("--store-root")
        .arg(&home)
        .env("AICX_HOME", &home)
        .env("AICX_MUTATION_WARN_DELAY_SECONDS", "0")
        .env_remove("AICX_NO_MUTATION_WARN")
        .output()
        .expect("run aicx migrate-intent-schema --dry-run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("aicx migrate-intent-schema: note:"),
        "dry-run must NOT emit the mutation warning; got:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&home);
}
