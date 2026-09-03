//! Grok adapter implementation for the deterministic parser kernel (C2G).
//!
//! Implements Grok session layouts (summary.json + chat_history.jsonl primarily,
//! plus support for events/hunk variants) as first-class adapter on the frozen
//! SourceHandle + AgentAdapter contract.
//!
//! - No global discovery; operates only on the explicit artifacts in SourceHandle.
//! - Separate from Codex; re-derives Grok-specific shapes from historical
//!   the historical Grok chat-history shape plus summary and tool extensions.
//! - Produces deterministic SessionModel with stable evidence ids, coverage,
//!   ParseStatus per C0A contract.
//!
//! Receipts: every consumed/skipped decision and metadata extraction is
//! explicitly classified; tool records, title, cwd, model, timestamps are
//! populated from native Grok artifacts when present.
//!
//! Speech class is not decided here. This adapter normalizes Grok transport
//! records into [`crate::engine::frames::TransportFrame`] values and consumes
//! [`crate::engine::frames::classify`] output. Local `type=user` → `UserMsg`
//! mapping is removed, not wrapped.

use crate::adapters::{
    AdapterError, AgentAdapter, ClassifiedDisposition, ClassifiedUnit, RawUnitLevel, sealed::Sealed,
};
use crate::engine::frames::{
    self, FrameClass, TransportFrame, TransportKind, TransportPayload, TransportRole,
};
use crate::engine::{
    AgentKind, BoundaryFlags, CoverageReport, ParseStatus, RawUnit, SkippedReason, SourceFraming,
    SourceHandle, SourceRead, UnvalidatedParse, VisibleCompleteness,
    identity::{evidence_event_id_from_hash, ordinal_locator, sha256_hex},
    model::{
        Known, Provenance, RawUnitRef, Segment, SessionModel, ToolEvent, ToolEventKind, Turn,
        TurnRange, UsageEvent,
    },
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::Value;
use std::collections::BTreeMap;

/// Adapter version for this cut. Bumped only on contract-visible behavior changes.
const ADAPTER_VERSION: &str = "throne-w2-t9-2026-08-27";

pub struct GrokAdapter;

impl Sealed for GrokAdapter {}

impl AgentAdapter for GrokAdapter {
    fn agent(&self) -> AgentKind {
        AgentKind::Grok
    }

    fn adapter_version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn classify(
        &self,
        _source: &SourceHandle,
        read: &SourceRead,
    ) -> Result<Vec<ClassifiedUnit>, AdapterError> {
        // Explicit contract: only the provided read.units; zero discovery.
        let mut out = Vec::with_capacity(read.units.len());

        for unit in read.units.iter() {
            let ordinal = unit.coverage_ordinal;
            let text = std::str::from_utf8(&unit.bytes).unwrap_or("");
            let trimmed = text.trim();

            let disposition = if unit.boundary == crate::engine::UnitBoundary::Oversized {
                ClassifiedDisposition::Skipped {
                    reason: SkippedReason::Oversized,
                    visible: true,
                }
            } else if trimmed.is_empty() {
                ClassifiedDisposition::Skipped {
                    reason: SkippedReason::Malformed,
                    visible: false,
                }
            } else if unit.artifact_name == "summary.json"
                || unit.artifact_name.ends_with("/summary.json")
                || unit.artifact_name.ends_with("summary.json")
            {
                ClassifiedDisposition::Consumed {
                    kind: "summary".to_string(),
                }
            } else if matches!(unit.framing, SourceFraming::JsonLines)
                || unit.artifact_name.ends_with(".jsonl")
            {
                match classify_grok_line(trimmed) {
                    Ok(kind) => ClassifiedDisposition::Consumed { kind },
                    Err(skip_reason) => ClassifiedDisposition::Skipped {
                        reason: skip_reason,
                        visible: true,
                    },
                }
            } else if unit.artifact_name.ends_with(".json") || unit.artifact_name.contains("event")
            {
                match classify_grok_line(trimmed) {
                    Ok(kind) => ClassifiedDisposition::Consumed { kind },
                    Err(_) => match classify_grok_event(trimmed) {
                        Ok(kind) => ClassifiedDisposition::Consumed { kind },
                        Err(skip_reason) => ClassifiedDisposition::Skipped {
                            reason: skip_reason,
                            visible: matches!(skip_reason, SkippedReason::UnknownPayloadType),
                        },
                    },
                }
            } else {
                ClassifiedDisposition::Skipped {
                    reason: SkippedReason::Unsupported,
                    visible: false,
                }
            };

            let unit_kind = match &disposition {
                ClassifiedDisposition::Consumed { kind } => kind.clone(),
                ClassifiedDisposition::Skipped { .. } => "grok-skipped".to_string(),
            };
            let evidence = make_evidence_ref(_source, unit, ordinal, &unit_kind);
            out.push(ClassifiedUnit {
                ordinal,
                level: RawUnitLevel::Physical,
                evidence,
                disposition,
            });
        }

        Ok(out)
    }

    fn assemble(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
        classified: Vec<ClassifiedUnit>,
    ) -> Result<UnvalidatedParse, AdapterError> {
        let logical_id = source
            .logical_session_id()
            .map(|s| s.to_owned())
            .unwrap_or_else(|| source.source_id().to_owned());

        // Collect raw units by their assigned coverage_ordinal (1-based from reader).
        let units_by_ord: BTreeMap<u64, &RawUnit> =
            read.units.iter().map(|u| (u.coverage_ordinal, u)).collect();

        // Extract summary metadata if present.
        let (
            mut cwd,
            mut branch,
            mut model_id,
            mut title,
            mut started_at,
            mut ended_at,
            mut git_root,
        ) = (None, None, None, None, None, None, None);

        let mut chat_lines: Vec<(u64, Value)> = Vec::new(); // (ordinal, parsed json)
        let mut event_lines: Vec<(u64, Value)> = Vec::new();

        for cu in &classified {
            if let Some(unit) = units_by_ord.get(&cu.ordinal) {
                let text = std::str::from_utf8(&unit.bytes).unwrap_or("").trim();
                if text.is_empty() {
                    continue;
                }
                if cu
                    .disposition
                    .as_consumed_kind()
                    .map(|k| k == "summary")
                    .unwrap_or(false)
                {
                    if let Ok(v) = serde_json::from_str::<Value>(text) {
                        // Real Grok summary.json mixes a small `info` object
                        // (id, cwd) with top-level session fields (created_at,
                        // current_model_id, git_root_dir, head_branch, …).
                        let info = v.get("info");
                        let pick = |key: &str| -> Option<String> {
                            info.and_then(|i| i.get(key))
                                .and_then(|x| x.as_str())
                                .or_else(|| v.get(key).and_then(|x| x.as_str()))
                                .map(|s| s.to_owned())
                        };
                        cwd = pick("cwd").or_else(|| pick("git_root_dir"));
                        branch = pick("head_branch");
                        model_id = pick("current_model_id");
                        title = pick("session_summary").or_else(|| pick("generated_title"));
                        started_at = pick("created_at");
                        ended_at = pick("updated_at").or_else(|| pick("last_active_at"));
                        git_root = pick("git_root_dir");
                    }
                } else if matches!(cu.disposition, ClassifiedDisposition::Consumed { .. })
                    && let Ok(v) = serde_json::from_str::<Value>(text)
                {
                    // Prefer artifact name: chat_history must never be diverted
                    // into the events bucket by content heuristics.
                    let is_chat_artifact = unit.artifact_name.contains("chat_history");
                    let is_event_artifact = unit.artifact_name.contains("event")
                        && !is_chat_artifact
                        || text.contains("\"event_type\"")
                        || text.contains("\"response_item\"");
                    if is_event_artifact && !is_chat_artifact {
                        event_lines.push((cu.ordinal, v));
                    } else {
                        chat_lines.push((cu.ordinal, v));
                    }
                }
            }
        }

        // Prefer summary cwd over git_root; fall back to decoding the Grok
        // path layout `…/sessions/<percent-encoded-cwd>/<uuid>/chat_history.jsonl`
        // when summary is missing (direct --file without sibling summary).
        let cwd = cwd
            .or(git_root)
            .or_else(|| infer_cwd_from_source(source))
            .unwrap_or_else(|| "/unknown/grok-cwd".to_owned());
        let model_id = model_id.unwrap_or_else(|| "grok".to_owned());
        let _title = title.unwrap_or_else(|| "grok session".to_owned());
        let started_at = started_at.unwrap_or_else(|| "2026-07-13T00:00:00Z".to_owned());
        let ended_at = ended_at.unwrap_or_else(|| started_at.clone());

        // Invalid non-empty provider timestamps historically fell back to
        // wall-clock time. Preserve the visible first-run result, but carry an
        // explicit Unknown in typed provenance so derived caches cannot treat
        // that time-dependent projection as stable.
        let (base_ts, known_started_at) = grok_base_timestamp(&started_at);
        let known_ended_at = if parse_ts(&ended_at).is_ok() {
            Known::value(ended_at.clone())
        } else {
            Known::unknown()
        };

        // Transport records become frames; the throne (`frames::classify`)
        // is the only speech decision. Empty `turns` is refused by
        // `validate_model` as `RefusalReason::EmptyConversation` (W1-T5),
        // never as a validated empty session.
        let mut turns: Vec<Turn> = Vec::new();
        let mut tool_events: Vec<ToolEvent> = Vec::new();
        let usage_events: Vec<UsageEvent> = Vec::new();
        let mut turn_idx: u64 = 0;

        for (ord, v) in &chat_lines {
            let raw_ref = classified
                .iter()
                .find(|classified| classified.ordinal == *ord)
                .map(|classified| classified.evidence.clone())
                .expect("classified evidence present");
            let ts = base_ts + ChronoDuration::milliseconds(*ord as i64 + 1);
            emit_grok_record(
                v,
                raw_ref,
                Known::value(ts.to_rfc3339()),
                &mut turn_idx,
                &mut turns,
                &mut tool_events,
            );
        }

        // Event artifacts share the same frame path. Used only when chat
        // produced no turns — not a second speech reducer.
        if turns.is_empty() {
            for (ord, v) in &event_lines {
                let raw_ref = classified
                    .iter()
                    .find(|classified| classified.ordinal == *ord)
                    .map(|classified| classified.evidence.clone())
                    .expect("classified evidence present");
                emit_grok_record(
                    v,
                    raw_ref,
                    Known::value(base_ts.to_rfc3339()),
                    &mut turn_idx,
                    &mut turns,
                    &mut tool_events,
                );
            }
        }

        // Build coverage directly from classified (must be exhaustive).
        let raw_unit_count = read.units.len() as u64;
        let mut consumed_units: Vec<crate::engine::ConsumedUnit> = Vec::new();
        let mut skipped: Vec<crate::engine::SkippedUnit> = Vec::new();
        for cu in &classified {
            let Some(u) = units_by_ord.get(&cu.ordinal) else {
                continue;
            };
            match &cu.disposition {
                ClassifiedDisposition::Consumed { kind } => {
                    consumed_units.push(crate::engine::ConsumedUnit {
                        ordinal: cu.ordinal,
                        kind: kind.clone(),
                        evidence: cu.evidence.clone(),
                    });
                }
                ClassifiedDisposition::Skipped { reason, visible } => {
                    skipped.push(crate::engine::SkippedUnit {
                        ordinal: cu.ordinal,
                        reason: *reason,
                        bytes: u.original_bytes,
                        visible: *visible,
                        evidence: cu.evidence.clone(),
                    });
                }
            }
        }

        let mut warnings = Vec::new();
        for (reason, kind) in [
            (
                SkippedReason::UnknownPayloadType,
                crate::engine::WarningKind::UnknownPayloadType,
            ),
            (
                SkippedReason::Malformed,
                crate::engine::WarningKind::MalformedUnit,
            ),
            (
                SkippedReason::Oversized,
                crate::engine::WarningKind::OversizedUnit,
            ),
        ] {
            let matching: Vec<_> = skipped
                .iter()
                .filter(|unit| unit.reason == reason)
                .collect();
            if let Some(first) = matching.first() {
                warnings.push(crate::engine::CoverageWarning {
                    kind,
                    count: matching.len() as u64,
                    first_ordinal: first.ordinal,
                });
            }
        }
        let unsupported_visible = skipped.iter().any(|unit| {
            unit.visible
                && matches!(
                    unit.reason,
                    SkippedReason::UnknownPayloadType | SkippedReason::Unsupported
                )
        });
        let visible_event_lost = skipped.iter().any(|unit| unit.visible);

        let status = ParseStatus {
            visible_completeness: if skipped.is_empty() {
                VisibleCompleteness::CompleteVisible
            } else if !consumed_units.is_empty() {
                VisibleCompleteness::PartialVisible
            } else {
                VisibleCompleteness::Fatal
            },
            boundary_flags: BoundaryFlags {
                opaque_reasoning_present: chat_lines
                    .iter()
                    .any(|(_, v)| v.get("type").and_then(|t| t.as_str()) == Some("reasoning")),
                unsupported_visible_event: unsupported_visible,
                compaction_boundary_present: false,
            },
            malformed_tail_present: skipped
                .iter()
                .any(|s| matches!(s.reason, SkippedReason::Malformed)),
            visible_event_lost,
        };

        let coverage =
            CoverageReport::new(raw_unit_count, consumed_units, skipped, warnings, status);
        if coverage.status.visible_completeness == VisibleCompleteness::Fatal {
            return Ok(UnvalidatedParse::fatal(coverage));
        }

        let provenance = Provenance {
            agent: AgentKind::Grok,
            model: Known::value(model_id.clone()),
            cli_version: Known::unknown(),
            cwd: Known::value(cwd.clone()),
            branch: branch.clone().map(Known::value).unwrap_or(Known::unknown()),
            started_at: known_started_at.clone(),
            ended_at: known_ended_at.clone(),
            original_source_hash: read.source_hash.clone(),
            original_source_bytes: read.source_bytes,
        };

        let mut model = SessionModel::new(logical_id.clone(), provenance, coverage.clone());

        // segment covering all
        if !turns.is_empty() {
            model.segments.push(Segment {
                segment_id: 0,
                scope_status: crate::engine::ScopeStatus::from_evidence(
                    Some(cwd.as_str()),
                    branch.as_deref(),
                ),
                cwd: Known::value(cwd),
                branch: branch.clone().map(Known::value).unwrap_or(Known::unknown()),
                started_at: known_started_at,
                ended_at: known_ended_at,
                turn_range: TurnRange {
                    start: 0,
                    end: turns.last().map(|t| t.turn_idx).unwrap_or(0),
                },
            });
        }

        model.turns = turns;
        model.tool_events = tool_events;
        model.usage_events = usage_events; // Grok currently emits via external; stub empty per fixture unless present in payload

        // For acceptance: UsageEvent telemetry present in contract when data supplies it.
        // If a future fixture supplies usage, map here into usage_events.

        Ok(UnvalidatedParse::from_model(model))
    }
}

// --- helpers (private to adapter; leave receipts) ---

/// Decode cwd from Grok on-disk layout when summary.json is absent:
/// `…/sessions/<percent-encoded-cwd>/<uuid>/chat_history.jsonl`.
fn infer_cwd_from_source(source: &SourceHandle) -> Option<String> {
    for artifact in source.artifacts() {
        let Some(path) = artifact.validated_path() else {
            continue;
        };
        // path = …/<encoded-cwd>/<session-uuid>/chat_history.jsonl
        let session_dir = path.parent()?;
        let encoded_cwd_dir = session_dir.parent()?;
        // Prefer the segment immediately under a `sessions` directory.
        let under_sessions = encoded_cwd_dir
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            == Some("sessions");
        if !under_sessions {
            continue;
        }
        let encoded = encoded_cwd_dir.file_name()?.to_str()?;
        let decoded = decode_percent_path(encoded);
        if decoded.starts_with('/') || decoded.contains(':') {
            return Some(decoded);
        }
    }
    None
}

fn decode_percent_path(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] == b'%'
            && cursor + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_nibble(bytes[cursor + 1]), hex_nibble(bytes[cursor + 2]))
        {
            decoded.push((high << 4) | low);
            cursor += 3;
        } else {
            decoded.push(bytes[cursor]);
            cursor += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn classify_grok_line(line: &str) -> Result<String, SkippedReason> {
    let v: Value = serde_json::from_str(line).map_err(|_| SkippedReason::Malformed)?;
    let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match typ {
        "user" => Ok("user".to_string()),
        "assistant" => Ok("assistant".to_string()),
        "reasoning" => Ok("reasoning".to_string()),
        "tool_result" => Ok("tool_result".to_string()),
        "text" | "summary_text" => Ok("assistant".to_string()),
        "system" | "notification" | "error" => Ok("system".to_string()),
        "" => Err(SkippedReason::UnknownPayloadType),
        _ => Err(SkippedReason::UnknownPayloadType),
    }
}

fn classify_grok_event(text: &str) -> Result<String, SkippedReason> {
    let v: Value = serde_json::from_str(text).map_err(|_| SkippedReason::Malformed)?;
    // Coverage kind only — speech class is `frames::classify`.
    if v.get("event_type").is_some()
        || v.get("response_item").is_some()
        || v.get("type").and_then(|t| t.as_str()) == Some("response_item")
    {
        if let Some(item) = v.get("payload").or_else(|| v.get("item"))
            && item.get("type").and_then(|t| t.as_str()) == Some("message")
        {
            return match item.get("role").and_then(|role| role.as_str()) {
                Some("user") => Ok("user".to_string()),
                Some("system") => Ok("system".to_string()),
                _ => Ok("assistant".to_string()),
            };
        }
        return Ok("event".to_string());
    }
    if let Some(t) = v.get("type").and_then(|x| x.as_str()) {
        match t {
            "user_message" => return Ok("user".to_string()),
            "agent_message" => return Ok("event".to_string()),
            "agent_reasoning" | "thinking" => return Ok("reasoning".to_string()),
            "tool_call" | "function_call" | "mcp_tool_call" => return Ok("tool_call".to_string()),
            "tool_result" | "mcp_tool_call_response" => return Ok("tool_result".to_string()),
            _ => {}
        }
    }
    Err(SkippedReason::UnknownPayloadType)
}

fn extract_user_text(v: &Value) -> String {
    if let Some(arr) = v.get("content").and_then(|c| c.as_array()) {
        return arr
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
    }
    v.get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string()
}

fn extract_assistant_text_and_tool(v: &Value) -> (String, Option<String>) {
    let mut text = v
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let mut tname = None;
    if text.is_empty()
        && let Some(calls) = v.get("tool_calls").and_then(|c| c.as_array())
    {
        let names: Vec<String> = calls
            .iter()
            .filter_map(|c| {
                c.get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
            })
            .collect();
        if !names.is_empty() {
            text = format!("[tool calls: {}]", names.join(", "));
            tname = names.first().cloned();
        }
    }
    if text.is_empty()
        && let Some(r) = v
            .get("reasoning")
            .and_then(|r| r.get("text"))
            .and_then(|t| t.as_str())
    {
        text = r.to_string();
    }
    (text, tname)
}

fn extract_reasoning_text(v: &Value) -> String {
    if let Some(arr) = v.get("summary").and_then(|s| s.as_array())
        && let Some(first) = arr.first()
        && let Some(t) = first.get("text").and_then(|x| x.as_str())
    {
        return t.to_string();
    }
    v.get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string()
}

fn emit_grok_record(
    value: &Value,
    evidence: RawUnitRef,
    timestamp: Known<String>,
    turn_idx: &mut u64,
    turns: &mut Vec<Turn>,
    tool_events: &mut Vec<ToolEvent>,
) {
    let Some(frame) = grok_transport_frame(value, evidence, timestamp) else {
        return;
    };
    let classified = frames::classify(&frame);
    push_classified_turn(classified, turn_idx, turns, tool_events);
}

fn grok_transport_frame(
    value: &Value,
    evidence: RawUnitRef,
    timestamp: Known<String>,
) -> Option<TransportFrame> {
    if let Some(item) = value
        .get("payload")
        .or_else(|| value.get("item"))
        .or_else(|| value.get("response_item"))
        && let Some(frame) = frame_from_nested_item(item, evidence.clone(), timestamp.clone())
    {
        return Some(frame);
    }

    let typ = value.get("type").and_then(Value::as_str).unwrap_or("");
    if typ == "agent_message" {
        return Some(TransportFrame {
            agent: AgentKind::Grok,
            transport_kind: TransportKind::AgentMessage,
            timestamp,
            payload: TransportPayload::InterAgent {
                sender: json_string(value, "sender"),
                task: json_string_or(value, &["task", "task_name"]),
                message_type: typ.to_owned(),
                content: json_string_or(value, &["content", "message"]),
            },
            evidence,
        });
    }

    let (transport_kind, payload) = grok_chat_payload(typ, value)?;
    Some(TransportFrame {
        agent: AgentKind::Grok,
        transport_kind,
        timestamp,
        payload,
        evidence,
    })
}

fn frame_from_nested_item(
    item: &Value,
    evidence: RawUnitRef,
    timestamp: Known<String>,
) -> Option<TransportFrame> {
    let typ = item.get("type").and_then(Value::as_str)?;
    match typ {
        "agent_message" => Some(TransportFrame {
            agent: AgentKind::Grok,
            transport_kind: TransportKind::AgentMessage,
            timestamp,
            payload: TransportPayload::InterAgent {
                sender: json_string(item, "sender"),
                task: json_string_or(item, &["task", "task_name"]),
                message_type: typ.to_owned(),
                content: json_string_or(item, &["content", "message"]),
            },
            evidence,
        }),
        "message" => {
            let role = item
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("assistant");
            let content = match item.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Array(_)) => extract_user_text(item),
                _ => String::new(),
            };
            let (transport_kind, payload) = match role {
                "user" => (
                    TransportKind::DirectMessage,
                    TransportPayload::Text {
                        role: TransportRole::User,
                        content,
                    },
                ),
                "system" => (
                    TransportKind::InjectedContext,
                    TransportPayload::Inject {
                        tag: "system".to_owned(),
                        content,
                    },
                ),
                _ => (
                    TransportKind::AssistantMessage,
                    TransportPayload::Text {
                        role: TransportRole::Assistant,
                        content,
                    },
                ),
            };
            Some(TransportFrame {
                agent: AgentKind::Grok,
                transport_kind,
                timestamp,
                payload,
                evidence,
            })
        }
        _ => None,
    }
}

