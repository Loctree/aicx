// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! Wave B close-out cross-cut: strict project filter surface guard.
//!
//! Bug #38 cut the last live substring project-filter call-site (the rank
//! fallback fuzzy path in `src/rank.rs`). With Wave B-1 (dashboard), B-2
//! (steer-index), and B-3 (rank) all routed through the canonical
//! `aicx::store::project_filter_matches`, every `-p <project>` surface in
//! the pipeline agrees: `vista` does NOT match `vista-portal`.
//!
//! This file is the surface-wide regression: it pins behavior across the
//! four canonical paths so a future refactor cannot silently re-introduce
//! `.to_lowercase().contains()` on any of them.
//!
//! Sub-cases:
//! 1. store path — `store::project_filter_matches` direct call.
//! 2. dashboard — `dashboard::project_matches_filter` public wrapper.
//! 3. steer-index — replicates the `metadata_matches` split-and-delegate
//!    shape from `src/steer_index/search.rs`, plus a source-level invariant grep
//!    so the canonical helper stays wired in.
//! 4. rank — replicates the `fuzzy_search_store_one` split-and-delegate
//!    shape from `src/rank.rs`, plus a source-level invariant grep that
//!    the substring matcher is gone.

use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aicx::api::Aicx;
use aicx::dashboard::project_matches_filter;
use aicx::intents::{IntentExtraction, IntentsConfig};
use aicx::store::{ProjectMatchMode, project_filter_matches, require_project_resolution};

const LEAKY_FILTER: &str = "vista";
const LEAKY_CANDIDATE_ORG: &str = "vetcoders";
const LEAKY_CANDIDATE_REPO: &str = "vista-portal";
const LEAKY_CANDIDATE_SLUG: &str = "vetcoders/vista-portal";
const CANONICAL_TARGET_SLUG: &str = "vetcoders/vista";
static NEXT_STORE_ID: AtomicU64 = AtomicU64::new(0);

fn split_slug(slug: &str) -> (&str, &str) {
    slug.split_once('/').unwrap_or(("", slug))
}

fn read_source(rel: &str) -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn unique_store_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-strict-filter-{label}-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos(),
        NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

fn write_intent_chunk(root: &std::path::Path, project: &str, marker: &str, sequence: u32) {
    let (organization, repository) = project
        .split_once('/')
        .expect("strict-filter fixture uses owner/repo slugs");
    let directory = root
        .join("store")
        .join(organization)
        .join(repository)
        .join("2026_0717")
        .join("conversations")
        .join("codex");
    fs::create_dir_all(&directory).expect("create strict-filter fixture directory");
    let filename = aicx::store::session_basename(
        "2026-07-17",
        "codex",
        &format!("strict-{sequence}"),
        sequence,
    );
    fs::write(
        directory.join(filename),
        format!(
            "[project: {project} | agent: codex | date: 2026-07-17 | frame_kind: user_msg]\n\n\
             [signals]\nIntent:\n- Preserve strict project identity marker {marker}\n[/signals]\n"
        ),
    )
    .expect("write strict-filter intent chunk");
}

fn write_ownerless_intent_chunk(
    root: &std::path::Path,
    repository: &str,
    marker: &str,
    sequence: u32,
) {
    let directory = root
        .join("store")
        .join(repository)
        .join("2026_0717")
        .join("conversations")
        .join("codex");
    fs::create_dir_all(&directory).expect("create ownerless fixture directory");
    let filename = aicx::store::session_basename(
        "2026-07-17",
        "codex",
        &format!("ownerless-{sequence}"),
        sequence,
    );
    fs::write(
        directory.join(filename),
        format!(
            "[project: {repository} | agent: codex | date: 2026-07-17 | frame_kind: user_msg]\n\n\
             [signals]\nIntent:\n- Preserve ownerless project identity marker {marker}\n[/signals]\n"
        ),
    )
    .expect("write ownerless intent chunk");
}

