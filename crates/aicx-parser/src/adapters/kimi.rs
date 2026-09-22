//! Kimi Code CLI `wire.jsonl` adapter for the deterministic parser kernel.
//!
//! This module consumes exactly the [`SourceHandle`] supplied by the caller.
//! It never discovers sessions, reads siblings, or consults process state.
//!
//! A Kimi wire file is one JSON record per line, append-only, written per
//! agent lane (`session_<uuid>/agents/<agentId>/wire.jsonl`). Conversation
//! content lives on two envelopes: `context.append_message` (operator
//! prompts) and `context.append_loop_event` (`content.part` text/think,
//! `tool.call`, `tool.result`, `step.begin`/`step.end`). Everything else —
//! `llm.request`, `usage.record`, `token_counting.*`, `metadata`,
//! `profile.bind`, turn/prompt/task lifecycle markers — is bookkeeping:
//! deliberately skipped as non-visible so it never inflates the visible-skip
//! count, and deliberately not consumed so the coverage ledger says what the
//! adapter claims as conversation.

// C5X owns the shared module export. Until that dispatch cut lands this sealed
// implementation is intentionally private and would otherwise trip dead-code.
#![allow(dead_code)]

use super::{
    AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel, sealed,
};
use crate::engine::frames::{
    self, FrameClass, TransportFrame, TransportKind, TransportPayload, TransportRole,
};
use crate::engine::{
    AgentKind, BoundaryFlags, ConsumedUnit, ContextEpochRef, CoverageReport, CoverageWarning,
    Known, ParseStatus, Provenance, ProviderConversationRef, RawUnitRef, Segment, SessionModel,
    SkillInvocation, SkippedReason, SkippedUnit, SourceHandle, SourceRead, ToolEvent,
    ToolEventKind, Turn, TurnKind, TurnRange, TurnRole, UnitBoundary, UnvalidatedParse,
    VisibleCompleteness, WarningKind, evidence_event_id_from_hash, ordinal_locator, sha256_hex,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub const KIMI_ADAPTER_VERSION: &str = "kimi-wire-v1";

/// Known wire envelopes that carry no conversation. They are understood and
/// deliberately dropped — not a coverage gap — so they must not surface as
/// visible unknown payloads.
///
/// `context.undo` (with `count = N`) semantically retracts the N previous
/// context messages and `context.clear` drops the whole context; the wire
/// stays append-only, so the retracted turns remain part of the recorded
/// history. They are bookkeeping markers here, matching the
/// skipped/non-visible contract of the other adapter envelopes above.
const BOOKKEEPING_TYPES: &[&str] = &[
    "metadata",
    "profile.bind",
    "runtime.set_binding",
    "permission.set_mode",
    "permission.record_approval_result",
    "llm.request",
    "llm.tools_snapshot",
    "usage.record",
    "token_counting.measured",
    "token_counting.turn_recorded",
    "token_counting.rebased",
    "turn.prompt",
    "turn.ended",
    "turn.steer",
    "turn.cancel",
    "turn.step.interrupted",
    "prompt.accepted",
    "prompt.completed",
    "prompt.aborted",
    "prompt.steered",
    "interaction.request",
    "interaction.resolved",
    "goal.create",
    "goal.update",
    "goal.clear",
    "task.started",
    "task.terminated",
    "task.waitDelivered",
    "file_history.tracked",
    "file_history.checkpoint",
    "tools.update_store",
    "full_compaction.begin",
    "full_compaction.complete",
    "full_compaction.cancel",
    "plan_mode.enter",
    "plan_mode.cancel",
    "swarm_mode.enter",
    "swarm_mode.exit",
    "plugin.session_start",
    "context.undo",
    "context.clear",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct KimiAdapter;

impl sealed::Sealed for KimiAdapter {}

impl AgentAdapter for KimiAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Kimi
    }

    fn adapter_version(&self) -> &'static str {
        KIMI_ADAPTER_VERSION
    }

    fn classify(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
    ) -> Result<Vec<ClassifiedUnit>, AdapterError> {
        if source.artifacts().len() != 1 {
            return Err(AdapterError::new(
                "classify",
                "a Kimi wire source is exactly one explicit JSONL artifact",
            ));
        }
        if source.artifacts()[0].framing() != crate::engine::SourceFraming::JsonLines {
            return Err(AdapterError::new(
                "classify",
                "Kimi wire artifacts must use json_lines framing",
            ));
        }
        let session_id = source
            .logical_session_id()
            .unwrap_or_else(|| source.source_id());
        read.units
            .iter()
            .map(|raw| classify_raw(source, session_id, raw))
            .collect()
    }

    fn assemble(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
        classified: Vec<ClassifiedUnit>,
    ) -> Result<UnvalidatedParse, AdapterError> {
        assemble_kimi(source, read, classified)
    }
}

