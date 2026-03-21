//! AI Contexters
//!
//! Extracts timeline and decisions from AI agent session files:
//! - Claude Code: ~/.claude/projects/*/*.jsonl
//! - Codex: ~/.codex/history.jsonl
//! - Gemini: ~/.gemini/tmp/<hash>/chats/session-*.json
//! - Gemini Antigravity: ~/.gemini/antigravity/{conversations/<uuid>.pb,brain/<uuid>/}
//!
//! Features: incremental extraction, deduplication, rotation, append mode.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use ai_contexters::chunker::{self, ChunkerConfig};
use ai_contexters::dashboard::{self, DashboardConfig};
use ai_contexters::dashboard_server::{self, DashboardServerConfig};
use ai_contexters::init::{self, InitOptions};
use ai_contexters::intents;
use ai_contexters::memex::{self, MemexConfig};
use ai_contexters::output::{self, OutputConfig, OutputFormat, OutputMode, ReportMetadata};
use ai_contexters::rank;
use ai_contexters::sources::{self, ExtractionConfig};
use ai_contexters::state::StateManager;
use ai_contexters::store;

/// AI Contexters - timeline and decisions from AI sessions
#[derive(Parser)]
#[command(name = "aicx")]
#[command(author = "M&K (c)2026 VetCoders")]
#[command(version)]
struct Cli {
    /// Redact secrets (tokens/keys) from outputs before writing/syncing.
    ///
    /// Use `--no-redact-secrets` to disable (not recommended).
    #[arg(
        long = "no-redact-secrets",
        action = ArgAction::SetFalse,
        default_value_t = true,
        global = true
    )]
    redact_secrets: bool,

    /// Project filter (used if no subcommand is provided)
    #[arg(short, long, global = true)]
    project: Option<String>,

    /// Hours to look back (used if no subcommand is provided)
    #[arg(short = 'H', long, default_value = "48", global = true)]
    hours: u64,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum StdoutEmit {
    /// Print store chunk paths (one per line).
    Paths,
    /// Print JSON report (includes `store_paths` for convenience).
    Json,
    /// Print nothing to stdout.
    None,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RefsEmit {
    /// Print a compact per-project summary.
    Summary,
    /// Print raw file paths (one per line).
    Paths,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExtractInputFormat {
    Claude,
    Codex,
    Gemini,
    GeminiAntigravity,
}

#[derive(Subcommand)]
enum Commands {
    /// Extract timeline from Claude Code sessions
    Claude {
        /// Project directory filter(s): -p foo bar baz
        #[arg(short, long, num_args = 1..)]
        project: Vec<String>,

        /// Hours to look back (default: 48)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Output directory (omit to only write to store)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Output format: md, json, both
        #[arg(short, long, default_value = "both")]
        format: String,

        /// Append to a single timeline file instead of creating new files
        #[arg(long)]
        append_to: Option<PathBuf>,

        /// Keep only last N output files (0 = unlimited)
        #[arg(long, default_value = "0")]
        rotate: usize,

        /// Use incremental mode (skip already-processed entries)
        #[arg(long)]
        incremental: bool,

        /// Only include user messages (exclude assistant + reasoning)
        #[arg(long)]
        user_only: bool,

        /// Include assistant messages (legacy flag; now default)
        #[arg(long, hide = true, conflicts_with = "user_only")]
        include_assistant: bool,

        /// Include loctree snapshot in output
        #[arg(long)]
        loctree: bool,

        /// Project root for loctree snapshot (defaults to cwd)
        #[arg(long)]
        project_root: Option<PathBuf>,

        /// Also chunk and sync to memex after extraction
        #[arg(long)]
        memex: bool,

        /// Force full extraction, ignore dedup hashes
        #[arg(long)]
        force: bool,

        /// What to print to stdout: paths, json, none (default: none)
        #[arg(long, value_enum, default_value_t = StdoutEmit::None)]
        emit: StdoutEmit,

        /// Conversation-first mode: emit denoised user/assistant transcript only
        #[arg(long)]
        conversation: bool,
    },

    /// Extract timeline from Codex history
    Codex {
        /// Project/repo filter(s): -p foo bar baz
        #[arg(short, long, num_args = 1..)]
        project: Vec<String>,

        /// Hours to look back (default: 48)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Output directory (omit to only write to store)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Output format: md, json, both
        #[arg(short, long, default_value = "both")]
        format: String,

        /// Append to a single timeline file
        #[arg(long)]
        append_to: Option<PathBuf>,

        /// Keep only last N output files (0 = unlimited)
        #[arg(long, default_value = "0")]
        rotate: usize,

        /// Use incremental mode
        #[arg(long)]
        incremental: bool,

        /// Only include user messages (exclude assistant + reasoning)
        #[arg(long)]
        user_only: bool,

        /// Include assistant messages (legacy flag; now default)
        #[arg(long, hide = true, conflicts_with = "user_only")]
        include_assistant: bool,

        /// Include loctree snapshot
        #[arg(long)]
        loctree: bool,

        /// Project root for loctree snapshot
        #[arg(long)]
        project_root: Option<PathBuf>,

        /// Also chunk and sync to memex after extraction
        #[arg(long)]
        memex: bool,

        /// Force full extraction, ignore dedup hashes
        #[arg(long)]
        force: bool,

        /// What to print to stdout: paths, json, none (default: none)
        #[arg(long, value_enum, default_value_t = StdoutEmit::None)]
        emit: StdoutEmit,

        /// Conversation-first mode: emit denoised user/assistant transcript only
        #[arg(long)]
        conversation: bool,
    },

    /// Extract from all agents (Claude + Codex + Gemini)
    All {
        /// Project filter(s): -p foo bar baz
        #[arg(short, long, num_args = 1..)]
        project: Vec<String>,

        /// Hours to look back
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Output directory (omit to only write to store)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Append to a single timeline file
        #[arg(long)]
        append_to: Option<PathBuf>,

        /// Keep only last N output files (0 = unlimited)
        #[arg(long, default_value = "0")]
        rotate: usize,

        /// Use incremental mode
        #[arg(long)]
        incremental: bool,

        /// Only include user messages (exclude assistant + reasoning)
        #[arg(long)]
        user_only: bool,

        /// Include assistant messages (legacy flag; now default)
        #[arg(long, hide = true, conflicts_with = "user_only")]
        include_assistant: bool,

        /// Include loctree snapshot
        #[arg(long)]
        loctree: bool,

        /// Project root for loctree snapshot
        #[arg(long)]
        project_root: Option<PathBuf>,

        /// Also chunk and sync to memex after extraction
        #[arg(long)]
        memex: bool,

        /// Force full extraction, ignore dedup hashes
        #[arg(long)]
        force: bool,

        /// What to print to stdout: paths, json, none (default: none)
        #[arg(long, value_enum, default_value_t = StdoutEmit::None)]
        emit: StdoutEmit,

        /// Conversation-first mode: emit denoised user/assistant transcript only
        #[arg(long)]
        conversation: bool,
    },

    /// Extract timeline from a single agent session file (direct path).
    ///
    /// Example:
    ///   aicx extract --format claude /path/to/session.jsonl -o /tmp/report.md
    Extract {
        /// Input format (agent): claude | codex | gemini | gemini-antigravity
        #[arg(long, value_enum, alias = "input-format")]
        format: ExtractInputFormat,

        /// Explicit project/repo name (overrides inference)
        #[arg(short, long)]
        project: Option<String>,

        /// Input path (JSONL / JSON / Antigravity brain directory depending on agent)
        input: PathBuf,

        /// Output file path (e.g. /tmp/report.md)
        #[arg(short, long)]
        output: PathBuf,

        /// Only include user messages (exclude assistant + reasoning)
        #[arg(long)]
        user_only: bool,

        /// Include assistant messages (legacy flag; now default)
        #[arg(long, hide = true, conflicts_with = "user_only")]
        include_assistant: bool,

        /// Maximum message characters in markdown (0 = no truncation)
        #[arg(long, default_value = "0")]
        max_message_chars: usize,

        /// Conversation-first mode: emit denoised user/assistant transcript only
        #[arg(long)]
        conversation: bool,
    },

    /// Store contexts in central store (~/.ai-contexters/) and optionally sync to memex
    Store {
        /// Project name(s): -p foo bar baz
        #[arg(short, long, num_args = 1..)]
        project: Vec<String>,

        /// Agent filter: claude, codex, gemini (default: all)
        #[arg(short, long)]
        agent: Option<String>,

        /// Hours to look back (default: 48)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Only include user messages (exclude assistant + reasoning)
        #[arg(long)]
        user_only: bool,

        /// Include assistant messages (legacy flag; now default)
        #[arg(long, hide = true, conflicts_with = "user_only")]
        include_assistant: bool,

        /// Also chunk and sync to memex
        #[arg(long)]
        memex: bool,

        /// What to print to stdout: paths, json, none (default: none)
        #[arg(long, value_enum, default_value_t = StdoutEmit::None)]
        emit: StdoutEmit,
    },

    /// Sync stored chunks to rmcp-memex vector memory
    MemexSync {
        /// Namespace in vector store
        #[arg(short, long, default_value = "ai-contexts")]
        namespace: String,

        /// Use per-chunk upsert instead of batch index
        #[arg(long)]
        per_chunk: bool,

        /// Override LanceDB path
        #[arg(long)]
        db_path: Option<PathBuf>,
    },

    /// List available projects/sessions
    List,

    /// List context files from global store (references)
    Refs {
        /// Hours to look back (filter by file mtime)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Project filter
        #[arg(short, long)]
        project: Option<String>,

        /// What to print to stdout: summary, paths (default: summary)
        #[arg(long, value_enum, default_value_t = RefsEmit::Summary)]
        emit: RefsEmit,

        /// Legacy alias for `--emit summary`
        #[arg(short, long, hide = true)]
        summary: bool,

        /// Filter out low-signal noise (<15 lines, task-notifications only)
        #[arg(long)]
        strict: bool,
    },

    /// Rank and filter artifacts by content quality (QUALITY > quantity)
    Rank {
        /// Hours to look back
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Project filter
        #[arg(short, long)]
        project: String,

        /// Only show chunks scoring >= 5 (hide noise)
        #[arg(long)]
        strict: bool,

        /// Show only top N bundles by score
        #[arg(long)]
        top: Option<usize>,
    },

    /// Manage dedup state
    State {
        /// Reset all dedup hashes
        #[arg(long)]
        reset: bool,

        /// Project to reset (with --reset)
        #[arg(short, long)]
        project: Option<String>,

        /// Show state info/statistics
        #[arg(long)]
        info: bool,
    },

    /// Generate a searchable HTML dashboard from the aicx store.
    Dashboard {
        /// Store root directory (default: ~/.ai-contexters)
        #[arg(long)]
        store_root: Option<PathBuf>,

        /// Output HTML path
        #[arg(short, long, default_value = "aicx-dashboard.html")]
        output: PathBuf,

        /// Document title
        #[arg(long, default_value = "AI Contexters Dashboard")]
        title: String,

        /// Max preview characters per record (0 = no truncation)
        #[arg(long, default_value = "320")]
        preview_chars: usize,
    },

    /// Run dashboard HTTP server with on-demand regeneration endpoints.
    DashboardServe {
        /// Store root directory (default: ~/.ai-contexters)
        #[arg(long)]
        store_root: Option<PathBuf>,

        /// Bind host IP address (example: 127.0.0.1)
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Bind TCP port
        #[arg(long, default_value = "8033")]
        port: u16,

        /// Artifact path written on startup and each regeneration
        #[arg(long, default_value = "aicx-dashboard.html")]
        artifact: PathBuf,

        /// Document title
        #[arg(long, default_value = "AI Contexters Dashboard")]
        title: String,

        /// Max preview characters per record (0 = no truncation)
        #[arg(long, default_value = "320")]
        preview_chars: usize,
    },

    /// Extract structured intents and decisions from stored context
    Intents {
        /// Project filter (required)
        #[arg(short, long)]
        project: String,

        /// Hours to look back (default: 720 = 30 days)
        #[arg(short = 'H', long, default_value = "720")]
        hours: u64,

        /// Output format: markdown or json
        #[arg(long, default_value = "markdown", value_parser = ["markdown", "json"])]
        emit: String,

        /// Only show high-confidence intents
        #[arg(long)]
        strict: bool,

        /// Filter by kind: decision, intent, outcome, task
        #[arg(long, value_parser = ["decision", "intent", "outcome", "task"])]
        kind: Option<String>,
    },

    /// Run aicx as an MCP server (stdio or streamable HTTP)
    Serve {
        /// Transport: stdio (default) or sse
        #[arg(long, default_value = "stdio", value_parser = ["stdio", "sse"])]
        transport: String,

        /// Port for SSE transport (default: 8044)
        #[arg(long, default_value = "8044")]
        port: u16,
    },

    /// Initialize repo context and run an agent
    Init {
        /// Project name override
        #[arg(short, long)]
        project: Option<String>,

        /// Agent override: claude or codex
        #[arg(short, long)]
        agent: Option<String>,

        /// Model override (optional; if omitted uses agent default)
        #[arg(long)]
        model: Option<String>,

        /// Hours to look back for context (default: 4800)
        #[arg(short = 'H', long, default_value = "4800")]
        hours: u64,

        /// Maximum lines per context section in the prompt
        #[arg(long, default_value = "1200")]
        max_lines: usize,

        /// Only include user messages in context (exclude assistant + reasoning)
        #[arg(long)]
        user_only: bool,

        /// Include assistant messages (legacy flag; now default)
        #[arg(long, hide = true, conflicts_with = "user_only")]
        include_assistant: bool,

        /// Action focus appended to the prompt
        #[arg(long)]
        action: Option<String>,

        /// Additional agent prompt appended after core rules (verbatim)
        #[arg(long)]
        agent_prompt: Option<String>,

        /// Read additional agent prompt from a file (verbatim)
        #[arg(long)]
        agent_prompt_file: Option<PathBuf>,

        /// Build context/prompt only, do not run an agent
        #[arg(long)]
        no_run: bool,

        /// Skip "Run? (y)es / (n)o" confirmation
        #[arg(long)]
        no_confirm: bool,

        /// Do not auto-modify `.gitignore`
        #[arg(long)]
        no_gitignore: bool,
    },

    /// Migrate older file-centric contexts to canonical repo-centric directories
    Migrate {
        /// Dry run: show what would be moved without modifying files
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("ai_contexters=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
    let redact_secrets = cli.redact_secrets;

    match cli.command {
        Some(Commands::Claude {
            project,
            hours,
            output,
            format,
            append_to,
            rotate,
            incremental,
            user_only,
            include_assistant: include_assistant_flag,
            loctree,
            project_root,
            memex,
            force,
            emit,
            conversation,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;
            run_extraction(ExtractionParams {
                agents: &["claude"],
                project,
                hours,
                output_dir: output.as_deref(),
                format: &format,
                append_to,
                rotate,
                incremental,
                include_assistant,
                include_loctree: loctree,
                project_root,
                sync_memex: memex,
                force,
                redact_secrets,
                emit,
                conversation,
            })?;
        }
        Some(Commands::Codex {
            project,
            hours,
            output,
            format,
            append_to,
            rotate,
            incremental,
            user_only,
            include_assistant: include_assistant_flag,
            loctree,
            project_root,
            memex,
            force,
            emit,
            conversation,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;
            run_extraction(ExtractionParams {
                agents: &["codex"],
                project,
                hours,
                output_dir: output.as_deref(),
                format: &format,
                append_to,
                rotate,
                incremental,
                include_assistant,
                include_loctree: loctree,
                project_root,
                sync_memex: memex,
                force,
                redact_secrets,
                emit,
                conversation,
            })?;
        }
        Some(Commands::All {
            project,
            hours,
            output,
            append_to,
            rotate,
            incremental,
            user_only,
            include_assistant: include_assistant_flag,
            loctree,
            project_root,
            memex,
            force,
            emit,
            conversation,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;
            run_extraction(ExtractionParams {
                agents: &["claude", "codex", "gemini"],
                project,
                hours,
                output_dir: output.as_deref(),
                format: "both",
                append_to,
                rotate,
                incremental,
                include_assistant,
                include_loctree: loctree,
                project_root,
                sync_memex: memex,
                force,
                redact_secrets,
                emit,
                conversation,
            })?;
        }
        Some(Commands::Extract {
            format,
            project,
            input,
            output,
            user_only,
            include_assistant: include_assistant_flag,
            max_message_chars,
            conversation,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;
            run_extract_file(
                format,
                project,
                input,
                output,
                include_assistant,
                max_message_chars,
                redact_secrets,
                conversation,
            )?;
        }
        Some(Commands::Store {
            project,
            agent,
            hours,
            user_only,
            include_assistant: include_assistant_flag,
            memex,
            emit,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;
            run_store(
                project,
                agent,
                hours,
                include_assistant,
                memex,
                emit,
                redact_secrets,
            )?;
        }
        Some(Commands::MemexSync {
            namespace,
            per_chunk,
            db_path,
        }) => {
            run_memex_sync(&namespace, per_chunk, db_path)?;
        }
        Some(Commands::List) => {
            let sources = sources::list_available_sources()?;
            if sources.is_empty() {
                println!("No AI agent session sources found.");
            } else {
                println!("=== Available Sources ===\n");
                for info in &sources {
                    let size_mb = info.size_bytes as f64 / 1024.0 / 1024.0;
                    println!(
                        "  [{:>7}] {} ({} sessions, {:.1} MB)",
                        info.agent,
                        info.path.display(),
                        info.sessions,
                        size_mb,
                    );
                }
            }
        }
        Some(Commands::Init {
            project,
            agent,
            model,
            hours,
            max_lines,
            action,
            agent_prompt,
            agent_prompt_file,
            user_only,
            include_assistant: include_assistant_flag,
            no_run,
            no_confirm,
            no_gitignore,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;
            let opts = InitOptions {
                project,
                agent,
                model,
                horizon_hours: hours,
                max_lines,
                include_assistant,
                redact_secrets,
                action,
                agent_prompt,
                agent_prompt_file,
                no_run,
                no_confirm,
                no_gitignore,
            };
            init::run_init(opts)?;
        }
        Some(Commands::Refs {
            hours,
            project,
            emit,
            summary,
            strict,
        }) => {
            let emit = if summary { RefsEmit::Summary } else { emit };
            run_refs(hours, project, emit, strict)?;
        }
        Some(Commands::Rank {
            hours,
            project,
            strict,
            top,
        }) => {
            run_rank(hours, &project, redact_secrets, strict, top)?;
        }
        Some(Commands::State {
            reset,
            project,
            info,
        }) => {
            run_state(reset, project, info)?;
        }
        Some(Commands::Dashboard {
            store_root,
            output,
            title,
            preview_chars,
        }) => {
            run_dashboard(DashboardRunArgs {
                store_root,
                output,
                title,
                preview_chars,
            })?;
        }
        Some(Commands::DashboardServe {
            store_root,
            host,
            port,
            artifact,
            title,
            preview_chars,
        }) => {
            run_dashboard_server(DashboardServerRunArgs {
                store_root,
                host,
                port,
                artifact,
                title,
                preview_chars,
            })?;
        }
        Some(Commands::Intents {
            project,
            hours,
            emit,
            strict,
            kind,
        }) => {
            run_intents(&project, hours, &emit, strict, kind.as_deref())?;
        }
        Some(Commands::Serve { transport, port }) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                match transport.as_str() {
                    "sse" => ai_contexters::mcp::run_sse(port).await,
                    _ => ai_contexters::mcp::run_stdio().await,
                }
            })?;
        }
        Some(Commands::Migrate { dry_run }) => {
            ai_contexters::store::run_migration(dry_run)?;
        }
        None => {
            let project = cli.project.unwrap_or_else(sources::detect_project_name);
            let hours = cli.hours;
            run_rank(hours, &project, redact_secrets, false, None)?;
        }
    }

    Ok(())
}

fn run_intents(
    project: &str,
    hours: u64,
    emit: &str,
    strict: bool,
    kind: Option<&str>,
) -> Result<()> {
    let kind_filter = kind.map(|k| match k {
        "decision" => intents::IntentKind::Decision,
        "intent" => intents::IntentKind::Intent,
        "outcome" => intents::IntentKind::Outcome,
        "task" => intents::IntentKind::Task,
        _ => unreachable!("clap validates this"),
    });

    let config = intents::IntentsConfig {
        project: project.to_string(),
        hours,
        strict,
        kind_filter,
    };

    let records = intents::extract_intents(&config)?;

    if records.is_empty() {
        eprintln!(
            "No intents found for project '{}' in last {} hours.",
            project, hours
        );
        return Ok(());
    }

    match emit {
        "json" => {
            let json = intents::format_intents_json(&records)?;
            println!("{}", json);
        }
        _ => {
            let md = intents::format_intents_markdown(&records);
            print!("{}", md);
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_extract_file(
    format: ExtractInputFormat,
    explicit_project: Option<String>,
    input: PathBuf,
    output_path: PathBuf,
    include_assistant: bool,
    max_message_chars: usize,
    redact_secrets: bool,
    conversation: bool,
) -> Result<()> {
    // For direct file extraction we intentionally don't apply a time cutoff;
    // set cutoff far in the past.
    let cutoff = Utc::now() - chrono::Duration::days(365 * 200);
    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff,
        include_assistant,
        watermark: None,
    };

    let mut entries = match format {
        ExtractInputFormat::Claude => sources::extract_claude_file(&input, &config)?,
        ExtractInputFormat::Codex => sources::extract_codex_file(&input, &config)?,
        ExtractInputFormat::Gemini => sources::extract_gemini_file(&input, &config)?,
        ExtractInputFormat::GeminiAntigravity => {
            sources::extract_gemini_antigravity_file(&input, &config)?
        }
    };

    // Sort by timestamp (extractors should already do this).
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    // Apply secret redaction in-place (TimelineEntry is now single definition in sources)
    if redact_secrets {
        for e in &mut entries {
            e.message = ai_contexters::redact::redact_secrets(&e.message);
        }
    }
    // Collect derived data from entries before moving them.
    let mut sessions: Vec<String> = entries.iter().map(|e| e.session_id.clone()).collect();
    sessions.sort();
    sessions.dedup();

    // Canonical Precedence: Explicit --project > Inferred Repo > File Provenance
    let file_label = input
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "(unknown)".to_string());

    let inferred_repos = sources::repo_labels_from_entries(&entries, &[]);
    let project_identity = explicit_project.unwrap_or_else(|| {
        if inferred_repos.is_empty() {
            format!("file: {file_label}")
        } else {
            inferred_repos.join("+")
        }
    });

    let hours_back = entries
        .first()
        .map(|e| (Utc::now() - e.timestamp).num_hours().max(0) as u64)
        .unwrap_or(0);

    let output_entries = entries;

    let metadata = ReportMetadata {
        generated_at: Utc::now(),
        project_filter: Some(project_identity),
        hours_back,
        total_entries: output_entries.len(),
        sessions,
    };

    if conversation {
        let project_filter = metadata
            .project_filter
            .as_ref()
            .map(|p| vec![p.clone()])
            .unwrap_or_default();
        let conv_msgs = sources::to_conversation(&output_entries, &project_filter);

        let ext = output_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("md")
            .to_lowercase();

        if ext == "json" {
            output::write_conversation_json(&output_path, &conv_msgs, &metadata)?;
        } else {
            output::write_conversation_markdown(&output_path, &conv_msgs, &metadata)?;
        }
    } else {
        let ext = output_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("md")
            .to_lowercase();

        if ext == "json" {
            output::write_json_report_to_path(&output_path, &output_entries, &metadata)?;
        } else {
            output::write_markdown_report_to_path(
                &output_path,
                &output_entries,
                &metadata,
                max_message_chars,
                None,
            )?;
        }
    }

    Ok(())
}

struct ExtractionParams<'a> {
    agents: &'a [&'a str],
    project: Vec<String>,
    hours: u64,
    output_dir: Option<&'a Path>,
    format: &'a str,
    append_to: Option<PathBuf>,
    rotate: usize,
    incremental: bool,
    include_assistant: bool,
    include_loctree: bool,
    project_root: Option<PathBuf>,
    sync_memex: bool,
    force: bool,
    conversation: bool,
    redact_secrets: bool,
    emit: StdoutEmit,
}

fn run_extraction(params: ExtractionParams<'_>) -> Result<()> {
    let ExtractionParams {
        agents,
        project,
        hours,
        output_dir,
        format,
        append_to,
        rotate,
        incremental,
        include_assistant,
        include_loctree,
        project_root,
        sync_memex,
        force,
        conversation,
        redact_secrets,
        emit,
    } = params;

    // Load state for incremental/dedup
    let mut state = StateManager::load();
    let project_name = if project.is_empty() {
        "_global".to_string()
    } else {
        project.join("+")
    };

    let cutoff = Utc::now() - chrono::Duration::hours(hours as i64);

    // Determine watermark (incremental mode uses per-source watermark)
    let watermark = if incremental {
        let source_key = format!(
            "{}:{}",
            agents.join("+"),
            if project.is_empty() {
                "all".to_string()
            } else {
                project.join("+")
            }
        );
        state.get_watermark(&source_key)
    } else {
        None
    };

    let config = ExtractionConfig {
        project_filter: project.clone(),
        cutoff,
        include_assistant,
        watermark,
    };

    // Extract from requested sources
    let mut entries = Vec::new();

    for &agent in agents {
        let agent_entries = match agent {
            "claude" => sources::extract_claude(&config)?,
            "codex" => sources::extract_codex(&config)?,
            "gemini" => sources::extract_gemini(&config)?,
            _ => Vec::new(),
        };

        eprintln!("  [{}] {} entries", agent, agent_entries.len());
        entries.extend(agent_entries);
    }

    // Two-level dedup (skip if --force):
    //
    // 1. Exact dedup: (agent, timestamp, message) — catches same entry
    //    from multiple session JSONL files within the same agent.
    // 2. Overlap dedup: (message, timestamp_bucket_60s) — catches the same
    //    prompt broadcast to multiple agents simultaneously (e.g., 8 parallel
    //    Claude sessions receiving identical 3-paragraph context).
    //
    // We mark_seen during filtering so duplicates within a single run
    // are caught — not just across runs.
    let pre_dedup = entries.len();
    let overlap_project = format!("_overlap:{project_name}");
    if !force {
        let mut deduped = Vec::with_capacity(entries.len());
        for e in entries {
            let exact = StateManager::content_hash(&e.agent, e.timestamp.timestamp(), &e.message);
            if !state.is_new(&project_name, exact) {
                continue; // exact duplicate
            }

            let overlap = StateManager::overlap_hash(e.timestamp.timestamp(), &e.message);
            if !state.is_new(&overlap_project, overlap) {
                continue; // cross-agent overlap duplicate
            }

            state.mark_seen(&project_name, exact);
            state.mark_seen(&overlap_project, overlap);
            deduped.push(e);
        }
        entries = deduped;
    }

    if pre_dedup != entries.len() {
        eprintln!(
            "  Dedup: {} → {} entries (skipped {} seen)",
            pre_dedup,
            entries.len(),
            pre_dedup - entries.len(),
        );
    }

    // Sort by timestamp
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    // Filter self-echo (aicx's own search/rank/store calls that create feedback loops)
    let pre_echo = entries.len();
    entries.retain(|e| !ai_contexters::sanitize::is_self_echo(&e.message));
    let echo_filtered = pre_echo - entries.len();
    if echo_filtered > 0 {
        eprintln!("  Filtered {echo_filtered} self-echo entries");
    }

    // Apply secret redaction in-place (TimelineEntry is now single definition in sources)
    if redact_secrets {
        for e in &mut entries {
            e.message = ai_contexters::redact::redact_secrets(&e.message);
        }
    }
    // Collect derived data from entries before moving them.
    let mut sessions: Vec<String> = entries.iter().map(|e| e.session_id.clone()).collect();
    sessions.sort();
    sessions.dedup();

    let output_entries = entries;

    let metadata = ReportMetadata {
        generated_at: Utc::now(),
        project_filter: if project.is_empty() {
            None
        } else {
            Some(project.join(", "))
        },
        hours_back: hours,
        total_entries: output_entries.len(),
        sessions,
    };

    // ── Store-first: group entries by repo (from cwd) × agent × date ──
    //
    // Writes agent-friendly chunks (~1500 tokens) to central store.
    // Raw path emission is opt-in via `--emit paths` / `--emit json`.
    let chunker_config = ai_contexters::chunker::ChunkerConfig::default();
    let mut all_written_paths: Vec<std::path::PathBuf> = Vec::new();

    if !output_entries.is_empty() {
        let mut repo_groups: std::collections::BTreeMap<
            (String, String, String),
            Vec<output::TimelineEntry>,
        > = std::collections::BTreeMap::new();

        for entry in &output_entries {
            let repo = sources::repo_name_from_cwd(entry.cwd.as_deref(), &project);
            let date = entry.timestamp.format("%Y-%m-%d").to_string();
            repo_groups
                .entry((repo, entry.agent.clone(), date))
                .or_default()
                .push(entry.clone());
        }

        let mut index = store::load_index();
        let now = Utc::now();
        let time_str = now.format("%H%M%S").to_string();

        // Per-repo summary counters
        let mut repo_summary: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, usize>,
        > = std::collections::BTreeMap::new();

        for ((repo, agent_name, date), group_entries) in &repo_groups {
            let paths = store::write_context_chunked(
                repo,
                agent_name,
                date,
                &time_str,
                group_entries,
                &chunker_config,
            )?;
            store::update_index(&mut index, repo, agent_name, date, group_entries.len());
            *repo_summary
                .entry(repo.clone())
                .or_default()
                .entry(agent_name.clone())
                .or_insert(0) += group_entries.len();
            all_written_paths.extend(paths);
        }
        store::save_index(&index)?;

        // Summary to stderr (diagnostics)
        eprintln!(
            "✓ {} entries → {} chunks",
            output_entries.len(),
            all_written_paths.len(),
        );
        for (repo, agents_map) in &repo_summary {
            let total: usize = agents_map.values().sum();
            let detail: Vec<String> = agents_map
                .iter()
                .map(|(a, c)| format!("{}: {}", a, c))
                .collect();
            eprintln!("  {}: {} entries ({})", repo, total, detail.join(", "));
        }
    }

    // stdout emission (integration-friendly).
    match emit {
        StdoutEmit::Paths => {
            // agent-readable paths (one per line)
            for path in &all_written_paths {
                println!("{}", path.display());
            }
        }
        StdoutEmit::Json => {
            let store_paths: Vec<String> = all_written_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect();

            if conversation {
                #[derive(Serialize)]
                struct JsonConvStdout<'a> {
                    generated_at: chrono::DateTime<Utc>,
                    project_filter: &'a Option<String>,
                    hours_back: u64,
                    total_messages: usize,
                    sessions: &'a [String],
                    messages: Vec<sources::ConversationMessage>,
                    store_paths: Vec<String>,
                }

                let conv_msgs = sources::to_conversation(&output_entries, &project);
                let report = JsonConvStdout {
                    generated_at: metadata.generated_at,
                    project_filter: &metadata.project_filter,
                    hours_back: metadata.hours_back,
                    total_messages: conv_msgs.len(),
                    sessions: &metadata.sessions,
                    messages: conv_msgs,
                    store_paths,
                };
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                #[derive(Serialize)]
                struct JsonStdoutReport<'a> {
                    generated_at: chrono::DateTime<Utc>,
                    project_filter: &'a Option<String>,
                    hours_back: u64,
                    total_entries: usize,
                    sessions: &'a [String],
                    entries: &'a [output::TimelineEntry],
                    store_paths: Vec<String>,
                }

                let report = JsonStdoutReport {
                    generated_at: metadata.generated_at,
                    project_filter: &metadata.project_filter,
                    hours_back: metadata.hours_back,
                    total_entries: metadata.total_entries,
                    sessions: &metadata.sessions,
                    entries: &output_entries,
                    store_paths,
                };
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
        }
        StdoutEmit::None => {}
    }

    // ── Optional local output (only when -o explicitly passed) ──
    if let Some(local_dir) = output_dir {
        if conversation {
            // Conversation-first mode: denoised transcript output
            let conv_msgs = sources::to_conversation(&output_entries, &project);
            let date_str = metadata.generated_at.format("%Y%m%d_%H%M%S");
            let prefix = metadata.project_filter.as_deref().unwrap_or("all");

            let out_format = match format {
                "md" => OutputFormat::Markdown,
                "json" => OutputFormat::Json,
                _ => OutputFormat::Both,
            };

            fs::create_dir_all(local_dir)?;

            if out_format == OutputFormat::Markdown || out_format == OutputFormat::Both {
                let md_path = local_dir.join(format!("{}_conversation_{}.md", prefix, date_str));
                output::write_conversation_markdown(&md_path, &conv_msgs, &metadata)?;
            }
            if out_format == OutputFormat::Json || out_format == OutputFormat::Both {
                let json_path =
                    local_dir.join(format!("{}_conversation_{}.json", prefix, date_str));
                output::write_conversation_json(&json_path, &conv_msgs, &metadata)?;
            }
        } else {
            let out_format = match format {
                "md" => OutputFormat::Markdown,
                "json" => OutputFormat::Json,
                _ => OutputFormat::Both,
            };

            let mode = if let Some(ref path) = append_to {
                OutputMode::AppendTimeline(path.clone())
            } else {
                OutputMode::NewFile
            };

            let out_config = OutputConfig {
                dir: local_dir.to_path_buf(),
                format: out_format,
                mode,
                max_files: rotate,
                max_message_chars: 0,
                include_loctree,
                project_root,
            };

            let written = output::write_report(&out_config, &output_entries, &metadata)?;
            for path in &written {
                eprintln!("  → {}", path.display());
            }

            // Rotation
            if rotate > 0 {
                let prefix = agents.join("_");
                let deleted = output::rotate_outputs(local_dir, &prefix, rotate)?;
                if deleted > 0 {
                    eprintln!("  Rotated: deleted {} old files", deleted);
                }
            }
        }
    }

    // Update state (hashes already marked during dedup filtering above)
    if !output_entries.is_empty() {
        if force {
            // When --force skips dedup, we still mark entries as seen for future runs
            for e in &output_entries {
                let exact =
                    StateManager::content_hash(&e.agent, e.timestamp.timestamp(), &e.message);
                let overlap = StateManager::overlap_hash(e.timestamp.timestamp(), &e.message);
                state.mark_seen(&project_name, exact);
                state.mark_seen(&overlap_project, overlap);
            }
        }

        if incremental {
            let source_key = format!(
                "{}:{}",
                agents.join("+"),
                if project.is_empty() {
                    "all".to_string()
                } else {
                    project.join("+")
                }
            );
            if let Some(latest) = output_entries.last() {
                state.update_watermark(&source_key, latest.timestamp);
            }
        }

        state.record_run(
            output_entries.len(),
            agents.iter().map(|s| s.to_string()).collect(),
        );
        state.prune_old_hashes(50_000);
        state.save()?;
    }

    if output_entries.is_empty() {
        eprintln!(
            "✓ 0 entries from {} sessions ({})",
            metadata.sessions.len(),
            agents.join("+"),
        );
    }

    // Memex sync: chunk entries and push to vector store
    if sync_memex && !output_entries.is_empty() {
        let agent_name = agents.join("+");

        let chunker_config = ChunkerConfig::default();
        let chunks =
            chunker::chunk_entries(&output_entries, &project_name, &agent_name, &chunker_config);

        if !chunks.is_empty() {
            let chunks_dir = store::chunks_dir()?;
            chunker::write_chunks_to_dir(&chunks, &chunks_dir)?;
            eprintln!(
                "  Chunked: {} chunks ({}) → {}",
                chunks.len(),
                chunker::chunk_summary(&chunks),
                chunks_dir.display()
            );

            let memex_config = MemexConfig::default();
            match memex::sync_new_chunks(&chunks_dir, &memex_config) {
                Ok(result) => {
                    eprintln!(
                        "  Memex: {} pushed, {} skipped",
                        result.chunks_pushed, result.chunks_skipped,
                    );
                    for err in &result.errors {
                        eprintln!("  Memex error: {}", err);
                    }
                }
                Err(e) => eprintln!("  Memex sync failed: {}", e),
            }
        }
    }

    Ok(())
}

/// Store extracted contexts in central store and optionally sync to memex.
fn run_store(
    project: Vec<String>,
    agent: Option<String>,
    hours: u64,
    include_assistant: bool,
    sync_memex: bool,
    emit: StdoutEmit,
    redact_secrets: bool,
) -> Result<()> {
    let cutoff = Utc::now() - chrono::Duration::hours(hours as i64);

    let agents: Vec<&str> = match agent.as_deref() {
        Some("claude") => vec!["claude"],
        Some("codex") => vec!["codex"],
        Some("gemini") => vec!["gemini"],
        _ => vec!["claude", "codex", "gemini"],
    };

    let config = ExtractionConfig {
        project_filter: project.clone(),
        cutoff,
        include_assistant,
        watermark: None,
    };

    let mut all_entries = Vec::new();
    for &ag in &agents {
        let agent_entries = match ag {
            "claude" => sources::extract_claude(&config)?,
            "codex" => sources::extract_codex(&config)?,
            "gemini" => sources::extract_gemini(&config)?,
            _ => Vec::new(),
        };
        eprintln!("  [{}] {} entries", ag, agent_entries.len());
        all_entries.extend(agent_entries);
    }

    all_entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    // Filter self-echo (prevents feedback loops from aicx's own tool calls)
    let pre_echo = all_entries.len();
    all_entries.retain(|e| !ai_contexters::sanitize::is_self_echo(&e.message));
    let echo_filtered = pre_echo - all_entries.len();
    if echo_filtered > 0 {
        eprintln!("  Filtered {echo_filtered} self-echo entries");
    }

    if all_entries.is_empty() {
        eprintln!("No entries found.");
        return Ok(());
    }

    // Apply redaction in-place (single TimelineEntry type)
    if redact_secrets {
        for e in &mut all_entries {
            e.message = ai_contexters::redact::redact_secrets(&e.message);
        }
    }
    // Group by repo (from cwd) × agent × date and write chunked to central store
    let chunker_config = ai_contexters::chunker::ChunkerConfig::default();
    let mut index = store::load_index();
    let mut stored_count = 0usize;
    let mut all_written_paths: Vec<std::path::PathBuf> = Vec::new();

    let mut repo_groups: std::collections::BTreeMap<
        (String, String, String),
        Vec<output::TimelineEntry>,
    > = std::collections::BTreeMap::new();

    for entry in &all_entries {
        let repo = sources::repo_name_from_cwd(entry.cwd.as_deref(), &project);
        let date = entry.timestamp.format("%Y-%m-%d").to_string();
        repo_groups
            .entry((repo, entry.agent.clone(), date))
            .or_default()
            .push(entry.clone());
    }

    let now = Utc::now();
    let time_str = now.format("%H%M%S").to_string();

    let mut repo_summary: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, usize>,
    > = std::collections::BTreeMap::new();

    for ((repo, agent_name, date), group_entries) in &repo_groups {
        let paths = store::write_context_chunked(
            repo,
            agent_name,
            date,
            &time_str,
            group_entries,
            &chunker_config,
        )?;
        store::update_index(&mut index, repo, agent_name, date, group_entries.len());
        stored_count += group_entries.len();
        *repo_summary
            .entry(repo.clone())
            .or_default()
            .entry(agent_name.clone())
            .or_insert(0) += group_entries.len();
        all_written_paths.extend(paths);
    }

    store::save_index(&index)?;

    eprintln!(
        "✓ {} entries → {} chunks",
        stored_count,
        all_written_paths.len(),
    );
    for (repo, agents_map) in &repo_summary {
        let total: usize = agents_map.values().sum();
        let detail: Vec<String> = agents_map
            .iter()
            .map(|(a, c)| format!("{}: {}", a, c))
            .collect();
        eprintln!("  {}: {} entries ({})", repo, total, detail.join(", "));
    }

    match emit {
        StdoutEmit::Paths => {
            for path in &all_written_paths {
                println!("{}", path.display());
            }
        }
        StdoutEmit::Json => {
            let store_paths: Vec<String> = all_written_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "total_entries": stored_count,
                    "total_chunks": all_written_paths.len(),
                    "store_paths": store_paths,
                    "repos": repo_summary,
                }))?
            );
        }
        StdoutEmit::None => {}
    }

    // Chunk and sync to memex if requested
    if sync_memex && !all_entries.is_empty() {
        let agent_label = agents.join("+");
        let store_proj = if project.is_empty() {
            "_global".to_string()
        } else {
            project.join("+")
        };
        let chunker_config = ChunkerConfig::default();
        let chunks =
            chunker::chunk_entries(&all_entries, &store_proj, &agent_label, &chunker_config);

        if !chunks.is_empty() {
            let chunks_dir = store::chunks_dir()?;
            chunker::write_chunks_to_dir(&chunks, &chunks_dir)?;
            eprintln!("  Chunked: {}", chunker::chunk_summary(&chunks));

            let memex_config = MemexConfig::default();
            match memex::sync_new_chunks(&chunks_dir, &memex_config) {
                Ok(result) => {
                    eprintln!(
                        "  Memex: {} pushed, {} skipped",
                        result.chunks_pushed, result.chunks_skipped
                    );
                }
                Err(e) => eprintln!("  Memex sync failed: {}", e),
            }
        }
    }

    Ok(())
}

fn run_rank(
    hours: u64,
    project: &str,
    redact_secrets: bool,
    strict: bool,
    top: Option<usize>,
) -> Result<()> {
    // Unconditionally sync store incrementally before ranking
    let _ = run_extraction(ExtractionParams {
        agents: &["claude", "codex", "gemini"],
        project: vec![project.to_string()],
        hours,
        output_dir: None,
        format: "none",
        append_to: None,
        rotate: 0,
        incremental: true,
        include_assistant: true,
        include_loctree: false,
        project_root: None,
        sync_memex: false,
        force: false,
        conversation: false,
        redact_secrets,
        emit: StdoutEmit::None,
    });

    let project = ai_contexters::sanitize::safe_project_name(project)?;
    let base = store::store_base_dir()?;
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600);

    let mut files: Vec<PathBuf> = Vec::new();
    let proj_dir = base.join(project);

    if proj_dir.is_dir() {
        let proj_dir = ai_contexters::sanitize::validate_dir_path(&proj_dir)?;
        for date_entry in
            ai_contexters::sanitize::read_dir_validated(&proj_dir)?.filter_map(|e| e.ok())
        {
            let date_path = date_entry.path();
            if !date_path.is_dir() {
                continue;
            }
            let date_path = ai_contexters::sanitize::validate_dir_path(&date_path)?;
            for file_entry in
                ai_contexters::sanitize::read_dir_validated(&date_path)?.filter_map(|e| e.ok())
            {
                let fpath = file_entry.path();
                if fpath
                    .extension()
                    .is_some_and(|ext| ext == "md" || ext == "json")
                    && let Ok(meta) = fpath.metadata()
                    && let Ok(mtime) = meta.modified()
                    && mtime >= cutoff
                {
                    files.push(fpath);
                }
            }
        }
    }

    files.sort();

    if files.is_empty() {
        println!(
            "No context files found within last {} hours for project {}.",
            hours, project
        );
        return Ok(());
    }

    // Group by prefix (e.g. 2026-03-12/160800_codex)
    let mut bundles: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();

    for f in files {
        let parent_name = f
            .parent()
            .and_then(|p| p.file_name())
            .unwrap_or_default()
            .to_string_lossy();
        let name = f.file_name().unwrap_or_default().to_string_lossy();
        let file_prefix = name.split('-').next().unwrap_or(&name);
        let prefix = format!("{}/{}", parent_name, file_prefix);
        bundles.entry(prefix).or_default().push(f);
    }

    // Score each chunk and compute bundle averages
    struct ScoredBundle {
        key: String,
        files: Vec<(PathBuf, rank::ChunkScore)>,
        avg_score: f32,
        max_score: u8,
        total_signal: usize,
        total_lines: usize,
    }

    let mut scored_bundles: Vec<ScoredBundle> = Vec::new();

    for (key, bundle_files) in &bundles {
        let scored_files: Vec<(PathBuf, rank::ChunkScore)> = bundle_files
            .iter()
            .map(|f| (f.clone(), rank::score_chunk_file(f)))
            .collect();

        let total_score: u32 = scored_files.iter().map(|(_, s)| s.score as u32).sum();
        let avg_score = total_score as f32 / scored_files.len().max(1) as f32;
        let max_score = scored_files.iter().map(|(_, s)| s.score).max().unwrap_or(0);
        let total_signal: usize = scored_files.iter().map(|(_, s)| s.signal_lines).sum();
        let total_lines: usize = scored_files.iter().map(|(_, s)| s.total_lines).sum();

        scored_bundles.push(ScoredBundle {
            key: key.clone(),
            files: scored_files,
            avg_score,
            max_score,
            total_signal,
            total_lines,
        });
    }

    // Sort by average score descending, then by key descending (recency)
    scored_bundles.sort_by(|a, b| {
        b.avg_score
            .partial_cmp(&a.avg_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.key.cmp(&a.key))
    });

    // Apply --strict filter
    if strict {
        scored_bundles.retain(|b| b.avg_score >= 5.0);
    }

    // Apply --top limit
    if let Some(n) = top {
        scored_bundles.truncate(n);
    }

    if scored_bundles.is_empty() {
        println!(
            "No artifacts above quality threshold for {} (last {}h).",
            project, hours
        );
        return Ok(());
    }

    println!("Ranked Artifacts for {} (last {}h):", project, hours);
    println!("------------------------------------------------------------");

    for bundle in &scored_bundles {
        let label = match bundle.avg_score.round() as u8 {
            0..=2 => "NOISE",
            3..=4 => "LOW",
            5..=7 => "MEDIUM",
            _ => "HIGH",
        };
        let density = if bundle.total_lines > 0 {
            bundle.total_signal as f32 / bundle.total_lines as f32
        } else {
            0.0
        };

        println!(
            "- Bundle: {} ({} files) — Avg: {:.1}/10  Peak: {}/10  Density: {:.0}%  [{}]",
            bundle.key,
            bundle.files.len(),
            bundle.avg_score,
            bundle.max_score,
            density * 100.0,
            label,
        );

        for (path, score) in &bundle.files {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            println!(
                "    {} {} {}/10 (sig:{} noise:{} total:{})",
                match score.label {
                    "HIGH" => "+",
                    "MEDIUM" => "~",
                    "LOW" => "-",
                    _ => "x",
                },
                name,
                score.score,
                score.signal_lines,
                score.noise_lines,
                score.total_lines,
            );
        }
    }

    // Summary stats
    let total_bundles = scored_bundles.len();
    let high_count = scored_bundles.iter().filter(|b| b.avg_score >= 8.0).count();
    let medium_count = scored_bundles
        .iter()
        .filter(|b| b.avg_score >= 5.0 && b.avg_score < 8.0)
        .count();
    let low_count = scored_bundles
        .iter()
        .filter(|b| b.avg_score >= 3.0 && b.avg_score < 5.0)
        .count();
    let noise_count = scored_bundles.iter().filter(|b| b.avg_score < 3.0).count();

    println!("------------------------------------------------------------");
    println!(
        "Summary: {} bundles — HIGH: {} | MEDIUM: {} | LOW: {} | NOISE: {}",
        total_bundles, high_count, medium_count, low_count, noise_count,
    );

    Ok(())
}

