//! Cursor adapter structural and differential test suite.
//!
//! Exercises Cursor CLI agent-transcript normalization (`role`/`message.content`
//! JSONL plus `turn_ended` control rows) against the shared frame taxonomy
//! throne, including the harness `<timestamp>` / `<user_query>` wrapper idiom.

use aicx_parser::adapters::cursor::CURSOR_ADAPTER_VERSION;
use aicx_parser::adapters::{AgentAdapter, CursorAdapter};
use aicx_parser::engine::{
    AgentKind, Known, RawUnitReader, ReaderPolicy, SessionModel, SkippedReason, SourceArtifact,
    SourceFraming, SourceHandle, TurnKind, TurnRole, ValidatedParse, VisibleCompleteness,
    WarningKind, validate_parse,
};
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(rel_path: &str) -> String {
    fs::read_to_string(repo_root().join("tests/fixtures").join(rel_path))
        .expect("read fixture file")
}

fn parse_session(session_id: &str, body: &str) -> ValidatedParse {
    let artifact = SourceArtifact::memory(
        "session.jsonl",
        body.as_bytes().to_vec(),
        SourceFraming::JsonLines,
    )
    .expect("artifact");
    let source = SourceHandle::new(
        AgentKind::Cursor,
        session_id,
        Some(session_id.to_owned()),
        vec![artifact],
    )
    .expect("source handle");
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .expect("bounded read");
    let adapter = CursorAdapter;
    let classified = adapter.classify(&source, &read).expect("classify");
    let parse = adapter
        .assemble(&source, &read, classified)
        .expect("assemble");
    validate_parse(parse).expect("validation")
}

/// Oracle envelope projection (`parser_oracle.envelope.v1`) — same shape the
/// junie/claude/codex native golden lanes emit, so `compare.py --all` can run
/// the cursor `rust_golden` case against `cursor/expected.json`.
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
        "agent": "cursor",
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
fn cursor_native_golden_matches_reviewed_fixture() {
    let body = fixture("parser_engine/cursor/minimal.jsonl");
    let parse = parse_session("44444444-4444-4444-8444-444444444444", &body);
    let ValidatedParse::Session(session) = parse else {
        panic!("expected validated session");
    };
    let envelope = envelope_from(session.model());
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/parser_engine/cursor/expected.json"
    ))
    .expect("cursor native golden");

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
            .is_some_and(|text| text.contains("oracle") || text.contains("Cursor"))
    );
}

#[test]
fn cursor_adapter_conforms_to_trait_and_avoids_discovery() {
    assert_eq!(CursorAdapter.agent(), AgentKind::Cursor);
    assert_eq!(CursorAdapter.adapter_version(), CURSOR_ADAPTER_VERSION);

    let source = include_str!("../src/adapters/cursor.rs");
    for forbidden in [
        "read_dir(",
        "walkdir",
        "glob(",
        "Command::new",
        "std::process",
        "std::fs",
        "File::open",
        "dirs::",
        "std::env",
    ] {
        assert!(
            !source.contains(forbidden),
            "adapter source must not perform discovery: {forbidden}"
        );
    }
}

#[test]
fn cursor_minimal_jsonl_matches_oracle_envelope() {
    let body = fixture("parser_engine/cursor/minimal.jsonl");
    let parse = parse_session("44444444-4444-4444-8444-444444444444", &body);
    let ValidatedParse::Session(session) = parse else {
        panic!("expected validated session");
    };
    let model = session.model();
    assert_eq!(model.turns.len(), 2);
    assert_eq!(model.turns[0].role, TurnRole::User);
    assert_eq!(model.turns[0].kind, TurnKind::UserMsg);
    assert_eq!(model.turns[0].text, "Build the Cursor oracle.");

    assert_eq!(model.turns[1].role, TurnRole::Assistant);
    assert_eq!(model.turns[1].kind, TurnKind::AgentReply);
    assert_eq!(model.turns[1].text, "The Cursor oracle is ready.");

    assert_eq!(model.coverage.raw_line_count, 3);
    assert_eq!(model.coverage.consumed.len(), 3);
    assert!(model.coverage.skipped.is_empty());
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
}

