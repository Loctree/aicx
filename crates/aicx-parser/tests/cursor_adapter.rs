//! Cursor adapter contract suite: the registered adapter over real-shape
//! agent-transcript JSONL fixtures, driven through the shared adapter boundary.

use aicx_parser::adapters::registered_adapter;
use aicx_parser::engine::{
    AgentKind, ParserEngine, ReaderPolicy, SkippedReason, SourceArtifact, SourceFraming,
    SourceHandle, ToolEventKind, TurnKind, TurnRole, ValidatedParse, VisibleCompleteness,
    WarningKind,
};

fn source(session_id: &str, body: &[u8]) -> SourceHandle {
    SourceHandle::new(
        AgentKind::Cursor,
        session_id,
        Some(session_id.to_owned()),
        vec![
            SourceArtifact::memory("transcript.jsonl", body.to_vec(), SourceFraming::JsonLines)
                .expect("memory source"),
        ],
    )
    .expect("explicit source handle")
}

fn parse(session_id: &str, body: &[u8]) -> ValidatedParse {
    ParserEngine::new(ReaderPolicy::default())
        .parse_registered(&source(session_id, body))
        .expect("cursor parse must close without panic")
}

fn assert_closed_coverage(parse: &ValidatedParse) {
    let coverage = match parse {
        ValidatedParse::Session(session) => &session.model().coverage,
        ValidatedParse::Fatal(fatal) => fatal.coverage(),
    };
    assert_eq!(
        coverage.consumed_count + coverage.skipped_count,
        coverage.raw_unit_count,
        "every physical and logical unit must terminate consumed XOR skipped"
    );
    assert_eq!(
        coverage.raw_unit_count,
        coverage.consumed.len() as u64 + coverage.skipped.len() as u64
    );
}

#[test]
fn cursor_is_registered_with_identity_and_version() {
    let adapter = registered_adapter(AgentKind::Cursor);
    assert_eq!(adapter.agent(), AgentKind::Cursor);
    assert_eq!(adapter.adapter_version(), "cursor-transcript-v1");
}

#[test]
fn own_session_fixture_projects_the_config_conversation() {
    let body = include_bytes!("../../../tests/fixtures/parser_engine/cursor/own_session.jsonl");
    let ValidatedParse::Session(session) = parse("e1789670-9ae3-4e58-8d2d-9becef7add4a", body)
    else {
        panic!("own session must project a model");
    };
    let model = session.model();
    assert_eq!(model.provenance.agent, AgentKind::Cursor);
    assert_closed_coverage(&ValidatedParse::Session(session.clone()));
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    assert_eq!(model.turns[0].role, TurnRole::User);
    assert_eq!(model.turns[0].kind, TurnKind::UserMsg);
    assert_eq!(model.turns[0].text, "config");
    let assistant: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::AgentReply)
        .collect();
    assert!(
        assistant
            .iter()
            .any(|turn| turn.text.contains("config landscape")),
        "assistant reply must read as the real conversation"
    );
    let tools: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::ToolCall)
        .map(|turn| match &turn.tool_name {
            aicx_parser::engine::Known::Value(name) => name.as_str(),
            aicx_parser::engine::Known::Unknown(_) => "",
        })
        .collect();
    assert_eq!(tools, ["AskQuestion", "Read", "Glob"]);
    assert_eq!(model.tool_events.len(), 3);
    assert!(
        model
            .tool_events
            .iter()
            .all(|event| event.kind == ToolEventKind::Call)
    );
    assert_eq!(
        model.coverage.consumed_of_kind("turn_ended"),
        1,
        "turn_ended is recognized metadata, not a silent drop"
    );
}

