//! Deterministic parser kernel: explicit source → bounded read → sealed adapter
//! → exhaustive validation → projection-safe result.

pub mod canonical;
pub mod coverage;
pub mod identity;
pub mod model;
pub mod reader;
pub mod source;
pub mod validate;

pub use canonical::{CANONICAL_SCHEMA, canonical_bytes, canonical_fingerprint};
pub use coverage::{
    BoundaryFlags, ConsumedUnit, CoverageReport, CoverageWarning, OrdinalRange, ParseStatus,
    SkippedReason, SkippedUnit, VisibleCompleteness, WarningKind,
};
pub use identity::{
    EVIDENCE_ID_VERSION, EvidenceIdError, evidence_event_id, evidence_event_id_from_hash,
    ordinal_locator, sha256_hex,
};
pub use model::{
    CounterSemantics, Known, Provenance, RawUnitRef, ReportedCost, SESSION_MODEL_SCHEMA, Segment,
    SessionModel, SkillInvocation, TokenComponents, ToolEvent, ToolEventKind, Turn, TurnKind,
    TurnRange, TurnRole, UnknownValue, UsageEvent, UsageSpan,
};
pub use reader::{
    DEFAULT_MAX_SOURCE_BYTES, DEFAULT_MAX_UNIT_BYTES, RawUnit, RawUnitReader, ReaderError,
    ReaderPolicy, SourceRead, UnitBoundary,
};
pub use source::{AgentKind, SourceArtifact, SourceError, SourceFraming, SourceHandle};
pub use validate::{
    FatalParse, UnvalidatedParse, ValidatedParse, ValidatedSession, ValidationError,
    validate_coverage, validate_model, validate_parse,
};

use crate::adapters::{AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel};
use std::collections::BTreeSet;
use std::fmt;

#[derive(Debug, Clone)]
pub struct ParserEngine {
    reader: RawUnitReader,
}

impl Default for ParserEngine {
    fn default() -> Self {
        Self::new(ReaderPolicy::default())
    }
}

impl ParserEngine {
    pub const fn new(reader_policy: ReaderPolicy) -> Self {
        Self {
            reader: RawUnitReader::new(reader_policy),
        }
    }

    pub const fn reader_policy(&self) -> ReaderPolicy {
        self.reader.policy()
    }

    /// Parse exactly the artifacts already present in `source`.
    pub fn parse<A: AgentAdapter + ?Sized>(
        &self,
        source: &SourceHandle,
        adapter: &A,
    ) -> Result<ValidatedParse, EngineError> {
        if source.agent() != adapter.agent() {
            return Err(EngineError::AdapterMismatch {
                source: source.agent(),
                adapter: adapter.agent(),
            });
        }
        if adapter.adapter_version().is_empty() {
            return Err(EngineError::InvalidAdapterVersion);
        }
        let read = self.reader.read(source).map_err(EngineError::Reader)?;
        let classified = adapter
            .classify(source, &read)
            .map_err(EngineError::Adapter)?;
        validate_classification(source, &read, &classified)?;
        let classified_count = classified.len() as u64;
        let parse = adapter
            .assemble(source, &read, classified)
            .map_err(EngineError::Adapter)?;
        if parse.coverage.raw_line_count != read.units.len() as u64
            || parse.coverage.raw_unit_count != classified_count
        {
            return Err(classification_error(
                "assembled coverage counts differ from classified physical/logical units",
            ));
        }
        validate_parse(parse).map_err(EngineError::Validation)
    }
}

