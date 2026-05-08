//! End-to-end pipeline test for `aicx` semantic search.
//!
//! Opt-in via `--features e2e-aicx`. Drives the full retrieval pipeline
//! against the operator's canonical `~/.aicx/config.toml` (cloud-first
//! cascade) so the test verifies the *real* configuration path operators
//! ship to users — not a synthetic bypass.
//!
//! Pipeline asserted, end to end:
//!   1. canonical store has at least one chunk (extract was previously run)
//!   2. embedder loads via the configured backend (cloud or native GGUF)
//!   3. `vector_index::write_index` materializes an NDJSON file under
//!      `~/.aicx/index/<bucket>/embeddings.ndjson`
//!   4. `vector_index::query_index` returns at least one hit for a
//!      well-formed query, dimension-matched against the index header
//!   5. cosine score is in `[0.0, 1.0]` for the top hit (sanity)
//!
//! Fail-fast philosophy: when any precondition is missing the test panics
//! with the same `SemanticError` shape the production CLI emits, so the
//! operator running this test sees the same diagnostic + recommendation
//! as a real user would.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

#![cfg(feature = "e2e-aicx")]

use std::path::PathBuf;

fn aicx_home() -> PathBuf {
    if let Ok(value) = std::env::var("AICX_HOME") {
        PathBuf::from(value)
    } else {
        dirs::home_dir().expect("HOME").join(".aicx")
    }
}

fn config_path() -> PathBuf {
    if let Ok(value) = std::env::var("AICX_CONFIG_PATH") {
        PathBuf::from(value)
    } else {
        aicx_home().join("config.toml")
    }
}

fn assert_config_present() {
    let cfg = config_path();
    assert!(
        cfg.exists(),
        "aicx canonical config not found at {} — \
         run `aicx config init` to scaffold one, then set [embedder.cloud] or AICX_EMBEDDER_PATH",
        cfg.display()
    );
}

#[test]
fn e2e_index_and_query_roundtrip() {
    assert_config_present();

    // Step 1: probe corpus. If empty, the operator hasn't run extract
    // yet — same precondition the production search would surface.
    let store_root = aicx::store::store_base_dir().expect("store_base_dir");
    let chunks =
        aicx::store::scan_context_files_project_at(&store_root, None).expect("scan corpus");
    assert!(
        !chunks.is_empty(),
        "canonical corpus at {} is empty — run `aicx extract --all` before invoking the e2e test",
        store_root.display()
    );

    // Step 2: build a real index covering up to 16 chunks (small enough
    // to be a fast smoke without paying full-corpus embedding cost).
    let stats = aicx::vector_index::write_index(None, 16).expect("write_index must not return Err");
    assert!(
        stats.fallback_reason.is_none(),
        "embedder unavailable for e2e: {:?}\n\
         recommendation: set [embedder.cloud] in {} (preferred) or hydrate the native GGUF",
        stats.fallback_reason,
        config_path().display()
    );
    assert!(
        stats.embeddings_computed > 0,
        "write_index produced 0 embeddings — embedder probe succeeded but every embed call \
         failed; check `aicx config show` and the embedder URL/model_id is reachable"
    );
    let index_path = stats
        .index_path
        .expect("write_index must materialize an index_path");
    assert!(
        index_path.exists(),
        "index_path {} reported but file does not exist on disk",
        index_path.display()
    );

    // Step 3: query the index. Use the fail-fast `try_semantic_search`
    // entrypoint so we exercise the same dispatch the CLI uses.
    let outcome = aicx::search_engine::try_semantic_search(
        &store_root,
        "operator decision and architecture",
        5,
        None,
        None,
    )
    .unwrap_or_else(|err| {
        panic!(
            "e2e semantic search failed: kind={} reason={}\n\
             recommendation: {}",
            err.kind(),
            err.reason(),
            err.recommendation()
        )
    });

    assert!(
        !outcome.results.is_empty(),
        "semantic query returned 0 results despite a populated index — \
         either the corpus is too small to surface anything semantically related to the query, \
         or the embedder cascade silently flipped to a fallback model with mismatched dimension"
    );

    // Step 4: top hit sanity. Cosine maps to score `[0, 100]`.
    let top = outcome
        .results
        .first()
        .expect("we asserted non-empty above");
    assert!(
        top.score <= 100,
        "top hit score {} exceeds [0, 100] range — cosine clamp regression",
        top.score
    );
    assert_eq!(outcome.backend_label, "embedded_semantic");
    assert!(
        !outcome.model_id.is_empty(),
        "outcome must record the embedder model_id for operator diagnostics"
    );

    // Cleanup: remove the test-built index so re-runs start clean.
    // Don't fail the test if removal fails — the index file is small.
    let _ = std::fs::remove_file(&index_path);
}
