//! Explicit AICX Oracle provenance for search-like surfaces.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleBackend {
    FilesystemFuzzy,
    SteerMetadata,
    ContentSemantic,
    Hybrid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleIndexKind {
    None,
    MetadataSteer,
    ContentChunks,
    OnionContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleReadiness {
    Ready,
    Degraded,
    UnsafeForLoctreeScope,
}

#[derive(Debug, Clone, Serialize)]
pub struct OracleStatus {
    pub backend: OracleBackend,
    pub index_kind: OracleIndexKind,
    pub fallback_reason: Option<String>,
    pub store_root: String,
    pub indexed_count: usize,
    pub scanned_count: usize,
    pub candidate_count: usize,
    pub source_paths_verified: bool,
    pub stale_or_unknown: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OracleEnvelope<T>
where
    T: Serialize,
{
    pub oracle_status: OracleStatus,
    pub results: usize,
    pub items: T,
}

impl OracleStatus {
    pub fn filesystem_fuzzy(
        store_root: &Path,
        scanned_count: usize,
        candidate_count: usize,
        source_paths_verified: bool,
    ) -> Self {
        Self {
            backend: OracleBackend::FilesystemFuzzy,
            index_kind: OracleIndexKind::None,
            fallback_reason: Some(
                "fallback_filesystem_fuzzy: content index unavailable".to_string(),
            ),
            store_root: display_path(store_root),
            indexed_count: 0,
            scanned_count,
            candidate_count,
            source_paths_verified,
            stale_or_unknown: true,
        }
    }

    pub fn metadata_steer(
        store_root: &Path,
        indexed_count: usize,
        candidate_count: usize,
        source_paths_verified: bool,
    ) -> Self {
        Self {
            backend: OracleBackend::SteerMetadata,
            index_kind: OracleIndexKind::MetadataSteer,
            fallback_reason: None,
            store_root: display_path(store_root),
            indexed_count,
            scanned_count: indexed_count,
            candidate_count,
            source_paths_verified,
            stale_or_unknown: !source_paths_verified,
        }
    }
}

pub fn readiness(statuses: &[OracleStatus]) -> OracleReadiness {
    if statuses.iter().any(|status| status.stale_or_unknown) {
        OracleReadiness::UnsafeForLoctreeScope
    } else if statuses
        .iter()
        .any(|status| status.fallback_reason.is_some())
    {
        OracleReadiness::Degraded
    } else {
        OracleReadiness::Ready
    }
}

pub fn verify_paths<I, P>(paths: I) -> bool
where
    I: IntoIterator<Item = P>,
    P: Into<PathBuf>,
{
    paths.into_iter().all(|path| path.into().exists())
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().to_string()
}
