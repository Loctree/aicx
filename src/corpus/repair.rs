use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

use crate::corpus::inference::system_timestamp;
use crate::corpus::inference::{
    derived_markdown_hash, infer_agent, infer_project, infer_session_id,
};
use crate::corpus::io::{
    markdown_files, validate_optional_root, write_bytes_validated, write_text_validated,
};
use crate::corpus::noise::{detect_noise_classes, repair_markdown_content};
use crate::corpus::roots::default_roots;
use crate::corpus::types::{
    CorpusRepairItem, CorpusRepairManifest, CorpusRepairOptions, NoiseClass, NoiseSet,
};
use crate::corpus::{REPAIR_MANIFEST_DIR, REPAIR_VERSION};
use crate::sanitize;

pub fn repair(options: &CorpusRepairOptions) -> Result<CorpusRepairManifest> {
    if options.apply && options.dry_run {
        return Err(anyhow!("--apply and --dry-run cannot be used together"));
    }

    let roots = if options.roots.is_empty() {
        default_roots()?
    } else {
        options.roots.clone()
    }
    .into_iter()
    .map(validate_optional_root)
    .collect::<Result<Vec<_>>>()?;
    let dry_run = !options.apply || options.dry_run;
    let generated_at = Utc::now();
    let mut manifest = CorpusRepairManifest {
        repair_version: REPAIR_VERSION.to_string(),
        generated_at: generated_at.to_rfc3339(),
        dry_run,
        apply: options.apply,
        backup: options.backup,
        roots: roots.clone(),
        scanned_markdown_files: 0,
        candidates: 0,
        repaired_files: 0,
        skipped_files: 0,
        manifest_path: options.manifest_path.clone(),
        items: Vec::new(),
    };

    for root in &roots {
        if !root.is_dir() {
            continue;
        }
        for path in markdown_files(root)? {
            manifest.scanned_markdown_files += 1;
            let content = sanitize::read_to_string_validated(&path)
                .with_context(|| format!("read markdown {}", path.display()))?;
            let noise = detect_noise_classes(&content);
            if noise.is_empty() {
                continue;
            }
            manifest.candidates += 1;
            let (repaired, removed) = repair_markdown_content(&content);
            if repaired == content {
                manifest.skipped_files += 1;
                continue;
            }

            let original_hash = derived_markdown_hash(&content);
            let repaired_hash = derived_markdown_hash(&repaired);
            let sidecar_path = path.with_extension("meta.json");
            let backup_path = if options.apply && options.backup {
                Some(write_backup(root, &path, &content, &generated_at)?)
            } else {
                None
            };

            if options.apply {
                write_text_validated(&path, &repaired)
                    .with_context(|| format!("write repaired markdown {}", path.display()))?;
                write_repair_sidecar(
                    root,
                    &path,
                    &sidecar_path,
                    &removed,
                    &original_hash,
                    &repaired_hash,
                    &generated_at,
                )?;
                manifest.repaired_files += 1;
            }

            manifest.items.push(CorpusRepairItem {
                path,
                action: if options.apply {
                    "repair".to_string()
                } else {
                    "would_repair".to_string()
                },
                backup_path,
                sidecar_path,
                removed_noise_classes: removed.iter().map(|c| c.as_str().to_string()).collect(),
                original_content_hash: original_hash,
                repaired_content_hash: repaired_hash,
            });
        }
    }

    if options.apply || options.manifest_path.is_some() {
        let manifest_path = write_manifest(
            &roots,
            &manifest,
            &generated_at,
            options.manifest_path.as_deref(),
        )?;
        manifest.manifest_path = Some(manifest_path);
    }

    Ok(manifest)
}

fn write_repair_sidecar(
    root: &Path,
    path: &Path,
    sidecar_path: &Path,
    removed: &NoiseSet,
    original_hash: &str,
    repaired_hash: &str,
    repaired_at: &DateTime<Utc>,
) -> Result<()> {
    let existing = sanitize::read_to_string_validated(sidecar_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let mut sidecar = existing;
    let content = sanitize::read_to_string_validated(path).unwrap_or_default();

    sidecar.insert("repair_version".to_string(), json!(REPAIR_VERSION));
    sidecar.insert("repaired_at".to_string(), json!(repaired_at.to_rfc3339()));
    sidecar.insert("source_was_derived".to_string(), json!(true));
    sidecar.insert("raw_source_missing".to_string(), json!(true));
    sidecar.insert(
        "removed_noise_classes".to_string(),
        json!(removed.iter().map(NoiseClass::as_str).collect::<Vec<_>>()),
    );
    sidecar.insert("original_content_hash".to_string(), json!(original_hash));
    sidecar.insert("repaired_content_hash".to_string(), json!(repaired_hash));
    sidecar.insert("source_app".to_string(), json!(infer_agent(path, &content)));
    sidecar.insert("source_path".to_string(), json!(path.display().to_string()));
    sidecar.insert("source_hash".to_string(), json!(original_hash));
    sidecar.insert(
        "session_id".to_string(),
        sidecar
            .get("session_id")
            .cloned()
            .unwrap_or_else(|| json!(infer_session_id(path))),
    );
    sidecar.insert(
        "project".to_string(),
        sidecar
            .get("project")
            .cloned()
            .unwrap_or_else(|| json!(infer_project(root, path))),
    );
    sidecar.insert(
        "repo/cwd".to_string(),
        sidecar.get("cwd").cloned().unwrap_or(Value::Null),
    );
    sidecar.insert(
        "timestamp".to_string(),
        json!(
            fs::metadata(path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map(system_timestamp)
                .unwrap_or_else(|| repaired_at.to_rfc3339())
        ),
    );
    sidecar.insert("role".to_string(), json!("derived_markdown"));
    sidecar.insert("turn_index".to_string(), json!(0));
    sidecar.insert(
        "model/agent".to_string(),
        sidecar
            .get("agent_model")
            .or_else(|| sidecar.get("agent"))
            .cloned()
            .unwrap_or_else(|| json!(infer_agent(path, &content))),
    );
    sidecar.insert("transform_version".to_string(), json!(REPAIR_VERSION));

    if let Some(parent) = sidecar_path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_bytes_validated(
        sidecar_path,
        &serde_json::to_vec_pretty(&Value::Object(sidecar))?,
    )?;
    Ok(())
}

fn write_backup(root: &Path, path: &Path, content: &str, now: &DateTime<Utc>) -> Result<PathBuf> {
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let relative = path.strip_prefix(root).unwrap_or(path);
    let backup_path = root
        .join(REPAIR_MANIFEST_DIR)
        .join(&stamp)
        .join("backups")
        .join(relative)
        .with_extension("md.bak");
    if let Some(parent) = backup_path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_bytes_validated(&backup_path, content.as_bytes())?;
    Ok(backup_path)
}

fn write_manifest(
    roots: &[PathBuf],
    manifest: &CorpusRepairManifest,
    now: &DateTime<Utc>,
    manifest_path: Option<&Path>,
) -> Result<PathBuf> {
    let path = if let Some(path) = manifest_path {
        path.to_path_buf()
    } else {
        let Some(root) = roots.iter().find(|root| root.is_dir()) else {
            return Ok(PathBuf::new());
        };
        let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
        root.join(REPAIR_MANIFEST_DIR)
            .join(format!("corpus-repair-{stamp}.json"))
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut written_manifest = manifest.clone();
    written_manifest.manifest_path = Some(path.clone());
    write_bytes_validated(&path, &serde_json::to_vec_pretty(&written_manifest)?)?;
    Ok(path)
}
