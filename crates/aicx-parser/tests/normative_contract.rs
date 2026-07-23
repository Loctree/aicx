mod support;

use aicx_parser::engine::*;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use support::{evidence, model_with_text};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(path: &str) -> String {
    fs::read_to_string(repo_root().join(path)).expect("read normative fixture")
}

#[derive(Debug, Deserialize)]
struct StatusFixture {
    case: Vec<StatusCase>,
}

#[derive(Debug, Deserialize)]
struct StatusCase {
    name: String,
    visible_completeness: String,
    opaque_reasoning_present: bool,
    unsupported_visible_event: bool,
    malformed_tail_present: bool,
    visible_event_lost: bool,
    warnings_count: u64,
    model_projected: bool,
    expect: String,
}

#[test]
fn normative_contract_status_truth_table() {
    let table: StatusFixture = toml::from_str(&fixture(
        "tests/fixtures/parser_engine/contract/parse_status_truth_table.toml",
    ))
    .expect("parse status truth table");
    assert_eq!(table.case.len(), 11);
    for case in table.case {
        let visible = match case.visible_completeness.as_str() {
            "complete_visible" => Some(VisibleCompleteness::CompleteVisible),
            "partial_visible" => Some(VisibleCompleteness::PartialVisible),
            "fatal" => Some(VisibleCompleteness::Fatal),
            _ => None,
        };
        let result = visible
            .ok_or_else(|| "unknown completeness".to_owned())
            .and_then(|visible| {
                let unit = evidence(1, "message", case.name.as_bytes());
                let warnings = (case.warnings_count > 0)
                    .then_some(CoverageWarning {
                        kind: if case.unsupported_visible_event {
                            WarningKind::UnsupportedVisibleEvent
                        } else if case.opaque_reasoning_present {
                            WarningKind::OpaqueReasoning
                        } else {
                            WarningKind::MalformedUnit
                        },
                        count: case.warnings_count,
                        first_ordinal: 1,
                    })
                    .into_iter()
                    .collect();
                let coverage = CoverageReport::new(
                    1,
                    vec![ConsumedUnit {
                        ordinal: 1,
                        kind: "message".to_owned(),
                        evidence: unit,
                    }],
                    Vec::new(),
                    warnings,
                    ParseStatus {
                        visible_completeness: visible,
                        boundary_flags: BoundaryFlags {
                            opaque_reasoning_present: case.opaque_reasoning_present,
                            unsupported_visible_event: case.unsupported_visible_event,
                            compaction_boundary_present: false,
                        },
                        malformed_tail_present: case.malformed_tail_present,
                        visible_event_lost: case.visible_event_lost,
                    },
                );
                validate_coverage(&coverage, case.model_projected)
                    .map_err(|error| error.to_string())
            });
        assert_eq!(
            result.is_ok(),
            case.expect == "valid",
            "truth-table case {} produced {result:?}",
            case.name
        );
    }
}

#[test]
fn normative_contract_usage_matrix() {
    let matrix: toml::Value = toml::from_str(&fixture(
        "tests/fixtures/parser_engine/contract/usage_matrix.toml",
    ))
    .expect("parse usage matrix");
    let events = matrix["event"].as_array().expect("valid usage events");
    assert_eq!(events.len(), 5);
    let mut models_by_session: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for event in events {
        let mut usage = usage_from_fixture(event).expect("valid UsageEvent fixture");
        let session = event["session"].as_str().unwrap().to_owned();
        if let Known::Value(model) = &usage.model {
            models_by_session
                .entry(session)
                .or_default()
                .push(model.clone());
        }
        let mut model = model_with_text("usage matrix");
        usage.evidence = model.coverage.consumed[0].evidence.clone();
        model.usage_events = vec![usage];
        validate_parse(UnvalidatedParse::from_model(model)).expect("usage fixture validates");
    }
    assert_eq!(
        models_by_session["s-drift-1"],
        vec!["claude-opus-4-8".to_owned(), "claude-sonnet-5".to_owned()]
    );

    let invalid = matrix["invalid_event"]
        .as_array()
        .expect("invalid usage events");
    assert_eq!(invalid.len(), 4);
    for event in invalid {
        let converted = usage_from_fixture(event);
        if let Ok(mut usage) = converted {
            let mut model = model_with_text("invalid usage matrix");
            usage.evidence = model.coverage.consumed[0].evidence.clone();
            model.usage_events = vec![usage];
            assert!(
                validate_parse(UnvalidatedParse::from_model(model)).is_err(),
                "invalid fixture unexpectedly validated: {}",
                event["name"].as_str().unwrap()
            );
        }
    }
}

