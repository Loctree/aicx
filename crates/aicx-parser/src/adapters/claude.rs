//! Claude Code project-session adapter for the deterministic parser kernel.
//!
//! Source truth: one `~/.claude/projects/<encoded-cwd>/<uuid>.jsonl` file,
//! JSONL-framed. Every physical line terminates as exactly one
//! `consumed(kind)` XOR `skipped(reason)`; recognized `message.content[]`
//! blocks of assistant/user rows become logical raw units per the frozen
//! taxonomy (`tests/parser_oracle/raw_unit_taxonomy.toml`).
//!
//! Explicit non-session contract: `~/.claude/history.jsonl` rows
//! (`display`/`pastedContents`, no `type`, no `sessionId`) are NOT a Claude
//! session. This adapter never conflates them: rows without a recognized
//! `type` are skipped as unknown payloads, and a source in which no parsed
//! row carries a `sessionId` terminates as a fatal parse (donor rule:
//! "no sessionId found — not a Claude session"). History import, if it is
//! ever revived, is a separate non-session importer behind the locator
//! boundary — never this adapter.
//!
//! Divergences from the donor (`tb_core/claude_jsonl.py`), each deliberate:
//! - Donor truncates skill-invocation payloads to a 200-char preview; the
//!   kernel keeps full turn text (previews are projection-layer, and literal
//!   operator content is never dropped by the parser).
//! - Donor consumes Cowork/desktop `rate_limit_event`/`result` rows; the
//!   frozen C0A taxonomy does not declare them for the session adapter, so
//!   they terminate as skipped(unknown_payload_type) with a typed warning.
//! - Donor does not model usage; `message.usage` becomes typed `UsageEvent`s
//!   with delta semantics per the C0A §4 normative extension.

use super::{AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel};
use crate::engine::{
    AgentKind, BoundaryFlags, ConsumedUnit, CounterSemantics, CoverageReport, CoverageWarning,
    Known, ParseStatus, Provenance, RawUnit, RawUnitRef, ReportedCost, Segment, SessionModel,
    SkillInvocation, SkippedReason, SkippedUnit, SourceFraming, SourceHandle, SourceRead,
    TokenComponents, ToolEvent, ToolEventKind, Turn, TurnKind, TurnRange, TurnRole, UnitBoundary,
    UnvalidatedParse, UsageEvent, VisibleCompleteness, WarningKind, evidence_event_id_from_hash,
    ordinal_locator, sha256_hex,
};
use crate::skill_collapse::detect_skill_marker;
use serde_json::Value;
use std::collections::BTreeMap;

pub const CLAUDE_ADAPTER_VERSION: &str = "claude-adapter-v1";

/// Anthropic usage provider token for typed `UsageEvent`s.
const USAGE_PROVIDER: &str = "anthropic";

/// Locally synthesized turns (interrupts, API-error placeholders) carry this
/// model marker; it is not real provenance.
const SYNTHETIC_MODEL: &str = "<synthetic>";

/// Recognized non-conversational record types, consumed as `metadata_record`
/// (frozen taxonomy list — recognized means consumed, never skipped).
const METADATA_TYPES: [&str; 9] = [
    "summary",
    "attachment",
    "file-history-snapshot",
    "pr-link",
    "queue-operation",
    "ai-title",
    "mode",
    "permission-mode",
    "last-prompt",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeAdapter;

impl super::sealed::Sealed for ClaudeAdapter {}

impl AgentAdapter for ClaudeAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn adapter_version(&self) -> &'static str {
        CLAUDE_ADAPTER_VERSION
    }

    fn classify(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
    ) -> Result<Vec<ClassifiedUnit>, AdapterError> {
        Ok(analyze(source, read)?.classified)
    }

    fn assemble(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
        classified: Vec<ClassifiedUnit>,
    ) -> Result<UnvalidatedParse, AdapterError> {
        let analysis = analyze(source, read)?;
        if analysis.classified != classified {
            return Err(AdapterError::new(
                "assemble",
                "classified units drifted between classify and assemble",
            ));
        }
        Ok(analysis.into_parse(source, read))
    }
}

// ---------------------------------------------------------------------------
// Deterministic single-pass analysis
// ---------------------------------------------------------------------------

