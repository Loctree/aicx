use super::decision::is_decision_message;
use super::report::{
    STRIP_FOOTER_MARKER, dynamic_fence_for, find_last_sync_timestamp, rfind_subslice, strip_footer,
    write_formatted_message,
};
use super::*;
use chrono::{TimeZone, Utc};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Global counter to ensure each test gets a unique directory
static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_test_dir(name: &str) -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    // Test-only scratch under target/ — not std::env::temp_dir() (avoids
    // temp-dir policy noise; production code never calls this helper).
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-tmp")
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
        harness_noise_dropped: 0,
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
            cwd: Some("/home/user".to_string()),
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
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
            source_path: None,
            source_sha256: None,
            source_line_span: None,
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
            source_path: None,
            source_sha256: None,
            source_line_span: None,
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

mod conversation;
mod report;
