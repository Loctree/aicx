use super::*;

#[test]
fn full_index_eta_zero_when_no_embeddings() {
    let stats = IndexStats {
        chunks_total: 1000,
        chunks_sampled: 10,
        embeddings_computed: 0,
        embed_errors: 10,
        dimension: None,
        model_id: None,
        model_profile: None,
        fallback_reason: Some("test".into()),
        elapsed_ms: 100,
        dry_run: true,
        index_path: None,
        resumed_embeddings: 0,
        resume_tmp_path: None,
    };
    assert!(stats.full_index_eta_secs().is_none());
}

#[test]
fn full_index_eta_scales_linearly() {
    let stats = IndexStats {
        chunks_total: 10_000,
        chunks_sampled: 10,
        embeddings_computed: 10,
        embed_errors: 0,
        dimension: Some(1024),
        model_id: Some("F2LLM-v2-0.6B.Q4_K_M.gguf".into()),
        model_profile: Some("base".into()),
        fallback_reason: None,
        // 10 embeds in 1000 ms ⇒ 100 ms per embed.
        elapsed_ms: 1000,
        dry_run: true,
        index_path: None,
        resumed_embeddings: 0,
        resume_tmp_path: None,
    };
    // 10000 * 100 ms = 1_000_000 ms = 1000 s.
    assert_eq!(stats.full_index_eta_secs(), Some(1000));
}

#[test]
fn render_stats_text_includes_fallback_reason_when_set() {
    let stats = IndexStats {
        chunks_total: 0,
        chunks_sampled: 0,
        embeddings_computed: 0,
        embed_errors: 0,
        dimension: None,
        model_id: None,
        model_profile: None,
        fallback_reason: Some("native-embedder feature not compiled in".into()),
        elapsed_ms: 5,
        dry_run: true,
        index_path: None,
        resumed_embeddings: 0,
        resume_tmp_path: None,
    };
    let text = render_stats_text(&stats);
    assert!(text.contains("fallback_reason:"));
    assert!(text.contains("native-embedder feature not compiled in"));
    assert!(text.contains("dry-run only"));
}

#[test]
fn render_stats_text_includes_eta_when_available() {
    let stats = IndexStats {
        chunks_total: 5_000,
        chunks_sampled: 50,
        embeddings_computed: 50,
        embed_errors: 0,
        dimension: Some(1024),
        model_id: Some("F2LLM-v2-0.6B.Q4_K_M.gguf".into()),
        model_profile: Some("base".into()),
        fallback_reason: None,
        elapsed_ms: 5_000,
        dry_run: true,
        index_path: None,
        resumed_embeddings: 0,
        resume_tmp_path: None,
    };
    let text = render_stats_text(&stats);
    assert!(text.contains("full_index_eta_secs:"));
    assert!(text.contains("dimension:"));
    assert!(text.contains("F2LLM-v2-0.6B.Q4_K_M.gguf"));
}

#[test]
fn render_stats_json_round_trips() {
    let stats = IndexStats {
        chunks_total: 42,
        chunks_sampled: 8,
        embeddings_computed: 8,
        embed_errors: 0,
        dimension: Some(1024),
        model_id: Some("model-x".into()),
        model_profile: Some("base".into()),
        fallback_reason: None,
        elapsed_ms: 800,
        dry_run: true,
        index_path: None,
        resumed_embeddings: 0,
        resume_tmp_path: None,
    };
    let json = render_stats_json(&stats).expect("serialize");
    assert!(json.contains("\"chunks_total\":42"));
    assert!(json.contains("\"dry_run\":true"));
    assert!(json.contains("\"model_id\":\"model-x\""));
}

#[test]
fn take_prefix_bytes_short_input_unchanged() {
    let s = "hello";
    assert_eq!(take_prefix_bytes(s, 10), "hello");
}

#[test]
fn take_prefix_bytes_caps_at_codepoint_boundary() {
    // "ą" is 2 bytes in UTF-8. Cap at 1 byte must not split it.
    let s = "ąść";
    let out = take_prefix_bytes(s, 1);
    assert_eq!(out, "");
}

#[test]
fn take_prefix_bytes_preserves_codepoints_under_cap() {
    let s = "ąść";
    // Bytes: ą=0xC4 0x85 (2), ś=0xC5 0x9B (2), ć=0xC4 0x87 (2). 6 total.
    let out = take_prefix_bytes(s, 4);
    // Cap of 4 must include exactly two codepoints (ą + ś).
    assert_eq!(out, "ąś");
}

#[test]
fn hybrid_materialization_mode_skips_noop_incremental_when_manifest_matches() {
    assert_eq!(
        decide_hybrid_materialization(true, 0, 0, true, true, true),
        HybridMaterializationMode::Skip
    );
}

#[test]
fn hybrid_materialization_mode_uses_incremental_insert_for_real_delta() {
    assert_eq!(
        decide_hybrid_materialization(true, 3, 0, true, true, true),
        HybridMaterializationMode::IncrementalInsert
    );
}

#[test]
fn hybrid_materialization_mode_falls_back_to_full_when_manifest_mismatches() {
    assert_eq!(
        decide_hybrid_materialization(true, 3, 0, false, true, true),
        HybridMaterializationMode::FullRebuild
    );
}

#[test]
fn hybrid_materialization_mode_falls_back_to_full_without_existing_hybrid() {
    assert_eq!(
        decide_hybrid_materialization(true, 3, 0, true, true, false),
        HybridMaterializationMode::FullRebuild
    );
}

#[test]
fn hybrid_materialization_mode_falls_back_to_full_when_hybrid_source_is_stale() {
    assert_eq!(
        decide_hybrid_materialization(true, 3, 0, true, false, true),
        HybridMaterializationMode::FullRebuild
    );
}

#[test]
fn hybrid_materialization_mode_falls_back_to_full_on_noop_without_existing_hybrid() {
    assert_eq!(
        decide_hybrid_materialization(true, 0, 0, true, true, false),
        HybridMaterializationMode::FullRebuild
    );
}