#[test]
fn minimal_fixture_accounts_user_assistant_tool_and_turn_ended() {
    let body = include_bytes!("../../../tests/fixtures/parser_engine/cursor/minimal.jsonl");
    let ValidatedParse::Session(session) = parse("cursor-minimal", body) else {
        panic!("session");
    };
    let model = session.model();
    assert_closed_coverage(&ValidatedParse::Session(session.clone()));
    let roles: Vec<_> = model
        .turns
        .iter()
        .map(|turn| (turn.role, turn.kind))
        .collect();
    assert_eq!(
        roles,
        [
            (TurnRole::User, TurnKind::UserMsg),
            (TurnRole::Assistant, TurnKind::AgentReply),
            (TurnRole::Tool, TurnKind::ToolCall),
        ]
    );
    assert_eq!(model.turns[0].text, "config");
    assert!(matches!(
        &model.turns[0].timestamp,
        aicx_parser::engine::Known::Unknown(_)
    ));
    assert_eq!(model.coverage.consumed_count, 6);
    assert_eq!(model.coverage.skipped_count, 0);
    assert_eq!(model.coverage.raw_line_count, 3);
    assert_eq!(model.coverage.raw_unit_count, 6);
}

#[test]
fn malformed_input_fail_closes_with_typed_warning() {
    let body = include_bytes!("../../../tests/fixtures/parser_engine/cursor/malformed.jsonl");
    let parsed = parse("cursor-malformed", body);
    assert_closed_coverage(&parsed);
    let ValidatedParse::Session(session) = parsed else {
        panic!("malformed tail must not fatal a session that already has a user turn");
    };
    let model = session.model();
    assert_eq!(model.turns[0].text, "hello");
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
    assert!(
        model.coverage.status.visible_event_lost || model.coverage.status.malformed_tail_present
    );
    assert!(
        model
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.reason == SkippedReason::Malformed && unit.visible)
    );
    assert!(
        model
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.kind == WarningKind::MalformedUnit)
    );
}

#[test]
fn unknown_payload_is_skipped_visible_not_panic() {
    let body = br#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>\nhi\n</user_query>"}]}}
{"type":"future_visible_event","payload":{"shape":"unknown"}}
"#;
    let parsed = parse("cursor-unknown", body);
    assert_closed_coverage(&parsed);
    let ValidatedParse::Session(session) = parsed else {
        panic!("unknown envelope must not fatal");
    };
    let model = session.model();
    assert!(
        model
            .coverage
            .status
            .boundary_flags
            .unsupported_visible_event
    );
    assert!(
        model
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.reason == SkippedReason::UnknownPayloadType && unit.visible)
    );
}

/// Walk-around: parse a full live Cursor transcript when present on this
/// machine. Absence is not a failure — CI does not carry operator stores.
/// Explicitly opt-in (`AICX_WALKAROUND_LIVE=1`): a live read prints real
/// conversation previews to stderr, which must never leak into local or CI
/// logs from a default test run.
#[test]
fn walk_around_live_own_session_if_present() {
    if std::env::var_os("AICX_WALKAROUND_LIVE").is_none() {
        return;
    }
    let path = std::path::PathBuf::from(env!("HOME")).join(
        ".cursor/projects/Users-polyversai-vibecrafted-worktrees-vetcoders-vibecrafted-2026-0829-cursor-260829/agent-transcripts/e1789670-9ae3-4e58-8d2d-9becef7add4a/e1789670-9ae3-4e58-8d2d-9becef7add4a.jsonl",
    );
    if !path.is_file() {
        return;
    }
    let body = std::fs::read(&path).expect("live transcript readable");
    let parsed = parse("e1789670-9ae3-4e58-8d2d-9becef7add4a", &body);
    assert_closed_coverage(&parsed);
    let ValidatedParse::Session(session) = parsed else {
        panic!("live own session must project");
    };
    let model = session.model();
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    assert_eq!(model.turns[0].text, "config");
    eprintln!(
        "WALKAROUND live e1789670: lines={} units={} consumed={} skipped={} turns={}",
        model.coverage.raw_line_count,
        model.coverage.raw_unit_count,
        model.coverage.consumed_count,
        model.coverage.skipped_count,
        model.turns.len()
    );
    for turn in model.turns.iter().take(6) {
        let preview: String = turn.text.chars().take(120).collect();
        eprintln!(
            "  t{} {:?} {:?}: {preview}",
            turn.turn_idx, turn.role, turn.kind
        );
    }
}
