use super::*;
use crate::oracle::OracleStatus;
use crate::sources::shared::{IntentLineModality, intent_line_modality};
use filetime::{FileTime, set_file_mtime};
use std::fs;
use std::path::PathBuf;

fn chunk_path(root: &Path, project: &str, date: &str, name: &str) -> PathBuf {
    let date_compact = crate::store::compact_date(date);
    let agent = if name.contains("_claude") || name.contains("claude") {
        "claude"
    } else if name.contains("_gemini") || name.contains("gemini") {
        "gemini"
    } else {
        "codex"
    };
    let sequence = name
        .trim_end_matches(".md")
        .rsplit_once('-')
        .and_then(|(_, tail)| tail.parse::<u32>().ok())
        .unwrap_or(1);
    let basename = crate::store::session_basename(date, agent, "intentstest01", sequence);
    let dir = root
        .join("store")
        .join("local")
        .join(project)
        .join(date_compact)
        .join("conversations")
        .join(agent);
    fs::create_dir_all(&dir).expect("create chunk dir");
    dir.join(basename)
}

fn write_chunk(root: &Path, project: &str, date: &str, name: &str, body: &str) {
    write_chunk_with_sidecar(root, project, date, name, body, Some(FrameKind::UserMsg));
}

fn migration_test_root(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("unix time")
        .as_nanos();
    std::env::temp_dir().join(format!("aicx-intents-{label}-{nanos}"))
}

fn extract_demo_extraction(label: &str, body: &str) -> IntentExtraction {
    let root = migration_test_root(label);
    let _ = fs::remove_dir_all(&root);

    write_chunk(&root, "demo", "2026-03-15", "120000_codex-001.md", body);

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let extraction =
        extract_intents_from_root_at_with_stats(&config, &root, now).expect("extract intents");
    let _ = fs::remove_dir_all(root);
    extraction
}

fn extract_demo_records(label: &str, body: &str) -> Vec<IntentRecord> {
    extract_demo_extraction(label, body).records
}

#[test]
fn local_command_artifacts_do_not_become_intents() {
    let root = migration_test_root("local-command-artifact-intent");
    let _ = fs::remove_dir_all(&root);
    write_chunk(
        &root,
        "demo",
        "2026-03-15",
        "120000_codex-001.md",
        r#"[project: demo | agent: claude | date: 2026-03-15 | frame_kind: user_msg]

[signals]
Intent:
- Next steps:
- *  issuer: C=US; O=Let's Encrypt; CN=E7
[/signals]

[12:00:00] user: <local-command-caveat>DO NOT respond to these messages</local-command-caveat>
[12:00:00] user: <bash-stdout>curl output
* subjectAltName: host "api.libraxis.cloud" matched cert's "api.libraxis.cloud"
* issuer: C=US; O=Let's Encrypt; CN=E7
* SSL certificate verify ok.
</bash-stdout>
"#,
    );

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: Some(IntentKind::Intent),
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );
    let records = extract_intents_from_root_at(&config, &root, now).expect("extract intents");
    assert!(
        records.is_empty(),
        "local command artifacts leaked: {records:?}"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn pasted_skill_doc_outline_does_not_become_intent() {
    let root = migration_test_root("pasted-skill-doc-outline");
    let _ = fs::remove_dir_all(&root);
    write_chunk(
        &root,
        "demo",
        "2026-03-15",
        "120000_claude-001.md",
        r#"[project: demo | agent: claude | date: 2026-03-15 | frame_kind: user_msg]

[signals]
Intent:
- 10. Use `cron` to keep heartbeat and schedule the next step when the session
- Input: "You drive. I want this local AI stack to feel production-ready."
Notes:
- Base directory for this skill: /Users/maciejgad/.claude/skills/vc-ownership
[/signals]

[12:00:00] user: Base directory for this skill: /Users/maciejgad/.claude/skills/vc-ownership

# vc-ownership

10. Use `cron` to keep heartbeat and schedule the next step when the session
"#,
    );

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: Some(IntentKind::Intent),
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );
    let records = extract_intents_from_root_at(&config, &root, now).expect("extract intents");
    assert!(records.is_empty(), "skill doc outline leaked: {records:?}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn charter_block_does_not_become_intent_or_outcome() {
    let records = extract_demo_records(
        "charter-block-filter",
        r#"[project: demo | agent: codex | date: 2026-03-15 | frame_kind: user_msg]

[12:00:00] user: # AGENTS.md instructions for /Users/maciejgad/vc-workspace/vetcoders/aicx
<INSTRUCTIONS>
<!-- loctree-doctrine: v1 -->
## **LOCTREE + AICX + VIBECRAFTED — ZŁOTE RUNO**
Done Is A Market Condition
Code is not done because a narrow check turned green
Product truth beats local elegance.
</INSTRUCTIONS>
[12:01:00] user: Task: keep the real user directive visible after charter
"#,
    );

    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Task
                && record
                    .summary
                    .contains("keep the real user directive visible after charter")
        }),
        "real user directive after charter was not preserved: {records:?}",
    );
    assert!(
        !records.iter().any(|record| {
            record.summary.contains("Done Is A Market Condition")
                || record
                    .summary
                    .contains("Code is not done because a narrow check turned green")
                || record
                    .summary
                    .contains("Product truth beats local elegance")
        }),
        "charter block leaked intent/outcome records: {records:?}",
    );
}

#[test]
fn repeated_charter_signal_lines_do_not_multiply_across_sessions() {
    let tmp = migration_test_root("charter-signal-dedup");
    let _ = fs::remove_dir_all(&tmp);

    let body = r#"[project: demo | agent: codex | date: 2026-03-15 | frame_kind: user_msg]

[signals]
Intent:
- Done Is A Market Condition
- Code is not done because a narrow check turned green
[/signals]

[12:00:00] user: Done Is A Market Condition
"#;

    write_chunk_with_session(
        &tmp,
        "demo",
        "2026-03-15",
        "codex",
        "charter-session-a",
        1,
        body,
    );
    write_chunk_with_session(
        &tmp,
        "demo",
        "2026-03-15",
        "codex",
        "charter-session-b",
        2,
        body,
    );

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");
    let charter_count = records
        .iter()
        .filter(|record| {
            record.summary.contains("Done Is A Market Condition")
                || record
                    .summary
                    .contains("Code is not done because a narrow check turned green")
        })
        .count();
    assert!(
        charter_count <= 1,
        "charter line multiplied across sessions: {records:?}",
    );
    assert_eq!(
        charter_count, 0,
        "charter doctrine should be filtered before candidate promotion: {records:?}",
    );

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn collapse_session_merges_exact_daily_duplicates_across_session_forks() {
    let make_record = |session_id: &str, source_chunk: &str| IntentRecord {
        kind: IntentKind::Intent,
        summary: "przerobimy ScreenScribe na portal".to_string(),
        context: None,
        evidence: vec![],
        project: "VetCoders/ScreenScribe".to_string(),
        agent: "codex".to_string(),
        date: "2026-05-31".to_string(),
        timestamp: None,
        session_id: session_id.to_string(),
        count: None,
        first_chunk: None,
        last_chunk: None,
        source_chunk: source_chunk.to_string(),
        source: None,
    };

    let records = vec![
        make_record("fork-a", "a.md"),
        make_record("fork-b", "b.md"),
        make_record("fork-c", "c.md"),
    ];
    let collapsed = apply_display_filters(
        records,
        &IntentDisplayFilters {
            collapse_session: true,
            ..Default::default()
        },
    );

    assert_eq!(collapsed.len(), 1);
    assert_eq!(collapsed[0].count, Some(3));
    assert!(collapsed[0].source_chunk.contains("a.md"));
    assert!(collapsed[0].source_chunk.contains("b.md"));
    assert!(collapsed[0].source_chunk.contains("c.md"));
}

#[test]
fn collapse_session_tolerates_existing_none_count() {
    let make_record = |summary: &str, count| IntentRecord {
        kind: IntentKind::Intent,
        summary: summary.to_string(),
        context: None,
        evidence: vec![],
        project: "VetCoders/ScreenScribe".to_string(),
        agent: "codex".to_string(),
        date: "2026-05-31".to_string(),
        timestamp: None,
        session_id: "same-session".to_string(),
        count,
        first_chunk: None,
        last_chunk: None,
        source_chunk: format!("{summary}.md"),
        source: None,
    };

    let collapsed = apply_display_filters(
        vec![make_record("first", None), make_record("second", Some(2))],
        &IntentDisplayFilters {
            collapse_session: true,
            ..Default::default()
        },
    );

    assert_eq!(collapsed.len(), 1);
    assert_eq!(collapsed[0].count, Some(3));
}

#[test]
fn migrate_intent_schema_dry_run_at_scans_all_projects_without_filter() {
    let root = migration_test_root("intent-migration-all-projects");
    write_chunk(
        &root,
        "alpha",
        "2026-04-14",
        "093000_claude-001.md",
        "[09:30:00] assistant: result: alpha migration passed\n",
    );
    write_chunk(
        &root,
        "beta",
        "2026-04-15",
        "101500_codex-001.md",
        "[10:15:00] user: question: should beta keep legacy links?\n",
    );

    let report = migrate_intent_schema_dry_run_at(&root.join("store"), None)
        .expect("global migration dry run should work");

    assert_eq!(report.total_chunks, 2);
    assert_eq!(report.entries_found, 2);
    assert_eq!(report.per_project.get("alpha"), Some(&1));
    assert_eq!(report.per_project.get("beta"), Some(&1));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn head_blockquote_reference_is_pasted_reference_not_intent() {
    let line = "> intent: Let's ship the mirrored roadmap";
    assert_eq!(
        intent_line_modality("user", line),
        IntentLineModality::PastedReference
    );

    let records = extract_demo_records(
        "blockquote-reference-modality",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:00:00] user: > intent: Let's ship the mirrored roadmap\n",
    );

    assert!(
        !records
            .iter()
            .any(|record| record.kind == IntentKind::Intent),
        "head blockquote reference leaked intent records: {records:?}"
    );
}

#[test]
fn pasted_text_placeholder_reference_is_pasted_reference_not_intent() {
    let line = "[Pasted text #1 +9 lines] Let's ship the mirrored roadmap";
    assert_eq!(
        intent_line_modality("user", line),
        IntentLineModality::PastedReference
    );

    let records = extract_demo_records(
        "placeholder-reference-modality",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:00:00] user: [Pasted text #1 +9 lines] Let's ship the mirrored roadmap\n",
    );

    assert!(
        !records
            .iter()
            .any(|record| record.kind == IntentKind::Intent),
        "pasted-text placeholder leaked intent records: {records:?}"
    );
}

#[test]
fn zadanie_head_is_typed_directive_and_becomes_task() {
    let line = "Zadanie: dopnij pasted-vs-typed modality gate";
    assert_eq!(
        intent_line_modality("user", line),
        IntentLineModality::TypedDirective
    );

    let records = extract_demo_records(
        "zadanie-typed-directive-modality",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:00:00] user: Zadanie: dopnij pasted-vs-typed modality gate\n",
    );

    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Task
                && record
                    .summary
                    .contains("Zadanie: dopnij pasted-vs-typed modality gate")
        }),
        "typed Zadanie directive did not produce a task record: {records:?}"
    );
}

#[test]
fn user_question_and_why_lines_bridge_into_main_intents_view() {
    let extraction = extract_demo_extraction(
        "md-radar-question-why-bridge",
        "[project: demo | agent: codex | date: 2026-03-15 | frame_kind: user_msg]\n\n\
         [12:00:00] user: Question: should md-radar keep the current storage model?\n\
         [12:01:00] user: why: human intent must stay visible before agent outcomes\n\
         [12:02:00] user: Task: decide whether md-radar keeps current storage before migration work\n",
    );

    assert!(
        extraction.stats.candidate_count > 0,
        "md-radar fixture produced no candidates: {extraction:?}"
    );
    let records = extraction.records;
    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Intent
                && record.summary == "should md-radar keep the current storage model?"
        }),
        "user question did not surface as Lane 1 intent: {records:?}"
    );
    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Intent
                && record.summary == "human intent must stay visible before agent outcomes"
        }),
        "user why-line did not surface as Lane 1 intent: {records:?}"
    );
}

#[test]
fn md_radar_style_user_messages_surface_in_main_intents_view() {
    let extraction = extract_demo_extraction(
        "md-radar-natural-human-lines",
        "[project: m-szymanska/md-radar | agent: codex | date: 2026-06-15 | frame_kind: user_msg]\n\n\
         [12:00:00] user: Proszę odpal /vc-init na tym repo i ustal, gdzie zaczęła się wcześniejsza sesja.\n\
         [12:01:00] user: Czy AICX umie wyciągać intents z JSONL?\n\
         [12:02:00] user: Usuń hardkody i ścieżki z README, bo to ma być gotowe dla świeżego repo.\n",
    );

    assert!(
        extraction.stats.scanned_count == 1,
        "md-radar fixture should scan one user_msg chunk: {extraction:?}"
    );
    assert!(
        extraction.stats.candidate_count >= 3,
        "md-radar-style user messages produced no candidates: {extraction:?}"
    );

    let records = extraction.records;
    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Intent
                && record.summary.contains("Proszę odpal /vc-init na tym repo")
        }),
        "human /vc-init request disappeared from Lane 1: {records:?}"
    );
    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Intent
                && record
                    .summary
                    .contains("Czy AICX umie wyciągać intents z JSONL?")
        }),
        "human question disappeared from Lane 1: {records:?}"
    );
    assert!(
        records.iter().any(|record| {
            matches!(record.kind, IntentKind::Intent | IntentKind::Decision)
                && record.summary.contains("Usuń hardkody i ścieżki z README")
        }),
        "human cleanup request disappeared from Lane 1: {records:?}"
    );
}

