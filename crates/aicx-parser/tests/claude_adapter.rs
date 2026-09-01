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

fn assistant_row_with_thinking_block(session: &str, block: &str) -> String {
    format!(
        concat!(
            r#"{{"type":"assistant","sessionId":"{session}","#,
            r#""timestamp":"2026-07-31T08:00:00.000Z","#,
            r#""message":{{"role":"assistant","model":"claude-opus-5","#,
            r#""content":[{block},{{"type":"text","text":"Gotowe."}}]}}}}"#,
            "\n"
        ),
        session = session,
        block = block,
    )
}

#[test]
fn claude_signature_only_thinking_block_is_consumed_silently() {
    // Since 2026-07 the harness writes thinking blocks signature-only: the
    // reasoning text never reaches the JSONL. Treating that as an unknown
    // payload made every reasoning session flag itself as carrying
    // unsupported visible events.
    let session = "66666666-6666-4666-8666-666666666666";
    let body = assistant_row_with_thinking_block(
        session,
        r#"{"type":"thinking","thinking":"","signature":"ErUBCkYIBxgCKkDd"}"#,
    );
    let model = session_model(session, &body);

    assert!(
        model.coverage.warnings.is_empty(),
        "a known block with no body is not an unknown payload, got {:?}",
        model.coverage.warnings
    );
    let status = status(&model);
    assert!(!status.boundary_flags.unsupported_visible_event);
    assert!(!status.visible_event_lost);
    assert_eq!(
        status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );

    // Still consumed as a thinking_block — accounting is untouched, only the
    // turn projection is skipped.
    assert!(
        model
            .coverage
            .consumed
            .iter()
            .any(|unit| unit.kind == "thinking_block"),
        "the block is consumed, never skipped"
    );
    assert_eq!(model.coverage.skipped_count, 0);
    assert_eq!(
        model
            .turns
            .iter()
            .filter(|turn| turn.kind == TurnKind::InternalThought)
            .count(),
        0,
        "an empty body yields no internal_thought turn"
    );
    assert_eq!(
        model
            .turns
            .iter()
            .filter(|turn| turn.kind == TurnKind::AgentReply)
            .count(),
        1,
        "the sibling text block is unaffected"
    );
}

#[test]
fn claude_thinking_block_with_a_body_still_becomes_an_internal_thought() {
    let session = "66666666-6666-4666-8666-666666666667";
    let body = assistant_row_with_thinking_block(
        session,
        r#"{"type":"thinking","thinking":"  Plan the cut.  ","signature":"sig-abc"}"#,
    );
    let model = session_model(session, &body);

    let thoughts: Vec<&str> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::InternalThought)
        .map(|turn| turn.text.as_str())
        .collect();
    assert_eq!(thoughts, vec!["Plan the cut."], "unchanged behavior");
    assert_eq!(model.turns[0].role, TurnRole::Assistant);
    assert!(model.coverage.warnings.is_empty());
    assert!(!status(&model).boundary_flags.unsupported_visible_event);
}

#[test]
fn claude_thinking_block_without_the_field_stays_an_unsupported_shape() {
    // The silent path is for a KNOWN block with no body. A block missing the
    // field entirely is still an unrecognized shape and must stay visible.
    let session = "66666666-6666-4666-8666-666666666668";
    let body =
        assistant_row_with_thinking_block(session, r#"{"type":"thinking","signature":"sig-abc"}"#);
    let model = session_model(session, &body);

    assert!(status(&model).boundary_flags.unsupported_visible_event);
    assert!(
        model
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.kind == WarningKind::UnknownPayloadType),
        "a missing field is not the same as an empty one"
    );
}

