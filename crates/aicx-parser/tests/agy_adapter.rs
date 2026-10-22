use aicx_parser::engine::*;

const CLI: &[u8] = include_bytes!("../../../tests/fixtures/parser_engine/agy/cli.jsonl");
const IDE: &[u8] = include_bytes!("../../../tests/fixtures/parser_engine/agy/ide.jsonl");
const NEEDLE: &str = "Preserve the agy request about lexical search.";
const FIRST_TOKEN: &str = "agypipelineinitial91dbea";
const TOOL_TOKEN: &str = "agytoolonly893fed";
const DISPATCH: &str = "AGY_DISPATCH_NOT_INTENT";

fn source(bytes: &[u8], source_id: &str) -> SourceHandle {
    SourceHandle::new(
        AgentKind::Agy,
        source_id,
        Some(source_id.to_owned()),
        vec![
            SourceArtifact::memory("trajectory.jsonl", bytes.to_vec(), SourceFraming::JsonLines)
                .unwrap(),
        ],
    )
    .unwrap()
}

fn parse(bytes: &[u8], source_id: &str) -> ValidatedSession {
    match ParserEngine::default()
        .parse_registered(&source(bytes, source_id))
        .unwrap()
    {
        ValidatedParse::Session(session) => *session,
        ValidatedParse::Fatal(_) => panic!("fixture must retain a model"),
    }
}

#[test]
fn aliases_resolve_to_agy_and_leave_legacy_gemini() {
    assert_eq!(AgentKind::parse("agy"), Some(AgentKind::Agy));
    assert_eq!(AgentKind::parse("antigravity"), Some(AgentKind::Agy));
    assert_eq!(AgentKind::parse("antigravity-cli"), Some(AgentKind::Agy));
    assert_eq!(AgentKind::Agy.as_str(), "agy");
    assert_eq!(
        AgentKind::parse("gemini-antigravity"),
        Some(AgentKind::Gemini)
    );
}

#[test]
fn cli_fixture_is_deterministic_and_fail_closed() {
    let session = parse(CLI, "11111111-1111-4111-8111-111111111111");
    let again = parse(CLI, "11111111-1111-4111-8111-111111111111");
    assert_eq!(
        canonical_bytes(&session).unwrap(),
        canonical_bytes(&again).unwrap()
    );
    let model = session.model();
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::PartialVisible
    );
    assert!(model.coverage.check_totality().is_ok());
    assert!(model.coverage.status.visible_event_lost);
    assert!(
        model
            .coverage
            .status
            .boundary_flags
            .unsupported_visible_event
    );
    assert!(
        !model
            .coverage
            .status
            .boundary_flags
            .opaque_reasoning_present
    );
    assert!(model.usage_events.is_empty());

    let humans: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::UserMsg)
        .map(|turn| turn.text.clone())
        .collect();
    assert_eq!(humans.len(), 1);
    assert!(humans[0].contains(NEEDLE));
    assert!(humans[0].contains(FIRST_TOKEN));
    assert!(!humans[0].contains(DISPATCH));
    assert!(!humans[0].contains(TOOL_TOKEN));

    let notes: Vec<_> = model
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::SystemNote)
        .map(|turn| turn.text.clone())
        .collect();
    assert!(
        notes
            .iter()
            .any(|text| text.contains("You are running under Vibecrafted core runtime."))
    );
    assert!(notes.iter().any(|text| text.contains(DISPATCH)));

    let joined = model
        .turns
        .iter()
        .map(|turn| turn.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("The 20.1 planner line stays visible."));
    assert!(joined.contains("Null status still speaks."));
    assert!(joined.contains(TOOL_TOKEN));
    for forbidden in [
        "AGY_BOOKKEEPING_FIELD30_DO_NOT_PROJECT",
        "AGY_UNKNOWN_STEP_TEXT_DO_NOT_PROJECT",
        "AGY_BAD_FORMAT_DO_NOT_PROJECT",
    ] {
        assert!(!joined.contains(forbidden), "{forbidden} leaked into turns");
    }

    assert!(
        model
            .coverage
            .consumed
            .iter()
            .any(|unit| unit.evidence.unit_kind == "unknown_step:8")
    );
    assert!(
        model
            .coverage
            .consumed
            .iter()
            .any(|unit| unit.evidence.unit_kind == "bookkeeping:23")
    );
    assert!(
        model
            .coverage
            .consumed
            .iter()
            .any(|unit| unit.evidence.unit_kind == "bookkeeping:101")
    );
    assert!(model.coverage.skipped.iter().any(|unit| {
        unit.reason == SkippedReason::Unsupported && unit.evidence.unit_kind == "user_input"
    }));
    assert!(
        model
            .coverage
            .skipped
            .iter()
            .any(|unit| unit.reason == SkippedReason::Malformed)
    );
    assert!(
        model.coverage.warnings.iter().any(|warning| {
            warning.kind == WarningKind::UnknownPayloadType && warning.count > 0
        })
    );
    assert!(
        model
            .coverage
            .warnings
            .iter()
            .any(|warning| warning.kind == WarningKind::MalformedUnit)
    );
    match &model.conversation {
        ProviderConversationRef::Agy {
            session_id,
            trajectory_id,
            ..
        } => {
            assert_eq!(session_id, "11111111-1111-4111-8111-111111111111");
            assert_eq!(
                trajectory_id.as_deref(),
                Some("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
            );
        }
        other => panic!("agy conversation ref expected, got {other:?}"),
    }
    assert_eq!(model.session_id, "11111111-1111-4111-8111-111111111111");
    assert_eq!(
        model.provenance.cwd,
        Known::value("/tmp/agy-fixture/work".to_owned())
    );
}

