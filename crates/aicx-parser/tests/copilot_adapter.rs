use aicx_parser::engine::*;
use serde_json::{Value, json};

const EVENTS: &[u8] = include_bytes!("../../../tests/fixtures/parser_engine/copilot/events.jsonl");
const WORKSPACE: &[u8] =
    include_bytes!("../../../tests/fixtures/parser_engine/copilot/workspace.yaml");

fn source(bytes: &[u8], yaml: Option<&[u8]>) -> SourceHandle {
    let mut artifacts = vec![
        SourceArtifact::memory("events.jsonl", bytes.to_vec(), SourceFraming::JsonLines).unwrap(),
    ];
    if let Some(yaml) = yaml {
        artifacts.push(
            SourceArtifact::memory(
                "workspace.yaml",
                yaml.to_vec(),
                SourceFraming::WholeDocument,
            )
            .unwrap(),
        );
    }
    SourceHandle::new(
        AgentKind::Copilot,
        "copilot-store",
        Some("copilot-store".to_owned()),
        artifacts,
    )
    .unwrap()
}

fn parse(bytes: &[u8], yaml: Option<&[u8]>) -> ValidatedSession {
    match ParserEngine::default()
        .parse_registered(&source(bytes, yaml))
        .unwrap()
    {
        ValidatedParse::Session(session) => *session,
        ValidatedParse::Fatal(_) => panic!("fixture must retain a valid conversation"),
    }
}

fn event(kind: &str, id: &str, data: Value) -> String {
    format!(
        "{}\n",
        json!({"type":kind,"id":id,"parentId":null,"timestamp":"2026-09-30T11:00:00Z","data":data})
    )
}

fn human() -> String {
    event(
        "user.message",
        "human",
        json!({"content":"Preserve my request.","messageId":"human"}),
    )
}

#[test]
fn native_fixture_covers_reasoning_tools_usage_resume_and_injected_context() {
    let parsed = parse(EVENTS, Some(WORKSPACE));
    let model = parsed.model();
    assert_eq!(model.provenance.agent, AgentKind::Copilot);
    assert_eq!(
        model.provenance.cwd,
        Known::value("/fixture/repo".to_owned())
    );
    assert_eq!(
        model.provenance.branch,
        Known::value("feature/copilot".to_owned())
    );
    assert_eq!(
        model.provenance.cli_version,
        Known::value("1.0.0".to_owned())
    );
    assert_eq!(model.conversation.node_id(), "copilot-fixture");
    let humans: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.role == TurnRole::User)
        .collect();
    assert_eq!(
        humans.len(),
        2,
        "repeated actual prompts survive; subagent input is not a Founder prompt"
    );
    assert_eq!(humans[0].text, humans[1].text);
    assert_ne!(
        humans[0].raw_unit_refs[0].evidence_event_id,
        humans[1].raw_unit_refs[0].evidence_event_id
    );
    assert!(
        model
            .turns
            .iter()
            .any(|turn| turn.kind == TurnKind::InternalThought)
    );
    assert!(
        model
            .turns
            .iter()
            .any(|turn| matches!(turn.frame_class, Some(FrameClass::InterAgent { .. })))
    );
    assert!(model.turns.iter().any(|turn| matches!(
        turn.frame_class,
        Some(FrameClass::Inject {
            kind: InjectKind::AgentInstructions
        })
    )));
    assert_eq!(
        model.tool_events.len(),
        2,
        "request/start mirrors produce one call and one result"
    );
    assert_eq!(
        model.tool_events[0].correlation_id,
        model.tool_events[1].correlation_id
    );
    assert_eq!(model.tool_events[0].raw_unit_refs.len(), 2);
    let call = &model.turns[model.tool_events[0].turn_idx as usize];
    assert!(
        matches!(&call.frame_class, Some(FrameClass::ShellAction { cmd, executor:ShellExecutor::Agent, .. }) if cmd == "cargo test -p aicx-parser")
    );
    let result = &model.turns[model.tool_events[1].turn_idx as usize];
    let payload: Value = serde_json::from_str(&result.text).unwrap();
    let Some(FrameClass::ShellAction {
        result: retained, ..
    }) = &call.frame_class
    else {
        panic!()
    };
    assert_eq!(
        retained.text, result.text,
        "full command projection carries its correlated completion"
    );
    assert_eq!(retained.hash, sha256_hex(retained.text.as_bytes()));
    assert_eq!(retained.chars, retained.text.chars().count() as u64);
    assert_eq!(payload.pointer("/shellExecution/exitCode"), Some(&json!(0)));
    assert_eq!(
        payload.pointer("/result/detailedContent"),
        Some(&json!("All fixtures matched."))
    );
    assert_eq!(model.usage_events.len(), 2);
    assert_eq!(
        model.usage_events[0].counter_semantics,
        CounterSemantics::Delta
    );
    assert_eq!(
        model.usage_events[1].counter_semantics,
        CounterSemantics::Cumulative
    );
    assert_eq!(model.usage_events[0].tokens.reasoning, Known::value(12));
    assert!(
        matches!(model.usage_events[0].cost, Known::Unknown(_)),
        "Copilot cost multipliers carry no fiat currency"
    );
    assert_eq!(model.context_epochs.len(), 1);
    assert!(model.context_epochs[0].first_turn_after.is_some());
    assert!(
        !model.turns.iter().any(|turn| turn
            .text
            .contains("The Founder requested the Copilot oracle")),
        "compaction is history, never new speech"
    );
    assert_eq!(model.skill_invocations[0].skill_name, "vc-workflow");
    assert_eq!(model.scope_status(), ScopeStatus::MixedCandidate);
    assert_eq!(model.coverage.raw_line_count, 18);
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
    assert!(model.coverage.check_totality().is_ok());
}

