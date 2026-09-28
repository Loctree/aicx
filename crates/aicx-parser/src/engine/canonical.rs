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
use std::collections::BTreeMap;

pub const CANONICAL_SCHEMA: &str = "aicx.parser.canonical.v1";

#[derive(Serialize)]
struct CanonicalSession<'a> {
    schema: &'static str,
    session_id: &'a str,
    provenance: CanonicalProvenance<'a>,
    segments: Vec<CanonicalSegment<'a>>,
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

/// C0A segment fields only. `Segment::scope_status` (W2-R1) is derived
/// evidence and stays out of the frozen fingerprint.
///
/// `cwd` is here because it is the fact the rollout RECORDED.
/// `Segment::scope_root` and `scope_conflict` are deliberately absent: they
/// are this host's resolution of the recorded facts against the local
/// filesystem, so including them would give the same source bytes different
/// fingerprints on different machines — the opposite of what a canonical
/// projection is for.
#[derive(Serialize)]
struct CanonicalSegment<'a> {
    segment_id: u32,
    cwd: &'a Known<String>,
    branch: &'a Known<String>,
    started_at: &'a Known<String>,
    ended_at: &'a Known<String>,
    turn_range: super::model::TurnRange,
}

/// Segments as the RECORDED facts draw them, plus the model-segment →
/// canonical-segment remap that retargets `Turn::segment_id`.
///
/// An adapter may split a span where only derived scope changed: Codex cuts
/// a segment wherever a turn window's workdirs resolve to another checkout,
/// and whether they do depends on which checkouts exist on this disk.
/// Keeping those cuts would move `segment_id`, `turn_range` and every turn's
/// `segment_id` with the host's filesystem — the same drift that keeps
/// `scope_root` out. So adjacent segments that agree on the recorded `cwd`
/// and `branch` fold back into one. An adapter that only ever splits on
/// recorded drift never produces such a pair, and its projection is
/// unchanged.
fn canonical_segments(segments: &[Segment]) -> (Vec<CanonicalSegment<'_>>, BTreeMap<u32, u32>) {
    let mut folded: Vec<CanonicalSegment<'_>> = Vec::with_capacity(segments.len());
    let mut remap = BTreeMap::new();
    for segment in segments {
        match folded.last_mut() {
            Some(last) if last.cwd == &segment.cwd && last.branch == &segment.branch => {
                last.ended_at = &segment.ended_at;
                last.turn_range.end = segment.turn_range.end;
            }
            _ => folded.push(CanonicalSegment {
                segment_id: folded.len() as u32,
                cwd: &segment.cwd,
                branch: &segment.branch,
                started_at: &segment.started_at,
                ended_at: &segment.ended_at,
                turn_range: segment.turn_range,
            }),
        }
        remap.insert(segment.segment_id, folded.len().saturating_sub(1) as u32);
    }
    (folded, remap)
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
    let (segments, segment_remap) = canonical_segments(&model.segments);
    let canonical = CanonicalSession {
        schema: CANONICAL_SCHEMA,
        session_id: &model.session_id,
        provenance: canonical_provenance(&model.provenance),
        segments,
        skill_invocations: &model.skill_invocations,
        turns: model
            .turns
            .iter()
            .map(|turn| canonical_turn(turn, &segment_remap))
            .collect(),
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

fn canonical_turn<'a>(turn: &'a Turn, segment_remap: &BTreeMap<u32, u32>) -> CanonicalTurn<'a> {
    CanonicalTurn {
        turn_idx: turn.turn_idx,
        role: turn.role,
        timestamp: &turn.timestamp,
        kind: turn.kind,
        text_hash: &turn.text_hash,
        text_chars: turn.text_chars,
        tool_name: &turn.tool_name,
        // Validation guarantees every turn names a segment; the fallback only
        // keeps this total.
        segment_id: segment_remap
            .get(&turn.segment_id)
            .copied()
            .unwrap_or(turn.segment_id),
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
