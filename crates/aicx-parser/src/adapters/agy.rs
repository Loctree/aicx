//! Native Antigravity CLI (`agy`) trajectory adapter.
//!
//! Discovery and SQLite live outside this kernel. The app opens each
//! conversation database read-only and hands this adapter one JSONL artifact:
//! a header row of `trajectory_metadata_blob`, then one row per `steps` record
//! sorted by `idx`. `step_payload` bytes stay protobuf. This module walks the
//! wire format itself. There is no schema generator and no `.proto`.
//!
//! Human speech is only step type 14 (`USER_INPUT`) whose text was written by
//! a person. A Vibecrafted dispatch brief on that same step is injected
//! context. Planner prose is step 15. Tool calls are step 132. Every other
//! step type is consumed with a coverage warning — never dropped.

use super::{
    AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel, sealed,
};
use crate::engine::frames::{self, TransportFrame, TransportKind, TransportPayload, TransportRole};
use crate::engine::scope_evidence::{
    WindowScope, WorkdirEvidence, effective_window_scope, recorded_workdir, tool_call_workdirs,
};
use crate::engine::{
    AgentKind, BoundaryFlags, ConsumedUnit, CoverageReport, CoverageWarning, FrameClass, Known,
    ParseStatus, Provenance, ProviderConversationRef, RawUnit, RawUnitRef, ScopeStatus, Segment,
    SessionModel, SkippedReason, SkippedUnit, SourceFraming, SourceHandle, SourceRead, ToolEvent,
    ToolEventKind, Turn, TurnKind, TurnRange, TurnRole, UnitBoundary, UnvalidatedParse,
    VisibleCompleteness, WarningKind, evidence_event_id_from_hash, ordinal_locator, sha256_hex,
};
use serde::Deserialize;
use serde_json::Value;

pub const AGY_ADAPTER_VERSION: &str = "agy-trajectory-v1";

const MAX_DEPTH: usize = 8;
const MAX_PROTO_BYTES: usize = 16 * 1024 * 1024;
const USER_INPUT: i64 = 14;
const PLANNER_RESPONSE: i64 = 15;
const TOOL_CALL: i64 = 132;
const BOOKKEEPING: &[i64] = &[23, 101];
const DISPATCH_HEADS: &[&str] = &[
    "You are running under Vibecrafted core runtime.",
    "Read and follow the private task file: ",
];
const SHELL_TOOLS: &[&str] = &["run_command", "shell", "bash", "powershell"];
const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[derive(Debug, Clone, Copy, Default)]
pub struct AgyAdapter;

impl sealed::Sealed for AgyAdapter {}

impl AgentAdapter for AgyAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Agy
    }

    fn adapter_version(&self) -> &'static str {
        AGY_ADAPTER_VERSION
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
        Ok(analysis.finish(source))
    }
}

/// Header fields the session list can show without walking every step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyHeaderPreview {
    pub cascade_id: Option<String>,
    pub trajectory_id: Option<String>,
    pub cwd: Option<String>,
    pub project_name: Option<String>,
    pub started_at: Option<String>,
}

/// Speech-bearing fields of one step. Bookkeeping and unknown types yield
/// neither human nor assistant text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgyStepPreview {
    pub timestamp: Option<String>,
    pub human_text: Option<String>,
    pub assistant_text: Option<String>,
}

/// Decode the `trajectory_metadata_blob` payload. A corrupt blob yields an
/// empty preview; the caller still has the filename cascade id.
pub fn preview_header(metadata: &[u8]) -> AgyHeaderPreview {
    let Some(root) = parse_message(metadata) else {
        return AgyHeaderPreview {
            cascade_id: None,
            trajectory_id: None,
            cwd: None,
            project_name: None,
            started_at: None,
        };
    };
    AgyHeaderPreview {
        cascade_id: text_at(&root, &[6]),
        trajectory_id: text_at(&root, &[3]),
        cwd: text_at(&root, &[7]).as_deref().and_then(workspace_cwd),
        project_name: text_at(&root, &[18]),
        started_at: stamp_at(&root, &[2, 1], &[2, 2]),
    }
}