fn is_noise_artifact(path: &std::path::Path) -> bool {
    if !path.is_file() || path.extension().is_none_or(|ext| ext != "md") {
        return false;
    }
    let Ok(content) = ai_contexters::sanitize::read_to_string_validated(path) else {
        return false;
    };

    let lines: Vec<&str> = content.lines().collect();
    if lines.len() >= 15 {
        return false; // Not short enough to be considered noise
    }

    // Check if it's task-notification only
    let mut is_noise = true;
    for line in &lines {
        let l = line.trim().to_lowercase();
        if l.is_empty()
            || l.starts_with("[project:")
            || l.starts_with("[signals")
            || l.starts_with("[/signals")
            || l.starts_with("-") // checklist/signals
            || (l.starts_with("[") && l.contains("] ") && l.contains("tool:")) // e.g. [14:30:00] assistant: Tool: ...
            || l.contains("task-notification")
            || l.contains("background command")
            || l.contains("task killed")
            || l.contains("task update")
            || l.contains("ran command")
            || l.contains("ran find")
            || l.contains("called loctree")
            || l.contains("killed process")
        {
            continue;
        } else {
            // Found some actual signal line that is not a known noise pattern
            is_noise = false;
            break;
        }
    }

    is_noise
}

/// List context files from the global store, filtered by recency.
fn run_refs(hours: u64, project: Option<String>, emit: RefsEmit, strict: bool) -> Result<()> {
    let base = ai_contexters::sanitize::validate_dir_path(&store::store_base_dir()?)?;

    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600);

    let mut files: Vec<PathBuf> = Vec::new();

    let project_dirs: Vec<_> = if let Some(ref p) = project {
        let safe_project = ai_contexters::sanitize::safe_project_name(p)?;
        let d = base.join(safe_project);
        if d.is_dir() {
            vec![ai_contexters::sanitize::validate_dir_path(&d)?]
        } else {
            vec![]
        }
    } else {
        ai_contexters::sanitize::read_dir_validated(&base)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "memex"))
            .filter_map(|path| ai_contexters::sanitize::validate_dir_path(&path).ok())
            .collect()
    };

    for proj_dir in project_dirs {
        for date_entry in
            ai_contexters::sanitize::read_dir_validated(&proj_dir)?.filter_map(|e| e.ok())
        {
            let date_path = date_entry.path();
            if !date_path.is_dir() {
                continue;
            }
            let date_path = ai_contexters::sanitize::validate_dir_path(&date_path)?;
            for file_entry in
                ai_contexters::sanitize::read_dir_validated(&date_path)?.filter_map(|e| e.ok())
            {
                let fpath = file_entry.path();
                if fpath
                    .extension()
                    .is_some_and(|ext| ext == "md" || ext == "json")
                    && let Ok(meta) = fpath.metadata()
                    && let Ok(mtime) = meta.modified()
                    && mtime >= cutoff
                {
                    if strict && is_noise_artifact(&fpath) {
                        continue;
                    }
                    files.push(fpath);
                }
            }
        }
    }

    files.sort();

    if files.is_empty() {
        eprintln!("No context files found within last {} hours.", hours);
    } else {
        match emit {
            RefsEmit::Summary => print_refs_summary(&files)?,
            RefsEmit::Paths => {
                let stdout = io::stdout();
                let mut out = io::BufWriter::new(stdout.lock());
                for f in &files {
                    if let Err(err) = writeln!(out, "{}", f.display()) {
                        if err.kind() == io::ErrorKind::BrokenPipe {
                            return Ok(());
                        }
                        return Err(err.into());
                    }
                }
                if let Err(err) = out.flush() {
                    if err.kind() == io::ErrorKind::BrokenPipe {
                        return Ok(());
                    }
                    return Err(err.into());
                }
                if io::stderr().is_terminal() {
                    eprintln!("({} files)", files.len());
                }
            }
        }
    }

    Ok(())
}

