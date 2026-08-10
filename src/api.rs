//! Stable in-process API for consumers that want AICX as a library.
//!
//! The CLI remains the product shell, but this facade is the supported crate
//! boundary. With default features callers get corpus, retrieval, intent, and
//! health operations without importing CLI-private glue from `main.rs`.
//! With `default-features = false, features = ["loctree-consumer"]`, callers get
//! the stable read core: chunk listing/reading, typed chunk references, session
//! types, and pure intent extraction stages without embedding or app surfaces.

use anyhow::{Context, Result};
use serde::Serialize;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[cfg(feature = "app")]
use crate::doctor::{DoctorOptions, DoctorReport};
use crate::intents::{IntentExtraction, IntentsConfig};
use crate::legacy_archive::{ReadContextChunk, StoredContextFile};
#[cfg(feature = "app")]
use crate::rank::FuzzyResult;
use crate::sessions::{self, SessionInfo};
#[cfg(feature = "app")]
use crate::timeline::FrameKind;

/// Configuration for an [`Aicx`] library handle.
#[derive(Debug, Clone)]
pub struct AicxConfig {
    /// AICX base directory. Defaults to `~/.aicx`.
    pub aicx_home: PathBuf,
}

impl AicxConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            aicx_home: crate::aicx_home::ensure()?,
        })
    }

    pub fn with_aicx_home(path: impl Into<PathBuf>) -> Self {
        Self {
            aicx_home: path.into(),
        }
    }
}

/// In-process AICX client.
#[derive(Debug, Clone)]
pub struct Aicx {
    config: AicxConfig,
}

impl Aicx {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            config: AicxConfig::from_env()?,
        })
    }

    pub fn with_aicx_home(path: impl Into<PathBuf>) -> Self {
        Self {
            config: AicxConfig::with_aicx_home(path),
        }
    }

    pub fn config(&self) -> &AicxConfig {
        &self.config
    }

    pub fn aicx_home(&self) -> &Path {
        &self.config.aicx_home
    }

    pub fn list_chunks(&self) -> Result<Vec<StoredContextFile>> {
        crate::legacy_archive::scan_context_files_at(&self.config.aicx_home)
    }

    pub fn read_chunk(
        &self,
        reference: impl AsRef<str>,
        max_chars: Option<usize>,
    ) -> Result<ReadContextChunk> {
        crate::legacy_archive::read_context_chunk_at(
            &self.config.aicx_home,
            reference.as_ref(),
            max_chars,
        )
    }

    /// Search the published source/extract generation. Fails fast with a
    /// descriptive error when any precondition is
    /// missing (embedder unhydrated, index not built, dimension mismatch).
    #[cfg(feature = "app")]
    pub fn semantic_search(
        &self,
        query: impl AsRef<str>,
        opts: SearchOptions,
    ) -> Result<SearchResults> {
        let owned_projects = if opts.projects.is_empty() {
            opts.project.into_iter().collect::<Vec<_>>()
        } else {
            opts.projects
        };
        let project_scopes_owned =
            search_project_scopes(&self.config.aicx_home, &owned_projects, opts.project_match)?;
        let project_scopes: Vec<Option<&str>> = project_scopes_owned
            .iter()
            .map(|scope| scope.as_deref())
            .collect();

        let kind_filter = match opts.kind.as_deref() {
            Some(kind) => Some(
                crate::timeline::Kind::parse(kind)
                    .ok_or_else(|| anyhow::anyhow!("unknown corpus kind `{kind}`"))?,
            ),
            None => None,
        };

        let outcome = crate::search_engine::try_semantic_search(
            &self.config.aicx_home,
            query.as_ref(),
            opts.limit,
            &project_scopes,
            opts.frame_kind,
            kind_filter.map(|kind| kind.dir_name()),
        )
        .map_err(anyhow::Error::from)
        .context("semantic search unavailable")?;

        Ok(SearchResults {
            results: outcome.results,
            scanned: outcome.scanned,
        })
    }

    pub fn extract_intents(&self, config: &IntentsConfig) -> Result<IntentExtraction> {
        crate::intents::extract_intents_from_root_at_with_stats(
            config,
            &self.config.aicx_home,
            chrono::Utc::now(),
        )
    }

    pub fn extract_intents_for_projects(
        &self,
        config: &IntentsConfig,
        projects: &[String],
    ) -> Result<IntentExtraction> {
        crate::intents::extract_intents_from_root_at_for_projects_with_stats(
            config,
            projects,
            &self.config.aicx_home,
            chrono::Utc::now(),
        )
    }

    #[cfg(feature = "app")]
    pub async fn doctor(&self, opts: &DoctorOptions) -> Result<DoctorReport> {
        crate::doctor::run_at(&self.config.aicx_home, opts).await
    }

    pub fn index_status(&self, project: Option<&str>) -> Result<IndexStatus> {
        index_status_at(&self.config.aicx_home, project)
    }
}

#[derive(Debug, Clone)]
#[cfg(feature = "app")]
pub struct SearchOptions {
    pub limit: usize,
    pub projects: Vec<String>,
    pub project: Option<String>,
    pub project_match: crate::legacy_archive::ProjectMatchMode,
    pub frame_kind: Option<FrameKind>,
    pub kind: Option<String>,
}

#[cfg(feature = "app")]
impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 10,
            projects: Vec::new(),
            project: None,
            project_match: crate::legacy_archive::ProjectMatchMode::Exact,
            frame_kind: None,
            kind: None,
        }
    }
}

