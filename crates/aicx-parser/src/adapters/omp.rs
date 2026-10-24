//! Native oh-my-pi (`omp`) and upstream `pi` session JSONL adapter.
//!
//! Discovery lives outside this kernel. One selected artifact is one session
//! file: `<encoded-cwd>/<ISO-ts>_<uuidv7>.jsonl` under `~/.omp/agent/sessions`
//! or `~/.pi/agent/sessions`. Schema source: oh-my-pi
//! `packages/coding-agent` session entries, `CURRENT_SESSION_VERSION = 3`.
//!
//! Active branch, matching `SessionIndex` in `session-manager.ts`:
//! `title` and `session` are file headers, not tree nodes. Every other known
//! entry with an `id` is inserted in file order, and the leaf is the last
//! insert. `pathTo` walks `parentId` to the root with a per-path cycle set,
//! then reverses to root→leaf. Duplicate ids: the last occurrence wins.
//! Turns are emitted in that path order. Off-branch entries stay covered as
//! `off_branch:<type>` and never become speech.
//!
//! Human speech is only `message.role = "user"` that omp would treat as typed
//! by a person: attribution missing or `"user"`, not `synthetic`, not a
//! mid-turn steer, and not a Vibecrafted dispatch brief. `custom_message`,
//! `attribution: "agent"`, `developer`, and `bashExecution` are harness or
//! agent lanes.

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
    RawUnitRef, ScopeStatus, Segment, SessionModel, SkippedReason, SkippedUnit, SourceFraming,
    SourceHandle, SourceRead, TokenComponents, ToolEvent, ToolEventKind, Turn, TurnKind, TurnRange,
    TurnRole, UnitBoundary, UnvalidatedParse, UsageEvent, VisibleCompleteness, WarningKind,
    evidence_event_id_from_hash, ordinal_locator, sha256_hex,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const OMP_ADAPTER_VERSION: &str = "omp-session-v3";

/// Session-entry `type` values declared by upstream `SessionEntry`.
/// A new provider type is a visible gap, never a silent drop.
const SESSION_ENTRY_TYPES: &[&str] = &[
    "message",
    "model_usage",
    "thinking_level_change",
    "model_change",
    "service_tier_change",
    "compaction",
    "branch_summary",
    "custom",
    "custom_message",
    "label",
    "title_change",
    "ttsr_injection",
    "session_init",
    "mode_change",
    "credential_pin",
    "reset_boundary",
];

const DISPATCH_HEADS: &[&str] = &[
    "You are running under Vibecrafted core runtime.",
    "Read and follow the private task file: ",
];

const REDACTED_KEYS: &[&str] = &[
    "textSignature",
    "thoughtSignature",
    "signature",
    "providerPromptCacheKey",
    "taskPromptCacheKey",
];

#[derive(Debug, Clone, Copy, Default)]
pub struct OmpAdapter;

impl sealed::Sealed for OmpAdapter {}

impl AgentAdapter for OmpAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Omp
    }

    fn adapter_version(&self) -> &'static str {
        OMP_ADAPTER_VERSION
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

struct Draft {
    segment: Segment,
    workdirs: Vec<WorkdirEvidence>,
}

struct Call {
    name: String,
    payload: String,
    turn: usize,
}

struct Line {
    value: Option<Value>,
    type_name: String,
    entry_id: Option<String>,
    parent_id: Option<String>,
    tree: bool,
    early: Option<ClassifiedDisposition>,
}

struct Analysis {
    classified: Vec<ClassifiedUnit>,
    turns: Vec<Turn>,
    tools: Vec<ToolEvent>,
    usage: Vec<UsageEvent>,
    epochs: Vec<(ContextEpochRef, u64)>,
    drafts: Vec<Draft>,
    calls: BTreeMap<String, Call>,
    warnings: Vec<CoverageWarning>,
    provenance: Provenance,
    conversation_id: String,
    parent_session_id: Option<String>,
    parent_observed: bool,
    current_cwd: Known<String>,
    segment_started: Known<String>,
    visible_lost: bool,
    unsupported_visible: bool,
    malformed_tail: bool,
    opaque_reasoning: bool,
}