#[derive(Default)]
struct RefsAgentSummary {
    files: usize,
    days: BTreeSet<String>,
}

#[derive(Default)]
struct RefsProjectSummary {
    total_files: usize,
    min_date: Option<String>,
    max_date: Option<String>,
    latest: Option<String>,
    agents: BTreeMap<String, RefsAgentSummary>,
}

fn extract_agent_from_filename(path: &Path) -> String {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return "unknown".to_string();
    };
    let Some((_, tail)) = stem.split_once('_') else {
        return "unknown".to_string();
    };
    let agent = tail
        .split_once('-')
        .map(|(a, _)| a)
        .filter(|a| !a.is_empty())
        .unwrap_or(tail);
    if agent.is_empty() {
        "unknown".to_string()
    } else {
        agent.to_ascii_lowercase()
    }
}

fn print_refs_summary(files: &[PathBuf]) -> Result<()> {
    let mut by_project: BTreeMap<String, RefsProjectSummary> = BTreeMap::new();

    for path in files {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown-file")
            .to_string();
        let date = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown-date")
            .to_string();
        let project = path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("_unknown")
            .to_string();
        let latest_rel = format!("{}/{}", date, file_name);
        let agent = extract_agent_from_filename(path);

        let project_summary = by_project.entry(project).or_default();
        project_summary.total_files += 1;

        if project_summary
            .min_date
            .as_ref()
            .is_none_or(|min_date| &date < min_date)
        {
            project_summary.min_date = Some(date.clone());
        }
        if project_summary
            .max_date
            .as_ref()
            .is_none_or(|max_date| &date > max_date)
        {
            project_summary.max_date = Some(date.clone());
        }
        if project_summary
            .latest
            .as_ref()
            .is_none_or(|latest| &latest_rel > latest)
        {
            project_summary.latest = Some(latest_rel);
        }

        let agent_summary = project_summary.agents.entry(agent).or_default();
        agent_summary.files += 1;
        agent_summary.days.insert(date);
    }

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());

    for (project, summary) in &by_project {
        let date_range = match (&summary.min_date, &summary.max_date) {
            (Some(min), Some(max)) => format!("{min} .. {max}"),
            _ => "unknown".to_string(),
        };

        let agent_details = summary
            .agents
            .iter()
            .map(|(agent, data)| format!("{agent}: {} files/{} days", data.files, data.days.len()))
            .collect::<Vec<_>>()
            .join(", ");

        let latest = summary.latest.as_deref().unwrap_or("unknown");

        if let Err(err) = writeln!(
            out,
            "{}: {} files ({}) [{}] latest: {}",
            project, summary.total_files, date_range, agent_details, latest
        ) {
            if err.kind() == io::ErrorKind::BrokenPipe {
                return Ok(());
            }
            return Err(err.into());
        }
    }

    if let Err(err) = out.flush() {
        if err.kind() == io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(err.into());
    }

    Ok(())
}