struct Analysis {
    classified: Vec<ClassifiedUnit>,
    consumed: Vec<ConsumedUnit>,
    skipped: Vec<SkippedUnit>,
    warnings: Vec<CoverageWarning>,
    turns: Vec<Turn>,
    tool_events: Vec<ToolEvent>,
    usage_events: Vec<UsageEvent>,
    skill_invocations: Vec<SkillInvocation>,
    segments: Vec<SegmentDraft>,
    session_id_seen: bool,
    model: Known<String>,
    cli_version: Known<String>,
    first_cwd: Known<String>,
    first_branch: Known<String>,
    started_at: Known<String>,
    ended_at: Known<String>,
    opaque_reasoning_present: bool,
    unsupported_visible_event: bool,
    malformed_tail_present: bool,
    visible_event_lost: bool,
}

struct SegmentDraft {
    cwd: Known<String>,
    branch: Known<String>,
    first_turn: Option<u64>,
    last_turn: u64,
    started_at: Known<String>,
    ended_at: Known<String>,
}

struct Ctx<'a> {
    agent: AgentKind,
    session_id: &'a str,
    next_logical_ordinal: u64,
    tool_names: BTreeMap<String, String>,
}

fn analyze(source: &SourceHandle, read: &SourceRead) -> Result<Analysis, AdapterError> {
    if source.artifacts().len() != 1 {
        return Err(AdapterError::new(
            "classify",
            "a Claude session is exactly one JSONL artifact; multi-artifact grouping is locator-owned",
        ));
    }
    if source.artifacts()[0].framing() != SourceFraming::JsonLines {
        return Err(AdapterError::new(
            "classify",
            "claude session artifacts must use json_lines framing",
        ));
    }

    let session_id = source
        .logical_session_id()
        .unwrap_or_else(|| source.source_id());
    let mut ctx = Ctx {
        agent: source.agent(),
        session_id,
        next_logical_ordinal: read.units.len() as u64 + 1,
        tool_names: BTreeMap::new(),
    };
    let mut analysis = Analysis {
        classified: Vec::new(),
        consumed: Vec::new(),
        skipped: Vec::new(),
        warnings: Vec::new(),
        turns: Vec::new(),
        tool_events: Vec::new(),
        usage_events: Vec::new(),
        skill_invocations: Vec::new(),
        segments: Vec::new(),
        session_id_seen: false,
        model: Known::unknown(),
        cli_version: Known::unknown(),
        first_cwd: Known::unknown(),
        first_branch: Known::unknown(),
        started_at: Known::unknown(),
        ended_at: Known::unknown(),
        opaque_reasoning_present: false,
        unsupported_visible_event: false,
        malformed_tail_present: false,
        visible_event_lost: false,
    };
    analysis.segments.push(SegmentDraft {
        cwd: Known::unknown(),
        branch: Known::unknown(),
        first_turn: None,
        last_turn: 0,
        started_at: Known::unknown(),
        ended_at: Known::unknown(),
    });

    // Logical units must come after every physical ordinal, so the walk
    // buffers logical classifications and appends them after the physical run.
    let mut logical: Vec<ClassifiedUnit> = Vec::new();
    for raw in &read.units {
        walk_physical_unit(raw, &mut ctx, &mut analysis, &mut logical)?;
    }
    analysis.classified.extend(logical);
    Ok(analysis)
}

