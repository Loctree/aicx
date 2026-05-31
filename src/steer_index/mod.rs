//! BM25 + LanceDB steer index for fast session retrieval.
//!
//! The steer index is a dual-layer search structure over the canonical store:
//! a BM25 text index for keyword ranking and a LanceDB vector store for
//! metadata-filtered recall.  Public functions delegate to the store base
//! directory discovered at runtime, keeping callers free of path logic.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

#[cfg(feature = "lance")]
mod documents;
#[cfg(not(feature = "lance"))]
mod fallback;
#[cfg(feature = "lance")]
mod hooks;
#[cfg(feature = "lance")]
mod lifecycle;
#[cfg(feature = "lance")]
mod metadata;
#[cfg(feature = "lance")]
mod paths;
#[cfg(feature = "lance")]
mod search;
#[cfg(feature = "lance")]
mod sync;
#[cfg(feature = "lance")]
mod types;

#[cfg(all(test, feature = "lance"))]
mod tests;

use anyhow::Result;
#[cfg(feature = "lance")]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::progress::{FailureLog, Reporter};

pub use crate::steer_index_contract::SteerFilter;
#[cfg(feature = "lance")]
use hooks::call_steer_read_lock_hook;
#[cfg(feature = "lance")]
use lifecycle::{
    ensure_steer_index_compatible_at, ensure_steer_index_compatible_for_write_at,
    query_steer_index_at, rebuild_steer_index_if_needed_at,
};
#[cfg(feature = "lance")]
use metadata::{is_steer_incompatible, warn_if_steer_incompatible};
#[cfg(feature = "lance")]
use paths::steer_lock_path_at;
#[cfg(feature = "lance")]
use search::{search_bm25_candidates_at, search_store_scan_at};
#[cfg(feature = "lance")]
use sync::{sync_steer_index_at, sync_steer_index_at_with_reporter};
#[cfg(feature = "lance")]
pub use types::SteerIncompatible;

/// Builds or updates the fast steer index using rmcp-memex LanceDB backend.
/// Treats the sidecar as the source of truth for every touched chunk.
pub async fn sync_steer_index(new_files: &[&PathBuf]) -> Result<()> {
    #[cfg(not(feature = "lance"))]
    {
        fallback::sync_noop(new_files).await
    }

    #[cfg(feature = "lance")]
    {
        if new_files.is_empty() {
            return Ok(());
        }

        let base = crate::store::store_base_dir()?;
        let _lock = crate::locks::acquire_exclusive(crate::locks::steer_lock_path()?)?;
        ensure_steer_index_compatible_for_write_at(&base).await?;
        sync_steer_index_at(&base, new_files).await
    }
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
    #[cfg(not(feature = "lance"))]
    {
        fallback::sync_with_progress_noop(new_files, reporter, failures).await
    }

    #[cfg(feature = "lance")]
    {
        if new_files.is_empty() {
            return Ok(());
        }

        let base = crate::store::store_base_dir()?;
        let _lock = crate::locks::acquire_exclusive(crate::locks::steer_lock_path()?)?;
        ensure_steer_index_compatible_for_write_at(&base).await?;
        sync_steer_index_at_with_reporter(&base, new_files, reporter, failures).await
    }
}

pub async fn query_steer_index_count() -> Result<usize> {
    #[cfg(not(feature = "lance"))]
    {
        fallback::query_count_disabled().await
    }

    #[cfg(feature = "lance")]
    {
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
}

pub async fn try_rebuild_steer_index_if_needed_at(base: &Path) -> Result<()> {
    #[cfg(not(feature = "lance"))]
    {
        let _ = base;
        Ok(())
    }

    #[cfg(feature = "lance")]
    {
        fs::create_dir_all(base)?;
        let _lock = crate::locks::acquire_exclusive(steer_lock_path_at(base))?;
        rebuild_steer_index_if_needed_at(base).await
    }
}

pub async fn rebuild_steer_index_if_needed() -> Result<()> {
    #[cfg(not(feature = "lance"))]
    {
        fallback::rebuild_if_needed_noop().await
    }

    #[cfg(feature = "lance")]
    {
        let base = crate::store::store_base_dir()?;
        try_rebuild_steer_index_if_needed_at(&base).await
    }
}

pub async fn search_steer_index(
    filter: &SteerFilter<'_>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    #[cfg(not(feature = "lance"))]
    {
        fallback::search_disabled(filter, limit).await
    }

    #[cfg(feature = "lance")]
    {
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
}
