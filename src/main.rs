//! AI Contexters
//!
//! Extracts timeline and decisions from AI agent session files:
//! - Claude Code: ~/.claude/projects/*/*.jsonl
//! - Codex: ~/.codex/history.jsonl
//! - Gemini: ~/.gemini/tmp/<hash>/chats/session-*.json
//!
//! Features: incremental extraction, deduplication, rotation, append mode.
//!
//! Created by M&K (c)2026 VetCoders

use anyhow::Result;
use chrono::Utc;
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

use ai_contexters::chunker::{self, ChunkerConfig};
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

        /// Append to a single timeline file instead of creating new files
        #[arg(long)]
        append_to: Option<PathBuf>,

        /// Keep only last N output files (0 = unlimited)
        #[arg(long, default_value = "0")]
        rotate: usize,

        /// Use incremental mode (skip already-processed entries)
        #[arg(long)]
        incremental: bool,

        /// Include assistant messages (can be large)
        #[arg(long)]
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
    },

    /// Extract timeline from Codex history
    Codex {
        /// Project/repo filter
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

        /// Append to a single timeline file
        #[arg(long)]
        append_to: Option<PathBuf>,

        /// Keep only last N output files (0 = unlimited)
        #[arg(long, default_value = "0")]
        rotate: usize,

        /// Use incremental mode
        #[arg(long)]
        incremental: bool,

        /// Include loctree snapshot
        #[arg(long)]
        loctree: bool,

        /// Project root for loctree snapshot
        #[arg(long)]
        project_root: Option<PathBuf>,

        /// Also chunk and sync to memex after extraction
        #[arg(long)]
        memex: bool,
    },

    /// Extract from all agents (Claude + Codex + Gemini)
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

        /// Append to a single timeline file
        #[arg(long)]
        append_to: Option<PathBuf>,

        /// Keep only last N output files (0 = unlimited)
        #[arg(long, default_value = "0")]
        rotate: usize,

        /// Use incremental mode
        #[arg(long)]
        incremental: bool,

        /// Include assistant messages
        #[arg(long)]
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
    },

    /// Store contexts in central store (~/.ai-contexters/) and optionally sync to memex
    Store {
        /// Project name (required for store organization)
        #[arg(short, long)]
        project: Option<String>,

        /// Agent filter: claude, codex, gemini (default: all)
        #[arg(short, long)]
        agent: Option<String>,

        /// Hours to look back (default: 48)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Include assistant messages
        #[arg(long)]
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

        /// Build context/prompt only, do not run an agent
        #[arg(long)]
        no_run: bool,

        /// Skip "Run? (y)es / (n)o" confirmation
        #[arg(long)]
        no_confirm: bool,
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

    match cli.command {
        Commands::Claude {
            project,
            hours,
            output,
            format,
            append_to,
            rotate,
            incremental,
            include_assistant,
            loctree,
            project_root,
            memex,
        } => {
            run_extraction(
                &["claude"],
                project,
                hours,
                &output,
                &format,
                append_to,
                rotate,
                incremental,
                include_assistant,
                loctree,
                project_root,
                memex,
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
            loctree,
            project_root,
            memex,
        } => {
            run_extraction(
                &["codex"],
                project,
                hours,
                &output,
                &format,
                append_to,
                rotate,
                incremental,
                false,
                loctree,
                project_root,
                memex,
            )?;
        }
        Commands::All {
            project,
            hours,
            output,
            append_to,
            rotate,
            incremental,
            include_assistant,
            loctree,
            project_root,
            memex,
        } => {
            run_extraction(
                &["claude", "codex", "gemini"],
                project,
                hours,
                &output,
                "both",
                append_to,
                rotate,
                incremental,
                include_assistant,
                loctree,
                project_root,
                memex,
            )?;
        }
        Commands::Store {
            project,
            agent,
            hours,
            include_assistant,
            memex,
        } => {
            run_store(project, agent, hours, include_assistant, memex)?;
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
            no_run,
            no_confirm,
        } => {
            let opts = InitOptions {
                project,
                agent,
                model,
                horizon_hours: hours,
                max_lines,
                no_run,
                no_confirm,
            };
            init::run_init(opts)?;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_extraction(
    agents: &[&str],
    project: Option<String>,
    hours: u64,
    output_dir: &Path,
    format: &str,
    append_to: Option<PathBuf>,
    rotate: usize,
    incremental: bool,
    include_assistant: bool,
    include_loctree: bool,
    project_root: Option<PathBuf>,
    sync_memex: bool,
) -> Result<()> {
    // Load state for incremental/dedup
    let mut state = StateManager::load();

    let cutoff = Utc::now() - chrono::Duration::hours(hours as i64);

    // Determine watermark (incremental mode uses per-source watermark)
    let watermark = if incremental {
        let source_key = format!(
            "{}:{}",
            agents.join("+"),
            project.as_deref().unwrap_or("all")
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

    // Dedup via state
    let pre_dedup = entries.len();
    entries.retain(|e| {
        let hash = StateManager::content_hash(
            &e.agent,
            &e.session_id,
            e.timestamp.timestamp(),
            &e.message,
        );
        state.is_new(hash)
    });

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
            message: e.message.clone(),
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
        project_filter: project.clone(),
        hours_back: hours,
        total_entries: output_entries.len(),
        sessions: sessions.clone(),
    };

    // Build output config
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
        dir: output_dir.to_path_buf(),
        format: out_format,
        mode,
        max_files: rotate,
        max_message_chars: 0, // no truncation
        include_loctree,
        project_root,
    };

    // Write output
    let written = output::write_report(&out_config, &output_entries, &metadata)?;

    for path in &written {
        eprintln!("  → {}", path.display());
    }

    // Rotation
    if rotate > 0 {
        let prefix = agents.join("_");
        let deleted = output::rotate_outputs(output_dir, &prefix, rotate)?;
        if deleted > 0 {
            eprintln!("  Rotated: deleted {} old files", deleted);
        }
    }

    // Update state
    if !entries.is_empty() {
        for e in &entries {
            let hash = StateManager::content_hash(
                &e.agent,
                &e.session_id,
                e.timestamp.timestamp(),
                &e.message,
            );
            state.mark_seen(hash);
        }

        if incremental {
            let source_key = format!(
                "{}:{}",
                agents.join("+"),
                project.as_deref().unwrap_or("all")
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

    eprintln!(
        "✓ {} entries from {} sessions ({})",
        output_entries.len(),
        sessions.len(),
        agents.join("+"),
    );

    // Memex sync: chunk entries and push to vector store
    if sync_memex && !output_entries.is_empty() {
        let proj_name = project.as_deref().unwrap_or("unknown");
        let agent_name = agents.join("+");

        let chunker_config = ChunkerConfig::default();
        let chunks =
            chunker::chunk_entries(&output_entries, proj_name, &agent_name, &chunker_config);

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
    project: Option<String>,
    agent: Option<String>,
    hours: u64,
    include_assistant: bool,
    sync_memex: bool,
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
            message: e.message.clone(),
            branch: e.branch.clone(),
            cwd: e.cwd.clone(),
        })
        .collect();

    // Group by agent+date and write to central store
    let proj_name = project.as_deref().unwrap_or("unknown");
    let mut index = store::load_index();
    let mut stored_count = 0usize;

    // Group entries by (agent, date)
    let mut groups: std::collections::BTreeMap<(String, String), Vec<output::TimelineEntry>> =
        std::collections::BTreeMap::new();

    for entry in &output_entries {
        let date = entry.timestamp.format("%Y-%m-%d").to_string();
        groups
            .entry((entry.agent.clone(), date))
            .or_default()
            .push(entry.clone());
    }

    for ((agent_name, date), group_entries) in &groups {
        let path = store::write_context(proj_name, agent_name, date, group_entries)?;
        store::update_index(&mut index, proj_name, agent_name, date, group_entries.len());
        stored_count += group_entries.len();
        eprintln!("  → {} ({} entries)", path.display(), group_entries.len());
    }

    store::save_index(&index)?;
    eprintln!(
        "✓ Stored {} entries in {} groups",
        stored_count,
        groups.len()
    );

    // Chunk and sync to memex if requested
    if sync_memex && !output_entries.is_empty() {
        let agent_label = agents.join("+");
        let chunker_config = ChunkerConfig::default();
        let chunks =
            chunker::chunk_entries(&output_entries, proj_name, &agent_label, &chunker_config);

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