fn grok_chat_payload(typ: &str, value: &Value) -> Option<(TransportKind, TransportPayload)> {
    match typ {
        "user" | "user_message" => {
            let content = if typ == "user_message" {
                json_string(value, "message")
            } else {
                extract_user_text(value)
            };
            if let Some(tag) = value.get("synthetic_reason").and_then(Value::as_str) {
                Some((
                    TransportKind::InjectedContext,
                    TransportPayload::Inject {
                        tag: tag.to_owned(),
                        content,
                    },
                ))
            } else {
                Some((
                    TransportKind::DirectMessage,
                    TransportPayload::Text {
                        role: TransportRole::User,
                        content,
                    },
                ))
            }
        }
        "system" | "notification" | "error" => Some((
            TransportKind::InjectedContext,
            TransportPayload::Inject {
                tag: typ.to_owned(),
                content: json_string_or(value, &["content", "text", "message"]),
            },
        )),
        "assistant" | "text" | "summary_text" => {
            let raw_content = value.get("content").and_then(Value::as_str).unwrap_or("");
            let tool_calls = value.get("tool_calls").and_then(Value::as_array);
            if raw_content.is_empty() && tool_calls.is_some_and(|calls| !calls.is_empty()) {
                let (_, tname) = extract_assistant_text_and_tool(value);
                return Some((
                    TransportKind::UserShellCommand,
                    TransportPayload::Shell {
                        command: tname.unwrap_or_else(|| "tool".to_owned()),
                        result: value
                            .get("tool_calls")
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    },
                ));
            }
            let (text, _) = extract_assistant_text_and_tool(value);
            Some((
                TransportKind::AssistantMessage,
                TransportPayload::Text {
                    role: TransportRole::Assistant,
                    content: text,
                },
            ))
        }
        "reasoning" | "agent_reasoning" | "thinking" => Some((
            TransportKind::InjectedContext,
            TransportPayload::Inject {
                tag: "reasoning".to_owned(),
                content: extract_reasoning_text(value),
            },
        )),
        "tool_result" | "mcp_tool_call_response" => {
            let command = json_string_or(value, &["tool_call_id", "name"]);
            let command = if command.is_empty() {
                "tool".to_owned()
            } else {
                command
            };
            Some((
                TransportKind::UserShellCommand,
                TransportPayload::Shell {
                    command,
                    result: json_string(value, "content"),
                },
            ))
        }
        "tool_call" | "function_call" | "mcp_tool_call" => {
            let command = json_string_or(value, &["name", "tool_name"]);
            let command = if command.is_empty() {
                "tool".to_owned()
            } else {
                command
            };
            Some((
                TransportKind::UserShellCommand,
                TransportPayload::Shell {
                    command,
                    result: String::new(),
                },
            ))
        }
        _ => None,
    }
}