fn usage_from_fixture(value: &toml::Value) -> Result<UsageEvent, String> {
    let semantics = match value["counter_semantics"].as_str().unwrap_or_default() {
        "snapshot" => CounterSemantics::Snapshot,
        "delta" => CounterSemantics::Delta,
        "cumulative" => CounterSemantics::Cumulative,
        other => return Err(format!("invalid counter semantics: {other}")),
    };
    let tokens = value["tokens"].as_table().ok_or("missing tokens")?;
    let cost = value["cost"].as_table().ok_or("missing cost")?;
    let cost = if cost.get("value").and_then(toml::Value::as_str) == Some("unknown") {
        Known::unknown()
    } else {
        let amount = cost
            .get("amount")
            .and_then(toml::Value::as_float)
            .ok_or("reported cost missing amount")?;
        let currency = cost
            .get("currency")
            .and_then(toml::Value::as_str)
            .ok_or("reported cost missing currency")?;
        Known::value(ReportedCost {
            amount,
            currency: currency.to_owned(),
        })
    };
    let timestamp = match value["timestamp"].as_str().unwrap_or_default() {
        "unknown" => Known::unknown(),
        timestamp => Known::value(timestamp.to_owned()),
    };
    Ok(UsageEvent {
        provider: value["provider"].as_str().unwrap_or_default().to_owned(),
        model: match value["model"].as_str().unwrap_or_default() {
            "unknown" => Known::unknown(),
            model => Known::value(model.to_owned()),
        },
        tokens: TokenComponents {
            input: known_token(tokens.get("input"))?,
            output: known_token(tokens.get("output"))?,
            reasoning: known_token(tokens.get("reasoning"))?,
            cache_read: known_token(tokens.get("cache_read"))?,
            cache_creation: known_token(tokens.get("cache_creation"))?,
        },
        cost,
        timestamp,
        span: Known::unknown(),
        counter_semantics: semantics,
        evidence: evidence(1, "usage", value["name"].as_str().unwrap().as_bytes()),
    })
}

fn known_token(value: Option<&toml::Value>) -> Result<Known<u64>, String> {
    match value {
        Some(toml::Value::String(value)) if value == "unknown" => Ok(Known::unknown()),
        Some(toml::Value::Integer(value)) if *value >= 0 => Ok(Known::value(*value as u64)),
        _ => Err("token must be non-negative integer or unknown".to_owned()),
    }
}

#[test]
fn normative_contract_evidence_identity_properties() {
    let base = fixture("tests/fixtures/parser_engine/contract/identity_base.jsonl");
    let appended = fixture("tests/fixtures/parser_engine/contract/identity_append.jsonl");
    let mutated = fixture("tests/fixtures/parser_engine/contract/identity_mutated.jsonl");
    let base_ids = ids_for(&base);
    let appended_ids = ids_for(&appended);
    let mutated_ids = ids_for(&mutated);
    assert_eq!(&appended_ids[..base_ids.len()], base_ids);
    assert_eq!(
        base_ids
            .iter()
            .zip(&mutated_ids)
            .filter(|(left, right)| left != right)
            .count(),
        1
    );
    assert_eq!(
        base_ids.len(),
        base_ids.iter().collect::<BTreeSet<_>>().len()
    );
    assert!(
        base_ids
            .iter()
            .all(|id| !id.contains("/Users/") && !id.contains("/Volumes/"))
    );

    // Relocation cannot affect the pure derivation because no path is accepted.
    assert_eq!(ids_for(&base), ids_for(&base));
    assert_ne!(
        evidence_event_id(AgentKind::Codex, "s", "000001", "message", b"before").unwrap(),
        evidence_event_id(AgentKind::Codex, "s", "000001", "message", b"after").unwrap()
    );
}

fn ids_for(source: &str) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let value: serde_json::Value = serde_json::from_str(line).expect("identity JSONL");
            let kind = value["type"].as_str().unwrap_or("unknown");
            evidence_event_id(
                AgentKind::Codex,
                "contract-session",
                &ordinal_locator(index as u64 + 1),
                kind,
                line.as_bytes(),
            )
            .expect("identity fixture id")
        })
        .collect()
}

#[test]
fn normative_contract_field_matrix_limits_fingerprint_to_normative_truth() {
    let fields: toml::Value = toml::from_str(&fixture("tests/parser_oracle/normative_fields.toml"))
        .expect("parse normative field matrix");
    let mut fingerprint_paths = BTreeSet::new();
    for group in ["field", "aicx_field"] {
        for field in fields[group].as_array().expect("field matrix group") {
            if field["kernel_fingerprint"].as_bool() == Some(true) {
                assert_eq!(field["class"].as_str(), Some("normative"));
                fingerprint_paths.insert(field["path"].as_str().unwrap().to_owned());
            }
        }
    }
    for required in [
        "session_id",
        "usage_events[]",
        "evidence_event_id",
        "visible_completeness",
        "boundary_flags.opaque_reasoning_present",
        "boundary_flags.unsupported_visible_event",
    ] {
        assert!(fingerprint_paths.contains(required), "missing {required}");
    }
    assert!(!fingerprint_paths.contains("generated_at"));
    assert!(!fingerprint_paths.contains("intent.summary"));
}

#[test]
fn normative_contract_taxonomy_fixture_is_exhaustive_across_agents() {
    let taxonomy: toml::Value = toml::from_str(&fixture(
        "tests/fixtures/parser_engine/contract/taxonomy_units.toml",
    ))
    .expect("parse taxonomy fixtures");
    let units = taxonomy["unit"].as_array().expect("taxonomy units");
    let agents: BTreeSet<_> = units
        .iter()
        .map(|unit| unit["agent"].as_str().unwrap())
        .collect();
    assert_eq!(
        agents,
        BTreeSet::from(["claude", "codex", "gemini", "grok", "junie"])
    );
    assert!(units.len() >= 38);
    assert!(units.iter().all(|unit| {
        unit["expected_kind"].as_str().is_some()
            && unit["sample"].as_str().is_some()
            && matches!(unit["level"].as_str(), Some("physical" | "logical"))
    }));
}
