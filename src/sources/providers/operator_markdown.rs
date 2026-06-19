#![allow(unused_imports)]
use crate::sources::*;
use chrono::{Duration, NaiveDate, NaiveTime, TimeZone};
use serde::Deserialize;

use crate::timeline::FrameKind;

const OPERATOR_MD_AGENT: &str = "operator";
const OPERATOR_MD_KIND: &str = "operator-md";
/// Default discovery window applied when a caller does NOT supply its own cutoff.
///
/// Historically this acted as an unconditional ceiling, which silently capped
/// `aicx store --agent operator-md -H 0` (all-time backfill) at 30 days. It
/// is now a *default* honored only when `caller_cutoff` is `None` in
/// [`discover_operator_markdown_from`]. Callers that thread an
/// `ExtractionConfig::cutoff` through (e.g. the store pipeline) bypass this
/// default entirely, so explicit lookback flags are honored.
const OPERATOR_MD_RECENT_DAYS: i64 = 30;

/// A discovered operator-authored markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorMarkdown {
    pub path: PathBuf,
    pub modified: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct OperatorMarkdownFrontmatter {
    #[serde(default, rename = "aicx_import")]
    _aicx_import: Option<serde_yaml::Value>,
    #[serde(default)]
    pub(crate) project: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    source_format: Option<String>,
}

pub fn discover_operator_markdown(home: &Path) -> Vec<OperatorMarkdown> {
    discover_operator_markdown_from(home, None, None)
}

/// Discover operator markdown from an explicit file or directory.
///
/// Directory inputs scan only their direct `.md` children; recursive import is
/// intentionally left to a future explicit flag so an accidental home/workspace
/// path does not walk a huge tree.
pub fn discover_operator_markdown_from_input(input: &Path) -> Result<Vec<OperatorMarkdown>> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    collect_operator_markdown_input(input, &mut entries, &mut seen)?;
    entries.sort_by_key(|entry| (entry.modified, entry.path.clone()));
    Ok(entries)
}

fn collect_operator_markdown_input(
    input: &Path,
    entries: &mut Vec<OperatorMarkdown>,
    seen: &mut HashSet<PathBuf>,
) -> Result<()> {
    let meta = fs::metadata(input)
        .with_context(|| format!("operator-md input not readable: {}", input.display()))?;
    if meta.is_file() {
        push_operator_markdown_input(input, meta, entries, seen);
        return Ok(());
    }
    if meta.is_dir() {
        for entry in fs::read_dir(input)
            .with_context(|| {
                format!(
                    "operator-md input directory not readable: {}",
                    input.display()
                )
            })?
            .flatten()
        {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_file() {
                push_operator_markdown_input(&path, meta, entries, seen);
            }
        }
        return Ok(());
    }
    anyhow::bail!(
        "operator-md input is neither file nor directory: {}",
        input.display()
    );
}

fn push_operator_markdown_input(
    path: &Path,
    meta: fs::Metadata,
    entries: &mut Vec<OperatorMarkdown>,
    seen: &mut HashSet<PathBuf>,
) {
    if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
        return;
    }
    let Ok(modified) = meta.modified() else {
        return;
    };
    if !seen.insert(path.to_path_buf()) {
        return;
    }
    entries.push(OperatorMarkdown {
        path: path.to_path_buf(),
        modified: DateTime::<Utc>::from(modified),
    });
}

