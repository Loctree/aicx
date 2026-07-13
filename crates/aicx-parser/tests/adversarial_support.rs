#![allow(dead_code)]

use aicx_parser::engine::{
    AgentKind, CoverageReport, ParserEngine, RawUnitReader, ReaderPolicy, SessionModel,
    SourceArtifact, SourceFraming, SourceHandle, ValidatedParse, ValidatedSession, canonical_bytes,
};
use std::collections::{BTreeMap, BTreeSet};

pub const SECRET_SENTINEL: &str = "AICX_SECRET_SENTINEL_C5A_DO_NOT_PROJECT";

#[derive(Clone, Copy, Debug)]
pub struct AgentCase {
    pub agent: AgentKind,
    pub artifact: &'static str,
    pub source_id: &'static str,
    pub base: &'static [u8],
    pub mutation_needle: &'static str,
}

pub fn cases() -> [AgentCase; 5] {
    [
        AgentCase {
            agent: AgentKind::Codex,
            artifact: "rollout.jsonl",
            source_id: "adversarial-codex",
            base: include_bytes!("../../../tests/fixtures/parser_engine/codex/minimal.jsonl"),
            mutation_needle: "Build the parser oracle harness.",
        },
        AgentCase {
            agent: AgentKind::Claude,
            artifact: "session.jsonl",
            source_id: "adversarial-claude",
            base: include_bytes!("../../../tests/fixtures/parser_engine/claude/minimal.jsonl"),
            mutation_needle: "Freeze the oracle contract.",
        },
        AgentCase {
            agent: AgentKind::Gemini,
            artifact: "session.jsonl",
            source_id: "adversarial-gemini",
            base: include_bytes!("../../../tests/fixtures/parser_engine/gemini/minimal.json"),
            mutation_needle: "Build the Gemini oracle.",
        },
        AgentCase {
            agent: AgentKind::Grok,
            artifact: "chat_history.jsonl",
            source_id: "adversarial-grok",
            base: include_bytes!(
                "../../../tests/fixtures/parser_engine/grok/session/chat_history.jsonl"
            ),
            mutation_needle: "Build the Grok oracle.",
        },
        AgentCase {
            agent: AgentKind::Junie,
            artifact: "events.jsonl",
            source_id: "adversarial-junie",
            base: include_bytes!(
                "../../../tests/fixtures/parser_engine/junie/session-20260713/events.jsonl"
            ),
            mutation_needle: "Build the Junie oracle.",
        },
    ]
}

pub fn source(case: AgentCase, bytes: Vec<u8>) -> SourceHandle {
    SourceHandle::new(
        case.agent,
        case.source_id,
        Some(case.source_id.to_owned()),
        vec![
            SourceArtifact::memory(case.artifact, bytes, SourceFraming::JsonLines)
                .expect("memory artifact"),
        ],
    )
    .expect("finite source handle")
}

pub fn parse(case: AgentCase, bytes: Vec<u8>, policy: ReaderPolicy) -> ValidatedParse {
    try_parse(case, bytes, policy).unwrap_or_else(|error| {
        panic!(
            "{} mutation must close explicitly: {error}",
            case.agent.as_str()
        )
    })
}

pub fn try_parse(
    case: AgentCase,
    bytes: Vec<u8>,
    policy: ReaderPolicy,
) -> Result<ValidatedParse, String> {
    ParserEngine::new(policy)
        .parse_registered(&source(case, bytes))
        .map_err(|error| error.to_string())
}

pub fn assembled_model(case: AgentCase, bytes: Vec<u8>) -> Result<Option<SessionModel>, String> {
    let source = source(case, bytes);
    let read = RawUnitReader::new(ReaderPolicy::default())
        .read(&source)
        .map_err(|error| error.to_string())?;
    let adapter = aicx_parser::adapters::registered_adapter(case.agent);
    let classified = adapter
        .classify(&source, &read)
        .map_err(|error| error.to_string())?;
    adapter
        .assemble(&source, &read, classified)
        .map(|parse| parse.model)
        .map_err(|error| error.to_string())
}

pub fn coverage(parse: &ValidatedParse) -> &CoverageReport {
    match parse {
        ValidatedParse::Session(session) => &session.model().coverage,
        ValidatedParse::Fatal(fatal) => fatal.coverage(),
    }
}

