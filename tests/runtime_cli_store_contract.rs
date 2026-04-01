use serde_json::{Value, json};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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

fn run_aicx(home: &Path, args: &[&str]) -> Output {
    run_aicx_with_env(home, args, &[])
}

fn run_aicx_with_env(home: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
    fs::create_dir_all(home).expect("create temp HOME");
    let mut command = Command::new(env!("CARGO_BIN_EXE_aicx"));
    command.args(args).env("HOME", home);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().expect("run aicx")
}

#[cfg(unix)]
fn write_fake_rmcp_memex(path: &Path) {
    write_file(
        path,
        r#"#!/bin/sh
if [ -n "${AICX_MEMEX_LOG_PATH:-}" ]; then
  echo "$*" >> "$AICX_MEMEX_LOG_PATH"
fi

if [ "$1" = "--version" ]; then
  echo "rmcp-memex test stub"
  exit 0
fi

if [ "$1" = "import" ]; then
  input=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      -i)
        shift
        input="$1"
        ;;
    esac
    shift
  done
  count=0
  if [ -n "$input" ] && [ -f "$input" ]; then
    count=$(wc -l < "$input" | tr -d '[:space:]')
  fi
  printf 'Imported: %s documents\n' "$count"
  printf 'Skipped: 0\n'
  printf 'Errors: 0\n'
  exit 0
fi

if [ "$1" = "upsert" ]; then
  exit 0
fi

echo "unsupported rmcp-memex invocation: $*" >&2
exit 1
"#,
    );

    let mut permissions = fs::metadata(path)
        .expect("stat fake rmcp-memex")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake rmcp-memex");
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

    assert_eq!(store_paths.len(), 2);
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

    assert_eq!(payload["total_entries"].as_u64(), Some(4));
    assert_eq!(payload["total_chunks"].as_u64(), Some(2));
    assert_eq!(store_paths.len(), 2);
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

#[cfg(unix)]
#[test]
fn store_cli_memex_updates_shared_sync_state() {
    let root = unique_test_dir("store-command-memex");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("VetCoders").join("ai-contexters");
    let history = home.join(".codex").join("history.jsonl");
    let bin_dir = root.join("bin");
    let stub_path = bin_dir.join("rmcp-memex");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    fs::create_dir_all(&bin_dir).expect("create fake bin dir");
    write_fake_rmcp_memex(&stub_path);
    write_codex_history(
        &history,
        "memex-store-sess",
        Some(&repo_root),
        &[
            ("user", now - 120, "Please sync this seam to memex."),
            ("assistant", now - 110, "Sending canonical chunks now."),
        ],
    );

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = run_aicx_with_env(
        &home,
        &[
            "store", "--agent", "codex", "-H", "24", "--memex", "--emit", "json",
        ],
        &[("PATH", &path_env)],
    );
    let payload = parse_stdout_json(&output);
    let store_paths = json_paths(&payload, "store_paths");
    assert_eq!(store_paths.len(), 1);

    let sync_state_path = home.join(".aicx").join("memex").join("sync_state.json");
    let sync_state: ai_contexters::memex::MemexSyncState =
        serde_json::from_str(&fs::read_to_string(&sync_state_path).expect("read memex sync state"))
            .expect("parse memex sync state");

    let chunk_id = store_paths[0]
        .file_stem()
        .and_then(|name| name.to_str())
        .expect("chunk file stem");
    assert_eq!(sync_state.total_pushes, 1);
    assert!(sync_state.last_synced.is_some());
    assert!(sync_state.synced_chunks.contains(chunk_id));
    assert!(sync_state.chunk_payload_hashes.contains_key(chunk_id));

    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn store_cli_memex_resyncs_changed_chunk_payloads() {
    let root = unique_test_dir("store-command-memex-resync");
    let home = root.join("home");
    let repo_root = home.join("hosted").join("VetCoders").join("ai-contexters");
    let history = home.join(".codex").join("history.jsonl");
    let bin_dir = root.join("bin");
    let stub_path = bin_dir.join("rmcp-memex");
    let memex_log = root.join("memex.log");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs() as i64;

    fs::create_dir_all(repo_root.join(".git")).expect("create repo root");
    fs::create_dir_all(&bin_dir).expect("create fake bin dir");
    write_fake_rmcp_memex(&stub_path);

    write_codex_history(
        &history,
        "memex-store-sess",
        Some(&repo_root),
        &[
            ("user", now - 120, "Please sync this seam to memex."),
            ("assistant", now - 110, "Sending canonical chunks now."),
        ],
    );

    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let log_env = memex_log.display().to_string();
    let envs = [
        ("PATH", path_env.as_str()),
        ("AICX_MEMEX_LOG_PATH", log_env.as_str()),
    ];

    let first = run_aicx_with_env(
        &home,
        &[
            "store", "--agent", "codex", "-H", "24", "--memex", "--emit", "json",
        ],
        &envs,
    );
    let first_payload = parse_stdout_json(&first);
    let store_paths = json_paths(&first_payload, "store_paths");
    assert_eq!(store_paths.len(), 1);

    write_codex_history(
        &history,
        "memex-store-sess",
        Some(&repo_root),
        &[
            ("user", now - 120, "Please sync this seam to memex."),
            (
                "assistant",
                now - 110,
                "Sending canonical chunks now, with updated steering metadata.",
            ),
        ],
    );

    let second = run_aicx_with_env(
        &home,
        &[
            "store", "--agent", "codex", "-H", "24", "--memex", "--emit", "json",
        ],
        &envs,
    );
    assert_success(&second);
    let second_stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        second_stderr.contains("Memex: 1 pushed, 0 skipped"),
        "expected changed chunk to re-sync\nstderr:\n{}",
        second_stderr
    );

    let sync_state_path = home.join(".aicx").join("memex").join("sync_state.json");
    let sync_state: ai_contexters::memex::MemexSyncState =
        serde_json::from_str(&fs::read_to_string(&sync_state_path).expect("read memex sync state"))
            .expect("parse memex sync state");

    let chunk_id = store_paths[0]
        .file_stem()
        .and_then(|name| name.to_str())
        .expect("chunk file stem");
    assert_eq!(sync_state.total_pushes, 2);
    assert!(sync_state.chunk_payload_hashes.contains_key(chunk_id));

    let log = fs::read_to_string(&memex_log).expect("read memex log");
    assert!(
        log.lines().any(|line| line.starts_with("import ")),
        "expected initial batch import\nlog:\n{}",
        log
    );
    assert!(
        log.lines().any(|line| line.starts_with("upsert ")),
        "expected changed chunk to use upsert\nlog:\n{}",
        log
    );

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
    assert_eq!(canonical_paths.len(), 1);
    assert_eq!(salvage_paths.len(), 3);
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

    let rebuilt_chunk =
        fs::read_to_string(&canonical_paths[0]).expect("read rebuilt canonical chunk");
    assert!(rebuilt_chunk.contains("Please inspect the migration seam."));
    assert!(rebuilt_chunk.contains("Reviewing the repo-centric store now."));
    assert!(!rebuilt_chunk.contains("input:"));

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
