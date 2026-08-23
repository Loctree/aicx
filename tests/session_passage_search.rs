// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const SESSION_ID: &str = "f842d85e-1111-4222-8333-444444444444";
const QUERY: &str = "vc-trust";

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-session-passages-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, content).expect("write fixture");
}

fn run_aicx(aicx_home: &Path, args: &[&str]) -> Output {
    fs::create_dir_all(aicx_home).expect("create scratch AICX_HOME");
    Command::new(env!("CARGO_BIN_EXE_aicx"))
        .args(args)
        .env("AICX_HOME", aicx_home)
        .env("AICX_ALLOW_TMP", "1")
        .output()
        .expect("run aicx")
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn parse_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn seed_session(aicx_home: &Path) -> PathBuf {
    let source = aicx_home
        .join("sources")
        .join(format!("{SESSION_ID}.jsonl"));
    let message = [
        "Operational release notes:",
        "bootstrap line one",
        "bootstrap line two",
        "prevc-trustpost is a boundary decoy",
        "background line four",
        "Decision: vc-trust is the sole release-trust owner.",
        "proof line six",
        "proof line seven",
        "separation line eight",
        "separation line nine",
        "separation line ten",
        "Second proof: vc-trust blocks unsigned provenance.",
        "closing line twelve",
    ]
    .join("\n");
    let rows = [
        json!({
            "timestamp": "2026-07-24T08:00:00Z",
            "type": "session_meta",
            "payload": {
                "id": SESSION_ID,
                "cwd": "/Volumes/vc-workspace/Loctree/aicx",
                "model": "gpt-test"
            }
        }),
        json!({
            "timestamp": "2026-07-24T08:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Recover the release trust decision."}]
            }
        }),
        json!({
            "timestamp": "2026-07-24T08:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": message}]
            }
        }),
    ];
    write_file(
        &source,
        &rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    );

    let (source_len, source_mtime_ns) =
        aicx::catalog::live_source_fingerprint(&source).expect("source fingerprint");
    let entry = aicx::catalog::CatalogEntry {
        schema: aicx::catalog::CATALOG_SCHEMA.to_string(),
        session_id: SESSION_ID.to_string(),
        agent: "codex".to_string(),
        project: Some("Loctree/aicx".to_string()),
        date: Some("2026-07-24".to_string()),
        cwd: Some("/Volumes/vc-workspace/Loctree/aicx".to_string()),
        source_path: source.display().to_string(),
        source_len: Some(source_len),
        source_mtime_ns: Some(source_mtime_ns),
        title: Some("vc-trust replay".to_string()),
        machine: Some("scratch".to_string()),
        logical_session_id: None,
    };
    write_file(
        &aicx::catalog::sessions_path_for(aicx_home),
        &format!("{}\n", serde_json::to_string(&entry).unwrap()),
    );
    source
}

