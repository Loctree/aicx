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
use clap::{ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use ai_contexters::dashboard::{self, DashboardConfig};
use ai_contexters::dashboard_server::{self, DashboardServerConfig};
use ai_contexters::intents;
use ai_contexters::memex::{self, MemexConfig};
use ai_contexters::output::{self, OutputConfig, OutputFormat, OutputMode, ReportMetadata};
use ai_contexters::rank;
use ai_contexters::sources::{self, ExtractionConfig};
use ai_contexters::state::StateManager;
use ai_contexters::store;

/// AI Contexters - timeline and decisions from AI sessions
#[derive(Debug, Parser)]
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

#[derive(Debug, Subcommand)]
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

    /// Store contexts in central store (~/.aicx/) and optionally sync to memex
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

    /// Sync stored chunks to rmcp-memex semantic index
    MemexSync {
        /// Namespace in the semantic index
        #[arg(short, long, default_value = "ai-contexts")]
        namespace: String,

        /// Use per-chunk upsert instead of batch import
        #[arg(long)]
        per_chunk: bool,

        /// Override LanceDB path
        #[arg(long)]
        db_path: Option<PathBuf>,

        /// Explicitly wipe the current rmcp-memex store + BM25 index before rebuilding.
        /// Use this when the embedding model or dimension changed.
        #[arg(long)]
        reindex: bool,
    },

    /// List available projects/sessions
    List,

    /// List context files from global store (references)
    Refs {
        /// Hours to look back (filter by canonical chunk date)
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
        /// Store root directory (default: ~/.aicx)
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

    /// Run dashboard HTTP server with server-shell UI and on-demand data regeneration.
    DashboardServe {
        /// Store root directory (default: ~/.aicx)
        #[arg(long)]
        store_root: Option<PathBuf>,

        /// Bind host IP address (example: 127.0.0.1)
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Bind TCP port
        #[arg(long, default_value = "8033")]
        port: u16,

        /// Legacy compatibility path retained for status surfaces; not written in server mode
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

    #[command(
        about = "Retired compatibility shim; prints migration guidance",
        long_about = "aicx init has been retired.\n\nContext initialisation is now handled by /vc-init inside Claude Code.\nSee: https://vibecrafted.io/\n\nLegacy flags are still accepted for compatibility, but they have no effect."
    )]
    Init {
        /// Project name override
        #[arg(short, long, hide = true)]
        project: Option<String>,

        /// Agent override: claude or codex
        #[arg(short, long, hide = true)]
        agent: Option<String>,

        /// Model override (optional; if omitted uses agent default)
        #[arg(long, hide = true)]
        model: Option<String>,

        /// Hours to look back for context (default: 4800)
        #[arg(short = 'H', long, default_value = "4800", hide = true)]
        hours: u64,

        /// Maximum lines per context section in the prompt
        #[arg(long, default_value = "1200", hide = true)]
        max_lines: usize,

        /// Only include user messages in context (exclude assistant + reasoning)
        #[arg(long, hide = true)]
        user_only: bool,

        /// Include assistant messages (legacy flag; now default)
        #[arg(long, hide = true, conflicts_with = "user_only")]
        include_assistant: bool,

        /// Action focus appended to the prompt
        #[arg(long, hide = true)]
        action: Option<String>,

        /// Additional agent prompt appended after core rules (verbatim)
        #[arg(long, hide = true)]
        agent_prompt: Option<String>,

        /// Read additional agent prompt from a file (verbatim)
        #[arg(long, hide = true)]
        agent_prompt_file: Option<PathBuf>,

        /// Build context/prompt only, do not run an agent
        #[arg(long, hide = true)]
        no_run: bool,

        /// Skip "Run? (y)es / (n)o" confirmation
        #[arg(long, hide = true)]
        no_confirm: bool,

        /// Do not auto-modify `.gitignore`
        #[arg(long, hide = true)]
        no_gitignore: bool,
    },

    /// Ad-hoc terminal fuzzy search across the aicx store (no dashboard needed)
    Search {
        /// Search query string
        query: String,

        /// Project filter (org/repo substring, case-insensitive)
        #[arg(short, long)]
        project: Option<String>,

        /// Hours to look back (0 = all time)
        #[arg(short = 'H', long, default_value = "0")]
        hours: u64,

        /// Filter by date: single day (2026-03-28), range (2026-03-20..2026-03-28),
        /// or open-ended (2026-03-20.. or ..2026-03-28)
        #[arg(short, long)]
        date: Option<String>,

        /// Maximum results to return
        #[arg(short, long, default_value = "10")]
        limit: usize,

        /// Minimum score threshold (0-100)
        #[arg(short, long, value_parser = clap::value_parser!(u8).range(0..=100))]
        score: Option<u8>,

        /// Emit compact JSON instead of plain text
        #[arg(short = 'j', long)]
        json: bool,
    },

    /// Retrieve chunks by steering metadata (frontmatter fields).
    ///
    /// Filters by run_id, prompt_id, agent, kind, project, and/or date range
    /// using sidecar metadata — no filesystem grep needed.
    ///
    /// Example:
    ///   aicx steer --run-id mrbl-001
    ///   aicx steer --project ai-contexters --kind reports --date 2026-03-28
    Steer {
        /// Filter by run_id (exact match)
        #[arg(long)]
        run_id: Option<String>,

        /// Filter by prompt_id (exact match)
        #[arg(long)]
        prompt_id: Option<String>,

        /// Filter by agent: claude, codex, gemini
        #[arg(short, long)]
        agent: Option<String>,

        /// Filter by kind: conversations, plans, reports, other
        #[arg(short, long)]
        kind: Option<String>,

        /// Filter by project (case-insensitive substring)
        #[arg(short, long)]
        project: Option<String>,

        /// Filter by date: single day (2026-03-28), range (2026-03-20..2026-03-28),
        /// or open-ended (2026-03-20.. or ..2026-03-28)
        #[arg(short, long)]
        date: Option<String>,

        /// Maximum results
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },

    /// Truthfully rebuild legacy contexts into canonical AICX store or salvage them under legacy-store
    Migrate {
        /// Dry run: show what would be moved without modifying files
        #[arg(long)]
        dry_run: bool,

        /// Override legacy input store root (default: ~/.ai-contexters)
        #[arg(long)]
        legacy_root: Option<PathBuf>,

        /// Override AICX store root (default: ~/.aicx)
        #[arg(long)]
        store_root: Option<PathBuf>,
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
            reindex,
        }) => {
            run_memex_sync(&namespace, per_chunk, db_path, reindex)?;
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
        Some(Commands::Init { .. }) => {
            eprintln!("aicx init has been retired.");
            eprintln!("Context initialisation is now handled by /vc-init inside Claude Code.");
            eprintln!("See: https://vibecrafted.io/");
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
        Some(Commands::Search {
            query,
            project,
            hours,
            date,
            limit,
            score,
            json,
        }) => {
            run_search(
                &query,
                project.as_deref(),
                hours,
                date.as_deref(),
                limit,
                score,
                json,
            )?;
        }
        Some(Commands::Steer {
            run_id,
            prompt_id,
            agent,
            kind,
            project,
            date,
            limit,
        }) => {
            run_steer(
                run_id.as_deref(),
                prompt_id.as_deref(),
                agent.as_deref(),
                kind.as_deref(),
                project.as_deref(),
                date.as_deref(),
                limit,
            )?;
        }
        Some(Commands::Migrate {
            dry_run,
            legacy_root,
            store_root,
        }) => {
            ai_contexters::store::run_migration_with_paths(dry_run, legacy_root, store_root)?;
        }
        None => {
            Cli::command().print_help()?;
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

fn sync_memex_if_requested(sync_memex: bool, all_written_paths: &[PathBuf]) -> Result<()> {
    if sync_memex && !all_written_paths.is_empty() {
        let memex_config = MemexConfig::default();
        // Keep extractor/store `--memex` on the same stateful transport seam as
        // the dedicated `memex-sync` command so sync state and observability do
        // not drift between code paths.
        let result = memex::sync_new_chunk_paths(all_written_paths, &memex_config)
            .context("Failed to sync canonical chunks to external dependency rmcp-memex")?;
        eprintln!(
            "  Memex: {} pushed, {} skipped",
            result.chunks_pushed, result.chunks_skipped
        );
        for err in &result.errors {
            eprintln!("  Memex error: {}", err);
        }
    }
    Ok(())
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

    let chunker_config = ai_contexters::chunker::ChunkerConfig::default();
    let mut all_written_paths: Vec<std::path::PathBuf> = Vec::new();

    if !output_entries.is_empty() {
        let store_summary = store::store_semantic_segments(&output_entries, &chunker_config)?;
        let newly_written_paths = store_summary.written_paths.clone();
        all_written_paths.extend(newly_written_paths.iter().cloned());

        // Update fast local metadata index
        if let Ok(rt) = tokio::runtime::Runtime::new() {
            let path_refs: Vec<&PathBuf> = newly_written_paths.iter().collect();
            let _ = rt.block_on(ai_contexters::steer_index::sync_steer_index(&path_refs));
        }

        // Summary to stderr (diagnostics)
        eprintln!(
            "✓ {} entries → {} chunks",
            output_entries.len(),
            all_written_paths.len(),
        );
        for (repo, agents_map) in &store_summary.project_summary {
            let total: usize = agents_map.values().sum();
            let detail: Vec<String> = agents_map
                .iter()
                .map(|(a, c)| format!("{}: {}", a, c))
                .collect();
            eprintln!("  {}: {} entries ({})", repo, total, detail.join(", "));
        }

        sync_memex_if_requested(sync_memex, &newly_written_paths)?;
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
    let chunker_config = ai_contexters::chunker::ChunkerConfig::default();
    let stderr_is_tty = io::stderr().is_terminal();
    let mut progress_width = 0usize;
    let store_result = if stderr_is_tty {
        store::store_semantic_segments_with_progress(
            &all_entries,
            &chunker_config,
            |done, total| {
                let message = format!("  Chunking... {done}/{total} segments");
                let width = progress_width.max(message.len());
                progress_width = width;
                eprint!("\r{message:<width$}");
                let _ = io::stderr().flush();
            },
        )
    } else {
        store::store_semantic_segments(&all_entries, &chunker_config)
    };
    if stderr_is_tty && progress_width > 0 {
        eprint!("\r{:<width$}\r", "", width = progress_width);
        let _ = io::stderr().flush();
    }
    let store_summary = store_result?;
    let stored_count = store_summary.total_entries;
    let all_written_paths = store_summary.written_paths.clone();

    // Update fast local metadata index
    if let Ok(rt) = tokio::runtime::Runtime::new() {
        let path_refs: Vec<&PathBuf> = all_written_paths.iter().collect();
        let _ = rt.block_on(ai_contexters::steer_index::sync_steer_index(&path_refs));
    }

    eprintln!(
        "✓ {} entries → {} chunks",
        stored_count,
        all_written_paths.len(),
    );
    for (repo, agents_map) in &store_summary.project_summary {
        let total: usize = agents_map.values().sum();
        let detail: Vec<String> = agents_map
            .iter()
            .map(|(a, c)| format!("{}: {}", a, c))
            .collect();
        eprintln!("  {}: {} entries ({})", repo, total, detail.join(", "));
    }

    sync_memex_if_requested(sync_memex, &all_written_paths)?;

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
                    "repos": store_summary.project_summary,
                }))?
            );
        }
        StdoutEmit::None => {}
    }

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