/// Discover operator markdown files, optionally including `<repo>/docs/operator`.
///
/// `caller_cutoff` is the earliest file-mtime the caller is interested in:
/// - `None` falls back to a 30-day default window
///   ([`OPERATOR_MD_RECENT_DAYS`]). This is the legacy convenience for source
///   enumeration paths that have no [`ExtractionConfig`] to hand in.
/// - `Some(t)` honors `t` directly. `t = UNIX epoch` therefore means
///   "all time", which is what `aicx store --agent operator-md -H 0` needs.
pub fn discover_operator_markdown_from(
    home: &Path,
    repo_root: Option<&Path>,
    caller_cutoff: Option<DateTime<Utc>>,
) -> Vec<OperatorMarkdown> {
    let mut dirs = vec![
        home.join("Downloads"),
        home.join(".vibecrafted").join("inbox"),
    ];
    if let Some(repo_root) = repo_root {
        dirs.push(repo_root.join("docs").join("operator"));
    }

    let cutoff =
        caller_cutoff.unwrap_or_else(|| Utc::now() - Duration::days(OPERATOR_MD_RECENT_DAYS));
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    for dir in dirs {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let modified = DateTime::<Utc>::from(modified);
            if modified < cutoff || !seen.insert(path.clone()) {
                continue;
            }
            entries.push(OperatorMarkdown { path, modified });
        }
    }

    entries.sort_by_key(|entry| (entry.modified, entry.path.clone()));
    entries
}

/// Extract operator-authored markdown from Downloads, the Vibecrafted inbox,
/// and the current repo's `docs/operator` directory when present.
pub fn extract_operator_markdown(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let home = dirs::home_dir().context("No home dir")?;
    let repo_root = std::env::current_dir()
        .ok()
        .and_then(|cwd| discover_git_root_from_path(&cwd));
    extract_operator_markdown_from_home_and_repo(&home, repo_root.as_deref(), config)
}

/// Extract operator-authored markdown using an explicit home directory.
pub fn extract_operator_markdown_from_home(
    home: &Path,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    extract_operator_markdown_from_home_and_repo(home, None, config)
}

