mod adversarial_support;

use adversarial_support::*;
use aicx_parser::engine::{ReaderPolicy, sha256_hex};
use std::collections::BTreeSet;

#[test]
fn identical_bytes_and_config_are_canonical_over_one_hundred_runs() {
    let mut failures = Vec::new();
    for case in cases() {
        let mut hashes = BTreeSet::new();
        let mut expected = None;
        for run in 0..100 {
            let parsed = match try_parse(case, case.base.to_vec(), ReaderPolicy::default()) {
                Ok(parsed) => parsed,
                Err(error) => {
                    failures.push(format!("{} run {run}: {error}", case.agent.as_str()));
                    break;
                }
            };
            assert_closed_coverage(case, &parsed);
            let bytes = canonical(&parsed)
                .unwrap_or_else(|| panic!("{} baseline unexpectedly fatal", case.agent.as_str()));
            let hash = sha256_hex(&bytes);
            hashes.insert(hash);
            if let Some(expected) = &expected {
                assert_eq!(
                    &bytes,
                    expected,
                    "{} run {run} drifted",
                    case.agent.as_str()
                );
            } else {
                expected = Some(bytes);
            }
        }
        if let Some(hash) = hashes.first() {
            assert_eq!(
                hashes.len(),
                1,
                "{} produced multiple hashes",
                case.agent.as_str()
            );
            eprintln!("determinism {} sha256={hash}", case.agent.as_str());
        }
    }
    assert!(
        failures.is_empty(),
        "production adapters prevented deterministic projection:\n{}",
        failures.join("\n")
    );
}