#[cfg(feature = "app")]
fn search_project_scopes(
    aicx_home: &Path,
    projects: &[String],
    match_mode: crate::legacy_archive::ProjectMatchMode,
) -> Result<Vec<Option<String>>> {
    if projects.is_empty() {
        return Ok(vec![None]);
    }
    let corpus = crate::legacy_archive::project_identities_in_store_or_index_at(aicx_home)?;
    let resolution =
        crate::legacy_archive::require_project_resolution(projects, &corpus, match_mode)?;
    Ok(resolution.selected.into_iter().map(Some).collect())
}

#[derive(Debug, Clone, Serialize)]
#[cfg(feature = "app")]
pub struct SearchResults {
    pub results: Vec<FuzzyResult>,
    pub scanned: usize,
}

/// Truthful semantic-readiness verdict for a single project bucket.
///
/// Loctree (and any other oracle) reads `IndexStatus::readiness` first to
/// decide whether semantic retrieval is safe. A `Pending` bucket means an
/// in-flight build crashed or is mid-rebuild — the only artifact on disk is
/// the `*.tmp` checkpoint, never atomically renamed into place, so it MUST
/// NOT be queried as if it were a complete corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexReadiness {
    /// No semantic index file present on disk (committed or temp).
    Missing,
    /// Only the `.ndjson.tmp` checkpoint exists; no committed final index.
    /// Treat as semantically unsafe — the embed loop never atomically
    /// committed, so the checkpoint may be torn or partial.
    Pending,
    /// Atomically committed final index exists at the canonical path. Safe
    /// to query. A temp checkpoint may coexist when a rebuild is in
    /// flight over a previously committed index.
    Ready,
    /// Source sessions are newer than the newest canonical chunk, so the
    /// sessions -> chunks stage has not caught up yet.
    StaleChunks,
    /// Canonical chunks exist that have not been represented in the committed
    /// semantic index yet.
    StaleIndex,
    /// Pending-corpus census exceeded its hard deadline (or was skipped as
    /// unbounded). Status still returns immediately with whatever is known
    /// from catalog/CURRENT; operators must not wait on silent source walks.
    PendingScanTimeout,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub canonical_chunks: usize,
    pub semantic_index_present: bool,
    pub semantic_index_path: Option<String>,
    pub semantic_index_rows: usize,
    pub newest_chunk_mtime: Option<String>,
    pub source_sessions: usize,
    pub newest_session_updated_at: Option<String>,
    pub sessions_newer_than_chunks: usize,
    pub sessions_without_timestamps: usize,
    pub chunking_lag_secs: Option<u64>,
    pub semantic_index_mtime: Option<String>,
    pub semantic_lag_secs: Option<u64>,
    pub pending_chunks: usize,
    pub temp_index_present: bool,
    pub temp_index_path: Option<String>,
    pub temp_index_rows: usize,
    pub temp_index_mtime: Option<String>,
    pub temp_index_bytes: Option<u64>,
    /// Truthful readiness verdict consumed by Loctree and other oracles.
    /// `Ready` only when the canonical final index is atomically present.
    pub readiness: IndexReadiness,
    /// Storage backend for the index file (currently always `"ndjson"`;
    /// changes when the Lance migration lands).
    pub backend: String,
    /// On-disk bucket name (the safe-bucketed project slug, or `"_all"`
    /// when the cross-project bucket is queried).
    pub project_bucket: String,
    /// RFC3339 timestamp of the committed final index, when present.
    /// Mirrors `semantic_index_mtime` under an explicit semantic name so
    /// MCP callers do not have to know that the mtime equals the commit
    /// time (true because `write_index` atomic-renames into place).
    pub committed_at: Option<String>,
    /// Lexical plane: `ready` when CURRENT has tantivy docs, else `missing`.
    #[serde(default = "default_lexical_status")]
    pub lexical_status: String,
    /// Dense plane: `ready` | `not_built` | `missing`.
    /// Orthogonal to `readiness` (catalog lag). Lexical-only CURRENT is
    /// normal; run `aicx index --semantic` on the owner host for dense.
    #[serde(default = "default_dense_status")]
    pub dense_status: String,
    /// Manifest dense_kind (`optional_not_built`, `exact_mmap_v1`, …).
    #[serde(default)]
    pub dense_kind: String,
    /// Dense row count from CURRENT manifest (0 when not built).
    #[serde(default)]
    pub dense_count: usize,
    /// Operator hint when dense is absent but lexical is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dense_recommendation: Option<String>,
}

#[allow(dead_code)] // serde(default = ...) for IndexStatus
fn default_lexical_status() -> String {
    "unknown".to_string()
}

#[allow(dead_code)] // serde(default = ...) for IndexStatus
fn default_dense_status() -> String {
    "unknown".to_string()
}

#[allow(dead_code)] // shared with residual IndexStatus constructors
fn plane_fields_for_missing() -> (String, String, String, usize, Option<String>) {
    (
        "missing".to_string(),
        "missing".to_string(),
        "missing".to_string(),
        0,
        Some("run `aicx catalog rebuild` then `aicx index` (lexical); opt in with `aicx index --semantic` for dense".to_string()),
    )
}

fn plane_fields_from_manifest(
    dense_kind: &str,
    dense_count: usize,
    lexical_docs: usize,
) -> (String, String, String, usize, Option<String>) {
    let lexical_status = if lexical_docs > 0 {
        "ready".to_string()
    } else {
        "missing".to_string()
    };
    let (dense_status, rec) = if dense_kind == "optional_not_built" || dense_count == 0 {
        (
            "not_built".to_string(),
            Some(
                "lexical CURRENT is enough for default search; run `aicx index --semantic` on the owner host for dense / `search --deep`"
                    .to_string(),
            ),
        )
    } else {
        ("ready".to_string(), None)
    };
    (
        lexical_status,
        dense_status,
        dense_kind.to_string(),
        dense_count,
        rec,
    )
}

