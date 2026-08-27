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

// ---------------------------------------------------------------------------
// 2026-08 schema change: user-run shell actions and control payloads arrive as
// `role=user` input_text messages. The adapter must classify plain echo as a
// timestamped human utterance, shell actions as a `$ cmd` ToolCall marker plus
// a verbatim ToolResult (the substrate keeps the full result), and control
// payloads as system turns. Projections filter on frame kind downstream.
// ---------------------------------------------------------------------------

fn dialogue_texts(model: &SessionModel) -> Vec<String> {
    model
        .turns
        .iter()
        .filter(|turn| matches!(turn.kind, TurnKind::UserMsg | TurnKind::AgentReply))
        .map(|turn| turn.text.clone())
        .collect()
}

#[test]
fn codex_shell_action_keeps_command_marker_and_retains_result() {
    let body = br#"{"timestamp":"2026-08-24T10:00:00Z","type":"session_meta","payload":{"id":"shell-envelope","cwd":"/repo"}}
{"timestamp":"2026-08-24T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"sprawdz wersje aicx na dragonie"}]}}
{"timestamp":"2026-08-24T10:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<user_shell_command>\n<command>\npsa loctree\n</command>\n<result>\nExit code: 0\nmaciejgad 40740 loctree-lsp --root /private/repo --stdio\n</result>\n</user_shell_command>"}]}}
{"timestamp":"2026-08-24T10:00:03Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"widze wynik, wersja siedzi"}]}}
"#;
    let model = model("shell-envelope", body);

    let dialogue = dialogue_texts(&model);
    assert_eq!(
        dialogue,
        vec![
            "sprawdz wersje aicx na dragonie".to_owned(),
            "widze wynik, wersja siedzi".to_owned(),
        ],
        "conversation frames must carry zero envelope bytes"
    );
    assert!(
        dialogue
            .iter()
            .all(|text| !text.contains("<user_shell_command") && !text.contains("<result>")),
        "no envelope markers may leak into dialogue frames"
    );

    let calls: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::ToolCall)
        .collect();
    assert_eq!(calls.len(), 1, "shell action becomes one call marker");
    assert_eq!(calls[0].role, TurnRole::Tool);
    assert_eq!(
        calls[0].tool_name,
        Known::value("user_shell_command".to_owned())
    );
    assert_eq!(calls[0].text, "$ psa loctree");
    assert!(!calls[0].text.contains("maciejgad 40740"));

    let results: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::ToolResult)
        .collect();
    assert_eq!(results.len(), 1, "the result is retained in the substrate");
    assert_eq!(results[0].role, TurnRole::Tool);
    assert_eq!(
        results[0].tool_name,
        Known::value("user_shell_command".to_owned())
    );
    assert!(results[0].text.starts_with("<user_shell_command>"));
    assert!(results[0].text.contains("maciejgad 40740"));
    assert!(
        results[0]
            .text
            .trim_end()
            .ends_with("</user_shell_command>"),
        "result turn preserves the verbatim envelope for full extraction"
    );
    let events: Vec<_> = model
        .tool_events
        .iter()
        .filter(|event| event.tool_name == "user_shell_command")
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        events,
        vec![
            aicx_parser::engine::ToolEventKind::Call,
            aicx_parser::engine::ToolEventKind::Result
        ],
        "marker and result each register a tool event"
    );

    // Order survives: user prose, call marker, result, assistant reply.
    let kinds: Vec<TurnKind> = model.turns.iter().map(|turn| turn.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TurnKind::UserMsg,
            TurnKind::ToolCall,
            TurnKind::ToolResult,
            TurnKind::AgentReply
        ]
    );
}

