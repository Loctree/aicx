//! Output engine for ai-contexters
//!
//! Handles report generation in Markdown and JSON formats with support for:
//! - NewFile mode (timestamped files, current behavior)
//! - AppendTimeline mode (append to single file, deduplication by date)
//! - File rotation (keep last N files)
//! - Loctree snapshot embedding
//! - Decision markers and proper code block handling
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sanitize;

// ============================================================================
// Types
// ============================================================================

/// Configuration for the output engine.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub dir: PathBuf,
    pub format: OutputFormat,
    pub mode: OutputMode,
    /// Rotation: keep last N files (0 = unlimited)
    pub max_files: usize,
    /// Maximum message characters in markdown (0 = no truncation)
    pub max_message_chars: usize,
    /// Include loctree snapshot in output
    pub include_loctree: bool,
    /// Project root for loctree snapshot
    pub project_root: Option<PathBuf>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            format: OutputFormat::Both,
            mode: OutputMode::NewFile,
            max_files: 0,
            max_message_chars: 0,
            include_loctree: false,
            project_root: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OutputFormat {
    Markdown,
    Json,
    Both,
}

#[derive(Debug, Clone)]
pub enum OutputMode {
    /// Create new timestamped file each run (original behavior)
    NewFile,
    /// Append to a single timeline file, deduplicating by date
    AppendTimeline(PathBuf),
}

/// Re-export from sources — single definition, no twin.
pub use crate::sources::TimelineEntry;

/// Re-export the denoised conversation message type.
pub use crate::sources::ConversationMessage;

/// Metadata about the generated report.
#[derive(Debug, Clone, Serialize)]
pub struct ReportMetadata {
    pub generated_at: DateTime<Utc>,
    pub project_filter: Option<String>,
    pub hours_back: u64,
    pub total_entries: usize,
    pub sessions: Vec<String>,
}

// ============================================================================
// Decision markers
// ============================================================================

/// Keywords that signal an important decision or architectural note.
const DECISION_KEYWORDS: &[&str] = &[
    "decision:",
    "plan:",
    "architecture",
    "BREAKING",
    "TODO:",
    "FIXME:",
];

/// Case-sensitive keywords (checked without lowercasing).
const DECISION_KEYWORDS_CASE_SENSITIVE: &[&str] = &["WAŻNE", "KEY"];

fn is_decision_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    DECISION_KEYWORDS
        .iter()
        .any(|kw| lower.contains(&kw.to_lowercase()))
        || DECISION_KEYWORDS_CASE_SENSITIVE
            .iter()
            .any(|kw| message.contains(kw))
}

// ============================================================================
// Public API
// ============================================================================

/// Write a report according to the given configuration.
/// Returns paths of all files written.
pub fn write_report(
    config: &OutputConfig,
    entries: &[TimelineEntry],
    metadata: &ReportMetadata,
) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(&config.dir)
        .with_context(|| format!("Failed to create output dir: {}", config.dir.display()))?;

    let mut written_paths = Vec::new();

    match &config.mode {
        OutputMode::NewFile => {
            let date_str = metadata.generated_at.format("%Y%m%d_%H%M%S");
            let prefix = metadata.project_filter.as_deref().unwrap_or("all");

            if config.format == OutputFormat::Json || config.format == OutputFormat::Both {
                let json_path = config
                    .dir
                    .join(format!("{}_memory_{}.json", prefix, date_str));
                write_json_report(&json_path, entries, metadata)?;
                written_paths.push(json_path);
            }

            if config.format == OutputFormat::Markdown || config.format == OutputFormat::Both {
                let md_path = config
                    .dir
                    .join(format!("{}_memory_{}.md", prefix, date_str));
                let loctree = maybe_loctree_snapshot(config)?;
                write_markdown_full(
                    &md_path,
                    entries,
                    metadata,
                    config.max_message_chars,
                    loctree.as_deref(),
                )?;
                written_paths.push(md_path);
            }
        }
        OutputMode::AppendTimeline(timeline_path) => {
            let resolved = if timeline_path.is_relative() {
                config.dir.join(timeline_path)
            } else {
                timeline_path.clone()
            };

            if config.format == OutputFormat::Json || config.format == OutputFormat::Both {
                let json_path = resolved.with_extension("json");
                append_json_timeline(&json_path, entries, metadata)?;
                written_paths.push(json_path);
            }

            if config.format == OutputFormat::Markdown || config.format == OutputFormat::Both {
                let md_path = if resolved.extension().is_some_and(|e| e == "md") {
                    resolved.clone()
                } else {
                    resolved.with_extension("md")
                };
                let loctree = maybe_loctree_snapshot(config)?;
                append_markdown_timeline(
                    &md_path,
                    entries,
                    metadata,
                    config.max_message_chars,
                    loctree.as_deref(),
                )?;
                written_paths.push(md_path);
            }
        }
    }

    // Rotate if configured
    if config.max_files > 0 && matches!(&config.mode, OutputMode::NewFile) {
        let prefix = metadata.project_filter.as_deref().unwrap_or("all");
        let deleted = rotate_outputs(&config.dir, prefix, config.max_files)?;
        if deleted > 0 {
            eprintln!("  Rotated: removed {} old file(s)", deleted);
        }
    }

    Ok(written_paths)
}

