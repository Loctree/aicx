//! AI Contexters — the operator front door for agent session logs.
//!
//! `aicx` orchestrates a two-layer pipeline: canonical corpus first,
//! optional semantic index second. Materialization is always explicit.
//!
//! Two-layer architecture:
//!   1. **Canonical corpus** (`~/.aicx/`) — deduplicated, chunked, steerable markdown.
//!      Built by extractors (`claude`, `codex`, `all`) and `store`. This is ground truth.
//!   2. **Optional semantic index** — local embedding-backed retrieval for builds that
//!      opt into native embedder support. The corpus remains useful without it.
//!
//! Supported sources:
//! - Claude Code: ~/.claude/projects/*/*.jsonl
//! - Codex: ~/.codex/history.jsonl
//! - Gemini: ~/.gemini/tmp/<hash>/chats/session-*.json
//! - Gemini Antigravity: ~/.gemini/antigravity/{conversations/<uuid>.pb,brain/<uuid>/}
//! - Junie: ~/.junie/sessions/session-*/events.jsonl
//! - CodeScribe: ~/.codescribe/transcriptions/YYYY-MM-DD/*.{txt,md,json}
//! - Operator markdown: ~/Downloads/*.md, ~/.vibecrafted/inbox/*.md
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use aicx::corpus;
use aicx::dashboard::{self, DashboardConfig, DashboardScope};
use aicx::dashboard_server::{self, DashboardCorsPolicy, DashboardServerConfig};
use aicx::intents;
use aicx::mcp::{self, McpTransport};
use aicx::output::{self, OutputConfig, OutputFormat, OutputMode, ReportMetadata};
use aicx::rank;
use aicx::reports_extractor::{self, ReportsExtractorConfig};
use aicx::sources::{self, ExtractionConfig};
use aicx::state::StateManager;
use aicx::store;
use aicx::timeline;

fn print_intent_schema_migration_report(report: &intents::MigrationReport) {
    eprintln!("=== Intent Schema Migration (dry run) ===");
    eprintln!("Chunks scanned:   {}", report.total_chunks);
    eprintln!("Entries found:    {}", report.entries_found);
    eprintln!("Unresolved:       {}", report.unresolved_count);
    eprintln!();
    eprintln!("Per type:");
    let mut types: Vec<_> = report.per_type.iter().collect();
    types.sort_by(|a, b| b.1.cmp(a.1));
    for (t, count) in &types {
        eprintln!("  {:<12} {}", t, count);
    }
    eprintln!();
    eprintln!("Per project:");
    let mut projects: Vec<_> = report.per_project.iter().collect();
    projects.sort_by(|a, b| b.1.cmp(a.1));
    for (p, count) in &projects {
        eprintln!("  {:<30} {}", p, count);
    }
}

