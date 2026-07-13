//! Junie adapter implementation for the deterministic parser kernel (C5).
//!
//! This adapter consumes only the explicit [`SourceHandle`] artifacts supplied by
//! the caller. Directory/session discovery stays outside the frozen parser
//! boundary. Junie `events.jsonl` contains physical event rows and streaming
//! agent block updates; block updates are also projected as deterministic logical
//! last-snapshot units so coverage records both the source events and the final
//! visible assistant/tool surface.

// C5X owns shared dispatch/export. Until that convergence cut wires the module,
// this implementation remains private and would otherwise trip dead-code gates.
#![allow(dead_code)]

use super::{
    AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel, sealed,
};
use crate::engine::{
    AgentKind, BoundaryFlags, ConsumedUnit, CoverageReport, CoverageWarning, Known, ParseStatus,
    Provenance, RawUnit, RawUnitRef, Segment, SessionModel, SkippedReason, SkippedUnit,
    SourceHandle, SourceRead, ToolEvent, ToolEventKind, Turn, TurnKind, TurnRange, TurnRole,
    UnitBoundary, UnvalidatedParse, VisibleCompleteness, WarningKind, evidence_event_id_from_hash,
    ordinal_locator, sha256_hex,
};
use serde_json::Value;
use std::collections::BTreeMap;

pub const JUNIE_ADAPTER_VERSION: &str = "junie-native-v1";

#[derive(Debug, Clone, Copy, Default)]
pub struct JunieAdapter;

impl sealed::Sealed for JunieAdapter {}

impl AgentAdapter for JunieAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Junie
    }

    fn adapter_version(&self) -> &'static str {
        JUNIE_ADAPTER_VERSION
    }

    fn classify(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
    ) -> Result<Vec<ClassifiedUnit>, AdapterError> {
        let session_id = session_id(source);
        let mut classified = Vec::with_capacity(read.units.len());
        let mut latest_blocks = BTreeMap::<BlockKey, LogicalBlock>::new();

        for raw in &read.units {
            let parsed = serde_json::from_slice::<Value>(&raw.bytes);
            let (unit_kind, disposition, logical_block) = classify_raw(raw, parsed.as_ref());
            let evidence = raw_evidence(source.agent(), &session_id, raw, &unit_kind)?;
            classified.push(ClassifiedUnit {
                ordinal: raw.coverage_ordinal,
                level: RawUnitLevel::Physical,
                evidence: evidence.clone(),
                disposition,
            });
            if let Some(block) = logical_block {
                latest_blocks.insert(block.key.clone(), block.with_parent(evidence));
            }
        }

        for (next_ordinal, block) in
            (read.units.len() as u64 + 1..).zip(latest_blocks.into_values())
        {
            let logical_kind = block.logical_kind();
            let locator = format!(
                "{}_{}-{}",
                ordinal_locator(block.parent.physical_ordinal),
                logical_kind,
                sanitize_locator(&block.key.step_id)
            );
            let evidence_event_id = evidence_event_id_from_hash(
                source.agent(),
                &session_id,
                &locator,
                &logical_kind,
                &block.parent.content_hash,
            )
            .map_err(|error| AdapterError::new("classify", error.to_string()))?;
            classified.push(ClassifiedUnit {
                ordinal: next_ordinal,
                level: RawUnitLevel::Logical {
                    parent_ordinal: block.parent.coverage_ordinal,
                },
                evidence: RawUnitRef {
                    evidence_event_id,
                    coverage_ordinal: next_ordinal,
                    physical_ordinal: block.parent.physical_ordinal,
                    locator,
                    unit_kind: logical_kind.clone(),
                    artifact: block.parent.artifact.clone(),
                    content_hash: block.parent.content_hash.clone(),
                    original_bytes: block.parent.original_bytes,
                },
                disposition: ClassifiedDisposition::Consumed { kind: logical_kind },
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
        assemble_junie(source, read, classified)
    }
}

fn classify_raw(
    raw: &RawUnit,
    parsed: Result<&Value, &serde_json::Error>,
) -> (String, ClassifiedDisposition, Option<LogicalBlock>) {
    if raw.boundary == UnitBoundary::Oversized {
        return (
            "oversized".to_owned(),
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::Oversized,
                visible: true,
            },
            None,
        );
    }

    let Ok(value) = parsed else {
        return (
            "malformed".to_owned(),
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::Malformed,
                visible: true,
            },
            None,
        );
    };

    let event_kind = event_kind(value);
    match event_kind.as_deref() {
        Some("UserPromptEvent") => {
            let kind = if bool_at(value, &["isMetaPrompt"]) || bool_at(value, &["metaPrompt"]) {
                "meta_prompt"
            } else {
                "prompt"
            };
            (
                kind.to_owned(),
                ClassifiedDisposition::Consumed { kind: kind.into() },
                None,
            )
        }
        Some("UserResponseEvent") => (
            "response".to_owned(),
            ClassifiedDisposition::Consumed {
                kind: "response".to_owned(),
            },
            None,
        ),
        Some("SystemMessageEvent") => (
            "system_message".to_owned(),
            ClassifiedDisposition::Consumed {
                kind: "system_message".to_owned(),
            },
            None,
        ),
        Some("CurrentDirectoryChangedEvent" | "CurrentDirectoryUpdatedEvent") => (
            "session_anchor".to_owned(),
            ClassifiedDisposition::Consumed {
                kind: "session_anchor".to_owned(),
            },
            None,
        ),
        Some("MetaPromptEvent") | Some("PromptMetadataEvent") => (
            "meta_prompt".to_owned(),
            ClassifiedDisposition::Consumed {
                kind: "meta_prompt".to_owned(),
            },
            None,
        ),
        Some(kind) if block_flavor(kind).is_some() => (
            format!("agent_event:{kind}"),
            ClassifiedDisposition::Consumed {
                kind: format!("agent_event:{kind}"),
            },
            logical_block(value, kind),
        ),
        Some("AgentStateChangedEvent" | "ProgressStartedEvent" | "ProgressFinishedEvent") => (
            "agent_event:state".to_owned(),
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::Unsupported,
                visible: false,
            },
            None,
        ),
        Some(_) => (
            "unknown_payload".to_owned(),
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::UnknownPayloadType,
                visible: true,
            },
            None,
        ),
        None => (
            "unknown_payload".to_owned(),
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::UnknownPayloadType,
                visible: true,
            },
            None,
        ),
    }
}

