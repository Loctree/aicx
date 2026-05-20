use aicx::sanitize::{ContentSanitizationWarning, sanitize_chunk_content};
use aicx::sources::{
    ExtractionConfig, discover_operator_markdown, extract_operator_markdown_from_home,
};
use aicx::timeline::FrameKind;
use chrono::{TimeZone, Utc};
use filetime::{FileTime, set_file_mtime};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-operator-md-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, content).expect("write fixture");
}

fn extraction_config() -> ExtractionConfig {
    ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(),
        include_assistant: true,
        watermark: None,
    }
}

#[test]
fn operator_markdown_discovers_and_extracts_intents_decisions_tasks() {
    let root = unique_test_dir("signals");
    let home = root.join("home");
    fs::create_dir_all(home.join("Libraxis/vc-runtime/rust-memex/.git")).expect("create repo hint");

    let buglog = home
        .join("Downloads")
        .join("2026-05-01-memex-buglog-one.md");
    write_file(
        &buglog,
        r#"---
project: rust-memex
date: 2026-05-01
author: Monika
---
# rust-memex bug log

P0: Semantic search must not drop operator-written decisions.
- [ ] Wire operator-md ingest into the store path.
Decision: Keep operator-authored bug logs as first-class AICX input.
Outcome: Existing agent-only ingest missed the bug log.

## Follow-up

- P1: Dashboard should expose this source later.
"#,
    );

    let old = home.join("Downloads").join("2026-03-01-old.md");
    write_file(&old, "P0: This stale file should not be discovered.");
    set_file_mtime(&old, FileTime::from_unix_time(1, 0)).expect("set old mtime");

    let discovered = discover_operator_markdown(&home);
    assert_eq!(discovered.len(), 1, "old markdown should be ignored");

    let entries =
        extract_operator_markdown_from_home(&home, &extraction_config()).expect("extract");
    assert_eq!(entries.len(), 5);
    assert!(entries.iter().all(|entry| entry.agent == "operator"));
    assert!(
        entries
            .iter()
            .all(|entry| entry.frame_kind == Some(FrameKind::UserMsg))
    );
    assert!(entries.iter().all(|entry| {
        entry
            .cwd
            .as_deref()
            .is_some_and(|cwd| cwd.ends_with("rust-memex"))
    }));
    assert!(
        entries
            .iter()
            .any(|entry| entry.message.contains("kind: intent")
                && entry.message.contains("severity: P0")
                && entry.message.contains("Intent: [P0]"))
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.message.contains("kind: task")
                && entry.message.contains("- [ ] Wire operator-md ingest"))
    );
    assert!(entries.iter().any(|entry| {
        entry
            .message
            .contains("Decision: Keep operator-authored bug logs")
    }));
    assert!(entries.iter().any(|entry| {
        entry
            .message
            .contains("Outcome: Existing agent-only ingest")
    }));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_with_nul_byte_message_strips_and_warns() {
    let root = unique_test_dir("nul-message");
    let home = root.join("home");
    fs::create_dir_all(home.join("Loctree/aicx/.git")).expect("create repo hint");

    let note = home.join("Downloads").join("2026-05-20-nul.md");
    write_file(
        &note,
        "---\nproject: aicx\ndate: 2026-05-20\n---\n# AICX\n\nP0: strip\0 this before chunking\n",
    );

    let entries =
        extract_operator_markdown_from_home(&home, &extraction_config()).expect("extract");
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].message.contains('\0'));
    assert!(entries[0].message.contains("strip this before chunking"));

    let raw = "P0: strip\0 this before chunking";
    let sanitized = sanitize_chunk_content(raw);
    assert_eq!(
        sanitized.warnings,
        vec![ContentSanitizationWarning::NullByteStripped(9)]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_with_crlf_normalizes() {
    let root = unique_test_dir("crlf-message");
    let home = root.join("home");
    fs::create_dir_all(home.join("Loctree/aicx/.git")).expect("create repo hint");

    let note = home.join("Downloads").join("2026-05-20-crlf.md");
    write_file(
        &note,
        "---\r\nproject: aicx\r\ndate: 2026-05-20\r\n---\r\n# AICX\r\n\r\nP0: normalize\rthis message\r\n",
    );

    let entries =
        extract_operator_markdown_from_home(&home, &extraction_config()).expect("extract");
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].message.contains('\r'));
    assert!(
        entries[0]
            .message
            .contains("Intent: [P0] normalize\nthis message")
    );

    let _ = fs::remove_dir_all(&root);
}
