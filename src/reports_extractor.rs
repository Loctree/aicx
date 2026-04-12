//! Vibecrafted artifact report extractor and standalone HTML explorer.
//!
//! Scans `~/.vibecrafted/artifacts/<org>/<repo>/...` style trees, merges markdown
//! reports with optional `.meta.json` companions, and produces a shareable HTML
//! artifact plus JSON bundle for client-side re-import.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MAX_MARKDOWN_DETAIL_CHARS: usize = 48_000;
const MAX_TRANSCRIPT_TAIL_BYTES: u64 = 48 * 1024;

/// Configuration for report-artifact extraction and HTML generation.
#[derive(Debug, Clone)]
pub struct ReportsExtractorConfig {
    /// Vibecrafted artifacts root (default from CLI: ~/.vibecrafted/artifacts).
    pub artifacts_root: PathBuf,
    /// Org filter under the artifacts root.
    pub org: String,
    /// Repo name under the artifacts root.
    pub repo: String,
    /// Inclusive start date filter.
    pub date_from: Option<NaiveDate>,
    /// Inclusive end date filter.
    pub date_to: Option<NaiveDate>,
    /// Optional workflow/path filter (case-insensitive substring).
    pub workflow: Option<String>,
    /// HTML document title.
    pub title: String,
    /// Max characters in record previews (0 = no truncation).
    pub preview_chars: usize,
}

/// Generation output for the standalone reports explorer.
#[derive(Debug, Clone)]
pub struct ReportsExtractorArtifact {
    /// Rendered standalone HTML page.
    pub html: String,
    /// Pretty JSON bundle with the same embedded payload.
    pub bundle_json: String,
    /// Aggregate stats for CLI output.
    pub stats: ReportsExplorerStats,
    /// Scan assumptions surfaced to the operator and the HTML.
    pub assumptions: Vec<String>,
}

/// Aggregate stats for the explorer payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportsExplorerStats {
    pub total_records: usize,
    pub total_reports: usize,
    pub total_plans: usize,
    pub total_meta_only: usize,
    pub total_transcript_backed: usize,
    pub completed_records: usize,
    pub incomplete_records: usize,
    pub total_days: usize,
    pub total_workflows: usize,
    pub total_agents: usize,
    pub avg_duration_s: Option<f64>,
}

/// Embedded JSON payload consumed by the standalone HTML app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportsExplorerPayload {
    pub schema_version: u32,
    pub generated_at: String,
    pub artifacts_root: String,
    pub resolved_org: String,
    pub resolved_repo: String,
    pub scan_root: String,
    pub selected_date: Option<String>,
    pub selected_workflow: Option<String>,
    pub stats: ReportsExplorerStats,
    pub assumptions: Vec<String>,
    pub workflows: Vec<String>,
    pub agents: Vec<String>,
    pub statuses: Vec<String>,
    pub lanes: Vec<String>,
    pub days: Vec<String>,
    pub records: Vec<ReportsExplorerRecord>,
}

/// One workflow/report artifact entry shown in the HTML explorer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportsExplorerRecord {
    pub id: usize,
    pub key: String,
    pub org: String,
    pub repo: String,
    pub workflow: String,
    pub lane: String,
    pub record_kind: String,
    pub status: String,
    pub agent: String,
    pub skill_code: Option<String>,
    pub mode: Option<String>,
    pub run_id: Option<String>,
    pub prompt_id: Option<String>,
    pub session_id: Option<String>,
    pub date_bucket: String,
    pub date_iso: String,
    pub title: String,
    pub file_name: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub meta_path: Option<String>,
    pub transcript_path: Option<String>,
    pub input_path: Option<String>,
    pub launcher_path: Option<String>,
    pub updated_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_s: Option<f64>,
    pub loop_nr: Option<u32>,
    pub headings: Vec<String>,
    pub preview: String,
    pub detail_text: String,
    pub search_blob: String,
    pub has_markdown: bool,
    pub has_meta: bool,
    pub has_transcript: bool,
    pub sort_ts: i64,
}

#[derive(Debug, Default, Clone)]
struct Candidate {
    md_path: Option<PathBuf>,
    meta_path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct ArtifactMeta {
    updated_at: Option<String>,
    status: Option<String>,
    agent: Option<String>,
    mode: Option<String>,
    input: Option<String>,
    report: Option<String>,
    transcript: Option<String>,
    launcher: Option<String>,
    prompt_id: Option<String>,
    run_id: Option<String>,
    loop_nr: Option<u32>,
    skill_code: Option<String>,
    exit_code: Option<i32>,
    completed_at: Option<String>,
    duration_s: Option<f64>,
    session_id: Option<String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct ArtifactFrontmatterEnvelope {
    status: Option<String>,
    created: Option<String>,
    #[serde(flatten)]
    report: crate::frontmatter::ReportFrontmatter,
}

#[derive(Debug, Default, Clone)]
struct DateFilter {
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
}

/// Build a standalone HTML explorer and JSON bundle from Vibecrafted artifacts.
pub fn build_reports_explorer(config: &ReportsExtractorConfig) -> Result<ReportsExtractorArtifact> {
    let artifacts_root = crate::sanitize::validate_dir_path(&config.artifacts_root)?;
    let repo_root = artifacts_root.join(&config.org).join(&config.repo);
    let repo_root = crate::sanitize::validate_dir_path(&repo_root).with_context(|| {
        format!(
            "Artifacts repo not found: {}/{} under {}",
            config.org,
            config.repo,
            artifacts_root.display()
        )
    })?;
    let payload = scan_reports(&repo_root, &artifacts_root, config)?;
    let bundle_json =
        serde_json::to_string_pretty(&payload).context("Failed to serialize reports bundle")?;
    let html = render_reports_html(&payload, &config.title)?;

    Ok(ReportsExtractorArtifact {
        html,
        bundle_json,
        stats: payload.stats.clone(),
        assumptions: payload.assumptions.clone(),
    })
}

fn scan_reports(
    repo_root: &Path,
    artifacts_root: &Path,
    config: &ReportsExtractorConfig,
) -> Result<ReportsExplorerPayload> {
    let mut assumptions = vec![
        "Scans Vibecrafted markdown plans/reports plus optional .meta.json companions under the central artifacts tree.".to_string(),
        "Meta-only or transcript-backed runs are surfaced honestly instead of being dropped from the explorer.".to_string(),
        "Standalone HTML includes an embedded JSON payload and can merge additional bundle files client-side.".to_string(),
    ];
    assumptions
        .push("Legacy artifacts are skipped by default in this first explorer pass.".to_string());
    if let Some(workflow) = config.workflow.as_ref() {
        assumptions.push(format!(
            "Workflow filter applied during extraction: {}",
            workflow
        ));
    }
    if config.date_from.is_some() || config.date_to.is_some() {
        assumptions.push(format!(
            "Date window applied during extraction: {} .. {}",
            config
                .date_from
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "open".to_string()),
            config
                .date_to
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "open".to_string())
        ));
    }

