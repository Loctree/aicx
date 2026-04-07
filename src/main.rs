//! AI Contexters — the operator front door for agent session history.
//!
//! `aicx` orchestrates a two-layer pipeline: canonical corpus first,
//! semantic materialization second. Materialization can stay explicit or run
//! behind the background daemon.
//!
//! Two-layer architecture:
//!   1. **Canonical corpus** (`~/.aicx/`) — deduplicated, chunked, steerable markdown.
//!      Built by extractors (`claude`, `codex`, `all`) and `store`. This is ground truth.
//!   2. **Semantic materialization** (memex) — vector + BM25 index for embedding-aware
//!      retrieval by agents and MCP tools. Built by `memex-sync` or `--memex` on extractors.
//!      Memex is the retrieval kernel; `aicx` is the orchestrator.
//!
//! Supported sources:
//! - Claude Code: ~/.claude/projects/*/*.jsonl
//! - Codex: ~/.codex/history.jsonl
//! - Gemini: ~/.gemini/tmp/<hash>/chats/session-*.json
//! - Gemini Antigravity: ~/.gemini/antigravity/{conversations/<uuid>.pb,brain/<uuid>/}
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

use ai_contexters::daemon::{self, ControlOutcome, DaemonConfig, DaemonStatusSnapshot};
use ai_contexters::dashboard::{self, DashboardConfig};
use ai_contexters::dashboard_server::{self, DashboardServerConfig};
use ai_contexters::intents;
use ai_contexters::memex::{self, MemexConfig, SyncProgress, SyncProgressPhase};
use ai_contexters::output::{self, OutputConfig, OutputFormat, OutputMode, ReportMetadata};
use ai_contexters::rank;
use ai_contexters::sources::{self, ExtractionConfig};
use ai_contexters::state::StateManager;
use ai_contexters::store;

const CLI_AFTER_HELP: &str = "\
Most people want one of these:
  aicx
      Guided front door: quick state, suggested next moves, and the shortest path forward.

  aicx doctor
      One honest readiness check plus the next command to run.

  aicx doctor --fix
      Repair what can be repaired automatically, then rerun the check.

  aicx dashboard --open
      Write a local HTML snapshot and open it in your browser.

  aicx latest -p <project>
      Show the newest stored chunks with readable previews and chainable refs.

  aicx all -H 24 --incremental --memex
      Refresh the canonical corpus and catch memex up in one pass.

  aicx search \"query\"
      Fast recall across the canonical store on disk.

  aicx read <ref-or-path>
      Open one stored chunk directly after discovery.

  aicx steer --project <project>
      Exact retrieval by run, prompt, project, agent, or date metadata.
";

