use super::*;
use chrono::TimeZone;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Global counter to ensure each test gets a unique directory
static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_test_dir(name: &str) -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    // FP: test-only helper; uniqueness comes from process::id() + atomic
    // counter + test name, and this path never participates in production
    // file handling.
    let dir = std::env::temp_dir() // nosemgrep: rust.lang.security.temp-dir.temp-dir -- FP: test-only unique dir uses pid + atomic counter + test name, never production file handling.
        .join(format!("ai_ctx_test_{}_{}_{}", std::process::id(), n, name));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

fn conversation_message(message: &str) -> ConversationMessage {
    ConversationMessage {
        timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "sess-redaction".to_string(),
        role: "user".to_string(),
        message: message.to_string(),
        repo_project: "test".to_string(),
        source_path: None,
        branch: None,
        message_kind: crate::timeline::MessageKind::Conversation,
        collapse_stub_kind: None,
    }
}

fn conversation_metadata(total: usize) -> ReportMetadata {
    ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 24,
        total_entries: total,
        sessions: vec!["sess-redaction".to_string()],
    }
}

fn conversation_stats(redaction_enabled: bool, messages: usize) -> ConversationExtractStats {
    ConversationExtractStats {
        aicx_version: env!("CARGO_PKG_VERSION"),
        redaction_enabled,
        raw_entries: messages,
        conversation_messages: messages,
        conversation_projection: "user_assistant_only",
        exact_short_duplicates_dropped: 0,
    }
}

fn sample_entries() -> Vec<TimelineEntry> {
    vec![
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 10, 30, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "abc12345-6789".to_string(),
            role: "user".to_string(),
            message: "Fix the build pipeline".to_string(),
            branch: Some("feat/pipeline".to_string()),
            cwd: Some("/home/project".to_string()),
            timestamp_source: None,
            frame_kind: None,
        },
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 10, 31, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "abc12345-6789".to_string(),
            role: "assistant".to_string(),
            message: "decision: We should use incremental builds".to_string(),
            branch: Some("feat/pipeline".to_string()),
            cwd: None,
            timestamp_source: None,
            frame_kind: None,
        },
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 9, 0, 0).unwrap(),
            agent: "codex".to_string(),
            session_id: "def98765-4321".to_string(),
            role: "user".to_string(),
            message: "Show me the code structure".to_string(),
            branch: None,
            cwd: None,
            timestamp_source: None,
            frame_kind: None,
        },
    ]
}

fn sample_metadata() -> ReportMetadata {
    ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 14, 0, 0).unwrap(),
        project_filter: Some("testproject".to_string()),
        hours_back: 48,
        total_entries: 3,
        sessions: vec!["abc12345-6789".to_string(), "def98765-4321".to_string()],
    }
}

// --- Rotation tests ---

#[test]
fn test_rotation_no_files() {
    let dir = unique_test_dir("rot_none");
    let deleted = rotate_outputs(&dir, "test", 5).unwrap();
    assert_eq!(deleted, 0);
    cleanup(&dir);
}

#[test]
fn test_rotation_under_limit() {
    let dir = unique_test_dir("rot_under");
    for i in 0..3 {
        fs::write(
            dir.join(format!("test_memory_2026010{}_120000.md", i)),
            "content",
        )
        .unwrap();
    }
    let deleted = rotate_outputs(&dir, "test", 5).unwrap();
    assert_eq!(deleted, 0);
    cleanup(&dir);
}

#[test]
fn test_rotation_over_limit() {
    let dir = unique_test_dir("rot_over");
    for i in 0..5 {
        fs::write(
            dir.join(format!("test_memory_2026010{}_120000.md", i)),
            "content",
        )
        .unwrap();
    }
    let deleted = rotate_outputs(&dir, "test", 2).unwrap();
    assert_eq!(deleted, 3);

    // Verify only the 2 newest remain
    let remaining: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(remaining.len(), 2);
    assert!(remaining.contains(&"test_memory_20260103_120000.md".to_string()));
    assert!(remaining.contains(&"test_memory_20260104_120000.md".to_string()));

    cleanup(&dir);
}

