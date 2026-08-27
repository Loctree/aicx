//! Typed, loud refusal. The engine never answers a conversation question with
//! `Ok(empty)`: when it cannot produce speech it says why, with the evidence
//! it examined, the threshold it applied and what it actually found.
//!
//! Contract origin (W1-T5, plan aicx-one-taxonomy-fusion-260827):
//! - grok `019fdeca` (2-line degenerate session) today yields exit 0,
//!   `Messages=0`, empty body. That is [`RefusalReason::EmptyConversation`].
//! - the resolver may today substitute a different `source_id` for a unique
//!   alias without saying so. That is [`SubstitutionError`].
//! - adapter choice must be a detect-with-evidence gate, not a path guess.
//!   That is [`AdapterDetection`] / [`RefusalReason::DetectionBelowThreshold`].
//!
//! Only the type contract lives here. Wiring into `extraction/mod.rs`,
//! `mcp_session.rs` and the resolver is W2-T12.

use super::coverage::{CoverageReport, SkippedReason};
use super::identity::PackageIdentity;
use super::source::AgentKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// Schema tag for serialized refusals (reports, MCP payloads, oracle runner).
pub const REFUSAL_SCHEMA: &str = "aicx.parser.refusal.v1";

/// Minimum number of visible turns a session must project before the engine
/// calls it a conversation. Below this the engine refuses, it does not
/// return an empty success.
pub const MIN_VISIBLE_TURNS: u64 = 1;

/// Minimum number of human utterances a `--user-only` projection must yield
/// before it is a human transcript rather than a monologue.
pub const MIN_HUMAN_UTTERANCES: u64 = 1;

/// What the engine examined, what it required and what it found.
///
/// Every refusal carries one of these. A refusal without evidence is a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusalEvidence {
    /// Number of raw units the reader produced for the source.
    pub raw_unit_count: u64,
    /// Units the adapter consumed, keyed by unit kind (mirror of
    /// `CoverageReport::consumed_by_kind`).
    pub consumed_by_kind: BTreeMap<String, u64>,
    /// Units the adapter knowingly skipped, keyed by reason (mirror of
    /// `CoverageReport::known_skipped`).
    pub known_skipped: BTreeMap<SkippedReason, u64>,
    /// The threshold that was applied (`MIN_VISIBLE_TURNS`,
    /// `MIN_HUMAN_UTTERANCES`, or a detection score).
    pub threshold: u64,
    /// What was actually measured against that threshold.
    pub found: u64,
    /// Human-readable trail of what was inspected, in order. Never empty.
    pub examined: Vec<String>,
}

impl RefusalEvidence {
    /// Build evidence straight from a coverage ledger so the refusal and the
    /// coverage report cannot disagree about what was consumed.
    pub fn from_coverage(
        coverage: &CoverageReport,
        threshold: u64,
        found: u64,
        examined: Vec<String>,
    ) -> Self {
        Self {
            raw_unit_count: coverage.raw_unit_count,
            consumed_by_kind: coverage.consumed_by_kind.clone(),
            known_skipped: coverage.known_skipped.clone(),
            threshold,
            found,
            examined,
        }
    }

    /// Total consumed units according to the evidence ledger.
    pub fn consumed_total(&self) -> u64 {
        self.consumed_by_kind.values().sum()
    }

    /// Total knowingly skipped units according to the evidence ledger.
    pub fn skipped_total(&self) -> u64 {
        self.known_skipped.values().sum()
    }

    /// The evidence is total when consumed + skipped == raw_unit_count.
    /// A refusal built on a non-total ledger is itself a defect.
    pub fn is_total(&self) -> bool {
        self.consumed_total()
            .checked_add(self.skipped_total())
            .is_some_and(|sum| sum == self.raw_unit_count)
    }
}

/// One detection probe an adapter ran against the source before claiming it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionProbe {
    /// Stable probe name (e.g. `session_meta_record`, `queue_operation_type`).
    pub name: String,
    /// Whether the probe matched.
    pub matched: bool,
    /// First coverage ordinal where it matched, if any.
    pub first_ordinal: Option<u64>,
}