fn assemble_junie(
    source: &SourceHandle,
    read: &SourceRead,
    classified: Vec<ClassifiedUnit>,
) -> Result<UnvalidatedParse, AdapterError> {
    let session_id = session_id(source);
    let mut state = Assembly::new(read, session_id);

    for unit in &read.units {
        let Some(classified_unit) = classified.iter().find(|item| {
            item.ordinal == unit.coverage_ordinal && matches!(item.level, RawUnitLevel::Physical)
        }) else {
            continue;
        };
        if unit.boundary == UnitBoundary::UnterminatedTail {
            state.malformed_tail = true;
            state.visible_lost = true;
            state.warn(WarningKind::UnterminatedTail, unit.coverage_ordinal);
        }
        match &classified_unit.disposition {
            ClassifiedDisposition::Consumed { kind } => {
                let value: Value = serde_json::from_slice(&unit.bytes)
                    .map_err(|error| AdapterError::new("assemble", error.to_string()))?;
                state.consume_physical(kind, &value, classified_unit.evidence.clone());
            }
            ClassifiedDisposition::Skipped { reason, visible } => {
                state.observe_skip(classified_unit, *reason, *visible, unit.boundary);
            }
        }
    }

    for logical in classified
        .iter()
        .filter(|unit| matches!(unit.level, RawUnitLevel::Logical { .. }))
    {
        state.consume_logical(logical);
    }

    let model = state.finish(classified);
    if model.coverage.status.visible_completeness == VisibleCompleteness::Fatal {
        Ok(UnvalidatedParse::fatal(model.coverage))
    } else {
        Ok(UnvalidatedParse::from_model(model))
    }
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
    segment_started_at: Known<String>,
    turns: Vec<Turn>,
    tools: Vec<ToolEvent>,
    latest_blocks: BTreeMap<(u64, String), LogicalBlock>,
    warnings: Vec<CoverageWarning>,
    unsupported_visible: bool,
    malformed_tail: bool,
    visible_lost: bool,
}

impl<'a> Assembly<'a> {
    fn new(read: &'a SourceRead, session_id: String) -> Self {
        let inferred = infer_timestamp(&session_id);
        Self {
            read,
            session_id,
            model: Known::unknown(),
            cli_version: Known::unknown(),
            cwd: Known::unknown(),
            branch: Known::unknown(),
            started_at: known_option(inferred.clone()),
            ended_at: known_option(inferred.clone()),
            segment_started_at: known_option(inferred),
            turns: Vec::new(),
            tools: Vec::new(),
            latest_blocks: BTreeMap::new(),
            warnings: Vec::new(),
            unsupported_visible: false,
            malformed_tail: false,
            visible_lost: false,
        }
    }

