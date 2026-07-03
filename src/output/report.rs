use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sanitize;

use super::decision::is_decision_message;
use super::{OutputConfig, OutputFormat, OutputMode, ReportMetadata, TimelineEntry};

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

    let entries = sanitize::read_dir_validated(dir)
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

    let file = sanitize::create_file_validated(path)
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
    let mut file = sanitize::create_file_validated(path)
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
    // SECURITY: --append-to is a user-controlled CLI path. Validate before
    // any read/write to prevent path traversal. Downstream strip_footer and
    // truncate_file_atomic now reopen via sanitizer helpers instead of trusting
    // this caller-side validation alone.
    let path = &sanitize::validate_write_path(path)?;
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

pub(crate) fn find_last_sync_timestamp(path: &Path) -> Result<Option<DateTime<Utc>>> {
    let file = sanitize::open_file_validated(path)?;
    let mut reader = BufReader::new(file);

    let mut last_sync: Option<DateTime<Utc>> = None;

    while let Some(line) = sanitize::read_line_capped(&mut reader, sanitize::MAX_VALIDATED_BYTES)? {
        if line.exceeded {
            continue;
        }
        let line = line.line.trim_end_matches(['\r', '\n']);
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

/// Size of the tail window scanned on the first pass for the footer marker.
/// 64 KiB easily covers the footer ai-contexters writes today (a few hundred
/// bytes) while keeping memory flat regardless of timeline length.
const STRIP_FOOTER_TAIL_WINDOW: u64 = 64 * 1024;

/// Larger fallback window. If the marker is not in the last 64 KiB we widen
/// the scan once before giving up. Still bounded — never read the whole file.
const STRIP_FOOTER_TAIL_WINDOW_LARGE: u64 = 1024 * 1024;

pub(crate) const STRIP_FOOTER_MARKER: &[u8] = b"---\n*Generated by ai-contexters";

pub(crate) fn strip_footer(path: &Path) -> Result<()> {
    let mut file = sanitize::open_file_validated(path)
        .with_context(|| format!("strip_footer: open failed: {}", path.display()))?;
    let file_size = file
        .metadata()
        .with_context(|| format!("strip_footer: stat failed: {}", path.display()))?
        .len();

    let pos = match find_footer_position(&mut file, file_size, STRIP_FOOTER_TAIL_WINDOW)? {
        Some(p) => Some(p),
        None => find_footer_position(&mut file, file_size, STRIP_FOOTER_TAIL_WINDOW_LARGE)?,
    };

    let Some(pos) = pos else {
        // Non-destructive fallback: marker absent from the last 1 MiB. The
        // file might be truncated, hand-edited, or written by a different
        // tool. Refuse to rewrite blindly.
        tracing::warn!(
            "strip_footer: marker not in last {} bytes; file left intact: {}",
            STRIP_FOOTER_TAIL_WINDOW_LARGE,
            path.display()
        );
        return Ok(());
    };

    truncate_file_atomic(path, pos, &mut file)
}

fn find_footer_position(file: &mut fs::File, file_size: u64, window: u64) -> Result<Option<u64>> {
    if file_size == 0 {
        return Ok(None);
    }
    let tail_len = std::cmp::min(window, file_size);
    let start = file_size - tail_len;

    file.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; tail_len as usize];
    file.read_exact(&mut buf)?;

    Ok(rfind_subslice(&buf, STRIP_FOOTER_MARKER).map(|p| start + p as u64))
}

pub(crate) fn rfind_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .rev()
        .find(|&i| &haystack[i..i + needle.len()] == needle)
}