/// Evidence-backed adapter claim over a source. Adapter selection is a gate
/// over these, never a guess from the file path or directory name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterDetection {
    pub agent: AgentKind,
    /// Probes that ran, in order. Never empty for a claiming adapter.
    pub probes: Vec<DetectionProbe>,
    /// Number of probes that matched.
    pub score: u64,
    /// Number of matched probes required for the claim to hold.
    pub threshold: u64,
}

impl AdapterDetection {
    pub fn new(agent: AgentKind, probes: Vec<DetectionProbe>, threshold: u64) -> Self {
        let score = probes.iter().filter(|probe| probe.matched).count() as u64;
        Self {
            agent,
            probes,
            score,
            threshold,
        }
    }

    /// The claim holds only when the score reaches the threshold.
    pub fn holds(&self) -> bool {
        self.threshold > 0 && self.score >= self.threshold
    }

    /// Turn a failed detection into its refusal, with the probe trail.
    pub fn refuse(&self) -> RefusalReason {
        RefusalReason::DetectionBelowThreshold {
            agent: self.agent,
            probes: self.probes.clone(),
            score: self.score,
            threshold: self.threshold,
        }
    }
}

/// Every way the engine refuses to hand back speech. One variant per case in
/// the oracle assertion manifest (`tests/fixtures/parser_engine/assertions.toml`).
///
/// Enforcement points:
/// - `EmptyConversation` — `engine/validate.rs` (this cut, replaces the
///   `turns=[]` acceptance).
/// - the rest — W2-T12 guardians at the extraction / MCP / resolver seams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum RefusalReason {
    /// Units were consumed but zero turns were projected. Oracle:
    /// `grok-019fdeca-typed-refusal` (2-line degenerate session).
    EmptyConversation {
        agent: AgentKind,
        session_id: String,
        evidence: RefusalEvidence,
    },
    /// Turns exist but none of them is human speech. Oracle:
    /// `junie-260528-user-only-usermsg` (no `UserPromptEvent` in the log),
    /// `gemini-9048328b-user-only-usermsg`.
    NoHumanUtterance {
        agent: AgentKind,
        session_id: String,
        evidence: RefusalEvidence,
    },
    /// Raw human records exist in the source but the projection dropped all
    /// of them. Oracle: `gemini-9048328b-raw-user-records` (1 raw user, 0
    /// projected) — the refusal says so instead of returning 0 quietly.
    HumanRecordsDropped {
        agent: AgentKind,
        session_id: String,
        raw_human_records: u64,
        projected_human_turns: u64,
        evidence: RefusalEvidence,
    },
    /// Every candidate utterance was a compaction replay or a duplicate body
    /// (A2: identical bodies under rotated `msg_*` ids). Oracle:
    /// `a2-01a042f9-compacted-no-new-utterances`.
    OnlyReplayedContent {
        agent: AgentKind,
        session_id: String,
        compaction_replay: u64,
        duplicate_body: u64,
        evidence: RefusalEvidence,
    },
    /// The projection filter removed every turn a valid model had. The caller
    /// learns "the conversation was filtered away", not "there was none".
    ProjectionFilteredAll {
        agent: AgentKind,
        session_id: String,
        turns_before_filter: u64,
        filter: String,
        evidence: RefusalEvidence,
    },
    /// No adapter reached its detection threshold for the source.
    DetectionBelowThreshold {
        agent: AgentKind,
        probes: Vec<DetectionProbe>,
        score: u64,
        threshold: u64,
    },
    /// More than one adapter reached its threshold; the engine does not pick.
    DetectionAmbiguous { claims: Vec<AdapterDetection> },
    /// The resolver would have answered with a different package than the
    /// one requested. Carries the full substitution record.
    Substitution(SubstitutionError),
    /// The coverage ledger is not total; refusing is safer than projecting
    /// from a report with a silent hole.
    LedgerNotTotal {
        agent: AgentKind,
        session_id: String,
        raw_unit_count: u64,
        consumed_total: u64,
        skipped_total: u64,
    },
}

