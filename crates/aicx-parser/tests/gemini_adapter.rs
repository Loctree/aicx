//! Gemini adapter structural and differential test suite.
//!
//! Exercises Gemini and Antigravity transport normalization against the shared
//! frame taxonomy throne. Tests are structured for post-embargo validation (W4).

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

#[path = "../src/adapters/gemini.rs"]
mod gemini;

use aicx_parser::engine::{
    DEFAULT_MAX_UNIT_BYTES, Known, RawUnitReader, ReaderPolicy, SourceArtifact, SourceFraming,
    TurnKind, TurnRole, ValidatedParse, VisibleCompleteness, WarningKind, validate_parse,
};
use gemini::{GEMINI_ADAPTER_VERSION, GeminiAdapter};
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(rel_path: &str) -> String {
    fs::read_to_string(repo_root().join("tests/fixtures").join(rel_path))
        .expect("read fixture file")
}

fn parse_session(session_id: &str, body: &str, framing: SourceFraming) -> ValidatedParse {
    let filename = if framing == SourceFraming::JsonLines {
        "session.jsonl"
    } else {
        "session.json"
    };
    let artifact =
        SourceArtifact::memory(filename, body.as_bytes().to_vec(), framing).expect("artifact");
    let source = SourceHandle::new(
        AgentKind::Gemini,
        session_id,
        Some(session_id.to_owned()),
        vec![artifact],
    )
    .expect("source handle");
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .expect("bounded read");
    let adapter = GeminiAdapter;
    let classified = adapter.classify(&source, &read).expect("classify");
    let parse = adapter
        .assemble(&source, &read, classified)
        .expect("assemble");
    validate_parse(parse).expect("validation")
}