#[test]
fn user_comment_after_local_command_output_still_surfaces() {
    let records = extract_demo_records(
        "mixed-command-output-human-comment",
        "[project: demo | agent: codex | date: 2026-03-15 | frame_kind: user_msg]\n\n\
         [12:00:00] user: <local-command-caveat>DO NOT respond to these messages</local-command-caveat>\n\
         <bash-stdout>curl output\n\
         * issuer: C=US; O=Let's Encrypt; CN=E7\n\
         * SSL certificate verify ok.\n\
         </bash-stdout>\n\
         Question: should md-radar keep the current storage model after this evidence?\n",
    );

    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Intent
                && record.summary
                    == "should md-radar keep the current storage model after this evidence?"
        }),
        "human question after command output was dropped with the artifact entry: {records:?}"
    );
    assert!(
        !records
            .iter()
            .any(|record| record.summary.contains("Let's Encrypt")),
        "local command artifact leaked into intent records: {records:?}"
    );
}

#[test]
fn typed_directive_with_reference_marker_mid_body_remains_task() {
    let line =
        "Zadanie: analyze this pasted reference > intent: old plan [Pasted text #2 +4 lines]";
    assert_eq!(
        intent_line_modality("user", line),
        IntentLineModality::TypedDirective
    );

    let records = extract_demo_records(
        "mid-body-reference-marker-modality",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:00:00] user: Zadanie: analyze this pasted reference > intent: old plan [Pasted text #2 +4 lines]\n",
    );

    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Task
                && record
                    .summary
                    .contains("Zadanie: analyze this pasted reference")
        }),
        "typed directive with mid-body reference markers was not preserved: {records:?}"
    );
}

fn write_chunk_with_sidecar(
    root: &Path,
    project: &str,
    date: &str,
    name: &str,
    body: &str,
    frame_kind: Option<FrameKind>,
) {
    let path = chunk_path(root, project, date, name);
    fs::write(&path, body).expect("write chunk");
    let agent = if name.contains("_claude") || name.contains("claude") {
        "claude"
    } else if name.contains("_gemini") || name.contains("gemini") {
        "gemini"
    } else {
        "codex"
    };
    write_sidecar(&path, project, agent, date, "intentstest01", frame_kind);
}

fn write_chunk_with_session(
    root: &Path,
    project: &str,
    date: &str,
    agent: &str,
    session_id: &str,
    sequence: u32,
    body: &str,
) -> PathBuf {
    let date_compact = crate::store::compact_date(date);
    let basename = crate::store::session_basename(date, agent, session_id, sequence);
    let dir = root
        .join("store")
        .join("local")
        .join(project)
        .join(date_compact)
        .join("conversations")
        .join(agent);
    fs::create_dir_all(&dir).expect("create chunk dir");
    let path = dir.join(basename);
    fs::write(&path, body).expect("write chunk");
    write_sidecar(
        &path,
        project,
        agent,
        date,
        session_id,
        Some(FrameKind::UserMsg),
    );
    path
}

fn write_sidecar(
    path: &Path,
    project: &str,
    agent: &str,
    date: &str,
    session_id: &str,
    frame_kind: Option<FrameKind>,
) {
    let sidecar = crate::chunker::ChunkMetadataSidecar {
        id: path
            .file_stem()
            .expect("chunk file stem")
            .to_string_lossy()
            .to_string(),
        project: format!("local/{project}"),
        agent: agent.to_string(),
        date: date.to_string(),
        session_id: session_id.to_string(),
        cwd: None,
        timestamp_source: None,
        kind: crate::store::Kind::Conversations,
        run_id: None,
        prompt_id: None,
        frame_kind,
        speaker_hint: None,
        agent_model: None,
        started_at: None,
        completed_at: None,
        token_usage: None,
        findings_count: None,
        workflow_phase: None,
        mode: None,
        skill_code: None,
        framework_version: None,
        intent_entries: Vec::new(),
        tags: Vec::new(),
        artifact_family: None,
        schema_version: None,
        truth_status: None,
        learning_use: None,
        keywords: None,
        content_sha256: None,
        noise_lines_dropped: 0,
    };
    fs::write(
        path.with_extension("meta.json"),
        serde_json::to_vec_pretty(&sidecar).expect("serialize sidecar"),
    )
    .expect("write sidecar");
}