/// Extract operator-authored markdown using explicit home and repo roots.
pub fn extract_operator_markdown_from_home_and_repo(
    home: &Path,
    repo_root: Option<&Path>,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let mut entries = Vec::new();

    for document in discover_operator_markdown_from(home, repo_root, Some(config.cutoff)) {
        match parse_operator_markdown_document(home, &document, config, true, false) {
            Ok(mut parsed) => entries.append(&mut parsed),
            Err(e) => eprintln!(
                "Operator markdown extraction warning ({}): {}",
                document.path.display(),
                e
            ),
        }
    }

    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

/// Extract operator markdown from an explicit file or directory.
///
/// This is the bridge for ad-hoc `.md` exports: the file still goes through the
/// same operator-md parser and canonical store writer, but discovery is scoped
/// to the provided path rather than Downloads / inbox / docs/operator.
pub fn extract_operator_markdown_from_input(
    home: &Path,
    input: &Path,
    config: &ExtractionConfig,
) -> Result<Vec<TimelineEntry>> {
    let mut entries = Vec::new();
    for document in discover_operator_markdown_from_input(input)? {
        match parse_operator_markdown_document(home, &document, config, false, true) {
            Ok(mut parsed) => entries.append(&mut parsed),
            Err(e) => eprintln!(
                "Operator markdown extraction warning ({}): {}",
                document.path.display(),
                e
            ),
        }
    }
    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

fn parse_operator_markdown_document(
    home: &Path,
    document: &OperatorMarkdown,
    config: &ExtractionConfig,
    allow_body_project_inference: bool,
    plain_markdown_fallback: bool,
) -> Result<Vec<TimelineEntry>> {
    let content = sanitize::read_to_string_validated(&document.path)?;
    let (frontmatter, body) = split_operator_frontmatter(&content);
    let project_hint = infer_operator_project_hint(
        &frontmatter,
        &body,
        &document.path,
        config,
        allow_body_project_inference,
    );
    let cwd_hint = frontmatter
        .cwd
        .as_deref()
        .and_then(|cwd| normalize_operator_frontmatter_cwd(home, cwd))
        .or_else(|| resolve_operator_cwd_hint(home, &document.path, project_hint.as_deref()));
    let base_timestamp = frontmatter
        .date
        .as_deref()
        .and_then(parse_operator_timestamp)
        .unwrap_or(document.modified);
    let session_id = frontmatter
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            format!(
                "{}-{}",
                operator_path_fingerprint(&document.path),
                document
                    .path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy())
                    .unwrap_or_else(|| "operator-md".into())
            )
        });

    if let Some(entries) = parse_chatgpt_markdown_document(
        &body,
        &document.path,
        &frontmatter,
        &session_id,
        cwd_hint.clone(),
        base_timestamp,
        config,
    ) {
        return Ok(entries);
    }

    let mut entries = Vec::new();
    let mut heading: Option<String> = None;
    let mut sequence = 0i64;

    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(next_heading) = parse_markdown_heading(line) {
            heading = Some(next_heading);
            continue;
        }

        let parsed = if let Some((done, task)) = parse_operator_checklist_task(line) {
            if done {
                None
            } else {
                Some(OperatorMarkdownSignal {
                    kind: "task",
                    severity: None,
                    display_line: format!("- [ ] {task}"),
                    text: task,
                })
            }
        } else if let Some(decision) = strip_operator_prefix(line, "Decision:") {
            Some(OperatorMarkdownSignal {
                kind: "decision",
                severity: None,
                text: decision.to_string(),
                display_line: format!("Decision: {}", decision.trim()),
            })
        } else if let Some(outcome) = strip_operator_prefix(line, "Outcome:") {
            Some(OperatorMarkdownSignal {
                kind: "outcome",
                severity: None,
                text: outcome.to_string(),
                display_line: format!("Outcome: {}", outcome.trim()),
            })
        } else {
            operator_severity_marker(line).map(|severity| {
                let text = strip_operator_severity_prefix(line, severity);
                OperatorMarkdownSignal {
                    kind: "intent",
                    severity: Some(severity),
                    text: text.to_string(),
                    display_line: format!("Intent: [{severity}] {}", text.trim()),
                }
            })
        };

        let Some(signal) = parsed else {
            continue;
        };
        let timestamp = base_timestamp + Duration::seconds(sequence);
        sequence += 1;
        if timestamp < config.cutoff || config.watermark.is_some_and(|w| timestamp < w) {
            continue;
        }

        entries.push(build_timeline_entry(
            timestamp,
            OPERATOR_MD_AGENT,
            &session_id,
            "user",
            format_operator_markdown_message(
                &document.path,
                &frontmatter,
                heading.as_deref(),
                &signal,
            ),
            TimelineEntryMeta {
                cwd: cwd_hint.clone(),
                frame_kind: Some(FrameKind::UserMsg),
                ..TimelineEntryMeta::default()
            },
        ));
    }

    if entries.is_empty()
        && plain_markdown_fallback
        && let Some(entry) = parse_plain_markdown_document(
            &body,
            &document.path,
            &frontmatter,
            &session_id,
            cwd_hint,
            base_timestamp,
            config,
        )
    {
        entries.push(entry);
    }

    Ok(entries)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatGptMarkdownSection {
    Prompt,
    Response,
}

fn parse_chatgpt_markdown_document(
    body: &str,
    path: &Path,
    frontmatter: &OperatorMarkdownFrontmatter,
    session_id: &str,
    cwd_hint: Option<String>,
    base_timestamp: DateTime<Utc>,
    config: &ExtractionConfig,
) -> Option<Vec<TimelineEntry>> {
    let mut sections: Vec<(ChatGptMarkdownSection, String)> = Vec::new();
    let mut current: Option<ChatGptMarkdownSection> = None;
    let mut lines: Vec<String> = Vec::new();

    for raw_line in body.lines() {
        if let Some(next_section) = parse_chatgpt_section_heading(raw_line) {
            if let Some(section) = current.take() {
                let text = lines.join("\n").trim().to_string();
                if !text.is_empty() {
                    sections.push((section, text));
                }
                lines.clear();
            }
            current = Some(next_section);
            continue;
        }
        if current.is_some() {
            lines.push(raw_line.to_string());
        }
    }

    if let Some(section) = current {
        let text = lines.join("\n").trim().to_string();
        if !text.is_empty() {
            sections.push((section, text));
        }
    }

    if sections.is_empty() {
        return None;
    }

    let mut entries = Vec::new();
    for (sequence, (section, text)) in sections.into_iter().enumerate() {
        let timestamp = base_timestamp + Duration::seconds(sequence as i64);
        if timestamp < config.cutoff || config.watermark.is_some_and(|w| timestamp < w) {
            continue;
        }

        let (role, frame_kind) = match section {
            ChatGptMarkdownSection::Prompt => ("user", FrameKind::UserMsg),
            ChatGptMarkdownSection::Response => ("assistant", FrameKind::AgentReply),
        };
        entries.push(build_timeline_entry(
            timestamp,
            OPERATOR_MD_AGENT,
            session_id,
            role,
            format_chatgpt_markdown_message(path, frontmatter, section, &text),
            TimelineEntryMeta {
                cwd: cwd_hint.clone(),
                frame_kind: Some(frame_kind),
                ..TimelineEntryMeta::default()
            },
        ));
    }

    Some(entries)
}