fn walk_physical_unit(
    raw: &RawUnit,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    if raw.boundary == UnitBoundary::Oversized {
        // The bounded bytes are a prefix; content is lost for parsing.
        analysis.visible_event_lost = true;
        warn(analysis, WarningKind::OversizedUnit, raw.coverage_ordinal);
        return skip_physical(
            raw,
            "oversized",
            SkippedReason::Oversized,
            true,
            ctx,
            analysis,
        );
    }

    let text = std::str::from_utf8(&raw.bytes).ok();
    let parsed = text.and_then(|value| serde_json::from_str::<Value>(value).ok());
    let is_blank = text.is_some_and(|value| value.trim().is_empty());
    let unterminated = raw.boundary == UnitBoundary::UnterminatedTail;

    let Some(value) = parsed else {
        if is_blank {
            // A blank line carries no visible event; it is an unknown payload,
            // not a loss.
            warn(
                analysis,
                WarningKind::UnknownPayloadType,
                raw.coverage_ordinal,
            );
            return skip_physical(
                raw,
                "unknown",
                SkippedReason::UnknownPayloadType,
                false,
                ctx,
                analysis,
            );
        }
        // Malformed JSON is concrete visible loss; at an unterminated tail it
        // is the truncated-write signature.
        if unterminated {
            analysis.malformed_tail_present = true;
        } else {
            analysis.visible_event_lost = true;
        }
        warn(analysis, WarningKind::MalformedUnit, raw.coverage_ordinal);
        return skip_physical(
            raw,
            "malformed",
            SkippedReason::Malformed,
            true,
            ctx,
            analysis,
        );
    };

    if unterminated {
        // Parsed cleanly despite the missing terminator: preserved, flagged.
        warn(
            analysis,
            WarningKind::UnterminatedTail,
            raw.coverage_ordinal,
        );
    }

    let Some(object) = value.as_object() else {
        analysis.unsupported_visible_event = true;
        warn(
            analysis,
            WarningKind::UnknownPayloadType,
            raw.coverage_ordinal,
        );
        return skip_physical(
            raw,
            "unknown",
            SkippedReason::UnknownPayloadType,
            true,
            ctx,
            analysis,
        );
    };

    if string_field(object, "sessionId").is_some() || string_field(object, "session_id").is_some() {
        analysis.session_id_seen = true;
    }
    if analysis.cli_version == Known::unknown()
        && let Some(version) = string_field(object, "version")
    {
        analysis.cli_version = Known::value(version.to_owned());
    }
    if analysis.first_cwd == Known::unknown()
        && let Some(cwd) = string_field(object, "cwd")
    {
        analysis.first_cwd = Known::value(cwd.to_owned());
    }
    if analysis.first_branch == Known::unknown()
        && let Some(branch) = string_field(object, "gitBranch")
    {
        analysis.first_branch = Known::value(branch.to_owned());
    }
    let timestamp = known_timestamp(string_field(object, "timestamp"));
    if let Known::Value(ts) = &timestamp {
        if analysis.started_at == Known::unknown() {
            analysis.started_at = Known::value(ts.clone());
        }
        analysis.ended_at = Known::value(ts.clone());
    }

    let top_type = string_field(object, "type").unwrap_or_default();
    match top_type {
        "user" | "assistant" => {
            consume_physical(raw, top_type, ctx, analysis)?;
            maybe_split_segment(object, analysis);
            walk_conversational_row(raw, object, top_type, &timestamp, ctx, analysis, logical)?;
        }
        "system" => {
            // Consumed as session context metadata, never a chat turn.
            consume_physical(raw, "system", ctx, analysis)?;
            backfill_segment(object, analysis);
        }
        kind if METADATA_TYPES.contains(&kind) => {
            consume_physical(raw, "metadata_record", ctx, analysis)?;
            backfill_segment(object, analysis);
        }
        _ => {
            analysis.unsupported_visible_event = true;
            warn(
                analysis,
                WarningKind::UnknownPayloadType,
                raw.coverage_ordinal,
            );
            skip_physical(
                raw,
                "unknown",
                SkippedReason::UnknownPayloadType,
                true,
                ctx,
                analysis,
            )?;
        }
    }
    Ok(())
}

