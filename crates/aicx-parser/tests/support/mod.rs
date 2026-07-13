#![allow(dead_code)]

use aicx_parser::engine::*;

pub fn evidence(ordinal: u64, kind: &str, bytes: &[u8]) -> RawUnitRef {
    let locator = ordinal_locator(ordinal);
    let content_hash = sha256_hex(bytes);
    RawUnitRef {
        evidence_event_id: evidence_event_id(
            AgentKind::Codex,
            "session-test",
            &locator,
            kind,
            bytes,
        )
        .expect("test evidence id"),
        coverage_ordinal: ordinal,
        physical_ordinal: ordinal,
        locator,
        unit_kind: kind.to_owned(),
        artifact: "session.jsonl".to_owned(),
        content_hash,
        original_bytes: bytes.len() as u64,
    }
}

pub fn complete_coverage(evidence: &[RawUnitRef]) -> CoverageReport {
    CoverageReport::new(
        evidence.len() as u64,
        evidence
            .iter()
            .map(|evidence| ConsumedUnit {
                ordinal: evidence.coverage_ordinal,
                kind: evidence.unit_kind.clone(),
                evidence: evidence.clone(),
            })
            .collect(),
        Vec::new(),
        Vec::new(),
        ParseStatus {
            visible_completeness: VisibleCompleteness::CompleteVisible,
            boundary_flags: BoundaryFlags::default(),
            malformed_tail_present: false,
            visible_event_lost: false,
        },
    )
}

pub fn model_with_text(text: &str) -> SessionModel {
    let evidence = evidence(1, "message", text.as_bytes());
    let coverage = complete_coverage(std::slice::from_ref(&evidence));
    let provenance = Provenance {
        agent: AgentKind::Codex,
        model: Known::value("gpt-5".to_owned()),
        cli_version: Known::unknown(),
        cwd: Known::value("repo".to_owned()),
        branch: Known::value("main".to_owned()),
        started_at: Known::value("2026-07-13T00:00:00Z".to_owned()),
        ended_at: Known::unknown(),
        original_source_hash: sha256_hex(text.as_bytes()),
        original_source_bytes: text.len() as u64,
    };
    let mut model = SessionModel::new("session-test", provenance, coverage);
    model.segments.push(Segment {
        segment_id: 0,
        cwd: Known::value("repo".to_owned()),
        branch: Known::value("main".to_owned()),
        started_at: Known::value("2026-07-13T00:00:00Z".to_owned()),
        ended_at: Known::unknown(),
        turn_range: TurnRange { start: 0, end: 0 },
    });
    model.turns.push(Turn {
        turn_idx: 0,
        role: TurnRole::Assistant,
        timestamp: Known::value("2026-07-13T00:00:01Z".to_owned()),
        kind: TurnKind::AgentReply,
        text: text.to_owned(),
        text_hash: sha256_hex(text.as_bytes()),
        text_chars: text.chars().count() as u64,
        tool_name: Known::unknown(),
        segment_id: 0,
        raw_unit_refs: vec![evidence.clone()],
    });
    model.tool_events.push(ToolEvent {
        kind: ToolEventKind::Call,
        turn_idx: 0,
        tool_name: "shell".to_owned(),
        correlation_id: Known::value("call-1".to_owned()),
        payload_hash: sha256_hex(b"{}"),
        payload_bytes: 2,
        raw_unit_refs: vec![evidence.clone()],
    });
    model.usage_events.push(UsageEvent {
        provider: "openai".to_owned(),
        model: Known::value("gpt-5".to_owned()),
        tokens: TokenComponents {
            input: Known::value(10),
            output: Known::value(5),
            reasoning: Known::unknown(),
            cache_read: Known::unknown(),
            cache_creation: Known::unknown(),
        },
        cost: Known::unknown(),
        timestamp: Known::value("2026-07-13T00:00:01Z".to_owned()),
        span: Known::unknown(),
        counter_semantics: CounterSemantics::Cumulative,
        evidence,
    });
    model
}

pub fn validated_model(text: &str) -> ValidatedSession {
    match validate_parse(UnvalidatedParse::from_model(model_with_text(text)))
        .expect("valid synthetic model")
    {
        ValidatedParse::Session(session) => *session,
        ValidatedParse::Fatal(_) => panic!("synthetic model unexpectedly fatal"),
    }
}
