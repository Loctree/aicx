//! After this version is configured, bare `aicx` prints short help.
//!
//! First start (no survey / what's-new markers) still prints the dashboard
//! URL. Configured homes must not reopen the wizard, must not open a
//! browser, and must not list hidden commands.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

fn aicx_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aicx"))
}

fn unique_home(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "aicx-bare-help-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp AICX_HOME");
    root
}

fn run_bare(home: &Path, extra_env: &[(&str, &str)]) -> (String, String, bool) {
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut cmd = Command::new(aicx_bin());
    cmd.env("AICX_HOME", home)
        .env("AICX_ONBOARDING_DRY_RUN", "1")
        .env("AICX_ONBOARDING_NO_OPEN", "1")
        .env("AICX_MCP_PORT", "18099")
        .env("CI", "1");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("run bare aicx");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn hidden_command_listed(help: &str, name: &str) -> bool {
    help.lines()
        .any(|line| line.trim_start().starts_with(&format!("{name} ")))
}

#[test]
fn unconfigured_home_prints_the_dashboard_url() {
    let home = unique_home("first");
    let (stdout, stderr, ok) = run_bare(&home, &[]);
    let combined = format!("{stdout}{stderr}");
    assert!(ok, "bare aicx on an empty home failed:\n{combined}");
    assert!(
        combined.contains("http://127.0.0.1:18099/") || combined.contains("http://127.0.0.1:8044/"),
        "first start must print the dashboard URL:\n{combined}"
    );
    assert!(
        combined.contains("Type the phrases you use")
            && combined.contains("Save stores them on this machine"),
        "first start must name the save in plain language:\n{combined}"
    );
    assert!(
        !combined.contains("intent_phrases.toml") && !combined.contains("config.toml"),
        "first start must not name config files:\n{combined}"
    );
    assert!(
        !combined.contains("Usage: aicx"),
        "first start must not print short help instead of the dashboard:\n{combined}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn configured_home_prints_short_help_without_hidden_commands() {
    let home = unique_home("configured");
    std::fs::create_dir_all(home.join("state")).unwrap();
    std::fs::write(home.join("intent_phrases.toml"), "phrases = [\"find\"]\n").unwrap();
    std::fs::write(
        home.join("config.toml"),
        "[embedder]\nbackend = \"gguf\"\nprofile = \"base\"\n",
    )
    .unwrap();
    std::fs::write(
        home.join("state/whats-new-offered"),
        format!("{}\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    std::fs::write(
        home.join("state/first-run-complete"),
        "first-run-complete\n",
    )
    .unwrap();
    let unit = home.join("aicx-mcp.plist");
    std::fs::write(
        &unit,
        "--transport http --host 127.0.0.1 --port 18099 --no-require-auth --experimental-auto-refresh\n",
    )
    .unwrap();

    let (stdout, stderr, ok) = run_bare(
        &home,
        &[("AICX_SERVICE_UNIT", unit.to_str().expect("unit path"))],
    );
    let combined = format!("{stdout}{stderr}");
    assert!(ok, "bare aicx after configuration failed:\n{combined}");
    assert!(
        !stdout.trim().is_empty(),
        "configured bare help must not be empty\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("Usage: aicx"),
        "configured bare aicx must print short help:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("index") && stdout.contains("search"),
        "short help must list the daily drivers:\n{stdout}"
    );
    assert!(
        !stdout.contains("repair-runtime") || stdout.contains("doctor"),
        "short help must not be only a repair-runtime hint:\n{stdout}"
    );
    for hidden in [
        "dashboard",
        "catalog",
        "reports",
        "intents",
        "migrate",
        "claude",
        "codex",
    ] {
        assert!(
            !hidden_command_listed(&stdout, hidden),
            "{hidden} must stay hidden from configured bare help:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("http://127.0.0.1:18099/") && !stdout.contains("http://127.0.0.1:8044/"),
        "configured bare aicx must not print the dashboard URL:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn help_full_still_lists_power_user_commands() {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    let bin = BIN.get_or_init(aicx_bin);
    let output = Command::new(bin)
        .args(["--help-full"])
        .output()
        .expect("run --help-full");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "aicx --help-full failed");
    for shown in ["catalog", "dashboard", "intents", "migrate"] {
        assert!(
            hidden_command_listed(&stdout, shown) || stdout.contains(&format!("\n  {shown} ")),
            "{shown} must appear in --help-full:\n{stdout}"
        );
    }
}
