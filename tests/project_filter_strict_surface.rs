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
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aicx::api::Aicx;
use aicx::dashboard::project_matches_filter;
use aicx::intents::{IntentExtraction, IntentsConfig};
use aicx::store::project_filter_matches;

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
fn intents_collector_is_strict_and_preserves_explicit_wildcards() {
    let root = strict_filter_corpus();

    let exact = extract_via_api(&root, "LibraxisAI/vista");
    assert_eq!(
        exact
            .records
            .iter()
            .map(|record| record.project.as_str())
            .collect::<Vec<_>>(),
        ["LibraxisAI/vista"],
        "exact slug must exclude VistaScribe-dev and vista-portal"
    );

    let bare = extract_via_api(&root, "vista");
    let bare_projects = bare
        .records
        .iter()
        .map(|record| record.project.as_str())
        .collect::<Vec<_>>();
    assert!(bare_projects.contains(&"LibraxisAI/vista"));
    assert!(bare_projects.contains(&"Another/vista"));
    assert!(!bare_projects.contains(&"VetCoders/vista-portal"));

    let owner = extract_via_api(&root, "LibraxisAI/");
    let owner_projects = owner
        .records
        .iter()
        .map(|record| record.project.as_str())
        .collect::<Vec<_>>();
    assert!(owner_projects.contains(&"LibraxisAI/vista"));
    assert!(owner_projects.contains(&"LibraxisAI/VistaScribe-dev"));
    assert!(!owner_projects.contains(&"VetCoders/vista-portal"));

    let repo = extract_via_api(&root, "/vista");
    let repo_projects = repo
        .records
        .iter()
        .map(|record| record.project.as_str())
        .collect::<Vec<_>>();
    assert!(repo_projects.contains(&"LibraxisAI/vista"));
    assert!(repo_projects.contains(&"Another/vista"));
    assert!(!repo_projects.contains(&"VetCoders/vista-portal"));

    fs::remove_dir_all(root).expect("remove strict-filter corpus");
}

#[test]
fn intents_cli_and_mcp_api_path_share_strict_collector_semantics() {
    let root = strict_filter_corpus();

    // The CLI calls the same collector + envelope formatter in `run_intents`.
    // Exercise those production functions directly here; the delivery smoke
    // launches the real binary against the same three-bucket corpus.
    let cli = render_mcp_api_payload(&root, "LibraxisAI/vista");
    assert_eq!(payload_projects(&cli), ["LibraxisAI/vista"]);

    let mcp = render_mcp_api_payload(&root, "vista");
    let mcp_projects = payload_projects(&mcp);
    assert!(mcp_projects.contains(&"LibraxisAI/vista"));
    assert!(mcp_projects.contains(&"Another/vista"));
    assert!(!mcp_projects.contains(&"LibraxisAI/VistaScribe-dev"));
    assert!(!mcp_projects.contains(&"VetCoders/vista-portal"));

    let intents_source = read_source("src/intents.rs");
    assert!(intents_source.contains("store::project_filter_matches"));
    assert!(!intents_source.contains(".contains(&project.to_ascii_lowercase())"));
    let mcp_source = read_source("src/mcp.rs");
    assert!(mcp_source.contains("extract_intents_with_stats_for_projects"));
    assert!(mcp_source.contains("format_intents_oracle_json"));
    let cli_source = read_source("src/main.rs");
    assert!(cli_source.contains("extract_intents_with_stats_for_projects"));
    assert!(cli_source.contains("format_intents_oracle_json"));

    fs::remove_dir_all(root).expect("remove strict-filter corpus");
}
