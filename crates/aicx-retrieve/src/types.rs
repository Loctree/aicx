// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Opaque lexical commit identifier produced by a lexical index.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LexicalCommitId(pub String);

/// Minimal source chunk reference consumed by lexical indexes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub id: String,
    pub source_path: String,
    pub text: String,
    pub metadata: serde_json::Value,
}

/// Source chunk plus embedding consumed by dense indexes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DenseChunkRef {
    pub chunk: ChunkRef,
    pub embedding: Vec<f32>,
}

/// Unified retrieval hit emitted by lexical and dense adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub chunk_id: String,
    pub score: f32,
    pub rank: usize,
    pub source: String,
    pub metadata: serde_json::Value,
}

/// Lexical query request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexicalQuery {
    pub text: String,
    pub limit: usize,
    pub filters: FilterSet,
}

/// Structured filters shared across lexical and dense retrieval paths.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FilterSet {
    pub values: BTreeMap<String, serde_json::Value>,
}

/// Dense-vector distance semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Distance {
    Cosine,
    Euclidean,
    Dot,
}

/// Retrieval mode as the caller requested it, before any degradation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestedMode {
    /// Semantic retrieval over the committed hybrid stack (lexical + dense + fusion).
    Hybrid,
    /// Caller explicitly asked for lexical/filesystem retrieval only.
    Lexical,
}

/// Retrieval legs that actually executed. Evidence, never intent: callers set
/// this from what ran, and absence of evidence maps to [`ExecutedPath::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutedPath {
    /// Lexical + dense legs ran and were fused.
    HybridFusion,
    /// Only the dense leg ran (hybrid manifest missing or stale).
    DenseOnly,
    /// Only a lexical/filesystem leg ran.
    LexicalOnly,
    /// No execution evidence exists for this outcome.
    None,
}

/// Completeness of the executed retrieval relative to the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalCompleteness {
    /// The requested path executed fully.
    Complete,
    /// The requested path executed, but bounded examination under-delivered
    /// the requested limit (filter pushdown exhausted its cap).
    Partial,
    /// A weaker path than requested executed.
    Degraded,
    /// Execution evidence is absent; nothing about health may be claimed.
    Unknown,
}

/// Fallback reason stamped when a caller supplies no execution evidence.
pub const FALLBACK_REASON_EVIDENCE_MISSING: &str = "execution_evidence_missing: caller supplied no executed path or examined count; \
     refusing to claim a healthy retrieval";

/// Fallback reason stamped when execution degraded without a caller-supplied reason.
pub const FALLBACK_REASON_DEGRADED_UNSPECIFIED: &str = "degraded_execution_reason_missing: executed path is weaker than requested \
     and the caller supplied no reason";

/// Execution evidence handed to [`RetrievalOutcome::from_evidence`]. The
/// optional fields are deliberate: absent evidence degrades the outcome to
/// `unknown` — it never defaults to healthy.
#[derive(Debug, Clone, Default)]
pub struct RetrievalEvidence {
    pub executed_path: Option<ExecutedPath>,
    pub examined_count: Option<usize>,
    pub matched_count: usize,
    pub fallback_reason: Option<String>,
    pub stale_evidence: bool,
}

/// One typed retrieval status describing what actually executed. CLI text,
/// CLI JSON, and MCP JSON all render from this same value; no renderer may
/// derive health from backend label strings or manifest presence on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalOutcome {
    pub requested_mode: RequestedMode,
    pub executed_path: ExecutedPath,
    pub completeness: RetrievalCompleteness,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub examined_count: usize,
    pub matched_count: usize,
    pub stale_evidence: bool,
}

impl RetrievalOutcome {
    /// Build an outcome from execution evidence. Completeness is derived
    /// here and only here: missing evidence yields `unknown`, an executed
    /// path weaker than the request yields `degraded` (with a canonical
    /// reason when the caller supplied none), and only a full match between
    /// request and execution yields `complete`.
    pub fn from_evidence(requested_mode: RequestedMode, evidence: RetrievalEvidence) -> Self {
        let RetrievalEvidence {
            executed_path,
            examined_count,
            matched_count,
            fallback_reason,
            stale_evidence,
        } = evidence;

        let (Some(executed_path), Some(examined_count)) = (executed_path, examined_count) else {
            return Self {
                requested_mode,
                executed_path: ExecutedPath::None,
                completeness: RetrievalCompleteness::Unknown,
                fallback_reason: Some(
                    fallback_reason.unwrap_or_else(|| FALLBACK_REASON_EVIDENCE_MISSING.to_string()),
                ),
                examined_count: examined_count.unwrap_or(0),
                matched_count,
                stale_evidence,
            };
        };

        let (completeness, fallback_reason) = match (requested_mode, executed_path) {
            (RequestedMode::Hybrid, ExecutedPath::HybridFusion)
            | (RequestedMode::Lexical, ExecutedPath::LexicalOnly) => {
                (RetrievalCompleteness::Complete, fallback_reason)
            }
            (_, ExecutedPath::None) => (
                RetrievalCompleteness::Unknown,
                Some(
                    fallback_reason.unwrap_or_else(|| FALLBACK_REASON_EVIDENCE_MISSING.to_string()),
                ),
            ),
            _ => (
                RetrievalCompleteness::Degraded,
                Some(
                    fallback_reason
                        .unwrap_or_else(|| FALLBACK_REASON_DEGRADED_UNSPECIFIED.to_string()),
                ),
            ),
        };

        Self {
            requested_mode,
            executed_path,
            completeness,
            fallback_reason,
            examined_count,
            matched_count,
            stale_evidence,
        }
    }

