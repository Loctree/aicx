//! Codex rollout adapter for the deterministic parser kernel.
//!
//! This module consumes exactly the [`SourceHandle`] supplied by the caller.
//! It never discovers sessions, reads siblings, or consults process state.

// C5X owns the shared module export. Until that dispatch cut lands this sealed
// implementation is intentionally private and would otherwise trip dead-code.
#![allow(dead_code)]

use super::{
    AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel, sealed,
};
use crate::engine::frames::{
    self, FrameClass, InjectKind, TransportFrame, TransportKind, TransportPayload, TransportRole,
};
use crate::engine::frames_rules;
use crate::engine::{
    AgentKind, BoundaryFlags, ConsumedUnit, CounterSemantics, CoverageReport, CoverageWarning,
    Known, ParseStatus, Provenance, RawUnitRef, ReportedCost, Segment, SessionModel,
    SkillInvocation, SkippedReason, SkippedUnit, SourceHandle, SourceRead, TokenComponents,
    ToolEvent, ToolEventKind, Turn, TurnKind, TurnRange, TurnRole, UnitBoundary, UnvalidatedParse,
    UsageEvent, VisibleCompleteness, WarningKind, evidence_event_id_from_hash, ordinal_locator,
    sha256_hex,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const CODEX_ADAPTER_VERSION: &str = "codex-rollout-v3";

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexAdapter;

impl sealed::Sealed for CodexAdapter {}

impl AgentAdapter for CodexAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn adapter_version(&self) -> &'static str {
        CODEX_ADAPTER_VERSION
    }

    fn classify(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
    ) -> Result<Vec<ClassifiedUnit>, AdapterError> {
        if source.artifacts().len() != 1 {
            return Err(AdapterError::new(
                "classify",
                "a Codex rollout is exactly one explicit JSONL artifact",
            ));
        }
        if source.artifacts()[0].framing() != crate::engine::SourceFraming::JsonLines {
            return Err(AdapterError::new(
                "classify",
                "Codex rollout artifacts must use json_lines framing",
            ));
        }
        let session_id = source
            .logical_session_id()
            .unwrap_or_else(|| source.source_id());
        let mut classified = read
            .units
            .iter()
            .map(|raw| classify_raw(source, session_id, raw))
            .collect::<Result<Vec<_>, _>>()?;
        let mut next_ordinal = classified.len() as u64 + 1;
        let mut seen_replay_hashes = BTreeSet::new();
        for raw in &read.units {
            if raw.boundary == UnitBoundary::Oversized {
                continue;
            }
            let Ok(value) = serde_json::from_slice::<Value>(&raw.bytes) else {
                continue;
            };
            if let Some(logical) = classify_logical(source, session_id, raw, &value, next_ordinal)?
            {
                classified.push(logical);
                next_ordinal += 1;
            }
            let replay = classify_compaction_replays(
                source,
                session_id,
                raw,
                &value,
                next_ordinal,
                &mut seen_replay_hashes,
            )?;
            next_ordinal += replay.len() as u64;
            classified.extend(replay);
        }
        Ok(classified)
    }

    fn assemble(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
        classified: Vec<ClassifiedUnit>,
    ) -> Result<UnvalidatedParse, AdapterError> {
        assemble_codex(source, read, classified)
    }
}

fn classify_logical(
    source: &SourceHandle,
    session_id: &str,
    raw: &crate::engine::RawUnit,
    event: &Value,
    ordinal: u64,
) -> Result<Option<ClassifiedUnit>, AdapterError> {
    let event_type = string_at(event, &["type"]).unwrap_or("");
    let payload = &event["payload"];
    let payload_type = string_at(payload, &["type"]).unwrap_or("");
    let (unit_kind, disposition) = match (event_type, payload_type) {
        ("event_msg", "token_count") => (
            "token_count",
            ClassifiedDisposition::Consumed {
                kind: "token_count".to_owned(),
            },
        ),
        ("response_item", "message") => (
            "message",
            ClassifiedDisposition::Consumed {
                kind: "message".to_owned(),
            },
        ),
        ("response_item", "agent_message") => (
            "agent_message",
            ClassifiedDisposition::Consumed {
                kind: "agent_message".to_owned(),
            },
        ),
        ("response_item", "function_call") => (
            "function_call",
            ClassifiedDisposition::Consumed {
                kind: "function_call".to_owned(),
            },
        ),
        ("response_item", "function_call_output") => (
            "function_call_output",
            ClassifiedDisposition::Consumed {
                kind: "function_call_output".to_owned(),
            },
        ),
        ("response_item", "encrypted_reasoning") => (
            "encrypted_reasoning",
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::EncryptedOpaque,
                visible: false,
            },
        ),
        ("response_item", "reasoning") if payload.get("encrypted_content").is_some() => (
            "encrypted_reasoning",
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::EncryptedOpaque,
                visible: false,
            },
        ),
        ("response_item", "reasoning") => (
            "reasoning",
            ClassifiedDisposition::Consumed {
                kind: "reasoning".to_owned(),
            },
        ),
        _ => return Ok(None),
    };
    let bytes = serde_json::to_vec(payload)
        .map_err(|error| AdapterError::new("classify", error.to_string()))?;
    let content_hash = sha256_hex(&bytes);
    let locator = format!("{}:payload", ordinal_locator(raw.physical_ordinal));
    let evidence_event_id = evidence_event_id_from_hash(
        source.agent(),
        session_id,
        &locator,
        unit_kind,
        &content_hash,
    )
    .map_err(|error| AdapterError::new("classify", error.to_string()))?;
    Ok(Some(ClassifiedUnit {
        ordinal,
        level: RawUnitLevel::Logical {
            parent_ordinal: raw.coverage_ordinal,
        },
        evidence: RawUnitRef {
            evidence_event_id,
            coverage_ordinal: ordinal,
            physical_ordinal: raw.physical_ordinal,
            locator,
            unit_kind: unit_kind.to_owned(),
            artifact: raw.artifact_name.clone(),
            content_hash,
            original_bytes: bytes.len() as u64,
        },
        disposition,
    }))
}

