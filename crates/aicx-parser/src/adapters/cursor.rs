//! Cursor adapter implementation for the deterministic parser kernel.
//!
//! Source: `~/.cursor/projects/<project-slug>/agent-transcripts/<uuid>/<uuid>.jsonl`.
//! Three record shapes exist on the wire:
//! `{"role":"user","message":{"content":[...]}}`,
//! `{"role":"assistant","message":{"content":[...]}}`, and
//! `{"type":"turn_ended","status":"..."}` control rows.
//!
//! The transport carries no in-band session id, cwd, model, usage, or per-row
//! timestamps. Identity is the store-side UUID (catalog concern); the only
//! wall-clock evidence is the harness `<timestamp>` wrapper inside operator
//! text, which this adapter parses into turn timestamps when present.
//! `<user_query>` wraps operator speech and is unwrapped before framing —
//! it is never an inject.

use super::{
    AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel, sealed,
};
use crate::engine::frames::{self, TransportFrame, TransportKind, TransportPayload, TransportRole};
use crate::engine::{
    AgentKind, BoundaryFlags, ConsumedUnit, CoverageReport, CoverageWarning, Known, ParseStatus,
    Provenance, RawUnit, RawUnitRef, Segment, SessionModel, SkippedReason, SkippedUnit,
    SourceHandle, SourceRead, ToolEvent, ToolEventKind, Turn, TurnKind, TurnRange, TurnRole,
    UnitBoundary, UnvalidatedParse, VisibleCompleteness, WarningKind, evidence_event_id_from_hash,
    ordinal_locator, sha256_hex,
};
use serde_json::Value;
use std::collections::HashMap;

pub const CURSOR_ADAPTER_VERSION: &str = "cursor-native-v1";

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
        let session_id = session_id(source);
        let mut classified = Vec::with_capacity(read.units.len());
        for raw in &read.units {
            let (unit_kind, disposition) = classify_raw(raw);
            let evidence = raw_evidence(source.agent(), &session_id, raw, &unit_kind)?;
            classified.push(ClassifiedUnit {
                ordinal: raw.coverage_ordinal,
                level: RawUnitLevel::Physical,
                evidence,
                disposition,
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
        assemble_cursor(source, read, classified)
    }
}

fn classify_raw(raw: &RawUnit) -> (String, ClassifiedDisposition) {
    if raw.boundary == UnitBoundary::Oversized {
        return (
            "oversized".to_owned(),
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::Oversized,
                visible: true,
            },
        );
    }

    let Ok(value) = serde_json::from_slice::<Value>(&raw.bytes) else {
        return (
            "malformed".to_owned(),
            ClassifiedDisposition::Skipped {
                reason: SkippedReason::Malformed,
                visible: true,
            },
        );
    };

    match value.get("role").and_then(Value::as_str) {
        Some("user") => (
            "user_message".to_owned(),
            ClassifiedDisposition::Consumed {
                kind: "user_message".to_owned(),
            },
        ),
        Some("assistant") => (
            "assistant_message".to_owned(),
            ClassifiedDisposition::Consumed {
                kind: "assistant_message".to_owned(),
            },
        ),
        _ => match value.get("type").and_then(Value::as_str) {
            Some("turn_ended") => (
                "turn_control".to_owned(),
                ClassifiedDisposition::Consumed {
                    kind: "turn_control".to_owned(),
                },
            ),
            _ => (
                "unknown_payload".to_owned(),
                ClassifiedDisposition::Skipped {
                    reason: SkippedReason::UnknownPayloadType,
                    visible: true,
                },
            ),
        },
    }
}