#[test]
fn codex_mixed_prose_and_envelope_preserves_prose() {
    let body = br#"{"timestamp":"2026-08-24T10:01:00Z","type":"session_meta","payload":{"id":"mixed","cwd":"/repo"}}
{"timestamp":"2026-08-24T10:01:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"zobacz co wyszlo:\n<user_shell_command>\n<command>\nls\n</command>\n<result>\nplik.txt\n</result>\n</user_shell_command>\ni powiedz co dalej"}]}}
"#;
    let model = model("mixed", body);
    let kinds: Vec<TurnKind> = model.turns.iter().map(|turn| turn.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TurnKind::UserMsg,
            TurnKind::ToolCall,
            TurnKind::ToolResult,
            TurnKind::UserMsg
        ],
        "prose survives on both sides of the stripped envelope"
    );
    assert_eq!(model.turns[0].text, "zobacz co wyszlo:");
    assert_eq!(model.turns[3].text, "i powiedz co dalej");
    assert_eq!(model.turns[1].text, "$ ls");
    assert!(model.turns[2].text.contains("plik.txt"));
}

#[test]
fn codex_literal_tag_mention_stays_dialogue() {
    let body = br#"{"timestamp":"2026-08-24T10:02:00Z","type":"session_meta","payload":{"id":"mention","cwd":"/repo"}}
{"timestamp":"2026-08-24T10:02:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"czemu `<user_shell_command>` pojawia sie w ekstrakcie?"}]}}
{"timestamp":"2026-08-24T10:02:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"przyklad zacytowany w fence:\n```\n<user_shell_command>\n<command>x</command>\n</user_shell_command>\n```\nto jest cytat do dyskusji"}]}}
"#;
    let model = model("mention", body);
    let kinds: Vec<TurnKind> = model.turns.iter().map(|turn| turn.kind).collect();
    assert_eq!(
        kinds,
        vec![TurnKind::UserMsg, TurnKind::UserMsg],
        "inline mention and fenced quotation are human dialogue, not envelopes"
    );
    assert_eq!(
        model.turns[0].text,
        "czemu `<user_shell_command>` pojawia sie w ekstrakcie?"
    );
    assert!(
        model.turns[1].text.contains("</user_shell_command>"),
        "fenced quotation stays byte-preserved in the dialogue turn"
    );
    assert!(model.tool_events.is_empty());
}

#[test]
fn codex_control_envelopes_become_system_notes() {
    let body = br#"{"timestamp":"2026-08-24T10:03:00Z","type":"session_meta","payload":{"id":"control","cwd":"/repo"}}
{"timestamp":"2026-08-24T10:03:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<codex_internal_context version=\"1\">\ngoal state snapshot\n</codex_internal_context>"}]}}
{"timestamp":"2026-08-24T10:03:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<INSTRUCTIONS>\nalways answer in haiku\n</INSTRUCTIONS>"}]}}
{"timestamp":"2026-08-24T10:03:03Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"a teraz normalne pytanie"}]}}
"#;
    let model = model("control", body);
    let kinds: Vec<TurnKind> = model.turns.iter().map(|turn| turn.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TurnKind::SystemNote,
            TurnKind::SystemNote,
            TurnKind::UserMsg
        ],
        "control envelopes leave the dialogue channel entirely"
    );
    assert_eq!(model.turns[0].role, TurnRole::System);
    assert_eq!(model.turns[1].role, TurnRole::System);
    assert_eq!(dialogue_texts(&model), vec!["a teraz normalne pytanie"]);
}

#[test]
fn codex_huge_shell_result_is_retained() {
    let huge = "x".repeat(50_000);
    let text = format!(
        "<user_shell_command>\n<command>\ncat big\n</command>\n<result>\n{huge}\n</result>\n</user_shell_command>"
    );
    let line = serde_json::json!({
        "timestamp": "2026-08-24T10:04:01Z",
        "type": "response_item",
        "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}
    });
    let body = format!(
        "{}\n{}\n",
        r#"{"timestamp":"2026-08-24T10:04:00Z","type":"session_meta","payload":{"id":"huge","cwd":"/repo"}}"#,
        serde_json::to_string(&line).expect("serialize huge line"),
    );
    let model = model("huge", body.as_bytes());
    assert!(
        dialogue_texts(&model).is_empty(),
        "no dialogue frames at all"
    );
    let calls: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::ToolCall)
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].text, "$ cat big");
    assert!(calls[0].text_chars < 50);
    let results: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::ToolResult)
        .collect();
    assert_eq!(results.len(), 1);
    assert!(
        results[0].text_chars > 50_000,
        "the substrate keeps the whole result; projections decide what to show"
    );
    assert!(results[0].text.contains(&huge));
}

