//! Semantic-only search dispatch with fail-fast diagnostics.
//!
//! `aicx search` is **semantic-by-contract** since v0.7: a query is encoded
//! through the in-process embedder ([`crate::embedder`], which re-exports
//! the local [`aicx_embeddings`] crate's GGUF + cloud HTTP stack) and
//! matched against a materialized vector index of the canonical store.
//!
//! There is no silent fuzzy fallback. When a precondition is missing
//! (embedder unhydrated, index not built, empty/low-signal corpus,
//! dimension mismatch between query and index), [`try_semantic_search`]
//! returns a typed [`SemanticError`] with a human-readable `reason` AND
//! an actionable `recommendation`. The caller renders the error and
//! exits non-zero so operators see exactly what to do, instead of
//! receiving "0 results" without a story.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::path::Path;

use serde::Serialize;

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

/// Result of a semantic search call.
pub type SemanticOutcome = SemanticSearchOutcome;

/// Fail-fast typed error for semantic-search preconditions. Each variant
/// captures the diagnostic the operator needs to fix the problem and
/// retry.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SemanticError {
    /// Binary was compiled without any embedder feature flag.
    EmbedderFeatureMissing {
        reason: String,
        recommendation: String,
    },
    /// Embedder feature is compiled in but [`crate::embedder::EmbeddingEngine::new`]
    /// returned an error (model not hydrated, cloud endpoint unreachable, ...).
    EmbedderUnavailable {
        reason: String,
        recommendation: String,
    },
    /// `index_path(project)` does not exist on disk yet — operator never ran
    /// `aicx index`.
    IndexNotBuilt {
        path: std::path::PathBuf,
        reason: String,
        recommendation: String,
    },
    /// Index exists but is currently locked by an active writer.
    IndexBusy {
        path: std::path::PathBuf,
        reason: String,
        recommendation: String,
    },
    /// Index file exists but cannot be read or parsed.
    IndexCorrupt {
        path: std::path::PathBuf,
        reason: String,
        recommendation: String,
    },
    /// Index file dimension does not match the current embedder dimension.
    /// The corpus was indexed with a different model — semantic similarity
    /// across the boundary is meaningless. Force a rebuild.
    DimensionMismatch {
        path: std::path::PathBuf,
        index_dim: usize,
        embedder_dim: usize,
        reason: String,
        recommendation: String,
    },
    /// Index file exists but contains zero entries (empty NDJSON body).
    EmptyIndex {
        path: std::path::PathBuf,
        reason: String,
        recommendation: String,
    },
    /// Index returned 0 hits for this query — distinct from EmptyIndex.
    /// Indicates the corpus has chunks but none scored above threshold.
    /// (Currently we surface ALL hits regardless of score, so this only
    /// fires when the corpus is empty post-filter.)
    NoResults {
        path: std::path::PathBuf,
        scanned: usize,
        reason: String,
        recommendation: String,
    },
}

impl SemanticError {
    /// One-line reason for terse logs / oracle_status.
    pub fn reason(&self) -> &str {
        match self {
            Self::EmbedderFeatureMissing { reason, .. }
            | Self::EmbedderUnavailable { reason, .. }
            | Self::IndexNotBuilt { reason, .. }
            | Self::IndexBusy { reason, .. }
            | Self::IndexCorrupt { reason, .. }
            | Self::DimensionMismatch { reason, .. }
            | Self::EmptyIndex { reason, .. }
            | Self::NoResults { reason, .. } => reason,
        }
    }

    /// Actionable recommendation the operator can paste into their shell.
    pub fn recommendation(&self) -> &str {
        match self {
            Self::EmbedderFeatureMissing { recommendation, .. }
            | Self::EmbedderUnavailable { recommendation, .. }
            | Self::IndexNotBuilt { recommendation, .. }
            | Self::IndexBusy { recommendation, .. }
            | Self::IndexCorrupt { recommendation, .. }
            | Self::DimensionMismatch { recommendation, .. }
            | Self::EmptyIndex { recommendation, .. }
            | Self::NoResults { recommendation, .. } => recommendation,
        }
    }

    /// Stable kind label for `oracle_status: backend=fail_fast kind=...`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::EmbedderFeatureMissing { .. } => "embedder_feature_missing",
            Self::EmbedderUnavailable { .. } => "embedder_unavailable",
            Self::IndexNotBuilt { .. } => "index_not_built",
            Self::IndexBusy { .. } => "index_busy",
            Self::IndexCorrupt { .. } => "index_corrupt",
            Self::DimensionMismatch { .. } => "dimension_mismatch",
            Self::EmptyIndex { .. } => "empty_index",
            Self::NoResults { .. } => "no_results",
        }
    }
}