fn assemble_cursor(
    source: &SourceHandle,
    read: &SourceRead,
    classified: Vec<ClassifiedUnit>,
) -> Result<UnvalidatedParse, AdapterError> {
    let session_id = session_id(source);
    let mut state = Assembly::new(read, session_id);

    // Index once by ordinal: transcripts run to thousands of lines and a
    // per-unit linear scan turns assembly quadratic.
    let mut by_ordinal: HashMap<u64, &ClassifiedUnit> = HashMap::with_capacity(classified.len());
    for item in &classified {
        if matches!(item.level, RawUnitLevel::Physical) {
            by_ordinal.insert(item.ordinal, item);
        }
    }
    for unit in &read.units {
        let Some(classified_unit) = by_ordinal.get(&unit.coverage_ordinal).copied() else {
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
                state.observe_skip(classified_unit, *reason, *visible);
            }
        }
    }

    drop(by_ordinal);
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
    started_at: Known<String>,
    ended_at: Known<String>,
    turns: Vec<Turn>,
    tools: Vec<ToolEvent>,
    warnings: Vec<CoverageWarning>,
    unsupported_visible: bool,
    malformed_tail: bool,
    visible_lost: bool,
}

impl<'a> Assembly<'a> {
    fn new(read: &'a SourceRead, session_id: String) -> Self {
        Self {
            read,
            session_id,
            started_at: Known::unknown(),
            ended_at: Known::unknown(),
            turns: Vec::new(),
            tools: Vec::new(),
            warnings: Vec::new(),
            unsupported_visible: false,
            malformed_tail: false,
            visible_lost: false,
        }
    }

    fn consume_physical(&mut self, kind: &str, value: &Value, evidence: RawUnitRef) {
        match kind {
            "user_message" => self.consume_message(value, TransportRole::User, evidence),
            "assistant_message" => self.consume_message(value, TransportRole::Assistant, evidence),
            // turn_ended: known control row, accounted in coverage, no turn.
            _ => {}
        }
    }

