//! Semantic search dispatch with typed diagnostics.
//!
//! The library primitive is **semantic-by-contract**: a query is encoded
//! through the in-process embedder ([`crate::embedder`], which re-exports
//! the local [`aicx_embeddings`] crate's GGUF + cloud HTTP stack) and
//! matched against a materialized vector index of the canonical store.
//!
//! The primitive never pretends fuzzy results are semantic. When a precondition is missing
//! (embedder unhydrated, index not built, empty/low-signal corpus,
//! dimension mismatch between query and index), [`try_semantic_search`]
//! returns a typed [`SemanticError`] with a human-readable `reason` AND
//! an actionable `recommendation`. The CLI may then explicitly degrade to
//! filesystem-fuzzy while surfacing the semantic failure in its rendered output.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

std::thread_local! {
    /// CLI-only recovery selection. The process executes one command, while
    /// library/MCP callers pass `SemanticSearchFilters::legacy_dense` directly.
    pub static LEGACY_DENSE_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

use aicx_retrieve::{
    ExecutedPath, RequestedMode, RetrievalCompleteness, RetrievalEvidence, RetrievalOutcome,
};

use crate::rank::FuzzyResult;
use crate::sanitize::normalize_query;
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
    /// Manifest-backed hybrid retrieval status, when the live path is the
    /// committed hybrid stack rather than a legacy vector scan.
    pub retrieval_status: Option<HybridRetrievalStatus>,
}

/// Result of a semantic search call.
pub type SemanticOutcome = SemanticSearchOutcome;

#[derive(Debug, Clone, Copy, Default)]
struct CandidateBoundary {
    examined: usize,
    saturated: bool,
}

const BACKEND_HYBRID_RRF: &str = "hybrid_rrf";
const BACKEND_HYBRID_RRF_GLOBAL_SCOPED: &str = "hybrid_rrf_global_scoped";
const BACKEND_LEXICAL: &str = "lexical_tantivy";
const BACKEND_LEXICAL_GLOBAL_SCOPED: &str = "lexical_tantivy_global_scoped";
const BACKEND_SEMANTIC_DENSE_ONLY: &str = "semantic_dense_only";
const BACKEND_SEMANTIC_DENSE_ONLY_GLOBAL_SCOPED: &str = "semantic_dense_only_global_scoped";
const BACKEND_SEMANTIC_LEGACY_DENSE: &str = "semantic_legacy_dense";
const BACKEND_SEMANTIC_LEGACY_DENSE_GLOBAL_SCOPED: &str = "semantic_legacy_dense_global_scoped";
/// Retired multi-shard fan-out budget (kept for regression tests only).
#[cfg(all(test, any(feature = "native-embedder", feature = "cloud-embedder")))]
const GLOBAL_SHARD_BUDGET: usize = 16;

#[cfg(all(test, any(feature = "native-embedder", feature = "cloud-embedder")))]
fn bounded_global_shards(available: &[String]) -> (&[String], bool) {
    let selected = &available[..available.len().min(GLOBAL_SHARD_BUDGET)];
    (selected, available.len() > selected.len())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HybridRetrievalStatus {
    pub generation_id: String,
    pub source_chunk_count: usize,
    pub dense_count: usize,
    pub lexical_doc_count: usize,
    pub fusion_algorithm: String,
    pub dense_kind: String,
}

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
    /// The committed vector artifact exists, but the hybrid manifest that
    /// binds lexical+dense generations is missing.
    RetrievalManifestMissing {
        path: std::path::PathBuf,
        reason: String,
        recommendation: String,
    },
    /// The hybrid manifest exists but no longer matches the dense/lexical
    /// artifacts, embedder fingerprint, or committed source hash.
    RetrievalManifestStale {
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
            | Self::RetrievalManifestMissing { reason, .. }
            | Self::RetrievalManifestStale { reason, .. }
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
            | Self::RetrievalManifestMissing { recommendation, .. }
            | Self::RetrievalManifestStale { recommendation, .. }
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
            Self::RetrievalManifestMissing { .. } => "retrieval_manifest_missing",
            Self::RetrievalManifestStale { .. } => "retrieval_manifest_stale",
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
    try_semantic_search_with_boundary(
        _store_root,
        query,
        limit,
        project_filters,
        frame_kind_filter,
        kind_filter,
        None,
    )
    .map(|(outcome, _)| outcome)
}

fn try_semantic_search_with_boundary(
    _store_root: &Path,
    query: &str,
    limit: usize,
    project_filters: &[Option<&str>],
    frame_kind_filter: Option<FrameKind>,
    kind_filter: Option<&str>,
    candidate_filters: Option<&SemanticSearchFilters>,
) -> std::result::Result<(SemanticOutcome, CandidateBoundary), SemanticError> {
    #[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
    {
        let _ = (
            query,
            limit,
            project_filters,
            frame_kind_filter,
            kind_filter,
            candidate_filters,
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
        // Doctrine (2026-07-23): always query the published `_all` hybrid
        // CURRENT generation once. Project is a metadata filter, never a
        // separate index precondition and never a fan-out over project shards
        // (that path hung multi-second / multi-GB).
        let global_query = project_filters.is_empty()
            || (project_filters.len() == 1 && project_filters[0].is_none());
        let scopes: Vec<Option<&str>> = if global_query {
            vec![None]
        } else {
            project_filters.to_vec()
        };
        let per_scope_limit = limit.max(1);
        let deep = candidate_filters.map(|f| f.deep).unwrap_or(false);
        let legacy_dense = candidate_filters.map(|f| f.legacy_dense).unwrap_or(false);
        let mut merged_results = Vec::new();
        let mut scanned = 0usize;
        let mut model_id = None;
        let mut hybrid_statuses = Vec::new();
        let mut any_dense_only = false;
        let mut any_legacy_dense = false;
        let mut any_lexical = false;
        let mut any_global_project_scope = false;
        let mut boundary = CandidateBoundary::default();
        for scope in scopes {
            let (mut outcome, scope_boundary) = if deep || legacy_dense {
                try_semantic_search_native(
                    query,
                    per_scope_limit,
                    scope,
                    frame_kind_filter,
                    kind_filter,
                    candidate_filters,
                )?
            } else {
                try_lexical_search_native(
                    query,
                    per_scope_limit,
                    scope,
                    frame_kind_filter,
                    kind_filter,
                    candidate_filters,
                )?
            };
            boundary.examined = boundary.examined.saturating_add(scope_boundary.examined);
            boundary.saturated |= scope_boundary.saturated;
            if outcome
                .backend_label
                .starts_with(BACKEND_SEMANTIC_DENSE_ONLY)
                || outcome
                    .backend_label
                    .starts_with(BACKEND_SEMANTIC_LEGACY_DENSE)
            {
                any_dense_only = true;
            }
            if outcome
                .backend_label
                .starts_with(BACKEND_SEMANTIC_LEGACY_DENSE)
            {
                any_legacy_dense = true;
            }
            if outcome.backend_label.starts_with(BACKEND_LEXICAL) {
                any_lexical = true;
            }
            if outcome.backend_label.ends_with("_global_scoped") {
                any_global_project_scope = true;
            }
            scanned += outcome.scanned;
            model_id.get_or_insert(outcome.model_id.clone());
            if let Some(status) = outcome.retrieval_status.clone() {
                hybrid_statuses.push(status);
            }
            merged_results.append(&mut outcome.results);
        }
        apply_recency_prior(&mut merged_results);
        merged_results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));
        merged_results.truncate(limit);
        Ok((
            SemanticOutcome {
                results: merged_results,
                scanned,
                backend_label: if any_legacy_dense && any_global_project_scope {
                    BACKEND_SEMANTIC_LEGACY_DENSE_GLOBAL_SCOPED
                } else if any_legacy_dense {
                    BACKEND_SEMANTIC_LEGACY_DENSE
                } else if any_dense_only && any_global_project_scope {
                    BACKEND_SEMANTIC_DENSE_ONLY_GLOBAL_SCOPED
                } else if any_dense_only {
                    BACKEND_SEMANTIC_DENSE_ONLY
                } else if any_lexical && any_global_project_scope {
                    BACKEND_LEXICAL_GLOBAL_SCOPED
                } else if any_lexical {
                    BACKEND_LEXICAL
                } else if any_global_project_scope {
                    BACKEND_HYBRID_RRF_GLOBAL_SCOPED
                } else {
                    BACKEND_HYBRID_RRF
                },
                model_id: model_id.unwrap_or_else(|| "unknown".to_string()),
                retrieval_status: merge_hybrid_statuses(&hybrid_statuses),
            },
            boundary,
        ))
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[derive(Debug)]
struct SemanticBucketScope<'a> {
    index_project: Option<&'a str>,
    retrieval_project_filter: Option<&'a str>,
    index_path: std::path::PathBuf,
    used_global_project_scope: bool,
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[derive(Clone, Copy)]
struct SemanticRetrievalFilters<'a> {
    kind: Option<&'a str>,
    frame_kind: Option<FrameKind>,
    project: Option<&'a str>,
    agent: Option<&'a str>,
    date: Option<&'a str>,
    candidate_filters: Option<&'a SemanticSearchFilters>,
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn index_not_built_error(path: std::path::PathBuf, project_filter: Option<&str>) -> SemanticError {
    let recommendation = match project_filter {
        Some(p) => format!(
            "run `aicx index` to build the global index used by `search -p {p}`; \
             optionally run `aicx index --project {p}` to materialize a local project cache"
        ),
        None => {
            "run `aicx index` (one-off; subsequent runs query the index in-process)".to_string()
        }
    };
    let legacy_hint = legacy_index_hint(project_filter, &path)
        .map(|hint| format!(" {hint}"))
        .unwrap_or_default();
    SemanticError::IndexNotBuilt {
        path: path.clone(),
        reason: format!(
            "vector index not yet materialized at {}{}",
            path.display(),
            legacy_hint
        ),
        recommendation,
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn legacy_index_hint(project_filter: Option<&str>, canonical_path: &Path) -> Option<String> {
    let legacy_path = crate::os_user_home()?
        .join("index")
        .join(crate::vector_index::index_bucket_name(project_filter))
        .join("embeddings.ndjson");
    if legacy_path.exists() && legacy_path != canonical_path {
        Some(format!(
            "Found legacy index at {}; current AICX uses {}.",
            legacy_path.display(),
            canonical_path.display()
        ))
    } else {
        None
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn select_semantic_bucket_scope<'a>(
    project_filter: Option<&'a str>,
    project_index_path: std::path::PathBuf,
    all_index_path: std::path::PathBuf,
) -> std::result::Result<SemanticBucketScope<'a>, SemanticError> {
    // Prefer the global `_all` generation for every query. Project is a
    // metadata filter on that corpus, not a separate index requirement.
    // Optional per-project shards remain usable as a local cache when the
    // global bucket is missing (migration / partial rollouts).
    if all_index_path.exists() {
        return Ok(SemanticBucketScope {
            index_project: None,
            retrieval_project_filter: project_filter,
            index_path: all_index_path,
            used_global_project_scope: project_filter.is_some(),
        });
    }

    if project_index_path.exists() {
        return Ok(SemanticBucketScope {
            index_project: project_filter,
            retrieval_project_filter: project_filter,
            index_path: project_index_path,
            used_global_project_scope: false,
        });
    }

    Err(index_not_built_error(
        if project_filter.is_some() {
            project_index_path
        } else {
            all_index_path
        },
        project_filter,
    ))
}

/// Lexical-first retrieval against the published `_all` hybrid CURRENT
/// generation (Tantivy only). No embedder bootstrap, no dense mmap, no
/// primary-NDJSON rehash. Project/kind/frame/agent filters are metadata
/// pushdown. Dense re-rank lives behind `--deep` / `SemanticSearchFilters::deep`.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn try_lexical_search_native(
    query: &str,
    limit: usize,
    project_filter: Option<&str>,
    frame_kind_filter: Option<FrameKind>,
    kind_filter: Option<&str>,
    candidate_filters: Option<&SemanticSearchFilters>,
) -> std::result::Result<(SemanticOutcome, CandidateBoundary), SemanticError> {
    if query.trim().is_empty() {
        return Err(SemanticError::NoResults {
            path: std::path::PathBuf::new(),
            scanned: 0,
            reason: "query is empty or whitespace-only".to_string(),
            recommendation:
                "pass a non-empty query, e.g. `aicx search 'how does the noise filter work'`"
                    .to_string(),
        });
    }

    // Always the global `_all` hybrid root — project is a filter, not a bucket.
    let hybrid_root =
        crate::vector_index::hybrid_root_dir(None).map_err(|err| SemanticError::IndexNotBuilt {
            path: std::path::PathBuf::new(),
            reason: format!("could not resolve hybrid root: {err}"),
            recommendation: "ensure $AICX_HOME (or $HOME) is writable, then run `aicx index`"
                .to_string(),
        })?;
    let gen_dir = crate::vector_index::resolve_hybrid_generation_dir(&hybrid_root);
    let manifest_path = gen_dir.join("manifest.json");
    if !manifest_path.exists() {
        return Err(SemanticError::IndexNotBuilt {
            path: manifest_path.clone(),
            reason: format!(
                "no published hybrid generation at {} (CURRENT + generations/)",
                hybrid_root.display()
            ),
            recommendation: "run `aicx index` to publish a hybrid generation with tantivy_lex"
                .to_string(),
        });
    }

    let manifest = aicx_retrieve::Manifest::read_from_path(&manifest_path).map_err(|err| {
        SemanticError::RetrievalManifestStale {
            path: manifest_path.clone(),
            reason: format!("could not read retrieval manifest: {err}"),
            recommendation: "run `aicx index` to rebuild the hybrid retrieval bucket".to_string(),
        }
    })?;

    // Fail closed when CURRENT did not resolve and the root still names the
    // retired brute-force NDJSON dense adapter (would otherwise hang).
    if gen_dir == hybrid_root && manifest.dense_kind == aicx_retrieve::BRUTE_FORCE_KIND {
        return Err(SemanticError::RetrievalManifestStale {
            path: manifest_path.clone(),
            reason: "published hybrid manifest names the retired NDJSON dense adapter \
                     and no CURRENT generation pointer is available"
                .to_string(),
            recommendation:
                "run `aicx index` to publish an mmap generation, or pass `--legacy-dense` only for recovery"
                    .to_string(),
        });
    }

    let lexical = aicx_retrieve::TantivyAdapter::new(gen_dir.clone()).map_err(|err| {
        SemanticError::RetrievalManifestStale {
            path: manifest_path.clone(),
            reason: format!("could not open hybrid lexical artifact: {err:#}"),
            recommendation: "run `aicx index` to rebuild the committed hybrid artifacts"
                .to_string(),
        }
    })?;

    let retrieval_filters = SemanticRetrievalFilters {
        kind: kind_filter,
        frame_kind: frame_kind_filter,
        project: project_filter,
        agent: candidate_filters.and_then(|filters| filters.agent.as_deref()),
        date: candidate_filters.and_then(candidate_exact_date),
        candidate_filters,
    };
    let filters = hybrid_filters(retrieval_filters);
    // Recency is a re-ranker, so it needs a wider lexical candidate set than
    // the final result count. This used to be expensive because every
    // candidate triggered a store-file preview read; conversation previews
    // now come from indexed metadata and the top set performs no source I/O.
    // A 500-hit global floor keeps the intended fresh *and* relevant July
    // result in view. Exact project pushdown needs only 100 candidates and
    // avoids cold page-fault cost on the scoped sub-second path.
    let window = lexical_rerank_window(limit, project_filter.is_some());

    // Push project (and other equality filters) into Tantivy so BM25 ranks
    // *inside* the project, not post-filters a global top-N (which silently
    // emptied project-scoped queries when the project sat outside that window).
    let mut query_filters = filters.clone();
    // Client-side project_filter_matches still handles bare/wildcard forms;
    // for exact owner/repo prefer the indexed project_filter field.
    if project_filter.is_some_and(|p| p.contains('/') && !p.starts_with('/') && !p.ends_with('/')) {
        // already in hybrid_filters
    } else {
        // Bare / wildcard: do not pin project_filter; client retain below.
        query_filters.values.remove("project");
    }

    let raw = aicx_retrieve::LexicalIndex::query(
        &lexical,
        &aicx_retrieve::LexicalQuery {
            text: query.to_string(),
            limit: window,
            filters: query_filters,
        },
    )
    .map_err(|err| SemanticError::IndexCorrupt {
        path: gen_dir.clone(),
        reason: format!("lexical query failed: {err:#}"),
        recommendation: "retry; if it persists rebuild with `aicx index`".to_string(),
    })?;

    let examined = raw.len();
    let mut hits: Vec<aicx_retrieve::Hit> = raw
        .into_iter()
        .filter(|hit| {
            metadata_matches_filters(&hit.metadata, &filters)
                && candidate_filters
                    .is_none_or(|f| semantic_candidate_metadata_matches(&hit.metadata, f))
        })
        .collect();

    // Project filter may be a bare name / wildcard form resolved by the CLI
    // into a concrete slug; also accept case-insensitive equality on the
    // stored project metadata string.
    if let Some(project) = project_filter {
        hits.retain(|hit| {
            hit.metadata
                .get("project")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|stored| {
                    stored.eq_ignore_ascii_case(project)
                        || crate::store::project_filter_matches(
                            stored.split_once('/').map(|(o, _)| o).unwrap_or(""),
                            stored.split_once('/').map(|(_, r)| r).unwrap_or(stored),
                            project,
                        )
                })
        });
    }

    let used_global = project_filter.is_some();
    let backend_label = if used_global {
        BACKEND_LEXICAL_GLOBAL_SCOPED
    } else {
        BACKEND_LEXICAL
    };
    // Build results without per-hit store file I/O. Preview lines are filled
    // only for the truncated top set so missing-store paths cannot dominate.
    // Timestamp (when present) drives the recency prior; fall back to date.
    let mut results: Vec<FuzzyResult> = hits
        .into_iter()
        .map(|h| {
            let path = hit_path(&h);
            let score_pct = lexical_score_pct(h.score);
            let matched_lines = hit_metadata_lines(&h, "preview_lines");
            let date = hit_metadata_string(&h, "date");
            let timestamp = hit_metadata_optional_string(&h, "timestamp")
                .or_else(|| hit_metadata_optional_string(&h, "session_date"))
                .or_else(|| {
                    if date.is_empty() || date == "-" {
                        None
                    } else {
                        Some(date.clone())
                    }
                });
            FuzzyResult {
                file: path.to_string_lossy().to_string(),
                path: path.to_string_lossy().to_string(),
                project: hit_metadata_string(&h, "project"),
                kind: hit_metadata_string(&h, "kind"),
                frame_kind: hit_metadata_optional_string(&h, "frame_kind"),
                agent: hit_metadata_string(&h, "agent"),
                date,
                timestamp,
                score: score_pct,
                label: format!("{backend_label}:{}", h.chunk_id),
                density: h.score,
                matched_lines,
                session_id: hit_metadata_optional_string(&h, "session_id"),
                cwd: hit_metadata_optional_string(&h, "cwd"),
            }
        })
        .collect();

    apply_recency_prior(&mut results);
    // Lexical path used to stop at BM25+recency. That left pre-filter CURRENT
    // generations ranking thought-token streams and JSON event dumps above
    // readable operator answers. Quality demotion + preview sanitize close
    // that lie until (and after) the next signal-filter rebuild.
    for result in &mut results {
        sanitize_lexical_preview_lines(result);
    }
    apply_lexical_quality_prior(query, &mut results);
    results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));
    results.truncate(limit);
    // Prefer indexed preview metadata. Only open source files when preview is
    // empty and the path exists — cold project-scoped searches were paying
    // multi-hundred-ms for optional body peeks on every top hit.
    for result in &mut results {
        if !result.matched_lines.is_empty() {
            continue;
        }
        if result.frame_kind.as_deref() == Some("conversation") {
            continue;
        }
        let path = std::path::Path::new(&result.path);
        if path.exists() {
            result.matched_lines = semantic_preview_lines(path);
            sanitize_lexical_preview_lines(result);
        }
    }

    let retrieval_status = Some(HybridRetrievalStatus::from(&manifest));
    let boundary = CandidateBoundary {
        examined,
        saturated: examined >= window && results.len() < limit,
    };

    Ok((
        SemanticOutcome {
            results,
            scanned: examined,
            backend_label,
            model_id: manifest.embedder_model.clone(),
            retrieval_status,
        },
        boundary,
    ))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn metadata_matches_filters(
    metadata: &serde_json::Value,
    filters: &aicx_retrieve::FilterSet,
) -> bool {
    filters.values.iter().all(|(key, expected)| {
        if key == "project" {
            // Project uses richer matching in the caller.
            return true;
        }
        metadata.get(key) == Some(expected)
    })
}

/// Map a Tantivy BM25 score onto 0..=100 for operator-facing display.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn lexical_score_pct(score: f32) -> u8 {
    // BM25 commonly lands in ~0..25 for short queries; clamp generously.
    ((score.max(0.0) / 25.0 * 100.0).round() as i32).clamp(0, 100) as u8
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn lexical_rerank_window(limit: usize, project_scoped: bool) -> usize {
    // Project equality is pushed into Tantivy when the filter is exact
    // `owner/repo`, so a modest window is enough. Global search needs a wider
    // floor for the recency re-ranker to still surface a fresh near-match.
    limit.max(if project_scoped { 80 } else { 500 })
}

/// Recency prior: fresh conversations win over repeated stale mentions when
/// lexical relevance is reasonably close. The seven-day decay deliberately
/// reflects the operator's default question: "where did we discuss this
/// recently?"
///
/// Scores are intentionally allowed to exceed 100 after the boost. The old
/// `.min(100)` clamp erased the prior whenever BM25 already sat near the
/// display ceiling (common for short operator queries), so same-day and
/// ten-day-old hits tied at 100 and ranking fell back to date noise.
fn apply_recency_prior(results: &mut [FuzzyResult]) {
    let today = chrono::Utc::now().date_naive();
    for result in results.iter_mut() {
        let raw = result
            .timestamp
            .as_deref()
            .map(|ts| ts.get(..10).unwrap_or(ts))
            .unwrap_or(result.date.as_str());
        if raw.is_empty() || raw == "-" {
            continue;
        }
        let Ok(date) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") else {
            continue;
        };
        let age_days = (today - date).num_days().max(0) as f32;
        let boost = (45.0 * (-age_days / 7.0).exp()).round() as u8;
        // Cap at 145 (100 lexical + max 45 recency) — keeps relative ranking
        // while still fitting the historical u8 score field.
        result.score = result.score.saturating_add(boost).min(145);
    }
}

