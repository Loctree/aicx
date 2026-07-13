//! Claude adapter differential + contract suite (C3).
//!
//! Wave convention: the adapter source is compiled into this test crate via
//! `#[path]` behind a shadow of the frozen `adapters::mod` boundary, so the
//! cut never edits the shared boundary file. C5X convergence registers the
//! module in-crate; the shadow below is byte-compatible with the frozen trait.

mod engine {
    pub use aicx_parser::engine::*;
}

mod sealed {
    pub trait Sealed {}
}

mod skill_collapse {
    pub use aicx_parser::skill_collapse::detect_skill_marker;
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

#[path = "../src/adapters/claude.rs"]
mod claude;

use aicx_parser::engine::{
    CounterSemantics, Known, ParseStatus, RawUnitReader, ReaderPolicy, SessionModel,
    SourceArtifact, SourceFraming, ToolEventKind, TurnKind, TurnRole, ValidatedParse,
    VisibleCompleteness, WarningKind, validate_parse,
};
use claude::{CLAUDE_ADAPTER_VERSION, ClaudeAdapter};
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn memory_source(session_id: &str, body: &str) -> SourceHandle {
    let artifact = SourceArtifact::memory(
        "session.jsonl",
        body.as_bytes().to_vec(),
        SourceFraming::JsonLines,
    )
    .expect("memory artifact");
    SourceHandle::new(
        AgentKind::Claude,
        session_id,
        Some(session_id.to_owned()),
        vec![artifact],
    )
    .expect("source handle")
}

fn parse_session(session_id: &str, body: &str) -> ValidatedParse {
    let source = memory_source(session_id, body);
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .expect("bounded read");
    let adapter = ClaudeAdapter;
    let classified = adapter.classify(&source, &read).expect("classification");
    let parse = adapter
        .assemble(&source, &read, classified)
        .expect("assembly");
    validate_parse(parse).expect("kernel validation")
}

fn session_model(session_id: &str, body: &str) -> SessionModel {
    match parse_session(session_id, body) {
        ValidatedParse::Session(session) => session.into_model(),
        ValidatedParse::Fatal(fatal) => {
            panic!("unexpected fatal parse: {:?}", fatal.coverage().status)
        }
    }
}

fn fixture(name: &str) -> String {
    fs::read_to_string(
        repo_root()
            .join("tests/fixtures/parser_engine/claude")
            .join(name),
    )
    .expect("read claude fixture")
}

fn status(model: &SessionModel) -> ParseStatus {
    model.coverage.status
}

// ---------------------------------------------------------------------------
// Differential oracle envelope (donor golden, frozen by C0)
// ---------------------------------------------------------------------------

fn envelope_from(parse: &ValidatedParse, agent: &str) -> serde_json::Value {
    let (coverage, session_id, turns, usage) = match parse {
        ValidatedParse::Session(session) => {
            let model = session.model();
            (
                &model.coverage,
                model.session_id.clone(),
                model.turns.clone(),
                model.usage_events.clone(),
            )
        }
        ValidatedParse::Fatal(fatal) => (fatal.coverage(), String::new(), Vec::new(), Vec::new()),
    };
    let physical = coverage.raw_line_count;
    let consumed_physical = coverage
        .consumed
        .iter()
        .filter(|unit| unit.ordinal <= physical)
        .count() as u64;
    let skipped_physical = coverage
        .skipped
        .iter()
        .filter(|unit| unit.ordinal <= physical)
        .count() as u64;
    let visible_turns: Vec<serde_json::Value> = turns
        .iter()
        .filter(|turn| matches!(turn.kind, TurnKind::UserMsg | TurnKind::AgentReply))
        .enumerate()
        .map(|(ordinal, turn)| {
            serde_json::json!({
                "ordinal": ordinal as u64,
                "role": match turn.role {
                    TurnRole::User => "user",
                    _ => "assistant",
                },
                "kind": "message",
                "text": turn.text,
            })
        })
        .collect();
    let mut boundaries: Vec<&str> = Vec::new();
    if coverage.status.boundary_flags.opaque_reasoning_present {
        boundaries.push("opaque_reasoning_present");
    }
    if coverage.status.boundary_flags.unsupported_visible_event {
        boundaries.push("unsupported_visible_event");
    }
    let visible = match coverage.status.visible_completeness {
        VisibleCompleteness::CompleteVisible => "complete_visible",
        VisibleCompleteness::PartialVisible => "partial_visible",
        VisibleCompleteness::Fatal => "fatal",
    };
    let intent_summary = turns
        .iter()
        .find(|turn| turn.kind == TurnKind::UserMsg)
        .map(|turn| turn.text.clone())
        .unwrap_or_default();
    serde_json::json!({
        "schema": "parser_oracle.envelope.v1",
        "agent": agent,
        "session_id": session_id,
        "visible_turns": visible_turns,
        "coverage": {
            "raw_units": physical,
            "consumed": consumed_physical,
            "skipped": skipped_physical,
        },
        "status": { "visible": visible, "boundaries": boundaries },
        "usage": usage
            .iter()
            .map(|event| serde_json::to_value(event).expect("usage event serializes"))
            .collect::<Vec<_>>(),
        "heuristic": { "intent_summary": intent_summary },
    })
}

#[test]
fn claude_oracle_envelope_matches_frozen_golden() {
    let body = fixture("minimal.jsonl");
    let parse = parse_session("22222222-2222-4222-8222-222222222222", &body);
    let envelope = envelope_from(&parse, "claude");

    let expected: serde_json::Value =
        serde_json::from_str(&fixture("expected.json")).expect("frozen golden parses");
    for field in [
        "agent",
        "session_id",
        "visible_turns",
        "coverage",
        "status",
        "usage",
    ] {
        assert_eq!(
            envelope.get(field),
            expected.get(field),
            "exact mismatch at $.{field}"
        );
    }
    let summary = envelope["heuristic"]["intent_summary"]
        .as_str()
        .expect("intent summary");
    assert!(summary.contains("oracle") && summary.contains("contract"));

    let out_dir = repo_root().join("target/parser_oracle");
    fs::create_dir_all(&out_dir).expect("create envelope dir");
    fs::write(
        out_dir.join("claude_minimal.envelope.json"),
        serde_json::to_vec_pretty(&envelope).expect("envelope serializes"),
    )
    .expect("write envelope artifact");
}

// ---------------------------------------------------------------------------
// Content blocks, tool pairs, usage, segments, skills
// ---------------------------------------------------------------------------

#[test]
fn claude_rich_session_models_blocks_tools_usage_segments() {
    let session = "33333333-3333-4333-8333-333333333333";
    let body = fixture("rich.jsonl");
    let model = session_model(session, &body);

    assert_eq!(model.coverage.raw_line_count, 9);
    assert_eq!(model.coverage.raw_unit_count, 14, "9 physical + 5 logical");
    assert_eq!(model.coverage.consumed_count, 14);
    assert_eq!(model.coverage.skipped_count, 0);
    assert_eq!(
        status(&model).visible_completeness,
        VisibleCompleteness::CompleteVisible
    );

    let consumed_kinds: Vec<&str> = model
        .coverage
        .consumed
        .iter()
        .map(|unit| unit.kind.as_str())
        .collect();
    for kind in [
        "metadata_record",
        "user",
        "assistant",
        "system",
        "text_block",
        "thinking_block",
        "tool_use_block",
        "tool_result_block",
    ] {
        assert!(
            consumed_kinds.contains(&kind),
            "missing consumed kind {kind}"
        );
    }

    // Turns: user, thinking, assistant text, tool call, tool result, user
    // (skill), assistant text — system rows are metadata, never chat.
    let kinds: Vec<TurnKind> = model.turns.iter().map(|turn| turn.kind).collect();
    assert_eq!(
        kinds,
        vec![
            TurnKind::UserMsg,
            TurnKind::InternalThought,
            TurnKind::AgentReply,
            TurnKind::ToolCall,
            TurnKind::ToolResult,
            TurnKind::UserMsg,
            TurnKind::AgentReply,
        ]
    );
    assert_eq!(model.turns[1].role, TurnRole::Assistant);
    assert_eq!(model.turns[4].role, TurnRole::Tool);
    assert!(
        model.turns[4].text.contains("test result: ok")
            && model.turns[4]
                .text
                .contains("[tool_result non-text content: image]"),
        "non-text tool_result blocks stay visible via the donor sentinel"
    );

    // Block-level turns retain parent (physical) + child (logical) identity.
    for turn_idx in [1_usize, 2, 3, 4] {
        let refs = &model.turns[turn_idx].raw_unit_refs;
        assert_eq!(refs.len(), 2, "turn {turn_idx} carries parent+child refs");
        assert!(refs[0].locator.len() == 6, "parent locator is the ordinal");
        assert!(
            refs[1].locator.contains(":blk:"),
            "child locator is block-scoped"
        );
        assert_eq!(refs[0].physical_ordinal, refs[1].physical_ordinal);
    }

    // Tool pair correlated by tool_use id, result resolves the tool name.
    assert_eq!(model.tool_events.len(), 2);
    let call = &model.tool_events[0];
    let result = &model.tool_events[1];
    assert_eq!(call.kind, ToolEventKind::Call);
    assert_eq!(call.tool_name, "Bash");
    assert_eq!(call.correlation_id, Known::value("toolu_01".to_owned()));
    assert_eq!(result.kind, ToolEventKind::Result);
    assert_eq!(result.tool_name, "Bash");
    assert_eq!(result.correlation_id, Known::value("toolu_01".to_owned()));

    // Usage: typed delta events with per-event model provenance (drift legal),
    // unknown components stay unknown, reported cost only.
    assert_eq!(model.usage_events.len(), 2);
    let first = &model.usage_events[0];
    assert_eq!(first.provider, "anthropic");
    assert_eq!(first.model, Known::value("claude-opus-4-8".to_owned()));
    assert_eq!(first.counter_semantics, CounterSemantics::Delta);
    assert_eq!(first.tokens.input, Known::value(1200));
    assert_eq!(first.tokens.cache_read, Known::value(18000));
    assert_eq!(first.tokens.cache_creation, Known::value(2400));
    assert_eq!(first.tokens.reasoning, Known::unknown());
    assert_eq!(first.cost, Known::unknown());
    let second = &model.usage_events[1];
    assert_eq!(second.model, Known::value("claude-sonnet-5".to_owned()));
    assert_eq!(second.tokens.cache_read, Known::unknown());
    match &second.cost {
        Known::Value(cost) => {
            assert!((cost.amount - 0.4185).abs() < 1e-9);
            assert_eq!(cost.currency, "USD");
        }
        Known::Unknown(_) => panic!("reported costUSD must survive as typed cost"),
    }
    assert!(second.evidence.evidence_event_id.starts_with("ev1:claude:"));

    // cwd change opens a new segment; coverage of turns is exact.
    assert_eq!(model.segments.len(), 2);
    assert_eq!(
        model.segments[0].cwd,
        Known::value("/repo/alpha".to_owned())
    );
    assert_eq!(model.segments[1].cwd, Known::value("/repo/beta".to_owned()));
    assert_eq!(model.segments[0].turn_range.start, 0);
    assert_eq!(model.segments[0].turn_range.end, 4);
    assert_eq!(model.segments[1].turn_range.start, 5);
    assert_eq!(model.segments[1].turn_range.end, 6);

    // Skill boilerplate: detected AND full literal operator content retained.
    assert_eq!(model.skill_invocations.len(), 1);
    let skill = &model.skill_invocations[0];
    assert_eq!(skill.skill_name, "vc-implement");
    assert_eq!(skill.turn_idx, 5);
    assert!(
        model.turns[5]
            .text
            .contains("ARGUMENTS: dokoncz przeniesienie"),
        "skill payload text must not be truncated by the parser"
    );

    // Provenance from first carriers; adapter identity is frozen.
    assert_eq!(ClaudeAdapter.adapter_version(), CLAUDE_ADAPTER_VERSION);
    assert_eq!(model.session_id, session);
    assert_eq!(
        model.provenance.model,
        Known::value("claude-opus-4-8".to_owned())
    );
    assert_eq!(
        model.provenance.cli_version,
        Known::value("2.0.1".to_owned())
    );
    assert_eq!(model.provenance.cwd, Known::value("/repo/alpha".to_owned()));
    assert_eq!(
        model.provenance.started_at,
        Known::value("2026-07-13T05:00:00Z".to_owned())
    );
    assert_eq!(
        model.provenance.ended_at,
        Known::value("2026-07-13T05:01:06Z".to_owned())
    );

    // Determinism: identical bytes -> identical validated model bytes.
    let again = session_model(session, &body);
    assert_eq!(
        serde_json::to_vec(&model).expect("model serializes"),
        serde_json::to_vec(&again).expect("model serializes"),
    );
}

// ---------------------------------------------------------------------------
// Status truth table on native inputs (C0A §3.3)
// ---------------------------------------------------------------------------

#[test]
fn claude_opaque_thinking_alone_stays_complete_visible() {
    let model = session_model(
        "44444444-4444-4444-8444-444444444444",
        &fixture("opaque_thinking.jsonl"),
    );
    let parse_status = status(&model);
    assert_eq!(
        parse_status.visible_completeness,
        VisibleCompleteness::CompleteVisible,
        "opaque reasoning alone never degrades visible completeness"
    );
    assert!(parse_status.boundary_flags.opaque_reasoning_present);
    assert!(!parse_status.malformed_tail_present);
    assert!(!parse_status.visible_event_lost);
    let opaque = model
        .coverage
        .skipped
        .iter()
        .find(|unit| unit.reason == SkippedReason::EncryptedOpaque)
        .expect("redacted_thinking terminates as skipped(encrypted_opaque)");
    assert!(!opaque.visible);
    assert!(
        model
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.kind == WarningKind::OpaqueReasoning)
    );
    assert!(
        model
            .turns
            .iter()
            .any(|turn| turn.text == "Widoczna odpowiedz."),
        "visible sibling text block still projects"
    );
}