fn parse_chatgpt_section_heading(line: &str) -> Option<ChatGptMarkdownSection> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 {
        return None;
    }
    let heading = trimmed.get(level..)?.trim();
    let heading = heading.trim_end_matches(':').trim();
    if heading.eq_ignore_ascii_case("prompt") {
        Some(ChatGptMarkdownSection::Prompt)
    } else if heading.eq_ignore_ascii_case("response") {
        Some(ChatGptMarkdownSection::Response)
    } else {
        None
    }
}

fn format_chatgpt_markdown_message(
    path: &Path,
    frontmatter: &OperatorMarkdownFrontmatter,
    section: ChatGptMarkdownSection,
    text: &str,
) -> String {
    let source_format = frontmatter_source_format(frontmatter).unwrap_or("chatgpt-markdown");
    let mut message = format!(
        "source: {OPERATOR_MD_KIND}\nsource_file: {}\nsource_format: {source_format}\nsection: {}",
        path.display(),
        match section {
            ChatGptMarkdownSection::Prompt => "prompt",
            ChatGptMarkdownSection::Response => "response",
        }
    );
    if let Some(project) = frontmatter
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        message.push_str(&format!("\nproject: {}", project.trim()));
    }
    if let Some(author) = frontmatter
        .author
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        message.push_str(&format!("\nauthor: {}", author.trim()));
    }
    message.push_str("\n\n");
    message.push_str(text.trim());
    message
}

fn parse_plain_markdown_document(
    body: &str,
    path: &Path,
    frontmatter: &OperatorMarkdownFrontmatter,
    session_id: &str,
    cwd_hint: Option<String>,
    base_timestamp: DateTime<Utc>,
    config: &ExtractionConfig,
) -> Option<TimelineEntry> {
    if base_timestamp < config.cutoff || config.watermark.is_some_and(|w| base_timestamp < w) {
        return None;
    }
    let text = body.trim();
    if text.is_empty() {
        return None;
    }
    Some(build_timeline_entry(
        base_timestamp,
        OPERATOR_MD_AGENT,
        session_id,
        "user",
        format_plain_markdown_message(path, frontmatter, text),
        TimelineEntryMeta {
            cwd: cwd_hint,
            frame_kind: Some(FrameKind::UserMsg),
            ..TimelineEntryMeta::default()
        },
    ))
}

fn format_plain_markdown_message(
    path: &Path,
    frontmatter: &OperatorMarkdownFrontmatter,
    text: &str,
) -> String {
    let source_format = frontmatter_source_format(frontmatter).unwrap_or("plain-markdown");
    let mut message = format!(
        "source: {OPERATOR_MD_KIND}\nsource_file: {}\nsource_format: {source_format}\nsection: body",
        path.display()
    );
    if let Some(project) = frontmatter
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        message.push_str(&format!("\nproject: {}", project.trim()));
    }
    if let Some(author) = frontmatter
        .author
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        message.push_str(&format!("\nauthor: {}", author.trim()));
    }
    message.push_str("\n\n");
    message.push_str(text.trim());
    message
}

#[derive(Debug, Clone)]
struct OperatorMarkdownSignal {
    kind: &'static str,
    severity: Option<&'static str>,
    text: String,
    display_line: String,
}

