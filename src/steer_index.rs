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
use std::fs;
use std::path::{Path, PathBuf};

use crate::types::FrameKind;

const STEER_NAMESPACE: &str = "steer";
const STEER_BM25_DIR: &str = "steer_bm25";
const STEER_METADATA_FILE: &str = "steer_index_meta.json";
const STEER_INDEX_METADATA_VERSION: u32 = 1;
const STEER_SENTINEL_DIMENSION: usize = 1;
const MIN_CANDIDATES: usize = 200;
const CANDIDATE_MULTIPLIER: usize = 20;

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
    let db_path = steer_db_path(base);
    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    storage.ensure_collection().await?;

    let (filtered_paths, _) = crate::store::filter_ignored_paths_at(base, new_files)?;
    let filtered_refs: Vec<&PathBuf> = filtered_paths.iter().collect();
    let docs = build_steer_docs(&filtered_refs);

    if docs.is_empty() {
        return Ok(());
    }

    for doc in &docs {
        let _ = storage.delete_document(STEER_NAMESPACE, &doc.id).await;
    }

    for chunk in docs.chunks(1000) {
        storage.add_to_store(chunk.to_vec()).await?;
    }

    sync_steer_bm25_at(base, &docs).await?;
    write_steer_metadata(base)?;

    Ok(())
}

async fn rebuild_all_steer_index_at(
    base: &Path,
    all_files: &[crate::store::StoredContextFile],
) -> Result<()> {
    let db_path = steer_db_path(base);
    if db_path.exists() {
        fs::remove_dir_all(&db_path)?;
    }

    let bm25_path = steer_bm25_path(base);
    if bm25_path.exists() {
        fs::remove_dir_all(&bm25_path)?;
    }

    let paths: Vec<PathBuf> = all_files.iter().map(|file| file.path.clone()).collect();
    let path_refs: Vec<&PathBuf> = paths.iter().collect();
    sync_steer_index_at(base, &path_refs).await
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
    let bm25 = BM25Index::new(&steer_bm25_config(base, true))?;
    let bm25_docs = bm25.doc_count() as usize;

    if bm25_docs == expected_docs {
        return Ok(false);
    }

    let paths: Vec<PathBuf> = files.into_iter().map(|file| file.path).collect();
    let path_refs: Vec<&PathBuf> = paths.iter().collect();
    let docs = build_steer_docs(&path_refs);

    tracing::info!(
        "Bootstrapping steer BM25 from store scan (bm25: {}, store: {})",
        bm25_docs,
        expected_docs
    );
    let bm25_writer = BM25Index::new(&steer_bm25_config(base, false))?;
    let _ = bm25_writer.delete_namespace_term(STEER_NAMESPACE).await;
    sync_steer_bm25_at(base, &docs).await?;

    Ok(true)
}

