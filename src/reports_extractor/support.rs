use super::types::{ArtifactFrontmatterEnvelope, ArtifactMeta, DateFilter};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, NaiveDate, Utc};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MAX_MARKDOWN_DETAIL_CHARS: usize = 48_000;
const MAX_TRANSCRIPT_TAIL_BYTES: u64 = 48 * 1024;

pub(super) fn validate_artifact_dir(scan_root: &Path, path: &Path) -> Result<PathBuf> {
    let validated = crate::sanitize::validate_dir_path(path)?;
    ensure_artifact_descendant(scan_root, &validated)?;
    Ok(validated)
}

pub(super) fn validate_artifact_file(scan_root: &Path, path: &Path) -> Result<PathBuf> {
    let validated = crate::sanitize::validate_read_path(path)?;
    ensure_artifact_descendant(scan_root, &validated)?;
    if !validated.is_file() {
        return Err(anyhow!(
            "Artifact path is not a file: {}",
            validated.display()
        ));
    }
    Ok(validated)
}

fn ensure_artifact_descendant(scan_root: &Path, path: &Path) -> Result<()> {
    if !path.starts_with(scan_root) {
        return Err(anyhow!(
            "Artifact path escapes scan root: {} is outside {}",
            path.display(),
            scan_root.display()
        ));
    }
    Ok(())
}

pub(super) fn path_string_without_suffix(path: &Path, suffix: &str) -> String {
    let value = path.display().to_string();
    value.strip_suffix(suffix).unwrap_or(&value).to_string()
}

pub(super) fn build_record_key(
    run_id: Option<&str>,
    absolute_path: &str,
    relative_path: &str,
    meta_path: Option<&str>,
) -> String {
    // Composite identity: relative_path is ALWAYS part of the key so that two
    // artifacts that happen to share a `run_id` (or even a `meta_path` from a
    // sibling tree) do not collide in the explorer payload or in the JS
    // mergePayload Map. Format: `run:{run_id}@path:{relative_path}`,
    // `meta:{meta_path}@path:{relative_path}`, or
    // `path:{absolute_path}:{relative_path}` as a last resort.
    if let Some(value) = run_id {
        format!("run:{value}@path:{relative_path}")
    } else if let Some(value) = meta_path {
        format!("meta:{value}@path:{relative_path}")
    } else {
        format!("path:{absolute_path}:{relative_path}")
    }
}

#[derive(Debug, Clone)]
pub(super) struct ParsedMarkdown {
    pub(super) frontmatter: ArtifactFrontmatterEnvelope,
    pub(super) body: String,
    pub(super) headings: Vec<String>,
}

pub(super) fn read_markdown(scan_root: &Path, path: &Path) -> Result<ParsedMarkdown> {
    let path = validate_artifact_file(scan_root, path)?;
    let raw = crate::sanitize::read_to_string_validated(&path)
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

pub(super) fn read_meta(scan_root: &Path, path: &Path) -> Result<ArtifactMeta> {
    let path = validate_artifact_file(scan_root, path)?;
    let raw = crate::sanitize::read_to_string_validated(&path)
        .with_context(|| format!("Failed to read artifact metadata: {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse artifact metadata: {}", path.display()))
}

pub(super) fn resolve_artifact_reference(
    scan_root: &Path,
    origin: &Path,
    raw_path: &str,
) -> Option<PathBuf> {
    let raw_path = Path::new(raw_path);
    let candidate = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        origin.parent().unwrap_or(scan_root).join(raw_path)
    };
    validate_artifact_file(scan_root, &candidate).ok()
}

