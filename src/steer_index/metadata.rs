use anyhow::Result;
use chrono::{DateTime, Utc};
use rmcp_memex::storage::{SCHEMA_VERSION, StorageManager};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use super::paths::{
    STEER_INDEX_METADATA_VERSION, STEER_NAMESPACE, STEER_SENTINEL_DIMENSION, steer_bm25_path,
    steer_db_path, steer_metadata_path,
};
use super::types::SteerIncompatible;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct SteerIndexMetadata {
    format_version: u32,
    namespace: String,
    db_path: String,
    bm25_path: String,
    vector_dimension: usize,
    storage_schema_version: u32,
    updated_at: DateTime<Utc>,
}

pub(super) fn warn_if_steer_incompatible(err: &anyhow::Error) {
    if let Some(incompatible) = err.downcast_ref::<SteerIncompatible>() {
        tracing::warn!("{incompatible}; run `aicx doctor --rebuild-steer-index`");
    }
}

pub(super) fn is_steer_incompatible(err: &anyhow::Error) -> bool {
    err.downcast_ref::<SteerIncompatible>().is_some()
}

pub(super) fn load_steer_metadata(base: &Path) -> Option<SteerIndexMetadata> {
    let raw = fs::read_to_string(steer_metadata_path(base)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub(super) fn steer_metadata_matches_current(base: &Path, metadata: &SteerIndexMetadata) -> bool {
    metadata.format_version == STEER_INDEX_METADATA_VERSION
        && metadata.namespace == STEER_NAMESPACE
        && metadata.db_path == steer_db_path(base).display().to_string()
        && metadata.bm25_path == steer_bm25_path(base).display().to_string()
        && metadata.vector_dimension == STEER_SENTINEL_DIMENSION
        && metadata.storage_schema_version == SCHEMA_VERSION
}

pub(super) fn write_steer_metadata(base: &Path) -> Result<()> {
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

pub(super) async fn detect_steer_index_dimension_at(base: &Path) -> Result<Option<usize>> {
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