pub fn session(parse: &ValidatedParse) -> Option<&ValidatedSession> {
    match parse {
        ValidatedParse::Session(session) => Some(session),
        ValidatedParse::Fatal(_) => None,
    }
}

pub fn assert_closed_coverage(case: AgentCase, parse: &ValidatedParse) {
    let coverage = coverage(parse);
    assert_eq!(
        coverage.consumed_count + coverage.skipped_count,
        coverage.raw_unit_count,
        "{} must terminate every physical/logical unit",
        case.agent.as_str()
    );
    assert_eq!(
        coverage.raw_unit_count,
        coverage.consumed.len() as u64 + coverage.skipped.len() as u64
    );
    let ordinals: BTreeSet<_> = coverage
        .consumed
        .iter()
        .map(|unit| unit.ordinal)
        .chain(coverage.skipped.iter().map(|unit| unit.ordinal))
        .collect();
    assert_eq!(ordinals, (1..=coverage.raw_unit_count).collect());
    let evidence: BTreeSet<_> = coverage
        .consumed
        .iter()
        .map(|unit| unit.evidence.evidence_event_id.as_str())
        .chain(
            coverage
                .skipped
                .iter()
                .map(|unit| unit.evidence.evidence_event_id.as_str()),
        )
        .collect();
    assert_eq!(evidence.len() as u64, coverage.raw_unit_count);
}

pub fn terminated_base(case: AgentCase) -> Vec<u8> {
    let mut bytes = case.base.to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes
}

pub fn malformed_tail(case: AgentCase) -> Vec<u8> {
    let mut bytes = terminated_base(case);
    bytes.extend_from_slice(br#"{"type":"truncated"#);
    bytes
}

pub fn unknown_event(case: AgentCase) -> Vec<u8> {
    let mut bytes = terminated_base(case);
    let line = match case.agent {
        AgentKind::Junie => r#"{"kind":"FutureJunieEvent","payload":{"shape":"unknown"}}"#,
        _ => r#"{"type":"future_visible_event","payload":{"shape":"unknown"}}"#,
    };
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    bytes
}

pub fn opaque_event(case: AgentCase) -> Vec<u8> {
    let mut bytes = terminated_base(case);
    let line = match case.agent {
        AgentKind::Codex => format!(
            r#"{{"type":"response_item","payload":{{"type":"encrypted_reasoning","encrypted_content":"{SECRET_SENTINEL}"}}}}"#
        ),
        AgentKind::Claude => format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"redacted_thinking","data":"{SECRET_SENTINEL}"}}]}}}}"#
        ),
        AgentKind::Gemini => {
            format!(r#"{{"type":"future_opaque_event","encryptedPayload":"{SECRET_SENTINEL}"}}"#)
        }
        AgentKind::Grok => {
            format!(r#"{{"type":"future_opaque_event","ciphertext":"{SECRET_SENTINEL}"}}"#)
        }
        AgentKind::Junie => {
            format!(r#"{{"kind":"FutureOpaqueEvent","ciphertext":"{SECRET_SENTINEL}"}}"#)
        }
    };
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    bytes
}

pub fn mutate_visible(case: AgentCase) -> Vec<u8> {
    let source = String::from_utf8(case.base.to_vec()).expect("fixture utf-8");
    assert!(source.contains(case.mutation_needle));
    source
        .replacen(case.mutation_needle, "MUTATED visible event", 1)
        .into_bytes()
}

pub fn evidence_by_locator(parse: &ValidatedParse) -> BTreeMap<(String, String), String> {
    let coverage = coverage(parse);
    coverage
        .consumed
        .iter()
        .map(|unit| &unit.evidence)
        .chain(coverage.skipped.iter().map(|unit| &unit.evidence))
        .map(|evidence| {
            (
                (evidence.locator.clone(), evidence.unit_kind.clone()),
                evidence.evidence_event_id.clone(),
            )
        })
        .collect()
}

pub fn canonical(parse: &ValidatedParse) -> Option<Vec<u8>> {
    session(parse).map(|session| canonical_bytes(session).expect("canonical bytes"))
}