fn analyze(source: &SourceHandle, read: &SourceRead) -> Result<Analysis, AdapterError> {
    let jsonl = source
        .artifacts()
        .iter()
        .filter(|artifact| artifact.framing() == SourceFraming::JsonLines)
        .count();
    if jsonl != 1 || source.artifacts().len() != 1 {
        return Err(AdapterError::new(
            "classify",
            "omp needs exactly one JSONL session artifact",
        ));
    }
    let store_id = source
        .logical_session_id()
        .unwrap_or_else(|| source.source_id())
        .to_owned();
    let mut state = Analysis {
        classified: Vec::new(),
        turns: Vec::new(),
        tools: Vec::new(),
        usage: Vec::new(),
        epochs: Vec::new(),
        drafts: Vec::new(),
        calls: BTreeMap::new(),
        warnings: Vec::new(),
        conversation_id: store_id.clone(),
        parent_session_id: None,
        parent_observed: false,
        current_cwd: Known::unknown(),
        segment_started: Known::unknown(),
        visible_lost: false,
        unsupported_visible: false,
        malformed_tail: false,
        opaque_reasoning: false,
        provenance: Provenance {
            agent: AgentKind::Omp,
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

    let mut lines = Vec::with_capacity(read.units.len());
    for raw in &read.units {
        lines.push(prepare_line(&mut state, raw));
    }
    let id_index = tree_index(&lines);
    let project_order = active_path(&lines, &id_index);
    let active: BTreeSet<usize> = project_order.iter().copied().collect();

    let mut evidence_by_ordinal = BTreeMap::new();
    for (index, (raw, line)) in read.units.iter().zip(lines.iter()).enumerate() {
        let on_branch = active.contains(&index);
        let kind = coverage_kind(line, on_branch);
        let disposition = line.early.clone().unwrap_or_else(|| {
            if line.tree && !on_branch {
                consumed(&kind)
            } else if line.type_name == "unknown" {
                state.unknown(raw.coverage_ordinal)
            } else {
                consumed(&kind)
            }
        });
        let classified = unit(source, raw, &store_id, &kind, disposition)?;
        evidence_by_ordinal.insert(raw.coverage_ordinal, classified.evidence.clone());
        state.classified.push(classified);
    }

    for line in &lines {
        if line.type_name == "session"
            && let Some(value) = &line.value
        {
            state.session_header(value);
        }
    }

    for index in project_order {
        let line = &lines[index];
        let Some(value) = line.value.as_ref() else {
            continue;
        };
        let Some(raw) = read.units.get(index) else {
            continue;
        };
        let Some(evidence) = evidence_by_ordinal.get(&raw.coverage_ordinal).cloned() else {
            continue;
        };
        state.project(value, evidence);
    }

    state.classified.sort_by_key(|unit| unit.ordinal);
    Ok(state)
}

fn prepare_line(state: &mut Analysis, raw: &RawUnit) -> Line {
    if raw.boundary == UnitBoundary::Oversized {
        state.visible_lost = true;
        state.warn(WarningKind::OversizedUnit, raw.coverage_ordinal);
        state.ensure_segment();
        if let Some(draft) = state.drafts.last_mut() {
            draft.workdirs.push(WorkdirEvidence::Opaque);
        }
        return Line {
            value: None,
            type_name: "oversized".to_owned(),
            entry_id: None,
            parent_id: None,
            tree: false,
            early: Some(skip(SkippedReason::Oversized, true)),
        };
    }
    if raw.bytes.iter().all(u8::is_ascii_whitespace) {
        return Line {
            value: None,
            type_name: "blank".to_owned(),
            entry_id: None,
            parent_id: None,
            tree: false,
            early: Some(skip(SkippedReason::Unsupported, false)),
        };
    }
    let parsed = serde_json::from_slice::<Value>(&raw.bytes);
    let Ok(value @ Value::Object(_)) = parsed else {
        state.visible_lost = true;
        state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
        if raw.boundary == UnitBoundary::UnterminatedTail {
            state.malformed_tail = true;
            state.warn(WarningKind::UnterminatedTail, raw.coverage_ordinal);
        }
        return Line {
            value: None,
            type_name: "malformed".to_owned(),
            entry_id: None,
            parent_id: None,
            tree: false,
            early: Some(skip(SkippedReason::Malformed, true)),
        };
    };
    if raw.boundary == UnitBoundary::UnterminatedTail {
        state.warn(WarningKind::UnterminatedTail, raw.coverage_ordinal);
    }
    let type_name = string(&value, "type").unwrap_or("unknown").to_owned();
    let known_header = type_name == "title" || type_name == "session";
    let known_entry = SESSION_ENTRY_TYPES.contains(&type_name.as_str());
    if !known_header && !known_entry {
        return Line {
            value: Some(value),
            type_name: "unknown".to_owned(),
            entry_id: None,
            parent_id: None,
            tree: false,
            early: None,
        };
    }
    let entry_id = string(&value, "id")
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    let parent_id = value
        .get("parentId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    let tree = known_entry && entry_id.is_some();
    Line {
        value: Some(value),
        type_name,
        entry_id,
        parent_id,
        tree,
        early: None,
    }
}

fn tree_index(lines: &[Line]) -> BTreeMap<String, usize> {
    let mut index = BTreeMap::new();
    for (ordinal, line) in lines.iter().enumerate() {
        if line.tree
            && let Some(id) = &line.entry_id
        {
            index.insert(id.clone(), ordinal);
        }
    }
    index
}

/// Root→leaf indexes. The leaf is the last inserted tree entry.
/// Duplicate ids keep the last occurrence, matching `SessionIndex.insert`.
fn active_path(lines: &[Line], id_index: &BTreeMap<String, usize>) -> Vec<usize> {
    let Some(leaf_id) = lines.iter().rev().find_map(|line| {
        line.tree
            .then_some(line.entry_id.as_deref())
            .flatten()
            .map(str::to_owned)
    }) else {
        return Vec::new();
    };
    let mut cursor = Some(leaf_id);
    let mut seen = BTreeSet::new();
    let mut path = Vec::new();
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) {
            break;
        }
        let Some(&index) = id_index.get(&id) else {
            break;
        };
        path.push(index);
        cursor = lines[index].parent_id.clone();
    }
    path.reverse();
    path
}

fn coverage_kind(line: &Line, on_branch: bool) -> String {
    if line.tree && !on_branch {
        return format!("off_branch:{}", sanitize_kind(&line.type_name));
    }
    if !line.tree && SESSION_ENTRY_TYPES.contains(&line.type_name.as_str()) {
        return format!("unlinked:{}", sanitize_kind(&line.type_name));
    }
    match line.type_name.as_str() {
        "message" => {
            let role = line
                .value
                .as_ref()
                .and_then(|value| value.get("message"))
                .and_then(|message| string(message, "role"))
                .unwrap_or("unknown");
            format!("message:{}", sanitize_kind(role))
        }
        "custom" | "custom_message" => {
            let custom = line
                .value
                .as_ref()
                .and_then(|value| string(value, "customType"))
                .unwrap_or("untyped");
            format!("{}:{}", line.type_name, sanitize_kind(custom))
        }
        other => sanitize_kind(other),
    }
}

fn sanitize_kind(kind: &str) -> String {
    let mut cleaned = String::new();
    for ch in kind.chars().take(128) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':' | '@' | '+') {
            cleaned.push(ch);
        } else {
            cleaned.push('_');
        }
    }
    if cleaned.is_empty() {
        "unknown_payload".to_owned()
    } else {
        cleaned
    }
}