/// Drop thought-token / pure event-stream lines from operator-facing previews.
///
/// Pre-filter index generations stored raw `runtime_runs` token streams in
/// `preview_lines`. Leaving them in the result body made search look like the
/// mill still owned the product surface even when BM25 hit the right session.
/// When a line is an `item.completed` envelope carrying `agent_message` text,
/// unwrap that text so ranking and display use the human answer, not the JSON
/// wrapper (works even before the next signal-filter rebuild).
fn sanitize_lexical_preview_lines(result: &mut FuzzyResult) {
    if result.matched_lines.is_empty() {
        return;
    }
    let cleaned: Vec<String> = result
        .matched_lines
        .iter()
        .filter_map(|line| normalize_preview_line(line))
        .collect();
    if !cleaned.is_empty() {
        result.matched_lines = cleaned;
    }
}

fn normalize_preview_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if is_thought_token_or_event_noise_line(trimmed) {
        return None;
    }
    if let Some(text) = unwrap_agent_message_event(trimmed) {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        return Some(text.chars().take(240).collect());
    }
    Some(trimmed.to_string())
}

fn unwrap_agent_message_event(line: &str) -> Option<String> {
    // Indexed previews are often truncated to 240 chars, so full JSON parse
    // fails on the majority of live hits. Prefer a structural parse when the
    // line is complete; fall back to a bounded `"text":"..."` scrape that
    // still works on truncated agent_message envelopes.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
        let ty = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ty == "item.completed" {
            if let Some(item) = value.get("item") {
                let item_ty = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if matches!(item_ty, "agent_message" | "message") {
                    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                        return Some(text.to_string());
                    }
                }
            }
        } else if matches!(ty, "agent_message" | "message") {
            if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
                return Some(text.to_string());
            }
        }
    }
    scrape_agent_message_text_field(line)
}