fn strict_filter_corpus() -> PathBuf {
    let root = unique_store_root("corpus");
    write_intent_chunk(&root, "LibraxisAI/vista", "TARGET", 1);
    write_intent_chunk(&root, "LibraxisAI/VistaScribe-dev", "SCRIBE", 2);
    write_intent_chunk(&root, "VetCoders/vista-portal", "PORTAL", 3);
    write_intent_chunk(&root, "Another/vista", "CROSS_ORG", 4);
    root
}

fn config(project: &str) -> IntentsConfig {
    IntentsConfig {
        project: project.to_string(),
        hours: 0,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    }
}

fn extract_via_api(root: &std::path::Path, project: &str) -> IntentExtraction {
    Aicx::with_store_root(root)
        .extract_intents(&config(project))
        .expect("extract strict-filter intents through public API")
}

fn payload_projects(payload: &Value) -> Vec<&str> {
    payload["items"]
        .as_array()
        .expect("intents envelope items")
        .iter()
        .map(|item| item["project"].as_str().expect("intent project"))
        .collect()
}

fn payload_session_ids(payload: &Value) -> Vec<&str> {
    payload["items"]
        .as_array()
        .expect("intents envelope items")
        .iter()
        .map(|item| item["session_id"].as_str().expect("intent session_id"))
        .collect()
}

fn render_mcp_api_payload(root: &std::path::Path, project: &str) -> Value {
    let extraction = extract_via_api(root, project);
    let oracle_status = aicx::oracle::OracleStatus::canonical_corpus_scan(
        root,
        extraction.stats.scanned_count,
        extraction.stats.candidate_count,
        extraction.stats.source_paths_verified,
    );
    let body = aicx::intents::format_intents_oracle_json(&extraction.records, oracle_status)
        .expect("serialize the payload used by MCP intents");
    serde_json::from_str(&body).expect("parse MCP intents envelope")
}

fn run_git(checkout: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(args)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn synthetic_checkout(root: &std::path::Path, remote: &str) -> PathBuf {
    let checkout = root.join("checkout");
    fs::create_dir_all(&checkout).expect("create synthetic checkout");
    run_git(&checkout, &["init", "--quiet"]);
    run_git(&checkout, &["remote", "add", "origin", remote]);
    checkout
}

fn set_remote(checkout: &std::path::Path, remote: &str) {
    run_git(checkout, &["remote", "set-url", "origin", remote]);
}

fn ingest_historical_session(
    root: &std::path::Path,
    checkout: &std::path::Path,
    session_id: &str,
    marker: &str,
) -> PathBuf {
    let home = root.join("home");
    let history = home
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("07")
        .join("17")
        .join(format!("rollout-2026-07-17T12-00-00-{session_id}.jsonl"));
    fs::create_dir_all(history.parent().expect("history parent"))
        .expect("create Codex history directory");
    let timestamp = "2026-07-17T12:00:00Z";
    let rows = [
        serde_json::json!({
            "timestamp": timestamp,
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": timestamp,
                "cwd": checkout,
                "model": "gpt-test"
            }
        }),
        serde_json::json!({
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {
                "type": "user_message",
                "message": format!("Let's preserve immutable historical identity marker {marker}")
            }
        }),
    ];
    fs::write(
        &history,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .expect("write Codex history fixture");

    let ingest = Command::new(env!("CARGO_BIN_EXE_aicx"))
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("AICX_ALLOW_TMP", "1")
        .env_remove("AICX_HOME")
        .args(["codex", "-H", "0", "--emit", "json"])
        .output()
        .expect("run real Codex ingest");
    assert!(
        ingest.status.success(),
        "Codex ingest failed: {}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    println!(
        "ingest session={session_id} status={} store={}",
        ingest.status,
        home.join(".aicx").display()
    );
    home.join(".aicx")
}

fn cli_intents(root: &std::path::Path, project: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_aicx"))
        .env("AICX_HOME", root)
        .env("AICX_ALLOW_TMP", "1")
        .args(["intents", "-p", project, "--emit", "json", "-H", "0"])
        .output()
        .expect("run CLI intents")
}

fn mcp_intents_response(root: &std::path::Path, project: &str) -> Value {
    mcp_intents_response_with_arguments(
        root,
        serde_json::json!({"project": project, "hours": 0, "emit": "json", "limit": 100}),
    )
}

fn mcp_intents_response_with_arguments(root: &std::path::Path, arguments: Value) -> Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aicx-mcp"))
        .env("AICX_HOME", root)
        .env("AICX_ALLOW_TMP", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aicx-mcp");
    let mut stdin = child.stdin.take().expect("take aicx-mcp stdin");
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "immutable-identity-test", "version": "1"}
        }
    });
    let initialized = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "aicx_intents",
            "arguments": arguments
        }
    });
    for request in [initialize, initialized, call] {
        writeln!(stdin, "{request}").expect("write MCP request");
    }
    drop(stdin);
    let output = child.wait_with_output().expect("wait for aicx-mcp");
    assert!(
        output.status.success(),
        "aicx-mcp failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("MCP stdout utf-8")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|response| response["id"] == 2)
        .expect("MCP tool response")
}