#[test]
fn vc_trust_replay_searches_then_reads_every_passage_without_grep() {
    let root = unique_root("dogfood");
    let aicx_home = root.join(".aicx");
    let source = seed_session(&aicx_home);

    let index = run_aicx(
        &aicx_home,
        &["index", "--json", "--full-rescan", "--cache-extracts"],
    );
    assert_success(&index, "index scratch corpus");

    let search = run_aicx(&aicx_home, &["search", QUERY, "--json", "--limit", "5"]);
    assert_success(&search, "search scratch corpus");
    let search_payload = parse_json(&search);
    assert_eq!(search_payload["coverage"]["scanned_sessions"], 1);
    assert_eq!(search_payload["coverage"]["total_sessions"], 1);
    assert_eq!(search_payload["coverage"]["skipped"], json!({}));
    assert_eq!(search_payload["items"][0]["session_id"], SESSION_ID);
    assert!(
        String::from_utf8_lossy(&search.stderr).contains("scanned 1 of 1 sessions; skipped: none")
    );

    let passages = run_aicx(
        &aicx_home,
        &[
            "search",
            QUERY,
            "--session",
            "f842d85e",
            "--literal",
            "--json",
        ],
    );
    assert_success(&passages, "read matching passages");
    let payload = parse_json(&passages);
    assert_eq!(payload["session_id"], SESSION_ID);
    assert_eq!(payload["mode"], "literal");
    assert_eq!(payload["context"], 2);
    assert_eq!(payload["cache_hit"], true);
    assert_eq!(payload["coverage"]["scanned_sessions"], 1);
    assert_eq!(payload["coverage"]["total_sessions"], 1);
    let items = payload["passages"].as_array().expect("passages");
    assert_eq!(items.len(), 2, "every separated exact passage must return");
    assert_eq!(items[0]["passage"], 1);
    assert_eq!(items[1]["passage"], 2);
    assert_eq!(items[0]["source_path"], source.display().to_string());
    assert!(
        items
            .iter()
            .all(|item| item["line_span"]["end"].as_u64().unwrap()
                >= item["line_span"]["start"].as_u64().unwrap())
    );
    let passage_blob = payload.to_string();
    assert!(passage_blob.contains("Decision: vc-trust"));
    assert!(passage_blob.contains("Second proof: vc-trust"));
    assert!(
        !items.iter().any(|item| item["text"]
            .as_str()
            .unwrap()
            .lines()
            .all(|line| line.contains("prevc-trustpost"))),
        "boundary decoy must not become its own literal passage"
    );

    let lexical_only = run_aicx(&aicx_home, &["search", QUERY, "--json"]);
    assert_success(&lexical_only, "CURRENT lexical search");
    let lexical_payload = parse_json(&lexical_only);
    assert_eq!(
        lexical_payload["oracle_status"]["backend"],
        "lexical_tantivy"
    );
    assert!(!lexical_payload.to_string().contains("filesystem_fuzzy"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn passage_parse_on_read_and_missing_current_are_honest() {
    let root = unique_root("parse-on-read");
    let aicx_home = root.join(".aicx");
    let source = seed_session(&aicx_home);

    let passages = run_aicx(
        &aicx_home,
        &[
            "search",
            QUERY,
            "--session",
            SESSION_ID,
            "--literal",
            "--context",
            "0",
            "--json",
        ],
    );
    assert_success(&passages, "parse source on read");
    let payload = parse_json(&passages);
    assert_eq!(payload["cache_hit"], false);
    assert_eq!(payload["passages"].as_array().unwrap().len(), 2);
    assert_eq!(
        payload["passages"][0]["source_path"],
        source.display().to_string()
    );
    assert_eq!(
        payload["passages"][0]["line_span"]["start"],
        payload["passages"][0]["line_span"]["end"]
    );
    assert!(
        payload["passages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["text"].as_str().unwrap().contains("vc-trust")
                && !item["text"].as_str().unwrap().contains("prevc-trustpost")),
        "context-zero literal passages must exclude the identifier-boundary decoy"
    );

    let no_semantic = run_aicx(&aicx_home, &["search", QUERY, "--json"]);
    assert!(
        !no_semantic.status.success(),
        "missing CURRENT must fail instead of scanning legacy chunks"
    );
    let stderr = String::from_utf8_lossy(&no_semantic.stderr);
    assert!(stderr.contains("index_not_built"));
    assert!(stderr.contains("scanned 0 of 1 sessions; skipped: codex_unindexed=1"));
    assert!(!stderr.contains("scanned 0 chunks"));
    assert!(!stderr.contains("filesystem-fuzzy"));

    let missing_session = run_aicx(
        &aicx_home,
        &["search", QUERY, "--session", "does-not-exist", "--json"],
    );
    assert!(!missing_session.status.success());
    assert!(
        String::from_utf8_lossy(&missing_session.stderr)
            .contains("scanned 0 of 1 sessions; skipped: session_unreadable=1")
    );

    let _ = fs::remove_dir_all(root);
}