/// Best-effort extract of the human `text` field from a (possibly truncated)
/// vibecrafted `item.completed` / `agent_message` JSON line.
fn scrape_agent_message_text_field(line: &str) -> Option<String> {
    let looks_like_agent = line.contains("\"agent_message\"")
        || line.contains("\"type\":\"message\"")
        || line.contains("\"type\": \"message\"");
    if !looks_like_agent {
        return None;
    }
    // Locate `"text":"` (with optional space) and take until the next unescaped
    // quote or end-of-line. Truncated previews rarely close the string.
    let markers = ["\"text\":\"", "\"text\": \""];
    let start = markers
        .iter()
        .find_map(|marker| line.find(marker).map(|idx| idx + marker.len()))?;
    let rest = &line[start..];
    let mut out = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                match next {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            continue;
        }
        if ch == '"' {
            break;
        }
        out.push(ch);
        if out.chars().count() >= 240 {
            break;
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn is_thought_token_or_event_noise_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Compact JSON thought tokens from vibecrafted runtime_runs.
    if trimmed.contains("\"type\":\"thought\"") || trimmed.contains("\"type\": \"thought\"") {
        return true;
    }
    // Generic event envelopes with no human prose (thread.started, turn.*, item.started).
    if trimmed.starts_with('{')
        && (trimmed.contains("\"type\":\"thread.")
            || trimmed.contains("\"type\":\"turn.")
            || trimmed.contains("\"type\":\"item.started\"")
            || trimmed.contains("\"type\": \"thread.")
            || trimmed.contains("\"type\": \"turn.")
            || trimmed.contains("\"type\": \"item.started\""))
    {
        return true;
    }
    // item.completed error / step shells without agent_message body.
    if trimmed.starts_with('{')
        && (trimmed.contains("\"type\":\"item.completed\"")
            || trimmed.contains("\"type\": \"item.completed\""))
        && !trimmed.contains("\"agent_message\"")
        && !trimmed.contains("\"type\":\"message\"")
    {
        return true;
    }
    // Single-token thought fragments: {"type":"thought","data":"The"}
    if trimmed.starts_with('{')
        && trimmed.contains("\"type\"")
        && trimmed.contains("thought")
        && trimmed.contains("\"data\"")
        && trimmed.chars().count() < 120
    {
        return true;
    }
    false
}

/// Demote low-signal lexical hits and lightly boost query-term evidence in
/// the preview body. Runs after the recency prior so fresh noise still loses
/// to a slightly older readable answer.
fn apply_lexical_quality_prior(query: &str, results: &mut [FuzzyResult]) {
    let normalized_query = normalize_query(query);
    let query_terms: Vec<&str> = normalized_query
        .split_whitespace()
        .filter(|term| term.len() >= 3)
        .collect();
    for result in results.iter_mut() {
        if low_signal_semantic_result(result) {
            result.score = result.score.saturating_sub(30);
        }
        if thought_token_preview_ratio(result) >= 0.5 {
            result.score = result.score.saturating_sub(40);
        }
        if query_terms.is_empty() {
            continue;
        }
        let haystack = normalize_query(&result.matched_lines.join(" "));
        let matched = query_terms
            .iter()
            .filter(|term| haystack.contains(**term))
            .count();
        if matched > 0 {
            result.score = result
                .score
                .saturating_add((matched.saturating_mul(4).min(16)) as u8)
                .min(145);
        } else if !haystack.is_empty() {
            // Preview present but zero query terms → soft demotion so pure
            // recency cannot park an unrelated transcript on top.
            result.score = result.score.saturating_sub(8);
        }
    }
}

fn thought_token_preview_ratio(result: &FuzzyResult) -> f32 {
    if result.matched_lines.is_empty() {
        return 0.0;
    }
    let noisy = result
        .matched_lines
        .iter()
        .filter(|line| is_thought_token_or_event_noise_line(line))
        .count();
    // After sanitize, ratio is usually 0; compute against original would be
    // better, but post-sanitize emptiness of remaining lines is handled by
    // low_signal. Keep ratio for any residual unfiltered noise patterns.
    noisy as f32 / result.matched_lines.len() as f32
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn try_semantic_search_native(
    query: &str,
    limit: usize,
    project_filter: Option<&str>,
    frame_kind_filter: Option<FrameKind>,
    kind_filter: Option<&str>,
    candidate_filters: Option<&SemanticSearchFilters>,
) -> std::result::Result<(SemanticOutcome, CandidateBoundary), SemanticError> {
    // Resolve + verify the committed index FIRST, BEFORE paying the
    // (potentially heavy) embedder bootstrap. On a host with no local index
    // (e.g. a read mirror, which serves semantic from a remote mesh host and
    // keeps `indexed/` empty by design) this makes `aicx search` / the MCP
    // `aicx_search` fail-fast with `IndexNotBuilt` WITHOUT loading the
    // embedder — so a client retrying a deterministically-missing index does
    // not pay a model/config bootstrap (the most expensive step) on every
    // call. Functionally identical to checking after; the order is what saves
    // the CPU.
    let project_index_path = crate::vector_index::index_path(project_filter).map_err(|err| {
        SemanticError::IndexNotBuilt {
            path: std::path::PathBuf::new(),
            reason: format!("could not resolve index path: {err}"),
            recommendation: "ensure $AICX_HOME (or $HOME) is writable, then run `aicx index`"
                .to_string(),
        }
    })?;
    let all_index_path = if project_filter.is_some() {
        crate::vector_index::index_path(None).map_err(|err| SemanticError::IndexNotBuilt {
            path: std::path::PathBuf::new(),
            reason: format!("could not resolve _all index path: {err}"),
            recommendation: "ensure $AICX_HOME (or $HOME) is writable, then run `aicx index`"
                .to_string(),
        })?
    } else {
        project_index_path.clone()
    };
    let scope = select_semantic_bucket_scope(project_filter, project_index_path, all_index_path)?;
    let path = scope.index_path.clone();

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

    // Index exists — NOW probe the embedder (the expensive bootstrap step).
    let mut engine = match crate::embedder::EmbeddingEngine::new() {
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

    // D-9: cap query length BEFORE the embedder is touched so a runaway
    // caller (CLI or MCP) cannot pin the tokenizer or POST a multi-MB
    // body to a remote endpoint. Mirrors the byte budget enforced inside
    // every embedder backend; surfaced here for a clean structured error.
    if query.len() > aicx_embeddings::MAX_EMBED_INPUT_BYTES {
        return Err(SemanticError::NoResults {
            path: path.clone(),
            scanned: 0,
            reason: format!(
                "query is {} bytes; semantic embedder rejects inputs over {} bytes",
                query.len(),
                aicx_embeddings::MAX_EMBED_INPUT_BYTES
            ),
            recommendation: "trim the query to a sentence or two; embeddings only need a short focus phrase to retrieve similar chunks".to_string(),
        });
    }

    let manifest_path =
        crate::vector_index::hybrid_manifest_path(scope.index_project).map_err(|err| {
            SemanticError::IndexCorrupt {
                path: path.clone(),
                reason: format!("could not resolve hybrid manifest path: {err}"),
                recommendation: "ensure $AICX_HOME (or $HOME) is writable, then run `aicx index`"
                    .to_string(),
            }
        })?;
    // Embed the query once — both the hybrid path and the dense-only
    // fallback need the query vector.
    let query_embedding = match engine.embed(query) {
        Ok(embedding) => embedding,
        Err(err) => {
            return Err(SemanticError::EmbedderUnavailable {
                reason: format!("semantic embedder could not encode query: {err}"),
                recommendation:
                    "check `aicx config show` for resolved backend; if unhealthy run `aicx doctor` \
                     then retry"
                        .to_string(),
            });
        }
    };

    let legacy_dense = candidate_filters.map(|f| f.legacy_dense).unwrap_or(false);

    let retrieval_filters = SemanticRetrievalFilters {
        kind: kind_filter,
        frame_kind: frame_kind_filter,
        project: scope.retrieval_project_filter,
        agent: candidate_filters.and_then(|filters| filters.agent.as_deref()),
        date: candidate_filters.and_then(candidate_exact_date),
        candidate_filters,
    };

    // Recovery is an explicit operator choice. It bypasses the versioned
    // hybrid generation entirely and truthfully reports the old primary
    // NDJSON reader as a degraded dense-only execution path.
    if legacy_dense {
        let backend_label = if scope.used_global_project_scope {
            BACKEND_SEMANTIC_LEGACY_DENSE_GLOBAL_SCOPED
        } else {
            BACKEND_SEMANTIC_LEGACY_DENSE
        };
        return query_dense_only_from_primary_filtered(
            &path,
            &query_embedding,
            embedder_dim,
            limit,
            retrieval_filters,
            &info.model_id,
            backend_label,
        )
        .map(|outcome| (outcome, CandidateBoundary::default()));
    }

    // A missing manifest means no hybrid generation was ever published, so
    // the committed primary index may serve an explicit dense-only fallback.
    // Once a manifest exists it is authoritative: stale/corrupt generation
    // state fails closed and never activates the old reader implicitly.
    let hybrid = if manifest_path.exists() {
        // A published manifest is authoritative. Any parse, binding, lexical,
        // or mmap failure is corruption/staleness and must escape as a typed
        // error; reading the old primary NDJSON here would hide a broken
        // generation behind stale data.
        load_hybrid_index(scope.index_project, &path, &info, &manifest_path)?
    } else {
        // Manifest was never committed — serve dense-only directly from the
        // primary committed index (already validated above: exists, correct
        // dimension, non-empty).
        return query_dense_only_from_primary_filtered(
            &path,
            &query_embedding,
            embedder_dim,
            limit,
            retrieval_filters,
            &info.model_id,
            if scope.used_global_project_scope {
                BACKEND_SEMANTIC_DENSE_ONLY_GLOBAL_SCOPED
            } else {
                BACKEND_SEMANTIC_DENSE_ONLY
            },
        )
        .map(|outcome| (outcome, CandidateBoundary::default()));
    };
    let manifest = hybrid.manifest().cloned();
    let filters = hybrid_filters(retrieval_filters);
    let extra_filter_active = candidate_filters.is_some_and(supported_candidate_filter_active);
    let query_result = match hybrid.query_hybrid_with_budget_and_filter(
        aicx_retrieve::HybridQueryInput {
            query_text: query,
            query_embedding: &query_embedding,
            filters,
            limit,
        },
        aicx_retrieve::DEFAULT_FILTER_REFILL_BUDGET,
        extra_filter_active,
        |metadata| {
            candidate_filters
                .is_none_or(|filters| semantic_candidate_metadata_matches(metadata, filters))
        },
    ) {
        Ok(result) => result,
        Err(err) => return Err(index_query_error(&path, err)),
    };
    let boundary = CandidateBoundary {
        examined: query_result.examined_count,
        saturated: query_result.retrieval_outcome.completeness == RetrievalCompleteness::Partial,
    };
    let hits = query_result.hits;

    let retrieval_status = manifest.as_ref().map(HybridRetrievalStatus::from);
    let scanned = boundary.examined;
    let results: Vec<FuzzyResult> = hits
        .into_iter()
        .take(limit)
        .map(|h| {
            let path = hit_path(&h);
            let score_pct = hybrid_score_pct(h.score);
            let matched_lines = semantic_preview_lines(&path);
            let label_backend = if scope.used_global_project_scope {
                BACKEND_HYBRID_RRF_GLOBAL_SCOPED
            } else {
                BACKEND_HYBRID_RRF
            };
            let label = format!("{label_backend}:{}", h.chunk_id);
            FuzzyResult {
                file: path.to_string_lossy().to_string(),
                path: path.to_string_lossy().to_string(),
                project: hit_metadata_string(&h, "project"),
                kind: hit_metadata_string(&h, "kind"),
                frame_kind: hit_metadata_optional_string(&h, "frame_kind"),
                agent: hit_metadata_string(&h, "agent"),
                date: hit_metadata_string(&h, "date"),
                timestamp: None,
                score: score_pct,
                label,
                density: h.score,
                matched_lines,
                session_id: hit_metadata_optional_string(&h, "session_id"),
                cwd: hit_metadata_optional_string(&h, "cwd"),
            }
        })
        .collect();

    Ok((
        SemanticOutcome {
            results,
            scanned,
            backend_label: if scope.used_global_project_scope {
                BACKEND_HYBRID_RRF_GLOBAL_SCOPED
            } else {
                BACKEND_HYBRID_RRF
            },
            model_id: info.model_id,
            retrieval_status,
        },
        boundary,
    ))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn load_hybrid_index(
    project_filter: Option<&str>,
    source_index_path: &std::path::Path,
    info: &crate::embedder::EmbeddingModelInfo,
    manifest_path: &std::path::Path,
) -> std::result::Result<aicx_retrieve::HybridIndex, SemanticError> {
    // The published manifest is the sole adapter resolver. Parse it before
    // opening either payload so malformed metadata cannot make readers touch
    // stale sibling artifacts.
    let manifest = aicx_retrieve::Manifest::read_from_path(manifest_path).map_err(|err| {
        SemanticError::RetrievalManifestStale {
            path: manifest_path.to_path_buf(),
            reason: format!("could not read retrieval manifest: {err}"),
            recommendation: "run `aicx index` to rebuild the hybrid retrieval bucket".to_string(),
        }
    })?;
    let manifest_dir = crate::vector_index::hybrid_index_dir(project_filter).map_err(|err| {
        SemanticError::RetrievalManifestStale {
            path: manifest_path.to_path_buf(),
            reason: format!("could not resolve hybrid index dir: {err}"),
            recommendation: "run `aicx index` to rebuild the hybrid retrieval bucket".to_string(),
        }
    })?;
    // Do NOT re-hash the multi-GB primary NDJSON on the search hot path —
    // that alone made global search hang for tens of seconds. Generation
    // bindings (lexical commit, dense count/kind, embedder fingerprint)
    // still validate the published CURRENT generation.
    let _ = source_index_path;
    let lexical = Box::new(
        aicx_retrieve::TantivyAdapter::new(manifest_dir.clone()).map_err(|err| {
            SemanticError::RetrievalManifestStale {
                path: manifest_path.to_path_buf(),
                reason: format!("could not open hybrid lexical artifact: {err:#}"),
                recommendation: "run `aicx index` to rebuild the committed hybrid artifacts"
                    .to_string(),
            }
        })?,
    );

    let dense: Box<dyn aicx_retrieve::DenseIndex> = match manifest.dense_kind.as_str() {
        aicx_retrieve::MMAP_DENSE_KIND => {
            let mmap_path =
                crate::vector_index::hybrid_dense_mmap_path(project_filter).map_err(|err| {
                    SemanticError::RetrievalManifestStale {
                        path: manifest_path.to_path_buf(),
                        reason: format!("could not resolve hybrid dense mmap path: {err}"),
                        recommendation: "run `aicx index` to rebuild the hybrid retrieval bucket"
                            .to_string(),
                    }
                })?;
            if !mmap_path.exists() {
                return Err(SemanticError::RetrievalManifestStale {
                    path: manifest_path.to_path_buf(),
                    reason: format!(
                        "hybrid dense mmap artifact is missing at {}",
                        mmap_path.display()
                    ),
                    recommendation: "run `aicx index` to rebuild the committed hybrid artifacts"
                        .to_string(),
                });
            }
            let expected_distance = match manifest.embedder_distance.as_str() {
                "cosine" => aicx_retrieve::Distance::Cosine,
                "euclidean" => aicx_retrieve::Distance::Euclidean,
                "dot" => aicx_retrieve::Distance::Dot,
                other => {
                    return Err(SemanticError::RetrievalManifestStale {
                        path: manifest_path.to_path_buf(),
                        reason: format!("unknown embedder distance in manifest: {other}"),
                        recommendation: "run `aicx index` to rebuild the hybrid retrieval bucket"
                            .to_string(),
                    });
                }
            };
            let expected_hash = aicx_retrieve::decode_source_hash_blake3(
                &manifest.source_hash_blake3,
            )
            .map_err(|err| SemanticError::RetrievalManifestStale {
                path: manifest_path.to_path_buf(),
                reason: format!("could not decode source hash: {err}"),
                recommendation: "run `aicx index` to rebuild the hybrid retrieval bucket"
                    .to_string(),
            })?;
            Box::new(
                aicx_retrieve::MmapDenseAdapter::open(
                    &mmap_path,
                    info.dimension,
                    expected_distance,
                    Some(expected_hash),
                )
                .map_err(|err| SemanticError::RetrievalManifestStale {
                    path: manifest_path.to_path_buf(),
                    reason: format!("could not open hybrid dense mmap artifact: {err:#}"),
                    recommendation: "run `aicx index` to rebuild the committed hybrid artifacts"
                        .to_string(),
                })?,
            )
        }
        aicx_retrieve::BRUTE_FORCE_KIND => {
            return Err(SemanticError::RetrievalManifestStale {
                path: manifest_path.to_path_buf(),
                reason: "published hybrid manifest names the retired NDJSON dense adapter"
                    .to_string(),
                recommendation: "run `aicx index` to publish an mmap generation, or explicitly use `aicx search --legacy-dense` for recovery"
                    .to_string(),
            });
        }
        other => {
            return Err(SemanticError::RetrievalManifestStale {
                path: manifest_path.to_path_buf(),
                reason: format!("unsupported dense index kind: {other}"),
                recommendation: "run `aicx index` to rebuild the committed hybrid artifacts"
                    .to_string(),
            });
        }
    };

    let fusion = Box::new(aicx_retrieve::ReciprocalRankFusion::default());
    let fingerprint = crate::vector_index::hybrid_embedder_fingerprint(info);
    aicx_retrieve::HybridIndex::load_from_manifest(
        lexical,
        dense,
        fusion,
        manifest_dir,
        fingerprint,
        None,
    )
    .map_err(|err| SemanticError::RetrievalManifestStale {
        path: manifest_path.to_path_buf(),
        reason: format!("hybrid retrieval manifest does not match live artifacts: {err:#}"),
        recommendation:
            "run `aicx index` to rebuild lexical+dense artifacts from the canonical corpus"
                .to_string(),
    })
}

/// Dense-only semantic fallback that reads the PRIMARY committed index
/// (`index_path` = `vector_index::index_path`, i.e. `indexed/<bucket>/embeddings.ndjson`)
/// directly, bypassing the hybrid manifest + tantivy lexical layer entirely.
///
/// Engaged only when no hybrid manifest has been published, or when the
/// operator explicitly selects `--legacy-dense`. A present but invalid
/// generation fails closed instead of reading stale primary vectors.
///
/// This is NOT the doctrinal "silent fuzzy fallback" (see module docs): it is
/// explicit semantic search over real embeddings, surfaced via
/// `backend_label = "semantic_dense_only"` or `"semantic_legacy_dense"`.
#[cfg(all(any(feature = "native-embedder", feature = "cloud-embedder"), test))]
fn query_dense_only_from_primary(
    index_path: &std::path::Path,
    query_embedding: &[f32],
    dim: usize,
    limit: usize,
    filters: SemanticRetrievalFilters<'_>,
    model_id: &str,
    used_global_project_scope: bool,
) -> std::result::Result<SemanticOutcome, SemanticError> {
    query_dense_only_from_primary_filtered(
        index_path,
        query_embedding,
        dim,
        limit,
        filters,
        model_id,
        if used_global_project_scope {
            BACKEND_SEMANTIC_DENSE_ONLY_GLOBAL_SCOPED
        } else {
            BACKEND_SEMANTIC_DENSE_ONLY
        },
    )
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn query_dense_only_from_primary_filtered(
    index_path: &std::path::Path,
    query_embedding: &[f32],
    dim: usize,
    limit: usize,
    filters: SemanticRetrievalFilters<'_>,
    model_id: &str,
    backend_label: &'static str,
) -> std::result::Result<SemanticOutcome, SemanticError> {
    use aicx_retrieve::{BruteForceAdapter, ChunkRef, DenseChunkRef, DenseIndex, Distance};

    let used_global_project_scope = backend_label.ends_with("_global_scoped");

    let (header, mut entries) = crate::vector_index::read_committed_index_entries_matching_project(
        index_path,
        filters.project,
    )
    .map_err(|err| SemanticError::IndexCorrupt {
        path: index_path.to_path_buf(),
        reason: format!("dense-only fallback could not read committed index: {err:#}"),
        recommendation: format!(
            "delete and rebuild: `rm -f {} && aicx index`",
            index_path.display()
        ),
    })?;

    // Defensive dimension guard: a committed index built with a different
    // embedder (operator's F2LLM 2048 -> qwen3 4096 migration) must NOT be
    // scored against the current query vector. Surface DimensionMismatch
    // rather than ranking meaningless cross-model cosine.
    if header.dimension != dim {
        return Err(SemanticError::DimensionMismatch {
            path: index_path.to_path_buf(),
            index_dim: header.dimension,
            embedder_dim: dim,
            reason: format!(
                "dense-only fallback: committed index dimension={} (model {}), current embedder dimension={}",
                header.dimension, header.model_id, dim
            ),
            recommendation: format!(
                "rebuild with the current embedder: `rm -f {} && aicx index`",
                index_path.display()
            ),
        });
    }

    if header.entry_count == 0 {
        return Err(SemanticError::EmptyIndex {
            path: index_path.to_path_buf(),
            reason: format!(
                "dense-only fallback: committed index at {} contains 0 entries",
                index_path.display()
            ),
            recommendation: "run `aicx extract --all` to populate the corpus, then `aicx index`"
                .to_string(),
        });
    }

    if let Some(candidate_filters) = filters.candidate_filters {
        entries.retain(|entry| {
            let metadata = crate::vector_index::index_entry_metadata_json(entry);
            semantic_candidate_metadata_matches(&metadata, candidate_filters)
        });
    }

    if entries.is_empty() {
        return Ok(SemanticOutcome {
            results: Vec::new(),
            scanned: 0,
            backend_label,
            model_id: model_id.to_string(),
            retrieval_status: None,
        });
    }

    let scanned = entries.len();
    let dense_chunks: Vec<DenseChunkRef> = entries
        .into_iter()
        .map(|entry| {
            let metadata = crate::vector_index::index_entry_metadata_json(&entry);
            DenseChunkRef {
                chunk: ChunkRef {
                    id: entry.id,
                    source_path: entry.path.to_string_lossy().to_string(),
                    // Dense ranking scores embeddings, not text — keep the
                    // body out of memory (227k chunks otherwise).
                    text: String::new(),
                    metadata,
                },
                embedding: entry.embedding,
            }
        })
        .collect();

    let mut dense = BruteForceAdapter::new(dim).with_distance(Distance::Cosine);
    DenseIndex::build(&mut dense, &dense_chunks).map_err(|err| SemanticError::IndexCorrupt {
        path: index_path.to_path_buf(),
        reason: format!("dense-only fallback could not build in-memory dense index: {err:#}"),
        recommendation: format!(
            "delete and rebuild: `rm -f {} && aicx index`",
            index_path.display()
        ),
    })?;

    let filter_set = hybrid_filters(filters);
    let hits = DenseIndex::query(&dense, query_embedding, limit, &filter_set).map_err(|err| {
        SemanticError::IndexCorrupt {
            path: index_path.to_path_buf(),
            reason: format!("dense-only fallback query failed: {err:#}"),
            recommendation: "retry; if it persists rebuild with `aicx index`".to_string(),
        }
    })?;

    let results: Vec<FuzzyResult> = hits
        .into_iter()
        .map(|h| {
            let path = hit_path(&h);
            let score_pct = dense_score_pct(h.score);
            let matched_lines = semantic_preview_lines(&path);
            let label = if used_global_project_scope {
                format!("dense_only_global_scoped:{}", h.chunk_id)
            } else {
                format!("dense_only:{}", h.chunk_id)
            };
            FuzzyResult {
                file: path.to_string_lossy().to_string(),
                path: path.to_string_lossy().to_string(),
                project: hit_metadata_string(&h, "project"),
                kind: hit_metadata_string(&h, "kind"),
                frame_kind: hit_metadata_optional_string(&h, "frame_kind"),
                agent: hit_metadata_string(&h, "agent"),
                date: hit_metadata_string(&h, "date"),
                timestamp: None,
                score: score_pct,
                label,
                density: h.score,
                matched_lines,
                session_id: hit_metadata_optional_string(&h, "session_id"),
                cwd: hit_metadata_optional_string(&h, "cwd"),
            }
        })
        .collect();

    Ok(SemanticOutcome {
        results,
        scanned,
        backend_label,
        model_id: model_id.to_string(),
        retrieval_status: None,
    })
}

/// Map a brute-force Cosine **similarity** (`[-1.0, 1.0]`, higher = closer)
/// to a `[0, 100]` percentage. Distinct from [`hybrid_score_pct`], which
/// scales an RRF-fused score — the dense-only leg is not RRF-fused, so it
/// must not borrow that scaling.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn dense_score_pct(cosine_similarity: f32) -> u8 {
    let clamped = cosine_similarity.clamp(-1.0, 1.0);
    (((clamped + 1.0) * 0.5 * 100.0).round() as i32).clamp(0, 100) as u8
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn hybrid_filters(filters: SemanticRetrievalFilters<'_>) -> aicx_retrieve::FilterSet {
    let mut set = aicx_retrieve::FilterSet::default();
    if let Some(project) = filters.project {
        set.values.insert(
            "project".to_string(),
            serde_json::Value::String(project.to_string()),
        );
    }
    if let Some(kind) = filters.kind {
        set.values.insert(
            "kind".to_string(),
            serde_json::Value::String(kind.to_string()),
        );
    }
    if let Some(frame_kind) = filters.frame_kind {
        set.values.insert(
            "frame_kind".to_string(),
            serde_json::Value::String(frame_kind.as_str().to_string()),
        );
    }
    if let Some(agent) = filters.agent {
        set.values.insert(
            "agent".to_string(),
            serde_json::Value::String(agent.to_string()),
        );
    }
    if let Some(date) = filters.date {
        set.values.insert(
            "date".to_string(),
            serde_json::Value::String(date.replace('-', "")),
        );
    }
    set
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn hit_path(hit: &aicx_retrieve::Hit) -> std::path::PathBuf {
    hit.metadata
        .get("source_path")
        .and_then(serde_json::Value::as_str)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(&hit.chunk_id))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn hit_metadata_string(hit: &aicx_retrieve::Hit, key: &str) -> String {
    hit_metadata_optional_string(hit, key).unwrap_or_else(|| "-".to_string())
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn hit_metadata_optional_string(hit: &aicx_retrieve::Hit, key: &str) -> Option<String> {
    match hit.metadata.get(key)? {
        serde_json::Value::String(value) if !value.is_empty() => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn hit_metadata_lines(hit: &aicx_retrieve::Hit, key: &str) -> Vec<String> {
    hit.metadata
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect()
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn hybrid_score_pct(score: f32) -> u8 {
    let max_rrf = 2.0 / (aicx_retrieve::RRF_K_DEFAULT as f32 + 1.0);
    ((score.max(0.0) / max_rrf * 100.0).round() as u8).min(100)
}

impl From<&aicx_retrieve::Manifest> for HybridRetrievalStatus {
    fn from(manifest: &aicx_retrieve::Manifest) -> Self {
        Self {
            generation_id: manifest.generation_id.clone(),
            source_chunk_count: manifest.source_chunk_count,
            dense_count: manifest.dense_count,
            lexical_doc_count: manifest.lexical_doc_count,
            fusion_algorithm: manifest.fusion_algorithm.clone(),
            dense_kind: manifest.dense_kind.clone(),
        }
    }
}

/// Map an executed semantic outcome onto the typed [`RetrievalOutcome`].
/// Evidence-only mapping: a dense-only backend label maps to the dense leg,
/// a hybrid manifest maps to fused execution, and a semantic success with
/// neither (legacy vector scan) maps to explicit unknown — never healthy.
pub fn semantic_retrieval_outcome(
    backend_label: &str,
    retrieval_status: Option<&HybridRetrievalStatus>,
    examined_count: usize,
    matched_count: usize,
    stale_evidence: bool,
) -> RetrievalOutcome {
    let legacy_dense = backend_label.starts_with(BACKEND_SEMANTIC_LEGACY_DENSE);
    let dense_only = backend_label.starts_with(BACKEND_SEMANTIC_DENSE_ONLY);
    let lexical = backend_label.starts_with(BACKEND_LEXICAL);
    let (executed_path, fallback_reason, requested_mode) = if legacy_dense {
        (
            Some(ExecutedPath::DenseOnly),
            Some(
                "operator_selected_legacy_dense: served brute-force cosine from the committed primary NDJSON index"
                    .to_string(),
            ),
            RequestedMode::Hybrid,
        )
    } else if dense_only {
        (
            Some(ExecutedPath::DenseOnly),
            Some(
                "hybrid_unavailable: lexical fusion leg missing or stale; served dense-only \
                 cosine from the committed primary index"
                    .to_string(),
            ),
            RequestedMode::Hybrid,
        )
    } else if lexical {
        (
            Some(ExecutedPath::LexicalOnly),
            None,
            RequestedMode::Lexical,
        )
    } else if retrieval_status.is_some() {
        (
            Some(ExecutedPath::HybridFusion),
            None,
            RequestedMode::Hybrid,
        )
    } else {
        (
            None,
            Some(format!(
                "execution_evidence_missing: semantic backend '{backend_label}' returned no \
                 hybrid manifest evidence"
            )),
            RequestedMode::Hybrid,
        )
    };
    RetrievalOutcome::from_evidence(
        requested_mode,
        RetrievalEvidence {
            executed_path,
            examined_count: Some(examined_count),
            matched_count,
            fallback_reason,
            stale_evidence,
        },
    )
}

impl SemanticSearchOutcome {
    /// Typed execution status for this outcome. See [`semantic_retrieval_outcome`].
    pub fn retrieval_outcome(
        &self,
        matched_count: usize,
        stale_evidence: bool,
    ) -> RetrievalOutcome {
        semantic_retrieval_outcome(
            self.backend_label,
            self.retrieval_status.as_ref(),
            self.scanned,
            matched_count,
            stale_evidence,
        )
    }
}

/// Typed execution status for the lexical/filesystem leg. A present
/// `semantic_fallback_reason` marks a degraded fallback out of a failed
/// hybrid request; `None` marks an operator-requested lexical run.
pub fn lexical_retrieval_outcome(
    semantic_fallback_reason: Option<String>,
    examined_count: usize,
    matched_count: usize,
    stale_evidence: bool,
) -> RetrievalOutcome {
    let requested_mode = if semantic_fallback_reason.is_some() {
        RequestedMode::Hybrid
    } else {
        RequestedMode::Lexical
    };
    RetrievalOutcome::from_evidence(
        requested_mode,
        RetrievalEvidence {
            executed_path: Some(ExecutedPath::LexicalOnly),
            examined_count: Some(examined_count),
            matched_count,
            fallback_reason: semantic_fallback_reason,
            stale_evidence,
        },
    )
}

/// Single owner mapping the typed [`RetrievalOutcome`] onto an
/// [`crate::oracle::OracleStatus`] for every search JSON surface (CLI
/// search/evidence, MCP search/evidence). No caller may bucket on manifest
/// presence or backend label strings again.
pub fn search_oracle_status_from_retrieval(
    store_root: &Path,
    retrieval: &RetrievalOutcome,
    hybrid_status: Option<&HybridRetrievalStatus>,
    candidate_count: usize,
    source_paths_verified: bool,
) -> crate::oracle::OracleStatus {
    use crate::oracle::OracleStatus;
    let fallback_reason = || {
        retrieval
            .fallback_reason
            .clone()
            .unwrap_or_else(|| aicx_retrieve::FALLBACK_REASON_EVIDENCE_MISSING.to_string())
    };
    let status = match (retrieval.executed_path, hybrid_status) {
        (ExecutedPath::HybridFusion, Some(hybrid)) => {
            OracleStatus::hybrid_rrf(store_root, hybrid, candidate_count, source_paths_verified)
        }
        // Fusion without manifest evidence should be unreachable; fail closed
        // instead of synthesizing a healthy hybrid claim.
        (ExecutedPath::HybridFusion, None) | (ExecutedPath::None, _) => {
            OracleStatus::retrieval_unknown(
                store_root,
                retrieval.examined_count,
                candidate_count,
                fallback_reason(),
            )
        }
        (ExecutedPath::DenseOnly, _) => OracleStatus::semantic_dense_only(
            store_root,
            retrieval.examined_count,
            candidate_count,
            source_paths_verified,
            fallback_reason(),
        ),
        (ExecutedPath::LexicalOnly, hybrid) => {
            // Intentional lexical-first (default) always carries a hybrid
            // generation status when the Tantivy leg answered. True
            // filesystem-fuzzy (no generation / semantic failed closed) has
            // no hybrid status and usually a fallback_reason — keep that
            // labeled fuzzy so oracle honesty is not re-written as index.
            match hybrid {
                Some(_) => OracleStatus::lexical_tantivy(
                    store_root,
                    retrieval.examined_count,
                    candidate_count,
                    source_paths_verified,
                    hybrid,
                ),
                None if retrieval.fallback_reason.is_none()
                    && retrieval.requested_mode == aicx_retrieve::RequestedMode::Lexical =>
                {
                    OracleStatus::lexical_tantivy(
                        store_root,
                        retrieval.examined_count,
                        candidate_count,
                        source_paths_verified,
                        None,
                    )
                }
                None => OracleStatus::filesystem_fuzzy(
                    store_root,
                    retrieval.examined_count,
                    candidate_count,
                    source_paths_verified,
                ),
            }
        }
    };
    status.with_retrieval(retrieval.clone())
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn merge_hybrid_statuses(statuses: &[HybridRetrievalStatus]) -> Option<HybridRetrievalStatus> {
    match statuses {
        [] => None,
        [status] => Some(status.clone()),
        many => Some(HybridRetrievalStatus {
            generation_id: "multiple".to_string(),
            source_chunk_count: many.iter().map(|status| status.source_chunk_count).sum(),
            dense_count: many.iter().map(|status| status.dense_count).sum(),
            lexical_doc_count: many.iter().map(|status| status.lexical_doc_count).sum(),
            fusion_algorithm: many
                .first()
                .map(|status| status.fusion_algorithm.clone())
                .unwrap_or_else(|| "rrf".to_string()),
            dense_kind: if many
                .iter()
                .all(|status| status.dense_kind == many[0].dense_kind)
            {
                many[0].dense_kind.clone()
            } else {
                "mixed".to_string()
            },
        }),
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn read_index_header(path: &std::path::Path) -> Option<crate::vector_index::IndexHeader> {
    use std::io::BufReader;
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let first =
        crate::sanitize::read_line_capped(&mut reader, crate::sanitize::MAX_VALIDATED_BYTES)
            .ok()??;
    if first.exceeded {
        return None;
    }
    serde_json::from_str(first.line.trim()).ok()
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn index_appears_empty(path: &std::path::Path) -> bool {
    use std::io::BufReader;
    let Ok(file) = std::fs::File::open(path) else {
        return true;
    };
    let mut reader = BufReader::new(file);
    // Skip header (first line); if no second line, index is empty.
    if crate::sanitize::read_line_capped(&mut reader, crate::sanitize::MAX_VALIDATED_BYTES)
        .ok()
        .flatten()
        .is_none()
    {
        return true;
    }
    loop {
        match crate::sanitize::read_line_capped(&mut reader, crate::sanitize::MAX_VALIDATED_BYTES) {
            Ok(Some(line)) if !line.line.trim().is_empty() => return false,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return true,
        }
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn semantic_preview_lines(path: &std::path::Path) -> Vec<String> {
    const MAX_LINES: usize = 6;
    let Ok(content) = crate::sanitize::read_to_string_validated(path) else {
        return Vec::new();
    };
    crate::card_header::card_body(&content)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !crate::card_header::is_bracket_header_line(line))
        .filter(|line| *line != "[signals]" && *line != "[/signals]")
        .filter(|line| !line.starts_with("id: "))
        .take(MAX_LINES)
        .map(|line| line.strip_prefix("content: ").unwrap_or(line).to_string())
        .collect()
}

/// Caller-supplied filters applied INSIDE the retrieval primitive by
/// [`try_semantic_search_filtered`]. Previously the CLI / MCP / dashboard
/// paths fetched a top-N pool from the index and applied these checks
/// afterward, so a corpus whose top-N all sat outside the user's filter
/// window returned a silent empty result even when valid hits existed
/// further down the ranking. Pushing the filters into the retrieval
/// wrapper closes that gap.
#[derive(Debug, Default, Clone)]
pub struct SemanticSearchFilters {
    /// Exact-match agent slug (e.g. `"claude"`, `"codex"`).
    pub agent: Option<String>,
    /// Minimum chunk score on the canonical 0..=100 scale.
    pub score_min: Option<u8>,
    /// Inclusive lower date bound, day granularity (`YYYY-MM-DD`).
    pub date_lo: Option<String>,
    /// Inclusive upper date bound, day granularity (`YYYY-MM-DD`).
    pub date_hi: Option<String>,
    /// Pre-computed `YYYY-MM-DD` lower bound derived from `--hours`,
    /// applied only when neither `date_lo` nor `date_hi` is set so the
    /// explicit date filter wins (matching legacy precedence).
    pub hours_cutoff: Option<String>,
    /// Use legacy NDJSON reader for dense vector search instead of versioned mmap.
    pub legacy_dense: bool,
    /// When true, run dense re-rank (hybrid RRF). Default false is lexical-first
    /// over the published `_all` CURRENT tantivy generation with a recency prior.
    pub deep: bool,
}

impl SemanticSearchFilters {
    /// True iff at least one post-fetch filter is active.
    pub fn is_active(&self) -> bool {
        self.agent.is_some()
            || self.score_min.is_some()
            || self.date_lo.is_some()
            || self.date_hi.is_some()
            || self.hours_cutoff.is_some()
    }
}

fn supported_candidate_filter_active(filters: &SemanticSearchFilters) -> bool {
    filters.agent.is_some()
        || filters.date_lo.is_some()
        || filters.date_hi.is_some()
        || filters.hours_cutoff.is_some()
}

fn candidate_exact_date(filters: &SemanticSearchFilters) -> Option<&str> {
    match (filters.date_lo.as_deref(), filters.date_hi.as_deref()) {
        (Some(lo), Some(hi)) if lo == hi => Some(lo),
        _ => None,
    }
}

fn semantic_candidate_metadata_matches(
    metadata: &serde_json::Value,
    filters: &SemanticSearchFilters,
) -> bool {
    if let Some(agent) = filters.agent.as_deref()
        && metadata.get("agent").and_then(serde_json::Value::as_str) != Some(agent)
    {
        return false;
    }

    let date_filter_active =
        filters.date_lo.is_some() || filters.date_hi.is_some() || filters.hours_cutoff.is_some();
    if !date_filter_active {
        return true;
    }
    let Some(raw_date) = metadata.get("date").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let date = raw_date.replace('-', "");
    let normalize = |value: &str| value.replace('-', "");

    if filters.date_lo.is_some() || filters.date_hi.is_some() {
        filters
            .date_lo
            .as_deref()
            .is_none_or(|lo| date >= normalize(lo))
            && filters
                .date_hi
                .as_deref()
                .is_none_or(|hi| date <= normalize(hi))
    } else {
        filters
            .hours_cutoff
            .as_deref()
            .is_none_or(|cutoff| date >= normalize(cutoff))
    }
}

/// Diagnostic emitted when filter pushdown examined the full bounded
/// pool but still failed to satisfy the user's `limit`. The presence of
/// this payload tells the caller "we ran out of candidates inside the
/// cap, not because the underlying corpus is empty." Surfaced to JSON +
/// stderr so operators can decide whether to widen filters or raise the
/// cap rather than misreading the partial set as "nothing exists."
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FilterPushdownDiagnostic {
    pub kind: &'static str,
    pub examined: usize,
    pub matched: usize,
    pub requested_limit: usize,
    pub examined_cap_ratio: usize,
}

/// Result of [`try_semantic_search_filtered`]. `outcome.results` carries
/// the FILTERED candidate set (NOT truncated to user `limit`) so the
/// caller can apply its own sort order before taking the final slice.
#[derive(Debug)]
pub struct FilteredSemanticOutcome {
    pub outcome: SemanticSearchOutcome,
    pub diagnostic: Option<FilterPushdownDiagnostic>,
}

/// Merge an optional `filter_pushdown` diagnostic into a JSON payload
/// rendered by `rank::render_search_json_with_oracle` (or any
/// equivalent caller). Additive shape: callers that ignore the field
/// see the canonical search response untouched.
///
/// Shared between `aicx search` (CLI, `src/main.rs`) and `aicx_search`
/// (MCP, `src/mcp.rs`) so both surfaces emit byte-identical diagnostic
/// JSON.
pub fn inject_filter_pushdown_diagnostic(
    rendered: &str,
    diagnostic: Option<&FilterPushdownDiagnostic>,
) -> Result<String, serde_json::Error> {
    let Some(diag) = diagnostic else {
        return Ok(rendered.to_string());
    };
    let mut value: serde_json::Value = serde_json::from_str(rendered)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("filter_pushdown".to_string(), serde_json::to_value(diag)?);
    }
    serde_json::to_string(&value)
}

/// Bounded examined-pool ratio. The retrieval wrapper fetches at most
/// `max(user_limit * RATIO, MIN)` candidates from the hybrid index, then
/// applies post-filters across that pool. Anything beyond the cap is
/// considered "underlying pool exhausted" relative to this query, even
/// if the corpus has more chunks — the bound exists so a very tight
/// filter cannot pin the embedder / index against a multi-million-chunk
/// store.
pub const FILTER_EXAMINED_CAP_RATIO: usize = 10;
/// Floor for the examined pool size so tiny user limits (e.g. `--limit 1`)
/// still have enough material for filter pushdown to be meaningful.
pub const FILTER_EXAMINED_CAP_MIN: usize = 50;

fn default_search_quality_active(frame_kind_filter: Option<FrameKind>) -> bool {
    frame_kind_filter.is_none()
}

fn semantic_fetch_limit(
    user_limit: usize,
    frame_kind_filter: Option<FrameKind>,
    post_filters: &SemanticSearchFilters,
) -> usize {
    if post_filters.is_active() || default_search_quality_active(frame_kind_filter) {
        user_limit
            .saturating_mul(FILTER_EXAMINED_CAP_RATIO)
            .max(FILTER_EXAMINED_CAP_MIN)
    } else {
        user_limit.max(1)
    }
}

/// Filter-aware semantic retrieval. Wraps [`try_semantic_search`] with a
/// bounded examined-pool fetch + canonical post-filter application so
/// CLI (`aicx search`) and MCP (`aicx_search`) share one truth for the
/// pushdown shape. Returns up to `examined_cap` filtered candidates,
/// already restricted to the filter window; the caller is responsible
/// for the final sort + `take(user_limit)`.
///
/// When filters are active and the wrapper exhausted the cap without
/// satisfying `user_limit`, the diagnostic flags `filter_yielded_partial`
/// so the caller can surface the situation to operators instead of
/// rendering a misleading silent empty.
pub fn try_semantic_search_filtered(
    store_root: &Path,
    query: &str,
    user_limit: usize,
    project_filters: &[Option<&str>],
    frame_kind_filter: Option<FrameKind>,
    kind_filter: Option<&str>,
    post_filters: &SemanticSearchFilters,
) -> std::result::Result<FilteredSemanticOutcome, SemanticError> {
    let fetch_limit = semantic_fetch_limit(user_limit, frame_kind_filter, post_filters);

    let (outcome, candidate_boundary) = try_semantic_search_with_boundary(
        store_root,
        query,
        fetch_limit,
        project_filters,
        frame_kind_filter,
        kind_filter,
        Some(post_filters),
    )?;

    let SemanticSearchOutcome {
        results,
        scanned,
        backend_label,
        model_id,
        retrieval_status,
    } = outcome;

    let examined = results.len();
    let quality_filtered = apply_default_semantic_quality(results, query, frame_kind_filter);
    let filtered = apply_semantic_post_filters(quality_filtered, post_filters);
    let matched = filtered.len();
    let diagnostic = partial_pushdown_diagnostic(
        post_filters.is_active() || default_search_quality_active(frame_kind_filter),
        examined,
        matched,
        user_limit,
        fetch_limit,
    )
    .or_else(|| {
        candidate_boundary
            .saturated
            .then_some(FilterPushdownDiagnostic {
                kind: "filter_yielded_partial",
                examined: candidate_boundary.examined,
                matched,
                requested_limit: user_limit,
                examined_cap_ratio: FILTER_EXAMINED_CAP_RATIO,
            })
    });

    Ok(FilteredSemanticOutcome {
        outcome: SemanticSearchOutcome {
            results: filtered,
            scanned,
            backend_label,
            model_id,
            retrieval_status,
        },
        diagnostic,
    })
}

fn apply_default_semantic_quality(
    mut results: Vec<FuzzyResult>,
    query: &str,
    frame_kind_filter: Option<FrameKind>,
) -> Vec<FuzzyResult> {
    if frame_kind_filter.is_none() {
        results.retain(default_visible_frame);
    }

    for result in &mut results {
        result.score = semantic_quality_score(query, result);
    }
    results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));
    dedupe_semantic_results(results)
}

fn default_visible_frame(result: &FuzzyResult) -> bool {
    match result.frame_kind.as_deref().and_then(FrameKind::parse) {
        Some(FrameKind::UserMsg | FrameKind::AgentReply) | None => true,
        Some(FrameKind::InternalThought | FrameKind::ToolCall | FrameKind::SystemNote) => false,
    }
}

fn semantic_quality_score(query: &str, result: &FuzzyResult) -> u8 {
    let mut score = result.score as i16;
    score += match result.frame_kind.as_deref().and_then(FrameKind::parse) {
        Some(FrameKind::UserMsg) => 6,
        Some(FrameKind::AgentReply) => 5,
        None => 1,
        Some(FrameKind::InternalThought | FrameKind::ToolCall | FrameKind::SystemNote) => -20,
    };

    let normalized_query = normalize_query(query);
    let query_terms: Vec<&str> = normalized_query
        .split_whitespace()
        .filter(|term| term.len() >= 3)
        .collect();
    let haystack = semantic_result_haystack(result);
    let anchors = query_anchors(&normalized_query);
    if !normalized_query.is_empty() && haystack.contains(&normalized_query) {
        score += 12;
    }
    score += anchor_quality_delta(&anchors, &haystack);
    score += informative_agent_reply_delta(result, &anchors, &haystack);
    if !query_terms.is_empty() {
        let matched_terms = query_terms
            .iter()
            .filter(|term| haystack.contains(**term))
            .count();
        score += (matched_terms.saturating_mul(3).min(12)) as i16;
        if matched_terms == 0 {
            score -= 15;
        }
    }
    if low_signal_semantic_result(result) {
        score -= 8;
    }

    score.clamp(0, 100) as u8
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QueryAnchor {
    token: String,
    parts: Vec<String>,
    strong: bool,
}

fn query_anchors(normalized_query: &str) -> Vec<QueryAnchor> {
    let mut seen = HashSet::new();
    let mut anchors = Vec::new();

    for raw_token in normalized_query.split_whitespace() {
        let token = raw_token
            .trim_matches(|ch: char| !is_anchor_char(ch))
            .to_string();
        if token.len() < 3 || !seen.insert(token.clone()) {
            continue;
        }

        let parts = split_anchor_parts(&token);
        let has_separator = token.chars().any(is_anchor_separator);
        let has_digit = token.chars().any(|ch| ch.is_ascii_digit());
        let strong = token.len() >= 5 && ((has_separator && parts.len() >= 2) || has_digit);

        anchors.push(QueryAnchor {
            token,
            parts,
            strong,
        });
    }

    anchors
}

fn is_anchor_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/' | ':' | '.')
}

fn is_anchor_separator(ch: char) -> bool {
    matches!(ch, '-' | '_' | '/' | ':' | '.')
}

fn split_anchor_parts(token: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    token
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| part.len() >= 3)
        .filter_map(|part| {
            let part = part.to_string();
            seen.insert(part.clone()).then_some(part)
        })
        .collect()
}

fn anchor_quality_delta(anchors: &[QueryAnchor], haystack: &str) -> i16 {
    let strong_anchors: Vec<&QueryAnchor> = anchors.iter().filter(|anchor| anchor.strong).collect();
    if strong_anchors.is_empty() {
        return 0;
    }

    let mut exact_matches = 0usize;
    let mut full_part_matches = 0usize;
    let mut matched_parts = HashSet::new();

    for anchor in strong_anchors {
        if haystack.contains(&anchor.token) {
            exact_matches += 1;
        }

        let matched_count = anchor
            .parts
            .iter()
            .filter(|part| {
                let matched = haystack.contains(part.as_str());
                if matched {
                    matched_parts.insert((*part).clone());
                }
                matched
            })
            .count();

        if anchor.parts.len() >= 2 && matched_count == anchor.parts.len() {
            full_part_matches += 1;
        }
    }

    let mut delta = 0i16;
    delta += (exact_matches.saturating_mul(28).min(56)) as i16;
    delta += (full_part_matches.saturating_mul(18).min(36)) as i16;
    delta += (matched_parts.len().saturating_mul(4).min(16)) as i16;

    if exact_matches == 0 && full_part_matches == 0 {
        if matched_parts.is_empty() {
            delta -= 30;
        } else {
            delta -= 8;
        }
    }

    delta
}

fn informative_agent_reply_delta(
    result: &FuzzyResult,
    anchors: &[QueryAnchor],
    haystack: &str,
) -> i16 {
    if result.frame_kind.as_deref().and_then(FrameKind::parse) != Some(FrameKind::AgentReply) {
        return 0;
    }
    if !has_strong_anchor_evidence(anchors, haystack) {
        return 0;
    }

    let text = normalize_query(&result.matched_lines.join(" "));
    let word_count = text
        .split_whitespace()
        .filter(|word| word.chars().filter(|ch| ch.is_ascii_alphabetic()).count() >= 3)
        .count();
    if word_count < 10 {
        return 0;
    }

    let answer_markers = [
        "to jest to",
        "chodzi o",
        "dlatego",
        "poniewaz",
        "przyczyna",
        "diagnoza",
        "wniosek",
        "incydent",
        "root cause",
        "because",
        "reason",
        "summary",
        "diagnosis",
        "conclusion",
    ];
    if answer_markers.iter().any(|marker| text.contains(marker)) {
        22
    } else if word_count >= 22 {
        8
    } else {
        0
    }
}

fn has_strong_anchor_evidence(anchors: &[QueryAnchor], haystack: &str) -> bool {
    anchors.iter().any(|anchor| {
        anchor.strong
            && (haystack.contains(&anchor.token)
                || (anchor.parts.len() >= 2
                    && anchor
                        .parts
                        .iter()
                        .all(|part| haystack.contains(part.as_str()))))
    })
}

fn semantic_result_haystack(result: &FuzzyResult) -> String {
    normalize_query(&format!(
        "{} {} {} {} {} {} {}",
        result.project,
        result.kind,
        result.frame_kind.as_deref().unwrap_or_default(),
        result.agent,
        result.date,
        result.path,
        result.matched_lines.join(" ")
    ))
}

fn low_signal_semantic_result(result: &FuzzyResult) -> bool {
    let joined = result.matched_lines.join(" ");
    let normalized = normalize_query(&joined);
    if normalized.trim().is_empty() {
        return true;
    }
    let noisy_needles = [
        "last_token_usage",
        "input_tokens",
        "reasoning_output_tokens",
        "model_context_window",
        "toolu_",
        "web_search",
        "open_page",
        "no matching deferred tools found",
        "type thought",
        "type thread started",
        "type turn started",
        "type item started",
        "type item completed",
        "skill descriptions were shortened",
    ];
    if noisy_needles
        .iter()
        .any(|needle| normalized.contains(needle))
    {
        return true;
    }
    // Raw vibecrafted token streams often survive as short JSON lines.
    if result
        .matched_lines
        .iter()
        .filter(|line| is_thought_token_or_event_noise_line(line))
        .count()
        * 2
        >= result.matched_lines.len().max(1)
    {
        return true;
    }
    let content_chars = normalized
        .chars()
        .filter(|ch| ch.is_ascii_alphabetic())
        .count();
    content_chars < 8
}

fn dedupe_semantic_results(results: Vec<FuzzyResult>) -> Vec<FuzzyResult> {
    let mut seen_snippets = HashSet::new();
    let mut deduped = Vec::with_capacity(results.len());
    for result in results {
        let snippet_key = normalize_query(&result.matched_lines.join("\n"));
        if snippet_key.len() >= 24
            && !seen_snippets.insert(format!(
                "{}|{}|{}",
                result.project,
                result.frame_kind.as_deref().unwrap_or("-"),
                snippet_key
            ))
        {
            continue;
        }
        deduped.push(result);
    }
    deduped
}

/// Pure helper for the post-fetch filter pass. Kept `pub(crate)` so unit
/// tests can drive it without spinning up the embedder + hybrid index.
pub(crate) fn apply_semantic_post_filters(
    mut results: Vec<FuzzyResult>,
    filters: &SemanticSearchFilters,
) -> Vec<FuzzyResult> {
    if let Some(min) = filters.score_min {
        results.retain(|r| r.score >= min);
    }
    if let Some(ref agent) = filters.agent {
        results.retain(|r| r.agent == *agent);
    }
    if filters.date_lo.is_some() || filters.date_hi.is_some() {
        let lo = filters.date_lo.as_deref();
        let hi = filters.date_hi.as_deref();
        results.retain(|r| {
            lo.is_none_or(|lo| r.date.as_str() >= lo) && hi.is_none_or(|hi| r.date.as_str() <= hi)
        });
    } else if let Some(ref cutoff) = filters.hours_cutoff {
        let cutoff = cutoff.as_str();
        results.retain(|r| r.date.as_str() >= cutoff);
    }
    results
}

/// Decide whether to emit a `filter_yielded_partial` diagnostic. Encoded
/// as a standalone helper so the decision rule is unit-testable without
/// reaching into the retrieval primitive.
pub(crate) fn partial_pushdown_diagnostic(
    filters_active: bool,
    examined: usize,
    matched: usize,
    user_limit: usize,
    fetch_limit: usize,
) -> Option<FilterPushdownDiagnostic> {
    if !filters_active {
        return None;
    }
    if matched >= user_limit {
        return None;
    }
    // We only flag "partial under cap" when the wrapper fetched the full
    // examined cap without satisfying the limit. A short pool (examined
    // < fetch_limit) means the index itself ran out of candidates, which
    // is a corpus-side signal — not a pushdown-cap signal.
    if examined < fetch_limit {
        return None;
    }
    Some(FilterPushdownDiagnostic {
        kind: "filter_yielded_partial",
        examined,
        matched,
        requested_limit: user_limit,
        examined_cap_ratio: FILTER_EXAMINED_CAP_RATIO,
    })
}

/// Compose the canonical `oracle_status` line emitted to stderr after a
/// search call. Health (prefix, index, fallback, scope safety) derives ONLY
/// from the typed [`RetrievalOutcome`]; the backend label contributes the
/// display name and the global-scope display token, never the health.
pub fn render_semantic_status_line(
    backend_label: &str,
    model_id: Option<&str>,
    retrieval: &RetrievalOutcome,
    retrieval_status: Option<&HybridRetrievalStatus>,
) -> String {
    let manifest = retrieval_status
        .map(|status| {
            format!(
                " manifest_generation={} source_chunks={} dense_count={} dense_kind={} lexical_doc_count={} fusion={}",
                status.generation_id,
                status.source_chunk_count,
                status.dense_count,
                status.dense_kind,
                status.lexical_doc_count,
                status.fusion_algorithm
            )
        })
        .unwrap_or_default();
    let (prefix, completeness_label) = match retrieval.completeness {
        RetrievalCompleteness::Complete => ("", "complete"),
        RetrievalCompleteness::Partial => ("[partial] ", "partial"),
        RetrievalCompleteness::Degraded => ("[degraded] ", "degraded"),
        RetrievalCompleteness::Unknown => ("[unknown] ", "unknown"),
    };
    let index_label = match retrieval.executed_path {
        ExecutedPath::HybridFusion => "hybrid",
        ExecutedPath::DenseOnly => "dense_only",
        ExecutedPath::LexicalOnly => "none",
        ExecutedPath::None => "unknown",
    };
    // First token of the fallback reason keeps the line greppable
    // (`hybrid_unavailable`, `execution_evidence_missing`, error kinds).
    let fallback_label = match (&retrieval.fallback_reason, retrieval.requested_mode) {
        (Some(reason), _) => reason
            .split(':')
            .next()
            .unwrap_or("unspecified")
            .to_string(),
        (None, RequestedMode::Lexical) => "operator_requested".to_string(),
        (None, _) => "none".to_string(),
    };
    let scope_safe = matches!(
        retrieval.executed_path,
        ExecutedPath::HybridFusion | ExecutedPath::DenseOnly
    ) && !retrieval.stale_evidence;
    let chunk_noun = if retrieval.executed_path == ExecutedPath::LexicalOnly {
        "scanned"
    } else {
        "candidate"
    };
    let model = model_id
        .map(|model_id| format!(" model={model_id}"))
        .unwrap_or_default();
    let scope_label = if backend_label.ends_with("_global_scoped") {
        " scope=global_project_filter"
    } else {
        ""
    };
    format!(
        "{}{} result(s) from {} {} chunks. oracle_status: backend={} index={} fallback={} completeness={}{} loctree_scope_safe={}{}{}",
        prefix,
        retrieval.matched_count,
        retrieval.examined_count,
        chunk_noun,
        backend_label,
        index_label,
        fallback_label,
        completeness_label,
        model,
        scope_safe,
        scope_label,
        manifest
    )
}

/// Bounded fetch limit for fuzzy retrieval. When post-filters are active we
/// over-fetch up to `FILTER_EXAMINED_CAP_RATIO`× the requested limit (floored
/// at `FILTER_EXAMINED_CAP_MIN`) so inside-window matches that sit below the
/// raw top-N still surface instead of being lost to a silent-empty result.
pub fn fuzzy_fetch_limit(user_limit: usize, filters_active: bool) -> usize {
    if filters_active {
        user_limit
            .saturating_mul(FILTER_EXAMINED_CAP_RATIO)
            .max(FILTER_EXAMINED_CAP_MIN)
    } else {
        user_limit
    }
}

/// Apply the shared score/agent/date/hours post-filters to a fuzzy result set
/// in place. Date bounds win over the `--hours` cutoff to match legacy
/// precedence.
fn apply_fuzzy_post_filters(
    results: &mut Vec<crate::rank::FuzzyResult>,
    post_filters: &SemanticSearchFilters,
) {
    if let Some(min_score) = post_filters.score_min {
        results.retain(|r| r.score >= min_score);
    }
    if let Some(ref agent_filter) = post_filters.agent {
        results.retain(|r| r.agent == *agent_filter);
    }
    if post_filters.date_lo.is_some() || post_filters.date_hi.is_some() {
        let lo = post_filters.date_lo.as_deref();
        let hi = post_filters.date_hi.as_deref();
        results.retain(|r| {
            lo.is_none_or(|lo| r.date.as_str() >= lo) && hi.is_none_or(|hi| r.date.as_str() <= hi)
        });
    } else if let Some(ref cutoff) = post_filters.hours_cutoff {
        let cutoff = cutoff.as_str();
        results.retain(|r| r.date.as_str() >= cutoff);
    }
}

/// Shared fuzzy retrieval + post-filter primitive. Both the CLI `aicx search`
/// fallback and the MCP `aicx_search` fallback route through this so the two
/// surfaces cannot drift in *what* they retrieve and filter. Fetches a bounded
/// pool then applies the post-filters; ordering/truncation is the caller's job
/// via [`finalize_fuzzy_results`].
pub fn fuzzy_search_with_post_filters(
    store_root: &Path,
    query: &str,
    limit: usize,
    project_scopes: &[Option<&str>],
    frame_kind: Option<FrameKind>,
    post_filters: &SemanticSearchFilters,
) -> anyhow::Result<(Vec<crate::rank::FuzzyResult>, usize)> {
    let fetch_limit = fuzzy_fetch_limit(limit, post_filters.is_active());
    let (mut results, scanned) = crate::rank::fuzzy_search_store(
        store_root,
        query,
        fetch_limit,
        project_scopes,
        frame_kind,
    )?;
    apply_fuzzy_post_filters(&mut results, post_filters);
    // Doctrine 2026-07-23: filesystem-fuzzy is the last resort when no
    // hybrid index exists. Recency-ranked literal — not letter-soup of
    // stale cards burying a same-day plan (operator repro: 2026-03 over
    // 2026-07-22).
    apply_recency_prior(&mut results);
    results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));
    Ok((results, scanned))
}

/// Shared "finalize" step for fuzzy/semantic result sets: optional kind retain,
/// then sort, then truncate to `limit`. Both CLI and MCP search call this so
/// ordering and limit semantics stay byte-identical across surfaces. `sort`
/// accepts `"newest"` / `"oldest"` / `"score"`. `None` falls back to descending
/// score; any unknown token falls back to `"newest"` (timestamp/date
/// descending), matching the unit test below.
pub fn finalize_fuzzy_results(
    mut results: Vec<crate::rank::FuzzyResult>,
    kind_filter: Option<&str>,
    sort: Option<&str>,
    limit: usize,
) -> Vec<crate::rank::FuzzyResult> {
    if let Some(kind_filter) = kind_filter {
        results.retain(|r| r.kind == kind_filter);
    }
    match sort {
        Some(sort_order) => results.sort_by(|a, b| {
            let t_a = a.timestamp.as_deref().unwrap_or(a.date.as_str());
            let t_b = b.timestamp.as_deref().unwrap_or(b.date.as_str());
            match sort_order {
                "newest" => t_b.cmp(t_a),
                "oldest" => t_a.cmp(t_b),
                "score" => b.score.cmp(&a.score).then(t_b.cmp(t_a)),
                _ => t_b.cmp(t_a),
            }
        }),
        // Default: score first, then recency — fuzzy fallback must not bury
        // fresh hits under letter-soup scoring of stale cards.
        None => results.sort_by(|a, b| {
            let t_a = a.timestamp.as_deref().unwrap_or(a.date.as_str());
            let t_b = b.timestamp.as_deref().unwrap_or(b.date.as_str());
            b.score.cmp(&a.score).then_with(|| t_b.cmp(t_a))
        }),
    }
    results.into_iter().take(limit).collect()
}

/// Append an `index_snapshot` honesty hint to a rendered hybrid search payload.
///
/// Hybrid results reflect the committed index *manifest* (a snapshot of the
/// corpus at index-build time), not a live freshness check — that check scans
/// canonical chunks and is intentionally kept off the search hot path. Without
/// this hint a `hybrid_rrf` result reads as "fully fresh"; the hint keeps the
/// surface honest by pointing callers at `aicx index status` to confirm there
/// are no pending (un-embedded) chunks. Shared by CLI and MCP so both surfaces
/// emit the identical honesty signal.
pub fn inject_index_snapshot_hint(rendered: &str, source_chunks: usize) -> anyhow::Result<String> {
    let mut value: serde_json::Value = serde_json::from_str(rendered).map_err(|e| {
        anyhow::anyhow!("parse rendered search payload for index_snapshot hint: {e}")
    })?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "index_snapshot".to_string(),
            serde_json::json!({
                "source_chunks": source_chunks,
                "freshness_verified": false,
                "note": "results reflect the committed index snapshot; run `aicx index status` to confirm there are no pending (un-embedded) chunks",
            }),
        );
    }
    serde_json::to_string(&value)
        .map_err(|e| anyhow::anyhow!("serialize search payload with index_snapshot hint: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn fuzzy(score: u8, kind: &str, date: &str, ts: Option<&str>) -> crate::rank::FuzzyResult {
        crate::rank::FuzzyResult {
            file: "f".to_string(),
            path: "p".to_string(),
            project: "proj".to_string(),
            kind: kind.to_string(),
            frame_kind: None,
            agent: "claude".to_string(),
            date: date.to_string(),
            timestamp: ts.map(|s| s.to_string()),
            score,
            label: "l".to_string(),
            density: 0.0,
            matched_lines: Vec::new(),
            session_id: None,
            cwd: None,
        }
    }

    #[test]
    fn recency_prior_prefers_a_fresh_close_match_over_a_stale_repeat() {
        let today = chrono::Utc::now().date_naive();
        let fresh_date = today.format("%Y-%m-%d").to_string();
        let stale_date = (today - chrono::Duration::days(10))
            .format("%Y-%m-%d")
            .to_string();
        let mut results = vec![
            fuzzy(50, "conversations", &fresh_date, None),
            fuzzy(70, "conversations", &stale_date, None),
        ];

        apply_recency_prior(&mut results);

        assert!(
            results[0].score > results[1].score,
            "a current close match should beat a ten-day-old repeated mention"
        );
    }

    #[test]
    fn recency_prior_still_separates_when_lexical_scores_are_already_capped() {
        // Operator repro: short queries map many BM25 hits to score=100; the
        // prior must still prefer today's hit over a week-old repeat.
        let today = chrono::Utc::now().date_naive();
        let fresh_date = today.format("%Y-%m-%d").to_string();
        let stale_date = (today - chrono::Duration::days(7))
            .format("%Y-%m-%d")
            .to_string();
        let mut results = vec![
            fuzzy(100, "conversations", &stale_date, None),
            fuzzy(100, "conversations", &fresh_date, None),
        ];

        apply_recency_prior(&mut results);
        results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));

        assert!(
            results[0].date == fresh_date && results[0].score > results[1].score,
            "fresh capped hit must outrank stale capped hit; got scores {} ({}), {} ({})",
            results[0].score,
            results[0].date,
            results[1].score,
            results[1].date
        );
        assert!(
            results[0].score > 100,
            "recency boost must be allowed past the old display ceiling of 100"
        );
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn lexical_rerank_window_keeps_a_wide_recency_candidate_pool() {
        assert_eq!(lexical_rerank_window(1, false), 500);
        assert_eq!(lexical_rerank_window(50, false), 500);
        assert_eq!(lexical_rerank_window(1, true), 80);
        assert_eq!(lexical_rerank_window(50, true), 80);
        assert_eq!(lexical_rerank_window(750, true), 750);
    }

    #[test]
    fn sanitize_lexical_preview_drops_thought_tokens_keeps_prose() {
        let mut result = fuzzy(100, "conversations", "2026-07-22", None);
        result.matched_lines = vec![
            r#"{"type":"thought","data":"The"}"#.to_string(),
            r#"{"type":"thread.started","thread_id":"abc"}"#.to_string(),
            "Routing strzałek taby is W2-B-4c in vc-procs.".to_string(),
        ];
        sanitize_lexical_preview_lines(&mut result);
        assert_eq!(result.matched_lines.len(), 1);
        assert!(result.matched_lines[0].contains("W2-B-4c"));
    }

    #[test]
    fn sanitize_lexical_preview_unwraps_item_completed_agent_message() {
        let mut result = fuzzy(100, "conversations", "2026-07-22", None);
        result.matched_lines = vec![
            r#"{"type":"item.completed","item":{"id":"item_0","type":"error","message":"Skill descriptions were shortened"}}"#.to_string(),
            r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"Routing strzałek is W2-B-4c."}}"#.to_string(),
        ];
        sanitize_lexical_preview_lines(&mut result);
        assert_eq!(result.matched_lines.len(), 1);
        assert_eq!(result.matched_lines[0], "Routing strzałek is W2-B-4c.");
    }

    #[test]
    fn sanitize_lexical_preview_scrapes_truncated_agent_message_json() {
        // Live CURRENT stores 240-char truncated preview lines that are not
        // valid JSON — still must surface the human text for ranking.
        let mut result = fuzzy(100, "conversations", "2026-07-22", None);
        result.matched_lines = vec![
            r#"{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"I’m using `vc-workflow` as requested, with its required `vc-init` orientation pass first. I’ll map the complete Loctree at ..."#.to_string(),
        ];
        sanitize_lexical_preview_lines(&mut result);
        assert_eq!(result.matched_lines.len(), 1);
        assert!(
            result.matched_lines[0].starts_with("I’m using `vc-workflow`"),
            "got {:?}",
            result.matched_lines[0]
        );
        assert!(!result.matched_lines[0].contains("item.completed"));
    }

    #[test]
    fn lexical_quality_prior_demotes_thought_spam_below_readable_answer() {
        let mut results = vec![
            {
                let mut noisy = fuzzy(100, "conversations", "2026-07-22", None);
                noisy.matched_lines = vec![
                    r#"{"type":"thought","data":"The"}"#.to_string(),
                    r#"{"type":"thought","data":" user"}"#.to_string(),
                    r#"{"type":"thought","data":" wants"}"#.to_string(),
                ];
                noisy.label = "noise".to_string();
                noisy
            },
            {
                let mut clean = fuzzy(80, "conversations", "2026-07-22", None);
                clean.matched_lines = vec![
                    "Operator asked about routing strzałek taby.".to_string(),
                    "Answer: W2-B-4c in vc-procs.".to_string(),
                ];
                clean.label = "signal".to_string();
                clean
            },
        ];
        for result in &mut results {
            sanitize_lexical_preview_lines(result);
        }
        apply_lexical_quality_prior("routing strzałek taby", &mut results);
        results.sort_by_key(|b| std::cmp::Reverse(b.score));
        assert_eq!(
            results[0].label,
            "signal",
            "readable query-matched answer must outrank thought-token spam; scores signal={} noise={}",
            results
                .iter()
                .find(|r| r.label == "signal")
                .map(|r| r.score)
                .unwrap_or(0),
            results
                .iter()
                .find(|r| r.label == "noise")
                .map(|r| r.score)
                .unwrap_or(0),
        );
    }

    #[test]
    fn fuzzy_fetch_limit_over_fetches_only_when_filters_active() {
        // Inactive filters: fetch exactly the requested limit.
        assert_eq!(fuzzy_fetch_limit(5, false), 5);
        // Active filters: over-fetch CAP_RATIO× the limit, floored at CAP_MIN.
        assert_eq!(
            fuzzy_fetch_limit(20, true),
            20 * FILTER_EXAMINED_CAP_RATIO // 200, above the floor
        );
        assert_eq!(fuzzy_fetch_limit(1, true), FILTER_EXAMINED_CAP_MIN);
    }

    #[test]
    fn finalize_fuzzy_results_defaults_to_descending_score() {
        let input = vec![
            fuzzy(10, "decision", "2026-01-01", None),
            fuzzy(50, "decision", "2026-01-01", None),
            fuzzy(30, "decision", "2026-01-01", None),
        ];
        let out = finalize_fuzzy_results(input, None, None, 10);
        assert_eq!(
            out.iter().map(|r| r.score).collect::<Vec<_>>(),
            vec![50, 30, 10]
        );
    }

    #[test]
    fn finalize_fuzzy_results_honors_newest_and_oldest() {
        let input = vec![
            fuzzy(10, "decision", "2026-01-01", Some("2026-01-01T00:00:00Z")),
            fuzzy(10, "decision", "2026-01-03", Some("2026-01-03T00:00:00Z")),
            fuzzy(10, "decision", "2026-01-02", Some("2026-01-02T00:00:00Z")),
        ];
        let newest = finalize_fuzzy_results(input.clone(), None, Some("newest"), 10);
        assert_eq!(
            newest.iter().map(|r| r.date.clone()).collect::<Vec<_>>(),
            vec!["2026-01-03", "2026-01-02", "2026-01-01"]
        );
        let oldest = finalize_fuzzy_results(input, None, Some("oldest"), 10);
        assert_eq!(
            oldest.iter().map(|r| r.date.clone()).collect::<Vec<_>>(),
            vec!["2026-01-01", "2026-01-02", "2026-01-03"]
        );
    }

    #[test]
    fn finalize_fuzzy_results_retains_kind_then_truncates() {
        let input = vec![
            fuzzy(90, "decision", "2026-01-01", None),
            fuzzy(80, "task", "2026-01-01", None),
            fuzzy(70, "decision", "2026-01-01", None),
            fuzzy(60, "decision", "2026-01-01", None),
        ];
        let out = finalize_fuzzy_results(input, Some("decision"), None, 2);
        assert_eq!(out.len(), 2, "kind retain then truncate to limit");
        assert!(out.iter().all(|r| r.kind == "decision"));
        assert_eq!(
            out.iter().map(|r| r.score).collect::<Vec<_>>(),
            vec![90, 70]
        );
    }

    #[test]
    fn finalize_fuzzy_results_unknown_sort_token_falls_back_to_newest() {
        // The shared primitive treats any unknown sort token as "newest" so a
        // caller passing an out-of-contract string degrades gracefully instead
        // of panicking or diverging from the MCP surface.
        let input = vec![
            fuzzy(10, "decision", "2026-01-01", Some("2026-01-01T00:00:00Z")),
            fuzzy(10, "decision", "2026-01-02", Some("2026-01-02T00:00:00Z")),
        ];
        let out = finalize_fuzzy_results(input, None, Some("nonsense"), 10);
        assert_eq!(
            out.iter().map(|r| r.date.clone()).collect::<Vec<_>>(),
            vec!["2026-01-02", "2026-01-01"]
        );
    }

    #[test]
    fn inject_index_snapshot_hint_marks_freshness_unverified() {
        let payload = inject_index_snapshot_hint(
            r#"{"oracle_status":{"backend":"hybrid_rrf"},"results":2,"items":[]}"#,
            11906,
        )
        .expect("index_snapshot hint should inject");
        let json: serde_json::Value =
            serde_json::from_str(&payload).expect("payload stays valid JSON");
        assert_eq!(json["results"], 2);
        assert_eq!(json["index_snapshot"]["source_chunks"], 11906);
        assert_eq!(json["index_snapshot"]["freshness_verified"], false);
        assert!(
            json["index_snapshot"]["note"]
                .as_str()
                .unwrap()
                .contains("aicx index status")
        );
    }
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    static SEARCH_TEST_AICX_HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct SearchTestAicxHomeGuard {
        previous: Option<OsString>,
        dir: PathBuf,
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for SearchTestAicxHomeGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => {
                    // SAFETY: tests that mutate AICX_HOME are serialized by
                    // SEARCH_TEST_AICX_HOME_LOCK for the guard lifetime.
                    unsafe { std::env::set_var("AICX_HOME", previous) };
                }
                None => {
                    // SAFETY: tests that mutate AICX_HOME are serialized by
                    // SEARCH_TEST_AICX_HOME_LOCK for the guard lifetime.
                    unsafe { std::env::remove_var("AICX_HOME") };
                }
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn set_search_test_aicx_home(label: &str) -> SearchTestAicxHomeGuard {
        let guard = SEARCH_TEST_AICX_HOME_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("search AICX_HOME test lock");
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "aicx-search-engine-{label}-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create isolated search AICX_HOME");
        let previous = std::env::var_os("AICX_HOME");
        // SAFETY: guarded by SEARCH_TEST_AICX_HOME_LOCK for the full
        // lifetime of the returned guard.
        unsafe { std::env::set_var("AICX_HOME", &dir) };
        SearchTestAicxHomeGuard {
            previous,
            dir,
            _guard: guard,
        }
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    fn test_semantic_filters<'a>(project: Option<&'a str>) -> SemanticRetrievalFilters<'a> {
        SemanticRetrievalFilters {
            kind: None,
            frame_kind: None,
            project,
            agent: None,
            date: None,
            candidate_filters: None,
        }
    }

    #[test]
    fn fail_fast_carries_actionable_recommendation() {
        let home = set_search_test_aicx_home("fail-fast");
        // In any test environment we either lack the feature flag, lack a
        // hydrated embedder, or lack a built index. The function must
        // never panic, and the typed error must carry both a non-empty
        // `reason` AND a non-empty `recommendation` so the operator
        // knows what to do next.
        let result = try_semantic_search(&home.dir, "any query", 10, &[None], None, None);

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

    /// Legacy semantic label with NO hybrid manifest evidence must render as
    /// explicit unknown — never as a healthy `index=hybrid fallback=none`.
    #[test]
    fn semantic_status_line_marks_backend_and_index() {
        let retrieval = semantic_retrieval_outcome("embedded_semantic", None, 11_237, 0, false);
        let line = render_semantic_status_line(
            "embedded_semantic",
            Some("F2LLM-v2-0.6B.Q4_K_M.gguf"),
            &retrieval,
            None,
        );
        assert!(line.contains("backend=embedded_semantic"));
        assert!(
            line.contains("index=unknown") && line.contains("completeness=unknown"),
            "no execution evidence must render as unknown, not hybrid: {line}"
        );
        assert!(
            line.contains("fallback=execution_evidence_missing"),
            "line was: {line}"
        );
        assert!(line.contains("model=F2LLM-v2-0.6B.Q4_K_M.gguf"));
        assert!(
            line.contains("loctree_scope_safe=false"),
            "unknown execution must not claim scope safety: {line}"
        );
    }

    /// Patch 3 / Bug B+ observability: the dense-only degraded path must NOT
    /// render as a healthy hybrid query. It must say so out loud (operator
    /// sees the quality drop), not silently claim `index=hybrid fallback=none`.
    #[test]
    fn semantic_status_line_flags_dense_only_as_degraded() {
        let retrieval = semantic_retrieval_outcome("semantic_dense_only", None, 227_290, 5, false);
        let line = render_semantic_status_line(
            "semantic_dense_only",
            Some("qwen3-embedding-8b"),
            &retrieval,
            None,
        );
        assert!(line.contains("backend=semantic_dense_only"));
        assert!(
            !line.contains("index=hybrid"),
            "dense-only must not claim index=hybrid: {line}"
        );
        assert!(
            !line.contains("fallback=none"),
            "dense-only is a fallback; must not claim fallback=none: {line}"
        );
        assert!(
            line.contains("degraded") && line.contains("fallback=hybrid_unavailable"),
            "dense-only must surface degraded status explicitly: {line}"
        );
        assert!(line.contains("completeness=degraded"), "line was: {line}");
    }

    #[test]
    fn semantic_status_line_marks_hybrid_global_project_scope() {
        let status = HybridRetrievalStatus {
            generation_id: "g-global".to_string(),
            source_chunk_count: 259_007,
            dense_count: 259_007,
            lexical_doc_count: 258_990,
            fusion_algorithm: "rrf".to_string(),
            dense_kind: aicx_retrieve::MMAP_DENSE_KIND.to_string(),
        };
        let retrieval = semantic_retrieval_outcome(
            BACKEND_HYBRID_RRF_GLOBAL_SCOPED,
            Some(&status),
            259_007,
            10,
            false,
        );
        let line = render_semantic_status_line(
            BACKEND_HYBRID_RRF_GLOBAL_SCOPED,
            Some("qwen3-embedding:8b"),
            &retrieval,
            Some(&status),
        );

        assert!(line.contains("backend=hybrid_rrf_global_scoped"));
        assert!(line.contains("index=hybrid"));
        assert!(line.contains("fallback=none"));
        assert!(line.contains("completeness=complete"));
        assert!(line.contains("scope=global_project_filter"));
        assert!(
            !line.contains("degraded"),
            "global-scoped hybrid search should not claim degraded dense-only: {line}"
        );
    }

    /// World-model contract: the typed mapping refuses to invent execution
    /// evidence. Legacy semantic success without a manifest is unknown, the
    /// dense-only label is degraded with a reason, hybrid+manifest is complete.
    #[test]
    fn semantic_retrieval_outcome_maps_evidence_not_labels_to_health() {
        let legacy = semantic_retrieval_outcome("embedded_semantic", None, 100, 5, false);
        assert_eq!(legacy.completeness, RetrievalCompleteness::Unknown);
        assert_eq!(legacy.executed_path, ExecutedPath::None);

        let dense = semantic_retrieval_outcome(BACKEND_SEMANTIC_DENSE_ONLY, None, 1000, 5, false);
        assert_eq!(dense.completeness, RetrievalCompleteness::Degraded);
        assert_eq!(dense.executed_path, ExecutedPath::DenseOnly);
        assert!(
            dense
                .fallback_reason
                .as_deref()
                .unwrap()
                .starts_with("hybrid_unavailable")
        );

        let status = HybridRetrievalStatus {
            generation_id: "g".to_string(),
            source_chunk_count: 10,
            dense_count: 10,
            lexical_doc_count: 10,
            fusion_algorithm: "rrf".to_string(),
            dense_kind: aicx_retrieve::MMAP_DENSE_KIND.to_string(),
        };
        let hybrid = semantic_retrieval_outcome(BACKEND_HYBRID_RRF, Some(&status), 10, 3, false);
        assert_eq!(hybrid.completeness, RetrievalCompleteness::Complete);
        assert_eq!(hybrid.executed_path, ExecutedPath::HybridFusion);
        assert_eq!(hybrid.fallback_reason, None);
    }

    /// Project-filter queries use the global `_all` generation with metadata
    /// pushdown when the project shard is absent.
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn project_bucket_missing_falls_back_to_global_with_filter() {
        let dir =
            std::env::temp_dir().join(format!("aicx-semantic-global-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let project_index_path = dir.join("vetcoders_vista").join("embeddings.ndjson");
        let all_index_path = dir.join("_all").join("embeddings.ndjson");
        std::fs::create_dir_all(all_index_path.parent().unwrap()).expect("create all bucket");
        std::fs::write(&all_index_path, "{}\n").expect("touch all index");

        let scope = select_semantic_bucket_scope(
            Some("vetcoders/vista"),
            project_index_path.clone(),
            all_index_path.clone(),
        )
        .expect("global fallback");

        assert_eq!(scope.index_path, all_index_path);
        assert_eq!(scope.index_project, None);
        assert_eq!(scope.retrieval_project_filter, Some("vetcoders/vista"));
        assert!(scope.used_global_project_scope);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Global `_all` is preferred over a project shard when both exist.
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn global_bucket_preferred_over_project_shard() {
        let dir =
            std::env::temp_dir().join(format!("aicx-project-shard-select-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project_index_path = dir.join("target").join("embeddings.ndjson");
        let all_index_path = dir.join("_all").join("embeddings.ndjson");
        std::fs::create_dir_all(project_index_path.parent().unwrap()).expect("project dir");
        std::fs::create_dir_all(all_index_path.parent().unwrap()).expect("global dir");
        std::fs::write(&project_index_path, "target\n").expect("project index");
        std::fs::write(&all_index_path, "foreign\n").expect("global index");

        let scope = select_semantic_bucket_scope(
            Some("vetcoders/target"),
            project_index_path,
            all_index_path.clone(),
        )
        .expect("global preferred");

        assert_eq!(scope.index_path, all_index_path);
        assert_eq!(scope.index_project, None);
        assert_eq!(scope.retrieval_project_filter, Some("vetcoders/target"));
        assert!(scope.used_global_project_scope);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn global_shard_budget_is_explicit_and_reports_saturation() {
        let projects: Vec<String> = (0..GLOBAL_SHARD_BUDGET + 1)
            .map(|index| format!("org/project-{index:02}"))
            .collect();
        let (selected, saturated) = bounded_global_shards(&projects);

        assert_eq!(selected.len(), GLOBAL_SHARD_BUDGET);
        assert!(saturated, "an omitted shard must make retrieval partial");
        assert_eq!(selected[0], "org/project-00");
        assert_eq!(selected[GLOBAL_SHARD_BUDGET - 1], "org/project-15");
    }

    /// A published but corrupt manifest is a typed stale-generation error.
    /// The caller propagates this error and may only use the primary NDJSON
    /// path when the manifest is absent or the operator chose --legacy-dense.
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn corrupt_published_manifest_fails_before_any_adapter_is_opened() {
        let dir = std::env::temp_dir().join(format!(
            "aicx-corrupt-retrieval-manifest-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tempdir");
        let manifest_path = dir.join("manifest.json");
        std::fs::write(&manifest_path, b"{not-json").expect("corrupt manifest");
        let info = crate::embedder::EmbeddingModelInfo {
            model_id: "test-model".to_string(),
            dimension: 3,
            backend: "test".to_string(),
            profile: aicx_embeddings::EmbeddingProfile::Base,
            source: aicx_embeddings::NativeEmbeddingSource::ExplicitPath(dir.join("model.gguf")),
        };

        let err = load_hybrid_index(None, &dir.join("missing.ndjson"), &info, &manifest_path)
            .expect_err("corrupt published manifest must fail closed");
        assert!(matches!(err, SemanticError::RetrievalManifestStale { .. }));
        assert!(err.reason().contains("could not read retrieval manifest"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F1 regression: when dense-only fallback queries the global `_all` index,
    /// project filtering must happen inside retrieval before top-N selection. A
    /// very close hit from another project must not crowd out the requested
    /// project.
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn dense_only_global_index_project_filter_is_strict_before_limit() {
        use crate::vector_index::{IndexEntry, IndexHeader};
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!(
            "aicx-dense-only-project-filter-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let vista_chunk = dir.join("vista.md");
        let other_chunk = dir.join("other.md");
        std::fs::write(&vista_chunk, "vista transcription duplication note").expect("write vista");
        std::fs::write(&other_chunk, "other project with near identical embedding")
            .expect("write other");

        let header = IndexHeader {
            schema_version: "1".to_string(),
            model_id: "test-model".to_string(),
            model_profile: "test".to_string(),
            dimension: 3,
            generated_at: "2026-06-03T00:00:00Z".to_string(),
            entry_count: 2,
        };
        let mk_entry =
            |id: &str, project: &str, path: &std::path::Path, emb: Vec<f32>| IndexEntry {
                id: id.to_string(),
                project: project.to_string(),
                agent: "claude".to_string(),
                date: "20260603".to_string(),
                path: path.to_path_buf(),
                kind: "conversations".to_string(),
                session_id: format!("sess-{id}"),
                frame_kind: Some("agent_reply".to_string()),
                cwd: None,
                embedding: emb,
            };
        let other = mk_entry(
            "other-hit",
            "vetcoders/other",
            &other_chunk,
            vec![1.0, 0.0, 0.0],
        );
        let other_bad_dim = mk_entry(
            "other-bad-dim",
            "vetcoders/other",
            &other_chunk,
            vec![1.0, 0.0],
        );
        let vista = mk_entry(
            "vista-hit",
            "vetcoders/vista",
            &vista_chunk,
            vec![0.8, 0.2, 0.0],
        );
        let index_path = dir.join("embeddings.ndjson");
        {
            let mut f = std::fs::File::create(&index_path).expect("create index");
            writeln!(f, "{}", serde_json::to_string(&header).unwrap()).unwrap();
            writeln!(f, "{}", serde_json::to_string(&other).unwrap()).unwrap();
            writeln!(f, "{}", serde_json::to_string(&other_bad_dim).unwrap()).unwrap();
            writeln!(f, "{}", serde_json::to_string(&vista).unwrap()).unwrap();
        }

        let query = vec![1.0_f32, 0.0, 0.0];
        let outcome = query_dense_only_from_primary(
            &index_path,
            &query,
            3,
            1,
            test_semantic_filters(Some("vetcoders/vista")),
            "test-model",
            false,
        )
        .expect("dense-only global query should retain requested project hits");

        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.results[0].project, "vetcoders/vista");
        assert!(
            outcome.results[0].label.contains("vista-hit"),
            "expected requested project hit, got {}",
            outcome.results[0].label
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn dense_only_missing_project_is_true_empty_not_empty_index() {
        use crate::vector_index::{IndexEntry, IndexHeader};
        use std::io::Write;

        let dir =
            std::env::temp_dir().join(format!("aicx-dense-only-true-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let chunk_path = dir.join("other.md");
        std::fs::write(&chunk_path, "other project").expect("write chunk");
        let index_path = dir.join("embeddings.ndjson");
        let header = IndexHeader {
            schema_version: "1".to_string(),
            model_id: "test-model".to_string(),
            model_profile: "test".to_string(),
            dimension: 3,
            generated_at: "2026-07-22T00:00:00Z".to_string(),
            entry_count: 1,
        };
        let entry = IndexEntry {
            id: "other-hit".to_string(),
            project: "foreign/project".to_string(),
            agent: "codex".to_string(),
            date: "20260722".to_string(),
            path: chunk_path,
            kind: "conversations".to_string(),
            session_id: "session-other".to_string(),
            frame_kind: Some("agent_reply".to_string()),
            cwd: None,
            embedding: vec![1.0, 0.0, 0.0],
        };
        {
            let mut file = std::fs::File::create(&index_path).expect("create index");
            writeln!(file, "{}", serde_json::to_string(&header).unwrap()).unwrap();
            writeln!(file, "{}", serde_json::to_string(&entry).unwrap()).unwrap();
        }

        let outcome = query_dense_only_from_primary(
            &index_path,
            &[1.0, 0.0, 0.0],
            3,
            5,
            test_semantic_filters(Some("target/project")),
            "test-model",
            true,
        )
        .expect("missing project inside a populated global index is a true empty outcome");

        assert!(outcome.results.is_empty());
        assert_eq!(outcome.scanned, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Patch 3 / Bug B+: when the hybrid stack is unavailable, semantic
    /// search must degrade to dense-only ranking over the PRIMARY committed
    /// index instead of hard-failing. This proves the dense leg reads the
    /// committed `embeddings.ndjson` directly, ranks by cosine, labels itself
    /// `semantic_dense_only`, and surfaces the closest embedding first.
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn dense_only_from_primary_ranks_by_cosine_without_hybrid() {
        use crate::vector_index::{IndexEntry, IndexHeader};
        use std::io::Write;

        let dir =
            std::env::temp_dir().join(format!("aicx-dense-only-ranks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let chunk_a = dir.join("a.md");
        let chunk_b = dir.join("b.md");
        std::fs::write(&chunk_a, "alpha chunk body about the noise filter").expect("write a");
        std::fs::write(&chunk_b, "beta chunk body about something unrelated").expect("write b");

        let header = IndexHeader {
            schema_version: "1".to_string(),
            model_id: "test-model".to_string(),
            model_profile: "test".to_string(),
            dimension: 3,
            generated_at: "2026-06-01T00:00:00Z".to_string(),
            entry_count: 2,
        };
        let mk_entry = |id: &str, path: &std::path::Path, emb: Vec<f32>| IndexEntry {
            id: id.to_string(),
            project: "test/repo".to_string(),
            agent: "claude".to_string(),
            date: "20260601".to_string(),
            path: path.to_path_buf(),
            kind: "conversations".to_string(),
            session_id: format!("sess-{id}"),
            frame_kind: Some("agent_reply".to_string()),
            cwd: None,
            embedding: emb,
        };
        let entry_a = mk_entry("chunk-a", &chunk_a, vec![1.0, 0.0, 0.0]);
        let entry_b = mk_entry("chunk-b", &chunk_b, vec![0.0, 1.0, 0.0]);

        let index_path = dir.join("embeddings.ndjson");
        {
            let mut f = std::fs::File::create(&index_path).expect("create index");
            writeln!(f, "{}", serde_json::to_string(&header).unwrap()).unwrap();
            writeln!(f, "{}", serde_json::to_string(&entry_a).unwrap()).unwrap();
            writeln!(f, "{}", serde_json::to_string(&entry_b).unwrap()).unwrap();
        }

        // Query closest to entry_a's [1, 0, 0].
        let query = vec![0.9_f32, 0.1, 0.0];
        let outcome = query_dense_only_from_primary(
            &index_path,
            &query,
            3,
            10,
            test_semantic_filters(None),
            "test-model",
            false,
        )
        .expect("dense-only query should succeed on a valid primary index");

        assert_eq!(
            outcome.backend_label, "semantic_dense_only",
            "dense-only path must label itself explicitly, not as hybrid"
        );
        assert!(
            !outcome.results.is_empty(),
            "a valid dense index must yield at least one hit"
        );
        assert!(
            outcome.results[0].label.contains("chunk-a"),
            "closest embedding (chunk-a) must rank first, got label: {}",
            outcome.results[0].label
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn dense_only_global_scope_labels_backend_explicitly() {
        use crate::vector_index::{IndexEntry, IndexHeader};
        use std::io::Write;

        let dir =
            std::env::temp_dir().join(format!("aicx-dense-only-all-label-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let chunk_path = dir.join("vista.md");
        std::fs::write(&chunk_path, "vista fallback chunk").expect("write chunk");
        let index_path = dir.join("embeddings.ndjson");

        let header = IndexHeader {
            schema_version: "1".to_string(),
            model_id: "test-model".to_string(),
            model_profile: "test".to_string(),
            dimension: 3,
            generated_at: "2026-06-04T00:00:00Z".to_string(),
            entry_count: 1,
        };
        let entry = IndexEntry {
            id: "vista-hit".to_string(),
            project: "vetcoders/vista".to_string(),
            agent: "claude".to_string(),
            date: "20260604".to_string(),
            path: chunk_path,
            kind: "conversations".to_string(),
            session_id: "sess-vista-hit".to_string(),
            frame_kind: Some("agent_reply".to_string()),
            cwd: None,
            embedding: vec![1.0, 0.0, 0.0],
        };

        {
            let mut f = std::fs::File::create(&index_path).expect("create index");
            writeln!(f, "{}", serde_json::to_string(&header).unwrap()).unwrap();
            writeln!(f, "{}", serde_json::to_string(&entry).unwrap()).unwrap();
        }

        let outcome = query_dense_only_from_primary(
            &index_path,
            &[1.0, 0.0, 0.0],
            3,
            10,
            test_semantic_filters(Some("vetcoders/vista")),
            "test-model",
            true,
        )
        .expect("dense-only global scoped query should succeed");

        assert_eq!(
            outcome.backend_label,
            BACKEND_SEMANTIC_DENSE_ONLY_GLOBAL_SCOPED
        );
        assert!(
            outcome.results[0]
                .label
                .starts_with("dense_only_global_scoped:"),
            "global-scoped hit label should be explicit, got {}",
            outcome.results[0].label
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Migration safety (operator's F2LLM 2048 -> qwen3 4096 case): a committed
    /// index built at a different dimension must be REJECTED, never scored —
    /// cross-model cosine is meaningless. The dense-only leg must surface
    /// DimensionMismatch, not silently rank garbage.
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn dense_only_rejects_dimension_mismatch_instead_of_ranking_garbage() {
        use crate::vector_index::{IndexEntry, IndexHeader};
        use std::io::Write;

        let dir = std::env::temp_dir().join(format!(
            "aicx-dense-only-dimmismatch-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let chunk = dir.join("c.md");
        std::fs::write(&chunk, "body").expect("write");

        // Committed index was built at dimension 2 (older model)...
        let header = IndexHeader {
            schema_version: "1".to_string(),
            model_id: "old-2d-model".to_string(),
            model_profile: "test".to_string(),
            dimension: 2,
            generated_at: "2026-06-01T00:00:00Z".to_string(),
            entry_count: 1,
        };
        let entry = IndexEntry {
            id: "c".to_string(),
            project: "test/repo".to_string(),
            agent: "claude".to_string(),
            date: "20260601".to_string(),
            path: chunk.clone(),
            kind: "conversations".to_string(),
            session_id: "s".to_string(),
            frame_kind: None,
            cwd: None,
            embedding: vec![1.0, 0.0],
        };
        let index_path = dir.join("embeddings.ndjson");
        {
            let mut f = std::fs::File::create(&index_path).unwrap();
            writeln!(f, "{}", serde_json::to_string(&header).unwrap()).unwrap();
            writeln!(f, "{}", serde_json::to_string(&entry).unwrap()).unwrap();
        }

        // ...but the current embedder is dimension 3.
        let query = vec![1.0_f32, 0.0, 0.0];
        let err = query_dense_only_from_primary(
            &index_path,
            &query,
            3,
            10,
            test_semantic_filters(None),
            "qwen3-3d",
            false,
        )
        .expect_err("dimension mismatch must error, not rank cross-model garbage");
        assert_eq!(
            err.kind(),
            "dimension_mismatch",
            "expected dimension_mismatch, got kind={} reason={}",
            err.kind(),
            err.reason()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A committed index with a header but zero data rows must surface
    /// EmptyIndex (actionable), never panic or return an empty-but-Ok outcome.
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn dense_only_empty_index_is_typed_error_not_panic() {
        use crate::vector_index::IndexHeader;
        use std::io::Write;

        let dir =
            std::env::temp_dir().join(format!("aicx-dense-only-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let header = IndexHeader {
            schema_version: "1".to_string(),
            model_id: "m".to_string(),
            model_profile: "test".to_string(),
            dimension: 3,
            generated_at: "2026-06-01T00:00:00Z".to_string(),
            entry_count: 0,
        };
        let index_path = dir.join("embeddings.ndjson");
        {
            let mut f = std::fs::File::create(&index_path).unwrap();
            writeln!(f, "{}", serde_json::to_string(&header).unwrap()).unwrap();
        }

        let query = vec![1.0_f32, 0.0, 0.0];
        let err = query_dense_only_from_primary(
            &index_path,
            &query,
            3,
            10,
            test_semantic_filters(None),
            "m",
            false,
        )
        .expect_err("empty committed index must error");
        assert_eq!(err.kind(), "empty_index");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fake_hit(rank: usize, date: &str, agent: &str, score: u8) -> FuzzyResult {
        fake_hit_with_frame(rank, date, agent, score, None, vec![format!("rank {rank}")])
    }

    fn fake_hit_with_frame(
        rank: usize,
        date: &str,
        agent: &str,
        score: u8,
        frame_kind: Option<&str>,
        matched_lines: Vec<String>,
    ) -> FuzzyResult {
        FuzzyResult {
            file: format!("rank-{rank}.md"),
            path: format!("rank-{rank}.md"),
            project: "test/repo".to_string(),
            kind: "conversations".to_string(),
            frame_kind: frame_kind.map(ToString::to_string),
            agent: agent.to_string(),
            date: date.to_string(),
            timestamp: None,
            score,
            label: format!("test:{rank}"),
            density: 0.5,
            matched_lines,
            session_id: None,
            cwd: None,
        }
    }

    #[test]
    fn default_semantic_quality_drops_machine_frames_and_recovers_human_hits() {
        let pool = vec![
            fake_hit_with_frame(
                0,
                "2026-06-01",
                "codex",
                99,
                Some("tool_call"),
                vec!["[12:00:00] tool: id: toolu_01abc".to_string()],
            ),
            fake_hit_with_frame(
                1,
                "2026-06-01",
                "codex",
                98,
                Some("system_note"),
                vec![r#"[12:00:01] system: {"last_token_usage":{"input_tokens":123}}"#.to_string()],
            ),
            fake_hit_with_frame(
                2,
                "2026-06-01",
                "claude",
                82,
                Some("user_msg"),
                vec!["[21:47:27] user: dorobmy shot features (1, 2, 3 powyzej)".to_string()],
            ),
            fake_hit_with_frame(
                3,
                "2026-06-01",
                "claude",
                81,
                Some("agent_reply"),
                vec!["[21:48:10] assistant: shot features implementation plan".to_string()],
            ),
        ];

        let filtered = apply_default_semantic_quality(pool, "shot features", None);
        let frames: Vec<Option<&str>> = filtered
            .iter()
            .map(|result| result.frame_kind.as_deref())
            .collect();

        assert_eq!(
            frames,
            vec![Some("user_msg"), Some("agent_reply")],
            "default operator search should hide machine frames from top results"
        );
        assert!(
            filtered[0].score > 82,
            "literal human match should get a quality boost, got {}",
            filtered[0].score
        );
    }

    #[test]
    fn default_semantic_quality_prioritizes_hyphenated_anchor_overlap() {
        let pool = vec![
            fake_hit_with_frame(
                0,
                "2026-06-01",
                "codex",
                93,
                Some("agent_reply"),
                vec![
                    "[10:00:00] assistant: zrobmy tak, przeklikam appke i zobaczymy co sie wysypie"
                        .to_string(),
                ],
            ),
            fake_hit_with_frame(
                1,
                "2026-06-01",
                "claude",
                68,
                Some("agent_reply"),
                vec![
                    "[10:01:00] assistant: To jest to - vista leak + devista; leak-vista-info poszlo do public repo"
                        .to_string(),
                ],
            ),
        ];

        let filtered =
            apply_default_semantic_quality(pool, "vista-leak o co w tym chodziolo?", None);

        assert_eq!(
            filtered[0].label, "test:1",
            "hyphenated query anchors should beat higher dense-score vibe matches"
        );
        assert!(
            filtered[0].score > filtered[1].score,
            "anchor-bearing hit should outrank no-anchor hit: {filtered:?}"
        );
    }

    #[test]
    fn default_semantic_quality_prioritizes_underscore_anchor_overlap() {
        let pool = vec![
            fake_hit_with_frame(
                0,
                "2026-06-01",
                "codex",
                91,
                Some("agent_reply"),
                vec!["[10:00:00] assistant: auth flow and trusted device notes".to_string()],
            ),
            fake_hit_with_frame(
                1,
                "2026-06-01",
                "claude",
                64,
                Some("agent_reply"),
                vec![
                    "[10:01:00] assistant: deep-link includes completion_token and portal completion endpoint"
                        .to_string(),
                ],
            ),
        ];

        let filtered = apply_default_semantic_quality(pool, "completion_token auth leak", None);

        assert_eq!(
            filtered[0].label, "test:1",
            "underscore/code-ish anchors should carry lexical evidence weight"
        );
    }

    #[test]
    fn default_semantic_quality_prioritizes_digit_code_anchor_overlap() {
        let pool = vec![
            fake_hit_with_frame(
                0,
                "2026-06-01",
                "codex",
                88,
                Some("agent_reply"),
                vec![
                    "[10:00:00] assistant: Silver and Sztudio connection is now stable".to_string(),
                ],
            ),
            fake_hit_with_frame(
                1,
                "2026-06-01",
                "claude",
                57,
                Some("agent_reply"),
                vec![
                    "[10:01:00] assistant: Sztudio uses qwen3 embedding model for semantic index"
                        .to_string(),
                ],
            ),
        ];

        let filtered = apply_default_semantic_quality(pool, "qwen3-embedding sztudio silver", None);

        assert_eq!(
            filtered[0].label, "test:1",
            "digit/code anchors should prevent generic host mentions from winning"
        );
    }

    #[test]
    fn default_semantic_quality_prefers_answer_like_agent_reply_over_anchor_echo() {
        let pool = vec![
            fake_hit_with_frame(
                0,
                "2026-06-01",
                "codex",
                76,
                Some("user_msg"),
                vec![
                    "[10:00:00] user: spoko, ostatnio w podobny sposob poszlo duzo leak-vista-info"
                        .to_string(),
                ],
            ),
            fake_hit_with_frame(
                1,
                "2026-06-01",
                "claude",
                60,
                Some("agent_reply"),
                vec![
                    "[10:01:00] assistant: To jest to - vista leak plus devista; leak-vista-info poszlo do public repo i trzeba zamknac incydent"
                        .to_string(),
                ],
            ),
        ];

        let filtered =
            apply_default_semantic_quality(pool, "vista-leak o co w tym chodzilo?", None);

        assert_eq!(
            filtered[0].label, "test:1",
            "answer-like agent replies with the same anchor evidence should beat short anchor echoes"
        );
    }

    #[test]
    fn explicit_frame_kind_keeps_requested_machine_frame() {
        let pool = vec![
            fake_hit_with_frame(
                0,
                "2026-06-01",
                "codex",
                92,
                Some("tool_call"),
                vec!["[12:00:00] tool: rg frame_kind".to_string()],
            ),
            fake_hit_with_frame(
                1,
                "2026-06-01",
                "codex",
                80,
                Some("user_msg"),
                vec!["[12:00:03] user: frame kind please".to_string()],
            ),
        ];

        let filtered =
            apply_default_semantic_quality(pool, "frame_kind", Some(FrameKind::ToolCall));

        assert!(
            filtered
                .iter()
                .any(|result| result.frame_kind.as_deref() == Some("tool_call")),
            "explicit frame-kind search must still allow tool_call results"
        );
    }

    #[test]
    fn default_semantic_quality_expands_examined_pool_without_user_filters() {
        let filters = SemanticSearchFilters::default();

        assert_eq!(
            semantic_fetch_limit(10, None, &filters),
            10 * FILTER_EXAMINED_CAP_RATIO,
            "default human-facing quality gate needs a wider pool than the displayed limit"
        );
        assert_eq!(
            semantic_fetch_limit(10, Some(FrameKind::ToolCall), &filters),
            10,
            "explicit frame-kind queries should not pay the default quality-pool expansion"
        );
    }

    #[test]
    fn candidate_metadata_filters_compose_agent_and_date_before_ranking() {
        let filters = SemanticSearchFilters {
            agent: Some("codex".to_string()),
            date_lo: Some("2026-07-20".to_string()),
            date_hi: Some("2026-07-22".to_string()),
            ..Default::default()
        };

        assert!(semantic_candidate_metadata_matches(
            &serde_json::json!({"agent": "codex", "date": "20260721"}),
            &filters,
        ));
        assert!(!semantic_candidate_metadata_matches(
            &serde_json::json!({"agent": "claude", "date": "20260721"}),
            &filters,
        ));
        assert!(!semantic_candidate_metadata_matches(
            &serde_json::json!({"agent": "codex", "date": "20260723"}),
            &filters,
        ));

        let exact = hybrid_filters(SemanticRetrievalFilters {
            kind: Some("conversations"),
            frame_kind: Some(FrameKind::AgentReply),
            project: Some("target/project"),
            agent: filters.agent.as_deref(),
            date: Some("2026-07-21"),
            candidate_filters: Some(&filters),
        });
        assert_eq!(
            exact.values.get("project"),
            Some(&serde_json::json!("target/project"))
        );
        assert_eq!(exact.values.get("agent"), Some(&serde_json::json!("codex")));
        assert_eq!(
            exact.values.get("date"),
            Some(&serde_json::json!("20260721"))
        );
    }

    /// Bug #31 regression: top-N raw semantic hits sit outside the
    /// filter window, but valid hits exist further down the pool. The
    /// pushdown wrapper must surface the inside-window hits at user
    /// `limit=10`, not silently return zero.
    #[test]
    fn filter_pushdown_recovers_hits_when_top_n_sit_outside_window() {
        // 50-chunk pool: ranks 0..10 dated 2026-05-22 (outside the
        // "since 2026-05-23" window), ranks 10..30 dated 2026-05-23
        // (inside), ranks 30..50 dated 2026-05-21 (also outside).
        let pool: Vec<FuzzyResult> = (0..50)
            .map(|rank| {
                let date = if rank < 10 {
                    "2026-05-22"
                } else if rank < 30 {
                    "2026-05-23"
                } else {
                    "2026-05-21"
                };
                fake_hit(rank, date, "claude", 50)
            })
            .collect();

        // Honesty check: without filters, the pool's top 10 are all
        // outside the 2026-05-23 window — proves the test corpus is
        // shaped to expose the pre-fix bug.
        let untouched =
            apply_semantic_post_filters(pool.clone(), &SemanticSearchFilters::default());
        let pre_fix_top_ten: Vec<&str> =
            untouched.iter().take(10).map(|r| r.date.as_str()).collect();
        assert!(
            pre_fix_top_ten.iter().all(|d| *d == "2026-05-22"),
            "test corpus precondition failed: top-10 should all be 2026-05-22, got {:?}",
            pre_fix_top_ten
        );

        // Pushdown: hours cutoff at 2026-05-23 should retain ranks
        // 10..30 (20 hits), all inside the window.
        let filters = SemanticSearchFilters {
            hours_cutoff: Some("2026-05-23".to_string()),
            ..Default::default()
        };
        let filtered = apply_semantic_post_filters(pool.clone(), &filters);
        assert_eq!(
            filtered.len(),
            20,
            "expected 20 inside-window hits to survive, got {}",
            filtered.len()
        );
        for r in filtered.iter().take(10) {
            assert_eq!(
                r.date.as_str(),
                "2026-05-23",
                "filter survivor outside window: {}",
                r.date
            );
        }

        // No partial diagnostic: matched (20) >= user_limit (10).
        let diag = partial_pushdown_diagnostic(
            filters.is_active(),
            pool.len(),
            filtered.len(),
            10,
            FILTER_EXAMINED_CAP_MIN,
        );
        assert!(
            diag.is_none(),
            "expected no partial diagnostic when limit satisfied, got {diag:?}"
        );
    }

    /// Pool fully examined but filters yield fewer than user limit →
    /// wrapper must emit `filter_yielded_partial` so the caller can
    /// surface "examined the cap, found N < limit" instead of pretending
    /// the corpus is empty.
    #[test]
    fn filter_pushdown_emits_partial_when_cap_examined_under_limit() {
        let user_limit = 10;
        let fetch_limit = FILTER_EXAMINED_CAP_MIN; // 50
        let examined = fetch_limit; // pool returned the full cap
        let matched = 3;

        let diag = partial_pushdown_diagnostic(true, examined, matched, user_limit, fetch_limit)
            .expect("expected partial diagnostic when cap examined and filters under-deliver");
        assert_eq!(diag.kind, "filter_yielded_partial");
        assert_eq!(diag.examined, examined);
        assert_eq!(diag.matched, matched);
        assert_eq!(diag.requested_limit, user_limit);
        assert_eq!(diag.examined_cap_ratio, FILTER_EXAMINED_CAP_RATIO);
    }

    #[test]
    fn inject_filter_pushdown_none_is_byte_preserving_pass_through() {
        let rendered = "{this is intentionally not json";

        let injected = inject_filter_pushdown_diagnostic(rendered, None)
            .expect("None diagnostic should not parse or mutate rendered payload");

        assert_eq!(injected, rendered);
    }

    #[test]
    fn inject_filter_pushdown_some_adds_object_field() {
        let diag = FilterPushdownDiagnostic {
            kind: "filter_yielded_partial",
            examined: 50,
            matched: 3,
            requested_limit: 10,
            examined_cap_ratio: FILTER_EXAMINED_CAP_RATIO,
        };

        let injected = inject_filter_pushdown_diagnostic(r#"{"results":3}"#, Some(&diag))
            .expect("object payload should accept filter_pushdown diagnostic");
        let payload: serde_json::Value =
            serde_json::from_str(&injected).expect("injected payload should remain JSON");

        assert_eq!(payload["results"], 3);
        assert_eq!(payload["filter_pushdown"]["kind"], "filter_yielded_partial");
        assert_eq!(payload["filter_pushdown"]["examined"], 50);
        assert_eq!(payload["filter_pushdown"]["matched"], 3);
        assert_eq!(payload["filter_pushdown"]["requested_limit"], 10);
        assert_eq!(
            payload["filter_pushdown"]["examined_cap_ratio"],
            FILTER_EXAMINED_CAP_RATIO
        );
    }

    #[test]
    fn inject_filter_pushdown_some_leaves_non_object_root_shape() {
        let diag = FilterPushdownDiagnostic {
            kind: "filter_yielded_partial",
            examined: 50,
            matched: 3,
            requested_limit: 10,
            examined_cap_ratio: FILTER_EXAMINED_CAP_RATIO,
        };

        let injected = inject_filter_pushdown_diagnostic(r#"[{"results":3}]"#, Some(&diag))
            .expect("non-object root should still round-trip as JSON");
        let payload: serde_json::Value =
            serde_json::from_str(&injected).expect("round-tripped payload should parse");

        assert!(payload.is_array());
        assert_eq!(payload[0]["results"], 3);
        assert!(payload.get("filter_pushdown").is_none());
    }

    #[test]
    fn inject_filter_pushdown_some_overwrites_existing_field() {
        let diag = FilterPushdownDiagnostic {
            kind: "filter_yielded_partial",
            examined: 100,
            matched: 9,
            requested_limit: 25,
            examined_cap_ratio: FILTER_EXAMINED_CAP_RATIO,
        };

        let injected = inject_filter_pushdown_diagnostic(
            r#"{"filter_pushdown":{"kind":"stale"},"results":9}"#,
            Some(&diag),
        )
        .expect("existing diagnostic field should be replaced by current diagnostic");
        let payload: serde_json::Value =
            serde_json::from_str(&injected).expect("injected payload should remain JSON");

        assert_eq!(payload["filter_pushdown"]["kind"], "filter_yielded_partial");
        assert_eq!(payload["filter_pushdown"]["examined"], 100);
        assert_eq!(payload["filter_pushdown"]["matched"], 9);
        assert_eq!(payload["filter_pushdown"]["requested_limit"], 25);
    }

    /// Short pool (corpus-side exhaustion) is NOT a partial-cap event —
    /// the diagnostic stays silent so the caller can distinguish
    /// "examined cap, still under-delivered" from "corpus genuinely
    /// small" and surface different operator guidance.
    #[test]
    fn filter_pushdown_silent_when_pool_shorter_than_cap() {
        let user_limit = 10;
        let fetch_limit = FILTER_EXAMINED_CAP_MIN;
        let examined = 12; // index ran out of candidates well below the cap
        let matched = 2;

        let diag = partial_pushdown_diagnostic(true, examined, matched, user_limit, fetch_limit);
        assert!(
            diag.is_none(),
            "short-pool exhaustion must not raise the partial-cap diagnostic, got {diag:?}"
        );
    }

    /// Inactive filter set is a pass-through: no diagnostic, no retain
    /// pruning. Verifies the wrapper does not pay any cost when the
    /// caller has no filters to push down.
    #[test]
    fn filter_pushdown_no_filters_is_pass_through() {
        let pool: Vec<FuzzyResult> = (0..5)
            .map(|rank| fake_hit(rank, "2026-05-23", "claude", 50))
            .collect();
        let filters = SemanticSearchFilters::default();
        let filtered = apply_semantic_post_filters(pool.clone(), &filters);
        assert_eq!(filtered.len(), pool.len(), "pass-through must keep all");
        let diag = partial_pushdown_diagnostic(filters.is_active(), 5, 5, 10, 50);
        assert!(diag.is_none());
    }

    /// Date range + agent + score compose correctly inside the wrapper
    /// so the CLI / MCP do not need to re-implement the predicate
    /// stack. Locks the precedence: explicit date range wins over the
    /// `--hours` cutoff (matches legacy ordering preserved by the
    /// wrapper).
    #[test]
    fn filter_pushdown_composes_agent_date_score() {
        let pool = vec![
            fake_hit(0, "2026-05-20", "claude", 80),
            fake_hit(1, "2026-05-22", "claude", 40),
            fake_hit(2, "2026-05-23", "codex", 90),
            fake_hit(3, "2026-05-23", "claude", 90),
            fake_hit(4, "2026-05-24", "claude", 95),
        ];
        let filters = SemanticSearchFilters {
            agent: Some("claude".to_string()),
            score_min: Some(75),
            date_lo: Some("2026-05-23".to_string()),
            date_hi: Some("2026-05-24".to_string()),
            // hours_cutoff is set but ignored because date_lo/hi wins.
            hours_cutoff: Some("2026-05-01".to_string()),
            legacy_dense: false,
            deep: false,
        };
        let filtered = apply_semantic_post_filters(pool, &filters);
        let ids: Vec<&str> = filtered.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(ids, vec!["test:3", "test:4"]);
    }

    #[test]
    fn semantic_status_line_surfaces_hybrid_manifest_counts() {
        let status = HybridRetrievalStatus {
            generation_id: "g-test".to_string(),
            source_chunk_count: 123,
            dense_count: 123,
            lexical_doc_count: 122,
            fusion_algorithm: "rrf".to_string(),
            dense_kind: aicx_retrieve::MMAP_DENSE_KIND.to_string(),
        };
        let retrieval = semantic_retrieval_outcome("hybrid_rrf", Some(&status), 123, 3, false);
        let line =
            render_semantic_status_line("hybrid_rrf", Some("model"), &retrieval, Some(&status));
        assert!(line.contains("backend=hybrid_rrf"));
        assert!(line.contains("manifest_generation=g-test"));
        assert!(line.contains("source_chunks=123"));
        assert!(line.contains("dense_count=123"));
        assert!(line.contains("dense_kind=exact_mmap_v1"));
        assert!(line.contains("lexical_doc_count=122"));
    }
}
