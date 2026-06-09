use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::redact::redact_secrets;
use crate::sanitize;

use super::report::{write_formatted_message, write_markdown_footer};
use super::{ConversationExtractStats, ConversationMessage, ReportMetadata};

pub fn write_conversation_markdown(
    path: &Path,
    messages: &[ConversationMessage],
    metadata: &ReportMetadata,
) -> Result<PathBuf> {
    write_conversation_markdown_with_redaction(path, messages, metadata, true)
}

/// Write a denoised conversation transcript as Markdown with explicit redaction control.
pub fn write_conversation_markdown_with_redaction(
    path: &Path,
    messages: &[ConversationMessage],
    metadata: &ReportMetadata,
    redact: bool,
) -> Result<PathBuf> {
    let validated = sanitize::validate_write_path(path)?;
    if let Some(parent) = validated.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent dir: {}", parent.display()))?;
    }

    let redacted_messages;
    let messages = if redact {
        redacted_messages = redact_conversation_messages(messages);
        redacted_messages.as_slice()
    } else {
        messages
    };

    let mut file = sanitize::create_file_validated(&validated)
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

            // P0 cognitive: per-message stamps below are time-only. Emit a date
            // heading whenever the day changes so a reader can tell "yesterday"
            // from "8 months ago" — the year/date was previously absent from the
            // extract, leaving bare `[HH:MM:SS]` stamps with no anchor.
            let mut last_date: Option<String> = None;
            for msg in session_msgs {
                let date = msg.timestamp.format("%Y-%m-%d").to_string();
                if last_date.as_deref() != Some(date.as_str()) {
                    writeln!(file, "#### {}\n", date)?;
                    last_date = Some(date);
                }
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
    extract_stats: &ConversationExtractStats,
) -> Result<PathBuf> {
    write_conversation_json_with_redaction(path, messages, metadata, extract_stats, true)
}

/// Write a denoised conversation transcript as JSON with explicit redaction control.
pub fn write_conversation_json_with_redaction(
    path: &Path,
    messages: &[ConversationMessage],
    metadata: &ReportMetadata,
    extract_stats: &ConversationExtractStats,
    redact: bool,
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
        extract_stats: &'a ConversationExtractStats,
        messages: &'a [ConversationMessage],
    }

    let redacted_messages;
    let messages = if redact {
        redacted_messages = redact_conversation_messages(messages);
        redacted_messages.as_slice()
    } else {
        messages
    };

    let report = ConversationReport {
        generated_at: metadata.generated_at,
        project_filter: &metadata.project_filter,
        hours_back: metadata.hours_back,
        total_messages: messages.len(),
        sessions: &metadata.sessions,
        extract_stats,
        messages,
    };

    let file = sanitize::create_file_validated(&validated)
        .with_context(|| format!("Failed to create: {}", path.display()))?;
    serde_json::to_writer_pretty(file, &report)?;
    eprintln!("  -> {}", path.display());
    Ok(validated)
}

fn redact_conversation_messages(messages: &[ConversationMessage]) -> Vec<ConversationMessage> {
    messages
        .iter()
        .cloned()
        .map(|mut msg| {
            msg.message = redact_secrets(&msg.message);
            msg
        })
        .collect()
}