/// Manage dedup state.
fn run_state(reset: bool, project: Option<String>, info: bool) -> Result<()> {
    let mut state = StateManager::load();

    if info {
        eprintln!("=== State Info ===");
        eprintln!("  Total hashes: {}", state.total_hashes());
        eprintln!("  Projects: {}", state.seen_hashes.len());
        for (proj, set) in &state.seen_hashes {
            eprintln!("    {}: {} hashes", proj, set.len());
        }
        eprintln!("  Watermarks: {}", state.last_processed.len());
        for (src, ts) in &state.last_processed {
            eprintln!("    {}: {}", src, ts);
        }
        eprintln!("  Runs: {}", state.runs.len());
        return Ok(());
    }

    if reset {
        if let Some(ref p) = project {
            state.reset_project(p);
            state.save()?;
            eprintln!("Reset hashes for project: {}", p);
        } else {
            state.reset_all();
            state.save()?;
            eprintln!("Reset all dedup hashes.");
        }
        return Ok(());
    }

    eprintln!("Use --info to show state or --reset to clear. See --help.");
    Ok(())
}

/// Sync stored chunks to rmcp-memex vector memory.
fn run_memex_sync(namespace: &str, per_chunk: bool, db_path: Option<PathBuf>) -> Result<()> {
    if !memex::check_memex_available() {
        eprintln!("Error: rmcp-memex not found in PATH.");
        eprintln!("Install with: cargo install rmcp-memex");
        std::process::exit(1);
    }

    let chunks_dir = store::chunks_dir()?;
    if !chunks_dir.exists() {
        eprintln!("No chunks directory found at: {}", chunks_dir.display());
        eprintln!("Run `aicx store --memex` first to generate chunks.");
        return Ok(());
    }

    let config = MemexConfig {
        namespace: namespace.to_string(),
        db_path,
        batch_mode: !per_chunk,
    };

    eprintln!("Syncing chunks from: {}", chunks_dir.display());
    eprintln!("  Namespace: {}", config.namespace);
    eprintln!(
        "  Mode: {}",
        if config.batch_mode {
            "batch"
        } else {
            "per-chunk"
        }
    );

    let result = memex::sync_new_chunks(&chunks_dir, &config)?;

    eprintln!(
        "✓ Memex sync: {} pushed, {} skipped",
        result.chunks_pushed, result.chunks_skipped,
    );

    for err in &result.errors {
        eprintln!("  Error: {}", err);
    }

    Ok(())
}