#[test]
fn test_rotation_mixed_extensions() {
    let dir = unique_test_dir("rot_mixed");
    for i in 0..4 {
        fs::write(
            dir.join(format!("proj_memory_2026010{}_120000.md", i)),
            "md",
        )
        .unwrap();
        fs::write(
            dir.join(format!("proj_memory_2026010{}_120000.json", i)),
            "json",
        )
        .unwrap();
    }
    // 8 files total, keep 4
    let deleted = rotate_outputs(&dir, "proj", 4).unwrap();
    assert_eq!(deleted, 4);
    cleanup(&dir);
}

#[test]
fn test_rotation_ignores_other_files() {
    let dir = unique_test_dir("rot_ignore");
    // Non-matching files
    fs::write(dir.join("other_file.md"), "keep").unwrap();
    fs::write(dir.join("README.md"), "keep").unwrap();
    // Matching files
    for i in 0..3 {
        fs::write(
            dir.join(format!("test_memory_2026010{}_120000.md", i)),
            "rotate",
        )
        .unwrap();
    }
    let deleted = rotate_outputs(&dir, "test", 1).unwrap();
    assert_eq!(deleted, 2);

    // Non-matching files still exist
    assert!(dir.join("other_file.md").exists());
    assert!(dir.join("README.md").exists());
    cleanup(&dir);
}

#[test]
fn test_rotation_zero_means_unlimited() {
    let dir = unique_test_dir("rot_zero");
    for i in 0..10 {
        fs::write(
            dir.join(format!("x_memory_2026010{}_120000.md", i)),
            "content",
        )
        .unwrap();
    }
    let deleted = rotate_outputs(&dir, "x", 0).unwrap();
    assert_eq!(deleted, 0);
    cleanup(&dir);
}

// --- NewFile mode tests ---

#[test]
fn test_new_file_mode_creates_files() {
    let dir = unique_test_dir("newfile");
    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Both,
        mode: OutputMode::NewFile,
        ..Default::default()
    };

    let entries = sample_entries();
    let metadata = sample_metadata();

    let paths = write_report(&config, &entries, &metadata).unwrap();
    assert_eq!(paths.len(), 2); // json + md

    for p in &paths {
        assert!(p.exists(), "File should exist: {}", p.display());
    }

    // Check markdown content
    let md_path = paths
        .iter()
        .find(|p| p.extension().unwrap() == "md")
        .unwrap();
    let content = fs::read_to_string(md_path).unwrap();
    assert!(content.contains("# Agent Memory Timeline"));
    assert!(content.contains("## 2026-01-22"));
    assert!(content.contains("## 2026-01-23"));
    assert!(content.contains("[Claude]"));
    assert!(content.contains("[Codex]"));

    cleanup(&dir);
}

#[test]
fn test_decision_markers() {
    let dir = unique_test_dir("decision");
    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Markdown,
        mode: OutputMode::NewFile,
        ..Default::default()
    };

    let entries = sample_entries();
    let metadata = sample_metadata();

    let paths = write_report(&config, &entries, &metadata).unwrap();
    let content = fs::read_to_string(&paths[0]).unwrap();

    // Entry with "decision:" should have pin marker (U+1F4CC)
    assert!(content.contains("\u{1f4cc}"));

    cleanup(&dir);
}

#[test]
fn test_no_truncation_by_default() {
    let dir = unique_test_dir("notrunc");
    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Markdown,
        mode: OutputMode::NewFile,
        max_message_chars: 0,
        ..Default::default()
    };

    let long_message = "x".repeat(2000);
    let entries = vec![TimelineEntry {
        timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "longsess1".to_string(),
        role: "user".to_string(),
        message: long_message.clone(),
        branch: None,
        cwd: None,
        timestamp_source: None,
        frame_kind: None,
    }];
    let metadata = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 24,
        total_entries: 1,
        sessions: vec!["longsess1".to_string()],
    };

    let paths = write_report(&config, &entries, &metadata).unwrap();
    let content = fs::read_to_string(&paths[0]).unwrap();

    assert!(content.contains(&long_message));
    assert!(!content.contains("[truncated"));

    cleanup(&dir);
}

