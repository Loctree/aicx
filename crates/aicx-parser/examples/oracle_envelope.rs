//! Materialize `parser_oracle.envelope.v1` from a fixture via the real AICX parser SUT.
//! Used by `tests/parser_oracle/compare.py --all` so the subject is AICX, not Transcript Builder.

use aicx_parser::adapters::registered_adapter;
use aicx_parser::engine::{
    AgentKind, ParserEngine, ReaderPolicy, SourceArtifact, SourceFraming, SourceHandle, TurnKind,
    TurnRole, ValidatedParse, VisibleCompleteness,
};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let mut agent = None;
    let mut fixture = None;
    let mut session_id = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--agent" => agent = args.next(),
            "--fixture" => fixture = args.next(),
            "--session-id" => session_id = args.next(),
            other => {
                eprintln!("unknown arg: {other}");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(agent), Some(fixture)) = (agent, fixture) else {
        eprintln!("usage: oracle_envelope --agent <name> --fixture <path> [--session-id <id>]");
        return ExitCode::from(2);
    };
    let session_id = session_id.unwrap_or_else(|| "oracle-envelope".to_owned());
    let fixture_path = PathBuf::from(&fixture);
    let body = match fs::read(&fixture_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read fixture: {e}");
            return ExitCode::from(1);
        }
    };
    let agent_kind = match agent.as_str() {
        "codex" => AgentKind::Codex,
        "claude" => AgentKind::Claude,
        "gemini" => AgentKind::Gemini,
        "grok" => AgentKind::Grok,
        "junie" => AgentKind::Junie,
        other => {
            eprintln!("unsupported agent: {other}");
            return ExitCode::from(2);
        }
    };
    let framing = if fixture_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"))
    {
        SourceFraming::JsonLines
    } else {
        SourceFraming::WholeDocument
    };
    let name = fixture_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("fixture")
        .to_owned();
    let source = match SourceHandle::new(
        agent_kind,
        &session_id,
        Some(session_id.clone()),
        vec![SourceArtifact::memory(&name, body, framing).expect("memory artifact")],
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("source: {e}");
            return ExitCode::from(1);
        }
    };
    let engine = ParserEngine::new(ReaderPolicy::default());
    let parse = match engine.parse(&source, registered_adapter(agent_kind)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("parse: {e}");
            return ExitCode::from(1);
        }
    };
    let envelope = match envelope_from(&parse) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    println!("{}", serde_json::to_string_pretty(&envelope).expect("json"));
    ExitCode::SUCCESS
}

fn envelope_from(parse: &ValidatedParse) -> Result<serde_json::Value, String> {
    let ValidatedParse::Session(session) = parse else {
        return Err("fatal parse has no envelope".into());
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
    if model
        .coverage
        .status
        .boundary_flags
        .compaction_boundary_present
    {
        boundaries.push("compaction_boundary_present");
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
    Ok(serde_json::json!({
        "schema": "parser_oracle.envelope.v1",
        "agent": model.provenance.agent.as_str(),
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
    }))
}