/// Build and write an AI context dashboard HTML file.
struct DashboardServerRunArgs {
    store_root: Option<PathBuf>,
    host: String,
    port: u16,
    artifact: PathBuf,
    title: String,
    preview_chars: usize,
}

/// Run dashboard server mode with artifact regeneration endpoints.
fn run_dashboard_server(args: DashboardServerRunArgs) -> Result<()> {
    let root = if let Some(path) = args.store_root {
        path
    } else {
        store::store_base_dir()?
    };
    let host: std::net::IpAddr = args.host.parse().with_context(|| {
        format!(
            "Invalid --host IP address '{}'. Example valid value: 127.0.0.1",
            args.host
        )
    })?;
    if !host.is_loopback() {
        return Err(anyhow::anyhow!(
            "Refusing non-loopback --host '{}'. Dashboard server is local-only for safety.",
            host
        ));
    }
    let artifact_path = ai_contexters::sanitize::validate_write_path(&args.artifact)?;

    let config = DashboardServerConfig {
        store_root: root,
        title: args.title,
        preview_chars: args.preview_chars,
        artifact_path,
        host,
        port: args.port,
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create tokio runtime for dashboard server")?;

    runtime.block_on(dashboard_server::run_dashboard_server(config))
}

/// Build and write an AI context dashboard HTML file.
struct DashboardRunArgs {
    store_root: Option<PathBuf>,
    output: PathBuf,
    title: String,
    preview_chars: usize,
}

/// Build and write an AI context dashboard HTML file.
fn run_dashboard(args: DashboardRunArgs) -> Result<()> {
    let root = if let Some(path) = args.store_root {
        path
    } else {
        store::store_base_dir()?
    };

    let config = DashboardConfig {
        store_root: root.clone(),
        title: args.title,
        preview_chars: args.preview_chars,
    };

    let artifact = dashboard::build_dashboard(&config)?;

    let mut output_path = ai_contexters::sanitize::validate_write_path(&args.output)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output directory: {}", parent.display()))?;
    }
    output_path = ai_contexters::sanitize::validate_write_path(&output_path)?;
    fs::write(&output_path, artifact.html)
        .with_context(|| format!("Failed to write dashboard: {}", output_path.display()))?;

    eprintln!("✓ Dashboard generated");
    eprintln!("  Output: {}", output_path.display());
    eprintln!("  Store: {}", root.display());
    eprintln!(
        "  Stats: {} projects, {} days, {} files, {} agents",
        artifact.stats.total_projects,
        artifact.stats.total_days,
        artifact.stats.total_files,
        artifact.stats.agents_detected
    );
    eprintln!("  Backend: {}", artifact.stats.search_backend);
    eprintln!(
        "  Estimated timeline entries: {}",
        artifact.stats.total_entries_estimate
    );
    if !artifact.assumptions.is_empty() {
        eprintln!("  Assumptions:");
        for assumption in artifact.assumptions.iter().take(8) {
            eprintln!("    - {}", assumption);
        }
    }

    println!("{}", output_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{FileTime, set_file_mtime};
    use std::fs;

    fn unique_test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aicx-main-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn set_mtime(path: &Path, unix_seconds: i64) {
        set_file_mtime(path, FileTime::from_unix_time(unix_seconds, 0)).unwrap();
    }

    #[test]
    fn claude_defaults_to_silent_stdout() {
        let cli = Cli::try_parse_from(["aicx", "claude"]).expect("claude command should parse");

        match cli.command {
            Some(Commands::Claude { emit, .. }) => {
                assert!(matches!(emit, StdoutEmit::None));
            }
            _ => panic!("expected claude command"),
        }
    }

    #[test]
    fn codex_defaults_to_silent_stdout() {
        let cli = Cli::try_parse_from(["aicx", "codex"]).expect("codex command should parse");

        match cli.command {
            Some(Commands::Codex { emit, .. }) => {
                assert!(matches!(emit, StdoutEmit::None));
            }
            _ => panic!("expected codex command"),
        }
    }

    #[test]
    fn all_defaults_to_silent_stdout() {
        let cli = Cli::try_parse_from(["aicx", "all"]).expect("all command should parse");

        match cli.command {
            Some(Commands::All { emit, .. }) => {
                assert!(matches!(emit, StdoutEmit::None));
            }
            _ => panic!("expected all command"),
        }
    }

    #[test]
    fn store_defaults_to_silent_stdout() {
        let cli = Cli::try_parse_from(["aicx", "store"]).expect("store command should parse");

        match cli.command {
            Some(Commands::Store { emit, .. }) => {
                assert!(matches!(emit, StdoutEmit::None));
            }
            other => panic!("expected store command, got {:?}", other.map(|_| "other")),
        }
    }

    #[test]
    fn store_accepts_explicit_paths_emit() {
        let cli = Cli::try_parse_from(["aicx", "store", "--emit", "paths"])
            .expect("store command with explicit emit should parse");

        match cli.command {
            Some(Commands::Store { emit, .. }) => {
                assert!(matches!(emit, StdoutEmit::Paths));
            }
            other => panic!("expected store command, got {:?}", other.map(|_| "other")),
        }
    }

    #[test]
    fn refs_default_to_summary_stdout() {
        let cli = Cli::try_parse_from(["aicx", "refs"]).expect("refs command should parse");

        match cli.command {
            Some(Commands::Refs { emit, .. }) => {
                assert!(matches!(emit, RefsEmit::Summary));
            }
            _ => panic!("expected refs command"),
        }
    }

    #[test]
    fn refs_accept_explicit_paths_emit() {
        let cli = Cli::try_parse_from(["aicx", "refs", "--emit", "paths"])
            .expect("refs command with explicit emit should parse");

        match cli.command {
            Some(Commands::Refs { emit, .. }) => {
                assert!(matches!(emit, RefsEmit::Paths));
            }
            _ => panic!("expected refs command"),
        }
    }

    #[test]
    fn extract_accepts_gemini_antigravity_format() {
        let cli = Cli::try_parse_from([
            "aicx",
            "extract",
            "--format",
            "gemini-antigravity",
            "/tmp/brain/uuid",
            "-o",
            "/tmp/report.md",
        ])
        .expect("extract command with gemini-antigravity should parse");

        match cli.command {
            Some(Commands::Extract { format, .. }) => {
                assert!(matches!(format, ExtractInputFormat::GeminiAntigravity));
            }
            _ => panic!("expected extract command"),
        }
    }

    #[test]
    fn run_extract_file_uses_repo_identity_over_file_provenance() {
        let root = unique_test_dir("extract-repo-identity");
        let brain = root.join("brain").join("conv-9");
        let step_output = brain
            .join(".system_generated")
            .join("steps")
            .join("001")
            .join("output.txt");
        let report = root.join("report.md");

        write_file(
            &step_output,
            r#"{"project":"/Users/tester/workspace/RepoDelta","decision":"Group by repo identity."}"#,
        );
        set_mtime(&step_output, 1_706_745_900);

        run_extract_file(
            ExtractInputFormat::GeminiAntigravity,
            None,
            brain,
            report.clone(),
            true,
            0,
            false,
            false,
        )
        .unwrap();

        let output = fs::read_to_string(&report).unwrap();
        assert!(output.contains("| Filter | RepoDelta |"));
        assert!(output.contains("Gemini Antigravity recovery report"));
        assert!(!output.contains("| Filter | file:"));

        let _ = fs::remove_dir_all(&root);
    }
}