    fn consume_physical(&mut self, kind: &str, value: &Value, evidence: RawUnitRef) {
        let timestamp = timestamp_for(value, &self.started_at);
        if matches!(self.started_at, Known::Unknown(_)) {
            self.started_at = timestamp.clone();
            self.segment_started_at = timestamp.clone();
        }
        if matches!(timestamp, Known::Value(_)) {
            self.ended_at = timestamp.clone();
        }

        match kind {
            "prompt" => {
                let text = first_text(
                    value,
                    &[
                        &["prompt"],
                        &["message"],
                        &["content"],
                        &["data", "prompt"],
                        &["data", "content"],
                    ],
                );
                self.push_turn(TurnRole::User, TurnKind::UserMsg, timestamp, text, evidence);
            }
            "response" => {
                let text = first_text(
                    value,
                    &[
                        &["response"],
                        &["message"],
                        &["content"],
                        &["data", "response"],
                        &["data", "content"],
                    ],
                );
                self.push_turn(TurnRole::User, TurnKind::UserMsg, timestamp, text, evidence);
            }
            "system_message" | "meta_prompt" => {
                let text = first_text(
                    value,
                    &[
                        &["message"],
                        &["prompt"],
                        &["content"],
                        &["data", "message"],
                        &["data", "content"],
                    ],
                );
                self.push_turn(
                    TurnRole::System,
                    TurnKind::SystemNote,
                    timestamp,
                    text,
                    evidence,
                );
            }
            "session_anchor" => {
                self.cwd = known_string(first_present(
                    value,
                    &[
                        &["currentDirectory"],
                        &["cwd"],
                        &["path"],
                        &["data", "currentDirectory"],
                        &["data", "cwd"],
                        &["data", "path"],
                        &["event", "agentEvent", "currentDirectory"],
                        &["event", "agentEvent", "cwd"],
                        &["event", "agentEvent", "path"],
                    ],
                ));
                if let Some(branch) = first_present(
                    value,
                    &[
                        &["branch"],
                        &["data", "branch"],
                        &["event", "agentEvent", "branch"],
                    ],
                ) {
                    self.branch = Known::value(branch.to_owned());
                }
            }
            _ if kind.starts_with("agent_event:") => {
                if let Some(block) =
                    logical_block(value, event_kind(value).as_deref().unwrap_or(""))
                {
                    self.latest_blocks.insert(
                        (evidence.physical_ordinal, block.logical_kind()),
                        block.with_parent(evidence),
                    );
                }
            }
            _ => {}
        }

        if let Some(model) = first_present(value, &[&["model"], &["data", "model"]]) {
            self.model = Known::value(model.to_owned());
        }
    }

    fn consume_logical(&mut self, unit: &ClassifiedUnit) {
        let Some(block) = self
            .latest_blocks
            .get(&(
                unit.evidence.physical_ordinal,
                unit.evidence.unit_kind.clone(),
            ))
            .cloned()
        else {
            return;
        };
        let timestamp = block
            .timestamp
            .or_else(|| known_value(&self.ended_at).map(str::to_owned));
        let timestamp = known_option(timestamp);
        match block.flavor {
            BlockFlavor::Text => {
                self.push_turn(
                    TurnRole::Assistant,
                    TurnKind::AgentReply,
                    timestamp,
                    block.text,
                    unit.evidence.clone(),
                );
            }
            BlockFlavor::Reasoning => {
                self.push_turn(
                    TurnRole::Assistant,
                    TurnKind::InternalThought,
                    timestamp,
                    block.text,
                    unit.evidence.clone(),
                );
            }
            BlockFlavor::ToolCall => {
                let turn_idx = self.turns.len() as u64;
                let tool_name = if block.tool_name.is_empty() {
                    "unknown_junie_tool".to_owned()
                } else {
                    block.tool_name
                };
                self.push_turn(
                    TurnRole::Tool,
                    TurnKind::ToolCall,
                    timestamp,
                    block.text.clone(),
                    unit.evidence.clone(),
                );
                self.tools.push(ToolEvent {
                    kind: ToolEventKind::Call,
                    turn_idx,
                    tool_name,
                    correlation_id: known_option(block.correlation_id),
                    payload_hash: sha256_hex(block.text.as_bytes()),
                    payload_bytes: block.text.len() as u64,
                    raw_unit_refs: vec![unit.evidence.clone()],
                });
            }
            BlockFlavor::ToolResult => {
                let turn_idx = self.turns.len() as u64;
                let tool_name = if block.tool_name.is_empty() {
                    "unknown_junie_tool".to_owned()
                } else {
                    block.tool_name
                };
                self.push_turn(
                    TurnRole::Tool,
                    TurnKind::ToolResult,
                    timestamp,
                    block.text.clone(),
                    unit.evidence.clone(),
                );
                self.tools.push(ToolEvent {
                    kind: ToolEventKind::Result,
                    turn_idx,
                    tool_name,
                    correlation_id: known_option(block.correlation_id),
                    payload_hash: sha256_hex(block.text.as_bytes()),
                    payload_bytes: block.text.len() as u64,
                    raw_unit_refs: vec![unit.evidence.clone()],
                });
            }
        }
    }