/// Month names → number, supports English + Polish.
fn month_number(s: &str) -> Option<u32> {
    match s {
        "january" | "jan" | "styczen" | "stycznia" | "styczeń" => Some(1),
        "february" | "feb" | "luty" | "lutego" => Some(2),
        "march" | "mar" | "marzec" | "marca" => Some(3),
        "april" | "apr" | "kwiecien" | "kwietnia" | "kwiecień" => Some(4),
        "may" | "maj" | "maja" => Some(5),
        "june" | "jun" | "czerwiec" | "czerwca" => Some(6),
        "july" | "jul" | "lipiec" | "lipca" => Some(7),
        "august" | "aug" | "sierpien" | "sierpnia" | "sierpień" => Some(8),
        "september" | "sep" | "wrzesien" | "września" | "wrzesień" => Some(9),
        "october" | "oct" | "pazdziernik" | "października" | "październik" => Some(10),
        "november" | "nov" | "listopad" | "listopada" => Some(11),
        "december" | "dec" | "grudzien" | "grudnia" | "grudzień" => Some(12),
        _ => None,
    }
}

/// Extract inline date hints from query, returning (cleaned_query, Option<date_filter>).
/// Recognises: "january 2026", "march 2026", "2026-03", "2026-01-15".
fn extract_date_from_query(query: &str) -> (String, Option<String>) {
    let words: Vec<&str> = query.split_whitespace().collect();
    let lower: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let mut used = vec![false; words.len()];
    let mut date_filter: Option<String> = None;

    // Pattern 1: "<month> <year>" e.g. "january 2026"
    for i in 0..words.len().saturating_sub(1) {
        if let Some(m) = month_number(&lower[i]) {
            if let Ok(y) = lower[i + 1].parse::<u32>() {
                if (2020..=2099).contains(&y) {
                    let days = days_in_month(y, m);
                    let lo = format!("{y:04}-{m:02}-01");
                    let hi = format!("{y:04}-{m:02}-{days:02}");
                    date_filter = Some(format!("{lo}..{hi}"));
                    used[i] = true;
                    used[i + 1] = true;
                }
            }
        }
    }

    // Pattern 2: "<year> <month>" e.g. "2026 january"
    if date_filter.is_none() {
        for i in 0..words.len().saturating_sub(1) {
            if let Ok(y) = lower[i].parse::<u32>() {
                if (2020..=2099).contains(&y) {
                    if let Some(m) = month_number(&lower[i + 1]) {
                        let days = days_in_month(y, m);
                        let lo = format!("{y:04}-{m:02}-01");
                        let hi = format!("{y:04}-{m:02}-{days:02}");
                        date_filter = Some(format!("{lo}..{hi}"));
                        used[i] = true;
                        used[i + 1] = true;
                    }
                }
            }
        }
    }

    // Pattern 3: YYYY-MM (no day) e.g. "2026-01"
    if date_filter.is_none() {
        let re_ym = regex::Regex::new(r"^(\d{4})-(\d{2})$").unwrap();
        for (i, w) in lower.iter().enumerate() {
            if let Some(caps) = re_ym.captures(w) {
                let y: u32 = caps[1].parse().unwrap();
                let m: u32 = caps[2].parse().unwrap();
                if (1..=12).contains(&m) {
                    let days = days_in_month(y, m);
                    let lo = format!("{y:04}-{m:02}-01");
                    let hi = format!("{y:04}-{m:02}-{days:02}");
                    date_filter = Some(format!("{lo}..{hi}"));
                    used[i] = true;
                }
            }
        }
    }

    // Pattern 4: full ISO date YYYY-MM-DD → single day
    if date_filter.is_none() {
        let re_ymd = regex::Regex::new(r"^(\d{4}-\d{2}-\d{2})$").unwrap();
        for (i, w) in lower.iter().enumerate() {
            if re_ymd.is_match(w) {
                date_filter = Some(w.clone());
                used[i] = true;
            }
        }
    }

    let cleaned: Vec<&str> = words
        .iter()
        .enumerate()
        .filter(|(i, _)| !used[*i])
        .map(|(_, w)| *w)
        .collect();

    (cleaned.join(" "), date_filter)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Parse a date filter string into (Option<start>, Option<end>) inclusive bounds.
/// Formats: "2026-03-28", "2026-03-20..2026-03-28", "2026-03-20..", "..2026-03-28"
fn parse_date_filter(s: &str) -> Result<(Option<String>, Option<String>)> {
    if let Some((left, right)) = s.split_once("..") {
        let lo = if left.is_empty() {
            None
        } else {
            Some(left.to_string())
        };
        let hi = if right.is_empty() {
            None
        } else {
            Some(right.to_string())
        };
        Ok((lo, hi))
    } else {
        // single day
        Ok((Some(s.to_string()), Some(s.to_string())))
    }
}

/// Ad-hoc terminal fuzzy search across the aicx store.
fn run_search(
    query: &str,
    project: Option<&str>,
    hours: u64,
    date: Option<&str>,
    limit: usize,
    score: Option<u8>,
    json: bool,
) -> Result<()> {
    // Extract inline date hints from query if no explicit --date given
    let (effective_query, inline_date) = if date.is_none() {
        extract_date_from_query(query)
    } else {
        (query.to_string(), None)
    };
    let effective_date = date.map(String::from).or(inline_date);
    let search_query = if effective_date.is_some() && effective_query.is_empty() {
        // date-only query: match everything, rely on date filter
        "*".to_string()
    } else if !effective_query.is_empty() {
        effective_query
    } else {
        query.to_string()
    };

    let root = store::store_base_dir()?;
    // Fetch more results pre-filter so score/date/hours filtering has material to work with.
    let fetch_limit = if effective_date.is_some() || score.is_some() || hours > 0 {
        limit.saturating_mul(5).max(50)
    } else {
        limit
    };

    // Try fast search with rmcp_memex first (instant), fallback to brute-force if it fails or returns nothing
    let (results, scanned) = if let Ok(rt) = tokio::runtime::Runtime::new() {
        match rt.block_on(memex::fast_memex_search(
            &search_query,
            fetch_limit,
            project,
        )) {
            Ok((res, scan)) if !res.is_empty() => (res, scan),
            Err(err) if memex::is_compatibility_error(&err) => return Err(err),
            _ => rank::fuzzy_search_store(&root, &search_query, fetch_limit, project)?,
        }
    } else {
        rank::fuzzy_search_store(&root, &search_query, fetch_limit, project)?
    };

    let mut results = results;

    if let Some(min_score) = score {
        results.retain(|r| r.score >= min_score);
    }

    // Apply date filter (day granularity) — takes priority over hours.
    let results: Vec<_> = if let Some(ref d) = effective_date {
        let (lo, hi) = parse_date_filter(d)?;
        results
            .into_iter()
            .filter(|r| {
                lo.as_ref().is_none_or(|lo| r.date.as_str() >= lo.as_str())
                    && hi.as_ref().is_none_or(|hi| r.date.as_str() <= hi.as_str())
            })
            .collect()
    } else if hours > 0 {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
        let cutoff_date = cutoff.format("%Y-%m-%d").to_string();
        results
            .into_iter()
            .filter(|r| r.date >= cutoff_date)
            .collect()
    } else {
        results
    };
    // Truncate to requested limit after date filtering
    let results: Vec<_> = results.into_iter().take(limit).collect();

    if json {
        println!("{}", rank::render_search_json(&results, scanned)?);
        return Ok(());
    }

    if results.is_empty() {
        eprintln!("No matches for {:?} (scanned {} chunks).", query, scanned);
        return Ok(());
    }

    print!(
        "{}",
        rank::render_search_text(&results, io::stdout().is_terminal())
    );
    let _ = io::stdout().flush();

    if io::stderr().is_terminal() {
        eprintln!(
            "\n{} result(s) from {} scanned chunks.",
            results.len(),
            scanned
        );
    }
    Ok(())
}

/// Retrieve chunks by steering metadata (frontmatter sidecar fields).
fn run_steer(
    run_id: Option<&str>,
    prompt_id: Option<&str>,
    agent: Option<&str>,
    kind: Option<&str>,
    project: Option<&str>,
    date: Option<&str>,
    limit: usize,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;

    let (date_lo, date_hi) = if let Some(d) = date {
        let bounds = parse_date_filter(d)?;
        (bounds.0, bounds.1)
    } else {
        (None, None)
    };

    let metadatas = rt.block_on(ai_contexters::steer_index::search_steer_index(
        run_id,
        prompt_id,
        agent,
        kind,
        project,
        date_lo.as_deref(),
        date_hi.as_deref(),
        limit,
    ))?;

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let color = stdout.is_terminal();
    let matched = metadatas.len();

    for meta in metadatas {
        let path = meta.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        let p = meta.get("project").and_then(|v| v.as_str()).unwrap_or("?");
        let a = meta.get("agent").and_then(|v| v.as_str()).unwrap_or("?");
        let d = meta.get("date").and_then(|v| v.as_str()).unwrap_or("?");
        let k = meta.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
        let run_str = meta.get("run_id").and_then(|v| v.as_str()).unwrap_or("-");
        let prompt_str = meta
            .get("prompt_id")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let model_str = meta
            .get("agent_model")
            .and_then(|v| v.as_str())
            .unwrap_or("-");

        if color {
            let _ = writeln!(
                out,
                "\x1b[1;36m{}\x1b[0m | \x1b[35m{}\x1b[0m | \x1b[90m{}\x1b[0m | {}",
                p, a, d, k
            );
            let _ = writeln!(
                out,
                "  run_id: \x1b[33m{run_str}\x1b[0m  prompt_id: \x1b[33m{prompt_str}\x1b[0m  model: \x1b[90m{model_str}\x1b[0m"
            );
            let _ = writeln!(out, "  \x1b[90;4m{}\x1b[0m", path);
            let _ = writeln!(out);
        } else {
            let _ = writeln!(out, "{} | {} | {} | {}", p, a, d, k);
            let _ = writeln!(
                out,
                "  run_id: {run_str}  prompt_id: {prompt_str}  model: {model_str}"
            );
            let _ = writeln!(out, "  {}", path);
            let _ = writeln!(out);
        }
    }

    let _ = out.flush();
    if io::stderr().is_terminal() {
        eprintln!("{matched} match(es) from steer index.");
    }

    Ok(())
}

/// List context files from the global store, filtered by recency.
fn run_refs(hours: u64, project: Option<String>, emit: RefsEmit, strict: bool) -> Result<()> {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600);
    let mut files = store::context_files_since(cutoff, project.as_deref())?;
    if strict {
        files.retain(|file| !is_noise_artifact(&file.path));
    }

    if files.is_empty() {
        eprintln!("No context files found within last {} hours.", hours);
    } else {
        match emit {
            RefsEmit::Summary => print_refs_summary(&files)?,
            RefsEmit::Paths => {
                let stdout = io::stdout();
                let mut out = io::BufWriter::new(stdout.lock());
                for f in &files {
                    if let Err(err) = writeln!(out, "{}", f.path.display()) {
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

fn print_refs_summary(files: &[store::StoredContextFile]) -> Result<()> {
    let mut by_project: BTreeMap<String, RefsProjectSummary> = BTreeMap::new();

    for path in files {
        let file_name = path
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown-file")
            .to_string();
        let date = path.date_iso.clone();
        let project = path.project.clone();
        let latest_rel = format!("{}/{}/{}", date, path.kind.dir_name(), file_name);
        let agent = path.agent.to_ascii_lowercase();

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

/// Sync stored chunks to rmcp-memex semantic index.
fn run_memex_sync(
    namespace: &str,
    per_chunk: bool,
    db_path: Option<PathBuf>,
    reindex: bool,
) -> Result<()> {
    if !memex::check_memex_available() {
        anyhow::bail!("rmcp-memex not found in PATH. Install with: cargo install rmcp-memex");
    }

    let truth = memex::resolve_runtime_truth(db_path.as_deref())?;

    let canonical_root = store::canonical_store_dir()?;
    let chunk_paths: Vec<PathBuf> = store::scan_context_files()?
        .into_iter()
        .map(|file| file.path)
        .collect();
    if chunk_paths.is_empty() {
        eprintln!(
            "No canonical stored chunks found under: {}",
            canonical_root.display()
        );
        eprintln!("Run `aicx store`, `aicx all`, or another extractor first.");
        return Ok(());
    }

    let config = MemexConfig {
        namespace: namespace.to_string(),
        db_path: db_path.clone(),
        batch_mode: !per_chunk,
        preprocess: true,
    };

    eprintln!(
        "Syncing canonical chunks from: {}",
        canonical_root.display()
    );
    eprintln!("  Chunk files: {}", chunk_paths.len());
    eprintln!("  Namespace: {}", config.namespace);
    eprintln!("  Embedding model: {}", truth.embedding_model);
    eprintln!("  Embedding dims: {}", truth.embedding_dimension);
    eprintln!("  LanceDB path: {}", truth.db_path.display());
    eprintln!("  BM25 path: {}", truth.bm25_path.display());
    if let Some(path) = truth.config_path.as_ref() {
        eprintln!("  Config: {}", path.display());
    }
    eprintln!(
        "  Mode: {}",
        if config.batch_mode {
            "batch import (metadata-rich)"
        } else {
            "per-chunk (metadata-rich)"
        }
    );

    if reindex {
        eprintln!("  Reindex: wiping current rmcp-memex store before import");
        eprintln!(
            "  Warning: Lance vector schema is shared across the whole store, so other namespaces in {} will need a rebuild too.",
            truth.db_path.display()
        );
        memex::reset_semantic_index(namespace, db_path.as_deref())?;
    }

    let result = memex::sync_new_chunk_paths(&chunk_paths, &config)?;

    eprintln!(
        "✓ Memex sync: {} pushed, {} skipped",
        result.chunks_pushed, result.chunks_skipped,
    );

    for err in &result.errors {
        eprintln!("  Error: {}", err);
    }

    Ok(())
}

/// Run the dashboard server shell against the central store.
struct DashboardServerRunArgs {
    store_root: Option<PathBuf>,
    host: String,
    port: u16,
    artifact: PathBuf,
    title: String,
    preview_chars: usize,
}

/// Run dashboard server mode with server-shell HTML and API-backed regeneration.
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
    let artifact_path = args.artifact;

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
    fn search_accepts_score_and_json_flags() {
        let cli = Cli::try_parse_from(["aicx", "search", "dashboard", "--score", "60", "--json"])
            .expect("search command with score/json should parse");

        match cli.command {
            Some(Commands::Search { score, json, .. }) => {
                assert_eq!(score, Some(60));
                assert!(json);
            }
            _ => panic!("expected search command"),
        }
    }

    #[test]
    fn rank_subcommand_is_rejected() {
        let err = Cli::try_parse_from(["aicx", "rank", "-p", "foo"])
            .expect_err("rank subcommand should be rejected");
        let rendered = err.to_string();
        assert!(rendered.contains("unrecognized subcommand"));
        assert!(rendered.contains("rank"));
    }

    #[test]
    fn top_level_help_marks_init_as_retired() {
        let mut cmd = Cli::command();
        let rendered = cmd.render_help().to_string();

        assert!(rendered.contains("init"));
        assert!(rendered.contains("Retired compatibility shim"));
        assert!(!rendered.contains("Initialize repo context and run an agent"));
    }

    #[test]
    fn init_help_explains_retirement_and_hides_legacy_flags() {
        let mut cmd = Cli::command();
        let init = cmd
            .find_subcommand_mut("init")
            .expect("init subcommand should exist for compatibility");
        let rendered = init.render_long_help().to_string();

        assert!(rendered.contains("aicx init has been retired."));
        assert!(rendered.contains("/vc-init inside Claude Code."));
        assert!(!rendered.contains("--agent"));
        assert!(!rendered.contains("--action"));
        assert!(!rendered.contains("--no-run"));
        assert!(!rendered.contains("Initialize repo context and run an agent"));
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
    fn migrate_accepts_custom_roots() {
        let cli = Cli::try_parse_from([
            "aicx",
            "migrate",
            "--dry-run",
            "--legacy-root",
            "/tmp/legacy",
            "--store-root",
            "/tmp/aicx",
        ])
        .expect("migrate command with explicit roots should parse");

        match cli.command {
            Some(Commands::Migrate {
                dry_run,
                legacy_root,
                store_root,
            }) => {
                assert!(dry_run);
                assert_eq!(legacy_root, Some(PathBuf::from("/tmp/legacy")));
                assert_eq!(store_root, Some(PathBuf::from("/tmp/aicx")));
            }
            _ => panic!("expected migrate command"),
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