#[test]
fn extracts_and_dedups_signal_records() {
    let tmp = std::env::temp_dir().join(format!(
        "ai-contexters-intents-{}-signals",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);

    let chunk_one = r#"[project: demo | agent: codex | date: 2026-03-15]

[signals]
Decision:
- [decision] Reuse normalize_key from src/chunker.rs:508 for overlap dedup
Intent:
- Let's ship the intention engine this week
Outcome:
- [skill_outcome] p0=0 after cargo test
RED LIGHT: checklist detected (open: 1, done: 0)
- [ ] wire CLI
[/signals]

[12:00:00] user: Let's ship the intention engine this week
[12:01:00] assistant: [decision] Reuse normalize_key from src/chunker.rs:508 for overlap dedup
[12:02:00] assistant: [skill_outcome] p0=0 after cargo test
"#;

    let chunk_two = r#"[project: demo | agent: codex | date: 2026-03-15]

[signals]
Decision:
- [decision] Reuse normalize_key from src/chunker.rs:508 for overlap dedup
Outcome:
- outcome: p0=0 after cargo test
RED LIGHT: checklist detected (open: 0, done: 1)
- [x] wire CLI
[/signals]

[12:05:00] assistant: outcome: p0=0 after cargo test
"#;

    write_chunk(&tmp, "demo", "2026-03-15", "120000_codex-001.md", chunk_one);
    write_chunk(&tmp, "demo", "2026-03-15", "120500_codex-002.md", chunk_two);

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");

    assert_eq!(records.len(), 3);
    assert!(records.iter().any(|record| {
        record.kind == IntentKind::Decision
            && record.summary.contains("Reuse normalize_key")
            && record
                .evidence
                .iter()
                .any(|item| item == "src/chunker.rs:508")
    }));
    assert!(records.iter().any(|record| {
        record.kind == IntentKind::Intent
            && record.summary == "Let's ship the intention engine this week"
    }));
    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Outcome && record.summary.contains("p0=0")
        })
    );
    assert!(!records.iter().any(|record| record.kind == IntentKind::Task));

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn extraction_stats_report_scanned_chunks_and_candidates_before_display_filters() {
    let tmp = std::env::temp_dir().join(format!(
        "ai-contexters-intents-{}-stats",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);

    write_chunk(
        &tmp,
        "demo",
        "2026-03-15",
        "120000_codex-001.md",
        "[signals]\nDecision:\n- [decision] Keep canonical corpus first\n[/signals]\n",
    );
    write_chunk(
        &tmp,
        "demo",
        "2026-03-15",
        "120500_codex-002.md",
        "[signals]\nOutcome:\n- outcome: canonical oracle JSON verified\n[/signals]\n",
    );

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let extraction =
        extract_intents_from_root_at_with_stats(&config, &tmp, now).expect("extract intents");

    assert_eq!(extraction.stats.scanned_count, 2);
    assert_eq!(extraction.stats.candidate_count, extraction.records.len());
    assert!(extraction.stats.candidate_count >= 2);
    assert!(extraction.stats.source_paths_verified);

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn hours_filter_uses_canonical_chunk_date_not_mtime() {
    let tmp = std::env::temp_dir().join(format!(
        "ai-contexters-intents-{}-canonical-date",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);

    write_chunk(
        &tmp,
        "demo",
        "2026-03-01",
        "120000_codex-001.md",
        "[signals]\nIntent:\n- stale synced intent\n[/signals]\n",
    );
    write_chunk(
        &tmp,
        "demo",
        "2026-03-15",
        "120500_codex-002.md",
        "[signals]\nIntent:\n- fresh canonical intent\n[/signals]\n",
    );
    let stale_path = chunk_path(&tmp, "demo", "2026-03-01", "120000_codex-001.md");
    let fresh_mtime = FileTime::from_unix_time(1_773_580_800, 0); // 2026-03-15T12:00:00Z
    set_file_mtime(&stale_path, fresh_mtime).expect("set stale mtime");

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");

    assert!(
        records
            .iter()
            .any(|record| record.summary == "fresh canonical intent")
    );
    assert!(
        !records
            .iter()
            .any(|record| record.summary == "stale synced intent"),
        "mtime drift must not make stale canonical chunks fresh: {records:?}"
    );

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn extracts_raw_lines_and_keeps_surviving_open_tasks() {
    let tmp =
        std::env::temp_dir().join(format!("ai-contexters-intents-{}-raw", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);

    let chunk = r#"[project: demo | agent: claude | date: 2026-03-14]

[11:00:00] user: Proponuję uprościć parser chunków
Bo overlap robi bałagan.
[11:01:00] assistant: decision: keep parser flat around src/intents.rs:1
commit abcdef1 proves the old path was wrong.
[11:02:00] assistant: validation: p1=0 and score=9 after checks
[11:03:00] assistant: - [ ] add CLI polish
"#;

    write_chunk(&tmp, "demo", "2026-03-14", "110000_claude-001.md", chunk);

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 48,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(9, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");

    assert!(records.iter().any(|record| {
        record.kind == IntentKind::Intent
            && record.summary == "Proponuję uprościć parser chunków"
            && record
                .context
                .as_deref()
                .is_some_and(|ctx| ctx.contains("Bo overlap robi bałagan"))
    }));
    assert!(records.iter().any(|record| {
        record.kind == IntentKind::Decision
            && record
                .evidence
                .iter()
                .any(|item| item == "src/intents.rs:1")
            && record.evidence.iter().any(|item| item == "abcdef1")
    }));
    assert!(records.iter().any(|record| {
        record.kind == IntentKind::Outcome
            && record.evidence.iter().any(|item| item == "p1=0")
            && record.evidence.iter().any(|item| item == "score=9")
    }));
    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Task && record.summary == "add CLI polish"
        })
    );
}

#[test]
fn tool_call_and_agent_reply_do_not_become_outcomes() {
    let tmp = migration_test_root("tool-agent-outcome-source-role");
    let _ = fs::remove_dir_all(&tmp);

    let chunk = r#"[project: demo | agent: codex | date: 2026-03-15 | frame_kind: user_msg]

[12:00:00] tool_call: (mcp__aicx__aicx_intents completed with no output)
[12:01:00] agent_reply: validation: p0=0 and cargo test passed
[12:02:00] user: Why keep the canonical index path for first users?
"#;

    write_chunk(&tmp, "demo", "2026-03-15", "120000_codex-001.md", chunk);

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");
    assert!(
        !records
            .iter()
            .any(|record| record.kind == IntentKind::Outcome),
        "tool_call/agent_reply diagnostics must not promote to outcome: {records:?}"
    );
    assert!(
        records.iter().any(|record| {
            record.kind == IntentKind::Intent
                && record.summary.contains("Why keep the canonical index path")
        }),
        "human why/question should remain visible as intent truth: {records:?}"
    );

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn strict_mode_filters_heuristic_only_intents() {
    let tmp = std::env::temp_dir().join(format!(
        "ai-contexters-intents-{}-strict",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);

    let chunk = r#"[project: demo | agent: codex | date: 2026-03-15]

[12:00:00] user: Let's keep only the sharp path.
"#;

    write_chunk(&tmp, "demo", "2026-03-15", "120000_codex-001.md", chunk);

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: true,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");
    assert!(records.is_empty());
}

#[test]
fn frame_kind_filter_keeps_only_matching_chunks() {
    let tmp = std::env::temp_dir().join(format!(
        "ai-contexters-intents-{}-frame-kind",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);

    write_chunk_with_sidecar(
        &tmp,
        "demo",
        "2026-03-15",
        "120000_codex-001.md",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:00:00] user: Let's keep only user intent truth.\n",
        Some(FrameKind::UserMsg),
    );
    write_chunk_with_sidecar(
        &tmp,
        "demo",
        "2026-03-15",
        "120100_codex-002.md",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:01:00] assistant: decision: assistant-only steering\n",
        Some(FrameKind::AgentReply),
    );

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: Some(FrameKind::UserMsg),
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, IntentKind::Intent);
    assert_eq!(records[0].summary, "Let's keep only user intent truth.");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn default_frame_kind_is_user_msg() {
    assert_eq!(IntentsConfig::default_frame_kind(), FrameKind::UserMsg);

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };

    assert_eq!(config.effective_frame_kind(), FrameKind::UserMsg);
}

#[test]
fn default_frame_kind_admits_user_chunk() {
    let tmp = std::env::temp_dir().join(format!(
        "ai-contexters-intents-{}-default-user-frame-kind",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);

    write_chunk_with_sidecar(
        &tmp,
        "demo",
        "2026-03-15",
        "120000_codex-001.md",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:00:00] user: Let's keep only user intent truth.\n",
        Some(FrameKind::UserMsg),
    );

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, IntentKind::Intent);
    assert_eq!(records[0].summary, "Let's keep only user intent truth.");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn default_frame_kind_admits_user_chunk_and_rejects_agent_chunk() {
    let tmp = std::env::temp_dir().join(format!(
        "ai-contexters-intents-{}-default-frame-kind",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);

    write_chunk_with_sidecar(
        &tmp,
        "demo",
        "2026-03-15",
        "120000_codex-001.md",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:00:00] user: Let's keep only user intent truth.\n",
        Some(FrameKind::UserMsg),
    );
    write_chunk_with_sidecar(
        &tmp,
        "demo",
        "2026-03-15",
        "120100_codex-002.md",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:01:00] assistant: decision: assistant-only steering\n",
        Some(FrameKind::AgentReply),
    );

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, IntentKind::Intent);
    assert_eq!(records[0].summary, "Let's keep only user intent truth.");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn explicit_agent_frame_kind_override_still_admits_agent_chunk() {
    let tmp = std::env::temp_dir().join(format!(
        "ai-contexters-intents-{}-agent-frame-kind-override",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);

    write_chunk_with_sidecar(
        &tmp,
        "demo",
        "2026-03-15",
        "120000_codex-001.md",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:00:00] user: Let's keep only user intent truth.\n",
        Some(FrameKind::UserMsg),
    );
    write_chunk_with_sidecar(
        &tmp,
        "demo",
        "2026-03-15",
        "120100_codex-002.md",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:01:00] assistant: decision: assistant-only steering\n",
        Some(FrameKind::AgentReply),
    );

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: Some(FrameKind::AgentReply),
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, IntentKind::Decision);
    assert_eq!(records[0].summary, "assistant-only steering");

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn formats_markdown_with_required_sections() {
    let records = vec![IntentRecord {
        kind: IntentKind::Decision,
        summary: "Keep the parser flat".to_string(),
        context: Some("It removes overlap bugs.".to_string()),
        evidence: vec!["src/intents.rs:42".to_string()],
        project: "demo".to_string(),
        agent: "codex".to_string(),
        date: "2026-03-15".to_string(),
        timestamp: None,
        session_id: "test".to_string(),
        count: None,
        first_chunk: None,
        last_chunk: None,
        source_chunk: "/tmp/demo/2026-03-15/120000_codex-001.md".to_string(),
        source: None,
    }];

    let markdown = format_intents_markdown(&records);
    assert!(markdown.contains("DECISION: Keep the parser flat"));
    assert!(markdown.contains("WHY: It removes overlap bugs."));
    assert!(markdown.contains("EVIDENCE:"));
    assert!(markdown.contains("source_chunk: /tmp/demo/2026-03-15/120000_codex-001.md"));
}

#[test]
fn formats_json_with_same_fields() {
    let records = vec![IntentRecord {
        kind: IntentKind::Outcome,
        summary: "p0=0 after validation".to_string(),
        context: None,
        evidence: vec!["p0=0".to_string()],
        project: "demo".to_string(),
        agent: "claude".to_string(),
        date: "2026-03-15".to_string(),
        timestamp: None,
        session_id: "test".to_string(),
        count: None,
        first_chunk: None,
        last_chunk: None,
        source_chunk: "/tmp/demo/2026-03-15/120500_claude-002.md".to_string(),
        source: None,
    }];

    let json = format_intents_json(&records).expect("serialize intents");
    assert!(json.contains("\"kind\": \"outcome\""));
    assert!(json.contains("\"summary\": \"p0=0 after validation\""));
    assert!(json.contains("\"source_chunk\": \"/tmp/demo/2026-03-15/120500_claude-002.md\""));
}

#[test]
fn formats_oracle_json_as_canonical_corpus_not_semantic_fallback() {
    let records = vec![IntentRecord {
        kind: IntentKind::Decision,
        summary: "Canonical corpus stays source of truth".to_string(),
        context: None,
        evidence: vec!["decision: canonical first".to_string()],
        project: "Loctree/aicx".to_string(),
        agent: "codex".to_string(),
        date: "2026-05-04".to_string(),
        timestamp: None,
        session_id: "sess-canonical".to_string(),
        count: None,
        first_chunk: None,
        last_chunk: None,
        source_chunk: "/tmp/aicx/chunk.md".to_string(),
        source: None,
    }];

    let status = OracleStatus::canonical_corpus_scan(Path::new("/tmp/aicx"), 1, 1, true);
    let json = format_intents_oracle_json(&records, status).expect("serialize oracle intents");
    let payload: serde_json::Value =
        serde_json::from_str(&json).expect("oracle intents JSON should parse");

    assert_eq!(payload["oracle_status"]["backend"], "canonical_corpus");
    assert_eq!(payload["oracle_status"]["index_kind"], "canonical_chunks");
    assert_eq!(
        payload["oracle_status"]["fallback_reason"],
        serde_json::Value::Null
    );
    assert_eq!(payload["oracle_status"]["loctree_scope_safe"], true);
    assert!(
        payload["oracle_status"]["loctree_scope_note"]
            .as_str()
            .unwrap()
            .contains("not a semantic similarity oracle")
    );
}

#[test]
fn strip_case_prefix_is_utf8_safe() {
    let text = "Działa pięknie — pełny artifact pack z Rust flow...";
    assert_eq!(strip_case_insensitive_prefix(text, "validation:"), text);
}

// ── C0.1 native regression anchors ───────────────────────────────
//
// Born as falsifying RED tests that locked the then-broken behavior of the
// native intent stream (plan C0.1, A/B/C drift finding: "native intent
// stream jest częściowo lustrem własnego szumu"). The underlying fixes have
// since landed on this branch, so these now run GREEN as permanent
// regression guards for the post-fix invariants: agent chunks yield no
// operator intents, frame_kind filtering defaults sanely, and limit
// semantics stay honest. The `_drift_red` names are kept for history.

#[test]
fn agent_chunk_still_yields_intents_drift_red() {
    // F1 / role-drift.
    // RED: CURRENT behavior = a pure agent-authored chunk (assistant role,
    // FrameKind::AgentReply) still produces intent records — agent meta-chatter
    // drifts into operator intents (records.len() > 0).
    // DESIRED post-fix invariant = a pure agent chunk yields ZERO operator
    // intents (records.is_empty()). We assert the DESIRED invariant, so this is
    // RED now and flips GREEN when role-drift is fixed.
    let tmp = std::env::temp_dir().join(format!(
        "ai-contexters-intents-{}-agent-drift",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&tmp);

    write_chunk_with_sidecar(
        &tmp,
        "demo",
        "2026-03-15",
        "120100_codex-002.md",
        "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:01:00] assistant: decision: assistant-only steering\n",
        Some(FrameKind::AgentReply),
    );

    let config = IntentsConfig {
        project: "demo".to_string(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let now = DateTime::<Utc>::from_naive_utc_and_offset(
        NaiveDate::from_ymd_opt(2026, 3, 15)
            .expect("date")
            .and_hms_opt(13, 0, 0)
            .expect("time"),
        Utc,
    );

    let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");

    let leaked = records.len();
    let _ = fs::remove_dir_all(&tmp);

    assert!(
        records.is_empty(),
        "agent-authored chunk leaked {leaked} intent(s) (role-drift); desired post-fix = zero: {records:?}",
    );
}

#[test]
fn unresolved_filter_narrows_when_session_resolved() {
    // F2 anchor (was `unresolved_filter_is_noop_without_outcomes_red`).
    // `--unresolved` must drop Intents whose session is resolved by a matching
    // Outcome and keep Intents whose session has none. With one resolved session
    // (Intent + its Outcome) and one open Intent, the filtered output must DIFFER
    // from the full output (real narrowing): the resolved Intent is dropped, the
    // open Intent survives.
    // (The CLI-level `--kind intent` interaction is fixed in `run_intents`, which
    // defers the kind filter so Outcomes survive the resolution check.)
    let records = vec![
        IntentRecord {
            kind: IntentKind::Intent,
            summary: "ship native intents audit".to_string(),
            context: None,
            evidence: vec![],
            project: "demo".to_string(),
            agent: "codex".to_string(),
            date: "2026-03-15".to_string(),
            timestamp: None,
            session_id: "sess-resolved".to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: "/tmp/demo/resolved-intent.md".to_string(),
            source: None,
        },
        IntentRecord {
            kind: IntentKind::Outcome,
            summary: "native intents audit shipped".to_string(),
            context: None,
            evidence: vec![],
            project: "demo".to_string(),
            agent: "codex".to_string(),
            date: "2026-03-15".to_string(),
            timestamp: None,
            session_id: "sess-resolved".to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: "/tmp/demo/resolved-outcome.md".to_string(),
            source: None,
        },
        IntentRecord {
            kind: IntentKind::Intent,
            summary: "lock regression anchor".to_string(),
            context: None,
            evidence: vec![],
            project: "demo".to_string(),
            agent: "codex".to_string(),
            date: "2026-03-15".to_string(),
            timestamp: None,
            session_id: "sess-open".to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: "/tmp/demo/open-intent.md".to_string(),
            source: None,
        },
    ];

    let unresolved_out = apply_display_filters(
        records.clone(),
        &IntentDisplayFilters {
            unresolved: true,
            ..Default::default()
        },
    );
    let full_out = apply_display_filters(records.clone(), &IntentDisplayFilters::default());

    assert_ne!(
        unresolved_out, full_out,
        "--unresolved removed nothing (dead filter): output is byte-identical to full output",
    );
    assert!(
        !unresolved_out
            .iter()
            .any(|r| r.kind == IntentKind::Intent && r.session_id == "sess-resolved"),
        "resolved Intent (session has an Outcome) must be filtered out by --unresolved",
    );
    assert!(
        unresolved_out.iter().any(|r| r.session_id == "sess-open"),
        "open Intent (no Outcome in session) must survive --unresolved",
    );
}

#[test]
fn none_limit_does_not_clip_roadmap() {
    // F3 anchor (was `default_limit_clips_below_explicit_limit_red`).
    // `apply_display_filters` must treat `limit: None` as unlimited so the intents
    // CLI can map its `--limit` default to None and stop silently clipping a 12+
    // item roadmap; an explicit `Some(n)` still truncates to n.
    // (The CLI default override lives in `run_intents`: default sentinel -> None.)
    let records: Vec<IntentRecord> = (0..12)
        .map(|i| IntentRecord {
            kind: IntentKind::Intent,
            summary: format!("planned roadmap item {i}"),
            context: None,
            evidence: vec![],
            project: "demo".to_string(),
            agent: "codex".to_string(),
            date: "2026-03-15".to_string(),
            timestamp: None,
            session_id: format!("sess-limit-{i}"),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: format!("/tmp/demo/limit-{i}.md"),
            source: None,
        })
        .collect();

    let no_limit = apply_display_filters(
        records.clone(),
        &IntentDisplayFilters {
            limit: None,
            ..Default::default()
        },
    );
    let explicit_10 = apply_display_filters(
        records.clone(),
        &IntentDisplayFilters {
            limit: Some(10),
            ..Default::default()
        },
    );

    assert_eq!(
        no_limit.len(),
        records.len(),
        "limit: None must not clip the roadmap (got {} of {})",
        no_limit.len(),
        records.len(),
    );
    assert_eq!(
        explicit_10.len(),
        10,
        "explicit limit must still truncate (got {})",
        explicit_10.len(),
    );
}

// ── classifier tests ────────────────────────────────────────────

mod classifier {
    use super::*;
    use crate::types::{EntryState, EntryType};

    #[derive(Debug, serde::Deserialize)]
    struct CoreOntologyGolden {
        id: String,
        speaker: String,
        text: String,
        expected: String,
        #[serde(rename = "not")]
        not_labels: Vec<String>,
        reason: String,
    }

    fn core_ontology_goldens() -> Vec<CoreOntologyGolden> {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/intents_core_ontology_goldens.json"
        ))
        .expect("core ontology fixture must be valid JSON")
    }

    fn current_classifier_label(golden: &CoreOntologyGolden) -> Option<&'static str> {
        if parse_checklist_task(&golden.text).is_some() {
            return Some("task");
        }

        let is_user = golden.speaker.eq_ignore_ascii_case("user");
        let (entry_type, _) = classify_line_entry_type(&golden.text, is_user)?;
        Some(match entry_type {
            EntryType::Decision => "decision",
            EntryType::Task => "task",
            EntryType::Commitment => "commitment",
            EntryType::Intent | EntryType::Question | EntryType::Why => "intent",
            EntryType::Outcome | EntryType::Result => "outcome",
            EntryType::Assumption => "assumption",
            EntryType::Insight => "insight",
            EntryType::Argue => "argue",
        })
    }

    #[test]
    fn core_ontology_golden_fixture_is_well_formed() {
        let goldens = core_ontology_goldens();
        assert_eq!(goldens.len(), 15);

        let mut ids = HashSet::new();
        for golden in &goldens {
            assert!(ids.insert(&golden.id), "duplicate golden id {}", golden.id);
            assert!(
                !golden.text.trim().is_empty(),
                "{} must include text",
                golden.id
            );
            assert!(
                !golden.expected.trim().is_empty(),
                "{} must include expected label",
                golden.id
            );
            assert!(
                !golden.reason.trim().is_empty(),
                "{} must include a reason",
                golden.id
            );
        }
    }

    #[test]
    #[ignore = "target ontology audit: current classifier is expected to fail until semantics are fixed"]
    fn core_ontology_goldens_match_target_semantics() {
        let mut failures = Vec::new();

        for golden in core_ontology_goldens() {
            let actual = current_classifier_label(&golden).unwrap_or("unclassified");
            if actual != golden.expected || golden.not_labels.iter().any(|label| label == actual) {
                failures.push(format!(
                    "{}: expected {}, got {}; text={:?}; reason={}",
                    golden.id, golden.expected, actual, golden.text, golden.reason
                ));
            }
        }

        assert!(
            failures.is_empty(),
            "core ontology mismatches:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn classifies_decision_marker() {
        let result = classify_line_entry_type("[decision] Use flat parser", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Decision));
    }

    #[test]
    fn classifies_decision_prefix() {
        let result = classify_line_entry_type("decision: keep normalize_key", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Decision));
    }

    #[test]
    fn classifies_question_mark() {
        let result =
            classify_line_entry_type("How does the auth middleware handle sessions?", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Question));
    }

    #[test]
    fn classifies_question_prefix() {
        let result = classify_line_entry_type("question: can we reuse normalize_key?", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Question));
    }

    #[test]
    fn classifies_assumption() {
        let result =
            classify_line_entry_type("assumption: the store root is always ~/.aicx", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Assumption));
    }

    #[test]
    fn classifies_polish_assumption() {
        let result = classify_line_entry_type("zakładam że ścieżka zawsze istnieje", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Assumption));
    }

    #[test]
    fn classifies_ascii_polish_assumption() {
        let result =
            classify_line_entry_type("zakladam, ze cwd mozna wywnioskowac z tresci", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Assumption));
    }

    #[test]
    fn classifies_insight_marker() {
        let result = classify_line_entry_type(
            "insight: aicx is an intention engine not a formatter",
            false,
        );
        assert_eq!(result.map(|r| r.0), Some(EntryType::Insight));
    }

    #[test]
    fn classifies_result_marker() {
        let result = classify_line_entry_type("result: latency 450ms p99", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Result));
    }

    #[test]
    fn classifies_result_from_keywords() {
        let result = classify_line_entry_type("tests 276/276 passed, 0 warnings", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Result));
    }

    #[test]
    fn classifies_outcome_tag() {
        let result = classify_line_entry_type("[skill_outcome] p0=0 after cargo test", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Outcome));
    }

    #[test]
    fn classifies_file_completion_report_as_outcome() {
        let result = classify_line_entry_type(
            "plik docs/INTENTS_CLASSIFICATION_RULES.md zostal dodany",
            false,
        );
        assert_eq!(result.map(|r| r.0), Some(EntryType::Outcome));
    }

    #[test]
    fn classifies_observed_batch_counts_as_outcome() {
        let result = classify_line_entry_type(
            "batch 10 plikow dal 66 records: 48 intent, 14 outcome, 4 decision",
            true,
        );
        assert_eq!(result.map(|r| r.0), Some(EntryType::Outcome));
    }

    #[test]
    fn classifies_why_marker() {
        let result =
            classify_line_entry_type("because the old auth middleware stores tokens wrong", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Why));
    }

    #[test]
    fn classifies_argue_marker() {
        let result = classify_line_entry_type(
            "on the other hand, rewriting is cheaper than patching",
            false,
        );
        assert_eq!(result.map(|r| r.0), Some(EntryType::Argue));
    }

    #[test]
    fn classifies_user_intent() {
        let result = classify_line_entry_type("Let's ship the intention engine this week", true);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Intent));
    }

    #[test]
    fn classifies_polish_user_intent() {
        let result = classify_line_entry_type("Proponuję uprościć parser chunków", true);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Intent));
    }

    #[test]
    fn classifies_polish_operator_requirement_as_intent() {
        let result = classify_line_entry_type(
            "Nie może być tak, że ostatnie słowo gubi kolor akcentu",
            true,
        );
        assert_eq!(result.map(|r| r.0), Some(EntryType::Intent));
    }

    #[test]
    fn classifies_polish_musimy_requirement_as_intent() {
        let result = classify_line_entry_type("musimy dodac parser dla obcych md", true);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Intent));
    }

    #[test]
    fn classifies_polish_trzeba_requirement_as_intent() {
        let result = classify_line_entry_type("trzeba sprawdzic outcomes i claims", true);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Intent));
    }

    #[test]
    fn classifies_operator_import_policy_as_decision() {
        let result = classify_line_entry_type(
            "obce md importujemy tylko przez operator-md, bez zgadywania cwd",
            true,
        );
        assert_eq!(result.map(|r| r.0), Some(EntryType::Decision));
    }

    #[test]
    fn classifies_operator_scope_rejection_as_decision() {
        let result = classify_line_entry_type("nie fixujemy tego teraz", true);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Decision));
    }

    #[test]
    fn classifies_required_artifact_property_as_decision() {
        let result = classify_line_entry_type(
            "Materiał ma być deterministyczny — musi mieć source_hash",
            true,
        );
        assert_eq!(result.map(|r| r.0), Some(EntryType::Decision));
    }

    #[test]
    fn classifies_canonical_question_as_question_not_decision() {
        let result =
            classify_line_entry_type("Why keep the canonical index path for first users?", true);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Question));
    }

    #[test]
    fn classifies_task_directive_as_task() {
        let result = classify_line_entry_type("task: stworz plik z zasadami klasyfikacji", true);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Task));
    }

    #[test]
    fn classifies_polish_action_request_as_task() {
        let result = classify_line_entry_type("stworz prosze plik z zasadami klasyfikacji", true);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Task));
    }

    #[test]
    fn classifies_bare_checkbox_as_task() {
        let result =
            classify_line_entry_type("[ ] stworz prosze plik z zasadami klasyfikacji", true);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Task));
    }

    #[test]
    fn classifies_future_promise_as_commitment() {
        let result = classify_line_entry_type("zrobie to zaraz", false);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Commitment));
    }

    #[test]
    fn abstains_on_ambiguous_line() {
        let result = classify_line_entry_type("some random code comment", false);
        assert!(result.is_none());
    }

    #[test]
    fn abstains_on_short_question() {
        let result = classify_line_entry_type("what?", false);
        assert!(result.is_none());
    }

    #[test]
    fn classify_chunk_all_eleven_types() {
        let content = r#"[project: demo | agent: claude | date: 2026-04-15]

[signals]
Decision:
- [decision] Use 11-type taxonomy
Intent:
- Let's ship intent engine
Task:
- task: add semantic fixture coverage
Outcome:
- outcome: migration completed successfully
[/signals]

[12:00:00] user: Proponuję dodać link graph
[12:01:00] assistant: assumption: store root always at ~/.aicx
[12:02:00] assistant: insight: aicx is an intention retrieval engine
[12:03:00] assistant: result: tests 276/276 passed
[12:04:00] user: How does the chunker handle overlap?
[12:05:00] assistant: because the old approach created duplicates
[12:06:00] assistant: on the other hand, flat parsing is simpler
[12:07:00] assistant: promise: zrobie to zaraz
"#;

        let entries = classify_chunk_entries(
            content,
            "/tmp/test/chunk-001.md",
            Some("demo"),
            Some("claude"),
            Some("sess-01"),
            "2026-04-15",
        );

        let types: HashSet<EntryType> = entries.iter().map(|e| e.entry_type).collect();
        assert!(types.contains(&EntryType::Decision), "missing Decision");
        assert!(types.contains(&EntryType::Task), "missing Task");
        assert!(types.contains(&EntryType::Commitment), "missing Commitment");
        assert!(types.contains(&EntryType::Outcome), "missing Outcome");
        assert!(types.contains(&EntryType::Intent), "missing Intent");
        assert!(types.contains(&EntryType::Assumption), "missing Assumption");
        assert!(types.contains(&EntryType::Insight), "missing Insight");
        assert!(types.contains(&EntryType::Result), "missing Result");
        assert!(types.contains(&EntryType::Question), "missing Question");
        assert!(types.contains(&EntryType::Why), "missing Why");
        assert!(types.contains(&EntryType::Argue), "missing Argue");

        for entry in &entries {
            assert!(!entry.id.is_empty());
            assert!(!entry.title.is_empty());
            assert!(entry.confidence >= CLASSIFIER_ABSTAIN_THRESHOLD);
            assert_eq!(entry.date, "2026-04-15");
            assert_eq!(entry.source_chunk, "/tmp/test/chunk-001.md");
        }
    }

    #[test]
    fn stable_ids_are_deterministic() {
        let content = "[12:00:00] user: Let's ship intent engine\n";
        let a = classify_chunk_entries(content, "/chunk.md", None, None, None, "2026-04-15");
        let b = classify_chunk_entries(content, "/chunk.md", None, None, None, "2026-04-15");
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.id, y.id);
        }
    }

    #[test]
    fn tags_are_inferred() {
        let content = "[12:00:00] assistant: result: auth login tests passed\n";
        let entries = classify_chunk_entries(content, "/c.md", None, None, None, "2026-04-15");
        let result_entry = entries.iter().find(|e| e.entry_type == EntryType::Result);
        assert!(result_entry.is_some());
        let tags = &result_entry.unwrap().tags;
        assert!(tags.contains(&"auth".to_string()) || tags.contains(&"testing".to_string()));
    }

    #[test]
    fn initial_state_mapping() {
        assert_eq!(initial_state(EntryType::Intent), EntryState::Proposed);
        assert_eq!(initial_state(EntryType::Task), EntryState::Proposed);
        assert_eq!(initial_state(EntryType::Commitment), EntryState::Proposed);
        assert_eq!(initial_state(EntryType::Question), EntryState::Proposed);
        assert_eq!(initial_state(EntryType::Assumption), EntryState::Proposed);
        assert_eq!(initial_state(EntryType::Decision), EntryState::Active);
        assert_eq!(initial_state(EntryType::Insight), EntryState::Active);
        assert_eq!(initial_state(EntryType::Outcome), EntryState::Done);
        assert_eq!(initial_state(EntryType::Result), EntryState::Done);
        assert_eq!(initial_state(EntryType::Why), EntryState::Active);
        assert_eq!(initial_state(EntryType::Argue), EntryState::Active);
    }

    // Area E.7: keyword classifier must respect word boundaries, negation,
    // inline code spans, and fenced code blocks.

    #[test]
    fn test_intent_let_us_not_refactor_is_not_intent() {
        let result = classify_line_entry_type("Let's not refactor the parser today", true);
        assert!(
            result.map(|r| r.0) != Some(EntryType::Intent),
            "negation `let's not` must invert the keyword polarity"
        );
        assert!(!looks_like_intent_line(
            "Let's not refactor the parser today"
        ));
    }

    #[test]
    fn test_intent_polish_nie_mam_pomyslu_is_not_intent() {
        // Diacritic-aware word boundary: `pomysłu` should not match keyword
        // `pomysł` because the following `u` is alphanumeric.
        assert!(!looks_like_intent_line("nie mam pomysłu na ten task"));
        let classified = classify_line_entry_type("nie mam pomysłu na ten task", true);
        assert!(
            classified.map(|r| r.0) != Some(EntryType::Intent),
            "Polish negation `nie mam` + suffixed `pomysłu` must not classify as intent"
        );
    }

    #[test]
    fn test_intent_inline_code_let_us_encrypt_is_not_intent() {
        let line = "We rotated certs via `let's encrypt` last Tuesday";
        assert!(
            !looks_like_intent_line(line),
            "keyword inside backtick inline code must not classify"
        );
    }

    #[test]
    fn test_intent_in_fenced_code_block_is_not_intent() {
        let chunk = "[project: demo | agent: codex | date: 2026-05-20]\n\n\
            [12:00:00] user: see the snippet below\n\
            ```\n\
            let's encrypt --domain example.com\n\
            ```\n\
            [12:01:00] user: that's all\n";
        let entries = classify_chunk_entries(
            chunk,
            "fake.md",
            Some("demo"),
            Some("codex"),
            None,
            "2026-05-20",
        );
        assert!(
            entries.iter().all(|e| e.entry_type != EntryType::Intent),
            "lines inside ``` fence must be excluded from classification, got: {entries:?}"
        );
    }

    #[test]
    fn test_intent_real_let_us_refactor_still_classifies() {
        let result = classify_line_entry_type("Let's refactor the parser today", true);
        assert_eq!(
            result.map(|r| r.0),
            Some(EntryType::Intent),
            "positive `let's refactor` must still classify as intent"
        );
        assert!(looks_like_intent_line("Let's refactor the parser today"));
    }

    #[test]
    fn test_intent_polish_chce_zrobic_still_classifies() {
        assert!(
            looks_like_intent_line("chcę zrobić nowy parser"),
            "positive Polish intent should still match keyword `chcę`"
        );
        let result = classify_line_entry_type("chcę zrobić nowy parser", true);
        assert_eq!(result.map(|r| r.0), Some(EntryType::Intent));
    }
}

