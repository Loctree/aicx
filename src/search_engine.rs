//! Semantic-first search dispatch with explicit fuzzy fallback.
//!
//! `aicx search` is intended to be semantic by default: a query is encoded
//! through the in-process embedder ([`crate::embedder`], which re-exports
//! the local [`aicx_embeddings`] crate's GGUF stack) and matched against a
//! materialized vector index of the canonical store. Fuzzy filesystem
//! search ([`crate::rank::fuzzy_search_store`]) is the graceful fallback
//! when:
//!
//! - the binary was built without the `native-embedder` feature,
//! - the GGUF model cannot be resolved from `~/.aicx/embedder.toml`,
//!   `~/.aicx/config.toml`, env (`AICX_EMBEDDER_*`), or the local HF cache,
//! - the embedder fails to initialize (memory pressure, missing backend),
//! - or the vector index has not yet been built (`aicx index` is the
//!   shipping command for that).
//!
//! Every attempt resolves to a typed [`SearchPath`] so the caller can emit
//! an honest `oracle_status`: `backend=embedded_semantic` for a successful
//! semantic hit, otherwise `backend=filesystem_fuzzy_fallback` plus a human-
//! readable `fallback_reason`. Operators can therefore tell exactly which
//! retrieval ran from a single line of stderr or one JSON field.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::path::Path;

use anyhow::Result;

use crate::rank::FuzzyResult;
use crate::timeline::FrameKind;

/// Successful semantic search outcome with rendering-ready data.
#[derive(Debug)]
pub struct SemanticSearchOutcome {
    /// Top-N results ranked by cosine similarity to the query embedding.
    pub results: Vec<FuzzyResult>,
    /// Total candidate chunks the engine considered before truncating to
    /// the requested `limit`.
    pub scanned: usize,
    /// Stable backend label for `oracle_status` JSON / stderr output.
    /// Currently always `"embedded_semantic"`, reserved for future
    /// distinctions such as `"cloud_semantic"` once a cloud provider lands.
    pub backend_label: &'static str,
    /// Embedder model identifier surfaced for operator diagnostics
    /// (e.g. `"F2LLM-v2-0.6B.Q4_K_M.gguf"`).
    pub model_id: String,
}

/// Outcome of a semantic-vs-fallback dispatch.
#[derive(Debug)]
pub enum SearchPath {
    /// Semantic search succeeded; caller should render `results` and emit
    /// `oracle_status` with `backend=embedded_semantic`.
    Semantic(SemanticSearchOutcome),
    /// Semantic search is not currently available. Caller should fall
    /// back to [`crate::rank::fuzzy_search_store`] and emit
    /// `oracle_status` with `backend=filesystem_fuzzy_fallback` and the
    /// returned `reason`.
    Fallback { reason: String },
}