/// Write a Markdown report to an explicit file path (overwrites).
///
/// This is a lightweight helper used by the CLI `extract` subcommand where
/// the user wants a single output file like `/tmp/report.md` instead of
/// the timestamped output directory layout.
pub fn write_markdown_report_to_path(
    path: &Path,
    entries: &[TimelineEntry],
    metadata: &ReportMetadata,
    max_chars: usize,
    loctree_snapshot: Option<&str>,
) -> Result<PathBuf> {
    let validated = sanitize::validate_write_path(path)?;
    if let Some(parent) = validated.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent dir: {}", parent.display()))?;
    }

    write_markdown_full(&validated, entries, metadata, max_chars, loctree_snapshot)?;
    Ok(validated)
}

/// Write a JSON report to an explicit file path (overwrites).
pub fn write_json_report_to_path(
    path: &Path,
    entries: &[TimelineEntry],
    metadata: &ReportMetadata,
) -> Result<PathBuf> {
    let validated = sanitize::validate_write_path(path)?;
    if let Some(parent) = validated.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent dir: {}", parent.display()))?;
    }

    write_json_report(&validated, entries, metadata)?;
    Ok(validated)
}

/// Delete oldest files matching `{prefix}_memory_*.{json,md}`, keeping only `max_files`.
/// Returns number of files deleted.
pub fn rotate_outputs(dir: &Path, prefix: &str, max_files: usize) -> Result<usize> {
    if max_files == 0 {
        return Ok(0);
    }

    let pattern_prefix = format!("{}_memory_", prefix);
    let mut matching: Vec<PathBuf> = Vec::new();

    let entries = fs::read_dir(dir)
        .with_context(|| format!("Failed to read dir for rotation: {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if name_str.starts_with(&pattern_prefix)
            && (name_str.ends_with(".json") || name_str.ends_with(".md"))
        {
            matching.push(entry.path());
        }
    }

    // Sort by filename (which includes timestamp, so lexicographic = chronological)
    matching.sort();

    let mut deleted = 0;
    if matching.len() > max_files {
        let to_remove = matching.len() - max_files;
        for path in matching.iter().take(to_remove) {
            fs::remove_file(path)
                .with_context(|| format!("Failed to remove: {}", path.display()))?;
            deleted += 1;
        }
    }

    Ok(deleted)
}

/// Capture a loctree snapshot for the given project directory.
/// Returns Ok(None) if loctree is not installed or the command fails.
pub fn capture_loctree_snapshot(project: &Path) -> Result<Option<String>> {
    let output = Command::new("loct")
        .args(["--for-ai", "--json"])
        .current_dir(project)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            if stdout.trim().is_empty() {
                Ok(None)
            } else {
                Ok(Some(stdout))
            }
        }
        Ok(_) => Ok(None),  // Command ran but failed (non-zero exit)
        Err(_) => Ok(None), // Command not found or couldn't execute
    }
}

// ============================================================================
// Internal: JSON output
// ============================================================================

fn write_json_report(
    path: &Path,
    entries: &[TimelineEntry],
    metadata: &ReportMetadata,
) -> Result<()> {
    #[derive(Serialize)]
    struct JsonReport<'a> {
        generated_at: DateTime<Utc>,
        project_filter: &'a Option<String>,
        hours_back: u64,
        total_entries: usize,
        sessions: &'a [String],
        entries: &'a [TimelineEntry],
    }

    let report = JsonReport {
        generated_at: metadata.generated_at,
        project_filter: &metadata.project_filter,
        hours_back: metadata.hours_back,
        total_entries: metadata.total_entries,
        sessions: &metadata.sessions,
        entries,
    };

    let validated = sanitize::validate_write_path(path)?;
    // SECURITY: path sanitized via validate_write_path (traversal + allowlist)
    let file = File::create(&validated) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .with_context(|| format!("Failed to create: {}", path.display()))?;
    serde_json::to_writer_pretty(file, &report)?;
    eprintln!("  -> {}", path.display());
    Ok(())
}

fn append_json_timeline(
    path: &Path,
    entries: &[TimelineEntry],
    metadata: &ReportMetadata,
) -> Result<()> {
    // For JSON append, we write newline-delimited JSON (one entry per line)
    // This makes it appendable without parsing the whole file
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("Failed to open for append: {}", path.display()))?;

    // Write a sync marker as a special entry
    let sync_marker = serde_json::json!({
        "__sync": metadata.generated_at.to_rfc3339(),
        "total_entries": metadata.total_entries,
        "project_filter": metadata.project_filter,
    });
    writeln!(file, "{}", serde_json::to_string(&sync_marker)?)?;

    for entry in entries {
        writeln!(file, "{}", serde_json::to_string(entry)?)?;
    }

    eprintln!(
        "  -> {} (appended {} entries)",
        path.display(),
        entries.len()
    );
    Ok(())
}

// ============================================================================
// Internal: Markdown output
// ============================================================================

