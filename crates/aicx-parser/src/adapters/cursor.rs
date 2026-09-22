//! Cursor agent-transcript JSONL adapter for the deterministic parser kernel.
//!
//! This module consumes exactly the [`SourceHandle`] supplied by the caller.
//! It never discovers sessions, reads `agent-tools/` siblings, or consults
//! process state. Tool results live out-of-band and are not invented here.
//!
//! A Cursor transcript is one JSON record per line, append-only, stored at
//! `~/.cursor/projects/<slug>/agent-transcripts/<uuid>/<uuid>.jsonl`.
//! Conversation rows are `{role, message.content[]}`; the only recognized
//! bookkeeping envelope is `{type:"turn_ended", status}`.

use super::{
    AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel, sealed,
};
use crate::engine::frames::{self, FrameClass, TransportFrame, TransportKind, TransportPayload};
use crate::engine::{
    AgentKind, BoundaryFlags, ConsumedUnit, CoverageReport, CoverageWarning, Known, ParseStatus,
    Provenance, ProviderConversationRef, RawUnitRef, Segment, SessionModel, SkippedReason,
    SkippedUnit, SourceHandle, SourceRead, ToolEvent, ToolEventKind, Turn, TurnKind, TurnRange,
    TurnRole, UnitBoundary, UnvalidatedParse, VisibleCompleteness, WarningKind,
    evidence_event_id_from_hash, ordinal_locator, sha256_hex,
};
use serde_json::Value;

pub const CURSOR_ADAPTER_VERSION: &str = "cursor-transcript-v1";

const MAX_LOGICAL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default)]
pub struct CursorAdapter;

impl sealed::Sealed for CursorAdapter {}

impl AgentAdapter for CursorAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Cursor
    }

    fn adapter_version(&self) -> &'static str {
        CURSOR_ADAPTER_VERSION
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
        analysis.into_parse(source, read)
    }
}

struct Analysis {
    classified: Vec<ClassifiedUnit>,
    consumed: Vec<ConsumedUnit>,
    skipped: Vec<SkippedUnit>,
    warnings: Vec<CoverageWarning>,
    turns: Vec<Turn>,
    tools: Vec<ToolEvent>,
    started_at: Known<String>,
    ended_at: Known<String>,
    unsupported_visible: bool,
    malformed_tail: bool,
    visible_lost: bool,
}

struct Ctx<'a> {
    session_id: &'a str,
    next_logical_ordinal: u64,
}

fn analyze(source: &SourceHandle, read: &SourceRead) -> Result<Analysis, AdapterError> {
    if source.artifacts().len() != 1 {
        return Err(AdapterError::new(
            "classify",
            "a Cursor transcript source is exactly one explicit JSONL artifact",
        ));
    }
    if source.artifacts()[0].framing() != crate::engine::SourceFraming::JsonLines {
        return Err(AdapterError::new(
            "classify",
            "Cursor transcript artifacts must use json_lines framing",
        ));
    }
    let session_id = source
        .logical_session_id()
        .unwrap_or_else(|| source.source_id());
    let mut ctx = Ctx {
        session_id,
        next_logical_ordinal: read.units.len() as u64 + 1,
    };
    let mut analysis = Analysis {
        classified: Vec::new(),
        consumed: Vec::new(),
        skipped: Vec::new(),
        warnings: Vec::new(),
        turns: Vec::new(),
        tools: Vec::new(),
        started_at: Known::unknown(),
        ended_at: Known::unknown(),
        unsupported_visible: false,
        malformed_tail: false,
        visible_lost: false,
    };
    let mut logical = Vec::new();
    for raw in &read.units {
        walk_physical(raw, &mut ctx, &mut analysis, &mut logical)?;
    }
    analysis.classified.extend(logical);
    Ok(analysis)
}

