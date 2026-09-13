//! Kimi adapter contract suite: the registered adapter over real-shape
//! `wire.jsonl` fixtures, driven through the shared adapter boundary.

use aicx_parser::adapters::registered_adapter;
use aicx_parser::engine::{
    AgentKind, ParserEngine, RawUnitReader, ReaderPolicy, SkippedReason, SourceArtifact,
    SourceFraming, SourceHandle, ToolEventKind, TurnKind, TurnRole, ValidatedParse,
    VisibleCompleteness,
};

fn source(session_id: &str, body: &[u8]) -> SourceHandle {
    SourceHandle::new(
        AgentKind::Kimi,
        session_id,
        Some(session_id.to_owned()),
        vec![
            SourceArtifact::memory("wire.jsonl", body.to_vec(), SourceFraming::JsonLines)
                .expect("memory source"),
        ],
    )
    .expect("explicit source handle")
}

fn parse(session_id: &str, body: &[u8]) -> ValidatedParse {
    ParserEngine::new(ReaderPolicy::default())
        .parse_registered(&source(session_id, body))
        .expect("kimi parse must close")
}

#[test]
fn kimi_is_registered_with_identity_and_version() {
    let adapter = registered_adapter(AgentKind::Kimi);
    assert_eq!(adapter.agent(), AgentKind::Kimi);
    assert_eq!(adapter.adapter_version(), "kimi-wire-v1");
}

#[test]
fn kimi_minimal_fixture_projects_operator_assistant_and_tool_lanes() {
    let body = include_bytes!("../../../tests/fixtures/parser_engine/kimi/minimal.jsonl");
    let ValidatedParse::Session(session) = parse("afee4590-3e31-42a6-b3d4-f2341ddf0726", body)
    else {
        panic!("session")
    };
    let model = session.model();
    assert_eq!(model.provenance.agent, AgentKind::Kimi);
    let roles: Vec<_> = model.turns.iter().map(|turn| turn.role).collect();
    assert_eq!(
        roles,
        [
            TurnRole::User,
            TurnRole::Assistant,
            TurnRole::Assistant,
            TurnRole::Tool,
            TurnRole::Tool
        ]
    );
    assert_eq!(model.turns[1].kind, TurnKind::InternalThought);
    assert_eq!(model.turns[2].kind, TurnKind::AgentReply);
    assert_eq!(model.tool_events[0].kind, ToolEventKind::Call);
    assert_eq!(model.tool_events[1].kind, ToolEventKind::Result);
    assert_eq!(
        model.tool_events[0].correlation_id, model.tool_events[1].correlation_id,
        "tool.call and tool.result correlate on toolCallId"
    );
    assert!(
        model
            .coverage
            .status
            .boundary_flags
            .compaction_boundary_present
    );
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    // Epoch-millis wire stamps normalize onto RFC 3339.
    assert!(matches!(
        &model.turns[0].timestamp,
        aicx_parser::engine::Known::Value(stamp) if stamp.contains('T')
    ));
}

#[test]
fn kimi_tool_result_spilled_to_output_path_names_the_spill() {
    let body = br#"{"type":"context.append_loop_event","agentId":"main","event":{"type":"tool.call","uuid":"u1","turnId":"0","step":1,"toolCallId":"tool_9","name":"Read","args":{"path":"/tmp/big.rs"}},"time":1789296071200}
{"type":"context.append_loop_event","agentId":"main","event":{"type":"tool.result","parentUuid":"u1","toolCallId":"tool_9","result":{"output_path":"/tmp/.kimi-spill/out-1.txt"}},"time":1789296071250}
"#;
    let source = source("kimi-spill", body);
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .expect("bounded read");
    let adapter = registered_adapter(AgentKind::Kimi);
    let classified = adapter.classify(&source, &read).expect("classify");
    let model = adapter
        .assemble(&source, &read, classified)
        .expect("assemble")
        .model
        .expect("session model");
    let result_turn = model
        .turns
        .iter()
        .find(|turn| turn.kind == TurnKind::ToolResult)
        .expect("tool result turn");
    assert_eq!(
        result_turn.text,
        "[tool output stored at /tmp/.kimi-spill/out-1.txt]"
    );
    assert!(matches!(
        &result_turn.tool_name,
        aicx_parser::engine::Known::Value(name) if name == "Read"
    ));
}

#[test]
fn kimi_bookkeeping_never_counts_as_visible_gap() {
    let body = br#"{"type":"metadata","protocol_version":"1.5","created_at":1789296071162}
{"type":"runtime.set_binding","workspaceId":"wd_repo_deadbeef","runtimeId":"local","agentId":"main","time":1789296071163}
{"type":"llm.request","agentId":"main","kind":"loop","model":"k3-256k","time":1789296071164}
{"type":"usage.record","agentId":"main","usage":{"inputOther":1,"output":1},"usageScope":"turn","time":1789296071165}
{"type":"token_counting.measured","agentId":"main","length":3,"tokens":28606,"time":1789296071166}
{"type":"turn.prompt","agentId":"main","input":[{"type":"text","text":"pomoc"}],"time":1789296071167}
{"type":"context.append_message","agentId":"main","message":{"role":"user","content":[{"type":"text","text":"pomoc"}]},"time":1789296071168}
"#;
    let ValidatedParse::Session(session) = parse("kimi-bookkeeping", body) else {
        panic!("session")
    };
    let model = session.model();
    // Only the append_message record is conversation; the turn.prompt twin
    // and the telemetry envelopes are deliberate non-visible skips.
    assert_eq!(model.turns.len(), 1);
    assert_eq!(model.turns[0].role, TurnRole::User);
    assert_eq!(model.coverage.consumed_count, 1);
    assert!(
        model
            .coverage
            .skipped
            .iter()
            .all(|unit| unit.reason == SkippedReason::Unsupported && !unit.visible)
    );
    assert!(
        !model
            .coverage
            .status
            .boundary_flags
            .unsupported_visible_event
    );
}
