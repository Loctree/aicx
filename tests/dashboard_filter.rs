// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! Regression tests for Wave B-1 (bugs #27, #28, #30):
//!
//! - #27: dashboard per-event project filter is strict (no substring leak).
//! - #28: covered inline in `src/dashboard_server.rs` because the rollup is
//!   crate-private; this file asserts the public dashboard filter surface
//!   behaves strictly.
//! - #30: the rust-memex CLI fork (`run_memex_cli` / `run_memex_cross_search`)
//!   is gone from `src/dashboard_server.rs`; assert by greppable absence
//!   so the dead surface cannot creep back into the dashboard process.

use std::fs;
use std::path::PathBuf;

use aicx::dashboard::project_matches_filter;

#[test]
fn project_filter_does_not_match_substring_leak() {
    // Bug #27: a startup scope / request filter of `vista` MUST NOT match
    // `vetcoders/vista-portal`. The dashboard layer used to route this
    // through `lowercase().contains()` and silently merged the two
    // projects together.
    assert!(
        !project_matches_filter("vetcoders/vista-portal", Some("vista")),
        "strict filter must reject `vista` against canonical project `vetcoders/vista-portal`"
    );
    assert!(
        !project_matches_filter("vista-portal", Some("vista")),
        "strict filter must reject `vista` against canonical bucket `vista-portal`"
    );

    // Positive control: the canonical `vetcoders/vista` slug MUST match
    // `-p vista` via the cross-org repo-name rule that lives in
    // `aicx::store::project_filter_matches`. The dashboard surface now
    // agrees with store / mcp / rank / steer on this.
    assert!(
        project_matches_filter("vetcoders/vista", Some("vista")),
        "strict filter must accept `vista` against canonical project `vetcoders/vista` (cross-org repo-name match)"
    );
    assert!(
        project_matches_filter("vetcoders/vista", Some("vetcoders/vista")),
        "strict filter must accept exact `<owner>/<repo>` slug"
    );

    // Empty / None filter keeps the "no filter applied" behavior.
    assert!(project_matches_filter("anything", None));
    assert!(project_matches_filter("anything", Some("")));
    assert!(project_matches_filter("anything", Some("   ")));
}

#[test]
fn dashboard_server_source_has_no_memex_cli_fork() {
    // Bug #30: the dashboard process no longer shells out to a rust-memex
    // sub-process. `run_memex_cli` and `run_memex_cross_search` are
    // removed; this test guards the absence at the source level so a
    // future refactor can't silently re-introduce the fork.
    //
    // CARGO_MANIFEST_DIR points at the workspace member root (the aicx
    // crate root). Tests live in `<root>/tests/` and the source we're
    // checking is `<root>/src/dashboard_server.rs`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source_path = manifest_dir.join("src/dashboard_server.rs");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", source_path.display()));

    for needle in [
        "fn run_memex_cli",
        "fn run_memex_cross_search",
        "apply_memex_cli_env",
    ] {
        assert!(
            !source.contains(needle),
            "rust-memex CLI fork resurfaced in src/dashboard_server.rs: matched {needle:?}"
        );
    }
}
