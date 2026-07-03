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

use crate::chunker::ChunkerConfig;
#[cfg(feature = "app")]
use crate::doctor::{DoctorOptions, DoctorReport};
use crate::intents::{IntentExtraction, IntentsConfig};
#[cfg(feature = "app")]
use crate::rank::FuzzyResult;
use crate::sessions::{self, SessionInfo};
use crate::store::{ReadContextChunk, StoreWriteSummary, StoredContextFile};
#[cfg(feature = "app")]
use crate::timeline::FrameKind;
use crate::timeline::TimelineEntry;

/// Configuration for an [`Aicx`] library handle.
#[derive(Debug, Clone)]
pub struct AicxConfig {
    /// AICX base directory. Defaults to `~/.aicx`.
    pub store_root: PathBuf,
}

impl AicxConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            store_root: crate::store::store_base_dir()?,
        })
    }

    pub fn with_store_root(path: impl Into<PathBuf>) -> Self {
        Self {
            store_root: path.into(),
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

    pub fn with_store_root(path: impl Into<PathBuf>) -> Self {
        Self {
            config: AicxConfig::with_store_root(path),
        }
    }

    pub fn config(&self) -> &AicxConfig {
        &self.config
    }

    pub fn store_root(&self) -> &Path {
        &self.config.store_root
    }

    pub fn list_chunks(&self) -> Result<Vec<StoredContextFile>> {
        crate::store::scan_context_files_at(&self.config.store_root)
    }

    pub fn read_chunk(
        &self,
        reference: impl AsRef<str>,
        max_chars: Option<usize>,
    ) -> Result<ReadContextChunk> {
        crate::store::read_context_chunk_at(&self.config.store_root, reference.as_ref(), max_chars)
    }

    pub fn store_entries(
        &self,
        entries: &[TimelineEntry],
        opts: &StoreOptions,
    ) -> Result<StoreWriteSummary> {
        self.store_entries_with_progress(entries, opts, |_, _| {})
    }

    pub fn store_entries_with_progress<F>(
        &self,
        entries: &[TimelineEntry],
        opts: &StoreOptions,
        progress: F,
    ) -> Result<StoreWriteSummary>
    where
        F: FnMut(usize, usize),
    {
        crate::store::store_semantic_segments_at(
            &self.config.store_root,
            entries,
            &opts.chunker,
            progress,
        )
    }

    /// Run a semantic search against the canonical store's persistent vector
    /// index. Fails fast with a descriptive error when any precondition is
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
        let project_scopes_owned = search_project_scopes(&self.config.store_root, &owned_projects)?;
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
            &self.config.store_root,
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
            &self.config.store_root,
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
            &self.config.store_root,
            chrono::Utc::now(),
        )
    }

    #[cfg(feature = "app")]
    pub async fn doctor(&self, opts: &DoctorOptions) -> Result<DoctorReport> {
        crate::doctor::run_at(&self.config.store_root, opts).await
    }

    pub fn index_status(&self, project: Option<&str>) -> Result<IndexStatus> {
        index_status_at(&self.config.store_root, project)
    }
}

#[derive(Debug, Clone, Default)]
pub struct StoreOptions {
    pub chunker: ChunkerConfig,
}

#[derive(Debug, Clone)]
#[cfg(feature = "app")]
pub struct SearchOptions {
    pub limit: usize,
    pub projects: Vec<String>,
    pub project: Option<String>,
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
            frame_kind: None,
            kind: None,
        }
    }
}

#[cfg(feature = "app")]
fn search_project_scopes(store_root: &Path, projects: &[String]) -> Result<Vec<Option<String>>> {
    if projects.is_empty() {
        return Ok(vec![None]);
    }
    let resolved =
        crate::store::resolve_filters_to_store_or_index_slugs_at_or_error(store_root, projects)?;
    Ok(resolved.into_iter().map(Some).collect())
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
}

pub fn index_status_at(base: &Path, project: Option<&str>) -> Result<IndexStatus> {
    index_status_at_with_sessions(base, project, None)
}

