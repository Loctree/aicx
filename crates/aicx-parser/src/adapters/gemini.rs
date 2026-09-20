//! Gemini (whole-file JSON, JSONL streams, Antigravity conversation/step artifacts) adapter
//! for the deterministic parser kernel.
//!
//! Supports shapes from AICX legacy Antigravity and Gemini session exports without
//! filesystem discovery. Whole-file roots and nested logical units (messages, parts,
//! thoughts, tool calls) are modeled explicitly per C0A taxonomy and C4 contract.
//! Raw accounting stays honest: physical units reflect framing (1 for WholeDocument,
//! 1-per-line for JsonLines); logical ordinals are post-physical with parent links.
//!
//! Logical units are bounded too. A whole-file session is one physical unit
//! whatever its size, so the only place a 300 MB tool result can be refused
//! without taking the conversation with it is here: an oversized nested block
//! terminates as `skipped(oversized)` with its own evidence, and the message
//! that carried it is consumed without it (see [`reduce_oversized_message`]).
//!
//! No shared dispatch touched. Subformat detection by shape only (no guessing paths).

use super::{AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel};
use crate::engine::frames::{
    self, ClassifiedFrame, TransportFrame, TransportKind, TransportPayload, TransportRole,
};
use crate::engine::{
    AgentKind, BoundaryFlags, ConsumedUnit, CounterSemantics, CoverageReport, CoverageWarning,
    DEFAULT_MAX_UNIT_BYTES, Known, ParseStatus, Provenance, RawUnit, RawUnitRef, Segment,
    SessionModel, Sha256Stream, SkippedReason, SkippedUnit, SourceFraming, SourceHandle,
    SourceRead, TokenComponents, ToolEvent, ToolEventKind, Turn, TurnRange, UnitBoundary,
    UnvalidatedParse, UsageEvent, VisibleCompleteness, WarningKind, evidence_event_id_from_hash,
    ordinal_locator, sha256_hex,
};
use serde_json::Value;
use std::borrow::Cow;

pub const GEMINI_ADAPTER_VERSION: &str = "gemini-adapter-v2-throne";

/// Provider for usage events.
const USAGE_PROVIDER: &str = "google";

/// Bound on one logical unit this adapter will consume, in canonical JSON
/// bytes. The same number as the reader's per-line cap: a nested block is a
/// unit like any other and gets the same bound. Gemini stores a whole
/// conversation in one JSON document, so this is what decides how much of a
/// session one oversized tool result takes down with it — exactly itself.
const MAX_LOGICAL_UNIT_BYTES: u64 = DEFAULT_MAX_UNIT_BYTES as u64;