pub fn index_status_at(base: &Path, project: Option<&str>) -> Result<IndexStatus> {
    index_status_at_with_sessions(base, project, None)
}

/// (project, source_mtime_ns) per catalog row — the census fingerprint is the
/// zero-walk freshness truth for status surfaces.
#[cfg(feature = "app")]
fn catalog_rows_for_status(base: &Path) -> Vec<(Option<String>, Option<u64>)> {
    crate::catalog::read_entries_at(base)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| (entry.project, entry.source_mtime_ns))
        .collect()
}

#[cfg(not(feature = "app"))]
fn catalog_rows_for_status(_base: &Path) -> Vec<(Option<String>, Option<u64>)> {
    // The slim library profile intentionally excludes source discovery and
    // catalog ingestion. Status remains bounded and reports legacy read-core
    // state without pulling the app graph into `loctree-consumer`.
    Vec::new()
}

/// RFC3339 of a unix-nanosecond mtime.
fn mtime_ns_to_rfc3339(mtime_ns: u64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp(
        (mtime_ns / 1_000_000_000) as i64,
        (mtime_ns % 1_000_000_000) as u32,
    )
    .map(|dt| dt.to_rfc3339())
}

fn index_status_at_with_sessions(
    base: &Path,
    project: Option<&str>,
    source_sessions_override: Option<&[SessionInfo]>,
) -> Result<IndexStatus> {
    // Prefer the CURRENT hybrid generation — the same surface `aicx search`
    // queries. Store cards + embeddings.ndjson are residual mill artifacts and
    // must not be reported as readiness truth when CURRENT exists.
    if let Some(status) = hybrid_current_index_status(base, project)? {
        return Ok(status);
    }

    let project_bucket = canonical_bucket_name(project);
    let semantic_index_path = semantic_index_path_for_bucket(base, &project_bucket);
    let temp_index_path = semantic_index_path.with_extension("ndjson.tmp");
    let semantic_index_mtime = semantic_index_path
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    let temp_metadata = temp_index_path.metadata().ok();
    let temp_index_mtime = temp_metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok());
    let temp_index_bytes = temp_metadata.as_ref().map(|metadata| metadata.len());
    let semantic_index_present = semantic_index_mtime.is_some();
    let temp_index_present = temp_index_mtime.is_some();

    // Boundedness first: when CURRENT/catalog/residual mill are all absent,
    // report Missing immediately. Never walk live agent trees (codex 42 GB,
    // claude 5.9 GB) just to print "missing" — audit measured >60 s hangs.
    let catalog_rows = catalog_rows_for_status(base);
    let catalog_entries: Vec<Option<String>> = catalog_rows
        .iter()
        .map(|(project, _)| project.clone())
        .collect();
    let residual_mill_present = residual_store_surface_present(base);
    if catalog_entries.is_empty()
        && !residual_mill_present
        && !semantic_index_present
        && !temp_index_present
        && source_sessions_override.is_none()
    {
        return Ok(IndexStatus {
            canonical_chunks: 0,
            semantic_index_present: false,
            semantic_index_path: None,
            semantic_index_rows: 0,
            newest_chunk_mtime: None,
            source_sessions: 0,
            newest_session_updated_at: None,
            sessions_newer_than_chunks: 0,
            sessions_without_timestamps: 0,
            chunking_lag_secs: None,
            semantic_index_mtime: None,
            semantic_lag_secs: None,
            pending_chunks: 0,
            temp_index_present: false,
            temp_index_path: None,
            temp_index_rows: 0,
            temp_index_mtime: None,
            temp_index_bytes: None,
            readiness: IndexReadiness::Missing,
            backend: "none".to_string(),
            project_bucket,
            committed_at: None,
            lexical_status: "missing".to_string(),
            dense_status: "missing".to_string(),
            dense_kind: "missing".to_string(),
            dense_count: 0,
            dense_recommendation: Some(
                "run `aicx catalog rebuild` then `aicx index`; opt in with `aicx index --semantic` for dense"
                    .to_string(),
            ),
        });
    }

    // Catalog-only path: durable session identity without residual mill walk.
    // Source-of-truth census of live agent roots is intentionally NOT done
    // here — that was the hang. Operators admit sources via `catalog rebuild`.
    if !catalog_entries.is_empty()
        && !residual_mill_present
        && !semantic_index_present
        && !temp_index_present
        && source_sessions_override.is_none()
    {
        let project_filter = project.map(str::trim).filter(|value| !value.is_empty());
        let catalog_in_scope = match project_filter {
            None => catalog_entries.len(),
            Some(filter) => catalog_entries
                .iter()
                .filter(|project| {
                    project
                        .as_deref()
                        .is_some_and(|p| p.eq_ignore_ascii_case(filter))
                })
                .count(),
        };
        let newest_session_mtime_ns = match project_filter {
            None => catalog_rows
                .iter()
                .filter_map(|(_, mtime_ns)| *mtime_ns)
                .max(),
            Some(filter) => catalog_rows
                .iter()
                .filter(|(project, _)| {
                    project
                        .as_deref()
                        .is_some_and(|p| p.eq_ignore_ascii_case(filter))
                })
                .filter_map(|(_, mtime_ns)| *mtime_ns)
                .max(),
        };
        return Ok(IndexStatus {
            canonical_chunks: 0,
            semantic_index_present: false,
            semantic_index_path: None,
            semantic_index_rows: 0,
            newest_chunk_mtime: None,
            source_sessions: catalog_in_scope,
            newest_session_updated_at: newest_session_mtime_ns.and_then(mtime_ns_to_rfc3339),
            sessions_newer_than_chunks: catalog_in_scope,
            sessions_without_timestamps: 0,
            chunking_lag_secs: None,
            semantic_index_mtime: None,
            semantic_lag_secs: None,
            pending_chunks: catalog_in_scope,
            temp_index_present: false,
            temp_index_path: None,
            temp_index_rows: 0,
            temp_index_mtime: None,
            temp_index_bytes: None,
            readiness: IndexReadiness::Missing,
            backend: "catalog_only".to_string(),
            project_bucket: project_filter
                .map(|filter| canonical_bucket_name(Some(filter)))
                .unwrap_or_else(|| "_all".to_string()),
            committed_at: None,
            lexical_status: "missing".to_string(),
            dense_status: "missing".to_string(),
            dense_kind: "missing".to_string(),
            dense_count: 0,
            dense_recommendation: Some(
                "run `aicx catalog rebuild` then `aicx index`; opt in with `aicx index --semantic` for dense"
                    .to_string(),
            ),
        });
    }

    let chunks = if residual_mill_present {
        crate::legacy_archive::scan_context_files_project_at(base, project).unwrap_or_default()
    } else {
        Vec::new()
    };
    let newest_chunk = chunks
        .iter()
        .filter_map(|chunk| chunk.path.metadata().ok()?.modified().ok())
        .max();

    // Prefer explicit override (tests) or catalog counts. Live agent-tree
    // discovery is last-resort and hard-deadline bounded.
    let discovered_sessions;
    let source_sessions = match source_sessions_override {
        Some(sessions) => sessions,
        None if !catalog_entries.is_empty() => {
            // Empty slice: chunking lag uses catalog counts injected below.
            discovered_sessions = Vec::new();
            &discovered_sessions
        }
        None => {
            discovered_sessions =
                discover_source_sessions_for_status_bounded(base, project, newest_chunk);
            &discovered_sessions
        }
    };
    let chunking = calculate_chunking_lag(source_sessions, newest_chunk);
    let semantic_index_rows = count_index_rows(&semantic_index_path)?;
    let temp_index_rows = count_index_rows(&temp_index_path)?;
    let semantic_lag_secs = match (newest_chunk, semantic_index_mtime) {
        (Some(chunk), Some(index)) => Some(
            chunk
                .duration_since(index)
                .unwrap_or(Duration::ZERO)
                .as_secs(),
        ),
        _ => None,
    };

    let pending_chunks = chunks.len().saturating_sub(semantic_index_rows);

    // When we skipped live discovery but have catalog rows, surface catalog
    // counts as source_sessions so status is still useful.
    let catalog_count = if source_sessions_override.is_none() && !catalog_entries.is_empty() {
        let project_filter = project.map(str::trim).filter(|value| !value.is_empty());
        match project_filter {
            None => catalog_entries.len(),
            Some(filter) => catalog_entries
                .iter()
                .filter(|project| {
                    project
                        .as_deref()
                        .is_some_and(|p| p.eq_ignore_ascii_case(filter))
                })
                .count(),
        }
    } else {
        chunking.source_sessions
    };
    let sessions_newer = if source_sessions_override.is_none() && !catalog_entries.is_empty() {
        catalog_count.saturating_sub(chunks.len())
    } else {
        chunking.sessions_newer_than_chunks
    };

    let readiness = match (semantic_index_present, temp_index_present) {
        (false, true) => IndexReadiness::Pending,
        (false, false) => IndexReadiness::Missing,
        (true, _) if sessions_newer > 0 => IndexReadiness::StaleChunks,
        (true, _) if pending_chunks > 0 || semantic_lag_secs.unwrap_or(0) > 0 => {
            IndexReadiness::StaleIndex
        }
        (true, _) => IndexReadiness::Ready,
    };
    let committed_at = semantic_index_mtime.map(system_time_to_rfc3339);

    Ok(IndexStatus {
        canonical_chunks: chunks.len(),
        semantic_index_present,
        semantic_index_path: semantic_index_present.then(|| path_for_json(&semantic_index_path)),
        semantic_index_rows,
        newest_chunk_mtime: newest_chunk.map(system_time_to_rfc3339),
        source_sessions: catalog_count,
        newest_session_updated_at: chunking
            .newest_session_updated_at
            .map(|value| value.to_rfc3339()),
        sessions_newer_than_chunks: sessions_newer,
        sessions_without_timestamps: chunking.sessions_without_timestamps,
        chunking_lag_secs: chunking.chunking_lag_secs,
        semantic_index_mtime: committed_at.clone(),
        semantic_lag_secs,
        pending_chunks,
        temp_index_present,
        temp_index_path: temp_index_present.then(|| path_for_json(&temp_index_path)),
        temp_index_rows,
        temp_index_mtime: temp_index_mtime.map(system_time_to_rfc3339),
        temp_index_bytes,
        readiness,
        backend: "ndjson".to_string(),
        project_bucket,
        committed_at,
        lexical_status: if semantic_index_present {
            "legacy_ndjson".to_string()
        } else {
            "missing".to_string()
        },
        dense_status: if semantic_index_present {
            "legacy_ndjson".to_string()
        } else {
            "missing".to_string()
        },
        dense_kind: "legacy_ndjson".to_string(),
        dense_count: semantic_index_rows,
        dense_recommendation: Some(
            "residual NDJSON mill path; prefer CURRENT hybrid via `aicx index` (+ `--semantic`)"
                .to_string(),
        ),
    })
}