#[test]
fn metadata_fallback_is_order_independent_and_never_reads_siblings() {
    let prompt = human();
    let parsed = parse(prompt.as_bytes(), Some(WORKSPACE));
    assert_eq!(
        parsed.model().provenance.cwd,
        Known::value("/fixture/stale-cwd".to_owned())
    );
    assert_eq!(
        parsed.model().conversation.node_id(),
        "copilot-workspace-fallback"
    );
    let mut reversed = source(prompt.as_bytes(), Some(WORKSPACE))
        .artifacts()
        .to_vec();
    reversed.reverse();
    let reversed = SourceHandle::new(
        AgentKind::Copilot,
        "copilot-store",
        Some("copilot-store".to_owned()),
        reversed,
    )
    .unwrap();
    let ValidatedParse::Session(reverse) =
        ParserEngine::default().parse_registered(&reversed).unwrap()
    else {
        panic!()
    };
    assert_eq!(parsed.model().turns[0].text, reverse.model().turns[0].text);
    assert_eq!(
        parsed.model().turns[0].raw_unit_refs[0].evidence_event_id,
        reverse.model().turns[0].raw_unit_refs[0].evidence_event_id
    );
    let no_yaml = parse(prompt.as_bytes(), None);
    assert!(matches!(no_yaml.model().provenance.cwd, Known::Unknown(_)));
}

#[test]
fn malformed_metadata_does_not_erase_events_or_fabricate_scope() {
    let parsed = parse(human().as_bytes(), Some(b"cwd: [unterminated"));
    assert!(matches!(parsed.model().provenance.cwd, Known::Unknown(_)));
    assert!(
        parsed
            .model()
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.reason == SkippedReason::Malformed && !unit.visible)
    );
    assert_eq!(parsed.model().turns.len(), 1);
}

