//! Corpus audit and deterministic derived-markdown repair.
//!
//! Raw agent logs remain provenance. This module only inspects or rewrites
//! derived markdown artifacts that feed retrieval.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::sanitize;

const REPAIR_VERSION: &str = "aicx-corpus-repair-v1";
const REPAIR_MANIFEST_DIR: &str = "repair-manifests";

#[derive(Debug, Clone)]
pub struct CorpusAuditOptions {
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct CorpusRepairOptions {
    pub roots: Vec<PathBuf>,
    pub dry_run: bool,
    pub apply: bool,
    pub backup: bool,
    pub manifest_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusAuditReport {
    pub roots: Vec<RootAuditReport>,
    pub totals: CorpusAuditTotals,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CorpusAuditTotals {
    pub roots_present: usize,
    pub roots_missing: usize,
    pub markdown_files: usize,
    pub files_with_noise: usize,
    pub noise_classes: BTreeMap<String, usize>,
    pub agents: BTreeMap<String, usize>,
    pub frame_kinds: BTreeMap<String, usize>,
    pub path_dates: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RootAuditReport {
    pub root: PathBuf,
    pub present: bool,
    pub markdown_files: usize,
    pub files_with_noise: usize,
    pub noise_classes: BTreeMap<String, usize>,
    pub agents: BTreeMap<String, usize>,
    pub frame_kinds: BTreeMap<String, usize>,
    pub path_dates: BTreeMap<String, usize>,
    pub artifact_birthtime_dates: BTreeMap<String, usize>,
    pub artifact_mtime_dates: BTreeMap<String, usize>,
    pub examples: Vec<CorpusFileFinding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusFileFinding {
    pub path: PathBuf,
    pub agent: String,
    pub frame_kind: Option<String>,
    pub path_date: Option<String>,
    pub artifact_birthtime: Option<String>,
    pub artifact_mtime: Option<String>,
    pub noise_classes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusRepairManifest {
    pub repair_version: String,
    pub generated_at: String,
    pub dry_run: bool,
    pub apply: bool,
    pub backup: bool,
    pub roots: Vec<PathBuf>,
    pub scanned_markdown_files: usize,
    pub candidates: usize,
    pub repaired_files: usize,
    pub skipped_files: usize,
    pub manifest_path: Option<PathBuf>,
    pub items: Vec<CorpusRepairItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusRepairItem {
    pub path: PathBuf,
    pub action: String,
    pub backup_path: Option<PathBuf>,
    pub sidecar_path: PathBuf,
    pub removed_noise_classes: Vec<String>,
    pub original_content_hash: String,
    pub repaired_content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum NoiseClass {
    Signature,
    ThoughtSignature,
    EmptyThinking,
    InlineThinkingJson,
    InternalThoughtFrame,
    MassiveToolJson,
}

impl NoiseClass {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Signature => "signature",
            Self::ThoughtSignature => "thoughtSignature",
            Self::EmptyThinking => "empty_thinking",
            Self::InlineThinkingJson => "inline_thinking_json",
            Self::InternalThoughtFrame => "internal_thought_frame",
            Self::MassiveToolJson => "massive_tool_json",
        }
    }
}

pub fn default_roots() -> Result<Vec<PathBuf>> {
    let home = dirs::home_dir().context("No home directory")?;
    Ok(vec![
        home.join(".aicx"),
        home.join(".ai-contexters"),
        home.join(".xcia"),
    ])
}

pub fn audit(options: &CorpusAuditOptions) -> Result<CorpusAuditReport> {
    let roots = if options.roots.is_empty() {
        default_roots()?
    } else {
        options.roots.clone()
    }
    .into_iter()
    .map(validate_optional_root)
    .collect::<Result<Vec<_>>>()?;

    let mut reports = Vec::new();
    let mut totals = CorpusAuditTotals::default();

    for root in roots {
        let report = audit_root(&root)?;
        if report.present {
            totals.roots_present += 1;
        } else {
            totals.roots_missing += 1;
        }
        totals.markdown_files += report.markdown_files;
        totals.files_with_noise += report.files_with_noise;
        merge_counts(&mut totals.noise_classes, &report.noise_classes);
        merge_counts(&mut totals.agents, &report.agents);
        merge_counts(&mut totals.frame_kinds, &report.frame_kinds);
        merge_counts(&mut totals.path_dates, &report.path_dates);
        reports.push(report);
    }

    Ok(CorpusAuditReport {
        roots: reports,
        totals,
    })
}

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

            let original_hash = content_hash(&content);
            let repaired_hash = content_hash(&repaired);
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

pub fn format_audit_text(report: &CorpusAuditReport) -> String {
    let mut out = String::new();
    out.push_str("=== AICX Corpus Audit ===\n\n");
    out.push_str(&format!(
        "roots: {} present, {} missing\n",
        report.totals.roots_present, report.totals.roots_missing
    ));
    out.push_str(&format!(
        "markdown_files: {}\nfiles_with_noise: {}\n\n",
        report.totals.markdown_files, report.totals.files_with_noise
    ));
    push_counts(&mut out, "noise_classes", &report.totals.noise_classes);
    push_counts(&mut out, "agents", &report.totals.agents);
    push_counts(&mut out, "frame_kinds", &report.totals.frame_kinds);
    push_counts(&mut out, "path_dates", &report.totals.path_dates);

    out.push_str("\nroots:\n");
    for root in &report.roots {
        out.push_str(&format!(
            "- {}: {} (markdown={}, noisy={})\n",
            root.root.display(),
            if root.present { "present" } else { "missing" },
            root.markdown_files,
            root.files_with_noise
        ));
        for example in &root.examples {
            out.push_str(&format!(
                "  example: {} [{}]\n",
                example.path.display(),
                example.noise_classes.join(", ")
            ));
        }
    }
    out
}

pub fn format_repair_text(manifest: &CorpusRepairManifest) -> String {
    let mut out = String::new();
    out.push_str("=== AICX Corpus Repair ===\n\n");
    out.push_str(&format!(
        "mode: {}\n",
        if manifest.apply { "apply" } else { "dry-run" }
    ));
    out.push_str(&format!(
        "scanned_markdown_files: {}\ncandidates: {}\nrepaired_files: {}\nskipped_files: {}\n\n",
        manifest.scanned_markdown_files,
        manifest.candidates,
        manifest.repaired_files,
        manifest.skipped_files
    ));
    for item in &manifest.items {
        out.push_str(&format!(
            "- {} {} [{}]\n",
            item.action,
            item.path.display(),
            item.removed_noise_classes.join(", ")
        ));
    }
    out
}

fn audit_root(root: &Path) -> Result<RootAuditReport> {
    if !root.is_dir() {
        return Ok(RootAuditReport {
            root: root.to_path_buf(),
            present: false,
            markdown_files: 0,
            files_with_noise: 0,
            noise_classes: BTreeMap::new(),
            agents: BTreeMap::new(),
            frame_kinds: BTreeMap::new(),
            path_dates: BTreeMap::new(),
            artifact_birthtime_dates: BTreeMap::new(),
            artifact_mtime_dates: BTreeMap::new(),
            examples: Vec::new(),
        });
    }

    let mut report = RootAuditReport {
        root: root.to_path_buf(),
        present: true,
        markdown_files: 0,
        files_with_noise: 0,
        noise_classes: BTreeMap::new(),
        agents: BTreeMap::new(),
        frame_kinds: BTreeMap::new(),
        path_dates: BTreeMap::new(),
        artifact_birthtime_dates: BTreeMap::new(),
        artifact_mtime_dates: BTreeMap::new(),
        examples: Vec::new(),
    };

    for path in markdown_files(root)? {
        report.markdown_files += 1;
        let content = sanitize::read_to_string_validated(&path).unwrap_or_default();
        let classes = detect_noise_classes(&content);
        let agent = infer_agent(&path, &content);
        inc(&mut report.agents, agent.clone());
        if let Some(frame_kind) = infer_frame_kind(&path, &content) {
            inc(&mut report.frame_kinds, frame_kind);
        }
        if let Some(path_date) = infer_path_date(&path) {
            inc(&mut report.path_dates, path_date);
        }
        if let Ok(meta) = fs::metadata(&path) {
            if let Ok(created) = meta.created() {
                inc(&mut report.artifact_birthtime_dates, system_date(created));
            }
            if let Ok(modified) = meta.modified() {
                inc(&mut report.artifact_mtime_dates, system_date(modified));
            }
        }

        if !classes.is_empty() {
            report.files_with_noise += 1;
            for class in &classes {
                inc(&mut report.noise_classes, class.as_str().to_string());
            }
            if report.examples.len() < 20 {
                report.examples.push(CorpusFileFinding {
                    path: path.clone(),
                    agent,
                    frame_kind: infer_frame_kind(&path, &content),
                    path_date: infer_path_date(&path),
                    artifact_birthtime: fs::metadata(&path)
                        .ok()
                        .and_then(|m| m.created().ok())
                        .map(system_timestamp),
                    artifact_mtime: fs::metadata(&path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(system_timestamp),
                    noise_classes: classes.iter().map(|c| c.as_str().to_string()).collect(),
                });
            }
        }
    }

    Ok(report)
}

fn detect_noise_classes(content: &str) -> BTreeSet<NoiseClass> {
    let mut classes = BTreeSet::new();
    if content.contains("\"signature\"") || content.contains("signature:") {
        classes.insert(NoiseClass::Signature);
    }
    if content.contains("thoughtSignature") {
        classes.insert(NoiseClass::ThoughtSignature);
    }
    if content.contains("\"type\":\"thinking\"") || content.contains("\"type\": \"thinking\"") {
        classes.insert(NoiseClass::InlineThinkingJson);
    }
    if content.contains("\"thinking\":\"\"") || content.contains("\"thinking\": \"\"") {
        classes.insert(NoiseClass::EmptyThinking);
    }
    if content.contains("frame_kind: internal_thought")
        || content.contains("\"frame_kind\":\"internal_thought\"")
        || content.contains("\"frame_kind\": \"internal_thought\"")
    {
        classes.insert(NoiseClass::InternalThoughtFrame);
    }
    if content.lines().any(|line| {
        line.len() > 4_000
            && (line.contains("\"tool_use\"")
                || line.contains("\"tool_result\"")
                || line.contains("\"input\""))
    }) {
        classes.insert(NoiseClass::MassiveToolJson);
    }
    classes
}

fn repair_markdown_content(content: &str) -> (String, BTreeSet<NoiseClass>) {
    let signature_re =
        Regex::new(r#"\s*,?\s*"(signature|thoughtSignature)"\s*:\s*"[^"]*""#).unwrap();
    let mut out = Vec::new();
    let mut removed = BTreeSet::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(repaired_thinking) = repair_inline_thinking_json_line(line) {
            removed.insert(NoiseClass::InlineThinkingJson);
            if is_empty_thinking_signature_line(trimmed) {
                removed.insert(NoiseClass::EmptyThinking);
            }
            if line.contains("thoughtSignature") {
                removed.insert(NoiseClass::ThoughtSignature);
            }
            if line.contains("\"signature\"") {
                removed.insert(NoiseClass::Signature);
            }
            if let Some(repaired_thinking) = repaired_thinking {
                out.push(repaired_thinking);
            }
            continue;
        }

        let mut repaired = signature_re.replace_all(line, "").to_string();
        if repaired != line {
            if line.contains("thoughtSignature") {
                removed.insert(NoiseClass::ThoughtSignature);
            }
            if line.contains("\"signature\"") {
                removed.insert(NoiseClass::Signature);
            }
        }
        repaired = normalize_json_commas(repaired);
        if !is_empty_thinking_signature_line(repaired.trim()) {
            out.push(repaired);
        }
    }

    let mut repaired = out.join("\n");
    if content.ends_with('\n') {
        repaired.push('\n');
    }
    (repaired, removed)
}

fn repair_inline_thinking_json_line(line: &str) -> Option<Option<String>> {
    let (prefix, candidate) = split_markdown_json_prefix(line);
    let value = serde_json::from_str::<Value>(candidate).ok()?;
    let object = value.as_object()?;
    let is_thinking = object.get("type").and_then(Value::as_str) == Some("thinking")
        || object.get("thought").and_then(Value::as_bool) == Some(true);
    if !is_thinking {
        return None;
    }

    for key in ["thinking", "text", "content", "summary"] {
        if let Some(text) = object
            .get(key)
            .and_then(extract_text_from_repair_json_value)
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
        {
            return Some(Some(format!("{prefix}{text}")));
        }
    }

    Some(None)
}

fn split_markdown_json_prefix(line: &str) -> (&str, &str) {
    let first_json = line.find('{').unwrap_or(line.len());
    line.split_at(first_json)
}

fn extract_text_from_repair_json_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(extract_text_from_repair_json_value)
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(object) => ["text", "content", "summary", "thinking"]
            .iter()
            .filter_map(|key| object.get(*key))
            .find_map(extract_text_from_repair_json_value),
        _ => None,
    }
}

fn is_empty_thinking_signature_line(line: &str) -> bool {
    let candidate = line
        .trim_start_matches('>')
        .trim_start_matches('-')
        .trim_start_matches('*')
        .trim();
    candidate.contains("\"type\"")
        && candidate.contains("\"thinking\"")
        && (candidate.contains("\"thinking\":\"\"") || candidate.contains("\"thinking\": \"\""))
        && (candidate.contains("\"signature\"") || candidate.contains("thoughtSignature"))
}

fn normalize_json_commas(line: String) -> String {
    line.replace("{,", "{")
        .replace(",}", "}")
        .replace("[,", "[")
        .replace(",]", "]")
}

fn write_repair_sidecar(
    root: &Path,
    path: &Path,
    sidecar_path: &Path,
    removed: &BTreeSet<NoiseClass>,
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
        json!(removed.iter().map(|c| c.as_str()).collect::<Vec<_>>()),
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

fn markdown_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for start in scan_start_dirs(root) {
        if start.exists() {
            collect_markdown_files(&start, &mut files)?;
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn scan_start_dirs(root: &Path) -> Vec<PathBuf> {
    let is_aicx_root = root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ".aicx");
    if is_aicx_root {
        let mut starts = vec![
            root.join("store"),
            root.join("non-repository-contexts"),
            root.join("chunks"),
        ];
        if root.extension().and_then(|s| s.to_str()) == Some("md") {
            starts.push(root.to_path_buf());
        }
        starts
    } else {
        vec![root.to_path_buf()]
    }
}

fn collect_markdown_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let dir = sanitize::validate_read_path(dir)?;
    let metadata = fs::symlink_metadata(&dir)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        if dir.extension().and_then(|s| s.to_str()) == Some("md") {
            files.push(dir.to_path_buf());
        }
        return Ok(());
    }

    for entry in
        sanitize::read_dir_validated(&dir).with_context(|| format!("read dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(
            name,
            "target"
                | ".git"
                | REPAIR_MANIFEST_DIR
                | "lancedb"
                | "lance"
                | "steer"
                | "steer-index"
                | "bm25"
                | "indexes"
        ) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_markdown_files(&path, files)?;
        } else if metadata.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md") {
            files.push(path);
        }
    }
    Ok(())
}

fn validate_optional_root(root: PathBuf) -> Result<PathBuf> {
    if root.exists() {
        sanitize::validate_read_path(&root)
    } else {
        Ok(root)
    }
}

fn write_text_validated(path: &Path, content: &str) -> Result<()> {
    write_bytes_validated(path, content.as_bytes())
}

fn write_bytes_validated(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = sanitize::create_file_validated(path)?;
    file.write_all(content)?;
    Ok(())
}

fn infer_agent(path: &Path, content: &str) -> String {
    let haystack = format!(
        "{} {}",
        path.display(),
        content.lines().take(20).collect::<String>()
    )
    .to_ascii_lowercase();
    for agent in [
        "claude",
        "codex",
        "gemini",
        "junie",
        "codescribe",
        "operator-md",
    ] {
        if haystack.contains(agent) {
            return agent.to_string();
        }
    }
    "unknown".to_string()
}

fn infer_frame_kind(path: &Path, content: &str) -> Option<String> {
    if let Some(value) = content.lines().find_map(|line| {
        line.strip_prefix("frame_kind:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }) {
        return Some(value.to_string());
    }
    sanitize::read_to_string_validated(&path.with_extension("meta.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| {
            value
                .get("frame_kind")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn infer_path_date(path: &Path) -> Option<String> {
    static DATE_RE: OnceLock<Regex> = OnceLock::new();
    let re = DATE_RE.get_or_init(|| Regex::new(r"(20\d{2})[-_]?([01]\d)[-_]?([0-3]\d)").unwrap());
    let text = path.display().to_string();
    re.captures(&text)
        .map(|captures| format!("{}-{}-{}", &captures[1], &captures[2], &captures[3]))
}

fn infer_session_id(path: &Path) -> String {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return "unknown".to_string();
    };
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() >= 5 {
        parts[3].to_string()
    } else {
        parts.get(2).copied().unwrap_or("unknown").to_string()
    }
}

fn infer_project(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| {
            let mut components = relative.components().filter_map(|c| c.as_os_str().to_str());
            match components.next() {
                Some("store") => Some(format!(
                    "{}/{}",
                    components.next().unwrap_or("unknown"),
                    components.next().unwrap_or("unknown")
                )),
                Some(other) => Some(other.to_string()),
                None => None,
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn system_timestamp(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339()
}

fn system_date(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).format("%Y-%m-%d").to_string()
}

fn content_hash(content: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn merge_counts(target: &mut BTreeMap<String, usize>, source: &BTreeMap<String, usize>) {
    for (key, value) in source {
        *target.entry(key.clone()).or_default() += value;
    }
}

fn inc(map: &mut BTreeMap<String, usize>, key: String) {
    *map.entry(key).or_default() += 1;
}

fn push_counts(out: &mut String, label: &str, counts: &BTreeMap<String, usize>) {
    out.push_str(&format!("{label}:\n"));
    if counts.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for (key, value) in counts {
            out.push_str(&format!("  {key}: {value}\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let input = "{\"type\":\"thinking\",\"thinking\":\"useful private note\",\"signature\":\"abc123\"}\n";
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
}
