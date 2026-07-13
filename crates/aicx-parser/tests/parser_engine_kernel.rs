mod support;

use aicx_parser::adapters::AgentAdapter;
use aicx_parser::engine::*;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};
use support::{complete_coverage, evidence, model_with_text, validated_model};

#[test]
fn parser_engine_api_requires_an_explicit_source_handle() {
    #[allow(dead_code)]
    fn compile_witness<A: AgentAdapter + ?Sized>(
        engine: &ParserEngine,
        source: &SourceHandle,
        adapter: &A,
    ) {
        let _ = engine.parse(source, adapter);
    }

    // Source-level contract: the kernel has no discovery/process escape hatch.
    let source = include_str!("../src/engine/source.rs");
    let reader = include_str!("../src/engine/reader.rs");
    let kernel = include_str!("../src/engine/mod.rs");
    for forbidden in [
        "read_dir(",
        "walkdir",
        "glob(",
        "Command::new",
        "std::process",
    ] {
        assert!(!source.contains(forbidden), "source contains {forbidden}");
        assert!(!reader.contains(forbidden), "reader contains {forbidden}");
        assert!(!kernel.contains(forbidden), "kernel contains {forbidden}");
    }
}

#[test]
fn session_model_roundtrip_preserves_typed_kernel_truth() {
    let model = model_with_text("typed roundtrip");
    let bytes = serde_json::to_vec(&model).expect("serialize SessionModel");
    let restored: SessionModel = serde_json::from_slice(&bytes).expect("deserialize SessionModel");
    assert_eq!(restored, model);
    assert_eq!(restored.turns[0].raw_unit_refs.len(), 1);
    assert_eq!(restored.tool_events[0].tool_name, "shell");
    assert_eq!(restored.usage_events[0].provider, "openai");
    assert!(
        restored.turns[0].raw_unit_refs[0]
            .evidence_event_id
            .starts_with("ev1:codex:")
    );
    assert_eq!(restored.coverage.consumed_count, 1);
}

#[test]
fn coverage_validator_rejects_mutation_matrix() {
    let refs = vec![
        evidence(1, "message", b"one"),
        evidence(2, "message", b"two"),
        evidence(3, "message", b"three"),
    ];
    let base = complete_coverage(&refs);
    validate_coverage(&base, true).expect("baseline coverage");

    let mut invalid_count = base.clone();
    invalid_count.consumed_count += 1;
    assert_eq!(
        validate_coverage(&invalid_count, true)
            .expect_err("count mutation")
            .invariant,
        "coverage_counts"
    );

    let mut overlap = base.clone();
    overlap.raw_unit_count += 1;
    overlap.skipped_count = 1;
    overlap.skipped.push(SkippedUnit {
        ordinal: 1,
        reason: SkippedReason::Malformed,
        bytes: refs[0].original_bytes,
        visible: true,
        evidence: refs[0].clone(),
    });
    assert_eq!(
        validate_coverage(&overlap, true)
            .expect_err("overlap mutation")
            .invariant,
        "coverage_overlap"
    );

    let mut out_of_range = base.clone();
    out_of_range.consumed[2].ordinal = 4;
    out_of_range.consumed[2].evidence.coverage_ordinal = 4;
    out_of_range.consumed_ranges[0].end = 4;
    assert_eq!(
        validate_coverage(&out_of_range, true)
            .expect_err("out-of-range mutation")
            .invariant,
        "coverage_ordinal"
    );

    let mut gap_encoding = base.clone();
    gap_encoding.consumed_ranges = vec![
        OrdinalRange { start: 1, end: 1 },
        OrdinalRange { start: 3, end: 3 },
    ];
    assert_eq!(
        validate_coverage(&gap_encoding, true)
            .expect_err("gap mutation")
            .invariant,
        "consumed_ranges"
    );

    let mut silent_unknown = base.clone();
    let removed = silent_unknown.consumed.remove(1);
    silent_unknown.consumed_count -= 1;
    silent_unknown.skipped_count += 1;
    silent_unknown.skipped.push(SkippedUnit {
        ordinal: removed.ordinal,
        reason: SkippedReason::UnknownPayloadType,
        bytes: removed.evidence.original_bytes,
        visible: false,
        evidence: removed.evidence,
    });
    silent_unknown.consumed_ranges = vec![
        OrdinalRange { start: 1, end: 1 },
        OrdinalRange { start: 3, end: 3 },
    ];
    assert_eq!(
        validate_coverage(&silent_unknown, true)
            .expect_err("silent unknown mutation")
            .invariant,
        "silent_unknown"
    );
}