fn walk_physical(
    raw: &crate::engine::RawUnit,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    if raw.boundary == UnitBoundary::Oversized {
        analysis.visible_lost = true;
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

    let parsed = serde_json::from_slice::<Value>(&raw.bytes);
    if raw.boundary == UnitBoundary::UnterminatedTail {
        if parsed.is_err() {
            analysis.malformed_tail = true;
            analysis.visible_lost = true;
            warn(analysis, WarningKind::MalformedUnit, raw.coverage_ordinal);
            warn(
                analysis,
                WarningKind::UnterminatedTail,
                raw.coverage_ordinal,
            );
            return skip_physical(
                raw,
                "malformed",
                SkippedReason::Malformed,
                true,
                ctx,
                analysis,
            );
        }
        warn(
            analysis,
            WarningKind::UnterminatedTail,
            raw.coverage_ordinal,
        );
    }

    let Ok(value) = parsed else {
        analysis.visible_lost = true;
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

    let role = string_at(&value, &["role"]);
    let record_type = string_at(&value, &["type"]);
    match (role, record_type) {
        (Some("user"), _) => {
            let evidence = consume_physical(raw, "user", ctx)?;
            analysis
                .classified
                .push(physical_consumed(raw, evidence.clone(), "user"));
            analysis.consumed.push(ConsumedUnit {
                ordinal: raw.coverage_ordinal,
                kind: "user".to_owned(),
                evidence: evidence.clone(),
            });
            walk_content(
                &value,
                raw,
                TurnRole::User,
                &evidence,
                ctx,
                analysis,
                logical,
            )?;
        }
        (Some("assistant"), _) => {
            let evidence = consume_physical(raw, "assistant", ctx)?;
            analysis
                .classified
                .push(physical_consumed(raw, evidence.clone(), "assistant"));
            analysis.consumed.push(ConsumedUnit {
                ordinal: raw.coverage_ordinal,
                kind: "assistant".to_owned(),
                evidence: evidence.clone(),
            });
            walk_content(
                &value,
                raw,
                TurnRole::Assistant,
                &evidence,
                ctx,
                analysis,
                logical,
            )?;
        }
        (_, Some("turn_ended")) => {
            let evidence = consume_physical(raw, "turn_ended", ctx)?;
            analysis
                .classified
                .push(physical_consumed(raw, evidence.clone(), "turn_ended"));
            analysis.consumed.push(ConsumedUnit {
                ordinal: raw.coverage_ordinal,
                kind: "turn_ended".to_owned(),
                evidence,
            });
        }
        (Some(other), _) => {
            analysis.unsupported_visible = true;
            warn(
                analysis,
                WarningKind::UnknownPayloadType,
                raw.coverage_ordinal,
            );
            skip_physical(
                raw,
                &format!("unknown_role:{other}"),
                SkippedReason::UnknownPayloadType,
                true,
                ctx,
                analysis,
            )?;
        }
        _ => {
            analysis.unsupported_visible = true;
            warn(
                analysis,
                WarningKind::UnknownPayloadType,
                raw.coverage_ordinal,
            );
            skip_physical(
                raw,
                "unknown_payload",
                SkippedReason::UnknownPayloadType,
                true,
                ctx,
                analysis,
            )?;
        }
    }
    Ok(())
}

fn walk_content(
    record: &Value,
    raw: &crate::engine::RawUnit,
    role: TurnRole,
    parent: &RawUnitRef,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    let Some(content) = record.pointer("/message/content") else {
        return Ok(());
    };
    if let Some(text) = content.as_str() {
        if !text.is_empty() {
            push_speech_turn(role, text, parent, analysis);
        }
        return Ok(());
    }
    let Some(blocks) = content.as_array() else {
        return Ok(());
    };
    for (index, block) in blocks.iter().enumerate() {
        classify_block(block, raw, role, parent, index, ctx, analysis, logical)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn classify_block(
    block: &Value,
    raw: &crate::engine::RawUnit,
    role: TurnRole,
    _parent: &RawUnitRef,
    index: usize,
    ctx: &mut Ctx<'_>,
    analysis: &mut Analysis,
    logical: &mut Vec<ClassifiedUnit>,
) -> Result<(), AdapterError> {
    let bytes = serde_json::to_vec(block).unwrap_or_default();
    let locator = format!(
        "{}:blk:{}",
        ordinal_locator(raw.physical_ordinal),
        index + 1
    );
    if bytes.len() > MAX_LOGICAL_BYTES {
        analysis.visible_lost = true;
        warn(
            analysis,
            WarningKind::OversizedUnit,
            ctx.next_logical_ordinal,
        );
        let unit = skip_logical(
            raw,
            ctx,
            &locator,
            "oversized",
            SkippedReason::Oversized,
            true,
            bytes.len() as u64,
            &sha256_hex(&bytes),
        )?;
        analysis.skipped.push(SkippedUnit {
            ordinal: unit.ordinal,
            bytes: unit.evidence.original_bytes,
            reason: SkippedReason::Oversized,
            visible: true,
            evidence: unit.evidence.clone(),
        });
        logical.push(unit);
        return Ok(());
    }
    let block_type = string_at(block, &["type"]).unwrap_or("");
    match block_type {
        "text" => {
            let unit = consume_logical(raw, ctx, &locator, "text", &bytes)?;
            analysis.consumed.push(ConsumedUnit {
                ordinal: unit.ordinal,
                kind: "text".to_owned(),
                evidence: unit.evidence.clone(),
            });
            let text = string_at(block, &["text"]).unwrap_or("");
            if !text.is_empty() {
                push_speech_turn(role, text, &unit.evidence, analysis);
            }
            logical.push(unit);
        }
        "tool_use" => {
            let unit = consume_logical(raw, ctx, &locator, "tool_use", &bytes)?;
            analysis.consumed.push(ConsumedUnit {
                ordinal: unit.ordinal,
                kind: "tool_use".to_owned(),
                evidence: unit.evidence.clone(),
            });
            let name = string_at(block, &["name"]).unwrap_or("unknown_tool");
            let body = block.get("input").map(value_text).unwrap_or_default();
            push_tool_call(name, body, &unit.evidence, analysis);
            logical.push(unit);
        }
        _ => {
            analysis.unsupported_visible = true;
            warn(
                analysis,
                WarningKind::UnknownPayloadType,
                ctx.next_logical_ordinal,
            );
            let unit = skip_logical(
                raw,
                ctx,
                &locator,
                "unknown_payload",
                SkippedReason::UnknownPayloadType,
                true,
                bytes.len() as u64,
                &sha256_hex(&bytes),
            )?;
            analysis.skipped.push(SkippedUnit {
                ordinal: unit.ordinal,
                bytes: unit.evidence.original_bytes,
                reason: SkippedReason::UnknownPayloadType,
                visible: true,
                evidence: unit.evidence.clone(),
            });
            logical.push(unit);
        }
    }
    Ok(())
}

fn push_speech_turn(role: TurnRole, text: &str, evidence: &RawUnitRef, analysis: &mut Analysis) {
    let (body, timestamp) = match role {
        TurnRole::User => peel_user_query(text),
        _ => (text.to_owned(), Known::unknown()),
    };
    if body.is_empty() {
        return;
    }
    if matches!(&analysis.started_at, Known::Unknown(_))
        && let Known::Value(stamp) = &timestamp
    {
        analysis.started_at = Known::value(stamp.clone());
    }
    if let Known::Value(stamp) = &timestamp {
        analysis.ended_at = Known::value(stamp.clone());
    }
    let kind = match role {
        TurnRole::User => TurnKind::UserMsg,
        TurnRole::Assistant => TurnKind::AgentReply,
        TurnRole::System => TurnKind::SystemNote,
        TurnRole::Tool => TurnKind::ToolCall,
    };
    let turn_idx = analysis.turns.len() as u64;
    analysis.turns.push(Turn {
        turn_idx,
        role,
        timestamp,
        kind,
        text_hash: sha256_hex(body.as_bytes()),
        text_chars: body.chars().count() as u64,
        text: body,
        tool_name: Known::unknown(),
        segment_id: 0,
        raw_unit_refs: vec![evidence.clone()],
        frame_class: None,
    });
}

fn push_tool_call(name: &str, body: String, evidence: &RawUnitRef, analysis: &mut Analysis) {
    // A `tool_use` block is the agent's own tool lane; route it through the
    // shared frame taxonomy so the executor axis (`--agent-commands`) can
    // prove who ran it — a class-less shell action is dropped whenever a
    // specific executor is requested.
    let classified = frames::classify(&TransportFrame {
        agent: AgentKind::Cursor,
        transport_kind: TransportKind::AgentToolCall,
        timestamp: Known::unknown(),
        payload: TransportPayload::Shell {
            command: body.clone(),
            result: String::new(),
        },
        evidence: evidence.clone(),
    });
    let frame_class = match classified.class {
        class @ FrameClass::ShellAction { .. } => Some(class),
        _ => None,
    };
    let turn_idx = analysis.turns.len() as u64;
    analysis.turns.push(Turn {
        turn_idx,
        role: TurnRole::Tool,
        timestamp: Known::unknown(),
        kind: TurnKind::ToolCall,
        text_hash: sha256_hex(body.as_bytes()),
        text_chars: body.chars().count() as u64,
        text: body.clone(),
        tool_name: Known::value(name.to_owned()),
        segment_id: 0,
        raw_unit_refs: vec![evidence.clone()],
        frame_class,
    });
    analysis.tools.push(ToolEvent {
        kind: ToolEventKind::Call,
        turn_idx,
        tool_name: name.to_owned(),
        correlation_id: Known::unknown(),
        payload_hash: sha256_hex(body.as_bytes()),
        payload_bytes: body.len() as u64,
        raw_unit_refs: vec![evidence.clone()],
    });
}

impl Analysis {
    fn into_parse(
        mut self,
        source: &SourceHandle,
        read: &SourceRead,
    ) -> Result<UnvalidatedParse, AdapterError> {
        self.warnings.sort_by_key(|warning| warning.first_ordinal);
        let status = ParseStatus {
            visible_completeness: if self.visible_lost || self.malformed_tail {
                VisibleCompleteness::PartialVisible
            } else {
                VisibleCompleteness::CompleteVisible
            },
            boundary_flags: BoundaryFlags {
                opaque_reasoning_present: false,
                unsupported_visible_event: self.unsupported_visible,
                compaction_boundary_present: false,
            },
            malformed_tail_present: self.malformed_tail,
            visible_event_lost: self.visible_lost,
        };
        let coverage = CoverageReport::with_raw_line_count(
            read.units.len() as u64,
            self.consumed.len() as u64 + self.skipped.len() as u64,
            self.consumed,
            self.skipped,
            self.warnings,
            status,
        );
        let session_id = source
            .logical_session_id()
            .unwrap_or_else(|| source.source_id())
            .to_owned();
        let provenance = Provenance {
            agent: AgentKind::Cursor,
            model: Known::unknown(),
            cli_version: Known::unknown(),
            cwd: Known::unknown(),
            branch: Known::unknown(),
            started_at: self.started_at,
            ended_at: self.ended_at,
            original_source_hash: read.source_hash.clone(),
            original_source_bytes: read.source_bytes,
        };
        let mut model = SessionModel::new(session_id.clone(), provenance, coverage);
        model.conversation = ProviderConversationRef::Cursor {
            session_id,
            worker_id: None,
            unobserved: vec!["worker_id".to_owned()],
        };
        model.turns = self.turns;
        model.tool_events = self.tools;
        if !model.turns.is_empty() {
            let end = model.turns.len() as u64 - 1;
            model.segments = vec![Segment {
                segment_id: 0,
                scope_root: None,
                cwd: Known::unknown(),
                branch: Known::unknown(),
                started_at: model.provenance.started_at.clone(),
                ended_at: model.provenance.ended_at.clone(),
                turn_range: TurnRange { start: 0, end },
                scope_status: crate::engine::ScopeStatus::from_evidence(None, None),
                // Only the Codex adapter observes explicit tool-call workdirs.
                scope_conflict: false,
            }];
        }
        Ok(UnvalidatedParse::from_model(model))
    }
}

fn consume_physical(
    raw: &crate::engine::RawUnit,
    unit_kind: &str,
    ctx: &Ctx<'_>,
) -> Result<RawUnitRef, AdapterError> {
    let locator = ordinal_locator(raw.physical_ordinal);
    let evidence_event_id = evidence_event_id_from_hash(
        AgentKind::Cursor,
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

fn consume_logical(
    raw: &crate::engine::RawUnit,
    ctx: &mut Ctx<'_>,
    locator: &str,
    unit_kind: &str,
    bytes: &[u8],
) -> Result<ClassifiedUnit, AdapterError> {
    let content_hash = sha256_hex(bytes);
    let evidence_event_id = evidence_event_id_from_hash(
        AgentKind::Cursor,
        ctx.session_id,
        locator,
        unit_kind,
        &content_hash,
    )
    .map_err(|error| AdapterError::new("classify", error.to_string()))?;
    let ordinal = ctx.next_logical_ordinal;
    ctx.next_logical_ordinal += 1;
    Ok(ClassifiedUnit {
        ordinal,
        level: RawUnitLevel::Logical {
            parent_ordinal: raw.coverage_ordinal,
        },
        evidence: RawUnitRef {
            evidence_event_id,
            coverage_ordinal: ordinal,
            physical_ordinal: raw.physical_ordinal,
            locator: locator.to_owned(),
            unit_kind: unit_kind.to_owned(),
            artifact: raw.artifact_name.clone(),
            content_hash,
            original_bytes: bytes.len() as u64,
        },
        disposition: ClassifiedDisposition::Consumed {
            kind: unit_kind.to_owned(),
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn skip_logical(
    raw: &crate::engine::RawUnit,
    ctx: &mut Ctx<'_>,
    locator: &str,
    unit_kind: &str,
    reason: SkippedReason,
    visible: bool,
    original_bytes: u64,
    content_hash: &str,
) -> Result<ClassifiedUnit, AdapterError> {
    let evidence_event_id = evidence_event_id_from_hash(
        AgentKind::Cursor,
        ctx.session_id,
        locator,
        unit_kind,
        content_hash,
    )
    .map_err(|error| AdapterError::new("classify", error.to_string()))?;
    let ordinal = ctx.next_logical_ordinal;
    ctx.next_logical_ordinal += 1;
    Ok(ClassifiedUnit {
        ordinal,
        level: RawUnitLevel::Logical {
            parent_ordinal: raw.coverage_ordinal,
        },
        evidence: RawUnitRef {
            evidence_event_id,
            coverage_ordinal: ordinal,
            physical_ordinal: raw.physical_ordinal,
            locator: locator.to_owned(),
            unit_kind: unit_kind.to_owned(),
            artifact: raw.artifact_name.clone(),
            content_hash: content_hash.to_owned(),
            original_bytes,
        },
        disposition: ClassifiedDisposition::Skipped { reason, visible },
    })
}

fn skip_physical(
    raw: &crate::engine::RawUnit,
    unit_kind: &str,
    reason: SkippedReason,
    visible: bool,
    ctx: &Ctx<'_>,
    analysis: &mut Analysis,
) -> Result<(), AdapterError> {
    let locator = ordinal_locator(raw.physical_ordinal);
    let evidence_event_id = evidence_event_id_from_hash(
        AgentKind::Cursor,
        ctx.session_id,
        &locator,
        unit_kind,
        &raw.content_hash,
    )
    .map_err(|error| AdapterError::new("classify", error.to_string()))?;
    let evidence = RawUnitRef {
        evidence_event_id,
        coverage_ordinal: raw.coverage_ordinal,
        physical_ordinal: raw.physical_ordinal,
        locator,
        unit_kind: unit_kind.to_owned(),
        artifact: raw.artifact_name.clone(),
        content_hash: raw.content_hash.clone(),
        original_bytes: raw.original_bytes,
    };
    analysis.classified.push(ClassifiedUnit {
        ordinal: raw.coverage_ordinal,
        level: RawUnitLevel::Physical,
        evidence: evidence.clone(),
        disposition: ClassifiedDisposition::Skipped { reason, visible },
    });
    analysis.skipped.push(SkippedUnit {
        ordinal: raw.coverage_ordinal,
        bytes: raw.original_bytes,
        reason,
        visible,
        evidence,
    });
    Ok(())
}

fn physical_consumed(
    raw: &crate::engine::RawUnit,
    evidence: RawUnitRef,
    kind: &str,
) -> ClassifiedUnit {
    ClassifiedUnit {
        ordinal: raw.coverage_ordinal,
        level: RawUnitLevel::Physical,
        evidence,
        disposition: ClassifiedDisposition::Consumed {
            kind: kind.to_owned(),
        },
    }
}

fn warn(analysis: &mut Analysis, kind: WarningKind, ordinal: u64) {
    if let Some(warning) = analysis
        .warnings
        .iter_mut()
        .find(|warning| warning.kind == kind)
    {
        warning.count += 1;
        warning.first_ordinal = warning.first_ordinal.min(ordinal);
    } else {
        analysis.warnings.push(CoverageWarning {
            kind,
            count: 1,
            first_ordinal: ordinal,
        });
    }
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str()
}

fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default())
}

/// Cursor wraps operator speech in `<user_query>`. `<timestamp>` is a
/// human clock (`Thursday, Sep 17, 2026, 10:43 PM (UTC+2)`), not RFC 3339,
/// so it stays unknown — the kernel forbids a known non-RFC3339 stamp.
fn peel_user_query(text: &str) -> (String, Known<String>) {
    if let Some(query) = tagged_inner(text, "user_query") {
        (query, Known::unknown())
    } else {
        (text.to_owned(), Known::unknown())
    }
}

fn tagged_inner(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim().to_owned())
}
