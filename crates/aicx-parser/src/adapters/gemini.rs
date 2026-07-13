//! Gemini (whole-file JSON, JSONL streams, Antigravity conversation/step artifacts) adapter
//! for the deterministic parser kernel.
//!
//! Supports shapes from AICX legacy Antigravity and Gemini session exports without
//! filesystem discovery. Whole-file roots and nested logical units (messages, parts,
//! thoughts, tool calls) are modeled explicitly per C0A taxonomy and C4 contract.
//! Raw accounting stays honest: physical units reflect framing (1 for WholeDocument,
//! 1-per-line for JsonLines); logical ordinals are post-physical with parent links.
//!
//! No shared dispatch touched. Subformat detection by shape only (no guessing paths).

use super::{AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel};
use crate::engine::{
    AgentKind, BoundaryFlags, ConsumedUnit, CounterSemantics, CoverageReport, CoverageWarning,
    Known, ParseStatus, Provenance, RawUnit, RawUnitRef, Segment, SessionModel, SkippedReason,
    SkippedUnit, SourceFraming, SourceHandle, SourceRead, TokenComponents, ToolEvent,
    ToolEventKind, Turn, TurnKind, TurnRange, TurnRole, UnitBoundary, UnvalidatedParse, UsageEvent,
    VisibleCompleteness, WarningKind, evidence_event_id_from_hash, ordinal_locator, sha256_hex,
};
use serde_json::Value;

pub const GEMINI_ADAPTER_VERSION: &str = "gemini-adapter-v1";

/// Provider for usage events.
const USAGE_PROVIDER: &str = "google";

#[derive(Debug, Clone, Copy, Default)]
pub struct GeminiAdapter;

impl super::sealed::Sealed for GeminiAdapter {}

impl AgentAdapter for GeminiAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Gemini
    }

    fn adapter_version(&self) -> &'static str {
        GEMINI_ADAPTER_VERSION
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
// Analysis state (parallel to other adapters)
// ---------------------------------------------------------------------------

struct Analysis {
    classified: Vec<ClassifiedUnit>,
    consumed: Vec<ConsumedUnit>,
    skipped: Vec<SkippedUnit>,
    warnings: Vec<CoverageWarning>,
    turns: Vec<Turn>,
    tool_events: Vec<ToolEvent>,
    usage_events: Vec<UsageEvent>,
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
}