#[test]
fn test_truncation_when_configured() {
    let dir = unique_test_dir("trunc");
    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Markdown,
        mode: OutputMode::NewFile,
        max_message_chars: 50,
        ..Default::default()
    };

    let entries = vec![TimelineEntry {
        timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "truncsess".to_string(),
        role: "user".to_string(),
        message: "a".repeat(200),
        branch: None,
        cwd: None,
        timestamp_source: None,
        frame_kind: None,
    }];
    let metadata = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 24,
        total_entries: 1,
        sessions: vec!["truncsess".to_string()],
    };

    let paths = write_report(&config, &entries, &metadata).unwrap();
    let content = fs::read_to_string(&paths[0]).unwrap();

    assert!(content.contains("[truncated at 50 chars, total 200]"));

    cleanup(&dir);
}

// --- AppendTimeline mode tests ---

#[test]
fn test_append_timeline_creates_new_file() {
    let dir = unique_test_dir("append_new");
    let timeline_path = dir.join("TIMELINE.md");

    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Markdown,
        mode: OutputMode::AppendTimeline(timeline_path.clone()),
        ..Default::default()
    };

    let entries = sample_entries();
    let metadata = sample_metadata();

    let paths = write_report(&config, &entries, &metadata).unwrap();
    assert!(timeline_path.exists());
    assert_eq!(paths.len(), 1);

    let content = fs::read_to_string(&timeline_path).unwrap();
    assert!(content.contains("# Agent Memory Timeline"));
    assert!(content.contains("## 2026-01-22"));
    // Initial sync marker should be present
    assert!(content.contains("<!-- sync: 2026-01-23T14:00:00+00:00 -->"));

    cleanup(&dir);
}

#[test]
fn test_append_timeline_deduplicates() {
    let dir = unique_test_dir("append_dedup");
    let timeline_path = dir.join("TIMELINE.md");

    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Markdown,
        mode: OutputMode::AppendTimeline(timeline_path.clone()),
        ..Default::default()
    };

    let entries = sample_entries();
    let metadata = sample_metadata();

    // First write (generated_at = 14:00, all entry timestamps < 14:00 except one at 09:00 on Jan 23)
    write_report(&config, &entries, &metadata).unwrap();

    // Second write with same entries but later generated_at
    // Since entries are at 10:30, 10:31, and 09:00 -- all before sync at 14:00
    // nothing new should be appended
    let metadata2 = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 15, 0, 0).unwrap(),
        ..sample_metadata()
    };
    write_report(&config, &entries, &metadata2).unwrap();

    let content = fs::read_to_string(&timeline_path).unwrap();
    // The initial sync marker from first write
    assert!(content.contains("<!-- sync: 2026-01-23T14:00:00+00:00 -->"));
    // Date headers should appear only once each (no duplicates)
    assert_eq!(content.matches("## 2026-01-22").count(), 1);

    cleanup(&dir);
}

#[test]
fn test_append_timeline_adds_new_entries() {
    let dir = unique_test_dir("append_add");
    let timeline_path = dir.join("TIMELINE.md");

    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Markdown,
        mode: OutputMode::AppendTimeline(timeline_path.clone()),
        ..Default::default()
    };

    // First write: entry at 10:00, generated_at at 12:00
    let entries1 = vec![TimelineEntry {
        timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 10, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "sess-aaa1".to_string(),
        role: "user".to_string(),
        message: "First entry".to_string(),
        branch: None,
        cwd: None,
        timestamp_source: None,
        frame_kind: None,
    }];
    let metadata1 = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 22, 12, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 24,
        total_entries: 1,
        sessions: vec!["sess-aaa1".to_string()],
    };
    write_report(&config, &entries1, &metadata1).unwrap();

    // Second write: includes old entry (before sync) + new entry (after sync)
    let entries2 = vec![
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 10, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "sess-aaa1".to_string(),
            role: "user".to_string(),
            message: "First entry".to_string(), // duplicate
            branch: None,
            cwd: None,
            timestamp_source: None,
            frame_kind: None,
        },
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 16, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "sess-bbb2".to_string(),
            role: "user".to_string(),
            message: "New entry after sync".to_string(),
            branch: None,
            cwd: None,
            timestamp_source: None,
            frame_kind: None,
        },
    ];
    let metadata2 = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 17, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 48,
        total_entries: 2,
        sessions: vec!["sess-aaa1".to_string(), "sess-bbb2".to_string()],
    };
    write_report(&config, &entries2, &metadata2).unwrap();

    let content = fs::read_to_string(&timeline_path).unwrap();
    // First entry should appear exactly once (not duplicated)
    assert_eq!(content.matches("First entry").count(), 1);
    // New entry should be present
    assert!(content.contains("New entry after sync"));
    // Second sync marker
    assert!(content.contains("<!-- sync: 2026-01-23T17:00:00+00:00 -->"));

    cleanup(&dir);
}

