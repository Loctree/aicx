mod adversarial_support;

use adversarial_support::*;
use aicx_parser::engine::{CounterSemantics, ReaderPolicy, SkippedReason};
use std::collections::BTreeSet;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn mutation_matrix_closes_every_adapter_boundary() {
    let mut failures = Vec::new();
    for case in cases() {
        let mutations = [
            ("baseline", case.base.to_vec(), ReaderPolicy::default()),
            (
                "truncated_tail",
                malformed_tail(case),
                ReaderPolicy::default(),
            ),
            (
                "unknown_event",
                unknown_event(case),
                ReaderPolicy::default(),
            ),
            (
                "opaque_payload",
                opaque_event(case),
                ReaderPolicy::default(),
            ),
            (
                "oversized_unit",
                terminated_base(case),
                ReaderPolicy {
                    max_source_bytes: 1024 * 1024,
                    max_unit_bytes: 32,
                },
            ),
        ];
        for (mutation, bytes, policy) in mutations {
            let parsed = match try_parse(case, bytes, policy) {
                Ok(parsed) => parsed,
                Err(error) => {
                    failures.push(format!("{}/{}: {error}", case.agent.as_str(), mutation));
                    continue;
                }
            };
            assert_closed_coverage(case, &parsed);
            if mutation == "oversized_unit" {
                assert!(
                    coverage(&parsed)
                        .skipped
                        .iter()
                        .any(|unit| unit.reason == SkippedReason::Oversized),
                    "{} must preserve oversized evidence",
                    case.agent.as_str()
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "production adapters violated adversarial closure:\n{}",
        failures.join("\n")
    );
}

#[test]
fn opaque_and_secret_payloads_never_leak_into_projections() {
    for case in cases() {
        let bytes = opaque_event(case);
        if let Some(model) = assembled_model(case, bytes.clone()).expect("adapter assembly") {
            let projected_text = model
                .turns
                .iter()
                .map(|turn| turn.text.as_str())
                .chain(
                    model
                        .skill_invocations
                        .iter()
                        .map(|skill| skill.skill_name.as_str()),
                )
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !projected_text.contains(SECRET_SENTINEL),
                "{} leaked opaque payload into typed projection",
                case.agent.as_str()
            );
        }
        if let Ok(parsed) = try_parse(case, bytes, ReaderPolicy::default()) {
            assert_closed_coverage(case, &parsed);
            let bytes = canonical(&parsed).expect("non-fatal mutation has canonical projection");
            assert!(!String::from_utf8_lossy(&bytes).contains(SECRET_SENTINEL));
        }
    }
}

#[test]
fn evidence_ids_are_append_stable_and_mutation_scoped_for_all_adapters() {
    for case in cases() {
        let Ok(base) = try_parse(case, case.base.to_vec(), ReaderPolicy::default()) else {
            eprintln!("{} evidence check blocked at baseline", case.agent.as_str());
            continue;
        };
        let Ok(appended) = try_parse(case, unknown_event(case), ReaderPolicy::default()) else {
            eprintln!("{} evidence check blocked at append", case.agent.as_str());
            continue;
        };
        let Ok(mutated) = try_parse(case, mutate_visible(case), ReaderPolicy::default()) else {
            eprintln!("{} evidence check blocked at mutation", case.agent.as_str());
            continue;
        };
        let base_ids = evidence_by_locator(&base);
        let appended_ids = evidence_by_locator(&appended);
        let mutated_ids = evidence_by_locator(&mutated);

        for (key, id) in &base_ids {
            assert_eq!(
                appended_ids.get(key),
                Some(id),
                "{} append changed evidence {key:?}",
                case.agent.as_str()
            );
        }
        let shared: Vec<_> = base_ids
            .keys()
            .filter(|key| mutated_ids.contains_key(*key))
            .collect();
        let changed = shared
            .iter()
            .filter(|key| base_ids.get(**key) != mutated_ids.get(**key))
            .count();
        let unchanged = shared
            .iter()
            .filter(|key| base_ids.get(**key) == mutated_ids.get(**key))
            .count();
        assert!(
            changed >= 1,
            "{} mutation changed no evidence id",
            case.agent.as_str()
        );
        assert!(
            unchanged >= 1,
            "{} mutation must not rewrite all evidence ids",
            case.agent.as_str()
        );
    }
}

#[test]
fn usage_counter_semantics_and_status_survive_mutation() {
    for case in cases() {
        for bytes in [
            case.base.to_vec(),
            malformed_tail(case),
            unknown_event(case),
        ] {
            let Ok(parsed) = try_parse(case, bytes, ReaderPolicy::default()) else {
                eprintln!("{} status check blocked by validator", case.agent.as_str());
                continue;
            };
            assert_closed_coverage(case, &parsed);
            let status = coverage(&parsed).status;
            assert!(
                !(status.malformed_tail_present
                    && matches!(
                        status.visible_completeness,
                        aicx_parser::engine::VisibleCompleteness::CompleteVisible
                    )),
                "{} marked malformed tail complete",
                case.agent.as_str()
            );
            if let Some(session) = session(&parsed) {
                for usage in &session.model().usage_events {
                    assert!(matches!(
                        usage.counter_semantics,
                        CounterSemantics::Snapshot
                            | CounterSemantics::Delta
                            | CounterSemantics::Cumulative
                    ));
                    assert!(!usage.provider.is_empty());
                }
            }
        }
    }
}

#[test]
fn three_thousand_unrelated_files_do_not_expand_selected_source_bytes() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("aicx-c5a-scaling-{unique}"));
    fs::create_dir_all(&root).expect("create scaling corpus");
    for index in 0..3_000 {
        fs::write(
            root.join(format!("unrelated-{index:04}.jsonl")),
            b"SECRET unrelated\n",
        )
        .expect("write unrelated source");
    }

    let mut checked = 0;
    for case in cases() {
        let Ok(parsed) = try_parse(case, case.base.to_vec(), ReaderPolicy::default()) else {
            eprintln!(
                "{} selected-byte check blocked at baseline",
                case.agent.as_str()
            );
            continue;
        };
        assert_closed_coverage(case, &parsed);
        checked += 1;
        let opened_source_files = 1_u64;
        let opened_source_bytes = session(&parsed)
            .map(|session| session.model().provenance.original_source_bytes)
            .unwrap_or_else(|| {
                coverage(&parsed)
                    .consumed
                    .iter()
                    .map(|unit| unit.evidence.original_bytes)
                    .sum()
            });
        assert_eq!(opened_source_files, 1);
        assert_eq!(opened_source_bytes, case.base.len() as u64);
    }
    assert_eq!(
        checked, 4,
        "only the known Grok baseline defect may block this proof"
    );

    let discovered: BTreeSet<_> = fs::read_dir(&root)
        .expect("read scaling corpus")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(discovered.len(), 3_000);
    fs::remove_dir_all(root).expect("remove scaling corpus");
}

#[test]
fn fixture_declares_complete_mutation_matrix() {
    let matrix =
        include_str!("../../../tests/fixtures/parser_engine/adversarial/mutation_matrix.json");
    let value: serde_json::Value = serde_json::from_str(matrix).expect("mutation matrix JSON");
    assert_eq!(value["agents"].as_array().expect("agents").len(), 5);
    assert_eq!(value["mutations"].as_array().expect("mutations").len(), 5);
}