    let date_filter = DateFilter {
        start: config.date_from,
        end: config.date_to,
    };
    let mut candidates = BTreeMap::<String, Candidate>::new();
    collect_candidates(repo_root, Path::new(""), &mut candidates)?;

    let mut records = Vec::<ReportsExplorerRecord>::new();
    let mut workflows = BTreeSet::<String>::new();
    let mut agents = BTreeSet::<String>::new();
    let mut statuses = BTreeSet::<String>::new();
    let mut lanes = BTreeSet::<String>::new();
    let mut days = BTreeSet::<String>::new();
    let mut duration_total = 0.0_f64;
    let mut duration_count = 0_u64;

    for candidate in candidates.values() {
        let record = finalize_candidate(candidate, repo_root, config, &date_filter)?;
        let Some(record) = record else {
            continue;
        };

        workflows.insert(record.workflow.clone());
        agents.insert(record.agent.clone());
        statuses.insert(record.status.clone());
        lanes.insert(record.lane.clone());
        days.insert(record.date_iso.clone());
        if let Some(duration) = record.duration_s {
            duration_total += duration;
            duration_count += 1;
        }
        records.push(record);
    }

    records.sort_by(|left, right| {
        right
            .sort_ts
            .cmp(&left.sort_ts)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    for (idx, record) in records.iter_mut().enumerate() {
        record.id = idx + 1;
    }

    let total_reports = records
        .iter()
        .filter(|record| record.record_kind != "plan")
        .count();
    let total_plans = records
        .iter()
        .filter(|record| record.record_kind == "plan")
        .count();
    let total_meta_only = records
        .iter()
        .filter(|record| record.has_meta && !record.has_markdown)
        .count();
    let total_transcript_backed = records
        .iter()
        .filter(|record| record.has_transcript)
        .count();
    let completed_records = records
        .iter()
        .filter(|record| normalized_eq(&record.status, "completed"))
        .count();
    let incomplete_records = records.len().saturating_sub(completed_records);

    let stats = ReportsExplorerStats {
        total_records: records.len(),
        total_reports,
        total_plans,
        total_meta_only,
        total_transcript_backed,
        completed_records,
        incomplete_records,
        total_days: days.len(),
        total_workflows: workflows.len(),
        total_agents: agents.len(),
        avg_duration_s: if duration_count > 0 {
            Some(duration_total / duration_count as f64)
        } else {
            None
        },
    };

    Ok(ReportsExplorerPayload {
        schema_version: 1,
        generated_at: Utc::now().to_rfc3339(),
        artifacts_root: artifacts_root.display().to_string(),
        resolved_org: config.org.clone(),
        resolved_repo: config.repo.clone(),
        scan_root: repo_root.display().to_string(),
        selected_date: format_date_window(config.date_from, config.date_to),
        selected_workflow: config.workflow.clone(),
        stats,
        assumptions,
        workflows: workflows.into_iter().collect(),
        agents: agents.into_iter().collect(),
        statuses: statuses.into_iter().collect(),
        lanes: lanes.into_iter().collect(),
        days: days.into_iter().collect(),
        records,
    })
}

fn collect_candidates(
    dir: &Path,
    relative: &Path,
    candidates: &mut BTreeMap<String, Candidate>,
) -> Result<()> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("Failed to read artifact directory: {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("Failed to iterate artifact directory: {}", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let rel = relative.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_candidates(&path, &rel, candidates)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let rel_string = rel.to_string_lossy();
        let contains_lane = rel_string.contains("/reports/") || rel_string.contains("/plans/");
        if !contains_lane && !rel_string.ends_with("/reports") && !rel_string.ends_with("/plans") {
            continue;
        }

        let file_name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name,
            None => continue,
        };

        if file_name.ends_with(".meta.json") {
            let key = path_string_without_suffix(&path, ".meta.json");
            candidates.entry(key).or_default().meta_path = Some(path);
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            let key = path_string_without_suffix(&path, ".md");
            candidates.entry(key).or_default().md_path = Some(path);
        }
    }

    Ok(())
}

fn path_string_without_suffix(path: &Path, suffix: &str) -> String {
    let value = path.display().to_string();
    value.strip_suffix(suffix).unwrap_or(&value).to_string()
}

