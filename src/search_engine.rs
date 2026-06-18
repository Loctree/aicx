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
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

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

const BACKEND_HYBRID_RRF: &str = "hybrid_rrf";
const BACKEND_SEMANTIC_DENSE_ONLY: &str = "semantic_dense_only";
const BACKEND_SEMANTIC_DENSE_ONLY_ALL_FALLBACK: &str = "semantic_dense_only_all_fallback";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HybridRetrievalStatus {
    pub generation_id: String,
    pub source_chunk_count: usize,
    pub dense_count: usize,
    pub lexical_doc_count: usize,
    pub fusion_algorithm: String,
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
        let mut hybrid_statuses = Vec::new();
        // Track whether any scope fell back to dense-only, so the merged
        // outcome reports the degraded backend instead of silently claiming
        // hybrid — the degraded status must reach the CLI/MCP boundary.
        let mut any_dense_only = false;
        let mut any_all_bucket_fallback = false;
        for scope in scopes {
            let mut outcome = try_semantic_search_native(
                query,
                per_scope_limit,
                scope,
                frame_kind_filter,
                kind_filter,
            )?;
            if outcome
                .backend_label
                .starts_with(BACKEND_SEMANTIC_DENSE_ONLY)
            {
                any_dense_only = true;
            }
            if outcome.backend_label.ends_with("_all_fallback") {
                any_all_bucket_fallback = true;
            }
            scanned += outcome.scanned;
            model_id.get_or_insert(outcome.model_id.clone());
            if let Some(status) = outcome.retrieval_status.clone() {
                hybrid_statuses.push(status);
            }
            merged_results.append(&mut outcome.results);
        }
        merged_results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));
        merged_results.truncate(limit);
        Ok(SemanticOutcome {
            results: merged_results,
            scanned,
            backend_label: if any_dense_only && any_all_bucket_fallback {
                BACKEND_SEMANTIC_DENSE_ONLY_ALL_FALLBACK
            } else if any_dense_only {
                BACKEND_SEMANTIC_DENSE_ONLY
            } else {
                BACKEND_HYBRID_RRF
            },
            model_id: model_id.unwrap_or_else(|| "unknown".to_string()),
            retrieval_status: merge_hybrid_statuses(&hybrid_statuses),
        })
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[derive(Debug)]
struct SemanticBucketScope<'a> {
    index_project: Option<&'a str>,
    retrieval_project_filter: Option<&'a str>,
    index_path: std::path::PathBuf,
    used_all_bucket_fallback: bool,
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[derive(Clone, Copy)]
struct SemanticRetrievalFilters<'a> {
    kind: Option<&'a str>,
    frame_kind: Option<FrameKind>,
    project: Option<&'a str>,
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn index_not_built_error(path: std::path::PathBuf, project_filter: Option<&str>) -> SemanticError {
    let cmd = match project_filter {
        Some(p) => format!("aicx index --project {p}"),
        None => "aicx index".to_string(),
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
        recommendation: format!(
            "run `{cmd}` (one-off; subsequent runs query the index in-process)"
        ),
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn legacy_index_hint(project_filter: Option<&str>, canonical_path: &Path) -> Option<String> {
    let legacy_path = dirs::home_dir()?
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
    if project_index_path.exists() {
        return Ok(SemanticBucketScope {
            index_project: project_filter,
            // Keep project pushdown even inside a project-specific bucket as
            // a defensive guard against stale or mixed-project artifacts.
            retrieval_project_filter: project_filter,
            index_path: project_index_path,
            used_all_bucket_fallback: false,
        });
    }

    if let Some(project) = project_filter {
        if all_index_path.exists() {
            return Ok(SemanticBucketScope {
                index_project: None,
                retrieval_project_filter: Some(project),
                index_path: all_index_path,
                used_all_bucket_fallback: true,
            });
        }
        return Err(index_not_built_error(project_index_path, project_filter));
    }

    Err(index_not_built_error(project_index_path, project_filter))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn try_semantic_search_native(
    query: &str,
    limit: usize,
    project_filter: Option<&str>,
    frame_kind_filter: Option<FrameKind>,
    kind_filter: Option<&str>,
) -> std::result::Result<SemanticOutcome, SemanticError> {
    // Resolve + verify the committed index FIRST, BEFORE paying the
    // (potentially heavy) embedder bootstrap. On a host with no local index
    // (e.g. the silver mirror, which serves semantic from sztudio and keeps
    // `indexed/` empty by design) this makes `aicx search` / the MCP
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

    let retrieval_filters = SemanticRetrievalFilters {
        kind: kind_filter,
        frame_kind: frame_kind_filter,
        project: scope.retrieval_project_filter,
    };

    if scope.used_all_bucket_fallback {
        // `_all` is a cross-project bucket. Loading its manifest-managed dense
        // brute-force artifact materializes every vector in memory before the
        // adapter can apply FilterSet. For project-scoped fallback, use the
        // primary committed index with a pre-deserialize project guard so the
        // operator does not pay an 11GB dense load for one repo query.
        return query_dense_only_from_primary(
            &path,
            &query_embedding,
            embedder_dim,
            limit,
            retrieval_filters,
            &info.model_id,
            true,
        );
    }

    // Patch 3 / Bug B+: the hybrid (tantivy lexical + dense fusion) stack can
    // be unavailable for manifest-side reasons — never committed
    // (`RetrievalManifestMissing`) or mid-rebuild / mismatched
    // (`RetrievalManifestStale`; e.g. `TantivyAdapter::build` wipes its dir
    // before committing). The dense embeddings in the committed index stay a
    // valid semantic artifact throughout, so degrade to dense-only ranking
    // instead of hard-failing the whole query. Lexical is part of the
    // ranking, not a precondition for semantic search.
    let hybrid = if manifest_path.exists() {
        match load_hybrid_index(scope.index_project, &path, &info, &manifest_path) {
            Ok(hybrid) => hybrid,
            Err(SemanticError::RetrievalManifestMissing { .. })
            | Err(SemanticError::RetrievalManifestStale { .. }) => {
                return query_dense_only_from_primary(
                    &path,
                    &query_embedding,
                    embedder_dim,
                    limit,
                    retrieval_filters,
                    &info.model_id,
                    false,
                );
            }
            Err(other) => return Err(other),
        }
    } else {
        // Manifest was never committed — serve dense-only directly from the
        // primary committed index (already validated above: exists, correct
        // dimension, non-empty).
        return query_dense_only_from_primary(
            &path,
            &query_embedding,
            embedder_dim,
            limit,
            retrieval_filters,
            &info.model_id,
            false,
        );
    };
    let manifest = hybrid.manifest().cloned();
    let filters = hybrid_filters(retrieval_filters);
    let hits = match hybrid.query_hybrid(aicx_retrieve::HybridQueryInput {
        query_text: query,
        query_embedding: &query_embedding,
        filters,
        limit,
    }) {
        Ok(hits) => hits,
        Err(err) => return Err(index_query_error(&path, err)),
    };

    if hits.is_empty() {
        let scanned = manifest
            .as_ref()
            .map(|manifest| manifest.source_chunk_count)
            .unwrap_or(0);
        return Err(SemanticError::NoResults {
            path: path.clone(),
            scanned,
            reason: format!(
                "hybrid index at {} produced 0 ranked hits for this query",
                manifest_path.display()
            ),
            recommendation: "either the index is empty (rebuild with `aicx index`) \
                 or your query has no semantic neighbours in the corpus — try broader phrasing"
                .to_string(),
        });
    }

    let retrieval_status = manifest.as_ref().map(HybridRetrievalStatus::from);
    let scanned = retrieval_status
        .as_ref()
        .map(|status| status.source_chunk_count)
        .unwrap_or(hits.len());
    let results: Vec<FuzzyResult> = hits
        .into_iter()
        .take(limit)
        .map(|h| {
            let path = hit_path(&h);
            let score_pct = hybrid_score_pct(h.score);
            let matched_lines = semantic_preview_lines(&path);
            let label = format!("hybrid_rrf:{}", h.chunk_id);
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
        backend_label: BACKEND_HYBRID_RRF,
        model_id: info.model_id,
        retrieval_status,
    })
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn load_hybrid_index(
    project_filter: Option<&str>,
    source_index_path: &std::path::Path,
    info: &crate::embedder::EmbeddingModelInfo,
    manifest_path: &std::path::Path,
) -> std::result::Result<aicx_retrieve::HybridIndex, SemanticError> {
    let manifest_dir = crate::vector_index::hybrid_index_dir(project_filter).map_err(|err| {
        SemanticError::RetrievalManifestStale {
            path: manifest_path.to_path_buf(),
            reason: format!("could not resolve hybrid index dir: {err}"),
            recommendation: "run `aicx index` to rebuild the hybrid retrieval bucket".to_string(),
        }
    })?;
    let dense_path = crate::vector_index::hybrid_dense_path(project_filter).map_err(|err| {
        SemanticError::RetrievalManifestStale {
            path: manifest_path.to_path_buf(),
            reason: format!("could not resolve hybrid dense path: {err}"),
            recommendation: "run `aicx index` to rebuild the hybrid retrieval bucket".to_string(),
        }
    })?;
    if !dense_path.exists() {
        return Err(SemanticError::RetrievalManifestStale {
            path: manifest_path.to_path_buf(),
            reason: format!(
                "hybrid dense artifact is missing at {}",
                dense_path.display()
            ),
            recommendation: "run `aicx index` to rebuild the committed hybrid artifacts"
                .to_string(),
        });
    }
    let source_hash = crate::vector_index::observed_source_hash_for_index_path(source_index_path)
        .map_err(|err| SemanticError::RetrievalManifestStale {
        path: manifest_path.to_path_buf(),
        reason: format!("could not hash committed source index: {err}"),
        recommendation: "run `aicx index` to rebuild the committed hybrid artifacts".to_string(),
    })?;
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
    let dense = Box::new(
        aicx_retrieve::load_from_ndjson(
            &dense_path,
            info.dimension,
            aicx_retrieve::Distance::Cosine,
        )
        .map_err(|err| SemanticError::RetrievalManifestStale {
            path: manifest_path.to_path_buf(),
            reason: format!("could not open hybrid dense artifact: {err:#}"),
            recommendation: "run `aicx index` to rebuild the committed hybrid artifacts"
                .to_string(),
        })?,
    );
    let fusion = Box::new(aicx_retrieve::ReciprocalRankFusion::default());
    let fingerprint = crate::vector_index::hybrid_embedder_fingerprint(info);
    aicx_retrieve::HybridIndex::load_from_manifest(
        lexical,
        dense,
        fusion,
        manifest_dir,
        fingerprint,
        &source_hash,
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
/// Engaged when the hybrid stack is unavailable for any manifest-side reason
/// (`RetrievalManifestMissing` / `RetrievalManifestStale`) — e.g. the lexical
/// index is mid-rebuild (`TantivyAdapter::build` wipes its dir) or the manifest
/// was never committed. The dense embeddings in the committed index remain a
/// valid semantic artifact throughout, so we degrade to dense-only cosine
/// ranking instead of hard-failing the whole query.
///
/// This is NOT the doctrinal "silent fuzzy fallback" (see module docs): it is
/// explicit semantic search over real embeddings, surfaced via
/// `backend_label = "semantic_dense_only"`. Hard-fail only when there is no
/// valid dense artifact to serve.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn query_dense_only_from_primary(
    index_path: &std::path::Path,
    query_embedding: &[f32],
    dim: usize,
    limit: usize,
    filters: SemanticRetrievalFilters<'_>,
    model_id: &str,
    used_all_bucket_fallback: bool,
) -> std::result::Result<SemanticOutcome, SemanticError> {
    use aicx_retrieve::{BruteForceAdapter, ChunkRef, DenseChunkRef, DenseIndex, Distance};

    let (header, entries) = crate::vector_index::read_committed_index_entries_matching_project(
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

    if entries.is_empty() {
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
            let label = if used_all_bucket_fallback {
                format!("dense_only_all_fallback:{}", h.chunk_id)
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
        backend_label: if used_all_bucket_fallback {
            BACKEND_SEMANTIC_DENSE_ONLY_ALL_FALLBACK
        } else {
            BACKEND_SEMANTIC_DENSE_ONLY
        },
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
        }
    }
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

    let outcome = try_semantic_search(
        store_root,
        query,
        fetch_limit,
        project_filters,
        frame_kind_filter,
        kind_filter,
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
    );

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
    if !normalized_query.is_empty() && haystack.contains(&normalized_query) {
        score += 12;
    }
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
    ];
    if noisy_needles
        .iter()
        .any(|needle| normalized.contains(needle))
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
/// successful semantic search call.
pub fn render_semantic_status_line(
    backend_label: &str,
    model_id: &str,
    result_count: usize,
    scanned: usize,
    retrieval_status: Option<&HybridRetrievalStatus>,
) -> String {
    let manifest = retrieval_status
        .map(|status| {
            format!(
                " manifest_generation={} source_chunks={} dense_count={} lexical_doc_count={} fusion={}",
                status.generation_id,
                status.source_chunk_count,
                status.dense_count,
                status.lexical_doc_count,
                status.fusion_algorithm
            )
        })
        .unwrap_or_default();
    // The dense-only degraded path (Bug B+) must not masquerade as a healthy
    // hybrid query: tell the operator the lexical fusion leg is unavailable
    // and that this is a fallback, not the full stack.
    let dense_only = backend_label.starts_with(BACKEND_SEMANTIC_DENSE_ONLY);
    let all_bucket_fallback = backend_label.ends_with("_all_fallback");
    let (prefix, index_label, fallback_label) = if dense_only && all_bucket_fallback {
        ("[degraded] ", "dense_only", "hybrid_unavailable,all_bucket")
    } else if dense_only {
        ("[degraded] ", "dense_only", "hybrid_unavailable")
    } else if all_bucket_fallback {
        ("", "hybrid", "all_bucket")
    } else {
        ("", "hybrid", "none")
    };
    format!(
        "{}{} result(s) from {} candidate chunks. oracle_status: backend={} index={} fallback={} model={} loctree_scope_safe=true{}",
        prefix,
        result_count,
        scanned,
        backend_label,
        index_label,
        fallback_label,
        model_id,
        manifest
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
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

    #[test]
    fn semantic_status_line_marks_backend_and_index() {
        let line = render_semantic_status_line(
            "embedded_semantic",
            "F2LLM-v2-0.6B.Q4_K_M.gguf",
            0,
            11_237,
            None,
        );
        assert!(line.contains("backend=embedded_semantic"));
        assert!(line.contains("index=hybrid"));
        assert!(line.contains("model=F2LLM-v2-0.6B.Q4_K_M.gguf"));
        assert!(line.contains("loctree_scope_safe=true"));
    }

    /// Patch 3 / Bug B+ observability: the dense-only degraded path must NOT
    /// render as a healthy hybrid query. It must say so out loud (operator
    /// sees the quality drop), not silently claim `index=hybrid fallback=none`.
    #[test]
    fn semantic_status_line_flags_dense_only_as_degraded() {
        let line = render_semantic_status_line(
            "semantic_dense_only",
            "qwen3-embedding-8b",
            5,
            227_290,
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
    }

    /// F1 regression: project-scoped semantic search may run on a host where
    /// only the cross-project `_all` bucket is materialized. In that case the
    /// search path should fall back to `_all`, while keeping the requested
    /// project as a strict retrieval filter.
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn project_bucket_missing_falls_back_to_all_with_project_filter() {
        let dir = std::env::temp_dir().join(format!(
            "aicx-semantic-scope-fallback-{}",
            std::process::id()
        ));
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
        .expect("missing project bucket should use existing _all bucket");

        assert_eq!(scope.index_path, all_index_path);
        assert_eq!(scope.index_project, None);
        assert_eq!(scope.retrieval_project_filter, Some("vetcoders/vista"));
        assert!(
            scope.used_all_bucket_fallback,
            "scope should explicitly mark the _all fallback"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F1 regression: when dense-only fallback queries `_all`, project
    /// filtering must happen inside retrieval before top-N selection. A very
    /// close hit from another project must not crowd out the requested project.
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn dense_only_all_bucket_project_filter_is_strict_before_limit() {
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
        .expect("dense-only _all query should retain requested project hits");

        assert_eq!(outcome.results.len(), 1);
        assert_eq!(outcome.results[0].project, "vetcoders/vista");
        assert!(
            outcome.results[0].label.contains("vista-hit"),
            "expected requested project hit, got {}",
            outcome.results[0].label
        );

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
    fn dense_only_all_bucket_fallback_labels_backend_explicitly() {
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
        .expect("dense-only all-bucket fallback should succeed");

        assert_eq!(
            outcome.backend_label,
            BACKEND_SEMANTIC_DENSE_ONLY_ALL_FALLBACK
        );
        assert!(
            outcome.results[0]
                .label
                .starts_with("dense_only_all_fallback:"),
            "all-bucket fallback hit label should be explicit, got {}",
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
        };
        let line = render_semantic_status_line("hybrid_rrf", "model", 3, 123, Some(&status));
        assert!(line.contains("backend=hybrid_rrf"));
        assert!(line.contains("manifest_generation=g-test"));
        assert!(line.contains("source_chunks=123"));
        assert!(line.contains("dense_count=123"));
        assert!(line.contains("lexical_doc_count=122"));
    }
}