/// Decode one `step_payload`. Dispatch briefs are not human speech.
pub fn preview_step(step_type: i64, payload: &[u8]) -> AgyStepPreview {
    let Some(root) = parse_message(payload) else {
        return AgyStepPreview::default();
    };
    let timestamp = stamp_at(&root, &[5, 1, 1], &[5, 1, 2]);
    match step_type {
        USER_INPUT => {
            let text = text_at(&root, &[19, 3, 1]).filter(|text| !text.trim().is_empty());
            let human_text = text.filter(|value| !is_dispatch(value));
            AgyStepPreview {
                timestamp,
                human_text,
                assistant_text: None,
            }
        }
        PLANNER_RESPONSE => AgyStepPreview {
            timestamp,
            human_text: None,
            assistant_text: assistant_prose(&root),
        },
        _ => AgyStepPreview {
            timestamp,
            human_text: None,
            assistant_text: None,
        },
    }
}

/// Standard base64 with padding. The app materializer and this adapter share
/// one alphabet so the stream digest does not depend on a second crate.
pub fn encode_b64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let mut index = 0;
    while index + 3 <= data.len() {
        let chunk =
            ((data[index] as u32) << 16) | ((data[index + 1] as u32) << 8) | data[index + 2] as u32;
        push_b64(&mut out, chunk, 4);
        index += 3;
    }
    match data.len() - index {
        1 => {
            let chunk = (data[index] as u32) << 16;
            push_b64(&mut out, chunk, 2);
            out.push('=');
            out.push('=');
        }
        2 => {
            let chunk = ((data[index] as u32) << 16) | ((data[index + 1] as u32) << 8);
            push_b64(&mut out, chunk, 3);
            out.push('=');
        }
        _ => {}
    }
    out
}

pub fn decode_b64(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    if !text.len().is_multiple_of(4) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|byte| **byte == b'=').count();
        if pad > 2 || (pad > 0 && chunk.len() < 4) {
            return None;
        }
        let mut value = 0u32;
        for (slot, byte) in chunk.iter().enumerate() {
            if *byte == b'=' {
                if slot < 4 - pad {
                    return None;
                }
                continue;
            }
            let sextet = b64_value(*byte)?;
            value = (value << 6) | u32::from(sextet);
        }
        value <<= (pad * 6) as u32;
        out.push((value >> 16) as u8);
        if pad < 2 {
            out.push((value >> 8) as u8);
        }
        if pad == 0 {
            out.push(value as u8);
        }
    }
    Some(out)
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum Node {
    Varint(u64),
    Fixed32(u32),
    Fixed64(u64),
    Text(String),
    Bytes(Vec<u8>),
    Message(Vec<(u64, Node)>),
}