fn mcp_intents_payload(root: &std::path::Path, project: &str) -> Value {
    let response = mcp_intents_response(root, project);
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("MCP intents result missing text: {response}"));
    serde_json::from_str(text).expect("parse MCP intents envelope")
}

fn write_legacy_chunk_without_identity(root: &std::path::Path, project: &str) {
    let (organization, repository) = project.split_once('/').expect("legacy owner/repo");
    let directory = root
        .join("store")
        .join(organization)
        .join(repository)
        .join("2026_0717")
        .join("conversations")
        .join("codex");
    fs::create_dir_all(&directory).expect("create legacy corpus directory");
    fs::write(
        directory.join(aicx::store::session_basename(
            "2026-07-17",
            "codex",
            "legacy-no-identity",
            1,
        )),
        "[signals]\nIntent:\n- Preserve legacy fallback marker\n[/signals]\n",
    )
    .expect("write legacy chunk without persisted project");
}

fn write_f6_intent_chunk(
    root: &std::path::Path,
    project: &str,
    date: &str,
    session_id: &str,
    sequence: u32,
    user_line: &str,
) {
    let (organization, repository) = project.split_once('/').expect("F6 owner/repo project");
    let directory = root
        .join("store")
        .join(organization)
        .join(repository)
        .join(format!(
            "{}_{}",
            date.get(..4).expect("F6 date year"),
            date.get(5..).expect("F6 date month-day").replace('-', "")
        ))
        .join("conversations")
        .join("codex");
    fs::create_dir_all(&directory).expect("create F6 fixture directory");
    let filename = aicx::store::session_basename(date, "codex", session_id, sequence);
    fs::write(
        directory.join(filename),
        format!(
            "[project: {project} | agent: codex | date: {date} | frame_kind: user_msg]\n\n\
             [12:00:00] user: {user_line}\n"
        ),
    )
    .expect("write F6 intent chunk");
}

fn cli_intents_payload_with_options(
    root: &std::path::Path,
    projects: &[&str],
    limit: usize,
    collapse_session: bool,
) -> Value {
    let mut args = vec!["intents".to_string()];
    for project in projects {
        args.extend(["-p".to_string(), (*project).to_string()]);
    }
    args.extend([
        "--emit".to_string(),
        "json".to_string(),
        "-H".to_string(),
        "0".to_string(),
        "--sort".to_string(),
        "newest".to_string(),
        "--limit".to_string(),
        limit.to_string(),
    ]);
    if collapse_session {
        args.push("--collapse-session".to_string());
    }

    let output = Command::new(env!("CARGO_BIN_EXE_aicx"))
        .env("AICX_HOME", root)
        .env("AICX_ALLOW_TMP", "1")
        .args(args)
        .output()
        .expect("run F6 CLI intents");
    assert!(
        output.status.success(),
        "F6 CLI intents failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse F6 CLI intents envelope")
}

fn mcp_intents_payload_with_options(
    root: &std::path::Path,
    projects: &[&str],
    limit: usize,
    collapse_session: bool,
) -> Value {
    let response = mcp_intents_response_with_arguments(
        root,
        serde_json::json!({
            "projects": projects,
            "hours": 0,
            "emit": "json",
            "sort": "newest",
            "limit": limit,
            "collapse_session": collapse_session
        }),
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("F6 MCP intents result missing text: {response}"));
    serde_json::from_str(text).expect("parse F6 MCP intents envelope")
}