#[test]
fn claude_malformed_middle_line_is_concrete_visible_loss() {
    let model = session_model(
        "55555555-5555-4555-8555-555555555555",
        &fixture("malformed_middle.jsonl"),
    );
    let parse_status = status(&model);
    assert_eq!(
        parse_status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
    assert!(parse_status.visible_event_lost);
    assert!(!parse_status.malformed_tail_present);
    let malformed = model
        .coverage
        .skipped
        .iter()
        .find(|unit| unit.reason == SkippedReason::Malformed)
        .expect("malformed line is typed data, never silence");
    assert_eq!(malformed.ordinal, 2);
    assert!(
        model
            .turns
            .iter()
            .any(|turn| turn.text == "Nadal widoczne."),
        "a malformed middle line never erases later valid units"
    );
}

#[test]
fn claude_malformed_tail_forbids_complete_visible() {
    let model = session_model(
        "55555555-5555-4555-8555-555555555555",
        &fixture("malformed_tail.jsonl"),
    );
    let parse_status = status(&model);
    assert!(parse_status.malformed_tail_present);
    assert_eq!(
        parse_status.visible_completeness,
        VisibleCompleteness::PartialVisible
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
fn claude_unknown_row_and_block_preserved_as_unsupported() {
    let model = session_model(
        "66666666-6666-4666-8666-666666666666",
        &fixture("unknown_shapes.jsonl"),
    );
    let parse_status = status(&model);
    assert_eq!(
        parse_status.visible_completeness,
        VisibleCompleteness::CompleteVisible,
        "preservation is not loss"
    );
    assert!(parse_status.boundary_flags.unsupported_visible_event);
    let unknown_units: Vec<_> = model
        .coverage
        .skipped
        .iter()
        .filter(|unit| unit.reason == SkippedReason::UnknownPayloadType)
        .collect();
    assert_eq!(
        unknown_units.len(),
        2,
        "unknown row + unknown content block"
    );
    assert!(unknown_units.iter().all(|unit| unit.visible));
    let warning = model
        .coverage
        .warnings
        .iter()
        .find(|warning| warning.kind == WarningKind::UnknownPayloadType)
        .expect("typed warning for every unknown payload");
    assert_eq!(warning.count, 2);
}

// ---------------------------------------------------------------------------
// Claude history is NOT a session — explicit non-conflation contract
// ---------------------------------------------------------------------------

#[test]
fn claude_history_rows_never_become_a_session() {
    let parse = parse_session("history-handle", &fixture("history_rows.jsonl"));
    let ValidatedParse::Fatal(fatal) = parse else {
        panic!("~/.claude/history.jsonl rows must never validate as a session");
    };
    assert_eq!(
        fatal.coverage().status.visible_completeness,
        VisibleCompleteness::Fatal
    );
    assert_eq!(fatal.coverage().consumed_count, 0);
    assert_eq!(fatal.coverage().skipped_count, 2);
    assert!(
        fatal
            .coverage()
            .skipped
            .iter()
            .all(|unit| unit.reason == SkippedReason::UnknownPayloadType),
        "history rows are unknown payloads for the session adapter"
    );
}

// ---------------------------------------------------------------------------
// Session-id drift: identity is locator-owned, rows are data
// ---------------------------------------------------------------------------

#[test]
fn claude_session_id_drift_is_locator_owned() {
    let model = session_model("drift-handle", &fixture("session_drift.jsonl"));
    assert_eq!(model.session_id, "drift-handle");
    assert_eq!(model.coverage.skipped_count, 0);
    assert_eq!(
        status(&model).visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    for unit in &model.coverage.consumed {
        assert!(
            unit.evidence
                .evidence_event_id
                .starts_with("ev1:claude:drift-handle:"),
            "evidence identity derives from the handle, never from drifting rows"
        );
    }
}

// ---------------------------------------------------------------------------
// Evidence identity: append-stable, mutation-scoped (derivation v1)
// ---------------------------------------------------------------------------

fn evidence_ids(model: &SessionModel) -> Vec<String> {
    model
        .coverage
        .consumed
        .iter()
        .map(|unit| unit.evidence.evidence_event_id.clone())
        .chain(
            model
                .coverage
                .skipped
                .iter()
                .map(|unit| unit.evidence.evidence_event_id.clone()),
        )
        .collect()
}

#[test]
fn claude_evidence_ids_are_append_stable_and_mutation_scoped() {
    let session = "99999999-9999-4999-8999-999999999999";
    let user = format!(
        "{{\"type\":\"user\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-07-13T10:00:00Z\",\"message\":{{\"role\":\"user\",\"content\":\"Pierwsza.\"}}}}"
    );
    let assistant = format!(
        "{{\"type\":\"assistant\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-07-13T10:00:02Z\",\"message\":{{\"role\":\"assistant\",\"model\":\"claude-opus-4-8\",\"content\":[{{\"type\":\"text\",\"text\":\"Odpowiedz.\"}}]}}}}"
    );
    let appended_row = format!(
        "{{\"type\":\"user\",\"sessionId\":\"{session}\",\"timestamp\":\"2026-07-13T10:00:05Z\",\"message\":{{\"role\":\"user\",\"content\":\"Druga.\"}}}}"
    );
    let base_body = format!("{user}\n{assistant}\n");
    let appended_body = format!("{user}\n{assistant}\n{appended_row}\n");
    let mutated_body = format!(
        "{user}\n{}\n",
        assistant.replace("Odpowiedz.", "Zmieniona odpowiedz.")
    );

    let base = session_model(session, &base_body);
    let appended = session_model(session, &appended_body);
    let mutated = session_model(session, &mutated_body);

    let base_ids = evidence_ids(&base);
    let appended_ids = evidence_ids(&appended);
    let mutated_ids = evidence_ids(&mutated);

    // Uniqueness within each parse.
    for ids in [&base_ids, &appended_ids, &mutated_ids] {
        let unique: std::collections::BTreeSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }
    // Append preserves every prior id byte-for-byte.
    for id in &base_ids {
        assert!(
            appended_ids.contains(id),
            "append must not disturb prior evidence id {id}"
        );
    }
    assert_eq!(appended_ids.len(), base_ids.len() + 1);
    // Mutating one raw unit changes exactly that physical unit and its
    // nested logical child; the untouched unit keeps its identity.
    let survivors: Vec<_> = base_ids
        .iter()
        .filter(|id| mutated_ids.contains(*id))
        .collect();
    assert_eq!(
        survivors.len(),
        1,
        "only the untouched user line keeps its id (physical+logical of the mutated line change)"
    );
    assert!(survivors[0].contains(":user:"));
}

// ---------------------------------------------------------------------------
// Frozen API conformance, no discovery, explicit input shape
// ---------------------------------------------------------------------------

#[test]
fn claude_adapter_performs_no_discovery_and_conforms_to_the_frozen_trait() {
    #[allow(dead_code)]
    fn conforms<A: AgentAdapter>(adapter: &A) -> AgentKind {
        adapter.agent()
    }
    assert_eq!(conforms(&ClaudeAdapter), AgentKind::Claude);
    assert!(!ClaudeAdapter.adapter_version().is_empty());

    let source = include_str!("../src/adapters/claude.rs");
    for forbidden in [
        "read_dir(",
        "walkdir",
        "glob(",
        "Command::new",
        "std::process",
        "std::fs",
        "File::open",
        "open_file_validated",
        "dirs::",
        "std::env",
    ] {
        assert!(
            !source.contains(forbidden),
            "adapter source must not reach for discovery/process/filesystem: {forbidden}"
        );
    }
}

#[test]
fn claude_adapter_rejects_multi_artifact_and_non_jsonl_framing() {
    let adapter = ClaudeAdapter;
    let reader = RawUnitReader::new(ReaderPolicy::default());

    let two = SourceHandle::new(
        AgentKind::Claude,
        "double",
        None,
        vec![
            SourceArtifact::memory("a.jsonl", b"{}\n".to_vec(), SourceFraming::JsonLines).unwrap(),
            SourceArtifact::memory("b.jsonl", b"{}\n".to_vec(), SourceFraming::JsonLines).unwrap(),
        ],
    )
    .unwrap();
    let read = reader.read(&two).unwrap();
    assert!(adapter.classify(&two, &read).is_err());

    let whole = SourceHandle::new(
        AgentKind::Claude,
        "whole",
        None,
        vec![
            SourceArtifact::memory("doc.json", b"{}".to_vec(), SourceFraming::WholeDocument)
                .unwrap(),
        ],
    )
    .unwrap();
    let read = reader.read(&whole).unwrap();
    assert!(adapter.classify(&whole, &read).is_err());
}
