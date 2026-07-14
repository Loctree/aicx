use super::support::{
    parse_artifact_frontmatter, prompt_workflow_slug, stem_workflow_slug, title_workflow_slug,
};
use super::*;
use chrono::{NaiveDate, Utc};
use std::fs;
use std::path::{Path, PathBuf};

fn tmp_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-reports-extractor-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, content).expect("write file");
}

#[test]
fn parses_artifact_frontmatter_and_status() {
    let input = "---\nagent: codex\nrun_id: wf-001\nprompt_id: prompt-001\nstatus: completed\ncreated: 2026-04-12T20:11:06+02:00\nmode: implement\nskill_code: vc-workflow\n---\n# Report\nBody";
    let (frontmatter, body) = parse_artifact_frontmatter(input);
    let frontmatter = frontmatter.expect("frontmatter");

    assert_eq!(frontmatter.status.as_deref(), Some("completed"));
    assert_eq!(
        frontmatter.created.as_deref(),
        Some("2026-04-12T20:11:06+02:00")
    );
    assert_eq!(
        frontmatter.report.telemetry.run_id.as_deref(),
        Some("wf-001")
    );
    assert_eq!(
        frontmatter.report.steering.skill_code.as_deref(),
        Some("vc-workflow")
    );
    assert_eq!(body, "# Report\nBody");
}

