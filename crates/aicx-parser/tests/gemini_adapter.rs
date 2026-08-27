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
    Known, ParseStatus, RawUnitReader, ReaderPolicy, SessionModel, SourceArtifact, SourceFraming,
    TurnKind, TurnRole, ValidatedParse, VisibleCompleteness, validate_parse,
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