fn analyze(source: &SourceHandle, read: &SourceRead) -> Result<Analysis, AdapterError> {
    if source.artifacts().len() != 1 {
        return Err(AdapterError::new(
            "classify",
            "gemini sources are single-artifact (whole JSON or JSONL or antigravity); grouping is locator-owned",
        ));
    }
    let artifact = &source.artifacts()[0];
    let framing = artifact.framing();
    if framing != SourceFraming::WholeDocument && framing != SourceFraming::JsonLines {
        return Err(AdapterError::new(
            "classify",
            "gemini adapter supports whole_document and json_lines only",
        ));
    }

    let session_id = source
        .logical_session_id()
        .unwrap_or_else(|| source.source_id());
    let mut ctx = Ctx {
        agent: source.agent(),
        session_id,
        next_logical_ordinal: read.units.len() as u64 + 1,
    };
    let mut analysis = Analysis {
        classified: Vec::new(),
        consumed: Vec::new(),
        skipped: Vec::new(),
        warnings: Vec::new(),
        turns: Vec::new(),
        tool_events: Vec::new(),
        usage_events: Vec::new(),
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
    let parsed: Option<Value> = text.and_then(|s| serde_json::from_str(s).ok());
    let is_blank = text.is_some_and(|s| s.trim().is_empty());
    let unterminated = raw.boundary == UnitBoundary::UnterminatedTail;

    let Some(value) = parsed else {
        if is_blank {
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
        if unterminated {
            analysis.malformed_tail_present = true;
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

    // Shape detection for Gemini / Antigravity (no fs)
    let shape = detect_shape(&value, raw.framing);

    match shape {
        GeminiShape::WholeFileDocument => {
            consume_physical(raw, "whole_file_document", ctx, analysis)?;
            // Extract top level fields for provenance
            if let Some(obj) = value.as_object() {
                if let Some(_sid) = string_field(obj, "sessionId") {
                    analysis.session_id_seen = true;
                    if analysis.started_at == Known::unknown()
                        && let Some(st) = string_field(obj, "startTime")
                    {
                        analysis.started_at = Known::value(st.to_owned());
                    }
                    if analysis.ended_at == Known::unknown()
                        && let Some(lt) = string_field(obj, "lastUpdated")
                    {
                        analysis.ended_at = Known::value(lt.to_owned());
                    }
                }
                // model inference from first gemini message if present
                if let Some(msgs) = obj.get("messages").and_then(Value::as_array) {
                    for m in msgs {
                        if let Some(mo) = m.as_object() {
                            if let Some(mdl) = string_field(mo, "model")
                                && analysis.model == Known::unknown()
                            {
                                analysis.model = Known::value(mdl.to_owned());
                            }
                            // Also check inside content if object
                            if let Some(c) = mo.get("content")
                                && let Some(co) = c.as_object()
                                && let Some(mdl) = string_field(co, "model")
                                && analysis.model == Known::unknown()
                            {
                                analysis.model = Known::value(mdl.to_owned());
                            }
                        }
                    }
                }
                // cwd / project from projectRoot (Antigravity style)
                if let Some(pr) = string_field(obj, "projectRoot")
                    && analysis.first_cwd == Known::unknown()
                {
                    analysis.first_cwd = Known::value(pr.to_owned());
                }
            }
            // Emit logical units from messages array
            if let Some(msgs) = value.get("messages").and_then(Value::as_array) {
                for (i, msg) in msgs.iter().enumerate() {
                    emit_gemini_message(raw, msg, i, ctx, analysis, logical)?;
                }
            }
        }
        GeminiShape::AntigravityConversation => {
            consume_physical(raw, "whole_file_document", ctx, analysis)?;
            analysis.session_id_seen = true;
            if let Some(obj) = value.as_object() {
                if let Some(pr) = string_field(obj, "projectRoot")
                    && analysis.first_cwd == Known::unknown()
                {
                    analysis.first_cwd = Known::value(pr.to_owned());
                }
                if let Some(msgs) = obj.get("messages").and_then(Value::as_array) {
                    for (i, msg) in msgs.iter().enumerate() {
                        emit_antigravity_message(raw, msg, i, ctx, analysis, logical)?;
                    }
                }
            }
        }
        GeminiShape::StreamHeader | GeminiShape::StateUpdate => {
            let kind = if matches!(shape, GeminiShape::StreamHeader) {
                "stream_header"
            } else {
                "state_update"
            };
            consume_physical(raw, kind, ctx, analysis)?;
            // minimal metadata, may backfill
            if let Some(obj) = value.as_object() {
                if let Some(_sid) = string_field(obj, "sessionId") {
                    analysis.session_id_seen = true;
                }
                if let Some(lt) = string_field(obj, "lastUpdated")
                    && analysis.ended_at == Known::unknown()
                {
                    analysis.ended_at = Known::value(lt.to_owned());
                }
            }
        }
        GeminiShape::JsonlLineMessage => {
            // treat each line as physical message container
            consume_physical(raw, "message", ctx, analysis)?;
            emit_gemini_message(raw, &value, 0, ctx, analysis, logical)?;
        }
        GeminiShape::Unknown => {
            if unterminated {
                analysis.malformed_tail_present = true;
            }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeminiShape {
    WholeFileDocument,
    AntigravityConversation,
    StreamHeader,
    StateUpdate,
    JsonlLineMessage,
    Unknown,
}

fn detect_shape(value: &Value, framing: SourceFraming) -> GeminiShape {
    let obj = match value.as_object() {
        Some(o) => o,
        None => return GeminiShape::Unknown,
    };
    if obj.contains_key("messages") {
        if obj.contains_key("projectRoot") || obj.contains_key("artifact") {
            return GeminiShape::AntigravityConversation;
        }
        if framing == SourceFraming::WholeDocument
            || obj.contains_key("sessionId")
            || obj.contains_key("startTime")
        {
            return GeminiShape::WholeFileDocument;
        }
        return GeminiShape::WholeFileDocument;
    }
    if obj.contains_key("$set") {
        return GeminiShape::StateUpdate;
    }
    if obj.contains_key("sessionId") && obj.contains_key("startTime") {
        return GeminiShape::StreamHeader;
    }
    if framing == SourceFraming::JsonLines {
        // heuristic for incremental
        if obj
            .get("type")
            .is_some_and(|t| t.as_str() == Some("gemini") || t.as_str() == Some("user"))
            || obj.contains_key("role")
        {
            return GeminiShape::JsonlLineMessage;
        }
    }
    GeminiShape::Unknown
}

fn emit_gemini_message(
    raw: &RawUnit,
    msg: &Value,
    block_index: usize,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    let obj = match msg.as_object() {
        Some(o) => o,
        None => return Ok(()),
    };
    let role_str = string_field(obj, "type")
        .or_else(|| string_field(obj, "role"))
        .unwrap_or("unknown");
    let is_user = role_str == "user";
    let is_assistant = matches!(role_str, "gemini" | "model" | "assistant");
    let kind = if is_user { "user" } else { "message" };

    let evidence = consume_logical(raw, msg, block_index, kind, ctx, logical, analysis)?;

    // text content
    let text = extract_text(obj);
    let timestamp = string_field(obj, "timestamp")
        .or_else(|| string_field(obj, "lastUpdated"))
        .map(|s| Known::value(s.to_owned()))
        .unwrap_or_else(Known::unknown);

    // model from this msg
    if is_assistant
        && let Some(mdl) = string_field(obj, "model")
        && analysis.model == Known::unknown()
    {
        analysis.model = Known::value(mdl.to_owned());
    }

    // usage if present
    if let Some(tokens_obj) = obj
        .get("tokens")
        .and_then(Value::as_object)
        .or_else(|| obj.get("usage").and_then(Value::as_object))
    {
        let ue = UsageEvent {
            provider: USAGE_PROVIDER.to_owned(),
            model: analysis.model.clone(),
            tokens: TokenComponents {
                input: usage_component(Some(tokens_obj), "input"),
                output: usage_component(Some(tokens_obj), "output"),
                reasoning: usage_component(Some(tokens_obj), "reasoning"),
                cache_read: Known::unknown(),
                cache_creation: Known::unknown(),
            },
            cost: Known::unknown(),
            timestamp: timestamp.clone(),
            span: Known::unknown(),
            counter_semantics: CounterSemantics::Snapshot,
            evidence: evidence.clone(),
        };
        analysis.usage_events.push(ue);
    }

    if !text.is_empty() || is_assistant {
        let turn_role = if is_user {
            TurnRole::User
        } else {
            TurnRole::Assistant
        };
        let turn_kind = if is_assistant && has_thought(obj) {
            TurnKind::InternalThought
        } else if is_assistant {
            TurnKind::AgentReply
        } else {
            TurnKind::UserMsg
        };
        push_turn(
            turn_role, turn_kind, &text, None, timestamp, evidence, analysis,
        );
    }

    // parts for richer shapes (antigravity style or gemini content.parts)
    if let Some(parts) = get_parts(obj) {
        for (pi, part) in parts.iter().enumerate() {
            emit_part(raw, part, block_index, pi, ctx, analysis, logical)?;
        }
    }

    // direct tool in message for some shapes
    if let Some(fc) = obj.get("functionCall").or_else(|| obj.get("tool_call")) {
        if let Some(name) = string_field(obj, "name")
            .or_else(|| fc.as_object().and_then(|f| string_field(f, "name")))
        {
            emit_tool_call(raw, name, fc, block_index, ctx, analysis, logical)?;
        }
    } else if let (Some(name), Some(_args)) = (string_field(obj, "name"), obj.get("args")) {
        let v = Value::Object(obj.clone());
        emit_tool_call(raw, name, &v, block_index, ctx, analysis, logical)?;
    }
    Ok(())
}

fn emit_antigravity_message(
    raw: &RawUnit,
    msg: &Value,
    block_index: usize,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    let obj = match msg.as_object() {
        Some(o) => o,
        None => return Ok(()),
    };
    let role = string_field(obj, "role").unwrap_or("unknown");
    let is_user = role == "user";
    let is_model = role == "model" || role == "assistant";

    let kind = if is_user { "user" } else { "message" };
    let evidence = consume_logical(raw, msg, block_index, kind, ctx, logical, analysis)?;

    let text = extract_text_from_parts(obj);
    let timestamp = string_field(obj, "timestamp")
        .map(|s| Known::value(s.to_owned()))
        .unwrap_or_else(Known::unknown);

    if !text.is_empty() || is_model {
        let turn_role = if is_user {
            TurnRole::User
        } else {
            TurnRole::Assistant
        };
        let mut turn_kind = if is_model {
            TurnKind::AgentReply
        } else {
            TurnKind::UserMsg
        };
        if is_model && has_thought_in_parts(obj) {
            turn_kind = TurnKind::InternalThought;
            analysis.opaque_reasoning_present = true; // or thought present
        }
        push_turn(
            turn_role,
            turn_kind,
            &text,
            None,
            timestamp.clone(),
            evidence.clone(),
            analysis,
        );
    }

    // parts
    if let Some(parts) = get_parts(obj) {
        for (pi, part) in parts.iter().enumerate() {
            emit_part(raw, part, block_index, pi, ctx, analysis, logical)?;
        }
    }
    Ok(())
}

fn emit_part(
    raw: &RawUnit,
    part: &Value,
    parent_block: usize,
    part_idx: usize,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    let obj = match part.as_object() {
        Some(o) => o,
        None => return Ok(()),
    };
    let is_thought = obj.get("thought").and_then(Value::as_bool).unwrap_or(false)
        || obj.contains_key("thinking")
        || string_field(obj, "subject").is_some(); // some thought shapes
    let text = extract_text(obj);
    if text.is_empty() && !is_thought {
        // may be functionCall inside part
        if let Some(fc) = obj.get("functionCall")
            && let Some(name) = string_field(obj, "name")
                .or_else(|| fc.as_object().and_then(|f| string_field(f, "name")))
        {
            return emit_tool_call(raw, name, fc, parent_block, ctx, analysis, logical);
        }
        return Ok(());
    }
    let kind = if is_thought { "thought" } else { "text_block" };
    let block_val = part.clone();
    let evidence = consume_logical(
        raw,
        &block_val,
        parent_block * 100 + part_idx,
        kind,
        ctx,
        logical,
        analysis,
    )?;

    let turn_kind = if is_thought {
        TurnKind::InternalThought
    } else {
        TurnKind::AgentReply
    };
    let role = TurnRole::Assistant;
    let ts = Known::unknown();
    push_turn(role, turn_kind, &text, None, ts, evidence, analysis);
    Ok(())
}

fn emit_tool_call(
    raw: &RawUnit,
    name: &str,
    call_val: &Value,
    block_index: usize,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    let block_val = call_val.clone();
    let evidence = consume_logical(
        raw,
        &block_val,
        block_index,
        "tool_call",
        ctx,
        logical,
        analysis,
    )?;

    let args = call_val
        .get("args")
        .or_else(|| call_val.get("arguments"))
        .cloned()
        .unwrap_or(Value::Null);
    let _payload = canonical_json(&args);
    let tool_event = ToolEvent {
        kind: ToolEventKind::Call,
        turn_idx: analysis.turns.len() as u64,
        tool_name: name.to_owned(),
        correlation_id: Known::unknown(),
        payload_hash: sha256_hex(&canonical_bytes(&args)),
        payload_bytes: canonical_bytes(&args).len() as u64,
        raw_unit_refs: vec![evidence.clone()],
    };
    analysis.tool_events.push(tool_event);

    // also a turn for it?
    push_turn(
        TurnRole::Tool,
        TurnKind::ToolCall,
        name,
        Some(name.to_owned()),
        Known::unknown(),
        evidence,
        analysis,
    );
    Ok(())
}

fn has_thought(obj: &serde_json::Map<String, Value>) -> bool {
    if let Some(parts) = get_parts_from_map(obj) {
        return parts
            .iter()
            .any(|p| p.get("thought").and_then(|v| v.as_bool()).unwrap_or(false));
    }
    false
}

fn has_thought_in_parts(obj: &serde_json::Map<String, Value>) -> bool {
    if let Some(parts) = get_parts_from_map(obj) {
        parts.iter().any(|p| {
            if let Some(po) = p.as_object() {
                po.get("thought").and_then(Value::as_bool).unwrap_or(false)
                    || po.contains_key("thinking")
            } else {
                false
            }
        })
    } else {
        false
    }
}

fn get_parts(obj: &serde_json::Map<String, Value>) -> Option<&Vec<Value>> {
    obj.get("content")
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .or_else(|| obj.get("parts").and_then(Value::as_array))
}

fn get_parts_from_map(obj: &serde_json::Map<String, Value>) -> Option<&Vec<Value>> {
    get_parts(obj)
}

fn extract_text(obj: &serde_json::Map<String, Value>) -> String {
    if let Some(s) = string_field(obj, "content") {
        return s.to_owned();
    }
    if let Some(c) = obj.get("content") {
        if let Some(s) = c.as_str() {
            return s.to_owned();
        }
        if let Some(parts) = c.get("parts").and_then(Value::as_array) {
            return parts
                .iter()
                .filter_map(|p| {
                    if let Some(po) = p.as_object() {
                        if po.get("thought").and_then(Value::as_bool).unwrap_or(false) {
                            return None;
                        }
                        string_field(po, "text").or_else(|| string_field(po, "content"))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
    }
    if let Some(t) = string_field(obj, "text") {
        return t.to_owned();
    }
    String::new()
}

fn extract_text_from_parts(obj: &serde_json::Map<String, Value>) -> String {
    let mut out = Vec::new();
    if let Some(parts) = get_parts_from_map(obj) {
        for p in parts {
            if let Some(po) = p.as_object() {
                if po.get("thought").and_then(Value::as_bool).unwrap_or(false) {
                    continue;
                }
                if let Some(t) = string_field(po, "text") {
                    out.push(t.to_owned());
                }
            }
        }
    } else if let Some(t) = string_field(obj, "content") {
        out.push(t.to_owned());
    }
    out.join("\n")
}

fn push_turn(
    role: TurnRole,
    kind: TurnKind,
    text: &str,
    tool_name: Option<String>,
    timestamp: Known<String>,
    evidence: RawUnitRef,
    analysis: &mut Analysis,
) {
    let turn_idx = analysis.turns.len() as u64;
    let refs = vec![evidence.clone()];
    analysis.turns.push(Turn {
        turn_idx,
        role,
        timestamp: timestamp.clone(),
        kind,
        text: text.to_owned(),
        text_hash: sha256_hex(text.as_bytes()),
        text_chars: text.chars().count() as u64,
        tool_name: tool_name.map_or_else(Known::unknown, Known::value),
        segment_id: 0, // finalized later
        raw_unit_refs: refs,
    });
    let segment = analysis.segments.last_mut().expect("segment draft");
    if segment.first_turn.is_none() {
        segment.first_turn = Some(turn_idx);
        segment.started_at = timestamp.clone();
    }
    segment.last_turn = turn_idx;
    if let Known::Value(_) = timestamp {
        segment.ended_at = timestamp;
    }
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
    .map_err(|e| AdapterError::new("classify", e.to_string()))?;
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
    .map_err(|e| AdapterError::new("classify", e.to_string()))?;
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

fn warn(analysis: &mut Analysis, kind: WarningKind, ordinal: u64) {
    if let Some(w) = analysis.warnings.iter_mut().find(|w| w.kind == kind) {
        w.count += 1;
    } else {
        analysis.warnings.push(CoverageWarning {
            kind,
            count: 1,
            first_ordinal: ordinal,
        });
    }
}

fn string_field<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(Value::as_str)
}

fn usage_component(usage: Option<&serde_json::Map<String, Value>>, key: &str) -> Known<u64> {
    usage
        .and_then(|u| u.get(key))
        .and_then(Value::as_u64)
        .map_or_else(Known::unknown, Known::value)
}

fn canonical_json(v: &Value) -> Vec<u8> {
    // stable-ish
    serde_json::to_vec(v).unwrap_or_default()
}

fn canonical_bytes(v: &Value) -> Vec<u8> {
    canonical_json(v)
}

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
            },
            malformed_tail_present: self.malformed_tail_present,
            visible_event_lost: self.visible_event_lost,
        };
        let mut warnings = self.warnings;
        warnings.sort_by_key(|w| w.first_ordinal);
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
        // skill_invocations left empty for gemini (no direct analog in basic)
        UnvalidatedParse::from_model(model)
    }
}

fn finalize_segments(drafts: Vec<SegmentDraft>, turns: &[Turn]) -> Vec<Segment> {
    if turns.is_empty() {
        return Vec::new();
    }
    drafts
        .into_iter()
        .filter(|d| d.first_turn.is_some())
        .enumerate()
        .map(|(i, d)| Segment {
            segment_id: i as u32,
            cwd: d.cwd,
            branch: d.branch,
            started_at: d.started_at,
            ended_at: d.ended_at,
            turn_range: TurnRange {
                start: d.first_turn.unwrap_or(0),
                end: d.last_turn,
            },
        })
        .collect()
}

// small helpers to satisfy when compiled under shadow
#[allow(unused)]
fn has_counter_semantics() -> bool {
    true
}
