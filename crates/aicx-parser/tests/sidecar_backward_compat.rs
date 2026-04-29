//! Backward-compatibility regression for `ChunkMetadataSidecar`.
//!
//! `noise_lines_dropped` was added in the noise-filter rollout (commit
//! `ffe288a`). The live store at `~/.aicx/store/` already contains ~1068
//! sidecars from older builds that lack this field. They MUST deserialize
//! without error and produce `noise_lines_dropped == 0`. New sidecars with
//! `noise_lines_dropped == 0` must skip the field on serialization so the
//! on-disk wire format stays compatible with consumers expecting the
//! pre-rollout shape.

use aicx_parser::ChunkMetadataSidecar;

const OLD_SIDECAR_JSON: &str = r#"{
  "id": "Loctree_aicx_2026-04-25_001",
  "project": "Loctree/aicx",
  "agent": "claude",
  "date": "2026-04-25",
  "session_id": "2921f021-3af4-4d6f-b378-73ad9575268e",
  "cwd": "/Users/polyversai/Libraxis/vc-runtime/aicx",
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
    assert_eq!(sidecar.workflow_phase.as_deref(), Some("implement"));
    assert_eq!(sidecar.skill_code.as_deref(), Some("vc-ownership"));
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