#[test]
fn codex_unterminated_envelope_fails_closed() {
    let body = br#"{"timestamp":"2026-08-24T10:05:00Z","type":"session_meta","payload":{"id":"unterminated","cwd":"/repo"}}
{"timestamp":"2026-08-24T10:05:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<user_shell_command>\n<command>\nenv\n</command>\n<result>\nAWS_SECRET=truncated-mid-flight"}]}}
"#;
    let model = model("unterminated", body);
    assert!(
        dialogue_texts(&model).is_empty(),
        "a malformed envelope must never leak into dialogue — fail closed"
    );
    assert_eq!(
        model
            .turns
            .iter()
            .filter(|turn| turn.kind == TurnKind::ToolCall)
            .count(),
        1
    );
    assert_eq!(
        model
            .turns
            .iter()
            .filter(|turn| turn.kind == TurnKind::ToolResult)
            .count(),
        1,
        "the unterminated body is retained as a tool result, never as dialogue"
    );
}

#[test]
fn codex_plain_echo_is_timestamped_human_dialogue() {
    let body = r#"{"timestamp":"2026-08-25T04:37:54Z","type":"session_meta","payload":{"id":"echo-seal","cwd":"/repo"}}
{"timestamp":"2026-08-25T04:37:55.745Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<user_shell_command>\n<command>\necho 'bo instrument miał być jeden'\n</command>\n<result>\nExit code: 0\nOutput:\nbo instrument miał być jeden\n</result>\n</user_shell_command>"}]}}
"#;
    let model = model("echo-seal", body.as_bytes());

    assert_eq!(dialogue_texts(&model), vec!["bo instrument miał być jeden"]);
    assert_eq!(model.turns[0].role, TurnRole::User);
    assert_eq!(model.turns[0].kind, TurnKind::UserMsg);
    assert_eq!(
        model.turns[0].timestamp,
        Known::value("2026-08-25T04:37:55.745Z".to_owned())
    );
    assert!(model.tool_events.is_empty());
}

#[test]
fn codex_echo_with_tee_or_append_stays_shell_action() {
    let body = br#"{"timestamp":"2026-08-25T19:30:00Z","type":"session_meta","payload":{"id":"echo-guards","cwd":"/repo"}}
{"timestamp":"2026-08-25T19:30:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<user_shell_command>\n<command>\necho \"$(pbpaste)\" | tee /tmp/crash.log\n</command>\n<result>\nlarge tee output\n</result>\n</user_shell_command>"}]}}
{"timestamp":"2026-08-25T19:30:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<user_shell_command>\n<command>\necho more >> /tmp/crash.log\n</command>\n<result>\n</result>\n</user_shell_command>"}]}}
"#;
    let model = model("echo-guards", body);

    assert!(dialogue_texts(&model).is_empty());
    let markers: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::ToolCall)
        .map(|turn| turn.text.as_str())
        .collect();
    assert_eq!(
        markers,
        [
            "$ echo \"$(pbpaste)\" | tee /tmp/crash.log",
            "$ echo more >> /tmp/crash.log",
        ]
    );
    let results: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::ToolResult)
        .collect();
    assert_eq!(results.len(), 2);
    assert!(results[0].text.contains("large tee output"));
    assert!(
        model
            .turns
            .iter()
            .all(|turn| matches!(turn.kind, TurnKind::ToolCall | TurnKind::ToolResult))
    );
}