fn truncate_file_atomic(path: &Path, pos: u64, src: &mut fs::File) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("strip_footer: path has no parent: {}", path.display()))?;
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        anyhow::anyhow!(
            "strip_footer: missing or non-UTF8 filename: {}",
            path.display()
        )
    })?;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp_name = format!(".{}.tmp.{}.{}", file_name, std::process::id(), nanos);
    let tmp_path = parent.join(tmp_name);

    let copy_result: io::Result<()> = (|| {
        let mut dst = sanitize::create_file_validated(&tmp_path)
            .map_err(|err| io::Error::other(err.to_string()))?;
        src.seek(SeekFrom::Start(0))?;

        const CHUNK: usize = 64 * 1024;
        let mut buf = vec![0u8; CHUNK];
        let mut remaining = pos;
        while remaining > 0 {
            let to_read = std::cmp::min(remaining as usize, CHUNK);
            src.read_exact(&mut buf[..to_read])?;
            dst.write_all(&buf[..to_read])?;
            remaining -= to_read as u64;
        }
        dst.flush()?;
        dst.sync_all()
    })();

    if let Err(err) = copy_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(anyhow::Error::from(err).context(format!(
            "strip_footer: stream-copy to tempfile failed: {}",
            path.display()
        )));
    }

    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "strip_footer: atomic rename {} -> {} failed",
            tmp_path.display(),
            path.display()
        )
    })?;

    // SECURITY: parent is path.parent() of a path validated at append_markdown_timeline entry; best-effort fsync after atomic rename.
    if !parent.as_os_str().is_empty()
        && let Ok(dir) = fs::OpenOptions::new().read(true).open(parent)
    {
        let _ = dir.sync_all();
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

pub(crate) fn write_formatted_message(w: &mut impl Write, message: &str) -> Result<()> {
    // CRLF/CR normalized before any downstream decision so writers see one newline form.
    let normalized = normalize_newlines(message);
    let body = normalized.as_ref();
    let has_code_blocks = body.contains("```");
    let is_multiline = body.contains('\n');

    if has_code_blocks {
        // Code-bearing messages: HTML blockquote + dynamic outer fence so inner
        // backticks (and any HTML/markdown they contain) cannot break out.
        write_blockquote_with_code(w, body)?;
    } else if !is_multiline {
        // Single line: markdown `>` blockquote with HTML escape.
        writeln!(w, "> {}", html_escape(body))?;
    } else {
        // Multi-line plain text: markdown `>` blockquote per line, HTML-escaped.
        for line in body.lines() {
            if line.is_empty() {
                writeln!(w, ">")?;
            } else {
                writeln!(w, "> {}", html_escape(line))?;
            }
        }
        writeln!(w)?;
    }

    Ok(())
}

/// Wrap a code-bearing message inside `<blockquote>` with an outer code fence
/// of dynamic length, guaranteeing the inner content cannot terminate the fence.
///
/// Caller is expected to pass CRLF-normalized input (see `normalize_newlines`).
fn write_blockquote_with_code(w: &mut impl Write, message: &str) -> Result<()> {
    let fence = dynamic_fence_for(message);
    writeln!(w, "<blockquote>")?;
    writeln!(w)?;
    writeln!(w, "{}", fence)?;
    for line in message.lines() {
        writeln!(w, "{}", line)?;
    }
    writeln!(w, "{}", fence)?;
    writeln!(w)?;
    writeln!(w, "</blockquote>")?;
    writeln!(w)?;
    Ok(())
}

/// HTML-escape the five characters that can break out of markdown blockquote
/// rendering when the downstream renderer treats raw HTML as live markup.
fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Pick a code fence one backtick longer than the longest run inside `content`
/// (minimum 3). Guarantees the fence cannot be closed early by inner backticks.
pub(crate) fn dynamic_fence_for(content: &str) -> String {
    let mut max_run = 0usize;
    let mut current = 0usize;
    for ch in content.chars() {
        if ch == '`' {
            current += 1;
            if current > max_run {
                max_run = current;
            }
        } else {
            current = 0;
        }
    }
    let fence_len = std::cmp::max(3, max_run + 1);
    "`".repeat(fence_len)
}

/// Normalize `\r\n` and lone `\r` to `\n`. Borrows when nothing to change.
fn normalize_newlines(s: &str) -> Cow<'_, str> {
    if s.contains('\r') {
        Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(s)
    }
}

pub(crate) fn write_markdown_footer(w: &mut impl Write) -> Result<()> {
    writeln!(w, "---\n*Generated by ai-contexters (c)2026 Vetcoders*")?;
    Ok(())
}
