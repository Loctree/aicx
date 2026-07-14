//! Backward-compatibility regression for `ChunkMetadataSidecar`.
//!
//! `noise_lines_dropped` was added in the noise-filter rollout (commit
//! `ffe288a`). The live store at `~/.aicx/store/` already contains ~1068
//! sidecars from older builds that lack this field. They MUST deserialize
//! without error and produce `noise_lines_dropped == 0`. New sidecars with
//! `noise_lines_dropped == 0` must skip the field on serialization so the
//! on-disk wire format stays compatible with consumers expecting the
//! pre-rollout shape.

use aicx_parser::{
    CARD_CLAIM_SCOPE_SESSION_CLOSE, CARD_FRESHNESS_CONTRACT_HISTORICAL, CARD_SCHEMA_VERSION,
    CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX, CardSignal, CardSource, ChunkMetadataSidecar,
};

const OLD_SIDECAR_JSON: &str = r#"{
  "id": "Loctree_aicx_2026-04-25_001",
  "project": "Loctree/aicx",
  "agent": "claude",
  "date": "2026-04-25",
  "session_id": "2921f021-3af4-4d6f-b378-73ad9575268e",
  "cwd": "/Users/user/test-org/vc-runtime/aicx",
  "kind": "conversations",
  "frame_kind": "agent_reply",
  "agent_model": "claude-opus-4-7",
  "started_at": "2026-04-25T15:58:59Z",
  "completed_at": "2026-04-25T18:25:39Z",
  "workflow_phase": "implement",
  "skill_code": "vc-ownership"
}"#;

#[test]
fn old_sidecar_without_noise_lines_dropped_deserializes_with_zero_default() {
    let sidecar: ChunkMetadataSidecar =
        serde_json::from_str(OLD_SIDECAR_JSON).expect("old-shape sidecar must deserialize");

    assert_eq!(
        sidecar.noise_lines_dropped, 0,
        "missing field must default to 0 via #[serde(default)]"
    );
    // Spot-check the rest of the parse to make sure we didn't mangle anything.
    assert_eq!(sidecar.id, "Loctree_aicx_2026-04-25_001");
    assert_eq!(sidecar.project, "Loctree/aicx");
    assert_eq!(sidecar.agent, "claude");
    assert_eq!(sidecar.session_id, "2921f021-3af4-4d6f-b378-73ad9575268e");
    assert_eq!(sidecar.schema_version, 1);
    assert_eq!(sidecar.timestamp_source, None);
    assert_eq!(sidecar.workflow_phase.as_deref(), Some("implement"));
    assert_eq!(sidecar.skill_code.as_deref(), Some("vc-ownership"));
}

#[test]
fn v1_sidecar_without_card_schema_defaults_contract_fields() {
    let sidecar: ChunkMetadataSidecar =
        serde_json::from_str(OLD_SIDECAR_JSON).expect("v1 sidecar must deserialize");

    assert_eq!(sidecar.schema_version, 1);
    assert!(sidecar.source.is_none());
    assert!(sidecar.claim_scope.is_none());
    assert!(sidecar.freshness_contract.is_none());
    assert!(sidecar.verification_state.is_none());
    assert!(sidecar.signals.is_none());
}

#[test]
fn v2_sidecar_round_trips_all_card_contract_fields_losslessly() {
    let value = serde_json::json!({
        "id": "Loctree_aicx_2026-07-02_001",
        "schema_version": CARD_SCHEMA_VERSION,
        "project": "Loctree/aicx",
        "agent": "codex",
        "date": "2026-07-02",
        "session_id": "session-001",
        "cwd": "/Users/user/test-org/aicx",
        "kind": "conversations",
        "frame_kind": "agent_reply",
        "content_sha256": "body-sha",
        "source": {
            "path": "/Users/user/.codex/sessions/session.jsonl",
            "sha256": "raw-sha",
            "span": [42, 88]
        },
        "claim_scope": CARD_CLAIM_SCOPE_SESSION_CLOSE,
        "freshness_contract": CARD_FRESHNESS_CONTRACT_HISTORICAL,
        "verification_state": CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX,
        "signals": [
            {
                "kind": "decision",
                "text": "ship typed card contracts",
                "line_span": [12, 14],
                "extractor_version": "signals.v1"
            }
        ]
    });

    let sidecar: ChunkMetadataSidecar =
        serde_json::from_value(value).expect("v2 sidecar must deserialize");
    assert_eq!(sidecar.schema_version, CARD_SCHEMA_VERSION);
    assert_eq!(
        sidecar.source,
        Some(CardSource {
            path: "/Users/user/.codex/sessions/session.jsonl".to_string(),
            sha256: Some("raw-sha".to_string()),
            span: Some((42, 88)),
        })
    );
    assert_eq!(
        sidecar.claim_scope.as_deref(),
        Some(CARD_CLAIM_SCOPE_SESSION_CLOSE)
    );
    assert_eq!(
        sidecar.freshness_contract.as_deref(),
        Some(CARD_FRESHNESS_CONTRACT_HISTORICAL)
    );
    assert_eq!(
        sidecar.verification_state.as_deref(),
        Some(CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX)
    );
    assert_eq!(
        sidecar.signals.as_deref(),
        Some(
            &[CardSignal {
                kind: "decision".to_string(),
                text: "ship typed card contracts".to_string(),
                line_span: Some((12, 14)),
                extractor_version: Some("signals.v1".to_string()),
            }][..]
        )
    );

    let serialized = serde_json::to_value(&sidecar).expect("serialize v2 sidecar");
    assert_eq!(serialized["schema_version"], CARD_SCHEMA_VERSION);
    let reparsed: ChunkMetadataSidecar =
        serde_json::from_value(serialized).expect("reparse v2 sidecar");
    assert_eq!(reparsed, sidecar);
}

