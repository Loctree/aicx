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
//!    shape from `src/steer_index.rs`, plus a source-level invariant grep
//!    so the canonical helper stays wired in.
//! 4. rank — replicates the `fuzzy_search_store_one` split-and-delegate
//!    shape from `src/rank.rs`, plus a source-level invariant grep that
//!    the substring matcher is gone.

use std::fs;
use std::path::PathBuf;

use aicx::dashboard::project_matches_filter;
use aicx::store::project_filter_matches;

const LEAKY_FILTER: &str = "vista";
const LEAKY_CANDIDATE_ORG: &str = "vetcoders";
const LEAKY_CANDIDATE_REPO: &str = "vista-portal";
const LEAKY_CANDIDATE_SLUG: &str = "vetcoders/vista-portal";
const CANONICAL_TARGET_SLUG: &str = "vetcoders/vista";

fn split_slug(slug: &str) -> (&str, &str) {
    slug.split_once('/').unwrap_or(("", slug))
}

fn read_source(rel: &str) -> String {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = manifest_dir.join(rel);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
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
    // `metadata_matches` in `src/steer_index.rs` is crate-private. Replicate
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
    let src = read_source("src/steer_index.rs");
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