mod session_level {
    use super::*;
    use crate::types::{EntryState, EntryType};

    fn make_entry(
        entry_type: EntryType,
        title: &str,
        date: &str,
        session_id: &str,
        project: &str,
    ) -> IntentEntry {
        IntentEntry {
            id: IntentEntry::stable_id(title, 0, entry_type),
            entry_type,
            state: initial_state(entry_type),
            title: title.to_string(),
            body: None,
            evidence: Vec::new(),
            links: Vec::new(),
            superseded_by: None,
            confidence: 0.9,
            tags: Vec::new(),
            project: Some(project.to_string()),
            agent: Some("claude".to_string()),
            session_id: Some(session_id.to_string()),
            timestamp: None,
            date: date.to_string(),
            source_chunk: "/test/chunk.md".to_string(),
        }
    }

    #[test]
    fn supersedes_marks_older_entry() {
        let mut entries = vec![
            make_entry(
                EntryType::Intent,
                "ship the new intent engine soon with basic features",
                "2026-04-10",
                "s1",
                "demo",
            ),
            make_entry(
                EntryType::Intent,
                "ship the new intent engine soon with full taxonomy",
                "2026-04-15",
                "s2",
                "demo",
            ),
        ];

        postprocess_session_entries(&mut entries, Some(30));

        let older_id = entries[0].id.clone();
        let newer_id = entries[1].id.clone();

        assert_eq!(entries[0].state, EntryState::Superseded);
        assert_eq!(entries[0].superseded_by, Some(newer_id.clone()));
        assert_eq!(entries[1].state, EntryState::Active);
        assert!(
            entries[1]
                .links
                .iter()
                .any(|l| l.relation == LinkType::Supersedes && l.target == older_id)
        );
    }