fn format_operator_markdown_message(
    path: &Path,
    frontmatter: &OperatorMarkdownFrontmatter,
    heading: Option<&str>,
    signal: &OperatorMarkdownSignal,
) -> String {
    let mut message = format!(
        "source: {OPERATOR_MD_KIND}\nkind: {}\nsource_file: {}",
        signal.kind,
        path.display()
    );
    if let Some(severity) = signal.severity {
        message.push_str(&format!("\nseverity: {severity}"));
    }
    if let Some(project) = frontmatter
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        message.push_str(&format!("\nproject: {}", project.trim()));
    }
    if let Some(author) = frontmatter
        .author
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        message.push_str(&format!("\nauthor: {}", author.trim()));
    }
    if let Some(source_format) = frontmatter_source_format(frontmatter) {
        message.push_str(&format!("\nsource_format: {source_format}"));
    }
    if let Some(heading) = heading.filter(|value| !value.trim().is_empty()) {
        message.push_str(&format!("\nheading: {}", heading.trim()));
    }
    message.push_str("\n\n");
    message.push_str(signal.display_line.trim());
    if !signal.text.trim().is_empty() && !signal.display_line.contains(signal.text.trim()) {
        message.push_str(&format!("\n{}", signal.text.trim()));
    }
    message
}

pub(crate) fn split_operator_frontmatter(content: &str) -> (OperatorMarkdownFrontmatter, String) {
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (OperatorMarkdownFrontmatter::default(), content.to_string());
    }

    let mut yaml = Vec::new();
    let mut body = Vec::new();
    let mut in_yaml = true;
    for line in lines {
        if in_yaml && line.trim() == "---" {
            in_yaml = false;
            continue;
        }
        if in_yaml {
            yaml.push(line);
        } else {
            body.push(line);
        }
    }

    if in_yaml {
        return (OperatorMarkdownFrontmatter::default(), content.to_string());
    }

    let frontmatter =
        serde_yaml::from_str::<OperatorMarkdownFrontmatter>(&yaml.join("\n")).unwrap_or_default();
    (frontmatter, body.join("\n"))
}

fn parse_operator_timestamp(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Some(timestamp.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%d", "%Y_%m%d"] {
        if let Ok(date) = NaiveDate::parse_from_str(value, format)
            && let Some(time) = NaiveTime::from_hms_opt(0, 0, 0)
        {
            return Some(Utc.from_utc_datetime(&date.and_time(time)));
        }
    }
    None
}

fn parse_markdown_heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|ch| *ch == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let text = trimmed.get(level..)?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn parse_operator_checklist_task(line: &str) -> Option<(bool, String)> {
    let line = line.trim_start();
    let mut chars = line.chars();
    if !matches!(chars.next()?, '-' | '*' | '+') {
        return None;
    }
    let rest = chars.as_str().trim_start().strip_prefix('[')?;
    let mut chars = rest.chars();
    let state = chars.next()?;
    let rest = chars.as_str().strip_prefix(']')?;
    let task = rest.trim_start();
    if task.is_empty() {
        return None;
    }
    match state {
        ' ' => Some((false, task.to_string())),
        'x' | 'X' => Some((true, task.to_string())),
        _ => None,
    }
}

fn strip_operator_prefix<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = strip_operator_bullet(line);
    if trimmed.len() < prefix.len() {
        return None;
    }
    let candidate = trimmed.get(..prefix.len())?;
    candidate
        .eq_ignore_ascii_case(prefix)
        .then(|| trimmed.get(prefix.len()..).unwrap_or("").trim())
        .filter(|value| !value.is_empty())
}

fn strip_operator_bullet(line: &str) -> &str {
    line.trim().trim_start_matches(['-', '*', '+']).trim_start()
}

fn operator_severity_marker(line: &str) -> Option<&'static str> {
    let upper = line.to_ascii_uppercase();
    let has_marker = |marker: &str| {
        upper
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|token| token == marker)
    };
    ["P0", "P1", "P2"]
        .into_iter()
        .find(|marker| has_marker(marker))
}