/// Attempt semantic search via the in-process embedder; otherwise return a
/// typed fallback signal that the caller routes to fuzzy search.
///
/// This function never panics and never spawns external processes. It is
/// safe to call on every `aicx search` invocation: in the absence of an
/// index it short-circuits to [`SearchPath::Fallback`] in microseconds, so
/// the cost of trying is bounded.
///
/// The function intentionally does not perform fuzzy search itself — that
/// keeps [`crate::rank::fuzzy_search_store`] as the single source of truth
/// for the lexical path and avoids accidental result-shape divergence.
pub fn try_semantic_search(
    _store_root: &Path,
    _query: &str,
    _limit: usize,
    _project_filter: Option<&str>,
    _frame_kind_filter: Option<FrameKind>,
) -> Result<SearchPath> {
    #[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
    {
        return Ok(SearchPath::Fallback {
            reason: "native-embedder feature not compiled in this binary".to_string(),
        });
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    {
        try_semantic_search_native(
            _store_root,
            _query,
            _limit,
            _project_filter,
            _frame_kind_filter,
        )
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn try_semantic_search_native(
    _store_root: &Path,
    _query: &str,
    _limit: usize,
    _project_filter: Option<&str>,
    _frame_kind_filter: Option<FrameKind>,
) -> Result<SearchPath> {
    // Probe the embedder before doing anything that touches the corpus.
    // If the GGUF model is missing or initialization fails, we want to
    // surface a precise reason rather than masking it as "no results".
    let engine = match crate::embedder::EmbeddingEngine::new() {
        Ok(engine) => engine,
        Err(err) => {
            return Ok(SearchPath::Fallback {
                reason: format!("embedder init failed: {err}"),
            });
        }
    };

    let _info = engine.info(); // touch to keep the value alive in scope

    // Vector index materialization lands in the next iteration as
    // `aicx index` (writes Lance vectors per chunk under
    // `~/.aicx/index/<project>/embeddings.lance`). Until that command
    // ships, the engine is provably loadable but there is no index to
    // query — so we fall back with an actionable reason that points the
    // operator at the next required step.
    Ok(SearchPath::Fallback {
        reason: "vector index not built yet (run `aicx index` after this iteration ships)"
            .to_string(),
    })
}

/// Compose the canonical `oracle_status` line emitted to stderr after a
/// search call, given the chosen path.
///
/// The shape mirrors the legacy hard-coded line so operators do not have
/// to learn a new format; only the values change to reflect reality.
pub fn render_oracle_status_line(path: &SearchPath, result_count: usize, scanned: usize) -> String {
    match path {
        SearchPath::Semantic(outcome) => format!(
            "{} result(s) from {} candidate chunks. oracle_status: backend={} index=lance fallback=none model={} loctree_scope_safe=true",
            result_count, scanned, outcome.backend_label, outcome.model_id
        ),
        SearchPath::Fallback { reason } => format!(
            "{} result(s) from {} scanned chunks. oracle_status: backend=filesystem_fuzzy_fallback index=none fallback_reason=\"{}\" loctree_scope_safe=false",
            result_count, scanned, reason
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn fallback_path_returns_actionable_reason() {
        // In any test environment we either lack the feature flag, lack a
        // built index, or both. The function must never panic and must
        // return a non-empty diagnostic reason that an operator can act
        // on.
        let result = try_semantic_search(
            Path::new("/tmp/aicx-search-engine-test"),
            "any query",
            10,
            None,
            None,
        )
        .expect("try_semantic_search must not return Err in fallback path");

        match result {
            SearchPath::Fallback { reason } => {
                assert!(!reason.is_empty(), "fallback reason must not be empty");
            }
            SearchPath::Semantic(_) => {
                // Allowed only if a developer has a fully wired index in
                // this test env. Iter 1 ships before that exists, so this
                // branch should not execute today; do not fail it though,
                // since the path is legal once Iter 2 lands.
            }
        }
    }

    #[test]
    fn oracle_status_line_for_fallback_includes_reason() {
        let path = SearchPath::Fallback {
            reason: "embedder init failed: no GGUF model found".to_string(),
        };
        let line = render_oracle_status_line(&path, 5, 421);
        assert!(line.contains("backend=filesystem_fuzzy_fallback"));
        assert!(line.contains("fallback_reason=\"embedder init failed: no GGUF model found\""));
        assert!(line.contains("5 result"));
        assert!(line.contains("421 scanned chunks"));
    }

    #[test]
    fn oracle_status_line_for_semantic_marks_backend_and_index() {
        let path = SearchPath::Semantic(SemanticSearchOutcome {
            results: Vec::new(),
            scanned: 11_237,
            backend_label: "embedded_semantic",
            model_id: "F2LLM-v2-0.6B.Q4_K_M.gguf".to_string(),
        });
        let line = render_oracle_status_line(&path, 0, 11_237);
        assert!(line.contains("backend=embedded_semantic"));
        assert!(line.contains("index=lance"));
        assert!(line.contains("model=F2LLM-v2-0.6B.Q4_K_M.gguf"));
        assert!(line.contains("loctree_scope_safe=true"));
    }
}