/// True when residual per-frame mill dirs still exist under the AICX home.
fn residual_store_surface_present(base: &Path) -> bool {
    base.join(crate::legacy_archive::LEGACY_CARDS_DIRNAME)
        .is_dir()
        || base
            .join(crate::legacy_archive::NON_REPOSITORY_CONTEXTS)
            .is_dir()
}

/// Report readiness from hybrid CURRENT when it is the search truth surface.
///
/// Returns `Ok(None)` when no usable CURRENT generation exists under `base`,
/// so callers can fall back to residual store/NDJSON status for migration.
#[cfg(feature = "app")]
fn hybrid_current_index_status(base: &Path, project: Option<&str>) -> Result<Option<IndexStatus>> {
    // Source-driven publish always lands in the global `_all` bucket; project
    // is a search-time filter, not a separate generation.
    let hybrid_root = base.join("indexed").join("_all").join("hybrid");
    if !hybrid_root.is_dir() {
        return Ok(None);
    }
    let generation_dir = crate::vector_index::resolve_hybrid_generation_dir(&hybrid_root);
    let manifest_path = generation_dir.join("manifest.json");
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let manifest = match aicx_retrieve::Manifest::read_from_path(&manifest_path) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(None),
    };
    // Search uses CURRENT for any generation with a lexical corpus. Prefer the
    // source-driven lexical shape (`optional_not_built`) but also accept denser
    // generations so status and search never disagree about "is there an index".
    if manifest.lexical_doc_count == 0 {
        return Ok(None);
    }

    let catalog_rows = catalog_rows_for_status(base);
    let catalog_total = catalog_rows.len();
    let project_filter = project.map(str::trim).filter(|value| !value.is_empty());
    let in_scope: Vec<&(Option<String>, Option<u64>)> = match project_filter {
        None => catalog_rows.iter().collect(),
        Some(filter) => catalog_rows
            .iter()
            .filter(|(project, _)| {
                project
                    .as_deref()
                    .is_some_and(|project| project.eq_ignore_ascii_case(filter))
            })
            .collect(),
    };
    let catalog_in_scope = in_scope.len();

    // Census fingerprints are the zero-walk freshness truth: the newest
    // session mtime recorded at last rebuild, and how many in-scope sessions
    // are newer than this CURRENT generation. `<none>`/0 while agents are
    // visibly active was the P0 "freshness lie" — never fake these again.
    let newest_session_mtime_ns = in_scope.iter().filter_map(|(_, mtime_ns)| *mtime_ns).max();
    let build_completed_ns = manifest
        .build_completed_at
        .timestamp_nanos_opt()
        .map(|nanos| nanos as u64);
    let sessions_newer = match build_completed_ns {
        Some(build_ns) => in_scope
            .iter()
            .filter(|(_, mtime_ns)| mtime_ns.is_some_and(|mtime| mtime > build_ns))
            .count(),
        None => 0,
    };
    let chunking_lag_secs = match (newest_session_mtime_ns, build_completed_ns) {
        (Some(newest), Some(build)) if newest > build => Some((newest - build) / 1_000_000_000),
        _ => None,
    };

    let committed_at = Some(manifest.build_completed_at.to_rfc3339());
    let generation_mtime = manifest_path
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(system_time_to_rfc3339)
        .or_else(|| committed_at.clone());

    // Catalog rows beyond CURRENT lexical docs mean sessions are admitted but
    // not yet published — stale index relative to the durable catalog.
    let pending = catalog_total.saturating_sub(manifest.lexical_doc_count);
    let readiness = if sessions_newer > 0 {
        IndexReadiness::StaleChunks
    } else if pending > 0 {
        IndexReadiness::StaleIndex
    } else {
        IndexReadiness::Ready
    };

    let backend = if manifest.dense_kind == "optional_not_built" {
        "hybrid_lexical"
    } else {
        "hybrid"
    };

    let (lexical_status, dense_status, dense_kind, dense_count, dense_recommendation) =
        plane_fields_from_manifest(
            &manifest.dense_kind,
            manifest.dense_count,
            manifest.lexical_doc_count,
        );
    Ok(Some(IndexStatus {
        // For the extract-era store, "chunks" == signal session documents.
        canonical_chunks: manifest.lexical_doc_count,
        semantic_index_present: true,
        semantic_index_path: Some(path_for_json(&manifest_path)),
        semantic_index_rows: manifest.lexical_doc_count,
        newest_chunk_mtime: generation_mtime.clone(),
        source_sessions: catalog_in_scope,
        newest_session_updated_at: newest_session_mtime_ns.and_then(mtime_ns_to_rfc3339),
        sessions_newer_than_chunks: sessions_newer,
        sessions_without_timestamps: 0,
        chunking_lag_secs,
        semantic_index_mtime: generation_mtime,
        semantic_lag_secs: None,
        pending_chunks: pending,
        temp_index_present: false,
        temp_index_path: None,
        temp_index_rows: 0,
        temp_index_mtime: None,
        temp_index_bytes: None,
        readiness,
        backend: backend.to_string(),
        // Surface the requested filter label for operators, but the generation
        // is always the global CURRENT under `_all`.
        project_bucket: project_filter
            .map(|filter| canonical_bucket_name(Some(filter)))
            .unwrap_or_else(|| "_all".to_string()),
        committed_at,
        lexical_status,
        dense_status,
        dense_kind,
        dense_count,
        dense_recommendation,
    }))
}

