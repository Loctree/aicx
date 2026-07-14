use anyhow::Result;
use std::path::PathBuf;

use crate::steer_index_contract::SteerFilter;

pub(super) async fn sync_noop(_new_files: &[&PathBuf]) -> Result<()> {
    Ok(())
}

pub(super) async fn sync_with_progress_noop(
    _new_files: &[&PathBuf],
    _reporter: std::sync::Arc<dyn crate::progress::Reporter>,
    _failures: &crate::progress::FailureLog,
) -> Result<()> {
    Ok(())
}

pub(super) async fn query_count_disabled() -> Result<usize> {
    Ok(0)
}

pub(super) async fn rebuild_if_needed_noop() -> Result<()> {
    // We do not fail the sync/rebuild cycle because the index is strictly optional.
    // The operator will be informed only if they actively attempt to query it.
    Ok(())
}

pub(super) async fn search_disabled(
    _filter: &SteerFilter<'_>,
    _limit: usize,
) -> Result<Vec<serde_json::Value>> {
    anyhow::bail!(
        "The LanceDB vector steer index is not enabled in this aicx build.\n\
         To use `aicx steer` and MCP `aicx_steer`, please install a pre-built binary \
         from GitHub Releases, or re-compile from source with `cargo build --release --features lance`.\n\
         Alternatively, use `aicx search` for fast filesystem-based semantic/fuzzy fallback."
    )
}