fn validate_classification(
    source: &SourceHandle,
    read: &SourceRead,
    classified: &[ClassifiedUnit],
) -> Result<(), EngineError> {
    let physical_count = classified
        .iter()
        .filter(|unit| unit.level == RawUnitLevel::Physical)
        .count();
    if physical_count != read.units.len() {
        return Err(classification_error(
            "physical classification count differs from reader unit count",
        ));
    }
    let mut ordinals = BTreeSet::new();
    let physical_ordinals: BTreeSet<_> = classified
        .iter()
        .filter(|unit| unit.level == RawUnitLevel::Physical)
        .map(|unit| unit.ordinal)
        .collect();
    for classification in classified {
        if !ordinals.insert(classification.ordinal) {
            return Err(classification_error("duplicate classified ordinal"));
        }
        let evidence = &classification.evidence;
        if evidence.coverage_ordinal != classification.ordinal {
            return Err(classification_error(
                "classified ordinal differs from evidence coverage ordinal",
            ));
        }
        match classification.level {
            RawUnitLevel::Physical => {
                let Some(raw) = read
                    .units
                    .iter()
                    .find(|unit| unit.coverage_ordinal == classification.ordinal)
                else {
                    return Err(classification_error(
                        "physical classified ordinal is out of range",
                    ));
                };
                if evidence.physical_ordinal != raw.physical_ordinal
                    || evidence.artifact != raw.artifact_name
                    || evidence.content_hash != raw.content_hash
                    || evidence.original_bytes != raw.original_bytes
                {
                    return Err(classification_error(
                        "physical classified evidence differs from bounded reader truth",
                    ));
                }
                if raw.boundary == UnitBoundary::Oversized
                    && !matches!(
                        &classification.disposition,
                        ClassifiedDisposition::Skipped {
                            reason: SkippedReason::Oversized,
                            ..
                        }
                    )
                {
                    return Err(classification_error(
                        "oversized raw unit must terminate as skipped(oversized)",
                    ));
                }
            }
            RawUnitLevel::Logical { parent_ordinal } => {
                if !physical_ordinals.contains(&parent_ordinal)
                    || classification.ordinal <= read.units.len() as u64
                {
                    return Err(classification_error(
                        "logical unit requires a physical parent and a post-physical ordinal",
                    ));
                }
            }
        }
        let session_id = source
            .logical_session_id()
            .unwrap_or_else(|| source.source_id());
        let expected_id = evidence_event_id_from_hash(
            source.agent(),
            session_id,
            &evidence.locator,
            &evidence.unit_kind,
            &evidence.content_hash,
        )
        .map_err(|error| classification_error(error.to_string()))?;
        if evidence.evidence_event_id != expected_id {
            return Err(classification_error(
                "evidence_event_id does not match derivation v1",
            ));
        }
        if let ClassifiedDisposition::Consumed { kind } = &classification.disposition
            && kind != &evidence.unit_kind
        {
            return Err(classification_error(
                "consumed kind differs from evidence unit kind",
            ));
        }
    }
    if ordinals.len() != classified.len()
        || (1..=classified.len() as u64).any(|ordinal| !ordinals.contains(&ordinal))
    {
        return Err(classification_error(
            "classified units must cover every reader ordinal exactly once",
        ));
    }
    Ok(())
}

fn classification_error(detail: impl Into<String>) -> EngineError {
    EngineError::Classification(detail.into())
}

#[derive(Debug)]
pub enum EngineError {
    AdapterMismatch {
        source: AgentKind,
        adapter: AgentKind,
    },
    InvalidAdapterVersion,
    Reader(ReaderError),
    Adapter(crate::adapters::AdapterError),
    Classification(String),
    Validation(ValidationError),
}

impl fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdapterMismatch { source, adapter } => write!(
                formatter,
                "source adapter mismatch: source={}, adapter={}",
                source.as_str(),
                adapter.as_str()
            ),
            Self::InvalidAdapterVersion => formatter.write_str("adapter version cannot be empty"),
            Self::Reader(error) => write!(formatter, "reader failed: {error}"),
            Self::Adapter(error) => error.fmt(formatter),
            Self::Classification(detail) => write!(formatter, "classification invalid: {detail}"),
            Self::Validation(error) => write!(formatter, "validation failed: {error}"),
        }
    }
}

