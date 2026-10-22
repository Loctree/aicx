#![cfg(feature = "app")]

//! Public CLI path over a synthetic Antigravity SQLite store.
//! Every subprocess gets a scratch HOME/AICX_HOME; no live source is read.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const CLI_ID: &str = "11111111-1111-4111-8111-111111111111";
const IDE_ID: &str = "22222222-2222-4222-8222-222222222222";
const FIRST_TOKEN: &str = "agypipelineinitial91dbea";
const IDE_TOKEN: &str = "agypipelineideroot47fdae";
const TOOL_TOKEN: &str = "agytoolonly893fed";
const DISPATCH: &str = "AGY_DISPATCH_NOT_INTENT";
static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn isolated_test_command(binary: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut command = Command::new(binary);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
}

struct AgyFixture {
    home: PathBuf,
}

impl AgyFixture {
    fn new() -> Self {
        let home = std::env::temp_dir().join(format!(
            "aicx-agy-pipeline-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        let cwd = home.join("work").join("agy-pipeline");
        fs::create_dir_all(&cwd).unwrap();
        assert!(
            isolated_test_command("git")
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .args(["init", "--quiet"])
                .arg(&cwd)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            isolated_test_command("git")
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .arg("-C")
                .arg(&cwd)
                .args([
                    "config",
                    "remote.origin.url",
                    "https://github.com/example/agy-pipeline.git",
                ])
                .status()
                .unwrap()
                .success()
        );
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/parser_engine/agy/build_db.py");
        let status = isolated_test_command("python3")
            .arg(script)
            .arg("emit")
            .arg("--home")
            .arg(&home)
            .arg("--cwd")
            .arg(cwd.display().to_string())
            .status()
            .unwrap();
        assert!(status.success(), "fixture sqlite emit failed");
        Self { home }
    }

    fn command(&self, binary: &str) -> Command {
        let mut command = isolated_test_command(binary);
        command
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("AICX_HOME", self.home.join(".aicx"))
            .env("AICX_ALLOW_TMP", "1")
            .env("AICX_NO_MUTATION_WARN", "1");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_aicx"))
            .args(args)
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert_success(&output);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "invalid JSON: {error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }
}

impl Drop for AgyFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn extract(fixture: &AgyFixture, name: &str, agent: &str, flags: &[&str]) -> String {
    let path = fixture.home.join(name);
    let mut args = vec![
        "extract",
        agent,
        "--session",
        CLI_ID,
        "--output",
        path.to_str().unwrap(),
    ];
    args.extend_from_slice(flags);
    assert_success(&fixture.run(&args));
    fs::read_to_string(path).unwrap()
}

fn search(fixture: &AgyFixture, token: &str, agent: &str) -> Value {
    fixture.json(&["search", token, "--json", "--hours", "0", "--agent", agent])
}

#[test]
fn agy_public_pipeline_discovers_both_stores() {
    let fixture = AgyFixture::new();
    let catalog = fixture.json(&["catalog", "rebuild", "--json"]);
    assert_eq!(catalog["agents"]["agy"], 2, "{catalog}");
    assert_eq!(catalog["total_sessions"], 2, "{catalog}");

    let resolved = fixture.json(&["catalog", "resolve", CLI_ID, "--json"]);
    assert_eq!(resolved["agent"], "agy");
    assert_eq!(resolved["session_id"], CLI_ID);
    assert!(
        resolved["source_path"]
            .as_str()
            .unwrap()
            .contains("/.gemini/antigravity-cli/conversations/")
    );
    let ide = fixture.json(&["catalog", "resolve", IDE_ID, "--json"]);
    assert!(
        ide["source_path"]
            .as_str()
            .unwrap()
            .contains("/.gemini/antigravity/conversations/")
    );

    for agent in ["agy", "antigravity", "antigravity-cli"] {
        let sessions = fixture.json(&["sessions", "list", "--all", "--agent", agent, "--json"]);
        let rows = sessions.as_array().expect("sessions list json is an array");
        assert_eq!(rows.len(), 2, "{agent}: {sessions}");
        assert!(rows.iter().all(|row| row["agent"] == "agy"));
        assert!(rows.iter().any(|row| row["session_id"] == CLI_ID));
        assert!(rows.iter().any(|row| row["session_id"] == IDE_ID));
    }

    let conversation = extract(&fixture, "conversation.md", "agy", &["--conversation"]);
    assert!(conversation.contains(FIRST_TOKEN), "{conversation}");
    assert!(!conversation.contains(TOOL_TOKEN), "{conversation}");
    assert!(!conversation.contains(DISPATCH), "{conversation}");
    let user = extract(
        &fixture,
        "user.md",
        "antigravity",
        &["--conversation", "--user-only"],
    );
    assert!(user.contains(FIRST_TOKEN), "{user}");
    assert!(!user.contains(TOOL_TOKEN), "{user}");
    assert!(!user.contains(DISPATCH), "{user}");
    assert!(
        !user.contains("AGY_BOOKKEEPING_FIELD30_DO_NOT_PROJECT"),
        "{user}"
    );

    let indexed = fixture.json(&["index", "--json", "--full-rescan", "--cache-extracts"]);
    assert!(
        indexed["sources_parsed"].as_u64().unwrap_or(0) >= 2,
        "{indexed}"
    );
    let found = search(&fixture, FIRST_TOKEN, "agy");
    assert!(
        found["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["session_id"] == CLI_ID),
        "{found}"
    );
    let alias = search(&fixture, IDE_TOKEN, "antigravity-cli");
    assert!(
        alias["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["session_id"] == IDE_ID),
        "{alias}"
    );
    let hidden = search(&fixture, TOOL_TOKEN, "agy");
    assert!(
        hidden["items"].as_array().unwrap().is_empty(),
        "tool output is not a search document: {hidden}"
    );

    let intents = fixture.json(&[
        "intents",
        "--hours",
        "0",
        "--agent",
        "agy",
        "--no-live",
        "--emit",
        "json",
    ]);
    let items = intents["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|item| item.to_string().contains(FIRST_TOKEN)),
        "{intents}"
    );
    assert!(
        items
            .iter()
            .any(|item| item.to_string().contains(IDE_TOKEN)),
        "{intents}"
    );
    assert!(items.iter().all(|item| item["agent"] == "agy"), "{intents}");
    assert!(
        items
            .iter()
            .all(|item| !item.to_string().contains(DISPATCH)),
        "{intents}"
    );
    assert!(
        items
            .iter()
            .all(|item| !item.to_string().contains(TOOL_TOKEN)),
        "{intents}"
    );

    // `extract all` writes a manifest (`--json`), not the timeline envelope
    // that `aicx all --emit json` prints. The human turn has to survive in
    // the file the manifest names.
    let extracted = fixture.json(&["extract", "all", "--provider", "agy", "--json", "--rebuild"]);
    assert_eq!(extracted["totals"]["failed"], 0, "{extracted}");
    assert!(
        extracted["totals"]["extracted"].as_u64().unwrap_or(0) >= 2,
        "{extracted}"
    );
    let rendered = extracted["entries"]
        .as_array()
        .expect("manifest entries")
        .iter()
        .filter(|entry| entry["outcome"] == "extracted")
        .map(|entry| {
            let path = entry["output_path"]
                .as_str()
                .expect("extracted row names its file");
            fs::read_to_string(path).unwrap_or_else(|error| panic!("read {path}: {error}"))
        })
        .collect::<Vec<_>>();
    assert!(
        rendered.iter().any(|body| body.contains(FIRST_TOKEN)),
        "{extracted}\n{rendered:?}"
    );
    assert!(
        rendered.iter().all(|body| !body.contains(DISPATCH)),
        "worker-contract text is not a bulk extract turn: {rendered:?}"
    );
}

#[test]
fn legacy_all_watermark_does_not_skip_agy() {
    let fixture = AgyFixture::new();
    let aicx_home = fixture.home.join(".aicx");
    fs::create_dir_all(&aicx_home).unwrap();
    let older_provider_key =
        "claude+codescribe+codex+copilot+cursor+gemini+grok+junie+kimi+omp:all";
    let old_watermark = chrono::DateTime::parse_from_rfc3339("2026-09-30T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut state = aicx::state::StateManager::default();
    state.update_watermark(older_provider_key, old_watermark);
    fs::write(
        aicx_home.join("state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();

    let ingested = fixture.json(&["all", "-H", "0", "--emit", "json"]);
    let entries = ingested["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|entry| entry["message"]
            .as_str()
            .is_some_and(|message| message.contains(FIRST_TOKEN))),
        "{ingested}"
    );
    let migrated: Value =
        serde_json::from_slice(&fs::read(aicx_home.join("state.json")).unwrap()).unwrap();
    assert_eq!(
        migrated["last_processed"]["agy+claude+codescribe+codex+copilot+cursor+gemini+grok+junie+kimi+omp:all"],
        "2026-09-30T00:00:00Z",
        "{migrated}"
    );
}