// --- Code block preservation ---

#[test]
fn test_code_blocks_preserved() {
    let dir = unique_test_dir("codeblocks");
    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Markdown,
        mode: OutputMode::NewFile,
        ..Default::default()
    };

    let msg = "Here's the fix:\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\nDone.";
    let entries = vec![TimelineEntry {
        timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "codetst1".to_string(),
        role: "assistant".to_string(),
        message: msg.to_string(),
        branch: None,
        cwd: None,
        timestamp_source: None,
        frame_kind: None,
    }];
    let metadata = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 24,
        total_entries: 1,
        sessions: vec!["codetst1".to_string()],
    };

    let paths = write_report(&config, &entries, &metadata).unwrap();
    let content = fs::read_to_string(&paths[0]).unwrap();

    // Code block should be intact (not prefixed with >)
    assert!(content.contains("```rust"));
    assert!(content.contains("fn main()"));
    assert!(content.contains("println!"));
    // Should use HTML blockquote for code-containing messages
    assert!(content.contains("<blockquote>"));
    assert!(content.contains("</blockquote>"));

    cleanup(&dir);
}

// --- Decision keyword detection ---

#[test]
fn test_is_decision_message_positive() {
    assert!(is_decision_message("decision: use incremental builds"));
    assert!(is_decision_message("The plan: refactor everything"));
    assert!(is_decision_message("New architecture proposal"));
    assert!(is_decision_message("WAŻNE: to jest krytyczne"));
    assert!(is_decision_message("KEY insight here"));
    assert!(is_decision_message("TODO: fix this later"));
    assert!(is_decision_message("FIXME: broken"));
    assert!(is_decision_message("BREAKING change in API"));
}

#[test]
fn test_is_decision_message_negative() {
    assert!(!is_decision_message("Just a regular message"));
    assert!(!is_decision_message("nothing special here"));
    assert!(!is_decision_message("the key to success")); // lowercase "key" should not match
}

// --- JSON output ---

#[test]
fn test_json_output() {
    let dir = unique_test_dir("json");
    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Json,
        mode: OutputMode::NewFile,
        ..Default::default()
    };

    let entries = sample_entries();
    let metadata = sample_metadata();

    let paths = write_report(&config, &entries, &metadata).unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].extension().unwrap(), "json");

    let content = fs::read_to_string(&paths[0]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["total_entries"], 3);
    assert_eq!(parsed["entries"].as_array().unwrap().len(), 3);

    cleanup(&dir);
}

#[test]
fn test_conversation_json_extract_stats_are_additive() {
    let dir = unique_test_dir("conversation_extract_stats");
    let path = dir.join("conversation.json");
    let messages = vec![ConversationMessage {
        timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "sess-stats".to_string(),
        role: "user".to_string(),
        message: "Hello".to_string(),
        repo_project: "test".to_string(),
        source_path: None,
        branch: None,
        message_kind: crate::timeline::MessageKind::Conversation,
        collapse_stub_kind: None,
    }];
    let metadata = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 24,
        total_entries: 2,
        sessions: vec!["sess-stats".to_string()],
    };
    let stats = ConversationExtractStats {
        aicx_version: env!("CARGO_PKG_VERSION"),
        redaction_enabled: true,
        raw_entries: 2,
        conversation_messages: messages.len(),
        conversation_projection: "user_assistant_only",
        exact_short_duplicates_dropped: 1,
    };

    write_conversation_json(&path, &messages, &metadata, &stats).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        parsed["extract_stats"]["aicx_version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(parsed["extract_stats"]["redaction_enabled"], true);
    assert_eq!(parsed["extract_stats"]["raw_entries"], 2);
    assert_eq!(parsed["extract_stats"]["conversation_messages"], 1);
    assert_eq!(
        parsed["extract_stats"]["conversation_projection"],
        "user_assistant_only"
    );
    assert_eq!(parsed["extract_stats"]["exact_short_duplicates_dropped"], 1);
    assert_eq!(
        parsed["extract_stats"]["conversation_messages"],
        parsed["messages"].as_array().unwrap().len()
    );
    assert!(
        parsed["extract_stats"]["raw_entries"].as_u64().unwrap()
            >= parsed["extract_stats"]["conversation_messages"]
                .as_u64()
                .unwrap()
    );

    cleanup(&dir);
}

