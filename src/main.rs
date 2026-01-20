//! AI Contexters
//!
//! Extracts timeline and decisions from AI agent session files:
//! - Claude Code: ~/.claude/projects/*/*.jsonl
//! - Codex: ~/.codex/history.jsonl
//!
//! Output: Markdown timeline + JSON queryable format
//!
//! Created by M&K (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// AI Contexters - timeline and decisions from AI sessions
#[derive(Parser)]
#[command(name = "ai-contexters")]
#[command(author = "M&K (c)2026 VetCoders")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Extract timeline from Claude Code sessions
    Claude {
        /// Project directory filter (e.g., "CodeScribe" or full path)
        #[arg(short, long)]
        project: Option<String>,

        /// Hours to look back (default: 48)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Output directory
        #[arg(short, long, default_value = ".")]
        output: PathBuf,

        /// Output format: md, json, both
        #[arg(short, long, default_value = "both")]
        format: String,
    },

    /// Extract timeline from Codex history
    Codex {
        /// Project/repo filter in message text
        #[arg(short, long)]
        project: Option<String>,

        /// Hours to look back (default: 48)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Output directory
        #[arg(short, long, default_value = ".")]
        output: PathBuf,

        /// Output format: md, json, both
        #[arg(short, long, default_value = "both")]
        format: String,
    },

    /// Extract from all agents (Claude + Codex)
    All {
        /// Project filter
        #[arg(short, long)]
        project: Option<String>,

        /// Hours to look back
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Output directory
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },

    /// List available projects/sessions
    List {
        /// Agent type: claude, codex, all
        #[arg(short, long, default_value = "all")]
        agent: String,
    },
}

// ============================================================================
// Data structures
// ============================================================================

/// Unified timeline entry (works for both Claude and Codex)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TimelineEntry {
    timestamp: DateTime<Utc>,
    agent: String,        // "claude" or "codex"
    session_id: String,
    role: String,         // "user" or "assistant"
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
}

/// Claude Code JSONL entry
#[derive(Debug, Deserialize)]
struct ClaudeEntry {
    #[serde(rename = "type")]
    entry_type: String,
    #[serde(default)]
    message: Option<serde_json::Value>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    #[serde(default)]
    session_id: Option<String>,
    #[serde(rename = "gitBranch")]
    #[serde(default)]
    git_branch: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// Codex history JSONL entry
#[derive(Debug, Deserialize)]
struct CodexEntry {
    session_id: String,
    text: String,
    ts: i64,
}

/// Extracted report
#[derive(Debug, Serialize)]
struct Report {
    generated_at: DateTime<Utc>,
    project_filter: Option<String>,
    hours_back: u64,
    total_entries: usize,
    sessions: Vec<String>,
    entries: Vec<TimelineEntry>,
}

// ============================================================================
// Main
// ============================================================================

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("ai_contexters=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Claude { project, hours, output, format } => {
            extract_claude(project, hours, &output, &format)?;
        }
        Commands::Codex { project, hours, output, format } => {
            extract_codex(project, hours, &output, &format)?;
        }
        Commands::All { project, hours, output } => {
            extract_all(project, hours, &output)?;
        }
        Commands::List { agent } => {
            list_sessions(&agent)?;
        }
    }

    Ok(())
}

// ============================================================================
// Claude Code extraction
// ============================================================================

fn extract_claude(
    project_filter: Option<String>,
    hours: u64,
    output: &Path,
    format: &str,
) -> Result<()> {
    let claude_dir = dirs::home_dir()
        .context("No home dir")?
        .join(".claude")
        .join("projects");

    if !claude_dir.exists() {
        anyhow::bail!("Claude projects dir not found: {}", claude_dir.display());
    }

    let cutoff = Utc::now() - chrono::Duration::hours(hours as i64);
    let mut entries: Vec<TimelineEntry> = Vec::new();
    let mut sessions: Vec<String> = Vec::new();

    // Find matching project directories
    for dir_entry in fs::read_dir(&claude_dir)? {
        let dir_entry = dir_entry?;
        let dir_name = dir_entry.file_name().to_string_lossy().to_string();

        // Filter by project if specified
        if let Some(ref filter) = project_filter
            && !dir_name.to_lowercase().contains(&filter.to_lowercase())
        {
            continue;
        }

        let project_dir = dir_entry.path();
        if !project_dir.is_dir() {
            continue;
        }

        // Process all JSONL files in this project
        for file_entry in fs::read_dir(&project_dir)? {
            let file_entry = file_entry?;
            let path = file_entry.path();

            if path.extension().is_some_and(|e| e == "jsonl") {
                let session_entries = parse_claude_jsonl(&path, cutoff)?;
                if !session_entries.is_empty() {
                    if let Some(first) = session_entries.first() {
                        sessions.push(first.session_id.clone());
                    }
                    entries.extend(session_entries);
                }
            }
        }
    }

    // Sort by timestamp
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    sessions.sort();
    sessions.dedup();

    let report = Report {
        generated_at: Utc::now(),
        project_filter,
        hours_back: hours,
        total_entries: entries.len(),
        sessions,
        entries,
    };

    write_output(&report, output, "claude", format)?;

    eprintln!(
        "✓ Extracted {} entries from {} sessions",
        report.total_entries,
        report.sessions.len()
    );

    Ok(())
}

