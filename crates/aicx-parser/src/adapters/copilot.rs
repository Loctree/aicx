//! Native GitHub Copilot CLI `events.jsonl` adapter.
//!
//! Inputs are explicitly selected artifacts, never discovered here. Optional
//! `workspace.yaml` supplies fallback metadata; event context wins. The SDK
//! event schema is published at github/copilot-sdk, generated/session-events.ts.
//! Streaming mirrors are suppressed only when their durable completion exists.
//! Unknown events and unfinished streams remain visible coverage losses.

use super::{
    AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel, sealed,
};
use crate::engine::ShellExecutor;
use crate::engine::frames::{self, TransportFrame, TransportKind, TransportPayload, TransportRole};
use crate::engine::scope_evidence::{
    WindowScope, WorkdirEvidence, effective_window_scope, recorded_workdir, tool_call_workdirs,
};
use crate::engine::{
    AgentKind, BoundaryFlags, ConsumedUnit, ContextEpochRef, CounterSemantics, CoverageReport,
    CoverageWarning, FrameClass, Known, ParseStatus, Provenance, ProviderConversationRef, RawUnit,
    RawUnitRef, ScopeStatus, Segment, SessionModel, SkillInvocation, SkippedReason, SkippedUnit,
    SourceFraming, SourceHandle, SourceRead, TokenComponents, ToolEvent, ToolEventKind, Turn,
    TurnKind, TurnRange, TurnRole, UnitBoundary, UnvalidatedParse, UsageEvent, VisibleCompleteness,
    WarningKind, evidence_event_id_from_hash, ordinal_locator, sha256_hex,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const COPILOT_ADAPTER_VERSION: &str = "copilot-events-v1";

/// Explicitly understood operational events with no conversation payload.
/// Prefix matching is deliberately absent: a new provider event is a gap.
const BOOKKEEPING: &[&str] = &[
    "session.permissions_changed",
    "session.permission_mode_changed",
    "session.remote_steerable_changed",
    "session.reasoning_effort_changed",
    "session.verbosity_changed",
    "session.reasoning_summary_changed",
    "session.limits_changed",
    "session.usage_checkpoint",
    "session.usage_info",
    "session.idle",
    "session.compaction_start",
    "session.snapshot_rewind",
    "session.tools_updated",
    "session.mcp_servers_loaded",
    "session.extensions_loaded",
    "session.mode_changed",
    "session.plan_changed",
    "session.workspace_file_changed",
    "session.model_deselected",
    "session.auto_tier_changed",
    "assistant.turn_start",
    "assistant.turn_end",
    "assistant.intent",
    "assistant.streaming_delta",
    "assistant.server_tool_progress",
    "hook.start",
    "hook.end",
    "permission.requested",
    "permission.completed",
    "skill.context_delivered_ref",
    "subagent.started",
    "subagent.configured",
    "subagent.completed",
    "model.turn_started",
    "model.model_call_started",
    "model.model_call_failure",
    "model.turn_ended",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct CopilotAdapter;

impl sealed::Sealed for CopilotAdapter {}

impl AgentAdapter for CopilotAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Copilot
    }
    fn adapter_version(&self) -> &'static str {
        COPILOT_ADAPTER_VERSION
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
                "classification changed during assembly",
            ));
        }
        Ok(analysis.finish(source, read))
    }
}

#[derive(Default)]
struct Completions {
    messages: BTreeSet<String>,
    reasoning: BTreeSet<String>,
    tool_calls: BTreeSet<String>,
    tool_results: BTreeSet<String>,
}

struct Draft {
    segment: Segment,
    workdirs: Vec<WorkdirEvidence>,
    closed: bool,
}

struct Call {
    name: String,
    payload: String,
    turn: usize,
    event: usize,
}

struct Analysis {
    classified: Vec<ClassifiedUnit>,
    turns: Vec<Turn>,
    tools: Vec<ToolEvent>,
    usage: Vec<UsageEvent>,
    skills: Vec<SkillInvocation>,
    epochs: Vec<(ContextEpochRef, u64)>,
    drafts: Vec<Draft>,
    calls: BTreeMap<String, Call>,
    warnings: Vec<CoverageWarning>,
    provenance: Provenance,
    conversation_id: String,
    parent_session_id: Option<String>,
    parent_observed: bool,
    current_cwd: Known<String>,
    current_branch: Known<String>,
    segment_started: Known<String>,
    visible_lost: bool,
    unsupported_visible: bool,
    malformed_tail: bool,
    opaque_reasoning: bool,
}