#[test]
fn test_conversation_json_extract_stats_can_report_redaction_disabled() {
    let dir = unique_test_dir("conversation_extract_stats_redaction_disabled");
    let path = dir.join("conversation.json");
    let messages = vec![ConversationMessage {
        timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "sess-stats".to_string(),
        role: "user".to_string(),
        message: "Hello".to_string(),
        repo_project: "test".to_string(),
        source_path: None,
        branch: None,
        message_kind: crate::timeline::MessageKind::Conversation,
        collapse_stub_kind: None,
    }];
    let metadata = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 24,
        total_entries: 1,
        sessions: vec!["sess-stats".to_string()],
    };
    let stats = ConversationExtractStats {
        aicx_version: env!("CARGO_PKG_VERSION"),
        redaction_enabled: false,
        raw_entries: 1,
        conversation_messages: messages.len(),
        conversation_projection: "user_assistant_only",
        exact_short_duplicates_dropped: 0,
    };

    write_conversation_json(&path, &messages, &metadata, &stats).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["extract_stats"]["redaction_enabled"], false);

    cleanup(&dir);
}

#[test]
fn write_conversation_outputs_redact_by_default() {
    let dir = unique_test_dir("conversation_redact_default");
    let md_path = dir.join("conversation.md");
    let json_path = dir.join("conversation.json");
    let raw = "raw token sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX should not leak";
    let messages = vec![conversation_message(raw)];
    let metadata = conversation_metadata(messages.len());
    let stats = conversation_stats(true, messages.len());

    write_conversation_markdown(&md_path, &messages, &metadata).unwrap();
    write_conversation_json(&json_path, &messages, &metadata, &stats).unwrap();

    let md = fs::read_to_string(&md_path).unwrap();
    let json = fs::read_to_string(&json_path).unwrap();
    assert!(!md.contains("sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX"));
    assert!(!json.contains("sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX"));
    assert!(md.contains("[REDACTED_ANTHROPIC_KEY]"));
    assert!(json.contains("[REDACTED_ANTHROPIC_KEY]"));

    cleanup(&dir);
}

#[test]
fn write_conversation_outputs_can_preserve_raw_with_explicit_flag() {
    let dir = unique_test_dir("conversation_redact_opt_out");
    let md_path = dir.join("conversation.md");
    let json_path = dir.join("conversation.json");
    let raw = "raw token sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX preserved";
    let messages = vec![conversation_message(raw)];
    let metadata = conversation_metadata(messages.len());
    let stats = conversation_stats(false, messages.len());

    write_conversation_markdown_with_redaction(&md_path, &messages, &metadata, false).unwrap();
    write_conversation_json_with_redaction(&json_path, &messages, &metadata, &stats, false)
        .unwrap();

    let md = fs::read_to_string(&md_path).unwrap();
    let json = fs::read_to_string(&json_path).unwrap();
    assert!(md.contains("sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX"));
    assert!(json.contains("sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX"));

    cleanup(&dir);
}

// --- Loctree ---

#[test]
fn test_loctree_snapshot_missing_binary() {
    let dir = unique_test_dir("loctree_missing_bin");
    std::fs::create_dir_all(&dir).unwrap();
    // loct may or may not be on PATH -- either way this should not panic
    let result = capture_loctree_snapshot(&dir).unwrap();
    let _ = result; // Just verify it doesn't error
    cleanup(&dir);
}

