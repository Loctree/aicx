//! Byte-stable serialization of C0A normative fields only.

use super::coverage::{
    BoundaryFlags, CoverageReport, CoverageWarning, OrdinalRange, ParseStatus, SkippedReason,
    VisibleCompleteness,
};
use super::identity::sha256_hex;
use super::model::{
    CounterSemantics, Known, Provenance, ReportedCost, Segment, SkillInvocation, TokenComponents,
    Turn, TurnKind, TurnRole, UsageEvent, UsageSpan,
};
use super::source::AgentKind;
use super::validate::ValidatedSession;
use serde::Serialize;

pub const CANONICAL_SCHEMA: &str = "aicx.parser.canonical.v1";

#[derive(Serialize)]
struct CanonicalSession<'a> {
    schema: &'static str,
    session_id: &'a str,
    provenance: CanonicalProvenance<'a>,
    segments: &'a [Segment],
    skill_invocations: &'a [SkillInvocation],
    turns: Vec<CanonicalTurn<'a>>,
    usage_events: Vec<CanonicalUsageEvent<'a>>,
    parser_coverage: CanonicalCoverage<'a>,
}

#[derive(Serialize)]
struct CanonicalProvenance<'a> {
    agent: AgentKind,
    model: &'a Known<String>,
    cli_version: &'a Known<String>,
    cwd: &'a Known<String>,
    branch: &'a Known<String>,
    started_at: &'a Known<String>,
    ended_at: &'a Known<String>,
    original_jsonl_hash: &'a str,
    original_jsonl_bytes: u64,
}

#[derive(Serialize)]
struct CanonicalTurn<'a> {
    turn_idx: u64,
    role: TurnRole,
    timestamp: &'a Known<String>,
    kind: TurnKind,
    text_hash: &'a str,
    text_chars: u64,
    tool_name: &'a Known<String>,
    segment_id: u32,
    raw_line_nos: Vec<u64>,
    evidence_event_ids: Vec<&'a str>,
}

#[derive(Serialize)]
struct CanonicalUsageEvent<'a> {
    provider: &'a str,
    model: &'a Known<String>,
    tokens: &'a TokenComponents,
    cost: &'a Known<ReportedCost>,
    timestamp: &'a Known<String>,
    span: &'a Known<UsageSpan>,
    counter_semantics: CounterSemantics,
    evidence_event_id: &'a str,
}

#[derive(Serialize)]
struct CanonicalCoverage<'a> {
    raw_line_count: u64,
    raw_unit_count: u64,
    consumed_count: u64,
    skipped_count: u64,
    consumed_ranges: &'a [OrdinalRange],
    consumed_evidence_event_ids: Vec<&'a str>,
    skipped_lines: Vec<CanonicalSkipped<'a>>,
    warnings: &'a [CoverageWarning],
    visible_completeness: VisibleCompleteness,
    boundary_flags: BoundaryFlags,
    malformed_tail_present: bool,
    visible_event_lost: bool,
}

#[derive(Serialize)]
struct CanonicalSkipped<'a> {
    line_no: u64,
    reason: SkippedReason,
    bytes: u64,
    evidence_event_id: &'a str,
}

pub fn canonical_bytes(session: &ValidatedSession) -> Result<Vec<u8>, serde_json::Error> {
    let model = session.model();
    let canonical = CanonicalSession {
        schema: CANONICAL_SCHEMA,
        session_id: &model.session_id,
        provenance: canonical_provenance(&model.provenance),
        segments: &model.segments,
        skill_invocations: &model.skill_invocations,
        turns: model.turns.iter().map(canonical_turn).collect(),
        usage_events: model.usage_events.iter().map(canonical_usage).collect(),
        parser_coverage: canonical_coverage(&model.coverage),
    };
    serde_json::to_vec(&canonical)
}

pub fn canonical_fingerprint(session: &ValidatedSession) -> Result<String, serde_json::Error> {
    Ok(sha256_hex(&canonical_bytes(session)?))
}

fn canonical_provenance(provenance: &Provenance) -> CanonicalProvenance<'_> {
    CanonicalProvenance {
        agent: provenance.agent,
        model: &provenance.model,
        cli_version: &provenance.cli_version,
        cwd: &provenance.cwd,
        branch: &provenance.branch,
        started_at: &provenance.started_at,
        ended_at: &provenance.ended_at,
        original_jsonl_hash: &provenance.original_source_hash,
        original_jsonl_bytes: provenance.original_source_bytes,
    }
}

fn canonical_turn(turn: &Turn) -> CanonicalTurn<'_> {
    CanonicalTurn {
        turn_idx: turn.turn_idx,
        role: turn.role,
        timestamp: &turn.timestamp,
        kind: turn.kind,
        text_hash: &turn.text_hash,
        text_chars: turn.text_chars,
        tool_name: &turn.tool_name,
        segment_id: turn.segment_id,
        raw_line_nos: turn
            .raw_unit_refs
            .iter()
            .map(|reference| reference.physical_ordinal)
            .collect(),
        evidence_event_ids: turn
            .raw_unit_refs
            .iter()
            .map(|reference| reference.evidence_event_id.as_str())
            .collect(),
    }
}

fn canonical_usage(usage: &UsageEvent) -> CanonicalUsageEvent<'_> {
    CanonicalUsageEvent {
        provider: &usage.provider,
        model: &usage.model,
        tokens: &usage.tokens,
        cost: &usage.cost,
        timestamp: &usage.timestamp,
        span: &usage.span,
        counter_semantics: usage.counter_semantics,
        evidence_event_id: &usage.evidence.evidence_event_id,
    }
}

fn canonical_coverage(coverage: &CoverageReport) -> CanonicalCoverage<'_> {
    let ParseStatus {
        visible_completeness,
        boundary_flags,
        malformed_tail_present,
        visible_event_lost,
    } = coverage.status;
    CanonicalCoverage {
        raw_line_count: coverage.raw_line_count,
        raw_unit_count: coverage.raw_unit_count,
        consumed_count: coverage.consumed_count,
        skipped_count: coverage.skipped_count,
        consumed_ranges: &coverage.consumed_ranges,
        consumed_evidence_event_ids: coverage
            .consumed
            .iter()
            .map(|unit| unit.evidence.evidence_event_id.as_str())
            .collect(),
        skipped_lines: coverage
            .skipped
            .iter()
            .map(|unit| CanonicalSkipped {
                line_no: unit.ordinal,
                reason: unit.reason,
                bytes: unit.bytes,
                evidence_event_id: &unit.evidence.evidence_event_id,
            })
            .collect(),
        warnings: &coverage.warnings,
        visible_completeness,
        boundary_flags,
        malformed_tail_present,
        visible_event_lost,
    }
}
