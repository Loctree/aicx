//! End-to-end smoke for immutable loctree context corpus packs.
//!
//! Opt-in via `--features e2e-aicx`; unlike the semantic e2e, this fixture is
//! hermetic and only exercises filesystem/CLI contracts.

#![cfg(feature = "e2e-aicx")]

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-context-pack-{name}-{}-{}",
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
            fallback
        })
        .clone()
}

fn run_aicx(home: &Path, args: &[&str]) -> Output {
    fs::create_dir_all(home).expect("create temp HOME");
    Command::new(ensure_aicx_binary_exists())
        .args(args)
        .env("HOME", home)
        // Drop any operator-pinned AICX_HOME so the spawned binary
        // resolves under the test's temp HOME — see frame_kind_contract.rs
        // for the full reasoning.
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

#[test]
fn context_pack_ingest_retains_immutable_pack_and_live_reads_skip_examples() {
    let root = unique_test_dir("ingest");
    let home = root.join("home");
    let pack = root.join("batch-alpha");

    write_file(
        &pack.join("raw").join("ctx-example.md"),
        "[project: VetCoders/aicx | agent: loct-context-pack | date: 2026-05-08]\n\n[signals]\nDecision:\n- [decision] Frozen prism example must not become live truth\n[/signals]\n",
    );
    write_file(
        &pack.join("sidecars").join("ctx-example.json"),
        &json!({
            "id": "ctx-example",
            "project": "VetCoders/aicx",
            "agent": "loct-context-pack",
            "date": "2026-05-08",
            "session_id": "batch-alpha",
            "kind": "reports",
            "artifact_family": "loct-context-pack",
            "schema_version": "context_corpus.v1",
            "truth_status": {
                "role": "example",
                "runtime_authoritative": false,
                "stale_against_current_head": true,
                "current_head_when_ingested": "269d13c"
            },
            "learning_use": {
                "allowed": ["retrieval-test"],
                "forbidden": ["live-truth"]
            },
            "keywords": ["prism", "context-corpus"]
        })
        .to_string(),
    );

    let pack_arg = pack.display().to_string();
    let first = parse_stdout_json(&run_aicx(
        &home,
        &[
            "ingest",
            "--source",
            "loct-context-pack",
            &pack_arg,
            "--emit",
            "json",
        ],
    ));
    assert_eq!(first["raw_written"].as_u64(), Some(1));
    assert_eq!(first["deduped_chunks"].as_u64(), Some(0));

    let target = home
        .join(".aicx")
        .join("context-corpus")
        .join("vetcoders")
        .join("aicx")
        .join("2026_0508")
        .join("loct-context-pack")
        .join("batch-alpha");
    assert!(target.join("raw").join("ctx-example.md").exists());
    assert!(target.join("sidecars").join("ctx-example.json").exists());
    assert!(target.join("index.jsonl").exists());

    let second = parse_stdout_json(&run_aicx(
        &home,
        &[
            "ingest",
            "--source",
            "loct-context-pack",
            &pack_arg,
            "--emit",
            "json",
        ],
    ));
    assert_eq!(second["raw_written"].as_u64(), Some(0));
    assert_eq!(second["deduped_chunks"].as_u64(), Some(1));

    let live_dir = home
        .join(".aicx")
        .join("store")
        .join("vetcoders")
        .join("aicx")
        .join("2026_0508")
        .join("reports")
        .join("codex");
    let live_chunk = live_dir.join("2026_0508_codex_live-sess_001.md");
    write_file(
        &live_chunk,
        "[project: vetcoders/aicx | agent: codex | date: 2026-05-08]\n\n[signals]\nDecision:\n- [decision] Live truth survives intent scan\n[/signals]\n",
    );
    write_file(
        &live_chunk.with_extension("meta.json"),
        &json!({
            "id": "live-sess",
            "project": "vetcoders/aicx",
            "agent": "codex",
            "date": "2026-05-08",
            "session_id": "live-sess",
            "kind": "reports"
        })
        .to_string(),
    );
    let example_chunk = live_dir.join("2026_0508_codex_example-sess_001.md");
    write_file(
        &example_chunk,
        "[project: vetcoders/aicx | agent: codex | date: 2026-05-08]\n\n[signals]\nDecision:\n- [decision] Frozen example must be filtered\n[/signals]\n",
    );
    write_file(
        &example_chunk.with_extension("meta.json"),
        &json!({
            "id": "example-sess",
            "project": "vetcoders/aicx",
            "agent": "codex",
            "date": "2026-05-08",
            "session_id": "example-sess",
            "kind": "reports",
            "artifact_family": "loct-context-pack",
            "truth_status": { "role": "example", "runtime_authoritative": false, "stale_against_current_head": true }
        })
        .to_string(),
    );

    let intents = run_aicx(&home, &["intents", "-p", "aicx", "--emit", "json"]);
    assert_success(&intents);
    let stdout = String::from_utf8_lossy(&intents.stdout);
    assert!(stdout.contains("Live truth survives intent scan"));
    assert!(!stdout.contains("Frozen example must be filtered"));

    let doctor_output = run_aicx(&home, &["doctor", "--check-dedup", "--format", "json"]);
    let doctor: Value = serde_json::from_slice(&doctor_output.stdout).expect("parse doctor json");
    assert_eq!(doctor["content_dedup"]["severity"].as_str(), Some("green"));

    let live_index = aicx::vector_index::index_path(Some("vetcoders/aicx")).expect("live index");
    let corpus_index =
        aicx::vector_index::context_corpus_index_path(Some("vetcoders/aicx")).expect("ctx index");
    assert_ne!(live_index, corpus_index);
    assert_eq!(
        live_index.file_name().and_then(|n| n.to_str()),
        Some("embeddings.ndjson")
    );
    assert_eq!(
        corpus_index.file_name().and_then(|n| n.to_str()),
        Some("context-corpus.embeddings.ndjson")
    );

    let _ = fs::remove_dir_all(&root);
}