    fn consume_message(&mut self, value: &Value, role: TransportRole, evidence: RawUnitRef) {
        let ordinal = evidence.coverage_ordinal;
        let Some(blocks) = value
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            // A consumed user/assistant unit with no content array lost its
            // whole visible payload — whether `message` is malformed or
            // absent entirely. Never a silent CompleteVisible.
            self.lose_visible(ordinal);
            return;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    // Fail closed: a text block without a string `text` field is
                    // a malformed visible shape, not an empty utterance.
                    let Some(text) = block.get("text").and_then(Value::as_str) else {
                        self.lose_visible(ordinal);
                        continue;
                    };
                    match role {
                        TransportRole::User => self.consume_operator_text(text, evidence.clone()),
                        _ => self.push_classified_frame(
                            Known::unknown(),
                            TransportKind::AssistantMessage,
                            TransportPayload::Text {
                                role: TransportRole::Assistant,
                                content: text.to_owned(),
                            },
                            evidence.clone(),
                        ),
                    }
                }
                Some("tool_use") => {
                    self.consume_tool_use(block, evidence.clone());
                }
                _ => {
                    self.lose_visible(ordinal);
                }
            }
        }
    }

    /// Record a lost visible payload honestly: warning at the real ordinal,
    /// coverage flags flipped, no fabricated turn.
    fn lose_visible(&mut self, ordinal: u64) {
        self.warn(WarningKind::UnsupportedVisibleEvent, ordinal);
        self.unsupported_visible = true;
        self.visible_lost = true;
    }

    fn consume_operator_text(&mut self, text: &str, evidence: RawUnitRef) {
        let split = split_operator_text(text);
        if let Known::Value(timestamp) = &split.timestamp {
            if matches!(self.started_at, Known::Unknown(_)) {
                self.started_at = Known::value(timestamp.clone());
            }
            self.ended_at = Known::value(timestamp.clone());
        }
        for (tag, content) in split.injects {
            self.push_classified_frame(
                split.timestamp.clone(),
                TransportKind::InjectedContext,
                TransportPayload::Inject { tag, content },
                evidence.clone(),
            );
        }
        if !split.human.is_empty() {
            self.push_classified_frame(
                split.timestamp,
                TransportKind::DirectMessage,
                TransportPayload::Text {
                    role: TransportRole::User,
                    content: split.human,
                },
                evidence,
            );
        }
    }

    fn consume_tool_use(&mut self, block: &Value, evidence: RawUnitRef) {
        // Fail closed: a tool_use block without a string `name`, or a Shell
        // call without a string `command`, is a malformed visible shape.
        // Account the loss instead of fabricating defaults.
        let Some(tool_name) = block.get("name").and_then(Value::as_str) else {
            self.lose_visible(evidence.coverage_ordinal);
            return;
        };
        let tool_name = tool_name.to_owned();
        // Missing `input` is a malformed tool_use, not an implicit null call —
        // serializing a fabricated Null would hash "null" as payload truth.
        let Some(input) = block.get("input").cloned() else {
            self.lose_visible(evidence.coverage_ordinal);
            return;
        };
        let text = if tool_name == "Shell" {
            let Some(command) = input.get("command").and_then(Value::as_str) else {
                self.lose_visible(evidence.coverage_ordinal);
                return;
            };
            command.to_owned()
        } else {
            serde_json::to_string(&input).unwrap_or_default()
        };
        let turn_idx = self.turns.len() as u64;
        let text_hash = sha256_hex(text.as_bytes());
        let payload_bytes = text.len() as u64;
        self.turns.push(Turn {
            turn_idx,
            role: TurnRole::Tool,
            timestamp: Known::unknown(),
            kind: TurnKind::ToolCall,
            text_chars: text.chars().count() as u64,
            text,
            text_hash: text_hash.clone(),
            tool_name: Known::value(tool_name.clone()),
            segment_id: 0,
            raw_unit_refs: vec![evidence.clone()],
            frame_class: None,
        });
        self.tools.push(ToolEvent {
            kind: ToolEventKind::Call,
            turn_idx,
            tool_name,
            correlation_id: Known::unknown(),
            payload_hash: text_hash,
            payload_bytes,
            raw_unit_refs: vec![evidence],
        });
    }

    fn push_classified_frame(
        &mut self,
        timestamp: Known<String>,
        transport_kind: TransportKind,
        payload: TransportPayload,
        evidence: RawUnitRef,
    ) {
        let classified = frames::classify(&TransportFrame {
            agent: AgentKind::Cursor,
            transport_kind,
            timestamp,
            payload,
            evidence,
        });
        // Role and lane are the throne's (W2-R1); the class rides on the turn.
        let role = classified.class.turn_role();
        let Some(kind) = classified.turn_kind else {
            return;
        };
        let text = classified.content;
        let text_hash = sha256_hex(text.as_bytes());
        self.turns.push(Turn {
            turn_idx: self.turns.len() as u64,
            role,
            timestamp: classified.seal.seal_ts,
            kind,
            text_chars: text.chars().count() as u64,
            text,
            text_hash,
            tool_name: Known::unknown(),
            segment_id: 0,
            raw_unit_refs: vec![classified.origin.evidence],
            frame_class: Some(classified.class),
        });
    }

    fn observe_skip(&mut self, unit: &ClassifiedUnit, reason: SkippedReason, visible: bool) {
        let warning = match reason {
            SkippedReason::UnknownPayloadType => WarningKind::UnknownPayloadType,
            SkippedReason::Malformed => WarningKind::MalformedUnit,
            SkippedReason::Oversized => WarningKind::OversizedUnit,
            SkippedReason::EncryptedOpaque => WarningKind::OpaqueReasoning,
            SkippedReason::Unsupported => WarningKind::UnsupportedVisibleEvent,
            SkippedReason::CompactionReplay | SkippedReason::DuplicateBody => return,
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
        // UnterminatedTail flags/warning are handled once in the assembly
        // loop for every unit (consumed and skipped alike) — warning again
        // here double-counted skipped tails.
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

    fn finish(self, classified: Vec<ClassifiedUnit>) -> SessionModel {
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
        let mut warnings = self.warnings;
        warnings.sort_by_key(|warning| warning.first_ordinal);
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
            warnings,
            ParseStatus {
                visible_completeness,
                boundary_flags: BoundaryFlags {
                    opaque_reasoning_present: false,
                    unsupported_visible_event: self.unsupported_visible,
                    compaction_boundary_present: false,
                },
                malformed_tail_present: self.malformed_tail,
                visible_event_lost: self.visible_lost,
            },
        );
        let provenance = Provenance {
            agent: AgentKind::Cursor,
            model: Known::unknown(),
            cli_version: Known::unknown(),
            cwd: Known::unknown(),
            branch: Known::unknown(),
            started_at: self.started_at,
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
                scope_status: crate::engine::ScopeStatus::from_evidence(None, None),
                cwd: Known::unknown(),
                branch: Known::unknown(),
                started_at: model.provenance.started_at.clone(),
                ended_at: model.provenance.ended_at.clone(),
                turn_range: TurnRange {
                    start: 0,
                    end: model.turns.len() as u64 - 1,
                },
            });
        }
        model
    }
}