#[test]
fn canonical_serialization_is_stable_and_content_bounded() {
    let session = validated_model("not emitted in canonical bytes");
    let expected = canonical_bytes(&session).expect("canonical serialization");
    for _ in 0..100 {
        assert_eq!(canonical_bytes(&session).unwrap(), expected);
        assert_eq!(canonical_fingerprint(&session).unwrap().len(), 64);
    }
    let text = String::from_utf8(expected).expect("canonical JSON");
    assert!(!text.contains("not emitted in canonical bytes"));
    assert!(!text.contains("generated_at"));
    assert!(!text.contains("original_jsonl_path"));
}

#[test]
fn reader_enforces_validated_open_and_max_unit_size() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("aicx-parser-kernel-{unique}.jsonl"));
    fs::write(&path, b"{}\n").expect("write test fixture");
    let artifact = SourceArtifact::validated_file("session.jsonl", &path, SourceFraming::JsonLines)
        .expect("validated-open explicit source");
    let source = SourceHandle::new(AgentKind::Codex, "source-test", None, vec![artifact])
        .expect("source handle");
    let read = RawUnitReader::new(ReaderPolicy {
        max_source_bytes: 64,
        max_unit_bytes: 1,
    })
    .read(&source)
    .expect("bounded read");
    assert_eq!(read.units[0].boundary, UnitBoundary::Oversized);
    assert_eq!(read.units[0].bytes.len(), 1);
    assert_eq!(read.units[0].original_bytes, 2);
    fs::remove_file(path).expect("remove test fixture");

    assert!(
        SourceArtifact::validated_file(
            "missing.jsonl",
            "../missing.jsonl",
            SourceFraming::JsonLines,
        )
        .is_err()
    );
}

#[test]
fn malformed_tail_keeps_earlier_valid_model_and_fatal_has_no_projection() {
    let good = evidence(1, "message", b"good");
    let bad = evidence(2, "malformed", b"{");
    let coverage = CoverageReport::new(
        2,
        vec![ConsumedUnit {
            ordinal: 1,
            kind: "message".to_owned(),
            evidence: good,
        }],
        vec![SkippedUnit {
            ordinal: 2,
            reason: SkippedReason::Malformed,
            bytes: 1,
            visible: true,
            evidence: bad,
        }],
        vec![CoverageWarning {
            kind: WarningKind::UnterminatedTail,
            count: 1,
            first_ordinal: 2,
        }],
        ParseStatus {
            visible_completeness: VisibleCompleteness::PartialVisible,
            boundary_flags: BoundaryFlags::default(),
            malformed_tail_present: true,
            visible_event_lost: false,
        },
    );
    validate_coverage(&coverage, true).expect("partial model preserves prefix");

    let mut fatal = coverage;
    fatal.status.visible_completeness = VisibleCompleteness::Fatal;
    assert_eq!(
        validate_coverage(&fatal, true)
            .expect_err("fatal projection")
            .invariant,
        "fatal_projection"
    );
    assert!(matches!(
        validate_parse(UnvalidatedParse::fatal(fatal)).expect("fatal without model"),
        ValidatedParse::Fatal(_)
    ));
}

#[test]
fn kernel_source_contains_no_store_before_validation_path() {
    let kernel = include_str!("../src/engine/mod.rs");
    for forbidden in ["std::fs::write", "create_file_validated", "store::"] {
        assert!(!kernel.contains(forbidden), "kernel contains {forbidden}");
    }
}
