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
    /// Manifest-backed hybrid retrieval status, when the live path is the
    /// committed hybrid stack rather than a legacy vector scan.
    pub retrieval_status: Option<HybridRetrievalStatus>,
}

/// Result of a semantic search call.
pub type SemanticOutcome = SemanticSearchOutcome;

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
            backend_label: "hybrid_rrf",
            model_id: model_id.unwrap_or_else(|| "unknown".to_string()),
            retrieval_status: merge_hybrid_statuses(&hybrid_statuses),
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
        crate::vector_index::hybrid_manifest_path(project_filter).map_err(|err| {
            SemanticError::IndexCorrupt {
                path: path.clone(),
                reason: format!("could not resolve hybrid manifest path: {err}"),
                recommendation: "ensure $AICX_HOME (or $HOME) is writable, then run `aicx index`"
                    .to_string(),
            }
        })?;
    if !manifest_path.exists() {
        let cmd = match project_filter {
            Some(p) => format!("aicx index --project {p}"),
            None => "aicx index".to_string(),
        };
        return Err(SemanticError::RetrievalManifestMissing {
            path: manifest_path.clone(),
            reason: format!(
                "hybrid retrieval manifest is missing at {}",
                manifest_path.display()
            ),
            recommendation: format!(
                "run `{cmd}` with the current binary so lexical+dense hybrid artifacts are committed"
            ),
        });
    }

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

    let hybrid = load_hybrid_index(project_filter, &path, &info, &manifest_path)?;
    let manifest = hybrid.manifest().cloned();
    let filters = hybrid_filters(kind_filter, frame_kind_filter);
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
                label: format!("hybrid_rrf:{}", h.chunk_id),
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
        backend_label: "hybrid_rrf",
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

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn hybrid_filters(
    kind_filter: Option<&str>,
    frame_kind_filter: Option<FrameKind>,
) -> aicx_retrieve::FilterSet {
    let mut filters = aicx_retrieve::FilterSet::default();
    if let Some(kind) = kind_filter {
        filters.values.insert(
            "kind".to_string(),
            serde_json::Value::String(kind.to_string()),
        );
    }
    if let Some(frame_kind) = frame_kind_filter {
        filters.values.insert(
            "frame_kind".to_string(),
            serde_json::Value::String(frame_kind.as_str().to_string()),
        );
    }
    filters
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
    let fetch_limit = if post_filters.is_active() {
        user_limit
            .saturating_mul(FILTER_EXAMINED_CAP_RATIO)
            .max(FILTER_EXAMINED_CAP_MIN)
    } else {
        user_limit.max(1)
    };

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
    let filtered = apply_semantic_post_filters(results, post_filters);
    let matched = filtered.len();
    let diagnostic = partial_pushdown_diagnostic(
        post_filters.is_active(),
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
    format!(
        "{} result(s) from {} candidate chunks. oracle_status: backend={} index=hybrid fallback=none model={} loctree_scope_safe=true{}",
        result_count, scanned, backend_label, model_id, manifest
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
            None,
        );
        assert!(line.contains("backend=embedded_semantic"));
        assert!(line.contains("index=hybrid"));
        assert!(line.contains("model=F2LLM-v2-0.6B.Q4_K_M.gguf"));
        assert!(line.contains("loctree_scope_safe=true"));
    }

    fn fake_hit(rank: usize, date: &str, agent: &str, score: u8) -> FuzzyResult {
        FuzzyResult {
            file: format!("rank-{rank}.md"),
            path: format!("rank-{rank}.md"),
            project: "test/repo".to_string(),
            kind: "conversations".to_string(),
            frame_kind: None,
            agent: agent.to_string(),
            date: date.to_string(),
            timestamp: None,
            score,
            label: format!("test:{rank}"),
            density: 0.5,
            matched_lines: Vec::new(),
            session_id: None,
            cwd: None,
        }
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