    #[test]
    fn supersession_promotes_winner_and_stamps_superseded_by() {
        let mut entries = vec![
            make_entry(
                EntryType::Intent,
                "add flag X parser fallback mode",
                "2026-04-10",
                "s1",
                "demo",
            ),
            make_entry(
                EntryType::Intent,
                "add flag X parser fallback mode; no, the real fix is Y",
                "2026-04-15",
                "s2",
                "demo",
            ),
            make_entry(
                EntryType::Intent,
                "ship unrelated docs cleanup",
                "2026-04-15",
                "s3",
                "demo",
            ),
        ];

        postprocess_session_entries(&mut entries, Some(7));
        postprocess_session_entries(&mut entries, Some(7));

        let loser_id = entries[0].id.clone();
        let winner_id = entries[1].id.clone();
        let json = serde_json::to_value(&entries).expect("serialize entries");

        assert_eq!(entries[0].state, EntryState::Superseded);
        assert_eq!(entries[0].superseded_by, Some(winner_id.clone()));
        assert_eq!(entries[1].state, EntryState::Active);
        assert!(entries[2].superseded_by.is_none());
        assert!(
            entries[1]
                .links
                .iter()
                .any(|l| l.relation == LinkType::Supersedes && l.target == loser_id)
        );

        assert_eq!(json[0]["status"], "superseded");
        assert_eq!(json[0]["superseded_by"], winner_id);
        assert_eq!(json[1]["status"], "active");
        assert_eq!(json[1]["links"][0]["relation"], "supersedes");
        assert_eq!(json[1]["links"][0]["target"], entries[0].id);
        assert!(json[2].get("superseded_by").is_none());
    }

    #[test]
    fn supersedes_chain_final_state_is_input_order_independent() {
        // P2-01: in a 3-link chain A <- B <- C, B both supersedes A and is
        // superseded by C. The final state must be A=Superseded(by B),
        // B=Superseded(by C), C=Active — regardless of input order. The old
        // pairwise action loop got this right only by accident of ascending
        // push order.
        let chain_entries = || {
            (
                make_entry(
                    EntryType::Intent,
                    "ship the new intent engine basic skeleton",
                    "2026-04-10",
                    "s1",
                    "demo",
                ),
                make_entry(
                    EntryType::Intent,
                    "ship the new intent engine with retries",
                    "2026-04-12",
                    "s2",
                    "demo",
                ),
                make_entry(
                    EntryType::Intent,
                    "ship the new intent engine final form",
                    "2026-04-15",
                    "s3",
                    "demo",
                ),
            )
        };

        let (a, b, c) = chain_entries();
        let (a_id, b_id, c_id) = (a.id.clone(), b.id.clone(), c.id.clone());
        let forward = vec![a, b, c];
        let (a2, b2, c2) = chain_entries();
        let reversed = vec![c2, b2, a2];

        for (label, mut entries) in [("forward", forward), ("reversed", reversed)] {
            detect_supersedes(&mut entries);

            let by_id = |id: &str| {
                entries
                    .iter()
                    .find(|e| e.id == id)
                    .unwrap_or_else(|| panic!("entry {id} missing in {label} run"))
            };
            let (a, b, c) = (by_id(&a_id), by_id(&b_id), by_id(&c_id));

            assert_eq!(
                a.state,
                EntryState::Superseded,
                "{label}: A must be superseded"
            );
            assert_eq!(
                a.superseded_by,
                Some(b_id.clone()),
                "{label}: A must be superseded by B (chain link, not by C)"
            );
            assert_eq!(
                b.state,
                EntryState::Superseded,
                "{label}: B must be superseded"
            );
            assert_eq!(
                b.superseded_by,
                Some(c_id.clone()),
                "{label}: B must be superseded by C"
            );
            assert_eq!(
                c.state,
                EntryState::Active,
                "{label}: C is the chain head and must stay Active"
            );
            assert!(
                c.superseded_by.is_none(),
                "{label}: C must not be superseded"
            );
            assert!(
                b.links
                    .iter()
                    .any(|l| l.relation == LinkType::Supersedes && l.target == a_id),
                "{label}: B must carry a supersedes link to A"
            );
            assert!(
                c.links
                    .iter()
                    .any(|l| l.relation == LinkType::Supersedes && l.target == b_id),
                "{label}: C must carry a supersedes link to B"
            );
        }
    }

    #[test]
    fn contradicted_assumption() {
        let mut entries = vec![
            make_entry(
                EntryType::Assumption,
                "store root always exists",
                "2026-04-10",
                "s1",
                "demo",
            ),
            make_entry(
                EntryType::Result,
                "store root failed validation error",
                "2026-04-11",
                "s1",
                "demo",
            ),
        ];

        postprocess_session_entries(&mut entries, Some(30));

        assert_eq!(entries[0].state, EntryState::Contradicted);
        assert!(
            entries[0]
                .links
                .iter()
                .any(|l| l.relation == LinkType::Contradicts)
        );
    }

    #[test]
    fn insight_links_to_sources() {
        let mut entries = vec![
            make_entry(
                EntryType::Result,
                "tests 276/276 passed",
                "2026-04-15",
                "s1",
                "demo",
            ),
            make_entry(
                EntryType::Outcome,
                "migration complete",
                "2026-04-15",
                "s1",
                "demo",
            ),
            make_entry(
                EntryType::Insight,
                "aicx is an intention engine",
                "2026-04-15",
                "s1",
                "demo",
            ),
        ];

        postprocess_session_entries(&mut entries, Some(30));

        let insight = &entries[2];
        assert!(!insight.links.is_empty());
        assert!(
            insight
                .links
                .iter()
                .all(|l| l.relation == LinkType::DerivedFrom)
        );
    }

    #[test]
    fn unresolved_intent_tagged_after_threshold() {
        let old_date = (chrono::Utc::now().date_naive() - chrono::Duration::days(10))
            .format("%Y-%m-%d")
            .to_string();
        let mut entries = vec![make_entry(
            EntryType::Intent,
            "implement dark mode",
            &old_date,
            "s-old",
            "demo",
        )];

        postprocess_session_entries(&mut entries, Some(7));

        assert!(entries[0].tags.contains(&"unresolved".to_string()));
    }

    #[test]
    fn recent_intent_not_tagged_unresolved() {
        let today = chrono::Utc::now()
            .date_naive()
            .format("%Y-%m-%d")
            .to_string();
        let mut entries = vec![make_entry(
            EntryType::Intent,
            "implement dark mode",
            &today,
            "s-today",
            "demo",
        )];

        postprocess_session_entries(&mut entries, Some(7));

        assert!(!entries[0].tags.contains(&"unresolved".to_string()));
    }
}

mod quality {
    use super::*;

    // ── metadata-noise filter ───────────────────────────────────────

    #[test]
    fn metadata_only_summary_dropped() {
        assert!(is_metadata_only_summary("1 Wierność źródłu"));
        assert!(is_metadata_only_summary(
            "4 Deterministyczność transformacji"
        ));
        assert!(is_metadata_only_summary("2."));
        assert!(is_metadata_only_summary("3) Section heading"));
        assert!(is_metadata_only_summary("..."));
        assert!(is_metadata_only_summary("... ```"));
        assert!(is_metadata_only_summary("```"));
        assert!(is_metadata_only_summary("---"));
    }

    #[test]
    fn real_decision_summary_kept() {
        assert!(!is_metadata_only_summary(
            "Materiał nie może dopowiadać, streszczać ani interpretować rozmowy",
        ));
        assert!(!is_metadata_only_summary(
            "Keep canonical corpus as the source of truth",
        ));
        assert!(!is_metadata_only_summary(
            "P1.2 — cache_scope_authority zawsze RepoVerified",
        ));
    }

    #[test]
    fn pipe_separated_numeric_headings_classified_as_noise() {
        assert!(is_section_heading_noise(
            "1 Wierność źródłu | 2 Retrieval quality",
        ));
        assert!(is_section_heading_noise("4 Deterministyczność | 5 Dedup"));
        // Real prose with pipes is not noise:
        assert!(!is_section_heading_noise(
            "He said X and we agreed | She replied Y so we shipped Z",
        ));
        assert!(!is_section_heading_noise(
            "Let's keep the parser flat | Avoid premature abstractions",
        ));
    }

    // ── sentence-aware truncation ──────────────────────────────────

    #[test]
    fn short_summary_returned_unchanged() {
        let text = "Keep the parser flat";
        assert_eq!(truncate_summary_for_display(text), text);
    }

    #[test]
    fn long_summary_ends_at_sentence_terminator_when_available() {
        let mut text = String::new();
        text.push_str("Pierwsza część decyzji o canonical corpus i jego znaczeniu dla całego stacku VetCoders w kontekście długoterminowej strategii AICX. ");
        // Force length > 480 bytes; ensure a full sentence-terminator
        // exists in the lookback window (last 80 bytes before cutoff).
        text.push_str(
            &"Druga część rozważań która dopisuje treść aż przekroczymy próg 480 bajtów. "
                .repeat(5),
        );
        assert!(text.len() > 480);

        let out = truncate_summary_for_display(&text);
        assert!(out.len() <= 480 + 4);
        // Output must end on a strong terminator or an ellipsis,
        // never mid-word with "...[truncated]".
        assert!(
            out.ends_with('.') || out.ends_with('!') || out.ends_with('?') || out.ends_with('…')
        );
        assert!(!out.contains("...[truncated]"));
    }

    #[test]
    fn long_summary_without_terminator_falls_back_to_word_boundary() {
        // Long text with no sentence terminators at all.
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau upsilon phi chi psi omega ".repeat(5);
        assert!(text.len() > 480);

        let out = truncate_summary_for_display(&text);
        assert!(out.ends_with('…'));
        assert!(!out.contains("...[truncated]"));
        // Ensure the cut landed on a word boundary (no partial word
        // immediately before the ellipsis).
        let stem = out.trim_end_matches(['…', ' ']);
        assert!(
            stem.chars().last().is_none_or(|c| !c.is_alphabetic())
                || stem.ends_with(|c: char| c.is_alphabetic())
        );
    }

    // ── content-scoped dedup ───────────────────────────────────────

    #[test]
    fn cross_session_identical_summary_collapses_with_consistent_provenance() {
        let tmp = std::env::temp_dir().join(format!(
            "ai-contexters-intents-{}-cross-session",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);

        let body = "[project: demo | agent: codex | date: 2026-05-02]\n\n\
                    [11:00:00] user: Materiał ma być deterministyczny — musi mieć source_hash i być wygenerowany z raw JSONL.\n";

        // Same operator-decision text appearing in two distinct sessions.
        write_chunk_with_session(&tmp, "demo", "2026-05-02", "codex", "019dcceb-48c", 3, body);
        write_chunk_with_session(
            &tmp,
            "demo",
            "2026-05-04",
            "codex",
            "019df273-2c1",
            27,
            body,
        );

        let config = IntentsConfig {
            project: "demo".to_string(),
            hours: 240,
            strict: false,
            min_confidence: None,
            kind_filter: Some(IntentKind::Decision),
            frame_kind: None,
        };
        let now = DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 5, 5)
                .expect("date")
                .and_hms_opt(0, 0, 0)
                .expect("time"),
            Utc,
        );

        let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");

        // Same normalized fact across sessions surfaces once, with the
        // selected source_chunk and session_id kept in sync.
        let decisions: Vec<&IntentRecord> = records
            .iter()
            .filter(|r| r.kind == IntentKind::Decision)
            .collect();
        assert_eq!(
            decisions.len(),
            1,
            "expected one collapsed decision for repeated text, got {decisions:?}",
        );