#[test]
fn store_path_rejects_substring_leak() {
    // Direct contract: `vista` is a bare cross-org repo-name token. Strict
    // semantics accept `vetcoders/vista` (org-or-repo equality) but reject
    // `vetcoders/vista-portal` (no substring fallback).
    assert!(
        !project_filter_matches(LEAKY_CANDIDATE_ORG, LEAKY_CANDIDATE_REPO, LEAKY_FILTER),
        "store: `-p vista` must NOT match `vetcoders/vista-portal`"
    );
    assert!(
        project_filter_matches("vetcoders", "vista", LEAKY_FILTER),
        "store: `-p vista` MUST match `vetcoders/vista` via cross-org repo-name rule"
    );
    assert!(
        project_filter_matches("vetcoders", "vista", CANONICAL_TARGET_SLUG),
        "store: exact `<owner>/<repo>` slug filter must match"
    );
    assert!(
        !project_filter_matches("vetcoders", "vista-portal", CANONICAL_TARGET_SLUG),
        "store: exact slug must not leak into substring sibling"
    );
}

#[test]
fn dashboard_path_rejects_substring_leak() {
    // Dashboard wraps the canonical helper via `project_matches_filter`.
    assert!(
        !project_matches_filter(LEAKY_CANDIDATE_SLUG, Some(LEAKY_FILTER)),
        "dashboard: `-p vista` must NOT match `vetcoders/vista-portal`"
    );
    assert!(
        project_matches_filter(CANONICAL_TARGET_SLUG, Some(LEAKY_FILTER)),
        "dashboard: `-p vista` MUST match `vetcoders/vista`"
    );
    assert!(
        project_matches_filter(CANONICAL_TARGET_SLUG, Some(CANONICAL_TARGET_SLUG)),
        "dashboard: exact slug filter must match"
    );
    // None / empty filter keeps the "no filter applied" identity.
    assert!(project_matches_filter("anything", None));
    assert!(project_matches_filter("anything", Some("")));
}

#[test]
fn steer_index_path_rejects_substring_leak() {
    // `metadata_matches` in `src/steer_index/search.rs` is crate-private. Replicate
    // its exact split-and-delegate shape against the canonical helper so the
    // contract this surface promises is locked in at the test boundary.
    let (organization, repository) = split_slug(LEAKY_CANDIDATE_SLUG);
    assert!(
        !project_filter_matches(organization, repository, LEAKY_FILTER),
        "steer: candidate `vetcoders/vista-portal` must NOT match `-p vista`"
    );

    let (organization, repository) = split_slug(CANONICAL_TARGET_SLUG);
    assert!(
        project_filter_matches(organization, repository, LEAKY_FILTER),
        "steer: candidate `vetcoders/vista` MUST match `-p vista`"
    );

    // Source-level invariant: the canonical helper is invoked from the
    // steer-index candidate filter, and the old `lowercase().contains`
    // sibling is gone. Guards against silent regression in B-2's file.
    let src = read_source("src/steer_index/search.rs");
    assert!(
        src.contains("crate::store::project_filter_matches"),
        "steer-index lost its routing to canonical `project_filter_matches`"
    );
    assert!(
        !src.contains("project_lower"),
        "steer-index resurrected the `project_lower` substring matcher"
    );
}

#[test]
fn rank_path_rejects_substring_leak() {
    // `fuzzy_search_store_one` in `src/rank.rs` keeps its filter helper
    // crate-private. Replicate the split-and-delegate shape from the new
    // (Bug #38) call-site against the canonical helper.
    let (organization, repository) = split_slug(LEAKY_CANDIDATE_SLUG);
    assert!(
        !project_filter_matches(organization, repository, LEAKY_FILTER),
        "rank: stored `vetcoders/vista-portal` must NOT match `-p vista`"
    );

    let (organization, repository) = split_slug(CANONICAL_TARGET_SLUG);
    assert!(
        project_filter_matches(organization, repository, LEAKY_FILTER),
        "rank: stored `vetcoders/vista` MUST match `-p vista`"
    );

    // Source-level invariant: the rank fallback fuzzy path routes through
    // `store::project_filter_matches` and the legacy lowercase-substring
    // sibling (`project_filter_lower` + `.contains(filter)`) is gone.
    let src = read_source("src/rank.rs");
    assert!(
        src.contains("store::project_filter_matches"),
        "rank lost its routing to canonical `project_filter_matches`"
    );
    assert!(
        !src.contains("project_filter_lower"),
        "rank resurrected the `project_filter_lower` substring matcher"
    );
}