// --- Config defaults ---

#[test]
fn test_output_config_default() {
    let config = OutputConfig::default();
    assert_eq!(config.max_files, 0);
    assert_eq!(config.max_message_chars, 0);
    assert!(!config.include_loctree);
    assert_eq!(config.format, OutputFormat::Both);
}

// --- Multiline message formatting ---

#[test]
fn test_multiline_without_code_uses_blockquote_lines() {
    let dir = unique_test_dir("multiline");
    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Markdown,
        mode: OutputMode::NewFile,
        ..Default::default()
    };

    let entries = vec![TimelineEntry {
        timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "multisss".to_string(),
        role: "user".to_string(),
        message: "Line one\nLine two\nLine three".to_string(),
        branch: None,
        cwd: None,
        timestamp_source: None,
        frame_kind: None,
    }];
    let metadata = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 24,
        total_entries: 1,
        sessions: vec!["multisss".to_string()],
    };

    let paths = write_report(&config, &entries, &metadata).unwrap();
    let content = fs::read_to_string(&paths[0]).unwrap();

    assert!(content.contains("> Line one\n> Line two\n> Line three"));
    // Should NOT use HTML blockquote (no code blocks)
    assert!(!content.contains("<blockquote>"));

    cleanup(&dir);
}

// --- Markdown safety policy (Area C P2) ---

fn render_message_markdown(msg: &str) -> String {
    let mut buf: Vec<u8> = Vec::new();
    write_formatted_message(&mut buf, msg).unwrap();
    String::from_utf8(buf).unwrap()
}

#[test]
fn test_html_escape_neutralizes_script_payload() {
    let out = render_message_markdown("<script>alert(1)</script>");
    // Script payload becomes inert text — no live `<script>` tag survives.
    assert!(out.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!out.contains("<script>"));
    assert!(!out.contains("</script>"));

    // Same protection holds for multi-line plain text.
    let multi = render_message_markdown("line a\n<img src=x onerror=alert(1)>\nline b");
    assert!(multi.contains("&lt;img src=x onerror=alert(1)&gt;"));
    assert!(!multi.contains("<img"));
}

#[test]
fn test_stray_triple_backtick_does_not_break_out() {
    // Message with a stray triple-backtick inside a code-bearing body must stay
    // contained — the outer fence must be longer than any inner backtick run.
    let msg = "intro\n```\nstray fence content\n```\ntrailing";
    let out = render_message_markdown(msg);
    assert!(out.contains("<blockquote>"));
    assert!(out.contains("</blockquote>"));
    // Outer fence must be strictly longer than the longest inner run (3 → 4).
    assert!(out.contains("````\n"));
    // The literal inner backticks still appear as text inside the wrapping fence.
    assert!(out.contains("stray fence content"));
    // Trailing context survives in the same artifact (proves no runaway block).
    assert!(out.contains("trailing"));
}

#[test]
fn test_link_injection_does_not_become_active_link() {
    // Markdown link-injection payload must surface as literal text in the artifact;
    // we never synthesize an `<a>` tag, and structural delimiters do not survive.
    let payload = "before ]([http://attacker.example/](http://attacker.example/)) after";
    let out = render_message_markdown(payload);
    // No HTML anchor tag was generated by the writer.
    assert!(!out.contains("<a "));
    assert!(!out.contains("</a>"));
    // The literal URL is present as text (proves we did not strip or fetch).
    assert!(out.contains("http://attacker.example/"));
    // The line still starts with a markdown blockquote prefix — structure intact.
    assert!(out.contains("> before "));
}

#[test]
fn test_crlf_normalized_to_lf() {
    let out = render_message_markdown("first\r\nsecond\rthird");
    // Output never carries CR back through.
    assert!(!out.contains('\r'));
    // All three lines surface as ordinary blockquote lines.
    assert!(out.contains("> first"));
    assert!(out.contains("> second"));
    assert!(out.contains("> third"));
}

