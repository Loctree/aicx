use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-runtime-cli-{name}-{}-{}",
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

fn write_codex_history(
    path: &Path,
    session_id: &str,
    cwd: Option<&Path>,
    messages: &[(&str, i64, &str)],
) {
    let timestamp = |value: i64| {
        chrono::DateTime::from_timestamp(value, 0)
            .expect("fixture timestamp")
            .to_rfc3339()
    };
    let first_ts = messages.first().map(|(_, ts, _)| *ts).unwrap_or(0);
    let mut lines = vec![
        json!({
            "timestamp": timestamp(first_ts),
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": timestamp(first_ts),
                "cwd": cwd.map(|path| path.display().to_string()).unwrap_or_default(),
                "model": "gpt-test"
            }
        })
        .to_string(),
    ];
    for (role, ts, text) in messages {
        lines.push(
            json!({
                "timestamp": timestamp(*ts),
                "type": "event_msg",
                "payload": {
                    "type": if *role == "user" { "user_message" } else { "agent_message" },
                    "message": text
                }
            })
            .to_string(),
        );
    }

    write_file(path, &lines.join("\n"));
}

fn write_claude_session_fixture(path: &Path, session_id: Option<&str>, text: &str) {
    write_claude_session_fixture_with_cwd(path, session_id, "/repo", text);
}