fn classify_raw(
    source: &SourceHandle,
    session_id: &str,
    raw: &crate::engine::RawUnit,
) -> Result<ClassifiedUnit, AdapterError> {
    let locator = ordinal_locator(raw.physical_ordinal);
    let parsed = serde_json::from_slice::<Value>(&raw.bytes);
    let (unit_kind, disposition) = if raw.boundary == UnitBoundary::Oversized {
        (
            "oversized".to_owned(),
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::Oversized,
                visible: true,
            },
        )
    } else if let Ok(value) = parsed {
        let record_type = string_at(&value, &["type"]).unwrap_or("");
        match record_type {
            "context.append_message" => (
                "message".to_owned(),
                ClassifiedDisposition::Consumed {
                    kind: "message".to_owned(),
                },
            ),
            "context.append_loop_event" => {
                let event_type = string_at(&value, &["event", "type"]).unwrap_or("");
                match event_type {
                    "step.begin" | "step.end" | "content.part" | "tool.call" | "tool.result" => {
                        let kind = format!("loop_event:{event_type}");
                        (kind.clone(), ClassifiedDisposition::Consumed { kind })
                    }
                    _ => (
                        "unknown_payload".to_owned(),
                        ClassifiedDisposition::Skipped {
                            reason: SkippedReason::UnknownPayloadType,
                            visible: true,
                        },
                    ),
                }
            }
            "context.apply_compaction" => (
                "compaction".to_owned(),
                ClassifiedDisposition::Consumed {
                    kind: "compaction".to_owned(),
                },
            ),
            known if BOOKKEEPING_TYPES.contains(&known) => (
                known.to_owned(),
                ClassifiedDisposition::Skipped {
                    reason: SkippedReason::Unsupported,
                    visible: false,
                },
            ),
            _ => (
                "unknown_payload".to_owned(),
                ClassifiedDisposition::Skipped {
                    reason: SkippedReason::UnknownPayloadType,
                    visible: true,
                },
            ),
        }
    } else {
        (
            "malformed".to_owned(),
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::Malformed,
                visible: true,
            },
        )
    };
    let evidence_event_id = evidence_event_id_from_hash(
        source.agent(),
        session_id,
        &locator,
        &unit_kind,
        &raw.content_hash,
    )
    .map_err(|error| AdapterError::new("classify", error.to_string()))?;
    Ok(ClassifiedUnit {
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
        disposition,
    })
}

fn assemble_kimi(
    source: &SourceHandle,
    read: &SourceRead,
    classified: Vec<ClassifiedUnit>,
) -> Result<UnvalidatedParse, AdapterError> {
    let mut state = Assembly::new(source, read);
    for (raw, classified) in read.units.iter().zip(
        classified
            .iter()
            .filter(|unit| unit.level == RawUnitLevel::Physical),
    ) {
        if raw.boundary == UnitBoundary::UnterminatedTail {
            state.malformed_tail = true;
            state.visible_lost = true;
            state.warn(WarningKind::UnterminatedTail, raw.coverage_ordinal);
        }
        match &classified.disposition {
            ClassifiedDisposition::Consumed { .. } => {
                let value: Value = serde_json::from_slice(&raw.bytes)
                    .map_err(|error| AdapterError::new("assemble", error.to_string()))?;
                state.consume(&value, classified.evidence.clone())?;
            }
            ClassifiedDisposition::Skipped { reason, visible } => {
                state.observe_skip(classified, *reason, *visible, raw.boundary);
            }
        }
    }
    state.finish(classified)
}