fn finalize_candidate(
    candidate: &Candidate,
    repo_root: &Path,
    config: &ReportsExtractorConfig,
    date_filter: &DateFilter,
) -> Result<Option<ReportsExplorerRecord>> {
    let primary_path = candidate
        .md_path
        .as_ref()
        .or(candidate.meta_path.as_ref())
        .ok_or_else(|| anyhow!("artifact candidate without markdown or metadata path"))?;
    let relative = primary_path
        .strip_prefix(repo_root)
        .with_context(|| {
            format!(
                "Failed to resolve relative artifact path for {}",
                primary_path.display()
            )
        })?
        .to_path_buf();

    let path_parts = relative_components(&relative);
    if path_parts.is_empty() {
        return Ok(None);
    }
    let date_bucket = path_parts[0].clone();
    if date_bucket == "legacy" {
        return Ok(None);
    }

    let date_iso = normalize_date_bucket(&date_bucket).unwrap_or_else(|| date_bucket.clone());
    if !matches_date_filter(&date_iso, date_filter) {
        return Ok(None);
    }

    let meta = if let Some(meta_path) = candidate.meta_path.as_ref() {
        Some(read_meta(meta_path)?)
    } else {
        None
    };

    let markdown = if let Some(md_path) = candidate.md_path.as_ref() {
        Some(read_markdown(md_path)?)
    } else {
        None
    };

    let title = derive_title(
        markdown.as_ref().map(|item| item.body.as_str()),
        primary_path,
        "day-root",
        meta.as_ref(),
    );
    let (lane, workflow) = derive_lane_and_workflow(
        &path_parts,
        primary_path,
        &title,
        markdown.as_ref(),
        meta.as_ref(),
    );
    if let Some(filter) = config.workflow.as_ref() {
        let haystack = format!("{workflow} {lane} {}", relative.display());
        if !contains_case_insensitive(&haystack, filter) {
            return Ok(None);
        }
    }

    let agent = derive_agent(&title, &path_parts, markdown.as_ref(), meta.as_ref());

    let status = derive_status(&lane, markdown.as_ref(), meta.as_ref());

    let transcript_path = meta
        .as_ref()
        .and_then(|item| item.transcript.as_ref())
        .map(PathBuf::from)
        .filter(|path| path.exists());

    let detail_text =
        build_detail_text(markdown.as_ref(), transcript_path.as_deref(), meta.as_ref());
    let preview = build_preview(
        markdown.as_ref(),
        detail_text.as_str(),
        config.preview_chars,
        &status,
        &title,
    );

    let headings = markdown
        .as_ref()
        .map(|item| item.headings.clone())
        .unwrap_or_default();

    let meta_path_string = candidate
        .meta_path
        .as_ref()
        .map(|path| path.display().to_string());
    let absolute_path = candidate
        .md_path
        .as_ref()
        .map(|path| path.display().to_string())
        .or_else(|| meta.as_ref().and_then(|item| item.report.clone()))
        .unwrap_or_else(|| primary_path.display().to_string());
    let relative_path = relative.display().to_string();

    let run_id = markdown
        .as_ref()
        .and_then(|item| item.frontmatter.report.telemetry.run_id.clone())
        .or_else(|| meta.as_ref().and_then(|item| item.run_id.clone()));
    let prompt_id = markdown
        .as_ref()
        .and_then(|item| item.frontmatter.report.telemetry.prompt_id.clone())
        .or_else(|| meta.as_ref().and_then(|item| item.prompt_id.clone()));
    let skill_code = markdown
        .as_ref()
        .and_then(|item| item.frontmatter.report.steering.skill_code.clone())
        .or_else(|| meta.as_ref().and_then(|item| item.skill_code.clone()));
    let mode = markdown
        .as_ref()
        .and_then(|item| item.frontmatter.report.steering.mode.clone())
        .or_else(|| meta.as_ref().and_then(|item| item.mode.clone()));
    let completed_at = meta
        .as_ref()
        .and_then(|item| item.completed_at.clone())
        .or_else(|| {
            markdown
                .as_ref()
                .and_then(|item| item.frontmatter.created.clone())
        });
    let updated_at = meta
        .as_ref()
        .and_then(|item| item.updated_at.clone())
        .or_else(|| Some(format_modified_utc(file_modified(primary_path))));
    let duration_s = meta.as_ref().and_then(|item| item.duration_s);
    let loop_nr = meta.as_ref().and_then(|item| item.loop_nr);
    let session_id = meta.as_ref().and_then(|item| item.session_id.clone());

    let search_blob = collapse_ws(&format!(
        "{} {} {} {} {} {} {} {} {} {} {} {}",
        title,
        workflow,
        lane,
        status,
        agent,
        skill_code.clone().unwrap_or_default(),
        run_id.clone().unwrap_or_default(),
        prompt_id.clone().unwrap_or_default(),
        relative_path,
        headings.join(" "),
        preview,
        detail_text
    ));

    let sort_ts = pick_sort_ts(
        completed_at.as_deref(),
        updated_at.as_deref(),
        file_modified(primary_path),
    );

    Ok(Some(ReportsExplorerRecord {
        id: 0,
        key: build_record_key(
            run_id.as_deref(),
            &absolute_path,
            &relative_path,
            meta_path_string.as_deref(),
        ),
        org: config.org.clone(),
        repo: config.repo.clone(),
        workflow,
        lane,
        record_kind: if path_contains_segment(&path_parts, "plans") {
            "plan".to_string()
        } else {
            "report".to_string()
        },
        status,
        agent,
        skill_code,
        mode,
        run_id,
        prompt_id,
        session_id,
        date_bucket,
        date_iso,
        title,
        file_name: primary_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact")
            .to_string(),
        relative_path,
        absolute_path,
        meta_path: meta_path_string,
        transcript_path: transcript_path.map(|path| path.display().to_string()),
        input_path: meta.as_ref().and_then(|item| item.input.clone()),
        launcher_path: meta.as_ref().and_then(|item| item.launcher.clone()),
        updated_at,
        completed_at,
        duration_s,
        loop_nr,
        headings,
        preview,
        detail_text,
        search_blob,
        has_markdown: candidate.md_path.is_some(),
        has_meta: candidate.meta_path.is_some(),
        has_transcript: meta
            .as_ref()
            .and_then(|item| item.transcript.as_ref())
            .map(|path| Path::new(path).exists())
            .unwrap_or(false),
        sort_ts,
    }))
}

fn build_record_key(
    run_id: Option<&str>,
    absolute_path: &str,
    relative_path: &str,
    meta_path: Option<&str>,
) -> String {
    run_id
        .map(|value| format!("run:{value}"))
        .or_else(|| meta_path.map(|value| format!("meta:{value}")))
        .unwrap_or_else(|| format!("path:{absolute_path}:{relative_path}"))
}

#[derive(Debug, Clone)]
struct ParsedMarkdown {
    frontmatter: ArtifactFrontmatterEnvelope,
    body: String,
    headings: Vec<String>,
}

fn read_markdown(path: &Path) -> Result<ParsedMarkdown> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read markdown artifact: {}", path.display()))?;
    let (frontmatter, body) = parse_artifact_frontmatter(&raw);
    let body = sanitize_text(body);
    let headings = extract_headings(&body);
    Ok(ParsedMarkdown {
        frontmatter: frontmatter.unwrap_or_default(),
        body,
        headings,
    })
}

fn read_meta(path: &Path) -> Result<ArtifactMeta> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Failed to read artifact metadata: {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse artifact metadata: {}", path.display()))
}

fn parse_artifact_frontmatter(text: &str) -> (Option<ArtifactFrontmatterEnvelope>, &str) {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return (None, text);
    }

    let after_open = trimmed[3..].strip_prefix('\n').unwrap_or(&trimmed[3..]);
    let Some(end) = after_open.find("\n---") else {
        return (None, text);
    };
    let yaml_str = &after_open[..end];
    let body_start = end + 4;
    let body = after_open[body_start..]
        .strip_prefix('\n')
        .unwrap_or(&after_open[body_start..]);
    let frontmatter = serde_yaml::from_str::<ArtifactFrontmatterEnvelope>(yaml_str).ok();
    (frontmatter, body)
}

fn derive_lane_and_workflow(
    path_parts: &[String],
    primary_path: &Path,
    title: &str,
    markdown: Option<&ParsedMarkdown>,
    meta: Option<&ArtifactMeta>,
) -> (String, String) {
    let lane = if let Some(idx) = path_parts.iter().position(|segment| segment == "reports") {
        if idx >= 2 && path_parts[idx - 1] == "marbles" {
            "marbles/reports".to_string()
        } else if idx >= 3 && path_parts[idx - 2] == "pipeline" {
            "pipeline/reports".to_string()
        } else {
            "reports".to_string()
        }
    } else if let Some(idx) = path_parts.iter().position(|segment| segment == "plans") {
        if idx >= 2 && path_parts[idx - 1] == "marbles" {
            "marbles/plans".to_string()
        } else if idx >= 3 && path_parts[idx - 2] == "pipeline" {
            "pipeline/plans".to_string()
        } else {
            "plans".to_string()
        }
    } else {
        "other".to_string()
    };

    let workflow = if path_contains_segment(path_parts, "marbles") {
        "marbles".to_string()
    } else if let Some(idx) = path_parts.iter().position(|segment| segment == "pipeline") {
        if let Some(slug) = path_parts.get(idx + 1) {
            format!("pipeline/{slug}")
        } else {
            "pipeline".to_string()
        }
    } else {
        infer_day_root_workflow(primary_path, title, markdown, meta)
            .unwrap_or_else(|| "day-root".to_string())
    };

    (lane, workflow)
}