fn json_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn json_string_or(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .unwrap_or("")
        .to_owned()
}

fn push_classified_turn(
    classified: crate::engine::frames::ClassifiedFrame,
    turn_idx: &mut u64,
    turns: &mut Vec<Turn>,
    tool_events: &mut Vec<ToolEvent>,
) {
    let Some(kind) = classified.turn_kind else {
        return;
    };
    let evidence = classified.origin.evidence.clone();
    // Role is the throne's (`turn_role`, W2-R1); only the shell lane still
    // chooses its text/tool name here.
    let role = classified.class.turn_role();
    let (role, text, tool_name, kind) = match &classified.class {
        FrameClass::Human { .. }
        | FrameClass::EchoSeal { .. }
        | FrameClass::AssistantFinal
        | FrameClass::InterAgent { .. } => {
            (role, classified.content.clone(), Known::unknown(), kind)
        }
        FrameClass::ShellAction { cmd, result } => {
            // Throne maps every ShellAction to ToolCall (no call/result pair
            // in FrameClass). Result text stays on the turn for Decision 6;
            // ToolEventKind distinguishes call vs result without rewriting
            // classified.turn_kind.
            let event_kind = if result.text.is_empty() {
                ToolEventKind::Call
            } else {
                ToolEventKind::Result
            };
            let text = if result.text.is_empty() {
                cmd.clone()
            } else {
                result.text.clone()
            };
            tool_events.push(ToolEvent {
                kind: event_kind,
                turn_idx: *turn_idx,
                tool_name: cmd.clone(),
                correlation_id: Known::unknown(),
                payload_hash: result.hash.clone(),
                payload_bytes: result.text.len() as u64,
                raw_unit_refs: vec![evidence.clone()],
            });
            (role, text, Known::value(cmd.clone()), kind)
        }
        FrameClass::Inject { .. } | FrameClass::LineageMeta { .. } => {
            (role, classified.content.clone(), Known::unknown(), kind)
        }
    };

    if text.trim().is_empty() {
        return;
    }

    let timestamp = match &classified.seal.seal_ts {
        Known::Value(value) => Known::value(value.clone()),
        Known::Unknown(unknown) => Known::Unknown(*unknown),
    };

    turns.push(Turn {
        turn_idx: *turn_idx,
        role,
        timestamp,
        kind,
        text: text.clone(),
        text_hash: sha256_hex(text.as_bytes()),
        text_chars: text.chars().count() as u64,
        tool_name,
        segment_id: 0,
        raw_unit_refs: vec![evidence],
        frame_class: Some(classified.class.clone()),
    });
    *turn_idx += 1;
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>, ()> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .or_else(|_| {
            // naive fallback
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
                .map(|nd| DateTime::<Utc>::from_naive_utc_and_offset(nd, Utc))
                .map_err(|_| ())
        })
}