struct Assembly<'a> {
    read: &'a SourceRead,
    session_id: String,
    agent_id: Option<String>,
    started_at: Known<String>,
    ended_at: Known<String>,
    segment_started_at: Known<String>,
    segments: Vec<SegmentDraft>,
    turns: Vec<Turn>,
    tools: Vec<ToolEvent>,
    tool_names: BTreeMap<String, String>,
    skills: Vec<SkillInvocation>,
    warnings: Vec<CoverageWarning>,
    unsupported_visible: bool,
    malformed_tail: bool,
    visible_lost: bool,
    compaction_boundary_present: bool,
    context_epochs: Vec<(ContextEpochRef, u64)>,
}

#[derive(Clone)]
struct SegmentDraft {
    started_at: Known<String>,
    ended_at: Known<String>,
    start_turn: u64,
}

impl<'a> Assembly<'a> {
    fn new(source: &'a SourceHandle, read: &'a SourceRead) -> Self {
        Self {
            read,
            session_id: source
                .logical_session_id()
                .unwrap_or_else(|| source.source_id())
                .to_owned(),
            agent_id: None,
            started_at: Known::unknown(),
            ended_at: Known::unknown(),
            segment_started_at: Known::unknown(),
            segments: Vec::new(),
            turns: Vec::new(),
            tools: Vec::new(),
            tool_names: BTreeMap::new(),
            skills: Vec::new(),
            warnings: Vec::new(),
            unsupported_visible: false,
            malformed_tail: false,
            visible_lost: false,
            compaction_boundary_present: false,
            context_epochs: Vec::new(),
        }
    }