fn classify_compaction_replays(
    source: &SourceHandle,
    session_id: &str,
    raw: &crate::engine::RawUnit,
    event: &Value,
    first_ordinal: u64,
    seen_hashes: &mut BTreeSet<String>,
) -> Result<Vec<ClassifiedUnit>, AdapterError> {
    let payload = event.get("payload").unwrap_or(event);
    let is_compaction = string_at(event, &["type"]) == Some("compacted")
        || (string_at(event, &["type"]) == Some("response_item")
            && string_at(payload, &["type"]) == Some("compacted"));
    if !is_compaction {
        return Ok(Vec::new());
    }
    let Some(history) = payload.get("replacement_history").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    history
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let bytes = serde_json::to_vec(item)
                .map_err(|error| AdapterError::new("classify", error.to_string()))?;
            let content_hash = sha256_hex(&bytes);
            let reason = if seen_hashes.insert(content_hash.clone()) {
                SkippedReason::CompactionReplay
            } else {
                SkippedReason::DuplicateBody
            };
            let unit_kind = reason.as_str();
            let ordinal = first_ordinal + index as u64;
            let locator = format!(
                "{}:payload.replacement_history[{index}]",
                ordinal_locator(raw.physical_ordinal)
            );
            let evidence_event_id = evidence_event_id_from_hash(
                source.agent(),
                session_id,
                &locator,
                unit_kind,
                &content_hash,
            )
            .map_err(|error| AdapterError::new("classify", error.to_string()))?;
            Ok(ClassifiedUnit {
                ordinal,
                level: RawUnitLevel::Logical {
                    parent_ordinal: raw.coverage_ordinal,
                },
                evidence: RawUnitRef {
                    evidence_event_id,
                    coverage_ordinal: ordinal,
                    physical_ordinal: raw.physical_ordinal,
                    locator,
                    unit_kind: unit_kind.to_owned(),
                    artifact: raw.artifact_name.clone(),
                    content_hash,
                    original_bytes: bytes.len() as u64,
                },
                disposition: ClassifiedDisposition::Skipped {
                    reason,
                    visible: false,
                },
            })
        })
        .collect()
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
        let event_type = string_at(&value, &["type"]).unwrap_or("");
        match event_type {
            "session_meta" | "turn_context" | "event_msg" | "response_item" | "compacted" => (
                event_type.to_owned(),
                ClassifiedDisposition::Consumed {
                    kind: event_type.to_owned(),
                },
            ),
            // Envelopes Codex started emitting on 2026-07-11 that carry
            // session state rather than conversation: `world_state` repeats
            // the whole AGENTS.md, and the inter-agent record is a routing
            // flag. Known and deliberately dropped — not a coverage gap, so
            // they must not inflate the visible-skip count the way an
            // genuinely unrecognized payload should.
            "world_state" | "inter_agent_communication_metadata" => (
                event_type.to_owned(),
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

fn assemble_codex(
    source: &SourceHandle,
    read: &SourceRead,
    classified: Vec<ClassifiedUnit>,
) -> Result<UnvalidatedParse, AdapterError> {
    let mut state = Assembly::new(source, read);
    // TB parity: when any response_item.message (user/assistant) exists, the
    // matching event_msg.user_message / agent_message units are footprint-only.
    state.have_response_item_chat = read.units.iter().any(|raw| {
        let Ok(value) = serde_json::from_slice::<Value>(&raw.bytes) else {
            return false;
        };
        string_at(&value, &["type"]) == Some("response_item")
            && string_at(&value, &["payload", "type"]) == Some("message")
            && matches!(
                string_at(&value, &["payload", "role"]),
                Some("user") | Some("assistant")
            )
    });
    state.have_response_item_agent_message = read.units.iter().any(|raw| {
        let Ok(value) = serde_json::from_slice::<Value>(&raw.bytes) else {
            return false;
        };
        string_at(&value, &["type"]) == Some("response_item")
            && string_at(&value, &["payload", "type"]) == Some("agent_message")
    });
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
    model: Known<String>,
    cli_version: Known<String>,
    cwd: Known<String>,
    branch: Known<String>,
    started_at: Known<String>,
    ended_at: Known<String>,
    current_cwd: Known<String>,
    current_branch: Known<String>,
    segment_started_at: Known<String>,
    segments: Vec<SegmentDraft>,
    turns: Vec<Turn>,
    tools: Vec<ToolEvent>,
    tool_names: BTreeMap<String, String>,
    usage: Vec<UsageEvent>,
    skills: Vec<SkillInvocation>,
    warnings: Vec<CoverageWarning>,
    opaque_reasoning: bool,
    unsupported_visible: bool,
    malformed_tail: bool,
    visible_lost: bool,
    session_meta_seen: bool,
    /// True when the session contains response_item.message chat ownership.
    have_response_item_chat: bool,
    /// True when response_item.agent_message owns the inter-agent envelope.
    have_response_item_agent_message: bool,
    /// At least one compaction marker was consumed (TB context_compacted class).
    compaction_boundary_present: bool,
    /// Content identity for sticky compaction history; transport ids rotate.
    seen_replay_hashes: BTreeSet<String>,
}

#[derive(Clone)]
struct SegmentDraft {
    cwd: Known<String>,
    branch: Known<String>,
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
            model: Known::unknown(),
            cli_version: Known::unknown(),
            cwd: Known::unknown(),
            branch: Known::unknown(),
            started_at: Known::unknown(),
            ended_at: Known::unknown(),
            current_cwd: Known::unknown(),
            current_branch: Known::unknown(),
            segment_started_at: Known::unknown(),
            segments: Vec::new(),
            turns: Vec::new(),
            tools: Vec::new(),
            tool_names: BTreeMap::new(),
            usage: Vec::new(),
            skills: Vec::new(),
            warnings: Vec::new(),
            opaque_reasoning: false,
            unsupported_visible: false,
            malformed_tail: false,
            visible_lost: false,
            session_meta_seen: false,
            have_response_item_chat: false,
            have_response_item_agent_message: false,
            compaction_boundary_present: false,
            seen_replay_hashes: BTreeSet::new(),
        }
    }

    fn consume(&mut self, event: &Value, evidence: RawUnitRef) -> Result<(), AdapterError> {
        let timestamp = known_string(string_at(event, &["timestamp"]));
        if matches!(self.started_at, Known::Unknown(_)) {
            self.started_at = timestamp.clone();
        }
        if matches!(timestamp, Known::Value(_)) {
            self.ended_at = timestamp.clone();
        }
        match string_at(event, &["type"]).unwrap_or("") {
            "session_meta" => self.session_meta(event),
            "turn_context" => self.turn_context(event, timestamp),
            "event_msg" => self.event_msg(event, timestamp, evidence),
            "response_item" => self.response_item(event, timestamp, evidence),
            // Top-level Codex compaction envelope (TB counts as context_compacted).
            "compacted" => {
                self.compaction_boundary_present = true;
                self.consume_compaction_replay(event, timestamp, evidence)
            }
            _ => Ok(()),
        }
    }

    fn session_meta(&mut self, event: &Value) -> Result<(), AdapterError> {
        if self.session_meta_seen {
            return Ok(());
        }
        self.session_meta_seen = true;
        // The resolved SourceHandle owns identity. Direct-file mode has no
        // catalog and may use a filename-derived physical id; an embedded
        // session_meta id is payload data, not authority to rewrite or reject
        // the already-selected source.
        self.cwd = known_string(string_at(event, &["payload", "cwd"]));
        self.current_cwd = self.cwd.clone();
        self.model = known_string(string_at(event, &["payload", "model"]));
        self.cli_version = known_string(string_at(event, &["payload", "cli_version"]));
        Ok(())
    }

    fn turn_context(
        &mut self,
        event: &Value,
        timestamp: Known<String>,
    ) -> Result<(), AdapterError> {
        let cwd = known_string(string_at(event, &["payload", "cwd"]));
        let branch = known_string(string_at(event, &["payload", "branch"]));
        let model = known_string(string_at(event, &["payload", "model"]));
        if matches!(model, Known::Value(_)) {
            self.model = model;
        }
        if cwd != self.current_cwd || branch != self.current_branch {
            self.close_segment(timestamp.clone());
            self.current_cwd = cwd;
            self.current_branch = branch;
            self.segment_started_at = timestamp;
            if !self.turns.is_empty() {
                self.segments.push(SegmentDraft {
                    cwd: self.current_cwd.clone(),
                    branch: self.current_branch.clone(),
                    started_at: self.segment_started_at.clone(),
                    ended_at: Known::unknown(),
                    start_turn: self.turns.len() as u64,
                });
            }
        }
        Ok(())
    }

    fn event_msg(
        &mut self,
        event: &Value,
        timestamp: Known<String>,
        evidence: RawUnitRef,
    ) -> Result<(), AdapterError> {
        let payload_type = string_at(event, &["payload", "type"]).unwrap_or("");
        match payload_type {
            // Compaction marker: consumed, no visible turn, not "unsupported".
            "context_compacted" => {
                self.compaction_boundary_present = true;
                Ok(())
            }
            // Dual-envelope: response_item.message owns chat; event_msg is footprint.
            "user_message" if self.have_response_item_chat => Ok(()),
            "agent_message"
                if self.have_response_item_chat || self.have_response_item_agent_message =>
            {
                Ok(())
            }
            "user_message" => self.push_classified_frame(text_frame(
                TransportKind::DirectMessage,
                TransportRole::User,
                text_at(event, &["payload", "message"]),
                timestamp,
                evidence,
            )),
            "agent_message" => self.push_classified_frame(text_frame(
                TransportKind::AssistantMessage,
                TransportRole::Assistant,
                text_at(event, &["payload", "message"]),
                timestamp,
                evidence,
            )),
            "agent_reasoning" | "thinking" | "thinking_delta" => self.push_turn(
                TurnRole::Assistant,
                TurnKind::InternalThought,
                first_text(
                    event,
                    &[
                        ["payload", "text"].as_slice(),
                        ["payload", "message"].as_slice(),
                    ],
                ),
                timestamp,
                Known::unknown(),
                evidence,
            ),
            "token_count" => {
                self.push_usage(event, timestamp, evidence);
                Ok(())
            }
            "function_call" | "tool_call" | "mcp_tool_call" => {
                self.push_tool_turn(event, timestamp, evidence, ToolEventKind::Call)
            }
            // `mcp_tool_call_end` / `web_search_end` are the current names for
            // what this adapter first learned as `mcp_tool_call_response` /
            // `web_search_complete`. Both spellings stay accepted so old
            // archives keep parsing.
            "tool_result" | "mcp_tool_call_response" | "mcp_tool_call_end" => {
                self.push_tool_turn(event, timestamp, evidence, ToolEventKind::Result)
            }
            // Codex applies edits through this event; the payload carries the
            // whole stdout and per-file change bodies, so the turn records the
            // outcome and the files, not the diff.
            "patch_apply_end" => self.push_turn(
                TurnRole::System,
                TurnKind::SystemNote,
                patch_apply_summary(&event["payload"]),
                timestamp,
                Known::unknown(),
                evidence,
            ),
            // Codex fans out to subagents now. Without this the transcript of
            // a session that dispatched a dozen of them reads as one agent
            // working alone.
            "sub_agent_activity" => self.push_turn(
                TurnRole::System,
                TurnKind::SystemNote,
                sub_agent_summary(&event["payload"]),
                timestamp,
                Known::unknown(),
                evidence,
            ),
            "thread_goal_updated" => self.push_turn(
                TurnRole::System,
                TurnKind::SystemNote,
                thread_goal_summary(&event["payload"]),
                timestamp,
                Known::unknown(),
                evidence,
            ),
            // An interrupted turn is operator behaviour worth keeping.
            "turn_aborted" => self.push_turn(
                TurnRole::System,
                TurnKind::SystemNote,
                format!(
                    "turn aborted: {}",
                    string_at(&event["payload"], &["reason"]).unwrap_or("unknown reason")
                ),
                timestamp,
                Known::unknown(),
                evidence,
            ),
            // Thread configuration echo — consumed, carries no conversation.
            "thread_settings_applied" => Ok(()),
            "task_started"
            | "task_complete"
            | "error"
            | "notification"
            | "web_search"
            | "web_search_complete"
            | "web_search_end" => self.push_turn(
                TurnRole::System,
                TurnKind::SystemNote,
                payload_text(&event["payload"]),
                timestamp,
                Known::unknown(),
                evidence,
            ),
            _ => {
                self.unsupported_visible = true;
                self.warn(
                    WarningKind::UnsupportedVisibleEvent,
                    evidence.coverage_ordinal,
                );
                Ok(())
            }
        }
    }

    fn response_item(
        &mut self,
        event: &Value,
        timestamp: Known<String>,
        evidence: RawUnitRef,
    ) -> Result<(), AdapterError> {
        let payload = &event["payload"];
        match string_at(payload, &["type"]).unwrap_or("") {
            "message" => {
                let role = match string_at(payload, &["role"]).unwrap_or("system") {
                    "user" => TransportRole::User,
                    "assistant" => TransportRole::Assistant,
                    "tool" => TransportRole::Tool,
                    _ => TransportRole::System,
                };
                let text = content_text(&payload["content"]);
                // The 2026-08 Codex schema change began serializing user-run
                // shell actions and internal control payloads as ordinary
                // `role=user` input_text messages. The schema says `user`; the
                // product semantics say tool/control event, and the adapter
                // owns that normalization — see
                // `push_user_message_frames`.
                if role == TransportRole::User {
                    return self.push_user_message_frames(text, timestamp, evidence);
                }
                let kind = if role == TransportRole::Assistant {
                    TransportKind::AssistantMessage
                } else {
                    TransportKind::DirectMessage
                };
                self.push_classified_frame(text_frame(kind, role, text, timestamp, evidence))
            }
            // Agent-to-agent dispatch: `author` hands a task to `recipient`.
            // A third chat envelope, distinct from `message`, so the
            // dual-envelope suppression above does not apply and this is the
            // only place the payload survives.
            "agent_message" => {
                let content = content_text(&payload["content"]);
                let frame = TransportFrame {
                    agent: AgentKind::Codex,
                    transport_kind: TransportKind::AgentMessage,
                    timestamp,
                    payload: TransportPayload::InterAgent {
                        sender: labeled_field(&content, "Sender")
                            .or_else(|| string_at(payload, &["author"]).map(str::to_owned))
                            .unwrap_or_else(|| "unknown".to_owned()),
                        task: labeled_field(&content, "Task name")
                            .or_else(|| string_at(payload, &["recipient"]).map(str::to_owned))
                            .unwrap_or_else(|| "unknown".to_owned()),
                        message_type: labeled_field(&content, "Message Type")
                            .unwrap_or_else(|| "unknown".to_owned()),
                        content,
                    },
                    evidence,
                };
                self.push_classified_frame(frame)
            }
            "function_call" | "custom_tool_call" | "web_search_call" => {
                self.push_tool_turn(event, timestamp, evidence, ToolEventKind::Call)
            }
            "function_call_output" | "custom_tool_call_output" => {
                self.push_tool_turn(event, timestamp, evidence, ToolEventKind::Result)
            }
            "reasoning" if payload.get("encrypted_content").is_some() => {
                // A modern Codex reasoning item may carry both an encrypted
                // body and a visible summary. The logical classifier marks
                // the opaque unit as EncryptedOpaque in both cases, so the
                // boundary flag must be set regardless of summary shape.
                self.opaque_reasoning = true;
                self.warn(WarningKind::OpaqueReasoning, evidence.coverage_ordinal);
                let visible = reasoning_text_visible(payload);
                if visible.is_empty() {
                    Ok(())
                } else {
                    self.push_turn(
                        TurnRole::Assistant,
                        TurnKind::InternalThought,
                        visible,
                        timestamp,
                        Known::unknown(),
                        evidence,
                    )
                }
            }
            "reasoning" => self.push_turn(
                TurnRole::Assistant,
                TurnKind::InternalThought,
                reasoning_text_visible(payload),
                timestamp,
                Known::unknown(),
                evidence,
            ),
            "encrypted_reasoning" => {
                self.opaque_reasoning = true;
                self.warn(WarningKind::OpaqueReasoning, evidence.coverage_ordinal);
                Ok(())
            }
            // response_item.compacted — TB footprint/context_compacted class.
            "compacted" => {
                self.compaction_boundary_present = true;
                self.consume_compaction_replay(payload, timestamp, evidence)
            }
            _ => {
                self.unsupported_visible = true;
                self.warn(
                    WarningKind::UnsupportedVisibleEvent,
                    evidence.coverage_ordinal,
                );
                Ok(())
            }
        }
    }

    fn push_turn(
        &mut self,
        role: TurnRole,
        kind: TurnKind,
        text: String,
        timestamp: Known<String>,
        tool_name: Known<String>,
        evidence: RawUnitRef,
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
        });
        Ok(())
    }

    /// Consume the throne's decision into the pre-taxonomy session model.
    /// This is projection plumbing, not a second content classifier.
    fn push_classified_frame(&mut self, frame: TransportFrame) -> Result<(), AdapterError> {
        let classified = frames::classify(&frame);
        let timestamp = classified.seal.seal_ts.clone();
        let evidence = classified.origin.evidence.clone();
        match classified.class {
            FrameClass::Human { .. } | FrameClass::EchoSeal { .. } => self.push_turn(
                TurnRole::User,
                classified
                    .turn_kind
                    .expect("human classes have a turn kind"),
                classified.content,
                timestamp,
                Known::unknown(),
                evidence,
            ),
            FrameClass::AssistantFinal => self.push_turn(
                TurnRole::Assistant,
                classified
                    .turn_kind
                    .expect("assistant class has a turn kind"),
                classified.content,
                timestamp,
                Known::unknown(),
                evidence,
            ),
            FrameClass::ShellAction { cmd, result } => {
                let marker = shell_action_marker(&cmd);
                self.push_turn(
                    TurnRole::Tool,
                    TurnKind::ToolCall,
                    marker.clone(),
                    timestamp.clone(),
                    Known::value("user_shell_command".to_owned()),
                    evidence.clone(),
                )?;
                let call_idx = self.turns.len() as u64 - 1;
                self.tools.push(ToolEvent {
                    kind: ToolEventKind::Call,
                    turn_idx: call_idx,
                    tool_name: "user_shell_command".to_owned(),
                    correlation_id: Known::unknown(),
                    payload_hash: sha256_hex(marker.as_bytes()),
                    payload_bytes: marker.len() as u64,
                    raw_unit_refs: vec![evidence.clone()],
                });
                if !result.text.is_empty() {
                    self.push_turn(
                        TurnRole::Tool,
                        TurnKind::ToolResult,
                        result.text.clone(),
                        timestamp,
                        Known::value("user_shell_command".to_owned()),
                        evidence.clone(),
                    )?;
                    let result_idx = self.turns.len() as u64 - 1;
                    self.tools.push(ToolEvent {
                        kind: ToolEventKind::Result,
                        turn_idx: result_idx,
                        tool_name: "user_shell_command".to_owned(),
                        correlation_id: Known::unknown(),
                        payload_hash: result.hash,
                        payload_bytes: result.text.len() as u64,
                        raw_unit_refs: vec![evidence],
                    });
                }
                Ok(())
            }
            FrameClass::Inject {
                kind: InjectKind::CompactionReplay,
            }
            | FrameClass::InterAgent { .. } => Ok(()),
            FrameClass::Inject { .. } | FrameClass::LineageMeta { .. } => self.push_turn(
                TurnRole::System,
                classified
                    .turn_kind
                    .expect("metadata classes have a turn kind"),
                classified.content,
                timestamp,
                Known::unknown(),
                evidence,
            ),
        }
    }

    fn consume_compaction_replay(
        &mut self,
        event_or_payload: &Value,
        timestamp: Known<String>,
        evidence: RawUnitRef,
    ) -> Result<(), AdapterError> {
        let payload = event_or_payload.get("payload").unwrap_or(event_or_payload);
        let Some(history) = payload.get("replacement_history").and_then(Value::as_array) else {
            return Ok(());
        };
        for item in history {
            let content = serde_json::to_string(item)
                .map_err(|error| AdapterError::new("assemble", error.to_string()))?;
            let frame = TransportFrame {
                agent: AgentKind::Codex,
                transport_kind: TransportKind::InjectedContext,
                timestamp: timestamp.clone(),
                payload: TransportPayload::Inject {
                    tag: "compacted.replacement_history".to_owned(),
                    content,
                },
                evidence: evidence.clone(),
            };
            let classified = frames::classify(&frame);
            if self.seen_replay_hashes.insert(classified.content_hash) {
                self.push_classified_frame(frame)?;
            }
        }
        Ok(())
    }

    fn push_tool_turn(
        &mut self,
        event: &Value,
        timestamp: Known<String>,
        evidence: RawUnitRef,
        kind: ToolEventKind,
    ) -> Result<(), AdapterError> {
        let payload = &event["payload"];
        let correlation_raw = string_at(payload, &["call_id"])
            .or_else(|| string_at(payload, &["id"]))
            .map(str::to_owned);
        let explicit_name = string_at(payload, &["name"])
            .or_else(|| string_at(payload, &["tool_name"]))
            .map(str::to_owned);
        let name = explicit_name
            .or_else(|| {
                correlation_raw
                    .as_ref()
                    .and_then(|call_id| self.tool_names.get(call_id).cloned())
            })
            .or_else(|| {
                (string_at(payload, &["type"]) == Some("web_search_call"))
                    .then(|| "web_search".to_owned())
            })
            .unwrap_or_else(|| "unknown_tool".to_owned());
        if kind == ToolEventKind::Call
            && let Some(call_id) = &correlation_raw
        {
            self.tool_names.insert(call_id.clone(), name.clone());
        }
        let correlation = known_string(correlation_raw.as_deref());
        let body = match kind {
            ToolEventKind::Call => payload.get("arguments").or_else(|| payload.get("input")),
            ToolEventKind::Result => payload.get("output"),
        }
        .map(value_text)
        .unwrap_or_else(|| payload_text(payload));
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

    /// Classify a `role=user` message body before it reaches the turn stream.
    ///
    /// A plain human message passes through byte-identical to the legacy
    /// path. A body carrying reserved Codex control envelopes (see
    /// [`match_envelope_open`]) is split: a plain `echo` envelope is an
    /// operator utterance sealed at the envelope timestamp (`UserMsg`);
    /// every other `<user_shell_command>` block becomes a `ToolCall` marker
    /// (`$ cmd`) followed by a `ToolResult` holding the whole envelope
    /// verbatim, each with a registered [`ToolEvent`]; control envelopes
    /// (`codex_internal_context`, `INSTRUCTIONS`, …) become
    /// `System`/`SystemNote` turns, and any genuine human prose around them
    /// survives as `UserMsg` turns in order. The substrate keeps the full
    /// result; projections filter on frame kind, so reclassified envelopes
    /// vanish from `--conversation` while full extraction retains them.
    fn push_user_message_frames(
        &mut self,
        text: String,
        timestamp: Known<String>,
        evidence: RawUnitRef,
    ) -> Result<(), AdapterError> {
        let Some(frames) = normalize_user_envelopes(&text, &timestamp, &evidence) else {
            // Fast path: no envelope anywhere — emit the original body
            // unchanged so ordinary messages keep byte-identical hashes.
            return self.push_classified_frame(text_frame(
                TransportKind::DirectMessage,
                TransportRole::User,
                text,
                timestamp,
                evidence,
            ));
        };
        for frame in frames {
            self.push_classified_frame(frame)?;
        }
        Ok(())
    }

    fn push_usage(&mut self, event: &Value, timestamp: Known<String>, evidence: RawUnitRef) {
        let info = &event["payload"]["info"];
        let model = string_at(info, &["model"])
            .or_else(|| string_at(event, &["payload", "model"]))
            .or_else(|| known_value(&self.model));
        let provider = string_at(info, &["provider"])
            .or_else(|| string_at(event, &["payload", "provider"]))
            .unwrap_or("openai");
        let cost = reported_cost(info);
        let mut emitted = false;

        for (field, semantics) in [
            ("total_token_usage", CounterSemantics::Cumulative),
            ("last_token_usage", CounterSemantics::Delta),
            ("token_usage", CounterSemantics::Snapshot),
        ] {
            if let Some(tokens) = info.get(field).filter(|value| value.is_object()) {
                self.usage.push(usage_event(
                    provider,
                    model,
                    tokens,
                    if emitted {
                        Known::unknown()
                    } else {
                        cost.clone()
                    },
                    timestamp.clone(),
                    semantics,
                    evidence.clone(),
                ));
                emitted = true;
            }
        }

        // Older snapshots flatten token components directly into `info`.
        if !emitted && has_usage_component(info) {
            self.usage.push(usage_event(
                provider,
                model,
                info,
                cost,
                timestamp,
                CounterSemantics::Snapshot,
                evidence,
            ));
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
                cwd: self.current_cwd.clone(),
                branch: self.current_branch.clone(),
                started_at: self.segment_started_at.clone(),
                ended_at: Known::unknown(),
                start_turn: 0,
            });
        }
    }

    fn close_segment(&mut self, ended_at: Known<String>) {
        if let Some(segment) = self.segments.last_mut() {
            segment.ended_at = ended_at;
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
                opaque_reasoning_present: self.opaque_reasoning,
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
            agent: AgentKind::Codex,
            model: self.model,
            cli_version: self.cli_version,
            cwd: self.cwd,
            branch: self.branch,
            started_at: self.started_at,
            ended_at: self.ended_at,
            original_source_hash: self.read.source_hash.clone(),
            original_source_bytes: self.read.source_bytes,
        };
        let mut model = SessionModel::new(self.session_id, provenance, coverage);
        model.turns = self.turns;
        model.tool_events = self.tools;
        model.usage_events = self.usage;
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
                        cwd: segment.cwd,
                        branch: segment.branch,
                        started_at: segment.started_at,
                        ended_at: segment.ended_at,
                        turn_range: TurnRange {
                            start: segment.start_turn,
                            end,
                        },
                    })
                })
                .collect();
            for index in 0..model.segments.len() {
                let next = model
                    .segments
                    .get(index + 1)
                    .map(|s| s.turn_range.start - 1)
                    .unwrap_or(model.turns.len() as u64 - 1);
                model.segments[index].turn_range.end = next;
            }
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
fn first_text(value: &Value, paths: &[&[&str]]) -> String {
    paths
        .iter()
        .find_map(|path| string_at(value, path))
        .unwrap_or("")
        .to_owned()
}
fn known_string(value: Option<&str>) -> Known<String> {
    value
        .filter(|v| !v.is_empty())
        .map(|v| Known::value(v.to_owned()))
        .unwrap_or_else(Known::unknown)
}
fn known_value(value: &Known<String>) -> Option<&str> {
    match value {
        Known::Value(value) => Some(value),
        Known::Unknown(_) => None,
    }
}
fn known_u64(value: Option<&Value>) -> Known<u64> {
    value
        .and_then(Value::as_u64)
        .map(Known::value)
        .unwrap_or_else(Known::unknown)
}
fn reported_cost(info: &Value) -> Known<ReportedCost> {
    match info.get("reported_cost").or_else(|| info.get("cost")) {
        Some(Value::Object(cost)) => match (
            cost.get("amount").and_then(Value::as_f64),
            cost.get("currency").and_then(Value::as_str),
        ) {
            (Some(amount), Some(currency)) => Known::value(ReportedCost {
                amount,
                currency: currency.to_owned(),
            }),
            _ => Known::unknown(),
        },
        Some(Value::Number(amount)) => amount.as_f64().map_or_else(Known::unknown, |amount| {
            Known::value(ReportedCost {
                amount,
                currency: string_at(info, &["currency"]).unwrap_or("USD").to_owned(),
            })
        }),
        _ => info
            .get("cost_usd")
            .and_then(Value::as_f64)
            .map_or_else(Known::unknown, |amount| {
                Known::value(ReportedCost {
                    amount,
                    currency: "USD".to_owned(),
                })
            }),
    }
}
fn has_usage_component(value: &Value) -> bool {
    [
        "input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
        "cached_input_tokens",
        "cache_creation_tokens",
    ]
    .iter()
    .any(|key| value.get(*key).and_then(Value::as_u64).is_some())
}
fn usage_event(
    provider: &str,
    model: Option<&str>,
    tokens: &Value,
    cost: Known<ReportedCost>,
    timestamp: Known<String>,
    counter_semantics: CounterSemantics,
    evidence: RawUnitRef,
) -> UsageEvent {
    UsageEvent {
        provider: provider.to_owned(),
        model: known_string(model),
        tokens: TokenComponents {
            input: known_u64(tokens.get("input_tokens")),
            output: known_u64(tokens.get("output_tokens")),
            reasoning: known_u64(tokens.get("reasoning_output_tokens")),
            cache_read: known_u64(tokens.get("cached_input_tokens")),
            cache_creation: known_u64(tokens.get("cache_creation_tokens")),
        },
        cost,
        timestamp,
        span: Known::unknown(),
        counter_semantics,
        evidence,
    }
}
fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default())
}
fn payload_text(payload: &Value) -> String {
    ["message", "text", "content", "error", "query", "result"]
        .iter()
        .find_map(|key| payload.get(*key))
        .map(value_text)
        .unwrap_or_else(|| value_text(payload))
}
/// `patch_apply_end` carries stdout plus a per-file change map whose bodies
/// are whole file contents. The transcript keeps the outcome and the paths;
/// the diff itself lives in the repository, not in a session record.
fn patch_apply_summary(payload: &Value) -> String {
    let outcome = match payload.get("success").and_then(Value::as_bool) {
        Some(true) => "ok",
        Some(false) => "failed",
        None => "unknown",
    };
    let mut summary = match payload.get("changes").and_then(Value::as_object) {
        Some(changes) if !changes.is_empty() => {
            let mut paths: Vec<&str> = changes.keys().map(String::as_str).collect();
            paths.sort_unstable();
            format!("patch apply {outcome}: {}", paths.join(", "))
        }
        _ => format!("patch apply {outcome}"),
    };
    if let Some(stderr) = payload.get("stderr").and_then(Value::as_str)
        && let Some(first) = stderr.lines().find(|line| !line.trim().is_empty())
    {
        summary.push_str(" — ");
        summary.push_str(first.trim());
    }
    summary
}