fn maybe_loctree_snapshot(config: &OutputConfig) -> Result<Option<String>> {
    if !config.include_loctree {
        return Ok(None);
    }
    match &config.project_root {
        Some(root) => capture_loctree_snapshot(root),
        None => Ok(None),
    }
}

fn write_markdown_full(
    path: &Path,
    entries: &[TimelineEntry],
    metadata: &ReportMetadata,
    max_chars: usize,
    loctree_snapshot: Option<&str>,
) -> Result<()> {
    let validated = sanitize::validate_write_path(path)?;
    // SECURITY: path sanitized via validate_write_path (traversal + allowlist)
    let mut file = File::create(&validated) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .with_context(|| format!("Failed to create: {}", path.display()))?;

    write_markdown_header(&mut file, metadata)?;

    // Write initial sync marker so append mode can track from when this file was created
    writeln!(
        file,
        "<!-- sync: {} -->",
        metadata.generated_at.to_rfc3339()
    )?;
    writeln!(file)?;

    if let Some(snapshot) = loctree_snapshot {
        write_loctree_section(&mut file, snapshot)?;
    }

    write_markdown_entries(&mut file, entries, max_chars)?;
    write_markdown_footer(&mut file)?;

    eprintln!("  -> {}", path.display());
    Ok(())
}

fn append_markdown_timeline(
    path: &Path,
    entries: &[TimelineEntry],
    metadata: &ReportMetadata,
    max_chars: usize,
    loctree_snapshot: Option<&str>,
) -> Result<()> {
    if !path.exists() {
        // First time: write full file (includes initial sync marker)
        return write_markdown_full(path, entries, metadata, max_chars, loctree_snapshot);
    }

    // Find the last sync marker to determine what's new
    let last_sync = find_last_sync_timestamp(path)?;

    // Filter entries to only include those after the last sync
    let new_entries: Vec<&TimelineEntry> = match last_sync {
        Some(ts) => entries.iter().filter(|e| e.timestamp > ts).collect(),
        None => entries.iter().collect(),
    };

    if new_entries.is_empty() {
        eprintln!("  -> {} (no new entries to append)", path.display());
        return Ok(());
    }

    // Remove the footer from existing file before appending
    strip_footer(path)?;

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("Failed to open for append: {}", path.display()))?;

    // Write sync separator
    writeln!(file)?;
    writeln!(
        file,
        "<!-- sync: {} -->",
        metadata.generated_at.to_rfc3339()
    )?;
    writeln!(file)?;

    if let Some(snapshot) = loctree_snapshot {
        write_loctree_section(&mut file, snapshot)?;
    }

    // Write only new entries
    let owned_entries: Vec<TimelineEntry> = new_entries.into_iter().cloned().collect();
    write_markdown_entries(&mut file, &owned_entries, max_chars)?;
    write_markdown_footer(&mut file)?;

    eprintln!(
        "  -> {} (appended {} entries)",
        path.display(),
        owned_entries.len()
    );
    Ok(())
}

fn find_last_sync_timestamp(path: &Path) -> Result<Option<DateTime<Utc>>> {
    let validated = sanitize::validate_read_path(path)?;
    // SECURITY: path sanitized via validate_read_path (traversal + canonicalize + allowlist)
    let file = File::open(&validated)?; // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let reader = BufReader::new(file);

    let mut last_sync: Option<DateTime<Utc>> = None;

    for line in reader.lines() {
        let line = line?;
        if let Some(ts) = line
            .strip_prefix("<!-- sync: ")
            .and_then(|s| s.strip_suffix(" -->"))
            .and_then(|ts_str| DateTime::parse_from_rfc3339(ts_str).ok())
        {
            last_sync = Some(ts.with_timezone(&Utc));
        }
    }

    Ok(last_sync)
}

fn strip_footer(path: &Path) -> Result<()> {
    let content = fs::read_to_string(path)?;
    let footer_marker = "---\n*Generated by ai-contexters";

    if let Some(pos) = content.rfind(footer_marker) {
        let trimmed = &content[..pos];
        fs::write(path, trimmed)?;
    }

    Ok(())
}

fn write_markdown_header(w: &mut impl Write, metadata: &ReportMetadata) -> Result<()> {
    writeln!(w, "# Agent Memory Timeline\n")?;
    writeln!(w, "| Field | Value |")?;
    writeln!(w, "|-------|-------|")?;
    writeln!(
        w,
        "| Generated | {} |",
        metadata.generated_at.format("%Y-%m-%d %H:%M:%S UTC")
    )?;
    writeln!(
        w,
        "| Filter | {} |",
        metadata.project_filter.as_deref().unwrap_or("(all)")
    )?;
    writeln!(w, "| Period | last {} hours |", metadata.hours_back)?;
    writeln!(w, "| Entries | {} |", metadata.total_entries)?;
    writeln!(w, "| Sessions | {} |", metadata.sessions.len())?;
    writeln!(w)?;
    writeln!(w, "---\n")?;
    Ok(())
}

