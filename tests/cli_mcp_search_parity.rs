// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! North Star parity: a human typing `aicx search <q>` and an agent calling
//! MCP `aicx_search` for the same query must get the same results.
//!
//! Both surfaces share the Tantivy-only retrieval and render path.
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

/// Build a small catalog/source fixture that can publish lexical CURRENT.
fn build_store_fixture(aicx_home: &Path) {
    const SESSION_ID: &str = "parity-1111-4222-8333-444444444444";
    let source = aicx_home
        .join("sources")
        .join(format!("{SESSION_ID}.jsonl"));
    let rows = [
        serde_json::json!({
            "timestamp": "2026-06-01T08:00:00Z",
            "type": "session_meta",
            "payload": {"id": SESSION_ID, "cwd": "/Volumes/vc-workspace/Loctree/aicx"}
        }),
        serde_json::json!({
            "timestamp": "2026-06-01T08:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Decision: deployment uses a local Tantivy CURRENT. Deployment stays reader-only over HTTP. Deployment does not require an embedder."}]
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
        date: Some("2026-06-01".to_string()),
        cwd: Some("/Volumes/vc-workspace/Loctree/aicx".to_string()),
        source_path: source.display().to_string(),
        source_len: Some(source_len),
        source_mtime_ns: Some(source_mtime_ns),
        title: Some("deployment parity".to_string()),
        machine: Some("scratch".to_string()),
        logical_session_id: None,
    };
    write_file(
        &aicx::catalog::sessions_path_for(aicx_home),
        &format!("{}\n", serde_json::to_string(&entry).unwrap()),
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

    let binary = ensure_aicx_binary_exists();
    let index = Command::new(&binary)
        .args(["index", "--json", "--full-rescan", "--cache-extracts"])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("AICX_HOME", &aicx_home)
        .env("AICX_ALLOW_TMP", "1")
        .env("AICX_EMBEDDER_PATH", root.join("poison-missing.gguf"))
        .output()
        .expect("publish lexical CURRENT");
    assert!(
        index.status.success(),
        "index failed: {}",
        String::from_utf8_lossy(&index.stderr)
    );

    // --- CLI surface: real binary over published lexical CURRENT. ---
    let output = Command::new(&binary)
        .args(["search", QUERY, "--json", "--limit", &LIMIT.to_string()])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("AICX_HOME", &aicx_home)
        .env("AICX_ALLOW_TMP", "1")
        .env("AICX_EMBEDDER_PATH", root.join("poison-missing.gguf"))
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

    // --- MCP surface: the exact lexical retrieval/render path used by
    //     aicx_search, run in-process against the same CURRENT. ---
    let aicx_home = aicx_home.clone();
    let scopes: Vec<Option<&str>> = vec![None];
    let post_filters = aicx::search_engine::SemanticSearchFilters {
        agent: None,
        score_min: None,
        date_lo: None,
        date_hi: None,
        hours_cutoff: None,
        // Poison the retired selectors: the public lexical entrypoint must
        // clear both before dispatch and still match the CLI.
        legacy_dense: true,
        deep: true,
    };
    let filtered = aicx::search_engine::try_lexical_search_filtered(
        &aicx_home,
        QUERY,
        LIMIT,
        &scopes,
        None,
        None,
        &post_filters,
    )
    .expect("mcp-path lexical retrieval");
    let scanned = filtered.outcome.scanned;
    let retrieval_status = filtered.outcome.retrieval_status.clone();
    let backend_label = filtered.outcome.backend_label;
    let results =
        aicx::search_engine::finalize_fuzzy_results(filtered.outcome.results, None, None, LIMIT);
    let retrieval = aicx::search_engine::semantic_retrieval_outcome(
        backend_label,
        retrieval_status.as_ref(),
        scanned,
        results.len(),
        false,
    );
    let oracle_status = aicx::search_engine::search_oracle_status_from_retrieval(
        &aicx_home,
        &retrieval,
        retrieval_status.as_ref(),
        results.len(),
        true,
    );
    let rendered =
        aicx::rank::render_search_json_with_oracle(&aicx_home, &results, scanned, oracle_status)
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
        item_count >= 1,
        "fixture has a lexical deployment hit; got {item_count} items"
    );

    let _ = fs::remove_dir_all(&root);
}