/// Slim (`loctree-consumer`) builds carry no retrieval surface, so the hybrid
/// CURRENT manifest cannot be read; status falls back to residual store/NDJSON.
#[cfg(not(feature = "app"))]
fn hybrid_current_index_status(
    _base: &Path,
    _project: Option<&str>,
) -> Result<Option<IndexStatus>> {
    Ok(None)
}

#[derive(Debug, Clone)]
struct ChunkingLag {
    source_sessions: usize,
    newest_session_updated_at: Option<chrono::DateTime<chrono::Utc>>,
    sessions_newer_than_chunks: usize,
    sessions_without_timestamps: usize,
    chunking_lag_secs: Option<u64>,
}

/// Bounded residual-mill census. Hard deadline: never block status >2s on a
/// live agent-tree walk (operator home can hold tens of GB of sessions).
const STATUS_SESSION_SCAN_DEADLINE: Duration = Duration::from_secs(2);

fn discover_source_sessions_for_status_bounded(
    base: &Path,
    project: Option<&str>,
    newest_chunk: Option<SystemTime>,
) -> Vec<SessionInfo> {
    let Ok(active_store_root) = crate::aicx_home::ensure() else {
        return Vec::new();
    };
    if active_store_root != base {
        return Vec::new();
    }
    let Some(home) = crate::os_user_home() else {
        return Vec::new();
    };

    // Spawn the census on a helper thread and abandon it on deadline. Status
    // must return; a hung walk is worse than an empty pending set.
    let project = project.map(str::to_string);
    let (tx, rx) = std::sync::mpsc::channel();
    let home = home.clone();
    std::thread::spawn(move || {
        let sessions = sessions::discover_sessions_at(&home, newest_chunk, None, None)
            .into_iter()
            .filter(|session| status_session_matches_project(project.as_deref(), session))
            .collect::<Vec<_>>();
        let _ = tx.send(sessions);
    });
    // Deadline exceeded → empty vec; readiness already reflects on-disk
    // artifacts. The abandoned thread exits when discovery finishes.
    rx.recv_timeout(STATUS_SESSION_SCAN_DEADLINE)
        .unwrap_or_default()
}

