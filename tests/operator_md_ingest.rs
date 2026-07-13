use aicx::importers::{
    discover_operator_markdown, discover_operator_markdown_from_input,
    extract_operator_markdown_from_home, extract_operator_markdown_from_input,
};
use aicx::sanitize::{ContentSanitizationWarning, sanitize_chunk_content};
use aicx::sources::ExtractionConfig;
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
author: operator
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
fn operator_markdown_explicit_input_extracts_chatgpt_prompt_response_sections() {
    let root = unique_test_dir("explicit-chatgpt-md");
    let home = root.join("home");
    fs::create_dir_all(home.join("Libraxis/vc-runtime/screenscribe")).expect("create repo hint");

    let export = root.join("exports").join("ChatGPT-screenscribe.md");
    write_file(
        &export,
        r#"# screenscribe 1

**Created:** 5/27/2026 1:07:20
**Updated:** 6/17/2026 10:31:54

## Prompt:
hej, zapoznaj sie szczegolowo z repozytorium vetcoders/screenscribe.

## Response:
Jasne. P0 - branch ma literalne artefakty diffa w Pythonie.

## Prompt:
zrob z tego plan napraw

## Response:
Decision: naprawiamy najpierw syntax error, potem MIME.
"#,
    );
    let mtime = Utc
        .with_ymd_and_hms(2026, 6, 17, 10, 35, 41)
        .unwrap()
        .timestamp();
    set_file_mtime(&export, FileTime::from_unix_time(mtime, 0)).expect("set mtime");

    let discovered = discover_operator_markdown_from_input(&export).expect("discover input");
    assert_eq!(discovered.len(), 1);

    let config = ExtractionConfig {
        project_filter: vec!["screenscribe".to_string()],
        // cutoff before the export's Created date (5/27): oś 5 now dates the
        // conversation to Created, not the 6/17 mtime, so a 6/1 cutoff would
        // (correctly) exclude it. This test exercises section parsing, not date
        // filtering.
        cutoff: Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(),
        include_assistant: true,
        watermark: None,
    };
    let entries =
        extract_operator_markdown_from_input(&home, &export, &config).expect("extract input");

    assert_eq!(entries.len(), 4);
    // dated to Created (5/27), not the mtime (6/17)
    assert_eq!(
        entries[0].timestamp.date_naive(),
        Utc.with_ymd_and_hms(2026, 5, 27, 0, 0, 0)
            .unwrap()
            .date_naive()
    );
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[0].frame_kind, Some(FrameKind::UserMsg));
    assert!(
        entries[0]
            .message
            .contains("source_format: chatgpt-markdown")
    );
    assert!(
        entries[0]
            .message
            .contains("zapoznaj sie szczegolowo z repozytorium")
    );
    assert_eq!(entries[1].role, "assistant");
    assert_eq!(entries[1].frame_kind, Some(FrameKind::AgentReply));
    assert!(entries[1].message.contains("P0 - branch"));
    assert!(entries.iter().all(|entry| {
        entry
            .cwd
            .as_deref()
            .is_some_and(|cwd| cwd.ends_with("screenscribe"))
    }));
    assert!(
        entries
            .iter()
            .any(|entry| entry.message.contains("Decision: naprawiamy najpierw"))
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn operator_markdown_chatgpt_dates_from_created_header_not_mtime() {
    // Round II / oś 5: a ChatGPT export carries a "**Created:**" timestamp. The
    // conversation must be dated to that (its real start), not to the file mtime
    // (the download time), which previously flattened weeks-old conversations to
    // "today".
    let root = unique_test_dir("op-md-created-date");
    let home = root.join("home");
    fs::create_dir_all(&home).expect("create home");
    let export = root.join("exports").join("ChatGPT-old.md");
    write_file(
        &export,
        "# old talk\n\n**Created:** 5/27/2026 1:07:20\n**Updated:** 6/17/2026 10:31:54\n\n## Prompt:\nzbadaj temat\n\n## Response:\nDecision: robimy X\n",
    );
    // mtime = download time, far after the real conversation date
    let mtime = Utc
        .with_ymd_and_hms(2026, 6, 17, 10, 35, 41)
        .unwrap()
        .timestamp();
    set_file_mtime(&export, FileTime::from_unix_time(mtime, 0)).expect("set mtime");

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        include_assistant: true,
        watermark: None,
    };
    let entries =
        extract_operator_markdown_from_input(&home, &export, &config).expect("extract input");
    assert!(!entries.is_empty());
    for entry in &entries {
        assert_eq!(
            entry.timestamp.date_naive(),
            Utc.with_ymd_and_hms(2026, 5, 27, 0, 0, 0)
                .unwrap()
                .date_naive(),
            "ChatGPT export must be dated to Created (5/27), not mtime (6/17)"
        );
    }
}