#[test]
fn build_reports_explorer_merges_markdown_and_meta_and_keeps_meta_only_runs() {
    let root = tmp_dir("merge-meta");
    let repo_root = root.join("Vetcoders").join("ai-contexters");
    let report_path = repo_root
        .join("2026_0412")
        .join("reports")
        .join("20260412_feature_codex.md");
    let meta_path = repo_root
        .join("2026_0412")
        .join("reports")
        .join("20260412_feature_codex.meta.json");
    let launching_meta = repo_root
        .join("2026_0411")
        .join("marbles")
        .join("reports")
        .join("20260411_1316_marbles-ancestor_L1_codex.meta.json");
    let transcript = repo_root
        .join("2026_0411")
        .join("marbles")
        .join("reports")
        .join("20260411_1316_marbles-ancestor_L1_codex.transcript.log");

    write_file(
        &report_path,
        "---\nagent: codex\nrun_id: wf-20260412-001\nprompt_id: report-artifacts\nstatus: completed\ncreated: 2026-04-12T20:11:06+02:00\nskill_code: vc-workflow\n---\n# Report Artifacts Dashboard\n## Findings\n- build static HTML\n",
    );
    write_file(
        &meta_path,
        r#"{
  "status": "completed",
  "agent": "codex",
  "run_id": "wf-20260412-001",
  "prompt_id": "report-artifacts",
  "skill_code": "impl",
  "duration_s": 12.5
}"#,
    );
    write_file(
        &launching_meta,
        &r#"{
  "status": "launching",
  "agent": "codex",
  "run_id": "marb-131611-001",
  "prompt_id": "marbles-ancestor_L1_20260411",
  "transcript": __TRANSCRIPT__
}"#
        .replace(
            "__TRANSCRIPT__",
            // serde_json quotes + escapes the path so a Windows path with
            // backslashes (`C:\…`) stays valid JSON; on Unix the value is
            // byte-identical to the previous raw embedding.
            &serde_json::to_string(&transcript.display().to_string())
                .expect("encode transcript path as JSON"),
        ),
    );
    write_file(&transcript, "[13:16:11] assistant: booting artifact scan\n");

    let config = ReportsExtractorConfig {
        artifacts_root: root.clone(),
        org: "Vetcoders".to_string(),
        repo: "ai-contexters".to_string(),
        date_from: Some(NaiveDate::from_ymd_opt(2026, 4, 11).expect("date")),
        date_to: Some(NaiveDate::from_ymd_opt(2026, 4, 12).expect("date")),
        workflow: None,
        title: "AICX Reports Explorer".to_string(),
        preview_chars: 120,
        deterministic: false,
    };

    let artifact = build_reports_explorer(&config).expect("build reports explorer");
    let payload: ReportsExplorerPayload =
        serde_json::from_str(&artifact.bundle_json).expect("parse bundle");

    assert_eq!(payload.records.len(), 2);
    assert!(
        payload
            .records
            .iter()
            .any(|record| record.has_markdown && record.has_meta)
    );
    assert!(
        payload
            .records
            .iter()
            .any(|record| !record.has_markdown && record.has_meta)
    );
    assert!(
        payload
            .records
            .iter()
            .any(|record| record.workflow == "report-artifacts")
    );
    assert!(
        payload
            .records
            .iter()
            .all(|record| record.workflow != "day-root")
    );
    assert!(artifact.html.contains("Workflow Report Explorer"));
    assert!(artifact.html.contains("Import JSON Bundle"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn build_reports_explorer_does_not_read_transcripts_outside_repo_root() {
    let root = tmp_dir("outside-transcript");
    let repo_root = root.join("Vetcoders").join("ai-contexters");
    let meta_path = repo_root
        .join("2026_0411")
        .join("reports")
        .join("20260411_external_transcript.meta.json");
    let outside = tmp_dir("outside-transcript-secret").join("secret.log");

    write_file(&outside, "outside secret that must not be imported\n");
    write_file(
        &meta_path,
        &r#"{
  "status": "launching",
  "agent": "codex",
  "run_id": "marb-outside-transcript",
  "prompt_id": "outside-transcript",
  "transcript": __TRANSCRIPT__
}"#
        .replace(
            "__TRANSCRIPT__",
            // serde_json quotes + escapes so a Windows path stays valid JSON.
            &serde_json::to_string(&outside.display().to_string())
                .expect("encode transcript path as JSON"),
        ),
    );

    let config = ReportsExtractorConfig {
        artifacts_root: root.clone(),
        org: "Vetcoders".to_string(),
        repo: "ai-contexters".to_string(),
        date_from: None,
        date_to: None,
        workflow: None,
        title: "AICX Reports Explorer".to_string(),
        preview_chars: 120,
        deterministic: false,
    };

    let artifact = build_reports_explorer(&config).expect("build reports explorer");
    let payload: ReportsExplorerPayload =
        serde_json::from_str(&artifact.bundle_json).expect("parse bundle");
    let record = payload.records.first().expect("record");

    assert!(!record.has_transcript);
    assert!(record.transcript_path.is_none());
    assert!(!record.detail_text.contains("outside secret"));

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(outside.parent().expect("outside parent"));
}

#[test]
fn infers_day_root_workflow_from_prompt_ids_and_file_stems() {
    assert_eq!(
        prompt_workflow_slug(Some("report-artifacts-dashboard_20260412")).as_deref(),
        Some("report-artifacts-dashboard")
    );
    assert_eq!(
        prompt_workflow_slug(Some(
            "20260612_0810_improve-aicx-installer-output-ux_20260612"
        ))
        .as_deref(),
        Some("improve-aicx-installer-output-ux")
    );
    assert_eq!(
        prompt_workflow_slug(Some(
            "20260612_0810_perform-the-vc-justdo-skill-on-this-repository_20260612"
        )),
        None
    );
    assert_eq!(
        stem_workflow_slug(Path::new(
            "/tmp/20260412_2031_report-artifacts-dashboard_codex.md"
        ))
        .as_deref(),
        Some("report-artifacts-dashboard")
    );
    for path in [
        "/tmp/20260612_075248_just-075248-9497_improve-aicx-installer-output-ux_codex.md",
        "/tmp/20260612_075248_just-075248-9497_improve-aicx-installer-output-ux_codex.meta.json",
        "/tmp/20260612_075248_just-075248-9497_improve-aicx-installer-output-ux_codex.transcript.log",
        "/tmp/20260612_075248_just-075248-9497_improve-aicx-installer-output-ux_codex_launch.sh",
    ] {
        assert_eq!(
            stem_workflow_slug(Path::new(path)).as_deref(),
            Some("improve-aicx-installer-output-ux"),
            "{path}"
        );
    }
    assert_eq!(
        title_workflow_slug("Examination: report artifacts dashboard").as_deref(),
        Some("examination-report-artifacts-dashboard")
    );
}