#[test]
fn initial_context_preserves_fallback_per_absent_field() {
    let start = |context: Value| {
        event(
            "session.start",
            "start",
            json!({"sessionId":"partial-context", "context":context}),
        )
    };
    let only_branch = start(json!({"branch":"event-branch"})) + &human();
    let parsed = parse(only_branch.as_bytes(), Some(WORKSPACE));
    assert_eq!(
        parsed.model().provenance.cwd,
        Known::value("/fixture/stale-cwd".to_owned())
    );
    assert_eq!(
        parsed.model().provenance.branch,
        Known::value("event-branch".to_owned())
    );
    assert_eq!(
        parsed.model().segments[0].cwd,
        parsed.model().provenance.cwd
    );
    let only_cwd = start(json!({"cwd":"/fixture/event-cwd"})) + &human();
    let parsed = parse(only_cwd.as_bytes(), Some(WORKSPACE));
    assert_eq!(
        parsed.model().provenance.cwd,
        Known::value("/fixture/event-cwd".to_owned())
    );
    assert_eq!(
        parsed.model().provenance.branch,
        Known::value("stale-branch".to_owned())
    );
    let explicit_unknown = start(json!({"cwd":null,"branch":"event-branch"})) + &human();
    let parsed = parse(explicit_unknown.as_bytes(), Some(WORKSPACE));
    assert!(matches!(parsed.model().provenance.cwd, Known::Unknown(_)));
}

#[test]
fn unknown_malformed_oversized_and_incomplete_lines_have_honest_coverage() {
    for tail in [
        event(
            "future.visible",
            "future",
            json!({"content":"not yet supported"}),
        ),
        "{broken}\n".to_owned(),
        "{\"type\":\"user.message\"".to_owned(),
    ] {
        let bytes = human() + &tail;
        let parsed = parse(bytes.as_bytes(), None);
        let coverage = &parsed.model().coverage;
        assert_eq!(
            coverage.status.visible_completeness,
            VisibleCompleteness::PartialVisible
        );
        assert!(coverage.status.visible_event_lost);
        assert_eq!(coverage.raw_line_count, 2);
        assert!(coverage.check_totality().is_ok());
    }
    let valid_tail = human().trim_end().to_owned();
    let parsed = parse(valid_tail.as_bytes(), None);
    assert_eq!(
        parsed.model().coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    assert!(
        parsed
            .model()
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.kind == WarningKind::UnterminatedTail)
    );
    let large = human()
        + &event(
            "tool.execution_start",
            "big",
            json!({"toolName":"bash","toolCallId":"large","arguments":{"command":"x".repeat(1000)}}),
        );
    let policy = ReaderPolicy {
        max_unit_bytes: 300,
        ..ReaderPolicy::default()
    };
    let ValidatedParse::Session(parsed) = ParserEngine::new(policy)
        .parse_registered(&source(large.as_bytes(), None))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        parsed.model().segments[0].scope_status,
        ScopeStatus::Unattributed
    );
    assert!(
        parsed
            .model()
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.reason == SkippedReason::Oversized && unit.visible)
    );
}

#[test]
fn append_and_repeated_prompts_preserve_evidence_identity() {
    let first = parse(EVENTS, Some(WORKSPACE));
    let bytes = String::from_utf8(EVENTS.to_vec()).unwrap()
        + &event(
            "user.message",
            "last-human",
            json!({"content":"Build the Copilot oracle.","messageId":"human-3"}),
        );
    let second = parse(bytes.as_bytes(), Some(WORKSPACE));
    assert_eq!(
        &second.model().turns[..first.model().turns.len()],
        first.model().turns.as_slice()
    );
    assert_ne!(
        first.model().snapshot.content_hash,
        second.model().snapshot.content_hash
    );
    let a = canonical_fingerprint(&first).unwrap();
    for _ in 0..10 {
        assert_eq!(
            a,
            canonical_fingerprint(&parse(EVENTS, Some(WORKSPACE))).unwrap()
        );
    }
}