#[test]
fn codex_frozen_human_shape_has_25_human_plus_9_echo_seals() {
    let body = fixture("human_shape_01a0369f.jsonl");
    let model = model("01a0369f-9313-7592-8303-3db46b6f8b47", &body);
    let users: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::UserMsg)
        .collect();

    assert_eq!(users.len(), 34, "25 human + 9 echo-seal utterances");
    let instrument = users
        .iter()
        .find(|turn| turn.text == "bo instrument miał być jeden")
        .expect("04:37 echo-seal");
    assert_eq!(
        instrument.timestamp,
        Known::value("2026-08-25T04:37:55.745Z".to_owned())
    );
    let canary = users
        .iter()
        .find(|turn| turn.text == "daj mi prompt canary zatem")
        .expect("04:40 echo-seal");
    assert_eq!(
        canary.timestamp,
        Known::value("2026-08-25T04:40:59.057Z".to_owned())
    );
    assert!(
        model
            .turns
            .iter()
            .any(|turn| turn.kind == TurnKind::ToolCall
                && turn.text == "$ ~/.scripts/rust-target-cleaner.sh ~/vc-workspace"),
        "the command is a timeline marker"
    );
    assert!(
        model
            .turns
            .iter()
            .any(|turn| turn.kind == TurnKind::ToolResult
                && turn.text.contains("rust-target-cleaner output sentinel")),
        "the result dump is retained in the substrate (Decision 6)"
    );
    assert!(
        !model
            .turns
            .iter()
            .filter(|turn| turn.kind == TurnKind::UserMsg)
            .any(|turn| turn.text.contains("rust-target-cleaner output sentinel")),
        "the result dump never enters the dialogue channel"
    );
}

#[test]
fn codex_multiline_echo_bus_is_one_human_frame() {
    let body = r#"{"timestamp":"2026-08-27T10:00:00Z","type":"session_meta","payload":{"id":"echo-bus","cwd":"/repo"}}
{"timestamp":"2026-08-27T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<user_shell_command>\n<command>\necho 'Monika Szymańska: pierwszy cytat\nMaciej Gad: drugi cytat\nwniosek operatora'\n</command>\n<result>\nquoted transcript\n</result>\n</user_shell_command>"}]}}
"#;
    let model = model("echo-bus", body.as_bytes());
    let users: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::UserMsg)
        .collect();
    assert_eq!(users.len(), 1, "one echo envelope is one human frame");
    assert!(users[0].text.contains("Monika Szymańska:"));
    assert!(users[0].text.contains("Maciej Gad:"));
}

#[test]
fn codex_response_agent_message_is_inter_agent_not_assistant() {
    let body = br#"{"timestamp":"2026-08-27T11:00:00Z","type":"session_meta","payload":{"id":"inter-agent","cwd":"/repo"}}
{"timestamp":"2026-08-27T11:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"keep the session visible"}]}}
{"timestamp":"2026-08-27T11:00:02Z","type":"response_item","payload":{"type":"agent_message","author":"/root/worker","recipient":"/root","content":[{"type":"input_text","text":"Message Type: FINAL_ANSWER\nTask name: /root\nSender: /root/worker\nPayload:\nworker result"}]}}
"#;
    let model = model("inter-agent", body);
    assert_eq!(
        model
            .turns
            .iter()
            .filter(|turn| turn.kind == TurnKind::AgentReply)
            .count(),
        0,
        "agent_message must never leak into the assistant lane"
    );
    assert_eq!(model.coverage.consumed_of_kind("agent_message"), 1);
}

#[test]
fn codex_compaction_replay_is_hash_deduplicated_and_not_speech() {
    let body = br#"{"timestamp":"2026-08-27T12:00:00Z","type":"session_meta","payload":{"id":"compaction","cwd":"/repo"}}
{"timestamp":"2026-08-27T12:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"one live utterance"}]}}
{"timestamp":"2026-08-27T12:00:02Z","type":"compacted","payload":{"replacement_history":[{"type":"message","role":"user","content":[{"type":"input_text","text":"sticky prefix"}]},{"type":"message","role":"user","content":[{"type":"input_text","text":"sticky prefix"}]}]}}
"#;
    let model = model("compaction", body);
    assert_eq!(
        dialogue_texts(&model),
        vec!["one live utterance".to_owned()]
    );
    assert_eq!(
        model.coverage.skipped_for(SkippedReason::CompactionReplay),
        1
    );
    assert_eq!(model.coverage.skipped_for(SkippedReason::DuplicateBody), 1);
    assert!(
        model
            .coverage
            .status
            .boundary_flags
            .compaction_boundary_present
    );
}
