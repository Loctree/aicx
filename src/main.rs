//! AI Contexters
//!
//! Extracts timeline and decisions from AI agent session files:
//! - Claude Code: ~/.claude/projects/*/*.jsonl
//! - Codex: ~/.codex/history.jsonl
//! - Gemini: ~/.gemini/tmp/<hash>/chats/session-*.json
//!
//! Features: incremental extraction, deduplication, rotation, append mode.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use ai_contexters::chunker::{self, ChunkerConfig};
use ai_contexters::dashboard::{self, DashboardConfig};
use ai_contexters::dashboard_server::{self, DashboardServerConfig};
use ai_contexters::init::{self, InitOptions};
use ai_contexters::memex::{self, MemexConfig};
use ai_contexters::output::{self, OutputConfig, OutputFormat, OutputMode, ReportMetadata};
use ai_contexters::sources::{self, ExtractionConfig};
use ai_contexters::state::StateManager;
use ai_contexters::store;

/// AI Contexters - timeline and decisions from AI sessions
#[derive(Parser)]
#[command(name = "ai-contexters")]
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

    #[command(subcommand)]
    command: Commands,
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
enum ExtractInputFormat {
    Claude,
    Codex,
    Gemini,
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

        /// What to print to stdout: paths, json, none (default: paths)
        #[arg(long, value_enum, default_value_t = StdoutEmit::Paths)]
        emit: StdoutEmit,
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

        /// What to print to stdout: paths, json, none (default: paths)
        #[arg(long, value_enum, default_value_t = StdoutEmit::Paths)]
        emit: StdoutEmit,
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

