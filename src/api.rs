//! Stable in-process API for consumers that want AICX as a library.
//!
//! The CLI remains the product shell, but this facade is the supported crate
//! boundary: callers get corpus, retrieval, intent, and health operations
//! without importing CLI-private glue from `main.rs`.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::doctor::{DoctorOptions, DoctorReport};
use crate::intents::{IntentExtraction, IntentsConfig};
use crate::rank::FuzzyResult;
use crate::store::{ReadContextChunk, StoredContextFile};
use crate::timeline::FrameKind;

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

    pub fn fuzzy_search(
        &self,
        query: impl AsRef<str>,
        opts: SearchOptions,
    ) -> Result<SearchResults> {
        let (results, scanned) = crate::rank::fuzzy_search_store(
            &self.config.store_root,
            query.as_ref(),
            opts.limit,
            opts.project.as_deref(),
            opts.frame_kind,
        )
        .map_err(anyhow::Error::from)
        .context("fuzzy search failed")?;

        Ok(SearchResults { results, scanned })
    }

    pub fn extract_intents(&self, config: &IntentsConfig) -> Result<IntentExtraction> {
        crate::intents::extract_intents_from_root_at_with_stats(
            config,
            &self.config.store_root,
            chrono::Utc::now(),
        )
    }

    pub async fn doctor(&self, opts: &DoctorOptions) -> Result<DoctorReport> {
        crate::doctor::run_at(&self.config.store_root, opts).await
    }

    pub fn index_status(&self, project: Option<&str>) -> Result<IndexStatus> {
        index_status_at(&self.config.store_root, project)
    }
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub limit: usize,
    pub project: Option<String>,
    pub frame_kind: Option<FrameKind>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            limit: 10,
            project: None,
            frame_kind: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResults {
    pub results: Vec<FuzzyResult>,
    pub scanned: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub canonical_chunks: usize,
    pub semantic_index_present: bool,
    pub newest_chunk_mtime: Option<String>,
    pub semantic_index_mtime: Option<String>,
    pub semantic_lag_secs: Option<u64>,
    pub pending_chunks: usize,
}

pub fn index_status_at(base: &Path, project: Option<&str>) -> Result<IndexStatus> {
    let chunks = crate::store::scan_context_files_project_at(base, project)?;
    let newest_chunk = chunks
        .iter()
        .filter_map(|chunk| chunk.path.metadata().ok()?.modified().ok())
        .max();

    let index_roots = [
        base.join("semantic_index"),
        base.join("vector_index"),
        base.join("index"),
        base.join("steer_db"),
    ];
    let semantic_index_mtime = index_roots
        .iter()
        .filter_map(|path| newest_mtime(path))
        .max();
    let semantic_index_present = semantic_index_mtime.is_some();
    let semantic_lag_secs = match (newest_chunk, semantic_index_mtime) {
        (Some(chunk), Some(index)) => Some(
            chunk
                .duration_since(index)
                .unwrap_or(Duration::ZERO)
                .as_secs(),
        ),
        _ => None,
    };

    let pending_chunks = if matches!(semantic_lag_secs, Some(lag) if lag > 0)
        || (newest_chunk.is_some() && semantic_index_mtime.is_none())
    {
        chunks.len()
    } else {
        0
    };

    Ok(IndexStatus {
        canonical_chunks: chunks.len(),
        semantic_index_present,
        newest_chunk_mtime: newest_chunk.map(system_time_to_rfc3339),
        semantic_index_mtime: semantic_index_mtime.map(system_time_to_rfc3339),
        semantic_lag_secs,
        pending_chunks,
    })
}

fn newest_mtime(root: &Path) -> Option<SystemTime> {
    let metadata = root.metadata().ok()?;
    let mut newest = metadata.modified().ok();
    if metadata.is_dir() {
        for entry in std::fs::read_dir(root).ok()?.flatten() {
            newest = newest.into_iter().chain(newest_mtime(&entry.path())).max();
        }
    }
    newest
}

fn system_time_to_rfc3339(value: SystemTime) -> String {
    let datetime: chrono::DateTime<chrono::Utc> = value.into();
    datetime.to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