impl RefusalReason {
    /// Stable machine tag, identical to the serde tag.
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::EmptyConversation { .. } => "empty_conversation",
            Self::NoHumanUtterance { .. } => "no_human_utterance",
            Self::HumanRecordsDropped { .. } => "human_records_dropped",
            Self::OnlyReplayedContent { .. } => "only_replayed_content",
            Self::ProjectionFilteredAll { .. } => "projection_filtered_all",
            Self::DetectionBelowThreshold { .. } => "detection_below_threshold",
            Self::DetectionAmbiguous { .. } => "detection_ambiguous",
            Self::Substitution(_) => "substitution",
            Self::LedgerNotTotal { .. } => "ledger_not_total",
        }
    }

    /// Evidence trail when the variant carries one.
    pub const fn evidence(&self) -> Option<&RefusalEvidence> {
        match self {
            Self::EmptyConversation { evidence, .. }
            | Self::NoHumanUtterance { evidence, .. }
            | Self::HumanRecordsDropped { evidence, .. }
            | Self::OnlyReplayedContent { evidence, .. }
            | Self::ProjectionFilteredAll { evidence, .. } => Some(evidence),
            Self::DetectionBelowThreshold { .. }
            | Self::DetectionAmbiguous { .. }
            | Self::Substitution(_)
            | Self::LedgerNotTotal { .. } => None,
        }
    }

    /// The `EmptyConversation` refusal built from a validated-but-empty model
    /// ledger. Used by `validate.rs`; W2 adapters must not build their own.
    pub fn empty_conversation(
        agent: AgentKind,
        session_id: impl Into<String>,
        coverage: &CoverageReport,
    ) -> Self {
        let examined = vec![
            format!("raw_unit_count={}", coverage.raw_unit_count),
            format!("consumed_count={}", coverage.consumed_count),
            format!("skipped_count={}", coverage.skipped_count),
            format!(
                "consumed_kinds={}",
                coverage
                    .consumed_by_kind
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            format!("visible_completeness={:?}", coverage.status.visible_completeness),
        ];
        Self::EmptyConversation {
            agent,
            session_id: session_id.into(),
            evidence: RefusalEvidence::from_coverage(coverage, MIN_VISIBLE_TURNS, 0, examined),
        }
    }
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyConversation {
                agent,
                session_id,
                evidence,
            } => write!(
                formatter,
                "refused: {} session {session_id} projected {} turn(s); threshold {} (raw {} units, consumed {}, skipped {})",
                agent.as_str(),
                evidence.found,
                evidence.threshold,
                evidence.raw_unit_count,
                evidence.consumed_total(),
                evidence.skipped_total(),
            ),
            Self::NoHumanUtterance {
                agent,
                session_id,
                evidence,
            } => write!(
                formatter,
                "refused: {} session {session_id} has {} turn(s) but no human utterance (threshold {})",
                agent.as_str(),
                evidence.found,
                evidence.threshold,
            ),
            Self::HumanRecordsDropped {
                agent,
                session_id,
                raw_human_records,
                projected_human_turns,
                ..
            } => write!(
                formatter,
                "refused: {} session {session_id} has {raw_human_records} raw human record(s) but projected {projected_human_turns}",
                agent.as_str(),
            ),
            Self::OnlyReplayedContent {
                agent,
                session_id,
                compaction_replay,
                duplicate_body,
                ..
            } => write!(
                formatter,
                "refused: {} session {session_id} carries only replayed content (compaction_replay={compaction_replay}, duplicate_body={duplicate_body})",
                agent.as_str(),
            ),
            Self::ProjectionFilteredAll {
                agent,
                session_id,
                turns_before_filter,
                filter,
                ..
            } => write!(
                formatter,
                "refused: {} session {session_id} had {turns_before_filter} turn(s); filter `{filter}` removed all of them",
                agent.as_str(),
            ),
            Self::DetectionBelowThreshold {
                agent,
                score,
                threshold,
                probes,
            } => write!(
                formatter,
                "refused: adapter {} scored {score}/{threshold} over {} probe(s)",
                agent.as_str(),
                probes.len(),
            ),
            Self::DetectionAmbiguous { claims } => write!(
                formatter,
                "refused: {} adapters claim the source: {}",
                claims.len(),
                claims
                    .iter()
                    .map(|claim| claim.agent.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            Self::Substitution(error) => write!(formatter, "refused: {error}"),
            Self::LedgerNotTotal {
                agent,
                session_id,
                raw_unit_count,
                consumed_total,
                skipped_total,
            } => write!(
                formatter,
                "refused: {} session {session_id} ledger not total: consumed {consumed_total} + skipped {skipped_total} != raw {raw_unit_count}",
                agent.as_str(),
            ),
        }
    }
}