        let record = decisions[0];
        let stem = std::path::Path::new(&record.source_chunk)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        assert!(
            stem.contains(&record.session_id),
            "session_id={} not found in source_chunk filename={}",
            record.session_id,
            stem,
        );
        assert_eq!(record.session_id, "019df273-2c1");

        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn cross_session_distinct_human_intents_do_not_collapse() {
        let tmp = std::env::temp_dir().join(format!(
            "ai-contexters-intents-{}-cross-session-distinct",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);

        let body_a = "[project: demo | agent: codex | date: 2026-05-02]\n\n\
                    [11:00:00] user: Materiał ma być deterministyczny — musi mieć source_hash.\n";
        let body_b = "[project: demo | agent: codex | date: 2026-05-04]\n\n\
                    [11:00:00] user: Materiał ma być audytowalny — musi mieć source_manifest.\n";

        write_chunk_with_session(
            &tmp,
            "demo",
            "2026-05-02",
            "codex",
            "019dcceb-48c",
            3,
            body_a,
        );
        write_chunk_with_session(
            &tmp,
            "demo",
            "2026-05-04",
            "codex",
            "019df273-2c1",
            27,
            body_b,
        );

        let config = IntentsConfig {
            project: "demo".to_string(),
            hours: 240,
            strict: false,
            min_confidence: None,
            kind_filter: Some(IntentKind::Decision),
            frame_kind: None,
        };
        let now = DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 5, 5)
                .expect("date")
                .and_hms_opt(0, 0, 0)
                .expect("time"),
            Utc,
        );

        let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");
        let decisions: Vec<&IntentRecord> = records
            .iter()
            .filter(|r| r.kind == IntentKind::Decision)
            .collect();
        assert_eq!(
            decisions.len(),
            2,
            "distinct human decisions collapsed too aggressively: {decisions:?}",
        );
        assert!(decisions.iter().any(|r| r.summary.contains("source_hash")));
        assert!(
            decisions
                .iter()
                .any(|r| r.summary.contains("source_manifest"))
        );

