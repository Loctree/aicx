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

        /// Force full extraction, ignore dedup hashes
        #[arg(long)]
        force: bool,
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

        /// Force full extraction, ignore dedup hashes
        #[arg(long)]
        force: bool,
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

        /// Force full extraction, ignore dedup hashes
        #[arg(long)]
        force: bool,
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
            force,
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
                force,
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
            force,
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
                force,
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
            force,
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
                force,
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
    force: bool,
) -> Result<()> {
    // Load state for incremental/dedup
    let mut state = StateManager::load();
    let project_name = project.as_deref().unwrap_or("_global").to_string();

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
            let exact = StateManager::content_hash(
                &e.agent,
                &e.session_id,
                e.timestamp.timestamp(),
                &e.message,
            );
            if !state.is_new(&project_name, exact) {
                continue; // exact duplicate
            }

            let overlap = StateManager::overlap_hash(
                &e.agent,
                &e.session_id,
                e.timestamp.timestamp(),
                &e.message,
            );
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

    // Write local output
    let written = output::write_report(&out_config, &output_entries, &metadata)?;

    for path in &written {
        eprintln!("  → {}", path.display());
    }

    // Dual write: always persist to global store (~/.ai-contexters/contexts/)
    if !output_entries.is_empty() {
        let mut groups: std::collections::BTreeMap<(String, String), Vec<output::TimelineEntry>> =
            std::collections::BTreeMap::new();
        for entry in &output_entries {
            let date = entry.timestamp.format("%Y-%m-%d").to_string();
            groups
                .entry((entry.agent.clone(), date))
                .or_default()
                .push(entry.clone());
        }

        let mut index = store::load_index();
        for ((agent_name, date), group_entries) in &groups {
            let path = store::write_context(&project_name, agent_name, date, group_entries)?;
            store::update_index(&mut index, &project_name, agent_name, date, group_entries.len());
            eprintln!("  store → {}", path.display());
        }
        store::save_index(&index)?;
    }

    // Rotation
    if rotate > 0 {
        let prefix = agents.join("_");
        let deleted = output::rotate_outputs(output_dir, &prefix, rotate)?;
        if deleted > 0 {
            eprintln!("  Rotated: deleted {} old files", deleted);
        }
    }

    // Update state (hashes already marked during dedup filtering above)
    if !entries.is_empty() {
        if force {
            // When --force skips dedup, we still mark entries as seen for future runs
            for e in &entries {
                let exact = StateManager::content_hash(
                    &e.agent,
                    &e.session_id,
                    e.timestamp.timestamp(),
                    &e.message,
                );
                let overlap = StateManager::overlap_hash(
                    &e.agent,
                    &e.session_id,
                    e.timestamp.timestamp(),
                    &e.message,
                );
                state.mark_seen(&project_name, exact);
                state.mark_seen(&overlap_project, overlap);
            }
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

/// List context files from the global store, filtered by recency.
fn run_refs(hours: u64, project: Option<String>) -> Result<()> {
    let ctx_dir = store::contexts_dir()?;
    if !ctx_dir.exists() {
        eprintln!("No contexts directory found at: {}", ctx_dir.display());
        return Ok(());
    }

    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(hours * 3600);

    let mut files: Vec<PathBuf> = Vec::new();

    // Walk contexts/<project>/<agent>/<date>.md
    let project_dirs: Vec<_> = if let Some(ref p) = project {
        let d = ctx_dir.join(p);
        if d.is_dir() { vec![d] } else { vec![] }
    } else {
        std::fs::read_dir(&ctx_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    };

    for proj_dir in project_dirs {
        for agent_entry in std::fs::read_dir(&proj_dir)?.filter_map(|e| e.ok()) {
            let agent_path = agent_entry.path();
            if !agent_path.is_dir() {
                continue;
            }
            for file_entry in std::fs::read_dir(&agent_path)?.filter_map(|e| e.ok()) {
                let fpath = file_entry.path();
                if fpath.extension().is_some_and(|ext| ext == "md")
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
        for f in &files {
            println!("{}", f.display());
        }
        eprintln!("({} files)", files.len());
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
