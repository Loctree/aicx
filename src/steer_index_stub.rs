//! Stub for steer_index when lance feature is disabled.

use crate::timeline::FrameKind;
use anyhow::Result;
use std::path::PathBuf;

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

pub async fn sync_steer_index(_new_files: &[&PathBuf]) -> Result<()> {
    Ok(())
}

pub async fn sync_steer_index_with_progress(
    _new_files: &[&PathBuf],
    _reporter: std::sync::Arc<dyn crate::progress::Reporter>,
    _failures: &crate::progress::FailureLog,
) -> Result<()> {
    Ok(())
}

pub async fn query_steer_index_count() -> Result<usize> {
    Ok(0)
}

pub async fn rebuild_steer_index_if_needed() -> Result<()> {
    // We do not fail the sync/rebuild cycle because the index is strictly optional.
    // The operator will be informed only if they actively attempt to query it.
    Ok(())
}

pub async fn search_steer_index(
    _filter: &SteerFilter<'_>,
    _limit: usize,
) -> Result<Vec<serde_json::Value>> {
    anyhow::bail!(
        "The LanceDB vector steer index is not enabled in this aicx build.\n\
         To use `aicx steer` and MCP `aicx_steer`, please install a pre-built binary \
         from GitHub Releases, or re-compile from source with `cargo build --release --features lance`."
    )
}