fn strip_operator_severity_prefix<'a>(line: &'a str, severity: &str) -> &'a str {
    let stripped = strip_operator_bullet(line);
    let Some(rest) = stripped.get(severity.len()..) else {
        return stripped.trim();
    };
    if stripped
        .get(..severity.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(severity))
    {
        rest.trim_start_matches([' ', '-', ':', ']']).trim()
    } else {
        stripped.trim()
    }
}

fn infer_operator_project_hint(
    frontmatter: &OperatorMarkdownFrontmatter,
    body: &str,
    path: &Path,
    config: &ExtractionConfig,
    allow_body_project_inference: bool,
) -> Option<String> {
    if let Some(project) = frontmatter
        .project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return Some(project.trim().to_string());
    }
    if config.project_filter.len() == 1 {
        return config.project_filter.first().cloned();
    }
    if !allow_body_project_inference {
        return None;
    }

    let lower_path = path.to_string_lossy().to_ascii_lowercase();
    let lower_body = body.to_ascii_lowercase();
    for candidate in ["rust-memex", "aicx", "loctree", "vc-context-engine"] {
        if lower_path.contains(candidate) || lower_body.contains(candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

fn normalize_operator_frontmatter_cwd(home: &Path, cwd: &str) -> Option<String> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return None;
    }
    if let Some(rest) = cwd.strip_prefix("~/") {
        return Some(home.join(rest).display().to_string());
    }
    Some(cwd.to_string())
}

fn frontmatter_source_format(frontmatter: &OperatorMarkdownFrontmatter) -> Option<&str> {
    frontmatter
        .source_format
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn resolve_operator_cwd_hint(
    home: &Path,
    path: &Path,
    project_hint: Option<&str>,
) -> Option<String> {
    if path
        .components()
        .any(|component| component.as_os_str().to_string_lossy() == "docs")
        && path
            .components()
            .any(|component| component.as_os_str().to_string_lossy() == "operator")
        && let Some(root) = discover_git_root_from_path(path)
    {
        return Some(root.display().to_string());
    }

    let project = project_hint?.trim();
    if project.is_empty() {
        return None;
    }
    let (org, repo) = project.split_once('/').unwrap_or(("", project));

    let candidates = if !org.is_empty() {
        vec![
            home.join(org).join(repo),
            home.join("Git").join(repo),
            home.join("Git").join(org).join(repo),
            home.join("Libraxis").join(org).join(repo),
            home.join("Libraxis")
                .join("vc-runtime")
                .join(org)
                .join(repo),
            home.join("Libraxis")
                .join("01_deployed_libraxis_vm")
                .join(org)
                .join(repo),
            home.join("hosted").join(org).join(repo),
            home.join("vc-workspace").join(org).join(repo),
        ]
    } else {
        vec![
            home.join(repo),
            home.join("Git").join(repo),
            home.join("Libraxis").join(repo),
            home.join("Libraxis").join("vc-runtime").join(repo),
            home.join("Libraxis")
                .join("01_deployed_libraxis_vm")
                .join(repo),
            home.join("hosted").join("VetCoders").join(repo),
            home.join("vc-workspace").join("VetCoders").join(repo),
        ]
    };

    candidates
        .into_iter()
        .filter_map(resolve_case_insensitive_dir)
        .find(|candidate| candidate.is_dir())
        .map(|candidate| candidate.display().to_string())
}

fn resolve_case_insensitive_dir(candidate: PathBuf) -> Option<PathBuf> {
    if let (Some(parent), Some(name)) = (candidate.parent(), candidate.file_name()) {
        let needle = name.to_string_lossy().to_ascii_lowercase();
        for entry in fs::read_dir(parent).ok()?.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if path.is_dir() && entry.file_name().to_string_lossy().to_ascii_lowercase() == needle {
                return Some(path);
            }
        }
    }
    candidate.is_dir().then_some(candidate)
}

fn discover_git_root_from_path(path: &Path) -> Option<PathBuf> {
    let seed = if path.is_file() { path.parent()? } else { path };
    seed.ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
}

fn operator_path_fingerprint(path: &Path) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