fn write_loctree_section(w: &mut impl Write, snapshot: &str) -> Result<()> {
    writeln!(w, "<details>")?;
    writeln!(w, "<summary>Loctree Snapshot</summary>\n")?;
    writeln!(w, "```json")?;
    write!(w, "{}", snapshot)?;
    if !snapshot.ends_with('\n') {
        writeln!(w)?;
    }
    writeln!(w, "```\n")?;
    writeln!(w, "</details>\n")?;
    Ok(())
}

fn write_markdown_entries(
    w: &mut impl Write,
    entries: &[TimelineEntry],
    max_chars: usize,
) -> Result<()> {
    // Group by date
    let mut by_date: HashMap<String, Vec<&TimelineEntry>> = HashMap::new();
    for entry in entries {
        let date = entry.timestamp.format("%Y-%m-%d").to_string();
        by_date.entry(date).or_default().push(entry);
    }

    let mut dates: Vec<_> = by_date.keys().cloned().collect();
    dates.sort();

    for date in &dates {
        writeln!(w, "## {}\n", date)?;

        let day_entries = by_date.get(date).unwrap();
        for entry in day_entries {
            write_single_entry(w, entry, max_chars)?;
        }
    }

    Ok(())
}

fn write_single_entry(w: &mut impl Write, entry: &TimelineEntry, max_chars: usize) -> Result<()> {
    let time = entry.timestamp.format("%H:%M:%S");
    let role_icon = if entry.role == "user" {
        "\u{1f464}"
    } else {
        "\u{1f916}"
    };
    let agent_badge = match entry.agent.as_str() {
        "claude" => "[Claude]",
        "codex" => "[Codex]",
        other => other,
    };

    let session_short = &entry.session_id[..8.min(entry.session_id.len())];

    // Decision marker
    let decision_pin = if is_decision_message(&entry.message) {
        "\u{1f4cc} "
    } else {
        ""
    };

    writeln!(
        w,
        "### {}{} {} {} `{}`\n",
        decision_pin, time, role_icon, agent_badge, session_short
    )?;

    if let Some(ref branch) = entry.branch {
        writeln!(w, "Branch: `{}`\n", branch)?;
    }

    if let Some(ref cwd) = entry.cwd {
        writeln!(w, "CWD: `{}`\n", cwd)?;
    }

    // Format message
    let msg = apply_truncation(&entry.message, max_chars);
    write_formatted_message(w, &msg)?;

    writeln!(w)?;
    Ok(())
}

fn apply_truncation(message: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return message.to_string();
    }

    let char_count = message.chars().count();
    if char_count <= max_chars {
        message.to_string()
    } else {
        let truncated: String = message.chars().take(max_chars).collect();
        format!(
            "{}...\n\n*[truncated at {} chars, total {}]*",
            truncated, max_chars, char_count
        )
    }
}

fn write_formatted_message(w: &mut impl Write, message: &str) -> Result<()> {
    let has_code_blocks = message.contains("```");
    let is_multiline = message.contains('\n');

    if !is_multiline {
        // Single line: simple blockquote
        writeln!(w, "> {}", message)?;
    } else if has_code_blocks {
        // Message with code blocks: use HTML blockquote to preserve code fences
        write_blockquote_with_code(w, message)?;
    } else {
        // Multi-line without code blocks: blockquote each line properly
        for line in message.lines() {
            if line.is_empty() {
                writeln!(w, ">")?;
            } else {
                writeln!(w, "> {}", line)?;
            }
        }
        writeln!(w)?;
    }

    Ok(())
}

fn write_blockquote_with_code(w: &mut impl Write, message: &str) -> Result<()> {
    // Use HTML <blockquote> when code fences are present
    // This avoids breaking code blocks with `>` prefixes
    writeln!(w, "<blockquote>")?;
    writeln!(w)?;

    let mut in_code_block = false;
    for line in message.lines() {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
        }
        writeln!(w, "{}", line)?;
    }

    writeln!(w)?;
    writeln!(w, "</blockquote>")?;
    writeln!(w)?;
    Ok(())
}

fn write_markdown_footer(w: &mut impl Write) -> Result<()> {
    writeln!(w, "---\n*Generated by ai-contexters (c)2026 VetCoders*")?;
    Ok(())
}

// ============================================================================
// Conversation-first output
// ============================================================================

