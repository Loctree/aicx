//! End-to-end coverage for the noise filter integrated through `chunk_entries`.
//!
//! Unit tests in `noise.rs` cover the regex layer in isolation; this suite
//! verifies that real noisy entries flowing through the full chunking pipeline
//! produce semantically clean chunks, with header/timestamp scaffolding intact.

use aicx_parser::ChunkerConfig;
use aicx_parser::chunker::chunk_entries;
use aicx_parser::timeline::TimelineEntry;
use chrono::{TimeZone, Utc};

fn entry(seconds_offset: i64, role: &str, message: &str) -> TimelineEntry {
    let ts = Utc.with_ymd_and_hms(2026, 4, 29, 18, 0, 0).unwrap()
        + chrono::Duration::seconds(seconds_offset);
    TimelineEntry {
        timestamp: ts,
        agent: "claude".to_string(),
        session_id: "noise-filter-e2e".to_string(),
        role: role.to_string(),
        message: message.to_string(),
        frame_kind: None,
        branch: None,
        cwd: None,
    }
}

#[test]
fn chunk_entries_strips_three_noise_classes_end_to_end() {
    // Repeat the noisy block enough times to reach the 500-token min chunk
    // threshold so chunk_entries actually emits a chunk.
    let mut messages = Vec::new();
    for round in 0..20 {
        messages.push(entry(
            round * 10,
            "assistant",
            &format!(
                "60 Passed:\nReal semantic content paragraph {round}.\ninput: {{\"command\":\"for run in agnt-{round}\"}}\n---\nMore real content for round {round}."
            ),
        ));
    }

    let chunks = chunk_entries(
        &messages,
        "test-project",
        "claude",
        &ChunkerConfig::default(),
    );
    assert!(
        !chunks.is_empty(),
        "expected at least one chunk to be emitted"
    );

    for (idx, chunk) in chunks.iter().enumerate() {
        let text = &chunk.text;

        // Class 1: line-numbered grep matches must be absent.
        assert!(
            !text.contains("60 Passed:"),
            "chunk {idx}: line-numbered noise leaked into chunk text:\n{text}"
        );

        // Class 2: tool-call echo must be absent.
        assert!(
            !text.contains("input: {\"command\""),
            "chunk {idx}: tool-call echo leaked into chunk text:\n{text}"
        );

        // Class 3: stray YAML delimiter must be absent on its own line.
        for line in text.lines() {
            assert!(
                line.trim() != "---",
                "chunk {idx}: stray YAML delimiter survived as line:\n{text}"
            );
        }

        // Semantic content must survive.
        assert!(
            text.contains("Real semantic content paragraph"),
            "chunk {idx}: semantic content was dropped:\n{text}"
        );
        assert!(
            text.contains("More real content"),
            "chunk {idx}: post-noise semantic content was dropped:\n{text}"
        );

        // Chunker scaffolding (project header + timestamp lines) must remain.
        assert!(
            text.contains("[project: test-project"),
            "chunk {idx}: project header missing — filter overreached:\n{text}"
        );
        assert!(
            text.contains("[18:0"),
            "chunk {idx}: timestamp scaffolding missing — filter overreached:\n{text}"
        );
    }
}

#[test]
fn chunk_entries_skips_entries_that_reduce_to_only_noise() {
    // Half the entries are pure noise, half are semantic. Pure-noise entries
    // should be elided; semantic entries should pass through.
    let entries = vec![
        entry(0, "user", "60 Passed:\n7 status: completed\n---"),
        entry(
            5,
            "assistant",
            "Decision: keep the retry path. This is a load-bearing observation that\n\
             must survive the filter, plus more text to push us past the minimum\n\
             token floor for a chunk window. "
                .repeat(20)
                .as_str(),
        ),
        entry(15, "user", "input: {\"echo\":\"x\"}"),
    ];

    let chunks = chunk_entries(
        &entries,
        "test-project",
        "claude",
        &ChunkerConfig::default(),
    );
    assert!(!chunks.is_empty(), "expected at least one chunk");

    for chunk in &chunks {
        let text = &chunk.text;
        assert!(
            text.contains("Decision: keep the retry path."),
            "semantic content was dropped:\n{text}"
        );
        assert!(
            !text.contains("60 Passed:"),
            "noise leaked through chunker:\n{text}"
        );
        assert!(
            !text.contains("input: {\"echo\""),
            "tool echo leaked through chunker:\n{text}"
        );
    }
}