    /// Downgrade a complete outcome to partial (bounded examination
    /// under-delivered the requested limit). Never upgrades: degraded and
    /// unknown outcomes stay exactly as constructed.
    pub fn mark_partial(mut self) -> Self {
        if self.completeness == RetrievalCompleteness::Complete {
            self.completeness = RetrievalCompleteness::Partial;
        }
        self
    }
}

#[cfg(test)]
mod retrieval_outcome_tests {
    use super::*;

    #[test]
    fn missing_executed_path_is_unknown_never_healthy() {
        let outcome = RetrievalOutcome::from_evidence(
            RequestedMode::Hybrid,
            RetrievalEvidence {
                executed_path: None,
                examined_count: Some(100),
                matched_count: 5,
                fallback_reason: None,
                stale_evidence: false,
            },
        );
        assert_eq!(outcome.completeness, RetrievalCompleteness::Unknown);
        assert_eq!(outcome.executed_path, ExecutedPath::None);
        assert!(
            outcome
                .fallback_reason
                .as_deref()
                .unwrap()
                .starts_with("execution_evidence_missing"),
        );
    }

    #[test]
    fn missing_examined_count_is_unknown_never_healthy() {
        let outcome = RetrievalOutcome::from_evidence(
            RequestedMode::Hybrid,
            RetrievalEvidence {
                executed_path: Some(ExecutedPath::HybridFusion),
                examined_count: None,
                matched_count: 5,
                fallback_reason: None,
                stale_evidence: false,
            },
        );
        assert_eq!(outcome.completeness, RetrievalCompleteness::Unknown);
        assert_eq!(outcome.examined_count, 0);
        assert!(outcome.fallback_reason.is_some());
    }

    #[test]
    fn hybrid_request_with_fusion_execution_is_complete() {
        let outcome = RetrievalOutcome::from_evidence(
            RequestedMode::Hybrid,
            RetrievalEvidence {
                executed_path: Some(ExecutedPath::HybridFusion),
                examined_count: Some(123),
                matched_count: 7,
                fallback_reason: None,
                stale_evidence: false,
            },
        );
        assert_eq!(outcome.completeness, RetrievalCompleteness::Complete);
        assert_eq!(outcome.fallback_reason, None);
    }

    #[test]
    fn dense_only_execution_under_hybrid_request_is_degraded_with_reason() {
        let outcome = RetrievalOutcome::from_evidence(
            RequestedMode::Hybrid,
            RetrievalEvidence {
                executed_path: Some(ExecutedPath::DenseOnly),
                examined_count: Some(1000),
                matched_count: 5,
                fallback_reason: None,
                stale_evidence: false,
            },
        );
        assert_eq!(outcome.completeness, RetrievalCompleteness::Degraded);
        assert!(
            outcome
                .fallback_reason
                .as_deref()
                .unwrap()
                .starts_with("degraded_execution_reason_missing"),
            "degraded execution without a caller reason must carry the canonical reason"
        );
    }

    #[test]
    fn lexical_request_with_lexical_execution_is_complete() {
        let outcome = RetrievalOutcome::from_evidence(
            RequestedMode::Lexical,
            RetrievalEvidence {
                executed_path: Some(ExecutedPath::LexicalOnly),
                examined_count: Some(40),
                matched_count: 3,
                fallback_reason: None,
                stale_evidence: false,
            },
        );
        assert_eq!(outcome.completeness, RetrievalCompleteness::Complete);
    }

    #[test]
    fn mark_partial_only_downgrades_complete() {
        let complete = RetrievalOutcome::from_evidence(
            RequestedMode::Hybrid,
            RetrievalEvidence {
                executed_path: Some(ExecutedPath::HybridFusion),
                examined_count: Some(123),
                matched_count: 1,
                fallback_reason: None,
                stale_evidence: false,
            },
        );
        assert_eq!(
            complete.mark_partial().completeness,
            RetrievalCompleteness::Partial
        );

        let degraded = RetrievalOutcome::from_evidence(
            RequestedMode::Hybrid,
            RetrievalEvidence {
                executed_path: Some(ExecutedPath::DenseOnly),
                examined_count: Some(1000),
                matched_count: 1,
                fallback_reason: Some("hybrid_unavailable: manifest stale".to_string()),
                stale_evidence: false,
            },
        );
        assert_eq!(
            degraded.mark_partial().completeness,
            RetrievalCompleteness::Degraded,
            "mark_partial must never soften a degraded outcome"
        );

        let unknown =
            RetrievalOutcome::from_evidence(RequestedMode::Hybrid, RetrievalEvidence::default());
        assert_eq!(
            unknown.mark_partial().completeness,
            RetrievalCompleteness::Unknown,
            "mark_partial must never soften an unknown outcome"
        );
    }

    #[test]
    fn explicit_none_execution_is_unknown_with_caller_reason_preserved() {
        let outcome = RetrievalOutcome::from_evidence(
            RequestedMode::Hybrid,
            RetrievalEvidence {
                executed_path: Some(ExecutedPath::None),
                examined_count: Some(0),
                matched_count: 0,
                fallback_reason: Some("legacy_caller: no manifest evidence".to_string()),
                stale_evidence: true,
            },
        );
        assert_eq!(outcome.completeness, RetrievalCompleteness::Unknown);
        assert_eq!(
            outcome.fallback_reason.as_deref(),
            Some("legacy_caller: no manifest evidence")
        );
        assert!(outcome.stale_evidence);
    }
}
