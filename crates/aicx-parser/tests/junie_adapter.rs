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

#[path = "../src/adapters/junie.rs"]
mod junie;

use aicx_parser::engine::{
    Known, RawUnitReader, ReaderPolicy, SessionModel, SourceArtifact, SourceFraming, ToolEventKind,
    TurnKind, TurnRole, ValidatedParse, VisibleCompleteness, validate_parse,
};
use junie::JunieAdapter;

#[test]
fn junie_native_matrix_models_core_shapes_and_coverage() {
    let source = fixture_source(
        "matrix",
        include_str!("../../../tests/fixtures/parser_engine/junie/native_matrix.events.jsonl"),
    );
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .unwrap();
    let adapter = JunieAdapter;
    assert_eq!(adapter.agent(), AgentKind::Junie);
    assert_eq!(adapter.adapter_version(), "junie-native-v1");

    let classified = adapter.classify(&source, &read).unwrap();
    let physical = classified
        .iter()
        .filter(|unit| matches!(unit.level, RawUnitLevel::Physical))
        .count();
    let logical = classified
        .iter()
        .filter(|unit| matches!(unit.level, RawUnitLevel::Logical { .. }))
        .count();
    assert_eq!(physical, 10, "all raw rows get explicit physical coverage");
    assert_eq!(
        logical, 4,
        "last block snapshots are projected deterministically"
    );
    assert!(classified.iter().any(|unit| matches!(
        &unit.disposition,
        ClassifiedDisposition::Skipped {
            reason: SkippedReason::UnknownPayloadType,
            visible: true,
        }
    )));

    let parsed = adapter
        .assemble(&source, &read, classified)
        .unwrap()
        .into_model();
    assert_eq!(parsed.session_id, "260408-214715-good");
    assert_eq!(parsed.provenance.agent, AgentKind::Junie);
    assert_eq!(
        parsed.provenance.started_at,
        Known::value("2026-04-08T21:47:15Z".to_owned()),
        "session id timestamp fallback is stable and explicit in the model"
    );
    assert_eq!(parsed.provenance.cwd, Known::value("/work/aicx".to_owned()));
    assert_eq!(parsed.coverage.raw_line_count, 10);
    assert_eq!(parsed.coverage.raw_unit_count, 14);
    assert_eq!(parsed.coverage.consumed_count, 12);
    assert_eq!(parsed.coverage.skipped_count, 2);
    assert_eq!(
        parsed.coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
    assert!(
        parsed
            .coverage
            .status
            .boundary_flags
            .unsupported_visible_event
    );

    let turns = parsed
        .turns
        .iter()
        .map(|turn| (turn.role, turn.kind, turn.text.as_str()))
        .collect::<Vec<_>>();
    assert!(turns.contains(&(TurnRole::System, TurnKind::SystemNote, "You are Junie.")));
    assert!(turns.contains(&(
        TurnRole::User,
        TurnKind::UserMsg,
        "Implement deterministic adapter."
    )));
    assert!(turns.contains(&(
        TurnRole::System,
        TurnKind::SystemNote,
        "Prefer explicit evidence."
    )));
    assert!(turns.contains(&(
        TurnRole::Assistant,
        TurnKind::InternalThought,
        "Need map first."
    )));
    assert!(turns.contains(&(
        TurnRole::Assistant,
        TurnKind::AgentReply,
        "Implemented Junie adapter."
    )));
    assert!(turns.contains(&(
        TurnRole::Tool,
        TurnKind::ToolCall,
        r#"{"path":"Cargo.toml"}"#
    )));
    assert!(turns.contains(&(TurnRole::Tool, TurnKind::ToolResult, "ok")));

    assert_eq!(parsed.tool_events.len(), 2);
    assert_eq!(parsed.tool_events[0].kind, ToolEventKind::Call);
    assert_eq!(parsed.tool_events[0].tool_name, "open");
    assert_eq!(parsed.tool_events[1].kind, ToolEventKind::Result);
    assert_eq!(parsed.tool_events[1].tool_name, "open");
    assert!(
        parsed.usage_events.is_empty(),
        "Junie has no native usage telemetry here"
    );
}

#[test]
fn junie_a2ux_live_envelope_maps_roles_and_latest_snapshots() {
    let source = fixture_source_with_session(
        "a2ux-live",
        include_str!("../../../tests/fixtures/parser_engine/junie/session-a2ux-live/events.jsonl"),
        "session-260713-155923-jemh",
    );
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .unwrap();
    let adapter = JunieAdapter;
    let model = adapter
        .assemble(&source, &read, adapter.classify(&source, &read).unwrap())
        .expect("assemble live Junie A2ux fixture")
        .into_model();

    assert_eq!(model.coverage.raw_line_count, 5);

    let turns = model
        .turns
        .iter()
        .map(|turn| (turn.role, turn.kind, turn.text.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(turns.len(), 3);
    assert!(turns.contains(&(
        TurnRole::Assistant,
        TurnKind::InternalThought,
        "Inspect the Junie parser and its normative contract."
    )));
    assert!(turns.contains(&(
        TurnRole::Tool,
        TurnKind::ToolCall,
        "cargo test -p aicx-parser junie"
    )));
    assert!(turns.contains(&(
        TurnRole::Assistant,
        TurnKind::AgentReply,
        "Implemented the Junie A2ux parser."
    )));
}

#[test]
fn junie_native_golden_matches_reviewed_fixture() {
    let source = fixture_source_with_session(
        "20260713",
        include_str!("../../../tests/fixtures/parser_engine/junie/session-20260713/events.jsonl"),
        "20260713",
    );
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .unwrap();
    let adapter = JunieAdapter;
    let parsed = adapter
        .assemble(&source, &read, adapter.classify(&source, &read).unwrap())
        .unwrap()
        .into_model();
    let envelope = envelope_from(&parsed);
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/parser_engine/junie/expected.json"
    ))
    .expect("Junie native golden");

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
            .is_some_and(|text| text.contains("Junie oracle"))
    );
}