/// Write a denoised conversation transcript as Markdown.
///
/// Groups messages by repo/project, then by session, then chronologically.
/// Only user/assistant messages appear — no tool calls, reasoning, or system noise.
pub fn write_conversation_markdown(
    path: &Path,
    messages: &[ConversationMessage],
    metadata: &ReportMetadata,
) -> Result<PathBuf> {
    let validated = sanitize::validate_write_path(path)?;
    if let Some(parent) = validated.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent dir: {}", parent.display()))?;
    }

    let mut file = File::create(&validated) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .with_context(|| format!("Failed to create: {}", path.display()))?;

    // Header
    writeln!(file, "# Conversation Transcript\n")?;
    writeln!(file, "| Field | Value |")?;
    writeln!(file, "|-------|-------|")?;
    writeln!(
        file,
        "| Generated | {} |",
        metadata.generated_at.format("%Y-%m-%d %H:%M:%S UTC")
    )?;
    writeln!(
        file,
        "| Filter | {} |",
        metadata.project_filter.as_deref().unwrap_or("(all)")
    )?;
    writeln!(file, "| Period | last {} hours |", metadata.hours_back)?;
    writeln!(file, "| Messages | {} |", messages.len())?;
    writeln!(file, "| Sessions | {} |", metadata.sessions.len())?;
    writeln!(file)?;
    writeln!(file, "---\n")?;

    // Group by repo_project -> session_id, preserving chronological order
    let mut by_project: std::collections::BTreeMap<&str, Vec<&ConversationMessage>> =
        std::collections::BTreeMap::new();
    for msg in messages {
        by_project.entry(&msg.repo_project).or_default().push(msg);
    }

    for (project, project_msgs) in &by_project {
        writeln!(file, "## Project: {}\n", project)?;

        // Sub-group by session (preserving insertion order)
        let mut session_order: Vec<&str> = Vec::new();
        let mut by_session: HashMap<&str, Vec<&&ConversationMessage>> = HashMap::new();
        for msg in project_msgs {
            let sid = msg.session_id.as_str();
            if !by_session.contains_key(sid) {
                session_order.push(sid);
            }
            by_session.entry(sid).or_default().push(msg);
        }

        for session_id in &session_order {
            let session_msgs = &by_session[session_id];
            let session_short = &session_id[..8.min(session_id.len())];
            let agent = session_msgs
                .first()
                .map(|m| m.agent.as_str())
                .unwrap_or("unknown");
            writeln!(file, "### Session `{}` [{}]\n", session_short, agent)?;

            if let Some(sp) = session_msgs.first().and_then(|m| m.source_path.as_deref()) {
                writeln!(file, "CWD: `{}`\n", sp)?;
            }

            for msg in session_msgs {
                let time = msg.timestamp.format("%H:%M:%S");
                let role_label = if msg.role == "user" {
                    "user"
                } else {
                    "assistant"
                };
                writeln!(file, "**[{}] {}:**\n", time, role_label)?;
                write_formatted_message(&mut file, &msg.message)?;
                writeln!(file)?;
            }
        }
    }

    write_markdown_footer(&mut file)?;
    eprintln!("  -> {}", path.display());
    Ok(validated)
}