#[test]
fn claude_file_history_delta_is_metadata_not_an_unsupported_event() {
    // Rewind/backup bookkeeping the harness writes per message: a backup
    // descriptor, the message ids it belongs to, and a tracking path. No
    // conversation content, so it carries no visible event.
    let session = "55555555-5555-4555-8555-555555555555";
    let body = concat!(
        r#"{"type":"user","message":{"role":"user","content":"popraw ten plik"},"#,
        r#""sessionId":"55555555-5555-4555-8555-555555555555","#,
        r#""timestamp":"2026-07-27T23:08:40.000Z"}"#,
        "\n",
        r#"{"type":"file-history-delta","messageId":"605ff099","#,
        r#""snapshotMessageId":"9f44590f","trackingPath":"/repo/notes.md","#,
        r#""backup":{"backupFileName":"68ccd2c3@v1","version":1,"#,
        r#""backupTime":"2026-07-27T23:08:49.286Z","realParentDir":"/repo"},"#,
        r#""timestamp":"2026-07-27T23:08:49.286Z"}"#,
        "\n",
    );
    let model = session_model(session, body);

    let delta = &model.coverage.consumed[1];
    assert_eq!(
        delta.kind, "metadata_record",
        "file-history-delta is the per-message successor of file-history-snapshot"
    );
    assert_eq!(model.coverage.skipped_count, 0);

    // The point of the fix: a healthy modern session must not degrade its own
    // parse status with the harness's own bookkeeping.
    assert!(
        model.coverage.warnings.is_empty(),
        "recognized bookkeeping emits no warning, got {:?}",
        model.coverage.warnings
    );
    let status = status(&model);
    assert!(
        !status.boundary_flags.unsupported_visible_event,
        "bookkeeping carries no visible event to preserve as unsupported"
    );
    assert_eq!(
        status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );

    // It is metadata, never chat: the only turn is the operator's message.
    assert_eq!(user_turn_texts(&model), vec!["popraw ten plik"]);
}

// ---------------------------------------------------------------------------
// Mid-turn queue projection (`queue-operation` enqueue -> user turn)
// ---------------------------------------------------------------------------

const QUEUE_SESSION: &str = "44444444-4444-4444-8444-444444444444";

fn queue_row(operation: &str, field: &str, text: &str, timestamp: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "queue-operation",
            "operation": operation,
            field: text,
            "sessionId": QUEUE_SESSION,
            "timestamp": timestamp,
        })
    )
}

fn user_row(text: &str, timestamp: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": text},
            "sessionId": QUEUE_SESSION,
            "timestamp": timestamp,
        })
    )
}

fn user_turn_texts(model: &SessionModel) -> Vec<&str> {
    model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::UserMsg)
        .map(|turn| turn.text.as_str())
        .collect()
}

#[test]
fn claude_queue_enqueue_content_becomes_a_user_turn() {
    let body = queue_row(
        "enqueue",
        "content",
        "czy mozesz to sprawdzic w runtime?",
        "2026-07-30T10:24:23.406Z",
    );
    let model = session_model(QUEUE_SESSION, &body);

    assert_eq!(
        user_turn_texts(&model),
        vec!["czy mozesz to sprawdzic w runtime?"],
        "a message submitted mid-turn exists only as the enqueue record"
    );
    let turn = &model.turns[0];
    assert_eq!(turn.role, TurnRole::User);
    assert_eq!(
        turn.timestamp,
        Known::value("2026-07-30T10:24:23.406Z".to_owned()),
        "the projection keeps the record's own timestamp"
    );

    // The record stays consumed as metadata (frozen taxonomy is untouched) and
    // the turn references exactly that evidence.
    let consumed_kinds: Vec<&str> = model
        .coverage
        .consumed
        .iter()
        .map(|unit| unit.kind.as_str())
        .collect();
    assert_eq!(consumed_kinds, vec!["metadata_record"]);
    assert_eq!(turn.raw_unit_refs.len(), 1);
    assert_eq!(turn.raw_unit_refs[0].unit_kind, "metadata_record");
    assert_eq!(
        turn.raw_unit_refs[0].evidence_event_id,
        model.coverage.consumed[0].evidence.evidence_event_id
    );
}

#[test]
fn claude_queue_enqueue_falls_back_to_the_prompt_field() {
    let body = queue_row(
        "enqueue",
        "prompt",
        "starszy build trzyma tekst w .prompt",
        "2026-07-30T10:24:23.406Z",
    );
    let model = session_model(QUEUE_SESSION, &body);

    assert_eq!(
        user_turn_texts(&model),
        vec!["starszy build trzyma tekst w .prompt"]
    );
}