#[test]
fn chunk_entries_records_noise_lines_dropped_on_sidecar() {
    // 5 entries, each contributing 3 noise lines → 15 dropped lines minimum
    // (chunker may emit 1+ chunks; we assert the aggregate per-chunk counter
    // is non-zero and equals what the entries supplied).
    let noise_per_entry =
        "60 Passed:\nReal content here.\ninput: {\"k\":1}\n---\nMore real content.";
    let entries: Vec<TimelineEntry> = (0..15)
        .map(|i| entry(i * 5, "assistant", noise_per_entry))
        .collect();

    let chunks = chunk_entries(
        &entries,
        "test-project",
        "claude",
        &ChunkerConfig::default(),
    );
    assert!(!chunks.is_empty());

    let total_dropped: usize = chunks.iter().map(|c| c.noise_lines_dropped).sum();
    assert!(
        total_dropped > 0,
        "expected non-zero noise_lines_dropped across chunks, got 0"
    );

    // Each chunk that consumed any of these entries must report >0 drops.
    for chunk in &chunks {
        assert!(
            chunk.noise_lines_dropped > 0,
            "chunk {} reported zero drops despite consuming noisy entries",
            chunk.id
        );
    }
}

#[test]
fn chunk_entries_disabled_filter_preserves_raw_content_and_zero_count() {
    let noisy = "60 Passed:\nReal content.\ninput: {\"k\":1}\n---";
    let entries: Vec<TimelineEntry> = (0..20).map(|i| entry(i * 5, "assistant", noisy)).collect();

    let config = ChunkerConfig {
        noise_filter_enabled: false,
        ..ChunkerConfig::default()
    };
    let chunks = chunk_entries(&entries, "test-project", "claude", &config);
    assert!(!chunks.is_empty());

    // Counter must be zero — filter was never engaged.
    for chunk in &chunks {
        assert_eq!(
            chunk.noise_lines_dropped, 0,
            "filter was disabled but counter advanced on chunk {}",
            chunk.id
        );
    }

    // At least one chunk must contain the raw noise (proving opt-out works).
    let any_chunk_has_noise = chunks.iter().any(|c| c.text.contains("60 Passed:"));
    assert!(
        any_chunk_has_noise,
        "noise_filter_enabled=false should preserve raw noise, but no chunk contains it"
    );
}

#[test]
fn chunk_entries_preserves_clean_input_unchanged() {
    // Real-world clean content should pass through with header scaffolding
    // intact and zero collateral damage.
    let clean = "Decision: extend retry budget by 30%.\n\
                 Plan: bump GLOBAL_RETRY_BUDGET in config.rs and add a regression test.\n\
                 1. Verify config loads.\n\
                 2. Verify the regression test fails on the old budget.\n\
                 3. Verify the regression test passes on the new budget.";
    let entries: Vec<TimelineEntry> = (0..20).map(|i| entry(i * 5, "assistant", clean)).collect();

    let chunks = chunk_entries(
        &entries,
        "test-project",
        "claude",
        &ChunkerConfig::default(),
    );
    assert!(!chunks.is_empty());

    for chunk in &chunks {
        assert!(
            chunk.text.contains("Decision: extend retry budget"),
            "clean semantic line dropped:\n{}",
            chunk.text
        );
        // Ordered list items must survive (regex excludes `\d+\.`).
        assert!(
            chunk.text.contains("1. Verify config loads."),
            "ordered-list item dropped — regex too aggressive:\n{}",
            chunk.text
        );
        assert!(
            chunk.text.contains("2. Verify the regression test"),
            "ordered-list item dropped — regex too aggressive:\n{}",
            chunk.text
        );
    }
}
