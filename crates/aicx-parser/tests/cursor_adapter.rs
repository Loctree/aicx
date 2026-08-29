//! Cursor adapter structural and differential test suite.
//!
//! Exercises Cursor CLI agent-transcript normalization (`role`/`message.content`
//! JSONL plus `turn_ended` control rows) against the shared frame taxonomy
//! throne, including the harness `<timestamp>` / `<user_query>` wrapper idiom.

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

#[path = "../src/adapters/cursor.rs"]
mod cursor;

use aicx_parser::engine::{
    Known, RawUnitReader, ReaderPolicy, SourceArtifact, SourceFraming, TurnKind, TurnRole,
    ValidatedParse, VisibleCompleteness, validate_parse,
};
use cursor::{CURSOR_ADAPTER_VERSION, CursorAdapter};
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
fn cursor_human_shape_004ffd2e_unwraps_operator_speech() {
    let body = fixture("parser_engine/cursor/human_shape_004ffd2e.jsonl");
    let parse = parse_session("004ffd2e-8b2a-4a41-bd7d-dee9f7df9950", &body);
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
    assert_eq!(
        wrapped.text,
        "!security unlock-keychain -p \"$(pbpaste)\" \"$HOME/Library/Keychains/login.keychain-db\""
    );
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