fn infer_day_root_workflow(
    primary_path: &Path,
    title: &str,
    markdown: Option<&ParsedMarkdown>,
    meta: Option<&ArtifactMeta>,
) -> Option<String> {
    prompt_workflow_slug(
        markdown
            .and_then(|item| item.frontmatter.report.telemetry.prompt_id.as_deref())
            .or_else(|| meta.and_then(|item| item.prompt_id.as_deref())),
    )
    .or_else(|| stem_workflow_slug(primary_path))
    .or_else(|| title_workflow_slug(title))
}

fn prompt_workflow_slug(prompt_id: Option<&str>) -> Option<String> {
    let prompt_id = prompt_id?;
    let prompt_id = prompt_id.trim();
    if prompt_id.is_empty() {
        return None;
    }

    let base = prompt_id
        .split_once('_')
        .map(|(left, _)| left)
        .unwrap_or(prompt_id);
    normalize_workflow_slug(base)
}

fn stem_workflow_slug(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let filtered = stem
        .split('_')
        .filter(|segment| !segment.is_empty())
        .filter(|segment| !looks_like_timestamp_segment(segment))
        .filter(|segment| !is_known_artifact_suffix(segment))
        .filter(|segment| !is_known_agent(segment))
        .collect::<Vec<_>>()
        .join("-");
    normalize_workflow_slug(&filtered)
}

fn title_workflow_slug(title: &str) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = trimmed
        .replace([':', '/'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    normalize_workflow_slug(&normalized)
}

fn normalize_workflow_slug(value: &str) -> Option<String> {
    let slug = value
        .trim_matches(|ch: char| ch == '_' || ch == '-' || ch.is_whitespace())
        .to_lowercase();
    let slug = slug
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'))
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = slug
        .replace('_', "-")
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if slug.is_empty() { None } else { Some(slug) }
}

fn looks_like_timestamp_segment(segment: &str) -> bool {
    let digits_only = segment.chars().all(|ch| ch.is_ascii_digit());
    digits_only && matches!(segment.len(), 4 | 6 | 8 | 12 | 14)
}

fn is_known_artifact_suffix(segment: &str) -> bool {
    matches!(
        segment.to_ascii_lowercase().as_str(),
        "context" | "research" | "report" | "reports" | "plan" | "plans" | "summary"
    )
}

fn path_contains_segment(path_parts: &[String], needle: &str) -> bool {
    path_parts.iter().any(|segment| segment == needle)
}

fn relative_components(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect()
}

fn derive_title(
    markdown_body: Option<&str>,
    primary_path: &Path,
    workflow: &str,
    meta: Option<&ArtifactMeta>,
) -> String {
    if let Some(body) = markdown_body
        && let Some(title) = extract_headings(body).into_iter().next()
    {
        return title;
    }

    if let Some(report) = meta.and_then(|item| item.report.as_deref())
        && let Some(stem) = Path::new(report).file_stem().and_then(|name| name.to_str())
    {
        return humanize_stem(stem);
    }

    let fallback = primary_path
        .file_stem()
        .and_then(|name| name.to_str())
        .map(humanize_stem)
        .unwrap_or_else(|| workflow.to_string());
    if fallback.is_empty() {
        workflow.to_string()
    } else {
        fallback
    }
}

fn derive_agent(
    title: &str,
    path_parts: &[String],
    markdown: Option<&ParsedMarkdown>,
    meta: Option<&ArtifactMeta>,
) -> String {
    markdown
        .and_then(|item| item.frontmatter.report.telemetry.agent.clone())
        .or_else(|| meta.and_then(|item| item.agent.clone()))
        .or_else(|| {
            path_parts
                .iter()
                .rev()
                .find(|segment| is_known_agent(segment))
                .cloned()
        })
        .or_else(|| agent_from_title(title))
        .unwrap_or_else(|| "unknown".to_string())
}

fn derive_status(
    lane: &str,
    markdown: Option<&ParsedMarkdown>,
    meta: Option<&ArtifactMeta>,
) -> String {
    if let Some(status) = meta.and_then(|item| item.status.clone()) {
        return status;
    }
    if let Some(status) = markdown.and_then(|item| item.frontmatter.status.clone()) {
        return status;
    }
    if lane.ends_with("/plans") || lane == "plans" {
        return "planned".to_string();
    }
    if markdown.is_some() {
        return "completed".to_string();
    }
    "unknown".to_string()
}

fn build_detail_text(
    markdown: Option<&ParsedMarkdown>,
    transcript_path: Option<&Path>,
    meta: Option<&ArtifactMeta>,
) -> String {
    if let Some(markdown) = markdown {
        return trim_chars(&markdown.body, MAX_MARKDOWN_DETAIL_CHARS);
    }

    if let Some(path) = transcript_path
        && let Ok(text) = read_tail_string(path, MAX_TRANSCRIPT_TAIL_BYTES)
        && !text.trim().is_empty()
    {
        return sanitize_text(&text);
    }

    let mut lines = Vec::new();
    if let Some(meta) = meta {
        if let Some(status) = meta.status.as_deref() {
            lines.push(format!("status: {}", status));
        }
        if let Some(run_id) = meta.run_id.as_deref() {
            lines.push(format!("run_id: {}", run_id));
        }
        if let Some(prompt_id) = meta.prompt_id.as_deref() {
            lines.push(format!("prompt_id: {}", prompt_id));
        }
        if let Some(mode) = meta.mode.as_deref() {
            lines.push(format!("mode: {}", mode));
        }
        if let Some(skill_code) = meta.skill_code.as_deref() {
            lines.push(format!("skill_code: {}", skill_code));
        }
        if let Some(updated_at) = meta.updated_at.as_deref() {
            lines.push(format!("updated_at: {}", updated_at));
        }
        if let Some(report) = meta.report.as_deref() {
            lines.push(format!("report: {}", report));
        }
        if let Some(transcript) = meta.transcript.as_deref() {
            lines.push(format!("transcript: {}", transcript));
        }
        if let Some(exit_code) = meta.exit_code {
            lines.push(format!("exit_code: {}", exit_code));
        }
    }

    if lines.is_empty() {
        "No markdown body or transcript was available for this artifact.".to_string()
    } else {
        lines.join("\n")
    }
}

fn build_preview(
    markdown: Option<&ParsedMarkdown>,
    detail_text: &str,
    preview_chars: usize,
    status: &str,
    title: &str,
) -> String {
    let base = if let Some(markdown) = markdown {
        collapse_ws(&markdown.body)
    } else {
        collapse_ws(detail_text)
    };
    let preview = trim_chars(&base, preview_chars);
    if preview.is_empty() {
        trim_chars(
            &format!("{status} artifact: {title}"),
            if preview_chars == 0 {
                80
            } else {
                preview_chars
            },
        )
    } else {
        preview
    }
}

fn extract_headings(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with('#') {
                return None;
            }
            let heading = trimmed.trim_start_matches('#').trim();
            if heading.is_empty() {
                None
            } else {
                Some(heading.to_string())
            }
        })
        .take(12)
        .collect()
}