    fn consume(&mut self, record: &Value, evidence: RawUnitRef) -> Result<(), AdapterError> {
        let timestamp = record_timestamp(record);
        if matches!(self.started_at, Known::Unknown(_)) {
            self.started_at = timestamp.clone();
        }
        if matches!(timestamp, Known::Value(_)) {
            self.ended_at = timestamp.clone();
        }
        if self.agent_id.is_none() {
            self.agent_id = string_at(record, &["agentId"])
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned);
        }
        match string_at(record, &["type"]).unwrap_or("") {
            "context.append_message" => self.append_message(record, timestamp, evidence),
            "context.append_loop_event" => self.loop_event(record, timestamp, evidence),
            "context.apply_compaction" => self.consume_compaction(record, timestamp, evidence),
            _ => Ok(()),
        }
    }

    fn append_message(
        &mut self,
        record: &Value,
        timestamp: Known<String>,
        evidence: RawUnitRef,
    ) -> Result<(), AdapterError> {
        let message = &record["message"];
        let role = match string_at(message, &["role"]).unwrap_or("system") {
            "user" => TransportRole::User,
            "assistant" => TransportRole::Assistant,
            "tool" => TransportRole::Tool,
            _ => TransportRole::System,
        };
        let text = content_text(message.get("content").unwrap_or(&Value::Null));
        let kind = if role == TransportRole::Assistant {
            TransportKind::AssistantMessage
        } else {
            TransportKind::DirectMessage
        };
        self.push_classified_frame(text_frame(kind, role, text, timestamp, evidence))
    }

    fn loop_event(
        &mut self,
        record: &Value,
        timestamp: Known<String>,
        evidence: RawUnitRef,
    ) -> Result<(), AdapterError> {
        let event = &record["event"];
        match string_at(event, &["type"]).unwrap_or("") {
            "content.part" => {
                let part = &event["part"];
                match string_at(part, &["type"]).unwrap_or("") {
                    "think" => self.push_turn(
                        TurnRole::Assistant,
                        TurnKind::InternalThought,
                        text_at(part, &["think"]),
                        timestamp,
                        Known::unknown(),
                        evidence,
                        None,
                    ),
                    // text and any future visible part kind land on the
                    // assistant lane; the text body lives on the same-named
                    // field for `text`, `text` is empty for kinds we do not
                    // know yet.
                    _ => self.push_classified_frame(text_frame(
                        TransportKind::AssistantMessage,
                        TransportRole::Assistant,
                        text_at(part, &["text"]),
                        timestamp,
                        evidence,
                    )),
                }
            }
            "tool.call" => self.push_tool_turn(event, timestamp, evidence, ToolEventKind::Call),
            "tool.result" => self.push_tool_turn(event, timestamp, evidence, ToolEventKind::Result),
            // step.begin / step.end mark the model loop; consumed for
            // coverage, carrying no conversation of their own.
            _ => Ok(()),
        }
    }

    fn consume_compaction(
        &mut self,
        record: &Value,
        timestamp: Known<String>,
        evidence: RawUnitRef,
    ) -> Result<(), AdapterError> {
        self.compaction_boundary_present = true;
        let mut epoch = ContextEpochRef {
            compaction_index: self.context_epochs.len() as u32,
            summary_provenance: evidence.evidence_event_id.clone(),
            replacement_refs: Vec::new(),
            trigger: Known::unknown(),
            first_turn_after: None,
        };
        // The compaction summary is the context the next epoch starts from.
        // Like a Codex compaction replay it is referenced by the epoch, never
        // re-emitted as speech.
        let summary = text_at(record, &["summary"]);
        if !summary.is_empty() {
            let frame = TransportFrame {
                agent: AgentKind::Kimi,
                transport_kind: TransportKind::InjectedContext,
                timestamp,
                payload: TransportPayload::Inject {
                    tag: "context.apply_compaction.summary".to_owned(),
                    content: summary,
                },
                evidence: evidence.clone(),
            };
            let classified = frames::classify(&frame);
            epoch.replacement_refs.push(classified.content_hash.clone());
            self.push_classified_frame(frame)?;
        }
        self.context_epochs.push((epoch, evidence.physical_ordinal));
        Ok(())
    }

    fn push_tool_turn(
        &mut self,
        event: &Value,
        timestamp: Known<String>,
        evidence: RawUnitRef,
        kind: ToolEventKind,
    ) -> Result<(), AdapterError> {
        let correlation_raw = string_at(event, &["toolCallId"]).map(str::to_owned);
        let explicit_name = string_at(event, &["name"]).map(str::to_owned);
        let name = explicit_name
            .or_else(|| {
                correlation_raw
                    .as_ref()
                    .and_then(|call_id| self.tool_names.get(call_id).cloned())
            })
            .unwrap_or_else(|| "unknown_tool".to_owned());
        if kind == ToolEventKind::Call
            && let Some(call_id) = &correlation_raw
        {
            self.tool_names.insert(call_id.clone(), name.clone());
        }
        let correlation = known_string(correlation_raw.as_deref());
        let body = match kind {
            ToolEventKind::Call => event
                .get("args")
                .or_else(|| event.get("arguments"))
                .or_else(|| event.get("input"))
                .map(value_text)
                .unwrap_or_default(),
            ToolEventKind::Result => tool_result_body(event),
        };
        let turn_kind = if kind == ToolEventKind::Call {
            TurnKind::ToolCall
        } else {
            TurnKind::ToolResult
        };
        self.push_turn(
            TurnRole::Tool,
            turn_kind,
            body.clone(),
            timestamp,
            Known::value(name.clone()),
            evidence.clone(),
            None,
        )?;
        let turn_idx = self.turns.len() as u64 - 1;
        self.tools.push(ToolEvent {
            kind,
            turn_idx,
            tool_name: name,
            correlation_id: correlation,
            payload_hash: sha256_hex(body.as_bytes()),
            payload_bytes: body.len() as u64,
            raw_unit_refs: vec![evidence],
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn push_turn(
        &mut self,
        role: TurnRole,
        kind: TurnKind,
        text: String,
        timestamp: Known<String>,
        tool_name: Known<String>,
        evidence: RawUnitRef,
        frame_class: Option<FrameClass>,
    ) -> Result<(), AdapterError> {
        if text.is_empty() {
            return Ok(());
        }
        self.ensure_segment();
        let turn_idx = self.turns.len() as u64;
        let segment_id = self.segments.len().saturating_sub(1) as u32;
        self.capture_skill_markers(turn_idx, &text, &timestamp);
        self.turns.push(Turn {
            turn_idx,
            role,
            timestamp,
            kind,
            text_hash: sha256_hex(text.as_bytes()),
            text_chars: text.chars().count() as u64,
            text,
            tool_name,
            segment_id,
            raw_unit_refs: vec![evidence],
            frame_class,
        });
        Ok(())
    }

    /// Consume the throne's decision into the session model, same projection
    /// plumbing as the other adapters: role and lane come from the class.
    fn push_classified_frame(&mut self, frame: TransportFrame) -> Result<(), AdapterError> {
        let classified = frames::classify(&frame);
        let timestamp = classified.seal.seal_ts.clone();
        let evidence = classified.origin.evidence.clone();
        let role = classified.class.turn_role();
        let class_for_turn = Some(classified.class.clone());
        match classified.class {
            FrameClass::Human { .. }
            | FrameClass::EchoSeal { .. }
            | FrameClass::AssistantFinal
            | FrameClass::InterAgent { .. } => self.push_turn(
                role,
                classified
                    .turn_kind
                    .expect("speech and inter-agent classes have a turn lane"),
                classified.content,
                timestamp,
                Known::unknown(),
                evidence,
                class_for_turn,
            ),
            FrameClass::ShellAction { .. } => {
                // Kimi emits no shell envelopes on the user lane; the class
                // exists only because a `role=tool` text frame maps here.
                self.push_turn(
                    role,
                    TurnKind::ToolCall,
                    classified.content,
                    timestamp,
                    Known::unknown(),
                    evidence,
                    class_for_turn,
                )
            }
            // The compaction summary is referenced by the context epoch,
            // never re-emitted as speech.
            FrameClass::Inject {
                kind: crate::engine::frames::InjectKind::CompactionReplay,
            } => Ok(()),
            FrameClass::Inject { .. } | FrameClass::LineageMeta { .. } => self.push_turn(
                role,
                classified
                    .turn_kind
                    .expect("metadata classes have a turn kind"),
                classified.content,
                timestamp,
                Known::unknown(),
                evidence,
                class_for_turn,
            ),
        }
    }

    fn capture_skill_markers(&mut self, turn_idx: u64, text: &str, timestamp: &Known<String>) {
        for token in text.split_whitespace() {
            let trimmed = token.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '/' && c != '-' && c != '_' && c != '$'
            });
            let name = trimmed
                .strip_prefix("/vc-")
                .map(|v| format!("vc-{v}"))
                .or_else(|| trimmed.strip_prefix("$vc-").map(|v| format!("vc-{v}")))
                .or_else(|| trimmed.starts_with("vc-").then(|| trimmed.to_owned()));
            if let Some(skill_name) = name.filter(|name| name.len() > 3)
                && !self
                    .skills
                    .iter()
                    .any(|skill| skill.turn_idx == turn_idx && skill.skill_name == skill_name)
            {
                self.skills.push(SkillInvocation {
                    turn_idx,
                    skill_name,
                    payload_hash: sha256_hex(text.as_bytes()),
                    payload_bytes: text.len() as u64,
                    first_invoked_at: timestamp.clone(),
                });
            }
        }
    }

    fn ensure_segment(&mut self) {
        if self.segments.is_empty() {
            self.segments.push(SegmentDraft {
                started_at: self.segment_started_at.clone(),
                ended_at: Known::unknown(),
                start_turn: 0,
            });
        }
    }

    fn observe_skip(
        &mut self,
        unit: &ClassifiedUnit,
        reason: SkippedReason,
        visible: bool,
        boundary: UnitBoundary,
    ) {
        let kind = match reason {
            SkippedReason::UnknownPayloadType => WarningKind::UnknownPayloadType,
            SkippedReason::Malformed => WarningKind::MalformedUnit,
            SkippedReason::Oversized => WarningKind::OversizedUnit,
            SkippedReason::EncryptedOpaque => WarningKind::OpaqueReasoning,
            SkippedReason::Unsupported => WarningKind::UnsupportedVisibleEvent,
            SkippedReason::CompactionReplay | SkippedReason::DuplicateBody => return,
        };
        self.warn(kind, unit.ordinal);
        if boundary == UnitBoundary::UnterminatedTail {
            self.malformed_tail = true;
            self.visible_lost |= visible;
            self.warn(WarningKind::UnterminatedTail, unit.ordinal);
        }
        if matches!(reason, SkippedReason::Malformed | SkippedReason::Oversized) && visible {
            self.visible_lost = true;
        }
        if visible
            && matches!(
                reason,
                SkippedReason::UnknownPayloadType | SkippedReason::Unsupported
            )
        {
            self.unsupported_visible = true;
        }
    }

    fn warn(&mut self, kind: WarningKind, ordinal: u64) {
        if let Some(warning) = self
            .warnings
            .iter_mut()
            .find(|warning| warning.kind == kind)
        {
            warning.count += 1;
            warning.first_ordinal = warning.first_ordinal.min(ordinal);
        } else {
            self.warnings.push(CoverageWarning {
                kind,
                count: 1,
                first_ordinal: ordinal,
            });
        }
    }

    fn finish(mut self, classified: Vec<ClassifiedUnit>) -> Result<UnvalidatedParse, AdapterError> {
        if let Some(last) = self.segments.last_mut() {
            last.ended_at = self.ended_at.clone();
        }
        let mut consumed = Vec::new();
        let mut skipped = Vec::new();
        for unit in classified {
            match unit.disposition {
                ClassifiedDisposition::Consumed { kind } => consumed.push(ConsumedUnit {
                    ordinal: unit.ordinal,
                    kind,
                    evidence: unit.evidence,
                }),
                ClassifiedDisposition::Skipped { reason, visible } => skipped.push(SkippedUnit {
                    ordinal: unit.ordinal,
                    bytes: unit.evidence.original_bytes,
                    reason,
                    visible,
                    evidence: unit.evidence,
                }),
            }
        }
        self.warnings.sort_by_key(|warning| warning.first_ordinal);
        let warnings = self.warnings;
        let status = ParseStatus {
            visible_completeness: if self.visible_lost || self.malformed_tail {
                VisibleCompleteness::PartialVisible
            } else {
                VisibleCompleteness::CompleteVisible
            },
            boundary_flags: BoundaryFlags {
                opaque_reasoning_present: false,
                unsupported_visible_event: self.unsupported_visible,
                compaction_boundary_present: self.compaction_boundary_present,
            },
            malformed_tail_present: self.malformed_tail,
            visible_event_lost: self.visible_lost,
        };
        let coverage = CoverageReport::with_raw_line_count(
            self.read.units.len() as u64,
            consumed.len() as u64 + skipped.len() as u64,
            consumed,
            skipped,
            warnings,
            status,
        );
        let provenance = Provenance {
            agent: AgentKind::Kimi,
            model: Known::unknown(),
            cli_version: Known::unknown(),
            cwd: Known::unknown(),
            branch: Known::unknown(),
            started_at: self.started_at,
            ended_at: self.ended_at,
            original_source_hash: self.read.source_hash.clone(),
            original_source_bytes: self.read.source_bytes,
        };
        let mut model = SessionModel::new(self.session_id.clone(), provenance, coverage);
        model.conversation = ProviderConversationRef::Kimi {
            session_id: self.session_id,
            agent_id: self.agent_id,
            unobserved: Vec::new(),
        };
        // Epochs learn their first following turn once the turn stream is
        // final: the boundary's own record carries no turn.
        let mut context_epochs = Vec::with_capacity(self.context_epochs.len());
        for (mut epoch, boundary_ordinal) in self.context_epochs {
            epoch.first_turn_after = self
                .turns
                .iter()
                .find(|turn| {
                    turn.raw_unit_refs
                        .iter()
                        .any(|reference| reference.physical_ordinal > boundary_ordinal)
                })
                .map(|turn| turn.turn_idx);
            context_epochs.push(epoch);
        }
        model.context_epochs = context_epochs;
        model.turns = self.turns;
        model.tool_events = self.tools;
        model.skill_invocations = self.skills;
        if !model.turns.is_empty() {
            model.segments = self
                .segments
                .into_iter()
                .enumerate()
                .filter_map(|(id, segment)| {
                    let end = model.turns.len() as u64 - 1;
                    (segment.start_turn <= end).then_some(Segment {
                        segment_id: id as u32,
                        cwd: Known::unknown(),
                        branch: Known::unknown(),
                        started_at: segment.started_at,
                        ended_at: segment.ended_at,
                        turn_range: TurnRange {
                            start: segment.start_turn,
                            end,
                        },
                        scope_status: crate::engine::ScopeStatus::from_evidence(None, None),
                        // Only the Codex adapter observes explicit tool-call workdirs.
                        scope_conflict: false,
                    })
                })
                .collect();
        }
        Ok(UnvalidatedParse::from_model(model))
    }
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str()
}