#[test]
fn test_dynamic_fence_avoids_collision() {
    // Helper-level invariants first: fence must exceed the longest internal run.
    assert_eq!(dynamic_fence_for("no backticks here"), "```");
    assert_eq!(dynamic_fence_for("```"), "````");
    assert_eq!(dynamic_fence_for("````"), "`````");
    assert_eq!(dynamic_fence_for("``a```b``"), "````");

    // End-to-end: a body containing 4 backticks must be wrapped with 5+.
    let msg = "before\n````\nfour-tick fence\n````\nafter";
    let out = render_message_markdown(msg);
    assert!(out.contains("`````\n"));
    assert!(!out.contains("``````\n")); // exactly one more than the inner run
}

// --- JSON regression (Area C P2 — confirm serde_json is RFC-compliant) ---

fn json_roundtrip_entry(msg: &str) -> serde_json::Value {
    let dir = unique_test_dir("json_regression");
    let config = OutputConfig {
        dir: dir.clone(),
        format: OutputFormat::Json,
        mode: OutputMode::NewFile,
        ..Default::default()
    };
    let entries = vec![TimelineEntry {
        timestamp: Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "json-regr".to_string(),
        role: "user".to_string(),
        message: msg.to_string(),
        branch: None,
        cwd: None,
        timestamp_source: None,
        frame_kind: None,
    }];
    let metadata = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 5, 20, 13, 0, 0).unwrap(),
        project_filter: Some("regr".to_string()),
        hours_back: 1,
        total_entries: 1,
        sessions: vec!["json-regr".to_string()],
    };
    let paths = write_report(&config, &entries, &metadata).unwrap();
    let content = fs::read_to_string(&paths[0]).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    cleanup(&dir);
    parsed
}

#[test]
fn test_json_escapes_control_chars() {
    // \u{0001} (SOH), \u{0008} (backspace), \u{000c} (form feed), \t, \n, \r
    let msg = "ctrl\u{0001}\u{0008}\u{000c}\t\n\rend";
    let parsed = json_roundtrip_entry(msg);
    let recovered = parsed["entries"][0]["message"].as_str().unwrap();
    assert_eq!(recovered, msg, "control chars must round-trip losslessly");
}

#[test]
fn test_json_handles_bom_in_message() {
    let msg = "\u{feff}body after BOM";
    let parsed = json_roundtrip_entry(msg);
    let recovered = parsed["entries"][0]["message"].as_str().unwrap();
    assert_eq!(recovered, msg, "U+FEFF must survive JSON round-trip");
    // Top-level JSON must be parseable (already proven by from_str success above).
    assert!(parsed.is_object());
}

#[test]
fn test_json_invalid_input_rejected_upstream() {
    // Unpaired surrogates cannot exist in a Rust `String` — the type system
    // already enforces well-formed UTF-8 / UTF-16-safe scalars. Confirm both
    // sides of the contract: (1) raw bytes containing an unpaired surrogate
    // fail at parse time, (2) any `String` we hand to serde_json round-trips.
    // Build at runtime so the compile-time `invalid_from_utf8` lint stays quiet.
    let bad_bytes: Vec<u8> = vec![0xEDu8, 0xA0u8, 0x80u8]; // UTF-8 of U+D800
    assert!(
        std::str::from_utf8(&bad_bytes).is_err(),
        "unpaired surrogate must fail UTF-8 parse before reaching the writer"
    );

    // Sanity: every valid scalar (including high BMP and supplementary planes)
    // survives the writer, proving the rejection is purely upstream.
    let msg = "supp \u{1f4cc} bmp \u{2603} done";
    let parsed = json_roundtrip_entry(msg);
    let recovered = parsed["entries"][0]["message"].as_str().unwrap();
    assert_eq!(recovered, msg);
}

// --- strip_footer tail-scan + atomic rewrite (Area C P3.3) ---

#[test]
fn test_strip_footer_small_file_works() {
    let dir = unique_test_dir("strip_small");
    let path = dir.join("timeline.md");
    let original = "head\n---\n*Generated by ai-contexters v1.0*\n";
    fs::write(&path, original).unwrap();

    strip_footer(&path).unwrap();

    let got = fs::read_to_string(&path).unwrap();
    assert_eq!(got, "head\n");
    cleanup(&dir);
}