impl std::error::Error for RefusalReason {}

/// How the resolver matched the requested handle.
///
/// Mirrors the root-crate `MatchKind` taxonomy so the refusal can carry it
/// without importing the root crate. Mapping is W2-T12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubstitutionKind {
    /// Full `source_id` match — never a substitution.
    ExactSourceId,
    /// Unique alias match. Allowed only when the caller opted into it
    /// explicitly; otherwise it is a substitution.
    ExactAlias,
    /// Unique prefix match on the store id.
    UniquePrefix,
    /// Catalog-side prefix resolution without a `MatchKind`
    /// (`catalog::resolve_session`).
    CatalogPrefix,
}

/// The resolver would answer with a package other than the one requested.
///
/// This is an error type, not a warning: substitution has to be explicit on
/// the request (`allow: Some(kind)`), otherwise the resolver refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubstitutionError {
    /// The handle the caller asked for, verbatim.
    pub requested: String,
    /// How the resolver matched it.
    pub kind: SubstitutionKind,
    /// The identity the resolver would have returned instead.
    pub resolved: PackageIdentity,
    /// Substitution kinds the caller explicitly allowed (empty = none).
    pub allowed: Vec<SubstitutionKind>,
    /// Other candidates seen while resolving, as (store_id, content_hash).
    pub candidates: Vec<PackageIdentity>,
}

impl SubstitutionError {
    /// A match is a substitution unless it is exact or explicitly allowed.
    pub fn is_substitution(&self) -> bool {
        self.kind != SubstitutionKind::ExactSourceId && !self.allowed.contains(&self.kind)
    }
}

impl fmt::Display for SubstitutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "resolver would substitute `{}` -> {} via {:?} ({} candidate(s); allowed: {:?})",
            self.requested,
            self.resolved,
            self.kind,
            self.candidates.len(),
            self.allowed,
        )
    }
}

impl std::error::Error for SubstitutionError {}

impl From<SubstitutionError> for RefusalReason {
    fn from(error: SubstitutionError) -> Self {
        Self::Substitution(error)
    }
}

#[cfg(test)]
mod tests {
    // Structural tests only; not run under the W1 compile embargo.
    use super::*;
    use crate::engine::coverage::{BoundaryFlags, ParseStatus, VisibleCompleteness};

    fn empty_coverage(raw_unit_count: u64) -> CoverageReport {
        CoverageReport::new(
            raw_unit_count,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ParseStatus {
                visible_completeness: VisibleCompleteness::CompleteVisible,
                boundary_flags: BoundaryFlags::default(),
                malformed_tail_present: false,
                visible_event_lost: false,
            },
        )
    }

    #[test]
    fn empty_conversation_carries_threshold_and_found() {
        let refusal = RefusalReason::empty_conversation(AgentKind::Grok, "019fdeca", &empty_coverage(0));
        assert_eq!(refusal.tag(), "empty_conversation");
        let evidence = refusal.evidence().expect("evidence");
        assert_eq!(evidence.threshold, MIN_VISIBLE_TURNS);
        assert_eq!(evidence.found, 0);
        assert!(!evidence.examined.is_empty());
        assert!(evidence.is_total());
    }

    #[test]
    fn detection_below_threshold_refuses_with_probe_trail() {
        let detection = AdapterDetection::new(
            AgentKind::Codex,
            vec![DetectionProbe {
                name: "session_meta_record".to_owned(),
                matched: false,
                first_ordinal: None,
            }],
            1,
        );
        assert!(!detection.holds());
        assert_eq!(detection.refuse().tag(), "detection_below_threshold");
    }

    #[test]
    fn exact_alias_is_a_substitution_unless_allowed() {
        let resolved = PackageIdentity::new(
            "store-1",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )
        .expect("identity");
        let mut error = SubstitutionError {
            requested: "alias".to_owned(),
            kind: SubstitutionKind::ExactAlias,
            resolved,
            allowed: Vec::new(),
            candidates: Vec::new(),
        };
        assert!(error.is_substitution());
        error.allowed.push(SubstitutionKind::ExactAlias);
        assert!(!error.is_substitution());
    }
}