fn text_at(value: &Value, path: &[&str]) -> String {
    string_at(value, path).unwrap_or("").to_owned()
}

fn known_string(value: Option<&str>) -> Known<String> {
    value
        .filter(|v| !v.is_empty())
        .map(|v| Known::value(v.to_owned()))
        .unwrap_or_else(Known::unknown)
}

fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default())
}

/// Kimi records stamp `time` as epoch milliseconds; a few lifecycle records
/// carry RFC 3339 strings (`finishedAt`, `abortedAt`) instead. Normalize
/// both onto RFC 3339 so downstream surfaces see one timestamp shape.
fn record_timestamp(record: &Value) -> Known<String> {
    for key in ["time", "created_at"] {
        if let Some(millis) = record.get(key).and_then(Value::as_u64)
            && let Some(stamp) = chrono::DateTime::from_timestamp_millis(millis as i64)
        {
            return Known::value(stamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        }
    }
    for key in ["finishedAt", "abortedAt"] {
        if let Some(stamp) = record.get(key).and_then(Value::as_str)
            && !stamp.is_empty()
        {
            return Known::value(stamp.to_owned());
        }
    }
    Known::unknown()
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .or_else(|| item.get("text").and_then(Value::as_str).map(str::to_owned))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => value_text(content),
    }
}