#[test]
fn gemini_adapter_conforms_to_trait_and_avoids_discovery() {
    assert_eq!(GeminiAdapter.agent(), AgentKind::Gemini);
    assert_eq!(GeminiAdapter.adapter_version(), GEMINI_ADAPTER_VERSION);

    let source = include_str!("../src/adapters/gemini.rs");
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
fn gemini_minimal_whole_document_matches_oracle_envelope() {
    let body = fixture("parser_engine/gemini/minimal.json");
    let parse = parse_session(
        "33333333-3333-4333-8333-333333333333",
        &body,
        SourceFraming::WholeDocument,
    );
    let ValidatedParse::Session(session) = parse else {
        panic!("expected validated session");
    };
    let model = session.model();
    assert_eq!(model.turns.len(), 2);
    assert_eq!(model.turns[0].role, TurnRole::User);
    assert_eq!(model.turns[0].kind, TurnKind::UserMsg);
    assert_eq!(model.turns[0].text, "Build the Gemini oracle.");

    assert_eq!(model.turns[1].role, TurnRole::Assistant);
    assert_eq!(model.turns[1].kind, TurnKind::AgentReply);
    assert_eq!(model.turns[1].text, "The Gemini oracle is ready.");
}

#[test]
fn gemini_human_shape_9048328b_preserves_assistant_count_and_projects_user() {
    let body = fixture("parser_engine/gemini/human_shape_9048328b.jsonl");
    let parse = parse_session(
        "9048328b-1b17-4ffc-bdcd-e6f959d95432",
        &body,
        SourceFraming::JsonLines,
    );
    let ValidatedParse::Session(session) = parse else {
        panic!("expected validated session");
    };
    let model = session.model();

    // User turn classified through throne
    let user_turns: Vec<_> = model
        .turns
        .iter()
        .filter(|t| t.role == TurnRole::User)
        .collect();
    assert_eq!(user_turns.len(), 1, "raw user record must be projected");
    assert_eq!(user_turns[0].kind, TurnKind::UserMsg);
    assert!(user_turns[0].text.contains("run_id: just-194604-65503"));

    // Assistant turns not degraded below baseline (21)
    let assistant_turns: Vec<_> = model
        .turns
        .iter()
        .filter(|t| t.role == TurnRole::Assistant)
        .collect();
    assert!(
        assistant_turns.len() >= 21,
        "assistant count {} below baseline 21",
        assistant_turns.len()
    );

    // Shell tool events captured
    assert!(!model.tool_events.is_empty(), "shell tool events captured");
}

/// One whole-file session, shaped like the real `chats/session-*.json` that
/// motivated the bound: a few hundred bytes of speech and one tool result far
/// over the unit cap.
fn whale_session(session_id: &str, huge_result: &str, extra_message: Option<&str>) -> String {
    let mut messages = vec![
        serde_json::json!({
            "id": "m1",
            "timestamp": "2026-03-09T19:37:44.370Z",
            "type": "user",
            "content": [{"text": "resume"}]
        }),
        serde_json::json!({
            "id": "m2",
            "timestamp": "2026-03-09T19:37:49.214Z",
            "type": "gemini",
            "content": "Reading the file now.",
            "model": "gemini-2.5-pro",
            "toolCalls": [
                {"name": "run_shell_command", "args": {"command": "ls"}, "result": "ok"},
                {"name": "read_file", "args": {"path": "big.bin"}, "result": huge_result}
            ]
        }),
        serde_json::json!({
            "id": "m3",
            "timestamp": "2026-03-09T19:38:00.000Z",
            "type": "gemini",
            "content": "Done."
        }),
    ];
    if let Some(text) = extra_message {
        messages.push(serde_json::json!({
            "id": "m4",
            "timestamp": "2026-03-09T19:38:05.000Z",
            "type": "user",
            "content": text
        }));
    }
    serde_json::json!({
        "sessionId": session_id,
        "projectHash": "6ecfd1eb2c47ae58fb4b79b7210b8721bbb8af48123b428beab936ab3d619489",
        "startTime": "2026-03-09T19:37:43.121Z",
        "lastUpdated": "2026-03-13T01:08:28.570Z",
        "messages": messages
    })
    .to_string()
}

#[test]
fn gemini_oversized_tool_result_loses_only_itself() {
    let session_id = "44444444-4444-4444-8444-444444444444";
    let huge = "x".repeat(DEFAULT_MAX_UNIT_BYTES + 4096);
    let body = whale_session(session_id, &huge, None);
    assert!(
        body.len() > DEFAULT_MAX_UNIT_BYTES,
        "the document itself must be over the line cap for this to prove anything"
    );

    let parse = parse_session(session_id, &body, SourceFraming::WholeDocument);
    let ValidatedParse::Session(session) = parse else {
        panic!("a session with one oversized tool result is still a session");
    };
    let model = session.model();

    // Every word of speech survives, in order.
    let texts: Vec<&str> = model
        .turns
        .iter()
        .filter(|turn| matches!(turn.kind, TurnKind::UserMsg | TurnKind::AgentReply))
        .map(|turn| turn.text.as_str())
        .collect();
    assert_eq!(texts, vec!["resume", "Reading the file now.", "Done."]);
    assert_eq!(
        model.provenance.model,
        Known::value("gemini-2.5-pro".to_owned())
    );

    // The small tool call next to the whale survives; the whale does not.
    let tool_names: Vec<&str> = model
        .tool_events
        .iter()
        .map(|event| event.tool_name.as_str())
        .collect();
    assert_eq!(tool_names, vec!["run_shell_command"]);

    // The loss is recorded exactly once, as the unit it was, at the locator
    // it would have consumed under (message 1, second tool call).
    let coverage = &model.coverage;
    let oversized: Vec<_> = coverage
        .skipped
        .iter()
        .filter(|unit| unit.reason == aicx_parser::engine::SkippedReason::Oversized)
        .collect();
    assert_eq!(oversized.len(), 1);
    assert!(oversized[0].bytes > DEFAULT_MAX_UNIT_BYTES as u64);
    assert_eq!(oversized[0].evidence.locator, "000001:blk:1002");
    assert_eq!(oversized[0].evidence.unit_kind, "tool_call");
    assert!(oversized[0].visible);
    assert_eq!(
        coverage
            .warnings
            .iter()
            .filter(|warning| warning.kind == WarningKind::OversizedUnit)
            .map(|warning| warning.count)
            .sum::<u64>(),
        1
    );
    assert_eq!(
        coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
    assert!(coverage.status.visible_event_lost);

    // Every message that did not carry the whale hashes exactly as it would
    // have without the bound: identity of the survivors is untouched.
    let plain = whale_session(session_id, "tiny", None);
    let ValidatedParse::Session(plain_session) =
        parse_session(session_id, &plain, SourceFraming::WholeDocument)
    else {
        panic!("control session");
    };
    let hash_of = |session: &aicx_parser::engine::ValidatedSession, locator: &str| {
        session
            .model()
            .coverage
            .consumed
            .iter()
            .find(|unit| unit.evidence.locator == locator)
            .map(|unit| unit.evidence.content_hash.clone())
            .unwrap_or_else(|| panic!("consumed unit at {locator}"))
    };
    for locator in ["000001:blk:0", "000001:blk:2", "000001:blk:1001"] {
        assert_eq!(
            hash_of(&session, locator),
            hash_of(&plain_session, locator),
            "{locator} must not change identity because a sibling was oversized"
        );
    }
}

#[test]
fn gemini_oversized_speech_skips_that_message_and_keeps_the_session() {
    let session_id = "55555555-5555-4555-8555-555555555555";
    let wall_of_text = "y".repeat(DEFAULT_MAX_UNIT_BYTES + 4096);
    let body = whale_session(session_id, "tiny", Some(&wall_of_text));

    let parse = parse_session(session_id, &body, SourceFraming::WholeDocument);
    let ValidatedParse::Session(session) = parse else {
        panic!("one oversized message must not sink the session");
    };
    let model = session.model();
    let texts: Vec<&str> = model
        .turns
        .iter()
        .filter(|turn| matches!(turn.kind, TurnKind::UserMsg | TurnKind::AgentReply))
        .map(|turn| turn.text.as_str())
        .collect();
    assert_eq!(texts, vec!["resume", "Reading the file now.", "Done."]);

    let oversized: Vec<_> = model
        .coverage
        .skipped
        .iter()
        .filter(|unit| unit.reason == aicx_parser::engine::SkippedReason::Oversized)
        .collect();
    assert_eq!(
        oversized.len(),
        1,
        "nothing removable explains the size: the message is the unit"
    );
    assert_eq!(oversized[0].evidence.locator, "000001:blk:3");
    assert_eq!(oversized[0].evidence.unit_kind, "user");
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
}

#[test]
fn gemini_antigravity_conversation_parsed() {
    let body = fixture("frame_kind/gemini_antigravity_conversation.json");
    let parse = parse_session("antigravity-session", &body, SourceFraming::WholeDocument);
    let ValidatedParse::Session(session) = parse else {
        panic!("expected validated session");
    };
    let model = session.model();
    assert!(model.turns.iter().any(|t| t.kind == TurnKind::UserMsg));
    assert!(model.turns.iter().any(|t| t.kind == TurnKind::AgentReply));
    assert!(
        model
            .coverage
            .status
            .boundary_flags
            .opaque_reasoning_present
    );
}