impl std::error::Error for EngineError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::{
        AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel,
    };

    struct ContractAdapter;

    impl crate::adapters::sealed::Sealed for ContractAdapter {}

    impl AgentAdapter for ContractAdapter {
        fn agent(&self) -> AgentKind {
            AgentKind::Codex
        }

        fn adapter_version(&self) -> &'static str {
            "contract-test-v1"
        }

        fn classify(
            &self,
            source: &SourceHandle,
            read: &SourceRead,
        ) -> Result<Vec<ClassifiedUnit>, AdapterError> {
            let mut classified = Vec::new();
            for raw in &read.units {
                let locator = ordinal_locator(raw.physical_ordinal);
                let unit_kind = "message".to_owned();
                let evidence_event_id = evidence_event_id_from_hash(
                    source.agent(),
                    source.source_id(),
                    &locator,
                    &unit_kind,
                    &raw.content_hash,
                )
                .map_err(|error| AdapterError::new("classify", error.to_string()))?;
                classified.push(ClassifiedUnit {
                    ordinal: raw.coverage_ordinal,
                    level: RawUnitLevel::Physical,
                    evidence: RawUnitRef {
                        evidence_event_id,
                        coverage_ordinal: raw.coverage_ordinal,
                        physical_ordinal: raw.physical_ordinal,
                        locator,
                        unit_kind: unit_kind.clone(),
                        artifact: raw.artifact_name.clone(),
                        content_hash: raw.content_hash.clone(),
                        original_bytes: raw.original_bytes,
                    },
                    disposition: ClassifiedDisposition::Consumed { kind: unit_kind },
                });
            }
            for (index, raw) in read.units.iter().enumerate() {
                let ordinal = read.units.len() as u64 + index as u64 + 1;
                let locator = format!("{:06}:logical:0", raw.physical_ordinal);
                let unit_kind = "text_block".to_owned();
                let logical_bytes = b"logical";
                let content_hash = sha256_hex(logical_bytes);
                let evidence_event_id = evidence_event_id_from_hash(
                    source.agent(),
                    source.source_id(),
                    &locator,
                    &unit_kind,
                    &content_hash,
                )
                .map_err(|error| AdapterError::new("classify", error.to_string()))?;
                classified.push(ClassifiedUnit {
                    ordinal,
                    level: RawUnitLevel::Logical {
                        parent_ordinal: raw.coverage_ordinal,
                    },
                    evidence: RawUnitRef {
                        evidence_event_id,
                        coverage_ordinal: ordinal,
                        physical_ordinal: raw.physical_ordinal,
                        locator,
                        unit_kind: unit_kind.clone(),
                        artifact: raw.artifact_name.clone(),
                        content_hash,
                        original_bytes: logical_bytes.len() as u64,
                    },
                    disposition: ClassifiedDisposition::Consumed { kind: unit_kind },
                });
            }
            Ok(classified)
        }

        fn assemble(
            &self,
            source: &SourceHandle,
            read: &SourceRead,
            classified: Vec<ClassifiedUnit>,
        ) -> Result<UnvalidatedParse, AdapterError> {
            let raw_unit_count = classified.len() as u64;
            let consumed = classified
                .into_iter()
                .map(|unit| ConsumedUnit {
                    ordinal: unit.ordinal,
                    kind: unit.evidence.unit_kind.clone(),
                    evidence: unit.evidence,
                })
                .collect();
            let coverage = CoverageReport::with_raw_line_count(
                read.units.len() as u64,
                raw_unit_count,
                consumed,
                Vec::new(),
                Vec::new(),
                ParseStatus {
                    visible_completeness: VisibleCompleteness::CompleteVisible,
                    boundary_flags: BoundaryFlags::default(),
                    malformed_tail_present: false,
                    visible_event_lost: false,
                },
            );
            let provenance = Provenance {
                agent: source.agent(),
                model: Known::unknown(),
                cli_version: Known::unknown(),
                cwd: Known::unknown(),
                branch: Known::unknown(),
                started_at: Known::unknown(),
                ended_at: Known::unknown(),
                original_source_hash: read.source_hash.clone(),
                original_source_bytes: read.source_bytes,
            };
            Ok(UnvalidatedParse::from_model(SessionModel::new(
                source.source_id(),
                provenance,
                coverage,
            )))
        }
    }

    #[test]
    fn engine_runs_only_the_explicit_source_through_validation() {
        let artifact =
            SourceArtifact::memory("session.jsonl", b"{}\n".to_vec(), SourceFraming::JsonLines)
                .unwrap();
        let source = SourceHandle::new(AgentKind::Codex, "session", None, vec![artifact]).unwrap();
        let parsed = ParserEngine::default()
            .parse(&source, &ContractAdapter)
            .expect("validated parse");
        let ValidatedParse::Session(session) = parsed else {
            panic!("expected validated session");
        };
        assert_eq!(session.model().coverage.raw_line_count, 1);
        assert_eq!(session.model().coverage.raw_unit_count, 2);
    }
}