#[test]
fn streaming_mirrors_require_complete_evidence_and_dont_duplicate_final_text() {
    let prefix = human()
        + &event(
            "assistant.message_delta",
            "delta",
            json!({"messageId":"m1","deltaContent":"Hi"}),
        );
    let orphan = parse(prefix.as_bytes(), None);
    assert_eq!(
        orphan.model().coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
    let complete = prefix
        + &event(
            "assistant.message",
            "full",
            json!({"messageId":"m1","content":"Hi there."}),
        );
    let parsed = parse(complete.as_bytes(), None);
    assert_eq!(
        parsed.model().coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    assert_eq!(
        parsed
            .model()
            .turns
            .iter()
            .filter(|turn| turn.role == TurnRole::Assistant)
            .count(),
        1
    );
    assert!(
        parsed
            .model()
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.reason == SkippedReason::DuplicateBody)
    );
}

#[test]
fn an_empty_assistant_completion_does_not_cover_nonempty_streamed_text() {
    for content in ["", "   "] {
        let bytes = human()
            + &event(
                "assistant.message_delta",
                "delta",
                json!({"messageId":"empty1","deltaContent":"Visible response fragment"}),
            )
            + &event(
                "assistant.message",
                "empty",
                json!({"messageId":"empty1","content":content}),
            );
        let parsed = parse(bytes.as_bytes(), None);
        assert_eq!(
            parsed.model().coverage.status.visible_completeness,
            VisibleCompleteness::PartialVisible
        );
        assert!(parsed.model().coverage.status.visible_event_lost);
        assert!(
            parsed
                .model()
                .coverage
                .skipped
                .iter()
                .any(|unit| unit.visible && unit.evidence.unit_kind == "assistant.message_delta")
        );
    }
}

#[test]
fn user_tools_errors_transformed_context_and_detached_lineage_keep_authority() {
    let bytes = event(
        "session.start",
        "start",
        json!({"sessionId":"native","detachedFromSpawningParentSessionId":"parent","context":{"cwd":"/fixture/repo"}}),
    ) + &event(
        "user.message",
        "u1",
        json!({"content":"Review the patch.","transformedContent":"Review the patch.\nInjected safety checklist.","attachments":[{"type":"file","path":"/fixture/file.rs"}]}),
    ) + &event(
        "tool.user_requested",
        "user-tool",
        json!({"toolName":"bash","toolCallId":"ut","arguments":{"command":"pwd"}}),
    ) + &event(
        "tool.execution_complete",
        "failed",
        json!({"toolCallId":"ut","success":false,"shellExecution":{"exitCode":1},"error":{"code":"ERR_FIXTURE","message":"Fixture failed"}}),
    );
    let parsed = parse(bytes.as_bytes(), None);
    assert_eq!(
        parsed.model().conversation.declared_parents(),
        vec![("detached_from_spawning_parent_session_id", "parent")]
    );
    let humans: Vec<_> = parsed
        .model()
        .turns
        .iter()
        .filter(|turn| turn.role == TurnRole::User)
        .collect();
    assert_eq!(humans.len(), 1);
    assert_eq!(humans[0].text, "Review the patch.");
    assert!(parsed.model().turns.iter().any(|turn| matches!(
        turn.frame_class,
        Some(FrameClass::ShellAction {
            executor: ShellExecutor::Human,
            ..
        })
    )));
    assert!(
        parsed
            .model()
            .turns
            .iter()
            .any(|turn| turn.kind == TurnKind::ToolResult && turn.text.contains("ERR_FIXTURE"))
    );
}

#[test]
fn aliases_registry_and_metadata_only_sessions_do_not_invent_turns() {
    for alias in [
        "copilot",
        "copilot-cli",
        "github-copilot",
        "github-copilot-cli",
    ] {
        assert_eq!(AgentKind::parse(alias), Some(AgentKind::Copilot));
    }
    let result = ParserEngine::default().parse_registered(&source(
        event("session.start", "start", json!({"sessionId":"empty"})).as_bytes(),
        None,
    ));
    let ValidatedParse::Session(parsed) = result.unwrap() else {
        panic!()
    };
    assert!(parsed.model().turns.is_empty());
}

#[test]
fn telemetry_parent_task_id_does_not_demote_a_real_user() {
    let bytes = event(
        "user.message",
        "real-human",
        json!({"content":"Ship the parser.", "parentAgentTaskId":"background-telemetry"}),
    ) + &event(
        "user.message",
        "skill-inject",
        json!({"content":"Hidden skill prompt", "source":"skill-pdf"}),
    );
    let parsed = parse(bytes.as_bytes(), None);
    assert_eq!(
        parsed
            .model()
            .turns
            .iter()
            .filter(|turn| turn.role == TurnRole::User)
            .count(),
        1
    );
    assert_eq!(parsed.model().turns[0].text, "Ship the parser.");
    assert!(matches!(
        parsed.model().turns[1].frame_class,
        Some(FrameClass::Inject { .. })
    ));
}

#[test]
fn a_command_field_on_a_non_shell_tool_is_not_execution_evidence() {
    let bytes = human()
        + &event(
            "tool.execution_start",
            "custom-tool",
            json!({"toolName":"custom", "toolCallId":"custom1", "arguments":{"command":"cargo test"}}),
        );
    let parsed = parse(bytes.as_bytes(), None);
    let call = &parsed.model().turns[parsed.model().tool_events[0].turn_idx as usize];
    assert_eq!(call.kind, TurnKind::ToolCall);
    assert!(call.frame_class.is_none());
}

#[test]
fn a_tool_start_is_not_completion_of_streamed_output() {
    let bytes = human()
        + &event(
            "tool.execution_start",
            "start",
            json!({"toolName":"bash","toolCallId":"stream1","arguments":{"command":"cargo test"}}),
        )
        + &event(
            "tool.execution_partial_result",
            "partial",
            json!({"toolCallId":"stream1","partialOutput":"Tests are running."}),
        );
    let orphan = parse(bytes.as_bytes(), None);
    let call = &orphan.model().turns[orphan.model().tool_events[0].turn_idx as usize];
    assert!(
        matches!(&call.frame_class, Some(FrameClass::ShellAction { result, .. }) if result.text.is_empty())
    );
    assert_eq!(
        orphan.model().coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
    assert!(
        orphan
            .model()
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.visible && unit.reason == SkippedReason::UnknownPayloadType)
    );
    let complete = bytes
        + &event(
            "tool.execution_complete",
            "complete",
            json!({"toolCallId":"stream1","success":true,"result":{"content":"Tests are running. All passed."}}),
        );
    let completed = parse(complete.as_bytes(), None);
    let call = &completed.model().turns[completed.model().tool_events[0].turn_idx as usize];
    assert!(
        matches!(&call.frame_class, Some(FrameClass::ShellAction { result, .. }) if result.text.contains("All passed."))
    );
    assert_eq!(
        completed.model().coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    assert!(
        completed
            .model()
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.reason == SkippedReason::DuplicateBody)
    );
}

#[test]
fn context_drift_and_explicit_tool_workdirs_use_repository_identity() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let scratch = std::env::temp_dir().join(format!("aicx-copilot-scope-{unique}"));
    let baseline = scratch.join("first");
    let foreign = scratch.join("second");
    for repo in [&baseline, &foreign] {
        std::fs::create_dir_all(repo.join(".git")).unwrap();
    }
    let baseline = baseline.to_str().unwrap();
    let foreign = foreign.to_str().unwrap();
    let start = event(
        "session.start",
        "start",
        json!({"sessionId":"scope", "context":{"cwd":baseline,"branch":"main"}}),
    );
    let call = |id: &str, cwd: &str| {
        event(
            "tool.execution_start",
            id,
            json!({"toolName":"bash","toolCallId":id,"arguments":{"command":"pwd", "cwd":cwd}}),
        )
    };
    let bytes = start + &human() + &call("first-call", baseline) + &call("second-call", foreign);
    let parsed = parse(bytes.as_bytes(), None);
    assert!(parsed.model().segments[0].scope_conflict);
    assert_eq!(parsed.model().segments[0].scope_workdirs.len(), 2);
    let drift = bytes
        + &event(
            "session.context_changed",
            "change",
            json!({"cwd":foreign,"branch":"feature"}),
        )
        + &event(
            "user.message",
            "human2",
            json!({"content":"Work in the second repository."}),
        );
    let parsed = parse(drift.as_bytes(), None);
    assert_eq!(parsed.model().segments.len(), 2);
    assert_eq!(
        parsed.model().segments[1].cwd,
        Known::value(foreign.to_owned())
    );
    assert_eq!(parsed.model().scope_status(), ScopeStatus::MixedCandidate);
    std::fs::remove_dir_all(scratch).unwrap();
}