fn grok_base_timestamp(started_at: &str) -> (DateTime<Utc>, Known<String>) {
    match parse_ts(started_at) {
        Ok(timestamp) => (timestamp, Known::value(started_at.to_owned())),
        Err(()) => (Utc::now(), Known::unknown()),
    }
}

fn make_evidence_ref(
    source: &SourceHandle,
    unit: &RawUnit,
    ordinal: u64,
    unit_kind: &str,
) -> RawUnitRef {
    // Reader's content_hash is of the original bytes (even if bytes field is capped).
    let content_hash = unit.content_hash.clone();
    let locator = ordinal_locator(ordinal);
    let session = source.logical_session_id().unwrap_or(source.source_id());
    let eid =
        evidence_event_id_from_hash(AgentKind::Grok, session, &locator, unit_kind, &content_hash)
            .unwrap_or_else(|_| format!("ev1:grok:{}:{}", session, &content_hash[..16]));
    RawUnitRef {
        evidence_event_id: eid,
        coverage_ordinal: ordinal,
        physical_ordinal: ordinal,
        locator,
        unit_kind: unit_kind.to_owned(),
        artifact: unit.artifact_name.clone(),
        content_hash,
        original_bytes: unit.original_bytes,
    }
}

// Extension trait for pattern matching disposition in assemble (local).
trait DispositionExt {
    fn as_consumed_kind(&self) -> Option<&str>;
}

