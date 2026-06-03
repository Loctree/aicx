use rmcp_memex::search::BM25Config;
use std::path::{Path, PathBuf};

pub(super) const STEER_NAMESPACE: &str = "steer";
pub(super) const STEER_BM25_DIR: &str = "steer_bm25";
pub(super) const STEER_METADATA_FILE: &str = "steer_index_meta.json";
pub(super) const STEER_NEXT_DIR: &str = ".steer.next";
pub(super) const STEER_PREV_DIR: &str = ".steer.prev";
pub(super) const STEER_INDEX_METADATA_VERSION: u32 = 1;
pub(super) const STEER_SENTINEL_DIMENSION: usize = 1;
pub(super) const MIN_CANDIDATES: usize = 200;
pub(super) const CANDIDATE_MULTIPLIER: usize = 20;

pub(super) fn steer_db_path(base: &Path) -> PathBuf {
    base.join("steer_db")
}

pub(super) fn steer_bm25_path(base: &Path) -> PathBuf {
    base.join(STEER_BM25_DIR)
}

pub(super) fn steer_metadata_path(base: &Path) -> PathBuf {
    base.join(STEER_METADATA_FILE)
}

pub(super) fn steer_lock_path_at(base: &Path) -> PathBuf {
    base.join("locks").join("steer.lock")
}

pub(super) fn steer_bm25_config(base: &Path, read_only: bool) -> BM25Config {
    BM25Config::multilingual()
        .with_path(steer_bm25_path(base).to_string_lossy().to_string())
        .with_read_only(read_only)
}