fn analyze(source: &SourceHandle, read: &SourceRead) -> Result<Analysis, AdapterError> {
    let mut events_count = 0;
    for artifact in source.artifacts() {
        match (artifact.name(), artifact.framing()) {
            ("workspace.yaml", SourceFraming::WholeDocument) => {}
            (_, SourceFraming::JsonLines) => events_count += 1,
            _ => {
                return Err(AdapterError::new(
                    "classify",
                    "Copilot needs one JSONL event artifact and optional workspace.yaml whole document",
                ));
            }
        }
    }
    if events_count != 1 {
        return Err(AdapterError::new(
            "classify",
            "Copilot needs exactly one event artifact",
        ));
    }
    let session_id = source
        .logical_session_id()
        .unwrap_or_else(|| source.source_id());
    let mut state = Analysis {
        classified: Vec::new(),
        turns: Vec::new(),
        tools: Vec::new(),
        usage: Vec::new(),
        skills: Vec::new(),
        epochs: Vec::new(),
        drafts: Vec::new(),
        calls: BTreeMap::new(),
        warnings: Vec::new(),
        conversation_id: session_id.to_owned(),
        parent_session_id: None,
        parent_observed: false,
        current_cwd: Known::unknown(),
        current_branch: Known::unknown(),
        segment_started: Known::unknown(),
        visible_lost: false,
        unsupported_visible: false,
        malformed_tail: false,
        opaque_reasoning: false,
        provenance: Provenance {
            agent: AgentKind::Copilot,
            model: Known::unknown(),
            cli_version: Known::unknown(),
            cwd: Known::unknown(),
            branch: Known::unknown(),
            started_at: Known::unknown(),
            ended_at: Known::unknown(),
            original_source_hash: read.source_hash.clone(),
            original_source_bytes: read.source_bytes,
        },
    };
    // Read metadata first independently of the caller's artifact order. It can
    // only fill gaps: the append-only event stream carries historical context.
    for raw in read
        .units
        .iter()
        .filter(|unit| unit.artifact_name == "workspace.yaml")
    {
        let parsed = serde_yaml::from_slice::<Value>(&raw.bytes);
        let disposition = if raw.boundary == UnitBoundary::Oversized {
            state.warn(WarningKind::OversizedUnit, raw.coverage_ordinal);
            skip(SkippedReason::Oversized, false)
        } else if let Ok(value @ Value::Object(_)) = parsed {
            state.workspace(&value);
            consumed("workspace_metadata")
        } else {
            state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
            skip(SkippedReason::Malformed, false)
        };
        state.classified.push(unit(
            source,
            raw,
            session_id,
            "workspace_metadata",
            disposition,
        )?);
    }
    let completions = collect_completions(read);
    let mut seen_events = BTreeMap::<String, String>::new();
    for raw in read
        .units
        .iter()
        .filter(|unit| unit.artifact_name != "workspace.yaml")
    {
        let parsed = serde_json::from_slice::<Value>(&raw.bytes);
        let (kind, disposition) = if raw.boundary == UnitBoundary::Oversized {
            state.visible_lost = true;
            state.warn(WarningKind::OversizedUnit, raw.coverage_ordinal);
            state.ensure_segment();
            state
                .drafts
                .last_mut()
                .unwrap()
                .workdirs
                .push(WorkdirEvidence::Opaque);
            ("oversized".to_owned(), skip(SkippedReason::Oversized, true))
        } else if raw.bytes.iter().all(u8::is_ascii_whitespace) {
            ("blank".to_owned(), skip(SkippedReason::Unsupported, false))
        } else if let Ok(value @ Value::Object(_)) = parsed {
            if raw.boundary == UnitBoundary::UnterminatedTail {
                state.warn(WarningKind::UnterminatedTail, raw.coverage_ordinal);
            }
            let kind = string(&value, "type")
                .unwrap_or("unknown_payload")
                .to_owned();
            let evidence = unit(source, raw, session_id, &kind, consumed(&kind))?.evidence;
            let event_id = string(&value, "id");
            let duplicate =
                event_id.is_some_and(|id| seen_events.get(id) == Some(&raw.content_hash));
            if let Some(id) = event_id {
                seen_events.insert(id.to_owned(), raw.content_hash.clone());
            }
            let disposition = if duplicate {
                skip(SkippedReason::DuplicateBody, false)
            } else {
                state.event(&value, evidence, &completions)?
            };
            (kind, disposition)
        } else {
            state.visible_lost = true;
            state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
            if raw.boundary == UnitBoundary::UnterminatedTail {
                state.malformed_tail = true;
                state.warn(WarningKind::UnterminatedTail, raw.coverage_ordinal);
            }
            ("malformed".to_owned(), skip(SkippedReason::Malformed, true))
        };
        state
            .classified
            .push(unit(source, raw, session_id, &kind, disposition)?);
    }
    state.classified.sort_by_key(|unit| unit.ordinal);
    Ok(state)
}