/// One line per subagent lifecycle event: enough to see the fan-out shape of
/// a session without carrying the child's own transcript.
fn sub_agent_summary(payload: &Value) -> String {
    let kind = string_at(payload, &["kind"]).unwrap_or("activity");
    match string_at(payload, &["agent_path"]) {
        Some(path) => format!("subagent {kind}: {path}"),
        None => format!("subagent {kind}"),
    }
}

/// The goal objective is operator intent stated in prose — the single most
/// retrievable thing in the payload.
fn thread_goal_summary(payload: &Value) -> String {
    match string_at(payload, &["goal", "objective"]) {
        Some(objective) => format!("thread goal: {objective}"),
        None => "thread goal updated".to_owned(),
    }
}

fn text_frame(
    transport_kind: TransportKind,
    role: TransportRole,
    content: String,
    timestamp: Known<String>,
    evidence: RawUnitRef,
) -> TransportFrame {
    TransportFrame {
        agent: AgentKind::Codex,
        transport_kind,
        timestamp,
        payload: TransportPayload::Text { role, content },
        evidence,
    }
}

fn labeled_field(content: &str, label: &str) -> Option<String> {
    content.lines().find_map(|line| {
        line.trim()
            .strip_prefix(label)
            .and_then(|tail| tail.strip_prefix(':'))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

/// Extract the command payload from a Codex `user_shell_command` envelope.
///
/// This parser is deliberately narrower than a shell parser: the Codex
/// envelope is the grammar boundary, and only a complete `<command>` element
/// is admitted. Malformed envelopes fail closed to a placeholder marker; the
/// envelope body itself is still retained as the tool result.
fn user_shell_command(body: &str) -> Option<&str> {
    let command = body.split_once("<command>")?.1;
    let command = command.split_once("</command>")?.0.trim();
    (!command.is_empty()).then_some(command)
}

fn shell_action_marker(command: &str) -> String {
    let first_line = command
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or("user_shell_command (malformed)");
    let continuation = command.lines().skip(1).any(|line| !line.trim().is_empty());
    format!("$ {first_line}{}", if continuation { " …" } else { "" })
}

/// Typed, whole-envelope opening-tag matcher.
///
/// Returns `(tag, is_tool, closes_on_same_line)` when the trimmed line is a
/// complete opening tag for a known envelope: `<tag>` or `<tag attrs…>`,
/// optionally terminated by the matching `</tag>` on the same line. A line
/// where the tag is followed by same-line prose is NOT an envelope — that is
/// a human mentioning the literal tag, and it stays in the conversation.
fn match_envelope_open(trimmed: &str) -> Option<(&'static str, bool, bool)> {
    if !trimmed.starts_with('<') {
        return None;
    }
    let candidates = std::iter::once(("user_shell_command", true)).chain(
        frames_rules::rules_for(AgentKind::Codex)
            .inject_tags
            .iter()
            .map(|rule| (rule.tag, false)),
    );
    for (tag, is_tool) in candidates {
        let after = match trimmed[1..].strip_prefix(tag) {
            Some(after) => after,
            None => continue,
        };
        // The character after the tag name must terminate or extend the tag
        // (`>` or whitespace before attributes) — `<user_shell_commandX>`
        // must not match.
        let attrs_ok = match after.chars().next() {
            Some('>') => true,
            Some(c) if c.is_whitespace() => true,
            _ => false,
        };
        if !attrs_ok {
            continue;
        }
        // The opening tag must close on this line.
        let Some(gt) = after.find('>') else { continue };
        let tail = after[gt + 1..].trim();
        if tail.is_empty() {
            return Some((tag, is_tool, false));
        }
        let close = format!("</{tag}>");
        if tail.ends_with(close.as_str()) {
            return Some((tag, is_tool, true));
        }
        // Opening tag followed by same-line prose: human mention, not an
        // envelope.
    }
    None
}

/// Split a `role=user` message body into prose and whole-envelope segments.
///
/// Line-anchored and fence-aware: an envelope starts only at a line that is a
/// complete opening tag (see [`match_envelope_open`]) outside a markdown code
/// fence, and runs through the line holding its matching close tag. Inline
/// mentions of the literal tag never match. An unterminated envelope runs to
/// the end of the message — fail closed: leaking a shell result is worse than
/// over-classifying a malformed envelope.
fn normalize_user_envelopes(
    text: &str,
    timestamp: &Known<String>,
    evidence: &RawUnitRef,
) -> Option<Vec<TransportFrame>> {
    fn push_envelope(
        output: &mut Vec<TransportFrame>,
        tag: &'static str,
        is_shell: bool,
        body: String,
        timestamp: &Known<String>,
        evidence: &RawUnitRef,
    ) {
        let payload = if is_shell {
            TransportPayload::Shell {
                command: user_shell_command(&body)
                    .unwrap_or("user_shell_command (malformed)")
                    .to_owned(),
                result: body,
            }
        } else {
            TransportPayload::Inject {
                tag: tag.to_owned(),
                content: body,
            }
        };
        output.push(TransportFrame {
            agent: AgentKind::Codex,
            transport_kind: if is_shell {
                TransportKind::UserShellCommand
            } else {
                TransportKind::InjectedContext
            },
            timestamp: timestamp.clone(),
            payload,
            evidence: evidence.clone(),
        });
    }

    let mut output = Vec::new();
    let mut prose = String::new();
    let mut open: Option<(&'static str, bool, String)> = None;
    let mut in_fence = false;
    let mut saw_envelope = false;
    for line in text.lines() {
        if let Some((tag, _, body)) = open.as_mut() {
            body.push_str(line);
            body.push('\n');
            if line.trim() == format!("</{tag}>") {
                let (tag, is_shell, body) = open.take().expect("open envelope");
                push_envelope(&mut output, tag, is_shell, body, timestamp, evidence);
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            prose.push_str(line);
            prose.push('\n');
            continue;
        }
        if !in_fence && let Some((tag, is_shell, closes_inline)) = match_envelope_open(trimmed) {
            saw_envelope = true;
            if !prose.trim().is_empty() {
                output.push(text_frame(
                    TransportKind::DirectMessage,
                    TransportRole::User,
                    std::mem::take(&mut prose).trim().to_owned(),
                    timestamp.clone(),
                    evidence.clone(),
                ));
            } else {
                prose.clear();
            }
            let mut body = String::from(line);
            body.push('\n');
            if closes_inline {
                push_envelope(&mut output, tag, is_shell, body, timestamp, evidence);
            } else {
                open = Some((tag, is_shell, body));
            }
            continue;
        }
        prose.push_str(line);
        prose.push('\n');
    }
    if let Some((tag, is_shell, body)) = open.take() {
        push_envelope(&mut output, tag, is_shell, body, timestamp, evidence);
    }
    if !prose.trim().is_empty() {
        output.push(text_frame(
            TransportKind::DirectMessage,
            TransportRole::User,
            prose.trim().to_owned(),
            timestamp.clone(),
            evidence.clone(),
        ));
    }
    saw_envelope.then_some(output)
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
fn reasoning_text_visible(payload: &Value) -> String {
    payload
        .get("summary")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        RawUnitReader, ReaderPolicy, SourceArtifact, SourceFraming, ValidatedParse, validate_parse,
    };
    use std::path::Path;
    use std::time::Instant;

    fn parse(bytes: &[u8], id: &str) -> SessionModel {
        let source = SourceHandle::new(
            AgentKind::Codex,
            id,
            Some(id.to_owned()),
            vec![
                SourceArtifact::memory("rollout.jsonl", bytes.to_vec(), SourceFraming::JsonLines)
                    .unwrap(),
            ],
        )
        .unwrap();
        let read = RawUnitReader::new(ReaderPolicy::default())
            .read(&source)
            .unwrap();
        let adapter = CodexAdapter;
        let classified = adapter.classify(&source, &read).unwrap();
        let ValidatedParse::Session(parsed) = validate_parse(
            adapter
                .assemble(&source, &read, classified)
                .expect("Codex assembly"),
        )
        .expect("Codex kernel validation") else {
            panic!("session")
        };
        parsed.into_model()
    }

    #[test]
    fn minimal_oracle_and_explicit_source() {
        let bytes = include_bytes!("../../../../tests/fixtures/parser_engine/codex/minimal.jsonl");
        let model = parse(bytes, "11111111-1111-4111-8111-111111111111");
        assert_eq!(model.turns.len(), 2);
        assert_eq!(model.turns[0].role, TurnRole::User);
        assert_eq!(model.coverage.raw_line_count, 4);
        assert_eq!(model.coverage.skipped_count, 0);
    }

    #[test]
    fn adapter_contains_no_discovery_or_subprocess_path() {
        let source = include_str!("codex.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production adapter prefix");
        for forbidden in [
            "read_dir",
            "walkdir",
            "glob(",
            "Command::new",
            "std::process",
            ".codex/sessions",
        ] {
            assert!(
                !source.contains(forbidden),
                "Codex adapter must accept an explicit SourceHandle, found {forbidden}"
            );
        }
    }

    #[test]
    fn usage_opaque_tools_segments_and_skips_are_typed() {
        let bytes = br#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"s1","cwd":"/a","model":"gpt-5"}}
{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"cwd":"/b","branch":"main"}}
{"timestamp":"2026-01-01T00:00:02Z","type":"response_item","payload":{"type":"function_call","name":"shell","call_id":"c1","arguments":"{}"}}
{"timestamp":"2026-01-01T00:00:03Z","type":"response_item","payload":{"type":"encrypted_reasoning","encrypted_content":"opaque"}}
{"timestamp":"2026-01-01T00:00:04Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":3,"reasoning_output_tokens":2}}}}
"#;
        let model = parse(bytes, "s1");
        assert_eq!(model.tool_events.len(), 1);
        assert_eq!(model.usage_events.len(), 1);
        assert!(
            model
                .coverage
                .status
                .boundary_flags
                .opaque_reasoning_present
        );
        assert_eq!(
            model.coverage.status.visible_completeness,
            VisibleCompleteness::CompleteVisible
        );
        assert_eq!(
            model.usage_events[0].counter_semantics,
            CounterSemantics::Cumulative
        );
    }

    #[test]
    fn evidence_ids_are_append_and_relocation_stable_and_mutation_scoped() {
        let base =
            include_bytes!("../../../../tests/fixtures/parser_engine/contract/identity_base.jsonl");
        let appended = include_bytes!(
            "../../../../tests/fixtures/parser_engine/contract/identity_append.jsonl"
        );
        let mutated = include_bytes!(
            "../../../../tests/fixtures/parser_engine/contract/identity_mutated.jsonl"
        );
        let ids = |bytes: &[u8]| {
            let model = parse(bytes, "0d3adbe6-1111-7000-8000-000000000001");
            let physical = model.coverage.raw_line_count;
            model
                .coverage
                .consumed
                .into_iter()
                .filter(|unit| unit.ordinal <= physical)
                .map(|u| u.evidence.evidence_event_id)
                .collect::<Vec<_>>()
        };
        let base_ids = ids(base);
        let append_ids = ids(appended);
        let mutation_ids = ids(mutated);
        assert_eq!(base_ids, append_ids[..base_ids.len()]);
        assert_eq!(
            base_ids
                .iter()
                .zip(mutation_ids.iter())
                .filter(|(a, b)| a != b)
                .count(),
            1
        );
        assert!(base_ids.iter().all(|id| !id.contains('/')));
    }

    #[test]
    #[ignore = "private operator fixture; run explicitly for C2 performance proof"]
    fn private_large_rollout_stays_below_two_seconds_and_stable() {
        let path = Path::new(
            "/Users/tester/.codex/sessions/2026/04/10/rollout-2026-04-10T17-42-48-019d780f-6763-7d40-a7f8-ab0c2313c576.jsonl",
        );
        let run = || {
            let artifact =
                SourceArtifact::validated_file("rollout.jsonl", path, SourceFraming::JsonLines)
                    .unwrap();
            let source = SourceHandle::new(
                AgentKind::Codex,
                "019d780f-6763-7d40-a7f8-ab0c2313c576",
                Some("019d780f-6763-7d40-a7f8-ab0c2313c576".to_owned()),
                vec![artifact],
            )
            .unwrap();
            let started = Instant::now();
            let read = RawUnitReader::new(ReaderPolicy::default())
                .read(&source)
                .expect("large rollout bounded read");
            let adapter = CodexAdapter;
            let classified = adapter
                .classify(&source, &read)
                .expect("large rollout classification");
            validate_parse(
                adapter
                    .assemble(&source, &read, classified)
                    .expect("large rollout assembly"),
            )
            .expect("large rollout validation");
            started.elapsed()
        };
        let _warmup = run();
        let first = run();
        let second = run();
        assert!(first.as_millis() <= 2_000, "first run: {first:?}");
        assert!(second.as_millis() <= 2_000, "second run: {second:?}");
        let slower = first.max(second).as_nanos() as f64;
        let faster = first.min(second).as_nanos() as f64;
        assert!(
            slower / faster <= 1.25,
            "run drift: {first:?} vs {second:?}"
        );
        eprintln!(
            "{{\"first_ms\":{},\"second_ms\":{},\"threshold_ms\":2000}}",
            first.as_millis(),
            second.as_millis()
        );
    }
}
