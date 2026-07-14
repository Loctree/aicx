// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! North Star parity: a human typing `aicx search <q>` and an agent calling
//! MCP `aicx_search` for the same query must get the same results.
//!
//! Both surfaces share one retrieval+render path. The MCP `aicx_search` fuzzy
//! fallback is literally:
//!   search_engine::fuzzy_search_with_post_filters
//!     -> search_engine::finalize_fuzzy_results
//!     -> rank::search_oracle_status + rank::render_search_json_with_oracle
//! (see `render_mcp_fuzzy_fallback_payload` in src/mcp.rs). The CLI fuzzy path
//! (`aicx search --no-semantic`) routes through the exact same functions.
//!
//! This test drives the real CLI binary as a subprocess and reproduces the MCP
//! render path in-process, then asserts the rendered `items` are byte-identical.
//! `aicx_search`'s `search` method is private to the crate, so the public
//! retrieval/render primitives are the faithful, stable stand-in for the MCP
//! surface here; an in-crate unit test additionally exercises the live tool.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-parity-{name}-{}-{}",
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

fn ensure_aicx_binary_exists() -> PathBuf {
    static BIN_PATH: OnceLock<PathBuf> = OnceLock::new();
    BIN_PATH
        .get_or_init(|| {
            if let Some(env_path) = std::env::var_os("CARGO_BIN_EXE_aicx").map(PathBuf::from)
                && env_path.is_file()
            {
                return env_path;
            }
            let mut fallback = current_profile_dir().join("aicx");
            if cfg!(windows) {
                fallback.set_extension("exe");
            }
            if fallback.is_file() {
                return fallback;
            }
            let cargo = std::env::var_os("CARGO")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("cargo"));
            let output = Command::new(&cargo)
                .args(["build", "--bin", "aicx"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .expect("build fallback aicx binary");
            assert!(
                output.status.success(),
                "fallback cargo build --bin aicx failed\nstderr:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            fallback
        })
        .clone()
}

/// Build a small canonical store fixture under `<home>/.aicx/store/...`.
fn build_store_fixture(aicx_home: &Path) {
    let base = aicx_home
        .join("store")
        .join("vetcoders")
        .join("aicx")
        .join("2026_0601")
        .join("reports")
        .join("codex");
    write_file(
        &base.join("2026_0601_codex_sess-p1_001.md"),
        "Decision: the remote backend is an optional accelerator for deployment, not a requirement.",
    );
    write_file(
        &base.join("2026_0601_codex_sess-p2_001.md"),
        "Note: local-first deployment search must work even when the backend is unreachable.",
    );
    write_file(
        &base.join("2026_0601_codex_sess-p3_001.md"),
        "Unrelated chunk about installer drift and binary pairs.",
    );
    write_file(
        &base.join("2026_0601_codex_sess-p4_001.md"),
        "Deployment runtime hosts the hybrid index; a local node mirrors a subset.",
    );
}

#[test]
fn cli_and_mcp_render_identical_search_items() {
    const QUERY: &str = "deployment";
    const LIMIT: usize = 10;

    let root = unique_test_dir("identical-items");
    let home = root.join("home");
    let aicx_home = home.join(".aicx");
    build_store_fixture(&aicx_home);

    // --- CLI surface: real binary, fuzzy path (--no-semantic), JSON output. ---
    let output = Command::new(ensure_aicx_binary_exists())
        .args([
            "search",
            QUERY,
            "--json",
            "--no-semantic",
            "--limit",
            &LIMIT.to_string(),
        ])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("AICX_ALLOW_TMP", "1")
        .env_remove("AICX_HOME")
        .output()
        .expect("run aicx search");
    assert!(
        output.status.success(),
        "cli search failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let cli_json: Value =
        serde_json::from_slice(&output.stdout).expect("cli search must emit valid JSON");
    let cli_items = cli_json
        .get("items")
        .cloned()
        .expect("cli payload has items");

    // --- MCP surface: the exact public retrieval+render path aicx_search uses
    //     for its fuzzy fallback, run in-process against the same store. ---
    let store_root = aicx_home.clone();
    let scopes: Vec<Option<&str>> = vec![None];
    let post_filters = aicx::search_engine::SemanticSearchFilters {
        agent: None,
        score_min: None,
        date_lo: None,
        date_hi: None,
        hours_cutoff: None,
    };
    let (results, scanned) = aicx::search_engine::fuzzy_search_with_post_filters(
        &store_root,
        QUERY,
        LIMIT,
        &scopes,
        None,
        &post_filters,
    )
    .expect("mcp-path fuzzy retrieval");
    let results = aicx::search_engine::finalize_fuzzy_results(results, None, None, LIMIT);
    let oracle_status = aicx::rank::search_oracle_status(&store_root, &results, scanned);
    let rendered =
        aicx::rank::render_search_json_with_oracle(&store_root, &results, scanned, oracle_status)
            .expect("mcp-path render");
    let mcp_json: Value =
        serde_json::from_str(&rendered).expect("mcp-path payload must be valid JSON");
    let mcp_items = mcp_json
        .get("items")
        .cloned()
        .expect("mcp payload has items");

    // North Star: same query, same store, same items on both surfaces.
    assert_eq!(
        cli_items, mcp_items,
        "CLI and MCP search must return identical items for the same query"
    );
    let item_count = cli_items.as_array().map(|a| a.len()).unwrap_or(0);
    assert!(
        item_count >= 3,
        "fixture has 3 chunks mentioning the query; got {item_count} items"
    );

    let _ = fs::remove_dir_all(&root);
}