/// aicx — operator front door for agent session logs.
///
/// Two-layer pipeline, both operator-driven:
///   Layer 1 (canonical corpus): extract, deduplicate, and chunk agent logs
///     into steerable markdown at ~/.aicx/. This is ground truth.
///   Layer 2 (optional semantic index): local embedding-backed retrieval for native builds,
///     while the canonical corpus stays portable and useful without it.
/// Quick start:
///   aicx all -H 4                      # build canonical corpus (layer 1)
#[derive(Debug, Parser)]
#[command(name = "aicx")]
#[command(author = "M&K (c)2026 VetCoders")]
#[command(version)]
#[command(verbatim_doc_comment)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Clone, Copy, Debug, Args)]
struct RedactionArgs {
    /// Redact secrets (tokens/keys) from outputs before writing/syncing.
    ///
    /// Use `--no-redact-secrets` to disable (not recommended).
    #[arg(
        long = "no-redact-secrets",
        action = ArgAction::SetFalse,
        default_value_t = true
    )]
    redact_secrets: bool,
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
enum CorpusEmit {
    /// Print a readable text report.
    Text,
    /// Print compact JSON.
    Json,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExtractInputFormat {
    Claude,
    Codex,
    Gemini,
    GeminiAntigravity,
    Junie,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum SortOrder {
    Newest,
    Oldest,
    Score,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
#[value(rename_all = "snake_case")]
enum FrameKindArg {
    UserMsg,
    AgentReply,
    InternalThought,
    ToolCall,
}

impl From<FrameKindArg> for timeline::FrameKind {
    fn from(value: FrameKindArg) -> Self {
        match value {
            FrameKindArg::UserMsg => Self::UserMsg,
            FrameKindArg::AgentReply => Self::AgentReply,
            FrameKindArg::InternalThought => Self::InternalThought,
            FrameKindArg::ToolCall => Self::ToolCall,
        }
    }
}

const DEFAULT_DASHBOARD_TITLE: &str = "AICX Dashboard";
const DEFAULT_REPORTS_TITLE: &str = "AICX Report Explorer";

#[derive(Debug, Clone, ValueEnum)]
enum IngestSource {
    OperatorMd,
}

impl IngestSource {
    fn as_agent(&self) -> &'static str {
        match self {
            Self::OperatorMd => "operator-md",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SourceProtectionBackend {
    #[value(name = "git-local")]
    GitLocal,
}

impl SourceProtectionBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::GitLocal => "git-local",
        }
    }
}

#[derive(Debug, Subcommand)]
enum SourcesCommands {
    /// Opt in to local source-root protection.
    Protect {
        /// Source root to protect. Must be an existing directory.
        #[arg(long)]
        root: PathBuf,

        /// Protection backend to use.
        #[arg(long, value_enum, default_value_t = SourceProtectionBackend::GitLocal)]
        backend: SourceProtectionBackend,

        /// Apply the plan. Omit for a dry run.
        #[arg(long)]
        apply: bool,

        /// Create an initial local commit after git-local setup.
        #[arg(long)]
        initial_snapshot: bool,

        /// Do not add safe local .gitignore suggestions.
        #[arg(long)]
        no_gitignore: bool,
    },
}

#[derive(Debug, Args, Clone)]
struct RetrievalFilters {
    #[arg(long, default_value_t = 10)]
    limit: usize,
    #[arg(long, value_enum)]
    sort: Option<SortOrder>,
    #[arg(long)]
    score: Option<u8>,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    since: Option<String>,
    #[arg(long)]
    until: Option<String>,
    #[arg(long, value_enum)]
    frame_kind: Option<FrameKindArg>,
}

#[derive(Debug, Clone, Args)]
struct DashboardArgs {
    /// Run the live local HTTP dashboard instead of generating a static HTML file
    #[arg(long, conflicts_with = "generate_html")]
    serve: bool,

    /// Generate a standalone HTML file (default mode when no mode flag is passed)
    #[arg(long)]
    generate_html: bool,

    /// Store root directory (default: ~/.aicx)
    #[arg(long)]
    store_root: Option<PathBuf>,

    /// Narrow the dashboard dataset to project/store buckets containing this string
    #[arg(short, long)]
    project: Option<String>,

    /// Narrow the dashboard dataset to the last N hours (omit for all time)
    #[arg(short = 'H', long)]
    hours: Option<u64>,

    /// Output HTML path (default: ~/.aicx/aicx-dashboard.html)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Bind host IP address (default: 127.0.0.1, server mode only)
    #[arg(long, requires = "serve")]
    host: Option<String>,

    /// Bind TCP port (default: 9478, server mode only)
    #[arg(long, requires = "serve")]
    port: Option<u16>,

    /// Suppress automatic browser open on startup (server mode only)
    #[arg(long, requires = "serve")]
    no_open: bool,

    /// Detach the dashboard server into the background (`--serve` implies `--no-open`)
    #[arg(long, requires = "serve")]
    bg: bool,

    /// CORS origin policy for server mode: `local` (default), `tailscale`, `all`, or an explicit URL
    #[arg(long, requires = "serve", value_name = "PRESET|URL")]
    allow_cors_origins: Option<String>,

    /// Document title
    #[arg(long, default_value = DEFAULT_DASHBOARD_TITLE)]
    title: String,

    /// Max preview characters per record (0 = no truncation)
    #[arg(long, default_value = "320")]
    preview_chars: usize,
}

#[derive(Debug, Clone, Args)]
struct ReportsArgs {
    /// Vibecrafted artifact root (default: ~/.vibecrafted/artifacts)
    #[arg(long)]
    artifacts_root: Option<PathBuf>,

    /// Artifact organization bucket
    #[arg(long, default_value = "VetCoders")]
    org: String,

    /// Repository bucket (defaults to the current directory name)
    #[arg(long)]
    repo: Option<String>,

    /// Workflow filter (matches workflow label, skill code, run/prompt IDs, lane, and title)
    #[arg(long)]
    workflow: Option<String>,

    /// Inclusive start date (YYYY-MM-DD or YYYY_MMDD)
    #[arg(long)]
    date_from: Option<String>,

    /// Inclusive end date (YYYY-MM-DD or YYYY_MMDD)
    #[arg(long)]
    date_to: Option<String>,

    /// Output HTML path (default: ~/.aicx/aicx-reports.html)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Optional JSON bundle output path for later import/merge
    #[arg(long)]
    bundle_output: Option<PathBuf>,

    /// Document title
    #[arg(long, default_value = DEFAULT_REPORTS_TITLE)]
    title: String,

    /// Max preview characters per record (0 = no truncation)
    #[arg(long, default_value = "280")]
    preview_chars: usize,
}

#[derive(Debug, Clone, Args)]
struct CorpusArgs {
    #[command(subcommand)]
    command: CorpusCommand,
}

#[derive(Debug, Clone, Args)]
struct CorpusRootArgs {
    /// Corpus root(s) to scan. Defaults to $HOME/.aicx, $HOME/.ai-contexters, and optional $HOME/.xcia.
    #[arg(long, num_args = 1..)]
    root: Vec<PathBuf>,
}

#[derive(Debug, Clone, Args)]
struct CorpusAuditArgs {
    #[command(flatten)]
    roots: CorpusRootArgs,

    /// Output format: text or json.
    #[arg(long, value_enum, default_value_t = CorpusEmit::Text)]
    emit: CorpusEmit,
}

#[derive(Debug, Clone, Args)]
struct CorpusRepairArgs {
    #[command(flatten)]
    roots: CorpusRootArgs,

    /// Scan and report changes without modifying files. This is the default when --apply is omitted.
    #[arg(long)]
    dry_run: bool,

    /// Apply deterministic markdown repairs.
    #[arg(long, conflicts_with = "dry_run")]
    apply: bool,

    /// Write backups before applying repairs.
    #[arg(long)]
    backup: bool,

    /// Write the repair manifest to an explicit path, including dry-run previews.
    #[arg(long)]
    manifest: Option<PathBuf>,

    /// Output format: text or json.
    #[arg(long, value_enum, default_value_t = CorpusEmit::Text)]
    emit: CorpusEmit,
}

#[derive(Debug, Clone, Subcommand)]
enum CorpusCommand {
    /// Audit derived markdown corpora for Claude signature/thinking leakage and tool JSON noise.
    Audit(CorpusAuditArgs),

    /// Repair derived markdown without inventing or summarizing semantic content.
    Repair(CorpusRepairArgs),
}

#[derive(Debug, Clone, Args)]
struct DashboardServeLegacyArgs {
    /// Store root directory (default: ~/.aicx)
    #[arg(long)]
    store_root: Option<PathBuf>,

    /// Bind host IP address (loopback only; example: 127.0.0.1)
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Bind TCP port
    #[arg(long, default_value = "9478")]
    port: u16,

    /// Suppress automatic browser open on startup
    #[arg(long)]
    no_open: bool,

    /// Legacy compatibility path retained for status surfaces; not written in server mode
    #[arg(long, hide = true)]
    artifact: Option<PathBuf>,

    /// Document title
    #[arg(long, default_value = DEFAULT_DASHBOARD_TITLE)]
    title: String,

    /// Max preview characters per record (0 = no truncation)
    #[arg(long, default_value = "320")]
    preview_chars: usize,
}

/// Subcommands for `aicx config`.
#[derive(Debug, Clone, Subcommand)]
enum ConfigAction {
    /// Write a default `~/.aicx/config.toml` with cloud-embedder
    /// pre-selected. Bails if the file exists unless `--force`.
    Init {
        /// Overwrite the existing config file if present.
        #[arg(long)]
        force: bool,

        /// Write to a custom path instead of `~/.aicx/config.toml`.
        /// Useful for shared / repo-local config snapshots.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Display the resolved embedder configuration after merging env,
    /// `embedder.toml`, `config.toml`, and built-in defaults.
    Show {
        /// Emit JSON instead of human-readable text.
        #[arg(short = 'j', long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum IndexAction {
    /// Show freshness and pending-corpus status for the semantic index.
    Status {
        /// Repo or store-bucket filter (case-insensitive substring)
        #[arg(short, long)]
        project: Option<String>,

        /// Emit JSON status instead of plain text
        #[arg(short = 'j', long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum Commands {
    // ── Layer 1: Canonical corpus ─────────────────────────────────────
    /// Extract + store Claude Code sessions into the canonical corpus (layer 1).
    ///
    /// Reads ~/.claude/projects/ logs, deduplicates, chunks, and writes
    /// steerable markdown to ~/.aicx/.
    #[command(display_order = 2)]
    Claude {
        #[command(flatten)]
        redaction: RedactionArgs,

        /// Source cwd/project filter(s): narrows session discovery before repo segmentation
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

        /// Ignore the stored watermark and previously-seen hashes for this run
        #[arg(long)]
        full_rescan: bool,

        /// Legacy no-op: incremental mode is now the default
        #[arg(long, hide = true, conflicts_with = "full_rescan")]
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
    /// steerable markdown to ~/.aicx/.
    #[command(display_order = 3)]
    Codex {
        #[command(flatten)]
        redaction: RedactionArgs,

        /// Source cwd/project filter(s): narrows session discovery before repo segmentation
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

        /// Ignore the stored watermark and previously-seen hashes for this run
        #[arg(long)]
        full_rescan: bool,

        /// Legacy no-op: incremental mode is now the default
        #[arg(long, hide = true, conflicts_with = "full_rescan")]
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

    /// Extract + store from all agents (Claude + Codex + Gemini + Junie + CodeScribe) into the canonical corpus (layer 1).
    ///
    /// The daily-driver command: runs each extractor, deduplicates, chunks, and
    /// writes steerable markdown to ~/.aicx/. By default, uses per-source
    /// watermarks to skip already-processed entries. Use --full-rescan to
    /// ignore the watermark and scan the full lookback window again.
    #[command(display_order = 1)]
    All {
        #[command(flatten)]
        redaction: RedactionArgs,

        /// Source cwd/project filter(s): narrows session discovery before repo segmentation
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

        /// Ignore the stored watermark and previously-seen hashes for this run
        #[arg(long)]
        full_rescan: bool,

        /// Legacy no-op: incremental mode is now the default
        #[arg(long, hide = true, conflicts_with = "full_rescan")]
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

    /// Extract a single session — by file path or by session id.
    ///
    /// Two modes:
    /// 1. File mode (legacy): `aicx extract --format claude /path/to/session.jsonl -o /tmp/report.md`
    /// 2. Session mode: `aicx extract --session <uuid> --agent {claude,codex,gemini,junie} [-o FILE]`
    ///
    /// In session mode, the chosen agent's source store is scanned, all timeline
    /// entries matching `--session` are filtered, and a denoised conversation
    /// Markdown transcript is written. Default output path is
    /// `~/.aicx/extracts/<agent>/<session_id>.md`.
    #[command(display_order = 5)]
    Extract {
        #[command(flatten)]
        redaction: RedactionArgs,

        /// Input format (agent), required in file mode: claude | codex | gemini | gemini-antigravity | junie
        #[arg(long, value_enum, alias = "input-format")]
        format: Option<ExtractInputFormat>,

        /// Explicit project/repo name (overrides inference)
        #[arg(short, long)]
        project: Option<String>,

        /// Session id (UUID or agent-native id) for session-mode extraction.
        /// Mutually exclusive with positional `input`.
        #[arg(long, conflicts_with = "input")]
        session: Option<String>,

        /// Source agent for session-mode extraction. Required together with `--session`.
        #[arg(long, value_enum, conflicts_with = "input")]
        agent: Option<ExtractInputFormat>,

        /// Hours to look back when scanning sources in session mode (default: 1 year).
        #[arg(short = 'H', long, default_value = "8760")]
        hours: u64,

        /// Input path (JSONL / JSON / Antigravity brain directory depending on agent).
        /// Used in file mode; mutually exclusive with `--session`.
        input: Option<PathBuf>,

        /// Output file path. In file mode this is required.
        /// In session mode, defaults to `~/.aicx/extracts/<agent>/<session_id>.md`.
        #[arg(short, long)]
        output: Option<PathBuf>,

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
    /// Store-first corpus builder: extracts, deduplicates, chunks, and writes
    /// steerable markdown. By default, this command uses per-source watermarks
    /// to skip previously scanned history. Use --full-rescan for backfills
    /// and targeted re-extraction when you need to ignore the watermark.
    ///
    #[command(display_order = 4)]
    Store {
        #[command(flatten)]
        redaction: RedactionArgs,

        /// Source cwd/project filter(s): narrows session discovery before repo segmentation
        #[arg(short, long, num_args = 1..)]
        project: Vec<String>,

        /// Agent filter: claude, codex, gemini, junie, codescribe, operator-md (default: all agents)
        #[arg(short, long, value_parser = ["claude", "codex", "gemini", "junie", "codescribe", "operator-md"])]
        agent: Option<String>,

        /// Hours to look back (default: 48)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Ignore the stored watermark and previously-seen hashes for this run
        #[arg(long)]
        full_rescan: bool,

        /// Legacy no-op: incremental mode is now the default
        #[arg(long, hide = true, conflicts_with = "full_rescan")]
        incremental: bool,

        /// Only include user messages (exclude assistant + reasoning)
        #[arg(long)]
        user_only: bool,

        /// Include assistant messages (legacy flag; now default)
        #[arg(long, hide = true, conflicts_with = "user_only")]
        include_assistant: bool,

        /// Disable structural-noise filter (line-numbered grep matches, tool
        /// echoes, stray YAML delimiters). Default: filter is ON. Use this
        /// for debugging or when raw upstream content must be preserved
        /// verbatim in the chunk text.
        #[arg(long)]
        no_noise_filter: bool,

        /// What to print to stdout: paths, json, none (default: none)
        #[arg(long, value_enum, default_value_t = StdoutEmit::None)]
        emit: StdoutEmit,
    },

    /// Ingest operator-owned source documents into the canonical corpus.
    #[command(display_order = 5)]
    Ingest {
        #[command(flatten)]
        redaction: RedactionArgs,

        /// Source adapter to ingest
        #[arg(long, value_enum)]
        source: IngestSource,

        /// Source cwd/project filter(s): narrows source discovery before repo segmentation
        #[arg(short, long, num_args = 1..)]
        project: Vec<String>,

        /// Hours to look back when --since is omitted (default: 720 = 30 days)
        #[arg(short = 'H', long, default_value = "720")]
        hours: u64,

        /// Lower date bound (YYYY-MM-DD or YYYY_MMDD)
        #[arg(long)]
        since: Option<String>,

        /// Ignore the stored watermark and previously-seen hashes for this run
        #[arg(long)]
        full_rescan: bool,

        /// Disable structural-noise filter
        #[arg(long)]
        no_noise_filter: bool,

        /// What to print to stdout: paths, json, none (default: none)
        #[arg(long, value_enum, default_value_t = StdoutEmit::None)]
        emit: StdoutEmit,
    },

    // ── Layer 1: Query & inspect ──────────────────────────────────────
    /// List raw agent session sources on disk (pre-extraction inputs).
    ///
    /// Shows Claude Code, Codex, Gemini, and Junie log paths with session counts
    /// and sizes. This is what extractors will read from — use `refs` to
    /// see what is already in the canonical store after extraction.
    #[command(display_order = 10)]
    List,

    /// Audit and explicitly protect raw source roots.
    #[command(display_order = 10)]
    Sources {
        #[command(subcommand)]
        command: SourcesCommands,
    },

    /// Interactive daily-driver entrypoint for corpus, doctor, intents, and store.
    #[command(display_order = 9)]
    Wizard {
        /// Render one frame and exit; used by automated smoke tests.
        #[arg(long, hide = true)]
        smoke_test: bool,
    },

    /// List chunks in the canonical store (layer 1 inventory).
    ///
    /// Shows what extractors have already written to ~/.aicx/.
    #[command(display_order = 11)]
    Refs {
        /// Hours to look back (filter by canonical chunk date)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Repo or store-bucket filter (case-insensitive substring)
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

    /// Generate a searchable HTML dashboard from the canonical store (layer 1), or serve it locally.
    Dashboard(#[command(flatten)] DashboardArgs),

    /// Extract Vibecrafted workflow and marbles reports into a standalone HTML explorer.
    Reports(#[command(flatten)] ReportsArgs),

    /// Audit or repair derived corpus markdown.
    Corpus(#[command(flatten)] CorpusArgs),

    /// Deprecated compatibility shim for `aicx reports`.
    #[command(name = "reports-extractor", hide = true)]
    ReportsExtractorLegacy(#[command(flatten)] ReportsArgs),

    /// Deprecated compatibility shim for `aicx dashboard --serve`.
    #[command(name = "dashboard-serve", hide = true)]
    DashboardServeLegacy(#[command(flatten)] DashboardServeLegacyArgs),

    /// Extract structured intents and decisions from canonical store (layer 1).
    /// `--emit json` includes oracle_status and is canonical corpus evidence,
    /// not semantic oracle output.
    Intents {
        /// Project filter (required)
        #[arg(short, long)]
        project: String,

        /// Hours to look back (default: 720 = 30 days)
        #[arg(short = 'H', long, default_value = "720")]
        hours: u64,

        #[command(flatten)]
        filters: RetrievalFilters,

        /// Return only intent entries without a matching outcome within the same session
        #[arg(long)]
        unresolved: bool,

        /// Collapse multiple intents from the same session into one entry
        #[arg(long)]
        collapse_session: bool,

        /// Output format: markdown or json (json includes oracle_status)
        #[arg(long, default_value = "markdown", value_parser = ["markdown", "json"])]
        emit: String,

        /// Only show high-confidence intents
        #[arg(long)]
        strict: bool,

        /// Filter by kind: decision, intent, outcome, task
        #[arg(long, value_parser = ["decision", "intent", "outcome", "task"])]
        kind: Option<String>,
    },

    /// Stream newly-arriving intents/chunks in a follow-like mode.
    Tail {
        /// Project filter (required)
        #[arg(short, long)]
        project: String,

        /// Hours to look back (default: 48)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Subscribe to filesystem events and stream new entries
        #[arg(long)]
        follow: bool,

        /// Filter by kind: decision, intent, outcome, task
        #[arg(short, long)]
        kind: Option<String>,

        #[command(flatten)]
        filters: RetrievalFilters,
    },

    /// Run aicx as an MCP server (stdio or streamable HTTP).
    ///
    /// Exposes search, steer, and rank tools over MCP for agent retrieval.
    /// `aicx_steer` and `aicx_rank` query the canonical corpus on disk.
    /// `aicx_search` is canonical-store fuzzy search and returns
    /// `oracle_status` so callers cannot mistake it for semantic retrieval.
    #[command(verbatim_doc_comment)]
    Serve {
        /// Transport: stdio (default) or http. Legacy alias: sse.
        #[arg(long, value_enum, default_value_t = McpTransport::Stdio)]
        transport: McpTransport,

        /// Port for streamable HTTP transport (default: 8044)
        #[arg(long, default_value = "8044")]
        port: u16,
    },

    #[command(
        hide = true,
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

    /// Search the canonical corpus. Semantic-first when the embedder is
    /// available, with explicit filesystem-fuzzy fallback otherwise.
    ///
    /// `aicx` aims to be semantic by default: queries are encoded through
    /// the in-process embedder ([`aicx_embeddings`] GGUF stack) and matched
    /// against a materialized vector index. When the embedder cannot load
    /// or no index has been built yet, the command falls back to
    /// filesystem-fuzzy search and emits a precise `oracle_status` line so
    /// the operator can tell which path actually ran.
    #[command(display_order = 12)]
    Search {
        /// Search query string
        query: String,

        /// Repo or store-bucket filter (case-insensitive substring)
        #[arg(short, long)]
        project: Option<String>,

        /// Hours to look back (0 = all time)
        #[arg(short = 'H', long, default_value = "0")]
        hours: u64,

        /// Filter by date: single day (2026-03-28), range (2026-03-20..2026-03-28),
        /// or open-ended (2026-03-20.. or ..2026-03-28)
        #[arg(short, long)]
        date: Option<String>,

        #[command(flatten)]
        filters: RetrievalFilters,

        /// Emit compact JSON instead of plain text
        #[arg(short = 'j', long)]
        json: bool,

        /// Force filesystem-fuzzy search; skip the embedded semantic path
        /// even when the embedder is available. Useful for debugging
        /// retrieval or comparing rankings.
        #[arg(long)]
        no_semantic: bool,
    },

    /// Build (or preview) the vector index used by semantic `aicx search`.
    ///
    /// Iter 2 ships dry-run only: probe the embedder, sample N chunks from
    /// the canonical store, embed them, report stats (count / dimension /
    /// model / ETA). Persistent Lance write of the per-chunk embeddings
    /// lands in Iter 3 once this surface is validated against real input.
    ///
    /// Why dry-run first: it is the smallest unit of evidence that the
    /// model loads, the corpus reads, and the embedder produces vectors
    /// of the expected dimension. Operators get an honest ETA before
    /// they commit to a full re-index that may take 10–30 minutes on CPU
    /// for a 10 k chunk corpus.
    #[command(display_order = 13, verbatim_doc_comment)]
    Index {
        #[command(subcommand)]
        action: Option<IndexAction>,

        /// Repo or store-bucket filter (case-insensitive substring)
        #[arg(short, long)]
        project: Option<String>,

        /// Stop after sampling this many chunks (0 = scan all)
        #[arg(long, default_value = "16")]
        sample: usize,

        /// Emit JSON stats instead of plain text
        #[arg(short = 'j', long)]
        json: bool,

        /// Dry-run only — Iter 2 ships this mode and only this mode. The
        /// flag is here today so the CLI surface stays stable when Iter 3
        /// adds the persistent Lance write under the same command.
        #[arg(long, default_value = "true")]
        dry_run: bool,
    },

    /// Manage the canonical AICX configuration at `~/.aicx/config.toml`.
    ///
    /// Subcommands:
    ///   - `init` — write a default `config.toml` with cloud-embedder
    ///     pre-selected (recommended) plus a fully-commented native GGUF
    ///     section. Bails if the file already exists unless `--force`.
    ///   - `show` — display the currently resolved [`EmbeddingConfig`]
    ///     after merging env, embedder.toml, config.toml, and defaults.
    ///
    /// The config file holds endpoint URL, model name, and the env-var
    /// name for the API key — never the key itself, so the file is
    /// safe to commit, sync, or share.
    #[command(display_order = 4, verbatim_doc_comment)]
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Read one canonical chunk by path, file name, or compact chunk reference.
    ///
    /// This closes the discover -> read loop: pass a path from `aicx search`,
    /// `aicx refs --emit paths`, dashboard `/api/chunk`, or MCP search results.
    #[command(display_order = 14)]
    Read {
        /// Absolute path, store-relative path, file name, or compact chunk reference
        reference: String,

        /// Truncate chunk content to this many UTF-8 characters
        #[arg(long)]
        max_chars: Option<usize>,

        /// Emit compact JSON instead of readable text
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
    #[command(verbatim_doc_comment)]
    Steer {
        /// Filter by run_id (exact match)
        #[arg(long)]
        run_id: Option<String>,

        /// Filter by prompt_id (exact match)
        #[arg(long)]
        prompt_id: Option<String>,

        /// Filter by kind: conversations, plans, reports, other
        #[arg(short, long)]
        kind: Option<String>,

        /// Filter by repo or store bucket (case-insensitive substring)
        #[arg(short, long)]
        project: Option<String>,

        /// Filter by date: single day (2026-03-28), range (2026-03-20..2026-03-28),
        /// or open-ended (2026-03-20.. or ..2026-03-28)
        #[arg(short, long)]
        date: Option<String>,

        /// Emit compact JSON with oracle_status instead of readable text
        #[arg(short = 'j', long)]
        json: bool,

        #[command(flatten)]
        filters: RetrievalFilters,
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

        /// Skip post-migration intent schema scan on the canonical store
        #[arg(long, default_value_t = false)]
        no_intent_schema: bool,
    },

    /// Classify stored chunks into 9-type intent entries and report counts.
    #[command(name = "migrate-intent-schema")]
    MigrateIntentSchema {
        /// Project filter (case-insensitive substring, defaults to scanning the whole store)
        #[arg(short, long)]
        project: Option<String>,

        /// Override AICX store root (default: ~/.aicx)
        #[arg(long)]
        store_root: Option<PathBuf>,

        /// Dry run: show classification counts without writing sidecars
        #[arg(long, default_value_t = true)]
        dry_run: bool,
    },

    /// Diagnose and optionally repair the canonical store and steer index.
    ///
    /// Runs integrity checks on the Lance steer DB, BM25 index, state.json,
    /// sidecar coverage, and corpus bucket names. With --fix, applies safe corrective actions:
    /// corrupted steer indexes are deleted and rebuilt from the canonical
    /// store (which is treated as ground truth and never modified).
    ///
    /// Exit codes: 0 on green/warning or after successful --fix; 1 if
    /// critical issues are detected without --fix.
    #[command(display_order = 12)]
    Doctor {
        /// Apply safe corrective actions for detected issues
        #[arg(long)]
        fix: bool,

        /// Move suspicious top-level corpus buckets to $HOME/.aicx/quarantine/
        #[arg(long)]
        fix_buckets: bool,

        /// Emit a reviewable bash script for missing sidecar backfill
        #[arg(long)]
        rebuild_sidecars: bool,

        /// Emit a reviewable bash script for deleting empty-body chunks
        #[arg(long)]
        prune_empty_bodies: bool,

        /// Print recommendations for green checks too
        #[arg(short, long)]
        verbose: bool,

        /// Output format: text (default), json
        #[arg(long, default_value = "text")]
        format: String,

        /// Report AICX Oracle readiness: ready | degraded | unsafe_for_loctree_scope
        #[arg(long)]
        oracle: bool,
    },

    /// Emit the full AICX health report as JSON for automation.
    #[command(display_order = 11)]
    Health,

    /// Warm/probe the configured local embedder before interactive search.
    #[command(display_order = 15)]
    Warmup {
        /// Emit JSON instead of readable text
        #[arg(short = 'j', long)]
        json: bool,
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
        Some(Commands::Claude {
            redaction,
            project,
            hours,
            output,
            format,
            append_to,
            rotate,
            full_rescan,
            incremental,
            user_only,
            include_assistant: include_assistant_flag,
            loctree,
            project_root,
            force,
            emit,
            conversation,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;
            warn_incremental_legacy_flag(incremental);
            run_extraction(ExtractionParams {
                agents: &["claude"],
                project,
                hours,
                output_dir: output.as_deref(),
                format: &format,
                append_to,
                rotate,
                full_rescan,
                include_assistant,
                include_loctree: loctree,
                project_root,
                force,
                redact_secrets: redaction.redact_secrets,
                emit,
                conversation,
            })?;
        }
        Some(Commands::Codex {
            redaction,
            project,
            hours,
            output,
            format,
            append_to,
            rotate,
            full_rescan,
            incremental,
            user_only,
            include_assistant: include_assistant_flag,
            loctree,
            project_root,
            force,
            emit,
            conversation,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;
            warn_incremental_legacy_flag(incremental);
            run_extraction(ExtractionParams {
                agents: &["codex"],
                project,
                hours,
                output_dir: output.as_deref(),
                format: &format,
                append_to,
                rotate,
                full_rescan,
                include_assistant,
                include_loctree: loctree,
                project_root,
                force,
                redact_secrets: redaction.redact_secrets,
                emit,
                conversation,
            })?;
        }
        Some(Commands::All {
            redaction,
            project,
            hours,
            output,
            append_to,
            rotate,
            full_rescan,
            incremental,
            user_only,
            include_assistant: include_assistant_flag,
            loctree,
            project_root,
            force,
            emit,
            conversation,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;
            warn_incremental_legacy_flag(incremental);
            run_extraction(ExtractionParams {
                agents: &["claude", "codex", "gemini", "junie", "codescribe"],
                project,
                hours,
                output_dir: output.as_deref(),
                format: "both",
                append_to,
                rotate,
                full_rescan,
                include_assistant,
                include_loctree: loctree,
                project_root,
                force,
                redact_secrets: redaction.redact_secrets,
                emit,
                conversation,
            })?;
        }
        Some(Commands::Extract {
            redaction,
            format,
            project,
            session,
            agent,
            hours,
            input,
            output,
            user_only,
            include_assistant: include_assistant_flag,
            max_message_chars,
            conversation,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;

            // Session mode: --session [+ --agent] -> scan sources, filter by session_id.
            if let Some(session_id) = session {
                let agent = agent
                    .or(format)
                    .context("--session requires --agent {claude|codex|gemini|junie}")?;
                run_extract_session(
                    &session_id,
                    agent,
                    output,
                    hours,
                    project,
                    ExtractFileOptions {
                        include_assistant,
                        max_message_chars,
                        redact_secrets: redaction.redact_secrets,
                        conversation: true, // session mode is conversation-first by default
                    },
                )?;
            } else {
                // File mode (legacy): --format <agent> + positional input + -o.
                let format = format
                    .context("file-mode extract requires --format {claude|codex|gemini|gemini-antigravity|junie}")?;
                let input = input.context("file-mode extract requires a positional INPUT path")?;
                let output = output.context("file-mode extract requires -o/--output <FILE>")?;
                run_extract_file(
                    format,
                    project,
                    input,
                    output,
                    ExtractFileOptions {
                        include_assistant,
                        max_message_chars,
                        redact_secrets: redaction.redact_secrets,
                        conversation,
                    },
                )?;
            }
        }
        Some(Commands::Store {
            redaction,
            project,
            agent,
            hours,
            full_rescan,
            incremental,
            user_only,
            include_assistant: include_assistant_flag,
            no_noise_filter,
            emit,
        }) => {
            let include_assistant = include_assistant_flag || !user_only;
            warn_incremental_legacy_flag(incremental);
            run_store(StoreRunArgs {
                project,
                agent,
                hours,
                cutoff: None,
                full_rescan,
                include_assistant,
                emit,
                redact_secrets: redaction.redact_secrets,
                noise_filter_enabled: !no_noise_filter,
            })?;
        }
        Some(Commands::Ingest {
            redaction,
            source,
            project,
            hours,
            since,
            full_rescan,
            no_noise_filter,
            emit,
        }) => {
            let has_explicit_since = since.is_some();
            let cutoff = parse_ingest_since(since.as_deref())?;
            run_store(StoreRunArgs {
                project,
                agent: Some(source.as_agent().to_string()),
                hours,
                cutoff,
                full_rescan: full_rescan || has_explicit_since,
                include_assistant: true,
                emit,
                redact_secrets: redaction.redact_secrets,
                noise_filter_enabled: !no_noise_filter,
            })?;
        }
        Some(Commands::List) => {
            let sources = sources::list_available_sources()?;
            if sources.is_empty() {
                println!("No AI agent session sources found.");
            } else {
                println!("=== Available Sources ===\n");
                for info in &sources {
                    let size_mb = info.size_bytes as f64 / 1024.0 / 1024.0;
                    let protection = if info.protected_by_git {
                        format!(
                            "protected by {} at {}{}",
                            info.protection_backend,
                            info.protection_root
                                .as_deref()
                                .map(Path::display)
                                .map(|display| display.to_string())
                                .unwrap_or_else(|| "<unknown>".to_string()),
                            if info.git_remote_count > 0 {
                                format!("; {} remote line(s)", info.git_remote_count)
                            } else {
                                "; no remote".to_string()
                            }
                        )
                    } else {
                        info.protection_warning
                            .clone()
                            .unwrap_or_else(|| "unprotected source material".to_string())
                    };
                    println!(
                        "  [{:>14}] {} ({} sessions, {:.1} MB) - {}",
                        info.agent,
                        info.path.display(),
                        info.sessions,
                        size_mb,
                        protection,
                    );
                }
            }
        }
        Some(Commands::Sources { command }) => run_sources_command(command)?,
        Some(Commands::Wizard { smoke_test }) => {
            if smoke_test {
                aicx::wizard::smoke_test()?;
            } else {
                aicx::wizard::run()?;
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
        Some(Commands::Dashboard(args)) => {
            run_dashboard_command(args)?;
        }
        Some(Commands::Reports(args)) => {
            run_reports_command(args)?;
        }
        Some(Commands::Corpus(args)) => {
            run_corpus_command(args)?;
        }
        Some(Commands::ReportsExtractorLegacy(args)) => {
            warn_legacy_subcommand("reports-extractor", "reports");
            run_reports_command(args)?;
        }
        Some(Commands::DashboardServeLegacy(args)) => {
            warn_legacy_subcommand("dashboard-serve", "dashboard --serve");
            run_dashboard_server(DashboardServerRunArgs {
                store_root: args.store_root,
                scope: DashboardScope::default(),
                host: args.host,
                port: args.port,
                no_open: args.no_open,
                bg: false,
                allow_cors_origins: None,
                artifact: args.artifact.unwrap_or(default_dashboard_output_path()?),
                title: args.title,
                preview_chars: args.preview_chars,
            })?;
        }
        Some(Commands::Intents {
            project,
            hours,
            filters,
            unresolved,
            collapse_session,
            emit,
            strict,
            kind,
        }) => {
            run_intents(
                &project,
                hours,
                filters,
                IntentsDisplayOptions {
                    emit: &emit,
                    strict,
                    kind: kind.as_deref(),
                    unresolved,
                    collapse_session,
                },
            )?;
        }
        Some(Commands::Tail {
            project,
            hours,
            follow,
            kind,
            filters,
        }) => {
            run_tail(&project, hours, follow, kind.as_deref(), filters)?;
        }
        Some(Commands::Serve { transport, port }) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async { mcp::run_transport(transport, port).await })?;
        }
        Some(Commands::Search {
            query,
            project,
            hours,
            date,
            filters,
            json,
            no_semantic,
        }) => {
            run_search(
                &query,
                project.as_deref(),
                hours,
                date.as_deref(),
                json,
                filters,
                no_semantic,
            )?;
        }
        Some(Commands::Index {
            action,
            project,
            sample,
            json,
            dry_run,
        }) => match action {
            Some(IndexAction::Status { project, json }) => {
                run_index_status(project.as_deref(), json)?;
            }
            None => run_index(project.as_deref(), sample, json, dry_run)?,
        },
        Some(Commands::Config { action }) => {
            run_config(action)?;
        }
        Some(Commands::Read {
            reference,
            max_chars,
            json,
        }) => {
            run_read(&reference, max_chars, json)?;
        }
        Some(Commands::Steer {
            run_id,
            prompt_id,
            kind,
            project,
            date,
            json,
            filters,
        }) => {
            run_steer(
                run_id.as_deref(),
                prompt_id.as_deref(),
                kind.as_deref(),
                project.as_deref(),
                date.as_deref(),
                json,
                filters,
            )?;
        }
        Some(Commands::Migrate {
            dry_run,
            legacy_root,
            store_root,
            no_intent_schema,
        }) => {
            let manifest =
                aicx::store::run_migration_with_paths(dry_run, legacy_root, store_root.clone())?;
            if !no_intent_schema {
                let intent_report = intents::migrate_intent_schema_dry_run_at(
                    &PathBuf::from(&manifest.store_root).join(store::CANONICAL_STORE_DIRNAME),
                    None,
                )?;
                print_intent_schema_migration_report(&intent_report);
            }
        }
        Some(Commands::MigrateIntentSchema {
            project,
            store_root,
            dry_run,
        }) => {
            let report = if let Some(store_root) = store_root {
                intents::migrate_intent_schema_dry_run_at(
                    &store_root.join(store::CANONICAL_STORE_DIRNAME),
                    project.as_deref(),
                )?
            } else {
                intents::migrate_intent_schema_dry_run(project.as_deref())?
            };
            if dry_run {
                print_intent_schema_migration_report(&report);
            }
            let json = serde_json::to_string_pretty(&report)?;
            println!("{json}");
        }
        Some(Commands::Doctor {
            fix,
            fix_buckets,
            rebuild_sidecars,
            prune_empty_bodies,
            verbose,
            format,
            oracle,
        }) => {
            let opts = aicx::doctor::DoctorOptions {
                fix,
                fix_buckets,
                rebuild_sidecars,
                prune_empty_bodies,
                verbose,
            };
            let rt = tokio::runtime::Runtime::new()
                .context("Failed to start tokio runtime for doctor")?;
            let report = rt.block_on(aicx::doctor::run(&opts))?;

            if oracle {
                let status = aicx::doctor::oracle_readiness(&report);
                if format == "json" {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                } else {
                    println!("{}", status.readiness_label);
                    print!("{}", aicx::doctor::format_oracle_readiness_text(&status));
                }
                std::process::exit(match status.readiness {
                    aicx::oracle::OracleReadiness::Ready
                    | aicx::oracle::OracleReadiness::Degraded => 0,
                    aicx::oracle::OracleReadiness::UnsafeForLoctreeScope => 1,
                });
            }

            match format.as_str() {
                "json" => {
                    let json = serde_json::to_string_pretty(&report)?;
                    println!("{json}");
                }
                _ => {
                    print!("{}", aicx::doctor::format_report_text(&report, verbose));
                }
            }

            let exit_code = match report.overall {
                aicx::doctor::Severity::Critical if !fix && !fix_buckets => 1,
                _ => 0,
            };
            std::process::exit(exit_code);
        }
        Some(Commands::Health) => {
            let opts = aicx::doctor::DoctorOptions {
                fix: false,
                fix_buckets: false,
                rebuild_sidecars: false,
                prune_empty_bodies: false,
                verbose: true,
            };
            let rt = tokio::runtime::Runtime::new()
                .context("Failed to start tokio runtime for health")?;
            let report = rt.block_on(aicx::doctor::run(&opts))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            std::process::exit(match report.overall {
                aicx::doctor::Severity::Critical => 1,
                _ => 0,
            });
        }
        Some(Commands::Warmup { json }) => {
            run_warmup(json)?;
        }
        None => {
            Cli::command().print_help()?;
        }
    }

    Ok(())
}

fn run_sources_command(command: SourcesCommands) -> Result<()> {
    match command {
        SourcesCommands::Protect {
            root,
            backend,
            apply,
            initial_snapshot,
            no_gitignore,
        } => run_source_protect(root, backend, apply, initial_snapshot, no_gitignore),
    }
}

fn run_source_protect(
    root: PathBuf,
    backend: SourceProtectionBackend,
    apply: bool,
    initial_snapshot: bool,
    no_gitignore: bool,
) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("source root does not exist: {}", root.display()))?;
    if !root.is_dir() {
        anyhow::bail!("source root must be a directory: {}", root.display());
    }

    let git_dir = root.join(".git");
    let already_protected = git_dir.is_dir();
    let will_init_git = matches!(backend, SourceProtectionBackend::GitLocal) && !already_protected;
    let will_write_gitignore =
        matches!(backend, SourceProtectionBackend::GitLocal) && !no_gitignore;

    println!("=== Source Protection Plan ===");
    println!("Root: {}", root.display());
    println!("Backend: {}", backend.as_str());
    println!("Mode: {}", if apply { "apply" } else { "dry-run" });
    println!(
        "Status: {}",
        if already_protected {
            "source root protected"
        } else {
            "unprotected source material"
        }
    );
    println!(
        "Create local .git: {}",
        if will_init_git { "yes" } else { "no" }
    );
    println!(
        "Add safe .gitignore suggestions: {}",
        if will_write_gitignore { "yes" } else { "no" }
    );
    println!("Create remote: no (AICX never configures a remote by default)");
    println!(
        "Initial local snapshot: {}",
        if initial_snapshot { "yes" } else { "no" }
    );

    if !apply {
        println!();
        println!("Dry run only. Re-run with --apply to modify this source root.");
        return Ok(());
    }

    match backend {
        SourceProtectionBackend::GitLocal => {
            if will_init_git {
                run_git(&root, &["init"])?;
            }
            if will_write_gitignore {
                add_source_protection_gitignore(&root)?;
            }
            if initial_snapshot {
                create_initial_source_snapshot(&root)?;
            }
        }
    }

    println!("source root protected: {}", root.display());
    println!("remote configured: no");
    Ok(())
}

fn run_git(root: &Path, args: &[&str]) -> Result<()> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git in {}", root.display()))?;

    if !output.status.success() {
        anyhow::bail!(
            "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
            args,
            root.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

fn add_source_protection_gitignore(root: &Path) -> Result<()> {
    const MARKER: &str = "# AICX source protection local git";
    const SUGGESTIONS: &str =
        "\n# AICX source protection local git\n.DS_Store\n*.tmp\ntarget/\nnode_modules/\n";

    let path = root.join(".gitignore");
    let existing = aicx::sanitize::read_to_string_validated(&path).unwrap_or_default();
    if existing.contains(MARKER) {
        return Ok(());
    }

    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(SUGGESTIONS);
    let mut file = aicx::sanitize::create_file_validated(&path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(next.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn create_initial_source_snapshot(root: &Path) -> Result<()> {
    run_git(root, &["add", "-A"])?;
    let diff_status = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .with_context(|| format!("failed to inspect staged snapshot in {}", root.display()))?;

    if diff_status.success() {
        println!("initial snapshot skipped: no staged changes");
        return Ok(());
    }

    run_git(
        root,
        &["commit", "-m", "aicx source protection initial snapshot"],
    )
}

/// Display toggles for `run_intents`. Packed so the caller reads like a struct
/// literal instead of a positional 8-tuple.
struct IntentsDisplayOptions<'a> {
    emit: &'a str,
    strict: bool,
    kind: Option<&'a str>,
    unresolved: bool,
    collapse_session: bool,
}

fn run_intents(
    project: &str,
    hours: u64,
    filters: RetrievalFilters,
    display: IntentsDisplayOptions<'_>,
) -> Result<()> {
    let IntentsDisplayOptions {
        emit,
        strict,
        kind,
        unresolved,
        collapse_session,
    } = display;
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
        frame_kind: filters.frame_kind.map(Into::into),
    };

    let extraction = intents::extract_intents_with_stats(&config)?;
    let records = extraction.records;

    let (date_lo, date_hi) = if let Some(ref d) = filters.since {
        let bounds = parse_date_filter(d)?;
        (bounds.0, bounds.1)
    } else {
        (None, filters.until.clone())
    };

    let display_filters = intents::IntentDisplayFilters {
        unresolved,
        collapse_session,
        agent: filters.agent.clone(),
        date_lo,
        date_hi,
        sort: filters.sort.map(|s| match s {
            SortOrder::Newest => intents::IntentSortOrder::Newest,
            SortOrder::Oldest => intents::IntentSortOrder::Oldest,
            // Score sort isn't meaningful for intents (no score field); fall back to newest.
            SortOrder::Score => intents::IntentSortOrder::Newest,
        }),
        limit: Some(filters.limit),
    };

    let records = intents::apply_display_filters(records, &display_filters);

    if records.is_empty() && emit != "json" {
        eprintln!(
            "No intents found for project '{}' in last {} hours.",
            project, hours
        );
        return Ok(());
    }

    match emit {
        "json" => {
            let store_root = store::store_base_dir()?;
            let oracle_status = aicx::oracle::OracleStatus::canonical_corpus_scan(
                &store_root,
                extraction.stats.scanned_count,
                extraction.stats.candidate_count,
                extraction.stats.source_paths_verified,
            );
            let json = intents::format_intents_oracle_json(&records, oracle_status)?;
            println!("{}", json);
        }
        _ => {
            let md = intents::format_intents_markdown(&records);
            print!("{}", md);
        }
    }

    Ok(())
}

fn run_tail(
    project: &str,
    hours: u64,
    follow: bool,
    kind: Option<&str>,
    mut filters: RetrievalFilters,
) -> Result<()> {
    if !follow {
        // One-shot mode
        if filters.limit == 10 {
            filters.limit = 20; // default 20 for tail
        }
        filters.sort = Some(SortOrder::Newest);
        return run_intents(
            project,
            hours,
            filters,
            IntentsDisplayOptions {
                emit: "markdown",
                strict: false,
                kind,
                unresolved: false,
                collapse_session: false,
            },
        );
    }

    let kind_filter = kind.map(|k| match k {
        "decision" => intents::IntentKind::Decision,
        "intent" => intents::IntentKind::Intent,
        "outcome" => intents::IntentKind::Outcome,
        "task" => intents::IntentKind::Task,
        _ => unreachable!("clap validates this"),
    });

    let mut config = intents::IntentsConfig {
        project: project.to_string(),
        hours,
        strict: false,
        kind_filter,
        frame_kind: filters.frame_kind.map(Into::into),
    };

    let mut last_seen = std::collections::HashSet::new();
    eprintln!("Watching for new intents in project '{}'...", project);

    loop {
        if let Ok(mut records) = intents::extract_intents(&config) {
            // Apply filtering identical to run_intents
            if let Some(agent_filter) = &filters.agent {
                records.retain(|r| r.agent == *agent_filter);
            }
            let (lo, hi) = if let Some(ref d) = filters.since {
                (
                    parse_date_filter(d).ok().and_then(|b| b.0),
                    parse_date_filter(d).ok().and_then(|b| b.1),
                )
            } else {
                (None, filters.until.clone())
            };
            if lo.is_some() || hi.is_some() {
                records.retain(|r| {
                    lo.as_ref().is_none_or(|lo| r.date.as_str() >= lo.as_str())
                        && hi.as_ref().is_none_or(|hi| r.date.as_str() <= hi.as_str())
                });
            }

            records.sort_by(|a, b| {
                let t_a = a.timestamp.as_deref().unwrap_or(a.date.as_str());
                let t_b = b.timestamp.as_deref().unwrap_or(b.date.as_str());
                t_a.cmp(t_b) // Oldest to newest for streaming
            });

            let mut new_records = Vec::new();
            for rec in records {
                let key = format!(
                    "{}|{}|{}|{}",
                    rec.source_chunk,
                    rec.timestamp.as_deref().unwrap_or(""),
                    rec.summary,
                    rec.agent
                );
                if last_seen.insert(key) {
                    new_records.push(rec);
                }
            }

            if !new_records.is_empty() {
                for rec in new_records {
                    let mut out = String::new();
                    out.push_str(&format!("### {} | {}\n", rec.kind.heading(), rec.agent));
                    out.push_str(&format!("{}: {}\n", rec.kind.heading(), rec.summary));
                    out.push_str(&format!(
                        "WHY: {}\n",
                        rec.context.as_deref().unwrap_or("not captured")
                    ));
                    out.push_str("EVIDENCE:\n");
                    out.push_str(&format!("- source_chunk: {}\n", rec.source_chunk));
                    for evidence in &rec.evidence {
                        out.push_str(&format!("- {}\n", evidence));
                    }
                    println!("{}\n", out);
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_secs(2));
        config.hours = 1; // shrink window after first pass
    }
}

/// Output-shaping toggles for `run_extract_file`. Keeps the constructor-like
/// call readable without an argument-list ceiling waiver.
struct ExtractFileOptions {
    include_assistant: bool,
    max_message_chars: usize,
    redact_secrets: bool,
    conversation: bool,
}

fn extract_input_format_label(format: ExtractInputFormat) -> &'static str {
    match format {
        ExtractInputFormat::Claude => "claude",
        ExtractInputFormat::Codex => "codex",
        ExtractInputFormat::Gemini => "gemini",
        ExtractInputFormat::GeminiAntigravity => "gemini",
        ExtractInputFormat::Junie => "junie",
    }
}

/// Resolve the default output path for `aicx extract --session ...`:
/// `~/.aicx/extracts/<agent>/<session_id>.md`.
fn default_session_extract_path(agent_label: &str, session_id: &str) -> Result<PathBuf> {
    let base = aicx::store::store_base_dir()?;
    let safe_session: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(base
        .join("extracts")
        .join(agent_label)
        .join(format!("{safe_session}.md")))
}

/// Run extraction filtered by `session_id` for a single agent and write a
/// denoised conversation Markdown transcript. Default output path is
/// `~/.aicx/extracts/<agent>/<session_id>.md`; override via `output`.
fn run_extract_session(
    session_id: &str,
    agent: ExtractInputFormat,
    output: Option<PathBuf>,
    hours: u64,
    explicit_project: Option<String>,
    options: ExtractFileOptions,
) -> Result<()> {
    let ExtractFileOptions {
        include_assistant,
        max_message_chars,
        redact_secrets,
        conversation,
    } = options;

    let agent_label = extract_input_format_label(agent);
    let cutoff = Utc::now() - chrono::Duration::hours(hours.max(1) as i64);
    let config = ExtractionConfig {
        project_filter: explicit_project
            .as_ref()
            .map(|p| vec![p.clone()])
            .unwrap_or_default(),
        cutoff,
        include_assistant,
        watermark: None,
    };

    let mut entries: Vec<timeline::TimelineEntry> = match agent {
        ExtractInputFormat::Claude => sources::extract_claude(&config)?,
        ExtractInputFormat::Codex => sources::extract_codex(&config)?,
        ExtractInputFormat::Gemini | ExtractInputFormat::GeminiAntigravity => {
            sources::extract_gemini(&config)?
        }
        ExtractInputFormat::Junie => sources::extract_junie(&config)?,
    };

    // Filter by session_id (exact match).
    entries.retain(|e| e.session_id == session_id);

    if entries.is_empty() {
        anyhow::bail!(
            "No entries found for session `{}` in agent `{}` within last {} hours.\n\
             Try: increase --hours, verify the session id, or check that the source store is populated.",
            session_id,
            agent_label,
            hours,
        );
    }

    entries.sort_by_key(|e| e.timestamp);

    let (mut entries, collapse_stats) =
        aicx_parser::collapse_repeats(entries, aicx_parser::DEFAULT_THRESHOLD_LINES);
    if collapse_stats.messages_collapsed > 0 {
        eprintln!(
            "Collapsed {} repeated message body/bodies (saved {} bytes)",
            collapse_stats.messages_collapsed, collapse_stats.bytes_saved,
        );
    }

    if redact_secrets {
        for e in &mut entries {
            e.message = aicx::redact::redact_secrets(&e.message);
        }
    }

    let output_path = match output {
        Some(p) => p,
        None => default_session_extract_path(agent_label, session_id)?,
    };

    let inferred_repos = sources::repo_labels_from_entries(&entries, &[]);
    let project_identity = explicit_project.unwrap_or_else(|| {
        if inferred_repos.is_empty() {
            format!("{agent_label}/{session_id}")
        } else {
            inferred_repos.join("+")
        }
    });

    let hours_back = entries
        .first()
        .map(|e| (Utc::now() - e.timestamp).num_hours().max(0) as u64)
        .unwrap_or(0);

    let metadata = ReportMetadata {
        generated_at: Utc::now(),
        project_filter: Some(project_identity.clone()),
        hours_back,
        total_entries: entries.len(),
        sessions: vec![session_id.to_string()],
    };

    if conversation {
        let conv_msgs = sources::to_conversation(&entries, &[project_identity]);
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
            output::write_json_report_to_path(&output_path, &entries, &metadata)?;
        } else {
            output::write_markdown_report_to_path(
                &output_path,
                &entries,
                &metadata,
                max_message_chars,
                None,
            )?;
        }
    }

    eprintln!(
        "Extracted {} entries from session `{}` ({}) -> {}",
        entries.len(),
        session_id,
        agent_label,
        output_path.display()
    );
    Ok(())
}

fn run_extract_file(
    format: ExtractInputFormat,
    explicit_project: Option<String>,
    input: PathBuf,
    output_path: PathBuf,
    options: ExtractFileOptions,
) -> Result<()> {
    let ExtractFileOptions {
        include_assistant,
        max_message_chars,
        redact_secrets,
        conversation,
    } = options;
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
        ExtractInputFormat::Junie => sources::extract_junie_file(&input, &config)?,
    };

    // Sort by timestamp (extractors should already do this).
    entries.sort_by_key(|a| a.timestamp);

    let (mut entries, collapse_stats) =
        aicx_parser::collapse_repeats(entries, aicx_parser::DEFAULT_THRESHOLD_LINES);
    if collapse_stats.messages_collapsed > 0 {
        eprintln!(
            "Collapsed {} repeated message body/bodies (saved {} bytes)",
            collapse_stats.messages_collapsed, collapse_stats.bytes_saved,
        );
    }

    // Apply secret redaction in-place (TimelineEntry is now a single timeline type)
    if redact_secrets {
        for e in &mut entries {
            e.message = aicx::redact::redact_secrets(&e.message);
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
            if conversation {
                "file input".to_string()
            } else {
                format!("file: {file_label}")
            }
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

#[derive(Debug, Clone, Serialize)]
struct StoreScopeSurface {
    requested_source_filters: Option<Vec<String>>,
    resolved_repositories: Vec<String>,
    includes_non_repository_contexts: bool,
    resolved_store_buckets: BTreeMap<String, BTreeMap<String, usize>>,
}

impl StoreScopeSurface {
    fn empty(requested_filters: &[String]) -> Self {
        Self {
            requested_source_filters: normalized_requested_source_filters(requested_filters),
            resolved_repositories: Vec::new(),
            includes_non_repository_contexts: false,
            resolved_store_buckets: BTreeMap::new(),
        }
    }

    fn from_store_summary(
        requested_filters: &[String],
        store_summary: &store::StoreWriteSummary,
    ) -> Self {
        Self {
            requested_source_filters: normalized_requested_source_filters(requested_filters),
            resolved_repositories: store_summary
                .project_summary
                .keys()
                .filter(|bucket| bucket.as_str() != store::NON_REPOSITORY_CONTEXTS)
                .cloned()
                .collect(),
            includes_non_repository_contexts: store_summary
                .project_summary
                .contains_key(store::NON_REPOSITORY_CONTEXTS),
            resolved_store_buckets: store_summary.project_summary.clone(),
        }
    }

    fn repository_buckets(&self) -> BTreeMap<String, BTreeMap<String, usize>> {
        self.resolved_store_buckets
            .iter()
            .filter(|(bucket, _)| bucket.as_str() != store::NON_REPOSITORY_CONTEXTS)
            .map(|(bucket, counts)| (bucket.clone(), counts.clone()))
            .collect()
    }
}

fn normalized_requested_source_filters(requested_filters: &[String]) -> Option<Vec<String>> {
    if requested_filters.is_empty() {
        None
    } else {
        Some(requested_filters.to_vec())
    }
}

fn render_requested_source_filters(requested_filters: &[String]) -> String {
    if requested_filters.is_empty() {
        "(all sources)".to_string()
    } else {
        requested_filters.join(", ")
    }
}

fn render_resolved_store_buckets(scope: &StoreScopeSurface) -> String {
    if scope.resolved_store_buckets.is_empty() {
        "(none written)".to_string()
    } else {
        scope
            .resolved_store_buckets
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

const INCREMENTAL_LEGACY_NOTE: &str =
    "# Note: --incremental is now the default and will be removed in 0.8.0";
const LEGACY_ALL_WATERMARK_AGENTS: &[&str] = &["claude", "codex", "gemini", "junie", "codescribe"];
const LEGACY_ALL_WATERMARK_KEY: &str = "claude+codex+gemini+junie";

fn extraction_source_key(agents: &[&str], project: &[String]) -> String {
    let agent_key = if agents == LEGACY_ALL_WATERMARK_AGENTS {
        LEGACY_ALL_WATERMARK_KEY.to_string()
    } else {
        agents.join("+")
    };
    format!(
        "{}:{}",
        agent_key,
        if project.is_empty() {
            "all".to_string()
        } else {
            project.join("+")
        }
    )
}

fn extraction_source_key_aliases(agents: &[&str], project: &[String]) -> Vec<String> {
    let project_key = if project.is_empty() {
        "all".to_string()
    } else {
        project.join("+")
    };
    let mut aliases = Vec::new();
    if agents == LEGACY_ALL_WATERMARK_AGENTS {
        aliases.push(format!(
            "claude+codex+gemini+junie+codescribe:{project_key}"
        ));
        aliases.push(format!("claude+codex+gemini:{project_key}"));
    }
    aliases
}

fn warn_incremental_legacy_flag(flag_used: bool) {
    if flag_used {
        eprintln!("{INCREMENTAL_LEGACY_NOTE}");
    }
}

fn warn_legacy_subcommand(legacy: &str, replacement: &str) {
    eprintln!("# Note: `aicx {legacy}` is deprecated; use `aicx {replacement}` instead.");
}

fn dedup_entries_for_state(
    entries: Vec<timeline::TimelineEntry>,
    state: &mut StateManager,
    project_name: &str,
    overlap_project: &str,
    full_rescan: bool,
) -> Vec<timeline::TimelineEntry> {
    let mut deduped = Vec::with_capacity(entries.len());
    let mut exact_seen_this_run = std::collections::HashSet::new();
    let mut overlap_seen_this_run = std::collections::HashSet::new();

    for entry in entries {
        let exact =
            StateManager::content_hash(&entry.agent, entry.timestamp.timestamp(), &entry.message);
        if full_rescan {
            if !exact_seen_this_run.insert(exact) {
                continue; // exact duplicate within the same rescan window
            }
        } else if !state.is_new(project_name, exact) {
            continue; // exact duplicate
        }

        let overlap = StateManager::overlap_hash(entry.timestamp.timestamp(), &entry.message);
        if full_rescan {
            if !overlap_seen_this_run.insert(overlap) {
                continue; // cross-agent overlap duplicate within the same rescan window
            }
        } else if !state.is_new(overlap_project, overlap) {
            continue; // cross-agent overlap duplicate
        }

        if !full_rescan {
            state.mark_seen(project_name, exact);
            state.mark_seen(overlap_project, overlap);
        }
        deduped.push(entry);
    }

    deduped
}

struct ExtractionParams<'a> {
    agents: &'a [&'a str],
    project: Vec<String>,
    hours: u64,
    output_dir: Option<&'a Path>,
    format: &'a str,
    append_to: Option<PathBuf>,
    rotate: usize,
    full_rescan: bool,
    include_assistant: bool,
    include_loctree: bool,
    project_root: Option<PathBuf>,
    force: bool,
    conversation: bool,
    redact_secrets: bool,
    emit: StdoutEmit,
}

struct StoreRunArgs {
    project: Vec<String>,
    agent: Option<String>,
    hours: u64,
    cutoff: Option<DateTime<Utc>>,
    full_rescan: bool,
    include_assistant: bool,
    emit: StdoutEmit,
    redact_secrets: bool,
    /// Whether the chunker should strip structural noise. Mirrors
    /// `ChunkerConfig::noise_filter_enabled`; the CLI surface is
    /// `--no-noise-filter` (negated to keep the default ergonomic).
    noise_filter_enabled: bool,
}

fn resolve_store_agents(agent: Option<&str>) -> Result<Vec<&'static str>> {
    match agent {
        Some("claude") => Ok(vec!["claude"]),
        Some("codex") => Ok(vec!["codex"]),
        Some("gemini") => Ok(vec!["gemini"]),
        Some("junie") => Ok(vec!["junie"]),
        Some("codescribe") => Ok(vec!["codescribe"]),
        Some("operator-md") => Ok(vec!["operator-md"]),
        Some(other) => Err(anyhow::anyhow!(
            "Unsupported --agent '{}'. Expected one of: claude, codex, gemini, junie, codescribe, operator-md.",
            other
        )),
        None => Ok(vec!["claude", "codex", "gemini", "junie", "codescribe"]),
    }
}

fn parse_ingest_since(value: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let date = parse_cli_date(Some(value), "--since")?
        .ok_or_else(|| anyhow::anyhow!("Invalid --since value '{}'", value))?;
    let datetime = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("Invalid --since date '{}'", value))?;
    Ok(Some(Utc.from_utc_datetime(&datetime)))
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
        full_rescan,
        include_assistant,
        include_loctree,
        project_root,
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

    // Default behavior is incremental. --full-rescan and the legacy --force
    // escape hatch both mean "scan the full lookback window".
    let source_key = extraction_source_key(agents, &project);
    let source_aliases = extraction_source_key_aliases(agents, &project);
    state.migrate_watermark_aliases(&source_key, &source_aliases);
    let watermark = if full_rescan || force {
        None
    } else {
        state.get_watermark(&source_key)
    };

    let config = ExtractionConfig {
        project_filter: project.clone(),
        cutoff,
        include_assistant,
        watermark,
    };
    eprintln!(
        "  Requested source filters: {}",
        render_requested_source_filters(&project)
    );

    // Extract from requested sources
    let mut entries = Vec::new();

    for &agent in agents {
        let agent_entries = match agent {
            "claude" => sources::extract_claude(&config)?,
            "codex" => sources::extract_codex(&config)?,
            "gemini" => sources::extract_gemini(&config)?,
            "junie" => sources::extract_junie(&config)?,
            "codescribe" => sources::extract_codescribe(&config)?,
            "operator-md" => sources::extract_operator_markdown(&config)?,
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
        entries = dedup_entries_for_state(
            entries,
            &mut state,
            &project_name,
            &overlap_project,
            full_rescan,
        );
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
    entries.sort_by_key(|a| a.timestamp);

    // Filter self-echo (aicx's own search/rank/store calls that create feedback loops)
    let pre_echo = entries.len();
    entries.retain(|e| !aicx::sanitize::is_self_echo(&e.message));
    let echo_filtered = pre_echo - entries.len();
    if echo_filtered > 0 {
        eprintln!("  Filtered {echo_filtered} self-echo entries");
    }

    // Apply secret redaction in-place (TimelineEntry is now a single timeline type)
    if redact_secrets {
        for e in &mut entries {
            e.message = aicx::redact::redact_secrets(&e.message);
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

    let chunker_config = aicx::chunker::ChunkerConfig::default();
    let mut all_written_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut written_empty_body_skipped = 0usize;
    let mut scope_surface = StoreScopeSurface::empty(&project);

    let structured_emit = matches!(emit, StdoutEmit::Json);
    let reporter = aicx::progress::select_reporter(structured_emit);
    let failures = aicx::progress::FailureLog::new();

    if !output_entries.is_empty() {
        let chunk_phase = aicx::progress::Phase::start(
            reporter.clone(),
            "chunk",
            Some(output_entries.len() as u64),
        );
        let store_summary = match store::store_semantic_segments(&output_entries, &chunker_config) {
            Ok(summary) => {
                let written = summary.written_paths.len() as u64;
                chunk_phase.finish_ok(format!("{written} chunks"));
                summary
            }
            Err(e) => {
                let record = chunk_phase.finish_err(&e, aicx::progress::recovery_hint_for("chunk"));
                failures.record(record);
                let _ = aicx::progress::render_failure_tail(&failures);
                return Err(e);
            }
        };
        scope_surface = StoreScopeSurface::from_store_summary(&project, &store_summary);
        written_empty_body_skipped = store_summary.skipped_empty_body;
        let newly_written_paths = store_summary.written_paths.clone();
        all_written_paths.extend(newly_written_paths.iter().cloned());

        // Update fast local metadata index
        if let Ok(rt) = tokio::runtime::Runtime::new() {
            let path_refs: Vec<&PathBuf> = newly_written_paths.iter().collect();
            if let Err(e) = rt.block_on(aicx::steer_index::sync_steer_index_with_progress(
                &path_refs,
                reporter.clone(),
                &failures,
            )) {
                eprintln!("⚠ steer index sync failed (search may be stale): {e}");
            }
        }

        // Summary to stderr (diagnostics)
        eprintln!(
            "✓ {} entries → {} chunks",
            output_entries.len(),
            all_written_paths.len(),
        );
        if written_empty_body_skipped > 0 {
            eprintln!("  Skipped {written_empty_body_skipped} empty-body chunk(s)");
        }
        for (repo, agents_map) in &store_summary.project_summary {
            let total: usize = agents_map.values().sum();
            let detail: Vec<String> = agents_map
                .iter()
                .map(|(a, c)| format!("{}: {}", a, c))
                .collect();
            eprintln!("  {}: {} entries ({})", repo, total, detail.join(", "));
        }
        eprintln!(
            "  Resolved store buckets: {}",
            render_resolved_store_buckets(&scope_surface)
        );
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
                    #[serde(flatten)]
                    scope: &'a StoreScopeSurface,
                    messages: Vec<timeline::ConversationMessage>,
                    store_paths: Vec<String>,
                    written_empty_body_skipped: usize,
                }

                let conv_msgs = sources::to_conversation(&output_entries, &project);
                let report = JsonConvStdout {
                    generated_at: metadata.generated_at,
                    project_filter: &metadata.project_filter,
                    hours_back: metadata.hours_back,
                    total_messages: conv_msgs.len(),
                    sessions: &metadata.sessions,
                    scope: &scope_surface,
                    messages: conv_msgs,
                    store_paths,
                    written_empty_body_skipped,
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
                    #[serde(flatten)]
                    scope: &'a StoreScopeSurface,
                    entries: &'a [timeline::TimelineEntry],
                    store_paths: Vec<String>,
                    written_empty_body_skipped: usize,
                }

                let report = JsonStdoutReport {
                    generated_at: metadata.generated_at,
                    project_filter: &metadata.project_filter,
                    hours_back: metadata.hours_back,
                    total_entries: metadata.total_entries,
                    sessions: &metadata.sessions,
                    scope: &scope_surface,
                    entries: &output_entries,
                    store_paths,
                    written_empty_body_skipped,
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
        if force || full_rescan {
            // When --force or --full-rescan bypasses persisted dedup state, we still
            // mark entries as seen so future incremental runs remain honest.
            for e in &output_entries {
                let exact =
                    StateManager::content_hash(&e.agent, e.timestamp.timestamp(), &e.message);
                let overlap = StateManager::overlap_hash(e.timestamp.timestamp(), &e.message);
                state.mark_seen(&project_name, exact);
                state.mark_seen(&overlap_project, overlap);
            }
        }

        if let Some(latest) = output_entries.last() {
            state.update_watermark(&source_key, latest.timestamp);
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

    if aicx::progress::render_failure_tail(&failures) {
        std::process::exit(2);
    }

    Ok(())
}

/// Store extracted contexts in the canonical corpus.
fn run_store(args: StoreRunArgs) -> Result<()> {
    let StoreRunArgs {
        project,
        agent,
        hours,
        cutoff,
        full_rescan,
        include_assistant,
        emit,
        redact_secrets,
        noise_filter_enabled,
    } = args;

    let cutoff = cutoff.unwrap_or_else(|| Utc::now() - chrono::Duration::hours(hours as i64));

    let agents = resolve_store_agents(agent.as_deref())?;

    let mut state = StateManager::load();
    let source_key = extraction_source_key(&agents, &project);
    let source_aliases = extraction_source_key_aliases(&agents, &project);
    state.migrate_watermark_aliases(&source_key, &source_aliases);
    let watermark = if full_rescan {
        None
    } else {
        state.get_watermark(&source_key)
    };

    let config = ExtractionConfig {
        project_filter: project.clone(),
        cutoff,
        include_assistant,
        watermark,
    };
    eprintln!(
        "  Requested source filters: {}",
        render_requested_source_filters(&project)
    );

    let mut all_entries = Vec::new();
    for &ag in &agents {
        let agent_entries = match ag {
            "claude" => sources::extract_claude(&config)?,
            "codex" => sources::extract_codex(&config)?,
            "gemini" => sources::extract_gemini(&config)?,
            "junie" => sources::extract_junie(&config)?,
            "codescribe" => sources::extract_codescribe(&config)?,
            "operator-md" => sources::extract_operator_markdown(&config)?,
            _ => Vec::new(),
        };
        eprintln!("  [{}] {} entries", ag, agent_entries.len());
        all_entries.extend(agent_entries);
    }

    all_entries.sort_by_key(|a| a.timestamp);

    let project_name = if project.is_empty() {
        "_global".to_string()
    } else {
        project.join("+")
    };
    let overlap_project = format!("_overlap:{project_name}");
    let pre_dedup = all_entries.len();
    all_entries = dedup_entries_for_state(
        all_entries,
        &mut state,
        &project_name,
        &overlap_project,
        full_rescan,
    );
    if pre_dedup != all_entries.len() {
        eprintln!(
            "  Dedup: {} → {} entries (skipped {} seen)",
            pre_dedup,
            all_entries.len(),
            pre_dedup - all_entries.len(),
        );
    }

    // Filter self-echo (prevents feedback loops from aicx's own tool calls)
    let pre_echo = all_entries.len();
    all_entries.retain(|e| !aicx::sanitize::is_self_echo(&e.message));
    let echo_filtered = pre_echo - all_entries.len();
    if echo_filtered > 0 {
        eprintln!("  Filtered {echo_filtered} self-echo entries");
    }

    if all_entries.is_empty() {
        eprintln!("No entries found.");
        if let StdoutEmit::Json = emit {
            let scope_surface = StoreScopeSurface::empty(&project);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "total_entries": 0,
                    "total_chunks": 0,
                    "requested_source_filters": scope_surface.requested_source_filters,
                    "resolved_repositories": scope_surface.resolved_repositories,
                    "includes_non_repository_contexts": scope_surface.includes_non_repository_contexts,
                    "resolved_store_buckets": scope_surface.resolved_store_buckets,
                    "repos": scope_surface.repository_buckets(),
                    "store_paths": Vec::<String>::new(),
                    "written_empty_body_skipped": 0,
                }))?
            );
        }
        return Ok(());
    }

    // Apply redaction in-place (single TimelineEntry type)
    if redact_secrets {
        for e in &mut all_entries {
            e.message = aicx::redact::redact_secrets(&e.message);
        }
    }
    if !noise_filter_enabled {
        eprintln!(
            "  [warn] --no-noise-filter active: chunks will retain raw scaffolding (line-numbered grep, tool echoes, YAML delimiters)"
        );
    }
    let chunker_config = aicx::chunker::ChunkerConfig {
        noise_filter_enabled,
        ..aicx::chunker::ChunkerConfig::default()
    };

    let structured_emit = matches!(emit, StdoutEmit::Json);
    let reporter = aicx::progress::select_reporter(structured_emit);
    let failures = aicx::progress::FailureLog::new();

    let chunk_phase =
        aicx::progress::Phase::start(reporter.clone(), "chunk", Some(all_entries.len() as u64));
    let store_result = store::store_semantic_segments_with_progress(
        &all_entries,
        &chunker_config,
        |done, _total| chunk_phase.tick(done as u64),
    );
    let store_summary = match store_result {
        Ok(summary) => {
            let written = summary.written_paths.len() as u64;
            chunk_phase.finish_ok(format!("{written} chunks"));
            summary
        }
        Err(e) => {
            let record = chunk_phase.finish_err(&e, aicx::progress::recovery_hint_for("chunk"));
            failures.record(record);
            let _ = aicx::progress::render_failure_tail(&failures);
            return Err(e);
        }
    };
    let stored_count = store_summary.total_entries;
    let all_written_paths = store_summary.written_paths.clone();
    let scope_surface = StoreScopeSurface::from_store_summary(&project, &store_summary);

    // Update fast local metadata index
    if let Ok(rt) = tokio::runtime::Runtime::new() {
        let path_refs: Vec<&PathBuf> = all_written_paths.iter().collect();
        if let Err(e) = rt.block_on(aicx::steer_index::sync_steer_index_with_progress(
            &path_refs,
            reporter.clone(),
            &failures,
        )) {
            eprintln!("⚠ steer index sync failed (search may be stale): {e}");
        }
    }

    eprintln!(
        "✓ {} entries → {} chunks",
        stored_count,
        all_written_paths.len(),
    );
    if store_summary.skipped_empty_body > 0 {
        eprintln!(
            "  Skipped {} empty-body chunk(s)",
            store_summary.skipped_empty_body
        );
    }
    for (repo, agents_map) in &store_summary.project_summary {
        let total: usize = agents_map.values().sum();
        let detail: Vec<String> = agents_map
            .iter()
            .map(|(a, c)| format!("{}: {}", a, c))
            .collect();
        eprintln!("  {}: {} entries ({})", repo, total, detail.join(", "));
    }
    eprintln!(
        "  Resolved store buckets: {}",
        render_resolved_store_buckets(&scope_surface)
    );

    if let Some(latest) = all_entries.last() {
        state.update_watermark(&source_key, latest.timestamp);
    }
    if full_rescan {
        for e in &all_entries {
            let exact = StateManager::content_hash(&e.agent, e.timestamp.timestamp(), &e.message);
            let overlap = StateManager::overlap_hash(e.timestamp.timestamp(), &e.message);
            state.mark_seen(&project_name, exact);
            state.mark_seen(&overlap_project, overlap);
        }
    }
    state.record_run(
        stored_count,
        agents.iter().map(|agent| (*agent).to_string()).collect(),
    );
    state.prune_old_hashes(50_000);
    state.save()?;

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
                    "requested_source_filters": scope_surface.requested_source_filters,
                    "resolved_repositories": scope_surface.resolved_repositories,
                    "includes_non_repository_contexts": scope_surface.includes_non_repository_contexts,
                    "resolved_store_buckets": scope_surface.resolved_store_buckets,
                    "repos": scope_surface.repository_buckets(),
                    "store_paths": store_paths,
                    "written_empty_body_skipped": store_summary.skipped_empty_body,
                }))?
            );
        }
        StdoutEmit::None => {}
    }

    if aicx::progress::render_failure_tail(&failures) {
        std::process::exit(2);
    }

    Ok(())
}

fn is_noise_artifact(path: &std::path::Path) -> bool {
    if !path.is_file() || path.extension().is_none_or(|ext| ext != "md") {
        return false;
    }
    let Ok(content) = aicx::sanitize::read_to_string_validated(path) else {
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
        if let Some(m) = month_number(&lower[i])
            && let Ok(y) = lower[i + 1].parse::<u32>()
            && (2020..=2099).contains(&y)
        {
            let days = days_in_month(y, m);
            let lo = format!("{y:04}-{m:02}-01");
            let hi = format!("{y:04}-{m:02}-{days:02}");
            date_filter = Some(format!("{lo}..{hi}"));
            used[i] = true;
            used[i + 1] = true;
        }
    }

    // Pattern 2: "<year> <month>" e.g. "2026 january"
    if date_filter.is_none() {
        for i in 0..words.len().saturating_sub(1) {
            if let Ok(y) = lower[i].parse::<u32>()
                && (2020..=2099).contains(&y)
                && let Some(m) = month_number(&lower[i + 1])
            {
                let days = days_in_month(y, m);
                let lo = format!("{y:04}-{m:02}-01");
                let hi = format!("{y:04}-{m:02}-{days:02}");
                date_filter = Some(format!("{lo}..{hi}"));
                used[i] = true;
                used[i + 1] = true;
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
            if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) {
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
    json: bool,
    filters: RetrievalFilters,
    no_semantic: bool,
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
    let fetch_limit = if effective_date.is_some()
        || filters.score.is_some()
        || hours > 0
        || filters.since.is_some()
        || filters.until.is_some()
    {
        filters.limit.saturating_mul(5).max(50)
    } else {
        filters.limit
    };

    // Try semantic-first dispatch unless operator forced fuzzy with
    // `--no-semantic`. The semantic path resolves quickly to a typed
    // SearchPath::Fallback when the embedder is unavailable or the
    // vector index has not yet been built, so the cost of trying is
    // bounded.
    let semantic_path = if no_semantic {
        aicx::search_engine::SearchPath::Fallback {
            reason: "operator passed --no-semantic".to_string(),
        }
    } else {
        match aicx::search_engine::try_semantic_search(
            &root,
            &search_query,
            fetch_limit,
            project,
            filters.frame_kind.map(Into::into),
        ) {
            Ok(path) => path,
            Err(err) => aicx::search_engine::SearchPath::Fallback {
                reason: format!("semantic dispatch errored: {err}"),
            },
        }
    };

    // Iter 1 always lands in the Fallback arm because no vector index is
    // wired yet. Iter 2 will materialize the index and the Semantic arm
    // will start returning real hits.
    let (results, scanned) = match &semantic_path {
        aicx::search_engine::SearchPath::Semantic(outcome) => {
            (outcome.results.clone(), outcome.scanned)
        }
        aicx::search_engine::SearchPath::Fallback { .. } => rank::fuzzy_search_store(
            &root,
            &search_query,
            fetch_limit,
            project,
            filters.frame_kind.map(Into::into),
        )?,
    };

    let mut results = results;

    if let Some(min_score) = filters.score {
        results.retain(|r| r.score >= min_score);
    }
    if let Some(agent_filter) = &filters.agent {
        results.retain(|r| r.agent == *agent_filter);
    }

    // Apply date filter (day granularity) — takes priority over hours.
    let (lo, hi) = if let Some(ref d) = effective_date {
        let bounds = parse_date_filter(d)?;
        (bounds.0, bounds.1)
    } else {
        (filters.since.clone(), filters.until.clone())
    };

    let mut results: Vec<_> = if lo.is_some() || hi.is_some() {
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

    if let Some(sort_order) = filters.sort {
        results.sort_by(|a, b| {
            let t_a = a.timestamp.as_deref().unwrap_or(a.date.as_str());
            let t_b = b.timestamp.as_deref().unwrap_or(b.date.as_str());
            match sort_order {
                SortOrder::Newest => t_b.cmp(t_a),
                SortOrder::Oldest => t_a.cmp(t_b),
                SortOrder::Score => b.score.cmp(&a.score).then(t_b.cmp(t_a)),
            }
        });
    } else {
        // default sort
        results.sort_by_key(|b| std::cmp::Reverse(b.score));
    }

    // Truncate to requested limit after date filtering
    let results: Vec<_> = results.into_iter().take(filters.limit).collect();

    if json {
        println!("{}", rank::render_search_json(&root, &results, scanned)?);
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
            "\n{}",
            aicx::search_engine::render_oracle_status_line(&semantic_path, results.len(), scanned)
        );
    }
    Ok(())
}

/// Default canonical config template written by `aicx config init`.
///
/// Set up to advertise cloud-embedder as the recommended VetCoders
/// production default with concrete provider examples; the native GGUF
/// section ships fully-commented so operators can flip backends without
/// hunting for the schema.
const DEFAULT_CONFIG_TOML: &str = r#"# aicx — Vibecrafted with AI Agents (c)2026 VetCoders
#
# Canonical AICX configuration. Loaded by `aicx` (CLI), `aicx-mcp`,
# and any in-process consumer of the embedder. Field precedence
# (highest first):
#   1. AICX_EMBEDDER_CONFIG env var  (explicit path override)
#   2. ~/.aicx/embedder.toml          (legacy, native fields only)
#   3. ~/.aicx/config.toml            (this file — canonical)
#   4. AICX_EMBEDDER_*                (per-field env overrides)
#
# Edit and re-save. No restart needed; aicx reloads on every invocation.

[embedder]
# Recommended VetCoders default: cloud HTTP embedder, zero-install,
# config-driven URL/model/API key. Switch to "gguf" for offline / dev
# workstations with native llama.cpp inference. Use "auto" to let the
# binary pick the strongest compiled-in backend.
backend = "cloud"

# Native GGUF profile (only consulted when backend = "gguf" or "auto"):
#   "base"    — F2LLM 0.6B Q4_K_M  (~397 MB, 1024 dim)
#   "dev"     — F2LLM 1.7B Q4_K_M  (~1.1 GB, 2048 dim)
#   "premium" — F2LLM 1.7B Q6_K    (~1.4 GB, 2048 dim)
profile = "base"

[embedder.cloud]
# OpenAI-compatible /v1/embeddings endpoint. Replace with your provider.
#   OpenAI:           https://api.openai.com/v1/embeddings
#   Voyage AI:        https://api.voyageai.com/v1/embeddings
#   Together AI:      https://api.together.xyz/v1/embeddings
#   OpenRouter:       https://openrouter.ai/api/v1/embeddings
#   Ollama local:     http://localhost:11434/v1/embeddings
#   Local LM Studio:  http://localhost:1234/v1/embeddings
#
# Local provider caveat: Ollama measured ~38s first-call coldstart
# from idle on 2026-05-06, then warm calls are much faster. Local
# providers are excellent for batched `aicx index` workflows where
# startup amortizes over many chunks. For one-shot CLI search, remote
# cloud providers usually feel faster. Run `aicx warmup` after idle to
# pre-load local daemons before an interactive search session.
url = "https://api.openai.com/v1/embeddings"

# Model identifier as accepted by the provider:
#   OpenAI:    text-embedding-3-small (1536 dim) | text-embedding-3-large (3072 dim)
#   Voyage:    voyage-3 (1024 dim) | voyage-large-2 (1536 dim)
#   Together:  BAAI/bge-large-en-v1.5 (1024 dim)
model = "text-embedding-3-small"

# Env var name holding the API key. Resolved at call time so secrets
# never sit in config files. Set the env var before running aicx:
#   export OPENAI_API_KEY=sk-...
api_key_env = "OPENAI_API_KEY"

# Output dimension (informational; some providers do not echo it).
dimension = 1536

# Request timeout in seconds.
timeout_secs = 30

# Optional extra headers (rarely needed; uncomment to use):
# [embedder.cloud.headers]
# "X-Trace-Id" = "vetcoders-aicx"
"#;

fn canonical_config_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve home directory for ~/.aicx/config.toml"))?;
    Ok(home.join(".aicx").join("config.toml"))
}

/// Dispatch `aicx config <action>`.
fn run_config(action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Init { force, path } => run_config_init(force, path),
        ConfigAction::Show { json } => run_config_show(json),
    }
}

/// Write the canonical config.toml template, refusing to overwrite
/// without `--force` so an operator never loses hand-tuned settings to
/// a stray init.
fn run_config_init(force: bool, path: Option<PathBuf>) -> Result<()> {
    let target = match path {
        Some(p) => p,
        None => canonical_config_path()?,
    };

    if target.exists() && !force {
        anyhow::bail!(
            "config file already exists at {}; pass --force to overwrite, or edit it directly",
            target.display()
        );
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create config directory at {}", parent.display())
        })?;
    }

    std::fs::write(&target, DEFAULT_CONFIG_TOML)
        .with_context(|| format!("failed to write config to {}", target.display()))?;

    eprintln!("aicx config init -> wrote {}", target.display());
    eprintln!("Edit it to set your endpoint / model / API key env var, then:");
    eprintln!("  export OPENAI_API_KEY=sk-...   # or your provider equivalent");
    eprintln!("  aicx search 'your query'");

    Ok(())
}

/// Print the resolved [`aicx_parser`]-compatible embedder config so the
/// operator can verify what backend / model / dimension will actually
/// run for the next `aicx search`.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn run_config_show(json: bool) -> Result<()> {
    let cfg = aicx::embedder::EmbeddingConfig::from_env();
    let resolved = cfg.resolved_model();
    let cloud_set = cfg.cloud.is_some();

    if json {
        let payload = serde_json::json!({
            "backend": cfg.backend.as_str(),
            "profile": cfg.profile.as_str(),
            "resolved_native": {
                "repo": resolved.repo,
                "filename": resolved.filename,
                "dimension_hint": resolved.dimension_hint,
                "approx_size": resolved.approx_size,
                "from_legacy_repo": resolved.from_legacy_repo,
            },
            "cloud": cfg.cloud.as_ref().map(|c| serde_json::json!({
                "url": c.url,
                "model": c.model,
                "api_key_env": c.api_key_env,
                "dimension": c.effective_dimension(),
                "timeout_secs": c.effective_timeout_secs(),
            })),
            "config_path": canonical_config_path().ok().map(|p| p.display().to_string()),
            "cloud_section_present": cloud_set,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    let path_display = canonical_config_path()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unresolved>".to_string());

    eprintln!("aicx config show — resolved embedder configuration");
    eprintln!("  config_path: {path_display}");
    eprintln!("  backend:     {}", cfg.backend.as_str());
    eprintln!("  profile:     {}", cfg.profile.as_str());
    eprintln!("  native.repo:           {}", resolved.repo);
    eprintln!("  native.filename:       {}", resolved.filename);
    eprintln!("  native.dimension_hint: {}", resolved.dimension_hint);
    eprintln!("  native.approx_size:    {}", resolved.approx_size);
    if resolved.from_legacy_repo {
        eprintln!("  native.from_legacy_repo: true (auto-mapped to F2LLM GGUF)");
    }
    if let Some(cloud) = &cfg.cloud {
        eprintln!("  cloud.url:           {}", cloud.url);
        eprintln!("  cloud.model:         {}", cloud.model);
        eprintln!(
            "  cloud.api_key_env:   {}",
            cloud.api_key_env.as_deref().unwrap_or("<unset>")
        );
        eprintln!("  cloud.dimension:     {}", cloud.effective_dimension());
        eprintln!("  cloud.timeout_secs:  {}", cloud.effective_timeout_secs());
    } else {
        eprintln!("  cloud:               <not configured> (run `aicx config init` to bootstrap)");
    }
    Ok(())
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
fn run_config_show(_json: bool) -> Result<()> {
    eprintln!(
        "aicx was built without any embedder feature. \
         Rebuild with `cargo install --features cloud-embedder` (recommended) \
         or `--features native-embedder` (offline GGUF)."
    );
    Ok(())
}

/// Build (or preview) the vector index. Iter 2 ships dry-run only.
fn run_index(project: Option<&str>, sample: usize, json: bool, dry_run: bool) -> Result<()> {
    if !dry_run {
        anyhow::bail!(
            "Iter 2 ships dry-run only; persistent Lance write lands in Iter 3. \
             Pass --dry-run=true (the default) for now."
        );
    }
    let stats = aicx::vector_index::dry_run_index(project, sample)?;
    if json {
        println!("{}", aicx::vector_index::render_stats_json(&stats)?);
    } else {
        eprint!("{}", aicx::vector_index::render_stats_text(&stats));
    }
    Ok(())
}

fn run_index_status(project: Option<&str>, json: bool) -> Result<()> {
    let client = aicx::Aicx::from_env()?;
    let status = client.index_status(project)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        eprintln!("aicx index status");
        eprintln!("  canonical_chunks:       {}", status.canonical_chunks);
        eprintln!(
            "  semantic_index_present: {}",
            status.semantic_index_present
        );
        eprintln!(
            "  newest_chunk_mtime:     {}",
            status.newest_chunk_mtime.as_deref().unwrap_or("<none>")
        );
        eprintln!(
            "  semantic_index_mtime:   {}",
            status.semantic_index_mtime.as_deref().unwrap_or("<none>")
        );
        eprintln!(
            "  semantic_lag_secs:      {}",
            status
                .semantic_lag_secs
                .map(|value| value.to_string())
                .unwrap_or_else(|| "<unknown>".to_string())
        );
        eprintln!("  pending_chunks:         {}", status.pending_chunks);
    }
    Ok(())
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn run_warmup(json: bool) -> Result<()> {
    let cfg = aicx::embedder::EmbeddingConfig::from_env();
    if cfg.backend == aicx::embedder::BackendPreference::Cloud
        && cfg.cloud.as_ref().is_some_and(|cloud| {
            !cloud.url.contains("localhost:")
                && !cloud.url.contains("127.0.0.1:")
                && !cloud.url.contains("0.0.0.0:")
        })
    {
        let payload = serde_json::json!({
            "skipped": true,
            "reason": "remote cloud backend; warmth probe skipped to avoid paid/noisy calls",
            "time_to_first_vector_ms": null,
        });
        if json {
            println!("{}", serde_json::to_string_pretty(&payload)?);
        } else {
            eprintln!("aicx warmup skipped: remote cloud backend");
        }
        return Ok(());
    }

    let start = std::time::Instant::now();
    let stats = aicx::vector_index::dry_run_index(None, 1)?;
    let elapsed = start.elapsed();
    let payload = serde_json::json!({
        "skipped": false,
        "time_to_first_vector_ms": elapsed.as_millis(),
        "embedded_chunks": stats.embeddings_computed,
        "model_id": stats.model_id,
        "model_profile": stats.model_profile,
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        eprintln!(
            "aicx warmup: first vector in {} ms ({} chunk probe)",
            elapsed.as_millis(),
            stats.embeddings_computed
        );
    }
    Ok(())
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
fn run_warmup(json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "skipped": true,
                "reason": "binary built without embedder features",
                "time_to_first_vector_ms": null,
            }))?
        );
    } else {
        eprintln!("aicx warmup unavailable: binary built without embedder features");
    }
    Ok(())
}

/// Read one canonical chunk and print metadata plus content.
fn run_read(reference: &str, max_chars: Option<usize>, json: bool) -> Result<()> {
    let chunk = store::read_context_chunk(reference, max_chars)?;

    if json {
        println!("{}", serde_json::to_string(&chunk)?);
        return Ok(());
    }

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    writeln!(
        out,
        "{} | {} | {} | {} | chunk {:03}",
        chunk.project, chunk.agent, chunk.date, chunk.kind, chunk.chunk
    )?;
    writeln!(out, "session: {}", chunk.session_id)?;
    writeln!(out, "path: {}", chunk.path.display())?;
    writeln!(out, "relative: {}", chunk.relative_path)?;
    writeln!(out, "bytes: {}", chunk.bytes)?;
    if chunk.truncated {
        writeln!(out, "truncated: true")?;
    }
    writeln!(out)?;
    write!(out, "{}", chunk.content)?;
    if !chunk.content.ends_with('\n') {
        writeln!(out)?;
    }
    out.flush()?;

    Ok(())
}

/// Retrieve chunks by steering metadata (frontmatter sidecar fields).
fn run_steer(
    run_id: Option<&str>,
    prompt_id: Option<&str>,
    kind: Option<&str>,
    project: Option<&str>,
    date: Option<&str>,
    json: bool,
    filters: RetrievalFilters,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;

    let effective_date = date;
    let (date_lo, date_hi) = if let Some(d) = effective_date {
        let bounds = parse_date_filter(d)?;
        (bounds.0, bounds.1)
    } else {
        (filters.since.clone(), filters.until.clone())
    };

    let filter = aicx::steer_index::SteerFilter {
        run_id,
        prompt_id,
        agent: filters.agent.as_deref(),
        kind,
        frame_kind: filters.frame_kind.map(Into::into),
        project,
        date_lo: date_lo.as_deref(),
        date_hi: date_hi.as_deref(),
    };
    let mut metadatas = rt.block_on(aicx::steer_index::search_steer_index(
        &filter,
        filters.limit,
    ))?;

    if let Some(sort_order) = filters.sort {
        metadatas.sort_by(|a, b| {
            let t_a = a
                .get("timestamp")
                .and_then(|v| v.as_str())
                .or_else(|| a.get("date").and_then(|v| v.as_str()))
                .unwrap_or("");
            let t_b = b
                .get("timestamp")
                .and_then(|v| v.as_str())
                .or_else(|| b.get("date").and_then(|v| v.as_str()))
                .unwrap_or("");
            match sort_order {
                SortOrder::Newest => t_b.cmp(t_a),
                SortOrder::Oldest => t_a.cmp(t_b),
                SortOrder::Score => std::cmp::Ordering::Equal, // steer_index has no score natively, ignore
            }
        });
    }

    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let color = stdout.is_terminal();
    let matched = metadatas.len();
    let store_root = store::store_base_dir()?;
    let oracle_status = aicx::oracle::OracleStatus::metadata_steer(
        &store_root,
        matched,
        matched,
        aicx::oracle::verify_paths(metadatas.iter().filter_map(|meta| {
            meta.get("path")
                .or_else(|| meta.get("source_chunk"))
                .and_then(|value| value.as_str())
                .map(std::path::PathBuf::from)
        })),
    );

    if json {
        let json = serde_json::to_string_pretty(&aicx::oracle::OracleEnvelope {
            oracle_status,
            results: metadatas.len(),
            items: &metadatas,
        })?;
        println!("{json}");
        return Ok(());
    }

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
        eprintln!(
            "{matched} match(es) from steer index. oracle_status: backend=steer_metadata index=metadata_steer derived=rebuildable_from_canonical_chunks loctree_scope_safe={}",
            oracle_status.loctree_scope_safe
        );
    }

    Ok(())
}

/// List chunks in the canonical store, filtered by recency.
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

struct DashboardServerRunArgs {
    store_root: Option<PathBuf>,
    scope: DashboardScope,
    host: String,
    port: u16,
    no_open: bool,
    bg: bool,
    allow_cors_origins: Option<String>,
    artifact: PathBuf,
    title: String,
    preview_chars: usize,
}

/// Run dashboard server mode with lightweight HTML shell and API-backed regeneration.
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
    let cors_policy = DashboardCorsPolicy::from_cli(args.allow_cors_origins.as_deref())?;
    dashboard_server::validate_dashboard_host_policy(
        host,
        &cors_policy,
        args.allow_cors_origins.is_some(),
    )?;
    let artifact_path = args.artifact;

    if args.bg {
        return spawn_dashboard_server_background(
            root,
            args.scope,
            host,
            args.port,
            &args.title,
            args.preview_chars,
            args.allow_cors_origins.as_deref(),
        );
    }

    if !host.is_loopback() {
        eprintln!(
            "! Warning: dashboard server is binding beyond loopback on http://{}:{}",
            host, args.port
        );
        eprintln!("  CORS policy: {}", cors_policy.describe());
    }

    let config = DashboardServerConfig {
        store_root: root,
        scope: args.scope,
        title: args.title,
        preview_chars: args.preview_chars,
        artifact_path,
        cors_policy,
        host,
        port: args.port,
    };

    if !args.no_open {
        let url = format!("http://{}:{}", host, args.port);
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open").arg(&url).spawn();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create tokio runtime for dashboard server")?;

    runtime.block_on(dashboard_server::run_dashboard_server(config))
}

fn spawn_dashboard_server_background(
    store_root: PathBuf,
    scope: DashboardScope,
    host: std::net::IpAddr,
    port: u16,
    title: &str,
    preview_chars: usize,
    allow_cors_origins: Option<&str>,
) -> Result<()> {
    let current_exe = std::env::current_exe().context("Resolve current aicx executable")?;
    let mut command = std::process::Command::new(&current_exe);
    command
        .arg("dashboard")
        .arg("--serve")
        .arg("--no-open")
        .arg("--host")
        .arg(host.to_string())
        .arg("--port")
        .arg(port.to_string())
        .arg("--store-root")
        .arg(store_root.as_os_str());

    if let Some(project) = scope.project.as_deref() {
        command.arg("--project").arg(project);
    }
    if let Some(hours) = scope.hours {
        command.arg("--hours").arg(hours.to_string());
    }
    if let Some(policy) = allow_cors_origins {
        command.arg("--allow-cors-origins").arg(policy);
    }
    if title != DEFAULT_DASHBOARD_TITLE {
        command.arg("--title").arg(title);
    }
    if preview_chars != 320 {
        command
            .arg("--preview-chars")
            .arg(preview_chars.to_string());
    }

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let child = command.spawn().with_context(|| {
        format!(
            "Spawn background dashboard server via {}",
            current_exe.display()
        )
    })?;

    eprintln!("✓ Dashboard server launched in background");
    eprintln!("  PID: {}", child.id());
    eprintln!("  URL: http://{}:{}", host, port);
    eprintln!("  Store: {}", store_root.display());
    Ok(())
}

/// Build and write an AI context dashboard HTML file.
struct DashboardRunArgs {
    store_root: Option<PathBuf>,
    scope: DashboardScope,
    output: PathBuf,
    title: String,
    preview_chars: usize,
}

fn default_dashboard_output_path() -> Result<PathBuf> {
    Ok(store::store_base_dir()?.join("aicx-dashboard.html"))
}

fn run_dashboard_command(args: DashboardArgs) -> Result<()> {
    if args.serve && args.generate_html {
        return Err(anyhow::anyhow!(
            "Choose either --serve or --generate-html, not both."
        ));
    }

    if args.serve {
        if args.output.is_some() {
            return Err(anyhow::anyhow!(
                "--output is only valid with generated HTML mode. Use `aicx dashboard --generate-html -o <path>`."
            ));
        }

        return run_dashboard_server(DashboardServerRunArgs {
            store_root: args.store_root,
            scope: DashboardScope {
                project: args.project,
                hours: args.hours,
            },
            host: args.host.unwrap_or_else(|| "127.0.0.1".to_string()),
            port: args.port.unwrap_or(9478),
            no_open: args.no_open,
            bg: args.bg,
            allow_cors_origins: args.allow_cors_origins,
            artifact: default_dashboard_output_path()?,
            title: args.title,
            preview_chars: args.preview_chars,
        });
    }

    if args.host.is_some()
        || args.port.is_some()
        || args.no_open
        || args.bg
        || args.allow_cors_origins.is_some()
    {
        return Err(anyhow::anyhow!(
            "--host, --port, --no-open, --bg, and --allow-cors-origins are only valid with --serve."
        ));
    }

    if !args.generate_html {
        eprintln!("# Tip: add --serve for live HTTP server mode");
    }

    run_dashboard(DashboardRunArgs {
        store_root: args.store_root,
        scope: DashboardScope {
            project: args.project,
            hours: args.hours,
        },
        output: args.output.unwrap_or(default_dashboard_output_path()?),
        title: args.title,
        preview_chars: args.preview_chars,
    })
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
        scope: args.scope,
    };

    let artifact = dashboard::build_dashboard(&config)?;

    let mut output_path = aicx::sanitize::validate_write_path(&args.output)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output directory: {}", parent.display()))?;
    }
    output_path = aicx::sanitize::validate_write_path(&output_path)?;
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

/// Build a standalone HTML explorer for Vibecrafted report artifacts.
struct ReportsExtractorRunArgs {
    artifacts_root: Option<PathBuf>,
    org: String,
    repo: Option<String>,
    workflow: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
    output: PathBuf,
    bundle_output: Option<PathBuf>,
    title: String,
    preview_chars: usize,
}

fn default_reports_output_path() -> Result<PathBuf> {
    Ok(store::store_base_dir()?.join("aicx-reports.html"))
}

fn run_reports_command(args: ReportsArgs) -> Result<()> {
    run_reports_extractor(ReportsExtractorRunArgs {
        artifacts_root: args.artifacts_root,
        org: args.org,
        repo: args.repo,
        workflow: args.workflow,
        date_from: args.date_from,
        date_to: args.date_to,
        output: args.output.unwrap_or(default_reports_output_path()?),
        bundle_output: args.bundle_output,
        title: args.title,
        preview_chars: args.preview_chars,
    })
}

fn run_corpus_command(args: CorpusArgs) -> Result<()> {
    match args.command {
        CorpusCommand::Audit(audit_args) => {
            let report = corpus::audit(&corpus::CorpusAuditOptions {
                roots: audit_args.roots.root,
            })?;
            if matches!(audit_args.emit, CorpusEmit::Json) {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", corpus::format_audit_text(&report));
            }
        }
        CorpusCommand::Repair(repair_args) => {
            let repair_manifest = corpus::repair(&corpus::CorpusRepairOptions {
                roots: repair_args.roots.root,
                dry_run: repair_args.dry_run,
                apply: repair_args.apply,
                backup: repair_args.backup,
                manifest_path: repair_args.manifest,
            })?;
            if matches!(repair_args.emit, CorpusEmit::Json) {
                println!("{}", serde_json::to_string_pretty(&repair_manifest)?);
            } else {
                print!("{}", corpus::format_repair_text(&repair_manifest));
            }
        }
    }

    Ok(())
}

fn run_reports_extractor(args: ReportsExtractorRunArgs) -> Result<()> {
    let artifacts_root = if let Some(path) = args.artifacts_root {
        path
    } else {
        default_vibecrafted_artifacts_root()?
    };
    let repo = if let Some(repo) = args.repo {
        repo
    } else {
        infer_repo_name_from_cwd()?
    };
    let date_from = parse_cli_date(args.date_from.as_deref(), "--date-from")?;
    let date_to = parse_cli_date(args.date_to.as_deref(), "--date-to")?;
    let bundle_output = args
        .bundle_output
        .clone()
        .unwrap_or_else(|| default_reports_bundle_path(&args.output));
    let config = ReportsExtractorConfig {
        artifacts_root: artifacts_root.clone(),
        org: args.org,
        repo: repo.clone(),
        date_from,
        date_to,
        workflow: args.workflow,
        title: args.title,
        preview_chars: args.preview_chars,
    };

    let artifact = reports_extractor::build_reports_explorer(&config)?;
    write_text_output(&args.output, &artifact.html, "report explorer HTML")?;
    write_text_output(
        &bundle_output,
        &artifact.bundle_json,
        "report explorer JSON bundle",
    )?;

    eprintln!("✓ Vibecrafted reports extracted");
    eprintln!("  Repo: {}/{}", config.org, repo);
    eprintln!("  Artifacts: {}", artifacts_root.display());
    eprintln!("  HTML: {}", args.output.display());
    eprintln!("  Bundle: {}", bundle_output.display());
    eprintln!(
        "  Stats: {} records, {} completed, {} incomplete, {} workflows",
        artifact.stats.total_records,
        artifact.stats.completed_records,
        artifact.stats.incomplete_records,
        artifact.stats.total_workflows
    );
    println!("{}", args.output.display());
    Ok(())
}

fn default_vibecrafted_artifacts_root() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    Ok(home.join(".vibecrafted").join("artifacts"))
}

fn default_reports_bundle_path(output: &Path) -> PathBuf {
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let stem = output
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("aicx-reports");
    parent.join(format!("{stem}.bundle.json"))
}

fn infer_repo_name_from_cwd() -> Result<String> {
    let cwd = std::env::current_dir().context("Cannot determine current directory")?;
    let mut probe = cwd.as_path();
    loop {
        if probe.join(".git").exists() {
            let repo = probe
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| anyhow::anyhow!("Could not infer --repo from git root"))?;
            return Ok(repo.to_string());
        }
        let Some(parent) = probe.parent() else {
            break;
        };
        probe = parent;
    }

    let repo = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("Could not infer --repo from the current directory"))?;
    Ok(repo.to_string())
}

fn parse_cli_date(value: Option<&str>, flag_name: &str) -> Result<Option<NaiveDate>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let formats = ["%Y-%m-%d", "%Y_%m%d"];
    for format in formats {
        if let Ok(date) = NaiveDate::parse_from_str(value, format) {
            return Ok(Some(date));
        }
    }
    Err(anyhow::anyhow!(
        "Invalid {} value '{}'. Use YYYY-MM-DD or YYYY_MMDD.",
        flag_name,
        value
    ))
}

fn write_text_output(path: &Path, content: &str, label: &str) -> Result<()> {
    let mut validated = aicx::sanitize::validate_write_path(path)?;
    if let Some(parent) = validated.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output directory: {}", parent.display()))?;
    }
    validated = aicx::sanitize::validate_write_path(&validated)?;
    fs::write(&validated, content)
        .with_context(|| format!("Failed to write {}: {}", label, validated.display()))
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
    fn ingest_accepts_operator_markdown_source_and_since() {
        let cli = Cli::try_parse_from([
            "aicx",
            "ingest",
            "--source",
            "operator-md",
            "--since",
            "2026-05-01",
            "--emit",
            "json",
        ])
        .expect("operator markdown ingest command should parse");

        match cli.command {
            Some(Commands::Ingest {
                source,
                since,
                emit,
                ..
            }) => {
                assert!(matches!(source, IngestSource::OperatorMd));
                assert_eq!(since.as_deref(), Some("2026-05-01"));
                assert!(matches!(emit, StdoutEmit::Json));
            }
            other => panic!("expected ingest command, got {:?}", other.map(|_| "other")),
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
            Some(Commands::Search { filters, json, .. }) => {
                assert_eq!(filters.score, Some(60));
                assert!(json);
            }
            _ => panic!("expected search command"),
        }
    }

    #[test]
    fn search_accepts_frame_kind_filter() {
        let cli = Cli::try_parse_from([
            "aicx",
            "search",
            "dashboard",
            "--frame-kind",
            "internal_thought",
        ])
        .expect("search command with frame-kind should parse");

        match cli.command {
            Some(Commands::Search { filters, .. }) => {
                assert_eq!(filters.frame_kind, Some(FrameKindArg::InternalThought));
            }
            _ => panic!("expected search command"),
        }
    }

    #[test]
    fn steer_accepts_frame_kind_filter() {
        let cli = Cli::try_parse_from(["aicx", "steer", "--frame-kind", "user_msg"])
            .expect("steer command with frame-kind should parse");

        match cli.command {
            Some(Commands::Steer { filters, .. }) => {
                assert_eq!(filters.frame_kind, Some(FrameKindArg::UserMsg));
            }
            _ => panic!("expected steer command"),
        }
    }

    #[test]
    fn intents_accepts_frame_kind_filter() {
        let cli = Cli::try_parse_from([
            "aicx",
            "intents",
            "--project",
            "ai-contexters",
            "--frame-kind",
            "tool_call",
        ])
        .expect("intents command with frame-kind should parse");

        match cli.command {
            Some(Commands::Intents { filters, .. }) => {
                assert_eq!(filters.frame_kind, Some(FrameKindArg::ToolCall));
            }
            _ => panic!("expected intents command"),
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
    fn top_level_help_hides_retired_init_from_primary_surface() {
        let mut cmd = Cli::command();
        let rendered = cmd.render_help().to_string();

        assert!(!rendered.contains("\n  init "));
        assert!(!rendered.contains("Retired compatibility shim"));
        assert!(!rendered.contains("Initialize repo context and run an agent"));
    }

    #[test]
    fn top_level_help_does_not_advertise_dead_root_flags() {
        let mut cmd = Cli::command();
        let rendered = cmd.render_long_help().to_string();

        assert!(!rendered.contains("used if no subcommand is provided"));
        assert!(!rendered.contains("Project filter (used if no subcommand is provided)"));
        assert!(!rendered.contains("Hours to look back (used if no subcommand is provided)"));
    }

    #[test]
    fn top_level_help_uses_semantic_index_language() {
        let mut cmd = Cli::command();
        let rendered = cmd.render_long_help().to_string();

        assert!(rendered.contains("Layer 2 (optional semantic index)"));
        assert!(!rendered.contains("retrieval kernel"));
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
    fn serve_accepts_http_and_legacy_sse_transport_names() {
        let http = Cli::try_parse_from(["aicx", "serve", "--transport", "http"])
            .expect("http transport should parse");
        let legacy = Cli::try_parse_from(["aicx", "serve", "--transport", "sse"])
            .expect("legacy sse alias should parse");

        match http.command {
            Some(Commands::Serve { transport, .. }) => {
                assert_eq!(transport, McpTransport::Http);
            }
            _ => panic!("expected serve command for http transport"),
        }

        match legacy.command {
            Some(Commands::Serve { transport, .. }) => {
                assert_eq!(transport, McpTransport::Http);
            }
            _ => panic!("expected serve command for legacy sse transport"),
        }
    }

    #[test]
    fn serve_help_prefers_http_name_and_explains_search_fallback() {
        let mut cmd = Cli::command();
        let serve = cmd
            .find_subcommand_mut("serve")
            .expect("serve subcommand should exist");
        let rendered = serve.render_long_help().to_string();

        assert!(rendered.contains("Transport: stdio (default) or http."));
        assert!(!rendered.contains("Transport: stdio (default) or sse"));
        assert!(rendered.contains("cannot mistake it for semantic retrieval"));
        assert!(!rendered.contains("embedding mode"));
    }

    #[test]
    fn search_help_explains_semantic_first_with_fuzzy_fallback() {
        // After the Iter 1 dispatch flip, `aicx search` is intentionally
        // semantic-first with an explicit filesystem-fuzzy fallback. The
        // help text must surface both legs of the contract so operators
        // know which retrieval ran (and why) when reading `--help`.
        let mut cmd = Cli::command();
        let search = cmd
            .find_subcommand_mut("search")
            .expect("search subcommand should exist");
        let rendered = search.render_long_help().to_string();

        // Semantic leg must be visible — this is the new default.
        assert!(
            rendered.to_lowercase().contains("semantic"),
            "search --help must mention semantic retrieval (the new default)"
        );
        // Fuzzy leg must be visible too — operators need to know it is
        // the fallback, not a hidden behaviour.
        assert!(
            rendered.to_lowercase().contains("fuzzy"),
            "search --help must mention fuzzy as the explicit fallback"
        );
        // Fallback contract must be named, not implied.
        assert!(
            rendered.to_lowercase().contains("fallback"),
            "search --help must call out the fallback path explicitly"
        );
        // Old "filesystem-only" framing must be gone — it would mislead
        // operators about what a build with `native-embedder` actually does.
        assert!(
            !rendered.contains("filesystem-only"),
            "search --help must not advertise the legacy filesystem-only contract"
        );
    }

    #[test]
    fn read_command_parses_discover_path_and_json_mode() {
        let cli = Cli::try_parse_from([
            "aicx",
            "read",
            "store/VetCoders/aicx/2026_0502/reports/codex/chunk.md",
            "--max-chars",
            "400",
            "--json",
        ])
        .expect("read command should parse");

        match cli.command {
            Some(Commands::Read {
                reference,
                max_chars,
                json,
            }) => {
                assert_eq!(
                    reference,
                    "store/VetCoders/aicx/2026_0502/reports/codex/chunk.md"
                );
                assert_eq!(max_chars, Some(400));
                assert!(json);
            }
            _ => panic!("expected read command"),
        }
    }

    #[test]
    fn steer_help_keeps_examples_split() {
        let mut cmd = Cli::command();
        let steer = cmd
            .find_subcommand_mut("steer")
            .expect("steer subcommand should exist");
        let rendered = steer.render_long_help().to_string();

        assert!(rendered.contains("aicx steer --run-id mrbl-001"));
        assert!(
            rendered
                .contains("aicx steer --project ai-contexters --kind reports --date 2026-03-28")
        );
        assert!(!rendered.contains("mrbl-001 aicx steer"));
        assert!(!rendered.contains("--no-redact-secrets"));
        assert!(!rendered.contains("--hours <HOURS>"));
    }

    #[test]
    fn top_level_help_hides_legacy_dashboard_and_reports_commands() {
        let mut cmd = Cli::command();
        let rendered = cmd.render_long_help().to_string();

        assert!(!rendered.contains("dashboard-serve"));
        assert!(!rendered.contains("reports-extractor"));
        assert!(rendered.contains("\n  dashboard "));
        assert!(rendered.contains("\n  reports "));
    }

    #[test]
    fn dashboard_help_describes_generate_and_serve_modes() {
        let mut cmd = Cli::command();
        let dashboard = cmd
            .find_subcommand_mut("dashboard")
            .expect("dashboard subcommand should exist");
        let rendered = dashboard.render_long_help().to_string();

        assert!(rendered.contains("--serve"));
        assert!(rendered.contains("--generate-html"));
        assert!(rendered.contains("~/.aicx/aicx-dashboard.html"));
        assert!(rendered.contains("--project <PROJECT>"));
        assert!(rendered.contains("--hours <HOURS>"));
        assert!(rendered.contains("--bg"));
        assert!(rendered.contains("--allow-cors-origins"));
        assert!(!rendered.contains("--artifact"));
    }

    #[test]
    fn dashboard_server_only_flags_require_serve_mode() {
        let err = Cli::try_parse_from(["aicx", "dashboard", "--host", "0.0.0.0"])
            .expect_err("server-only host flag should require --serve");
        let rendered = err.to_string();

        assert!(rendered.contains("--serve"));
    }

    #[test]
    fn dashboard_server_remote_flags_parse_with_explicit_cors_policy() {
        let cli = Cli::try_parse_from([
            "aicx",
            "dashboard",
            "--serve",
            "--host",
            "0.0.0.0",
            "--allow-cors-origins",
            "all",
            "--bg",
        ])
        .expect("remote dashboard serve flags should parse");

        match cli.command {
            Some(Commands::Dashboard(args)) => {
                assert!(args.serve);
                assert!(args.bg);
                assert_eq!(args.host.as_deref(), Some("0.0.0.0"));
                assert_eq!(args.allow_cors_origins.as_deref(), Some("all"));
            }
            _ => panic!("expected dashboard command"),
        }
    }

    #[test]
    fn reports_help_describes_embedded_html_and_bundle() {
        let mut cmd = Cli::command();
        let reports = cmd
            .find_subcommand_mut("reports")
            .expect("reports subcommand should exist");
        let rendered = reports.render_long_help().to_string();

        assert!(rendered.contains("standalone HTML explorer"));
        assert!(rendered.contains("~/.vibecrafted/artifacts"));
        assert!(rendered.contains("~/.aicx/aicx-reports.html"));
        assert!(rendered.contains("--bundle-output"));
        assert!(rendered.contains("--date-from"));
        assert!(rendered.contains("--date-to"));
        assert!(!rendered.contains("canonical store"));
    }

    #[test]
    fn corpus_audit_and_repair_commands_parse() {
        let audit = Cli::try_parse_from(["aicx", "corpus", "audit", "--emit", "json"])
            .expect("corpus audit should parse");
        match audit.command {
            Some(Commands::Corpus(CorpusArgs {
                command: CorpusCommand::Audit(args),
            })) => assert!(matches!(args.emit, CorpusEmit::Json)),
            _ => panic!("expected corpus audit command"),
        }

        let repair = Cli::try_parse_from([
            "aicx",
            "corpus",
            "repair",
            "--root",
            "/tmp/aicx-store",
            "--dry-run",
            "--backup",
            "--manifest",
            "/tmp/aicx-repair-preview.json",
        ])
        .expect("corpus repair should parse");
        match repair.command {
            Some(Commands::Corpus(CorpusArgs {
                command: CorpusCommand::Repair(args),
            })) => {
                assert_eq!(args.roots.root, vec![PathBuf::from("/tmp/aicx-store")]);
                assert!(args.dry_run);
                assert!(!args.apply);
                assert!(args.backup);
                assert_eq!(
                    args.manifest,
                    Some(PathBuf::from("/tmp/aicx-repair-preview.json"))
                );
            }
            _ => panic!("expected corpus repair command"),
        }
    }

    #[test]
    fn store_agent_filter_is_explicit_and_includes_junie() {
        let mut cmd = Cli::command();
        let store = cmd
            .find_subcommand_mut("store")
            .expect("store subcommand should exist");
        let rendered = store.render_long_help().to_string();

        assert!(rendered.contains("claude, codex, gemini, junie"));
        assert!(rendered.contains("codescribe"));
        assert!(rendered.contains("operator-md"));

        let cli = Cli::try_parse_from(["aicx", "store", "--agent", "junie"])
            .expect("store should accept junie agent filter");
        match cli.command {
            Some(Commands::Store { agent, .. }) => {
                assert_eq!(agent.as_deref(), Some("junie"));
            }
            _ => panic!("expected store command"),
        }

        let cli = Cli::try_parse_from(["aicx", "store", "--agent", "codescribe"])
            .expect("store should accept codescribe agent filter");
        match cli.command {
            Some(Commands::Store { agent, .. }) => {
                assert_eq!(agent.as_deref(), Some("codescribe"));
            }
            _ => panic!("expected store command"),
        }

        let cli = Cli::try_parse_from(["aicx", "store", "--agent", "operator-md"])
            .expect("store should accept operator-md agent filter");
        match cli.command {
            Some(Commands::Store { agent, .. }) => {
                assert_eq!(agent.as_deref(), Some("operator-md"));
            }
            _ => panic!("expected store command"),
        }

        let err = Cli::try_parse_from(["aicx", "store", "--agent", "oops"])
            .expect_err("store should reject unknown agent filters");
        assert!(err.to_string().contains("possible values"));
    }

    #[test]
    fn list_help_names_all_discovered_agent_sources() {
        let mut cmd = Cli::command();
        let list = cmd
            .find_subcommand_mut("list")
            .expect("list subcommand should exist");
        let rendered = list.render_long_help().to_string();

        assert!(rendered.contains("Claude Code, Codex, Gemini, and Junie log paths"));
    }

    #[test]
    fn legacy_dashboard_serve_subcommand_still_parses_hidden_compatibility_path() {
        let cli = Cli::try_parse_from(["aicx", "dashboard-serve", "--port", "9480"])
            .expect("legacy dashboard-serve alias should parse");

        match cli.command {
            Some(Commands::DashboardServeLegacy(args)) => {
                assert_eq!(args.port, 9480);
            }
            _ => panic!("expected hidden dashboard-serve compatibility command"),
        }
    }

    #[test]
    fn legacy_reports_extractor_subcommand_still_parses_hidden_compatibility_path() {
        let cli = Cli::try_parse_from(["aicx", "reports-extractor", "--repo", "demo"])
            .expect("legacy reports-extractor alias should parse");

        match cli.command {
            Some(Commands::ReportsExtractorLegacy(args)) => {
                assert_eq!(args.repo.as_deref(), Some("demo"));
            }
            _ => panic!("expected hidden reports-extractor compatibility command"),
        }
    }

    #[test]
    fn root_only_shortcuts_without_subcommand_are_rejected() {
        let err = Cli::try_parse_from(["aicx", "-H", "24"])
            .expect_err("root-only shortcut mode should not parse");
        let rendered = err.to_string();

        assert!(rendered.contains("unexpected argument '-H'"));
    }

    #[test]
    fn non_corpus_commands_reject_redaction_flags() {
        let err = Cli::try_parse_from(["aicx", "search", "dashboard", "--no-redact-secrets"])
            .expect_err("search should not accept corpus-building-only redaction flags");
        let rendered = err.to_string();

        assert!(rendered.contains("--no-redact-secrets"));
    }

    #[test]
    fn corpus_builders_accept_redaction_flags() {
        let cli = Cli::try_parse_from(["aicx", "claude", "--no-redact-secrets"])
            .expect("claude should accept corpus-building redaction flags");

        match cli.command {
            Some(Commands::Claude { redaction, .. }) => {
                assert!(!redaction.redact_secrets);
            }
            _ => panic!("expected claude command"),
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
                assert!(matches!(
                    format,
                    Some(ExtractInputFormat::GeminiAntigravity)
                ));
            }
            _ => panic!("expected extract command"),
        }
    }

    #[test]
    fn extract_accepts_junie_format() {
        let cli = Cli::try_parse_from([
            "aicx",
            "extract",
            "--format",
            "junie",
            "/tmp/session/events.jsonl",
            "-o",
            "/tmp/report.md",
        ])
        .expect("extract command with junie should parse");

        match cli.command {
            Some(Commands::Extract { format, .. }) => {
                assert!(matches!(format, Some(ExtractInputFormat::Junie)));
            }
            _ => panic!("expected extract command"),
        }
    }

    #[test]
    fn extract_accepts_session_mode() {
        let cli = Cli::try_parse_from([
            "aicx",
            "extract",
            "--session",
            "11111111-2222-3333-4444-555555555555",
            "--agent",
            "claude",
        ])
        .expect("extract --session should parse without positional input");

        match cli.command {
            Some(Commands::Extract {
                session,
                agent,
                input,
                output,
                ..
            }) => {
                assert_eq!(
                    session.as_deref(),
                    Some("11111111-2222-3333-4444-555555555555")
                );
                assert!(matches!(agent, Some(ExtractInputFormat::Claude)));
                assert!(input.is_none());
                assert!(output.is_none());
            }
            _ => panic!("expected extract command"),
        }
    }

    #[test]
    fn extract_session_and_input_are_mutually_exclusive() {
        let res = Cli::try_parse_from([
            "aicx",
            "extract",
            "--session",
            "abc",
            "--agent",
            "junie",
            "/tmp/session/events.jsonl",
        ]);
        assert!(
            res.is_err(),
            "--session must conflict with positional INPUT path"
        );
    }

    #[test]
    fn migrate_accepts_custom_roots() {
        let cli = Cli::try_parse_from([
            "aicx",
            "migrate",
            "--dry-run",
            "--no-intent-schema",
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
                no_intent_schema,
            }) => {
                assert!(dry_run);
                assert!(no_intent_schema);
                assert_eq!(legacy_root, Some(PathBuf::from("/tmp/legacy")));
                assert_eq!(store_root, Some(PathBuf::from("/tmp/aicx")));
            }
            _ => panic!("expected migrate command"),
        }
    }

    #[test]
    fn migrate_intent_schema_accepts_missing_project_and_defaults_to_dry_run() {
        let cli = Cli::try_parse_from(["aicx", "migrate-intent-schema"])
            .expect("migrate-intent-schema should parse without explicit project");

        match cli.command {
            Some(Commands::MigrateIntentSchema {
                project,
                store_root,
                dry_run,
            }) => {
                assert_eq!(project, None);
                assert_eq!(store_root, None);
                assert!(dry_run);
            }
            _ => panic!("expected migrate-intent-schema command"),
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
            ExtractFileOptions {
                include_assistant: true,
                max_message_chars: 0,
                redact_secrets: false,
                conversation: false,
            },
        )
        .unwrap();

        let output = fs::read_to_string(&report).unwrap();
        assert!(output.contains("| Filter | RepoDelta |"));
        assert!(output.contains("Gemini Antigravity recovery report"));
        assert!(!output.contains("| Filter | file:"));

        let _ = fs::remove_dir_all(&root);
    }
}