    fn push_turn(
        &mut self,
        role: TurnRole,
        kind: TurnKind,
        timestamp: Known<String>,
        text: String,
        evidence: RawUnitRef,
    ) {
        let text_hash = sha256_hex(text.as_bytes());
        self.turns.push(Turn {
            turn_idx: self.turns.len() as u64,
            role,
            timestamp,
            kind,
            text_chars: text.chars().count() as u64,
            text,
            text_hash,
            tool_name: Known::unknown(),
            segment_id: 0,
            raw_unit_refs: vec![evidence],
        });
    }

    fn observe_skip(
        &mut self,
        unit: &ClassifiedUnit,
        reason: SkippedReason,
        visible: bool,
        boundary: UnitBoundary,
    ) {
        let warning = match reason {
            SkippedReason::UnknownPayloadType => WarningKind::UnknownPayloadType,
            SkippedReason::Malformed => WarningKind::MalformedUnit,
            SkippedReason::Oversized => WarningKind::OversizedUnit,
            SkippedReason::EncryptedOpaque => WarningKind::OpaqueReasoning,
            SkippedReason::Unsupported => WarningKind::UnsupportedVisibleEvent,
        };
        self.warn(warning, unit.ordinal);
        if visible
            && matches!(
                reason,
                SkippedReason::UnknownPayloadType | SkippedReason::Unsupported
            )
        {
            self.unsupported_visible = true;
            self.visible_lost = true;
        }
        if visible && matches!(reason, SkippedReason::Malformed | SkippedReason::Oversized) {
            self.visible_lost = true;
        }
        if boundary == UnitBoundary::UnterminatedTail {
            self.malformed_tail = true;
            self.visible_lost |= visible;
            self.warn(WarningKind::UnterminatedTail, unit.ordinal);
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

    fn finish(mut self, classified: Vec<ClassifiedUnit>) -> SessionModel {
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
                    reason,
                    bytes: unit.evidence.original_bytes,
                    visible,
                    evidence: unit.evidence,
                }),
            }
        }
        self.warnings.sort_by_key(|warning| warning.first_ordinal);
        let visible_completeness = if consumed.is_empty() && !skipped.is_empty() {
            VisibleCompleteness::Fatal
        } else if self.visible_lost || self.malformed_tail || self.unsupported_visible {
            VisibleCompleteness::PartialVisible
        } else {
            VisibleCompleteness::CompleteVisible
        };
        let coverage = CoverageReport::with_raw_line_count(
            self.read.units.len() as u64,
            consumed.len() as u64 + skipped.len() as u64,
            consumed,
            skipped,
            self.warnings,
            ParseStatus {
                visible_completeness,
                boundary_flags: BoundaryFlags {
                    opaque_reasoning_present: false,
                    unsupported_visible_event: self.unsupported_visible,
                },
                malformed_tail_present: self.malformed_tail,
                visible_event_lost: self.visible_lost,
            },
        );
        let provenance = Provenance {
            agent: AgentKind::Junie,
            model: self.model,
            cli_version: self.cli_version,
            cwd: self.cwd.clone(),
            branch: self.branch.clone(),
            started_at: self.started_at.clone(),
            ended_at: self.ended_at.clone(),
            original_source_hash: self.read.source_hash.clone(),
            original_source_bytes: self.read.source_bytes,
        };
        let mut model = SessionModel::new(self.session_id, provenance, coverage);
        model.turns = self.turns;
        model.tool_events = self.tools;
        if !model.turns.is_empty() {
            model.segments.push(Segment {
                segment_id: 0,
                cwd: self.cwd,
                branch: self.branch,
                started_at: self.segment_started_at,
                ended_at: self.ended_at,
                turn_range: TurnRange {
                    start: 0,
                    end: model.turns.len() as u64 - 1,
                },
            });
        }
        model
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BlockKey {
    step_id: String,
    flavor: BlockFlavor,
}