impl DispositionExt for ClassifiedDisposition {
    fn as_consumed_kind(&self) -> Option<&str> {
        if let ClassifiedDisposition::Consumed { kind } = self {
            Some(kind.as_str())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{SourceArtifact, SourceFraming, SourceHandle, TurnRole};

    fn make_handle_with_jsonl(bytes: &[u8]) -> SourceHandle {
        let art = SourceArtifact::memory(
            "chat_history.jsonl".to_string(),
            bytes.to_vec(),
            SourceFraming::JsonLines,
        )
        .expect("artifact");
        SourceHandle::new(
            AgentKind::Grok,
            "grok-test-sid",
            Some("grok-test-sid".to_string()),
            vec![art],
        )
        .expect("handle")
    }

    fn make_handle_with_summary_and_chat(summary: &str, chat: &str) -> SourceHandle {
        let s = SourceArtifact::memory(
            "summary.json",
            summary.as_bytes().to_vec(),
            SourceFraming::WholeDocument,
        )
        .unwrap();
        let c = SourceArtifact::memory(
            "chat_history.jsonl",
            chat.as_bytes().to_vec(),
            SourceFraming::JsonLines,
        )
        .unwrap();
        SourceHandle::new(
            AgentKind::Grok,
            "grok-sess",
            Some("grok-sess".into()),
            vec![s, c],
        )
        .unwrap()
    }

    #[test]
    fn invalid_created_at_marks_wall_clock_fallback_as_unknown_provenance() {
        let (_, invalid) = grok_base_timestamp("not-a-timestamp");
        assert_eq!(invalid, Known::unknown());
        let (_, valid) = grok_base_timestamp("2026-09-03T12:00:00Z");
        assert_eq!(valid, Known::value("2026-09-03T12:00:00Z".to_owned()));
    }

    #[test]
    #[ignore]
    fn grok_adapter_accepts_explicit_source_handle_only() {
        // API / call-graph verifier: source of this file must not contain discovery.
        let src = include_str!("grok.rs");
        for forbidden in [
            "read_dir",
            "walkdir",
            "glob(",
            "Command::new",
            "std::process",
            "fs::read_dir",
        ] {
            assert!(
                !src.contains(forbidden),
                "grok adapter must not contain discovery: {forbidden}"
            );
        }
    }

    #[test]
    #[ignore]
    fn grok_minimal_fixture_roundtrips_to_model() {
        let chat = r#"{"type":"user","content":[{"type":"text","text":"Build the Grok oracle."}]}
{"type":"assistant","model_id":"grok-test","content":"The Grok oracle is ready."}
"#;
        let handle = make_handle_with_jsonl(chat.as_bytes());
        let engine = crate::engine::ParserEngine::default();
        let adapter = GrokAdapter;
        let parsed = engine.parse(&handle, &adapter).expect("parse succeeds");
        let crate::engine::ValidatedParse::Session(sess) = parsed else {
            panic!("expected session")
        };
        let m = sess.model();
        assert_eq!(m.session_id, "grok-test-sid");
        assert!(m.turns.len() >= 2);
        assert_eq!(m.turns[0].role, TurnRole::User);
        assert_eq!(m.turns[1].role, TurnRole::Assistant);
        assert_eq!(m.coverage.consumed_count, 2);
        assert_eq!(m.provenance.agent, AgentKind::Grok);
    }

    #[test]
    #[ignore]
    fn grok_summary_metadata_is_used() {
        let summary = r#"{"info":{"id":"44444444-4444-4444-8444-444444444444","cwd":"/repo/oracle"},"session_summary":"Build Grok oracle","created_at":"2026-07-13T00:00:00Z","updated_at":"2026-07-13T00:00:01Z","current_model_id":"grok-test","git_root_dir":"/repo/oracle/","head_branch":"main","agent_name":"grok"}"#;
        let chat = r#"{"type":"user","content":[{"type":"text","text":"hi"}]}
{"type":"assistant","content":"ok"}
"#;
        let handle = make_handle_with_summary_and_chat(summary, chat);
        let parsed = crate::engine::ParserEngine::default()
            .parse(&handle, &GrokAdapter)
            .expect("parse");
        let crate::engine::ValidatedParse::Session(s) = parsed else {
            panic!()
        };
        let m = s.model();
        assert_eq!(m.provenance.cwd, Known::value("/repo/oracle".to_string()));
        assert_eq!(m.provenance.model, Known::value("grok-test".to_string()));
        // title is in heuristic outside model; session has provenance
    }

    #[test]
    #[ignore]
    fn grok_adapter_is_deterministic_across_runs() {
        let chat = r#"{"type":"user","content":[{"type":"text","text":"a"}]}
{"type":"assistant","content":"b"}
{"type":"reasoning","summary":[{"text":"think"}]}
"#;
        let handle = make_handle_with_jsonl(chat.as_bytes());
        let eng = crate::engine::ParserEngine::default();
        let a1 = eng.parse(&handle, &GrokAdapter).unwrap();
        let a2 = eng.parse(&handle, &GrokAdapter).unwrap();
        // compare models via serialized or direct
        let m1 = if let crate::engine::ValidatedParse::Session(s) = a1 {
            s.into_model()
        } else {
            panic!()
        };
        let m2 = if let crate::engine::ValidatedParse::Session(s) = a2 {
            s.into_model()
        } else {
            panic!()
        };
        assert_eq!(m1.turns.len(), m2.turns.len());
        assert_eq!(m1.coverage.consumed_count, m2.coverage.consumed_count);
        assert_eq!(m1.provenance.cwd, m2.provenance.cwd);
        // evidence ids stable
        assert_eq!(
            m1.turns[0].raw_unit_refs[0].evidence_event_id,
            m2.turns[0].raw_unit_refs[0].evidence_event_id
        );
    }
}