fn status_session_matches_project(project: Option<&str>, session: &SessionInfo) -> bool {
    let Some(project) = project.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    if project == "_all" {
        return true;
    }

    let needle = project.to_ascii_lowercase();
    let slash_needle = needle.replace('_', "/");
    let repo_name = slash_needle
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or(&slash_needle);

    if session.project.as_deref().is_some_and(|value| {
        value.eq_ignore_ascii_case(project) || value.eq_ignore_ascii_case(repo_name)
    }) {
        return true;
    }

    session.repo_path.as_deref().is_some_and(|repo_path| {
        let normalized = repo_path.replace('\\', "/").to_ascii_lowercase();
        normalized == slash_needle
            || normalized.ends_with(&format!("/{repo_name}"))
            || normalized.contains(&format!("/{slash_needle}"))
    })
}

fn calculate_chunking_lag(
    source_sessions: &[SessionInfo],
    newest_chunk: Option<SystemTime>,
) -> ChunkingLag {
    let newest_chunk_at = newest_chunk.map(chrono::DateTime::<chrono::Utc>::from);
    let mut newest_session_updated_at: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut sessions_newer_than_chunks = 0usize;
    let mut sessions_without_timestamps = 0usize;

    for session in source_sessions {
        let Some(session_time) = session.updated_at.or(session.started_at) else {
            sessions_without_timestamps += 1;
            continue;
        };
        newest_session_updated_at =
            Some(newest_session_updated_at.map_or(session_time, |cur| cur.max(session_time)));
        if newest_chunk_at.is_none_or(|chunk_time| session_time > chunk_time) {
            sessions_newer_than_chunks += 1;
        }
    }

    let chunking_lag_secs = match (newest_session_updated_at, newest_chunk_at) {
        (Some(session_time), Some(chunk_time)) if session_time > chunk_time => {
            Some((session_time - chunk_time).num_seconds().max(0) as u64)
        }
        _ => None,
    };

    ChunkingLag {
        source_sessions: source_sessions.len(),
        newest_session_updated_at,
        sessions_newer_than_chunks,
        sessions_without_timestamps,
        chunking_lag_secs,
    }
}