trait IntoModelForTest {
    fn into_model(self) -> SessionModel;
}

impl IntoModelForTest for UnvalidatedParse {
    fn into_model(self) -> SessionModel {
        match validate_parse(self).unwrap() {
            ValidatedParse::Session(session) => session.into_model(),
            ValidatedParse::Fatal(_) => panic!("Junie fixture unexpectedly produced fatal parse"),
        }
    }
}

#[test]
fn junie_timestamp_fallback_is_repeatedly_canonical() {
    let source = fixture_source(
        "timestamp",
        include_str!("../../../tests/fixtures/parser_engine/junie/timestamp_fallback.events.jsonl"),
    );
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .unwrap();
    let adapter = JunieAdapter;

    let first = adapter
        .assemble(&source, &read, adapter.classify(&source, &read).unwrap())
        .unwrap()
        .into_model();
    let second = adapter
        .assemble(&source, &read, adapter.classify(&source, &read).unwrap())
        .unwrap()
        .into_model();
    assert_eq!(first, second);
    assert_eq!(
        first.turns[0].timestamp,
        Known::value("2026-04-08T21:48:23Z".to_owned())
    );
}

#[test]
fn junie_deliberate_corruption_self_test_marks_malformed_visible_loss() {
    let source = fixture_source(
        "corrupt",
        include_str!("../../../tests/fixtures/parser_engine/junie/corrupt_tail.events.jsonl"),
    );
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .unwrap();
    let adapter = JunieAdapter;
    let classified = adapter.classify(&source, &read).unwrap();
    assert!(classified.iter().any(|unit| matches!(
        &unit.disposition,
        ClassifiedDisposition::Skipped {
            reason: SkippedReason::Malformed,
            visible: true,
        }
    )));
    let model = adapter
        .assemble(&source, &read, classified)
        .unwrap()
        .into_model();
    assert!(model.coverage.status.visible_event_lost);
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
}

fn fixture_source(name: &str, body: &str) -> SourceHandle {
    fixture_source_with_session(name, body, "260408-214715-good")
}

fn fixture_source_with_session(name: &str, body: &str, logical_session_id: &str) -> SourceHandle {
    SourceHandle::new(
        AgentKind::Junie,
        format!("junie-{name}"),
        Some(logical_session_id.to_owned()),
        vec![
            SourceArtifact::memory("events.jsonl", body.as_bytes(), SourceFraming::JsonLines)
                .unwrap(),
        ],
    )
    .unwrap()
}

fn envelope_from(model: &SessionModel) -> serde_json::Value {
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
        "agent": "junie",
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
