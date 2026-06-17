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
    let mut lines = Vec::new();

    for (role, ts, text) in messages {
        let mut payload = serde_json::Map::new();
        payload.insert("session_id".to_string(), json!(session_id));
        payload.insert("text".to_string(), json!(text));
        payload.insert("ts".to_string(), json!(ts));
        payload.insert("role".to_string(), json!(role));
        if let Some(cwd) = cwd {
            payload.insert("cwd".to_string(), json!(cwd.display().to_string()));
        }
        lines.push(Value::Object(payload).to_string());
    }

    write_file(path, &lines.join("\n"));
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
    let mut payload = serde_json::Map::new();
    payload.insert("session_id".to_string(), json!(session_id));
    payload.insert("text".to_string(), json!(text));
    payload.insert("ts".to_string(), json!(ts));
    payload.insert("role".to_string(), json!(role));
    if let Some(cwd) = cwd {
        payload.insert("cwd".to_string(), json!(cwd.display().to_string()));
    }

    let mut existing = fs::read_to_string(path).unwrap_or_default();
    if !existing.is_empty() {
        existing.push('\n');
    }
    existing.push_str(&Value::Object(payload).to_string());
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
        .join("VetCoders")
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
    let history = home.join(".codex").join("history.jsonl");
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
    let history = codex_root.join("history.jsonl");
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
        &source_root.join("history.jsonl"),
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
    let history = source_root.join("history.jsonl");
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
            "--format",
            "codex",
            &input_arg,
            "-o",
            &output_arg,
            "--conversation",
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
    let repo_root = home.join("hosted").join("VetCoders").join("aicx");
    let history = home.join(".codex").join("history.jsonl");
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
    let repo_root = home.join("hosted").join("VetCoders").join("ai-contexters");
    let history = home.join(".codex").join("history.jsonl");
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
    write_file(
        &history,
        &format!(
            "{}\n{}",
            fs::read_to_string(&history).expect("read repo session history"),
            [
                json!({
                    "session_id": "nonrepo-sess",
                    "text": "Draft a migration plan before we know the repo.",
                    "ts": now - 100,
                    "role": "user"
                })
                .to_string(),
                json!({
                    "session_id": "nonrepo-sess",
                    "text": "Working without repository identity for now.",
                    "ts": now - 90,
                    "role": "assistant"
                })
                .to_string(),
            ]
            .join("\n")
        ),
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
        vec!["VetCoders/ai-contexters".to_string()]
    );
    assert_eq!(
        payload["includes_non_repository_contexts"].as_bool(),
        Some(true)
    );
    assert!(payload["resolved_store_buckets"]["VetCoders/ai-contexters"].is_object());
    assert!(payload["resolved_store_buckets"]["non-repository-contexts"].is_object());
    assert!(store_paths.iter().any(|path| {
        path.starts_with(
            home.join(".aicx")
                .join("store")
                .join("VetCoders")
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
    let repo_root = home.join("hosted").join("VetCoders").join("loctree");
    let history = home.join(".codex").join("history.jsonl");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    write_file(
        &history,
        &[
            json!({
                "session_id": "repo-store-sess",
                "text": "Please inspect the loctree runtime contract.",
                "ts": now - 120,
                "role": "user",
                "cwd": repo_root.display().to_string(),
            })
            .to_string(),
            json!({
                "session_id": "repo-store-sess",
                "text": "Reviewing canonical emission now.",
                "ts": now - 110,
                "role": "assistant",
                "cwd": repo_root.display().to_string(),
            })
            .to_string(),
            json!({
                "session_id": "unknown-store-sess",
                "text": "Planning first, repository unknown.",
                "ts": now - 100,
                "role": "user",
            })
            .to_string(),
            json!({
                "session_id": "unknown-store-sess",
                "text": "Still unresolved; keep this honest.",
                "ts": now - 90,
                "role": "assistant",
            })
            .to_string(),
        ]
        .join("\n"),
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
    assert_eq!(resolved_repositories, vec!["VetCoders/loctree".to_string()]);
    assert_eq!(
        payload["includes_non_repository_contexts"].as_bool(),
        Some(true)
    );
    assert!(payload["resolved_store_buckets"]["VetCoders/loctree"].is_object());
    assert!(payload["resolved_store_buckets"]["non-repository-contexts"].is_object());
    assert!(payload["repos"]["VetCoders/loctree"].is_object());
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
                .join("VetCoders")
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
    let repo_root = root.join("hosted").join("VetCoders").join("ai-contexters");
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
                .join("VetCoders")
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
    let repo_root = home.join("hosted").join("VetCoders").join("aicx");
    let history = home.join(".codex").join("history.jsonl");
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
    let repo_root = home.join("hosted").join("VetCoders").join("aicx");
    let history = home.join(".codex").join("history.jsonl");
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
    let repo_root = home.join("hosted").join("VetCoders").join("aicx");
    let history = home.join(".codex").join("history.jsonl");
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
    let repo_root = home.join("hosted").join("VetCoders").join("aicx");
    let history = home.join(".codex").join("history.jsonl");
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
    let repo_root = home.join("hosted").join("VetCoders").join("aicx");
    let history = home.join(".codex").join("history.jsonl");
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
    let history = home.join(".codex").join("history.jsonl");
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
    let history = home.join(".codex").join("history.jsonl");
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