#[test]
fn resolver_world_model_is_exact_fail_closed_and_explicitly_fuzzy() {
    let root = strict_filter_corpus();
    let corpus = aicx::store::project_identities_in_store_at(&root).expect("discover corpus");

    let ambiguous =
        require_project_resolution(&["vista".to_string()], &corpus, ProjectMatchMode::Exact)
            .expect_err("two exact bare identities must fail closed");
    assert_eq!(
        ambiguous.candidates(),
        ["Another/vista", "LibraxisAI/vista"]
    );

    let exact = require_project_resolution(
        &["LibraxisAI/vista".to_string()],
        &corpus,
        ProjectMatchMode::Exact,
    )
    .expect("owner/repo exact");
    assert_eq!(exact.selected, ["LibraxisAI/vista"]);

    let unique = require_project_resolution(
        &["vista-portal".to_string()],
        &corpus,
        ProjectMatchMode::Exact,
    )
    .expect("unique bare repo");
    assert_eq!(unique.selected, ["VetCoders/vista-portal"]);

    let fuzzy =
        require_project_resolution(&["vista".to_string()], &corpus, ProjectMatchMode::Fuzzy)
            .expect("explicit fuzzy family search");
    assert_eq!(
        fuzzy.selected,
        [
            "Another/vista",
            "LibraxisAI/VistaScribe-dev",
            "LibraxisAI/vista",
            "VetCoders/vista-portal",
        ]
    );

    fs::remove_dir_all(root).expect("remove strict-filter corpus");
}

#[test]
fn intents_cli_and_mcp_resolution_path_share_selected_session_set() {
    let root = strict_filter_corpus();
    let corpus = aicx::store::project_identities_in_store_at(&root).expect("discover corpus");
    let selected = require_project_resolution(
        &["LibraxisAI/vista".to_string()],
        &corpus,
        ProjectMatchMode::Exact,
    )
    .expect("shared exact resolution");

    let mcp = render_mcp_api_payload(&root, &selected.selected[0]);
    assert_eq!(payload_projects(&mcp), ["LibraxisAI/vista"]);

    let cli = Command::new(env!("CARGO_BIN_EXE_aicx"))
        .env("AICX_HOME", &root)
        .env("AICX_ALLOW_TMP", "1")
        .args([
            "intents",
            "-p",
            "LibraxisAI/vista",
            "--emit",
            "json",
            "-H",
            "0",
        ])
        .output()
        .expect("run CLI intents");
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli_payload: Value = serde_json::from_slice(&cli.stdout).expect("parse CLI envelope");
    assert_eq!(payload_projects(&cli_payload), payload_projects(&mcp));
    assert_eq!(payload_session_ids(&cli_payload), payload_session_ids(&mcp));

    let ambiguous_cli = Command::new(env!("CARGO_BIN_EXE_aicx"))
        .env("AICX_HOME", &root)
        .env("AICX_ALLOW_TMP", "1")
        .args(["intents", "-p", "vista", "--emit", "json", "-H", "0"])
        .output()
        .expect("run ambiguous CLI intents");
    assert!(!ambiguous_cli.status.success());
    let stderr = String::from_utf8_lossy(&ambiguous_cli.stderr);
    assert!(stderr.contains("Another/vista"), "{stderr}");
    assert!(stderr.contains("LibraxisAI/vista"), "{stderr}");
    assert!(!stderr.contains("vista-portal"), "{stderr}");

    let intents_source = read_source("src/intents.rs");
    assert!(intents_source.contains("store::project_filter_matches"));
    assert!(!intents_source.contains(".contains(&project.to_ascii_lowercase())"));
    let mcp_source = read_source("src/mcp.rs");
    assert!(mcp_source.contains("resolve_mcp_projects"));
    assert!(mcp_source.contains("project_resolution_mcp_error"));
    assert!(mcp_source.contains("extract_intents_with_stats_for_projects"));
    assert!(mcp_source.contains("format_intents_oracle_json"));
    let cli_source = read_source("src/main.rs");
    assert!(cli_source.contains("extract_intents_with_stats_for_projects"));
    assert!(cli_source.contains("format_intents_oracle_json"));

    fs::remove_dir_all(root).expect("remove strict-filter corpus");
}