/// Write a denoised conversation transcript as JSON.
///
/// Produces a structured JSON document with repo-centric grouping.
pub fn write_conversation_json(
    path: &Path,
    messages: &[ConversationMessage],
    metadata: &ReportMetadata,
) -> Result<PathBuf> {
    let validated = sanitize::validate_write_path(path)?;
    if let Some(parent) = validated.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent dir: {}", parent.display()))?;
    }

    #[derive(Serialize)]
    struct ConversationReport<'a> {
        generated_at: DateTime<Utc>,
        project_filter: &'a Option<String>,
        hours_back: u64,
        total_messages: usize,
        sessions: &'a [String],
        messages: &'a [ConversationMessage],
    }

    let report = ConversationReport {
        generated_at: metadata.generated_at,
        project_filter: &metadata.project_filter,
        hours_back: metadata.hours_back,
        total_messages: messages.len(),
        sessions: &metadata.sessions,
        messages,
    };

    let file = File::create(&validated) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .with_context(|| format!("Failed to create: {}", path.display()))?;
    serde_json::to_writer_pretty(file, &report)?;
    eprintln!("  -> {}", path.display());
    Ok(validated)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Global counter to ensure each test gets a unique directory
    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn unique_test_dir(name: &str) -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("ai_ctx_test_{}_{}_{}", std::process::id(), n, name));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    fn sample_entries() -> Vec<TimelineEntry> {
        vec![
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 10, 30, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "abc12345-6789".to_string(),
                role: "user".to_string(),
                message: "Fix the build pipeline".to_string(),
                branch: Some("feat/pipeline".to_string()),
                cwd: Some("/home/project".to_string()),
                frame_kind: None,
            },
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 10, 31, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "abc12345-6789".to_string(),
                role: "assistant".to_string(),
                message: "decision: We should use incremental builds".to_string(),
                branch: Some("feat/pipeline".to_string()),
                cwd: None,
                frame_kind: None,
            },
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 9, 0, 0).unwrap(),
                agent: "codex".to_string(),
                session_id: "def98765-4321".to_string(),
                role: "user".to_string(),
                message: "Show me the code structure".to_string(),
                branch: None,
                cwd: None,
                frame_kind: None,
            },
        ]
    }

    fn sample_metadata() -> ReportMetadata {
        ReportMetadata {
            generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 14, 0, 0).unwrap(),
            project_filter: Some("testproject".to_string()),
            hours_back: 48,
            total_entries: 3,
            sessions: vec!["abc12345-6789".to_string(), "def98765-4321".to_string()],
        }
    }

    // --- Rotation tests ---

    #[test]
    fn test_rotation_no_files() {
        let dir = unique_test_dir("rot_none");
        let deleted = rotate_outputs(&dir, "test", 5).unwrap();
        assert_eq!(deleted, 0);
        cleanup(&dir);
    }

    #[test]
    fn test_rotation_under_limit() {
        let dir = unique_test_dir("rot_under");
        for i in 0..3 {
            fs::write(
                dir.join(format!("test_memory_2026010{}_120000.md", i)),
                "content",
            )
            .unwrap();
        }
        let deleted = rotate_outputs(&dir, "test", 5).unwrap();
        assert_eq!(deleted, 0);
        cleanup(&dir);
    }

    #[test]
    fn test_rotation_over_limit() {
        let dir = unique_test_dir("rot_over");
        for i in 0..5 {
            fs::write(
                dir.join(format!("test_memory_2026010{}_120000.md", i)),
                "content",
            )
            .unwrap();
        }
        let deleted = rotate_outputs(&dir, "test", 2).unwrap();
        assert_eq!(deleted, 3);

        // Verify only the 2 newest remain
        let remaining: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&"test_memory_20260103_120000.md".to_string()));
        assert!(remaining.contains(&"test_memory_20260104_120000.md".to_string()));

        cleanup(&dir);
    }

    #[test]
    fn test_rotation_mixed_extensions() {
        let dir = unique_test_dir("rot_mixed");
        for i in 0..4 {
            fs::write(
                dir.join(format!("proj_memory_2026010{}_120000.md", i)),
                "md",
            )
            .unwrap();
            fs::write(
                dir.join(format!("proj_memory_2026010{}_120000.json", i)),
                "json",
            )
            .unwrap();
        }
        // 8 files total, keep 4
        let deleted = rotate_outputs(&dir, "proj", 4).unwrap();
        assert_eq!(deleted, 4);
        cleanup(&dir);
    }

    #[test]
    fn test_rotation_ignores_other_files() {
        let dir = unique_test_dir("rot_ignore");
        // Non-matching files
        fs::write(dir.join("other_file.md"), "keep").unwrap();
        fs::write(dir.join("README.md"), "keep").unwrap();
        // Matching files
        for i in 0..3 {
            fs::write(
                dir.join(format!("test_memory_2026010{}_120000.md", i)),
                "rotate",
            )
            .unwrap();
        }
        let deleted = rotate_outputs(&dir, "test", 1).unwrap();
        assert_eq!(deleted, 2);

        // Non-matching files still exist
        assert!(dir.join("other_file.md").exists());
        assert!(dir.join("README.md").exists());
        cleanup(&dir);
    }

    #[test]
    fn test_rotation_zero_means_unlimited() {
        let dir = unique_test_dir("rot_zero");
        for i in 0..10 {
            fs::write(
                dir.join(format!("x_memory_2026010{}_120000.md", i)),
                "content",
            )
            .unwrap();
        }
        let deleted = rotate_outputs(&dir, "x", 0).unwrap();
        assert_eq!(deleted, 0);
        cleanup(&dir);
    }

    // --- NewFile mode tests ---

    #[test]
    fn test_new_file_mode_creates_files() {
        let dir = unique_test_dir("newfile");
        let config = OutputConfig {
            dir: dir.clone(),
            format: OutputFormat::Both,
            mode: OutputMode::NewFile,
            ..Default::default()
        };

        let entries = sample_entries();
        let metadata = sample_metadata();

        let paths = write_report(&config, &entries, &metadata).unwrap();
        assert_eq!(paths.len(), 2); // json + md

        for p in &paths {
            assert!(p.exists(), "File should exist: {}", p.display());
        }

        // Check markdown content
        let md_path = paths
            .iter()
            .find(|p| p.extension().unwrap() == "md")
            .unwrap();
        let content = fs::read_to_string(md_path).unwrap();
        assert!(content.contains("# Agent Memory Timeline"));
        assert!(content.contains("## 2026-01-22"));
        assert!(content.contains("## 2026-01-23"));
        assert!(content.contains("[Claude]"));
        assert!(content.contains("[Codex]"));

        cleanup(&dir);
    }

    #[test]
    fn test_decision_markers() {
        let dir = unique_test_dir("decision");
        let config = OutputConfig {
            dir: dir.clone(),
            format: OutputFormat::Markdown,
            mode: OutputMode::NewFile,
            ..Default::default()
        };

        let entries = sample_entries();
        let metadata = sample_metadata();

        let paths = write_report(&config, &entries, &metadata).unwrap();
        let content = fs::read_to_string(&paths[0]).unwrap();

        // Entry with "decision:" should have pin marker (U+1F4CC)
        assert!(content.contains("\u{1f4cc}"));

        cleanup(&dir);
    }

    #[test]
    fn test_no_truncation_by_default() {
        let dir = unique_test_dir("notrunc");
        let config = OutputConfig {
            dir: dir.clone(),
            format: OutputFormat::Markdown,
            mode: OutputMode::NewFile,
            max_message_chars: 0,
            ..Default::default()
        };

        let long_message = "x".repeat(2000);
        let entries = vec![TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "longsess1".to_string(),
            role: "user".to_string(),
            message: long_message.clone(),
            branch: None,
            cwd: None,
            frame_kind: None,
        }];
        let metadata = ReportMetadata {
            generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
            project_filter: Some("test".to_string()),
            hours_back: 24,
            total_entries: 1,
            sessions: vec!["longsess1".to_string()],
        };

        let paths = write_report(&config, &entries, &metadata).unwrap();
        let content = fs::read_to_string(&paths[0]).unwrap();

        assert!(content.contains(&long_message));
        assert!(!content.contains("[truncated"));

        cleanup(&dir);
    }

    #[test]
    fn test_truncation_when_configured() {
        let dir = unique_test_dir("trunc");
        let config = OutputConfig {
            dir: dir.clone(),
            format: OutputFormat::Markdown,
            mode: OutputMode::NewFile,
            max_message_chars: 50,
            ..Default::default()
        };

        let entries = vec![TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "truncsess".to_string(),
            role: "user".to_string(),
            message: "a".repeat(200),
            branch: None,
            cwd: None,
            frame_kind: None,
        }];
        let metadata = ReportMetadata {
            generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
            project_filter: Some("test".to_string()),
            hours_back: 24,
            total_entries: 1,
            sessions: vec!["truncsess".to_string()],
        };

        let paths = write_report(&config, &entries, &metadata).unwrap();
        let content = fs::read_to_string(&paths[0]).unwrap();

        assert!(content.contains("[truncated at 50 chars, total 200]"));

        cleanup(&dir);
    }

    // --- AppendTimeline mode tests ---

    #[test]
    fn test_append_timeline_creates_new_file() {
        let dir = unique_test_dir("append_new");
        let timeline_path = dir.join("TIMELINE.md");

        let config = OutputConfig {
            dir: dir.clone(),
            format: OutputFormat::Markdown,
            mode: OutputMode::AppendTimeline(timeline_path.clone()),
            ..Default::default()
        };

        let entries = sample_entries();
        let metadata = sample_metadata();

        let paths = write_report(&config, &entries, &metadata).unwrap();
        assert!(timeline_path.exists());
        assert_eq!(paths.len(), 1);

        let content = fs::read_to_string(&timeline_path).unwrap();
        assert!(content.contains("# Agent Memory Timeline"));
        assert!(content.contains("## 2026-01-22"));
        // Initial sync marker should be present
        assert!(content.contains("<!-- sync: 2026-01-23T14:00:00+00:00 -->"));

        cleanup(&dir);
    }

    #[test]
    fn test_append_timeline_deduplicates() {
        let dir = unique_test_dir("append_dedup");
        let timeline_path = dir.join("TIMELINE.md");

        let config = OutputConfig {
            dir: dir.clone(),
            format: OutputFormat::Markdown,
            mode: OutputMode::AppendTimeline(timeline_path.clone()),
            ..Default::default()
        };

        let entries = sample_entries();
        let metadata = sample_metadata();

        // First write (generated_at = 14:00, all entry timestamps < 14:00 except one at 09:00 on Jan 23)
        write_report(&config, &entries, &metadata).unwrap();

        // Second write with same entries but later generated_at
        // Since entries are at 10:30, 10:31, and 09:00 -- all before sync at 14:00
        // nothing new should be appended
        let metadata2 = ReportMetadata {
            generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 15, 0, 0).unwrap(),
            ..sample_metadata()
        };
        write_report(&config, &entries, &metadata2).unwrap();

        let content = fs::read_to_string(&timeline_path).unwrap();
        // The initial sync marker from first write
        assert!(content.contains("<!-- sync: 2026-01-23T14:00:00+00:00 -->"));
        // Date headers should appear only once each (no duplicates)
        assert_eq!(content.matches("## 2026-01-22").count(), 1);

        cleanup(&dir);
    }

    #[test]
    fn test_append_timeline_adds_new_entries() {
        let dir = unique_test_dir("append_add");
        let timeline_path = dir.join("TIMELINE.md");

        let config = OutputConfig {
            dir: dir.clone(),
            format: OutputFormat::Markdown,
            mode: OutputMode::AppendTimeline(timeline_path.clone()),
            ..Default::default()
        };

        // First write: entry at 10:00, generated_at at 12:00
        let entries1 = vec![TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 10, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "sess-aaa1".to_string(),
            role: "user".to_string(),
            message: "First entry".to_string(),
            branch: None,
            cwd: None,
            frame_kind: None,
        }];
        let metadata1 = ReportMetadata {
            generated_at: Utc.with_ymd_and_hms(2026, 1, 22, 12, 0, 0).unwrap(),
            project_filter: Some("test".to_string()),
            hours_back: 24,
            total_entries: 1,
            sessions: vec!["sess-aaa1".to_string()],
        };
        write_report(&config, &entries1, &metadata1).unwrap();

        // Second write: includes old entry (before sync) + new entry (after sync)
        let entries2 = vec![
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 10, 0, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "sess-aaa1".to_string(),
                role: "user".to_string(),
                message: "First entry".to_string(), // duplicate
                branch: None,
                cwd: None,
                frame_kind: None,
            },
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 16, 0, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "sess-bbb2".to_string(),
                role: "user".to_string(),
                message: "New entry after sync".to_string(),
                branch: None,
                cwd: None,
                frame_kind: None,
            },
        ];
        let metadata2 = ReportMetadata {
            generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 17, 0, 0).unwrap(),
            project_filter: Some("test".to_string()),
            hours_back: 48,
            total_entries: 2,
            sessions: vec!["sess-aaa1".to_string(), "sess-bbb2".to_string()],
        };
        write_report(&config, &entries2, &metadata2).unwrap();

        let content = fs::read_to_string(&timeline_path).unwrap();
        // First entry should appear exactly once (not duplicated)
        assert_eq!(content.matches("First entry").count(), 1);
        // New entry should be present
        assert!(content.contains("New entry after sync"));
        // Second sync marker
        assert!(content.contains("<!-- sync: 2026-01-23T17:00:00+00:00 -->"));

        cleanup(&dir);
    }

    // --- Code block preservation ---

    #[test]
    fn test_code_blocks_preserved() {
        let dir = unique_test_dir("codeblocks");
        let config = OutputConfig {
            dir: dir.clone(),
            format: OutputFormat::Markdown,
            mode: OutputMode::NewFile,
            ..Default::default()
        };

        let msg = "Here's the fix:\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\nDone.";
        let entries = vec![TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "codetst1".to_string(),
            role: "assistant".to_string(),
            message: msg.to_string(),
            branch: None,
            cwd: None,
            frame_kind: None,
        }];
        let metadata = ReportMetadata {
            generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
            project_filter: Some("test".to_string()),
            hours_back: 24,
            total_entries: 1,
            sessions: vec!["codetst1".to_string()],
        };

        let paths = write_report(&config, &entries, &metadata).unwrap();
        let content = fs::read_to_string(&paths[0]).unwrap();

        // Code block should be intact (not prefixed with >)
        assert!(content.contains("```rust"));
        assert!(content.contains("fn main()"));
        assert!(content.contains("println!"));
        // Should use HTML blockquote for code-containing messages
        assert!(content.contains("<blockquote>"));
        assert!(content.contains("</blockquote>"));

        cleanup(&dir);
    }

    // --- Decision keyword detection ---

    #[test]
    fn test_is_decision_message_positive() {
        assert!(is_decision_message("decision: use incremental builds"));
        assert!(is_decision_message("The plan: refactor everything"));
        assert!(is_decision_message("New architecture proposal"));
        assert!(is_decision_message("WAŻNE: to jest krytyczne"));
        assert!(is_decision_message("KEY insight here"));
        assert!(is_decision_message("TODO: fix this later"));
        assert!(is_decision_message("FIXME: broken"));
        assert!(is_decision_message("BREAKING change in API"));
    }

    #[test]
    fn test_is_decision_message_negative() {
        assert!(!is_decision_message("Just a regular message"));
        assert!(!is_decision_message("nothing special here"));
        assert!(!is_decision_message("the key to success")); // lowercase "key" should not match
    }

    // --- JSON output ---

    #[test]
    fn test_json_output() {
        let dir = unique_test_dir("json");
        let config = OutputConfig {
            dir: dir.clone(),
            format: OutputFormat::Json,
            mode: OutputMode::NewFile,
            ..Default::default()
        };

        let entries = sample_entries();
        let metadata = sample_metadata();

        let paths = write_report(&config, &entries, &metadata).unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].extension().unwrap(), "json");

        let content = fs::read_to_string(&paths[0]).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["total_entries"], 3);
        assert_eq!(parsed["entries"].as_array().unwrap().len(), 3);

        cleanup(&dir);
    }

    // --- Loctree ---

    #[test]
    fn test_loctree_snapshot_missing_binary() {
        let dir = unique_test_dir("loctree_missing_bin");
        std::fs::create_dir_all(&dir).unwrap();
        // loct may or may not be on PATH -- either way this should not panic
        let result = capture_loctree_snapshot(&dir).unwrap();
        let _ = result; // Just verify it doesn't error
        cleanup(&dir);
    }

    // --- Config defaults ---

    #[test]
    fn test_output_config_default() {
        let config = OutputConfig::default();
        assert_eq!(config.max_files, 0);
        assert_eq!(config.max_message_chars, 0);
        assert!(!config.include_loctree);
        assert_eq!(config.format, OutputFormat::Both);
    }

    // --- Multiline message formatting ---

    #[test]
    fn test_multiline_without_code_uses_blockquote_lines() {
        let dir = unique_test_dir("multiline");
        let config = OutputConfig {
            dir: dir.clone(),
            format: OutputFormat::Markdown,
            mode: OutputMode::NewFile,
            ..Default::default()
        };

        let entries = vec![TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "multisss".to_string(),
            role: "user".to_string(),
            message: "Line one\nLine two\nLine three".to_string(),
            branch: None,
            cwd: None,
            frame_kind: None,
        }];
        let metadata = ReportMetadata {
            generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
            project_filter: Some("test".to_string()),
            hours_back: 24,
            total_entries: 1,
            sessions: vec!["multisss".to_string()],
        };

        let paths = write_report(&config, &entries, &metadata).unwrap();
        let content = fs::read_to_string(&paths[0]).unwrap();

        assert!(content.contains("> Line one\n> Line two\n> Line three"));
        // Should NOT use HTML blockquote (no code blocks)
        assert!(!content.contains("<blockquote>"));

        cleanup(&dir);
    }
}