fn humanize_stem(stem: &str) -> String {
    collapse_ws(&stem.replace(['_', '-'], " "))
}

fn agent_from_title(title: &str) -> Option<String> {
    ["codex", "claude", "gemini"]
        .iter()
        .find(|candidate| contains_case_insensitive(title, candidate))
        .map(|candidate| (*candidate).to_string())
}

fn is_known_agent(segment: &str) -> bool {
    matches!(segment, "codex" | "claude" | "gemini")
}

fn read_tail_string(path: &Path, max_bytes: u64) -> Result<String> {
    let mut file = fs::File::open(path)
        .with_context(|| format!("Failed to open transcript: {}", path.display()))?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn sanitize_text(input: &str) -> String {
    input.replace('\0', "").replace("\r\n", "\n")
}

fn normalize_date_bucket(bucket: &str) -> Option<String> {
    if bucket.len() != 9 {
        return None;
    }
    let parts = bucket.split('_').collect::<Vec<_>>();
    if parts.len() != 2 || parts[0].len() != 4 || parts[1].len() != 4 {
        return None;
    }
    let year = parts[0];
    let month = &parts[1][..2];
    let day = &parts[1][2..];
    let iso = format!("{year}-{month}-{day}");
    NaiveDate::parse_from_str(&iso, "%Y-%m-%d")
        .ok()
        .map(|_| iso)
}

fn format_date_window(start: Option<NaiveDate>, end: Option<NaiveDate>) -> Option<String> {
    if start.is_none() && end.is_none() {
        return None;
    }
    Some(format!(
        "{}..{}",
        start
            .map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        end.map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    ))
}

fn matches_date_filter(date_iso: &str, filter: &DateFilter) -> bool {
    if filter.start.is_none() && filter.end.is_none() {
        return true;
    }
    let Ok(date) = NaiveDate::parse_from_str(date_iso, "%Y-%m-%d") else {
        return false;
    };
    if let Some(start) = filter.start
        && date < start
    {
        return false;
    }
    if let Some(end) = filter.end
        && date > end
    {
        return false;
    }
    true
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

fn normalized_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut was_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !was_ws {
                out.push(' ');
            }
            was_ws = true;
        } else {
            out.push(ch);
            was_ws = false;
        }
    }
    out.trim().to_string()
}

fn trim_chars(input: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return input.to_string();
    }
    let mut out = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

fn file_modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
}

fn format_modified_utc(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "unknown".to_string();
    };
    let dt: DateTime<Utc> = modified.into();
    dt.to_rfc3339()
}

fn pick_sort_ts(
    completed_at: Option<&str>,
    updated_at: Option<&str>,
    modified: Option<SystemTime>,
) -> i64 {
    completed_at
        .and_then(parse_timestamp)
        .or_else(|| updated_at.and_then(parse_timestamp))
        .or_else(|| {
            modified.map(|value| {
                let dt: DateTime<Utc> = value.into();
                dt.timestamp()
            })
        })
        .unwrap_or_default()
}

fn parse_timestamp(raw: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).timestamp())
}

fn render_reports_html(payload: &ReportsExplorerPayload, title: &str) -> Result<String> {
    let payload_json =
        serde_json::to_string(payload).context("Failed to serialize reports explorer payload")?;
    let payload_json = payload_json
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");

    Ok(format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{}</title>
  <style>{}</style>
</head>
<body>
  <div class="app-shell">
    <header class="hero">
      <div>
        <h1>Workflow Report Explorer</h1>
        <p class="meta">Embedded browse + synthesis + import for Vibecrafted artifacts</p>
        <p class="meta">Repo: {} / {} | Generated {}</p>
      </div>
      <div class="hero-stats">
        <div class="stat-card"><strong>{}</strong><span>records</span></div>
        <div class="stat-card"><strong>{}</strong><span>days</span></div>
        <div class="stat-card"><strong>{}</strong><span>workflows</span></div>
      </div>
    </header>

    <section class="tool-row">
      <div class="search-wrap">
        <input id="rx-search" type="search" placeholder="Search titles, bodies, run IDs, headings…" autocomplete="off" />
      </div>
      <button id="rx-import-trigger" type="button">Import JSON Bundle</button>
      <button id="rx-download-bundle" type="button">Download Current Bundle</button>
      <button id="rx-reset-data" type="button">Reset Embedded Data</button>
      <input id="rx-import-file" type="file" accept=".json,application/json" hidden />
    </section>

    <section class="filters">
      <select id="rx-workflow"><option value="">All workflows</option></select>
      <select id="rx-lane"><option value="">All lanes</option></select>
      <select id="rx-agent"><option value="">All agents</option></select>
      <select id="rx-status"><option value="">All statuses</option></select>
      <select id="rx-day"><option value="">All days</option></select>
    </section>

    <section class="cards" id="rx-cards"></section>

    <section class="layout">
      <aside class="list-pane">
        <div id="rx-summary" class="summary"></div>
        <div id="rx-list" class="result-list"></div>
      </aside>

      <article class="detail-pane">
        <div class="detail-head">
          <div>
            <h2 id="rx-detail-title">Select a record</h2>
            <p id="rx-detail-meta" class="detail-meta"></p>
          </div>
          <button id="rx-copy-path" type="button">Copy Path</button>
        </div>

        <div class="detail-grid" id="rx-detail-grid"></div>
        <div id="rx-detail-headings" class="chip-row"></div>
        <p id="rx-detail-preview" class="detail-preview"></p>
        <pre id="rx-detail-content" class="detail-content">Use search or filters to inspect a workflow artifact.</pre>

        <details class="assumptions" open>
          <summary>Assumptions & provenance</summary>
          <ul id="rx-assumptions"></ul>
        </details>
      </article>
    </section>
  </div>

  <script id="rx-data" type="application/json">{}</script>
  <script>{}</script>
</body>
</html>
"#,
        html_escape(title),
        REPORTS_EXTRACTOR_CSS,
        html_escape(&payload.resolved_org),
        html_escape(&payload.resolved_repo),
        html_escape(&payload.generated_at),
        payload.stats.total_records,
        payload.stats.total_days,
        payload.stats.total_workflows,
        payload_json,
        REPORTS_EXTRACTOR_SCRIPT
    ))
}

fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const REPORTS_EXTRACTOR_CSS: &str = r#"
:root {
  color-scheme: dark;
  --bg: #071018;
  --panel: rgba(13, 20, 34, 0.94);
  --panel-2: rgba(16, 26, 42, 0.98);
  --line: #203048;
  --line-2: rgba(120, 161, 201, 0.16);
  --text: #e6eef8;
  --muted: #8ea2be;
  --accent: #4fd1c5;
  --accent-2: #7dd3fc;
  --warn: #f59e0b;
  --danger: #fb7185;
  --ok: #34d399;
}