        let _ = fs::remove_dir_all(tmp);
    }

    #[test]
    fn metadata_only_decision_filtered_at_extraction() {
        let tmp = std::env::temp_dir().join(format!(
            "ai-contexters-intents-{}-metadata-noise",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);

        let body = "[project: demo | agent: codex | date: 2026-05-04]\n\n\
                    [10:00:00] user: 4 Deterministyczność transformacji\n\
                    [10:01:00] user: Materiał musi mieć source_hash i być deterministycznie wygenerowany z raw JSONL.\n";

        write_chunk_with_session(
            &tmp,
            "demo",
            "2026-05-04",
            "codex",
            "019df273-2c1",
            27,
            body,
        );

        let config = IntentsConfig {
            project: "demo".to_string(),
            hours: 240,
            strict: false,
            min_confidence: None,
            kind_filter: None,
            frame_kind: None,
        };
        let now = DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 5, 5)
                .expect("date")
                .and_hms_opt(0, 0, 0)
                .expect("time"),
            Utc,
        );

        let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");

        // Numeric-list heading "4 Deterministyczność transformacji" must
        // not appear as the top decision/intent.
        assert!(
            !records
                .iter()
                .any(|r| r.summary.starts_with("4 Deterministyczność")),
            "metadata-only heading leaked into records: {records:?}",
        );

        // The real operator-decision line still survives.
        assert!(
            records
                .iter()
                .any(|r| r.summary.starts_with("Materiał musi mieć source_hash")),
            "real decision line dropped: records={records:?}",
        );

        let _ = fs::remove_dir_all(tmp);
    }

    // ── path-derived session reconciliation ────────────────────────

    #[test]
    fn reconcile_session_id_uses_filename_when_record_disagrees() {
        let mut records = vec![IntentRecord {
            kind: IntentKind::Intent,
            summary: "claim from session A but filename is from session B".to_string(),
            context: None,
            evidence: vec![],
            project: "demo".to_string(),
            agent: "codex".to_string(),
            date: "2026-05-04".to_string(),
            timestamp: None,
            session_id: "019dcceb-48c".to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk:
                "/store/Loctree/aicx/2026_0504/conversations/codex/2026_0504_codex_019df273-2c1_027.md"
                    .to_string(),
            source: None,
        }];

        reconcile_session_id_with_path(&mut records);

        assert_eq!(
            records[0].session_id, "019df273-2c1",
            "session_id should reflect what the cited filename actually contains",
        );
    }

    #[test]
    fn reconcile_keeps_session_id_when_already_consistent() {
        let mut records = vec![IntentRecord {
            kind: IntentKind::Decision,
            summary: "session_id matches filename".to_string(),
            context: None,
            evidence: vec![],
            project: "demo".to_string(),
            agent: "codex".to_string(),
            date: "2026-05-04".to_string(),
            timestamp: None,
            session_id: "019df273-2c1".to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk:
                "/store/Loctree/aicx/2026_0504/conversations/codex/2026_0504_codex_019df273-2c1_027.md"
                    .to_string(),
            source: None,
        }];

        reconcile_session_id_with_path(&mut records);

        assert_eq!(records[0].session_id, "019df273-2c1");
    }

    #[test]
    fn truncated_duplicate_record_is_dropped_when_full_source_twin_exists() {
        let source_chunk = "/tmp/2026_0504_codex_019df273-2c1_067.md".to_string();
        let mut records = vec![
            IntentRecord {
                kind: IntentKind::Decision,
                summary: "nie mamy ani jednego użytkownika. Jesteśmy teraz w San Francisco i potrzebujemy strategii. Zrób sobie aicx search...[truncated]".to_string(),
                evidence: Vec::new(),
                project: "aicx".to_string(),
                agent: "codex".to_string(),
                date: "2026-05-04".to_string(),
                session_id: "019df273-2c1".to_string(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk: source_chunk.clone(),
                timestamp: None,
                context: None,
                source: None,
            },
            IntentRecord {
                kind: IntentKind::Decision,
                summary: "nie mamy ani jednego użytkownika. Jesteśmy teraz w San Francisco i potrzebujemy strategii. Zrób sobie aicx search 'repozytoria libraxis loctree vetcoders' i pomóż.".to_string(),
                evidence: Vec::new(),
                project: "aicx".to_string(),
                agent: "codex".to_string(),
                date: "2026-05-04".to_string(),
                session_id: "019df273-2c1".to_string(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk,
                timestamp: None,
                context: None,
                source: None,
            },
        ];

        drop_truncated_duplicate_records(&mut records);

        assert_eq!(records.len(), 1);
        assert!(!records[0].summary.contains("...[truncated]"));
    }

    // Area E.3: dedup must scale linearly. The quadratic shape blew up on
    // 10k-record sessions (100M comparisons); the indexed version stays
    // O(N).

    fn make_record(
        kind: IntentKind,
        summary: &str,
        session_id: &str,
        source_chunk: &str,
    ) -> IntentRecord {
        IntentRecord {
            kind,
            summary: summary.to_string(),
            evidence: Vec::new(),
            project: "demo".to_string(),
            agent: "codex".to_string(),
            date: "2026-05-04".to_string(),
            session_id: session_id.to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: source_chunk.to_string(),
            timestamp: None,
            context: None,
            source: None,
        }
    }

    #[test]
    fn test_drop_truncated_duplicate_is_linear() {
        // Build 10k records: 5k full + 5k truncated prefix duplicates,
        // distributed across many (session, chunk) buckets so the bucket
        // index keeps each lookup O(1). The previous O(N²) shape on this
        // input ran ~100M comparisons.
        //
        // The threshold guards against quadratic regression, not micro-
        // benchmarks the runner's TSC. Run the dedup three times on
        // independent clones and take the median elapsed; assert against
        // a generous 500ms cap. Rationale:
        //
        //  * Linear (current) implementation lands well under 100ms on
        //    every runner we've measured (M-series Mac, x86_64 Linux CI).
        //  * Quadratic regression on N=10k explodes to multiple seconds,
        //    so 500ms still catches it with ~5x safety margin.
        //  * Median-of-3 smooths the one-in-a-while shared-runner load
        //    spike (the prior `< 200ms` cap tripped on a 201.98ms outlier
        //    on macos-self-hosted while linux passed the same test).
        let mut records = Vec::with_capacity(10_000);
        for i in 0..5_000 {
            let session = format!("s{:04}", i % 250);
            let chunk = format!("/tmp/s{:04}_c{:03}.md", i % 250, i % 50);
            let full = format!(
                "Decision number {i}: keep canonical store at ~/.aicx/store and rebuild semantic index nightly"
            );
            let truncated =
                format!("Decision number {i}: keep canonical store at ~/.aicx/store...[truncated]");
            records.push(make_record(IntentKind::Decision, &full, &session, &chunk));
            records.push(make_record(
                IntentKind::Decision,
                &truncated,
                &session,
                &chunk,
            ));
        }
        assert_eq!(records.len(), 10_000);

        let mut samples: Vec<std::time::Duration> = Vec::with_capacity(3);
        let mut last_result: Vec<IntentRecord> = Vec::new();
        for _ in 0..3 {
            let mut run = records.clone();
            let start = std::time::Instant::now();
            drop_truncated_duplicate_records(&mut run);
            samples.push(start.elapsed());
            last_result = run;
        }
        samples.sort();
        let median = samples[1];

        assert!(
            median.as_millis() < 500,
            "dedup must run in < 500ms (median of 3) on 10k records (got {median:?}; samples {samples:?}) — quadratic regression suspected"
        );
        assert_eq!(last_result.len(), 5_000);
        assert!(
            last_result
                .iter()
                .all(|r| !r.summary.contains("...[truncated]"))
        );
    }

    #[test]
    fn test_drop_truncated_dedup_keeps_fullest() {
        // Two truncated and one full; the full survives even when ordered
        // last in the input.
        let session = "sess-fullest";
        let chunk = "/tmp/fullest.md";
        let mut records = vec![
            make_record(
                IntentKind::Decision,
                "keep canonical store at ~/.aicx/store and...[truncated]",
                session,
                chunk,
            ),
            make_record(
                IntentKind::Decision,
                "keep canonical store at ~/.aicx/store and rebuild...[truncated]",
                session,
                chunk,
            ),
            make_record(
                IntentKind::Decision,
                "keep canonical store at ~/.aicx/store and rebuild semantic index nightly",
                session,
                chunk,
            ),
        ];

        drop_truncated_duplicate_records(&mut records);

        assert_eq!(records.len(), 1);
        assert!(!records[0].summary.contains("...[truncated]"));
        assert!(records[0].summary.ends_with("nightly"));
    }
}

// ── Area E.4 / E.9 / E.10 / E.11 regression coverage ────────────────

#[cfg(test)]
mod area_e_regressions {
    use super::*;

    fn make_record(kind: IntentKind, summary: &str) -> IntentRecord {
        IntentRecord {
            kind,
            summary: summary.to_string(),
            context: None,
            evidence: Vec::new(),
            project: "demo".to_string(),
            agent: "claude".to_string(),
            date: "2026-05-20".to_string(),
            timestamp: None,
            session_id: "sess".to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: "chunk-1".to_string(),
            source: None,
        }
    }

    #[test]
    fn dedup_intent_records_collapses_zero_width_variants() {
        // "fix au\u{200B}th" must dedup against "fix auth" — E.11 + E.12.
        let mut records = vec![
            make_record(IntentKind::Intent, "fix auth"),
            make_record(IntentKind::Intent, "fix au\u{200B}th"),
            make_record(IntentKind::Intent, "FIX  AUTH"),
        ];
        dedup_intent_records(&mut records);
        assert_eq!(
            records.len(),
            1,
            "dedup should collapse all three variants of 'fix auth', got {:?}",
            records
                .iter()
                .map(|r| r.summary.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn dedup_intent_records_preserves_distinct_kinds() {
        let mut records = vec![
            make_record(IntentKind::Intent, "fix auth"),
            make_record(IntentKind::Decision, "fix auth"),
        ];
        dedup_intent_records(&mut records);
        // Different kinds → keep both even when summary normalizes equal.
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn merge_evidence_deduplicates_without_quadratic_rebuild() {
        // E.4: merge of large additions must succeed without exploding;
        // we cannot directly measure complexity but we can assert the
        // logical behavior — final vector has no normalized duplicates.
        let mut existing: Vec<String> = (0..200).map(|i| format!("evidence line {i}")).collect();
        let additions: Vec<String> = (100..300)
            .map(|i| format!("EVIDENCE LINE {i}")) // case-different overlap
            .collect();
        merge_evidence(&mut existing, additions);
        // 0..200 already present, then 200..300 added → 300 unique.
        assert_eq!(existing.len(), 300);
        // No two entries share a normalized key.
        let mut keys: Vec<String> = existing.iter().map(|s| normalize_key(s)).collect();
        keys.sort();
        let unique_before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), unique_before, "merge_evidence left duplicates");
    }

    #[test]
    fn classify_line_entry_type_rejects_tests_meta_discussion() {
        // E.9: bare "we need to write tests for the auth flow" — no digits,
        // no PASS/FAIL — must NOT classify as Result.
        let result = classify_line_entry_type("we need to write tests for the auth flow", false);
        assert!(
            !matches!(result, Some((EntryType::Result, _))),
            "soft 'tests' marker without shape must not classify as Result, got {result:?}"
        );
    }

    #[test]
    fn classify_line_entry_type_accepts_tests_with_shape() {
        // E.9: same soft marker but with digits → Result is acceptable.
        let result = classify_line_entry_type("tests 276/276 passed", false);
        assert!(
            matches!(result, Some((EntryType::Result, _))),
            "tests + digits should classify as Result, got {result:?}"
        );
    }

    #[test]
    fn classify_line_entry_type_rejects_bare_error_meta() {
        // "should we treat this as an error: ?" is meta-discussion.
        let result = classify_line_entry_type("should we treat this as an error: ?", false);
        assert!(
            !matches!(result, Some((EntryType::Result, _))),
            "bare 'error:' meta-discussion must not classify as Result, got {result:?}"
        );
    }

    #[test]
    fn is_outcome_line_skips_bare_affirmation() {
        // E.10: "Zrobione" alone is not an outcome.
        assert!(!is_outcome_line("Zrobione"));
        assert!(!is_outcome_line("- Done"));
        assert!(!is_outcome_line("Gotowe."));
        // But with detail (colon + content) it counts again.
        assert!(is_outcome_line("Zrobione: build green"));
    }

    fn make_entry(id: &str, session: &str, entry_type: EntryType, title: &str) -> IntentEntry {
        IntentEntry {
            id: id.to_string(),
            entry_type,
            state: EntryState::Active,
            title: title.to_string(),
            body: None,
            evidence: Vec::new(),
            links: Vec::new(),
            superseded_by: None,
            confidence: 0.9,
            tags: Vec::new(),
            project: Some("demo".to_string()),
            agent: Some("claude".to_string()),
            session_id: Some(session.to_string()),
            timestamp: Some("2026-05-20T10:00:00Z".to_string()),
            date: "2026-05-20".to_string(),
            source_chunk: "chunk-1".to_string(),
        }
    }

    #[test]
    fn detect_contradicted_assumptions_groups_by_session() {
        // E.5: assumption + result in the SAME session should link;
        // a result in a DIFFERENT session must not contradict.
        let mut entries = vec![
            make_entry(
                "e1",
                "sess-A",
                EntryType::Assumption,
                "auth tokens never expire in dev",
            ),
            make_entry(
                "e2",
                "sess-A",
                EntryType::Result,
                "auth tokens fail to refresh in dev",
            ),
            make_entry(
                "e3",
                "sess-B",
                EntryType::Result,
                "auth tokens broken in prod too",
            ),
        ];
        detect_contradicted_assumptions(&mut entries);
        assert_eq!(entries[0].state, EntryState::Contradicted);
        assert!(
            entries[0].links.iter().any(|l| l.target == "e2"),
            "expected contradiction link to sess-A result e2, got {:?}",
            entries[0].links
        );
        assert!(
            !entries[0].links.iter().any(|l| l.target == "e3"),
            "cross-session result e3 must not contradict, got {:?}",
            entries[0].links
        );
    }

    #[test]
    fn detect_contradicted_assumptions_handles_no_overlap_cleanly() {
        // Assumption + Result in same session but with <2 word overlap
        // must NOT mark as contradicted.
        let mut entries = vec![
            make_entry("e1", "sess-A", EntryType::Assumption, "deployment is safe"),
            make_entry(
                "e2",
                "sess-A",
                EntryType::Result,
                "auth tokens fail to refresh",
            ),
        ];
        detect_contradicted_assumptions(&mut entries);
        assert_eq!(entries[0].state, EntryState::Active);
        assert!(entries[0].links.is_empty());
    }
}

// ── P3-09: flexible UTC date parsing for mixed-format comparisons ────

mod flexible_dates {
    use super::*;

    fn make_record(summary: &str, date: &str, timestamp: Option<&str>) -> IntentRecord {
        IntentRecord {
            kind: IntentKind::Intent,
            summary: summary.to_string(),
            context: None,
            evidence: Vec::new(),
            project: "demo".to_string(),
            agent: "codex".to_string(),
            date: date.to_string(),
            timestamp: timestamp.map(|t| t.to_string()),
            session_id: format!("sess-{summary}"),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: format!("/tmp/demo/{summary}.md"),
            source: None,
        }
    }

    #[test]
    fn parse_flexible_utc_normalizes_bare_dates_and_offsets() {
        let day = parse_flexible_utc("2026-05-04").expect("bare date must parse");
        assert_eq!(day.to_rfc3339(), "2026-05-04T00:00:00+00:00");

        // +02:00 offset normalizes to 21:30Z, which is BEFORE 22:00Z even
        // though the raw strings compare the other way lexicographically.
        let offset = parse_flexible_utc("2026-05-04T23:30:00+02:00").expect("offset must parse");
        let zulu = parse_flexible_utc("2026-05-04T22:00:00Z").expect("zulu must parse");
        assert!("2026-05-04T23:30:00+02:00" > "2026-05-04T22:00:00Z");
        assert!(offset < zulu, "typed comparison must use the UTC axis");

        assert_eq!(parse_flexible_utc("not-a-date"), None);
        assert_eq!(parse_flexible_utc("2026-13-99"), None);
    }

    #[test]
    fn sort_mixes_date_only_and_timestamped_records_consistently() {
        // Oldest-first expectation on the UTC axis:
        //   date-only (midnight UTC) < 21:30Z (23:30+02:00) < 22:00Z.
        // The old lexicographic sort produced date-only < 22:00Z < 23:30+02:00.
        let records = vec![
            make_record(
                "late-offset",
                "2026-05-04",
                Some("2026-05-04T23:30:00+02:00"),
            ),
            make_record("zulu", "2026-05-04", Some("2026-05-04T22:00:00Z")),
            make_record("date-only", "2026-05-04", None),
        ];

        let oldest = apply_display_filters(
            records.clone(),
            &IntentDisplayFilters {
                sort: Some(IntentSortOrder::Oldest),
                ..Default::default()
            },
        );
        let oldest_order: Vec<&str> = oldest.iter().map(|r| r.summary.as_str()).collect();
        assert_eq!(
            oldest_order,
            vec!["date-only", "late-offset", "zulu"],
            "Oldest sort must order date-only (midnight) before 21:30Z before 22:00Z"
        );

        let newest = apply_display_filters(
            records,
            &IntentDisplayFilters {
                sort: Some(IntentSortOrder::Newest),
                ..Default::default()
            },
        );
        let newest_order: Vec<&str> = newest.iter().map(|r| r.summary.as_str()).collect();
        assert_eq!(
            newest_order,
            vec!["zulu", "late-offset", "date-only"],
            "Newest sort must be the exact reverse of Oldest"
        );
    }

    #[test]
    fn date_range_filter_compares_on_utc_axis() {
        // 01:00+03:00 on 2026-05-04 is 22:00Z on 2026-05-03, so a
        // lo=2026-05-04 bound must EXCLUDE it. The old lexicographic
        // comparison kept it ("2026-05-04T..." >= "2026-05-04").
        let records = vec![
            make_record("pre-window", "2026-05-04T01:00:00+03:00", None),
            make_record("in-window", "2026-05-04", None),
            make_record("garbage-date", "someday", None),
        ];

        let filtered = apply_display_filters(
            records,
            &IntentDisplayFilters {
                date_lo: Some("2026-05-04".to_string()),
                date_hi: Some("2026-05-05".to_string()),
                ..Default::default()
            },
        );
        let kept: Vec<&str> = filtered.iter().map(|r| r.summary.as_str()).collect();
        assert_eq!(
            kept,
            vec!["in-window"],
            "offset timestamp before the UTC window and unparsable dates must be dropped"
        );
    }

    #[test]
    fn test_codescribe_parser_voice_provenance() {
        use super::CodescribeParser;
        let mut parser = CodescribeParser::new();

        // Single line tag
        let (cleaned, is_voice) =
            parser.process("<codescribe mode=\"voice\" lang=\"pl\">Pamiętaj o tym</codescribe>");
        assert_eq!(cleaned, "Pamiętaj o tym");
        assert!(is_voice);

        // Multi-line tag
        let mut parser_multi = CodescribeParser::new();
        let (cleaned1, is_voice1) = parser_multi.process("<codescribe mode=\"voice\">");
        assert_eq!(cleaned1, "");
        assert!(is_voice1);

        let (cleaned2, is_voice2) = parser_multi.process("Pojedyncza linia");
        assert_eq!(cleaned2, "Pojedyncza linia");
        assert!(is_voice2);

        let (cleaned3, is_voice3) = parser_multi.process("</codescribe>");
        assert_eq!(cleaned3, "");
        assert!(is_voice3);

        let (cleaned4, is_voice4) = parser_multi.process("zwykły tekst");
        assert_eq!(cleaned4, "zwykły tekst");
        assert!(!is_voice4);
    }

    #[test]
    fn test_garble_gate_repro_degradation() {
        use super::{IntentKind, StoredChunkFile, build_candidate};
        use chrono::Utc;
        use std::path::PathBuf;

        let text_repro = "Pamiętaj, że chcę to wrzucić w Injust mechanizm launchera VC-Ship, który zgodnie z moim specem dowiezie to od Arozet w całej intencjonalnej read/write kadensy. Zabezpieczam.";
        let chunk_file = StoredChunkFile {
            agent: "agy".to_string(),
            date: "2026-06-12".to_string(),
            path: PathBuf::from("chunk.md"),
            project: "aicx".to_string(),
            sequence: 1,
            timestamp: Utc::now(),
            session_id: "sess-1".to_string(),
        };

        // Repro case: no context, no evidence -> degraded (returns None)
        let candidate = build_candidate(
            IntentKind::Intent,
            text_repro,
            None,
            &chunk_file,
            "aicx",
            "chunk.md",
            false,
            Some("voice_transcript".to_string()),
        );
        assert!(
            candidate.is_none(),
            "Garbled transcription without context or evidence must be degraded/dropped"
        );

        // If it has evidence or context, it is preserved (just in case there's real intent)
        let candidate_with_context = build_candidate(
            IntentKind::Intent,
            text_repro,
            Some("Why: user explicitly requested SF strategy".to_string()),
            &chunk_file,
            "aicx",
            "chunk.md",
            false,
            Some("voice_transcript".to_string()),
        );
        assert!(
            candidate_with_context.is_some(),
            "Intent with context should not be blindly degraded"
        );
    }

    #[test]
    fn signals_revalidation_gate_classifier_wins_on_disagreement() {
        // Round II / oś 1: [signals] section headers are a strong HINT, not
        // ground truth. A line whose section says `Results:`/`Outcome:` must
        // still pass the shared classifier; on confident disagreement the
        // classifier wins (operator decision 2026-06-21).
        use super::{IntentKind, StoredChunkFile, extract_signal_candidates};
        use chrono::Utc;
        use std::path::PathBuf;

        let chunk_file = StoredChunkFile {
            agent: "codex".to_string(),
            date: "2026-06-21".to_string(),
            path: PathBuf::from("chunk.md"),
            project: "aicx".to_string(),
            sequence: 1,
            timestamp: Utc::now(),
            session_id: "sess-gate".to_string(),
        };

        let signal_lines: Vec<String> = vec![
            "Results:".to_string(),
            // (a) a question wrongly filed under Results: must NOT become outcome
            "- Czy ten gate dziala poprawnie i ma dla nas sens?".to_string(),
            // (b) a genuine result under Results: must stay outcome
            "- result: cargo test 825 passed, 0 failed".to_string(),
            "Outcome:".to_string(),
            // (c) an assumption wrongly filed under Outcome: must be dropped
            "- zakladam ze to zadziala bez problemow".to_string(),
        ];

        let (candidates, _tasks) =
            extract_signal_candidates(&chunk_file, "aicx", "chunk.md", &signal_lines);

        // (a) the question is reclassified to Intent, not Outcome, with provenance
        let question = candidates
            .iter()
            .find(|c| c.record.summary.to_lowercase().contains("gate dziala"))
            .expect("question candidate should survive (reclassified, not dropped)");
        assert_eq!(
            question.record.kind,
            IntentKind::Intent,
            "question under Results: must be reclassified to Intent, not Outcome"
        );
        assert!(
            question
                .record
                .source
                .as_deref()
                .is_some_and(|s| s.contains("revalidated")),
            "reclassified signal must carry revalidation provenance, got {:?}",
            question.record.source
        );

        // (b) the genuine result stays Outcome
        let result = candidates
            .iter()
            .find(|c| c.record.summary.to_lowercase().contains("825 passed"))
            .expect("genuine result candidate should survive as outcome");
        assert_eq!(result.record.kind, IntentKind::Outcome);
        assert!(
            result
                .record
                .source
                .as_deref()
                .is_some_and(|s| s.contains("signals")),
            "every signal-sourced record must carry signal provenance, got {:?}",
            result.record.source
        );

        // (c) the assumption filed under Outcome: is dropped (classifier maps it
        // to a non-bucket role)
        assert!(
            !candidates
                .iter()
                .any(|c| c.record.summary.to_lowercase().contains("zakladam")),
            "assumption wrongly under Outcome: must be dropped, not emitted as outcome"
        );
    }

    #[test]
    fn commit_block_indices_run_rule() {
        use super::commit_block_indices;
        let to_lines = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // a run of >=2 commit-log lines -> all suppressed
        let block = commit_block_indices(&to_lines(&[
            "Add Windows-compatible file locking",
            "Fix migration artifact path validation",
            "feat(search): anchored ranking",
        ]));
        assert_eq!(block.len(), 3);

        // a lone commit-style imperative is NOT suppressed (may be a real task)
        let lone = commit_block_indices(&to_lines(&["Add retry logic to the search client"]));
        assert!(lone.is_empty(), "lone imperative must stay a candidate");

        // lowercase prose 'add' is not a commit-subject verb
        let prose = commit_block_indices(&to_lines(&[
            "dodaj prosze walidacje",
            "add some tests when you can",
        ]));
        assert!(
            prose.is_empty(),
            "lowercase prose must not form a commit block"
        );
    }

    #[test]
    fn commit_list_block_is_not_classified_as_tasks() {
        // Round II / oś 2 cut 2 — document-role awareness: a pasted run of
        // commit/changelog lines (>=2 consecutive) is historical reference, not
        // operator tasks. A lone real task in another turn must still survive.
        let tmp = migration_test_root("commit-block-not-tasks");
        let _ = fs::remove_dir_all(&tmp);

        let chunk = "[project: demo | agent: codex | date: 2026-03-15 | frame_kind: user_msg]\n\n\
[12:00:00] user: dodaj prosze walidacje do importera operator-md\n\
[12:01:00] user: ostatnie commity ktore wrzucam dla kontekstu:\n\
Add Windows-compatible file locking and process checking\n\
Fix migration artifact path validation\n\
Update Cargo.lock dependencies\n";

        write_chunk(&tmp, "demo", "2026-03-15", "120000_codex-001.md", chunk);

        let config = IntentsConfig {
            project: "demo".to_string(),
            hours: 24,
            strict: false,
            min_confidence: None,
            kind_filter: None,
            frame_kind: None,
        };
        let now = DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 3, 15)
                .expect("date")
                .and_hms_opt(13, 0, 0)
                .expect("time"),
            Utc,
        );

        let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");

        for needle in ["windows-compatible", "migration artifact", "cargo.lock"] {
            assert!(
                !records
                    .iter()
                    .any(|r| r.summary.to_lowercase().contains(needle)),
                "commit-list line '{needle}' must not become a record: {records:?}"
            );
        }
        // the genuine Polish task in a separate turn survives
        assert!(
            records
                .iter()
                .any(|r| r.kind == IntentKind::Task
                    && r.summary.to_lowercase().contains("walidacje")),
            "real operator task must survive document-role filtering: {records:?}"
        );
    }

    #[test]
    fn is_code_fragment_line_contract() {
        use super::is_code_fragment_line;
        // code fragments -> true
        assert!(is_code_fragment_line("field(default_factory=list)"));
        assert!(is_code_fragment_line(
            "- DEFAULT_KEYWORDS_PATH = \"/etc/keywords\""
        ));
        assert!(is_code_fragment_line("MAX_RETRIES"));
        // prose -> false (even when it mentions a CONSTANT or a config word)
        assert!(!is_code_fragment_line(
            "musimy ustawic nowy DEFAULT_KEYWORDS_PATH w configu"
        ));
        assert!(!is_code_fragment_line(
            "od teraz importujemy tylko przez operator-md"
        ));
        assert!(!is_code_fragment_line("batch 10 plikow dal 66 records"));
        // a bare cap word without underscore is not constant-case
        assert!(!is_code_fragment_line("OK to powinno dzialac dobrze tej"));
    }

    #[test]
    fn signals_code_fragment_lines_are_dropped() {
        // Round II / oś 2 cut 1: code/log fragments under a [signals] section
        // header must NOT become records (they abstain in the classifier, so the
        // section hint would otherwise be honored). "default" in
        // DEFAULT_KEYWORDS_PATH / field(default_factory=list) previously made
        // them decisions.
        use super::{StoredChunkFile, extract_signal_candidates};
        use chrono::Utc;
        use std::path::PathBuf;

        let chunk_file = StoredChunkFile {
            agent: "codex".to_string(),
            date: "2026-06-21".to_string(),
            path: PathBuf::from("chunk.md"),
            project: "aicx".to_string(),
            sequence: 1,
            timestamp: Utc::now(),
            session_id: "sess-code".to_string(),
        };

        let signal_lines: Vec<String> = vec![
            "Decision:".to_string(),
            "- field(default_factory=list)".to_string(),
            "- DEFAULT_KEYWORDS_PATH = \"/etc/keywords\"".to_string(),
            // a genuine prose decision in the same section must survive
            "- od teraz importujemy tylko przez operator-md".to_string(),
        ];

        let (candidates, _tasks) =
            extract_signal_candidates(&chunk_file, "aicx", "chunk.md", &signal_lines);

        assert!(
            !candidates
                .iter()
                .any(|c| c.record.summary.to_lowercase().contains("default")),
            "code fragments containing 'default' must be dropped, got {:?}",
            candidates
                .iter()
                .map(|c| &c.record.summary)
                .collect::<Vec<_>>()
        );
        assert!(
            candidates
                .iter()
                .any(|c| c.record.summary.to_lowercase().contains("operator-md")),
            "genuine prose decision must survive the code-fragment guard"
        );
    }

    #[test]
    fn signals_revalidation_gate_e2e_question_under_results_not_outcome() {
        // Round II / oś 1 — pipeline-level guard: a full canonical chunk with a
        // [signals] Results: block carrying a question must NOT surface that
        // question as an outcome record after the whole extraction pipeline
        // (parse -> signals+raw -> dedup -> records), while a genuine result in
        // the same block is preserved.
        let tmp = migration_test_root("signals-gate-e2e-question-results");
        let _ = fs::remove_dir_all(&tmp);

        let chunk = r#"[project: demo | agent: codex | date: 2026-03-15 | frame_kind: user_msg]

[signals]
Results:
- Czy ten gate dziala poprawnie i ma dla nas sens?
- result: cargo test 825 passed, 0 failed
[/signals]

[12:00:00] user: musimy dodac walidacje signali w pipeline
"#;

        write_chunk(&tmp, "demo", "2026-03-15", "120000_codex-001.md", chunk);

        let config = IntentsConfig {
            project: "demo".to_string(),
            hours: 24,
            strict: false,
            min_confidence: None,
            kind_filter: None,
            frame_kind: None,
        };
        let now = DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 3, 15)
                .expect("date")
                .and_hms_opt(13, 0, 0)
                .expect("time"),
            Utc,
        );

        let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");

        // the question under Results: must not be an outcome anywhere in the pipeline output
        assert!(
            !records.iter().any(|r| r.kind == IntentKind::Outcome
                && r.summary.to_lowercase().contains("gate dziala")),
            "question under [signals] Results: must not become an outcome: {records:?}"
        );
        // the genuine result in the same block survives as an outcome
        assert!(
            records.iter().any(|r| r.kind == IntentKind::Outcome
                && r.summary.to_lowercase().contains("825 passed")),
            "genuine result under [signals] Results: must remain an outcome: {records:?}"
        );
    }

    #[test]
    fn test_voice_intent_sorts_lower() {
        use super::sort_intent_records;
        let mut r1 = make_record("Intent voice", "2026-06-12", None);
        r1.source = Some("voice_transcript".to_string());

        let mut r2 = make_record("Intent written", "2026-06-12", None);
        r2.source = None;

        let mut records = vec![r1.clone(), r2.clone()];
        sort_intent_records(&mut records);

        // Written should come first
        assert_eq!(records[0].summary, "Intent written");
        assert_eq!(records[1].summary, "Intent voice");
    }

    #[test]
    fn test_unresolved_mode_intent_vs_session() {
        let records = vec![
            IntentRecord {
                kind: IntentKind::Intent,
                summary: "implement search".to_string(),
                context: None,
                evidence: vec![],
                project: "demo".to_string(),
                agent: "codex".to_string(),
                date: "2026-06-12".to_string(),
                timestamp: None,
                session_id: "sess-1".to_string(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk: "chunk1.md".to_string(),
                source: None,
            },
            IntentRecord {
                kind: IntentKind::Intent,
                summary: "fix login".to_string(),
                context: None,
                evidence: vec![],
                project: "demo".to_string(),
                agent: "codex".to_string(),
                date: "2026-06-12".to_string(),
                timestamp: None,
                session_id: "sess-1".to_string(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk: "chunk1.md".to_string(),
                source: None,
            },
            IntentRecord {
                kind: IntentKind::Outcome,
                summary: "search was implemented successfully".to_string(),
                context: None,
                evidence: vec![],
                project: "demo".to_string(),
                agent: "codex".to_string(),
                date: "2026-06-12".to_string(),
                timestamp: None,
                session_id: "sess-1".to_string(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk: "chunk2.md".to_string(),
                source: None,
            },
        ];

        // 1. Session Mode: session contains Outcome, so all intents are filtered out
        let session_filtered = apply_display_filters(
            records.clone(),
            &IntentDisplayFilters {
                unresolved: true,
                unresolved_mode: UnresolvedMode::Session,
                ..Default::default()
            },
        );
        let has_intents = session_filtered
            .iter()
            .any(|r| r.kind == IntentKind::Intent);
        assert!(
            !has_intents,
            "Session mode should filter out all intents for resolved session"
        );

        // 2. Intent Mode: only "implement search" has a matching outcome, "fix login" should survive!
        let intent_filtered = apply_display_filters(
            records,
            &IntentDisplayFilters {
                unresolved: true,
                unresolved_mode: UnresolvedMode::Intent,
                ..Default::default()
            },
        );
        let remaining_intents: Vec<_> = intent_filtered
            .iter()
            .filter(|r| r.kind == IntentKind::Intent)
            .map(|r| r.summary.as_str())
            .collect();
        assert_eq!(remaining_intents, vec!["fix login"]);
    }

    #[test]
    fn test_kind_plus_unresolved_combination() {
        let records = vec![
            IntentRecord {
                kind: IntentKind::Intent,
                summary: "implement search".to_string(),
                context: None,
                evidence: vec![],
                project: "demo".to_string(),
                agent: "codex".to_string(),
                date: "2026-06-12".to_string(),
                timestamp: None,
                session_id: "sess-1".to_string(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk: "chunk1.md".to_string(),
                source: None,
            },
            IntentRecord {
                kind: IntentKind::Intent,
                summary: "fix login".to_string(),
                context: None,
                evidence: vec![],
                project: "demo".to_string(),
                agent: "codex".to_string(),
                date: "2026-06-12".to_string(),
                timestamp: None,
                session_id: "sess-1".to_string(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk: "chunk1.md".to_string(),
                source: None,
            },
            IntentRecord {
                kind: IntentKind::Outcome,
                summary: "search was implemented successfully".to_string(),
                context: None,
                evidence: vec![],
                project: "demo".to_string(),
                agent: "codex".to_string(),
                date: "2026-06-12".to_string(),
                timestamp: None,
                session_id: "sess-1".to_string(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk: "chunk2.md".to_string(),
                source: None,
            },
        ];

        // Apply unresolved (intent mode) and then kind filter (Intents only).
        // It must NOT return empty because "fix login" remains unresolved.
        let mut filtered = apply_display_filters(
            records,
            &IntentDisplayFilters {
                unresolved: true,
                unresolved_mode: UnresolvedMode::Intent,
                ..Default::default()
            },
        );
        // Defer kind filter simulation
        filtered.retain(|r| r.kind == IntentKind::Intent);

        let summaries: Vec<_> = filtered.iter().map(|r| r.summary.as_str()).collect();
        assert_eq!(summaries, vec!["fix login"]);
    }

    #[test]
    fn test_strict_confidence_threshold() {
        use super::dedup_candidates;
        use super::types::IntentCandidate;

        let chunk_file = StoredChunkFile {
            agent: "agy".to_string(),
            date: "2026-06-12".to_string(),
            path: PathBuf::from("chunk.md"),
            project: "aicx".to_string(),
            sequence: 1,
            timestamp: Utc::now(),
            session_id: "sess-1".to_string(),
        };

        // 1. Voice transcript intent without context/evidence (confidence 2)
        let c1 = IntentCandidate {
            record: IntentRecord {
                kind: IntentKind::Intent,
                summary: "low confidence intent".to_string(),
                context: None,
                evidence: vec![],
                project: "aicx".to_string(),
                agent: "agy".to_string(),
                date: "2026-06-12".to_string(),
                timestamp: None,
                session_id: "sess-1".to_string(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk: "chunk.md".to_string(),
                source: Some("voice_transcript".to_string()),
            },
            confidence: 2,
            timestamp: chunk_file.timestamp,
        };

        // 2. High confidence intent (confidence 4)
        let c2 = IntentCandidate {
            record: IntentRecord {
                kind: IntentKind::Intent,
                summary: "high confidence intent".to_string(),
                context: Some("explicit instruction".to_string()),
                evidence: vec![],
                project: "aicx".to_string(),
                agent: "agy".to_string(),
                date: "2026-06-12".to_string(),
                timestamp: None,
                session_id: "sess-1".to_string(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk: "chunk.md".to_string(),
                source: None,
            },
            confidence: 4,
            timestamp: chunk_file.timestamp,
        };

        let candidates = vec![c1, c2];

        // With strict = false, both kept
        let all = dedup_candidates(candidates.clone(), false, None, None);
        assert_eq!(all.len(), 2);

        // With strict = true (requires confidence >= 4), only high confidence kept
        let strict_records = dedup_candidates(candidates.clone(), true, None, None);
        assert_eq!(strict_records.len(), 1);
        assert_eq!(strict_records[0].summary, "high confidence intent");

        // With min_confidence = Some(3), only high confidence kept (confidence 4 >= 3)
        let min_conf_records = dedup_candidates(candidates, false, Some(3), None);
        assert_eq!(min_conf_records.len(), 1);
        assert_eq!(min_conf_records[0].summary, "high confidence intent");
    }
}
