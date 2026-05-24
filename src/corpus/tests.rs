use super::*;
use crate::corpus::noise::repair_markdown_content;
use crate::corpus::types::NoiseClass;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn tmp_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-corpus-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn repair_drops_empty_claude_thinking_signature_line() {
    let input =
        "before\n{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"abc123\"}\nafter\n";
    let (repaired, removed) = repair_markdown_content(input);
    assert_eq!(repaired, "before\nafter\n");
    assert!(removed.contains(&NoiseClass::Signature));
    assert!(removed.contains(&NoiseClass::EmptyThinking));
    assert!(removed.contains(&NoiseClass::InlineThinkingJson));
}

#[test]
fn repair_preserves_thinking_text_but_removes_signature_field() {
    let input =
        "{\"type\":\"thinking\",\"thinking\":\"useful private note\",\"signature\":\"abc123\"}\n";
    let (repaired, removed) = repair_markdown_content(input);
    assert_eq!(repaired, "useful private note\n");
    assert!(!repaired.contains("abc123"));
    assert!(removed.contains(&NoiseClass::Signature));
    assert!(removed.contains(&NoiseClass::InlineThinkingJson));
}

#[test]
fn repair_apply_writes_sidecar_metadata_and_manifest() {
    let root = tmp_root("apply");
    let file = root
        .join("store")
        .join("Loctree")
        .join("aicx")
        .join("2026_0502")
        .join("conversations")
        .join("claude")
        .join("2026_0502_claude_sess_001.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        "ok\n{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"abc123\"}\n",
    )
    .unwrap();

    let manifest = repair(&CorpusRepairOptions {
        roots: vec![root.clone()],
        dry_run: false,
        apply: true,
        backup: true,
        manifest_path: None,
    })
    .unwrap();

    assert_eq!(manifest.repaired_files, 1);
    let repaired = fs::read_to_string(&file).unwrap();
    assert!(!repaired.contains("signature"));
    let sidecar: Value =
        serde_json::from_str(&fs::read_to_string(file.with_extension("meta.json")).unwrap())
            .unwrap();
    assert_eq!(sidecar["repair_version"], REPAIR_VERSION);
    assert_eq!(sidecar["source_was_derived"], true);
    assert_eq!(sidecar["raw_source_missing"], true);
    let manifest_path = manifest
        .manifest_path
        .as_ref()
        .expect("apply writes default manifest");
    assert!(manifest_path.exists());
    let manifest_json: Value =
        serde_json::from_str(&fs::read_to_string(manifest_path).unwrap()).unwrap();
    assert_eq!(manifest_json["manifest_path"], json!(manifest_path));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn repair_dry_run_does_not_write_default_manifest() {
    let root = tmp_root("dry-run");
    let file = root
        .join("store")
        .join("Loctree")
        .join("aicx")
        .join("2026_0502")
        .join("conversations")
        .join("claude")
        .join("2026_0502_claude_sess_001.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        "ok\n{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"abc123\"}\n",
    )
    .unwrap();

    let manifest = repair(&CorpusRepairOptions {
        roots: vec![root.clone()],
        dry_run: true,
        apply: false,
        backup: false,
        manifest_path: None,
    })
    .unwrap();

    assert_eq!(manifest.candidates, 1);
    assert_eq!(manifest.repaired_files, 0);
    assert!(manifest.manifest_path.is_none());
    assert!(!root.join(REPAIR_MANIFEST_DIR).exists());
    assert!(fs::read_to_string(&file).unwrap().contains("signature"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn repair_dry_run_writes_requested_manifest() {
    let root = tmp_root("dry-run-manifest");
    let file = root
        .join("store")
        .join("Loctree")
        .join("aicx")
        .join("2026_0502")
        .join("conversations")
        .join("claude")
        .join("2026_0502_claude_sess_001.md");
    let manifest_path = root.join("repair-preview.json");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        "ok\n{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"abc123\"}\n",
    )
    .unwrap();

    let manifest = repair(&CorpusRepairOptions {
        roots: vec![root.clone()],
        dry_run: true,
        apply: false,
        backup: false,
        manifest_path: Some(manifest_path.clone()),
    })
    .unwrap();

    assert_eq!(manifest.candidates, 1);
    assert_eq!(manifest.repaired_files, 0);
    assert_eq!(manifest.manifest_path, Some(manifest_path.clone()));
    assert!(manifest_path.exists());
    let raw = fs::read_to_string(manifest_path).unwrap();
    assert!(raw.contains("\"would_repair\""));
    let manifest_json: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        manifest_json["manifest_path"],
        json!(manifest.manifest_path)
    );
    assert!(fs::read_to_string(&file).unwrap().contains("signature"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn repair_rejects_apply_and_dry_run_together() {
    let error = repair(&CorpusRepairOptions {
        roots: Vec::new(),
        dry_run: true,
        apply: true,
        backup: false,
        manifest_path: None,
    })
    .unwrap_err()
    .to_string();

    assert!(error.contains("--apply and --dry-run cannot be used together"));
}

#[test]
fn audit_reports_missing_roots_without_scanning() {
    let root = tmp_root("missing-audit");
    let report = audit(&CorpusAuditOptions {
        roots: vec![root.clone()],
    })
    .unwrap();

    assert_eq!(report.totals.roots_missing, 1);
    assert_eq!(report.totals.markdown_files, 0);
    assert_eq!(report.roots[0].root, root);
}

#[test]
fn audit_classifies_noisy_markdown_examples() {
    let root = tmp_root("audit");
    let file = root.join("store/Loctree/aicx/2026_0502/session.md");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        "frame_kind: internal_thought\n{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"abc123\"}\n",
    )
    .unwrap();

    let report = audit(&CorpusAuditOptions {
        roots: vec![root.clone()],
    })
    .unwrap();

    assert_eq!(report.totals.markdown_files, 1);
    assert_eq!(report.totals.files_with_noise, 1);
    assert_eq!(
        report.totals.noise_classes.get("internal_thought_frame"),
        Some(&1)
    );
    assert_eq!(report.roots[0].examples.len(), 1);

    let _ = fs::remove_dir_all(root);
}