#[test]
fn test_strip_footer_no_marker_leaves_file_intact() {
    let dir = unique_test_dir("strip_no_marker");
    let path = dir.join("timeline.md");
    let original = "no footer here\njust some content\n";
    fs::write(&path, original).unwrap();
    let before = fs::read(&path).unwrap();

    strip_footer(&path).unwrap();

    let after = fs::read(&path).unwrap();
    assert_eq!(
        before, after,
        "file must be byte-identical when marker absent"
    );
    cleanup(&dir);
}

#[test]
fn test_strip_footer_marker_at_very_end_works() {
    let dir = unique_test_dir("strip_end");
    let path = dir.join("timeline.md");
    // ~10 KiB of body so the marker really lives in the last ~100 bytes
    // and tail-scan has to find it via the absolute offset math.
    let body: String = "x".repeat(10 * 1024);
    let original = format!("{body}\n---\n*Generated by ai-contexters v9.9.9*\n");
    fs::write(&path, &original).unwrap();

    strip_footer(&path).unwrap();

    let got = fs::read_to_string(&path).unwrap();
    let expected = format!("{body}\n");
    assert_eq!(got, expected);
    cleanup(&dir);
}

#[test]
fn test_strip_footer_marker_far_from_end_non_destructive() {
    let dir = unique_test_dir("strip_far");
    let path = dir.join("timeline.md");
    // Place the marker in the first 10 KiB and pad the file out to ~2 MiB
    // so both the 64 KiB and 1 MiB tail windows miss it. Non-destructive
    // path must leave the file byte-identical and just log a warning.
    let mut content = String::from("head\n---\n*Generated by ai-contexters stray*\n");
    content.push_str(&"y".repeat(2 * 1024 * 1024));
    fs::write(&path, &content).unwrap();
    let before = fs::read(&path).unwrap();

    strip_footer(&path).unwrap();

    let after = fs::read(&path).unwrap();
    assert_eq!(
        before, after,
        "marker outside both tail windows must leave file intact"
    );
    cleanup(&dir);
}

#[test]
fn test_strip_footer_does_not_load_full_file_to_memory() {
    // Build a sparse-ish ~3 MiB file with the marker in the last 200 bytes.
    // The previous fs::read_to_string implementation would load the whole
    // thing; the new tail-scan + chunked-copy path keeps memory bounded.
    // We assert via behavior (correct trim) rather than mocking allocator —
    // this is the integration check called out as optional in the brief.
    let dir = unique_test_dir("strip_large");
    let path = dir.join("timeline.md");
    let body: String = "z".repeat(3 * 1024 * 1024);
    let original = format!("{body}\n---\n*Generated by ai-contexters tail-pin*\n");
    fs::write(&path, &original).unwrap();
    let original_size = fs::metadata(&path).unwrap().len();

    strip_footer(&path).unwrap();

    let after_size = fs::metadata(&path).unwrap().len();
    assert!(
        after_size < original_size,
        "file must shrink after strip ({after_size} < {original_size})"
    );
    // Read just the tail of the result to confirm marker is gone without
    // pulling the whole file into the test process either.
    let mut f = fs::File::open(&path).unwrap();
    f.seek(SeekFrom::End(-(STRIP_FOOTER_MARKER.len() as i64) - 16))
        .unwrap();
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).unwrap();
    assert!(
        rfind_subslice(&tail, STRIP_FOOTER_MARKER).is_none(),
        "marker must be absent from the trimmed file's tail"
    );
    cleanup(&dir);
}

#[test]
fn test_find_last_sync_timestamp_skips_oversized_line_and_advances() {
    let dir = unique_test_dir("sync_oversized");
    let path = dir.join("timeline.md");
    let expected = "2026-05-22T03:00:00Z";
    let mut content = "x".repeat(crate::sanitize::MAX_VALIDATED_BYTES + 1);
    content.push('\n');
    content.push_str(&format!("<!-- sync: {expected} -->\n"));
    fs::write(&path, content).unwrap();

    let found = find_last_sync_timestamp(&path).unwrap();
    let expected = chrono::DateTime::parse_from_rfc3339(expected)
        .unwrap()
        .with_timezone(&Utc);

    assert_eq!(found.unwrap(), expected);
    cleanup(&dir);
}