#[test]
fn operator_markdown_emits_structural_import_provenance() {
    // Round II / oś 3+5 cut 2: every extracted entry leads with a frontmatter
    // block carrying source_file, source_format and a content-hash import_id.
    // The chunker lifts these into the sidecar (see aicx-parser chunker test
    // test_chunk_entries_extracts_foreign_import_provenance) and strips the
    // block from the chunk body. import_id is deterministic per content.
    let root = unique_test_dir("op-md-import-provenance");
    let home = root.join("home");
    fs::create_dir_all(&home).expect("create home");
    let export = root.join("exports").join("ChatGPT-provenance.md");
    write_file(
        &export,
        "## Prompt:\nzbadaj temat importu\n\n## Response:\nDecision: importujemy tylko przez operator-md\n",
    );

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries =
        extract_operator_markdown_from_input(&home, &export, &config).expect("extract input");
    assert!(!entries.is_empty());
    for entry in &entries {
        assert!(
            entry.message.starts_with("---\n"),
            "entry must lead with a frontmatter block, got: {:?}",
            entry.message.lines().next()
        );
        assert!(entry.message.contains("source_file: "));
        assert!(entry.message.contains("source_format: "));
        assert!(
            entry.message.contains("import_id: blake3:"),
            "entry must carry a blake3 content-hash import_id"
        );
    }

    // determinism: the same content yields the same import_id
    let first_import_id = |message: &str| -> String {
        message
            .lines()
            .find_map(|l| l.strip_prefix("import_id: "))
            .expect("import_id present")
            .to_string()
    };
    let entries2 =
        extract_operator_markdown_from_input(&home, &export, &config).expect("re-extract");
    assert_eq!(
        first_import_id(&entries[0].message),
        first_import_id(&entries2[0].message)
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn operator_markdown_frontmatter_overrides_foreign_markdown_import_metadata() {
    let root = unique_test_dir("frontmatter-chatgpt-md");
    let home = root.join("home");
    let repo = home.join("Git").join("ScreenScribe");
    fs::create_dir_all(&repo).expect("create repo hint");

    let export = root.join("foreign").join("note.md");
    write_file(
        &export,
        r#"---
aicx_import: 1
project: vetcoders/screen_scribe_depr
cwd: ~/Git/ScreenScribe
date: 2026-06-17
source_format: chatgpt-export
author: monika
session_id: manual-chatgpt-screenscribe
---

## Prompt:
ustal priorytety dla ScreenScribe.

## Response:
Decision: najpierw kontrakt exportu, potem UI copy.
"#,
    );

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
        include_assistant: true,
        watermark: None,
    };
    let entries =
        extract_operator_markdown_from_input(&home, &export, &config).expect("extract input");

    assert_eq!(entries.len(), 2);
    let expected_timestamps = [
        Utc.with_ymd_and_hms(2026, 6, 17, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 6, 17, 0, 0, 1).unwrap(),
    ];
    assert!(entries.iter().all(|entry| {
        entry.session_id == "manual-chatgpt-screenscribe"
            && expected_timestamps.contains(&entry.timestamp)
    }));
    assert!(entries.iter().all(|entry| {
        entry
            .cwd
            .as_deref()
            .is_some_and(|cwd| cwd == repo.to_str().unwrap())
    }));
    assert!(entries[0].message.contains("source_format: chatgpt-export"));
    assert!(
        entries[0]
            .message
            .contains("project: vetcoders/screen_scribe_depr")
    );
    assert!(entries[0].message.contains("author: monika"));
    assert!(
        entries[1]
            .message
            .contains("Decision: najpierw kontrakt exportu")
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn operator_markdown_explicit_input_without_metadata_stays_unattached() {
    let root = unique_test_dir("explicit-unattached-md");
    let home = root.join("home");
    fs::create_dir_all(home.join("Git").join("aicx")).expect("create repo hint");

    let export = root.join("foreign").join("aicx-note.md");
    write_file(
        &export,
        r#"# Loose note

P0: aicx should not attach this explicit foreign markdown without metadata.
"#,
    );

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
        include_assistant: true,
        watermark: None,
    };
    let entries =
        extract_operator_markdown_from_input(&home, &export, &config).expect("extract input");

    assert_eq!(entries.len(), 1);
    assert!(entries[0].cwd.is_none());
    assert!(entries[0].message.contains("P0"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn operator_markdown_explicit_input_falls_back_to_plain_markdown_entry() {
    let root = unique_test_dir("plain-md-input");
    let home = root.join("home");

    let export = root.join("foreign").join("architecture.md");
    write_file(
        &export,
        r#"# Architecture note

This is a normal markdown document without chat transcript headings.

It should still become one operator-md entry when imported explicitly.
"#,
    );

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
        include_assistant: true,
        watermark: None,
    };
    let entries =
        extract_operator_markdown_from_input(&home, &export, &config).expect("extract input");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[0].frame_kind, Some(FrameKind::UserMsg));
    assert!(entries[0].cwd.is_none());
    assert!(entries[0].message.contains("source_format: plain-markdown"));
    assert!(entries[0].message.contains("# Architecture note"));

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
