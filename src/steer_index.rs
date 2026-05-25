//! BM25 + LanceDB steer index for fast session retrieval.
//!
//! The steer index is a dual-layer search structure over the canonical store:
//! a BM25 text index for keyword ranking and a LanceDB vector store for
//! metadata-filtered recall.  Public functions delegate to the store base
//! directory discovered at runtime, keeping callers free of path logic.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::Result;
use chrono::{DateTime, Utc};
use rmcp_memex::{
    search::{BM25Config, BM25Index},
    storage::{ChromaDocument, SCHEMA_VERSION, StorageManager},
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::progress::{FailureLog, NoopReporter, Phase, Reporter, recovery_hint_for};
use crate::timeline::FrameKind;

const STEER_NAMESPACE: &str = "steer";
const STEER_BM25_DIR: &str = "steer_bm25";
const STEER_METADATA_FILE: &str = "steer_index_meta.json";
const STEER_NEXT_DIR: &str = ".steer.next";
const STEER_PREV_DIR: &str = ".steer.prev";
const STEER_INDEX_METADATA_VERSION: u32 = 1;
const STEER_SENTINEL_DIMENSION: usize = 1;
const MIN_CANDIDATES: usize = 200;
const CANDIDATE_MULTIPLIER: usize = 20;

#[cfg(test)]
type TestHook = Arc<dyn Fn() + Send + Sync + 'static>;

#[cfg(test)]
static STEER_READ_LOCK_HOOK: std::sync::OnceLock<std::sync::Mutex<Option<TestHook>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
static STEER_REBUILD_SWAP_HOOK: std::sync::OnceLock<std::sync::Mutex<Option<TestHook>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
fn call_steer_read_lock_hook() {
    let hook = STEER_READ_LOCK_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("steer read hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(not(test))]
fn call_steer_read_lock_hook() {}

#[cfg(test)]
fn call_steer_rebuild_swap_hook() {
    let hook = STEER_REBUILD_SWAP_HOOK
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .expect("steer rebuild hook lock poisoned")
        .clone();
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(not(test))]
fn call_steer_rebuild_swap_hook() {}

trait Bm25CandidateHit {
    fn into_hit(self) -> (String, f32);
}

impl Bm25CandidateHit for (String, f32) {
    fn into_hit(self) -> (String, f32) {
        self
    }
}

impl Bm25CandidateHit for (String, String, f32) {
    fn into_hit(self) -> (String, f32) {
        let (id, _namespace, score) = self;
        (id, score)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SteerIndexMetadata {
    format_version: u32,
    namespace: String,
    db_path: String,
    bm25_path: String,
    vector_dimension: usize,
    storage_schema_version: u32,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteerIncompatible {
    RebuildRequired { reason: String },
    NotBootstrapped { reason: String },
}

impl SteerIncompatible {
    fn rebuild_required(reason: impl Into<String>) -> Self {
        Self::RebuildRequired {
            reason: reason.into(),
        }
    }

    fn not_bootstrapped(reason: impl Into<String>) -> Self {
        Self::NotBootstrapped {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for SteerIncompatible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RebuildRequired { reason } => {
                write!(f, "steer index requires rebuild: {reason}")
            }
            Self::NotBootstrapped { reason } => {
                write!(f, "steer index is not bootstrapped: {reason}")
            }
        }
    }
}

impl std::error::Error for SteerIncompatible {}

fn warn_if_steer_incompatible(err: &anyhow::Error) {
    if let Some(incompatible) = err.downcast_ref::<SteerIncompatible>() {
        tracing::warn!("{incompatible}; run `aicx doctor --rebuild-steer-index`");
    }
}

fn is_steer_incompatible(err: &anyhow::Error) -> bool {
    err.downcast_ref::<SteerIncompatible>().is_some()
}

fn chunk_id_for_path(file: &Path) -> String {
    file.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn steer_db_path(base: &Path) -> PathBuf {
    base.join("steer_db")
}

fn steer_bm25_path(base: &Path) -> PathBuf {
    base.join(STEER_BM25_DIR)
}

fn steer_metadata_path(base: &Path) -> PathBuf {
    base.join(STEER_METADATA_FILE)
}

fn steer_lock_path_at(base: &Path) -> PathBuf {
    base.join("locks").join("steer.lock")
}

fn steer_bm25_config(base: &Path, read_only: bool) -> BM25Config {
    BM25Config::multilingual()
        .with_path(steer_bm25_path(base).to_string_lossy().to_string())
        .with_read_only(read_only)
}

fn load_steer_metadata(base: &Path) -> Option<SteerIndexMetadata> {
    let raw = fs::read_to_string(steer_metadata_path(base)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn steer_metadata_matches_current(base: &Path, metadata: &SteerIndexMetadata) -> bool {
    metadata.format_version == STEER_INDEX_METADATA_VERSION
        && metadata.namespace == STEER_NAMESPACE
        && metadata.db_path == steer_db_path(base).display().to_string()
        && metadata.bm25_path == steer_bm25_path(base).display().to_string()
        && metadata.vector_dimension == STEER_SENTINEL_DIMENSION
        && metadata.storage_schema_version == SCHEMA_VERSION
}

fn write_steer_metadata(base: &Path) -> Result<()> {
    let metadata = SteerIndexMetadata {
        format_version: STEER_INDEX_METADATA_VERSION,
        namespace: STEER_NAMESPACE.to_string(),
        db_path: steer_db_path(base).display().to_string(),
        bm25_path: steer_bm25_path(base).display().to_string(),
        vector_dimension: STEER_SENTINEL_DIMENSION,
        storage_schema_version: SCHEMA_VERSION,
        updated_at: Utc::now(),
    };

    fs::write(
        steer_metadata_path(base),
        serde_json::to_vec_pretty(&metadata)?,
    )?;
    Ok(())
}

async fn detect_steer_index_dimension_at(base: &Path) -> Result<Option<usize>> {
    let db_path = steer_db_path(base);
    if !db_path.exists() {
        return Ok(None);
    }

    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    Ok(storage
        .all_documents(Some(STEER_NAMESPACE), 1)
        .await?
        .into_iter()
        .next()
        .map(|doc| doc.embedding.len()))
}

fn push_unique_term(terms: &mut Vec<String>, term: String) {
    if !term.is_empty() && !terms.iter().any(|existing| existing == &term) {
        terms.push(term);
    }
}

fn searchable_terms(value: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let lower = value.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return terms;
    }

    push_unique_term(&mut terms, lower.clone());

    let compact: String = lower
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if !compact.is_empty() {
        push_unique_term(&mut terms, compact);
    }

    for token in lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
    {
        push_unique_term(&mut terms, token.to_string());
    }

    terms
}

fn add_searchable_value(terms: &mut Vec<String>, label: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };

    for term in searchable_terms(value) {
        push_unique_term(terms, term.clone());
        push_unique_term(terms, format!("{label}:{term}"));
    }
}

fn add_query_value(terms: &mut Vec<String>, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };

    for term in searchable_terms(value) {
        push_unique_term(terms, term);
    }
}

fn build_steer_metadata(file: &Path) -> serde_json::Value {
    let sidecar = crate::store::load_sidecar(file);

    let mut meta = serde_json::Map::new();
    meta.insert(
        "path".to_string(),
        serde_json::Value::String(file.display().to_string()),
    );
    if let Some(s) = sidecar
        && let Ok(val) = serde_json::to_value(s)
        && let Some(obj) = val.as_object()
    {
        for (k, v) in obj {
            meta.insert(k.clone(), v.clone());
        }
    }

    serde_json::Value::Object(meta)
}

fn build_steer_search_text(meta: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut terms = Vec::new();

    add_searchable_value(
        &mut terms,
        "project",
        meta.get("project").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "agent",
        meta.get("agent").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "kind",
        meta.get("kind").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "frame_kind",
        meta.get("frame_kind").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "date",
        meta.get("date").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "run_id",
        meta.get("run_id").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "prompt_id",
        meta.get("prompt_id").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "session_id",
        meta.get("session_id").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "workflow_phase",
        meta.get("workflow_phase").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "mode",
        meta.get("mode").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "skill_code",
        meta.get("skill_code").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "framework_version",
        meta.get("framework_version").and_then(|v| v.as_str()),
    );

    terms.join(" ")
}

fn build_steer_doc(file: &Path) -> ChromaDocument {
    let metadata = build_steer_metadata(file);
    let text = metadata
        .as_object()
        .map(build_steer_search_text)
        .unwrap_or_default();

    ChromaDocument::new_flat(
        chunk_id_for_path(file),
        STEER_NAMESPACE.to_string(),
        vec![0.0; STEER_SENTINEL_DIMENSION], // Explicit sentinel vector for metadata-only index
        metadata,
        text,
    )
}

fn doc_ids(docs: &[ChromaDocument]) -> HashSet<String> {
    docs.iter().map(|doc| doc.id.clone()).collect()
}

fn file_ids(files: &[crate::store::StoredContextFile]) -> HashSet<String> {
    files
        .iter()
        .map(|file| chunk_id_for_path(&file.path))
        .collect()
}

fn steer_index_needs_rebuild(existing_ids: &HashSet<String>, store_ids: &HashSet<String>) -> bool {
    existing_ids != store_ids
}

fn build_steer_docs(new_files: &[&PathBuf]) -> Vec<ChromaDocument> {
    new_files
        .iter()
        .map(|file| build_steer_doc(file.as_path()))
        .collect()
}

async fn sync_steer_bm25_at(base: &Path, docs: &[ChromaDocument]) -> Result<()> {
    if docs.is_empty() {
        return Ok(());
    }

    let bm25 = BM25Index::new(&steer_bm25_config(base, false))?;
    let ids: Vec<String> = docs.iter().map(|doc| doc.id.clone()).collect();
    let _ = bm25.delete_documents(&ids).await;

    let bm25_docs: Vec<(String, String, String)> = docs
        .iter()
        .map(|doc| {
            (
                doc.id.clone(),
                STEER_NAMESPACE.to_string(),
                doc.document.clone(),
            )
        })
        .collect();
    bm25.add_documents(&bm25_docs).await?;

    Ok(())
}

async fn sync_steer_index_at(base: &Path, new_files: &[&PathBuf]) -> Result<()> {
    let reporter: Arc<dyn Reporter> = Arc::new(NoopReporter);
    let failures = FailureLog::new();
    sync_steer_index_at_with_reporter(base, new_files, reporter, &failures).await
}

/// Instrumented variant: emits separate `steer_sync` and `bm25_sync`
/// Phase events through the supplied reporter and records phase
/// failures into `failures` before propagating the error. Existing
/// callers reach this via the no-op shim above.
async fn sync_steer_index_at_with_reporter(
    base: &Path,
    new_files: &[&PathBuf],
    reporter: Arc<dyn Reporter>,
    failures: &FailureLog,
) -> Result<()> {
    sync_steer_index_at_with_reporter_and_filter_base(base, base, new_files, reporter, failures)
        .await
}

async fn sync_steer_index_at_with_reporter_and_filter_base(
    index_base: &Path,
    filter_base: &Path,
    new_files: &[&PathBuf],
    reporter: Arc<dyn Reporter>,
    failures: &FailureLog,
) -> Result<()> {
    let db_path = steer_db_path(index_base);
    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    storage.ensure_collection().await?;

    let (filtered_paths, _) = crate::store::filter_ignored_paths_at(filter_base, new_files)?;
    let filtered_refs: Vec<&PathBuf> = filtered_paths.iter().collect();
    let docs = build_steer_docs(&filtered_refs);

    if docs.is_empty() {
        return Ok(());
    }

    let total_docs = docs.len() as u64;

    let steer_phase = Phase::start(reporter.clone(), "steer_sync", Some(total_docs));
    let lance_result: Result<()> = async {
        let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
        for id in ids {
            let _ = storage.delete_document(STEER_NAMESPACE, id).await;
        }

        let mut written: u64 = 0;
        for chunk in docs.chunks(1000) {
            storage.add_to_store(chunk.to_vec()).await?;
            written += chunk.len() as u64;
            steer_phase.tick(written);
        }
        Ok(())
    }
    .await;

    match lance_result {
        Ok(()) => {
            steer_phase.finish_ok(format!("{total_docs} docs"));
        }
        Err(e) => {
            let record = steer_phase.finish_err(&e, recovery_hint_for("steer_sync"));
            failures.record(record);
            return Err(e);
        }
    }

    let bm25_phase = Phase::start(reporter.clone(), "bm25_sync", Some(total_docs));
    match sync_steer_bm25_at(index_base, &docs).await {
        Ok(()) => {
            bm25_phase.finish_ok(format!("{total_docs} docs"));
        }
        Err(e) => {
            let record = bm25_phase.finish_err(&e, recovery_hint_for("bm25_sync"));
            failures.record(record);
            return Err(e);
        }
    }

    write_steer_metadata(index_base)?;
    Ok(())
}

fn remove_dir_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn clear_steer_index_artifacts_at(base: &Path) -> Result<()> {
    remove_dir_if_exists(&steer_db_path(base))?;
    remove_dir_if_exists(&steer_bm25_path(base))?;
    remove_file_if_exists(&steer_metadata_path(base))?;
    remove_dir_if_exists(&base.join(STEER_NEXT_DIR))?;
    remove_dir_if_exists(&base.join(STEER_PREV_DIR))?;
    Ok(())
}

fn rename_if_exists(from: &Path, to: &Path) -> Result<bool> {
    if !from.exists() {
        return Ok(false);
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, to)?;
    Ok(true)
}

fn restore_if_missing(from: &Path, to: &Path) {
    if from.exists() && !to.exists() {
        let _ = fs::rename(from, to);
    }
}

async fn rebuild_all_steer_index_at(
    base: &Path,
    all_files: &[crate::store::StoredContextFile],
) -> Result<()> {
    let next_base = base.join(STEER_NEXT_DIR);
    let prev_base = base.join(STEER_PREV_DIR);
    remove_dir_if_exists(&next_base)?;
    remove_dir_if_exists(&prev_base)?;
    fs::create_dir_all(&next_base)?;

    let paths: Vec<PathBuf> = all_files.iter().map(|file| file.path.clone()).collect();
    let path_refs: Vec<&PathBuf> = paths.iter().collect();
    let reporter: Arc<dyn Reporter> = Arc::new(NoopReporter);
    let failures = FailureLog::new();
    sync_steer_index_at_with_reporter_and_filter_base(
        &next_base, base, &path_refs, reporter, &failures,
    )
    .await?;

    call_steer_rebuild_swap_hook();

    let db_path = steer_db_path(base);
    let bm25_path = steer_bm25_path(base);
    let meta_path = steer_metadata_path(base);
    let prev_db_path = prev_base.join("steer_db");
    let prev_bm25_path = prev_base.join(STEER_BM25_DIR);
    let prev_meta_path = prev_base.join(STEER_METADATA_FILE);
    let next_db_path = steer_db_path(&next_base);
    let next_bm25_path = steer_bm25_path(&next_base);

    remove_dir_if_exists(&prev_base)?;
    fs::create_dir_all(&prev_base)?;
    rename_if_exists(&db_path, &prev_db_path)?;
    rename_if_exists(&bm25_path, &prev_bm25_path)?;
    rename_if_exists(&meta_path, &prev_meta_path)?;

    let swap_result: Result<()> = (|| {
        rename_if_exists(&next_db_path, &db_path)?;
        rename_if_exists(&next_bm25_path, &bm25_path)?;
        write_steer_metadata(base)?;
        Ok(())
    })();

    if let Err(err) = swap_result {
        restore_if_missing(&prev_db_path, &db_path);
        restore_if_missing(&prev_bm25_path, &bm25_path);
        restore_if_missing(&prev_meta_path, &meta_path);
        return Err(err);
    }

    remove_dir_if_exists(&next_base)?;
    remove_dir_if_exists(&prev_base)?;
    Ok(())
}

async fn query_steer_index_at(base: &Path) -> Result<Vec<ChromaDocument>> {
    let db_path = steer_db_path(base);
    if !db_path.exists() {
        return Ok(vec![]);
    }
    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    storage.get_all_in_namespace(STEER_NAMESPACE).await
}

async fn bootstrap_steer_index_if_missing_at(base: &Path) -> Result<bool> {
    let files = crate::store::scan_context_files_at(base)?;
    if files.is_empty() {
        return Ok(false);
    }

    let expected_docs = files.len();
    let bm25_path = steer_bm25_path(base);
    if !bm25_path.exists() {
        let incompatible = SteerIncompatible::not_bootstrapped(format!(
            "BM25 index is missing (store has {expected_docs} files)"
        ));
        tracing::warn!("{incompatible}; run `aicx doctor --rebuild-steer-index`");
        return Err(incompatible.into());
    }

    let bm25 = BM25Index::new(&steer_bm25_config(base, true))?;
    let bm25_docs = bm25.doc_count() as usize;

    if bm25_docs == expected_docs {
        return Ok(false);
    }

    let incompatible = SteerIncompatible::not_bootstrapped(format!(
        "BM25 index has {bm25_docs} docs but store has {expected_docs} files"
    ));
    tracing::warn!("{incompatible}; run `aicx doctor --rebuild-steer-index`");
    Err(incompatible.into())
}

async fn ensure_steer_index_compatible_at(base: &Path) -> Result<()> {
    let actual_dimension = detect_steer_index_dimension_at(base).await?;

    match actual_dimension {
        Some(actual_dimension) if actual_dimension != STEER_SENTINEL_DIMENSION => {
            return Err(SteerIncompatible::rebuild_required(format!(
                "stored vectors use {actual_dimension} dims, expected {STEER_SENTINEL_DIMENSION}"
            ))
            .into());
        }
        Some(_) => {
            let metadata_ok = load_steer_metadata(base)
                .as_ref()
                .is_some_and(|metadata| steer_metadata_matches_current(base, metadata));
            if !metadata_ok {
                return Err(
                    SteerIncompatible::rebuild_required("metadata is missing or stale").into(),
                );
            }
        }
        None => {
            let files = crate::store::scan_context_files_at(base)?;
            if files.is_empty() {
                return Ok(());
            }
            return Err(SteerIncompatible::not_bootstrapped(format!(
                "LanceDB steer index is missing (store has {} files)",
                files.len()
            ))
            .into());
        }
    }

    Ok(())
}

async fn ensure_steer_index_compatible_for_write_at(base: &Path) -> Result<()> {
    match ensure_steer_index_compatible_at(base).await {
        Ok(()) => Ok(()),
        Err(err) => {
            let Some(incompatible) = err.downcast_ref::<SteerIncompatible>().cloned() else {
                return Err(err);
            };

            let files = crate::store::scan_context_files_at(base)?;
            if files.is_empty() {
                tracing::info!("Clearing empty steer index after {incompatible}");
                clear_steer_index_artifacts_at(base)?;
                return Ok(());
            }

            tracing::info!("Rebuilding steer index after {incompatible}");
            rebuild_all_steer_index_at(base, &files).await
        }
    }
}

/// Steer index filter — all 8 optional filters in one bag so helpers don't
/// need long argument lists and callers can build the filter once.
#[derive(Debug, Default, Clone)]
pub struct SteerFilter<'a> {
    pub run_id: Option<&'a str>,
    pub prompt_id: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub frame_kind: Option<FrameKind>,
    pub project: Option<&'a str>,
    pub date_lo: Option<&'a str>,
    pub date_hi: Option<&'a str>,
}

fn build_candidate_query(filter: &SteerFilter<'_>) -> Option<String> {
    let mut terms = Vec::new();

    add_query_value(&mut terms, filter.project);
    add_query_value(&mut terms, filter.agent);
    add_query_value(&mut terms, filter.kind);
    add_query_value(&mut terms, filter.frame_kind.map(FrameKind::as_str));
    add_query_value(&mut terms, filter.run_id);
    add_query_value(&mut terms, filter.prompt_id);

    if matches!((filter.date_lo, filter.date_hi), (Some(lo), Some(hi)) if lo == hi) {
        add_query_value(&mut terms, filter.date_lo);
    }

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

fn metadata_matches(meta: &serde_json::Value, filter: &SteerFilter<'_>) -> bool {
    let agent_lower = filter.agent.map(str::to_ascii_lowercase);
    let kind_lower = filter.kind.map(str::to_ascii_lowercase);

    // Strict canonical project filter: split the stored `<owner>/<repo>`
    // slug and delegate to `aicx::store::project_filter_matches` so the
    // steer-index path agrees with `aicx search`, dashboard, refs/since,
    // and rank. Substring fallback (`-p vista` matching `vista-portal`,
    // `vista-datasets`, etc.) is intentionally removed — Bug #29.
    if let Some(needle) = filter.project {
        let needle = needle.trim();
        if !needle.is_empty() {
            if let Some(stored) = meta.get("project").and_then(|v| v.as_str()) {
                let (organization, repository) = stored.split_once('/').unwrap_or(("", stored));
                if !crate::store::project_filter_matches(organization, repository, needle) {
                    return false;
                }
            } else {
                return false;
            }
        }
    }
    if let Some(ref needle) = agent_lower {
        if let Some(a) = meta.get("agent").and_then(|v| v.as_str()) {
            if a.to_ascii_lowercase() != *needle {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(ref needle) = kind_lower {
        if let Some(k) = meta.get("kind").and_then(|v| v.as_str()) {
            if k.to_ascii_lowercase() != *needle {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(expected) = filter.frame_kind
        && meta.get("frame_kind").and_then(|v| v.as_str()) != Some(expected.as_str())
    {
        return false;
    }
    if let Some(lo) = filter.date_lo {
        if let Some(d) = meta.get("date").and_then(|v| v.as_str()) {
            if d < lo {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(hi) = filter.date_hi {
        if let Some(d) = meta.get("date").and_then(|v| v.as_str()) {
            if d > hi {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(wanted) = filter.run_id
        && meta.get("run_id").and_then(|v| v.as_str()) != Some(wanted)
    {
        return false;
    }
    if let Some(wanted) = filter.prompt_id
        && meta.get("prompt_id").and_then(|v| v.as_str()) != Some(wanted)
    {
        return false;
    }

    true
}

fn build_store_scan_metadata(file: &crate::store::StoredContextFile) -> serde_json::Value {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "path".to_string(),
        serde_json::Value::String(file.path.display().to_string()),
    );
    meta.insert(
        "project".to_string(),
        serde_json::Value::String(file.project.clone()),
    );
    meta.insert(
        "agent".to_string(),
        serde_json::Value::String(file.agent.clone()),
    );
    meta.insert(
        "date".to_string(),
        serde_json::Value::String(file.date_iso.clone()),
    );
    meta.insert(
        "session_id".to_string(),
        serde_json::Value::String(file.session_id.clone()),
    );
    meta.insert(
        "kind".to_string(),
        serde_json::Value::String(file.kind.dir_name().to_string()),
    );

    if let Some(sidecar) = crate::store::load_sidecar(&file.path)
        && let Ok(val) = serde_json::to_value(sidecar)
        && let Some(obj) = val.as_object()
    {
        for (key, value) in obj {
            meta.insert(key.clone(), value.clone());
        }
    }

    serde_json::Value::Object(meta)
}

fn search_store_scan_at(
    base: &Path,
    filter: &SteerFilter<'_>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let files = crate::store::scan_context_files_at(base)?;
    let mut results = Vec::new();

    for file in files.into_iter().rev() {
        let meta = build_store_scan_metadata(&file);
        if !metadata_matches(&meta, filter) {
            continue;
        }

        results.push(meta);
        if results.len() >= limit {
            break;
        }
    }

    Ok(results)
}

async fn search_bm25_candidates_at(
    base: &Path,
    filter: &SteerFilter<'_>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let Some(query) = build_candidate_query(filter) else {
        return Ok(vec![]);
    };

    if !steer_bm25_path(base).exists() {
        bootstrap_steer_index_if_missing_at(base).await?;
        return Ok(vec![]);
    }

    let bm25 = BM25Index::new(&steer_bm25_config(base, true))?;
    if bm25.doc_count() == 0 {
        bootstrap_steer_index_if_missing_at(base).await?;
        return Ok(vec![]);
    }

    let candidate_limit = (limit.saturating_mul(CANDIDATE_MULTIPLIER)).max(MIN_CANDIDATES);
    let hits = bm25.search(&query, Some(STEER_NAMESPACE), candidate_limit)?;
    if hits.is_empty() {
        return Ok(vec![]);
    }

    let db_path = steer_db_path(base);
    if !db_path.exists() {
        return Ok(vec![]);
    }

    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    let mut seen_ids = HashSet::new();
    let mut results = Vec::new();

    for hit in hits {
        let (id, _score) = hit.into_hit();
        if !seen_ids.insert(id.clone()) {
            continue;
        }

        let Some(doc) = storage.get_document(STEER_NAMESPACE, &id).await? else {
            continue;
        };

        if !metadata_matches(&doc.metadata, filter) {
            continue;
        }

        results.push(doc.metadata);
        if results.len() >= limit {
            break;
        }
    }

    Ok(results)
}

async fn rebuild_steer_index_if_needed_at(base: &Path) -> Result<()> {
    ensure_steer_index_compatible_for_write_at(base).await?;

    let all_files = crate::store::scan_context_files_at(base)?;
    if all_files.is_empty() {
        clear_steer_index_artifacts_at(base)?;
        return Ok(());
    }

    let existing_docs = query_steer_index_at(base).await.unwrap_or_default();
    let existing_ids = doc_ids(&existing_docs);
    let store_ids = file_ids(&all_files);
    let bm25_needs_rebuild = BM25Index::new(&steer_bm25_config(base, true))
        .map(|index| index.doc_count() as usize != store_ids.len())
        .unwrap_or(true);

    if steer_index_needs_rebuild(&existing_ids, &store_ids) || bm25_needs_rebuild {
        tracing::info!(
            "Rebuilding steer index ({} docs vs {} files, bm25 stale: {})",
            existing_ids.len(),
            store_ids.len(),
            bm25_needs_rebuild
        );

        rebuild_all_steer_index_at(base, &all_files).await?;
    }

    Ok(())
}

/// Builds or updates the fast steer index using rmcp-memex LanceDB backend.
/// Treats the sidecar as the source of truth for every touched chunk.
pub async fn sync_steer_index(new_files: &[&PathBuf]) -> Result<()> {
    if new_files.is_empty() {
        return Ok(());
    }

    let base = crate::store::store_base_dir()?;
    let _lock = crate::locks::acquire_exclusive(crate::locks::steer_lock_path()?)?;
    ensure_steer_index_compatible_for_write_at(&base).await?;
    sync_steer_index_at(&base, new_files).await
}

/// Instrumented variant of [`sync_steer_index`] that emits Phase events
/// (`steer_sync` and `bm25_sync`) through `reporter` and pushes any
/// phase failure into `failures` before propagating the error to the
/// caller. The existing [`sync_steer_index`] entry point keeps its
/// signature and behavior; new code paths that want progress visibility
/// should call this variant.
pub async fn sync_steer_index_with_progress(
    new_files: &[&PathBuf],
    reporter: Arc<dyn Reporter>,
    failures: &FailureLog,
) -> Result<()> {
    if new_files.is_empty() {
        return Ok(());
    }

    let base = crate::store::store_base_dir()?;
    let _lock = crate::locks::acquire_exclusive(crate::locks::steer_lock_path()?)?;
    ensure_steer_index_compatible_for_write_at(&base).await?;
    sync_steer_index_at_with_reporter(&base, new_files, reporter, failures).await
}

pub async fn query_steer_index_count() -> Result<usize> {
    let base = crate::store::store_base_dir()?;
    let _lock = crate::locks::acquire_shared(crate::locks::steer_lock_path()?)?;
    call_steer_read_lock_hook();
    if let Err(err) = ensure_steer_index_compatible_at(&base).await {
        warn_if_steer_incompatible(&err);
        if is_steer_incompatible(&err) {
            return Ok(0);
        }
        return Err(err);
    }
    let docs = query_steer_index_at(&base).await?;
    Ok(docs.len())
}

pub async fn try_rebuild_steer_index_if_needed_at(base: &Path) -> Result<()> {
    fs::create_dir_all(base)?;
    let _lock = crate::locks::acquire_exclusive(steer_lock_path_at(base))?;
    rebuild_steer_index_if_needed_at(base).await
}

pub async fn rebuild_steer_index_if_needed() -> Result<()> {
    let base = crate::store::store_base_dir()?;
    try_rebuild_steer_index_if_needed_at(&base).await
}

pub async fn search_steer_index(
    filter: &SteerFilter<'_>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let base = crate::store::store_base_dir()?;
    let _lock = crate::locks::acquire_shared(crate::locks::steer_lock_path()?)?;
    call_steer_read_lock_hook();
    if let Err(err) = ensure_steer_index_compatible_at(&base).await {
        warn_if_steer_incompatible(&err);
        if is_steer_incompatible(&err) {
            return Ok(vec![]);
        }
        return Err(err);
    }

    let candidate_results = match search_bm25_candidates_at(&base, filter, limit).await {
        Ok(results) => results,
        Err(err) => {
            warn_if_steer_incompatible(&err);
            if is_steer_incompatible(&err) {
                return Ok(vec![]);
            }
            return Err(err);
        }
    };

    if candidate_results.len() >= limit || !candidate_results.is_empty() {
        return Ok(candidate_results);
    }

    search_store_scan_at(&base, filter, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunker::ChunkMetadataSidecar;
    use crate::store::Kind;
    use serde_json::json;
    use std::ffi::OsString;
    use std::fs;
    use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static AICX_HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    struct AicxHomeGuard {
        previous: Option<OsString>,
        dir: PathBuf,
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for AicxHomeGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => {
                    // SAFETY: tests that mutate AICX_HOME are serialized by
                    // AICX_HOME_LOCK and all spawned workers are joined before drop.
                    unsafe { std::env::set_var("AICX_HOME", previous) };
                }
                None => {
                    // SAFETY: tests that mutate AICX_HOME are serialized by
                    // AICX_HOME_LOCK and all spawned workers are joined before drop.
                    unsafe { std::env::remove_var("AICX_HOME") };
                }
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn set_temp_aicx_home(label: &str) -> AicxHomeGuard {
        let guard = AICX_HOME_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("AICX_HOME test lock");
        let dir = unique_test_dir(label);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp AICX_HOME");
        let previous = std::env::var_os("AICX_HOME");
        // SAFETY: guarded by AICX_HOME_LOCK for the full lifetime of this guard.
        unsafe { std::env::set_var("AICX_HOME", &dir) };
        AicxHomeGuard {
            previous,
            dir,
            _guard: guard,
        }
    }

    struct HookGuard {
        cell: &'static OnceLock<Mutex<Option<TestHook>>>,
    }

    impl Drop for HookGuard {
        fn drop(&mut self) {
            *self
                .cell
                .get_or_init(|| Mutex::new(None))
                .lock()
                .expect("hook lock poisoned") = None;
        }
    }

    fn install_hook(
        cell: &'static OnceLock<Mutex<Option<TestHook>>>,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> HookGuard {
        *cell
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("hook lock poisoned") = Some(Arc::new(hook));
        HookGuard { cell }
    }

    fn wait_flag(pair: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, ready) = &**pair;
        let mut value = lock.lock().expect("flag lock");
        while !*value {
            value = ready.wait(value).expect("flag wait");
        }
    }

    fn set_flag(pair: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, ready) = &**pair;
        *lock.lock().expect("flag lock") = true;
        ready.notify_all();
    }

    fn path_count(path: &Path) -> usize {
        if !path.exists() {
            return 0;
        }
        let mut count = 1;
        if path.is_dir() {
            for entry in fs::read_dir(path).expect("read dir") {
                count += path_count(&entry.expect("dir entry").path());
            }
        }
        count
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("aicx-steer-{label}-{}-{nanos}", std::process::id()))
    }

    fn write_store_chunk(base: &Path) -> PathBuf {
        let dir = base
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0405")
            .join("reports")
            .join("codex");
        fs::create_dir_all(&dir).expect("create canonical store");

        let chunk_path = dir.join("2026_0405_codex_session123_001.md");
        fs::write(&chunk_path, "# report\n\nembedding migration").expect("write chunk");
        fs::write(
            chunk_path.with_extension("meta.json"),
            serde_json::to_vec_pretty(&ChunkMetadataSidecar {
                id: "chunk-1".to_string(),
                project: "VetCoders/ai-contexters".to_string(),
                agent: "codex".to_string(),
                date: "2026-04-05".to_string(),
                session_id: "session123".to_string(),
                cwd: Some("/Users/maciejgad/vc-workspace/VetCoders/ai-contexters".to_string()),
                timestamp_source: None,
                kind: Kind::Reports,
                run_id: Some("impl-055522".to_string()),
                prompt_id: Some("20260405_045135".to_string()),
                frame_kind: Some(FrameKind::AgentReply),
                speaker_hint: None,
                agent_model: Some("gpt-5".to_string()),
                started_at: None,
                completed_at: None,
                token_usage: None,
                findings_count: None,
                workflow_phase: Some("implementation".to_string()),
                mode: None,
                skill_code: None,
                framework_version: Some("2026-04".to_string()),
                intent_entries: Vec::new(),
                tags: Vec::new(),
                artifact_family: None,
                schema_version: None,
                truth_status: None,
                learning_use: None,
                keywords: None,
                content_sha256: None,
                noise_lines_dropped: 0,
            })
            .expect("serialize sidecar"),
        )
        .expect("write sidecar");

        chunk_path
    }

    fn write_chunk_with_sidecar(
        base: &Path,
        file_name: &str,
        run_id: &str,
        prompt_id: &str,
    ) -> PathBuf {
        let chunk_path = base
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0331")
            .join("reports")
            .join("codex")
            .join(file_name);
        fs::create_dir_all(chunk_path.parent().unwrap()).unwrap();
        fs::write(&chunk_path, "# chunk\n\nbody").unwrap();

        let sidecar = ChunkMetadataSidecar {
            id: chunk_id_for_path(&chunk_path),
            project: "VetCoders/ai-contexters".to_string(),
            agent: "codex".to_string(),
            date: "2026-03-31".to_string(),
            session_id: "sess-1".to_string(),
            cwd: Some("/Users/tester/workspaces/ai-contexters".to_string()),
            timestamp_source: None,
            kind: Kind::Reports,
            run_id: Some(run_id.to_string()),
            prompt_id: Some(prompt_id.to_string()),
            frame_kind: Some(FrameKind::AgentReply),
            speaker_hint: None,
            agent_model: Some("gpt-5.4".to_string()),
            started_at: Some("2026-03-31T16:00:00Z".to_string()),
            completed_at: Some("2026-03-31T16:05:00Z".to_string()),
            token_usage: Some(1200),
            findings_count: Some(2),
            workflow_phase: Some("marbles".to_string()),
            mode: Some("session-first".to_string()),
            skill_code: Some("vc-marbles".to_string()),
            framework_version: Some("2026-03".to_string()),
            intent_entries: Vec::new(),
            tags: Vec::new(),
            artifact_family: None,
            schema_version: None,
            truth_status: None,
            learning_use: None,
            keywords: None,
            content_sha256: None,
            noise_lines_dropped: 0,
        };

        fs::write(
            chunk_path.with_extension("meta.json"),
            serde_json::to_string(&sidecar).unwrap(),
        )
        .unwrap();

        chunk_path
    }

    #[test]
    fn rebuild_detects_small_id_drift() {
        let existing_ids = HashSet::from([
            "2026_0331_codex_sess1_001".to_string(),
            "2026_0331_codex_sess1_002".to_string(),
        ]);
        let store_ids = HashSet::from([
            "2026_0331_codex_sess1_001".to_string(),
            "2026_0331_codex_sess2_001".to_string(),
        ]);

        assert!(steer_index_needs_rebuild(&existing_ids, &store_ids));
    }

    #[test]
    fn writer_repairs_incompatible_vector_dimension() {
        let base = unique_test_dir("rebuild");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).expect("create temp root");
        let chunk_path = write_store_chunk(&base);

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async {
            let storage = StorageManager::new_lance_only(&steer_db_path(&base).to_string_lossy())
                .await
                .expect("open steer db");
            storage
                .add_to_store(vec![ChromaDocument::new_flat(
                    "legacy-steer".to_string(),
                    STEER_NAMESPACE.to_string(),
                    vec![0.0; 8],
                    json!({"path": chunk_path.display().to_string()}),
                    "legacy steer".to_string(),
                )])
                .await
                .expect("insert legacy steer document");

            ensure_steer_index_compatible_for_write_at(&base)
                .await
                .expect("compatibility repair should succeed");

            let docs = query_steer_index_at(&base)
                .await
                .expect("query repaired steer index");
            assert_eq!(docs.len(), 1);
            assert_eq!(docs[0].embedding.len(), STEER_SENTINEL_DIMENSION);
            assert_eq!(docs[0].id, "2026_0405_codex_session123_001");
        });

        let metadata = load_steer_metadata(&base).expect("steer metadata should exist");
        assert!(steer_metadata_matches_current(&base, &metadata));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_search_steer_index_takes_shared_lock() {
        let home = set_temp_aicx_home("search-shared-lock");
        let chunk_path = write_store_chunk(&home.dir);
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(sync_steer_index_at(&home.dir, &[&chunk_path]))
            .expect("build steer index");

        let acquired = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let acquired_for_hook = acquired.clone();
        let release_for_hook = release.clone();
        let _hook = install_hook(&STEER_READ_LOCK_HOOK, move || {
            set_flag(&acquired_for_hook);
            wait_flag(&release_for_hook);
        });

        let worker = thread::spawn(|| {
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            rt.block_on(async {
                let filter = SteerFilter {
                    run_id: Some("impl-055522"),
                    ..SteerFilter::default()
                };
                search_steer_index(&filter, 10).await
            })
        });

        wait_flag(&acquired);
        let err = crate::locks::acquire_exclusive_with_timeout(
            steer_lock_path_at(&home.dir),
            Duration::from_millis(75),
        )
        .expect_err("exclusive lock should wait while search holds shared lock");
        assert!(err.to_string().contains("timed out"));
        set_flag(&release);

        let results = worker
            .join()
            .expect("search worker")
            .expect("search should complete");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_query_steer_index_count_does_not_mutate_under_shared() {
        let home = set_temp_aicx_home("query-no-mutate");
        let chunk_path = write_store_chunk(&home.dir);
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let storage =
                StorageManager::new_lance_only(&steer_db_path(&home.dir).to_string_lossy())
                    .await
                    .expect("open steer db");
            storage
                .add_to_store(vec![ChromaDocument::new_flat(
                    "legacy-steer".to_string(),
                    STEER_NAMESPACE.to_string(),
                    vec![0.0; 8],
                    json!({"path": chunk_path.display().to_string()}),
                    "legacy steer".to_string(),
                )])
                .await
                .expect("insert legacy steer document");
        });
        let before_count = path_count(&steer_db_path(&home.dir));

        let count = rt
            .block_on(query_steer_index_count())
            .expect("query should degrade to an empty count");
        assert_eq!(count, 0);
        assert_eq!(path_count(&steer_db_path(&home.dir)), before_count);

        let docs = rt
            .block_on(query_steer_index_at(&home.dir))
            .expect("read legacy docs");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].id, "legacy-steer");
        assert_eq!(docs[0].embedding.len(), 8);
    }

    #[test]
    fn test_incompatible_index_during_search_returns_diagnostic() {
        let home = set_temp_aicx_home("search-incompatible");
        let chunk_path = write_store_chunk(&home.dir);
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async {
            let storage =
                StorageManager::new_lance_only(&steer_db_path(&home.dir).to_string_lossy())
                    .await
                    .expect("open steer db");
            storage
                .add_to_store(vec![ChromaDocument::new_flat(
                    "legacy-steer".to_string(),
                    STEER_NAMESPACE.to_string(),
                    vec![0.0; 8],
                    json!({"path": chunk_path.display().to_string()}),
                    "legacy steer".to_string(),
                )])
                .await
                .expect("insert legacy steer document");
        });

        let results = rt
            .block_on(async {
                let filter = SteerFilter {
                    run_id: Some("impl-055522"),
                    ..SteerFilter::default()
                };
                search_steer_index(&filter, 10).await
            })
            .expect("search should degrade to empty results");
        assert!(results.is_empty());

        let docs = rt
            .block_on(query_steer_index_at(&home.dir))
            .expect("read legacy docs");
        assert_eq!(docs[0].id, "legacy-steer");
        assert_eq!(docs[0].embedding.len(), 8);
    }

    #[test]
    fn test_two_parallel_search_calls_on_missing_index_do_not_double_rebuild() {
        let home = set_temp_aicx_home("parallel-missing");
        write_store_chunk(&home.dir);

        let worker = || {
            thread::spawn(|| {
                let rt = tokio::runtime::Runtime::new().expect("runtime");
                rt.block_on(async {
                    let filter = SteerFilter {
                        run_id: Some("impl-055522"),
                        ..SteerFilter::default()
                    };
                    search_steer_index(&filter, 10).await
                })
            })
        };

        let first = worker();
        let second = worker();
        for result in [
            first.join().expect("first worker"),
            second.join().expect("second worker"),
        ] {
            let results = result.expect("missing index should degrade to empty results");
            assert!(results.is_empty());
        }

        assert!(!steer_db_path(&home.dir).exists());
        assert!(!steer_bm25_path(&home.dir).exists());
    }

    #[test]
    fn test_rebuild_atomic_swap_does_not_expose_partial_state() {
        let home = set_temp_aicx_home("atomic-swap");
        let first_chunk =
            write_chunk_with_sidecar(&home.dir, "2026_0331_codex_sess1_001.md", "mrbl-001", "p1");
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(sync_steer_index_at(&home.dir, &[&first_chunk]))
            .expect("build initial steer index");

        let second_chunk =
            write_chunk_with_sidecar(&home.dir, "2026_0331_codex_sess1_002.md", "mrbl-002", "p2");
        assert!(second_chunk.exists());

        let staged = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let staged_for_hook = staged.clone();
        let release_for_hook = release.clone();
        let _hook = install_hook(&STEER_REBUILD_SWAP_HOOK, move || {
            set_flag(&staged_for_hook);
            wait_flag(&release_for_hook);
        });

        let base = home.dir.clone();
        let writer = thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            rt.block_on(try_rebuild_steer_index_if_needed_at(&base))
        });

        wait_flag(&staged);
        assert!(steer_db_path(&home.dir).exists());
        assert!(steer_db_path(&home.dir.join(STEER_NEXT_DIR)).exists());
        let err = crate::locks::acquire_shared_with_timeout(
            steer_lock_path_at(&home.dir),
            Duration::from_millis(75),
        )
        .expect_err("reader should not enter while writer is staged");
        assert!(err.to_string().contains("timed out"));
        set_flag(&release);

        writer
            .join()
            .expect("writer thread")
            .expect("writer rebuild should finish");
        assert!(!home.dir.join(STEER_NEXT_DIR).exists());
        assert!(!home.dir.join(STEER_PREV_DIR).exists());

        let docs = rt
            .block_on(query_steer_index_at(&home.dir))
            .expect("query rebuilt steer index");
        assert_eq!(docs.len(), 2);
        assert!(
            docs.iter()
                .all(|doc| doc.embedding.len() == STEER_SENTINEL_DIMENSION)
        );
        let metadata = load_steer_metadata(&home.dir).expect("metadata");
        assert!(steer_metadata_matches_current(&home.dir, &metadata));
    }

    #[test]
    fn sync_replaces_existing_sidecar_metadata() {
        let temp = std::env::temp_dir().join(format!(
            "ai-ctx-steer-index-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&temp).unwrap();

        let chunk_path =
            write_chunk_with_sidecar(&temp, "2026_0331_codex_sess1_001.md", "mrbl-001", "p1");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let first_refs = vec![&chunk_path];
        rt.block_on(sync_steer_index_at(&temp, &first_refs))
            .unwrap();

        let mut updated_sidecar = crate::store::load_sidecar(&chunk_path).unwrap();
        updated_sidecar.run_id = Some("mrbl-002".to_string());
        updated_sidecar.prompt_id = Some("p2".to_string());
        fs::write(
            chunk_path.with_extension("meta.json"),
            serde_json::to_string(&updated_sidecar).unwrap(),
        )
        .unwrap();

        let second_refs = vec![&chunk_path];
        rt.block_on(sync_steer_index_at(&temp, &second_refs))
            .unwrap();

        let docs = rt.block_on(query_steer_index_at(&temp)).unwrap();
        assert_eq!(docs.len(), 1);
        assert!(docs[0].document.contains("run_id:mrbl"));
        assert_eq!(
            docs[0].metadata.get("run_id").and_then(|v| v.as_str()),
            Some("mrbl-002")
        );
        assert_eq!(
            docs[0].metadata.get("prompt_id").and_then(|v| v.as_str()),
            Some("p2")
        );

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn store_scan_metadata_falls_back_to_path_fields() {
        let temp = std::env::temp_dir().join(format!(
            "ai-ctx-steer-scan-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let chunk_dir = temp
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0331")
            .join("reports")
            .join("codex");
        fs::create_dir_all(&chunk_dir).unwrap();
        let chunk_path = chunk_dir.join("2026_0331_codex_sess1_001.md");
        fs::write(&chunk_path, "# chunk\n").unwrap();

        let files = crate::store::scan_context_files_at(&temp).unwrap();
        let meta = build_store_scan_metadata(&files[0]);
        assert_eq!(
            meta.get("project").and_then(|v| v.as_str()),
            Some("VetCoders/ai-contexters")
        );
        assert_eq!(meta.get("agent").and_then(|v| v.as_str()), Some("codex"));
        assert_eq!(meta.get("kind").and_then(|v| v.as_str()), Some("reports"));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn candidate_query_uses_filter_terms() {
        let filter = SteerFilter {
            run_id: Some("mrbl-001"),
            agent: Some("claude"),
            kind: Some("reports"),
            project: Some("VetCoders/vibecrafted"),
            ..SteerFilter::default()
        };
        let query = build_candidate_query(&filter).unwrap();

        assert!(query.contains("mrbl"));
        assert!(query.contains("claude"));
        assert!(query.contains("vibecrafted"));
    }

    #[test]
    fn metadata_matches_project_filter_is_strict_not_substring() {
        // Bug #29: steer-index candidate filter used to substring-match
        // `-p vista` against `vista-portal`. It now routes through the
        // canonical `aicx::store::project_filter_matches`, so the bare
        // name `vista` must NOT match a `vetcoders/vista-portal` slug.
        let meta = json!({ "project": "vetcoders/vista-portal" });
        let filter = SteerFilter {
            project: Some("vista"),
            ..SteerFilter::default()
        };
        assert!(
            !metadata_matches(&meta, &filter),
            "strict matcher must reject `vista` against `vetcoders/vista-portal`"
        );

        // Canonical strict slug still matches its exact target.
        let meta_exact = json!({ "project": "Loctree/aicx" });
        let filter_exact = SteerFilter {
            project: Some("Loctree/aicx"),
            ..SteerFilter::default()
        };
        assert!(
            metadata_matches(&meta_exact, &filter_exact),
            "exact slug must still match the canonical project"
        );

        // And the substring sibling `Loctree/aicx-portal` must NOT match
        // `Loctree/aicx` either — same strict-equality rule for slugs.
        let meta_sibling = json!({ "project": "Loctree/aicx-portal" });
        assert!(
            !metadata_matches(&meta_sibling, &filter_exact),
            "strict matcher must reject `Loctree/aicx` against `Loctree/aicx-portal`"
        );
    }
}