        /// What to print to stdout: paths, json, none (default: paths)
        #[arg(long, value_enum, default_value_t = StdoutEmit::Paths)]
        emit: StdoutEmit,
    },

    /// Extract timeline from a single agent session file (direct path).
    ///
    /// Example:
    ///   ai-contexters extract --format claude /path/to/session.jsonl -o /tmp/report.md
    Extract {
        /// Input format (agent): claude | codex | gemini
        #[arg(long, value_enum, alias = "input-format")]
        format: ExtractInputFormat,

        /// Input file path (JSONL / JSON depending on agent)
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

    /// Generate a searchable HTML dashboard from the ai-contexters store.
    Dashboard {
        /// Store root directory (default: ~/.ai-contexters)
        #[arg(long)]
        store_root: Option<PathBuf>,

        /// Output HTML path
        #[arg(short, long, default_value = "ai-contexters-dashboard.html")]
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
        #[arg(long, default_value = "ai-contexters-dashboard.html")]
        artifact: PathBuf,

        /// Document title
        #[arg(long, default_value = "AI Contexters Dashboard")]
        title: String,

        /// Max preview characters per record (0 = no truncation)
        #[arg(long, default_value = "320")]
        preview_chars: usize,
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
        Commands::Claude {
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
        } => {
            let include_assistant = include_assistant_flag || !user_only;
            run_extraction(
                &["claude"],
                project,
                hours,
                output.as_deref(),
                &format,
                append_to,
                rotate,
                incremental,
                include_assistant,
                loctree,
                project_root,
                memex,
                force,
                redact_secrets,
                emit,
            )?;
        }
        Commands::Codex {
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
        } => {
            let include_assistant = include_assistant_flag || !user_only;
            run_extraction(
                &["codex"],
                project,
                hours,
                output.as_deref(),
                &format,
                append_to,
                rotate,
                incremental,
                include_assistant,
                loctree,
                project_root,
                memex,
                force,
                redact_secrets,
                emit,
            )?;
        }
        Commands::All {
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
        } => {
            let include_assistant = include_assistant_flag || !user_only;
            run_extraction(
                &["claude", "codex", "gemini"],
                project,
                hours,
                output.as_deref(),
                "both",
                append_to,
                rotate,
                incremental,
                include_assistant,
                loctree,
                project_root,
                memex,
                force,
                redact_secrets,
                emit,
            )?;
        }
        Commands::Extract {
            format,
            input,
            output,
            user_only,
            include_assistant: include_assistant_flag,
            max_message_chars,
        } => {
            let include_assistant = include_assistant_flag || !user_only;
            run_extract_file(
                format,
                input,
                output,
                include_assistant,
                max_message_chars,
                redact_secrets,
            )?;
        }
        Commands::Store {
            project,
            agent,
            hours,
            user_only,
            include_assistant: include_assistant_flag,
            memex,
        } => {
            let include_assistant = include_assistant_flag || !user_only;
            run_store(
                project,
                agent,
                hours,
                include_assistant,
                memex,
                redact_secrets,
            )?;
        }
        Commands::MemexSync {
            namespace,
            per_chunk,
            db_path,
        } => {
            run_memex_sync(&namespace, per_chunk, db_path)?;
        }
        Commands::List => {
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
        Commands::Init {
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
        } => {
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
        Commands::Refs { hours, project } => {
            run_refs(hours, project)?;
        }
        Commands::State {
            reset,
            project,
            info,
        } => {
            run_state(reset, project, info)?;
        }
        Commands::Dashboard {
            store_root,
            output,
            title,
            preview_chars,
        } => {
            run_dashboard(DashboardRunArgs {
                store_root,
                output,
                title,
                preview_chars,
            })?;
        }
        Commands::DashboardServe {
            store_root,
            host,
            port,
            artifact,
            title,
            preview_chars,
        } => {
            run_dashboard_server(DashboardServerRunArgs {
                store_root,
                host,
                port,
                artifact,
                title,
                preview_chars,
            })?;
        }
    }

    Ok(())
}

fn run_extract_file(
    format: ExtractInputFormat,
    input: PathBuf,
    output_path: PathBuf,
    include_assistant: bool,
    max_message_chars: usize,
    redact_secrets: bool,
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
    };

    // Sort by timestamp (extractors should already do this).
    entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    // Convert sources::TimelineEntry → output::TimelineEntry
    let output_entries: Vec<output::TimelineEntry> = entries
        .iter()
        .map(|e| output::TimelineEntry {
            timestamp: e.timestamp,
            agent: e.agent.clone(),
            session_id: e.session_id.clone(),
            role: e.role.clone(),
            message: if redact_secrets {
                ai_contexters::redact::redact_secrets(&e.message)
            } else {
                e.message.clone()
            },
            branch: e.branch.clone(),
            cwd: e.cwd.clone(),
        })
        .collect();

    // Collect unique sessions
    let mut sessions: Vec<String> = entries.iter().map(|e| e.session_id.clone()).collect();
    sessions.sort();
    sessions.dedup();

    let file_label = input
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "(unknown)".to_string());

    let hours_back = entries
        .first()
        .map(|e| (Utc::now() - e.timestamp).num_hours().max(0) as u64)
        .unwrap_or(0);

    let metadata = ReportMetadata {
        generated_at: Utc::now(),
        project_filter: Some(format!("file: {file_label}")),
        hours_back,
        total_entries: output_entries.len(),
        sessions,
    };

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

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_extraction(
    agents: &[&str],
    project: Vec<String>,
    hours: u64,
    output_dir: Option<&Path>,
    format: &str,
    append_to: Option<PathBuf>,
    rotate: usize,
    incremental: bool,
    include_assistant: bool,
    include_loctree: bool,
    project_root: Option<PathBuf>,
    sync_memex: bool,
    force: bool,
    redact_secrets: bool,
    emit: StdoutEmit,
) -> Result<()> {
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

    // Convert sources::TimelineEntry → output::TimelineEntry
    let output_entries: Vec<output::TimelineEntry> = entries
        .iter()
        .map(|e| output::TimelineEntry {
            timestamp: e.timestamp,
            agent: e.agent.clone(),
            session_id: e.session_id.clone(),
            role: e.role.clone(),
            message: if redact_secrets {
                ai_contexters::redact::redact_secrets(&e.message)
            } else {
                e.message.clone()
            },
            branch: e.branch.clone(),
            cwd: e.cwd.clone(),
        })
        .collect();

    // Collect unique sessions
    let mut sessions: Vec<String> = entries.iter().map(|e| e.session_id.clone()).collect();
    sessions.sort();
    sessions.dedup();

    let metadata = ReportMetadata {
        generated_at: Utc::now(),
        project_filter: if project.is_empty() {
            None
        } else {
            Some(project.join(", "))
        },
        hours_back: hours,
        total_entries: output_entries.len(),
        sessions: sessions.clone(),
    };

    // ── Store-first: group entries by repo (from cwd) × agent × date ──
    //
    // Writes agent-friendly chunks (~1500 tokens) to central store.
    // Paths go to stdout so agents can read them directly.
    let chunker_config = ai_contexters::chunker::ChunkerConfig::default();
    let mut all_written_paths: Vec<std::path::PathBuf> = Vec::new();

    if !output_entries.is_empty() {
        let mut repo_groups: std::collections::BTreeMap<
            (String, String, String),
            Vec<output::TimelineEntry>,
        > = std::collections::BTreeMap::new();

        for entry in &output_entries {
            let repo = sources::repo_name_from_cwd(entry.cwd.as_deref());
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

            let store_paths: Vec<String> = all_written_paths
                .iter()
                .map(|p| p.display().to_string())
                .collect();

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
        StdoutEmit::None => {}
    }

    // ── Optional local output (only when -o explicitly passed) ──
    if let Some(local_dir) = output_dir {
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

    // Update state (hashes already marked during dedup filtering above)
    if !entries.is_empty() {
        if force {
            // When --force skips dedup, we still mark entries as seen for future runs
            for e in &entries {
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
            if let Some(latest) = entries.last() {
                state.update_watermark(&source_key, latest.timestamp);
            }
        }

        state.record_run(
            entries.len(),
            agents.iter().map(|s| s.to_string()).collect(),
        );
        state.prune_old_hashes(50_000);
        state.save()?;
    }

    if output_entries.is_empty() {
        eprintln!(
            "✓ 0 entries from {} sessions ({})",
            sessions.len(),
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

    if all_entries.is_empty() {
        eprintln!("No entries found.");
        return Ok(());
    }

    // Convert to output::TimelineEntry
    let output_entries: Vec<output::TimelineEntry> = all_entries
        .iter()
        .map(|e| output::TimelineEntry {
            timestamp: e.timestamp,
            agent: e.agent.clone(),
            session_id: e.session_id.clone(),
            role: e.role.clone(),
            message: if redact_secrets {
                ai_contexters::redact::redact_secrets(&e.message)
            } else {
                e.message.clone()
            },
            branch: e.branch.clone(),
            cwd: e.cwd.clone(),
        })
        .collect();

    // Group by repo (from cwd) × agent × date and write chunked to central store
    let chunker_config = ai_contexters::chunker::ChunkerConfig::default();
    let mut index = store::load_index();
    let mut stored_count = 0usize;
    let mut all_written_paths: Vec<std::path::PathBuf> = Vec::new();

    let mut repo_groups: std::collections::BTreeMap<
        (String, String, String),
        Vec<output::TimelineEntry>,
    > = std::collections::BTreeMap::new();

    for entry in &output_entries {
        let repo = sources::repo_name_from_cwd(entry.cwd.as_deref());
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

    // stdout: agent-readable paths
    for path in &all_written_paths {
        println!("{}", path.display());
    }

    // Chunk and sync to memex if requested
    if sync_memex && !output_entries.is_empty() {
        let agent_label = agents.join("+");
        let store_proj = if project.is_empty() {
            "_global".to_string()
        } else {
            project.join("+")
        };
        let chunker_config = ChunkerConfig::default();
        let chunks =
            chunker::chunk_entries(&output_entries, &store_proj, &agent_label, &chunker_config);

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

/// List context files from the global store, filtered by recency.
fn run_refs(hours: u64, project: Option<String>) -> Result<()> {
    let base = store::store_base_dir()?;

    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600);

    let mut files: Vec<PathBuf> = Vec::new();

    let project_dirs: Vec<_> = if let Some(ref p) = project {
        let d = base.join(p);
        if d.is_dir() { vec![d] } else { vec![] }
    } else {
        std::fs::read_dir(&base)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "memex"))
            .collect()
    };

    for proj_dir in project_dirs {
        for date_entry in std::fs::read_dir(&proj_dir)?.filter_map(|e| e.ok()) {
            let date_path = date_entry.path();
            if !date_path.is_dir() {
                continue;
            }
            for file_entry in std::fs::read_dir(&date_path)?.filter_map(|e| e.ok()) {
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
        eprintln!("No context files found within last {} hours.", hours);
    } else {
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
        eprintln!("Run `ai-contexters store --memex` first to generate chunks.");
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
        include_absolute_paths: false,
    };

    let artifact = dashboard::build_dashboard(&config)?;

    let output_path = ai_contexters::sanitize::validate_write_path(&args.output)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output directory: {}", parent.display()))?;
    }

    // Atomic write: temp file + rename (consistent with dashboard_server)
    let tmp_path = output_path.with_extension("tmp");
    fs::write(&tmp_path, &artifact.html)
        .with_context(|| format!("Failed to write temporary dashboard: {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &output_path).with_context(|| {
        let _ = fs::remove_file(&tmp_path);
        format!(
            "Failed to atomically replace dashboard: {}",
            output_path.display()
        )
    })?;

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