#[derive(Debug, Deserialize)]
struct StreamRow {
    record: Option<String>,
    metadata_b64: Option<String>,
    idx: Option<u64>,
    step_type: Option<i64>,
    status: Option<i64>,
    step_format: Option<i64>,
    payload_b64: Option<String>,
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

struct Analysis {
    classified: Vec<ClassifiedUnit>,
    turns: Vec<Turn>,
    tools: Vec<ToolEvent>,
    warnings: Vec<CoverageWarning>,
    provenance: Provenance,
    drafts: Vec<Draft>,
    calls: std::collections::BTreeMap<String, Call>,
    cascade_id: Option<String>,
    trajectory_id: Option<String>,
    trajectory_observed: bool,
    current_cwd: Known<String>,
    segment_started: Known<String>,
    seen_idx: std::collections::BTreeSet<u64>,
    last_idx: Option<u64>,
    saw_header: bool,
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
            "agy needs exactly one JSONL trajectory artifact",
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
        warnings: Vec::new(),
        drafts: Vec::new(),
        calls: std::collections::BTreeMap::new(),
        cascade_id: None,
        trajectory_id: None,
        trajectory_observed: false,
        current_cwd: Known::unknown(),
        segment_started: Known::unknown(),
        seen_idx: std::collections::BTreeSet::new(),
        last_idx: None,
        saw_header: false,
        visible_lost: false,
        unsupported_visible: false,
        malformed_tail: false,
        opaque_reasoning: false,
        provenance: Provenance {
            agent: AgentKind::Agy,
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
    for raw in &read.units {
        let (kind, disposition, pending) = classify_unit(&mut state, raw);
        let evidence = unit(source, raw, &store_id, &kind, &disposition)?;
        if let Some(pending) = pending {
            project_row(&mut state, evidence.clone(), &pending);
        }
        state.classified.push(ClassifiedUnit {
            ordinal: raw.coverage_ordinal,
            level: RawUnitLevel::Physical,
            evidence,
            disposition,
        });
    }
    if !state.saw_header && !read.units.is_empty() {
        state.warn(WarningKind::UnknownPayloadType, 1);
    }
    state.classified.sort_by_key(|unit| unit.ordinal);
    Ok(state)
}

struct Project {
    row: StreamRow,
    root: Option<Node>,
}

fn classify_unit(
    state: &mut Analysis,
    raw: &RawUnit,
) -> (String, ClassifiedDisposition, Option<Project>) {
    if raw.boundary == UnitBoundary::Oversized {
        state.visible_lost = true;
        state.warn(WarningKind::OversizedUnit, raw.coverage_ordinal);
        state.ensure_segment();
        if let Some(draft) = state.drafts.last_mut() {
            draft.workdirs.push(WorkdirEvidence::Opaque);
        }
        return (
            "oversized".to_owned(),
            skip(SkippedReason::Oversized, true),
            None,
        );
    }
    if raw.bytes.iter().all(u8::is_ascii_whitespace) {
        return (
            "blank".to_owned(),
            skip(SkippedReason::Unsupported, false),
            None,
        );
    }
    if raw.boundary == UnitBoundary::UnterminatedTail {
        state.malformed_tail = true;
        state.warn(WarningKind::UnterminatedTail, raw.coverage_ordinal);
    }
    let Ok(row) = serde_json::from_slice::<StreamRow>(&raw.bytes) else {
        state.visible_lost = true;
        state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
        return (
            "malformed".to_owned(),
            skip(SkippedReason::Malformed, true),
            None,
        );
    };
    match row.record.as_deref() {
        Some("header") => classify_header(state, raw, row),
        Some("step") => classify_step(state, raw, row),
        _ => {
            state.unsupported_visible = true;
            state.warn(WarningKind::UnknownPayloadType, raw.coverage_ordinal);
            (
                "unknown_record".to_owned(),
                skip(SkippedReason::UnknownPayloadType, true),
                None,
            )
        }
    }
}

fn classify_header(
    state: &mut Analysis,
    raw: &RawUnit,
    row: StreamRow,
) -> (String, ClassifiedDisposition, Option<Project>) {
    if state.saw_header {
        state.warn(WarningKind::UnknownPayloadType, raw.coverage_ordinal);
        return (
            "header:duplicate".to_owned(),
            consumed("header:duplicate"),
            None,
        );
    }
    state.saw_header = true;
    let payload = match row.metadata_b64.as_deref() {
        Some(text) => match decode_b64(text) {
            Some(bytes) => bytes,
            None => {
                state.visible_lost = true;
                state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
                return (
                    "header".to_owned(),
                    skip(SkippedReason::Malformed, true),
                    None,
                );
            }
        },
        None => Vec::new(),
    };
    let root = parse_message(&payload);
    if root.is_none() && !payload.is_empty() {
        state.visible_lost = true;
        state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
        return (
            "header".to_owned(),
            skip(SkippedReason::Malformed, true),
            None,
        );
    }
    (
        "header".to_owned(),
        consumed("header"),
        Some(Project { row, root }),
    )
}

fn classify_step(
    state: &mut Analysis,
    raw: &RawUnit,
    row: StreamRow,
) -> (String, ClassifiedDisposition, Option<Project>) {
    let Some(idx) = row.idx else {
        state.visible_lost = true;
        state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
        return (
            "malformed".to_owned(),
            skip(SkippedReason::Malformed, true),
            None,
        );
    };
    if state.last_idx.is_some_and(|last| idx < last) || !state.seen_idx.insert(idx) {
        state.visible_lost = true;
        state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
        return (
            "malformed".to_owned(),
            skip(SkippedReason::Malformed, true),
            None,
        );
    }
    state.last_idx = Some(idx);
    let Some(step_type) = row.step_type else {
        state.visible_lost = true;
        state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
        return (
            "malformed".to_owned(),
            skip(SkippedReason::Malformed, true),
            None,
        );
    };
    let kind = step_kind(step_type);
    if row.step_format.is_some_and(|format| format != 0) {
        state.visible_lost = true;
        state.unsupported_visible = true;
        state.warn(WarningKind::UnsupportedVisibleEvent, raw.coverage_ordinal);
        return (kind, skip(SkippedReason::Unsupported, true), None);
    }
    if row.step_format.is_none() || row.status.is_none() || row.payload_b64.as_deref() == Some("") {
        state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
    }
    let payload = match row.payload_b64.as_deref() {
        None | Some("") => Vec::new(),
        Some(text) => match decode_b64(text) {
            Some(bytes) => bytes,
            None => {
                state.visible_lost = true;
                state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
                return (kind, skip(SkippedReason::Malformed, true), None);
            }
        },
    };
    if payload.len() > MAX_PROTO_BYTES {
        state.visible_lost = true;
        state.warn(WarningKind::OversizedUnit, raw.coverage_ordinal);
        return (kind, skip(SkippedReason::Oversized, true), None);
    }
    let root = if payload.is_empty() {
        None
    } else {
        match parse_message(&payload) {
            Some(root) => Some(root),
            None => {
                state.visible_lost = true;
                state.warn(WarningKind::MalformedUnit, raw.coverage_ordinal);
                return (kind, skip(SkippedReason::Malformed, true), None);
            }
        }
    };
    if !matches!(step_type, USER_INPUT | PLANNER_RESPONSE | TOOL_CALL)
        && !BOOKKEEPING.contains(&step_type)
    {
        state.warn(WarningKind::UnknownPayloadType, raw.coverage_ordinal);
    }
    let disposition = consumed(&kind);
    (kind, disposition, Some(Project { row, root }))
}

fn project_row(state: &mut Analysis, evidence: RawUnitRef, project: &Project) {
    match project.row.record.as_deref() {
        Some("header") => {
            if let Some(root) = &project.root {
                apply_header(state, root);
            }
        }
        Some("step") => {
            let step_type = project.row.step_type.unwrap_or_default();
            let Some(root) = &project.root else {
                return;
            };
            let stamp = known_stamp(stamp_at(root, &[5, 1, 1], &[5, 1, 2]));
            state.note_time(&stamp);
            match step_type {
                USER_INPUT => {
                    if let Some(text) = text_at(root, &[19, 3, 1])
                        && !text.trim().is_empty()
                    {
                        if is_dispatch(&text) {
                            state.inject("dispatch_brief", text, stamp, evidence);
                        } else {
                            state.speech(TransportRole::User, text, stamp, evidence);
                        }
                    }
                }
                PLANNER_RESPONSE => {
                    if let Some(text) = assistant_prose(root) {
                        state.speech(TransportRole::Assistant, text, stamp, evidence);
                    }
                }
                TOOL_CALL => state.tool_call(root, stamp, evidence),
                _ => {}
            }
        }
        _ => {}
    }
}

fn apply_header(state: &mut Analysis, root: &Node) {
    if let Some(cascade) = text_at(root, &[6]).filter(|text| !text.is_empty()) {
        state.cascade_id = Some(cascade);
    }
    match text_at(root, &[3]).filter(|text| !text.is_empty()) {
        Some(trajectory) => {
            state.trajectory_id = Some(trajectory);
            state.trajectory_observed = true;
        }
        None => state.trajectory_observed = false,
    }
    if let Some(cwd) = text_at(root, &[7]).as_deref().and_then(workspace_cwd) {
        state.set_cwd(cwd);
    }
    if let Some(started) = stamp_at(root, &[2, 1], &[2, 2]) {
        let stamp = Known::value(started);
        if matches!(state.provenance.started_at, Known::Unknown(_)) {
            state.provenance.started_at = stamp.clone();
        }
        if matches!(state.segment_started, Known::Unknown(_)) {
            state.segment_started = stamp;
        }
    }
}

impl Analysis {
    fn tool_call(&mut self, root: &Node, stamp: Known<String>, evidence: RawUnitRef) {
        let id = text_at(root, &[5, 4, 1]).unwrap_or_else(|| "unknown".to_owned());
        let name = text_at(root, &[5, 4, 2]).unwrap_or_else(|| "unknown_tool".to_owned());
        let args_text = text_at(root, &[5, 4, 3]).unwrap_or_else(|| "{}".to_owned());
        let arguments = serde_json::from_str::<Value>(&args_text)
            .unwrap_or_else(|_| Value::String(args_text.clone()));
        let payload = value_text(&arguments);
        if let Some(previous) = self.calls.get(&id)
            && previous.payload == payload
            && previous.name == name
        {
            let turn = previous.turn;
            self.turns[turn].raw_unit_refs.push(evidence);
            return;
        }
        self.observe_workdirs(&name, &arguments, root);
        let command = shell_command(&name, &arguments);
        let frame_class = command.map(|command| {
            frames::classify(&TransportFrame {
                agent: AgentKind::Agy,
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
            stamp.clone(),
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
            raw_unit_refs: vec![evidence.clone()],
        });
        self.calls.insert(
            id.clone(),
            Call {
                name: name.clone(),
                payload,
                turn,
            },
        );
        if let Some(result) = text_at(root, &[140, 2, 1]).filter(|text| !text.trim().is_empty()) {
            let body = serde_json::json!({
                "toolCallId": id,
                "toolName": name,
                "content": result,
            });
            self.finish_tool(&id, &name, &body, stamp, evidence);
        } else if let Some(opaque) = node_at(root, &[5, 4, 7, 2, 1]).filter(|node| !node.is_text())
        {
            self.opaque_reasoning = true;
            self.warn(WarningKind::OpaqueReasoning, evidence.coverage_ordinal);
            self.tools.push(ToolEvent {
                kind: ToolEventKind::Result,
                turn_idx: turn as u64,
                tool_name: name,
                correlation_id: Known::value(id),
                payload_hash: sha256_hex(&opaque.opaque_bytes()),
                payload_bytes: opaque.opaque_len(),
                raw_unit_refs: vec![evidence],
            });
        }
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

    fn speech(
        &mut self,
        role: TransportRole,
        content: String,
        stamp: Known<String>,
        evidence: RawUnitRef,
    ) {
        let kind = if role == TransportRole::Assistant {
            TransportKind::AssistantMessage
        } else {
            TransportKind::DirectMessage
        };
        self.frame(
            kind,
            TransportPayload::Text { role, content },
            stamp,
            evidence,
        );
    }

    fn frame(
        &mut self,
        kind: TransportKind,
        payload: TransportPayload,
        stamp: Known<String>,
        evidence: RawUnitRef,
    ) {
        let classified = frames::classify(&TransportFrame {
            agent: AgentKind::Agy,
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
        let segment = &mut self.drafts.last_mut().expect("segment").segment;
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

    fn observe_workdirs(&mut self, name: &str, arguments: &Value, root: &Node) {
        let mut args = arguments.clone();
        if let Some(object) = args.as_object_mut()
            && !object.contains_key("workdir")
            && let Some(cwd) = object
                .get("cwd")
                .cloned()
                .or_else(|| object.get("Cwd").cloned())
        {
            object.insert("workdir".to_owned(), cwd);
        }
        if let Some(object) = args.as_object_mut()
            && !object.contains_key("workdir")
            && let Some(cwd) = text_at(root, &[140, 1]).as_deref().and_then(cwd_from_field)
        {
            object.insert("workdir".to_owned(), Value::String(cwd));
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

    fn set_cwd(&mut self, cwd: String) {
        if matches!(self.current_cwd, Known::Unknown(_)) {
            self.current_cwd = Known::value(cwd.clone());
            self.provenance.cwd = Known::value(cwd);
        }
        if let Some(draft) = self.drafts.last_mut()
            && matches!(draft.segment.cwd, Known::Unknown(_))
        {
            draft.segment.cwd = self.current_cwd.clone();
            draft.segment.scope_status =
                ScopeStatus::from_evidence(known_str(&self.current_cwd), None::<&str>);
        }
    }

    fn note_time(&mut self, stamp: &Known<String>) {
        let Known::Value(value) = stamp else {
            return;
        };
        if matches!(self.provenance.started_at, Known::Unknown(_)) {
            self.provenance.started_at = stamp.clone();
        }
        self.provenance.ended_at = stamp.clone();
        if matches!(self.segment_started, Known::Unknown(_)) {
            self.segment_started = Known::value(value.clone());
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
                        known_str(&self.current_cwd),
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
        let baseline = known_str(&draft.segment.cwd);
        let (scope, root) = effective_window_scope(&draft.workdirs, baseline);
        draft.segment.scope_workdirs = draft
            .workdirs
            .iter()
            .filter_map(WorkdirEvidence::path)
            .map(|path| recorded_workdir(path, baseline))
            .collect::<std::collections::BTreeSet<_>>()
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

    fn finish(mut self, source: &SourceHandle) -> UnvalidatedParse {
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
            consumed.len() as u64 + skipped.len() as u64,
            consumed.len() as u64 + skipped.len() as u64,
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
                    compaction_boundary_present: false,
                },
                malformed_tail_present: self.malformed_tail,
                visible_event_lost: self.visible_lost,
            },
        );
        let store_id = source
            .logical_session_id()
            .unwrap_or_else(|| source.source_id())
            .to_owned();
        let mut model = SessionModel::new(store_id.clone(), self.provenance, coverage);
        let unobserved = if self.trajectory_observed {
            Vec::new()
        } else {
            vec!["trajectory_id".to_owned()]
        };
        model.conversation = ProviderConversationRef::Agy {
            session_id: self.cascade_id.unwrap_or(store_id),
            trajectory_id: self.trajectory_id,
            unobserved,
        };
        model.turns = self.turns;
        model.tool_events = self.tools;
        model.segments = self
            .drafts
            .into_iter()
            .filter(|draft| draft.segment.turn_range.start < model.turns.len() as u64)
            .map(|draft| draft.segment)
            .collect();
        UnvalidatedParse::from_model(model)
    }
}

fn step_kind(step_type: i64) -> String {
    match step_type {
        USER_INPUT => "user_input".to_owned(),
        PLANNER_RESPONSE => "planner_response".to_owned(),
        TOOL_CALL => "tool_call".to_owned(),
        other if BOOKKEEPING.contains(&other) => format!("bookkeeping:{other}"),
        other => format!("unknown_step:{other}"),
    }
}

fn assistant_prose(root: &Node) -> Option<String> {
    text_at(root, &[20, 1])
        .filter(|text| !text.is_empty())
        .or_else(|| text_at(root, &[20, 3]).filter(|text| !text.is_empty()))
}

fn is_dispatch(text: &str) -> bool {
    let trimmed = text.trim();
    DISPATCH_HEADS.iter().any(|head| trimmed.starts_with(head))
}

fn shell_command(name: &str, arguments: &Value) -> Option<String> {
    if !SHELL_TOOLS.contains(&name) {
        return None;
    }
    let parsed = if let Some(raw) = arguments.as_str() {
        serde_json::from_str::<Value>(raw).unwrap_or_else(|_| arguments.clone())
    } else {
        arguments.clone()
    };
    parsed
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.is_empty())
        .map(str::to_owned)
}

fn cwd_from_field(text: &str) -> Option<String> {
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(text) {
        return map
            .get("Cwd")
            .or_else(|| map.get("cwd"))
            .or_else(|| map.get("workdir"))
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.is_empty())
            .map(str::to_owned);
    }
    let trimmed = text.trim();
    (!trimmed.is_empty() && trimmed.starts_with('/')).then(|| trimmed.to_owned())
}

fn workspace_cwd(raw: &str) -> Option<String> {
    let rest = raw.trim().strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest);
    decoded.starts_with('/').then_some(decoded)
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(hi), Some(lo)) =
                (hex_nibble(bytes[index + 1]), hex_nibble(bytes[index + 2]))
        {
            out.push((hi << 4) | lo);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn stamp_at(root: &Node, seconds: &[u64], nanos: &[u64]) -> Option<String> {
    let seconds = varint_at(root, seconds)?;
    let nanos = varint_at(root, nanos).unwrap_or(0);
    rfc3339(seconds, nanos)
}

fn rfc3339(seconds: u64, nanos: u64) -> Option<String> {
    let nanos = u32::try_from(nanos)
        .ok()
        .filter(|value| *value < 1_000_000_000)?;
    let seconds = i64::try_from(seconds).ok()?;
    chrono::DateTime::from_timestamp(seconds, nanos)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
}

fn known_stamp(stamp: Option<String>) -> Known<String> {
    stamp.map(Known::value).unwrap_or_else(Known::unknown)
}

fn text_at(root: &Node, path: &[u64]) -> Option<String> {
    match node_at(root, path)? {
        Node::Text(text) => Some(text.clone()),
        _ => None,
    }
}

fn varint_at(root: &Node, path: &[u64]) -> Option<u64> {
    match node_at(root, path)? {
        Node::Varint(value) => Some(*value),
        _ => None,
    }
}

fn node_at<'a>(root: &'a Node, path: &[u64]) -> Option<&'a Node> {
    let mut current = root;
    for number in path {
        let Node::Message(fields) = current else {
            return None;
        };
        current = fields
            .iter()
            .find(|(field, _)| field == number)
            .map(|(_, node)| node)?;
    }
    Some(current)
}

impl Node {
    fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }

    fn opaque_len(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::Text(text) => text.len() as u64,
            Self::Message(fields) => fields.len() as u64,
            Self::Varint(_) | Self::Fixed32(_) | Self::Fixed64(_) => 0,
        }
    }

    fn opaque_bytes(&self) -> Vec<u8> {
        match self {
            Self::Bytes(bytes) => bytes.clone(),
            Self::Text(text) => text.as_bytes().to_vec(),
            _ => Vec::new(),
        }
    }
}

fn parse_message(bytes: &[u8]) -> Option<Node> {
    if bytes.len() > MAX_PROTO_BYTES {
        return None;
    }
    parse_fields(bytes, 0).map(Node::Message)
}

fn parse_fields(bytes: &[u8], depth: usize) -> Option<Vec<(u64, Node)>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let mut fields = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let (key, used) = read_varint(&bytes[index..])?;
        index += used;
        let field = key >> 3;
        let wire = key & 7;
        if field == 0 {
            return None;
        }
        let node = match wire {
            0 => {
                let (value, used) = read_varint(&bytes[index..])?;
                index += used;
                Node::Varint(value)
            }
            1 => {
                let end = index.checked_add(8)?;
                if end > bytes.len() {
                    return None;
                }
                let mut wide = [0u8; 8];
                wide.copy_from_slice(&bytes[index..end]);
                index = end;
                Node::Fixed64(u64::from_le_bytes(wide))
            }
            2 => {
                let (len, used) = read_varint(&bytes[index..])?;
                index += used;
                let len = usize::try_from(len).ok()?;
                let end = index.checked_add(len)?;
                if end > bytes.len() || len > MAX_PROTO_BYTES {
                    return None;
                }
                let slice = &bytes[index..end];
                index = end;
                decode_length_delimited(slice, depth + 1)
            }
            5 => {
                let end = index.checked_add(4)?;
                if end > bytes.len() {
                    return None;
                }
                let mut narrow = [0u8; 4];
                narrow.copy_from_slice(&bytes[index..end]);
                index = end;
                Node::Fixed32(u32::from_le_bytes(narrow))
            }
            _ => return None,
        };
        fields.push((field, node));
    }
    Some(fields)
}

fn decode_length_delimited(bytes: &[u8], depth: usize) -> Node {
    if depth > MAX_DEPTH {
        return Node::Bytes(bytes.to_vec());
    }
    let text = std::str::from_utf8(bytes)
        .ok()
        .filter(|text| prefer_text(text));
    let message = parse_fields(bytes, depth).filter(|fields| !fields.is_empty());
    match (text, message) {
        (Some(text), Some(fields)) if message_beats_text(text, &fields) => Node::Message(fields),
        (Some(text), _) => Node::Text(text.to_owned()),
        (None, Some(fields)) => Node::Message(fields),
        (None, None) => Node::Bytes(bytes.to_vec()),
    }
}

/// A short ASCII wrapper (`field 1` + a one-byte length + prose) is valid
/// UTF-8 and also a message. Live tool results under 128 bytes are that
/// shape. Keep the message so `140.2.1` and `19.3.1` stay addressable.
/// A leaf whose own bytes are the prose does not parse as that wrapper, so
/// it stays text.
fn message_beats_text(outer: &str, fields: &[(u64, Node)]) -> bool {
    if fields.len() != 1 {
        return true;
    }
    match &fields[0].1 {
        Node::Text(inner) => inner.as_str() != outer,
        Node::Bytes(bytes) => std::str::from_utf8(bytes).ok() != Some(outer),
        Node::Message(_) | Node::Varint(_) | Node::Fixed32(_) | Node::Fixed64(_) => true,
    }
}

fn prefer_text(text: &str) -> bool {
    if text.is_empty() || !text_like(text) {
        return false;
    }
    let letters = text.chars().filter(char::is_ascii_alphabetic).count();
    text.contains([' ', '\n', '\t'])
        || text.starts_with('{')
        || text.starts_with('[')
        || letters.saturating_mul(2) > text.len()
        || text.contains('-')
        || text.contains('_')
}

fn text_like(text: &str) -> bool {
    text.chars()
        .all(|ch| matches!(ch, '\n' | '\r' | '\t' | ' '..='~' | '\u{00a0}'..='\u{ffff}'))
        && !text.chars().any(|ch| ch == '\u{fffd}')
}

fn read_varint(data: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0;
    for (index, byte) in data.iter().copied().enumerate() {
        if index >= 10 {
            return None;
        }
        let bits = u64::from(byte & 0x7f);
        if shift == 63 && bits > 1 {
            return None;
        }
        value |= bits << shift;
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
    None
}

fn consumed(kind: &str) -> ClassifiedDisposition {
    ClassifiedDisposition::Consumed {
        kind: kind.to_owned(),
    }
}

fn skip(reason: SkippedReason, visible: bool) -> ClassifiedDisposition {
    ClassifiedDisposition::Skipped { reason, visible }
}

fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn known_str(value: &Known<String>) -> Option<&str> {
    match value {
        Known::Value(value) => Some(value.as_str()),
        Known::Unknown(_) => None,
    }
}

fn unit(
    source: &SourceHandle,
    raw: &RawUnit,
    session_id: &str,
    kind: &str,
    disposition: &ClassifiedDisposition,
) -> Result<RawUnitRef, AdapterError> {
    let _ = disposition;
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
    Ok(RawUnitRef {
        evidence_event_id,
        coverage_ordinal: raw.coverage_ordinal,
        physical_ordinal: raw.physical_ordinal,
        locator,
        unit_kind: kind,
        artifact: raw.artifact_name.clone(),
        content_hash: raw.content_hash.clone(),
        original_bytes: raw.original_bytes,
    })
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

fn push_b64(out: &mut String, chunk: u32, count: usize) {
    // Sextets come off the top of the 24-bit chunk. A partial quantum (one or
    // two leftover bytes) still starts at bit 18. Counting down from `count`
    // drops that high sextet and invents a low one, so every payload whose
    // length is not a multiple of 3 round-trips into different bytes. User
    // text sits at the end of the step, and those corrupted tail bytes stop
    // being UTF-8, so the step is consumed with no speech.
    for shift in (4 - count..4).rev() {
        let index = ((chunk >> (shift * 6)) & 0x3f) as usize;
        out.push(B64[index] as char);
    }
}

fn b64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}