fn collect_completions(read: &SourceRead) -> Completions {
    let mut result = Completions::default();
    for raw in &read.units {
        if raw.boundary == UnitBoundary::Oversized {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(&raw.bytes) else {
            continue;
        };
        let data = &record["data"];
        match string(&record, "type") {
            Some("assistant.message") if string(data, "content").is_some() => {
                if let Some(id) = string(data, "messageId")
                    && string(data, "content").is_some_and(|text| !text.trim().is_empty())
                {
                    result.messages.insert(id.to_owned());
                }
                if let Some(requests) = data.get("toolRequests").and_then(Value::as_array) {
                    for request in requests {
                        if let Some(id) = string(request, "toolCallId") {
                            result.tool_calls.insert(id.to_owned());
                        }
                    }
                }
            }
            Some("assistant.reasoning") if string(data, "content").is_some() => {
                if let Some(id) = string(data, "reasoningId") {
                    result.reasoning.insert(id.to_owned());
                }
            }
            Some("tool.execution_start") => {
                if let Some(id) = string(data, "toolCallId") {
                    result.tool_calls.insert(id.to_owned());
                }
            }
            Some("tool.execution_complete") => {
                if let Some(id) = string(data, "toolCallId") {
                    result.tool_results.insert(id.to_owned());
                }
            }
            _ => {}
        }
    }
    result
}

impl Analysis {
    fn workspace(&mut self, value: &Value) {
        if let Some(id) = string(value, "id") {
            self.conversation_id = id.to_owned();
        }
        self.current_cwd = known(string(value, "cwd").or_else(|| string(value, "git_root")));
        self.current_branch = known(string(value, "branch"));
        self.provenance.cwd = self.current_cwd.clone();
        self.provenance.branch = self.current_branch.clone();
        self.provenance.started_at = timestamp(string(value, "created_at"));
        self.segment_started = self.provenance.started_at.clone();
    }

    fn event(
        &mut self,
        record: &Value,
        evidence: RawUnitRef,
        completions: &Completions,
    ) -> Result<ClassifiedDisposition, AdapterError> {
        let kind = string(record, "type").unwrap_or("");
        let data = &record["data"];
        if !data.is_object() {
            return Ok(self.unknown(evidence.coverage_ordinal));
        }
        let stamp = timestamp(string(record, "timestamp"));
        if matches!(self.provenance.started_at, Known::Unknown(_)) {
            self.provenance.started_at = stamp.clone();
            self.segment_started = stamp.clone();
        }
        if matches!(stamp, Known::Value(_)) {
            self.provenance.ended_at = stamp.clone();
        }
        match kind {
            "session.start" => {
                if let Some(id) = string(data, "sessionId").filter(|id| !id.is_empty()) {
                    self.conversation_id = id.to_owned();
                }
                self.parent_session_id =
                    string(data, "detachedFromSpawningParentSessionId").map(str::to_owned);
                self.parent_observed = self.parent_session_id.is_some();
                self.provenance.cli_version = known(string(data, "copilotVersion"));
                self.provenance.model = known(string(data, "selectedModel"));
                let start = timestamp(string(data, "startTime"));
                if matches!(start, Known::Value(_)) {
                    self.provenance.started_at = start.clone();
                    self.segment_started = start;
                }
                if let Some(context) = data.get("context").filter(|context| context.is_object()) {
                    self.context(context, stamp, true);
                }
            }
            "session.resume" => {
                self.new_window(stamp.clone());
                if let Some(context) = data.get("context").filter(|context| context.is_object()) {
                    self.context(context, stamp, false);
                }
                if let Some(model) = string(data, "selectedModel") {
                    self.provenance.model = known(Some(model));
                }
            }
            "session.context_changed" => self.context(data, stamp, false),
            "session.model_change" => {
                self.provenance.model = known(string(data, "newModel"));
            }
            "session.title_changed" => {}
            "user.message" => {
                let Some(content) = string(data, "content") else {
                    return Ok(self.unknown(evidence.coverage_ordinal));
                };
                // A source/subagent marker is runtime-authored input, not proof
                // of a Founder utterance. The original content alone is human
                // speech; transformedContent contains injected attachments.
                if let Some(sender) = string(record, "agentId")
                    .filter(|id| !id.is_empty())
                    .or_else(|| {
                        string(data, "source").filter(|source| source.starts_with("agent-"))
                    })
                {
                    self.frame(
                        TransportKind::AgentMessage,
                        TransportPayload::InterAgent {
                            sender: sender.to_owned(),
                            task: string(data, "parentAgentTaskId")
                                .unwrap_or("unknown")
                                .to_owned(),
                            message_type: kind.to_owned(),
                            content: content.to_owned(),
                        },
                        stamp,
                        evidence,
                    );
                } else if let Some(source) =
                    string(data, "source").filter(|source| !source.is_empty())
                {
                    self.inject(source, content.to_owned(), stamp, evidence);
                } else if data.get("isAutopilotContinuation").and_then(Value::as_bool) == Some(true)
                {
                    self.inject(
                        "autopilot_continuation",
                        content.to_owned(),
                        stamp,
                        evidence,
                    );
                } else {
                    self.new_window(stamp.clone());
                    let transport = if string(data, "delivery") == Some("queued") {
                        TransportKind::QueueOperation
                    } else {
                        TransportKind::DirectMessage
                    };
                    self.frame(
                        transport,
                        TransportPayload::Text {
                            role: TransportRole::User,
                            content: content.to_owned(),
                        },
                        stamp.clone(),
                        evidence.clone(),
                    );
                    if let Some(transformed) =
                        string(data, "transformedContent").filter(|text| *text != content)
                    {
                        self.inject(
                            "user.transformed_context",
                            transformed.to_owned(),
                            stamp.clone(),
                            evidence.clone(),
                        );
                    }
                    if let Some(attachments) = data
                        .get("attachments")
                        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
                    {
                        self.inject("user.attachments", value_text(attachments), stamp, evidence);
                    }
                }
            }
            "system.message" => {
                let Some(content) = string(data, "content") else {
                    return Ok(self.unknown(evidence.coverage_ordinal));
                };
                self.inject(
                    string(data, "role").unwrap_or("system"),
                    content.to_owned(),
                    stamp,
                    evidence,
                );
            }
            "assistant.message" => {
                let Some(content) = string(data, "content") else {
                    return Ok(self.unknown(evidence.coverage_ordinal));
                };
                if let Some(model) = string(data, "model") {
                    self.provenance.model = known(Some(model));
                }
                if data.get("reasoningOpaque").is_some_and(|v| !v.is_null())
                    || data.get("encryptedContent").is_some_and(|v| !v.is_null())
                {
                    self.opaque_reasoning = true;
                    self.warn(WarningKind::OpaqueReasoning, evidence.coverage_ordinal);
                }
                if let Some(reasoning) = string(data, "reasoningText") {
                    self.reasoning(
                        record,
                        reasoning.to_owned(),
                        stamp.clone(),
                        evidence.clone(),
                    );
                } else if let Some(blocks) = data
                    .pointer("/reasoningBlocks/blocks")
                    .and_then(Value::as_array)
                {
                    let reasoning = blocks
                        .iter()
                        .filter_map(|block| {
                            string(block, "text").or_else(|| string(block, "thinking"))
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.reasoning(record, reasoning, stamp.clone(), evidence.clone());
                }
                if let Some(sender) =
                    string(record, "agentId").or_else(|| string(data, "parentToolCallId"))
                {
                    self.frame(
                        TransportKind::AgentMessage,
                        TransportPayload::InterAgent {
                            sender: sender.to_owned(),
                            task: string(data, "parentToolCallId")
                                .unwrap_or("unknown")
                                .to_owned(),
                            message_type: kind.to_owned(),
                            content: content.to_owned(),
                        },
                        stamp.clone(),
                        evidence.clone(),
                    );
                } else {
                    self.frame(
                        TransportKind::AssistantMessage,
                        TransportPayload::Text {
                            role: TransportRole::Assistant,
                            content: content.to_owned(),
                        },
                        stamp.clone(),
                        evidence.clone(),
                    );
                }
                if let Some(requests) = data.get("toolRequests").filter(|v| !v.is_null()) {
                    let Some(requests) = requests.as_array() else {
                        return Ok(self.unknown(evidence.coverage_ordinal));
                    };
                    for request in requests {
                        if string(request, "name").is_none()
                            || string(request, "toolCallId").is_none()
                        {
                            return Ok(self.unknown(evidence.coverage_ordinal));
                        }
                        self.call(request, false, stamp.clone(), evidence.clone());
                    }
                }
            }
            "assistant.reasoning" => {
                let Some(content) = string(data, "content") else {
                    return Ok(self.unknown(evidence.coverage_ordinal));
                };
                self.reasoning(record, content.to_owned(), stamp, evidence);
            }
            "tool.execution_start" | "tool.user_requested" => {
                if string(data, "toolName").is_none() || string(data, "toolCallId").is_none() {
                    return Ok(self.unknown(evidence.coverage_ordinal));
                }
                self.observe_workdirs(data);
                self.call(data, kind == "tool.user_requested", stamp, evidence);
            }
            "tool.execution_complete" => {
                if string(data, "toolCallId").is_none()
                    || data.get("success").and_then(Value::as_bool).is_none()
                {
                    return Ok(self.unknown(evidence.coverage_ordinal));
                }
                self.result(data, stamp, evidence);
            }
            "assistant.usage" => self.usage(data, stamp, evidence, CounterSemantics::Delta),
            "session.shutdown" => {
                if let Some(metrics) = data.get("modelMetrics").and_then(Value::as_object) {
                    for (model, metric) in metrics {
                        if let Some(usage) = metric.get("usage") {
                            let mut usage = usage.clone();
                            if let Some(object) = usage.as_object_mut() {
                                object.insert("model".to_owned(), Value::String(model.clone()));
                            }
                            self.usage(
                                &usage,
                                stamp.clone(),
                                evidence.clone(),
                                CounterSemantics::Cumulative,
                            );
                        }
                    }
                }
                if let Some(error) = string(data, "errorReason") {
                    self.inject("session_error", error.to_owned(), stamp, evidence);
                }
            }
            "session.compaction_complete" => {
                if data.get("success").and_then(Value::as_bool) == Some(true) {
                    let summary = string(data, "summaryContent");
                    self.epochs.push((
                        ContextEpochRef {
                            compaction_index: self.epochs.len() as u32,
                            summary_provenance: evidence.evidence_event_id.clone(),
                            replacement_refs: summary
                                .map(|text| vec![sha256_hex(text.as_bytes())])
                                .unwrap_or_default(),
                            trigger: known(string(data, "trigger")),
                            first_turn_after: None,
                        },
                        evidence.physical_ordinal,
                    ));
                } else if let Some(error) = string(data, "error") {
                    self.inject("compaction_error", error.to_owned(), stamp, evidence);
                }
            }
            "skill.invoked" => {
                let Some(name) = string(data, "name") else {
                    return Ok(self.unknown(evidence.coverage_ordinal));
                };
                let Some(content) = string(data, "content") else {
                    return Ok(self.unknown(evidence.coverage_ordinal));
                };
                let turn_idx = self.turns.len() as u64;
                self.inject("skill.invoked", content.to_owned(), stamp.clone(), evidence);
                if self.turns.len() as u64 > turn_idx {
                    self.skills.push(SkillInvocation {
                        turn_idx,
                        skill_name: name.to_owned(),
                        payload_hash: sha256_hex(content.as_bytes()),
                        payload_bytes: content.len() as u64,
                        first_invoked_at: stamp,
                    });
                }
            }
            "system.notification" => {
                if let Some(content) = data.get("content") {
                    self.inject(kind, value_text(content), stamp, evidence);
                } else {
                    return Ok(self.unknown(evidence.coverage_ordinal));
                }
            }
            "session.warning" | "session.info" | "session.error" | "model.turn_failed"
            | "subagent.failed" => {
                let content = data
                    .get("message")
                    .or_else(|| data.get("error"))
                    .map(value_text)
                    .unwrap_or_else(|| value_text(data));
                self.inject(kind, content, stamp, evidence);
            }
            "assistant.message_delta"
            | "assistant.reasoning_delta"
            | "assistant.tool_call_delta"
            | "tool.execution_partial_result" => {
                let complete = match kind {
                    "assistant.message_delta" => string(data, "messageId")
                        .is_some_and(|id| completions.messages.contains(id)),
                    "assistant.reasoning_delta" => string(data, "reasoningId")
                        .is_some_and(|id| completions.reasoning.contains(id)),
                    "assistant.tool_call_delta" => string(data, "toolCallId")
                        .is_some_and(|id| completions.tool_calls.contains(id)),
                    _ => string(data, "toolCallId")
                        .is_some_and(|id| completions.tool_results.contains(id)),
                };
                if complete {
                    return Ok(skip(SkippedReason::DuplicateBody, false));
                }
                return Ok(self.unknown(evidence.coverage_ordinal));
            }
            known if BOOKKEEPING.contains(&known) => {
                return Ok(skip(SkippedReason::Unsupported, false));
            }
            _ => return Ok(self.unknown(evidence.coverage_ordinal)),
        }
        Ok(consumed(kind))
    }

    fn reasoning(
        &mut self,
        record: &Value,
        content: String,
        stamp: Known<String>,
        evidence: RawUnitRef,
    ) {
        if string(record, "agentId").is_some()
            || string(&record["data"], "parentToolCallId").is_some()
        {
            self.inject("subagent.reasoning", content, stamp, evidence);
        } else {
            self.turn(
                TurnRole::Assistant,
                TurnKind::InternalThought,
                content,
                stamp,
                Known::unknown(),
                evidence,
                None,
            );
        }
    }

    fn context(&mut self, data: &Value, stamp: Known<String>, initial: bool) {
        self.new_window(stamp);
        // Initial event context can be partial. An absent field preserves the
        // sidecar fallback; a recorded field still overrides it, including an
        // explicit value that cannot be attributed.
        if !initial || data.get("cwd").is_some() || data.get("gitRoot").is_some() {
            self.current_cwd = known(string(data, "cwd").or_else(|| string(data, "gitRoot")));
        }
        if !initial || data.get("branch").is_some() {
            self.current_branch = known(string(data, "branch"));
        }
        if initial || matches!(self.provenance.cwd, Known::Unknown(_)) {
            self.provenance.cwd = self.current_cwd.clone();
            self.provenance.branch = self.current_branch.clone();
        }
    }

    fn new_window(&mut self, stamp: Known<String>) {
        if self
            .drafts
            .last()
            .is_some_and(|draft| draft.segment.turn_range.start < self.turns.len() as u64)
        {
            self.finalize_window();
            let draft = self.drafts.last_mut().unwrap();
            draft.segment.ended_at = stamp.clone();
            draft.closed = true;
            // The next window is created lazily with the new current context.
        } else if self
            .drafts
            .last()
            .is_some_and(|draft| draft.segment.turn_range.start == self.turns.len() as u64)
        {
            self.drafts.pop();
        }
        self.segment_started = stamp;
    }

    fn ensure_segment(&mut self) {
        let need_new = self.drafts.last().is_none_or(|draft| draft.closed);
        if need_new {
            self.drafts.push(Draft {
                segment: Segment {
                    segment_id: self.drafts.len() as u32,
                    cwd: self.current_cwd.clone(),
                    branch: self.current_branch.clone(),
                    started_at: self.segment_started.clone(),
                    ended_at: Known::unknown(),
                    turn_range: TurnRange {
                        start: self.turns.len() as u64,
                        end: self.turns.len() as u64,
                    },
                    scope_status: ScopeStatus::from_evidence(
                        value(&self.current_cwd),
                        value(&self.current_branch),
                    ),
                    scope_conflict: false,
                    scope_root: None,
                    scope_workdirs: Vec::new(),
                },
                workdirs: Vec::new(),
                closed: false,
            });
        }
    }

    fn observe_workdirs(&mut self, data: &Value) {
        let mut payload = data.clone();
        // Copilot tools use both cwd and workdir; the shared scope reducer
        // consumes the normalized top-level argument, never quoted commands.
        if let Some(arguments) = payload.get_mut("arguments") {
            if let Some(raw) = arguments.as_str()
                && let Ok(parsed) = serde_json::from_str::<Value>(raw)
            {
                *arguments = parsed;
            }
            if let Some(arguments) = arguments.as_object_mut()
                && !arguments.contains_key("workdir")
                && let Some(cwd) = arguments.get("cwd").cloned()
            {
                arguments.insert("workdir".to_owned(), cwd);
            }
        }
        let workdirs = tool_call_workdirs(&payload);
        if !workdirs.is_empty() {
            self.ensure_segment();
            self.drafts.last_mut().unwrap().workdirs.extend(workdirs);
        }
    }

    fn finalize_window(&mut self) {
        let Some(draft) = self.drafts.last_mut() else {
            return;
        };
        let baseline = value(&draft.segment.cwd);
        let (scope, root) = effective_window_scope(&draft.workdirs, baseline);
        draft.segment.scope_workdirs = draft
            .workdirs
            .iter()
            .filter_map(WorkdirEvidence::path)
            .map(|path| recorded_workdir(path, baseline))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        match scope {
            WindowScope::Consistent => {
                draft.segment.scope_root = root;
                draft.segment.scope_status = ScopeStatus::NoDriftObserved;
            }
            WindowScope::Conflict => {
                draft.segment.scope_conflict = true;
                draft.segment.scope_status = ScopeStatus::MixedCandidate;
            }
            WindowScope::Unattributed => {
                draft.segment.scope_status = ScopeStatus::Unattributed;
            }
            WindowScope::Baseline => {}
        }
    }

    fn frame(
        &mut self,
        kind: TransportKind,
        payload: TransportPayload,
        stamp: Known<String>,
        evidence: RawUnitRef,
    ) {
        let classified = frames::classify(&TransportFrame {
            agent: AgentKind::Copilot,
            transport_kind: kind,
            timestamp: stamp,
            payload,
            evidence,
        });
        if let Some(kind) = classified.turn_kind {
            self.turn(
                classified.class.turn_role(),
                kind,
                classified.content,
                classified.seal.seal_ts,
                Known::unknown(),
                classified.origin.evidence,
                Some(classified.class),
            );
        }
    }

    fn inject(&mut self, tag: &str, content: String, stamp: Known<String>, evidence: RawUnitRef) {
        self.frame(
            TransportKind::InjectedContext,
            TransportPayload::Inject {
                tag: tag.to_owned(),
                content,
            },
            stamp,
            evidence,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn turn(
        &mut self,
        role: TurnRole,
        kind: TurnKind,
        text: String,
        stamp: Known<String>,
        tool_name: Known<String>,
        evidence: RawUnitRef,
        frame_class: Option<FrameClass>,
    ) {
        if text.is_empty() {
            return;
        }
        self.ensure_segment();
        let turn_idx = self.turns.len() as u64;
        let segment = &mut self.drafts.last_mut().unwrap().segment;
        segment.turn_range.end = turn_idx;
        self.turns.push(Turn {
            turn_idx,
            role,
            timestamp: stamp,
            kind,
            text_hash: sha256_hex(text.as_bytes()),
            text_chars: text.chars().count() as u64,
            text,
            tool_name,
            segment_id: segment.segment_id,
            raw_unit_refs: vec![evidence],
            frame_class,
        });
    }

    fn call(&mut self, data: &Value, human: bool, stamp: Known<String>, evidence: RawUnitRef) {
        let id = string(data, "toolCallId").unwrap_or("unknown").to_owned();
        let name = string(data, "toolName")
            .or_else(|| string(data, "name"))
            .unwrap_or("unknown_tool")
            .to_owned();
        let arguments = data.get("arguments").unwrap_or(&Value::Null);
        let payload = value_text(arguments);
        if let Some(previous) = self.calls.get(&id)
            && previous.payload == payload
            && previous.name == name
        {
            let turn = previous.turn;
            let event = previous.event;
            self.turns[turn].raw_unit_refs.push(evidence.clone());
            self.tools[event].raw_unit_refs.push(evidence);
            return;
        }
        let parsed_arguments = if let Some(raw) = arguments.as_str() {
            serde_json::from_str::<Value>(raw).unwrap_or_else(|_| arguments.clone())
        } else {
            arguments.clone()
        };
        let command = matches!(name.as_str(), "bash" | "powershell" | "shell")
            .then(|| {
                string(&parsed_arguments, "command").or_else(|| string(&parsed_arguments, "cmd"))
            })
            .flatten();
        let frame_class = command.map(|command| {
            frames::classify(&TransportFrame {
                agent: AgentKind::Copilot,
                transport_kind: if human {
                    TransportKind::UserShellCommand
                } else {
                    TransportKind::AgentToolCall
                },
                timestamp: stamp.clone(),
                payload: TransportPayload::Shell {
                    command: command.to_owned(),
                    result: String::new(),
                },
                evidence: evidence.clone(),
            })
            .class
        });
        let turn = self.turns.len();
        self.turn(
            TurnRole::Tool,
            TurnKind::ToolCall,
            payload.clone(),
            stamp,
            Known::value(name.clone()),
            evidence.clone(),
            frame_class,
        );
        let event = self.tools.len();
        self.tools.push(ToolEvent {
            kind: ToolEventKind::Call,
            turn_idx: turn as u64,
            tool_name: name.clone(),
            correlation_id: Known::value(id.clone()),
            payload_hash: sha256_hex(payload.as_bytes()),
            payload_bytes: payload.len() as u64,
            raw_unit_refs: vec![evidence],
        });
        self.calls.insert(
            id,
            Call {
                name,
                payload,
                turn,
                event,
            },
        );
    }

    fn result(&mut self, data: &Value, stamp: Known<String>, evidence: RawUnitRef) {
        let id = string(data, "toolCallId").unwrap_or("unknown");
        let name = self
            .calls
            .get(id)
            .map(|call| call.name.clone())
            .unwrap_or_else(|| "unknown_tool".to_owned());
        // Preserve completion facts, including failures and shell exitCode,
        // instead of flattening success/error into content or inventing a code.
        let payload = value_text(data);
        // Command projections consume the shared ShellAction's retained result.
        // Bind it by the native call id, even when other tools ran in between;
        // a start or streaming fragment never supplies completion evidence.
        if let Some(call) = self.calls.get(id)
            && let Some(FrameClass::ShellAction { cmd, executor, .. }) =
                &self.turns[call.turn].frame_class
        {
            let transport_kind = match executor {
                ShellExecutor::Human => TransportKind::UserShellCommand,
                ShellExecutor::Agent => TransportKind::AgentToolCall,
            };
            let frame = TransportFrame {
                agent: AgentKind::Copilot,
                transport_kind,
                timestamp: stamp.clone(),
                payload: TransportPayload::Shell {
                    command: cmd.clone(),
                    result: payload.clone(),
                },
                evidence: evidence.clone(),
            };
            self.turns[call.turn].frame_class = Some(frames::classify(&frame).class);
            self.turns[call.turn].raw_unit_refs.push(evidence.clone());
        }
        let turn = self.turns.len();
        self.turn(
            TurnRole::Tool,
            TurnKind::ToolResult,
            payload.clone(),
            stamp,
            Known::value(name.clone()),
            evidence.clone(),
            None,
        );
        self.tools.push(ToolEvent {
            kind: ToolEventKind::Result,
            turn_idx: turn as u64,
            tool_name: name,
            correlation_id: Known::value(id.to_owned()),
            payload_hash: sha256_hex(payload.as_bytes()),
            payload_bytes: payload.len() as u64,
            raw_unit_refs: vec![evidence],
        });
    }

    fn usage(
        &mut self,
        data: &Value,
        stamp: Known<String>,
        evidence: RawUnitRef,
        semantics: CounterSemantics,
    ) {
        let counter = |key| {
            data.get(key)
                .and_then(Value::as_u64)
                .map(Known::value)
                .unwrap_or_else(Known::unknown)
        };
        self.usage.push(UsageEvent {
            provider: "github-copilot".to_owned(),
            model: known(string(data, "model")),
            tokens: TokenComponents {
                input: counter("inputTokens"),
                output: counter("outputTokens"),
                reasoning: counter("reasoningTokens"),
                cache_read: counter("cacheReadTokens"),
                cache_creation: counter("cacheWriteTokens"),
            },
            cost: Known::unknown(),
            timestamp: stamp,
            span: Known::unknown(),
            counter_semantics: semantics,
            evidence,
        });
    }

    fn unknown(&mut self, ordinal: u64) -> ClassifiedDisposition {
        self.visible_lost = true;
        self.unsupported_visible = true;
        self.warn(WarningKind::UnknownPayloadType, ordinal);
        skip(SkippedReason::UnknownPayloadType, true)
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

    fn finish(mut self, source: &SourceHandle, read: &SourceRead) -> UnvalidatedParse {
        self.finalize_window();
        if let Some(draft) = self.drafts.last_mut() {
            draft.segment.ended_at = self.provenance.ended_at.clone();
        }
        let (mut consumed, mut skipped) = (Vec::new(), Vec::new());
        for unit in self.classified {
            match unit.disposition {
                ClassifiedDisposition::Consumed { kind } => consumed.push(ConsumedUnit {
                    ordinal: unit.ordinal,
                    kind,
                    evidence: unit.evidence,
                }),
                ClassifiedDisposition::Skipped { reason, visible } => skipped.push(SkippedUnit {
                    ordinal: unit.ordinal,
                    reason,
                    visible,
                    bytes: unit.evidence.original_bytes,
                    evidence: unit.evidence,
                }),
            }
        }
        self.warnings.sort_by_key(|warning| warning.first_ordinal);
        let coverage = CoverageReport::with_raw_line_count(
            read.units.len() as u64,
            read.units.len() as u64,
            consumed,
            skipped,
            self.warnings,
            ParseStatus {
                visible_completeness: if self.visible_lost || self.malformed_tail {
                    VisibleCompleteness::PartialVisible
                } else {
                    VisibleCompleteness::CompleteVisible
                },
                boundary_flags: BoundaryFlags {
                    opaque_reasoning_present: self.opaque_reasoning,
                    unsupported_visible_event: self.unsupported_visible,
                    compaction_boundary_present: !self.epochs.is_empty(),
                },
                malformed_tail_present: self.malformed_tail,
                visible_event_lost: self.visible_lost,
            },
        );
        let mut model = SessionModel::new(
            source
                .logical_session_id()
                .unwrap_or_else(|| source.source_id())
                .to_owned(),
            self.provenance,
            coverage,
        );
        model.conversation = ProviderConversationRef::Copilot {
            session_id: self.conversation_id,
            parent_session_id: self.parent_session_id,
            unobserved: if self.parent_observed {
                Vec::new()
            } else {
                vec!["parent_session_id".to_owned()]
            },
        };
        for (mut epoch, boundary) in self.epochs {
            epoch.first_turn_after = self
                .turns
                .iter()
                .find(|turn| {
                    turn.raw_unit_refs.iter().any(|reference| {
                        reference.artifact != "workspace.yaml"
                            && reference.physical_ordinal > boundary
                    })
                })
                .map(|turn| turn.turn_idx);
            model.context_epochs.push(epoch);
        }
        model.turns = self.turns;
        model.tool_events = self.tools;
        model.usage_events = self.usage;
        model.skill_invocations = self.skills;
        model.segments = self
            .drafts
            .into_iter()
            .filter(|draft| draft.segment.turn_range.start < model.turns.len() as u64)
            .map(|draft| draft.segment)
            .collect();
        UnvalidatedParse::from_model(model)
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn known(value: Option<&str>) -> Known<String> {
    value
        .filter(|value| !value.is_empty())
        .map(|value| Known::value(value.to_owned()))
        .unwrap_or_else(Known::unknown)
}

fn value(value: &Known<String>) -> Option<&str> {
    match value {
        Known::Value(value) => Some(value.as_str()),
        Known::Unknown(_) => None,
    }
}

fn timestamp(value: Option<&str>) -> Known<String> {
    value
        .filter(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())
        .map(|value| Known::value(value.to_owned()))
        .unwrap_or_else(Known::unknown)
}

fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn consumed(kind: &str) -> ClassifiedDisposition {
    ClassifiedDisposition::Consumed {
        kind: kind.to_owned(),
    }
}
fn skip(reason: SkippedReason, visible: bool) -> ClassifiedDisposition {
    ClassifiedDisposition::Skipped { reason, visible }
}

fn unit(
    source: &SourceHandle,
    raw: &RawUnit,
    session_id: &str,
    kind: &str,
    disposition: ClassifiedDisposition,
) -> Result<ClassifiedUnit, AdapterError> {
    let locator = format!(
        "{}:{}",
        raw.artifact_name,
        ordinal_locator(raw.physical_ordinal)
    );
    let kind = if kind.len() <= 128
        && !kind.contains(['/', '\\'])
        && !kind.chars().any(char::is_control)
    {
        kind
    } else {
        "unknown_payload"
    };
    let evidence_event_id = evidence_event_id_from_hash(
        source.agent(),
        session_id,
        &locator,
        kind,
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
            unit_kind: kind.to_owned(),
            artifact: raw.artifact_name.clone(),
            content_hash: raw.content_hash.clone(),
            original_bytes: raw.original_bytes,
        },
        disposition,
    })
}