* { box-sizing: border-box; }
body {
  margin: 0;
  font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif;
  background:
    radial-gradient(1200px 700px at 20% -5%, rgba(61, 148, 255, 0.18), transparent 60%),
    radial-gradient(900px 600px at 100% 0%, rgba(79, 209, 197, 0.16), transparent 58%),
    linear-gradient(180deg, #050a12 0%, #09131f 100%);
  color: var(--text);
}

.app-shell {
  max-width: 1580px;
  margin: 0 auto;
  padding: 20px;
}

.hero {
  display: flex;
  justify-content: space-between;
  gap: 18px;
  align-items: flex-start;
  margin-bottom: 16px;
}

.hero h1 {
  margin: 0;
  font-size: 1.65rem;
}

.meta {
  margin: 6px 0 0;
  color: var(--muted);
  font-size: 0.92rem;
}

.hero-stats {
  display: grid;
  grid-template-columns: repeat(3, minmax(100px, 1fr));
  gap: 10px;
}

.stat-card {
  border: 1px solid var(--line);
  background: var(--panel);
  border-radius: 14px;
  padding: 10px 12px;
  text-align: right;
  box-shadow: 0 12px 30px rgba(0, 0, 0, 0.18);
}

.stat-card strong {
  display: block;
  font-size: 1.15rem;
}

.stat-card span {
  color: var(--muted);
  text-transform: uppercase;
  font-size: 0.72rem;
  letter-spacing: 0.06em;
}

.tool-row,
.filters {
  display: grid;
  gap: 10px;
  margin-bottom: 12px;
}

.tool-row {
  grid-template-columns: minmax(280px, 1fr) repeat(3, auto);
}

.filters {
  grid-template-columns: repeat(5, minmax(130px, 1fr));
}

.search-wrap input,
.filters select,
.tool-row button {
  width: 100%;
  border: 1px solid var(--line);
  background: var(--panel);
  color: var(--text);
  border-radius: 12px;
  padding: 11px 13px;
  font-size: 0.96rem;
}

.tool-row button {
  cursor: pointer;
  transition: transform 0.14s ease, border-color 0.14s ease, background 0.14s ease;
}

.tool-row button:hover {
  transform: translateY(-1px);
  border-color: var(--accent);
  background: rgba(22, 35, 54, 0.98);
}

.search-wrap input:focus,
.filters select:focus {
  outline: none;
  border-color: var(--accent);
  box-shadow: 0 0 0 2px rgba(79, 209, 197, 0.16);
}

.cards {
  display: grid;
  grid-template-columns: repeat(5, minmax(120px, 1fr));
  gap: 10px;
  margin-bottom: 14px;
}

.metric {
  border: 1px solid var(--line-2);
  background: linear-gradient(180deg, rgba(15, 25, 41, 0.98), rgba(10, 18, 30, 0.96));
  border-radius: 14px;
  padding: 12px;
}

.metric strong {
  display: block;
  font-size: 1.28rem;
}

.metric span {
  display: block;
  margin-top: 6px;
  color: var(--muted);
  font-size: 0.78rem;
  text-transform: uppercase;
  letter-spacing: 0.06em;
}

.layout {
  display: grid;
  grid-template-columns: minmax(320px, 0.95fr) minmax(420px, 1.4fr);
  gap: 14px;
  min-height: calc(100vh - 300px);
}

.list-pane,
.detail-pane {
  border: 1px solid var(--line);
  border-radius: 18px;
  background: linear-gradient(180deg, var(--panel), var(--panel-2));
  box-shadow: 0 18px 45px rgba(0, 0, 0, 0.22);
  overflow: hidden;
  min-width: 0;
}

.summary {
  padding: 14px 16px;
  color: var(--muted);
  border-bottom: 1px solid var(--line);
}

.result-list {
  max-height: calc(100vh - 390px);
  overflow: auto;
}

.result-item {
  width: 100%;
  text-align: left;
  border: 0;
  border-bottom: 1px solid rgba(255, 255, 255, 0.04);
  background: transparent;
  color: inherit;
  padding: 14px 16px;
  cursor: pointer;
}

.result-item:hover,
.result-item.active {
  background: rgba(79, 209, 197, 0.09);
}

.result-title {
  font-weight: 600;
  margin-bottom: 8px;
}

.badge-row,
.chip-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.badge,
.chip {
  display: inline-flex;
  align-items: center;
  padding: 3px 8px;
  border-radius: 999px;
  font-size: 0.74rem;
  border: 1px solid var(--line);
  color: var(--muted);
  background: rgba(255, 255, 255, 0.03);
}

.badge.ok { color: var(--ok); border-color: rgba(52, 211, 153, 0.25); }
.badge.warn { color: var(--warn); border-color: rgba(245, 158, 11, 0.25); }
.badge.danger { color: var(--danger); border-color: rgba(251, 113, 133, 0.25); }

.result-preview {
  margin-top: 10px;
  color: var(--muted);
  font-size: 0.88rem;
  line-height: 1.45;
}

.detail-head {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  align-items: flex-start;
  padding: 16px 18px 10px;
  border-bottom: 1px solid var(--line);
}

.detail-head h2 {
  margin: 0;
  font-size: 1.2rem;
}

.detail-head button {
  border: 1px solid var(--line);
  background: var(--panel);
  color: var(--text);
  border-radius: 10px;
  padding: 8px 10px;
  cursor: pointer;
}

.detail-meta,
.detail-preview {
  color: var(--muted);
}

.detail-meta {
  margin: 6px 0 0;
}

.detail-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px;
  padding: 14px 18px 4px;
}

.detail-cell {
  border: 1px solid rgba(255, 255, 255, 0.04);
  border-radius: 10px;
  background: rgba(255, 255, 255, 0.02);
  padding: 8px 10px;
}

.detail-cell strong {
  display: block;
  font-size: 0.74rem;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.06em;
  margin-bottom: 4px;
}

.detail-cell span {
  display: block;
  word-break: break-word;
  line-height: 1.45;
}

.detail-preview,
.chip-row,
.detail-content,
.assumptions {
  margin-left: 18px;
  margin-right: 18px;
}

.detail-content {
  white-space: pre-wrap;
  word-break: break-word;
  padding: 16px;
  border: 1px solid rgba(255, 255, 255, 0.04);
  border-radius: 14px;
  background: rgba(4, 10, 18, 0.78);
  min-height: 280px;
  max-height: calc(100vh - 520px);
  overflow: auto;
}

.assumptions {
  margin-bottom: 16px;
}

.assumptions summary {
  cursor: pointer;
  color: var(--muted);
}

.empty-state {
  padding: 18px;
  color: var(--muted);
}

@media (max-width: 1100px) {
  .tool-row { grid-template-columns: 1fr 1fr; }
  .filters { grid-template-columns: repeat(2, 1fr); }
  .cards { grid-template-columns: repeat(2, 1fr); }
  .layout { grid-template-columns: 1fr; }
  .result-list { max-height: none; }
  .detail-content { max-height: none; }
}
"#;