/// W1-01 falsification fixture: a dense-only semantic outcome (hits exist,
/// hybrid manifest absent → `retrieval_status: None`) must NOT serialize as a
/// healthy `content_semantic` oracle status without a fallback_reason. This is
/// the known JSON false-green: the CLI text surface says `[degraded]` while
/// the CLI/MCP JSON surfaces claim a healthy semantic backend.
#[test]
fn dense_only_semantic_json_is_not_false_green() {
    let outcome = aicx::search_engine::SemanticSearchOutcome {
        results: Vec::new(),
        scanned: 227_290,
        backend_label: "semantic_dense_only",
        model_id: "qwen3-embedding-8b".to_string(),
        retrieval_status: None,
    };

    // Status construction through the single shared owner used by the CLI
    // JSON tail (src/main.rs) and the MCP success path (src/mcp.rs).
    let aicx_home = Path::new("/tmp/aicx");
    let retrieval = outcome.retrieval_outcome(5, false);
    let oracle_status = aicx::search_engine::search_oracle_status_from_retrieval(
        aicx_home,
        &retrieval,
        outcome.retrieval_status.as_ref(),
        5,
        true,
    );

    let json = serde_json::to_value(&oracle_status).expect("serialize oracle status");
    assert_ne!(
        json["backend"],
        Value::String("content_semantic".to_string()),
        "dense-only execution must not claim the healthy content_semantic backend: {json}"
    );
    assert!(
        json.get("fallback_reason").is_some_and(|r| !r.is_null()),
        "dense-only execution is degraded and must carry a fallback_reason: {json}"
    );
    assert_eq!(
        json["backend"],
        Value::String("semantic_dense_only".to_string())
    );
    assert_eq!(json["retrieval"]["completeness"], "degraded");
    assert_eq!(json["retrieval"]["executed_path"], "dense_only");
    assert_eq!(json["retrieval"]["requested_mode"], "hybrid");
}

/// W1-01 acceptance: the six retrieval scenarios serialize distinctly, and
/// identically across CLI and MCP — both surfaces construct `oracle_status`
/// through the same single owner (`search_oracle_status_from_retrieval`), so
/// one typed value yields one serialization everywhere.
#[test]
fn retrieval_outcome_scenarios_serialize_distinctly_across_surfaces() {
    use aicx::search_engine::{
        HybridRetrievalStatus, lexical_retrieval_outcome, semantic_retrieval_outcome,
    };

    let aicx_home = Path::new("/tmp/aicx");
    let hybrid_status = HybridRetrievalStatus {
        generation_id: "g-parity".to_string(),
        source_chunk_count: 123,
        dense_count: 123,
        lexical_doc_count: 122,
        fusion_algorithm: "rrf".to_string(),
        dense_kind: aicx_retrieve::MMAP_DENSE_KIND.to_string(),
    };

    let scenarios: Vec<(
        &str,
        aicx_retrieve::RetrievalOutcome,
        Option<&HybridRetrievalStatus>,
    )> = vec![
        (
            "dense_only",
            semantic_retrieval_outcome("semantic_dense_only", None, 1000, 5, false),
            None,
        ),
        (
            "legacy_dense",
            semantic_retrieval_outcome("semantic_legacy_dense", None, 1000, 5, false),
            None,
        ),
        (
            "lexical_fallback",
            lexical_retrieval_outcome(
                Some("embedder_unavailable: model not hydrated".to_string()),
                40,
                3,
                false,
            ),
            None,
        ),
        (
            "hybrid",
            semantic_retrieval_outcome("hybrid_rrf", Some(&hybrid_status), 123, 7, false),
            Some(&hybrid_status),
        ),
        (
            "partial_filter",
            semantic_retrieval_outcome("hybrid_rrf", Some(&hybrid_status), 123, 1, false)
                .mark_partial(),
            Some(&hybrid_status),
        ),
        (
            "empty_complete",
            semantic_retrieval_outcome("hybrid_rrf", Some(&hybrid_status), 123, 0, false),
            Some(&hybrid_status),
        ),
        (
            "empty_unknown",
            semantic_retrieval_outcome("embedded_semantic", None, 0, 0, false),
            None,
        ),
    ];

    let mut rendered: Vec<(&str, String)> = Vec::new();
    for (name, retrieval, hybrid) in &scenarios {
        // CLI and MCP both call this exact function; invoking it twice stands
        // in for the two surfaces and must be deterministic.
        let cli_status = aicx::search_engine::search_oracle_status_from_retrieval(
            aicx_home,
            retrieval,
            *hybrid,
            retrieval.matched_count,
            true,
        );
        let mcp_status = aicx::search_engine::search_oracle_status_from_retrieval(
            aicx_home,
            retrieval,
            *hybrid,
            retrieval.matched_count,
            true,
        );
        let cli_json = serde_json::to_string(&cli_status).expect("serialize cli status");
        let mcp_json = serde_json::to_string(&mcp_status).expect("serialize mcp status");
        assert_eq!(
            cli_json, mcp_json,
            "scenario {name} must serialize identically across surfaces"
        );

        let value: Value = serde_json::from_str(&cli_json).expect("parse status");
        assert!(
            value.get("retrieval").is_some(),
            "scenario {name} must carry the typed retrieval object: {value}"
        );
        rendered.push((name, cli_json));
    }

    for (i, (name_a, json_a)) in rendered.iter().enumerate() {
        for (name_b, json_b) in rendered.iter().skip(i + 1) {
            assert_ne!(
                json_a, json_b,
                "scenarios {name_a} and {name_b} must serialize distinctly"
            );
        }
    }
}