/// A tool result carries `result.output` inline; oversized results (>50k
/// chars) may be spilled to `output_path` with no inline body. Name the
/// spill location so the transcript says where the bytes went.
fn tool_result_body(event: &Value) -> String {
    let result = &event["result"];
    if let Some(output) = result.get("output") {
        return value_text(output);
    }
    for key in ["output_path", "outputPath"] {
        if let Some(path) = result.get(key).and_then(Value::as_str) {
            return format!("[tool output stored at {path}]");
        }
    }
    if result.is_null() {
        return String::new();
    }
    value_text(result)
}

fn text_frame(
    transport_kind: TransportKind,
    role: TransportRole,
    content: String,
    timestamp: Known<String>,
    evidence: RawUnitRef,
) -> TransportFrame {
    TransportFrame {
        agent: AgentKind::Kimi,
        transport_kind,
        timestamp,
        payload: TransportPayload::Text { role, content },
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        RawUnitReader, ReaderPolicy, SourceArtifact, SourceFraming, ValidatedParse, validate_parse,
    };
    use std::collections::BTreeSet;

    fn parse(bytes: &[u8], id: &str) -> SessionModel {
        let source = SourceHandle::new(
            AgentKind::Kimi,
            id,
            Some(id.to_owned()),
            vec![
                SourceArtifact::memory("wire.jsonl", bytes.to_vec(), SourceFraming::JsonLines)
                    .unwrap(),
            ],
        )
        .unwrap();
        let read = RawUnitReader::new(ReaderPolicy::default())
            .read(&source)
            .unwrap();
        let adapter = KimiAdapter;
        let classified = adapter.classify(&source, &read).unwrap();
        let ValidatedParse::Session(parsed) = validate_parse(
            adapter
                .assemble(&source, &read, classified)
                .expect("Kimi assembly"),
        )
        .expect("Kimi kernel validation") else {
            panic!("session")
        };
        parsed.into_model()
    }

    #[test]
    fn minimal_oracle_models_core_shapes_and_coverage() {
        let bytes = include_bytes!("../../../../tests/fixtures/parser_engine/kimi/minimal.jsonl");
        let model = parse(bytes, "afee4590-3e31-42a6-b3d4-f2341ddf0726");
        assert_eq!(model.provenance.agent, AgentKind::Kimi);
        assert_eq!(model.coverage.raw_line_count, 11);
        assert_eq!(model.coverage.consumed_count, 8);
        assert_eq!(model.coverage.skipped_count, 3);
        assert_eq!(model.turns.len(), 5);
        assert_eq!(model.turns[0].role, TurnRole::User);
        assert_eq!(model.turns[0].text, "Zbuduj parser kimi.");
        assert_eq!(model.turns[1].kind, TurnKind::InternalThought);
        assert_eq!(model.turns[2].role, TurnRole::Assistant);
        assert_eq!(model.tool_events.len(), 2);
        assert_eq!(model.tool_events[0].kind, ToolEventKind::Call);
        assert_eq!(model.tool_events[0].tool_name, "Bash");
        assert_eq!(model.tool_events[1].kind, ToolEventKind::Result);
        assert!(
            model
                .coverage
                .status
                .boundary_flags
                .compaction_boundary_present
        );
        assert_eq!(model.context_epochs.len(), 1);
        assert_eq!(model.context_epochs[0].replacement_refs.len(), 1);
        assert_eq!(
            model.coverage.status.visible_completeness,
            VisibleCompleteness::CompleteVisible
        );
        // Bookkeeping is a deliberate non-visible skip, never a coverage gap.
        assert!(
            !model
                .coverage
                .status
                .boundary_flags
                .unsupported_visible_event
        );
        assert_eq!(
            model.conversation.agent(),
            AgentKind::Kimi,
            "conversation ref keeps provider identity"
        );
    }

    #[test]
    fn unknown_records_are_visible_gaps_bookkeeping_is_not() {
        let bytes = br#"{"type":"brand_new_envelope","agentId":"main","time":1789296071162}
{"type":"llm.request","agentId":"main","model":"k3-256k","time":1789296071163}
{"type":"context.undo","agentId":"main","count":2,"time":1789296071164}
{"type":"context.clear","agentId":"main","time":1789296071165}
{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"ping"}]},"time":1789296071166}
not-json-at-all
"#;
        let model = parse(bytes, "s-kimi-gaps");
        assert!(
            model
                .coverage
                .status
                .boundary_flags
                .unsupported_visible_event
        );
        assert_eq!(model.turns.len(), 1);
        assert_eq!(model.turns[0].text, "ping");
        let skipped: BTreeSet<_> = model
            .coverage
            .skipped
            .iter()
            .map(|unit| (unit.reason, unit.visible))
            .collect();
        assert!(skipped.contains(&(SkippedReason::UnknownPayloadType, true)));
        assert!(skipped.contains(&(SkippedReason::Unsupported, false)));
        assert!(skipped.contains(&(SkippedReason::Malformed, true)));
        assert_eq!(
            model.coverage.status.visible_completeness,
            VisibleCompleteness::PartialVisible
        );
    }

    #[test]
    fn adapter_contains_no_discovery_or_subprocess_path() {
        let source = include_str!("kimi.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production adapter prefix");
        for forbidden in [
            "read_dir",
            "walkdir",
            "glob(",
            "Command::new",
            "std::process",
            ".kimi-code/sessions",
        ] {
            assert!(
                !source.contains(forbidden),
                "Kimi adapter must accept an explicit SourceHandle, found {forbidden}"
            );
        }
    }
}