#[test]
fn ownerless_bucket_world_model_has_cli_mcp_addressability_and_completeness() {
    let root = unique_store_root("ownerless-world-model");
    write_intent_chunk(&root, "A/repo", "OWNED", 1);
    write_ownerless_intent_chunk(&root, "repo", "ORPHANED", 2);

    let corpus = aicx::store::project_identities_in_store_at(&root).expect("discover corpus");
    assert_eq!(corpus, ["A/repo", "_/repo"]);

    let ambiguous_cli = cli_intents(&root, "repo");
    assert!(!ambiguous_cli.status.success());
    let stderr = String::from_utf8_lossy(&ambiguous_cli.stderr);
    assert!(stderr.contains("A/repo"), "{stderr}");
    assert!(stderr.contains("_/repo"), "{stderr}");

    let ambiguous_mcp = mcp_intents_response(&root, "repo");
    assert_eq!(
        ambiguous_mcp["error"]["data"]["candidates"],
        serde_json::json!(["A/repo", "_/repo"]),
        "{ambiguous_mcp}"
    );

    let ownerless_cli = cli_intents(&root, "_/repo");
    assert!(
        ownerless_cli.status.success(),
        "{}",
        String::from_utf8_lossy(&ownerless_cli.stderr)
    );
    let ownerless_cli_payload: Value =
        serde_json::from_slice(&ownerless_cli.stdout).expect("parse ownerless CLI payload");
    let ownerless_mcp_payload = mcp_intents_payload(&root, "_/repo");
    assert_eq!(payload_projects(&ownerless_cli_payload), ["_/repo"]);
    assert_eq!(
        payload_session_ids(&ownerless_cli_payload),
        payload_session_ids(&ownerless_mcp_payload)
    );
    assert_eq!(
        ownerless_cli_payload["completeness"]["orphaned_buckets"],
        serde_json::json!(["_/repo"])
    );
    assert_eq!(
        ownerless_mcp_payload["completeness"]["orphaned_buckets"],
        serde_json::json!(["_/repo"])
    );

    let wildcard_cli = cli_intents(&root, "/repo");
    assert!(
        wildcard_cli.status.success(),
        "{}",
        String::from_utf8_lossy(&wildcard_cli.stderr)
    );
    let wildcard_cli_payload: Value =
        serde_json::from_slice(&wildcard_cli.stdout).expect("parse wildcard CLI payload");
    let wildcard_mcp_payload = mcp_intents_payload(&root, "/repo");
    let mut wildcard_projects = payload_projects(&wildcard_cli_payload);
    wildcard_projects.sort_unstable();
    assert_eq!(wildcard_projects, ["A/repo", "_/repo"]);
    assert_eq!(
        payload_session_ids(&wildcard_cli_payload),
        payload_session_ids(&wildcard_mcp_payload)
    );
    assert_eq!(
        wildcard_cli_payload["completeness"]["orphaned_buckets"],
        serde_json::json!(["_/repo"])
    );
    assert_eq!(
        wildcard_cli_payload["completeness"]["scope"]["selected"],
        serde_json::json!(["A/repo", "_/repo"])
    );

    fs::remove_dir_all(root).expect("remove ownerless world-model corpus");
}