// SYNTHETIC fixture writer (same minimal Claude JSONL shape as
// tests/fixtures/parser_engine/claude/minimal.jsonl); cwd is the
// discovery-filter axis under test, not captured operator material.
fn write_claude_session_fixture_with_cwd(
    path: &Path,
    session_id: Option<&str>,
    cwd: &str,
    text: &str,
) {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let mut row = json!({
        "timestamp": timestamp,
        "type": "user",
        "cwd": cwd,
        "message": {"role": "user", "content": text}
    });
    if let Some(session_id) = session_id {
        row["sessionId"] = Value::String(session_id.to_owned());
    }
    write_file(path, &row.to_string());
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
        .env("AICX_ALLOW_TMP", "1")
        .env_remove("AICX_HOME")
        .output()
        .expect("run aicx")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn parse_stdout_json(output: &Output) -> Value {
    assert_success(output);
    serde_json::from_slice(&output.stdout).expect("parse stdout json")
}

fn parse_stdout_json_allow_failure(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "parse stdout json\nerror: {err}\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn json_paths(value: &Value, key: &str) -> Vec<PathBuf> {
    value[key]
        .as_array()
        .expect("json array")
        .iter()
        .map(|path| {
            PathBuf::from(
                path.as_str()
                    .expect("string path in json payload")
                    .to_string(),
            )
        })
        .collect()
}

fn json_strings(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .expect("json array")
        .iter()
        .map(|entry| entry.as_str().expect("json string").to_string())
        .collect()
}

fn read_state(home: &Path) -> Value {
    serde_json::from_str(
        &fs::read_to_string(home.join(".aicx").join("state.json")).expect("read state.json"),
    )
    .expect("parse state.json")
}

fn append_codex_entry(
    path: &Path,
    session_id: &str,
    cwd: Option<&Path>,
    role: &str,
    ts: i64,
    text: &str,
) {
    let _ = (session_id, cwd);
    let timestamp = chrono::DateTime::from_timestamp(ts, 0)
        .expect("fixture timestamp")
        .to_rfc3339();
    let payload = json!({
        "timestamp": timestamp,
        "type": "event_msg",
        "payload": {
            "type": if role == "user" { "user_message" } else { "agent_message" },
            "message": text
        }
    });

    let mut existing = fs::read_to_string(path).unwrap_or_default();
    if !existing.is_empty() {
        existing.push('\n');
    }
    existing.push_str(&payload.to_string());
    write_file(path, &existing);
}

#[test]
fn read_cli_returns_chunk_metadata_and_content() {
    let root = unique_test_dir("read-command");
    let home = root.join("home");
    let chunk = home
        .join(".aicx")
        .join("store")
        .join("vetcoders")
        .join("aicx")
        .join("2026_0502")
        .join("reports")
        .join("codex")
        .join("2026_0502_codex_sess-read01_001.md");
    write_file(&chunk, "Decision: make read the re-entry primitive.");

    let output = parse_stdout_json(&run_aicx(
        &home,
        &[
            "read",
            "store/vetcoders/aicx/2026_0502/reports/codex/2026_0502_codex_sess-read01_001.md",
            "--max-chars",
            "13",
            "--json",
        ],
    ));

    assert_eq!(output["project"].as_str(), Some("vetcoders/aicx"));
    assert_eq!(output["kind"].as_str(), Some("reports"));
    assert_eq!(output["agent"].as_str(), Some("codex"));
    assert_eq!(output["session_id"].as_str(), Some("sess-read01"));
    assert_eq!(output["chunk"].as_u64(), Some(1));
    assert_eq!(output["content"].as_str(), Some("Decision: mak"));
    assert_eq!(output["truncated"].as_bool(), Some(true));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_doctor_fix_critical_returns_non_zero_exit() {
    let root = unique_test_dir("doctor-fix-critical-exit");
    let home = root.join("home");
    let chunk = home
        .join(".aicx")
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0520")
        .join("conversations")
        .join("codex")
        .join("2026_0520_codex_sess-critical_001.md");
    write_file(&chunk, "critical chunk intentionally missing its sidecar");

    let output = run_aicx(&home, &["doctor", "--fix", "--format", "json"]);
    assert!(
        !output.status.success(),
        "doctor --fix must return non-zero when the post-fix report is still Critical\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report = parse_stdout_json_allow_failure(&output);
    assert_eq!(report["overall"].as_str(), Some("critical"));
    assert_eq!(
        report["sidecars"]["severity"].as_str(),
        Some("critical"),
        "missing sidecars should keep the post-fix report critical"
    );
    assert_eq!(
        report["sidecars"], report["sidecar_coverage"],
        "doctor JSON should expose the same sidecar check result under both legacy fields"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_doctor_force_yes_json_is_machine_readable_cleanup_report() {
    let root = unique_test_dir("doctor-force-yes-json");
    let home = root.join("home");

    let output = run_aicx(&home, &["doctor", "--force", "--yes", "--format", "json"]);
    let report = parse_stdout_json(&output);
    assert_eq!(report["mode"].as_str(), Some("force"));
    assert!(
        report["selected"].as_array().is_some(),
        "force cleanup JSON should expose selected actions"
    );
    assert!(
        report["final_report"]["overall"].is_string(),
        "force cleanup JSON should include final doctor report"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn list_cli_reports_unprotected_sources_without_creating_git() {
    let root = unique_test_dir("source-list-unprotected");
    let home = root.join("home");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    write_codex_history(&history, "source-list-sess", None, &[("user", 1, "hello")]);

    let output = run_aicx(&home, &["list"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("unprotected source material"));
    assert!(
        !home.join(".codex").join(".git").exists(),
        "list must stay read-only and never initialize git"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn list_cli_detects_existing_git_protection() {
    let root = unique_test_dir("source-list-protected");
    let home = root.join("home");
    let codex_root = home.join(".codex");
    let history = codex_root.join("sessions/2026/07/13/rollout-test.jsonl");
    fs::create_dir_all(codex_root.join(".git")).expect("create local source git");
    write_codex_history(
        &history,
        "source-list-protected-sess",
        None,
        &[("user", 1, "hello")],
    );

    let output = run_aicx(&home, &["list"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("protected by git-local"));
    assert!(stdout.contains("no remote"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn sources_protect_dry_run_does_not_initialize_git() {
    let root = unique_test_dir("source-protect-dry-run");
    let source_root = root.join("home").join(".codex");
    fs::create_dir_all(&source_root).expect("create source root");

    let root_arg = source_root.to_string_lossy().to_string();
    let output = run_aicx(
        &root.join("home"),
        &[
            "sources",
            "protect",
            "--root",
            &root_arg,
            "--backend",
            "git-local",
        ],
    );
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Dry run only"));
    assert!(!source_root.join(".git").exists());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn sources_protect_apply_creates_only_local_git_without_remote() {
    let root = unique_test_dir("source-protect-apply");
    let home = root.join("home");
    let source_root = home.join(".codex");
    fs::create_dir_all(&source_root).expect("create source root");
    write_file(
        &source_root.join("sessions/2026/07/13/rollout-test.jsonl"),
        "{\"text\":\"private local session\"}\n",
    );

    let root_arg = source_root.to_string_lossy().to_string();
    let output = run_aicx(
        &home,
        &[
            "sources",
            "protect",
            "--root",
            &root_arg,
            "--backend",
            "git-local",
            "--apply",
        ],
    );
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("source root protected"));
    assert!(stdout.contains("remote configured: no"));
    assert!(source_root.join(".git").is_dir());
    assert!(source_root.join(".gitignore").is_file());

    let remotes = Command::new("git")
        .arg("-C")
        .arg(&source_root)
        .args(["remote", "-v"])
        .output()
        .expect("git remote -v");
    assert_success(&remotes);
    assert!(remotes.stdout.is_empty());

    let sibling = home.join(".claude");
    assert!(!sibling.join(".git").exists());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn normal_store_and_extract_do_not_initialize_source_git() {
    let root = unique_test_dir("source-normal-readonly");
    let home = root.join("home");
    let source_root = home.join(".codex");
    let history = source_root.join("sessions/2026/07/13/rollout-test.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;
    write_codex_history(
        &history,
        "source-normal-readonly-sess",
        None,
        &[("user", now - 60, "private source root must stay read-only")],
    );

    let store_output = run_aicx(
        &home,
        &["store", "--agent", "codex", "-H", "24", "--emit", "json"],
    );
    assert_success(&store_output);
    assert!(
        !source_root.join(".git").exists(),
        "store must not initialize git in source roots"
    );

    let input_arg = history.display().to_string();
    let output_path = root.join("conversation.md");
    let output_arg = output_path.display().to_string();
    let extract_output = run_aicx(
        &home,
        &[
            "extract",
            "codex",
            "--file",
            &input_arg,
            "--conversation",
            "-o",
            &output_arg,
        ],
    );
    assert_success(&extract_output);
    assert!(
        !source_root.join(".git").exists(),
        "extract must not initialize git in source roots"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn store_cli_deduplicates_exact_entries_on_first_run() {
    let root = unique_test_dir("store-exact-dedup");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("Vetcoders").join("aicx");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_codex_history(
        &history,
        "store-dedup-sess",
        Some(&repo_root),
        &[
            ("user", now - 300, "duplicate store context"),
            ("user", now - 300, "duplicate store context"),
        ],
    );

    let output = parse_stdout_json(&run_aicx(
        &home,
        &["store", "--agent", "codex", "-H", "24", "--emit", "json"],
    ));
    assert_eq!(output["total_entries"].as_u64(), Some(1));

    let store_paths = json_paths(&output, "store_paths");
    let combined_store = store_paths
        .iter()
        .map(|path| fs::read_to_string(path).expect("read store chunk"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(combined_store.matches("duplicate store context").count(), 1);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn store_cli_codex_emits_repo_and_non_repo_canonical_roots() {
    let root = unique_test_dir("codex-command");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("Vetcoders").join("ai-contexters");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_codex_history(
        &history,
        "repo-sess",
        Some(&repo_root),
        &[
            (
                "user",
                now - 120,
                "Please inspect the repo-centric store seam.",
            ),
            (
                "assistant",
                now - 110,
                "Reviewing the runtime store truth now.",
            ),
        ],
    );
    write_codex_history(
        &history.with_file_name("rollout-nonrepo.jsonl"),
        "nonrepo-sess",
        None,
        &[
            (
                "user",
                now - 100,
                "Draft a migration plan before we know the repo.",
            ),
            (
                "assistant",
                now - 90,
                "Working without repository identity for now.",
            ),
        ],
    );

    let output = run_aicx(&home, &["codex", "-H", "24", "--emit", "json"]);
    let payload = parse_stdout_json(&output);
    let store_paths = json_paths(&payload, "store_paths");
    let resolved_repositories = json_strings(&payload, "resolved_repositories");

    assert!(
        store_paths.len() >= 2,
        "expected at least 2 store paths (repo + non-repo), got {}",
        store_paths.len()
    );
    assert!(payload["requested_source_filters"].is_null());
    assert_eq!(
        resolved_repositories,
        vec!["Vetcoders/ai-contexters".to_string()]
    );
    assert_eq!(
        payload["includes_non_repository_contexts"].as_bool(),
        Some(true)
    );
    assert!(payload["resolved_store_buckets"]["Vetcoders/ai-contexters"].is_object());
    assert!(payload["resolved_store_buckets"]["non-repository-contexts"].is_object());
    assert!(store_paths.iter().any(|path| {
        path.starts_with(
            home.join(".aicx")
                .join("store")
                .join("Vetcoders")
                .join("ai-contexters"),
        )
    }));
    assert!(
        store_paths
            .iter()
            .any(|path| { path.starts_with(home.join(".aicx").join("non-repository-contexts")) })
    );
    assert!(
        store_paths
            .iter()
            .all(|path| !path.to_string_lossy().contains(".codex/history.jsonl"))
    );
    assert!(store_paths.iter().all(|path| path.exists()));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn store_cli_store_command_emits_repo_and_non_repo_canonical_roots() {
    let root = unique_test_dir("store-command");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("Vetcoders").join("loctree");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_codex_history(
        &history,
        "repo-store-sess",
        Some(&repo_root),
        &[
            (
                "user",
                now - 120,
                "Please inspect the loctree runtime contract.",
            ),
            ("assistant", now - 110, "Reviewing canonical emission now."),
        ],
    );
    write_codex_history(
        &history.with_file_name("rollout-unknown.jsonl"),
        "unknown-store-sess",
        None,
        &[
            ("user", now - 100, "Planning first, repository unknown."),
            ("assistant", now - 90, "Still unresolved; keep this honest."),
        ],
    );

    let output = run_aicx(
        &home,
        &["store", "--agent", "codex", "-H", "24", "--emit", "json"],
    );
    let payload = parse_stdout_json(&output);
    let store_paths = json_paths(&payload, "store_paths");
    let resolved_repositories = json_strings(&payload, "resolved_repositories");

    assert!(
        payload["total_entries"].as_u64().unwrap_or(0) >= 4,
        "expected at least 4 entries"
    );
    assert!(
        payload["total_chunks"].as_u64().unwrap_or(0) >= 2,
        "expected at least 2 chunks"
    );
    assert!(payload["requested_source_filters"].is_null());
    assert_eq!(resolved_repositories, vec!["Vetcoders/loctree".to_string()]);
    assert_eq!(
        payload["includes_non_repository_contexts"].as_bool(),
        Some(true)
    );
    assert!(payload["resolved_store_buckets"]["Vetcoders/loctree"].is_object());
    assert!(payload["resolved_store_buckets"]["non-repository-contexts"].is_object());
    assert!(payload["repos"]["Vetcoders/loctree"].is_object());
    assert!(payload["repos"].get("non-repository-contexts").is_none());
    assert!(
        store_paths.len() >= 2,
        "expected at least 2 store paths, got {}",
        store_paths.len()
    );
    assert!(store_paths.iter().any(|path| {
        path.starts_with(
            home.join(".aicx")
                .join("store")
                .join("Vetcoders")
                .join("loctree"),
        )
    }));
    assert!(
        store_paths
            .iter()
            .any(|path| { path.starts_with(home.join(".aicx").join("non-repository-contexts")) })
    );
    assert!(store_paths.iter().all(|path| path.exists()));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn migration_cli_rebuilds_and_salvages_realistic_bundle() {
    let root = unique_test_dir("migration-rebuild-salvage");
    let home = root.join("home");
    let legacy_root = root.join("legacy");
    let store_root = root.join("aicx");
    let repo_root = root.join("hosted").join("Vetcoders").join("ai-contexters");
    let source_dir = root.join("sources");
    let existing_source = source_dir.join("rollout-existing.jsonl");
    let missing_source = source_dir.join("rollout-missing.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_codex_history(
        &existing_source,
        "mig-sess",
        Some(&repo_root),
        &[
            ("user", now - 120, "Please inspect the migration seam."),
            (
                "assistant",
                now - 110,
                "Reviewing the repo-centric store now.",
            ),
        ],
    );
    write_file(
        &legacy_root
            .join("demo")
            .join("2026-03-21")
            .join("101045_codex-001.md"),
        &format!("input: {}\n", existing_source.display()),
    );
    write_file(
        &legacy_root
            .join("demo")
            .join("2026-03-21")
            .join("101045_codex-002.md"),
        &format!("input: {}\n", missing_source.display()),
    );
    write_file(&legacy_root.join("state.json"), "{\"seen_hashes\":[]}\n");

    let legacy_root_arg = legacy_root.to_string_lossy().to_string();
    let store_root_arg = store_root.to_string_lossy().to_string();
    let output = run_aicx(
        &home,
        &[
            "migrate",
            "--legacy-root",
            &legacy_root_arg,
            "--store-root",
            &store_root_arg,
        ],
    );
    assert_success(&output);

    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(store_root.join("migration").join("manifest.json"))
            .expect("read migration manifest"),
    )
    .expect("parse migration manifest");
    let items = manifest["items"].as_array().expect("manifest items array");

    let rebuilt_bundle = items
        .iter()
        .find(|item| item["legacy_group"].as_str() == Some("demo/2026-03-21/101045_codex"))
        .expect("bundle migration item");
    let canonical_paths = json_paths(rebuilt_bundle, "canonical_paths");
    let salvage_paths = json_paths(rebuilt_bundle, "salvage_paths");

    assert_eq!(
        rebuilt_bundle["action"].as_str(),
        Some("rebuild_and_salvage")
    );
    assert_eq!(
        rebuilt_bundle["action_reason"].as_str(),
        Some("partial_source_recovery")
    );
    assert!(
        !canonical_paths.is_empty(),
        "expected at least 1 canonical path, got {}",
        canonical_paths.len()
    );
    assert!(
        salvage_paths.len() >= 3,
        "expected at least 3 salvage paths, got {}",
        salvage_paths.len()
    );
    assert!(
        canonical_paths[0].starts_with(
            store_root
                .join("store")
                .join("Vetcoders")
                .join("ai-contexters")
        )
    );
    assert!(canonical_paths[0].exists());
    assert!(salvage_paths.iter().all(|path| path.exists()));

    let all_canonical_content: String = canonical_paths
        .iter()
        .map(|p| fs::read_to_string(p).expect("read rebuilt canonical chunk"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all_canonical_content.contains("Please inspect the migration seam."));
    assert!(all_canonical_content.contains("Reviewing the repo-centric store now."));
    assert!(!all_canonical_content.contains("input:"));

    let salvaged_legacy = fs::read_to_string(
        store_root
            .join("legacy-store")
            .join("demo")
            .join("2026-03-21")
            .join("101045_codex-001.md"),
    )
    .expect("read salvaged legacy file");
    assert!(salvaged_legacy.contains("input:"));
    assert!(
        legacy_root
            .join("demo/2026-03-21/101045_codex-001.md")
            .exists()
    );
    assert!(
        legacy_root
            .join("demo/2026-03-21/101045_codex-002.md")
            .exists()
    );

    let loose_state = items
        .iter()
        .find(|item| item["legacy_group"].as_str() == Some("state.json"))
        .expect("loose state migration item");
    assert_eq!(loose_state["action"].as_str(), Some("salvage"));
    assert!(
        json_paths(loose_state, "salvage_paths")
            .iter()
            .any(|path| { path.ends_with(Path::new("legacy-store").join("state.json")) })
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn all_cli_defaults_to_incremental_and_full_rescan_recovers_backfill() {
    let root = unique_test_dir("all-incremental-default");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("Vetcoders").join("aicx");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_codex_history(
        &history,
        "warm-state-sess",
        Some(&repo_root),
        &[
            ("user", now - 300, "old context: inspect the runtime seam"),
            (
                "assistant",
                now - 290,
                "old reply: inspecting the runtime seam now",
            ),
        ],
    );

    let first = parse_stdout_json(&run_aicx(&home, &["all", "-H", "24", "--emit", "json"]));
    assert_eq!(first["total_entries"].as_u64(), Some(2));

    append_codex_entry(
        &history,
        "warm-state-sess",
        Some(&repo_root),
        "user",
        now - 120,
        "new context: only this should land on the next incremental run",
    );

    let second = parse_stdout_json(&run_aicx(&home, &["all", "-H", "24", "--emit", "json"]));
    assert_eq!(second["total_entries"].as_u64(), Some(1));
    let second_entries = second["entries"].as_array().expect("entries array");
    assert_eq!(second_entries.len(), 1);
    assert_eq!(
        second_entries[0]["message"].as_str(),
        Some("new context: only this should land on the next incremental run")
    );

    append_codex_entry(
        &history,
        "warm-state-sess",
        Some(&repo_root),
        "user",
        now - 240,
        "late backfill: older than watermark but still inside the lookback window",
    );

    let third = parse_stdout_json(&run_aicx(&home, &["all", "-H", "24", "--emit", "json"]));
    assert_eq!(third["total_entries"].as_u64(), Some(0));

    let fourth = parse_stdout_json(&run_aicx(
        &home,
        &["all", "-H", "24", "--full-rescan", "--emit", "json"],
    ));
    assert_eq!(fourth["total_entries"].as_u64(), Some(4));
    let fourth_entries = fourth["entries"].as_array().expect("entries array");
    assert_eq!(fourth_entries.len(), 4);
    let fourth_messages = fourth_entries
        .iter()
        .filter_map(|entry| entry["message"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        fourth_messages,
        vec![
            "old context: inspect the runtime seam",
            "old reply: inspecting the runtime seam now",
            "late backfill: older than watermark but still inside the lookback window",
            "new context: only this should land on the next incremental run",
        ]
    );

    let state = read_state(&home);
    let expected_watermark = chrono::DateTime::<chrono::Utc>::from_timestamp(now - 120, 0)
        .expect("valid timestamp")
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    assert_eq!(
        state["last_processed"]["claude+codex+gemini+junie+grok+codescribe:all"].as_str(),
        Some(expected_watermark.as_str())
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn all_cli_hours_zero_means_all_time() {
    let root = unique_test_dir("all-hours-zero");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("Vetcoders").join("aicx");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_codex_history(
        &history,
        "all-time-sess",
        Some(&repo_root),
        &[(
            "user",
            now - (90 * 24 * 3600),
            "ancient context should still land with hours zero",
        )],
    );

    let windowed = parse_stdout_json(&run_aicx(&home, &["all", "-H", "48", "--emit", "json"]));
    assert_eq!(windowed["total_entries"].as_u64(), Some(0));

    let all_time = parse_stdout_json(&run_aicx(
        &home,
        &["all", "-H", "0", "--full-rescan", "--emit", "json"],
    ));
    assert_eq!(all_time["total_entries"].as_u64(), Some(1));
    assert_eq!(
        all_time["entries"][0]["message"].as_str(),
        Some("ancient context should still land with hours zero")
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn all_cli_force_ignores_watermark_like_full_rescan() {
    let root = unique_test_dir("all-force-watermark");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("Vetcoders").join("aicx");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_codex_history(
        &history,
        "force-watermark-sess",
        Some(&repo_root),
        &[
            ("user", now - 300, "force old context"),
            ("assistant", now - 290, "force old reply"),
        ],
    );

    let first = parse_stdout_json(&run_aicx(&home, &["all", "-H", "24", "--emit", "json"]));
    assert_eq!(first["total_entries"].as_u64(), Some(2));

    append_codex_entry(
        &history,
        "force-watermark-sess",
        Some(&repo_root),
        "user",
        now - 295,
        "force late backfill inside lookback",
    );

    let incremental = parse_stdout_json(&run_aicx(&home, &["all", "-H", "24", "--emit", "json"]));
    assert_eq!(incremental["total_entries"].as_u64(), Some(0));

    let forced = parse_stdout_json(&run_aicx(
        &home,
        &["all", "-H", "24", "--force", "--emit", "json"],
    ));
    assert_eq!(forced["total_entries"].as_u64(), Some(3));
    let forced_messages = forced["entries"]
        .as_array()
        .expect("entries array")
        .iter()
        .filter_map(|entry| entry["message"].as_str())
        .collect::<Vec<_>>();
    assert!(forced_messages.contains(&"force late backfill inside lookback"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn store_cli_defaults_to_incremental_and_full_rescan_recovers_backfill() {
    let root = unique_test_dir("store-incremental-default");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("Vetcoders").join("aicx");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_codex_history(
        &history,
        "store-watermark-sess",
        Some(&repo_root),
        &[
            ("user", now - 300, "store old context"),
            ("assistant", now - 290, "store old reply"),
        ],
    );

    let first = parse_stdout_json(&run_aicx(
        &home,
        &["store", "--agent", "codex", "-H", "24", "--emit", "json"],
    ));
    assert_eq!(first["total_entries"].as_u64(), Some(2));

    append_codex_entry(
        &history,
        "store-watermark-sess",
        Some(&repo_root),
        "user",
        now - 120,
        "store new context",
    );

    let second = parse_stdout_json(&run_aicx(
        &home,
        &["store", "--agent", "codex", "-H", "24", "--emit", "json"],
    ));
    assert_eq!(second["total_entries"].as_u64(), Some(1));

    append_codex_entry(
        &history,
        "store-watermark-sess",
        Some(&repo_root),
        "user",
        now - 240,
        "store late backfill",
    );

    let third = parse_stdout_json(&run_aicx(
        &home,
        &["store", "--agent", "codex", "-H", "24", "--emit", "json"],
    ));
    assert_eq!(third["total_entries"].as_u64(), Some(0));

    let fourth = parse_stdout_json(&run_aicx(
        &home,
        &[
            "store",
            "--agent",
            "codex",
            "-H",
            "24",
            "--full-rescan",
            "--emit",
            "json",
        ],
    ));
    assert_eq!(fourth["total_entries"].as_u64(), Some(4));

    let store_paths = json_paths(&fourth, "store_paths");
    let combined_store = store_paths
        .iter()
        .map(|path| fs::read_to_string(path).expect("read store chunk"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(combined_store.contains("store late backfill"));

    let state = read_state(&home);
    let expected_watermark = chrono::DateTime::<chrono::Utc>::from_timestamp(now - 120, 0)
        .expect("valid timestamp")
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    assert_eq!(
        state["last_processed"]["codex:all"].as_str(),
        Some(expected_watermark.as_str())
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn store_cli_hours_zero_means_all_time() {
    let root = unique_test_dir("store-hours-zero");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("Vetcoders").join("aicx");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_codex_history(
        &history,
        "store-all-time-sess",
        Some(&repo_root),
        &[(
            "user",
            now - (90 * 24 * 3600),
            "store ancient context should still land with hours zero",
        )],
    );

    let windowed = parse_stdout_json(&run_aicx(
        &home,
        &["store", "--agent", "codex", "-H", "48", "--emit", "json"],
    ));
    assert_eq!(windowed["total_entries"].as_u64(), Some(0));

    let all_time = parse_stdout_json(&run_aicx(
        &home,
        &[
            "store",
            "--agent",
            "codex",
            "-H",
            "0",
            "--full-rescan",
            "--emit",
            "json",
        ],
    ));
    assert_eq!(all_time["total_entries"].as_u64(), Some(1));

    let store_paths = json_paths(&all_time, "store_paths");
    let combined_store = store_paths
        .iter()
        .map(|path| fs::read_to_string(path).expect("read store chunk"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(combined_store.contains("store ancient context should still land with hours zero"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_run_store_saves_state_on_empty_result() {
    let root = unique_test_dir("store-empty-save");
    let home = root.join("home");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    write_codex_history(&history, "empty-sess", None, &[]);

    let output = parse_stdout_json(&run_aicx(
        &home,
        &["store", "--agent", "codex", "-H", "24", "--emit", "json"],
    ));
    assert_eq!(output["total_entries"].as_u64(), Some(0));

    let state = read_state(&home);
    let runs = state["runs"].as_array().expect("runs array in state.json");
    assert_eq!(
        runs.len(),
        1,
        "state should save run history even when no entries were extracted via store"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_run_extraction_saves_state_on_empty_result() {
    let root = unique_test_dir("extract-empty-save");
    let home = root.join("home");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("13")
        .join("rollout-test.jsonl");
    write_codex_history(&history, "empty-sess", None, &[]);

    let output = parse_stdout_json(&run_aicx(&home, &["all", "-H", "24", "--emit", "json"]));
    assert_eq!(output["total_entries"].as_u64(), Some(0));

    let state = read_state(&home);
    let runs = state["runs"].as_array().expect("runs array in state.json");
    assert_eq!(
        runs.len(),
        1,
        "state should save run history even when no entries were extracted via all/extract"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn session_batch_quarantines_bad_claude_source_and_holds_watermark() {
    let root = unique_test_dir("claude-batch-quarantine");
    let home = root.join("home");
    let sessions = home.join(".claude").join("projects").join("project");
    let healthy = sessions.join("11111111-1111-4111-8111-111111111111.jsonl");
    let invalid = sessions.join("22222222-2222-4222-8222-222222222222.jsonl");
    write_claude_session_fixture(&healthy, Some("healthy-session"), "healthy prompt");
    write_claude_session_fixture(&invalid, None, "identity-free prompt");

    let output = run_aicx(&home, &["claude", "-H", "24", "--emit", "json"]);
    assert_success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("session ingest summary: ingested=1 skipped=1"));
    let invalid_name = invalid
        .file_name()
        .expect("invalid fixture file name")
        .to_string_lossy()
        .into_owned();
    // Path rendering differs per platform (Windows may canonicalize with a
    // verbatim \\?\ prefix); the unique fixture file name proves the skip
    // points at the offending source without asserting the exact rendering.
    assert!(stderr.contains(&invalid_name));
    assert!(stderr.contains("session_id=22222222-2222-4222-8222-222222222222"));
    assert!(stderr.contains("recover: aicx extract claude --file"));
    assert!(stderr.contains("Watermark held: 1 skipped session(s)"));
    assert!(stderr.contains("Canonical projection:"));

    let projection_dir = home
        .join(".aicx")
        .join("store")
        .join("canonical-projection-v1");
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(projection_dir.join("manifest.json"))
            .expect("canonical projection manifest"),
    )
    .expect("valid canonical projection manifest");
    let card_ids = manifest["card_ids"].as_array().expect("manifest card ids");
    assert_eq!(card_ids.len(), 1, "only the healthy session may project");
    let card_name = format!(
        "{}.json",
        card_ids[0].as_str().expect("card id").replace(':', "_")
    );
    let card: Value = serde_json::from_str(
        &fs::read_to_string(projection_dir.join("cards").join(card_name)).expect("canonical card"),
    )
    .expect("valid canonical card");
    assert_eq!(card["session_id"].as_str(), Some("healthy-session"));

    let state = read_state(&home);
    assert_eq!(
        state["last_processed"].as_object().map(|map| map.len()),
        Some(0),
        "a skipped session must keep the batch watermark retryable"
    );
    let diagnostics_dir = home.join(".aicx").join("state");
    let diagnostic = fs::read_dir(&diagnostics_dir)
        .expect("diagnostics directory")
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("diagnostics-")
        })
        .expect("diagnostics log");
    let log = fs::read_to_string(diagnostic.path()).expect("read diagnostics log");
    assert!(log.contains("session_skip agent=claude"));
    assert!(log.contains(&invalid_name));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn session_batch_returns_exit_three_when_every_session_is_quarantined() {
    let root = unique_test_dir("claude-batch-all-skipped");
    let home = root.join("home");
    let invalid = home
        .join(".claude")
        .join("projects")
        .join("project")
        .join("33333333-3333-4333-8333-333333333333.jsonl");
    write_claude_session_fixture(&invalid, None, "identity-free prompt");

    let output = run_aicx(&home, &["claude", "-H", "24"]);
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("session ingest summary: ingested=0 skipped=1"));
    assert!(stderr.contains("all 1 selected session(s) were skipped"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn conversations_batch_all_skipped_exits_three_like_other_batches() {
    // O4 (transplant P3-01): three divergent all-skipped paths — the
    // conversations batch used a generic bail (exit 1) while extraction
    // and store batches exit 3. One contract: all-skipped → exit 3.
    let root = unique_test_dir("conversations-all-skipped");
    let home = root.join("home");
    let out_dir = root.join("out");
    let invalid = home
        .join(".claude")
        .join("projects")
        .join("project")
        .join("77777777-7777-4777-8777-777777777777.jsonl");
    write_claude_session_fixture(&invalid, None, "identity-free prompt");

    let output = run_aicx(
        &home,
        &[
            "conversations",
            "-H",
            "24",
            "--out-dir",
            out_dir.to_str().expect("utf-8 out dir"),
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "all-skipped conversations batch must exit 3\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("all 1 selected session(s) were skipped"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn conversations_batch_empty_window_exits_zero() {
    // Empty discovery window is an empty result, not a failure.
    let root = unique_test_dir("conversations-empty-window");
    let home = root.join("home");
    let out_dir = root.join("out");

    let output = run_aicx(
        &home,
        &[
            "conversations",
            "-H",
            "24",
            "--out-dir",
            out_dir.to_str().expect("utf-8 out dir"),
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "empty window must exit 0\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn project_filter_narrows_batch_discovery() {
    // O1 (`~/.aicx/aicx-problems.md` 2026-07-17 15:12 UTC): `aicx claude -p X`
    // promises "narrows session discovery before repo segmentation", but batch
    // discovery ignored the filter and ingested every session in the window.
    let root = unique_test_dir("claude-batch-project-filter");
    let home = root.join("home");
    let alpha = home
        .join(".claude")
        .join("projects")
        .join("-repo-alpha")
        .join("44444444-4444-4444-8444-444444444444.jsonl");
    let beta = home
        .join(".claude")
        .join("projects")
        .join("-repo-beta")
        .join("55555555-5555-4555-8555-555555555555.jsonl");
    write_claude_session_fixture_with_cwd(
        &alpha,
        Some("alpha-session"),
        "/repo/alpha",
        "alpha prompt inside the filtered project",
    );
    write_claude_session_fixture_with_cwd(
        &beta,
        Some("beta-session"),
        "/repo/beta",
        "beta prompt outside the filtered project",
    );

    let output = run_aicx(
        &home,
        &["claude", "-H", "24", "-p", "alpha", "--emit", "json"],
    );
    assert_success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session ingest summary: ingested=1 skipped=0 filtered_out=1"),
        "batch summary must expose the filtered_out counter\nstderr:\n{stderr}"
    );

    let projection_dir = home
        .join(".aicx")
        .join("store")
        .join("canonical-projection-v1");
    let manifest: Value = serde_json::from_str(
        &fs::read_to_string(projection_dir.join("manifest.json"))
            .expect("canonical projection manifest"),
    )
    .expect("valid canonical projection manifest");
    let card_ids = manifest["card_ids"].as_array().expect("manifest card ids");
    assert_eq!(
        card_ids.len(),
        1,
        "only the session matching the -p filter may project; got {card_ids:?}"
    );
    let card_name = format!(
        "{}.json",
        card_ids[0].as_str().expect("card id").replace(':', "_")
    );
    let card: Value = serde_json::from_str(
        &fs::read_to_string(projection_dir.join("cards").join(card_name)).expect("canonical card"),
    )
    .expect("valid canonical card");
    assert_eq!(card["session_id"].as_str(), Some("alpha-session"));

    // Sessions cut by the filter are neither ingested nor skipped: the store
    // must contain no trace of the beta session's content.
    let mut stack = vec![home.join(".aicx").join("store")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(content) = fs::read_to_string(&path) {
                assert!(
                    !content.contains("beta prompt outside the filtered project"),
                    "filtered-out session leaked into store file {}",
                    path.display()
                );
            }
        }
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn project_filter_cutting_everything_is_not_all_skipped() {
    // A `-p` filter that excludes every discovered session is an empty result,
    // not a failure: exit 0, filtered_out counted, nothing skipped.
    let root = unique_test_dir("claude-batch-filter-all-out");
    let home = root.join("home");
    let beta = home
        .join(".claude")
        .join("projects")
        .join("-repo-beta")
        .join("66666666-6666-4666-8666-666666666666.jsonl");
    write_claude_session_fixture_with_cwd(
        &beta,
        Some("beta-session"),
        "/repo/beta",
        "beta prompt outside the filtered project",
    );

    let output = run_aicx(&home, &["claude", "-H", "24", "-p", "alpha"]);
    assert_success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session ingest summary: ingested=0 skipped=0 filtered_out=1"),
        "all-filtered batch must not be reported as all-skipped\nstderr:\n{stderr}"
    );
    assert!(!stderr.contains("selected session(s) were skipped"));

    let _ = fs::remove_dir_all(&root);
}