#[test]
fn cursor_human_shape_wrapped_unwraps_operator_speech() {
    let body = fixture("parser_engine/cursor/human_shape_wrapped.jsonl");
    let parse = parse_session("77777777-7777-4777-8777-777777777777", &body);
    let ValidatedParse::Session(session) = parse else {
        panic!("expected validated session");
    };
    let model = session.model();

    // Both user records project as human speech.
    let user_turns: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.role == TurnRole::User)
        .collect();
    assert_eq!(user_turns.len(), 2, "raw user records must be projected");
    assert_eq!(user_turns[0].kind, TurnKind::UserMsg);
    assert_eq!(
        user_turns[0].text,
        "Run the following command: git status -sb && git fetch --all --prune -f -v"
    );

    // The wrapped record is stripped to operator speech: no <timestamp> or
    // <user_query> residue in the human lane.
    let wrapped = user_turns[1];
    assert_eq!(wrapped.text, "!git log --oneline -5");
    assert!(!wrapped.text.contains("<user_query>"));
    assert!(!wrapped.text.contains("<timestamp>"));

    // The harness timestamp wrapper becomes the turn's RFC 3339 timestamp.
    assert_eq!(
        wrapped.timestamp,
        Known::value("2026-08-29T08:50:00+02:00".to_owned())
    );

    // Shell tool_use blocks become tool turns + events with the command text.
    let tool_turns: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::ToolCall)
        .collect();
    assert_eq!(tool_turns.len(), 2, "both tool_use blocks projected");
    assert_eq!(tool_turns[0].tool_name, Known::value("Shell".to_owned()));
    assert_eq!(
        tool_turns[0].text,
        "git status -sb && git fetch --all --prune -f -v"
    );
    assert_eq!(model.tool_events.len(), 2);

    // turn_ended is consumed, not skipped: full visible coverage.
    assert_eq!(model.coverage.raw_line_count, 5);
    assert_eq!(model.coverage.consumed.len(), 5);
    assert!(model.coverage.skipped.is_empty());
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );

    // Provenance timing comes from the wrapper evidence.
    assert_eq!(
        model.provenance.started_at,
        Known::value("2026-08-29T08:50:00+02:00".to_owned())
    );
    assert_eq!(
        model.provenance.ended_at,
        Known::value("2026-08-29T08:50:00+02:00".to_owned())
    );
}

#[test]
fn cursor_malformed_and_unknown_lines_are_accounted() {
    let body = concat!(
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
        "not json at all\n",
        "{\"type\":\"mystery\"}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}\n",
    );
    let parse = parse_session("55555555-5555-4555-8555-555555555555", body);
    let ValidatedParse::Session(session) = parse else {
        panic!("expected validated session");
    };
    let model = session.model();

    assert_eq!(model.coverage.raw_line_count, 4);
    assert_eq!(model.coverage.consumed.len(), 2);
    assert_eq!(model.coverage.skipped.len(), 2);
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
    assert!(model.coverage.status.visible_event_lost);

    let reasons: Vec<_> = model
        .coverage
        .skipped
        .iter()
        .map(|unit| unit.reason)
        .collect();
    assert!(reasons.contains(&SkippedReason::Malformed));
    assert!(reasons.contains(&SkippedReason::UnknownPayloadType));
}

#[test]
fn cursor_malformed_nested_shapes_are_accounted_not_defaulted() {
    // Consumed physical units with malformed visible payloads inside:
    // a text block without a string `text`, a tool_use without a string
    // `name`, and a Shell tool_use without a string `command`. Fail-closed
    // contract: no fabricated empty turns, coverage degrades honestly.
    let body = concat!(
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"real reply\"}]}}\n",
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":42}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"input\":{\"command\":\"ls\"}}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Shell\",\"input\":{}}]}}\n",
        "{\"role\":\"user\"}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\"}]}}\n",
    );
    let parse = parse_session("88888888-8888-4888-8888-888888888888", body);
    let ValidatedParse::Session(session) = parse else {
        panic!("expected validated session");
    };
    let model = session.model();

    // Only the one well-formed block projects a turn.
    assert_eq!(
        model.turns.len(),
        1,
        "malformed blocks must not fabricate turns"
    );
    assert_eq!(model.turns[0].text, "real reply");
    assert!(model.tool_events.is_empty());

    // Physical units stay consumed, but the lost visible payloads are
    // accounted: partial visibility, loss flags up, warnings at real ordinals.
    assert_eq!(model.coverage.raw_line_count, 7);
    assert_eq!(model.coverage.consumed.len(), 7);
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
    assert!(model.coverage.status.visible_event_lost);
    assert!(
        model
            .coverage
            .status
            .boundary_flags
            .unsupported_visible_event
    );
    let warning = model
        .coverage
        .warnings
        .iter()
        .find(|w| w.kind == WarningKind::UnsupportedVisibleEvent)
        .expect("unsupported visible warning");
    assert_eq!(warning.count, 6);
    // The first malformed unit is the SECOND physical line: its real ordinal
    // (whatever base the reader uses) must ride the warning — never a flat 0.
    assert_eq!(
        warning.first_ordinal, model.coverage.consumed[1].ordinal,
        "warning must carry the real ordinal of the lossy unit"
    );
    assert!(
        warning.first_ordinal > model.coverage.consumed[0].ordinal,
        "ordinal must not collapse to the buggy constant 0"
    );
}

#[test]
fn cursor_timestamp_parser_rejects_off_shape_input() {
    let body = concat!(
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"<timestamp>yesterday-ish</timestamp>\\n<user_query>\\nhello\\n</user_query>\"}]}}\n",
        "{\"role\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
    );
    let parse = parse_session("66666666-6666-4666-8666-666666666666", body);
    let ValidatedParse::Session(session) = parse else {
        panic!("expected validated session");
    };
    let model = session.model();
    let user_turn = model
        .turns
        .iter()
        .find(|turn| turn.role == TurnRole::User)
        .expect("user turn");
    assert_eq!(user_turn.text, "hello");
    assert_eq!(user_turn.timestamp, Known::unknown());
    assert_eq!(model.provenance.started_at, Known::unknown());
}