#[test]
fn base64_partial_quantum_matches_the_materializer_alphabet() {
    let agy = aicx_parser::adapters::agy::encode_b64;
    let decode = aicx_parser::adapters::agy::decode_b64;
    assert_eq!(agy(b"a"), "YQ==");
    assert_eq!(agy(b"ab"), "YWI=");
    assert_eq!(agy(b"abc"), "YWJj");
    assert_eq!(agy(b"hello"), "aGVsbG8=");
    for sample in [b"a".as_slice(), b"ab", b"abc", b"hello", b"\xff\x00", &[]] {
        assert_eq!(decode(&agy(sample)).unwrap(), sample);
    }

    // The app re-encodes sqlite blobs. That encoding has to be the fixture
    // alphabet, including payloads whose length mod 3 is 1 or 2 (the human
    // step is one of those). Key order matches `render_stream`; the `note`
    // field is not part of the stream.
    let mut stream = Vec::new();
    for line in CLI.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_slice(line).unwrap();
        if row["record"] == "header" {
            let raw = decode(row["metadata_b64"].as_str().unwrap()).unwrap();
            let encoded = agy(&raw);
            assert_eq!(encoded, row["metadata_b64"].as_str().unwrap());
            stream.extend(
                format!("{{\"metadata_b64\":\"{encoded}\",\"record\":\"header\",\"v\":1}}\n")
                    .into_bytes(),
            );
            continue;
        }
        let raw = decode(row["payload_b64"].as_str().unwrap()).unwrap();
        let encoded = agy(&raw);
        assert_eq!(encoded, row["payload_b64"].as_str().unwrap());
        let status = if row["status"].is_null() {
            "null".to_owned()
        } else {
            row["status"].to_string()
        };
        let step_format = if row["step_format"].is_null() {
            "null".to_owned()
        } else {
            row["step_format"].to_string()
        };
        stream.extend(
            format!(
                "{{\"idx\":{},\"payload_b64\":\"{encoded}\",\"record\":\"step\",\"status\":{status},\"step_format\":{step_format},\"step_type\":{},\"v\":1}}\n",
                row["idx"], row["step_type"]
            )
            .into_bytes(),
        );
    }
    let session = parse(&stream, "11111111-1111-4111-8111-111111111111");
    let humans: Vec<_> = session
        .model()
        .turns
        .iter()
        .filter(|turn| turn.kind == TurnKind::UserMsg)
        .map(|turn| turn.text.clone())
        .collect();
    assert_eq!(humans.len(), 1);
    assert!(humans[0].contains(NEEDLE));
    assert!(humans[0].contains(FIRST_TOKEN));
    assert!(
        session
            .model()
            .turns
            .iter()
            .any(|turn| turn.kind == TurnKind::SystemNote && turn.text.contains(DISPATCH))
    );
}

#[test]
fn ide_fixture_is_complete() {
    let session = parse(IDE, "22222222-2222-4222-8222-222222222222");
    let model = session.model();
    assert_eq!(
        model.coverage.status.visible_completeness,
        VisibleCompleteness::CompleteVisible
    );
    assert!(model.coverage.check_totality().is_ok());
    assert!(model.coverage.skipped.is_empty());
    let joined = model
        .turns
        .iter()
        .map(|turn| turn.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("agypipelineideroot47fdae"));
}