fn index_status_at_with_sessions(
    base: &Path,
    project: Option<&str>,
    source_sessions_override: Option<&[SessionInfo]>,
) -> Result<IndexStatus> {
    let chunks = crate::store::scan_context_files_project_at(base, project)?;
    let newest_chunk = chunks
        .iter()
        .filter_map(|chunk| chunk.path.metadata().ok()?.modified().ok())
        .max();

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
    let discovered_sessions;
    let source_sessions = match source_sessions_override {
        Some(sessions) => sessions,
        None => {
            discovered_sessions = discover_source_sessions_for_status(base, project, newest_chunk);
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

    let readiness = match (semantic_index_present, temp_index_present) {
        (false, true) => IndexReadiness::Pending,
        (false, false) => IndexReadiness::Missing,
        (true, _) if chunking.sessions_newer_than_chunks > 0 => IndexReadiness::StaleChunks,
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
        source_sessions: chunking.source_sessions,
        newest_session_updated_at: chunking
            .newest_session_updated_at
            .map(|value| value.to_rfc3339()),
        sessions_newer_than_chunks: chunking.sessions_newer_than_chunks,
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
    })
}

#[derive(Debug, Clone)]
struct ChunkingLag {
    source_sessions: usize,
    newest_session_updated_at: Option<chrono::DateTime<chrono::Utc>>,
    sessions_newer_than_chunks: usize,
    sessions_without_timestamps: usize,
    chunking_lag_secs: Option<u64>,
}

fn discover_source_sessions_for_status(
    base: &Path,
    project: Option<&str>,
    newest_chunk: Option<SystemTime>,
) -> Vec<SessionInfo> {
    let Ok(active_store_root) = crate::store::store_base_dir() else {
        return Vec::new();
    };
    if active_store_root != base {
        return Vec::new();
    }
    let Some(home) = crate::os_user_home() else {
        return Vec::new();
    };

    sessions::discover_sessions_at(&home, newest_chunk, None, None)
        .into_iter()
        .filter(|session| status_session_matches_project(project, session))
        .collect()
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
    fn client_can_scan_empty_store_root() {
        let root = std::env::temp_dir().join(format!("aicx-api-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        let client = Aicx::with_store_root(&root);
        let chunks = client.list_chunks().expect("scan chunks");
        assert!(chunks.is_empty());

        let status = client.index_status(None).expect("index status");
        assert_eq!(status.canonical_chunks, 0);
        assert!(!status.semantic_index_present);

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
    fn index_status_canonicalizes_project_bucket_slug() {
        let root = std::env::temp_dir().join(format!(
            "aicx-api-index-status-bucket-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        let status = index_status_at_with_sessions(&root, Some("Vetcoders/Loctree"), Some(&[]))
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
    fn index_status_marks_stale_chunks_when_sessions_are_newer_than_chunks() {
        let root = std::env::temp_dir().join(format!(
            "aicx-api-index-status-stale-chunks-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        let older = Utc.with_ymd_and_hms(2026, 6, 11, 12, 33, 0).unwrap();
        let newer = Utc.with_ymd_and_hms(2026, 6, 12, 9, 28, 0).unwrap();
        let entry = TimelineEntry {
            timestamp: older,
            agent: "claude".to_string(),
            session_id: "old-session".to_string(),
            role: "user".to_string(),
            message: "old chunk".to_string(),
            frame_kind: Some(FrameKind::UserMsg),
            branch: None,
            cwd: Some("/Users/me/vc-workspace/vetcoders/aicx".to_string()),
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
        };
        let summary = crate::store::store_semantic_segments_at(
            &root,
            &[entry],
            &ChunkerConfig::default(),
            |_, _| {},
        )
        .expect("store chunk");
        let index_dir = root.join("indexed").join("_all");
        std::fs::create_dir_all(&index_dir).expect("create index dir");
        let index_path = index_dir.join("embeddings.ndjson");
        std::fs::write(
            &index_path,
            "{\"schema_version\":\"1.0\"}\n{\"id\":\"old\"}\n",
        )
        .expect("write index");

        let older_file_time = filetime::FileTime::from_unix_time(older.timestamp(), 0);
        for path in &summary.written_paths {
            filetime::set_file_mtime(path, older_file_time).expect("set chunk mtime");
        }
        filetime::set_file_mtime(&index_path, older_file_time).expect("set index mtime");

        let session = SessionInfo {
            session_id: "fresh".to_string(),
            agent: "claude".to_string(),
            project: Some("aicx".to_string()),
            repo_path: Some("/Users/me/vc-workspace/vetcoders/aicx".to_string()),
            started_at: Some(newer),
            updated_at: Some(newer),
            message_count: 1,
            user_message_count: 1,
            agent_message_count: 0,
            title: Some("fresh work".to_string()),
            source_path: root.join("fresh.jsonl"),
            association: crate::sessions::Association::Exact,
            temporal_confidence: crate::sessions::TemporalConfidence::Full,
        };

        let status =
            index_status_at_with_sessions(&root, None, Some(&[session])).expect("index status");

        assert_eq!(status.source_sessions, 1);
        assert_eq!(status.sessions_newer_than_chunks, 1);
        assert_eq!(status.sessions_without_timestamps, 0);
        assert_eq!(status.newest_session_updated_at, Some(newer.to_rfc3339()));
        assert_eq!(status.readiness, IndexReadiness::StaleChunks);
        assert!(status.chunking_lag_secs.unwrap() > 0);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn client_can_store_entries_without_cli_globals() {
        let root = std::env::temp_dir().join(format!("aicx-api-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create root");

        let client = Aicx::with_store_root(&root);
        let entries = vec![TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 5, 6, 16, 0, 0).unwrap(),
            agent: "codex".to_string(),
            session_id: "api-lib-session".to_string(),
            role: "user".to_string(),
            message: "Decision: expose AICX as a real library surface.".to_string(),
            frame_kind: Some(FrameKind::UserMsg),
            branch: None,
            cwd: None,
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
        }];

        let summary = client
            .store_entries(&entries, &StoreOptions::default())
            .expect("store entries through public facade");

        assert_eq!(summary.total_entries, 1);
        assert_eq!(summary.written_paths.len(), 1);
        assert!(summary.written_paths[0].starts_with(root.join("non-repository-contexts")));
        assert_eq!(client.list_chunks().expect("scan chunks").len(), 1);

        let _ = std::fs::remove_dir_all(root);
    }
}