#[test]
fn historical_identity_survives_live_remote_rename_with_cli_mcp_parity() {
    let root = unique_store_root("immutable-rename");
    let checkout = synthetic_checkout(&root, "https://github.com/archive/old.git");
    let store = ingest_historical_session(&root, &checkout, "rename-session", "RENAME");

    set_remote(&checkout, "https://github.com/archive/new.git");

    let cli_old = cli_intents(&store, "archive/old");
    assert!(
        cli_old.status.success(),
        "persisted identity must remain queryable: {}",
        String::from_utf8_lossy(&cli_old.stderr)
    );
    let cli_old_payload: Value =
        serde_json::from_slice(&cli_old.stdout).expect("parse old CLI envelope");
    assert_eq!(payload_projects(&cli_old_payload), ["archive/old"]);
    assert!(!payload_session_ids(&cli_old_payload).is_empty());
    assert_eq!(
        cli_old_payload["completeness"]["identity_source"],
        "project-bucket-v1"
    );

    let mcp_old_payload = mcp_intents_payload(&store, "archive/old");
    assert_eq!(
        payload_session_ids(&cli_old_payload),
        payload_session_ids(&mcp_old_payload)
    );
    assert_eq!(
        mcp_old_payload["completeness"]["identity_source"],
        "project-bucket-v1"
    );

    let cli_new = cli_intents(&store, "archive/new");
    assert!(
        !cli_new.status.success(),
        "live remote must not rewrite historical identity: {}",
        String::from_utf8_lossy(&cli_new.stdout)
    );
    assert!(
        mcp_intents_response(&store, "archive/new")["error"].is_object(),
        "MCP must reject the same live-only identity as CLI"
    );
    println!(
        "rename-repo old_status={} old_sessions={:?} old_identity_source={} new_status={} new_contains_session=false",
        cli_old.status,
        payload_session_ids(&cli_old_payload),
        cli_old_payload["completeness"]["identity_source"],
        cli_new.status
    );

    fs::remove_dir_all(root).expect("remove immutable rename corpus");
}

#[test]
fn deprecated_checkout_does_not_capture_historical_sessions() {
    let root = unique_store_root("deprecated-checkout");
    let checkout = synthetic_checkout(&root, "https://github.com/vetcoders/screen_scribe.git");
    let store = ingest_historical_session(&root, &checkout, "screenscribe-history", "DEPRECATED");

    set_remote(
        &checkout,
        "https://github.com/vetcoders/screen_scribe_depr.git",
    );

    let historical = cli_intents(&store, "vetcoders/screen_scribe");
    assert!(
        historical.status.success(),
        "historical project must survive deprecated checkout: {}",
        String::from_utf8_lossy(&historical.stderr)
    );
    let historical_payload: Value =
        serde_json::from_slice(&historical.stdout).expect("parse historical CLI envelope");
    assert!(!payload_session_ids(&historical_payload).is_empty());
    let historical_mcp = mcp_intents_payload(&store, "vetcoders/screen_scribe");
    assert_eq!(
        payload_session_ids(&historical_payload),
        payload_session_ids(&historical_mcp)
    );

    let deprecated = cli_intents(&store, "vetcoders/screen_scribe_depr");
    assert!(
        !deprecated.status.success(),
        "current deprecated remote must not capture historical sessions"
    );
    assert!(
        mcp_intents_response(&store, "vetcoders/screen_scribe_depr")["error"].is_object(),
        "MCP must reject the same deprecated-only identity as CLI"
    );
    println!(
        "deprecated-checkout historical_status={} historical_sessions={:?} deprecated_status={} deprecated_contains_session=false",
        historical.status,
        payload_session_ids(&historical_payload),
        deprecated.status
    );

    fs::remove_dir_all(root).expect("remove deprecated checkout corpus");
}

