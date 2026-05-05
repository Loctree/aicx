//! Explicit AICX Oracle provenance for search-like surfaces.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleBackend {
    CanonicalCorpus,
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
    CanonicalChunks,
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
    pub source_layer: String,
    pub backend: OracleBackend,
    pub index_kind: OracleIndexKind,
    pub fallback_reason: Option<String>,
    pub derived_view: String,
    pub store_root: String,
    pub indexed_count: usize,
    pub scanned_count: usize,
    pub candidate_count: usize,
    pub source_paths_verified: bool,
    pub stale_or_unknown: bool,
    pub loctree_scope_safe: bool,
    pub loctree_scope_note: String,
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
    pub fn canonical_corpus_scan(
        store_root: &Path,
        scanned_count: usize,
        candidate_count: usize,
        source_paths_verified: bool,
    ) -> Self {
        Self {
            source_layer: canonical_layer(),
            backend: OracleBackend::CanonicalCorpus,
            index_kind: OracleIndexKind::CanonicalChunks,
            fallback_reason: None,
            derived_view: "canonical_chunk_scan_no_semantic_index".to_string(),
            store_root: display_path(store_root),
            indexed_count: 0,
            scanned_count,
            candidate_count,
            source_paths_verified,
            stale_or_unknown: !source_paths_verified,
            loctree_scope_safe: source_paths_verified,
            loctree_scope_note: if source_paths_verified {
                "safe_as_canonical_intent_evidence; not a semantic similarity oracle".to_string()
            } else {
                "unsafe_for_scope_narrowing; source chunks must be readable before Loctree trusts this surface".to_string()
            },
        }
    }

    pub fn filesystem_fuzzy(
        store_root: &Path,
        scanned_count: usize,
        candidate_count: usize,
        source_paths_verified: bool,
    ) -> Self {
        Self {
            source_layer: canonical_layer(),
            backend: OracleBackend::FilesystemFuzzy,
            index_kind: OracleIndexKind::None,
            fallback_reason: Some(
                "fallback_filesystem_fuzzy: content index unavailable".to_string(),
            ),
            derived_view: "none_filesystem_scan".to_string(),
            store_root: display_path(store_root),
            indexed_count: 0,
            scanned_count,
            candidate_count,
            source_paths_verified,
            stale_or_unknown: true,
            loctree_scope_safe: false,
            loctree_scope_note:
                "unsafe_for_scope_narrowing; use as routing evidence, then read canonical chunks"
                    .to_string(),
        }
    }

    pub fn metadata_steer(
        store_root: &Path,
        indexed_count: usize,
        candidate_count: usize,
        source_paths_verified: bool,
    ) -> Self {
        let loctree_scope_safe = source_paths_verified;
        Self {
            source_layer: canonical_layer(),
            backend: OracleBackend::SteerMetadata,
            index_kind: OracleIndexKind::MetadataSteer,
            fallback_reason: None,
            derived_view: "metadata_steer_index_rebuildable_from_canonical_chunks".to_string(),
            store_root: display_path(store_root),
            indexed_count,
            scanned_count: indexed_count,
            candidate_count,
            source_paths_verified,
            stale_or_unknown: !source_paths_verified,
            loctree_scope_safe,
            loctree_scope_note: if loctree_scope_safe {
                "safe_for_metadata_scope_when_followed_by_canonical_chunk_read".to_string()
            } else {
                "unsafe_for_scope_narrowing; metadata index returned paths that are not all readable"
                    .to_string()
            },
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

fn canonical_layer() -> String {
    "layer_1_canonical_corpus".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_fuzzy_status_is_explicitly_not_semantic_or_loctree_safe() {
        let status = OracleStatus::filesystem_fuzzy(Path::new("/tmp/aicx"), 40, 3, true);

        assert_eq!(status.source_layer, "layer_1_canonical_corpus");
        assert_eq!(status.backend, OracleBackend::FilesystemFuzzy);
        assert_eq!(status.index_kind, OracleIndexKind::None);
        assert!(
            status
                .fallback_reason
                .as_deref()
                .unwrap()
                .contains("fallback")
        );
        assert!(status.stale_or_unknown);
        assert!(!status.loctree_scope_safe);
        assert!(
            status
                .loctree_scope_note
                .contains("unsafe_for_scope_narrowing")
        );
    }

    #[test]
    fn canonical_intents_status_is_corpus_evidence_not_semantic_oracle() {
        let status = OracleStatus::canonical_corpus_scan(Path::new("/tmp/aicx"), 2, 2, true);

        assert_eq!(status.backend, OracleBackend::CanonicalCorpus);
        assert_eq!(status.index_kind, OracleIndexKind::CanonicalChunks);
        assert_eq!(status.fallback_reason, None);
        assert!(status.loctree_scope_safe);
        assert!(
            status
                .loctree_scope_note
                .contains("not a semantic similarity oracle")
        );
    }
}
