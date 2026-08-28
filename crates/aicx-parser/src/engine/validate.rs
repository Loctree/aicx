//! Invariant validator. Projection is possible only through [`ValidatedSession`].
//!
//! W1-T5 contract: a model with `turns = []` is not a valid session. It used
//! to validate (silent empty success — grok `019fdeca`, every adapter's
//! metadata-only path). Now it is `Err` carrying
//! [`RefusalReason::EmptyConversation`] with the coverage evidence. The
//! coverage ledger must also be total (`CoverageReport::check_totality`).

use super::coverage::{
    CoverageReport, SkippedReason, TotalityError, VisibleCompleteness, WarningKind, ranges_for,
};
use super::identity::{PackageIdentity, evidence_event_id_from_hash, sha256_hex};
use super::model::{Known, RawUnitRef, SESSION_MODEL_SCHEMA, SessionModel, UsageEvent};
use super::refusal::RefusalReason;
use chrono::DateTime;
use std::collections::BTreeSet;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct UnvalidatedParse {
    pub model: Option<SessionModel>,
    pub coverage: CoverageReport,
}

impl UnvalidatedParse {
    pub fn from_model(model: SessionModel) -> Self {
        Self {
            coverage: model.coverage.clone(),
            model: Some(model),
        }
    }

    pub fn fatal(coverage: CoverageReport) -> Self {
        Self {
            model: None,
            coverage,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValidatedParse {
    Session(Box<ValidatedSession>),
    Fatal(FatalParse),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedSession {
    model: SessionModel,
}

impl ValidatedSession {
    pub fn model(&self) -> &SessionModel {
        &self.model
    }

    /// The (store id, source content hash) pair this session stands for —
    /// the identity of the **snapshot** read. Infallible: `validate_model`
    /// already checked both halves. For "which conversation" use
    /// [`ValidatedSession::conversation`].
    pub fn identity(&self) -> PackageIdentity {
        PackageIdentity {
            store_id: self.model.session_id.clone(),
            content_hash: self.model.provenance.original_source_hash.clone(),
        }
    }

    /// Provider-tagged conversation identity (W2-R1).
    pub fn conversation(&self) -> &super::model::ProviderConversationRef {
        &self.model.conversation
    }

    /// Bytes-level snapshot identity (W2-R1).
    pub fn snapshot(&self) -> &super::model::SourceSnapshotRef {
        &self.model.snapshot
    }

    pub fn into_model(self) -> SessionModel {
        self.model
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FatalParse {
    coverage: CoverageReport,
}

impl FatalParse {
    pub fn coverage(&self) -> &CoverageReport {
        &self.coverage
    }
}

pub fn validate_parse(parse: UnvalidatedParse) -> Result<ValidatedParse, ValidationError> {
    validate_coverage(&parse.coverage, parse.model.is_some())?;
    match parse.model {
        Some(model) => {
            if model.coverage != parse.coverage {
                return Err(error(
                    "coverage_single_source",
                    "model coverage differs from parse-envelope coverage",
                ));
            }
            validate_model(&model)?;
            Ok(ValidatedParse::Session(Box::new(ValidatedSession {
                model,
            })))
        }
        None => Ok(ValidatedParse::Fatal(FatalParse {
            coverage: parse.coverage,
        })),
    }
}

pub fn validate_coverage(
    coverage: &CoverageReport,
    model_projected: bool,
) -> Result<(), ValidationError> {
    if coverage.consumed_count != coverage.consumed.len() as u64
        || coverage.skipped_count != coverage.skipped.len() as u64
    {
        return Err(error(
            "coverage_counts",
            "reported coverage counts differ from record lengths",
        ));
    }
    if coverage.consumed_count.checked_add(coverage.skipped_count) != Some(coverage.raw_unit_count)
    {
        return Err(error(
            "coverage_partition_count",
            "consumed + skipped must equal raw_unit_count",
        ));
    }
    if coverage.raw_line_count > coverage.raw_unit_count {
        return Err(error(
            "physical_logical_counts",
            "raw_line_count cannot exceed raw_unit_count",
        ));
    }
    coverage
        .check_totality()
        .map_err(|totality| totality_error(coverage, totality))?;

    let mut ordinals = BTreeSet::new();
    let mut evidence_ids = BTreeSet::new();
    for unit in &coverage.consumed {
        validate_coverage_unit(
            coverage.raw_unit_count,
            unit.ordinal,
            &unit.evidence,
            &mut ordinals,
            &mut evidence_ids,
        )?;
        if unit.kind.is_empty() {
            return Err(error("consumed_kind", "consumed unit kind cannot be empty"));
        }
    }
    for unit in &coverage.skipped {
        validate_coverage_unit(
            coverage.raw_unit_count,
            unit.ordinal,
            &unit.evidence,
            &mut ordinals,
            &mut evidence_ids,
        )?;
        if unit.bytes != unit.evidence.original_bytes {
            return Err(error(
                "skipped_byte_count",
                "skipped-unit bytes differ from evidence bytes",
            ));
        }
    }
    if ordinals.len() as u64 != coverage.raw_unit_count
        || (1..=coverage.raw_unit_count).any(|ordinal| !ordinals.contains(&ordinal))
    {
        return Err(error(
            "coverage_gap",
            "coverage ordinals must be gap-free from 1 through raw_unit_count",
        ));
    }
    let expected_ranges = ranges_for(coverage.consumed.iter().map(|unit| unit.ordinal));
    if coverage.consumed_ranges != expected_ranges {
        return Err(error(
            "consumed_ranges",
            "consumed_ranges do not exactly encode consumed ordinals",
        ));
    }

    for warning in &coverage.warnings {
        if warning.count == 0
            || warning.first_ordinal == 0
            || warning.first_ordinal > coverage.raw_unit_count
        {
            return Err(error(
                "warning_accounting",
                "warning count and first ordinal must reference covered units",
            ));
        }
    }
    let warning_count = |kind| {
        coverage
            .warnings
            .iter()
            .filter(|warning| warning.kind == kind)
            .map(|warning| warning.count)
            .sum::<u64>()
    };
    let skipped_count = |reason| {
        coverage
            .skipped
            .iter()
            .filter(|unit| unit.reason == reason)
            .count() as u64
    };
    if warning_count(WarningKind::UnknownPayloadType)
        < skipped_count(SkippedReason::UnknownPayloadType)
    {
        return Err(error(
            "silent_unknown",
            "every unknown payload must emit a typed warning",
        ));
    }
    if warning_count(WarningKind::OversizedUnit) < skipped_count(SkippedReason::Oversized) {
        return Err(error(
            "silent_oversized",
            "every oversized unit must emit a typed warning",
        ));
    }
    if skipped_count(SkippedReason::EncryptedOpaque) > 0
        && !coverage.status.boundary_flags.opaque_reasoning_present
    {
        return Err(error(
            "opaque_boundary",
            "encrypted opaque units require opaque_reasoning_present",
        ));
    }
    if skipped_count(SkippedReason::CompactionReplay) > 0
        && !coverage.status.boundary_flags.compaction_boundary_present
    {
        return Err(error(
            "compaction_boundary",
            "compaction_replay skips require compaction_boundary_present",
        ));
    }
    let unsupported_visible = coverage.skipped.iter().any(|unit| {
        unit.visible
            && matches!(
                unit.reason,
                SkippedReason::Unsupported | SkippedReason::UnknownPayloadType
            )
    });
    if unsupported_visible && !coverage.status.boundary_flags.unsupported_visible_event {
        return Err(error(
            "unsupported_boundary",
            "preserved unsupported visible units require the boundary flag",
        ));
    }

    let warnings_total = coverage
        .warnings
        .iter()
        .map(|warning| warning.count)
        .sum::<u64>();
    let status = coverage.status;
    if status.malformed_tail_present
        && status.visible_completeness == VisibleCompleteness::CompleteVisible
    {
        return Err(error(
            "malformed_complete",
            "malformed tail forbids complete_visible",
        ));
    }
    if status.visible_completeness == VisibleCompleteness::PartialVisible
        && !status.malformed_tail_present
        && !status.visible_event_lost
    {
        return Err(error(
            "partial_without_loss",
            "partial_visible requires concrete visible loss",
        ));
    }
    if status.boundary_flags.unsupported_visible_event && warnings_total == 0 {
        return Err(error(
            "unsupported_without_warning",
            "unsupported visible boundary requires a warning",
        ));
    }
    if status.visible_completeness == VisibleCompleteness::Fatal && model_projected {
        return Err(error(
            "fatal_projection",
            "fatal parse cannot project or ingest a model",
        ));
    }
    if status.visible_completeness != VisibleCompleteness::Fatal && !model_projected {
        return Err(error(
            "missing_nonfatal_model",
            "non-fatal parse must preserve its valid model",
        ));
    }
    Ok(())
}

fn validate_coverage_unit(
    max_ordinal: u64,
    ordinal: u64,
    evidence: &RawUnitRef,
    ordinals: &mut BTreeSet<u64>,
    evidence_ids: &mut BTreeSet<String>,
) -> Result<(), ValidationError> {
    if ordinal == 0 || ordinal > max_ordinal || evidence.coverage_ordinal != ordinal {
        return Err(error(
            "coverage_ordinal",
            "coverage ordinal is out of range or differs from evidence",
        ));
    }
    if !ordinals.insert(ordinal) {
        return Err(error(
            "coverage_overlap",
            "a raw unit cannot be both consumed and skipped",
        ));
    }
    validate_evidence(evidence)?;
    if !evidence_ids.insert(evidence.evidence_event_id.clone()) {
        return Err(error(
            "evidence_uniqueness",
            "duplicate evidence_event_id is fatal",
        ));
    }
    Ok(())
}

pub fn validate_model(model: &SessionModel) -> Result<(), ValidationError> {
    if model.schema != SESSION_MODEL_SCHEMA {
        return Err(error("model_schema", "unknown SessionModel schema"));
    }
    if model.session_id.is_empty() {
        return Err(error("session_id", "session id cannot be empty"));
    }
    validate_hash("source_hash", &model.provenance.original_source_hash)?;
    validate_known_timestamp(&model.provenance.started_at)?;
    validate_known_timestamp(&model.provenance.ended_at)?;
    for evidence in model
        .coverage
        .consumed
        .iter()
        .map(|unit| &unit.evidence)
        .chain(model.coverage.skipped.iter().map(|unit| &unit.evidence))
    {
        let expected = evidence_event_id_from_hash(
            model.provenance.agent,
            &model.session_id,
            &evidence.locator,
            &evidence.unit_kind,
            &evidence.content_hash,
        )
        .map_err(|error| ValidationError {
            invariant: "evidence_derivation",
            detail: error.to_string(),
            refusal: None,
        })?;
        if evidence.evidence_event_id != expected {
            return Err(error(
                "evidence_derivation",
                "evidence_event_id does not match derivation v1",
            ));
        }
    }

    if model.turns.is_empty() {
        return Err(ValidationError::refusal(
            "empty_conversation",
            RefusalReason::empty_conversation(
                model.provenance.agent,
                model.session_id.clone(),
                &model.coverage,
            ),
        ));
    }

    let known_evidence: BTreeSet<_> = model
        .coverage
        .consumed
        .iter()
        .map(|unit| unit.evidence.evidence_event_id.as_str())
        .chain(
            model
                .coverage
                .skipped
                .iter()
                .map(|unit| unit.evidence.evidence_event_id.as_str()),
        )
        .collect();
    for (index, turn) in model.turns.iter().enumerate() {
        if turn.turn_idx != index as u64 {
            return Err(error(
                "turn_order",
                "turn indexes must be contiguous from zero",
            ));
        }
        if turn.text_chars != turn.text.chars().count() as u64
            || turn.text_hash != sha256_hex(turn.text.as_bytes())
        {
            return Err(error(
                "turn_text_integrity",
                "turn text hash or character count is invalid",
            ));
        }
        validate_known_timestamp(&turn.timestamp)?;
        for evidence in &turn.raw_unit_refs {
            validate_evidence_reference(evidence, &known_evidence)?;
        }
    }
    validate_segments(model)?;
    for invocation in &model.skill_invocations {
        if invocation.turn_idx >= model.turns.len() as u64 || invocation.skill_name.is_empty() {
            return Err(error(
                "skill_invocation",
                "skill invocation must reference an existing turn and named skill",
            ));
        }
        validate_hash("skill_payload_hash", &invocation.payload_hash)?;
        validate_known_timestamp(&invocation.first_invoked_at)?;
    }
    for tool in &model.tool_events {
        if tool.turn_idx >= model.turns.len() as u64 || tool.tool_name.is_empty() {
            return Err(error(
                "tool_event",
                "tool event must reference an existing turn and named tool",
            ));
        }
        validate_hash("tool_payload_hash", &tool.payload_hash)?;
        for evidence in &tool.raw_unit_refs {
            validate_evidence_reference(evidence, &known_evidence)?;
        }
    }
    for usage in &model.usage_events {
        validate_usage(usage)?;
        validate_evidence_reference(&usage.evidence, &known_evidence)?;
    }
    Ok(())
}

fn validate_segments(model: &SessionModel) -> Result<(), ValidationError> {
    // `turns = []` is refused upstream in `validate_model`; no empty-chat path.
    let mut next_turn = 0_u64;
    let mut segment_ids = BTreeSet::new();
    for segment in &model.segments {
        if !segment_ids.insert(segment.segment_id)
            || segment.turn_range.start != next_turn
            || segment.turn_range.end < segment.turn_range.start
            || segment.turn_range.end >= model.turns.len() as u64
        {
            return Err(error(
                "segment_ranges",
                "segment turn ranges must be unique, ordered, gap-free, and in range",
            ));
        }
        for turn in
            &model.turns[segment.turn_range.start as usize..=segment.turn_range.end as usize]
        {
            if turn.segment_id != segment.segment_id {
                return Err(error(
                    "segment_binding",
                    "turn segment_id differs from its covering segment",
                ));
            }
        }
        next_turn = segment.turn_range.end + 1;
    }
    if next_turn != model.turns.len() as u64 {
        return Err(error(
            "segment_coverage",
            "segments must cover every turn exactly once",
        ));
    }
    Ok(())
}

fn validate_usage(usage: &UsageEvent) -> Result<(), ValidationError> {
    if usage.provider.is_empty() {
        return Err(error("usage_provider", "usage provider cannot be empty"));
    }
    if let Known::Value(model) = &usage.model
        && model.is_empty()
    {
        return Err(error("usage_model", "known usage model cannot be empty"));
    }
    if let Known::Value(cost) = &usage.cost
        && (!cost.amount.is_finite() || cost.amount < 0.0 || cost.currency.is_empty())
    {
        return Err(error(
            "usage_cost",
            "reported cost requires finite non-negative amount and currency",
        ));
    }
    validate_known_timestamp(&usage.timestamp)?;
    if let Known::Value(span) = &usage.span {
        let start = parse_timestamp(&span.start)?;
        let end = parse_timestamp(&span.end)?;
        if end < start {
            return Err(error("usage_span", "usage span ends before it starts"));
        }
    }
    Ok(())
}

fn validate_evidence_reference(
    evidence: &RawUnitRef,
    known_evidence: &BTreeSet<&str>,
) -> Result<(), ValidationError> {
    validate_evidence(evidence)?;
    if !known_evidence.contains(evidence.evidence_event_id.as_str()) {
        return Err(error(
            "evidence_reference",
            "model references evidence absent from coverage",
        ));
    }
    Ok(())
}

fn validate_evidence(evidence: &RawUnitRef) -> Result<(), ValidationError> {
    if !evidence.evidence_event_id.starts_with("ev1:")
        || evidence.locator.is_empty()
        || evidence.unit_kind.is_empty()
        || evidence.artifact.is_empty()
        || evidence.artifact.contains(['/', '\\'])
    {
        return Err(error(
            "evidence_shape",
            "evidence identity/locator/kind/artifact shape is invalid",
        ));
    }
    validate_hash("evidence_content_hash", &evidence.content_hash)
}

fn validate_hash(label: &'static str, hash: &str) -> Result<(), ValidationError> {
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(error(label, "expected a 64-character hexadecimal SHA-256"));
    }
    Ok(())
}

fn validate_known_timestamp(timestamp: &Known<String>) -> Result<(), ValidationError> {
    if let Known::Value(timestamp) = timestamp {
        parse_timestamp(timestamp)?;
    }
    Ok(())
}

fn parse_timestamp(timestamp: &str) -> Result<DateTime<chrono::FixedOffset>, ValidationError> {
    DateTime::parse_from_rfc3339(timestamp)
        .map_err(|_| error("timestamp", "known timestamp must be RFC3339"))
}

fn error(invariant: &'static str, detail: impl Into<String>) -> ValidationError {
    ValidationError {
        invariant,
        detail: detail.into(),
        refusal: None,
    }
}

fn totality_error(coverage: &CoverageReport, totality: TotalityError) -> ValidationError {
    let invariant = match totality {
        TotalityError::ConsumedByKind { .. } => "ledger_consumed_by_kind",
        TotalityError::KnownSkipped { .. } => "ledger_known_skipped",
        TotalityError::Partition { .. } => "coverage_partition_count",
        TotalityError::LedgerDrift { .. } => "ledger_drift",
    };
    ValidationError {
        invariant,
        detail: format!(
            "{totality} (raw {} / consumed {} / skipped {})",
            coverage.raw_unit_count, coverage.consumed_count, coverage.skipped_count
        ),
        refusal: None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub invariant: &'static str,
    pub detail: String,
    /// Set when the failure is a typed refusal (the input was understood and
    /// rejected) rather than a broken invariant (the input was inconsistent).
    pub refusal: Option<Box<RefusalReason>>,
}

impl ValidationError {
    pub fn refusal(invariant: &'static str, refusal: RefusalReason) -> Self {
        Self {
            invariant,
            detail: refusal.to_string(),
            refusal: Some(Box::new(refusal)),
        }
    }

    pub const fn is_refusal(&self) -> bool {
        self.refusal.is_some()
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.invariant, self.detail)
    }
}

impl std::error::Error for ValidationError {}