#[test]
fn sidecar_unknown_extra_fields_do_not_fail_deserialization() {
    let sidecar: ChunkMetadataSidecar = serde_json::from_value(serde_json::json!({
        "id": "Loctree_aicx_2026-07-02_002",
        "schema_version": CARD_SCHEMA_VERSION,
        "project": "Loctree/aicx",
        "agent": "codex",
        "date": "2026-07-02",
        "session_id": "session-002",
        "kind": "conversations",
        "source": {
            "path": "/tmp/raw.jsonl",
            "unknown_source_field": "ignored"
        },
        "unknown_outer_field": {
            "nested": true
        }
    }))
    .expect("serde should ignore unknown sidecar fields");

    assert_eq!(sidecar.schema_version, CARD_SCHEMA_VERSION);
    assert_eq!(
        sidecar.source.as_ref().map(|source| source.path.as_str()),
        Some("/tmp/raw.jsonl")
    );
}

#[test]
fn legacy_string_schema_version_remains_readable_as_v1() {
    let sidecar: ChunkMetadataSidecar = serde_json::from_value(serde_json::json!({
        "id": "ctx-001",
        "project": "Loctree/aicx",
        "agent": "loct-context-pack",
        "date": "2026-05-08",
        "session_id": "batch-001",
        "kind": "reports",
        "artifact_family": "loct-context-pack",
        "schema_version": "context_corpus.v1",
        "content_sha256": "abc123"
    }))
    .expect("legacy string schema version should remain readable");

    assert_eq!(sidecar.schema_version, 1);

    let serialized = serde_json::to_string(&sidecar).expect("serialize");
    assert!(
        !serialized.contains("schema_version"),
        "legacy v1 default should remain skipped on serialization, got: {serialized}"
    );
}

#[test]
fn new_sidecar_with_zero_drops_skips_field_on_serialize() {
    // Build a "new" sidecar manually with noise_lines_dropped == 0 and
    // verify the serialized JSON omits the field — preserves wire-format
    // backward compatibility for any consumer reading old shape.
    let mut sidecar: ChunkMetadataSidecar = serde_json::from_str(OLD_SIDECAR_JSON).unwrap();
    sidecar.noise_lines_dropped = 0;

    let json = serde_json::to_string(&sidecar).expect("serialize");
    assert!(
        !json.contains("noise_lines_dropped"),
        "zero-valued counter must be skipped on serialization, got: {json}"
    );
    assert!(
        !json.contains("timestamp_source"),
        "absent timestamp source must be skipped on serialization, got: {json}"
    );
}

#[test]
fn new_sidecar_with_nonzero_drops_emits_field_on_serialize() {
    let mut sidecar: ChunkMetadataSidecar = serde_json::from_str(OLD_SIDECAR_JSON).unwrap();
    sidecar.noise_lines_dropped = 42;

    let json = serde_json::to_string(&sidecar).expect("serialize");
    assert!(
        json.contains("\"noise_lines_dropped\":42"),
        "nonzero counter must be present on serialization, got: {json}"
    );

    // Round-trip: deserialize the new shape and confirm value survives.
    let roundtrip: ChunkMetadataSidecar =
        serde_json::from_str(&json).expect("new-shape round-trip");
    assert_eq!(roundtrip.noise_lines_dropped, 42);
}

#[test]
fn sidecar_with_timestamp_source_emits_field_on_serialize() {
    let mut sidecar: ChunkMetadataSidecar = serde_json::from_str(OLD_SIDECAR_JSON).unwrap();
    sidecar.timestamp_source = Some("fallback_previous".to_string());

    let json = serde_json::to_string(&sidecar).expect("serialize");
    assert!(
        json.contains("\"timestamp_source\":\"fallback_previous\""),
        "timestamp source must be present when inferred, got: {json}"
    );

    let roundtrip: ChunkMetadataSidecar =
        serde_json::from_str(&json).expect("timestamp-source round-trip");
    assert_eq!(
        roundtrip.timestamp_source.as_deref(),
        Some("fallback_previous")
    );
}

#[test]
fn round_trip_old_to_new_to_old_is_lossless_for_known_fields() {
    // Take an old sidecar, deserialize it (gets noise_lines_dropped=0
    // implicitly), serialize it back. The output should be parseable by both
    // old and new consumers (no new fields surface, all old fields preserved).
    let parsed: ChunkMetadataSidecar = serde_json::from_str(OLD_SIDECAR_JSON).unwrap();
    let reserialized = serde_json::to_string(&parsed).expect("re-serialize");
    let reparsed: ChunkMetadataSidecar =
        serde_json::from_str(&reserialized).expect("re-parse round-trip output");

    assert_eq!(parsed, reparsed, "round-trip must be lossless");
    assert_eq!(reparsed.noise_lines_dropped, 0);
    assert!(
        !reserialized.contains("noise_lines_dropped"),
        "field must remain skipped on round-trip when value stays zero"
    );
}