#[test]
fn claude_queue_enqueue_of_a_task_notification_is_not_operator_input() {
    let body = queue_row(
        "enqueue",
        "content",
        "<task-notification>\n<task-id>a715678b</task-id>\n</task-notification>",
        "2026-07-30T10:24:23.406Z",
    );
    let model = session_model(QUEUE_SESSION, &body);

    // W2-T8: the throne classifies the queued body as an injection
    // (`# claude` rule `task-notification` → TransportControl), so it lands
    // on the system lane with total accounting — never on the operator lane.
    assert!(
        user_turn_texts(&model).is_empty(),
        "harness notifications are machine chatter, not operator speech"
    );
    assert_eq!(
        model.turns.len(),
        1,
        "accounted as a system note, not dropped"
    );
    assert_eq!(model.turns[0].kind, TurnKind::SystemNote);
    assert_eq!(model.turns[0].role, TurnRole::System);
    assert_eq!(
        model.coverage.consumed_count, 1,
        "still consumed as metadata"
    );
}

#[test]
fn claude_queue_bookkeeping_operations_emit_no_turn() {
    for operation in ["remove", "dequeue", "popAll"] {
        let body = queue_row(
            operation,
            "content",
            "tekst juz raz zakolejkowany",
            "2026-07-30T10:24:23.406Z",
        );
        let model = session_model(QUEUE_SESSION, &body);

        assert!(
            model.turns.is_empty(),
            "{operation} repeats text that enqueue already carried"
        );
        assert_eq!(model.coverage.consumed_count, 1);
    }
}

#[test]
fn claude_re_enqueued_text_keeps_only_the_first_submission() {
    let body = format!(
        "{}{}{}",
        queue_row(
            "enqueue",
            "content",
            "ma to sens?",
            "2026-07-30T06:51:06.736Z"
        ),
        queue_row(
            "remove",
            "content",
            "ma to sens?",
            "2026-07-30T06:51:40.877Z"
        ),
        queue_row(
            "enqueue",
            "content",
            "ma to sens?",
            "2026-07-30T06:52:11.010Z"
        ),
    );
    let model = session_model(QUEUE_SESSION, &body);

    assert_eq!(
        user_turn_texts(&model),
        vec!["ma to sens?"],
        "an interrupt-and-resubmit is one message, dated at the first enqueue"
    );
    assert_eq!(
        model.turns[0].timestamp,
        Known::value("2026-07-30T06:51:06.736Z".to_owned())
    );
}

#[test]
fn claude_queued_text_delivered_after_the_turn_is_not_doubled() {
    let body = format!(
        "{}{}",
        queue_row(
            "enqueue",
            "content",
            "Dopiszmy jeszcze cos",
            "2026-07-30T06:55:06.868Z"
        ),
        user_row("Dopiszmy jeszcze cos", "2026-07-30T06:55:14.449Z"),
    );
    let model = session_model(QUEUE_SESSION, &body);

    assert_eq!(
        user_turn_texts(&model),
        vec!["Dopiszmy jeszcze cos"],
        "the real user row wins over the projection that anticipated it"
    );
    assert_eq!(
        model.turns[0].timestamp,
        Known::value("2026-07-30T06:55:14.449Z".to_owned()),
        "the surviving turn is the delivered one"
    );
    assert_eq!(
        model.turns[0].raw_unit_refs[0].unit_kind, "user",
        "the survivor is the real row, not the queue record"
    );
    // Dropping a projected turn must leave the model internally consistent.
    assert_eq!(model.turns[0].turn_idx, 0);
    assert_eq!(model.segments.len(), 1);
    assert_eq!(model.segments[0].turn_range.start, 0);
    assert_eq!(model.segments[0].turn_range.end, 0);
}

#[test]
fn claude_queued_text_delivered_before_the_enqueue_is_a_separate_message() {
    let body = format!(
        "{}{}",
        user_row("sprawdz to jeszcze raz", "2026-07-30T06:55:06.868Z"),
        queue_row(
            "enqueue",
            "content",
            "sprawdz to jeszcze raz",
            "2026-07-30T06:57:14.449Z"
        ),
    );
    let model = session_model(QUEUE_SESSION, &body);

    assert_eq!(
        user_turn_texts(&model).len(),
        2,
        "an earlier identical row cannot be the delivery of a later enqueue"
    );
}

// ---------------------------------------------------------------------------
// W2-T8 — throne contract (structural; not run under the W2 embargo)
// ---------------------------------------------------------------------------