#[derive(Debug, Clone)]
struct LogicalBlock {
    key: BlockKey,
    flavor: BlockFlavor,
    text: String,
    tool_name: String,
    correlation_id: Option<String>,
    timestamp: Option<String>,
    parent: RawUnitRef,
}

impl LogicalBlock {
    fn with_parent(mut self, parent: RawUnitRef) -> Self {
        self.parent = parent;
        self
    }

    fn logical_kind(&self) -> String {
        match self.flavor {
            BlockFlavor::Text => "agent_text_block_snapshot",
            BlockFlavor::Reasoning => "agent_reasoning_block_snapshot",
            BlockFlavor::ToolCall => "agent_tool_call_block_snapshot",
            BlockFlavor::ToolResult => "agent_tool_result_block_snapshot",
        }
        .to_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BlockFlavor {
    Text,
    Reasoning,
    ToolCall,
    ToolResult,
}

fn logical_block(value: &Value, event_kind: &str) -> Option<LogicalBlock> {
    let flavor = block_flavor(event_kind)?;
    let step_id = first_present(
        value,
        &[
            &["stepId"],
            &["requestId"],
            &["id"],
            &["data", "stepId"],
            &["data", "requestId"],
            &["data", "id"],
            &["event", "agentEvent", "stepId"],
            &["event", "agentEvent", "requestId"],
            &["event", "agentEvent", "id"],
        ],
    )
    .unwrap_or("unknown-step")
    .to_owned();
    let text = match flavor {
        BlockFlavor::ToolCall => first_text(
            value,
            &[
                &["arguments"],
                &["input"],
                &["content"],
                &["data", "arguments"],
                &["data", "input"],
                &["data", "content"],
            ],
        ),
        BlockFlavor::ToolResult => first_text(
            value,
            &[
                &["output"],
                &["result"],
                &["content"],
                &["data", "output"],
                &["data", "result"],
                &["data", "content"],
            ],
        ),
        BlockFlavor::Text | BlockFlavor::Reasoning => first_text(
            value,
            &[
                &["text"],
                &["content"],
                &["markdown"],
                &["data", "text"],
                &["data", "content"],
                &["data", "markdown"],
                &["event", "agentEvent", "text"],
                &["event", "agentEvent", "content"],
                &["event", "agentEvent", "markdown"],
                &["event", "agentEvent", "result", "text"],
                &["event", "agentEvent", "result", "content"],
                &["event", "agentEvent", "result", "markdown"],
            ],
        ),
    };
    Some(LogicalBlock {
        key: BlockKey { step_id, flavor },
        flavor,
        text,
        tool_name: first_present(
            value,
            &[
                &["toolName"],
                &["tool"],
                &["name"],
                &["data", "toolName"],
                &["data", "tool"],
                &["data", "name"],
                &["event", "agentEvent", "toolName"],
                &["event", "agentEvent", "tool"],
                &["event", "agentEvent", "name"],
            ],
        )
        .unwrap_or("")
        .to_owned(),
        correlation_id: first_present(
            value,
            &[
                &["toolCallId"],
                &["callId"],
                &["id"],
                &["data", "toolCallId"],
                &["data", "callId"],
                &["data", "id"],
                &["event", "agentEvent", "toolCallId"],
                &["event", "agentEvent", "callId"],
                &["event", "agentEvent", "id"],
            ],
        )
        .map(str::to_owned),
        timestamp: first_present(
            value,
            &[
                &["timestamp"],
                &["createdAt"],
                &["data", "timestamp"],
                &["event", "agentEvent", "timestamp"],
                &["event", "agentEvent", "createdAt"],
            ],
        )
        .map(str::to_owned),
        parent: RawUnitRef {
            evidence_event_id: String::new(),
            coverage_ordinal: 0,
            physical_ordinal: 0,
            locator: String::new(),
            unit_kind: String::new(),
            artifact: String::new(),
            content_hash: String::new(),
            original_bytes: 0,
        },
    })
}

fn block_flavor(kind: &str) -> Option<BlockFlavor> {
    match kind {
        "AgentTextBlockUpdatedEvent"
        | "AgentResultBlockUpdatedEvent"
        | "ResultBlockUpdatedEvent" => Some(BlockFlavor::Text),
        "AgentReasoningBlockUpdatedEvent" => Some(BlockFlavor::Reasoning),
        "AgentToolCallBlockUpdatedEvent" => Some(BlockFlavor::ToolCall),
        "AgentToolCallOutputBlockUpdatedEvent" | "AgentToolResultBlockUpdatedEvent" => {
            Some(BlockFlavor::ToolResult)
        }
        _ => None,
    }
}

fn raw_evidence(
    agent: AgentKind,
    session_id: &str,
    raw: &RawUnit,
    unit_kind: &str,
) -> Result<RawUnitRef, AdapterError> {
    let locator = ordinal_locator(raw.physical_ordinal);
    let evidence_event_id =
        evidence_event_id_from_hash(agent, session_id, &locator, unit_kind, &raw.content_hash)
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

fn event_kind(value: &Value) -> Option<String> {
    first_present(
        value,
        &[
            &["kind"],
            &["type"],
            &["eventType"],
            &["event", "kind"],
            &["data", "kind"],
            &["data", "type"],
        ],
    )
    .map(str::to_owned)
}

fn timestamp_for(value: &Value, fallback: &Known<String>) -> Known<String> {
    known_string(
        first_present(
            value,
            &[
                &["timestamp"],
                &["createdAt"],
                &["time"],
                &["data", "timestamp"],
                &["data", "createdAt"],
                &["event", "agentEvent", "timestamp"],
                &["event", "agentEvent", "createdAt"],
            ],
        )
        .or_else(|| first_present(value, &[&["requestId"]]).and_then(infer_timestamp_from_text))
        .or_else(|| known_value(fallback)),
    )
}

fn infer_timestamp(session_id: &str) -> Option<String> {
    infer_timestamp_from_text(session_id).map(str::to_owned)
}

fn infer_timestamp_from_text(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    for start in 0..bytes.len().saturating_sub(12) {
        if bytes.get(start + 6) == Some(&b'-')
            && bytes[start..start + 6].iter().all(u8::is_ascii_digit)
            && bytes[start + 7..start + 13].iter().all(u8::is_ascii_digit)
        {
            return text.get(start..start + 13);
        }
    }
    None
}

fn known_string(value: Option<&str>) -> Known<String> {
    match value.filter(|value| !value.is_empty()) {
        Some(value) => Known::value(if is_junie_short_timestamp(value) {
            format_junie_short_timestamp(value)
        } else {
            value.to_owned()
        }),
        None => Known::unknown(),
    }
}

fn known_option(value: Option<String>) -> Known<String> {
    match value.filter(|value| !value.is_empty()) {
        Some(value) => Known::value(if is_junie_short_timestamp(&value) {
            format_junie_short_timestamp(&value)
        } else {
            value
        }),
        None => Known::unknown(),
    }
}

fn known_value(value: &Known<String>) -> Option<&str> {
    match value {
        Known::Value(value) => Some(value.as_str()),
        Known::Unknown(_) => None,
    }
}

fn is_junie_short_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 13
        && bytes.get(6) == Some(&b'-')
        && bytes[..6].iter().all(u8::is_ascii_digit)
        && bytes[7..].iter().all(u8::is_ascii_digit)
}

fn format_junie_short_timestamp(value: &str) -> String {
    let year = &value[0..2];
    let month = &value[2..4];
    let day = &value[4..6];
    let hour = &value[7..9];
    let minute = &value[9..11];
    let second = &value[11..13];
    format!("20{year}-{month}-{day}T{hour}:{minute}:{second}Z")
}

fn session_id(source: &SourceHandle) -> String {
    source
        .logical_session_id()
        .unwrap_or_else(|| source.source_id())
        .to_owned()
}

fn first_present<'a>(value: &'a Value, paths: &[&[&str]]) -> Option<&'a str> {
    paths.iter().find_map(|path| string_at(value, path))
}

fn first_text(value: &Value, paths: &[&[&str]]) -> String {
    first_present(value, paths).unwrap_or("").to_owned()
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str()
}

fn bool_at(value: &Value, path: &[&str]) -> bool {
    let mut current = value;
    for key in path {
        let Some(next) = current.get(*key) else {
            return false;
        };
        current = next;
    }
    current.as_bool().unwrap_or(false)
}

fn sanitize_locator(value: &str) -> String {
    value
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() || char == '-' || char == '_' {
                char
            } else {
                '_'
            }
        })
        .collect()
}