impl Analysis {
    fn session_header(&mut self, value: &Value) {
        if let Some(id) = string(value, "id").filter(|id| !id.is_empty()) {
            self.conversation_id = id.to_owned();
        }
        if value.get("parentSession").is_some() {
            self.parent_observed = true;
            self.parent_session_id = string(value, "parentSession")
                .filter(|id| !id.is_empty())
                .map(str::to_owned);
        }
        if let Some(cwd) = string(value, "cwd").filter(|cwd| !cwd.is_empty()) {
            self.current_cwd = Known::value(cwd.to_owned());
            self.provenance.cwd = self.current_cwd.clone();
        }
        let stamp = timestamp(string(value, "timestamp"));
        if matches!(self.provenance.started_at, Known::Unknown(_)) {
            self.provenance.started_at = stamp.clone();
            self.segment_started = stamp;
        }
        self.note_end(string(value, "timestamp"));
        if let Some(dirs) = value.get("additionalDirectories").and_then(Value::as_array) {
            let evidence: Vec<_> = dirs
                .iter()
                .filter_map(Value::as_str)
                .filter(|dir| !dir.is_empty())
                .map(|dir| WorkdirEvidence::Explicit(dir.to_owned()))
                .collect();
            if !evidence.is_empty() {
                self.ensure_segment();
                if let Some(draft) = self.drafts.last_mut() {
                    draft.workdirs.extend(evidence);
                }
            }
        }
    }