const REPORTS_EXTRACTOR_SCRIPT: &str = r#"
(function() {
  const readEmbedded = () => {
    const node = document.getElementById('rx-data');
    return node ? JSON.parse(node.textContent || '{}') : { records: [] };
  };

  const normalizeText = (value) => {
    const chars = {
      '\u0141': 'L', '\u0142': 'l', '\u0104': 'A', '\u0105': 'a',
      '\u0106': 'C', '\u0107': 'c', '\u0118': 'E', '\u0119': 'e',
      '\u0143': 'N', '\u0144': 'n', '\u00d3': 'O', '\u00f3': 'o',
      '\u015a': 'S', '\u015b': 's', '\u0179': 'Z', '\u017a': 'z',
      '\u017b': 'Z', '\u017c': 'z'
    };
    return String(value || '')
      .replace(/[\u0141\u0142\u0104\u0105\u0106\u0107\u0118\u0119\u0143\u0144\u00d3\u00f3\u015a\u015b\u0179\u017a\u017b\u017c]/g, (ch) => chars[ch] || ch)
      .toLowerCase();
  };

  const embedded = readEmbedded();
  const state = {
    base: embedded,
    payload: embedded,
    records: Array.isArray(embedded.records) ? embedded.records.slice() : [],
    selectedKey: null,
    query: '',
    filters: {
      workflow: '',
      lane: '',
      agent: '',
      status: '',
      day: ''
    }
  };

  const ui = {
    search: document.getElementById('rx-search'),
    workflow: document.getElementById('rx-workflow'),
    lane: document.getElementById('rx-lane'),
    agent: document.getElementById('rx-agent'),
    status: document.getElementById('rx-status'),
    day: document.getElementById('rx-day'),
    cards: document.getElementById('rx-cards'),
    summary: document.getElementById('rx-summary'),
    list: document.getElementById('rx-list'),
    detailTitle: document.getElementById('rx-detail-title'),
    detailMeta: document.getElementById('rx-detail-meta'),
    detailGrid: document.getElementById('rx-detail-grid'),
    detailHeadings: document.getElementById('rx-detail-headings'),
    detailPreview: document.getElementById('rx-detail-preview'),
    detailContent: document.getElementById('rx-detail-content'),
    assumptions: document.getElementById('rx-assumptions'),
    importTrigger: document.getElementById('rx-import-trigger'),
    importFile: document.getElementById('rx-import-file'),
    downloadBundle: document.getElementById('rx-download-bundle'),
    resetData: document.getElementById('rx-reset-data'),
    copyPath: document.getElementById('rx-copy-path')
  };

  const selectOptions = (node, values, placeholder) => {
    const current = node.value;
    node.innerHTML = '';
    const opt = document.createElement('option');
    opt.value = '';
    opt.textContent = placeholder;
    node.appendChild(opt);
    values.forEach((value) => {
      const entry = document.createElement('option');
      entry.value = value;
      entry.textContent = value;
      node.appendChild(entry);
    });
    node.value = values.includes(current) ? current : '';
  };

  const updateFilterOptions = () => {
    const payload = state.payload || { workflows: [], lanes: [], agents: [], statuses: [], days: [] };
    selectOptions(ui.workflow, payload.workflows || [], 'All workflows');
    selectOptions(ui.lane, payload.lanes || [], 'All lanes');
    selectOptions(ui.agent, payload.agents || [], 'All agents');
    selectOptions(ui.status, payload.statuses || [], 'All statuses');
    selectOptions(ui.day, payload.days || [], 'All days');
  };

  const mergePayload = (incoming) => {
    if (!incoming || !Array.isArray(incoming.records)) {
      throw new Error('Imported file does not look like an AICX reports bundle.');
    }
    const merged = new Map();
    [...(state.payload.records || []), ...incoming.records].forEach((record) => {
      merged.set(record.key || record.absolute_path || record.relative_path, record);
    });
    const records = Array.from(merged.values()).sort((a, b) => (b.sort_ts || 0) - (a.sort_ts || 0));
    state.payload = {
      schema_version: incoming.schema_version || state.payload.schema_version || 1,
      generated_at: incoming.generated_at || state.payload.generated_at,
      artifacts_root: incoming.artifacts_root || state.payload.artifacts_root,
      resolved_org: incoming.resolved_org || state.payload.resolved_org,
      resolved_repo: incoming.resolved_repo || state.payload.resolved_repo,
      scan_root: incoming.scan_root || state.payload.scan_root,
      selected_date: state.payload.selected_date,
      selected_workflow: state.payload.selected_workflow,
      stats: state.payload.stats || {},
      assumptions: Array.from(new Set([...(state.payload.assumptions || []), ...(incoming.assumptions || [])])),
      workflows: Array.from(new Set(records.map((record) => record.workflow).filter(Boolean))).sort(),
      agents: Array.from(new Set(records.map((record) => record.agent).filter(Boolean))).sort(),
      statuses: Array.from(new Set(records.map((record) => record.status).filter(Boolean))).sort(),
      lanes: Array.from(new Set(records.map((record) => record.lane).filter(Boolean))).sort(),
      days: Array.from(new Set(records.map((record) => record.date_iso).filter(Boolean))).sort(),
      records
    };
    state.records = records;
    updateFilterOptions();
    render();
  };

  const filteredRecords = () => {
    const query = normalizeText(state.query);
    return (state.payload.records || []).filter((record) => {
      if (state.filters.workflow && record.workflow !== state.filters.workflow) return false;
      if (state.filters.lane && record.lane !== state.filters.lane) return false;
      if (state.filters.agent && record.agent !== state.filters.agent) return false;
      if (state.filters.status && record.status !== state.filters.status) return false;
      if (state.filters.day && record.date_iso !== state.filters.day) return false;
      if (!query) return true;
      return normalizeText(record.search_blob || '').includes(query);
    });
  };

  const metricCard = (label, value) => {
    const div = document.createElement('div');
    div.className = 'metric';
    const strong = document.createElement('strong');
    strong.textContent = String(value);
    const span = document.createElement('span');
    span.textContent = label;
    div.appendChild(strong);
    div.appendChild(span);
    return div;
  };

  const statusClass = (status) => {
    const normalized = String(status || '').toLowerCase();
    if (normalized === 'completed') return 'ok';
    if (normalized === 'launching' || normalized === 'planned' || normalized === 'running') return 'warn';
    return normalized ? 'danger' : '';
  };

  const renderCards = (records) => {
    ui.cards.innerHTML = '';
    const complete = records.filter((record) => String(record.status).toLowerCase() === 'completed').length;
    const partial = records.length - complete;
    const metaOnly = records.filter((record) => record.has_meta && !record.has_markdown).length;
    const workflows = new Set(records.map((record) => record.workflow).filter(Boolean)).size;
    const agents = new Set(records.map((record) => record.agent).filter(Boolean)).size;
    [
      ['visible records', records.length],
      ['completed', complete],
      ['partial/incomplete', partial],
      ['meta only', metaOnly],
      ['workflows', workflows || 0],
      ['agents', agents || 0]
    ].forEach(([label, value]) => ui.cards.appendChild(metricCard(label, value)));
  };

  const renderList = (records) => {
    ui.list.innerHTML = '';
    if (!records.length) {
      const empty = document.createElement('div');
      empty.className = 'empty-state';
      empty.textContent = 'No artifacts matched the current filters.';
      ui.list.appendChild(empty);
      return;
    }

    records.forEach((record) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'result-item' + (record.key === state.selectedKey ? ' active' : '');
      button.addEventListener('click', () => {
        state.selectedKey = record.key;
        render();
      });

      const title = document.createElement('div');
      title.className = 'result-title';
      title.textContent = record.title || record.file_name || 'artifact';
      button.appendChild(title);

      const badges = document.createElement('div');
      badges.className = 'badge-row';
      [
        record.workflow,
        record.lane,
        record.status,
        record.agent,
        record.date_iso
      ].filter(Boolean).forEach((value, idx) => {
        const badge = document.createElement('span');
        badge.className = 'badge' + (idx === 2 ? ' ' + statusClass(value) : '');
        badge.textContent = value;
        badges.appendChild(badge);
      });
      button.appendChild(badges);

      const preview = document.createElement('p');
      preview.className = 'result-preview';
      preview.textContent = record.preview || '';
      button.appendChild(preview);

      ui.list.appendChild(button);
    });
  };

  const detailCell = (label, value) => {
    if (!value) return null;
    const div = document.createElement('div');
    div.className = 'detail-cell';
    const strong = document.createElement('strong');
    strong.textContent = label;
    const span = document.createElement('span');
    span.textContent = String(value);
    div.appendChild(strong);
    div.appendChild(span);
    return div;
  };

  const renderDetail = (record) => {
    if (!record) {
      ui.detailTitle.textContent = 'Select a record';
      ui.detailMeta.textContent = '';
      ui.detailGrid.innerHTML = '';
      ui.detailHeadings.innerHTML = '';
      ui.detailPreview.textContent = '';
      ui.detailContent.textContent = 'Use search or filters to inspect a workflow artifact.';
      return;
    }

    ui.detailTitle.textContent = record.title || record.file_name || 'artifact';
    ui.detailMeta.textContent = [record.workflow, record.lane, record.status, record.agent, record.date_iso].filter(Boolean).join(' • ');
    ui.detailPreview.textContent = record.preview || '';
    ui.detailContent.textContent = record.detail_text || '';
    ui.copyPath.dataset.path = record.absolute_path || '';

    ui.detailGrid.innerHTML = '';
    [
      ['absolute path', record.absolute_path],
      ['relative path', record.relative_path],
      ['run id', record.run_id],
      ['prompt id', record.prompt_id],
      ['skill code', record.skill_code],
      ['mode', record.mode],
      ['completed at', record.completed_at],
      ['updated at', record.updated_at],
      ['duration (s)', record.duration_s],
      ['session id', record.session_id],
      ['transcript', record.transcript_path],
      ['launcher', record.launcher_path]
    ].forEach(([label, value]) => {
      const cell = detailCell(label, value);
      if (cell) ui.detailGrid.appendChild(cell);
    });

    ui.detailHeadings.innerHTML = '';
    (record.headings || []).forEach((heading) => {
      const chip = document.createElement('span');
      chip.className = 'chip';
      chip.textContent = heading;
      ui.detailHeadings.appendChild(chip);
    });
  };

  const renderAssumptions = () => {
    ui.assumptions.innerHTML = '';
    (state.payload.assumptions || []).forEach((item) => {
      const li = document.createElement('li');
      li.textContent = item;
      ui.assumptions.appendChild(li);
    });
  };

  const render = () => {
    const records = filteredRecords();
    renderCards(records);
    renderList(records);
    const selected = records.find((record) => record.key === state.selectedKey) || records[0] || null;
    if (selected) {
      state.selectedKey = selected.key;
    }
    renderDetail(selected);
    renderAssumptions();
    ui.summary.textContent = `Showing ${records.length} of ${(state.payload.records || []).length} records from ${state.payload.resolved_org || ''}/${state.payload.resolved_repo || ''}.`;
  };

  ui.search.addEventListener('input', () => {
    state.query = ui.search.value || '';
    render();
  });
  [['workflow', ui.workflow], ['lane', ui.lane], ['agent', ui.agent], ['status', ui.status], ['day', ui.day]].forEach(([key, node]) => {
    node.addEventListener('change', () => {
      state.filters[key] = node.value || '';
      render();
    });
  });

  ui.importTrigger.addEventListener('click', () => ui.importFile.click());
  ui.importFile.addEventListener('change', async () => {
    const file = ui.importFile.files && ui.importFile.files[0];
    if (!file) return;
    const text = await file.text();
    mergePayload(JSON.parse(text));
    ui.importFile.value = '';
  });

  ui.downloadBundle.addEventListener('click', () => {
    const blob = new Blob([JSON.stringify(state.payload, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `${(state.payload.resolved_repo || 'aicx-reports').replace(/[^a-z0-9_-]+/gi, '-')}.bundle.json`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
  });

  ui.resetData.addEventListener('click', () => {
    state.payload = state.base;
    state.records = Array.isArray(state.base.records) ? state.base.records.slice() : [];
    state.selectedKey = null;
    updateFilterOptions();
    render();
  });

  ui.copyPath.addEventListener('click', async () => {
    const path = ui.copyPath.dataset.path || '';
    if (!path) return;
    try {
      await navigator.clipboard.writeText(path);
      ui.copyPath.textContent = 'Copied';
      setTimeout(() => { ui.copyPath.textContent = 'Copy Path'; }, 900);
    } catch (_) {
      ui.copyPath.textContent = path;
    }
  });

  updateFilterOptions();
  render();
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

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
        let repo_root = root.join("VetCoders").join("ai-contexters");
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
  "transcript": "__TRANSCRIPT__"
}"#
            .replace("__TRANSCRIPT__", &transcript.display().to_string()),
        );
        write_file(&transcript, "[13:16:11] assistant: booting artifact scan\n");

        let config = ReportsExtractorConfig {
            artifacts_root: root.clone(),
            org: "VetCoders".to_string(),
            repo: "ai-contexters".to_string(),
            date_from: Some(NaiveDate::from_ymd_opt(2026, 4, 11).expect("date")),
            date_to: Some(NaiveDate::from_ymd_opt(2026, 4, 12).expect("date")),
            workflow: None,
            title: "AICX Reports Explorer".to_string(),
            preview_chars: 120,
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
    fn infers_day_root_workflow_from_prompt_ids_and_file_stems() {
        assert_eq!(
            prompt_workflow_slug(Some("report-artifacts-dashboard_20260412")).as_deref(),
            Some("report-artifacts-dashboard")
        );
        assert_eq!(
            stem_workflow_slug(Path::new(
                "/tmp/20260412_2031_report-artifacts-dashboard_codex.md"
            ))
            .as_deref(),
            Some("report-artifacts-dashboard")
        );
        assert_eq!(
            title_workflow_slug("Examination: report artifacts dashboard").as_deref(),
            Some("examination-report-artifacts-dashboard")
        );
    }
}