/// Key of the marker left where an oversized nested block was refused. It
/// keeps sibling indices — and so every survivor's locator — stable, and lets
/// the reduced parent carry a reference to the evidence of what was dropped.
const OVERSIZED_MARKER_KEY: &str = "aicx_oversized_block";

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
        GeminiShape::WholeFileDocument | GeminiShape::AntigravityConversation => {
            consume_physical(raw, "whole_file_document", ctx, analysis)?;
            // Extract top level fields for provenance
            if let Some(obj) = value.as_object() {
                if let Some(_sid) = string_field(obj, "sessionId") {
                    analysis.session_id_seen = true;
                }
                // An Antigravity conversation export carries no `sessionId`:
                // its identity is the artifact itself (`projectRoot` +
                // `messages`), so the document is the session, not a fatal
                // headless stream.
                if matches!(shape, GeminiShape::AntigravityConversation) {
                    analysis.session_id_seen = true;
                }
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
                // model inference from first gemini message if present
                if let Some(msgs) = obj.get("messages").and_then(Value::as_array) {
                    for m in msgs {
                        if let Some(mo) = m.as_object() {
                            if let Some(mdl) = string_field(mo, "model")
                                && analysis.model == Known::unknown()
                            {
                                analysis.model = Known::value(mdl.to_owned());
                            }
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
                    emit_message(raw, msg, i, ctx, analysis, logical)?;
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
            consume_physical(raw, "message", ctx, analysis)?;
            emit_message(raw, &value, 0, ctx, analysis, logical)?;
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
            analysis.unsupported_visible_event = true;
            analysis.visible_event_lost = true;
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
    if framing == SourceFraming::JsonLines
        && (obj
            .get("type")
            .is_some_and(|t| t.as_str() == Some("gemini") || t.as_str() == Some("user"))
            || obj.contains_key("role"))
    {
        return GeminiShape::JsonlLineMessage;
    }
    GeminiShape::Unknown
}

fn emit_message(
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

    let transport_role = match role_str {
        "user" => TransportRole::User,
        "gemini" | "model" | "assistant" => TransportRole::Assistant,
        "system" | "system_instruction" | "systemInstruction" | "context" => TransportRole::System,
        "tool" | "function" | "tool_response" => TransportRole::Tool,
        _ => TransportRole::Assistant,
    };

    let kind = match transport_role {
        TransportRole::User => "user",
        TransportRole::System => "system",
        _ => "message",
    };

    // Bound the unit before consuming it. A message over the cap is not
    // refused outright: on the tree that motivated this, a 298 MB message was
    // 228 bytes of speech plus one tool result, and the speech is the point.
    let measured = measure(msg);
    let (msg, measured): (Cow<'_, Value>, Measured) = if measured.bytes > MAX_LOGICAL_UNIT_BYTES {
        match reduce_oversized_message(raw, obj, block_index, kind, ctx, analysis, logical)? {
            Some((reduced, measured)) => (Cow::Owned(reduced), measured),
            None => return Ok(()),
        }
    } else {
        (Cow::Borrowed(msg), measured)
    };
    let Some(obj) = msg.as_object() else {
        return Ok(());
    };

    let evidence =
        consume_logical_measured(raw, measured, block_index, kind, ctx, logical, analysis)?;

    let text = extract_text(obj);
    let timestamp = string_field(obj, "timestamp")
        .or_else(|| string_field(obj, "lastUpdated"))
        .map(|s| Known::value(s.to_owned()))
        .unwrap_or_else(Known::unknown);

    if transport_role == TransportRole::Assistant
        && let Some(mdl) = string_field(obj, "model")
        && analysis.model == Known::unknown()
    {
        analysis.model = Known::value(mdl.to_owned());
    }

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

    if has_thought(obj) {
        analysis.opaque_reasoning_present = true;
    }

    // Delivery of normalized message text to the throne classifier
    if !text.is_empty() || transport_role == TransportRole::Assistant {
        let (transport_kind, payload) = match transport_role {
            TransportRole::User => (
                TransportKind::DirectMessage,
                TransportPayload::Text {
                    role: TransportRole::User,
                    content: text,
                },
            ),
            TransportRole::Assistant => (
                TransportKind::AssistantMessage,
                TransportPayload::Text {
                    role: TransportRole::Assistant,
                    content: text,
                },
            ),
            TransportRole::System => (
                TransportKind::InjectedContext,
                TransportPayload::Inject {
                    tag: "system".to_owned(),
                    content: text,
                },
            ),
            TransportRole::Tool => (
                TransportKind::DirectMessage,
                TransportPayload::Text {
                    role: TransportRole::Tool,
                    content: text,
                },
            ),
        };
        let frame = TransportFrame {
            agent: AgentKind::Gemini,
            transport_kind,
            timestamp: timestamp.clone(),
            payload,
            evidence: evidence.clone(),
        };
        let classified = frames::classify(&frame);
        push_classified_frame(&classified, None, analysis);
    }

    // Tool calls inside message
    if let Some(tool_calls) = obj.get("toolCalls").and_then(Value::as_array) {
        for (ti, tc) in tool_calls.iter().enumerate() {
            if is_oversized_marker(tc) {
                // Already terminated as skipped(oversized) during reduction.
                continue;
            }
            emit_tool_call(
                raw,
                tc,
                block_index * 1000 + ti + 1,
                timestamp.clone(),
                ctx,
                analysis,
                logical,
            )?;
        }
    } else if let Some(fc) = obj.get("functionCall") {
        emit_tool_call(
            raw,
            fc,
            block_index * 1000 + 1,
            timestamp.clone(),
            ctx,
            analysis,
            logical,
        )?;
    } else if let Some(parts) = get_parts(obj) {
        for (pi, part) in parts.iter().enumerate() {
            if let Some(fc) = part.get("functionCall") {
                emit_tool_call(
                    raw,
                    fc,
                    block_index * 1000 + pi + 1,
                    timestamp.clone(),
                    ctx,
                    analysis,
                    logical,
                )?;
            }
        }
    }

    Ok(())
}

fn emit_tool_call(
    raw: &RawUnit,
    call_val: &Value,
    block_index: usize,
    timestamp: Known<String>,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    let call_obj = call_val.as_object();
    let name = call_obj
        .and_then(|o| string_field(o, "name"))
        .or_else(|| {
            call_obj
                .and_then(|o| o.get("functionCall"))
                .and_then(Value::as_object)
                .and_then(|f| string_field(f, "name"))
        })
        .unwrap_or("tool");

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

    let is_shell = matches!(name, "run_shell_command" | "bash" | "shell");
    let (transport_kind, payload) = if is_shell {
        let command = string_field_from_val(&args, "command")
            .or_else(|| string_field_from_val(&args, "cmd"))
            .unwrap_or(name)
            .to_owned();
        let result = extract_tool_result_string(call_val);
        // The agent invoked the shell tool; the operator did not type it.
        (
            TransportKind::AgentToolCall,
            TransportPayload::Shell { command, result },
        )
    } else {
        (
            TransportKind::DirectMessage,
            TransportPayload::Text {
                role: TransportRole::Tool,
                content: name.to_owned(),
            },
        )
    };

    let frame = TransportFrame {
        agent: AgentKind::Gemini,
        transport_kind,
        timestamp,
        payload,
        evidence,
    };
    let classified = frames::classify(&frame);
    push_classified_frame(&classified, Some(name.to_owned()), analysis);
    Ok(())
}

fn push_classified_frame(
    classified: &ClassifiedFrame,
    tool_name: Option<String>,
    analysis: &mut Analysis,
) {
    let Some(turn_kind) = classified.turn_kind else {
        return;
    };
    // Role and lane are the throne's (W2-R1); the class rides on the turn.
    let role = classified.class.turn_role();
    let turn_idx = analysis.turns.len() as u64;
    let refs = vec![classified.origin.evidence.clone()];
    analysis.turns.push(Turn {
        turn_idx,
        role,
        timestamp: classified.seal.seal_ts.clone(),
        kind: turn_kind,
        text: classified.content.clone(),
        text_hash: classified.content_hash.clone(),
        text_chars: classified.content.chars().count() as u64,
        tool_name: tool_name.map_or_else(Known::unknown, Known::value),
        segment_id: 0,
        raw_unit_refs: refs,
        frame_class: Some(classified.class.clone()),
    });
    let segment = analysis.segments.last_mut().expect("segment draft");
    if segment.first_turn.is_none() {
        segment.first_turn = Some(turn_idx);
        segment.started_at = classified.seal.seal_ts.clone();
    }
    segment.last_turn = turn_idx;
    if let Known::Value(_) = classified.seal.seal_ts {
        segment.ended_at = classified.seal.seal_ts.clone();
    }
}

fn extract_tool_result_string(call_val: &Value) -> String {
    let Some(obj) = call_val.as_object() else {
        return String::new();
    };
    if let Some(res) = obj.get("result") {
        if let Some(s) = res.as_str() {
            return s.to_owned();
        }
        if let Some(arr) = res.as_array() {
            for item in arr {
                if let Some(iobj) = item.as_object()
                    && let Some(fr) = iobj.get("functionResponse").and_then(Value::as_object)
                    && let Some(resp) = fr.get("response").and_then(Value::as_object)
                    && let Some(out) = string_field(resp, "output")
                {
                    return out.to_owned();
                }
            }
        }
    }
    String::new()
}

fn string_field_from_val<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.as_object().and_then(|o| string_field(o, key))
}

fn has_thought(obj: &serde_json::Map<String, Value>) -> bool {
    if obj.get("thought").and_then(Value::as_bool).unwrap_or(false) || obj.contains_key("thinking")
    {
        return true;
    }
    if let Some(thoughts) = obj.get("thoughts").and_then(Value::as_array)
        && !thoughts.is_empty()
    {
        return true;
    }
    if let Some(parts) = get_parts(obj) {
        return parts.iter().any(|p| {
            if let Some(po) = p.as_object() {
                po.get("thought").and_then(Value::as_bool).unwrap_or(false)
                    || po.contains_key("thinking")
            } else {
                false
            }
        });
    }
    false
}

fn get_parts(obj: &serde_json::Map<String, Value>) -> Option<&Vec<Value>> {
    obj.get("content")
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .or_else(|| obj.get("parts").and_then(Value::as_array))
}

fn extract_text(obj: &serde_json::Map<String, Value>) -> String {
    if let Some(s) = string_field(obj, "content") {
        return s.to_owned();
    }
    if let Some(c) = obj.get("content") {
        if let Some(s) = c.as_str() {
            return s.to_owned();
        }
        if let Some(arr) = c.as_array() {
            let extracted = extract_text_from_array(arr);
            if !extracted.is_empty() {
                return extracted;
            }
        }
        if let Some(co) = c.as_object() {
            if let Some(parts) = co.get("parts").and_then(Value::as_array) {
                let extracted = extract_text_from_array(parts);
                if !extracted.is_empty() {
                    return extracted;
                }
            }
            if let Some(t) = string_field(co, "text") {
                return t.to_owned();
            }
        }
    }
    if let Some(parts) = obj.get("parts").and_then(Value::as_array) {
        let extracted = extract_text_from_array(parts);
        if !extracted.is_empty() {
            return extracted;
        }
    }
    if let Some(t) = string_field(obj, "text") {
        return t.to_owned();
    }
    String::new()
}

fn extract_text_from_array(arr: &[Value]) -> String {
    let mut parts = Vec::new();
    for item in arr {
        if let Some(s) = item.as_str() {
            if !s.is_empty() {
                parts.push(s.to_owned());
            }
        } else if let Some(obj) = item.as_object() {
            if obj.get("thought").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            if let Some(t) = string_field(obj, "text").or_else(|| string_field(obj, "content"))
                && !t.is_empty()
            {
                parts.push(t.to_owned());
            }
        }
    }
    parts.join("\n")
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
    let measured = measure(block);
    consume_logical_measured(raw, measured, block_index, kind, ctx, logical, analysis)
}

/// Consume a logical unit that has already been measured.
fn consume_logical_measured(
    raw: &RawUnit,
    measured: Measured,
    block_index: usize,
    kind: &str,
    ctx: &mut Ctx<'_>,
    logical: &mut Vec<ClassifiedUnit>,
    analysis: &mut Analysis,
) -> Result<RawUnitRef, AdapterError> {
    let ordinal = ctx.next_logical_ordinal;
    ctx.next_logical_ordinal += 1;
    let evidence = logical_evidence(raw, block_index, kind, ordinal, ctx, measured)?;
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

/// Terminate a logical unit as `skipped(oversized)`: evidence over the block
/// it would have been, the typed warning the validator requires, and the
/// `visible_event_lost` flag, because something the operator could have seen
/// is not in the model.
fn skip_logical_oversized(
    raw: &RawUnit,
    measured: Measured,
    block_index: usize,
    unit_kind: &str,
    ctx: &mut Ctx<'_>,
    logical: &mut Vec<ClassifiedUnit>,
    analysis: &mut Analysis,
) -> Result<(), AdapterError> {
    let ordinal = ctx.next_logical_ordinal;
    ctx.next_logical_ordinal += 1;
    let bytes = measured.bytes;
    let evidence = logical_evidence(raw, block_index, unit_kind, ordinal, ctx, measured)?;
    logical.push(ClassifiedUnit {
        ordinal,
        level: RawUnitLevel::Logical {
            parent_ordinal: raw.coverage_ordinal,
        },
        evidence: evidence.clone(),
        disposition: ClassifiedDisposition::Skipped {
            reason: SkippedReason::Oversized,
            visible: true,
        },
    });
    analysis.skipped.push(SkippedUnit {
        ordinal,
        reason: SkippedReason::Oversized,
        bytes,
        visible: true,
        evidence,
    });
    warn(analysis, WarningKind::OversizedUnit, ordinal);
    analysis.visible_event_lost = true;
    Ok(())
}

/// Rebuild an oversized message without the nested blocks that are themselves
/// oversized, recording each of those as a skipped logical unit with its own
/// evidence and leaving an index-stable marker in its place. Returns the
/// reduced message with its canonical length and hash, or `None` — after
/// recording the message itself as skipped — when nothing removable explains
/// its size (a single enormous text body, say).
///
/// Only one child array is reduced, mirroring the precedence
/// [`emit_message`] uses to emit tool calls (`toolCalls`, else `parts`, else
/// `content.parts`), so a skipped child's locator is exactly the locator it
/// would have consumed under.
fn reduce_oversized_message(
    raw: &RawUnit,
    obj: &serde_json::Map<String, Value>,
    block_index: usize,
    kind: &str,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<Option<(Value, Measured)>, AdapterError> {
    let mut reduced = serde_json::Map::with_capacity(obj.len());
    let has_tool_calls = obj.get("toolCalls").is_some_and(Value::is_array);
    let has_parts = obj.get("parts").is_some_and(Value::is_array);
    for (key, value) in obj {
        let value = match (key.as_str(), value) {
            ("toolCalls", Value::Array(items)) => Value::Array(reduce_children(
                raw,
                items,
                block_index,
                |_| "tool_call",
                ctx,
                analysis,
                logical,
            )?),
            ("parts", Value::Array(items)) if !has_tool_calls => Value::Array(reduce_children(
                raw,
                items,
                block_index,
                part_kind,
                ctx,
                analysis,
                logical,
            )?),
            ("content", Value::Object(content))
                if !has_tool_calls
                    && !has_parts
                    && content.get("parts").is_some_and(Value::is_array) =>
            {
                let mut content_reduced = serde_json::Map::with_capacity(content.len());
                for (content_key, content_value) in content {
                    let content_value = match (content_key.as_str(), content_value) {
                        ("parts", Value::Array(items)) => Value::Array(reduce_children(
                            raw,
                            items,
                            block_index,
                            part_kind,
                            ctx,
                            analysis,
                            logical,
                        )?),
                        _ => content_value.clone(),
                    };
                    content_reduced.insert(content_key.clone(), content_value);
                }
                Value::Object(content_reduced)
            }
            _ => value.clone(),
        };
        reduced.insert(key.clone(), value);
    }
    let reduced = Value::Object(reduced);
    let measured = measure(&reduced);
    if measured.bytes > MAX_LOGICAL_UNIT_BYTES {
        skip_logical_oversized(raw, measured, block_index, kind, ctx, logical, analysis)?;
        return Ok(None);
    }
    Ok(Some((reduced, measured)))
}

/// Copy `items`, replacing every child over the unit cap with a marker after
/// terminating it as `skipped(oversized)`. Child locators follow the
/// `block_index * 1000 + index + 1` scheme [`emit_tool_call`] consumes under.
fn reduce_children(
    raw: &RawUnit,
    items: &[Value],
    block_index: usize,
    kind_of: fn(&Value) -> &'static str,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<Vec<Value>, AdapterError> {
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let measured = measure(item);
        if measured.bytes > MAX_LOGICAL_UNIT_BYTES {
            let marker = oversized_marker(measured.bytes, &measured.hash);
            skip_logical_oversized(
                raw,
                measured,
                block_index * 1000 + index + 1,
                kind_of(item),
                ctx,
                logical,
                analysis,
            )?;
            out.push(marker);
        } else {
            out.push(item.clone());
        }
    }
    Ok(out)
}

fn part_kind(part: &Value) -> &'static str {
    if part.get("functionCall").is_some() {
        "tool_call"
    } else {
        "part"
    }
}

fn oversized_marker(bytes: u64, sha256: &str) -> Value {
    let mut evidence = serde_json::Map::with_capacity(2);
    evidence.insert("bytes".to_owned(), Value::from(bytes));
    evidence.insert("sha256".to_owned(), Value::from(sha256));
    let mut marker = serde_json::Map::with_capacity(1);
    marker.insert(OVERSIZED_MARKER_KEY.to_owned(), Value::Object(evidence));
    Value::Object(marker)
}

fn is_oversized_marker(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|obj| obj.len() == 1 && obj.contains_key(OVERSIZED_MARKER_KEY))
}

/// Canonical length and SHA-256 of one logical block: the two facts its
/// evidence needs and the one fact the unit bound is decided on.
#[derive(Debug, Clone)]
struct Measured {
    bytes: u64,
    hash: String,
}

/// Measure `canonical_json(value)` without materializing it. Byte-identical
/// to hashing [`canonical_json`]'s output, including the empty-bytes fallback
/// when serialization fails.
fn measure(value: &Value) -> Measured {
    let mut sink = Sha256Stream::new();
    if serde_json::to_writer(&mut sink, value).is_err() {
        return Measured {
            bytes: 0,
            hash: sha256_hex(&[]),
        };
    }
    Measured {
        bytes: sink.bytes_hashed(),
        hash: sink.finalize_hex(),
    }
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
    block_index: usize,
    unit_kind: &str,
    ordinal: u64,
    ctx: &Ctx<'_>,
    measured: Measured,
) -> Result<RawUnitRef, AdapterError> {
    let Measured {
        bytes: payload_len,
        hash: content_hash,
    } = measured;
    let locator = format!("{:06}:blk:{block_index}", raw.physical_ordinal);
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
        original_bytes: payload_len,
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
                compaction_boundary_present: false,
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
            scope_status: crate::engine::ScopeStatus::from_evidence(
                match &d.cwd {
                    Known::Value(cwd) => Some(cwd.as_str()),
                    Known::Unknown(_) => None,
                },
                match &d.branch {
                    Known::Value(branch) => Some(branch.as_str()),
                    Known::Unknown(_) => None,
                },
            ),
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
