#![cfg(feature = "app")]

use serde_json::json;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BASELINE_SESSION_ID: &str = "019faicx-mcp-refresh-e2e-0001";
const NEW_SESSION_ID: &str = "019faicx-mcp-refresh-e2e-0002";
const FAILURE_SESSION_ID: &str = "019faicx-mcp-refresh-e2e-0003";
const BASELINE_TOKEN: &str = "MCP_AUTO_REFRESH_BASELINE_7f2a9c";
const NEW_SOURCE_TOKEN: &str = "MCP_AUTO_REFRESH_NEW_SOURCE_a45c90";
const OVERLAP_TOKEN: &str = "MCP_AUTO_REFRESH_OVERLAP_b183de";

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn unique_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-mcp-auto-refresh-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ))
}

fn source_path(home: &Path, session_id: &str) -> PathBuf {
    home.join(".codex/sessions/2026/08/24")
        .join(format!("rollout-{session_id}.jsonl"))
}

fn seed_source(home: &Path, session_id: &str, token: &str) -> PathBuf {
    let path = source_path(home, session_id);
    fs::create_dir_all(path.parent().expect("source parent")).expect("create source parent");
    let rows = [
        json!({
            "timestamp": "2026-08-24T12:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": "2026-08-24T12:00:00Z",
                "cwd": "/Users/test/Git/aicx",
                "model": "gpt-test"
            }
        }),
        json!({
            "timestamp": "2026-08-24T12:00:01Z",
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": format!("Auto refresh fixture {token}")
            }
        }),
    ];
    fs::write(
        &path,
        rows.iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n",
    )
    .expect("write source");
    path
}

fn append_source(path: &Path, token: &str) {
    let row = json!({
        "timestamp": "2026-08-24T12:00:02Z",
        "type": "event_msg",
        "payload": {
            "type": "user_message",
            "message": format!("Overlap refresh fixture {token}")
        }
    });
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open source for append");
    writeln!(file, "{row}").expect("append source");
    file.sync_all().expect("sync source append");
}

fn spawn_daemon(home: &Path) -> Daemon {
    let child = Command::new(env!("CARGO_BIN_EXE_aicx-mcp"))
        .args([
            "--transport",
            "http",
            "--host",
            "127.0.0.1",
            "--port",
            "0",
            "--no-require-auth",
            "--refresh-interval-seconds",
            "10",
        ])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AICX_ALLOW_TMP", "1")
        .env("RUST_LOG", "mcp.refresh=debug")
        .env_remove("AICX_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn isolated MCP daemon");
    Daemon(child)
}

fn run_search(home: &Path, token: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aicx"))
        .args(["search", token, "--json", "--hours", "0", "--limit", "5"])
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AICX_ALLOW_TMP", "1")
        .env_remove("AICX_HOME")
        .output()
        .expect("run search")
}

fn run_aicx(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aicx"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AICX_ALLOW_TMP", "1")
        .env_remove("AICX_HOME")
        .output()
        .expect("run aicx")
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed\nstatus={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_for_search(home: &Path, token: &str, attempts: usize) {
    for _ in 0..attempts {
        let output = run_search(home, token);
        if output.status.success() && String::from_utf8_lossy(&output.stdout).contains(token) {
            return;
        }
        thread::sleep(Duration::from_secs(1));
    }
    let output = run_search(home, token);
    panic!(
        "token did not become searchable\nstatus={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn current_pointer(home: &Path) -> String {
    fs::read_to_string(home.join(".aicx/indexed/_all/hybrid/CURRENT")).expect("read CURRENT")
}

fn generation_count(home: &Path) -> usize {
    fs::read_dir(home.join(".aicx/indexed/_all/hybrid/generations"))
        .expect("read generation root")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("g-"))
        .count()
}

#[test]
fn http_auto_refresh_publishes_noops_and_serializes_overlap() {
    let root = unique_root();
    let home = root.join("home");
    fs::create_dir_all(&home).expect("create isolated home");
    let source = seed_source(&home, BASELINE_SESSION_ID, BASELINE_TOKEN);
    assert_success(
        &run_aicx(&home, &["catalog", "rebuild", "--json"]),
        "baseline catalog rebuild",
    );
    assert_success(
        &run_aicx(&home, &["index", "--json", "--full-rescan"]),
        "baseline index",
    );

    let first = spawn_daemon(&home);
    wait_for_search(&home, BASELINE_TOKEN, 5);
    let first_current = current_pointer(&home);
    let first_generations = generation_count(&home);

    thread::sleep(Duration::from_secs(12));
    assert_eq!(
        current_pointer(&home),
        first_current,
        "an unchanged refresh tick must not publish a meaningless generation"
    );
    assert_eq!(generation_count(&home), first_generations);

    seed_source(&home, NEW_SESSION_ID, NEW_SOURCE_TOKEN);
    wait_for_search(&home, NEW_SOURCE_TOKEN, 30);
    let new_source_current = current_pointer(&home);
    let new_source_generations = generation_count(&home);
    assert_ne!(new_source_current, first_current);
    assert_eq!(new_source_generations, first_generations + 1);

    append_source(&source, OVERLAP_TOKEN);
    let second = spawn_daemon(&home);
    wait_for_search(&home, OVERLAP_TOKEN, 30);
    let overlap_current = current_pointer(&home);
    assert_ne!(overlap_current, new_source_current);
    thread::sleep(Duration::from_secs(12));
    assert_eq!(
        current_pointer(&home),
        overlap_current,
        "overlapping daemons must serialize one meaningful publication"
    );
    assert_eq!(generation_count(&home), new_source_generations);
    drop(second);

    let failure_source = seed_source(&home, FAILURE_SESSION_ID, "MCP_REFRESH_FAILURE_SENTINEL");
    let mut permissions = fs::metadata(&failure_source)
        .expect("failure source metadata")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(0o000);
    }
    fs::set_permissions(&failure_source, permissions).expect("make source unreadable");
    let retained_current = current_pointer(&home);
    thread::sleep(Duration::from_secs(12));
    assert_eq!(
        current_pointer(&home),
        retained_current,
        "a failed refresh must preserve last-good CURRENT"
    );
    wait_for_search(&home, OVERLAP_TOKEN, 5);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&failure_source, fs::Permissions::from_mode(0o600))
            .expect("restore source permissions");
    }

    drop(first);
    fs::remove_dir_all(&root).expect("remove isolated home");
}