impl std::fmt::Display for SemanticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "semantic search unavailable: {}\n  recommendation: {}",
            self.reason(),
            self.recommendation()
        )
    }
}

impl std::error::Error for SemanticError {}

fn index_query_error(path: &std::path::Path, err: anyhow::Error) -> SemanticError {
    let reason = format!("index query failed: {err}");
    if reason.contains("timed out acquiring shared lock") {
        return SemanticError::IndexBusy {
            path: path.to_path_buf(),
            reason,
            recommendation: format!(
                "index is being written; wait for the active `aicx index` process to finish, \
                 then retry. Check the writer with `ps -p $(awk -F= '/^pid=/ {{print $2}}' {}) -o pid,etime,command`",
                crate::locks::lance_lock_path()
                    .unwrap_or_else(|_| std::path::PathBuf::from("~/.aicx/locks/lance.lock"))
                    .display()
            ),
        };
    }

    SemanticError::IndexCorrupt {
        path: path.to_path_buf(),
        reason,
        recommendation: format!(
            "delete and rebuild: `rm -f {} && aicx index`",
            path.display()
        ),
    }
}

/// Run a semantic search against the persistent vector index. Fails fast
/// with a typed [`SemanticError`] when any precondition is missing — no
/// silent fuzzy fallback. Each error variant carries an actionable
/// `recommendation` the operator can paste into their shell.
///
/// This function never panics and never spawns external processes.
pub fn try_semantic_search(
    _store_root: &Path,
    query: &str,
    limit: usize,
    project_filters: &[Option<&str>],
    frame_kind_filter: Option<FrameKind>,
    kind_filter: Option<&str>,
) -> std::result::Result<SemanticOutcome, SemanticError> {
    #[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
    {
        let _ = (
            query,
            limit,
            project_filters,
            frame_kind_filter,
            kind_filter,
        );
        Err(SemanticError::EmbedderFeatureMissing {
            reason: "this aicx binary was compiled without any embedder feature".to_string(),
            recommendation:
                "install a pre-built release from GitHub (e.g., `npm install -g @loctree/aicx`), \
                 or rebuild from source with `cargo install --path . --features native-embedder` \
                 (offline GGUF) or `cargo install --path . --features cloud-embedder` (HTTP /v1/embeddings)"
                    .to_string(),
        })
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    {
        let scopes = if project_filters.is_empty() {
            vec![None]
        } else {
            project_filters.to_vec()
        };
        let per_scope_limit = limit.max(1);
        let mut merged_results = Vec::new();
        let mut scanned = 0usize;
        let mut model_id = None;
        for scope in scopes {
            let mut outcome = try_semantic_search_native(
                query,
                per_scope_limit,
                scope,
                frame_kind_filter,
                kind_filter,
            )?;
            scanned += outcome.scanned;
            model_id.get_or_insert(outcome.model_id.clone());
            merged_results.append(&mut outcome.results);
        }
        merged_results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));
        merged_results.truncate(limit);
        Ok(SemanticOutcome {
            results: merged_results,
            scanned,
            backend_label: "embedded_semantic",
            model_id: model_id.unwrap_or_else(|| "unknown".to_string()),
        })
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn try_semantic_search_native(
    query: &str,
    limit: usize,
    project_filter: Option<&str>,
    frame_kind_filter: Option<FrameKind>,
    kind_filter: Option<&str>,
) -> std::result::Result<SemanticOutcome, SemanticError> {
    // Probe the embedder first.
    let engine = match crate::embedder::EmbeddingEngine::new() {
        Ok(engine) => engine,
        Err(err) => {
            let msg = err.to_string();
            let recommendation = if msg.contains("hydrated") || msg.contains("cache") {
                "run `hf download mradermacher/F2LLM-v2-0.6B-GGUF F2LLM-v2-0.6B.Q4_K_M.gguf` \
                 (or set `AICX_EMBEDDER_PATH=/path/to/your.gguf`), then retry"
                    .to_string()
            } else if msg.contains("cloud") || msg.contains("url") {
                "verify `~/.aicx/config.toml` `[embedder.cloud]` url + api_key_env are set, \
                 export the api key, retry"
                    .to_string()
            } else {
                "check `aicx config show` for resolved backend; if unhealthy run `aicx doctor` \
                 then retry"
                    .to_string()
            };
            return Err(SemanticError::EmbedderUnavailable {
                reason: format!("semantic embedder unavailable (optional): {msg}"),
                recommendation,
            });
        }
    };

    let info = engine.info().clone();
    let embedder_dim = info.dimension;

    let path = crate::vector_index::index_path(project_filter).map_err(|err| {
        SemanticError::IndexNotBuilt {
            path: std::path::PathBuf::new(),
            reason: format!("could not resolve index path: {err}"),
            recommendation: "ensure $AICX_HOME (or $HOME) is writable, then run `aicx index`"
                .to_string(),
        }
    })?;

    if !path.exists() {
        let cmd = match project_filter {
            Some(p) => format!("aicx index --project {p}"),
            None => "aicx index".to_string(),
        };
        return Err(SemanticError::IndexNotBuilt {
            path: path.clone(),
            reason: format!("vector index not yet materialized at {}", path.display()),
            recommendation: format!(
                "run `{cmd}` (one-off; subsequent runs query the index in-process)"
            ),
        });
    }

    // Touch the file once to surface IO errors early with a readable
    // recommendation.
    if let Err(err) = std::fs::metadata(&path) {
        return Err(SemanticError::IndexCorrupt {
            path: path.clone(),
            reason: format!("cannot stat index file: {err}"),
            recommendation: format!(
                "delete and rebuild: `rm -f {} && aicx index`",
                path.display()
            ),
        });
    }

    // Read header line first so we can detect dimension mismatch BEFORE
    // touching any vectors.
    if let Some(header) = read_index_header(&path) {
        if header.dimension != embedder_dim {
            return Err(SemanticError::DimensionMismatch {
                path: path.clone(),
                index_dim: header.dimension,
                embedder_dim,
                reason: format!(
                    "index built with dimension={} (model {}), current embedder is dimension={} (model {})",
                    header.dimension, header.model_id, embedder_dim, info.model_id
                ),
                recommendation: format!(
                    "rebuild the index with the current embedder: `rm -f {} && aicx index`",
                    path.display()
                ),
            });
        }
        if header.entry_count == 0 && index_appears_empty(&path) {
            return Err(SemanticError::EmptyIndex {
                path: path.clone(),
                reason: format!(
                    "index file at {} contains 0 entries — corpus may be empty or all chunks failed to embed",
                    path.display()
                ),
                recommendation: "run `aicx extract --all` to populate the canonical corpus, \
                     then rebuild: `aicx index`"
                    .to_string(),
            });
        }
    }

    // Probe the query string itself — empty / whitespace-only queries
    // cannot produce a meaningful embedding.
    if query.trim().is_empty() {
        return Err(SemanticError::NoResults {
            path: path.clone(),
            scanned: 0,
            reason: "query is empty or whitespace-only — embedder needs at least one token"
                .to_string(),
            recommendation:
                "pass a non-empty query, e.g. `aicx search 'how does the noise filter work'`"
                    .to_string(),
        });
    }

    // Drop the engine handle before query_index opens its own (locks are
    // shared so concurrent reads are fine).
    drop(engine);

    let hits = match crate::vector_index::query_index(
        project_filter,
        query,
        limit,
        kind_filter,
        frame_kind_filter.map(FrameKind::as_str),
    ) {
        Ok(hits) => hits,
        Err(err) => return Err(index_query_error(&path, err)),
    };

    if hits.is_empty() {
        return Err(SemanticError::NoResults {
            path: path.clone(),
            scanned: 0,
            reason: format!(
                "index at {} produced 0 ranked hits for this query",
                path.display()
            ),
            recommendation: "either the index is empty (rebuild with `aicx index`) \
                 or your query has no semantic neighbours in the corpus — try broader phrasing"
                .to_string(),
        });
    }

    let scanned = hits.len();
    let results: Vec<FuzzyResult> = hits
        .into_iter()
        .take(limit)
        .map(|h| {
            // Map cosine [-1, 1] → unsigned 0..=100 score for FuzzyResult
            // (downstream renderers share the shape with the lexical
            // path). Negative scores clamp to 0; lexical never emits
            // negatives either.
            let score_pct = ((h.score.max(0.0) * 100.0).round() as u8).min(100);
            let matched_lines = semantic_preview_lines(&h.path);
            FuzzyResult {
                file: h.path.to_string_lossy().to_string(),
                path: h.path.to_string_lossy().to_string(),
                project: h.project,
                kind: h.kind,
                frame_kind: h.frame_kind,
                agent: h.agent,
                date: h.date,
                timestamp: None,
                score: score_pct,
                label: format!("semantic:{}", h.id),
                density: h.score,
                matched_lines,
                session_id: Some(h.session_id),
                cwd: h.cwd,
            }
        })
        .collect();

    Ok(SemanticOutcome {
        results,
        scanned,
        backend_label: "embedded_semantic",
        model_id: info.model_id,
    })
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn read_index_header(path: &std::path::Path) -> Option<crate::vector_index::IndexHeader> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    serde_json::from_str(first.trim()).ok()
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn index_appears_empty(path: &std::path::Path) -> bool {
    use std::io::{BufRead, BufReader};
    let Ok(file) = std::fs::File::open(path) else {
        return true;
    };
    let reader = BufReader::new(file);
    // Skip header (first line); if no second line, index is empty.
    reader
        .lines()
        .skip(1)
        .find(|line| matches!(line, Ok(s) if !s.trim().is_empty()))
        .is_none()
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn semantic_preview_lines(path: &std::path::Path) -> Vec<String> {
    const MAX_LINES: usize = 6;
    let Ok(content) = crate::sanitize::read_to_string_validated(path) else {
        return Vec::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("[project:"))
        .filter(|line| *line != "[signals]" && *line != "[/signals]")
        .filter(|line| !line.starts_with("id: "))
        .take(MAX_LINES)
        .map(|line| line.strip_prefix("content: ").unwrap_or(line).to_string())
        .collect()
}

/// Compose the canonical `oracle_status` line emitted to stderr after a
/// successful semantic search call.
pub fn render_semantic_status_line(
    backend_label: &str,
    model_id: &str,
    result_count: usize,
    scanned: usize,
) -> String {
    format!(
        "{} result(s) from {} candidate chunks. oracle_status: backend={} index=lance fallback=none model={} loctree_scope_safe=true",
        result_count, scanned, backend_label, model_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn fail_fast_carries_actionable_recommendation() {
        // In any test environment we either lack the feature flag, lack a
        // hydrated embedder, or lack a built index. The function must
        // never panic, and the typed error must carry both a non-empty
        // `reason` AND a non-empty `recommendation` so the operator
        // knows what to do next.
        let result = try_semantic_search(
            Path::new("/tmp/aicx-search-engine-test"),
            "any query",
            10,
            &[None],
            None,
            None,
        );

        match result {
            Err(err) => {
                assert!(
                    !err.reason().is_empty(),
                    "fail-fast reason must not be empty"
                );
                assert!(
                    !err.recommendation().is_empty(),
                    "fail-fast recommendation must not be empty"
                );
                // `kind` is always a stable lowercase snake_case label.
                assert!(
                    !err.kind().is_empty()
                        && err
                            .kind()
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c == '_'),
                    "kind label must be snake_case lowercase: {:?}",
                    err.kind()
                );
            }
            Ok(_) => {
                // Only legal when a fully wired index exists in this
                // test env (developer host). Don't fail in that case;
                // the success branch is also a valid contract.
            }
        }
    }

    #[test]
    fn lock_timeout_is_index_busy_not_corrupt() {
        let err = index_query_error(
            Path::new("/tmp/aicx/indexed/_all/embeddings.ndjson"),
            anyhow::anyhow!("timed out acquiring shared lock: /tmp/aicx/locks/lance.lock"),
        );

        assert_eq!(err.kind(), "index_busy");
        assert!(err.reason().contains("timed out acquiring shared lock"));
        assert!(!err.recommendation().contains("rm -f"));
        assert!(err.recommendation().contains("aicx index"));
    }

    #[test]
    fn semantic_status_line_marks_backend_and_index() {
        let line = render_semantic_status_line(
            "embedded_semantic",
            "F2LLM-v2-0.6B.Q4_K_M.gguf",
            0,
            11_237,
        );
        assert!(line.contains("backend=embedded_semantic"));
        assert!(line.contains("index=lance"));
        assert!(line.contains("model=F2LLM-v2-0.6B.Q4_K_M.gguf"));
        assert!(line.contains("loctree_scope_safe=true"));
    }
}