/// Split one operator text block into wall-clock evidence, harness injects,
/// and actual speech. Recognized wrappers: `<timestamp>` (parsed into the
/// turn timestamp), `<user_query>` (unwrapped to speech), plus inject tags
/// owned by `frames_rules`. Unrecognized tags stay text — a mention in prose
/// is content, not transport.
struct OperatorSplit {
    timestamp: Known<String>,
    injects: Vec<(String, String)>,
    human: String,
}

const WRAPPER_TAGS: &[&str] = &[
    "timestamp",
    "user_query",
    "system_notification",
    "system_reminder",
    "system-reminder",
    "manually_attached_skills",
];

fn split_operator_text(text: &str) -> OperatorSplit {
    let mut timestamp = Known::unknown();
    let mut injects: Vec<(String, String)> = Vec::new();
    let mut human_parts: Vec<String> = Vec::new();
    let mut rest = text;

    while let Some(found) = find_next_wrapper(rest) {
        let (start, tag, inner, end) = found;
        let before = rest[..start].trim();
        if !before.is_empty() {
            human_parts.push(before.to_owned());
        }
        match tag {
            "timestamp" => {
                if let Some(parsed) = parse_harness_timestamp(inner) {
                    timestamp = Known::value(parsed);
                }
            }
            "user_query" => {
                let inner = inner.trim();
                if !inner.is_empty() {
                    human_parts.push(inner.to_owned());
                }
            }
            "manually_attached_skills" => {
                injects.push(("manually_attached_skills".to_owned(), inner.to_owned()));
            }
            other => {
                injects.push((other.to_owned(), inner.to_owned()));
            }
        }
        rest = &rest[end..];
    }
    let tail = rest.trim();
    if !tail.is_empty() {
        human_parts.push(tail.to_owned());
    }

    OperatorSplit {
        timestamp,
        injects,
        human: human_parts.join("\n"),
    }
}

/// Locate the next recognized `<tag>...</tag>` span. Returns the byte range
/// of the whole span, the tag, and the inner body.
fn find_next_wrapper(text: &str) -> Option<(usize, &str, &str, usize)> {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'<' {
            at += 1;
            continue;
        }
        let name_start = at + 1;
        let mut name_end = name_start;
        while name_end < bytes.len()
            && (bytes[name_end].is_ascii_alphanumeric()
                || bytes[name_end] == b'_'
                || bytes[name_end] == b'-')
        {
            name_end += 1;
        }
        if name_end == name_start || bytes.get(name_end) != Some(&b'>') {
            at += 1;
            continue;
        }
        let tag = &text[name_start..name_end];
        if !WRAPPER_TAGS.contains(&tag) {
            at = name_end + 1;
            continue;
        }
        let closing = format!("</{tag}>");
        let body_start = name_end + 1;
        // An unclosed recognized tag is content, not transport: skip just this
        // opening tag and keep scanning — aborting here would disable wrapper
        // parsing for the rest of the text.
        let Some(close_at) = text[body_start..].find(closing.as_str()) else {
            at = name_end + 1;
            continue;
        };
        let body_end = body_start + close_at;
        return Some((
            at,
            tag,
            &text[body_start..body_end],
            body_end + closing.len(),
        ));
    }
    None
}