#[test]
fn legacy_record_falls_back_to_path_heuristic_with_cli_mcp_parity() {
    let root = unique_store_root("legacy-path-heuristic");
    write_legacy_chunk_without_identity(&root, "archive/legacy");

    let cli = cli_intents(&root, "archive/legacy");
    assert!(
        cli.status.success(),
        "legacy path fallback must remain queryable: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli_payload: Value =
        serde_json::from_slice(&cli.stdout).expect("parse legacy CLI envelope");
    let mcp_payload = mcp_intents_payload(&root, "archive/legacy");

    assert_eq!(
        payload_session_ids(&cli_payload),
        payload_session_ids(&mcp_payload)
    );
    assert!(!payload_session_ids(&cli_payload).is_empty());
    assert_eq!(
        cli_payload["completeness"]["identity_source"],
        "path-heuristic"
    );
    assert_eq!(
        mcp_payload["completeness"]["identity_source"],
        "path-heuristic"
    );

    fs::remove_dir_all(root).expect("remove legacy fallback corpus");
}

#[test]
fn f6_newest_and_collapse_world_model_has_cli_mcp_parity() {
    let root = unique_store_root("f6-determinism");
    let projects = ["alpha/repo", "beta/repo"];

    // 120 sessions share the exact canonical timestamp. The newest-100 cut
    // therefore depends entirely on the total identity/chunk tie-break order.
    for index in 0..120 {
        let session_id = format!("tie-{index:03}");
        write_f6_intent_chunk(
            &root,
            projects[0],
            "2026-07-17",
            &session_id,
            1,
            &format!("Intent: Preserve deterministic tied session {session_id}"),
        );
    }

    // Same session id in two project identities must remain two collapsed
    // representatives even when their content is byte-for-byte identical.
    for project in projects {
        write_f6_intent_chunk(
            &root,
            project,
            "2026-07-17",
            "shared-id",
            1,
            "Intent: Preserve project-scoped shared session",
        );
    }

    // New technical task noise must not become the representative when the
    // same session contains an older substantive user intent.
    write_f6_intent_chunk(
        &root,
        projects[0],
        "2026-07-17",
        "quality-id",
        1,
        "Task: Set model to Opus 4.8",
    );
    write_f6_intent_chunk(
        &root,
        projects[0],
        "2026-07-16",
        "quality-id",
        2,
        "Intent: Make newest-N deterministic across the shared CLI and MCP collector",
    );

    let cli_first = cli_intents_payload_with_options(&root, &[projects[0]], 100, false);
    let cli_second = cli_intents_payload_with_options(&root, &[projects[0]], 100, false);
    let mcp_first = mcp_intents_payload_with_options(&root, &[projects[0]], 100, false);
    let mcp_second = mcp_intents_payload_with_options(&root, &[projects[0]], 100, false);
    let cli_ids = payload_session_ids(&cli_first);

    assert_eq!(cli_ids.len(), 100);
    assert_eq!(cli_ids, payload_session_ids(&cli_second));
    assert_eq!(cli_ids, payload_session_ids(&mcp_first));
    assert_eq!(cli_ids, payload_session_ids(&mcp_second));
    assert_eq!(cli_first["items"], cli_second["items"]);
    assert_eq!(cli_first["items"], mcp_first["items"]);
    assert_eq!(mcp_first["items"], mcp_second["items"]);
    println!("F6_NEWEST_IDS={}", cli_ids.join(","));

    let collapsed_cli = cli_intents_payload_with_options(&root, &projects, 500, true);
    let collapsed_mcp = mcp_intents_payload_with_options(&root, &projects, 500, true);
    assert_eq!(collapsed_cli["items"], collapsed_mcp["items"]);

    let collapsed_items = collapsed_cli["items"]
        .as_array()
        .expect("collapsed F6 items array");
    let shared = collapsed_items
        .iter()
        .filter(|item| item["session_id"] == "shared-id")
        .collect::<Vec<_>>();
    assert_eq!(
        shared.len(),
        2,
        "same session id across projects was merged"
    );
    assert_eq!(
        shared
            .iter()
            .map(|item| item["project"].as_str().expect("shared project"))
            .collect::<Vec<_>>(),
        projects
    );

    let quality = collapsed_items
        .iter()
        .find(|item| item["session_id"] == "quality-id")
        .expect("quality session representative");
    assert_eq!(quality["kind"], "intent");
    assert_eq!(
        quality["summary"],
        "Make newest-N deterministic across the shared CLI and MCP collector"
    );
    assert!(
        !quality["summary"]
            .as_str()
            .expect("quality summary")
            .contains("Set model")
    );

    fs::remove_dir_all(root).expect("remove F6 determinism corpus");
}
