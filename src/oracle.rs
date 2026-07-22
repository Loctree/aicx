//! Explicit AICX Oracle provenance for search-like surfaces.

#[cfg(feature = "app")]
use aicx_retrieve::RetrievalOutcome;
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::chunker::{
    CARD_CLAIM_SCOPE_SESSION_CLOSE, CARD_FRESHNESS_CONTRACT_HISTORICAL,
    CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX,
};

/// Claim-honesty frame (card contract v2): every intent/decision/outcome the
/// operator sees from aicx is a HISTORICAL claim valid at session close, not
/// live runtime truth. Fields mirror the card sidecar; `None` means the source
/// card predates schema v2 and is rendered as `unknown` on text surfaces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ClaimHonesty {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim_scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness_contract: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_state: Option<String>,
}

impl ClaimHonesty {
    /// Canonical frame stamped on aicx display envelopes: claims are bound to
    /// session close, historical by contract, and never verified by aicx.
    pub fn canonical() -> Self {
        Self {
            claim_scope: Some(CARD_CLAIM_SCOPE_SESSION_CLOSE.to_string()),
            freshness_contract: Some(CARD_FRESHNESS_CONTRACT_HISTORICAL.to_string()),
            verification_state: Some(CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX.to_string()),
        }
    }

    /// Compact operator-facing honesty line, e.g.
    /// `claims: historical @ session close · not verified by aicx`.
    /// Missing fields (pre-v2 cards) render as `unknown`.
    pub fn display_line(&self) -> String {
        let humanize = |value: &str| value.replace('_', " ");
        let freshness = self
            .freshness_contract
            .as_deref()
            .map(humanize)
            .unwrap_or_else(|| "unknown".to_string());
        let scope = self
            .claim_scope
            .as_deref()
            .map(humanize)
            .unwrap_or_else(|| "claim_scope=unknown".to_string());
        let verification = self
            .verification_state
            .as_deref()
            .map(humanize)
            .unwrap_or_else(|| "not verified by aicx".to_string());
        format!("claims: {freshness} @ {scope} · {verification}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OracleBackend {
    CanonicalCorpus,
    FilesystemFuzzy,
    SteerMetadata,
    ContentSemantic,
    Hybrid,
    #[serde(rename = "hybrid_rrf")]
    HybridRrf,
    /// Dense cosine leg served without the lexical fusion leg — a degraded
    /// hybrid execution, never a healthy semantic claim.
    SemanticDenseOnly,
    /// The caller produced results but carried no execution evidence; health
    /// cannot be claimed for this surface.
    RetrievalUnknown,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_generation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_source_chunk_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dense_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lexical_doc_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fusion_algorithm: Option<String>,
    /// Typed retrieval execution status (W1-01). One value rendered on every
    /// surface; additive key so existing JSON consumers are untouched.
    /// App-only: the slim `loctree-consumer` profile does not link
    /// `aicx-retrieve`, and this field is never emitted there.
    #[cfg(feature = "app")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<RetrievalOutcome>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OracleEnvelope<T>
where
    T: Serialize,
{
    pub oracle_status: OracleStatus,
    /// Honesty frame for every item in this envelope: historical claims,
    /// valid at session close, not verified by aicx. Additive key — existing
    /// envelope consumers keep their fields untouched.
    pub claim_honesty: ClaimHonesty,
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
            manifest_generation_id: None,
            manifest_source_chunk_count: None,
            dense_count: None,
            lexical_doc_count: None,
            fusion_algorithm: None,
            #[cfg(feature = "app")]
            retrieval: None,
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
            manifest_generation_id: None,
            manifest_source_chunk_count: None,
            dense_count: None,
            lexical_doc_count: None,
            fusion_algorithm: None,
            #[cfg(feature = "app")]
            retrieval: None,
        }
    }

    pub fn content_semantic(
        store_root: &Path,
        indexed_count: usize,
        candidate_count: usize,
        source_paths_verified: bool,
    ) -> Self {
        Self {
            source_layer: canonical_layer(),
            backend: OracleBackend::ContentSemantic,
            index_kind: OracleIndexKind::ContentChunks,
            fallback_reason: None,
            derived_view: "materialized_vector_index_from_canonical_chunks".to_string(),
            store_root: display_path(store_root),
            indexed_count,
            scanned_count: indexed_count,
            candidate_count,
            source_paths_verified,
            stale_or_unknown: !source_paths_verified,
            loctree_scope_safe: source_paths_verified,
            loctree_scope_note: if source_paths_verified {
                "safe_for_semantic_scope_when_followed_by_canonical_chunk_read".to_string()
            } else {
                "unsafe_for_scope_narrowing; semantic index returned paths that are not all readable"
                    .to_string()
            },
            manifest_generation_id: None,
            manifest_source_chunk_count: None,
            dense_count: None,
            lexical_doc_count: None,
            fusion_algorithm: None,
            #[cfg(feature = "app")]
            retrieval: None,
        }
    }

    #[cfg(feature = "app")]
    pub fn hybrid_rrf(
        store_root: &Path,
        status: &crate::search_engine::HybridRetrievalStatus,
        candidate_count: usize,
        source_paths_verified: bool,
    ) -> Self {
        Self {
            source_layer: canonical_layer(),
            backend: OracleBackend::HybridRrf,
            index_kind: OracleIndexKind::OnionContent,
            fallback_reason: None,
            derived_view: format!(
                "hybrid_rrf_manifest_bound_lexical_dense_index:{}",
                status.dense_kind
            ),
            store_root: display_path(store_root),
            indexed_count: status.source_chunk_count,
            scanned_count: status.source_chunk_count,
            candidate_count,
            source_paths_verified,
            stale_or_unknown: !source_paths_verified,
            loctree_scope_safe: source_paths_verified,
            loctree_scope_note: if source_paths_verified {
                "safe_for_hybrid_scope_when_followed_by_canonical_chunk_read".to_string()
            } else {
                "unsafe_for_scope_narrowing; hybrid index returned paths that are not all readable"
                    .to_string()
            },
            manifest_generation_id: Some(status.generation_id.clone()),
            manifest_source_chunk_count: Some(status.source_chunk_count),
            dense_count: Some(status.dense_count),
            lexical_doc_count: Some(status.lexical_doc_count),
            fusion_algorithm: Some(status.fusion_algorithm.clone()),
            #[cfg(feature = "app")]
            retrieval: None,
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
            manifest_generation_id: None,
            manifest_source_chunk_count: None,
            dense_count: None,
            lexical_doc_count: None,
            fusion_algorithm: None,
            #[cfg(feature = "app")]
            retrieval: None,
        }
    }

    /// Degraded hybrid execution: only the dense cosine leg ran (hybrid
    /// manifest missing or stale). Always carries a fallback reason — this
    /// status must never serialize as a healthy semantic backend.
    pub fn semantic_dense_only(
        store_root: &Path,
        scanned_count: usize,
        candidate_count: usize,
        source_paths_verified: bool,
        fallback_reason: String,
    ) -> Self {
        Self {
            source_layer: canonical_layer(),
            backend: OracleBackend::SemanticDenseOnly,
            index_kind: OracleIndexKind::ContentChunks,
            fallback_reason: Some(fallback_reason),
            derived_view: "dense_only_cosine_over_committed_primary_index".to_string(),
            store_root: display_path(store_root),
            indexed_count: scanned_count,
            scanned_count,
            candidate_count,
            source_paths_verified,
            stale_or_unknown: !source_paths_verified,
            loctree_scope_safe: source_paths_verified,
            loctree_scope_note: if source_paths_verified {
                "degraded_dense_only; semantically valid but lexical fusion leg is unavailable — \
                 follow with canonical chunk read"
                    .to_string()
            } else {
                "unsafe_for_scope_narrowing; dense-only index returned paths that are not all readable"
                    .to_string()
            },
            manifest_generation_id: None,
            manifest_source_chunk_count: None,
            dense_count: None,
            lexical_doc_count: None,
            fusion_algorithm: None,
            #[cfg(feature = "app")]
            retrieval: None,
        }
    }

    /// Results arrived without execution evidence (legacy caller, no hybrid
    /// manifest, no dense-only marker). Fails closed: stale/unknown, never
    /// scope-safe, never a healthy semantic claim.
    pub fn retrieval_unknown(
        store_root: &Path,
        scanned_count: usize,
        candidate_count: usize,
        fallback_reason: String,
    ) -> Self {
        Self {
            source_layer: canonical_layer(),
            backend: OracleBackend::RetrievalUnknown,
            index_kind: OracleIndexKind::None,
            fallback_reason: Some(fallback_reason),
            derived_view: "retrieval_execution_evidence_missing".to_string(),
            store_root: display_path(store_root),
            indexed_count: 0,
            scanned_count,
            candidate_count,
            source_paths_verified: false,
            stale_or_unknown: true,
            loctree_scope_safe: false,
            loctree_scope_note:
                "unsafe_for_scope_narrowing; retrieval carried no execution evidence — \
                 treat results as unverified routing signal"
                    .to_string(),
            manifest_generation_id: None,
            manifest_source_chunk_count: None,
            dense_count: None,
            lexical_doc_count: None,
            fusion_algorithm: None,
            #[cfg(feature = "app")]
            retrieval: None,
        }
    }

    /// Attach the typed retrieval execution status. Every search-like JSON
    /// surface (CLI search/evidence, MCP search/evidence) carries this same
    /// value; renderers must not re-derive health elsewhere.
    #[cfg(feature = "app")]
    pub fn with_retrieval(mut self, retrieval: RetrievalOutcome) -> Self {
        self.retrieval = Some(retrieval);
        self
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
    fn canonical_claim_honesty_matches_card_contract_constants() {
        let honesty = ClaimHonesty::canonical();

        assert_eq!(honesty.claim_scope.as_deref(), Some("session_close"));
        assert_eq!(honesty.freshness_contract.as_deref(), Some("historical"));
        assert_eq!(
            honesty.verification_state.as_deref(),
            Some("not_verified_by_aicx")
        );
        assert_eq!(
            honesty.display_line(),
            "claims: historical @ session close · not verified by aicx"
        );
    }

    #[test]
    fn empty_claim_honesty_renders_unknown_scope_not_a_fake_frame() {
        // Pre-v2 cards carry no honesty fields; the display line must say so
        // instead of implying a canonical frame that was never stamped.
        let honesty = ClaimHonesty::default();

        let line = honesty.display_line();
        assert!(line.contains("claim_scope=unknown"), "line was: {line}");
        assert!(line.starts_with("claims: unknown @ "), "line was: {line}");
        assert!(line.ends_with("not verified by aicx"), "line was: {line}");

        // Default (absent) honesty must serialize to ZERO keys so v1 records
        // stay byte-identical on JSON surfaces.
        let json = serde_json::to_value(&honesty).expect("serialize honesty");
        assert_eq!(json, serde_json::json!({}));
    }

    #[test]
    fn oracle_envelope_carries_claim_honesty_frame() {
        let envelope = OracleEnvelope {
            oracle_status: OracleStatus::canonical_corpus_scan(Path::new("/tmp/aicx"), 2, 2, true),
            claim_honesty: ClaimHonesty::canonical(),
            results: 0,
            items: Vec::<u8>::new(),
        };

        let json = serde_json::to_value(&envelope).expect("serialize envelope");
        let frame = &json["claim_honesty"];
        assert_eq!(frame["claim_scope"], "session_close");
        assert_eq!(frame["freshness_contract"], "historical");
        assert_eq!(frame["verification_state"], "not_verified_by_aicx");
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