#[test]
fn claude_queue_enqueue_is_sealed_with_the_enqueue_timestamp_on_the_fixture() {
    // Oracle: tests/fixtures/parser_engine/assertions.toml
    // (claude-67025fed-enqueue-count = 26, user-only-usermsg = 25,
    //  enqueue-seal = 2026-08-25T19:53:58.988Z, content presence).
    let body = fixture("human_shape_67025fed.jsonl");
    let model = session_model("67025fed-6f58-4077-8472-f41a099dd498", &body);

    let user_turns = user_turn_texts(&model);
    assert_eq!(
        user_turns.len(),
        25,
        "26 enqueue records, one empty body: 25 operator utterances"
    );
    assert!(
        user_turns[0].starts_with("Z mojej rozmowi z codexem"),
        "first enqueue body is projected as operator speech"
    );
    assert_eq!(
        model.turns[0].timestamp,
        Known::value("2026-08-25T19:53:58.988Z".to_owned()),
        "seal = transport enqueue timestamp, not render time"
    );
    assert!(
        model
            .turns
            .iter()
            .filter(|turn| turn.kind == TurnKind::UserMsg)
            .all(|turn| !turn.text.trim().is_empty()),
        "an empty enqueue is not an utterance"
    );
    // dequeue (6) and remove (20) are consumption bookkeeping: no lane.
    assert_eq!(model.turns.len(), 25, "only enqueue bodies become turns");
    assert_eq!(
        model.coverage.consumed_count, 52,
        "every record is accounted"
    );
}

#[test]
fn claude_system_reminder_block_in_the_user_lane_is_an_injection() {
    let body = format!(
        "{}\n",
        serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [
                    {"type": "text", "text": "<system-reminder>\nhook output\n</system-reminder>"},
                    {"type": "text", "text": "a teraz naprawdę: popraw ten plik"},
                ]
            },
            "sessionId": QUEUE_SESSION,
            "timestamp": "2026-07-30T10:24:23.406Z",
        })
    );
    let model = session_model(QUEUE_SESSION, &body);

    assert_eq!(
        user_turn_texts(&model),
        vec!["a teraz naprawdę: popraw ten plik"],
        "only the operator's own block is speech"
    );
    let notes: Vec<&str> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::SystemNote)
        .map(|turn| turn.text.as_str())
        .collect();
    assert_eq!(
        notes,
        vec!["<system-reminder>\nhook output\n</system-reminder>"],
        "the injection is retained in full on the system lane (Decision 6)"
    );
}

#[test]
fn claude_tag_mentioned_inside_prose_stays_operator_speech() {
    let body = user_row(
        "dlaczego <system-reminder> pojawia się w moim extractcie?",
        "2026-07-30T10:24:23.406Z",
    );
    let model = session_model(QUEUE_SESSION, &body);

    assert_eq!(
        user_turn_texts(&model),
        vec!["dlaczego <system-reminder> pojawia się w moim extractcie?"],
        "an injection is recognized only at the head of the payload"
    );
}

#[test]
fn claude_direct_user_text_is_human_direct_through_the_throne() {
    use aicx_parser::engine::{
        AgentKind, FrameClass, HumanChannel, TransportFrame, TransportKind, TransportPayload,
        TransportRole, classify,
    };
    let body = user_row("zwykła wiadomość", "2026-07-30T10:24:23.406Z");
    let model = session_model(QUEUE_SESSION, &body);
    let turn = &model.turns[0];
    assert_eq!(turn.kind, TurnKind::UserMsg);
    assert_eq!(turn.role, TurnRole::User);

    // The adapter's frame and the throne's verdict agree: same content hash,
    // same seal, `Human{Direct}` for a delivered row.
    let frame = TransportFrame {
        agent: AgentKind::Claude,
        transport_kind: TransportKind::DirectMessage,
        timestamp: turn.timestamp.clone(),
        payload: TransportPayload::Text {
            role: TransportRole::User,
            content: turn.text.clone(),
        },
        evidence: turn.raw_unit_refs[0].clone(),
    };
    let classified = classify(&frame);
    assert_eq!(
        classified.class,
        FrameClass::Human {
            channel: HumanChannel::Direct
        }
    );
    assert_eq!(classified.content_hash, turn.text_hash);
    assert_eq!(classified.seal.seal_ts, turn.timestamp);
    assert_eq!(classified.turn_kind, Some(TurnKind::UserMsg));
}