async fn ensure_steer_index_compatible_at(base: &Path) -> Result<()> {
    let actual_dimension = detect_steer_index_dimension_at(base).await?;

    match actual_dimension {
        Some(actual_dimension) if actual_dimension != STEER_SENTINEL_DIMENSION => {
            let files = crate::store::scan_context_files_at(base)?;
            if files.is_empty() {
                let meta_path = steer_metadata_path(base);
                if meta_path.exists() {
                    let _ = fs::remove_file(meta_path);
                }
                return Ok(());
            }

            tracing::info!(
                "Rebuilding steer index because stored vectors use {} dims, expected {}",
                actual_dimension,
                STEER_SENTINEL_DIMENSION
            );
            rebuild_all_steer_index_at(base, &files).await?;
        }
        Some(_) => {
            let metadata_ok = load_steer_metadata(base)
                .as_ref()
                .is_some_and(|metadata| steer_metadata_matches_current(base, metadata));
            if !metadata_ok {
                write_steer_metadata(base)?;
            }
        }
        None => {
            let meta_path = steer_metadata_path(base);
            if meta_path.exists() {
                let _ = fs::remove_file(meta_path);
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_candidate_query(
    run_id: Option<&str>,
    prompt_id: Option<&str>,
    agent: Option<&str>,
    kind: Option<&str>,
    frame_kind: Option<FrameKind>,
    project: Option<&str>,
    date_lo: Option<&str>,
    date_hi: Option<&str>,
) -> Option<String> {
    let mut terms = Vec::new();

    add_query_value(&mut terms, project);
    add_query_value(&mut terms, agent);
    add_query_value(&mut terms, kind);
    add_query_value(&mut terms, frame_kind.map(FrameKind::as_str));
    add_query_value(&mut terms, run_id);
    add_query_value(&mut terms, prompt_id);

    if matches!((date_lo, date_hi), (Some(lo), Some(hi)) if lo == hi) {
        add_query_value(&mut terms, date_lo);
    }

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

#[allow(clippy::too_many_arguments)]
fn metadata_matches(
    meta: &serde_json::Value,
    run_id: Option<&str>,
    prompt_id: Option<&str>,
    agent: Option<&str>,
    kind: Option<&str>,
    frame_kind: Option<FrameKind>,
    project: Option<&str>,
    date_lo: Option<&str>,
    date_hi: Option<&str>,
) -> bool {
    let project_lower = project.map(str::to_ascii_lowercase);
    let agent_lower = agent.map(str::to_ascii_lowercase);
    let kind_lower = kind.map(str::to_ascii_lowercase);

    if let Some(ref needle) = project_lower {
        if let Some(p) = meta.get("project").and_then(|v| v.as_str()) {
            if !p.to_ascii_lowercase().contains(needle) {
                return false;
            }
        } else {
            return false;
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
    if let Some(expected) = frame_kind
        && meta.get("frame_kind").and_then(|v| v.as_str()) != Some(expected.as_str())
    {
        return false;
    }
    if let Some(lo) = date_lo {
        if let Some(d) = meta.get("date").and_then(|v| v.as_str()) {
            if d < lo {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(hi) = date_hi {
        if let Some(d) = meta.get("date").and_then(|v| v.as_str()) {
            if d > hi {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(wanted) = run_id
        && meta.get("run_id").and_then(|v| v.as_str()) != Some(wanted)
    {
        return false;
    }
    if let Some(wanted) = prompt_id
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

#[allow(clippy::too_many_arguments)]
fn search_store_scan_at(
    base: &Path,
    run_id: Option<&str>,
    prompt_id: Option<&str>,
    agent: Option<&str>,
    kind: Option<&str>,
    frame_kind: Option<FrameKind>,
    project: Option<&str>,
    date_lo: Option<&str>,
    date_hi: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let files = crate::store::scan_context_files_at(base)?;
    let mut results = Vec::new();

    for file in files.into_iter().rev() {
        let meta = build_store_scan_metadata(&file);
        if !metadata_matches(
            &meta, run_id, prompt_id, agent, kind, frame_kind, project, date_lo, date_hi,
        ) {
            continue;
        }

        results.push(meta);
        if results.len() >= limit {
            break;
        }
    }

    Ok(results)
}

#[allow(clippy::too_many_arguments)]
async fn search_bm25_candidates_at(
    base: &Path,
    run_id: Option<&str>,
    prompt_id: Option<&str>,
    agent: Option<&str>,
    kind: Option<&str>,
    frame_kind: Option<FrameKind>,
    project: Option<&str>,
    date_lo: Option<&str>,
    date_hi: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let Some(query) = build_candidate_query(
        run_id, prompt_id, agent, kind, frame_kind, project, date_lo, date_hi,
    ) else {
        return Ok(vec![]);
    };

    let mut bm25 = BM25Index::new(&steer_bm25_config(base, true))?;
    if bm25.doc_count() == 0 && bootstrap_steer_index_if_missing_at(base).await? {
        bm25 = BM25Index::new(&steer_bm25_config(base, true))?;
    }

    if bm25.doc_count() == 0 {
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

        if !metadata_matches(
            &doc.metadata,
            run_id,
            prompt_id,
            agent,
            kind,
            frame_kind,
            project,
            date_lo,
            date_hi,
        ) {
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
    ensure_steer_index_compatible_at(base).await?;

    let all_files = crate::store::scan_context_files_at(base)?;
    if all_files.is_empty() {
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

        let db_path = steer_db_path(base);
        let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
        let _ = storage.delete_namespace_documents(STEER_NAMESPACE).await;
        let bm25 = BM25Index::new(&steer_bm25_config(base, false))?;
        let _ = bm25.delete_namespace_term(STEER_NAMESPACE).await;

        let paths: Vec<PathBuf> = all_files.into_iter().map(|f| f.path).collect();
        let path_refs: Vec<&PathBuf> = paths.iter().collect();
        sync_steer_index_at(base, &path_refs).await?;
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
    ensure_steer_index_compatible_at(&base).await?;
    sync_steer_index_at(&base, new_files).await
}

pub async fn query_steer_index() -> Result<Vec<ChromaDocument>> {
    let base = crate::store::store_base_dir()?;
    ensure_steer_index_compatible_at(&base).await?;
    query_steer_index_at(&base).await
}

pub async fn rebuild_steer_index_if_needed() -> Result<()> {
    let base = crate::store::store_base_dir()?;
    rebuild_steer_index_if_needed_at(&base).await
}

#[allow(clippy::too_many_arguments)]
pub async fn search_steer_index(
    run_id: Option<&str>,
    prompt_id: Option<&str>,
    agent: Option<&str>,
    kind: Option<&str>,
    frame_kind: Option<FrameKind>,
    project: Option<&str>,
    date_lo: Option<&str>,
    date_hi: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let base = crate::store::store_base_dir()?;
    ensure_steer_index_compatible_at(&base).await?;

    let candidate_results = search_bm25_candidates_at(
        &base, run_id, prompt_id, agent, kind, frame_kind, project, date_lo, date_hi, limit,
    )
    .await?;

    if candidate_results.len() >= limit || !candidate_results.is_empty() {
        return Ok(candidate_results);
    }

    search_store_scan_at(
        &base, run_id, prompt_id, agent, kind, frame_kind, project, date_lo, date_hi, limit,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunker::ChunkMetadataSidecar;
    use crate::store::Kind;
    use serde_json::json;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

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
                kind: Kind::Reports,
                run_id: Some("impl-055522".to_string()),
                prompt_id: Some("20260405_045135".to_string()),
                frame_kind: Some(FrameKind::AgentReply),
                agent_model: Some("gpt-5".to_string()),
                started_at: None,
                completed_at: None,
                token_usage: None,
                findings_count: None,
                workflow_phase: Some("implementation".to_string()),
                mode: None,
                skill_code: None,
                framework_version: Some("2026-04".to_string()),
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
            kind: Kind::Reports,
            run_id: Some(run_id.to_string()),
            prompt_id: Some(prompt_id.to_string()),
            frame_kind: Some(FrameKind::AgentReply),
            agent_model: Some("gpt-5.4".to_string()),
            started_at: Some("2026-03-31T16:00:00Z".to_string()),
            completed_at: Some("2026-03-31T16:05:00Z".to_string()),
            token_usage: Some(1200),
            findings_count: Some(2),
            workflow_phase: Some("marbles".to_string()),
            mode: Some("session-first".to_string()),
            skill_code: Some("vc-marbles".to_string()),
            framework_version: Some("2026-03".to_string()),
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
    fn steer_index_rebuilds_incompatible_vector_dimension() {
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

            ensure_steer_index_compatible_at(&base)
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
        let query = build_candidate_query(
            Some("mrbl-001"),
            None,
            Some("claude"),
            Some("reports"),
            None,
            Some("VetCoders/vibecrafted"),
            None,
            None,
        )
        .unwrap();

        assert!(query.contains("mrbl"));
        assert!(query.contains("claude"));
        assert!(query.contains("vibecrafted"));
    }
}