fn parse_claude_jsonl(path: &Path, cutoff: DateTime<Utc>) -> Result<Vec<TimelineEntry>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let entry: ClaudeEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Only process user/assistant messages
        if entry.entry_type != "user" && entry.entry_type != "assistant" {
            continue;
        }

        // Parse timestamp
        let timestamp = match &entry.timestamp {
            Some(ts) => {
                DateTime::parse_from_rfc3339(ts)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now())
            }
            None => continue,
        };

        // Filter by cutoff
        if timestamp < cutoff {
            continue;
        }

        // Extract message text
        let message = extract_message_text(&entry.message);
        if message.is_empty() {
            continue;
        }

        entries.push(TimelineEntry {
            timestamp,
            agent: "claude".to_string(),
            session_id: entry.session_id.unwrap_or_default(),
            role: entry.entry_type,
            message,
            branch: entry.git_branch,
            cwd: entry.cwd,
        });
    }

    Ok(entries)
}

fn extract_message_text(message: &Option<serde_json::Value>) -> String {
    match message {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            // Claude messages can be array of content blocks
            arr.iter()
                .filter_map(|item| {
                    if let Some(obj) = item.as_object()
                        && obj.get("type").and_then(|t| t.as_str()) == Some("text")
                    {
                        return obj.get("text").and_then(|t| t.as_str()).map(String::from);
                    }
                    None
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        Some(serde_json::Value::Object(obj)) => {
            // Message is object with "content" field (can be string or array)
            if let Some(content) = obj.get("content") {
                match content {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Array(arr) => {
                        // Content is array of blocks (assistant messages)
                        arr.iter()
                            .filter_map(|item| {
                                if let Some(block) = item.as_object()
                                    && block.get("type").and_then(|t| t.as_str()) == Some("text")
                                {
                                    return block.get("text").and_then(|t| t.as_str()).map(String::from);
                                }
                                None
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                    _ => String::new(),
                }
            } else if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                text.to_string()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

// ============================================================================
// Codex extraction
// ============================================================================

fn extract_codex(
    project_filter: Option<String>,
    hours: u64,
    output: &Path,
    format: &str,
) -> Result<()> {
    let codex_path = dirs::home_dir()
        .context("No home dir")?
        .join(".codex")
        .join("history.jsonl");

    if !codex_path.exists() {
        anyhow::bail!("Codex history not found: {}", codex_path.display());
    }

    let cutoff_ts = (Utc::now() - chrono::Duration::hours(hours as i64)).timestamp();
    let mut entries: Vec<TimelineEntry> = Vec::new();
    let mut sessions: Vec<String> = Vec::new();

    let file = File::open(&codex_path)?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let entry: CodexEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // Filter by timestamp
        if entry.ts < cutoff_ts {
            continue;
        }

        // Filter by project if specified
        if let Some(ref filter) = project_filter
            && !entry.text.to_lowercase().contains(&filter.to_lowercase())
        {
            continue;
        }

        let timestamp = Utc.timestamp_opt(entry.ts, 0).single().unwrap_or_else(Utc::now);

        if !sessions.contains(&entry.session_id) {
            sessions.push(entry.session_id.clone());
        }

        entries.push(TimelineEntry {
            timestamp,
            agent: "codex".to_string(),
            session_id: entry.session_id,
            role: "user".to_string(), // Codex only stores user messages
            message: entry.text,
            branch: None,
            cwd: None,
        });
    }

    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    let report = Report {
        generated_at: Utc::now(),
        project_filter,
        hours_back: hours,
        total_entries: entries.len(),
        sessions,
        entries,
    };

    write_output(&report, output, "codex", format)?;

    eprintln!(
        "✓ Extracted {} entries from {} sessions",
        report.total_entries,
        report.sessions.len()
    );

    Ok(())
}

// ============================================================================
// Combined extraction
// ============================================================================

fn extract_all(project_filter: Option<String>, hours: u64, output: &Path) -> Result<()> {
    eprintln!("Extracting Claude Code sessions...");
    extract_claude(project_filter.clone(), hours, output, "both")?;

    eprintln!("\nExtracting Codex history...");
    extract_codex(project_filter, hours, output, "both")?;

    Ok(())
}

// ============================================================================
// List sessions
// ============================================================================

fn list_sessions(agent: &str) -> Result<()> {
    if agent == "claude" || agent == "all" {
        println!("=== Claude Code Projects ===\n");

        let claude_dir = dirs::home_dir()
            .context("No home dir")?
            .join(".claude")
            .join("projects");

        if claude_dir.exists() {
            for entry in fs::read_dir(&claude_dir)? {
                let entry = entry?;
                if entry.path().is_dir() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // Decode path: -Users-maciejgad-hosted-VetCoders-CodeScribe
                    let decoded = name.replace('-', "/").trim_start_matches('/').to_string();

                    let jsonl_count = fs::read_dir(entry.path())?
                        .filter(|e| {
                            e.as_ref()
                                .ok()
                                .and_then(|e| e.path().extension().map(|ext| ext == "jsonl"))
                                .unwrap_or(false)
                        })
                        .count();

                    println!("  {} ({} sessions)", decoded, jsonl_count);
                }
            }
        }
    }

    if agent == "codex" || agent == "all" {
        println!("\n=== Codex History ===\n");

        let codex_path = dirs::home_dir()
            .context("No home dir")?
            .join(".codex")
            .join("history.jsonl");

        if codex_path.exists() {
            let metadata = fs::metadata(&codex_path)?;
            let size_mb = metadata.len() as f64 / 1024.0 / 1024.0;

            // Count unique sessions
            let file = File::open(&codex_path)?;
            let reader = BufReader::new(file);
            let mut sessions: std::collections::HashSet<String> = std::collections::HashSet::new();

            for line in reader.lines().take(10000).flatten() {
                if let Ok(entry) = serde_json::from_str::<CodexEntry>(&line) {
                    sessions.insert(entry.session_id);
                }
            }

            println!("  history.jsonl: {:.1} MB, ~{} sessions", size_mb, sessions.len());
        }
    }

    Ok(())
}

// ============================================================================
// Output writers
// ============================================================================

fn write_output(report: &Report, output_dir: &Path, prefix: &str, format: &str) -> Result<()> {
    fs::create_dir_all(output_dir)?;

    let date_str = report.generated_at.format("%Y%m%d_%H%M%S");

    if format == "json" || format == "both" {
        let json_path = output_dir.join(format!("{}_memory_{}.json", prefix, date_str));
        let mut file = File::create(&json_path)?;
        serde_json::to_writer_pretty(&mut file, report)?;
        eprintln!("  → {}", json_path.display());
    }

    if format == "md" || format == "both" {
        let md_path = output_dir.join(format!("{}_memory_{}.md", prefix, date_str));
        let mut file = File::create(&md_path)?;
        write_markdown(&mut file, report)?;
        eprintln!("  → {}", md_path.display());
    }

    Ok(())
}

fn write_markdown(file: &mut File, report: &Report) -> Result<()> {
    writeln!(file, "# Agent Memory Timeline\n")?;
    writeln!(file, "> Generated: {}", report.generated_at.format("%Y-%m-%d %H:%M:%S UTC"))?;
    writeln!(file, "> Filter: {:?}", report.project_filter)?;
    writeln!(file, "> Period: last {} hours", report.hours_back)?;
    writeln!(file, "> Entries: {}", report.total_entries)?;
    writeln!(file, "> Sessions: {}\n", report.sessions.len())?;
    writeln!(file, "---\n")?;

    // Group by date
    let mut by_date: HashMap<String, Vec<&TimelineEntry>> = HashMap::new();
    for entry in &report.entries {
        let date = entry.timestamp.format("%Y-%m-%d").to_string();
        by_date.entry(date).or_default().push(entry);
    }

    let mut dates: Vec<_> = by_date.keys().collect();
    dates.sort();

    for date in dates {
        writeln!(file, "## {}\n", date)?;

        for entry in by_date.get(date).unwrap() {
            let time = entry.timestamp.format("%H:%M:%S");
            let role_emoji = if entry.role == "user" { "👤" } else { "🤖" };
            let agent_badge = if entry.agent == "claude" { "[Claude]" } else { "[Codex]" };

            // Truncate long messages for MD (full in JSON)
            // Use char_indices to avoid breaking UTF-8
            let msg = if entry.message.chars().count() > 500 {
                let truncated: String = entry.message.chars().take(500).collect();
                format!("{}...", truncated)
            } else {
                entry.message.clone()
            };

            // Escape markdown and format as blockquote
            let msg = msg.replace('\n', "\n> ");

            writeln!(file, "### {} {} {} `{}`\n", time, role_emoji, agent_badge, &entry.session_id[..8.min(entry.session_id.len())])?;

            if let Some(ref branch) = entry.branch {
                writeln!(file, "Branch: `{}`\n", branch)?;
            }

            writeln!(file, "> {}\n", msg)?;
        }
    }

    writeln!(file, "---\n*Generated by ai-contexters (c)2026 VetCoders*")?;

    Ok(())
}