/// Parse the harness timestamp idiom: `Saturday, Aug 29, 2026, 8:50 AM (UTC+2)`
/// → RFC 3339. Anything off-shape is `None` — never a guess.
fn parse_harness_timestamp(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let without_weekday = match trimmed.split_once(',') {
        Some((head, tail)) if is_weekday(head.trim()) => tail.trim(),
        _ => trimmed,
    };
    let (date_time, zone) = without_weekday.split_once('(')?;
    let offset = parse_utc_offset(zone.trim_end_matches(')').trim())?;

    let mut parts = date_time.trim().split(',');
    let month_day = parts.next()?.trim();
    let year: u32 = parts.next()?.trim().parse().ok()?;
    let time_ampm = parts.next()?.trim();
    if parts.next().is_some() {
        return None;
    }

    let (month_name, day_text) = month_day.split_once(' ')?;
    let month = month_number(month_name)?;
    let day: u32 = day_text.trim().parse().ok()?;
    if day == 0 || day > days_in_month(year, month) {
        return None;
    }

    let (clock, meridiem) = time_ampm.rsplit_once(' ')?;
    let (hour_text, minute_text) = clock.split_once(':')?;
    let mut hour: u32 = hour_text.parse().ok()?;
    let minute: u32 = minute_text.parse().ok()?;
    if minute > 59 {
        return None;
    }
    // 12-hour clock: only 1..=12 make sense next to AM/PM ("23:59 PM" is
    // off-shape, never a guess).
    if !(1..=12).contains(&hour) {
        return None;
    }
    match meridiem {
        "AM" => {
            if hour == 12 {
                hour = 0;
            }
        }
        "PM" => {
            if hour != 12 {
                hour += 12;
            }
        }
        _ => return None,
    }

    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:00{offset}"
    ))
}

fn is_weekday(value: &str) -> bool {
    matches!(
        value,
        "Monday" | "Tuesday" | "Wednesday" | "Thursday" | "Friday" | "Saturday" | "Sunday"
    )
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap =
                (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

fn month_number(name: &str) -> Option<u32> {
    match name {
        "Jan" => Some(1),
        "Feb" => Some(2),
        "Mar" => Some(3),
        "Apr" => Some(4),
        "May" => Some(5),
        "Jun" => Some(6),
        "Jul" => Some(7),
        "Aug" => Some(8),
        "Sep" => Some(9),
        "Oct" => Some(10),
        "Nov" => Some(11),
        "Dec" => Some(12),
        _ => None,
    }
}

fn parse_utc_offset(zone: &str) -> Option<String> {
    let rest = zone.strip_prefix("UTC")?;
    if rest.is_empty() {
        return Some("+00:00".to_owned());
    }
    let (sign, digits) = match rest.as_bytes().first()? {
        b'+' => ("+", &rest[1..]),
        b'-' => ("-", &rest[1..]),
        _ => return None,
    };
    let (hours, minutes) = match digits.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None => (digits, "0"),
    };
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    // Real UTC offsets top out at ±14:00 exactly — 14:59 is off-shape.
    if hours > 14 || minutes > 59 || (hours == 14 && minutes > 0) {
        return None;
    }
    Some(format!("{sign}{hours:02}:{minutes:02}"))
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

fn session_id(source: &SourceHandle) -> String {
    source
        .logical_session_id()
        .unwrap_or_else(|| source.source_id())
        .to_owned()
}