fn walk_conversational_row(
    raw: &RawUnit,
    object: &serde_json::Map<String, Value>,
    top_type: &str,
    timestamp: &Known<String>,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    let physical_ref = physical_evidence(raw, top_type, ctx)?;
    let Some(message) = object.get("message").and_then(Value::as_object) else {
        if object.get("message").is_some() {
            analysis.unsupported_visible_event = true;
            warn(
                analysis,
                WarningKind::UnknownPayloadType,
                raw.coverage_ordinal,
            );
        }
        return Ok(());
    };

    if top_type == "assistant" {
        if analysis.model == Known::unknown()
            && let Some(model) = string_field(message, "model")
            && model != SYNTHETIC_MODEL
        {
            analysis.model = Known::value(model.to_owned());
        }
        emit_usage_event(object, message, timestamp, &physical_ref, analysis);
    }

    let role = string_field(message, "role").unwrap_or(top_type);
    match message.get("content") {
        None => Ok(()),
        Some(Value::String(text)) => {
            emit_text_turn(role, text, timestamp, vec![physical_ref], analysis);
            Ok(())
        }
        Some(Value::Array(blocks)) => {
            for (index, block) in blocks.iter().enumerate() {
                walk_content_block(
                    raw,
                    &physical_ref,
                    top_type,
                    role,
                    index,
                    block,
                    timestamp,
                    ctx,
                    analysis,
                    logical,
                )?;
            }
            Ok(())
        }
        Some(_) => {
            analysis.unsupported_visible_event = true;
            warn(
                analysis,
                WarningKind::UnknownPayloadType,
                raw.coverage_ordinal,
            );
            Ok(())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_content_block(
    raw: &RawUnit,
    physical_ref: &RawUnitRef,
    top_type: &str,
    role: &str,
    block_index: usize,
    block: &Value,
    timestamp: &Known<String>,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    // Bare string elements are the donor's synthetic text blocks: turn truth
    // lives in the physical line, no separate logical unit exists.
    if let Value::String(text) = block {
        emit_text_turn(role, text, timestamp, vec![physical_ref.clone()], analysis);
        return Ok(());
    }
    let Some(object) = block.as_object() else {
        analysis.unsupported_visible_event = true;
        warn(
            analysis,
            WarningKind::UnknownPayloadType,
            raw.coverage_ordinal,
        );
        return Ok(());
    };
    let block_type = string_field(object, "type").unwrap_or_default();

    match (top_type, block_type) {
        ("user", "text") => {
            // Frozen taxonomy declares text_block only under assistant rows;
            // user text is physical-line truth without a logical unit.
            if let Some(text) = string_field(object, "text") {
                emit_text_turn(role, text, timestamp, vec![physical_ref.clone()], analysis);
            } else {
                analysis.unsupported_visible_event = true;
                warn(
                    analysis,
                    WarningKind::UnknownPayloadType,
                    raw.coverage_ordinal,
                );
            }
            Ok(())
        }
        ("assistant", "text") => {
            let evidence = consume_logical(
                raw,
                block,
                block_index,
                "text_block",
                ctx,
                logical,
                analysis,
            )?;
            if let Some(text) = string_field(object, "text") {
                emit_text_turn(
                    role,
                    text,
                    timestamp,
                    vec![physical_ref.clone(), evidence],
                    analysis,
                );
            } else {
                analysis.unsupported_visible_event = true;
                warn(
                    analysis,
                    WarningKind::UnknownPayloadType,
                    raw.coverage_ordinal,
                );
            }
            Ok(())
        }
        ("assistant", "thinking") => {
            let evidence = consume_logical(
                raw,
                block,
                block_index,
                "thinking_block",
                ctx,
                logical,
                analysis,
            )?;
            if let Some(text) = string_field(object, "thinking") {
                if !text.trim().is_empty() {
                    push_turn(
                        TurnRole::Assistant,
                        TurnKind::InternalThought,
                        text.trim(),
                        timestamp,
                        Known::unknown(),
                        vec![physical_ref.clone(), evidence],
                        analysis,
                    );
                }
            } else {
                analysis.unsupported_visible_event = true;
                warn(
                    analysis,
                    WarningKind::UnknownPayloadType,
                    raw.coverage_ordinal,
                );
            }
            Ok(())
        }
        ("assistant", "tool_use") => {
            let evidence = consume_logical(
                raw,
                block,
                block_index,
                "tool_use_block",
                ctx,
                logical,
                analysis,
            )?;
            let name = string_field(object, "name").unwrap_or_default();
            let call_id = string_field(object, "id").unwrap_or_default();
            if !name.is_empty() && !call_id.is_empty() {
                ctx.tool_names.insert(call_id.to_owned(), name.to_owned());
            }
            let turn_idx = push_turn(
                TurnRole::Assistant,
                TurnKind::ToolCall,
                "",
                timestamp,
                known_nonempty(name),
                vec![physical_ref.clone(), evidence.clone()],
                analysis,
            );
            let payload = canonical_json(object.get("input").unwrap_or(&Value::Null));
            analysis.tool_events.push(ToolEvent {
                kind: ToolEventKind::Call,
                turn_idx,
                tool_name: nonempty_or(name, "unknown_tool"),
                correlation_id: known_nonempty(call_id),
                payload_hash: sha256_hex(&payload),
                payload_bytes: payload.len() as u64,
                raw_unit_refs: vec![physical_ref.clone(), evidence],
            });
            Ok(())
        }
        ("user", "tool_result") => {
            let evidence = consume_logical(
                raw,
                block,
                block_index,
                "tool_result_block",
                ctx,
                logical,
                analysis,
            )?;
            let correlation = string_field(object, "tool_use_id").unwrap_or_default();
            let tool_name = ctx.tool_names.get(correlation).cloned();
            let text = tool_result_text(object.get("content"));
            let turn_idx = push_turn(
                TurnRole::Tool,
                TurnKind::ToolResult,
                &text,
                timestamp,
                known_nonempty(tool_name.as_deref().unwrap_or_default()),
                vec![physical_ref.clone(), evidence.clone()],
                analysis,
            );
            let payload = canonical_json(object.get("content").unwrap_or(&Value::Null));
            analysis.tool_events.push(ToolEvent {
                kind: ToolEventKind::Result,
                turn_idx,
                tool_name: nonempty_or(tool_name.as_deref().unwrap_or_default(), "unknown_tool"),
                correlation_id: known_nonempty(correlation),
                payload_hash: sha256_hex(&payload),
                payload_bytes: payload.len() as u64,
                raw_unit_refs: vec![physical_ref.clone(), evidence],
            });
            Ok(())
        }
        (_, "redacted_thinking") => {
            // Known opaque reasoning: never parsed, never a loss by itself.
            analysis.opaque_reasoning_present = true;
            warn(analysis, WarningKind::OpaqueReasoning, raw.coverage_ordinal);
            skip_logical(
                raw,
                block,
                block_index,
                "redacted_thinking",
                SkippedReason::EncryptedOpaque,
                false,
                ctx,
                logical,
                analysis,
            )
        }
        _ => {
            // Unknown or misplaced block shape: preserved as data, flagged,
            // never silently dropped (donor spec §7 discipline).
            analysis.unsupported_visible_event = true;
            warn(
                analysis,
                WarningKind::UnknownPayloadType,
                raw.coverage_ordinal,
            );
            skip_logical(
                raw,
                block,
                block_index,
                "unknown",
                SkippedReason::UnknownPayloadType,
                true,
                ctx,
                logical,
                analysis,
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Turn / segment / skill assembly
// ---------------------------------------------------------------------------

fn emit_text_turn(
    role: &str,
    text: &str,
    timestamp: &Known<String>,
    refs: Vec<RawUnitRef>,
    analysis: &mut Analysis,
) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let (turn_role, kind) = match role {
        "user" => (TurnRole::User, TurnKind::UserMsg),
        "system" => (TurnRole::System, TurnKind::SystemNote),
        _ => (TurnRole::Assistant, TurnKind::AgentReply),
    };
    let turn_idx = push_turn(
        turn_role,
        kind,
        trimmed,
        timestamp,
        Known::unknown(),
        refs,
        analysis,
    );
    if turn_role == TurnRole::User
        && let Some(skill_name) = detect_skill_marker(trimmed)
    {
        analysis.skill_invocations.push(SkillInvocation {
            turn_idx,
            skill_name,
            payload_hash: sha256_hex(trimmed.as_bytes()),
            payload_bytes: trimmed.len() as u64,
            first_invoked_at: timestamp.clone(),
        });
    }
}

fn push_turn(
    role: TurnRole,
    kind: TurnKind,
    text: &str,
    timestamp: &Known<String>,
    tool_name: Known<String>,
    refs: Vec<RawUnitRef>,
    analysis: &mut Analysis,
) -> u64 {
    let turn_idx = analysis.turns.len() as u64;
    let segment_id = analysis.segments.len() as u32 - 1;
    analysis.turns.push(Turn {
        turn_idx,
        role,
        timestamp: timestamp.clone(),
        kind,
        text: text.to_owned(),
        text_hash: sha256_hex(text.as_bytes()),
        text_chars: text.chars().count() as u64,
        tool_name,
        segment_id,
        raw_unit_refs: refs,
    });
    let segment = analysis
        .segments
        .last_mut()
        .expect("analysis always holds one segment draft");
    if segment.first_turn.is_none() {
        segment.first_turn = Some(turn_idx);
        segment.started_at = timestamp.clone();
    }
    segment.last_turn = turn_idx;
    if let Known::Value(_) = timestamp {
        segment.ended_at = timestamp.clone();
    }
    turn_idx
}

fn maybe_split_segment(object: &serde_json::Map<String, Value>, analysis: &mut Analysis) {
    let cwd = string_field(object, "cwd");
    let branch = string_field(object, "gitBranch");
    let current = analysis
        .segments
        .last_mut()
        .expect("analysis always holds one segment draft");
    if let (Some(new_cwd), Known::Value(existing)) = (cwd, current.cwd.as_ref())
        && current.first_turn.is_some()
        && new_cwd != existing.as_str()
    {
        analysis.segments.push(SegmentDraft {
            cwd: Known::value(new_cwd.to_owned()),
            branch: branch.map_or_else(Known::unknown, |value| Known::value(value.to_owned())),
            first_turn: None,
            last_turn: 0,
            started_at: Known::unknown(),
            ended_at: Known::unknown(),
        });
        return;
    }
    if current.cwd == Known::unknown()
        && let Some(cwd) = cwd
    {
        current.cwd = Known::value(cwd.to_owned());
    }
    if current.branch == Known::unknown()
        && let Some(branch) = branch
    {
        current.branch = Known::value(branch.to_owned());
    }
}

fn backfill_segment(object: &serde_json::Map<String, Value>, analysis: &mut Analysis) {
    // Metadata rows may backfill an unset cwd/branch but never open a segment.
    let current = analysis
        .segments
        .last_mut()
        .expect("analysis always holds one segment draft");
    if current.cwd == Known::unknown()
        && let Some(cwd) = string_field(object, "cwd")
    {
        current.cwd = Known::value(cwd.to_owned());
    }
    if current.branch == Known::unknown()
        && let Some(branch) = string_field(object, "gitBranch")
    {
        current.branch = Known::value(branch.to_owned());
    }
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

fn emit_usage_event(
    row: &serde_json::Map<String, Value>,
    message: &serde_json::Map<String, Value>,
    timestamp: &Known<String>,
    evidence: &RawUnitRef,
    analysis: &mut Analysis,
) {
    let usage = message.get("usage").and_then(Value::as_object);
    let cost = row
        .get("costUSD")
        .and_then(Value::as_f64)
        .filter(|amount| amount.is_finite() && *amount >= 0.0);
    let tokens = TokenComponents {
        input: usage_component(usage, "input_tokens"),
        output: usage_component(usage, "output_tokens"),
        reasoning: Known::unknown(),
        cache_read: usage_component(usage, "cache_read_input_tokens"),
        cache_creation: usage_component(usage, "cache_creation_input_tokens"),
    };
    let has_tokens = [
        &tokens.input,
        &tokens.output,
        &tokens.cache_read,
        &tokens.cache_creation,
    ]
    .iter()
    .any(|component| matches!(component, Known::Value(_)));
    if !has_tokens && cost.is_none() {
        return;
    }
    let model = string_field(message, "model")
        .filter(|value| *value != SYNTHETIC_MODEL)
        .map_or_else(Known::unknown, |value| Known::value(value.to_owned()));
    analysis.usage_events.push(UsageEvent {
        provider: USAGE_PROVIDER.to_owned(),
        model,
        tokens,
        cost: cost.map_or_else(Known::unknown, |amount| {
            Known::value(ReportedCost {
                amount,
                currency: "USD".to_owned(),
            })
        }),
        timestamp: timestamp.clone(),
        span: Known::unknown(),
        counter_semantics: CounterSemantics::Delta,
        evidence: evidence.clone(),
    });
}

fn usage_component(usage: Option<&serde_json::Map<String, Value>>, key: &str) -> Known<u64> {
    usage
        .and_then(|object| object.get(key))
        .and_then(Value::as_u64)
        .map_or_else(Known::unknown, Known::value)
}

// ---------------------------------------------------------------------------
// Classification plumbing
// ---------------------------------------------------------------------------

fn physical_evidence(
    raw: &RawUnit,
    unit_kind: &str,
    ctx: &Ctx<'_>,
) -> Result<RawUnitRef, AdapterError> {
    let locator = ordinal_locator(raw.physical_ordinal);
    let evidence_event_id = evidence_event_id_from_hash(
        ctx.agent,
        ctx.session_id,
        &locator,
        unit_kind,
        &raw.content_hash,
    )
    .map_err(|error| AdapterError::new("classify", error.to_string()))?;
    Ok(RawUnitRef {
        evidence_event_id,
        coverage_ordinal: raw.coverage_ordinal,
        physical_ordinal: raw.physical_ordinal,
        locator,
        unit_kind: unit_kind.to_owned(),
        artifact: raw.artifact_name.clone(),
        content_hash: raw.content_hash.clone(),
        original_bytes: raw.original_bytes,
    })
}

fn logical_evidence(
    raw: &RawUnit,
    block: &Value,
    block_index: usize,
    unit_kind: &str,
    ordinal: u64,
    ctx: &Ctx<'_>,
) -> Result<RawUnitRef, AdapterError> {
    let locator = format!("{:06}:blk:{block_index}", raw.physical_ordinal);
    let payload = canonical_json(block);
    let content_hash = sha256_hex(&payload);
    let evidence_event_id = evidence_event_id_from_hash(
        ctx.agent,
        ctx.session_id,
        &locator,
        unit_kind,
        &content_hash,
    )
    .map_err(|error| AdapterError::new("classify", error.to_string()))?;
    Ok(RawUnitRef {
        evidence_event_id,
        coverage_ordinal: ordinal,
        physical_ordinal: raw.physical_ordinal,
        locator,
        unit_kind: unit_kind.to_owned(),
        artifact: raw.artifact_name.clone(),
        content_hash,
        original_bytes: payload.len() as u64,
    })
}

fn consume_physical(
    raw: &RawUnit,
    kind: &str,
    ctx: &Ctx<'_>,
    analysis: &mut Analysis,
) -> Result<(), AdapterError> {
    let evidence = physical_evidence(raw, kind, ctx)?;
    analysis.classified.push(ClassifiedUnit {
        ordinal: raw.coverage_ordinal,
        level: RawUnitLevel::Physical,
        evidence: evidence.clone(),
        disposition: ClassifiedDisposition::Consumed {
            kind: kind.to_owned(),
        },
    });
    analysis.consumed.push(ConsumedUnit {
        ordinal: raw.coverage_ordinal,
        kind: kind.to_owned(),
        evidence,
    });
    Ok(())
}

fn skip_physical(
    raw: &RawUnit,
    unit_kind: &str,
    reason: SkippedReason,
    visible: bool,
    ctx: &Ctx<'_>,
    analysis: &mut Analysis,
) -> Result<(), AdapterError> {
    let evidence = physical_evidence(raw, unit_kind, ctx)?;
    analysis.classified.push(ClassifiedUnit {
        ordinal: raw.coverage_ordinal,
        level: RawUnitLevel::Physical,
        evidence: evidence.clone(),
        disposition: ClassifiedDisposition::Skipped { reason, visible },
    });
    analysis.skipped.push(SkippedUnit {
        ordinal: raw.coverage_ordinal,
        reason,
        bytes: raw.original_bytes,
        visible,
        evidence,
    });
    Ok(())
}

fn consume_logical(
    raw: &RawUnit,
    block: &Value,
    block_index: usize,
    kind: &str,
    ctx: &mut Ctx<'_>,
    logical: &mut Vec<ClassifiedUnit>,
    analysis: &mut Analysis,
) -> Result<RawUnitRef, AdapterError> {
    let ordinal = ctx.next_logical_ordinal;
    ctx.next_logical_ordinal += 1;
    let evidence = logical_evidence(raw, block, block_index, kind, ordinal, ctx)?;
    logical.push(ClassifiedUnit {
        ordinal,
        level: RawUnitLevel::Logical {
            parent_ordinal: raw.coverage_ordinal,
        },
        evidence: evidence.clone(),
        disposition: ClassifiedDisposition::Consumed {
            kind: kind.to_owned(),
        },
    });
    analysis.consumed.push(ConsumedUnit {
        ordinal,
        kind: kind.to_owned(),
        evidence: evidence.clone(),
    });
    Ok(evidence)
}

#[allow(clippy::too_many_arguments)]
fn skip_logical(
    raw: &RawUnit,
    block: &Value,
    block_index: usize,
    unit_kind: &str,
    reason: SkippedReason,
    visible: bool,
    ctx: &mut Ctx<'_>,
    logical: &mut Vec<ClassifiedUnit>,
    analysis: &mut Analysis,
) -> Result<(), AdapterError> {
    let ordinal = ctx.next_logical_ordinal;
    ctx.next_logical_ordinal += 1;
    let evidence = logical_evidence(raw, block, block_index, unit_kind, ordinal, ctx)?;
    logical.push(ClassifiedUnit {
        ordinal,
        level: RawUnitLevel::Logical {
            parent_ordinal: raw.coverage_ordinal,
        },
        evidence: evidence.clone(),
        disposition: ClassifiedDisposition::Skipped { reason, visible },
    });
    analysis.skipped.push(SkippedUnit {
        ordinal,
        reason,
        bytes: evidence.original_bytes,
        visible,
        evidence,
    });
    Ok(())
}

fn warn(analysis: &mut Analysis, kind: WarningKind, ordinal: u64) {
    match analysis
        .warnings
        .iter_mut()
        .find(|warning| warning.kind == kind)
    {
        Some(warning) => warning.count += 1,
        None => analysis.warnings.push(CoverageWarning {
            kind,
            count: 1,
            first_ordinal: ordinal,
        }),
    }
}

// ---------------------------------------------------------------------------
// Final parse assembly
// ---------------------------------------------------------------------------

impl Analysis {
    fn into_parse(mut self, source: &SourceHandle, read: &SourceRead) -> UnvalidatedParse {
        let fatal = !self.session_id_seen;
        let visible_completeness = if fatal {
            VisibleCompleteness::Fatal
        } else if self.malformed_tail_present || self.visible_event_lost {
            VisibleCompleteness::PartialVisible
        } else {
            VisibleCompleteness::CompleteVisible
        };
        let status = ParseStatus {
            visible_completeness,
            boundary_flags: BoundaryFlags {
                opaque_reasoning_present: self.opaque_reasoning_present,
                unsupported_visible_event: self.unsupported_visible_event,
                compaction_boundary_present: false,
            },
            malformed_tail_present: self.malformed_tail_present,
            visible_event_lost: self.visible_event_lost,
        };
        let mut warnings = self.warnings;
        warnings.sort_by_key(|warning| warning.first_ordinal);
        let coverage = CoverageReport::with_raw_line_count(
            read.units.len() as u64,
            self.classified.len() as u64,
            self.consumed,
            self.skipped,
            warnings,
            status,
        );
        if fatal {
            return UnvalidatedParse::fatal(coverage);
        }

        let session_id = source
            .logical_session_id()
            .unwrap_or_else(|| source.source_id());
        let provenance = Provenance {
            agent: source.agent(),
            model: self.model,
            cli_version: self.cli_version,
            cwd: self.first_cwd,
            branch: self.first_branch,
            started_at: self.started_at,
            ended_at: self.ended_at,
            original_source_hash: read.source_hash.clone(),
            original_source_bytes: read.source_bytes,
        };
        let mut model = SessionModel::new(session_id, provenance, coverage);
        model.segments = finalize_segments(std::mem::take(&mut self.segments), &self.turns);
        model.turns = self.turns;
        model.tool_events = self.tool_events;
        model.usage_events = self.usage_events;
        model.skill_invocations = self.skill_invocations;
        UnvalidatedParse::from_model(model)
    }
}

fn finalize_segments(drafts: Vec<SegmentDraft>, turns: &[Turn]) -> Vec<Segment> {
    if turns.is_empty() {
        return Vec::new();
    }
    // A new draft opens only after the current one holds a turn, so an empty
    // draft can only be the trailing one. Dropping it never shifts survivor
    // indexes, and turn.segment_id (assigned as the draft index) stays valid.
    drafts
        .into_iter()
        .filter(|draft| draft.first_turn.is_some())
        .enumerate()
        .map(|(index, draft)| Segment {
            segment_id: index as u32,
            cwd: draft.cwd,
            branch: draft.branch,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            turn_range: TurnRange {
                start: draft.first_turn.expect("survivor draft has turns"),
                end: draft.last_turn,
            },
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn string_field<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn known_timestamp(raw: Option<&str>) -> Known<String> {
    match raw {
        Some(value) if chrono::DateTime::parse_from_rfc3339(value).is_ok() => {
            Known::value(value.to_owned())
        }
        _ => Known::unknown(),
    }
}

fn known_nonempty(value: &str) -> Known<String> {
    if value.is_empty() {
        Known::unknown()
    } else {
        Known::value(value.to_owned())
    }
}

fn nonempty_or(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

fn canonical_json(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("serde_json::Value serialization cannot fail")
}

fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => {
            let mut parts: Vec<String> = Vec::new();
            let mut non_text_kinds: Vec<String> = Vec::new();
            for block in blocks {
                match block {
                    Value::String(text) => parts.push(text.clone()),
                    Value::Object(object) => match string_field(object, "type") {
                        Some("text") => {
                            if let Some(text) = string_field(object, "text") {
                                parts.push(text.to_owned());
                            }
                        }
                        Some(kind) if !non_text_kinds.iter().any(|seen| seen == kind) => {
                            non_text_kinds.push(kind.to_owned());
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            let mut text = parts.join("\n\n").trim().to_owned();
            if !non_text_kinds.is_empty() {
                // Recognized non-text result blocks stay visible via a
                // descriptive sentinel (donor FIX F2), never silently dropped.
                let note = format!(
                    "[tool_result non-text content: {}]",
                    non_text_kinds.join(", ")
                );
                if text.is_empty() {
                    text = note;
                } else {
                    text.push_str("\n\n");
                    text.push_str(&note);
                }
            }
            text
        }
        _ => String::new(),
    }
}
