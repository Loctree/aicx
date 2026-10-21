use aicx_parser::engine::*;

const SESSION: &[u8] = include_bytes!("../../../tests/fixtures/parser_engine/omp/session.jsonl");
const PIN: &str = "OMP_FIXTURE_PIN_HASH";

fn source(bytes: &[u8]) -> SourceHandle {
    SourceHandle::new(
        AgentKind::Omp,
        "01999999-aaaa-7666-8666-111111111111",
        Some("01999999-aaaa-7666-8666-111111111111".to_owned()),
        vec![
            SourceArtifact::memory("session.jsonl", bytes.to_vec(), SourceFraming::JsonLines)
                .unwrap(),
        ],
    )
    .unwrap()
}

fn parse(bytes: &[u8]) -> ValidatedSession {
    match ParserEngine::default()
        .parse_registered(&source(bytes))
        .unwrap()
    {
        ValidatedParse::Session(session) => *session,
        ValidatedParse::Fatal(_) => panic!("fixture must retain a valid conversation"),
    }
}

#[test]
fn aliases_resolve_to_omp() {
    assert_eq!(AgentKind::parse("omp"), Some(AgentKind::Omp));
    assert_eq!(AgentKind::parse("pi"), Some(AgentKind::Omp));
    assert_eq!(AgentKind::parse("oh-my-pi"), Some(AgentKind::Omp));
    assert_eq!(AgentKind::Omp.as_str(), "omp");
}

#[test]
fn fixture_keeps_the_active_branch_and_covers_every_record() {
    let session = parse(SESSION);
    let model = session.model();
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    assert!(model.coverage.check_totality().is_ok());
    assert!(model.coverage.skipped.is_empty());

    let humans: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::UserMsg)
        .map(|turn| turn.text.as_str())
        .collect();
    assert_eq!(humans, ["Preserve the omp request about lexical search."]);

    let joined = model
        .turns
        .iter()
        .map(|turn| turn.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("I will add omp support."));
    assert!(joined.contains("Check the fixture."));
    assert!(joined.contains("Continue the fixture from the agent lane."));
    assert!(joined.contains("You are running under Vibecrafted core runtime."));
    assert!(joined.contains("Auto-continue the fixture."));
    assert!(joined.contains("Injected context card."));
    assert!(joined.contains("Harness note for the fixture."));
    assert!(joined.contains("Use the omp adapter skill."));
    assert!(joined.contains("Compacted the fixture branch."));
    assert!(joined.contains("cargo fmt --check"));
    assert!(joined.contains("\"exitCode\":1"));
    assert!(!joined.contains("This abandoned branch is not the live transcript."));
    assert!(!joined.contains(PIN));
    assert!(
        !canonical_bytes(&session)
            .unwrap()
            .windows(PIN.len())
            .any(|window| window == PIN.as_bytes())
    );

    assert!(
        model
            .coverage
            .consumed
            .iter()
            .any(|unit| unit.kind == "off_branch:message")
    );
    for kind in [
        "title",
        "session",
        "message:user",
        "message:assistant",
        "message:toolResult",
        "message:bashExecution",
        "message:developer",
        "message:custom",
        "custom:tool_execution_start",
        "custom_message:fleet-loct-context-card",
        "model_change",
        "model_usage",
        "credential_pin",
        "session_init",
        "compaction",
        "branch_summary",
        "title_change",
        "thinking_level_change",
        "mode_change",
        "label",
        "service_tier_change",
        "reset_boundary",
        "ttsr_injection",
    ] {
        assert!(
            model.coverage.consumed.iter().any(|unit| unit.kind == kind),
            "missing coverage kind {kind}"
        );
    }

    let bash_calls = model
        .tool_events
        .iter()
        .filter(|event| event.kind == ToolEventKind::Call && event.tool_name == "bash")
        .count();
    assert_eq!(bash_calls, 2, "tool_execution_start must not double-count");
    assert_eq!(
        model.provenance.model,
        Known::value("fixture-model".to_owned())
    );
    assert_eq!(
        model.provenance.cwd,
        Known::value("/fixture/omp-repo".to_owned())
    );
    match &model.conversation {
        ProviderConversationRef::Omp {
            session_id,
            parent_session_id,
            unobserved,
        } => {
            assert_eq!(session_id, "01999999-aaaa-7666-8666-111111111111");
            assert_eq!(
                parent_session_id.as_deref(),
                Some("01999999-aaaa-7666-8666-000000000000")
            );
            assert!(unobserved.is_empty());
        }
        other => panic!("unexpected conversation ref {other:?}"),
    }
    assert!(model.context_epochs.len() >= 2);
    assert_eq!(model.usage_events.len(), 2);
    assert!(
        model.segments.iter().any(|segment| segment
            .scope_workdirs
            .iter()
            .any(|dir| dir.contains("omp-extra"))),
        "additionalDirectories stay on the segment even when the roots are not on disk"
    );
    assert!(matches!(
        model.scope_status(),
        ScopeStatus::Unattributed | ScopeStatus::MixedCandidate
    ));
    assert!(
        model
            .turns
            .iter()
            .filter(|turn| turn.kind == TurnKind::UserMsg)
            .count()
            == 1
    );
}

#[test]
fn unknown_records_stay_visible_and_do_not_become_speech() {
    let mut bytes = SESSION.to_vec();
    bytes.extend_from_slice(
        br#"{"type":"future_visible_event","id":"future","parentId":"tt1","payload":{"shape":"unknown"}}"#,
    );
    bytes.push(b'\n');
    let session = parse(&bytes);
    let model = session.model();
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
    assert!(
        model
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.reason == SkippedReason::UnknownPayloadType && unit.visible)
    );
    assert!(!model.turns.iter().any(|turn| turn.text.contains("unknown")));
    assert!(model.coverage.check_totality().is_ok());
}

#[test]
fn repeat_parse_is_deterministic() {
    let first = canonical_bytes(&parse(SESSION)).unwrap();
    let second = canonical_bytes(&parse(SESSION)).unwrap();
    assert_eq!(first, second);
}