pub(super) fn parse_artifact_frontmatter(
    text: &str,
) -> (Option<ArtifactFrontmatterEnvelope>, &str) {
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

pub(super) fn derive_lane_and_workflow(
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

pub(super) fn prompt_workflow_slug(prompt_id: Option<&str>) -> Option<String> {
    let prompt_id = prompt_id?;
    let prompt_id = prompt_id.trim();
    if prompt_id.is_empty() {
        return None;
    }

    prompt_id
        .split('_')
        .filter_map(normalize_content_slug_part)
        .next()
}

pub(super) fn stem_workflow_slug(path: &Path) -> Option<String> {
    let stem = artifact_stem(path)?;
    let filtered = stem
        .split('_')
        .filter_map(normalize_content_slug_part)
        .collect::<Vec<_>>()
        .join("-");
    normalize_workflow_slug(&filtered)
}

pub(super) fn title_workflow_slug(title: &str) -> Option<String> {
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

    if slug.is_empty() || is_boilerplate_slug(&slug) {
        None
    } else {
        Some(slug)
    }
}

fn normalize_content_slug_part(segment: &str) -> Option<String> {
    let normalized = normalize_workflow_slug(segment)?;
    if looks_like_timestamp_segment(&normalized)
        || looks_like_run_id_segment(&normalized)
        || is_known_artifact_suffix(&normalized)
        || is_known_agent(&normalized)
    {
        None
    } else {
        Some(normalized)
    }
}

fn artifact_stem(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    for suffix in [
        ".meta.json",
        ".transcript.log",
        "_launch.sh",
        ".launch.sh",
        ".md",
        ".json",
        ".log",
        ".sh",
    ] {
        if let Some(stem) = name.strip_suffix(suffix) {
            return Some(stem.to_string());
        }
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string())
}

fn looks_like_timestamp_segment(segment: &str) -> bool {
    let digits_only = segment.chars().all(|ch| ch.is_ascii_digit());
    digits_only && matches!(segment.len(), 4 | 6 | 8 | 12 | 14)
}

fn looks_like_run_id_segment(segment: &str) -> bool {
    let mut parts = segment.split('-');
    let Some(prefix) = parts.next() else {
        return false;
    };
    if !is_known_run_id_prefix(prefix) {
        return false;
    }
    parts.any(|part| part.len() >= 4 && part.chars().all(|ch| ch.is_ascii_digit()))
}

fn is_known_run_id_prefix(prefix: &str) -> bool {
    matches!(
        prefix,
        "just"
            | "impl"
            | "rsch"
            | "revw"
            | "audt"
            | "mrbl"
            | "marb"
            | "dou"
            | "init"
            | "rel"
            | "workflow"
            | "followup"
            | "wflow"
    )
}

fn is_boilerplate_slug(slug: &str) -> bool {
    matches!(
        slug,
        "perform-the-vc-justdo-skill-on-this-repository"
            | "justdo-skill-on-this-repository"
            | "perform-the-skill"
            | "perform-skill"
            | "do-this-task"
            | "this-task"
            | "untitled"
    ) || (slug.starts_with("perform-the-vc-") && slug.ends_with("-skill-on-this-repository"))
        || (slug.starts_with("perform-") && slug.ends_with("-skill-on-this-repository"))
}

fn is_known_artifact_suffix(segment: &str) -> bool {
    matches!(
        segment.to_ascii_lowercase().as_str(),
        "context"
            | "research"
            | "report"
            | "reports"
            | "plan"
            | "plans"
            | "summary"
            | "meta"
            | "transcript"
            | "log"
            | "launch"
            | "launcher"
    )
}

pub(super) fn path_contains_segment(path_parts: &[String], needle: &str) -> bool {
    path_parts.iter().any(|segment| segment == needle)
}

pub(super) fn relative_components(path: &Path) -> Vec<String> {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect()
}

pub(super) fn derive_title(
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

pub(super) fn derive_agent(
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

pub(super) fn derive_status(
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

pub(super) fn build_detail_text(
    scan_root: &Path,
    markdown: Option<&ParsedMarkdown>,
    transcript_path: Option<&Path>,
    meta: Option<&ArtifactMeta>,
) -> String {
    if let Some(markdown) = markdown {
        return trim_chars(&markdown.body, MAX_MARKDOWN_DETAIL_CHARS);
    }

    if let Some(path) = transcript_path
        && let Ok(text) = read_tail_string(scan_root, path, MAX_TRANSCRIPT_TAIL_BYTES)
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

pub(super) fn build_preview(
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

fn read_tail_string(scan_root: &Path, path: &Path, max_bytes: u64) -> Result<String> {
    let path = validate_artifact_file(scan_root, path)?;
    let mut file = crate::sanitize::open_file_validated(&path)
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

pub(super) fn normalize_date_bucket(bucket: &str) -> Option<String> {
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

pub(super) fn format_date_window(
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Option<String> {
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

pub(super) fn matches_date_filter(date_iso: &str, filter: &DateFilter) -> bool {
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

pub(super) fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

pub(super) fn normalized_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

pub(super) fn collapse_ws(s: &str) -> String {
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

pub(super) fn file_modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
}

pub(super) fn format_modified_utc(modified: Option<SystemTime>) -> String {
    let Some(modified) = modified else {
        return "unknown".to_string();
    };
    let dt: DateTime<Utc> = modified.into();
    dt.to_rfc3339()
}

pub(super) fn pick_sort_ts(
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