/// aicx — operator front door for agent session history.
///
/// Two-layer pipeline with explicit canonical truth and optional background sync.
///
/// Layer 1 (canonical corpus): extract, deduplicate, and chunk agent logs
/// into steerable markdown at ~/.aicx/. This is ground truth.
///
/// Layer 2 (semantic materialization): embed the corpus into a vector + BM25
/// index (memex) for retrieval by agents and MCP tools. Use one-shot
/// `memex-sync` or hand the loop to `aicx-memex daemon`.
///
/// aicx is the orchestrator; memex is the retrieval kernel.
#[derive(Debug, Parser)]
#[command(name = "aicx")]
#[command(author = "M&K (c)2026 VetCoders")]
#[command(version)]
#[command(after_help = CLI_AFTER_HELP, after_long_help = CLI_AFTER_HELP)]
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
    /// Extract + store Claude Code sessions into the canonical corpus (layer 1).
    ///
    /// Reads ~/.claude/projects/ logs, deduplicates, chunks, and writes
    /// steerable markdown to ~/.aicx/. Add --memex to also push new chunks
    /// into the memex retrieval kernel (layer 2).
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

        /// After extraction, push new chunks into the memex retrieval kernel (layer 2).
        /// Shortcut for running `aicx memex-sync` as a separate step.
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

    /// Extract + store Codex sessions into the canonical corpus (layer 1).
    ///
    /// Reads ~/.codex/history.jsonl, deduplicates, chunks, and writes
    /// steerable markdown to ~/.aicx/. Add --memex to also push new chunks
    /// into the memex retrieval kernel (layer 2).
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

        /// After extraction, push new chunks into the memex retrieval kernel (layer 2).
        /// Shortcut for running `aicx memex-sync` as a separate step.
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

    /// Extract + store from all agents (Claude + Codex + Gemini) into the canonical corpus (layer 1).
    ///
    /// Runs each extractor, deduplicates, chunks, and writes steerable
    /// markdown to ~/.aicx/. Add --memex to also push new chunks into the
    /// memex retrieval kernel (layer 2).
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

        /// After extraction, push new chunks into the memex retrieval kernel (layer 2).
        /// Shortcut for running `aicx memex-sync` as a separate step.
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

    /// Extract a single session file and write it to a specific output path (layer 1, direct).
    ///
    /// Bypasses the canonical store — useful for one-off inspection or piping.
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

    /// Build the canonical corpus in ~/.aicx/ from agent logs (layer 1).
    ///
    /// The primary corpus-building command: extracts, deduplicates, chunks,
    /// and writes steerable markdown. Optional agent filter narrows the scope.
    /// Add --memex to also materialize new chunks into the memex retrieval
    /// kernel (layer 2) — a shortcut for running `memex-sync` separately.
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

        /// After extraction, push new chunks into the memex retrieval kernel (layer 2).
        /// Shortcut for running `aicx memex-sync` as a separate step.
        #[arg(long)]
        memex: bool,

        /// What to print to stdout: paths, json, none (default: none)
        #[arg(long, value_enum, default_value_t = StdoutEmit::None)]
        emit: StdoutEmit,
    },

    /// Materialize the canonical corpus into the memex retrieval kernel (layer 2).
    ///
    /// Reads chunks from ~/.aicx/, embeds them, and upserts into the rmcp-memex
    /// vector + BM25 index. Use this for explicit one-shot syncs and rebuilds,
    /// or run `aicx-memex daemon` / `aicx daemon` to keep the semantic layer
    /// fresh in the background.
    ///
    /// First build:    aicx memex-sync                (embed + index all unsynced chunks)
    /// Incremental:    aicx memex-sync                (only new chunks since last sync)
    /// Full rebuild:   aicx memex-sync --reindex      (wipe index, re-embed everything)
    /// Per-chunk mode: aicx memex-sync --per-chunk    (granular library writes instead of batch store)
    MemexSync {
        /// Namespace in the semantic index
        #[arg(short, long, default_value = "ai-contexts")]
        namespace: String,

        /// Use per-chunk library writes instead of batch store (slower, more granular)
        #[arg(long)]
        per_chunk: bool,

        /// Override LanceDB path
        #[arg(long)]
        db_path: Option<PathBuf>,

        /// Wipe the memex index and re-embed the entire canonical corpus.
        /// Use after an embedding model or dimension change, or when the
        /// index has drifted from the canonical store.
        #[arg(long)]
        reindex: bool,
    },

    /// Start the background memex/steer daemon on a Unix socket.
    ///
    /// The daemon keeps the canonical store fresh via incremental extraction,
    /// repairs steer indexing, and incrementally materializes chunks into memex.
    /// By default this command detaches into the background; use `--foreground`
    /// to keep logs in the current terminal.
    Daemon {
        /// Custom Unix socket path
        #[arg(long)]
        socket_path: Option<PathBuf>,

        /// Keep the daemon in the current terminal instead of detaching
        #[arg(long)]
        foreground: bool,

        /// Poll interval between refresh cycles
        #[arg(long, default_value = "300")]
        poll_seconds: u64,

        /// Lookback window for the incremental canonical refresh
        #[arg(long, default_value = "720")]
        refresh_hours: u64,

        /// Optional project filter(s) for the refresh loop
        #[arg(short, long, num_args = 1..)]
        project: Vec<String>,

        /// Namespace in the semantic index
        #[arg(short, long, default_value = "ai-contexts")]
        namespace: String,

        /// Override LanceDB path
        #[arg(long)]
        db_path: Option<PathBuf>,

        /// Use per-chunk library writes instead of batch memex sync
        #[arg(long)]
        per_chunk: bool,

        /// Skip the initial bootstrap cycle after daemon start
        #[arg(long)]
        no_bootstrap: bool,
    },

    #[command(hide = true)]
    DaemonRun {
        #[arg(long)]
        socket_path: Option<PathBuf>,

        #[arg(long, default_value = "300")]
        poll_seconds: u64,

        #[arg(long, default_value = "720")]
        refresh_hours: u64,

        #[arg(short, long, num_args = 1..)]
        project: Vec<String>,

        #[arg(short, long, default_value = "ai-contexts")]
        namespace: String,

        #[arg(long)]
        db_path: Option<PathBuf>,

        #[arg(long)]
        per_chunk: bool,

        #[arg(long)]
        no_bootstrap: bool,
    },

    /// Show daemon status from the Unix-socket control plane.
    DaemonStatus {
        /// Custom Unix socket path
        #[arg(long)]
        socket_path: Option<PathBuf>,

        /// Emit compact JSON instead of plain text
        #[arg(short = 'j', long)]
        json: bool,
    },

    /// Queue an immediate daemon sync cycle.
    DaemonSync {
        /// Custom Unix socket path
        #[arg(long)]
        socket_path: Option<PathBuf>,
    },

    /// Stop the background daemon.
    DaemonStop {
        /// Custom Unix socket path
        #[arg(long)]
        socket_path: Option<PathBuf>,
    },

    /// List raw agent session sources on disk (pre-extraction inputs).
    ///
    /// Shows Claude Code, Codex, and Gemini log paths with session counts
    /// and sizes. This is what extractors will read from — use `refs` to
    /// see what is already in the canonical store after extraction.
    List,

    /// Human-friendly readiness check for sources, canonical store, and daemon.
    ///
    /// Use this when you want one obvious answer to "is aicx actually ready,
    /// and what should I do next?" without stitching together `list`, `refs`,
    /// and `daemon-status` manually.
    Doctor {
        /// Hours to use for the "recent activity" window
        #[arg(short = 'H', long, default_value = "72")]
        hours: u64,

        /// Project filter for the canonical store summary
        #[arg(short, long)]
        project: Option<String>,

        /// Emit compact JSON instead of the human-readable summary
        #[arg(short = 'j', long)]
        json: bool,

        /// Repair what can be repaired automatically, then rerun the check
        #[arg(long, conflicts_with = "json")]
        fix: bool,
    },

    /// List chunks in the canonical store (layer 1 inventory).
    ///
    /// Shows what extractors have already written to ~/.aicx/.
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

    /// Manage extraction dedup state (watermarks and hashes).
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

    /// Generate a searchable HTML dashboard from the canonical store (layer 1).
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

        /// Open the generated dashboard in your default browser
        #[arg(long)]
        open: bool,
    },

    /// Run dashboard HTTP server with server-shell UI and on-demand data regeneration (layer 1).
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

        /// Open the live dashboard URL in your default browser
        #[arg(long)]
        open: bool,
    },

    /// Extract structured intents and decisions from canonical store (layer 1).
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

    /// Run aicx as an MCP server (stdio or streamable HTTP).
    ///
    /// Exposes search, read, steer, and rank tools over MCP for agent retrieval.
    /// Layer 1 tools (steer, search, read) work immediately — they query the
    /// canonical corpus on disk. Layer 2 tools (embedding-aware semantic
    /// search) require a materialized memex index — run `memex-sync` first.
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
        long_about = "aicx init has been retired.\n\nContext initialisation is now handled by /vc-init inside Claude Code.\nIf you want a local operator readiness check instead, run `aicx doctor`.\nSee: https://vibecrafted.io/\n\nLegacy flags are still accepted for compatibility, but they have no effect."
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

    /// Show the newest stored chunks with readable previews and chainable refs.
    ///
    /// Use this when you want the fastest answer to "what was I just working
    /// on?" without stitching together `refs`, `search`, and `read` manually.
    Latest {
        /// Hours to look back (filter by canonical chunk date)
        #[arg(short = 'H', long, default_value = "168")]
        hours: u64,

        /// Project filter (org/repo substring, case-insensitive)
        #[arg(short, long)]
        project: Option<String>,

        /// Maximum chunks to show (0 = unlimited)
        #[arg(short = 'l', long, default_value = "5")]
        limit: usize,

        /// Filter out low-signal noise (<15 lines, task-notifications only)
        #[arg(long)]
        strict: bool,

        /// Emit compact JSON instead of the human-readable summary
        #[arg(short = 'j', long)]
        json: bool,
    },

    /// Fuzzy search across the canonical corpus (layer 1, filesystem-only).
    ///
    /// Searches chunk content and frontmatter directly in ~/.aicx/ — works
    /// immediately, no memex index needed. For embedding-aware semantic
    /// retrieval, materialize the index with `memex-sync` first, then use
    /// MCP tools via `aicx serve`. Use `aicx read <ref-or-path>` to open one
    /// promising chunk after discovery.
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

    /// Open one stored chunk by AICX ref or absolute path.
    ///
    /// This is the "I found it, now show it" step after `search`, `refs`, or
    /// `steer`. Pass either a store-relative ref such as
    /// `store/VetCoders/ai-contexters/.../chunk.md` or a full path copied from
    /// another command.
    Read {
        /// Store-relative ref under ~/.aicx/ or absolute chunk path
        target: String,

        /// Truncate the returned content after N UTF-8 characters (0 = full chunk)
        #[arg(long, default_value = "0")]
        max_chars: usize,

        /// Truncate the returned content after N lines (0 = full chunk)
        #[arg(long, default_value = "0")]
        max_lines: usize,

        /// Emit compact JSON instead of the human-readable view
        #[arg(short = 'j', long)]
        json: bool,
    },

    /// Retrieve chunks by steering metadata (layer 1, frontmatter fields).
    ///
    /// Filters the canonical store by run_id, prompt_id, agent, kind, project,
    /// and/or date range using frontmatter metadata — no grep needed.
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

    /// Migrate legacy ~/.ai-contexters/ data into the canonical AICX store.
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
        Some(Commands::Daemon {
            socket_path,
            foreground,
            poll_seconds,
            refresh_hours,
            project,
            namespace,
            db_path,
            per_chunk,
            no_bootstrap,
        }) => {
            let config = build_daemon_config(
                socket_path,
                poll_seconds,
                refresh_hours,
                project,
                namespace,
                db_path,
                per_chunk,
                !no_bootstrap,
            );
            if foreground {
                daemon::run_foreground(config)?;
            } else {
                daemon::spawn_detached(&config)?;
                let socket_path = daemon_socket_display(config.socket_path.as_ref())?;
                eprintln!("✓ aicx-memex daemon started");
                eprintln!("  Socket: {}", socket_path.display());
            }
        }
        Some(Commands::DaemonRun {
            socket_path,
            poll_seconds,
            refresh_hours,
            project,
            namespace,
            db_path,
            per_chunk,
            no_bootstrap,
        }) => {
            let config = build_daemon_config(
                socket_path,
                poll_seconds,
                refresh_hours,
                project,
                namespace,
                db_path,
                per_chunk,
                !no_bootstrap,
            );
            daemon::run_foreground(config)?;
        }
        Some(Commands::DaemonStatus { socket_path, json }) => {
            run_daemon_status(socket_path.as_deref(), json)?;
        }
        Some(Commands::DaemonSync { socket_path }) => {
            run_daemon_sync(socket_path.as_deref())?;
        }
        Some(Commands::DaemonStop { socket_path }) => {
            run_daemon_stop(socket_path.as_deref())?;
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
        Some(Commands::Doctor {
            hours,
            project,
            json,
            fix,
        }) => {
            run_doctor(hours, project, json, fix)?;
        }
        Some(Commands::Init { .. }) => {
            eprintln!("aicx init has been retired.");
            eprintln!("Context initialisation is now handled by /vc-init inside Claude Code.");
            eprintln!("For a local readiness check, run: aicx doctor");
            eprintln!("See: https://vibecrafted.io/");
        }
        Some(Commands::Latest {
            hours,
            project,
            limit,
            strict,
            json,
        }) => {
            run_latest(hours, project, limit, strict, json)?;
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
            open,
        }) => {
            run_dashboard(DashboardRunArgs {
                store_root,
                output,
                title,
                preview_chars,
                open,
            })?;
        }
        Some(Commands::DashboardServe {
            store_root,
            host,
            port,
            artifact,
            title,
            preview_chars,
            open,
        }) => {
            run_dashboard_server(DashboardServerRunArgs {
                store_root,
                host,
                port,
                artifact,
                title,
                preview_chars,
                open,
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
        Some(Commands::Read {
            target,
            max_chars,
            max_lines,
            json,
        }) => {
            run_read(&target, max_chars, max_lines, json)?;
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
            if let Err(err) = run_front_door(cli.hours, cli.project.clone()) {
                eprintln!(
                    "Warning: could not build the guided front door ({err:#}). Showing full help instead."
                );
                Cli::command().print_help()?;
            }
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
fn build_daemon_config(
    socket_path: Option<PathBuf>,
    poll_seconds: u64,
    refresh_hours: u64,
    project: Vec<String>,
    namespace: String,
    db_path: Option<PathBuf>,
    per_chunk: bool,
    bootstrap: bool,
) -> DaemonConfig {
    DaemonConfig {
        socket_path,
        poll_seconds,
        refresh_hours,
        projects: project,
        namespace,
        db_path,
        per_chunk,
        bootstrap,
    }
}

fn daemon_socket_display(socket_path: Option<&PathBuf>) -> Result<PathBuf> {
    match socket_path {
        Some(path) => Ok(path.clone()),
        None => daemon::default_socket_path(),
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DoctorStatus {
    Ok,
    Partial,
    Missing,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorSourceFamily {
    family: String,
    locations: usize,
    sessions: usize,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorSourcesSummary {
    status: DoctorStatus,
    locations: usize,
    total_sessions: usize,
    total_size_bytes: u64,
    families: Vec<DoctorSourceFamily>,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorStoreSummary {
    status: DoctorStatus,
    recent_files: usize,
    total_files: usize,
    project_count: usize,
    latest_chunk: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorDaemonSummary {
    status: DoctorStatus,
    mode: String,
    socket_path: Option<String>,
    phase: Option<String>,
    detail: Option<String>,
    last_cycle_summary: Option<String>,
    last_error: Option<String>,
    bootstrap_completed: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorReport {
    generated_at: String,
    hours: u64,
    project_filter: Option<String>,
    sources: DoctorSourcesSummary,
    canonical_store: DoctorStoreSummary,
    daemon: DoctorDaemonSummary,
    next_steps: Vec<String>,
}

struct DaemonProbe {
    mode: &'static str,
    snapshot: Option<DaemonStatusSnapshot>,
    error: Option<String>,
}

#[derive(Debug, Default)]
struct DoctorRepairOutcome {
    actions: Vec<String>,
    warnings: Vec<String>,
}

fn run_doctor(hours: u64, project: Option<String>, json: bool, fix: bool) -> Result<()> {
    let report = build_doctor_report(hours, project.clone())?;
    if json {
        println!("{}", serde_json::to_string(&report)?);
    } else if fix {
        println!("Pre-fix check:");
        print_doctor_report(&report);
        println!();
        let repair = run_doctor_fix(&report);
        print_doctor_fix_outcome(&repair);
        println!();
        println!("Post-fix check:");
        let post_fix_report = build_doctor_report(hours, project)?;
        print_doctor_report(&post_fix_report);
    } else {
        print_doctor_report(&report);
    }
    Ok(())
}

fn run_front_door(hours: u64, project: Option<String>) -> Result<()> {
    let report = build_doctor_report(hours, project)?;
    print!("{}", render_front_door(&report));
    Ok(())
}

fn build_doctor_report(hours: u64, project: Option<String>) -> Result<DoctorReport> {
    let sources = summarize_sources(&sources::list_available_sources()?);
    let canonical_store = summarize_store(hours, project.as_deref())?;
    let daemon = summarize_daemon(&probe_daemon_status(None)?);

    let mut report = DoctorReport {
        generated_at: Utc::now().to_rfc3339(),
        hours,
        project_filter: project,
        sources,
        canonical_store,
        daemon,
        next_steps: Vec::new(),
    };
    report.next_steps = doctor_next_steps(&report);
    Ok(report)
}

fn render_front_door(report: &DoctorReport) -> String {
    let mut lines = Vec::new();
    lines.push("aicx".to_string());
    lines.push("  guided front door for agent session history".to_string());
    lines.push("  full command list: aicx --help".to_string());
    lines.push(format!("  window: last {}h", report.hours));
    if let Some(project) = &report.project_filter {
        lines.push(format!("  project filter: {project}"));
    }
    lines.push(format!(
        "  verdict: {}",
        doctor_verdict_label(doctor_overall_status(report))
    ));
    lines.push(format!("  summary: {}", doctor_overall_summary(report)));
    if let Some(latest) = &report.canonical_store.latest_chunk {
        lines.push(format!("  latest chunk: {latest}"));
    }
    lines.push(String::new());
    lines.push("Start here:".to_string());
    lines.push("  - aicx doctor".to_string());
    lines.push("    Full readiness report with the detailed section breakdown.".to_string());
    for step in report.next_steps.iter().take(3) {
        lines.push(format!("  - {step}"));
    }
    lines.push(String::new());
    lines.push("Useful shortcuts:".to_string());
    lines.push("  - aicx dashboard --open".to_string());
    lines.push("    Local browser snapshot when terminal-first is not the right mood.".to_string());
    lines.push(format!(
        "  - {}",
        front_door_latest_command(report.project_filter.as_deref())
    ));
    lines.push("    Fastest way back to the newest readable chunks.".to_string());
    lines.push(format!(
        "  - {}",
        front_door_search_command(report.project_filter.as_deref())
    ));
    lines.push(
        "    Search the saved canonical corpus without needing the daemon first.".to_string(),
    );
    lines.push(String::new());
    lines.join("\n")
}

fn front_door_latest_command(project: Option<&str>) -> String {
    match project {
        Some(project) => format!("aicx latest --project {project}"),
        None => "aicx latest".to_string(),
    }
}

fn front_door_search_command(project: Option<&str>) -> String {
    match project {
        Some(project) => format!("aicx search \"query\" --project {project}"),
        None => "aicx search \"query\"".to_string(),
    }
}

fn summarize_sources(found_sources: &[sources::SourceInfo]) -> DoctorSourcesSummary {
    let mut families: BTreeMap<String, DoctorSourceFamily> = BTreeMap::new();
    let mut total_sessions = 0usize;
    let mut total_size_bytes = 0u64;

    for source in found_sources {
        total_sessions += source.sessions;
        total_size_bytes += source.size_bytes;

        let family = source_family_name(&source.agent).to_string();
        let entry = families
            .entry(family.clone())
            .or_insert_with(|| DoctorSourceFamily {
                family,
                locations: 0,
                sessions: 0,
                size_bytes: 0,
            });
        entry.locations += 1;
        entry.sessions += source.sessions;
        entry.size_bytes += source.size_bytes;
    }

    DoctorSourcesSummary {
        status: if found_sources.is_empty() {
            DoctorStatus::Missing
        } else {
            DoctorStatus::Ok
        },
        locations: found_sources.len(),
        total_sessions,
        total_size_bytes,
        families: families.into_values().collect(),
    }
}

fn summarize_store(hours: u64, project_filter: Option<&str>) -> Result<DoctorStoreSummary> {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600);
    let recent_files = store::context_files_since(cutoff, project_filter)?;
    let total_files =
        store::context_files_since(std::time::SystemTime::UNIX_EPOCH, project_filter)?;

    let latest_chunk = recent_files
        .last()
        .or_else(|| total_files.last())
        .map(|file| file.path.display().to_string());

    let project_count = total_files
        .iter()
        .map(|file| file.project.clone())
        .collect::<BTreeSet<_>>()
        .len();

    let status = if !recent_files.is_empty() {
        DoctorStatus::Ok
    } else if !total_files.is_empty() {
        DoctorStatus::Partial
    } else {
        DoctorStatus::Missing
    };

    Ok(DoctorStoreSummary {
        status,
        recent_files: recent_files.len(),
        total_files: total_files.len(),
        project_count,
        latest_chunk,
    })
}

fn summarize_daemon(probe: &DaemonProbe) -> DoctorDaemonSummary {
    match &probe.snapshot {
        Some(snapshot) => DoctorDaemonSummary {
            status: match probe.mode {
                "live" => DoctorStatus::Ok,
                "busy" => DoctorStatus::Partial,
                "snapshot" => DoctorStatus::Partial,
                _ => DoctorStatus::Missing,
            },
            mode: probe.mode.to_string(),
            socket_path: Some(snapshot.socket_path.clone()),
            phase: Some(snapshot.phase.to_string()),
            detail: Some(snapshot.phase_detail.clone()),
            last_cycle_summary: snapshot.last_cycle_summary.clone(),
            last_error: snapshot.last_error.clone().or_else(|| probe.error.clone()),
            bootstrap_completed: Some(snapshot.bootstrap_completed),
        },
        None => DoctorDaemonSummary {
            status: DoctorStatus::Missing,
            mode: probe.mode.to_string(),
            socket_path: None,
            phase: None,
            detail: None,
            last_cycle_summary: None,
            last_error: probe.error.clone(),
            bootstrap_completed: None,
        },
    }
}

fn probe_daemon_status(socket_path: Option<&Path>) -> Result<DaemonProbe> {
    let resolved_socket = match socket_path {
        Some(path) => path.to_path_buf(),
        None => daemon::default_socket_path()?,
    };

    match daemon::request_status_at(&resolved_socket) {
        Ok(ControlOutcome::Status(snapshot)) => Ok(DaemonProbe {
            mode: "live",
            snapshot: Some(snapshot),
            error: None,
        }),
        Ok(other) => unreachable!(
            "status endpoint returned unexpected outcome: {}",
            other.message()
        ),
        Err(err) => {
            let fallback = daemon::load_last_known_status(socket_path)?;
            Ok(match fallback {
                Some(snapshot) => DaemonProbe {
                    mode: if daemon::is_control_plane_timeout(&err) {
                        "busy"
                    } else {
                        "snapshot"
                    },
                    snapshot: Some(snapshot),
                    error: Some(format!("{err:#}")),
                },
                None => DaemonProbe {
                    mode: "missing",
                    snapshot: None,
                    error: Some(format!("{err:#}")),
                },
            })
        }
    }
}

fn print_doctor_report(report: &DoctorReport) {
    println!("aicx doctor");
    println!("  window: last {}h", report.hours);
    if let Some(project) = &report.project_filter {
        println!("  project filter: {}", project);
    }
    println!(
        "  verdict: {}",
        doctor_verdict_label(doctor_overall_status(report))
    );
    println!("  summary: {}", doctor_overall_summary(report));
    println!();

    println!(
        "[{}] raw session history",
        doctor_status_label(report.sources.status)
    );
    println!(
        "  {} source locations, {} sessions, {} total",
        report.sources.locations,
        report.sources.total_sessions,
        format_bytes_compact(report.sources.total_size_bytes)
    );
    for family in &report.sources.families {
        println!(
            "  {}: {} locations, {} sessions, {}",
            family.family,
            family.locations,
            family.sessions,
            format_bytes_compact(family.size_bytes)
        );
    }
    if report.sources.status == DoctorStatus::Missing {
        println!("  expected sources live under ~/.claude/, ~/.codex/, and ~/.gemini/");
    }
    println!();

    println!(
        "[{}] saved AI context (~/.aicx/)",
        doctor_status_label(report.canonical_store.status)
    );
    println!(
        "  recent chunks: {} in the last {}h",
        report.canonical_store.recent_files, report.hours
    );
    println!(
        "  total chunks: {} across {} project(s)",
        report.canonical_store.total_files, report.canonical_store.project_count
    );
    if let Some(latest) = &report.canonical_store.latest_chunk {
        println!("  latest chunk: {}", latest);
    } else {
        println!("  latest chunk: none yet");
    }
    println!();

    println!(
        "[{}] background memex service",
        doctor_status_label(report.daemon.status)
    );
    match report.daemon.mode.as_str() {
        "live" => println!("  always-on semantic indexing is reachable right now"),
        "busy" => {
            println!("  background service is busy right now; showing the last known snapshot")
        }
        "snapshot" => {
            println!("  background service is offline right now, showing the last known snapshot")
        }
        _ => println!("  background service is not running yet; file-backed search still works"),
    }
    if let Some(socket_path) = &report.daemon.socket_path {
        println!("  socket: {}", socket_path);
    }
    if let Some(phase) = &report.daemon.phase {
        println!("  phase: {}", phase);
    }
    if let Some(detail) = &report.daemon.detail {
        println!("  detail: {}", detail);
    }
    if let Some(summary) = &report.daemon.last_cycle_summary {
        println!("  last cycle: {}", summary);
    }
    if let Some(last_error) = &report.daemon.last_error {
        println!("  last error: {}", last_error);
    }
    if let Some(bootstrap_completed) = report.daemon.bootstrap_completed {
        println!(
            "  bootstrap: {}",
            if bootstrap_completed {
                "completed"
            } else {
                "not completed yet"
            }
        );
    }
    println!();

    println!("What to do next:");
    for step in &report.next_steps {
        println!("  - {}", step);
    }
}

fn run_doctor_fix(report: &DoctorReport) -> DoctorRepairOutcome {
    let mut outcome = DoctorRepairOutcome::default();
    let project = report
        .project_filter
        .as_ref()
        .map(|name| vec![name.clone()])
        .unwrap_or_default();
    let mut store_ready = report.canonical_store.status != DoctorStatus::Missing;

    if report.sources.status == DoctorStatus::Ok
        && report.canonical_store.status != DoctorStatus::Ok
    {
        println!("doctor --fix: refreshing saved AI context from visible sources...");
        match run_extraction(ExtractionParams {
            agents: &["claude", "codex", "gemini"],
            project,
            hours: report.hours,
            output_dir: None,
            format: "both",
            append_to: None,
            rotate: 0,
            incremental: true,
            include_assistant: true,
            include_loctree: false,
            project_root: None,
            sync_memex: true,
            force: false,
            conversation: false,
            redact_secrets: true,
            emit: StdoutEmit::None,
        }) {
            Ok(()) => {
                store_ready = true;
                outcome.actions.push(format!(
                    "Refreshed the saved AI context for the last {}h and synced memex from visible sources.",
                    report.hours
                ));
            }
            Err(err) => outcome.warnings.push(format!(
                "Could not refresh the saved AI context automatically: {err:#}"
            )),
        }
    } else if report.sources.status == DoctorStatus::Missing
        && report.canonical_store.status == DoctorStatus::Missing
    {
        outcome.warnings.push(
            "No raw agent session logs are visible yet, so aicx cannot build the saved AI context automatically."
                .to_string(),
        );
    }

    if store_ready && report.daemon.status != DoctorStatus::Ok {
        println!("doctor --fix: starting or nudging background memex service...");
        match daemon::ensure_running_and_kick(Some("doctor --fix".to_string())) {
            Ok(control) => outcome.actions.push(match control {
                ControlOutcome::Status(_) => {
                    "Background memex service is already reachable.".to_string()
                }
                ControlOutcome::SyncQueued(_) => {
                    "Started or nudged background memex service and queued a sync.".to_string()
                }
                ControlOutcome::SyncAlreadyQueued(_) => {
                    "Background memex service was already waking up; sync is already queued."
                        .to_string()
                }
                ControlOutcome::SyncAlreadyRunning(_) => {
                    "Background memex service is already running and syncing.".to_string()
                }
                ControlOutcome::StopQueued(_) => {
                    "Background memex service reported a stop request instead of a sync."
                        .to_string()
                }
            }),
            Err(err) => outcome.warnings.push(format!(
                "Could not start or nudge the background memex service automatically: {err:#}"
            )),
        }
    }

    if outcome.actions.is_empty() && outcome.warnings.is_empty() {
        outcome
            .actions
            .push("Nothing had to change: the operator surface is already ready.".to_string());
    }

    outcome
}

fn print_doctor_fix_outcome(outcome: &DoctorRepairOutcome) {
    println!("Doctor repair:");
    for action in &outcome.actions {
        println!("  - {}", action);
    }
    for warning in &outcome.warnings {
        println!("  warning: {}", warning);
    }
}

fn doctor_next_steps(report: &DoctorReport) -> Vec<String> {
    let mut steps = Vec::new();
    let store_ready = report.canonical_store.status != DoctorStatus::Missing;

    if report.sources.status == DoctorStatus::Missing {
        if store_ready {
            steps.push(
                "Open Claude Code, Codex, or Gemini once if you want fresh sessions to land automatically, then rerun `aicx doctor`."
                    .to_string(),
            );
        } else {
            steps.push(
                "Open Claude Code, Codex, or Gemini once so local session logs exist, then rerun `aicx doctor`."
                    .to_string(),
            );
            steps.push(
                "For a one-off file, use `aicx extract --format claude /path/to/session.jsonl -o /tmp/report.md`."
                    .to_string(),
            );
        }
    }

    if report.canonical_store.status == DoctorStatus::Missing {
        if report.sources.status == DoctorStatus::Missing {
            steps.push(format!(
                "Once logs are available, run `aicx all -H {} --incremental --memex` to build the saved AI context and seed semantic search.",
                report.hours
            ));
        } else {
            steps.push(format!(
                "Run `aicx all -H {} --incremental --memex` to build the saved AI context and seed semantic search in one pass.",
                report.hours
            ));
        }
    } else if report.canonical_store.status == DoctorStatus::Partial {
        steps.push(format!(
            "Run `aicx all -H {} --incremental --memex` to refresh recent history and catch semantic search up.",
            report.hours
        ));
    }

    if doctor_can_self_heal(report) {
        steps.push(
            "Prefer the one-command repair path? Run `aicx doctor --fix` and let aicx repair what it can automatically."
                .to_string(),
        );
    }

    if store_ready {
        steps.push(
            "Prefer a browser surface? Run `aicx dashboard --open` for a local snapshot or `aicx dashboard-serve --open` for a live local UI."
                .to_string(),
        );
    }

    if report.daemon.mode == "busy" && store_ready {
        steps.push(
            "You can already use `aicx latest` for the newest chunks or `aicx search \"query\"` against the saved file-backed context right now."
                .to_string(),
        );
        steps.push(
            "Background indexing is already working on a cycle; let it finish, then rerun `aicx doctor`."
                .to_string(),
        );
    } else if report.daemon.status != DoctorStatus::Ok && store_ready {
        steps.push(
            "You can already use `aicx latest` for the newest chunks or `aicx search \"query\"` against the saved file-backed context right now."
                .to_string(),
        );
        steps.push(
            "Optional but recommended: run `aicx-memex daemon` to keep semantic search and metadata repair fresh in the background."
                .to_string(),
        );
    }

    if steps.is_empty() {
        let steer_hint = report
            .project_filter
            .as_deref()
            .map(|project| format!("`aicx steer --project {project}`"))
            .unwrap_or_else(|| "`aicx steer --project <your-project>`".to_string());
        steps.push(format!(
            "You're ready: use `aicx latest` for the newest chunks, `aicx search \"query\"` for fast recall, or {steer_hint} for metadata-grounded retrieval."
        ));
    }

    steps
}

fn doctor_can_self_heal(report: &DoctorReport) -> bool {
    (report.sources.status == DoctorStatus::Ok && report.canonical_store.status != DoctorStatus::Ok)
        || (report.canonical_store.status != DoctorStatus::Missing
            && report.daemon.status != DoctorStatus::Ok
            && report.daemon.mode != "busy")
}

fn doctor_overall_status(report: &DoctorReport) -> DoctorStatus {
    if report.canonical_store.status == DoctorStatus::Missing {
        DoctorStatus::Missing
    } else if report.sources.status != DoctorStatus::Ok
        || report.canonical_store.status != DoctorStatus::Ok
        || report.daemon.status != DoctorStatus::Ok
    {
        DoctorStatus::Partial
    } else {
        DoctorStatus::Ok
    }
}

fn doctor_overall_summary(report: &DoctorReport) -> String {
    if report.canonical_store.status == DoctorStatus::Missing {
        if report.sources.status == DoctorStatus::Missing {
            "No saved AI context exists yet, and no raw session logs are visible yet.".to_string()
        } else {
            "Raw session logs are visible, but the saved AI context in ~/.aicx/ still needs its first refresh."
                .to_string()
        }
    } else if report.sources.status == DoctorStatus::Missing {
        "Saved AI context already exists, but no fresh raw session locations are visible right now. You can still search what is already in ~/.aicx/.".to_string()
    } else if report.canonical_store.status == DoctorStatus::Partial {
        format!(
            "Saved AI context exists, but nothing new landed in the last {}h window yet.",
            report.hours
        )
    } else if report.daemon.status != DoctorStatus::Ok {
        "Raw session history and saved AI context are ready. You can work now, and the daemon is only needed for always-on semantic indexing and repair.".to_string()
    } else {
        "Sources, saved AI context, and background indexing are all ready.".to_string()
    }
}

fn doctor_verdict_label(status: DoctorStatus) -> &'static str {
    match status {
        DoctorStatus::Ok => "ready",
        DoctorStatus::Partial => "usable, but not fully automatic yet",
        DoctorStatus::Missing => "needs setup",
    }
}

fn doctor_status_label(status: DoctorStatus) -> &'static str {
    match status {
        DoctorStatus::Ok => "OK",
        DoctorStatus::Partial => "PARTIAL",
        DoctorStatus::Missing => "MISSING",
    }
}

fn source_family_name(agent: &str) -> &'static str {
    if agent.contains("claude") {
        "Claude"
    } else if agent.contains("codex") {
        "Codex"
    } else if agent.contains("gemini") {
        "Gemini"
    } else {
        "Other"
    }
}

fn format_bytes_compact(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit_idx = 0usize;

    while value >= 1024.0 && unit_idx < UNITS.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[unit_idx])
    } else {
        format!("{value:.1} {}", UNITS[unit_idx])
    }
}

fn run_daemon_status(socket_path: Option<&Path>, json: bool) -> Result<()> {
    let resolved_socket = match socket_path {
        Some(path) => path.to_path_buf(),
        None => daemon::default_socket_path()?,
    };
    let live = daemon::request_status_at(&resolved_socket);

    match live {
        Ok(ControlOutcome::Status(snapshot)) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
            } else {
                print_daemon_status(&snapshot, "running");
            }
            Ok(())
        }
        Err(err) => {
            let fallback = daemon::load_last_known_status(socket_path)?;
            if let Some(snapshot) = fallback {
                if json {
                    println!("{}", serde_json::to_string_pretty(&snapshot)?);
                } else if daemon::is_control_plane_timeout(&err) {
                    eprintln!("Daemon is busy right now; showing the last known state.");
                    print_daemon_status(&snapshot, "busy (last known state)");
                } else {
                    eprintln!("Daemon is not currently reachable: {err:#}");
                    print_daemon_status(&snapshot, "offline (last known state)");
                }
                Ok(())
            } else {
                Err(err)
            }
        }
        Ok(other) => unreachable!(
            "status endpoint returned unexpected outcome: {}",
            other.message()
        ),
    }
}

fn run_daemon_sync(socket_path: Option<&Path>) -> Result<()> {
    let outcome = daemon::request_sync(socket_path, Some("manual CLI request".to_string()))?;
    match &outcome {
        ControlOutcome::SyncQueued(snapshot)
        | ControlOutcome::SyncAlreadyQueued(snapshot)
        | ControlOutcome::SyncAlreadyRunning(snapshot) => {
            eprintln!("✓ {}", outcome.message());
            eprintln!("  Phase: {}", snapshot.phase);
        }
        ControlOutcome::Status(_) | ControlOutcome::StopQueued(_) => {}
    }
    Ok(())
}

fn run_daemon_stop(socket_path: Option<&Path>) -> Result<()> {
    let outcome = daemon::request_stop(socket_path)?;
    if let ControlOutcome::StopQueued(snapshot) = outcome {
        eprintln!("✓ stop queued");
        eprintln!("  PID: {}", snapshot.pid);
    }
    Ok(())
}

fn print_daemon_status(snapshot: &DaemonStatusSnapshot, state_label: &str) {
    println!("aicx-memex daemon: {}", state_label);
    println!("  pid: {}", snapshot.pid);
    println!("  socket: {}", snapshot.socket_path);
    println!("  phase: {}", snapshot.phase);
    println!("  detail: {}", snapshot.phase_detail);
    println!("  started_at: {}", snapshot.started_at);
    println!("  poll_seconds: {}", snapshot.poll_seconds);
    println!("  refresh_hours: {}", snapshot.refresh_hours);
    println!("  namespace: {}", snapshot.namespace);
    println!(
        "  projects: {}",
        if snapshot.projects.is_empty() {
            "all".to_string()
        } else {
            snapshot.projects.join(", ")
        }
    );
    println!(
        "  last_cycle_started_at: {}",
        snapshot
            .last_cycle_started_at
            .map(|ts| ts.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  last_cycle_completed_at: {}",
        snapshot
            .last_cycle_completed_at
            .map(|ts| ts.to_rfc3339())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  last_cycle_reason: {}",
        snapshot.last_cycle_reason.as_deref().unwrap_or("-")
    );
    println!(
        "  last_cycle_summary: {}",
        snapshot.last_cycle_summary.as_deref().unwrap_or("-")
    );
    println!(
        "  last_error: {}",
        snapshot.last_error.as_deref().unwrap_or("-")
    );
    println!(
        "  cycles: {} ok / {} failed",
        snapshot.successful_cycles, snapshot.failed_cycles
    );
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

struct MemexProgressPrinter {
    enabled: bool,
    width: usize,
}

impl MemexProgressPrinter {
    fn new() -> Self {
        Self {
            enabled: io::stderr().is_terminal(),
            width: 0,
        }
    }

    fn update(&mut self, progress: &SyncProgress) {
        if !self.enabled {
            return;
        }

        let message = render_memex_progress(progress);
        let width = self.width.max(message.len());
        self.width = width;
        eprint!("\r{message:<width$}");
        let _ = io::stderr().flush();
    }

    fn finish(&mut self) {
        if self.enabled && self.width > 0 {
            eprint!("\r{:<width$}\r", "", width = self.width);
            let _ = io::stderr().flush();
            self.width = 0;
        }
    }
}

fn render_memex_progress(progress: &SyncProgress) -> String {
    match progress.phase {
        SyncProgressPhase::Discovering => {
            format!(
                "  Memex scan... {}/{}",
                progress.done.max(1),
                progress.total.max(1)
            )
        }
        SyncProgressPhase::Embedding => {
            format!(
                "  Memex embed... {}/{}",
                progress.done.max(1),
                progress.total.max(1)
            )
        }
        SyncProgressPhase::Writing => {
            format!(
                "  Memex index... {}/{}",
                progress.done.max(1),
                progress.total.max(1)
            )
        }
        SyncProgressPhase::Completed => format!("  {}", progress.detail),
    }
}

fn sync_memex_paths(config: &MemexConfig, chunk_paths: &[PathBuf]) -> Result<memex::SyncResult> {
    let mut printer = MemexProgressPrinter::new();
    let enabled = printer.enabled;
    let result = if enabled {
        memex::sync_new_chunk_paths_with_progress(chunk_paths, config, |progress| {
            printer.update(&progress);
        })
    } else {
        memex::sync_new_chunk_paths(chunk_paths, config)
    };
    printer.finish();
    result
}

fn sync_memex_if_requested(sync_memex: bool, all_written_paths: &[PathBuf]) -> Result<()> {
    if sync_memex && !all_written_paths.is_empty() {
        let memex_config = MemexConfig::default();
        // Keep extractor/store `--memex` on the same stateful transport seam as
        // the dedicated `memex-sync` command so sync state and observability do
        // not drift between code paths.
        let result = sync_memex_paths(&memex_config, all_written_paths)
            .context("Failed to sync canonical chunks to external dependency rmcp-memex")?;
        eprintln!(
            "  Memex: {} pushed, {} skipped, {} ignored",
            result.chunks_pushed, result.chunks_skipped, result.chunks_ignored
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
    let _ = daemon::ensure_running_and_kick(Some("cli search refresh".to_string()));

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

#[derive(Debug, Clone, Serialize)]
struct LatestChunkItem {
    store_ref: String,
    path: String,
    project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    kind: String,
    agent: String,
    date: String,
    event_time: String,
    session_id: String,
    chunk: u32,
    preview: String,
    preview_truncated: bool,
}

struct LatestChunkCandidate {
    store_ref: String,
    event_time: String,
    file: store::StoredContextFile,
}

fn run_latest(
    hours: u64,
    project: Option<String>,
    limit: usize,
    strict: bool,
    json: bool,
) -> Result<()> {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600);
    let mut files = store::context_files_since(cutoff, project.as_deref())?;
    if strict {
        files.retain(|file| !is_noise_artifact(&file.path));
    }

    let store_base = store::store_base_dir()?;
    let mut candidates = files
        .into_iter()
        .map(|file| build_latest_candidate(&store_base, file))
        .collect::<Result<Vec<_>>>()?;

    candidates.sort_by(|left, right| {
        right
            .event_time
            .cmp(&left.event_time)
            .then_with(|| right.file.date_iso.cmp(&left.file.date_iso))
            .then_with(|| right.file.session_id.cmp(&left.file.session_id))
            .then_with(|| right.file.chunk.cmp(&left.file.chunk))
            .then_with(|| right.store_ref.cmp(&left.store_ref))
    });

    if limit > 0 && candidates.len() > limit {
        candidates.truncate(limit);
    }

    let items = candidates
        .into_iter()
        .map(latest_item_from_candidate)
        .collect::<Result<Vec<_>>>()?;

    if json {
        println!("{}", serde_json::to_string(&items)?);
        return Ok(());
    }

    print_latest_items(hours, project.as_deref(), strict, &items)
}

fn build_latest_candidate(
    store_base: &Path,
    file: store::StoredContextFile,
) -> Result<LatestChunkCandidate> {
    let store_ref = chunk_store_ref(store_base, &file.path)?;
    let sidecar = store::load_sidecar(&file.path);
    let event_time = sidecar
        .as_ref()
        .and_then(|meta| meta.completed_at.clone())
        .or_else(|| sidecar.as_ref().and_then(|meta| meta.started_at.clone()))
        .unwrap_or_else(|| format!("{}T00:00:00Z", file.date_iso));

    Ok(LatestChunkCandidate {
        store_ref,
        event_time,
        file,
    })
}

fn latest_item_from_candidate(candidate: LatestChunkCandidate) -> Result<LatestChunkItem> {
    let (preview, preview_truncated) = latest_chunk_preview(&candidate.file.path)?;

    Ok(LatestChunkItem {
        store_ref: candidate.store_ref,
        path: candidate.file.path.display().to_string(),
        project: candidate.file.project,
        repo: candidate.file.repo.as_ref().map(|repo| repo.slug()),
        kind: candidate.file.kind.dir_name().to_string(),
        agent: candidate.file.agent,
        date: candidate.file.date_iso,
        event_time: candidate.event_time,
        session_id: candidate.file.session_id,
        chunk: candidate.file.chunk,
        preview,
        preview_truncated,
    })
}

fn print_latest_items(
    hours: u64,
    project: Option<&str>,
    strict: bool,
    items: &[LatestChunkItem],
) -> Result<()> {
    println!("aicx latest");
    println!("  window: last {}h", hours);
    if let Some(project) = project {
        println!("  project filter: {}", project);
    }
    println!(
        "  mode: {}",
        if strict {
            "noise-filtered"
        } else {
            "all stored chunks"
        }
    );
    println!("  results: {}", items.len());
    println!();

    if items.is_empty() {
        println!("No stored chunks matched this latest view.");
        return Ok(());
    }

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    for (index, item) in items.iter().enumerate() {
        writeln!(
            out,
            "{}. {} | {} | {} | {}",
            index + 1,
            item.project,
            item.agent,
            item.date,
            item.kind
        )?;
        if let Some(repo) = &item.repo {
            writeln!(out, "   repo: {}", repo)?;
        }
        writeln!(out, "   ref: {}", item.store_ref)?;
        writeln!(out, "   event_time: {}", item.event_time)?;
        writeln!(
            out,
            "   session: {}  chunk: {}",
            item.session_id, item.chunk
        )?;
        writeln!(out, "   preview: {}", item.preview)?;
        if item.preview_truncated {
            writeln!(out, "   hint: aicx read {}", item.store_ref)?;
        }
        writeln!(out)?;
    }

    if let Some(first) = items.first() {
        writeln!(out, "Open one now: aicx read {}", first.store_ref)?;
    }
    out.flush()?;
    Ok(())
}

fn latest_chunk_preview(path: &Path) -> Result<(String, bool)> {
    const MAX_LINES: usize = 3;
    const MAX_CHARS: usize = 220;

    let content = ai_contexters::sanitize::read_to_string_validated(path)?;
    let mut preview_lines = Vec::new();
    let mut in_frontmatter = false;
    let mut skipped_frontmatter = false;

    for raw_line in content.lines() {
        let mut line = raw_line.trim();
        if line.is_empty() || line.starts_with("[project:") {
            continue;
        }

        if line == "---" {
            skipped_frontmatter = true;
            in_frontmatter = !in_frontmatter;
            continue;
        }
        if in_frontmatter {
            continue;
        }

        if line == "[signals]" || line == "[/signals]" {
            continue;
        }
        if let Some(rest) = line.strip_prefix("[signals]") {
            line = rest.trim();
            if line.is_empty() {
                continue;
            }
        }
        if line.starts_with('[') {
            if let Some((_, rest)) = line.split_once("] ") {
                line = rest.trim();
            }
        }
        if line.is_empty() {
            continue;
        }

        preview_lines.push(line.to_string());
        if preview_lines.len() >= MAX_LINES {
            break;
        }
    }

    if preview_lines.is_empty() && skipped_frontmatter {
        preview_lines
            .push("Frontmatter-only chunk; open with `aicx read` for full details.".to_string());
    } else if preview_lines.is_empty() {
        preview_lines.push("No readable preview available.".to_string());
    }

    let joined = preview_lines.join(" ");
    if joined.chars().count() > MAX_CHARS {
        let shortened = joined
            .chars()
            .take(MAX_CHARS.saturating_sub(1))
            .collect::<String>();
        Ok((format!("{shortened}..."), true))
    } else {
        Ok((joined, false))
    }
}

fn chunk_store_ref(store_base: &Path, path: &Path) -> Result<String> {
    let store_base = ai_contexters::sanitize::validate_dir_path(store_base)?;
    let path = ai_contexters::sanitize::validate_read_path(path)?;

    path.strip_prefix(&store_base)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|_| anyhow::anyhow!("Chunk path does not live under {}", store_base.display()))
}

fn run_read(target: &str, max_chars: usize, max_lines: usize, json: bool) -> Result<()> {
    let chunk = store::read_stored_chunk(
        target,
        store::ReadChunkOptions {
            max_chars,
            max_lines,
        },
    )?;

    if json {
        println!("{}", serde_json::to_string(&chunk)?);
        return Ok(());
    }

    println!("aicx read");
    println!("  ref: {}", chunk.store_ref);
    println!("  path: {}", chunk.path);
    println!("  project: {}", chunk.project);
    if let Some(repo) = &chunk.repo {
        println!("  repo: {}", repo);
    }
    println!("  kind: {}", chunk.kind.dir_name());
    println!("  agent: {}", chunk.agent);
    println!("  date: {}", chunk.date);
    println!("  session_id: {}", chunk.session_id);
    println!("  chunk: {}", chunk.chunk);
    if let Some(run_id) = &chunk.run_id {
        println!("  run_id: {}", run_id);
    }
    if let Some(prompt_id) = &chunk.prompt_id {
        println!("  prompt_id: {}", prompt_id);
    }
    if let Some(model) = &chunk.agent_model {
        println!("  model: {}", model);
    }
    if let Some(phase) = &chunk.workflow_phase {
        println!("  phase: {}", phase);
    }
    if let Some(mode) = &chunk.mode {
        println!("  mode: {}", mode);
    }
    if chunk.truncated {
        println!(
            "  content: truncated from {} lines / {} chars",
            chunk.original_lines, chunk.original_chars
        );
    } else {
        println!(
            "  content: full chunk ({} lines / {} chars)",
            chunk.original_lines, chunk.original_chars
        );
    }
    println!();
    print!("{}", chunk.content);
    if !chunk.content.ends_with('\n') {
        println!();
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
    let _ = daemon::ensure_running_and_kick(Some("cli steer refresh".to_string()));

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
    let truth = memex::resolve_runtime_truth(db_path.as_deref())?;
    let store_root = store::store_base_dir()?;

    let canonical_root = store::canonical_store_dir()?;
    let chunk_paths: Vec<PathBuf> = store::scan_context_files_raw()?
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
    let ignore_path = store_root.join(store::AICX_IGNORE_FILENAME);
    if ignore_path.is_file() {
        eprintln!("  Ignore file: {}", ignore_path.display());
    }
    eprintln!(
        "  Mode: {}",
        if config.batch_mode {
            "batch store (library-backed, metadata-rich)"
        } else {
            "per-chunk store (library-backed)"
        }
    );

    if reindex {
        eprintln!("  Reindex: wiping current rmcp-memex store before rebuild");
        eprintln!(
            "  Warning: Lance vector schema is shared across the whole store, so other namespaces in {} will need a rebuild too.",
            truth.db_path.display()
        );
        memex::reset_semantic_index(namespace, db_path.as_deref())?;
    }

    let result = sync_memex_paths(&config, &chunk_paths)?;

    eprintln!(
        "✓ Memex sync: {} pushed, {} skipped, {} ignored",
        result.chunks_pushed, result.chunks_skipped, result.chunks_ignored,
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
    open: bool,
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
    let dashboard_url = format!("http://{host}:{}", args.port);

    if args.open {
        let url = dashboard_url.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(450));
            if let Err(err) = open_browser_target(&url) {
                eprintln!(
                    "  Warning: dashboard server is running, but the browser could not be opened automatically: {err:#}"
                );
                eprintln!("  Open this URL manually: {url}");
            }
        });
    }

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
    open: bool,
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

    if args.open {
        let browser_path = output_path
            .canonicalize()
            .unwrap_or_else(|_| output_path.clone());
        let browser_target = browser_path.to_string_lossy().into_owned();
        match open_browser_target(&browser_target) {
            Ok(()) => eprintln!("  Opening: {}", browser_path.display()),
            Err(err) => {
                eprintln!(
                    "  Warning: dashboard was generated, but the browser could not be opened automatically: {err:#}"
                );
                eprintln!("  Open this file manually: {}", browser_path.display());
            }
        }
    }

    println!("{}", output_path.display());
    Ok(())
}

fn open_browser_target(target: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(target)
            .spawn()
            .with_context(|| format!("Failed to launch `open` for {target}"))?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", target])
            .spawn()
            .with_context(|| format!("Failed to launch `start` for {target}"))?;
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(target)
            .spawn()
            .with_context(|| format!("Failed to launch `xdg-open` for {target}"))?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err(anyhow::anyhow!(
        "Automatic browser opening is not supported on this platform yet. Open it manually: {target}"
    ))
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
    fn render_memex_progress_formats_live_stages() {
        assert_eq!(
            render_memex_progress(&SyncProgress {
                phase: SyncProgressPhase::Discovering,
                done: 12,
                total: 48,
                detail: String::new(),
            }),
            "  Memex scan... 12/48"
        );
        assert_eq!(
            render_memex_progress(&SyncProgress {
                phase: SyncProgressPhase::Embedding,
                done: 64,
                total: 256,
                detail: String::new(),
            }),
            "  Memex embed... 64/256"
        );
        assert_eq!(
            render_memex_progress(&SyncProgress {
                phase: SyncProgressPhase::Writing,
                done: 128,
                total: 256,
                detail: String::new(),
            }),
            "  Memex index... 128/256"
        );
    }

    #[test]
    fn render_memex_progress_passes_completed_detail_through() {
        assert_eq!(
            render_memex_progress(&SyncProgress {
                phase: SyncProgressPhase::Completed,
                done: 0,
                total: 0,
                detail: "Completed: 10 pushed, 2 skipped, 3 ignored".to_string(),
            }),
            "  Completed: 10 pushed, 2 skipped, 3 ignored"
        );
    }

    fn sample_doctor_report(project_filter: Option<&str>) -> DoctorReport {
        let project_filter = project_filter.map(str::to_string);
        let mut report = DoctorReport {
            generated_at: "2026-04-07T00:00:00Z".to_string(),
            hours: 72,
            project_filter,
            sources: DoctorSourcesSummary {
                status: DoctorStatus::Ok,
                locations: 1,
                total_sessions: 5,
                total_size_bytes: 1024,
                families: vec![DoctorSourceFamily {
                    family: "Codex".to_string(),
                    locations: 1,
                    sessions: 5,
                    size_bytes: 1024,
                }],
            },
            canonical_store: DoctorStoreSummary {
                status: DoctorStatus::Ok,
                recent_files: 4,
                total_files: 20,
                project_count: 1,
                latest_chunk: Some("/tmp/chunk.md".to_string()),
            },
            daemon: DoctorDaemonSummary {
                status: DoctorStatus::Missing,
                mode: "missing".to_string(),
                socket_path: None,
                phase: None,
                detail: None,
                last_cycle_summary: None,
                last_error: Some("socket missing".to_string()),
                bootstrap_completed: None,
            },
            next_steps: Vec::new(),
        };
        report.next_steps = doctor_next_steps(&report);
        report
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
    fn doctor_accepts_hours_project_and_json() {
        let cli = Cli::try_parse_from([
            "aicx",
            "doctor",
            "-H",
            "96",
            "--project",
            "ai-contexters",
            "--json",
        ])
        .expect("doctor command should parse");

        match cli.command {
            Some(Commands::Doctor {
                hours,
                project,
                json,
                fix,
            }) => {
                assert_eq!(hours, 96);
                assert_eq!(project.as_deref(), Some("ai-contexters"));
                assert!(json);
                assert!(!fix);
            }
            _ => panic!("expected doctor command"),
        }
    }

    #[test]
    fn doctor_accepts_fix_flag() {
        let cli = Cli::try_parse_from(["aicx", "doctor", "--fix"])
            .expect("doctor command with --fix should parse");

        match cli.command {
            Some(Commands::Doctor { fix, json, .. }) => {
                assert!(fix);
                assert!(!json);
            }
            _ => panic!("expected doctor command"),
        }
    }

    #[test]
    fn latest_accepts_limit_strict_and_json() {
        let cli = Cli::try_parse_from([
            "aicx",
            "latest",
            "-H",
            "240",
            "--project",
            "ai-contexters",
            "--limit",
            "7",
            "--strict",
            "--json",
        ])
        .expect("latest command should parse");

        match cli.command {
            Some(Commands::Latest {
                hours,
                project,
                limit,
                strict,
                json,
            }) => {
                assert_eq!(hours, 240);
                assert_eq!(project.as_deref(), Some("ai-contexters"));
                assert_eq!(limit, 7);
                assert!(strict);
                assert!(json);
            }
            _ => panic!("expected latest command"),
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
    fn read_accepts_ref_and_truncation_flags() {
        let cli = Cli::try_parse_from([
            "aicx",
            "read",
            "store/VetCoders/ai-contexters/2026_0331/reports/codex/2026_0331_codex_sess-read01_001.md",
            "--max-chars",
            "1200",
            "--max-lines",
            "40",
            "--json",
        ])
        .expect("read command should parse");

        match cli.command {
            Some(Commands::Read {
                target,
                max_chars,
                max_lines,
                json,
            }) => {
                assert!(target.contains("store/VetCoders/ai-contexters"));
                assert_eq!(max_chars, 1200);
                assert_eq!(max_lines, 40);
                assert!(json);
            }
            _ => panic!("expected read command"),
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
    fn top_level_help_includes_guided_examples() {
        let mut cmd = Cli::command();
        let rendered = cmd.render_help().to_string();

        assert!(rendered.contains("Most people want one of these:"));
        assert!(rendered.contains("  aicx"));
        assert!(rendered.contains("aicx doctor --fix"));
        assert!(rendered.contains("aicx dashboard --open"));
        assert!(rendered.contains("aicx latest -p <project>"));
        assert!(rendered.contains("aicx all -H 24 --incremental --memex"));
        assert!(rendered.contains("aicx search \"query\""));
        assert!(rendered.contains("aicx read <ref-or-path>"));
    }

    #[test]
    fn front_door_render_includes_guided_shortcuts() {
        let rendered = render_front_door(&sample_doctor_report(None));

        assert!(rendered.contains("guided front door"));
        assert!(rendered.contains("aicx --help"));
        assert!(rendered.contains("aicx doctor"));
        assert!(rendered.contains("aicx dashboard --open"));
        assert!(rendered.contains("aicx latest"));
        assert!(rendered.contains("aicx search \"query\""));
    }

    #[test]
    fn front_door_render_scopes_latest_and_search_when_project_is_set() {
        let rendered = render_front_door(&sample_doctor_report(Some("ai-contexters")));

        assert!(rendered.contains("project filter: ai-contexters"));
        assert!(rendered.contains("aicx latest --project ai-contexters"));
        assert!(rendered.contains("aicx search \"query\" --project ai-contexters"));
    }

    #[test]
    fn dashboard_accepts_open_flag() {
        let cli = Cli::try_parse_from(["aicx", "dashboard", "--open"])
            .expect("dashboard command with --open should parse");

        match cli.command {
            Some(Commands::Dashboard { open, .. }) => assert!(open),
            _ => panic!("expected dashboard command"),
        }
    }

    #[test]
    fn dashboard_serve_accepts_open_flag() {
        let cli = Cli::try_parse_from(["aicx", "dashboard-serve", "--open"])
            .expect("dashboard-serve command with --open should parse");

        match cli.command {
            Some(Commands::DashboardServe { open, .. }) => assert!(open),
            _ => panic!("expected dashboard-serve command"),
        }
    }

    #[test]
    fn doctor_verdict_is_partial_when_store_is_ready_but_daemon_is_missing() {
        let report = DoctorReport {
            generated_at: "2026-04-07T00:00:00Z".to_string(),
            hours: 72,
            project_filter: Some("ai-contexters".to_string()),
            sources: DoctorSourcesSummary {
                status: DoctorStatus::Ok,
                locations: 1,
                total_sessions: 5,
                total_size_bytes: 1024,
                families: vec![DoctorSourceFamily {
                    family: "Codex".to_string(),
                    locations: 1,
                    sessions: 5,
                    size_bytes: 1024,
                }],
            },
            canonical_store: DoctorStoreSummary {
                status: DoctorStatus::Ok,
                recent_files: 4,
                total_files: 20,
                project_count: 1,
                latest_chunk: Some("/tmp/chunk.md".to_string()),
            },
            daemon: DoctorDaemonSummary {
                status: DoctorStatus::Missing,
                mode: "missing".to_string(),
                socket_path: None,
                phase: None,
                detail: None,
                last_cycle_summary: None,
                last_error: None,
                bootstrap_completed: None,
            },
            next_steps: Vec::new(),
        };

        assert_eq!(doctor_overall_status(&report), DoctorStatus::Partial);
        assert_eq!(
            doctor_verdict_label(doctor_overall_status(&report)),
            "usable, but not fully automatic yet"
        );
        assert!(doctor_overall_summary(&report).contains("You can work now"));
    }

    #[test]
    fn doctor_next_steps_keep_search_available_when_daemon_is_missing() {
        let report = DoctorReport {
            generated_at: "2026-04-07T00:00:00Z".to_string(),
            hours: 24,
            project_filter: None,
            sources: DoctorSourcesSummary {
                status: DoctorStatus::Ok,
                locations: 1,
                total_sessions: 3,
                total_size_bytes: 2048,
                families: Vec::new(),
            },
            canonical_store: DoctorStoreSummary {
                status: DoctorStatus::Ok,
                recent_files: 3,
                total_files: 8,
                project_count: 1,
                latest_chunk: Some("/tmp/chunk.md".to_string()),
            },
            daemon: DoctorDaemonSummary {
                status: DoctorStatus::Missing,
                mode: "missing".to_string(),
                socket_path: None,
                phase: None,
                detail: None,
                last_cycle_summary: None,
                last_error: Some("socket missing".to_string()),
                bootstrap_completed: None,
            },
            next_steps: Vec::new(),
        };

        let steps = doctor_next_steps(&report);

        assert!(steps.iter().any(|step| step.contains("doctor --fix")));
        assert!(steps.iter().any(|step| step.contains("dashboard --open")));
        assert!(steps.iter().any(|step| step.contains("aicx search")));
        assert!(steps.iter().any(|step| step.contains("aicx-memex daemon")));
    }

    #[test]
    fn doctor_next_steps_treat_busy_daemon_as_in_progress() {
        let report = DoctorReport {
            generated_at: "2026-04-07T00:00:00Z".to_string(),
            hours: 24,
            project_filter: None,
            sources: DoctorSourcesSummary {
                status: DoctorStatus::Ok,
                locations: 1,
                total_sessions: 3,
                total_size_bytes: 2048,
                families: Vec::new(),
            },
            canonical_store: DoctorStoreSummary {
                status: DoctorStatus::Ok,
                recent_files: 3,
                total_files: 8,
                project_count: 1,
                latest_chunk: Some("/tmp/chunk.md".to_string()),
            },
            daemon: DoctorDaemonSummary {
                status: DoctorStatus::Partial,
                mode: "busy".to_string(),
                socket_path: Some("/tmp/aicx.sock".to_string()),
                phase: Some("refreshing_sources".to_string()),
                detail: Some("Refreshing canonical store (startup bootstrap)".to_string()),
                last_cycle_summary: None,
                last_error: Some("timed out".to_string()),
                bootstrap_completed: Some(false),
            },
            next_steps: Vec::new(),
        };

        let steps = doctor_next_steps(&report);

        assert!(
            steps
                .iter()
                .any(|step| step.contains("already working on a cycle"))
        );
        assert!(steps.iter().any(|step| step.contains("dashboard --open")));
        assert!(!steps.iter().any(|step| step.contains("doctor --fix")));
        assert!(!steps.iter().any(|step| step.contains("aicx-memex daemon")));
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
        assert!(rendered.contains("aicx doctor"));
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