fn canonical_bucket_name(project: Option<&str>) -> String {
    project
        .unwrap_or("_all")
        .chars()
        .map(|c| match c {
            '/' | '\\' => '_',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

fn semantic_index_path_for_bucket(base: &Path, bucket: &str) -> PathBuf {
    base.join("indexed").join(bucket).join("embeddings.ndjson")
}

fn count_index_rows(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let file = crate::sanitize::open_file_validated(path)
        .with_context(|| format!("open semantic index for status: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    if crate::sanitize::read_line_capped(&mut reader, crate::sanitize::MAX_VALIDATED_BYTES)?
        .is_none()
    {
        return Ok(0);
    }
    let mut rows = 0usize;
    while let Some(line) =
        crate::sanitize::read_line_capped(&mut reader, crate::sanitize::MAX_VALIDATED_BYTES)?
    {
        if !line.line.trim().is_empty() {
            rows += 1;
        }
    }
    Ok(rows)
}

fn path_for_json(path: &Path) -> String {
    path.display().to_string()
}

fn system_time_to_rfc3339(value: SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = value.into();
    datetime.to_rfc3339()
}

#[cfg(all(test, feature = "app"))]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn client_can_scan_empty_aicx_home() {
        let root = std::env::temp_dir().join(format!("aicx-api-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        let client = Aicx::with_aicx_home(&root);
        let chunks = client.list_chunks().expect("scan chunks");
        assert!(chunks.is_empty());

        let status = client.index_status(None).expect("index status");
        assert_eq!(status.canonical_chunks, 0);
        assert!(!status.semantic_index_present);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(feature = "app")]
    fn catalog_only_status_reports_newest_session_mtime_from_census_fingerprints() {
        let root = std::env::temp_dir().join(format!(
            "aicx-api-census-freshness-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let catalog_path = crate::catalog::sessions_path_for(&root);
        std::fs::create_dir_all(catalog_path.parent().expect("catalog parent"))
            .expect("create catalog dir");
        let older_ns: u64 = 1_753_000_000_000_000_000;
        let newer_ns: u64 = 1_753_600_000_000_000_000;
        let row = |session: &str, mtime_ns: u64| {
            serde_json::to_string(&crate::catalog::CatalogEntry {
                schema: crate::catalog::CATALOG_SCHEMA.to_string(),
                session_id: session.to_string(),
                agent: "claude".to_string(),
                project: Some("Loctree/aicx".to_string()),
                date: Some("2026-07-28".to_string()),
                cwd: None,
                source_path: format!("/tmp/{session}.jsonl"),
                source_len: Some(1),
                source_mtime_ns: Some(mtime_ns),
                title: None,
                machine: None,
                logical_session_id: None,
            })
            .expect("serialize row")
        };
        std::fs::write(
            &catalog_path,
            format!("{}\n{}\n", row("older", older_ns), row("newer", newer_ns)),
        )
        .expect("write census");

        let status = index_status_at(&root, None).expect("index status");
        let newest = status
            .newest_session_updated_at
            .expect("census fingerprints must surface a real newest_session_updated_at");
        let expected = chrono::DateTime::<chrono::Utc>::from_timestamp(
            (newer_ns / 1_000_000_000) as i64,
            (newer_ns % 1_000_000_000) as u32,
        )
        .expect("timestamp")
        .to_rfc3339();
        assert_eq!(
            newest, expected,
            "must pick the NEWEST in-scope fingerprint"
        );
        assert_eq!(status.backend, "catalog_only");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn index_status_reports_bucket_final_and_temp_indexes() {
        let root = std::env::temp_dir().join(format!(
            "aicx-api-index-status-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let all_dir = root.join("indexed").join("_all");
        let sibling_dir = root.join("indexed").join("vibecrafted");
        std::fs::create_dir_all(&all_dir).expect("create all index dir");
        std::fs::create_dir_all(&sibling_dir).expect("create sibling index dir");
        std::fs::write(
            all_dir.join("embeddings.ndjson"),
            "{\"schema_version\":\"1.0\"}\n{\"id\":\"a\"}\n{\"id\":\"b\"}\n",
        )
        .expect("write final index");
        std::fs::write(
            all_dir.join("embeddings.ndjson.tmp"),
            "{\"schema_version\":\"1.0\"}\n{\"id\":\"a\"}\n{\"id\":\"b\"}\n{\"id\":\"c\"}",
        )
        .expect("write temp index");
        std::fs::write(
            sibling_dir.join("embeddings.ndjson"),
            "{\"schema_version\":\"1.0\"}\n{\"id\":\"sibling\"}\n",
        )
        .expect("write sibling index");

        let status = index_status_at_with_sessions(&root, None, Some(&[])).expect("index status");

        assert!(status.semantic_index_present);
        assert_eq!(status.semantic_index_rows, 2);
        assert!(
            status
                .semantic_index_path
                .as_deref()
                // The reported path carries the OS separator; compare on the
                // canonical forward-slash form so `\indexed\_all\…` on Windows
                // still satisfies the `/`-literal suffix.
                .is_some_and(|path| path
                    .replace('\\', "/")
                    .ends_with("indexed/_all/embeddings.ndjson")),
            "status must report the _all query bucket, not sibling projects"
        );
        assert!(status.temp_index_present);
        assert_eq!(status.temp_index_rows, 3);
        assert!(status.temp_index_path.as_deref().is_some_and(|path| {
            path.replace('\\', "/")
                .ends_with("indexed/_all/embeddings.ndjson.tmp")
        }));
        assert_eq!(
            status.readiness,
            IndexReadiness::Ready,
            "committed final index must surface as Ready even with a coexisting rebuild checkpoint"
        );
        assert_eq!(status.backend, "ndjson");
        assert_eq!(status.project_bucket, "_all");
        assert!(
            status.committed_at.is_some(),
            "committed_at must mirror the final index mtime once the atomic commit landed"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn index_status_marks_pending_when_only_temp_checkpoint_exists() {
        let root = std::env::temp_dir().join(format!(
            "aicx-api-index-status-pending-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bucket_dir = root.join("indexed").join("_all");
        std::fs::create_dir_all(&bucket_dir).expect("create bucket dir");
        // Simulate a crashed embed loop: tmp checkpoint exists, atomic
        // rename never landed, so embeddings.ndjson is absent.
        std::fs::write(
            bucket_dir.join("embeddings.ndjson.tmp"),
            "{\"schema_version\":\"1.0\"}\n{\"id\":\"a\"}\n",
        )
        .expect("write temp index");

        let status = index_status_at_with_sessions(&root, None, Some(&[])).expect("index status");

        assert!(!status.semantic_index_present);
        assert!(status.temp_index_present);
        assert_eq!(
            status.readiness,
            IndexReadiness::Pending,
            "a lone temp checkpoint must surface as Pending so Loctree refuses semantic retrieval"
        );
        assert!(
            status.committed_at.is_none(),
            "committed_at must stay None when no atomic commit ever landed"
        );
        assert_eq!(status.project_bucket, "_all");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn index_status_marks_missing_when_no_artifact_exists() {
        let root = std::env::temp_dir().join(format!(
            "aicx-api-index-status-missing-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        let status = index_status_at_with_sessions(&root, None, Some(&[])).expect("index status");

        assert!(!status.semantic_index_present);
        assert!(!status.temp_index_present);
        assert_eq!(status.readiness, IndexReadiness::Missing);
        assert_eq!(status.backend, "ndjson");
        assert_eq!(status.project_bucket, "_all");
        assert!(status.committed_at.is_none());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn index_status_missing_is_immediate_without_live_source_census() {
        // Audit P1: empty AICX home must not hang scanning agent trees.
        let root = std::env::temp_dir().join(format!(
            "aicx-api-index-status-fast-missing-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        let started = std::time::Instant::now();
        // None override = production path (no forced empty session list).
        let status = index_status_at(&root, None).expect("index status");
        let wall = started.elapsed();

        assert_eq!(status.readiness, IndexReadiness::Missing);
        assert_eq!(status.backend, "none");
        assert_eq!(status.canonical_chunks, 0);
        assert!(
            wall < std::time::Duration::from_secs(2),
            "missing status must return immediately, got {wall:?}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn index_status_canonicalizes_project_bucket_slug() {
        let root = std::env::temp_dir().join(format!(
            "aicx-api-index-status-bucket-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        let status = index_status_at_with_sessions(&root, Some("vetcoders/Loctree"), Some(&[]))
            .expect("index status with project");

        // Mirrors the on-disk bucket: lowercase + path separators replaced.
        assert_eq!(status.project_bucket, "vetcoders_loctree");
        assert_eq!(status.readiness, IndexReadiness::Missing);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn index_status_serializes_readiness_as_snake_case_string() {
        let status = IndexStatus {
            canonical_chunks: 0,
            semantic_index_present: false,
            semantic_index_path: None,
            semantic_index_rows: 0,
            newest_chunk_mtime: None,
            source_sessions: 0,
            newest_session_updated_at: None,
            sessions_newer_than_chunks: 0,
            sessions_without_timestamps: 0,
            chunking_lag_secs: None,
            semantic_index_mtime: None,
            semantic_lag_secs: None,
            pending_chunks: 0,
            temp_index_present: true,
            temp_index_path: Some("/tmp/_all/embeddings.ndjson.tmp".to_string()),
            temp_index_rows: 1,
            temp_index_mtime: None,
            temp_index_bytes: Some(64),
            readiness: IndexReadiness::Pending,
            backend: "ndjson".to_string(),
            project_bucket: "_all".to_string(),
            committed_at: None,
            lexical_status: "missing".to_string(),
            dense_status: "missing".to_string(),
            dense_kind: "missing".to_string(),
            dense_count: 0,
            dense_recommendation: Some(
                "run `aicx catalog rebuild` then `aicx index`; opt in with `aicx index --semantic` for dense"
                    .to_string(),
            ),
        };

        let payload: serde_json::Value =
            serde_json::to_value(&status).expect("status should serialize");
        assert_eq!(payload["readiness"], "pending");
        assert_eq!(payload["backend"], "ndjson");
        assert_eq!(payload["project_bucket"], "_all");
        assert!(payload["committed_at"].is_null());
        assert_eq!(payload["temp_index_rows"], 1);
    }

    #[test]
    fn index_status_prefers_hybrid_current_over_ndjson_mill() {
        let root = std::env::temp_dir().join(format!(
            "aicx-api-hybrid-status-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        // Residual mill NDJSON that must NOT win status when CURRENT exists.
        let ndjson_dir = root.join("indexed").join("_all");
        std::fs::create_dir_all(&ndjson_dir).expect("ndjson dir");
        std::fs::write(
            ndjson_dir.join("embeddings.ndjson"),
            "{\"schema_version\":\"1.0\"}\n{\"id\":\"stale-mill-row\"}\n",
        )
        .expect("write residual ndjson");

        let gen_name = "g-2026-07-23T10-00-00Z-teststatus";
        let gen_dir = root
            .join("indexed")
            .join("_all")
            .join("hybrid")
            .join("generations")
            .join(gen_name);
        std::fs::create_dir_all(&gen_dir).expect("generation dir");
        let manifest = aicx_retrieve::Manifest {
            schema_version: "2.0".to_string(),
            generation_id: "g-2026-07-23T10:00:00Z-teststatus".to_string(),
            writer_version: "0.12.1".to_string(),
            build_id: "0.12.1+gtestbead".to_string(),
            source_chunk_count: 3,
            source_hash_blake3: "abc".to_string(),
            embedder_model: "optional".to_string(),
            embedder_url_hash: "not_built".to_string(),
            embedder_dim: 0,
            embedder_distance: "cosine".to_string(),
            dense_count: 0,
            dense_kind: "optional_not_built".to_string(),
            lexical_commit_id: "tantivy_test".to_string(),
            lexical_doc_count: 3,
            build_started_at: Utc.with_ymd_and_hms(2026, 7, 23, 10, 0, 0).unwrap(),
            build_completed_at: Utc.with_ymd_and_hms(2026, 7, 23, 10, 0, 12).unwrap(),
            build_wall_seconds: 12,
            fusion_algorithm: "rrf".to_string(),
            fusion_k: 60,
        };
        manifest
            .write_to_path(&gen_dir.join("manifest.json"))
            .expect("write hybrid manifest");
        std::fs::write(
            root.join("indexed")
                .join("_all")
                .join("hybrid")
                .join("CURRENT"),
            format!("{gen_name}\n"),
        )
        .expect("write CURRENT pointer");

        let status = index_status_at(&root, None).expect("hybrid status");
        assert_eq!(status.backend, "hybrid_lexical");
        assert_eq!(status.readiness, IndexReadiness::Ready);
        assert_eq!(status.semantic_index_rows, 3);
        assert_eq!(status.canonical_chunks, 3);
        assert!(
            status
                .semantic_index_path
                .as_deref()
                .is_some_and(|path| path.contains("manifest.json")
                    && path.contains("generations")
                    && !path.contains("embeddings.ndjson")),
            "status must point at CURRENT generation, not residual ndjson: {:?}",
            status.semantic_index_path
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
