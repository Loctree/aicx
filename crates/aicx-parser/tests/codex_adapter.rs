//! Codex adapter differential and adversarial contract suite (C2).
//!
//! C5X owns registration in the shared adapter boundary. This test shadows the
//! frozen boundary so the sealed Codex implementation can be verified without
//! editing shared dispatch during the parallel wave.

mod engine {
    pub use aicx_parser::engine::*;
}

mod sealed {
    pub trait Sealed {}
}

use engine::{AgentKind, RawUnitRef, SkippedReason, SourceHandle, SourceRead, UnvalidatedParse};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedUnit {
    pub ordinal: u64,
    pub level: RawUnitLevel,
    pub evidence: RawUnitRef,
    pub disposition: ClassifiedDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawUnitLevel {
    Physical,
    Logical { parent_ordinal: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifiedDisposition {
    Consumed {
        kind: String,
    },
    Skipped {
        reason: SkippedReason,
        visible: bool,
    },
}

pub trait AgentAdapter: sealed::Sealed + Send + Sync {
    fn agent(&self) -> AgentKind;
    fn adapter_version(&self) -> &'static str;
    fn classify(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
    ) -> Result<Vec<ClassifiedUnit>, AdapterError>;
    fn assemble(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
        classified: Vec<ClassifiedUnit>,
    ) -> Result<UnvalidatedParse, AdapterError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterError {
    pub stage: &'static str,
    pub detail: String,
}

impl AdapterError {
    pub fn new(stage: &'static str, detail: impl Into<String>) -> Self {
        Self {
            stage,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "adapter {} failed: {}", self.stage, self.detail)
    }
}

impl std::error::Error for AdapterError {}

#[path = "../src/adapters/codex.rs"]
mod codex;

use aicx_parser::engine::{
    CounterSemantics, Known, RawUnitReader, ReaderPolicy, SessionModel, SourceArtifact,
    SourceFraming, TurnKind, TurnRole, ValidatedParse, VisibleCompleteness,
    evidence_event_id_from_hash, validate_parse,
};
use codex::CodexAdapter;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn source(session_id: &str, body: &[u8]) -> SourceHandle {
    SourceHandle::new(
        AgentKind::Codex,
        session_id,
        Some(session_id.to_owned()),
        vec![
            SourceArtifact::memory("rollout.jsonl", body.to_vec(), SourceFraming::JsonLines)
                .expect("memory source"),
        ],
    )
    .expect("explicit source handle")
}

fn parse(session_id: &str, body: &[u8]) -> ValidatedParse {
    let source = source(session_id, body);
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .expect("bounded read");
    let adapter = CodexAdapter;
    let classified = adapter
        .classify(&source, &read)
        .expect("Codex classification");
    validate_parse(
        adapter
            .assemble(&source, &read, classified)
            .expect("Codex assembly"),
    )
    .expect("Codex kernel validation")
}

fn model(session_id: &str, body: &[u8]) -> SessionModel {
    match parse(session_id, body) {
        ValidatedParse::Session(session) => session.into_model(),
        ValidatedParse::Fatal(fatal) => panic!("unexpected fatal parse: {:?}", fatal.coverage()),
    }
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        repo_root()
            .join("tests/fixtures/parser_engine/codex")
            .join(name),
    )
    .expect("Codex fixture")
}

fn envelope_from(parse: &ValidatedParse) -> serde_json::Value {
    let ValidatedParse::Session(session) = parse else {
        panic!("oracle fixture must project a session")
    };
    let model = session.model();
    let physical = model.coverage.raw_line_count;
    let visible_turns = model
        .turns
        .iter()
        .filter(|turn| matches!(turn.kind, TurnKind::UserMsg | TurnKind::AgentReply))
        .enumerate()
        .map(|(ordinal, turn)| {
            serde_json::json!({
                "ordinal": ordinal as u64,
                "role": if turn.role == TurnRole::User { "user" } else { "assistant" },
                "kind": "message",
                "text": turn.text,
            })
        })
        .collect::<Vec<_>>();
    let mut boundaries = Vec::new();
    if model
        .coverage
        .status
        .boundary_flags
        .opaque_reasoning_present
    {
        boundaries.push("opaque_reasoning_present");
    }
    if model
        .coverage
        .status
        .boundary_flags
        .unsupported_visible_event
    {
        boundaries.push("unsupported_visible_event");
    }
    let visible = match model.coverage.status.visible_completeness {
        VisibleCompleteness::CompleteVisible => "complete_visible",
        VisibleCompleteness::PartialVisible => "partial_visible",
        VisibleCompleteness::Fatal => "fatal",
    };
    let intent_summary = model
        .turns
        .iter()
        .find(|turn| turn.kind == TurnKind::UserMsg)
        .map(|turn| turn.text.clone())
        .unwrap_or_default();
    serde_json::json!({
        "schema": "parser_oracle.envelope.v1",
        "agent": "codex",
        "session_id": model.session_id,
        "visible_turns": visible_turns,
        "coverage": {
            "raw_units": physical,
            "consumed": model.coverage.consumed.iter().filter(|unit| unit.ordinal <= physical).count(),
            "skipped": model.coverage.skipped.iter().filter(|unit| unit.ordinal <= physical).count(),
        },
        "status": { "visible": visible, "boundaries": boundaries },
        "usage": model.usage_events,
        "heuristic": { "intent_summary": intent_summary },
    })
}

#[test]
fn codex_differential_envelope_matches_frozen_oracle() {
    let body = fixture("minimal.jsonl");
    let parsed = parse("11111111-1111-4111-8111-111111111111", &body);
    let envelope = envelope_from(&parsed);
    let expected: serde_json::Value =
        serde_json::from_slice(&fixture("expected.json")).expect("oracle golden");
    for field in [
        "agent",
        "session_id",
        "visible_turns",
        "coverage",
        "status",
        "usage",
    ] {
        assert_eq!(envelope.get(field), expected.get(field), "$.{field}");
    }
    assert!(
        envelope["heuristic"]["intent_summary"]
            .as_str()
            .is_some_and(|text| text.contains("oracle"))
    );
    fs::write(
        "/tmp/aicx-codex-envelope.json",
        serde_json::to_vec_pretty(&envelope).expect("serialize envelope"),
    )
    .expect("write comparator artifact")
}

#[test]
fn codex_usage_preserves_cumulative_delta_snapshot_unknown_and_reported_cost() {
    let body = br#"{"timestamp":"2026-07-13T00:00:00Z","type":"session_meta","payload":{"id":"usage","cwd":"/repo","model":"gpt-a"}}
{"timestamp":"2026-07-13T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"provider":"openai","model":"gpt-b","total_token_usage":{"input_tokens":100,"cached_input_tokens":60,"output_tokens":40,"reasoning_output_tokens":20},"last_token_usage":{"input_tokens":10,"output_tokens":4},"reported_cost":{"amount":1.25,"currency":"USD"}}}}
{"timestamp":"2026-07-13T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"provider":"openai","model":"gpt-c","input_tokens":8,"output_tokens":3}}}
"#;
    let model = model("usage", body);
    assert_eq!(model.usage_events.len(), 3);
    assert_eq!(
        model.usage_events[0].counter_semantics,
        CounterSemantics::Cumulative
    );
    assert_eq!(
        model.usage_events[1].counter_semantics,
        CounterSemantics::Delta
    );
    assert_eq!(
        model.usage_events[2].counter_semantics,
        CounterSemantics::Snapshot
    );
    assert_eq!(model.usage_events[0].tokens.cache_read, Known::value(60));
    assert_eq!(model.usage_events[0].tokens.reasoning, Known::value(20));
    assert!(matches!(model.usage_events[0].cost, Known::Value(_)));
    assert_eq!(model.usage_events[1].cost, Known::unknown());
    assert_eq!(model.usage_events[2].cost, Known::unknown());
    assert_eq!(
        model.usage_events[2].model,
        Known::value("gpt-c".to_owned())
    );
}

#[test]
fn codex_opaque_and_unsupported_boundaries_do_not_fake_visible_loss() {
    let body = br#"{"timestamp":"2026-07-13T00:00:00Z","type":"session_meta","payload":{"id":"boundary","cwd":"/repo"}}
{"timestamp":"2026-07-13T00:00:01Z","type":"response_item","payload":{"type":"encrypted_reasoning","encrypted_content":"opaque"}}
{"timestamp":"2026-07-13T00:00:02Z","type":"event_msg","payload":{"type":"future_visible_event","message":"preserved boundary"}}
{"timestamp":"2026-07-13T00:00:03Z","type":"event_msg","payload":{"type":"agent_message","message":"visible answer"}}
"#;
    let model = model("boundary", body);
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    assert!(
        model
            .coverage
            .status
            .boundary_flags
            .opaque_reasoning_present
    );
    assert!(
        model
            .coverage
            .status
            .boundary_flags
            .unsupported_visible_event
    );
    assert!(!model.coverage.status.visible_event_lost);
    assert!(
        model
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.reason == SkippedReason::EncryptedOpaque && !unit.visible)
    );
}

#[test]
fn codex_real_encrypted_content_shape_sets_opaque_boundary() {
    let body = br#"{"timestamp":"2026-07-13T00:00:00Z","type":"session_meta","payload":{"id":"real-shape","cwd":"/repo"}}
{"timestamp":"2026-07-13T00:00:01Z","type":"response_item","payload":{"type":"reasoning","id":"synthetic","summary":[],"encrypted_content":"synthetic-ciphertext","internal_chat_message_metadata_passthrough":null}}
{"timestamp":"2026-07-13T00:00:02Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"visible answer"}]}}
"#;
    let model = model("real-shape", body);
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    assert!(
        model
            .coverage
            .status
            .boundary_flags
            .opaque_reasoning_present
    );
    assert!(
        model
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.reason == SkippedReason::EncryptedOpaque && !unit.visible)
    );
}

#[test]
fn codex_source_identity_cwd_segments_skills_tools_and_physical_evidence_survive() {
    let body = br#"{"timestamp":"2026-07-13T00:00:00Z","type":"session_meta","payload":{"id":"identity","cwd":"/repo/a","model":"gpt-5","cli_version":"1.2.3"}}
{"timestamp":"2026-07-13T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"Run /vc-implement now"}}
{"timestamp":"2026-07-13T00:00:02Z","type":"turn_context","payload":{"cwd":"/repo/b","branch":"main"}}
{"timestamp":"2026-07-13T00:00:03Z","type":"response_item","payload":{"type":"function_call","name":"shell","call_id":"c1","arguments":"{}"}}
{"timestamp":"2026-07-13T00:00:04Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"ok"}}
"#;
    let model = model("identity", body);
    assert_eq!(model.session_id, "identity");
    assert_eq!(model.provenance.cwd, Known::value("/repo/a".to_owned()));
    assert_eq!(model.segments.len(), 2);
    assert_eq!(model.segments[1].cwd, Known::value("/repo/b".to_owned()));
    assert_eq!(model.tool_events.len(), 2);
    assert_eq!(model.tool_events[0].tool_name, "shell");
    assert_eq!(model.tool_events[1].tool_name, "shell");
    assert_eq!(model.skill_invocations[0].skill_name, "vc-implement");
    let evidence = model
        .coverage
        .consumed
        .iter()
        .map(|unit| &unit.evidence.evidence_event_id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(evidence.len(), model.coverage.consumed.len());
    assert!(
        evidence
            .iter()
            .all(|id| id.starts_with("ev1:codex:") && !id.contains('/'))
    );
}

#[test]
fn codex_classification_is_gap_free_and_binds_logical_units_to_physical_parents() {
    let body = br#"{"timestamp":"2026-07-13T00:00:00Z","type":"session_meta","payload":{"id":"classified","cwd":"/repo"}}
{"timestamp":"2026-07-13T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":"hello"}}
{"timestamp":"2026-07-13T00:00:02Z","type":"response_item","payload":{"type":"reasoning","encrypted_content":"opaque"}}
{"timestamp":"2026-07-13T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5}}}}
"#;
    let source = source("classified", body);
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .expect("bounded read");
    let adapter = CodexAdapter;
    let classified = adapter.classify(&source, &read).expect("classification");
    let physical = classified
        .iter()
        .filter(|unit| unit.level == RawUnitLevel::Physical)
        .map(|unit| unit.ordinal)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(physical.len(), read.units.len());
    let mut evidence_ids = std::collections::BTreeSet::new();
    assert!(classified.iter().enumerate().all(|(index, unit)| {
        evidence_ids.insert(unit.evidence.evidence_event_id.clone())
            && unit.evidence.evidence_event_id
                == evidence_event_id_from_hash(
                    AgentKind::Codex,
                    "classified",
                    &unit.evidence.locator,
                    &unit.evidence.unit_kind,
                    &unit.evidence.content_hash,
                )
                .expect("derivation v1")
            && unit.ordinal == index as u64 + 1
            && unit.evidence.coverage_ordinal == unit.ordinal
            && match unit.level {
                RawUnitLevel::Physical => true,
                RawUnitLevel::Logical { parent_ordinal } => {
                    physical.contains(&parent_ordinal) && unit.ordinal > read.units.len() as u64
                }
            }
    }));
    let unvalidated = adapter
        .assemble(&source, &read, classified.clone())
        .expect("assembly");
    assert_eq!(unvalidated.coverage.raw_line_count, read.units.len() as u64);
    assert_eq!(unvalidated.coverage.raw_unit_count, classified.len() as u64);
    validate_parse(unvalidated).expect("classification-backed model validates");
}

#[test]
fn codex_requires_one_explicit_jsonl_artifact() {
    let artifact = |name, framing| {
        SourceArtifact::memory(name, b"{}\n".to_vec(), framing).expect("memory artifact")
    };
    let multi = SourceHandle::new(
        AgentKind::Codex,
        "multi",
        Some("multi".to_owned()),
        vec![
            artifact("first.jsonl", SourceFraming::JsonLines),
            artifact("second.jsonl", SourceFraming::JsonLines),
        ],
    )
    .expect("explicit multi-artifact handle");
    let multi_read = RawUnitReader::new(ReaderPolicy::default())
        .read(&multi)
        .expect("bounded multi read");
    let adapter = CodexAdapter;
    assert!(adapter.classify(&multi, &multi_read).is_err());

    let whole = SourceHandle::new(
        AgentKind::Codex,
        "whole",
        Some("whole".to_owned()),
        vec![artifact("rollout.json", SourceFraming::WholeDocument)],
    )
    .expect("explicit whole-document handle");
    let whole_read = RawUnitReader::new(ReaderPolicy::default())
        .read(&whole)
        .expect("bounded whole-document read");
    assert!(adapter.classify(&whole, &whole_read).is_err());
}

#[test]
fn codex_dual_envelope_dedups_event_msg_when_response_item_owns_chat() {
    let body = fixture("dual_envelope.jsonl");
    let session = model("22222222-2222-4222-8222-222222222222", &body);
    let visible: Vec<_> = session
        .turns
        .iter()
        .filter(|turn| matches!(turn.kind, TurnKind::UserMsg | TurnKind::AgentReply))
        .collect();
    assert_eq!(
        visible.len(),
        2,
        "expected one user + one assistant turn, got {}: {:?}",
        visible.len(),
        visible
            .iter()
            .map(|t| (t.role, t.kind, t.text.as_str()))
            .collect::<Vec<_>>()
    );
    assert_eq!(visible[0].role, TurnRole::User);
    assert_eq!(visible[0].text, "Diagnose the burn site.");
    assert_eq!(visible[1].role, TurnRole::Assistant);
    assert_eq!(visible[1].text, "I see double turns.");
    // No skip for dual envelope; chat must not double.
    assert_eq!(session.coverage.skipped_count, 0);
    assert!(
        session.coverage.consumed_count >= 6,
        "expected all physical units consumed, got {}",
        session.coverage.consumed_count
    );
}

#[test]
fn codex_compaction_markers_consumed_without_unsupported_or_visible_loss() {
    let body = fixture("compaction_markers.jsonl");
    let session = model("33333333-3333-4333-8333-333333333333", &body);
    assert!(
        session
            .coverage
            .status
            .boundary_flags
            .compaction_boundary_present,
        "compaction markers must set boundary flag"
    );
    assert!(
        !session
            .coverage
            .status
            .boundary_flags
            .unsupported_visible_event,
        "compaction must not be classified as unsupported visible"
    );
    let visible: Vec<_> = session
        .turns
        .iter()
        .filter(|turn| matches!(turn.kind, TurnKind::UserMsg | TurnKind::AgentReply))
        .map(|t| t.text.as_str())
        .collect();
    assert_eq!(
        visible,
        ["Before compact", "Ack before", "After compact", "Ack after"]
    );
    assert_eq!(session.coverage.skipped_count, 0);
    assert_eq!(session.coverage.raw_line_count, 8);
}