    fn project(&mut self, value: &Value, evidence: RawUnitRef) {
        let stamp = timestamp(string(value, "timestamp"));
        self.note_end(string(value, "timestamp"));
        let boundary = evidence.physical_ordinal;
        match string(value, "type") {
            Some("message") => self.message(value, stamp, evidence),
            Some("model_change") => {
                if let Some(model) = string(value, "model").filter(|model| !model.is_empty()) {
                    self.provenance.model = Known::value(model.to_owned());
                }
            }
            Some("model_usage") => self.usage_record(value, stamp, evidence),
            Some("custom_message") => {
                let tag = string(value, "customType").unwrap_or("custom_message");
                let content = message_text(value.get("content").unwrap_or(&Value::Null));
                self.inject(tag, content, stamp, evidence);
            }
            Some("session_init") => self.session_init(value, stamp, evidence),
            Some("compaction") => {
                let summary = string(value, "summary").unwrap_or("").to_owned();
                self.epoch(boundary, &evidence, string(value, "method"));
                self.inject("compaction", summary, stamp, evidence);
            }
            Some("branch_summary") => {
                let summary = string(value, "summary").unwrap_or("").to_owned();
                self.epoch(boundary, &evidence, Some("branch_summary"));
                self.inject("branch_summary", summary, stamp, evidence);
            }
            Some("reset_boundary") => self.epoch(boundary, &evidence, Some("reset_boundary")),
            Some("ttsr_injection") => {
                let names = value
                    .get("injectedRules")
                    .and_then(Value::as_array)
                    .map(|rules| {
                        rules
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                self.inject("ttsr_injection", names, stamp, evidence);
            }
            Some(
                "custom"
                | "credential_pin"
                | "title_change"
                | "thinking_level_change"
                | "service_tier_change"
                | "label"
                | "mode_change",
            ) => {}
            _ => {}
        }
    }

    fn message(&mut self, value: &Value, stamp: Known<String>, evidence: RawUnitRef) {
        let Some(message) = value.get("message") else {
            self.visible_lost = true;
            self.unsupported_visible = true;
            self.warn(WarningKind::UnknownPayloadType, evidence.coverage_ordinal);
            return;
        };
        let message_stamp = timestamp(string(message, "timestamp").or(match &stamp {
            Known::Value(value) => Some(value.as_str()),
            Known::Unknown(_) => None,
        }));
        match string(message, "role") {
            Some("user") => self.user_message(message, message_stamp, evidence),
            Some("assistant") => self.assistant_message(message, message_stamp, evidence),
            Some("toolResult") => self.tool_result(message, message_stamp, evidence),
            Some("bashExecution") => self.bash_execution(message, message_stamp, evidence),
            Some("developer") => {
                let content = message_text(message.get("content").unwrap_or(&Value::Null));
                self.inject("developer", content, message_stamp, evidence);
            }
            Some("custom") => {
                let tag = string(message, "customType").unwrap_or("custom");
                let content = message_text(message.get("content").unwrap_or(&Value::Null));
                self.inject(tag, content, message_stamp, evidence);
            }
            _ => {
                self.visible_lost = true;
                self.unsupported_visible = true;
                self.warn(WarningKind::UnknownPayloadType, evidence.coverage_ordinal);
            }
        }
    }

    fn user_message(&mut self, message: &Value, stamp: Known<String>, evidence: RawUnitRef) {
        let content = message_text(message.get("content").unwrap_or(&Value::Null));
        let attribution = string(message, "attribution");
        let synthetic = message
            .get("synthetic")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let steering = message
            .get("steering")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || message
                .get("liveSteered")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let dispatch = DISPATCH_HEADS
            .iter()
            .any(|marker| content.trim_start().starts_with(marker));
        let human =
            matches!(attribution, None | Some("user")) && !synthetic && !steering && !dispatch;
        if human {
            self.frame(
                TransportKind::DirectMessage,
                TransportPayload::Text {
                    role: TransportRole::User,
                    content,
                },
                stamp,
                evidence,
            );
            return;
        }
        let tag = if dispatch {
            "dispatch_brief"
        } else if synthetic {
            "synthetic_user"
        } else if steering {
            "steering"
        } else if attribution == Some("agent") {
            "agent_authored"
        } else {
            "user_unattributed"
        };
        self.inject(tag, content, stamp, evidence);
    }

    fn assistant_message(&mut self, message: &Value, stamp: Known<String>, evidence: RawUnitRef) {
        if let Some(model) = string(message, "model").filter(|model| !model.is_empty()) {
            self.provenance.model = Known::value(model.to_owned());
        }
        if let Some(usage) = message.get("usage") {
            self.push_usage(
                string(message, "provider").unwrap_or("omp"),
                string(message, "model"),
                usage,
                stamp.clone(),
                evidence.clone(),
            );
        }
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            let content = message_text(message.get("content").unwrap_or(&Value::Null));
            self.reply(content, stamp, evidence);
            return;
        };
        let mut saw_block = false;
        for block in blocks {
            let block_type = string(block, "type").unwrap_or("");
            match block_type {
                "text" => {
                    saw_block = true;
                    self.reply(
                        string(block, "text").unwrap_or("").to_owned(),
                        stamp.clone(),
                        evidence.clone(),
                    );
                }
                "thinking" => {
                    saw_block = true;
                    let text = thinking_text(block);
                    if text.is_empty() {
                        continue;
                    }
                    self.turn(
                        TurnRole::Assistant,
                        TurnKind::InternalThought,
                        text,
                        stamp.clone(),
                        Known::unknown(),
                        evidence.clone(),
                        None,
                    );
                }
                "toolCall" => {
                    saw_block = true;
                    self.tool_call(block, stamp.clone(), evidence.clone());
                }
                _ => {
                    if !block_type.is_empty() {
                        self.opaque_reasoning = true;
                        self.warn(WarningKind::OpaqueReasoning, evidence.coverage_ordinal);
                    }
                }
            }
        }
        if !saw_block && blocks.is_empty() {
            self.reply(
                message_text(message.get("content").unwrap_or(&Value::Null)),
                stamp,
                evidence,
            );
        }
    }

    fn reply(&mut self, content: String, stamp: Known<String>, evidence: RawUnitRef) {
        if content.trim().is_empty() {
            return;
        }
        self.frame(
            TransportKind::AssistantMessage,
            TransportPayload::Text {
                role: TransportRole::Assistant,
                content,
            },
            stamp,
            evidence,
        );
    }

    fn tool_call(&mut self, block: &Value, stamp: Known<String>, evidence: RawUnitRef) {
        let id = string(block, "id").unwrap_or("unknown").to_owned();
        let name = string(block, "name").unwrap_or("unknown_tool").to_owned();
        let arguments = redact(block.get("arguments").cloned().unwrap_or(Value::Null));
        let payload = value_text(&arguments);
        if let Some(previous) = self.calls.get(&id)
            && previous.payload == payload
            && previous.name == name
        {
            let turn = previous.turn;
            self.turns[turn].raw_unit_refs.push(evidence);
            return;
        }
        self.observe_workdirs(&name, &arguments);
        let command = shell_command(&name, &arguments);
        let frame_class = command.map(|command| {
            frames::classify(&TransportFrame {
                agent: AgentKind::Omp,
                transport_kind: TransportKind::AgentToolCall,
                timestamp: stamp.clone(),
                payload: TransportPayload::Shell {
                    command,
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
            },
        );
    }

    fn tool_result(&mut self, message: &Value, stamp: Known<String>, evidence: RawUnitRef) {
        let id = string(message, "toolCallId")
            .unwrap_or("unknown")
            .to_owned();
        let name = self
            .calls
            .get(&id)
            .map(|call| call.name.clone())
            .or_else(|| string(message, "toolName").map(str::to_owned))
            .unwrap_or_else(|| "unknown_tool".to_owned());
        let mut payload = Map::new();
        payload.insert("toolCallId".to_owned(), Value::String(id.clone()));
        payload.insert("toolName".to_owned(), Value::String(name.clone()));
        if let Some(is_error) = message.get("isError").and_then(Value::as_bool) {
            payload.insert("isError".to_owned(), Value::Bool(is_error));
            payload.insert("success".to_owned(), Value::Bool(!is_error));
        }
        let content = message_text(message.get("content").unwrap_or(&Value::Null));
        if !content.is_empty() {
            payload.insert("content".to_owned(), Value::String(content));
        }
        if let Some(details) = message.get("details") {
            let details = redact(details.clone());
            if let Some(code) = details.get("exitCode").cloned() {
                payload.insert("exitCode".to_owned(), code);
            }
            payload.insert("details".to_owned(), details);
        }
        self.finish_tool(&id, &name, &Value::Object(payload), stamp, evidence);
    }

    fn bash_execution(&mut self, message: &Value, stamp: Known<String>, evidence: RawUnitRef) {
        let id = evidence.evidence_event_id.clone();
        let command = string(message, "command").unwrap_or("").to_owned();
        let arguments = serde_json::json!({ "command": command });
        self.observe_workdirs("bash", &arguments);
        let frame_class = (!command.is_empty()).then(|| {
            frames::classify(&TransportFrame {
                agent: AgentKind::Omp,
                transport_kind: TransportKind::AgentToolCall,
                timestamp: stamp.clone(),
                payload: TransportPayload::Shell {
                    command,
                    result: String::new(),
                },
                evidence: evidence.clone(),
            })
            .class
        });
        let payload = value_text(&arguments);
        let turn = self.turns.len();
        self.turn(
            TurnRole::Tool,
            TurnKind::ToolCall,
            payload.clone(),
            stamp.clone(),
            Known::value("bash".to_owned()),
            evidence.clone(),
            frame_class,
        );
        self.tools.push(ToolEvent {
            kind: ToolEventKind::Call,
            turn_idx: turn as u64,
            tool_name: "bash".to_owned(),
            correlation_id: Known::value(id.clone()),
            payload_hash: sha256_hex(payload.as_bytes()),
            payload_bytes: payload.len() as u64,
            raw_unit_refs: vec![evidence.clone()],
        });
        self.calls.insert(
            id.clone(),
            Call {
                name: "bash".to_owned(),
                payload,
                turn,
            },
        );
        let mut result = Map::new();
        if let Some(code) = message.get("exitCode").cloned() {
            result.insert("exitCode".to_owned(), code);
        }
        if let Some(output) = string(message, "output") {
            result.insert("output".to_owned(), Value::String(output.to_owned()));
        }
        if let Some(cancelled) = message.get("cancelled").and_then(Value::as_bool) {
            result.insert("cancelled".to_owned(), Value::Bool(cancelled));
            if cancelled {
                result.insert("success".to_owned(), Value::Bool(false));
            }
        }
        if let Some(truncated) = message.get("truncated").and_then(Value::as_bool) {
            result.insert("truncated".to_owned(), Value::Bool(truncated));
        }
        self.finish_tool(&id, "bash", &Value::Object(result), stamp, evidence);
    }

    fn finish_tool(
        &mut self,
        id: &str,
        name: &str,
        payload_value: &Value,
        stamp: Known<String>,
        evidence: RawUnitRef,
    ) {
        let payload = value_text(payload_value);
        if let Some(call) = self.calls.get(id)
            && let Some(FrameClass::ShellAction { cmd, executor, .. }) =
                &self.turns[call.turn].frame_class
        {
            let transport_kind = match executor {
                ShellExecutor::Human => TransportKind::UserShellCommand,
                ShellExecutor::Agent => TransportKind::AgentToolCall,
            };
            let frame = TransportFrame {
                agent: AgentKind::Omp,
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
            Known::value(name.to_owned()),
            evidence.clone(),
            None,
        );
        self.tools.push(ToolEvent {
            kind: ToolEventKind::Result,
            turn_idx: turn as u64,
            tool_name: name.to_owned(),
            correlation_id: Known::value(id.to_owned()),
            payload_hash: sha256_hex(payload.as_bytes()),
            payload_bytes: payload.len() as u64,
            raw_unit_refs: vec![evidence],
        });
    }

    fn session_init(&mut self, value: &Value, stamp: Known<String>, evidence: RawUnitRef) {
        if let Some(model) = string(value, "resolvedModel").filter(|model| !model.is_empty()) {
            self.provenance.model = Known::value(model.to_owned());
        }
        let task = string(value, "task").unwrap_or("");
        let prompt = match value.get("systemPrompt") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        let mut content = String::new();
        if !task.is_empty() {
            content.push_str(task);
        }
        if !prompt.is_empty() {
            if !content.is_empty() {
                content.push_str("\n\n");
            }
            content.push_str(&prompt);
        }
        self.inject("session_init", content, stamp, evidence);
    }

    fn usage_record(&mut self, value: &Value, stamp: Known<String>, evidence: RawUnitRef) {
        let Some(usage) = value.get("usage") else {
            return;
        };
        if let Some(model) = string(value, "model").filter(|model| !model.is_empty()) {
            self.provenance.model = Known::value(model.to_owned());
        }
        self.push_usage(
            string(value, "provider").unwrap_or("omp"),
            string(value, "model"),
            usage,
            stamp,
            evidence,
        );
    }

    fn push_usage(
        &mut self,
        provider: &str,
        model: Option<&str>,
        usage: &Value,
        stamp: Known<String>,
        evidence: RawUnitRef,
    ) {
        let counter = |key| number_u64(usage.get(key));
        self.usage.push(UsageEvent {
            provider: provider.to_owned(),
            model: known(model),
            tokens: TokenComponents {
                input: counter("input"),
                output: counter("output"),
                reasoning: counter("reasoningTokens"),
                cache_read: counter("cacheRead"),
                cache_creation: counter("cacheWrite"),
            },
            cost: usage
                .get("cost")
                .map(cost_of)
                .unwrap_or_else(Known::unknown),
            timestamp: stamp,
            span: Known::unknown(),
            counter_semantics: CounterSemantics::Snapshot,
            evidence,
        });
    }

    fn epoch(&mut self, boundary: u64, evidence: &RawUnitRef, trigger: Option<&str>) {
        self.epochs.push((
            ContextEpochRef {
                compaction_index: self.epochs.len() as u32,
                summary_provenance: evidence.evidence_event_id.clone(),
                replacement_refs: Vec::new(),
                trigger: known(trigger),
                first_turn_after: None,
            },
            boundary,
        ));
    }

    fn note_end(&mut self, raw: Option<&str>) {
        let stamp = timestamp(raw);
        if matches!(stamp, Known::Value(_)) {
            self.provenance.ended_at = stamp;
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
            agent: AgentKind::Omp,
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
        if content.trim().is_empty() {
            return;
        }
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

    fn observe_workdirs(&mut self, name: &str, arguments: &Value) {
        let mut args = arguments.clone();
        if let Some(object) = args.as_object_mut()
            && !object.contains_key("workdir")
            && let Some(cwd) = object.get("cwd").cloned()
        {
            object.insert("workdir".to_owned(), cwd);
        }
        let payload = serde_json::json!({ "name": name, "arguments": args });
        let workdirs = tool_call_workdirs(&payload);
        if !workdirs.is_empty() {
            self.ensure_segment();
            if let Some(draft) = self.drafts.last_mut() {
                draft.workdirs.extend(workdirs);
            }
        }
    }

    fn ensure_segment(&mut self) {
        if self.drafts.is_empty() {
            self.drafts.push(Draft {
                segment: Segment {
                    segment_id: 0,
                    cwd: self.current_cwd.clone(),
                    branch: Known::unknown(),
                    started_at: self.segment_started.clone(),
                    ended_at: Known::unknown(),
                    turn_range: TurnRange {
                        start: self.turns.len() as u64,
                        end: self.turns.len() as u64,
                    },
                    scope_status: ScopeStatus::from_evidence(
                        value(&self.current_cwd),
                        None::<&str>,
                    ),
                    scope_conflict: false,
                    scope_root: None,
                    scope_workdirs: Vec::new(),
                },
                workdirs: Vec::new(),
            });
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
        model.conversation = ProviderConversationRef::Omp {
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
                    turn.raw_unit_refs
                        .iter()
                        .any(|reference| reference.physical_ordinal > boundary)
                })
                .map(|turn| turn.turn_idx);
            model.context_epochs.push(epoch);
        }
        model.turns = self.turns;
        model.tool_events = self.tools;
        model.usage_events = self.usage;
        model.segments = self
            .drafts
            .into_iter()
            .filter(|draft| draft.segment.turn_range.start < model.turns.len() as u64)
            .map(|draft| draft.segment)
            .collect();
        UnvalidatedParse::from_model(model)
    }
}

fn message_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.clone()),
                Value::Object(_) if string(item, "type") == Some("text") => {
                    string(item, "text").map(str::to_owned)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn thinking_text(block: &Value) -> String {
    let text = string(block, "text").unwrap_or("").trim().to_owned();
    if !text.is_empty() {
        return text;
    }
    match block.get("summary") {
        Some(Value::String(summary)) => summary.trim().to_owned(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| {
                string(item, "text")
                    .or_else(|| item.as_str())
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn shell_command(name: &str, arguments: &Value) -> Option<String> {
    if !matches!(name, "bash" | "powershell" | "shell") {
        return None;
    }
    let parsed = parse_args(arguments);
    string(&parsed, "command")
        .or_else(|| string(&parsed, "cmd"))
        .filter(|command| !command.is_empty())
        .map(str::to_owned)
}

fn parse_args(arguments: &Value) -> Value {
    if let Some(raw) = arguments.as_str() {
        serde_json::from_str::<Value>(raw).unwrap_or_else(|_| arguments.clone())
    } else {
        arguments.clone()
    }
}

fn redact(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut cleaned = Map::new();
            for (key, child) in map {
                if REDACTED_KEYS.contains(&key.as_str()) {
                    continue;
                }
                cleaned.insert(key, redact(child));
            }
            Value::Object(cleaned)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact).collect()),
        other => other,
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

fn number_u64(value: Option<&Value>) -> Known<u64> {
    let Some(value) = value else {
        return Known::unknown();
    };
    if let Some(number) = value.as_u64() {
        return Known::value(number);
    }
    if let Some(number) = value.as_f64()
        && number.is_finite()
        && number >= 0.0
        && number <= u64::MAX as f64
    {
        return Known::value(number.round() as u64);
    }
    Known::unknown()
}

fn cost_of(value: &Value) -> Known<crate::engine::ReportedCost> {
    let amount = value
        .as_f64()
        .or_else(|| value.get("total").and_then(Value::as_f64));
    match amount {
        Some(amount) if amount.is_finite() => Known::value(crate::engine::ReportedCost {
            amount,
            currency: "unspecified".to_owned(),
        }),
        _ => Known::unknown(),
    }
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
    let kind = sanitize_kind(kind);
    let evidence_event_id = evidence_event_id_from_hash(
        source.agent(),
        session_id,
        &locator,
        &kind,
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
            unit_kind: kind,
            artifact: raw.artifact_name.clone(),
            content_hash: raw.content_hash.clone(),
            original_bytes: raw.original_bytes,
        },
        disposition,
    })
}
