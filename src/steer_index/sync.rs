use anyhow::Result;
use rmcp_memex::{
    search::BM25Index,
    storage::{ChromaDocument, StorageManager},
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::progress::{FailureLog, NoopReporter, Phase, Reporter, recovery_hint_for};

use super::documents::build_steer_docs;
use super::metadata::write_steer_metadata;
use super::paths::{STEER_NAMESPACE, steer_bm25_config, steer_db_path};

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

pub(super) async fn sync_steer_index_at(base: &Path, new_files: &[&PathBuf]) -> Result<()> {
    let reporter: Arc<dyn Reporter> = Arc::new(NoopReporter);
    let failures = FailureLog::new();
    sync_steer_index_at_with_reporter(base, new_files, reporter, &failures).await
}

/// Instrumented variant: emits separate `steer_sync` and `bm25_sync`
/// Phase events through the supplied reporter and records phase
/// failures into `failures` before propagating the error. Existing
/// callers reach this via the no-op shim above.
pub(super) async fn sync_steer_index_at_with_reporter(
    base: &Path,
    new_files: &[&PathBuf],
    reporter: Arc<dyn Reporter>,
    failures: &FailureLog,
) -> Result<()> {
    sync_steer_index_at_with_reporter_and_filter_base(base, base, new_files, reporter, failures)
        .await
}

pub(super) async fn sync_steer_index_at_with_reporter_and_filter_base(
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
