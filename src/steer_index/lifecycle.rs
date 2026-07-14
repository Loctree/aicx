use anyhow::Result;
use rmcp_memex::{
    search::BM25Index,
    storage::{ChromaDocument, StorageManager},
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::progress::{FailureLog, NoopReporter, Reporter};

use super::documents::{doc_ids, file_ids, steer_index_needs_rebuild};
use super::hooks::call_steer_rebuild_swap_hook;
use super::metadata::{
    detect_steer_index_dimension_at, load_steer_metadata, steer_metadata_matches_current,
    write_steer_metadata,
};
use super::paths::{
    STEER_BM25_DIR, STEER_METADATA_FILE, STEER_NAMESPACE, STEER_NEXT_DIR, STEER_PREV_DIR,
    STEER_SENTINEL_DIMENSION, steer_bm25_config, steer_bm25_path, steer_db_path,
    steer_metadata_path,
};
use super::sync::sync_steer_index_at_with_reporter_and_filter_base;
use super::types::SteerIncompatible;

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

pub(super) fn clear_steer_index_artifacts_at(base: &Path) -> Result<()> {
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

pub(super) async fn rebuild_all_steer_index_at(
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

pub(super) async fn query_steer_index_at(base: &Path) -> Result<Vec<ChromaDocument>> {
    let db_path = steer_db_path(base);
    if !db_path.exists() {
        return Ok(vec![]);
    }
    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    storage.get_all_in_namespace(STEER_NAMESPACE).await
}

pub(super) async fn bootstrap_steer_index_if_missing_at(base: &Path) -> Result<bool> {
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
    tracing::warn!("{incompatible}; run `aicx doctor --fix`");
    Err(incompatible.into())
}

pub(super) async fn ensure_steer_index_compatible_at(base: &Path) -> Result<()> {
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

pub(super) async fn ensure_steer_index_compatible_for_write_at(base: &Path) -> Result<()> {
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

pub(super) async fn rebuild_steer_index_if_needed_at(base: &Path) -> Result<()> {
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
