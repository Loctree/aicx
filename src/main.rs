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
use std::io::{self, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

mod cli_config;

use aicx::corpus;
use aicx::dashboard::{self, DashboardConfig, DashboardScope};
use aicx::dashboard_server::{self, DashboardCorsPolicy, DashboardServerConfig};
use aicx::intents;
use aicx::mcp::{self, McpTransport};
use aicx::output::{self, OutputConfig, OutputFormat, OutputMode, ReportMetadata};
use aicx::rank;
use aicx::reports_extractor::{self, ReportsExtractorConfig};
use aicx::sessions;
use aicx::sources::{self, ExtractionConfig};
use aicx::state::StateManager;
use aicx::store;
use aicx::timeline;

#[derive(Debug, Clone)]
struct SessionResolution {
    canonical_id: String,
    note: Option<String>,
}

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
/// Operator-driven pipeline:
///   Canonical corpus: extract, deduplicate, and chunk agent logs into
///     steerable markdown at ~/.aicx/. This is ground truth.
///   Layer 2 (optional semantic index): local embedding-backed retrieval for native builds,
///     while the canonical corpus stays portable and useful without it.
/// Quick start:
///   aicx all -H 4                      # build canonical corpus
#[derive(Debug, Parser)]
#[command(name = "aicx")]
#[command(author = "(c)2026 VetCoders")]
#[command(version)]
#[command(verbatim_doc_comment)]
struct Cli {
    /// Verbose diagnostics: echo per-file extractor warnings to stderr.
    ///
    /// Default behavior aggregates warnings into a per-extractor SUMMARY
    /// (≤5 lines) at end of run; structured per-run detail is always written
    /// to `~/.aicx/state/diagnostics-<run-id>.log`. Pass `--verbose` to
    /// restore the pre-G-4 per-file echo for debugging individual sessions.
    #[arg(long, short = 'v', global = true)]
    verbose: bool,

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
    LoctContextPack,
}

impl IngestSource {
    fn as_agent(&self) -> &'static str {
        match self {
            Self::OperatorMd => "operator-md",
            Self::LoctContextPack => "loct-context-pack",
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
enum SessionsCommand {
    /// List discovered agent sessions, newest first.
    List {
        /// Restrict to sessions whose repo/cwd matches the current directory.
        #[arg(long)]
        cwd: bool,

        /// Filter by agent (claude | codex | gemini | junie).
        #[arg(long, value_parser = ["claude", "codex", "gemini", "junie"])]
        agent: Option<String>,

        /// Only sessions updated on/after this date (YYYY-MM-DD). Defaults to the
        /// last 30 days; pass --all to scan the full history.
        #[arg(long)]
        since: Option<String>,

        /// Scan the full session history (slower) instead of the default
        /// last-30-days window.
        #[arg(long)]
        all: bool,

        /// Max sessions to show (0 = all).
        #[arg(long, default_value_t = 20)]
        limit: usize,

        /// Output format: table | json.
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Show one session's metadata, located by id (or a unique prefix).
    Show {
        /// Session id (or a unique prefix).
        session_id: String,

        /// Output format: markdown | json.
        #[arg(long, default_value = "markdown")]
        format: String,
    },

    /// Unified truth report for one session: human intents (Lane 1), agent
    /// claims + evidence verification (Lanes 2-3), contract fractures (Lane 4)
    /// and clarify decisions (Lane 5) in a single rendering.
    Report {
        /// Session id (or a unique prefix).
        session_id: String,

        /// Agent: claude | codex | gemini | junie. Inferred from the session
        /// id when omitted.
        #[arg(long)]
        agent: Option<String>,

        /// Hours to look back when locating the session (default 720).
        #[arg(long, default_value_t = 720)]
        hours: u64,

        /// Repo root evidence is checked against (default: current directory).
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Max clarify questions (hard-capped at 5).
        #[arg(long, default_value_t = 5)]
        max: usize,

        /// Output format: markdown | json.
        #[arg(long, default_value = "markdown")]
        format: String,
    },
}

#[derive(Debug, Subcommand)]
enum ClaimsCommand {
    /// Extract Unverified claims (Lane 2) from a session's conversation.
    Extract {
        /// Session id (or unique prefix).
        #[arg(long)]
        session: String,

        /// Agent: claude | codex | gemini | junie. Inferred from the session id
        /// when omitted.
        #[arg(long)]
        agent: Option<String>,

        /// Hours to look back when locating the session (default 720).
        #[arg(long, default_value = "720")]
        hours: u64,

        /// Output format: json | summary.
        #[arg(long, default_value = "json")]
        format: String,
    },
}

#[derive(Debug, Subcommand)]
enum ResultsCommand {
    /// Collect repo evidence (artifact existence) for a session's claims and
    /// fold it into verification statuses (Lane 3).
    Collect {
        /// Session id (or unique prefix).
        #[arg(long)]
        session: String,

        /// Agent: claude | codex | gemini | junie. Inferred from the session id
        /// when omitted.
        #[arg(long)]
        agent: Option<String>,

        /// Hours to look back when locating the session (default 720).
        #[arg(long, default_value = "720")]
        hours: u64,

        /// Repo root evidence is checked against (default: current directory).
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Output format: json | summary.
        #[arg(long, default_value = "json")]
        format: String,
    },
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

/// Shared retrieval grammar (B-P1-11) used by `aicx search`,
/// `aicx steer`, `aicx intents`, and `aicx tail`. One struct, one
/// set of `help` bodies — so every retrieval command renders the same
/// vocabulary in `--help` output.
#[derive(Debug, Args, Clone)]
struct RetrievalFilters {
    /// Maximum number of results to return. Default is command-specific:
    /// search/steer 10, tail 20, intents unlimited (full roadmap).
    #[arg(long)]
    limit: Option<usize>,

    /// Sort order applied after filtering. Default: command-specific.
    #[arg(long, value_enum)]
    sort: Option<SortOrder>,

    /// Minimum score threshold (0-100; semantic match confidence).
    #[arg(long)]
    score: Option<u8>,

    /// Agent name filter: claude | codex | gemini | junie | codescribe.
    #[arg(long)]
    agent: Option<String>,

    /// Lower date bound: YYYY-MM-DD or relative (e.g., 2026-04-23..).
    #[arg(long)]
    since: Option<String>,

    /// Upper date bound: YYYY-MM-DD.
    #[arg(long)]
    until: Option<String>,

    /// Frame channel filter: user_msg | agent_reply | internal_thought | tool_call.
    #[arg(long, value_enum)]
    frame_kind: Option<FrameKindArg>,
}

const MAX_CLI_SEARCH_LIMIT: usize = 10_000;

/// Default `--limit` for bounded retrieval commands (`search`, `steer`) when
/// the operator passes none. `tail` defaults to 20 and `intents` to unlimited
/// — see [`RetrievalFilters::limit`].
const DEFAULT_RETRIEVAL_LIMIT: usize = 10;

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

    /// Optional explicit auth token (overrides env / file / generated). Server mode only.
    #[arg(long, requires = "serve", value_name = "TOKEN")]
    auth_token: Option<String>,

    /// Require Bearer auth on dashboard `/api/*` (default: true). Pass `--no-require-auth` to opt out.
    #[arg(long, requires = "serve", default_value_t = true, action = clap::ArgAction::Set)]
    require_auth: bool,

    /// Allow mutating dashboard API calls without Origin or Referer (tooling escape hatch).
    #[arg(long, requires = "serve")]
    allow_no_origin: bool,

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

    /// Overwrite existing HTML/bundle outputs. Without this flag, the command
    /// refuses to clobber a pre-existing file at either output path.
    #[arg(long, default_value_t = false)]
    force: bool,

    /// Derive `generated_at` from the latest record timestamp instead of
    /// `Utc::now()`. Also enabled via `AICX_REPORTS_DETERMINISTIC=1` env var.
    #[arg(long, default_value_t = false)]
    deterministic: bool,

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

#[derive(Debug, Clone, Subcommand)]
enum IndexAction {
    /// Show freshness and pending-corpus status for the semantic index.
    Status {
        /// Strict project filter, repeatable. Same shapes as `aicx index`:
        ///   `-p owner/repo`   strict `<owner>/<repo>` slug match
        ///   `-p owner/`       all repos under that owner (org wildcard)
        ///   `-p /repo`        same repo name across every owner
        ///   `-p name`         name matches an owner OR a repo (cross-org)
        ///
        /// Routed through the same canonical resolver as `aicx index`
        /// (`resolve_filters_to_slugs` / `project_filter_matches`), so
        /// `aicx index status -p X` and `aicx index -p X` always agree on
        /// which buckets exist for the same `-p`.
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

        /// Emit JSON status instead of plain text
        #[arg(short = 'j', long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum Commands {
    // ── Layer 1: Canonical corpus ─────────────────────────────────────
    /// Extract and store Agents' sessions into the canonical corpus (canonical corpus extraction).
    ///
    /// Reads claude-code session files, then
    /// writes steerable Markdown files to a central store.
    #[command(display_order = 2)]
    Claude {
        #[command(flatten)]
        redaction: RedactionArgs,

        /// Source cwd/project filter(s): narrows session discovery before repo segmentation
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

        /// Hours to look back (default: 48, 0 = all time)
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

    /// Extract and store Codex sessions into the canonical corpus.
    ///
    /// Reads codex session files, then
    /// writes steerable Markdown files to a central store.
    #[command(display_order = 3)]
    Codex {
        #[command(flatten)]
        redaction: RedactionArgs,

        /// Source cwd/project filter(s): narrows session discovery before repo segmentation
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

        /// Hours to look back (default: 48, 0 = all time)
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

    /// Extract and store from all agents (Claude + Codex + Gemini + Junie + CodeScribe) into the canonical corpus.
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
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

        /// Hours to look back (default: 48, 0 = all time)
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
    /// entries matching `--session` are filtered, and either a full timeline
    /// report or a denoised conversation Markdown transcript is written.
    /// Default output paths are `~/.aicx/extracts/<agent>/<session_id>.md`
    /// and `~/.aicx/extracts/<agent>/<session_id>_conversation.md`.
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

        /// Hours to look back when scanning sources in session mode (default: 1 year, 0 = all time).
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

    /// Batch-export conversation JSON files without writing to the canonical store.
    ///
    /// Thin wrapper around `aicx extract --conversation` semantics: scans source
    /// sessions, groups by session_id, and writes one JSON file per session.
    #[command(display_order = 6)]
    Conversations {
        #[command(flatten)]
        redaction: RedactionArgs,

        /// Source agent for batch conversation export (v1: claude only).
        #[arg(long, value_parser = ["claude"], default_value = "claude")]
        agent: String,

        /// Source cwd/project filter(s): narrows session discovery before export.
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

        /// Hours to look back when scanning source sessions (default: 1 year).
        #[arg(short = 'H', long, default_value = "8760")]
        hours: u64,

        /// Output directory. Files are written as
        /// `<out-dir>/<agent>/<sanitized-session-id>.json`. Session ids
        /// that contain characters other than `[A-Za-z0-9._-]` are
        /// sanitized; a SipHash suffix is appended to keep distinct ids
        /// from colliding after sanitization.
        #[arg(long)]
        out_dir: PathBuf,

        /// Maximum number of sessions to write, after deterministic session sorting.
        #[arg(long)]
        limit: Option<usize>,

        /// Preview discovery without writing; emits a JSON envelope on
        /// stdout (sessions_discovered, by_kind, by_agent, filters_applied)
        /// and a human-readable summary banner on stderr.
        ///
        /// Pipe-friendly: `aicx conversations --dry-run --out-dir /tmp | jq .`
        /// returns valid JSON; the operator banner still prints on stderr.
        #[arg(long)]
        dry_run: bool,
    },

    /// Build the canonical corpus in from local agents' session files.
    ///
    /// Store-first corpus builder: extracts, deduplicates, chunks, and writes
    /// steerable Markdown. By default, this command uses per-source watermarks
    /// to skip previously scanned history. Use --full-rescan for backfills
    /// and targeted re-extraction when you need to ignore the watermark.
    ///
    #[command(display_order = 4)]
    Store {
        #[command(flatten)]
        redaction: RedactionArgs,

        /// Source cwd/project filter(s): narrows session discovery before repo segmentation
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

        /// Agent filter: one of claude, codex, gemini, junie, codescribe, operator-md.
        /// Default: claude+codex+gemini+junie+codescribe (operator-md is opt-in
        /// via `--agent operator-md`).
        #[arg(short, long, value_parser = ["claude", "codex", "gemini", "junie", "codescribe", "operator-md"])]
        agent: Option<String>,

        /// Hours to look back (default: 48, 0 = all time)
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
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

        /// Hours to look back when --since is omitted (default: 720 = 30 days, 0 = all time)
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

        /// Source path for pack-style ingests such as --source loct-context-pack
        input: Option<PathBuf>,
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

    /// Discover and list agent sessions on disk (session surface).
    #[command(display_order = 6, alias = "session")]
    Sessions {
        #[command(subcommand)]
        command: SessionsCommand,
    },

    /// Lane 2: extract agent claims (audit targets) from a session.
    #[command(display_order = 6)]
    Claims {
        #[command(subcommand)]
        command: ClaimsCommand,
    },

    /// Lane 3: collect repo evidence for a session's claims and verify them.
    #[command(display_order = 6)]
    Results {
        #[command(subcommand)]
        command: ResultsCommand,
    },

    /// Lane 5: generate at most 5 A/B/C decision questions from verified gaps.
    #[command(display_order = 6)]
    Clarify {
        /// Session id (or unique prefix).
        #[arg(long)]
        session: String,

        /// Agent: claude | codex | gemini | junie. Inferred from the session id
        /// when omitted.
        #[arg(long)]
        agent: Option<String>,

        /// Hours to look back when locating the session (default 720).
        #[arg(long, default_value = "720")]
        hours: u64,

        /// Repo root evidence is checked against (default: current directory).
        #[arg(long)]
        repo: Option<PathBuf>,

        /// Max questions (hard-capped at 5).
        #[arg(long, default_value_t = 5, value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=5))]
        max: usize,

        /// Output format: markdown | json.
        #[arg(long, default_value = "markdown")]
        format: String,
    },

    /// Interactive daily-driver entrypoint for corpus, doctor, intents, and store.
    #[command(display_order = 9)]
    Wizard {
        /// Render one frame and exit; used by automated smoke tests.
        #[arg(long, hide = true)]
        smoke_test: bool,
    },

    /// List chunks in the canonical store inventory.
    ///
    /// Shows what extractors have already written to ~/.aicx/.
    #[command(display_order = 11)]
    Refs {
        /// Hours to look back (filter by canonical chunk date)
        #[arg(short = 'H', long, default_value = "48")]
        hours: u64,

        /// Strict project filter: `owner/repo`, `/repo` (cross-org repo
        /// name), `owner/` (org wildcard), or `name` (matches org OR
        /// repo). Substring matching is intentionally disabled — `-p vista`
        /// no longer leaks into `vista-portal`/`vista-datasets`.
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

        /// Project filter (applies to --info as well as --reset).
        /// Supports the standard shapes: `-p owner/repo`, `-p owner/`,
        /// `-p /repo`, or a bare `-p name` (cross-org).
        #[arg(short, long)]
        project: Option<String>,

        /// Show state info/statistics
        #[arg(long)]
        info: bool,
    },

    /// Generate a searchable HTML dashboard from the canonical store, or serve it locally.
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

    /// Extract structured intents from the canonical corpus.
    Intents {
        /// Repo or store-bucket filters. Omit to scan all projects.
        /// Repeated `-p` flags or comma list (`-p a,b`) form a union.
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

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

    /// Print recent intents/chunks (snapshot mode); add --follow to stream new arrivals.
    Tail {
        /// Repo or store-bucket filters. Omit to scan all projects.
        /// Repeated `-p` flags or comma list (`-p a,b`) form a union.
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

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

    /// Run aicx as an MCP server.
    Serve {
        /// Transport: stdio (default) or http. Legacy alias: sse.
        #[arg(long, value_enum, default_value_t = McpTransport::Stdio)]
        transport: McpTransport,

        /// Port for streamable HTTP transport (default: 8044)
        #[arg(long, default_value = "8044")]
        port: u16,

        /// Optional explicit auth token (overrides env / file / generated). HTTP transport only.
        #[arg(long, value_name = "TOKEN")]
        auth_token: Option<String>,

        /// Require Bearer auth on HTTP transport (default: true). Pass `--no-require-auth` to opt out.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        require_auth: bool,
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

    /// Search the canonical corpus. Semantic by default; `--no-semantic`
    /// runs the explicit filesystem-fuzzy fallback.
    #[command(display_order = 12)]
    Search {
        /// Search query string
        query: String,

        /// Project filter. Omit to search every project.
        ///
        /// Accepted forms (case-insensitive, repeatable):
        ///   `-p owner/repo`   strict `<owner>/<repo>` slug match
        ///   `-p owner/`       all repos under that owner (org wildcard)
        ///   `-p /repo`        same repo name across every owner
        ///   `-p name`         name matches an owner OR a repo (cross-org)
        ///
        /// Multiple `-p` flags or a comma list (`-p a,b`) form a union.
        /// Substring matching is intentionally not supported — `-p vista`
        /// no longer matches `vista-portal` / `vista-docs`. Use `-p vetcoders/Vista`
        /// or `-p /Vista` if you want exactness.
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

        /// Hours to look back (0 = all time)
        #[arg(short = 'H', long, default_value = "0")]
        hours: u64,

        /// Filter by date: single day (2026-03-28), range (2026-03-20..2026-03-28),
        /// or open-ended (2026-03-20.. or ..2026-03-28)
        #[arg(short, long)]
        date: Option<String>,

        #[command(flatten)]
        filters: RetrievalFilters,

        /// Filter by canonical corpus kind: conversations, plans, reports, other.
        #[arg(long, value_parser = ["conversations", "conversation", "plans", "plan", "reports", "report", "other"])]
        kind: Option<String>,

        /// Bypass semantic vector search and run filesystem-fuzzy search.
        #[arg(long)]
        no_semantic: bool,

        /// Emit compact JSON instead of plain text
        #[arg(short = 'j', long)]
        json: bool,
    },

    /// Build the semantic index. Use `--dry-run` to preview without writing.
    ///
    /// Default behaviour is INCREMENTAL: only sidecars whose mtime is newer
    /// than the existing index `header.generated_at` are embedded, and the
    /// new rows are appended to the committed index file. Pass
    /// `--full-rescan` to re-embed every chunk from scratch — useful when
    /// the embedder model changes, the index file is corrupt, or an
    /// operator wants a deterministic from-zero rebuild.
    #[command(display_order = 13)]
    Index {
        #[command(subcommand)]
        action: Option<IndexAction>,

        /// Project filter. Omit to index every project.
        ///
        /// Accepted forms (case-insensitive, repeatable):
        ///   `-p owner/repo`   strict `<owner>/<repo>` slug match
        ///   `-p owner/`       all repos under that owner (org wildcard)
        ///   `-p /repo`        same repo name across every owner
        ///   `-p name`         name matches an owner OR a repo (cross-org)
        ///
        /// Multiple `-p` flags or a comma list (`-p a,b`) form a union.
        /// Substring matching is intentionally not supported — `-p vista`
        /// no longer matches `vista-portal` / `vista-docs`.
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

        /// Stop after sampling this many chunks (0 = scan all)
        #[arg(long, default_value = "0")]
        sample: usize,

        /// Emit JSON stats instead of plain text
        #[arg(short = 'j', long)]
        json: bool,

        /// Preview only. Omit this flag to materialize the persistent
        /// semantic index used by `aicx search`.
        #[arg(
            long,
            default_value_t = false,
            default_missing_value = "true",
            num_args = 0..=1,
            value_parser = clap::builder::BoolishValueParser::new()
        )]
        dry_run: bool,

        /// Force a full re-embed of every chunk. Default is incremental:
        /// walk only sidecars newer than the existing index's
        /// `header.generated_at` and append. Use this flag after embedder
        /// model changes or when the committed index is suspect.
        #[arg(long)]
        full_rescan: bool,
    },

    /// Manage `$HOME/.aicx/config.toml` for embedders and endpoints.
    #[command(display_order = 4)]
    Config {
        #[command(subcommand)]
        action: cli_config::ConfigAction,
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

    /// Retrieve chunks by steering metadata (requires --features lance).
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

        /// Repo or store-bucket filters. Omit to search all projects.
        /// Repeated `-p` flags or comma list (`-p a,b`) form a union.
        #[arg(short, long, value_delimiter = ',')]
        project: Vec<String>,

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
        /// Strict project filter: `owner/repo`, `/repo` (cross-org repo
        /// name), `owner/` (org wildcard), or `name` (matches org OR
        /// repo). Omit to scan the whole store. Substring matching is
        /// intentionally disabled.
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
    /// sidecar coverage, and corpus bucket names. With
    /// `--rebuild-steer-index`, corrupted steer indexes are deleted and
    /// rebuilt from the canonical store (which is treated as ground truth
    /// and never modified). Other remediations live behind dedicated flags
    /// (`--prune-empty-bodies`, `--fix-buckets`, `aicx store --full-rescan`).
    ///
    /// Exit codes: 0 on green/warning or after successful rebuild; 1 if
    /// critical issues are detected without remediation.
    #[command(display_order = 12)]
    Doctor {
        /// Delete and rebuild the steer index from the canonical store
        /// when corrupted or schema-incompatible. Narrower contract than
        /// the legacy `--fix` (which was a no-op for sidecars/index
        /// consistency/empty bodies — those have dedicated flags).
        ///
        /// Legacy alias: `--fix` is accepted with a deprecation warning
        /// and will be removed in v1.0.
        #[arg(long = "rebuild-steer-index", alias = "fix")]
        rebuild_steer_index: bool,

        /// Move suspicious top-level corpus buckets to $HOME/.aicx/quarantine/.
        /// Buckets that are merely CamelCase (legitimate GitHub orgs like
        /// `LibraxisAI`, `VetCoders`, `Loctree`, `Szowesgad`) are
        /// canonicalized in place to lowercase instead of quarantined,
        /// merging into existing lowercase buckets if present.
        #[arg(long)]
        fix_buckets: bool,

        /// With --fix-buckets, preview the planned canonicalize/quarantine
        /// actions without modifying the filesystem. Output entries are
        /// prefixed with `[dry-run]`. Use this before running `--fix-buckets`
        /// against a large store to verify the classification before commit.
        #[arg(long)]
        dry_run: bool,

        /// Emit a reviewable bash script for missing sidecar backfill
        #[arg(long)]
        rebuild_sidecars: bool,

        /// Emit a reviewable bash script for moving empty-body chunks to quarantine
        #[arg(long)]
        prune_empty_bodies: bool,

        /// With --prune-empty-bodies, move empty-body chunks into recoverable quarantine
        #[arg(long, requires = "prune_empty_bodies")]
        apply: bool,

        /// Restore files from a quarantine manifest slug.
        #[arg(long, value_name = "SLUG")]
        restore_quarantine: Option<String>,

        /// Assume yes on doctor cleanup prompts.
        #[arg(short = 'y', long)]
        yes: bool,

        /// Skip dry-run preview and prompts; intended for CI cleanup runs.
        #[arg(long)]
        force: bool,

        /// Report duplicate content_sha256 groups across store and context-corpus
        #[arg(long)]
        check_dedup: bool,

        /// Print recommendations for green checks too
        #[arg(short, long)]
        verbose: bool,

        /// Run actual real HTTP POST / embedder tests instead of skipping them.
        /// Doctor stays fast and cheap by default; this flag exercises the AI provider.
        #[arg(long)]
        smoke: bool,

        /// Output format: text (default), json
        #[arg(long, default_value = "text")]
        format: String,

        /// Report AICX Oracle readiness: ready | degraded | unsafe_for_loctree_scope.
        ///
        /// Severity vocabulary mapping (B-P1-06):
        ///   ready                    ← all checks Green
        ///   degraded                 ← any Warning, or non-Green dashboard route
        ///   unsafe_for_loctree_scope ← any Critical in canonical / sidecars / content
        ///
        /// Doctor's per-check `severity` field uses TitleCase variants
        /// (Green / Warning / Critical / NotConfigured / Skipped /
        /// Unknown). The JSON serde renders them lowercase
        /// (`"green"`/`"warning"`/...) so machine consumers can match
        /// on stable tokens.
        #[arg(long, verbatim_doc_comment)]
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

/// Detect `aicx <cmd>` invocations that clap would otherwise reject with
/// a generic "the following required arguments were not provided" or
/// "missing required subcommand" error. Returns a `(cmd_name,
/// StructuredFailure)` pair when the canonical bad shape is present,
/// `None` otherwise.
///
/// Covers (Wave C §3.2):
/// - `aicx ingest` with no `--source <SOURCE>`
/// - `aicx conversations` with no `--out-dir <DIR>`
/// - `aicx sources` with no subcommand
///
/// Heuristic only: we exit early when we see `--help`/`-h`/`--version`
/// so clap's own help rendering wins.
fn detect_missing_required_boundary<I>(
    args: I,
) -> Option<(&'static str, aicx::cli::failure::StructuredFailure)>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    if args
        .iter()
        .any(|a| a == "--help" || a == "-h" || a == "--version" || a == "-V")
    {
        return None;
    }

    // Locate the leftmost top-level subcommand. We accept arbitrary
    // top-level flags before it (e.g. `aicx --verbose ingest`).
    let cmd_idx = args
        .iter()
        .position(|a| matches!(a.as_str(), "ingest" | "conversations" | "sources"))?;

    let cmd = args[cmd_idx].as_str();
    let tail = &args[cmd_idx + 1..];

    match cmd {
        "ingest" => {
            // `--source` is required (no default).
            if tail
                .iter()
                .any(|a| a == "--source" || a.starts_with("--source="))
            {
                return None;
            }
            Some((
                "aicx ingest",
                aicx::cli::failure::StructuredFailure::new(
                    "missing_required_arg",
                    "argument --source <SOURCE> is required",
                    "rerun with --source <name>, e.g. aicx ingest --source loct-context-pack <PACK_DIR>",
                )
                .with_fallback("aicx ingest --source loct-context-pack <PACK_DIR>"),
            ))
        }
        "conversations" => {
            // `--out-dir` is required (no default).
            if tail
                .iter()
                .any(|a| a == "--out-dir" || a.starts_with("--out-dir="))
            {
                return None;
            }
            Some((
                "aicx conversations",
                aicx::cli::failure::StructuredFailure::new(
                    "missing_required_arg",
                    "argument --out-dir <DIR> is required",
                    "rerun with --out-dir <path>, e.g. aicx conversations --out-dir ~/.aicx/conversations",
                )
                .with_fallback("aicx conversations --out-dir ~/.aicx/conversations"),
            ))
        }
        "sources" => {
            // First positional after `sources` must be a known subcommand.
            // `help` is clap's own — skip so it renders normally.
            if let Some(next) = tail.iter().find(|a| !a.starts_with('-')) {
                if matches!(next.as_str(), "protect" | "help") {
                    return None;
                }
                None
            } else {
                // No further positional → subcommand missing.
                Some((
                    "aicx sources",
                    aicx::cli::failure::StructuredFailure::new(
                        "missing_subcommand",
                        "aicx sources requires a subcommand (protect)",
                        "pick the action you want, e.g. aicx sources protect --root <PATH>",
                    )
                    .with_fallback("aicx sources protect --root <PATH>"),
                ))
            }
        }
        _ => None,
    }
}

/// Detect the `aicx config --show` mistake before clap rejects it with a
/// generic "unexpected argument" error. Returns a [`StructuredFailure`]
/// when the canonical bad shape is present, `None` otherwise.
///
/// Bad shape: a positional `config` followed (eventually) by `--show`,
/// with no intervening `show`/`init` positional that would mean the user
/// already picked a subcommand. We accept arbitrary top-level flags
/// before `config` (e.g. `aicx --verbose config --show`) and arbitrary
/// flags after `--show` (e.g. `aicx config --show --json`).
fn detect_config_show_flag_mistake<I>(args: I) -> Option<aicx::cli::failure::StructuredFailure>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();

    // Step 1: locate the first positional that equals `config`.
    let config_idx = args.iter().position(|a| a == "config")?;

    // Step 2: walk forward; if we hit `show`/`init` first, the user is
    // already on a valid subcommand path → no mistake.
    for arg in &args[config_idx + 1..] {
        if arg == "show" || arg == "init" {
            return None;
        }
        if arg == "--show" {
            return Some(
                aicx::cli::failure::StructuredFailure::new(
                    "flag_not_recognized",
                    "'--show' is not a valid flag for 'aicx config'",
                    "use the subcommand form: aicx config show",
                )
                .with_fallback("aicx config show"),
            );
        }
    }
    None
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("ai_contexters=info".parse().unwrap()),
        )
        .init();

    // Pre-parse intercept (B-P1-12): `aicx config --show` is a common
    // discoverability mistake — `--show` is a subcommand-positional, not
    // a flag. Catch it before clap and emit the structured hint pointing
    // at `aicx config show`. We only fire when the args follow the
    // canonical bad shape so that legitimate parses are never affected.
    if let Some(failure) = detect_config_show_flag_mistake(std::env::args().skip(1)) {
        let json = aicx::cli::failure::want_json_envelope(false);
        aicx::cli::failure::emit_and_error("aicx config", json, failure);
        std::process::exit(2);
    }

    // Pre-parse intercept (B-P1-08): clap-default "missing required
    // argument" rendering doesn't match the structured failure-as-state
    // identity (Wave B §1.2). For the boundary cases where the surface
    // is most likely to be hit by new operators — `aicx ingest`,
    // `aicx conversations`, `aicx sources` — emit the structured
    // envelope and exit 2 instead of letting clap own the output.
    if let Some((cmd_name, failure)) = detect_missing_required_boundary(std::env::args().skip(1)) {
        let json = aicx::cli::failure::want_json_envelope(false);
        aicx::cli::failure::emit_and_error(cmd_name, json, failure);
        std::process::exit(2);
    }

    let cli = Cli::parse();

    let diagnostics_state_dir = aicx::store::store_base_dir().ok().map(|d| d.join("state"));
    let _ = aicx::diagnostics::init(cli.verbose, diagnostics_state_dir);

    let result = run_command(cli.command);
    aicx::diagnostics::emit_summary();
    result
}

fn run_command(command: Option<Commands>) -> Result<()> {
    match command {
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
            warn_pending_mutation("claude");
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
            warn_pending_mutation("codex");
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
            warn_pending_mutation("all");
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

            let json = aicx::cli::failure::want_json_envelope(false);

            // Session mode: --session [+ --agent] -> scan sources, filter by session_id.
            if let Some(session_id) = session {
                let agent = match agent.or(format) {
                    Some(a) => a,
                    None => {
                        aicx::cli::failure::emit_and_error(
                            "aicx extract",
                            json,
                            aicx::cli::failure::StructuredFailure::new(
                                "missing_required_arg",
                                "--session requires --agent {claude|codex|gemini|junie}",
                                "rerun with --agent <name>, e.g. aicx extract --session <id> --agent claude",
                            )
                            .with_fallback("aicx extract --session <ID> --agent claude"),
                        );
                        std::process::exit(2);
                    }
                };
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
                        conversation,
                    },
                )?;
            } else {
                // File mode (legacy): --format <agent> + positional input + -o.
                let format = match format {
                    Some(f) => f,
                    None => {
                        aicx::cli::failure::emit_and_error(
                            "aicx extract",
                            json,
                            aicx::cli::failure::StructuredFailure::new(
                                "mode_mismatch",
                                "file-mode extract requires --format {claude|codex|gemini|gemini-antigravity|junie}",
                                "pass --format <agent> with positional INPUT and -o <FILE>, or switch to session mode with --session <id> --agent <name>",
                            )
                            .with_fallback(
                                "aicx extract --format claude path/to/session.jsonl -o /tmp/out.md",
                            ),
                        );
                        std::process::exit(2);
                    }
                };
                let input = match input {
                    Some(i) => i,
                    None => {
                        aicx::cli::failure::emit_and_error(
                            "aicx extract",
                            json,
                            aicx::cli::failure::StructuredFailure::new(
                                "input_path_required",
                                "file-mode extract requires a positional INPUT path",
                                "append the agent log path, e.g. aicx extract --format claude ~/.claude/projects/<repo>/<session>.jsonl -o /tmp/out.md",
                            ),
                        );
                        std::process::exit(2);
                    }
                };
                let output = match output {
                    Some(o) => o,
                    None => {
                        aicx::cli::failure::emit_and_error(
                            "aicx extract",
                            json,
                            aicx::cli::failure::StructuredFailure::new(
                                "output_path_required",
                                "file-mode extract requires -o/--output <FILE>",
                                "add -o /path/to/out.md to write the extracted markdown",
                            ),
                        );
                        std::process::exit(2);
                    }
                };
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
        Some(Commands::Conversations {
            redaction,
            agent,
            project,
            hours,
            out_dir,
            limit,
            dry_run,
        }) => {
            run_conversations_batch(ConversationsBatchOptions {
                agent,
                project_filter: project,
                hours,
                out_dir,
                limit,
                dry_run,
                redact_secrets: redaction.redact_secrets,
            })?;
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
            warn_pending_mutation("store");
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
            input,
        }) => {
            if matches!(source, IngestSource::LoctContextPack) {
                let input = match input.as_deref() {
                    Some(p) => p,
                    None => {
                        let json = aicx::cli::failure::want_json_envelope(false);
                        aicx::cli::failure::emit_and_error(
                            "aicx ingest",
                            json,
                            aicx::cli::failure::StructuredFailure::new(
                                "input_path_required",
                                "aicx ingest --source loct-context-pack requires <PACK_DIR>",
                                "append the pack directory path, e.g. aicx ingest --source loct-context-pack ~/.vibecrafted/inbox/loct-context-pack-2026-05-25",
                            )
                            .with_fallback("aicx ingest --source loct-context-pack <PACK_DIR>"),
                        );
                        std::process::exit(2);
                    }
                };
                let summary = store::ingest_loct_context_pack(input)?;
                match emit {
                    StdoutEmit::Paths => println!("{}", summary.target_dir.display()),
                    StdoutEmit::Json => println!("{}", serde_json::to_string_pretty(&summary)?),
                    StdoutEmit::None => {}
                }
                eprintln!(
                    "aicx ingest: {} chunks new, {} deduped → {}",
                    summary.raw_written,
                    summary.deduped_chunks,
                    summary.target_dir.display()
                );
                return Ok(());
            }
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
        Some(Commands::Sessions { command }) => run_sessions_command(command)?,
        Some(Commands::Claims { command }) => run_claims_command(command)?,
        Some(Commands::Results { command }) => run_results_command(command)?,
        Some(Commands::Clarify {
            session,
            agent,
            hours,
            repo,
            max,
            format,
        }) => run_clarify(&session, agent, hours, repo, max, &format)?,
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
                auth_token: None,
                require_auth: true,
                allow_no_origin: false,
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
        Some(Commands::Serve {
            transport,
            port,
            auth_token,
            require_auth,
        }) => {
            let auth_config = aicx::auth::load_auth_config(auth_token.as_deref(), require_auth)?;
            if matches!(transport, McpTransport::Http) && !require_auth {
                eprintln!(
                    "! Warning: MCP HTTP transport bound without auth — knowing the port is enough to invoke MCP tools."
                );
            }
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async { mcp::run_transport(transport, port, auth_config).await })?;
        }
        Some(Commands::Search {
            query,
            project,
            hours,
            date,
            filters,
            kind,
            no_semantic,
            json,
        }) => {
            run_search(SearchRunArgs {
                query: &query,
                projects: &project,
                hours,
                date: date.as_deref(),
                json,
                filters,
                kind: kind.as_deref(),
                no_semantic,
            })?;
        }
        Some(Commands::Index {
            action,
            project,
            sample,
            json,
            dry_run,
            full_rescan,
        }) => match action {
            Some(IndexAction::Status { project, json }) => {
                run_index_status(&project, json)?;
            }
            None => {
                if !dry_run {
                    warn_pending_mutation("index");
                }
                run_index(&project, sample, json, dry_run, full_rescan)?
            }
        },
        Some(Commands::Config { action }) => {
            cli_config::run_config(action)?;
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
                &project,
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
            if !dry_run {
                warn_pending_mutation("migrate");
            }
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
            if !dry_run {
                warn_pending_mutation("migrate-intent-schema");
            }
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
            rebuild_steer_index,
            fix_buckets,
            dry_run,
            rebuild_sidecars,
            prune_empty_bodies,
            apply,
            restore_quarantine,
            yes,
            force,
            check_dedup,
            verbose,
            smoke,
            format,
            oracle,
        }) => {
            if let Some(slug) = restore_quarantine {
                let report = aicx::doctor::restore_quarantine(&slug)?;
                match format.as_str() {
                    "json" => println!("{}", serde_json::to_string_pretty(&report)?),
                    _ => print!("{}", aicx::doctor::format_restore_text(&report)),
                }
                std::process::exit(if report.failures.is_empty() { 0 } else { 1 });
            }

            let fix = rebuild_steer_index; // Assuming `fix` is an alias for `--rebuild-steer-index`.
            let legacy_or_readonly = fix
                || fix_buckets
                || dry_run
                || rebuild_sidecars
                || prune_empty_bodies
                || apply
                || check_dedup
                || oracle
                || format == "json";
            if force || yes {
                let rt = tokio::runtime::Runtime::new()
                    .context("Failed to start tokio runtime for doctor cleanup")?;
                let base = aicx::store::store_base_dir()
                    .context("Failed to resolve aicx store base directory")?;
                let cleanup = rt.block_on(aicx::doctor::run_automated_cleanup_at(
                    &base,
                    force,
                    verbose,
                    smoke,
                    format != "json",
                ))?;
                match format.as_str() {
                    "json" => println!("{}", serde_json::to_string_pretty(&cleanup)?),
                    _ => print!("{}", aicx::doctor::format_cleanup_run_text(&cleanup)),
                }
                let failed = cleanup.applied.iter().any(|phase| phase.status != "ok");
                std::process::exit(
                    if failed || cleanup.final_report.overall == aicx::doctor::Severity::Critical {
                        1
                    } else {
                        0
                    },
                );
            }

            if !legacy_or_readonly && io::stdin().is_terminal() {
                let rt = tokio::runtime::Runtime::new()
                    .context("Failed to start tokio runtime for doctor interactive cleanup")?;
                let base = aicx::store::store_base_dir()
                    .context("Failed to resolve aicx store base directory")?;
                let cleanup = rt.block_on(aicx::doctor::run_interactive_cleanup_at(
                    &base, verbose, smoke,
                ))?;
                print!("{}", aicx::doctor::format_cleanup_run_text(&cleanup));
                let failed = cleanup.applied.iter().any(|phase| phase.status != "ok");
                std::process::exit(
                    if failed || cleanup.final_report.overall == aicx::doctor::Severity::Critical {
                        1
                    } else {
                        0
                    },
                );
            }

            // Surface the legacy `--fix` form as deprecated so callers can
            // migrate. We cannot tell from the parsed bool whether the
            // operator typed `--fix` or `--rebuild-steer-index`; inspect
            // the raw argv instead. The flag accepts both via Clap alias.
            if rebuild_steer_index && std::env::args().any(|arg| arg == "--fix") {
                eprintln!(
                    "aicx doctor: warning: '--fix' is deprecated; use '--rebuild-steer-index'. The old flag will be removed in v1.0."
                );
            }
            let opts = aicx::doctor::DoctorOptions {
                rebuild_steer_index,
                fix_buckets,
                dry_run,
                rebuild_sidecars,
                prune_empty_bodies,
                apply_prune_empty_bodies: apply,
                check_dedup,
                verbose,
                smoke,
            };
            let rt = tokio::runtime::Runtime::new()
                .context("Failed to start tokio runtime for doctor")?;
            let report = match rt.block_on(aicx::doctor::run(&opts)) {
                Ok(report) => report,
                Err(err) => {
                    // CLI-boundary failure-as-state for doctor. Catch the
                    // historical `--prune-empty-bodies` crash class (chunk
                    // path outside aicx root) and any other run failure
                    // here so the surface stays uniform with the rest of
                    // the family (per Wave B §1.2 / Cut D2 contract).
                    let json = aicx::cli::failure::want_json_envelope(format == "json");
                    let message = format!("{err:#}");
                    let kind = if message.contains("outside aicx canonical root")
                        || message.contains("outside store root")
                    {
                        "path_outside_aicx_root"
                    } else {
                        "doctor_run_failed"
                    };
                    let failure = aicx::cli::failure::StructuredFailure::new(
                        kind,
                        message,
                        "rerun with --verbose to see per-check details; \
                         if the path is genuinely outside ~/.aicx report it \
                         to the operator (possible store corruption or misconfigured roots)",
                    );
                    let wrapped = aicx::cli::failure::emit_and_error("aicx doctor", json, failure);
                    return Err(wrapped);
                }
            };

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
                aicx::doctor::Severity::Critical => 1,
                _ => 0,
            };
            std::process::exit(exit_code);
        }
        Some(Commands::Health) => {
            let opts = aicx::doctor::DoctorOptions {
                rebuild_steer_index: false,
                fix_buckets: false,
                dry_run: false,
                rebuild_sidecars: false,
                prune_empty_bodies: false,
                apply_prune_empty_bodies: false,
                check_dedup: false,
                verbose: true,
                smoke: false,
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

fn extract_input_format_from_str(s: &str) -> Option<ExtractInputFormat> {
    match s.to_lowercase().as_str() {
        "claude" => Some(ExtractInputFormat::Claude),
        "codex" => Some(ExtractInputFormat::Codex),
        "gemini" => Some(ExtractInputFormat::Gemini),
        "junie" => Some(ExtractInputFormat::Junie),
        _ => None,
    }
}

fn run_claims_command(command: ClaimsCommand) -> Result<()> {
    match command {
        ClaimsCommand::Extract {
            session,
            agent,
            hours,
            format,
        } => run_claims_extract(&session, agent, hours, &format),
    }
}

/// Everything the lane CLIs (claims / results / clarify) share: the session's
/// claims plus the temporal export-envelope context (P0 contract).
struct LaneSessionContext {
    canonical_id: String,
    agent: String,
    project: String,
    repo: Option<String>,
    source_files: Vec<String>,
    coverage: Option<intents::TimeCoverage>,
    warnings: Vec<String>,
    extracted_at: String,
    claims: Vec<intents::ClaimRecord>,
    user_intents: Vec<intents::UserIntentLine>,
}

impl LaneSessionContext {
    /// Wrap a lane payload in the machine-export envelope (schema_version,
    /// absolute generated_at, time coverage, timezone assumptions, warnings).
    /// `role_filter` declares which roles fed the payload: the claim-only
    /// lanes pass `agent_only`, the unified report passes `all`.
    fn envelope<T: serde::Serialize>(
        &self,
        mode: &str,
        role_filter: &str,
        payload: T,
    ) -> intents::LaneExport<T> {
        intents::LaneExport {
            schema_version: intents::LANE_SCHEMA_VERSION.to_string(),
            generated_at: self.extracted_at.clone(),
            project: self.project.clone(),
            repo: self.repo.clone(),
            session_id: Some(self.canonical_id.clone()),
            source_time_coverage: self.coverage.clone(),
            source_files: self.source_files.clone(),
            extraction_mode: mode.to_string(),
            role_filter: role_filter.to_string(),
            timezone_assumptions: intents::UTC_TIMEZONE_ASSUMPTION.to_string(),
            warnings: self.warnings.clone(),
            payload,
        }
    }
}

/// Build the lane-export [`intents::TimeCoverage`] from source timestamps:
/// earliest..latest, normalized to UTC and rendered RFC3339 with a literal
/// `Z` suffix. The envelope declares UTC (`timezone_assumptions`), so the
/// coverage bounds must never carry a local offset. Generic over the input
/// zone so the normalization itself is exercisable in tests.
fn lane_time_coverage<Tz: TimeZone>(
    timestamps: impl IntoIterator<Item = DateTime<Tz>>,
) -> Option<intents::TimeCoverage> {
    let mut earliest: Option<DateTime<Utc>> = None;
    let mut latest: Option<DateTime<Utc>> = None;
    for ts in timestamps {
        let ts = ts.with_timezone(&Utc);
        earliest = Some(earliest.map_or(ts, |cur| cur.min(ts)));
        latest = Some(latest.map_or(ts, |cur| cur.max(ts)));
    }
    let render = |t: DateTime<Utc>| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    earliest.zip(latest).map(|(a, b)| intents::TimeCoverage {
        earliest: render(a),
        latest: render(b),
    })
}

/// Locate a session, extract its conversation, and run Lane 2 claim
/// extraction with full temporal metadata. Shared by claims/results/clarify.
fn load_session_claims(
    session: &str,
    agent: Option<String>,
    hours: u64,
) -> Result<LaneSessionContext> {
    let home = dirs::home_dir().context("No home dir")?;
    let session_info = sessions::find_session_by_id(&home, session);
    let agent_str = match agent {
        Some(a) => a,
        None => session_info
            .as_ref()
            .map(|s| s.agent.clone())
            .context("could not infer agent from session id; pass --agent")?,
    };
    let fmt = extract_input_format_from_str(&agent_str)
        .with_context(|| format!("unknown agent '{agent_str}' (claude|codex|gemini|junie)"))?;

    let config = ExtractionConfig {
        project_filter: Vec::new(),
        cutoff: lookback_cutoff(hours),
        include_assistant: true,
        watermark: None,
    };
    let mut entries = match fmt {
        ExtractInputFormat::Claude => sources::extract_claude(&config)?,
        ExtractInputFormat::Codex => sources::extract_codex(&config)?,
        ExtractInputFormat::Gemini | ExtractInputFormat::GeminiAntigravity => {
            sources::extract_gemini(&config)?
        }
        ExtractInputFormat::Junie => sources::extract_junie(&config)?,
    };
    let label = extract_input_format_label(fmt);
    let resolution = resolve_session_reference(session, fmt, label, &entries)?;
    entries.retain(|e| e.session_id == resolution.canonical_id);
    if entries.is_empty() {
        anyhow::bail!(
            "no entries for session '{session}' (agent {agent_str}); try a larger --hours"
        );
    }

    // Project = last path segment of the recorded cwd, else agent/id.
    let repo = entries
        .iter()
        .find_map(|e| e.cwd.as_deref())
        .map(String::from);
    let project = repo
        .as_deref()
        .and_then(|c| c.trim_end_matches('/').rsplit('/').find(|s| !s.is_empty()))
        .map(String::from)
        .unwrap_or_else(|| format!("{agent_str}/{}", resolution.canonical_id));

    let extracted_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let coverage = lane_time_coverage(entries.iter().map(|e| e.timestamp));
    let source_files = session_info
        .as_ref()
        .map(|s| vec![s.source_path.display().to_string()])
        .unwrap_or_default();

    // Role filtering happens HERE, at the source build — not only inside the
    // lane stages (which re-guard as defense in depth, using the SAME shared
    // predicates). enumerate() runs BEFORE the filter so `source_ref` indices
    // keep pointing at positions in the full entry stream. Lane 1 takes the
    // strict user allowlist, Lane 2 the agent predicate — system/tool rows
    // enter neither lane.
    let to_source = |i: usize, e: &timeline::TimelineEntry| intents::ClaimSource {
        role: e.role.clone(),
        text: e.message.clone(),
        project: project.clone(),
        session_id: resolution.canonical_id.clone(),
        agent: Some(e.agent.clone()),
        source_ref: format!("{}#{i}", e.timestamp.to_rfc3339()),
        timestamp: Some(e.timestamp.to_rfc3339()),
        timestamp_partial: e.timestamp_source.is_some(),
    };
    let claim_sources: Vec<intents::ClaimSource> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| intents::is_agent_role(&e.role))
        .map(|(i, e)| to_source(i, e))
        .collect();
    // Lane 1 reuses the SAME harness-noise gate as `--conversation` denoising:
    // a "user"-role row that is really an injected skill body, `! command`
    // stdout, or a hook reminder must never feed the human-intent lane.
    let user_sources: Vec<intents::ClaimSource> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            intents::is_user_role(&e.role)
                && !sources::is_harness_injected_noise(&e.role, &e.message)
        })
        .map(|(i, e)| to_source(i, e))
        .collect();

    let claims = intents::extract_claims(&claim_sources, &extracted_at);
    let user_intents = intents::extract_user_intent_lines(&user_sources, &extracted_at);

    let mut warnings = Vec::new();
    let partial = claims.iter().filter(|c| c.timestamp_partial).count();
    if partial > 0 {
        warnings.push(format!(
            "{partial} claim(s) carry a partial/inferred source timestamp"
        ));
    }
    if source_files.is_empty() {
        warnings.push("source session file not resolved; provenance is session-id only".into());
    }

    Ok(LaneSessionContext {
        canonical_id: resolution.canonical_id,
        agent: agent_str,
        project,
        repo,
        source_files,
        coverage,
        warnings,
        extracted_at,
        claims,
        user_intents,
    })
}

fn run_claims_extract(
    session: &str,
    agent: Option<String>,
    hours: u64,
    format: &str,
) -> Result<()> {
    let ctx = load_session_claims(session, agent, hours)?;
    match format {
        "summary" => {
            println!(
                "{} claim(s) from session {} ({})",
                ctx.claims.len(),
                ctx.canonical_id,
                ctx.agent
            );
            for c in &ctx.claims {
                let flag = if c.risk_flags.is_empty() {
                    ""
                } else {
                    " [HIGH-RISK]"
                };
                println!(
                    "- {} (unverified){}: {}",
                    c.claim_type.label(),
                    flag,
                    truncate_table_cell(&c.claim_text, 90)
                );
            }
        }
        _ => {
            let export = ctx.envelope("claims", "agent_only", &ctx.claims);
            println!("{}", serde_json::to_string_pretty(&export)?);
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct ResultsPayload {
    claims: Vec<intents::ClaimRecord>,
    results: Vec<intents::ResultRecord>,
}

fn run_results_command(command: ResultsCommand) -> Result<()> {
    match command {
        ResultsCommand::Collect {
            session,
            agent,
            hours,
            repo,
            format,
        } => run_results_collect(&session, agent, hours, repo, &format),
    }
}

/// Lane 3 chain shared by `results collect` and `clarify`: extract claims,
/// collect artifact evidence against the repo, fold it into verification.
fn collect_and_verify(
    session: &str,
    agent: Option<String>,
    hours: u64,
    repo: Option<PathBuf>,
) -> Result<(LaneSessionContext, PathBuf, Vec<intents::ResultRecord>)> {
    let mut ctx = load_session_claims(session, agent, hours)?;
    let repo_root = match repo {
        Some(p) => p,
        None => std::env::current_dir().context("cannot resolve current dir; pass --repo")?,
    };
    let results = intents::collect_artifact_evidence(&ctx.claims, &repo_root, &ctx.extracted_at);
    intents::verify_claims(&mut ctx.claims, &results);
    Ok((ctx, repo_root, results))
}

fn run_results_collect(
    session: &str,
    agent: Option<String>,
    hours: u64,
    repo: Option<PathBuf>,
    format: &str,
) -> Result<()> {
    let (ctx, repo_root, results) = collect_and_verify(session, agent, hours, repo)?;
    match format {
        "summary" => {
            println!(
                "{} claim(s), {} evidence result(s) against {}",
                ctx.claims.len(),
                results.len(),
                repo_root.display()
            );
            for c in &ctx.claims {
                println!(
                    "- [{}] {}: {}",
                    format!("{:?}", c.verification_status).to_lowercase(),
                    c.claim_type.label(),
                    truncate_table_cell(&c.claim_text, 80)
                );
            }
        }
        _ => {
            let export = ctx.envelope(
                "results",
                "agent_only",
                ResultsPayload {
                    claims: ctx.claims.clone(),
                    results,
                },
            );
            println!("{}", serde_json::to_string_pretty(&export)?);
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct ClarifyPayload {
    fractures: Vec<intents::ContractFracture>,
    questions: Vec<intents::ClarifyQuestion>,
}

fn run_clarify(
    session: &str,
    agent: Option<String>,
    hours: u64,
    repo: Option<PathBuf>,
    max: usize,
    format: &str,
) -> Result<()> {
    let (ctx, _repo_root, _results) = collect_and_verify(session, agent, hours, repo)?;
    let fractures = intents::detect_fractures(&ctx.claims);
    let questions = intents::generate_clarify(&fractures, max);
    match format {
        "json" => {
            let export = ctx.envelope(
                "clarify",
                "agent_only",
                ClarifyPayload {
                    fractures,
                    questions,
                },
            );
            println!("{}", serde_json::to_string_pretty(&export)?);
        }
        _ => {
            println!("# Clarify — session {}\n", ctx.canonical_id);
            println!("- generated_at: {}", ctx.extracted_at);
            println!("- fractures: {}", fractures.len());
            println!("- questions: {} (cap 5)\n", questions.len());
            if questions.is_empty() {
                println!("No unresolved decisions — no contradicted or unbacked claims found.");
            }
            for (i, q) in questions.iter().enumerate() {
                println!("## {}. {}\n", i + 1, q.question);
                println!("why now: {}\n", q.why_now);
                for fact in &q.known_facts {
                    println!("- {fact}");
                }
                println!();
                for opt in &q.options {
                    println!("  {opt}");
                }
                println!("\n  default: {}", q.default_recommendation);
                println!("  cost of not deciding: {}\n", q.cost_of_not_deciding);
            }
        }
    }
    Ok(())
}

fn run_sessions_command(command: SessionsCommand) -> Result<()> {
    match command {
        SessionsCommand::List {
            cwd,
            agent,
            since,
            all,
            limit,
            format,
        } => run_sessions_list(cwd, agent, since, all, limit, &format),
        SessionsCommand::Show { session_id, format } => run_session_show(session_id, &format),
        SessionsCommand::Report {
            session_id,
            agent,
            hours,
            repo,
            max,
            format,
        } => run_session_report(&session_id, agent, hours, repo, max, &format),
    }
}

#[derive(serde::Serialize)]
struct SessionReportPayload {
    user_intents: Vec<intents::UserIntentLine>,
    claims: Vec<intents::ClaimRecord>,
    results: Vec<intents::ResultRecord>,
    fractures: Vec<intents::ContractFracture>,
    questions: Vec<intents::ClarifyQuestion>,
}

/// Unified per-session truth report: all five lanes in one rendering, so an
/// agent (or operator) entering a repo can answer in one pass — what the human
/// wanted, what the agent claimed, what evidence verified, what is
/// fake-complete, and what decision is still open.
fn run_session_report(
    session: &str,
    agent: Option<String>,
    hours: u64,
    repo: Option<PathBuf>,
    max: usize,
    format: &str,
) -> Result<()> {
    let (ctx, repo_root, results) = collect_and_verify(session, agent, hours, repo)?;
    let fractures = intents::detect_fractures(&ctx.claims);
    let questions = intents::generate_clarify(&fractures, max);
    if format == "json" {
        let export = ctx.envelope(
            "report",
            "all",
            SessionReportPayload {
                user_intents: ctx.user_intents.clone(),
                claims: ctx.claims.clone(),
                results,
                fractures,
                questions,
            },
        );
        println!("{}", serde_json::to_string_pretty(&export)?);
        return Ok(());
    }

    println!(
        "# Session truth report — {} ({})\n",
        ctx.canonical_id, ctx.agent
    );
    println!("- project: {}", ctx.project);
    println!("- repo evidence root: {}", repo_root.display());
    println!("- generated_at: {} (UTC)", ctx.extracted_at);
    if let Some(c) = &ctx.coverage {
        println!("- source time coverage: {} .. {}", c.earliest, c.latest);
    }
    for w in &ctx.warnings {
        println!("- warning: {w}");
    }

    println!("\n## Lane 1 — human intent ({})\n", ctx.user_intents.len());
    if ctx.user_intents.is_empty() {
        println!(
            "(no classified user intent lines; raw user text may still carry direction — see `aicx extract --conversation --user-only`)"
        );
    }
    for ui in &ctx.user_intents {
        println!(
            "- [{}] {} — {}",
            ui.entry_type,
            ui.timestamp.as_deref().unwrap_or("(no timestamp)"),
            truncate_table_cell(&ui.raw_text, 100)
        );
    }

    println!(
        "\n## Lanes 2-3 — agent claims vs evidence ({})\n",
        ctx.claims.len()
    );
    for c in &ctx.claims {
        let status = format!("{:?}", c.verification_status).to_lowercase();
        let flag = if c.risk_flags.is_empty() {
            ""
        } else {
            " [HIGH-RISK]"
        };
        println!(
            "- [{status}]{flag} {}: {}",
            c.claim_type.label(),
            truncate_table_cell(&c.claim_text, 90)
        );
    }
    let fake_complete: Vec<_> = ctx
        .claims
        .iter()
        .filter(|c| {
            matches!(
                c.verification_status,
                intents::VerificationStatus::Contradicted
            ) || (!c.risk_flags.is_empty()
                && !matches!(c.verification_status, intents::VerificationStatus::Verified))
        })
        .collect();
    println!("\n### Fake-complete candidates ({})\n", fake_complete.len());
    for c in &fake_complete {
        println!(
            "- {}: {}",
            c.claim_type.label(),
            truncate_table_cell(&c.claim_text, 90)
        );
    }

    println!("\n## Lane 4 — contract fractures ({})\n", fractures.len());
    for f in &fractures {
        println!(
            "- [{:?}] {} — promised: {} | runtime: {}",
            f.severity,
            f.claim_id,
            truncate_table_cell(&f.promised_surface, 60),
            truncate_table_cell(&f.runtime_surface, 60)
        );
    }

    println!(
        "\n## Lane 5 — clarify ({} question(s), cap 5)\n",
        questions.len()
    );
    if questions.is_empty() {
        println!("No unresolved human decisions detected from this session's claims.");
    }
    for (i, q) in questions.iter().enumerate() {
        println!("{}. {}", i + 1, q.question);
        for opt in &q.options {
            println!("   {opt}");
        }
        println!("   default: {}", q.default_recommendation);
    }
    Ok(())
}

fn run_session_show(session_id: String, format: &str) -> Result<()> {
    let home = dirs::home_dir().context("No home dir")?;
    let Some(info) = sessions::find_session_by_id(&home, &session_id) else {
        anyhow::bail!("no session found matching id '{session_id}'");
    };
    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&info)?),
        _ => {
            let ts = |t: Option<chrono::DateTime<Utc>>| {
                t.map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "(unknown)".to_string())
            };
            println!("# Session {}\n", info.session_id);
            println!("- agent: {}", info.agent);
            println!("- project: {}", info.project.as_deref().unwrap_or("-"));
            println!("- repo: {}", info.repo_path.as_deref().unwrap_or("-"));
            println!("- started: {}", ts(info.started_at));
            println!("- updated: {}", ts(info.updated_at));
            println!(
                "- messages: {} ({} user / {} agent)",
                info.message_count, info.user_message_count, info.agent_message_count
            );
            println!("- association: {:?}", info.association);
            println!("- temporal_confidence: {:?}", info.temporal_confidence);
            println!("- source: {}", info.source_path.display());
            if let Some(t) = &info.title {
                println!("- title: {t}");
            }
            println!(
                "\n## extract\n\n    aicx extract --agent {} --session {} --conversation",
                info.agent, info.session_id
            );
        }
    }
    Ok(())
}

fn parse_since_date(s: &str) -> Result<DateTime<Utc>> {
    let nd = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("invalid --since date '{s}' (expected YYYY-MM-DD)"))?;
    // NaiveTime::MIN is midnight — no panicking unwrap path.
    Ok(nd.and_time(chrono::NaiveTime::MIN).and_utc())
}

fn run_sessions_list(
    cwd_only: bool,
    agent: Option<String>,
    since: Option<String>,
    all: bool,
    limit: usize,
    format: &str,
) -> Result<()> {
    // Recency window: default to the last 30 days so the scan stays fast on
    // large histories; --since sets it explicitly, --all scans everything.
    let since_dt: Option<DateTime<Utc>> = if all {
        None
    } else if let Some(s) = &since {
        Some(parse_since_date(s)?)
    } else {
        Some(Utc::now() - chrono::Duration::days(30))
    };
    // Cheap mtime pre-filter mirrors the window so old files are skipped before
    // the expensive full parse.
    let modified_after: Option<std::time::SystemTime> = since_dt.map(|dt| {
        let secs = dt.timestamp().max(0) as u64;
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    });

    // Resolve cwd BEFORE discovery: Claude encodes the cwd in its project dir
    // name, so passing it down lets discovery prune non-matching dirs without
    // reading a single file — the fast path for `--cwd`.
    let here = if cwd_only {
        Some(std::env::current_dir()?.to_string_lossy().into_owned())
    } else {
        None
    };

    let home = dirs::home_dir().context("No home dir")?;
    // Gate discovery by --agent so e.g. `--agent gemini` scans only gemini
    // instead of reading every claude+codex file and filtering afterwards.
    let want_agent = agent.as_deref();
    let mut discovered = Vec::new();
    if want_agent.is_none_or(|a| a == "claude") {
        discovered.extend(sessions::discover_claude_sessions(
            &home.join(".claude").join("projects"),
            modified_after,
            here.as_deref(),
        ));
    }
    if want_agent.is_none_or(|a| a == "codex") {
        discovered.extend(sessions::discover_codex_sessions(
            &home.join(".codex").join("sessions"),
            modified_after,
        ));
    }
    if want_agent.is_none_or(|a| a == "gemini") {
        discovered.extend(sessions::discover_gemini_sessions(
            &home.join(".gemini").join("tmp"),
            modified_after,
            here.as_deref(),
        ));
    }
    if want_agent.is_none_or(|a| a == "junie") {
        // Junie has no cwd in its dir layout (the dir name is a timestamp), so
        // there is no pre-read prune; select_sessions applies the --cwd filter
        // against the recorded CurrentDirectoryUpdatedEvent cwd afterwards.
        discovered.extend(sessions::discover_junie_sessions(
            &home.join(".junie").join("sessions"),
            modified_after,
        ));
    }

    let selected = sessions::select_sessions(
        discovered,
        here.as_deref(),
        agent.as_deref(),
        since_dt,
        limit,
    );

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&selected)?),
        _ => {
            if selected.is_empty() {
                eprintln!("No sessions found.");
                return Ok(());
            }
            println!(
                "{:<10}  {:<6}  {:<22}  {:<25}  {:>5}  {:>4}  {:<8}  TITLE",
                "SESSION", "AGENT", "PROJECT", "UPDATED (UTC)", "MSGS", "USR", "ASSOC"
            );
            for s in &selected {
                let sid = session_id_table_prefix(&s.session_id);
                let project = s.project.as_deref().unwrap_or("-");
                let updated = s
                    .updated_at
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_else(|| "(no timestamp)".to_string());
                let assoc = format!("{:?}", s.association).to_lowercase();
                println!(
                    "{:<10}  {:<6}  {:<22}  {:<25}  {:>5}  {:>4}  {:<8}  {}",
                    sid,
                    s.agent,
                    truncate_table_cell(project, 22),
                    updated,
                    s.message_count,
                    s.user_message_count,
                    assoc,
                    truncate_table_cell(s.title.as_deref().unwrap_or(""), 60),
                );
            }
        }
    }
    Ok(())
}

/// First 8 chars of a session id for the sessions table — char-safe. Ids
/// normally are ASCII uuids, but the file-stem fallback can carry non-ASCII;
/// a byte slice would panic on a multibyte boundary.
fn session_id_table_prefix(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Char-boundary-safe cell truncation for table output (never byte-slices a
/// multibyte char — that path panics, cf. the thread-index UTF-8 crash).
fn truncate_table_cell(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let clipped: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{clipped}…")
    }
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
    projects: &[String],
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

    // F2 / dead `--unresolved`: the unresolved filter marks a session "resolved"
    // when it contains an Outcome. If a `--kind` filter strips Outcomes at
    // extraction time, the filter sees none and silently no-ops (output is
    // byte-identical to unfiltered). When `--unresolved` is active we defer the
    // kind filter: extract WITHOUT it so Outcomes survive the resolution check,
    // then re-apply it after the unresolved narrowing.
    let post_kind = if unresolved { kind_filter } else { None };
    let config = intents::IntentsConfig {
        project: projects.first().cloned().unwrap_or_default(),
        hours,
        strict,
        kind_filter: if unresolved { None } else { kind_filter },
        frame_kind: filters.frame_kind.map(Into::into),
    };

    let extraction = intents::extract_intents_with_stats_for_projects(&config, projects)?;
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
        // F3 / default-limit clip (P2-11): `--limit` is a true Option now.
        // None means "no limit" so a full intents roadmap (often 20-30
        // planned items) survives by default, while an explicit `--limit 10`
        // is honored instead of being mistaken for a default sentinel.
        limit: filters.limit,
    };

    let mut records = intents::apply_display_filters(records, &display_filters);

    // F2: re-apply the kind filter we deferred so `--unresolved` could see Outcomes.
    if let Some(k) = post_kind {
        records.retain(|r| r.kind == k);
    }

    if records.is_empty() && emit != "json" {
        eprintln!(
            "No intents found for {} in last {} hours.",
            project_scope_label(projects),
            hours
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
    projects: &[String],
    hours: u64,
    follow: bool,
    kind: Option<&str>,
    mut filters: RetrievalFilters,
) -> Result<()> {
    if !follow {
        // One-shot mode: default to 20 when no explicit --limit was passed
        // (an explicit `--limit 10` now means 10 — P2-11).
        if filters.limit.is_none() {
            filters.limit = Some(20);
        }
        filters.sort = Some(SortOrder::Newest);
        return run_intents(
            projects,
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
        project: projects.first().cloned().unwrap_or_default(),
        hours,
        strict: false,
        kind_filter,
        frame_kind: filters.frame_kind.map(Into::into),
    };

    let mut last_seen = std::collections::HashSet::new();
    eprintln!(
        "Watching for new intents in {}...",
        project_scope_label(projects)
    );

    loop {
        if let Ok(extraction) = intents::extract_intents_with_stats_for_projects(&config, projects)
        {
            let mut records = extraction.records;
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
const DEFAULT_SESSION_EXTRACT_FILENAME_STEM_MAX_BYTES: usize = 180;

fn safe_session_extract_stem(session_id: &str) -> String {
    let is_already_safe = !session_id.is_empty()
        && session_id.len() <= DEFAULT_SESSION_EXTRACT_FILENAME_STEM_MAX_BYTES
        && !session_id.chars().all(|c| c == '.')
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if is_already_safe {
        return session_id.to_string();
    }

    if session_id.is_empty() {
        return "session".to_string();
    }

    let mut safe = String::new();
    let mut previous_was_separator = false;

    for ch in session_id.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            ch
        } else {
            '_'
        };

        if mapped == '_' {
            if !previous_was_separator {
                safe.push(mapped);
            }
            previous_was_separator = true;
        } else {
            safe.push(mapped);
            previous_was_separator = false;
        }
    }

    let safe = safe.trim_matches(|ch| ch == '_' || ch == '.');
    let base = if safe.is_empty() { "session" } else { safe };
    let base_max_len = DEFAULT_SESSION_EXTRACT_FILENAME_STEM_MAX_BYTES - 17;
    let capped_base = if base.len() > base_max_len {
        &base[..base_max_len]
    } else {
        base
    };

    use siphasher::sip::SipHasher13;
    use std::hash::{Hash, Hasher};
    let mut hasher = SipHasher13::new();
    session_id.hash(&mut hasher);
    let suffix = hasher.finish();
    format!("{capped_base}-{suffix:016x}")
}

fn default_session_extract_path_for_stem(agent_label: &str, stem: &str) -> Result<PathBuf> {
    let base = aicx::store::store_base_dir()?;
    Ok(base
        .join("extracts")
        .join(agent_label)
        .join(format!("{stem}.md")))
}

/// Compose the default session-extract path for a given mode pair. The stem
/// encodes BOTH axes so the four modes never collide on disk:
///   * full, both roles            -> `<stem>.md`
///   * full, user-only             -> `<stem>_user.md`
///   * conversation, both roles    -> `<stem>_conversation.md`
///   * conversation, user-only     -> `<stem>_conversation_user.md`
fn default_session_extract_path_for(
    agent_label: &str,
    session_id: &str,
    conversation: bool,
    user_only: bool,
) -> Result<PathBuf> {
    let mut stem = safe_session_extract_stem(session_id);
    if conversation {
        stem.push_str("_conversation");
    }
    if user_only {
        stem.push_str("_user");
    }
    default_session_extract_path_for_stem(agent_label, &stem)
}

struct ConversationsBatchOptions {
    agent: String,
    project_filter: Vec<String>,
    hours: u64,
    out_dir: PathBuf,
    limit: Option<usize>,
    dry_run: bool,
    redact_secrets: bool,
}

struct ConversationBatchWriteOptions<'a> {
    agent_label: &'a str,
    entries: Vec<timeline::TimelineEntry>,
    project_filter: Vec<String>,
    out_dir: PathBuf,
    limit: Option<usize>,
    dry_run: bool,
    redaction_enabled: bool,
}

#[derive(Debug)]
struct ConversationBatchSummary {
    sessions_discovered: usize,
    sessions_written: usize,
    messages_total: usize,
    output_dir: PathBuf,
    failed_sessions: usize,
}

fn conversation_batch_safe_session_filename(session_id: &str) -> String {
    // Empty input has no original characters to disambiguate against, so
    // skip the hash suffix and use the fixed fallback. Realistically this
    // signals an upstream bug; we keep the existing observable contract.
    if session_id.is_empty() {
        return "session".to_string();
    }
    // Already-safe ids (alphanumeric plus `- _ .`) round-trip verbatim.
    // Previously this function collapsed runs of underscores and trimmed
    // leading/trailing ones for *every* input, which meant safe ids like
    // "a__b" and "a_b" — both valid on disk — would map to the same
    // filename and silently overwrite each other (no hash suffix was
    // added because no character was actually replaced).
    let is_already_safe = session_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if is_already_safe {
        return session_id.to_string();
    }

    // Otherwise the id contains characters we cannot put on disk. Replace
    // them with `_`, collapse resulting runs of underscores, trim leading
    // and trailing ones, and append a 64-bit SipHash fingerprint of the
    // original id so distinct unsafe ids that collapse to the same base
    // (e.g. "a/b" vs "a:b" both become "a_b") cannot overwrite each other.
    let mut safe = String::new();
    let mut previous_was_separator = false;

    for ch in session_id.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            ch
        } else {
            '_'
        };

        if mapped == '_' {
            if !previous_was_separator {
                safe.push(mapped);
            }
            previous_was_separator = true;
        } else {
            safe.push(mapped);
            previous_was_separator = false;
        }
    }

    let safe = safe.trim_matches('_');
    let base = if safe.is_empty() { "session" } else { safe };

    use siphasher::sip::SipHasher13;
    use std::hash::{Hash, Hasher};
    let mut hasher = SipHasher13::new();
    session_id.hash(&mut hasher);
    let suffix = hasher.finish();
    format!("{base}-{suffix:016x}")
}

fn conversation_batch_output_path(out_dir: &Path, agent_label: &str, session_id: &str) -> PathBuf {
    out_dir.join(agent_label).join(format!(
        "{}.json",
        conversation_batch_safe_session_filename(session_id)
    ))
}

fn run_conversations_batch(options: ConversationsBatchOptions) -> Result<()> {
    if options.agent != "claude" {
        anyhow::bail!("conversations v1 supports --agent claude only");
    }

    let cutoff = lookback_cutoff(options.hours);
    let config = ExtractionConfig {
        project_filter: options.project_filter.clone(),
        cutoff,
        include_assistant: true,
        watermark: None,
    };

    let entries = sources::extract_claude(&config)?;

    // Pre-compute discovery-aware histograms so dry-run can emit a
    // pipe-friendly JSON envelope alongside the human banner (Wave D
    // Cut D1 / B-P0-04 dual-channel parity with
    // `migrate-intent-schema --dry-run`).
    let by_kind = conversations_discovery_by_kind(&entries);
    let by_agent = conversations_discovery_by_agent(&entries);

    let dry_run = options.dry_run;
    let hours = options.hours;
    let limit = options.limit;
    let project_filter_snapshot = options.project_filter.clone();
    let agent_label = options.agent.clone();

    let summary = write_conversation_batch_outputs(ConversationBatchWriteOptions {
        agent_label: &options.agent,
        entries,
        project_filter: options.project_filter,
        out_dir: options.out_dir,
        limit: options.limit,
        dry_run,
        redaction_enabled: options.redact_secrets,
    })?;

    if dry_run {
        // Dual-channel emission (B-P0-04): JSON envelope on stdout for
        // pipeline consumers (`aicx conversations --dry-run | jq .`),
        // styled human banner on stderr for operators. Mirror the
        // `migrate-intent-schema --dry-run` gold-pattern.
        let envelope = serde_json::json!({
            "dry_run": true,
            "agent": agent_label,
            "sessions_discovered": summary.sessions_discovered,
            "messages_total": summary.messages_total,
            "by_kind": by_kind,
            "by_agent": by_agent,
            "filters_applied": {
                "project": project_filter_snapshot,
                "hours": hours,
                "limit": limit,
            },
            "output_dir": summary.output_dir.display().to_string(),
        });
        match serde_json::to_string_pretty(&envelope) {
            Ok(rendered) => println!("{rendered}"),
            Err(_) => println!("{envelope}"),
        }

        eprintln!("=== Conversations Dry-Run ===");
        eprintln!("Agent:              {}", agent_label);
        eprintln!("Sessions discovered: {}", summary.sessions_discovered);
        eprintln!("Messages total:      {}", summary.messages_total);
        eprintln!(
            "Output dir (would write to): {}",
            summary.output_dir.display()
        );
        if !by_kind.is_empty() {
            eprintln!();
            eprintln!("Per frame_kind:");
            let mut kinds: Vec<(&String, &usize)> = by_kind.iter().collect();
            kinds.sort_by(|a, b| b.1.cmp(a.1));
            for (kind, count) in kinds {
                eprintln!("  {:<24} {}", kind, count);
            }
        }
    } else {
        eprintln!("sessions_discovered={}", summary.sessions_discovered);
        eprintln!("sessions_written={}", summary.sessions_written);
        eprintln!("messages_total={}", summary.messages_total);
        eprintln!("output_dir={}", summary.output_dir.display());
        eprintln!("failed_sessions={}", summary.failed_sessions);
    }

    Ok(())
}

/// Frame-kind histogram across the extracted timeline entries.
///
/// Used by the dry-run JSON envelope (B-P0-04). Entries without a
/// `frame_kind` are bucketed under `"unknown"` so operators see the
/// "noise floor" for sessions where the parser could not classify.
fn conversations_discovery_by_kind(entries: &[timeline::TimelineEntry]) -> BTreeMap<String, usize> {
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for entry in entries {
        let bucket = entry
            .frame_kind
            .map(|kind| kind.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        *by_kind.entry(bucket).or_insert(0) += 1;
    }
    by_kind
}

/// Agent histogram across the extracted timeline entries.
///
/// In conversations v1 the agent is always `"claude"` (the only
/// supported source), but the field is emitted for forward-compat with
/// future multi-agent exports.
fn conversations_discovery_by_agent(
    entries: &[timeline::TimelineEntry],
) -> BTreeMap<String, usize> {
    let mut by_agent: BTreeMap<String, usize> = BTreeMap::new();
    for entry in entries {
        *by_agent.entry(entry.agent.clone()).or_insert(0) += 1;
    }
    by_agent
}

fn write_conversation_batch_outputs(
    options: ConversationBatchWriteOptions<'_>,
) -> Result<ConversationBatchSummary> {
    let ConversationBatchWriteOptions {
        agent_label,
        entries,
        project_filter,
        out_dir,
        limit,
        dry_run,
        redaction_enabled,
    } = options;

    let mut grouped: BTreeMap<String, Vec<timeline::TimelineEntry>> = BTreeMap::new();
    for entry in entries {
        grouped
            .entry(entry.session_id.clone())
            .or_default()
            .push(entry);
    }

    let sessions_discovered = grouped.len();
    if !dry_run {
        fs::create_dir_all(out_dir.join(agent_label)).with_context(|| {
            format!(
                "Failed to create conversation output dir: {}",
                out_dir.join(agent_label).display()
            )
        })?;
    }

    let mut sessions_written = 0;
    let mut messages_total = 0;
    let mut failed_sessions = 0;
    let max_sessions = limit.unwrap_or(usize::MAX);

    for (session_id, mut session_entries) in grouped.into_iter().take(max_sessions) {
        let result = write_conversation_batch_session(
            agent_label,
            &project_filter,
            &out_dir,
            &session_id,
            &mut session_entries,
            dry_run,
            redaction_enabled,
        );

        match result {
            Ok(messages_written) => {
                if !dry_run {
                    sessions_written += 1;
                }
                messages_total += messages_written;
            }
            Err(error) => {
                failed_sessions += 1;
                eprintln!("failed_session={} error={error:#}", session_id);
            }
        }
    }

    Ok(ConversationBatchSummary {
        sessions_discovered,
        sessions_written,
        messages_total,
        output_dir: out_dir,
        failed_sessions,
    })
}

fn write_conversation_batch_session(
    agent_label: &str,
    project_filter: &[String],
    out_dir: &Path,
    session_id: &str,
    entries: &mut Vec<timeline::TimelineEntry>,
    dry_run: bool,
    redaction_enabled: bool,
) -> Result<usize> {
    entries.sort_by_key(|entry| entry.timestamp);
    let (mut entries, _) = aicx_parser::collapse_repeats(
        std::mem::take(entries),
        aicx_parser::DEFAULT_THRESHOLD_LINES,
    );

    if redaction_enabled {
        for entry in &mut entries {
            entry.message = aicx::redact::redact_secrets(&entry.message);
        }
    }

    let inferred_repos = sources::repo_labels_from_entries(&entries, &[]);
    let project_identity = if !project_filter.is_empty() {
        project_filter.join("+")
    } else if inferred_repos.is_empty() {
        format!("{agent_label}/{session_id}")
    } else {
        inferred_repos.join("+")
    };

    let hours_back = entries
        .first()
        .map(|entry| (Utc::now() - entry.timestamp).num_hours().max(0) as u64)
        .unwrap_or(0);

    let metadata = ReportMetadata {
        generated_at: Utc::now(),
        project_filter: Some(project_identity.clone()),
        hours_back,
        total_entries: entries.len(),
        sessions: vec![session_id.to_string()],
    };
    let projection = sources::to_conversation_with_stats(&entries, &[project_identity]);
    let extract_stats = output::ConversationExtractStats {
        aicx_version: env!("CARGO_PKG_VERSION"),
        redaction_enabled,
        raw_entries: entries.len(),
        conversation_messages: projection.messages.len(),
        conversation_projection: "user_assistant_only",
        exact_short_duplicates_dropped: projection.exact_short_duplicates_dropped,
        harness_noise_dropped: projection.harness_noise_dropped,
    };

    if !dry_run {
        let output_path = conversation_batch_output_path(out_dir, agent_label, session_id);
        output::write_conversation_json_with_redaction(
            &output_path,
            &projection.messages,
            &metadata,
            &extract_stats,
            false,
        )?;
    }

    Ok(projection.messages.len())
}

fn uuid_suffix_from_stem(stem: &str) -> Option<&str> {
    let start = stem.len().checked_sub(36)?;
    let suffix = &stem[start..];
    let bytes = suffix.as_bytes();
    let is_uuid_like = bytes.iter().enumerate().all(|(idx, byte)| {
        if matches!(idx, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    });
    is_uuid_like.then_some(suffix)
}

fn read_codex_session_meta_id(path: &Path) -> Option<String> {
    // Route through the project-wide validated opener so symlink/path-safety
    // guarantees apply uniformly to every place that ingests Codex rollouts.
    let file = aicx::sanitize::open_file_validated(path).ok()?;
    let mut reader = BufReader::new(file);
    while let Ok(Some(line)) =
        aicx::sanitize::read_line_capped(&mut reader, aicx::sanitize::MAX_VALIDATED_BYTES)
    {
        if line.exceeded {
            continue;
        }
        let line = line.line;
        if !line.contains("\"session_meta\"") {
            continue;
        }
        // Skip malformed lines instead of bailing out of the whole scan —
        // a partially-written rollout file can have a truncated tail, and
        // the session_meta record we want is usually one of the first
        // entries. Treat a parse error on a single candidate line as a
        // miss for that line, not as "this file has no session_meta".
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if data.get("type").and_then(|value| value.as_str()) != Some("session_meta") {
            continue;
        }
        return data
            .get("payload")
            .and_then(|payload| payload.get("id"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string());
    }
    None
}

fn collect_codex_session_alias_matches(requested: &str) -> Result<BTreeSet<String>> {
    let mut matches = BTreeSet::new();
    let sessions_dir = dirs::home_dir()
        .context("No home dir")?
        .join(".codex")
        .join("sessions");
    if !sessions_dir.is_dir() {
        return Ok(matches);
    }

    let mut stack = vec![sessions_dir];
    while let Some(dir) = stack.pop() {
        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }

            let stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            let file_name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            let suffix = uuid_suffix_from_stem(stem);
            let suffix_owned: Option<String> = suffix.map(str::to_string);

            // Avoid reading session_meta from every JSONL in the tree.
            // Cheap path: try matching against the filename stem / UUID
            // suffix first. Only open the rollout when (a) the cheap
            // check matched and we need the canonical id for the output,
            // or (b) the filename carries no UUID suffix (non-rollout
            // layout) and we have nothing else to anchor on.
            let cheap_anchor: &str = suffix_owned.as_deref().unwrap_or(stem);
            let cheap_match = requested == stem
                || requested == file_name
                || cheap_anchor.starts_with(requested)
                || cheap_anchor.ends_with(requested);

            let canonical = if cheap_match || suffix.is_none() {
                read_codex_session_meta_id(&path)
                    .or_else(|| suffix_owned.clone())
                    .unwrap_or_else(|| stem.to_string())
            } else {
                // Have a UUID suffix and no cheap hit — trust the suffix
                // as the canonical id rather than opening the file.
                suffix_owned.clone().unwrap_or_default()
            };

            let alias_matches =
                cheap_match || canonical.starts_with(requested) || canonical.ends_with(requested);
            if alias_matches {
                matches.insert(canonical);
            }
        }
    }

    Ok(matches)
}

fn resolve_session_reference_from_candidates(
    requested: &str,
    session_ids: &BTreeSet<String>,
    alias_matches: BTreeSet<String>,
    agent_label: &str,
) -> Result<SessionResolution> {
    if session_ids.contains(requested) {
        return Ok(SessionResolution {
            canonical_id: requested.to_string(),
            note: None,
        });
    }

    let mut candidates: BTreeSet<String> = session_ids
        .iter()
        .filter(|session_id| session_id.starts_with(requested) || session_id.ends_with(requested))
        .cloned()
        .collect();
    // Restrict alias matches (gathered by walking `~/.codex/sessions/` for
    // filename UUID anchors) to ids that were actually extracted in the
    // current `--hours` / `--project` window. Without this guard, older
    // out-of-window sessions inflate the candidate set: a previously unique
    // in-window prefix can flip to "ambiguous", or the resolver can pick an
    // out-of-window id that then yields zero entries downstream.
    let in_window_aliases: BTreeSet<String> = alias_matches
        .into_iter()
        .filter(|alias| session_ids.contains(alias))
        .collect();
    candidates.extend(in_window_aliases);

    match candidates.len() {
        0 => anyhow::bail!(
            "No session matched `{}` in agent `{}`. Scanned {} extracted session id(s).\n\
             Try: use the full session id, increase --hours, or run `aicx extract --agent {} --help`.",
            requested,
            agent_label,
            session_ids.len(),
            agent_label,
        ),
        1 => {
            let canonical_id = candidates.into_iter().next().unwrap_or_default();
            Ok(SessionResolution {
                note: Some(format!("resolved `{requested}` to `{canonical_id}`")),
                canonical_id,
            })
        }
        _ => {
            let shown = candidates.iter().take(8).cloned().collect::<Vec<_>>();
            anyhow::bail!(
                "Ambiguous session reference `{}` in agent `{}`; matched {} sessions:\n  {}\n\
                 Use the full session id.",
                requested,
                agent_label,
                candidates.len(),
                shown.join("\n  "),
            )
        }
    }
}

fn resolve_session_reference(
    requested: &str,
    agent: ExtractInputFormat,
    agent_label: &str,
    entries: &[timeline::TimelineEntry],
) -> Result<SessionResolution> {
    let session_ids = entries
        .iter()
        .map(|entry| entry.session_id.clone())
        .collect::<BTreeSet<_>>();
    let alias_matches = if matches!(agent, ExtractInputFormat::Codex) {
        collect_codex_session_alias_matches(requested)?
    } else {
        BTreeSet::new()
    };
    resolve_session_reference_from_candidates(requested, &session_ids, alias_matches, agent_label)
}

/// Run extraction filtered by `session_id` for a single agent and write either
/// a full report or a denoised conversation transcript. The default output path
/// encodes both the `--conversation` and `--user-only` axes so the four modes
/// never collide:
///   * `~/.aicx/extracts/<agent>/<session_id>.md`
///   * `~/.aicx/extracts/<agent>/<session_id>_user.md` (`--user-only`)
///   * `~/.aicx/extracts/<agent>/<session_id>_conversation.md` (`--conversation`)
///   * `~/.aicx/extracts/<agent>/<session_id>_conversation_user.md` (both)
///
/// Override via `output`.
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
    let cutoff = lookback_cutoff(hours);
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

    let resolution = resolve_session_reference(session_id, agent, agent_label, &entries)?;
    if let Some(note) = &resolution.note {
        eprintln!("{note}");
    }

    entries.retain(|e| e.session_id == resolution.canonical_id);

    if entries.is_empty() {
        anyhow::bail!(
            "Resolved session `{}` to `{}`, but no entries were extractable for agent `{}` within {}.\n\
             Try: increase --hours, verify the project filter, or check that the source store is populated.",
            session_id,
            resolution.canonical_id,
            agent_label,
            lookback_label(hours),
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
        // `user_only` (== `!include_assistant`) is a distinct output axis: a
        // user-only extract must not overwrite the both-roles extract of the
        // same session/mode, so it earns its own `_user` suffix.
        None => default_session_extract_path_for(
            agent_label,
            &resolution.canonical_id,
            conversation,
            !include_assistant,
        )?,
    };

    let inferred_repos = sources::repo_labels_from_entries(&entries, &[]);
    let project_identity = explicit_project.unwrap_or_else(|| {
        if inferred_repos.is_empty() {
            format!("{agent_label}/{}", resolution.canonical_id)
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
        sessions: vec![resolution.canonical_id.clone()],
    };

    if conversation {
        let projection = sources::to_conversation_with_stats(&entries, &[project_identity]);
        let extract_stats = output::ConversationExtractStats {
            aicx_version: env!("CARGO_PKG_VERSION"),
            redaction_enabled: redact_secrets,
            raw_entries: entries.len(),
            conversation_messages: projection.messages.len(),
            conversation_projection: "user_assistant_only",
            exact_short_duplicates_dropped: projection.exact_short_duplicates_dropped,
            harness_noise_dropped: projection.harness_noise_dropped,
        };
        let ext = output_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("md")
            .to_lowercase();
        if ext == "json" {
            output::write_conversation_json_with_redaction(
                &output_path,
                &projection.messages,
                &metadata,
                &extract_stats,
                false,
            )?;
        } else {
            output::write_conversation_markdown_with_redaction(
                &output_path,
                &projection.messages,
                &metadata,
                false,
            )?;
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
        resolution.canonical_id,
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
        let projection = sources::to_conversation_with_stats(&output_entries, &project_filter);
        let extract_stats = output::ConversationExtractStats {
            aicx_version: env!("CARGO_PKG_VERSION"),
            redaction_enabled: redact_secrets,
            raw_entries: output_entries.len(),
            conversation_messages: projection.messages.len(),
            conversation_projection: "user_assistant_only",
            exact_short_duplicates_dropped: projection.exact_short_duplicates_dropped,
            harness_noise_dropped: projection.harness_noise_dropped,
        };

        let ext = output_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("md")
            .to_lowercase();

        if ext == "json" {
            output::write_conversation_json_with_redaction(
                &output_path,
                &projection.messages,
                &metadata,
                &extract_stats,
                false,
            )?;
        } else {
            output::write_conversation_markdown_with_redaction(
                &output_path,
                &projection.messages,
                &metadata,
                false,
            )?;
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

fn normalized_source_key_parts<'a>(parts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut normalized = parts
        .into_iter()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    normalized.sort_unstable();
    normalized
}

fn normalized_project_source_key(project: &[String]) -> String {
    if project.is_empty() {
        "all".to_string()
    } else {
        normalized_source_key_parts(project.iter().map(String::as_str)).join("+")
    }
}

fn normalized_agent_source_key(agents: &[&str]) -> String {
    let normalized_agents = normalized_source_key_parts(agents.iter().copied());
    let legacy_all_agents =
        normalized_source_key_parts(LEGACY_ALL_WATERMARK_AGENTS.iter().copied());
    if normalized_agents == legacy_all_agents {
        LEGACY_ALL_WATERMARK_KEY.to_string()
    } else {
        normalized_agents.join("+")
    }
}

fn extraction_source_key(agents: &[&str], project: &[String]) -> String {
    let agent_key = normalized_agent_source_key(agents);
    let project_key = normalized_project_source_key(project);
    format!("{agent_key}:{project_key}")
}

fn extraction_source_key_aliases(agents: &[&str], project: &[String]) -> Vec<String> {
    let project_key = normalized_project_source_key(project);
    let mut aliases = Vec::new();
    if normalized_source_key_parts(agents.iter().copied())
        == normalized_source_key_parts(LEGACY_ALL_WATERMARK_AGENTS.iter().copied())
    {
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

/// Default delay (seconds) after emitting a mutation warning, giving the
/// operator a window to Ctrl-C before any filesystem writes start. The
/// delay is configurable via the `AICX_MUTATION_WARN_DELAY_SECONDS` env
/// var so CI / wrappers can shorten it. Set to `0` for no pause.
const MUTATION_WARN_DELAY_SECONDS_DEFAULT: u64 = 3;

/// Emit a non-blocking note before a subcommand starts mutating
/// `~/.aicx/`, then sleep briefly so the operator can Ctrl-C if they
/// invoked the command by accident.
///
/// Wave D Cut D1 (B-P0-03): seven subcommands (`all`, `claude`, `codex`,
/// `store`, `migrate`, `migrate-intent-schema`, `index`) write to the
/// canonical store on bare no-arg invocations. Operators occasionally
/// trigger them by accident (typoed subcommand, muscle-memory from a
/// different repo, etc.). This warning gives a 3-second confirmation
/// window without changing the dry-run-default polarity (that lands in
/// D4 if approved).
///
/// Suppressed entirely when `AICX_NO_MUTATION_WARN=1` is set so shipped
/// scripts (`vc-init`, `vibecrafted-mcp`, `install.sh`, automation) can
/// invoke `aicx` programmatically without the pause.
///
/// The delay (default 3s) is configurable via
/// `AICX_MUTATION_WARN_DELAY_SECONDS`. A value of `0` keeps the warning
/// but skips the sleep entirely.
fn warn_pending_mutation(cmd: &str) {
    if mutation_warn_suppressed() {
        return;
    }
    let delay = mutation_warn_delay_seconds();
    if delay == 0 {
        eprintln!(
            "aicx {cmd}: note: about to write to ~/.aicx/. Pass --dry-run to preview \
             (where supported) or set AICX_NO_MUTATION_WARN=1 to silence this note."
        );
        return;
    }
    eprintln!(
        "aicx {cmd}: note: about to write to ~/.aicx/. Pass --dry-run to preview \
         (where supported) or Ctrl-C within {delay}s to abort. \
         Set AICX_NO_MUTATION_WARN=1 to silence this note."
    );
    std::thread::sleep(std::time::Duration::from_secs(delay));
}

fn mutation_warn_suppressed() -> bool {
    std::env::var("AICX_NO_MUTATION_WARN")
        .map(|value| !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

fn mutation_warn_delay_seconds() -> u64 {
    std::env::var("AICX_MUTATION_WARN_DELAY_SECONDS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .unwrap_or(MUTATION_WARN_DELAY_SECONDS_DEFAULT)
}

fn warn_legacy_subcommand(legacy: &str, replacement: &str) {
    eprintln!("# Note: `aicx {legacy}` is deprecated; use `aicx {replacement}` instead.");
}

fn report_dedup_progress<F>(progress: &mut F, idx: usize, total: usize)
where
    F: FnMut(usize),
{
    const TICK_EVERY: usize = 500;
    let scanned = idx + 1;
    if scanned.is_multiple_of(TICK_EVERY) || scanned == total {
        progress(scanned);
    }
}

/// Per-canonical-repo dedup for the post-segmentation pipeline.
///
/// Each segment carries its own canonical repo identity via
/// `SemanticSegment::project_label()`. We dedup each segment's entries
/// against `seen_hashes` keyed on that label (and `_overlap:{label}` for
/// the cross-agent overlap bucket) instead of the legacy `_global` /
/// `project.join("+")` keys. Cross-repo content collisions therefore no
/// longer falsely dedup.
///
/// Legacy `_global` / `_overlap:_global` buckets in `state.json` are
/// ignored by this path and evicted naturally by `prune_old_hashes`.
fn dedup_segments_per_repo<F>(
    segments: Vec<timeline::SemanticSegment>,
    state: &mut StateManager,
    full_rescan: bool,
    mut progress: F,
) -> Vec<timeline::SemanticSegment>
where
    F: FnMut(usize),
{
    let total_entries: usize = segments.iter().map(|s| s.entries.len()).sum();
    let mut total_scanned: usize = 0;
    let mut out = Vec::with_capacity(segments.len());

    // Cross-segment dedup state for `--full-rescan`: indexed by
    // canonical repo slug so duplicates appearing in different segments
    // of the same repo (e.g. multiple sessions touching the same repo)
    // are deduplicated together, matching the incremental code path that
    // uses `state.is_new(&project_label, ...)` for the same purpose.
    //
    // Before this fix, the HashSets were re-created per segment, so
    // full_rescan saw segment-local dedup only — a regression vs the
    // incremental path that prompted the chatgpt-codex-connector P1
    // review comment on PR #8.
    let mut full_rescan_exact_seen: std::collections::HashMap<
        String,
        std::collections::HashSet<String>,
    > = std::collections::HashMap::new();
    let mut full_rescan_overlap_seen: std::collections::HashMap<
        String,
        std::collections::HashSet<String>,
    > = std::collections::HashMap::new();

    for seg in segments {
        let project_label = seg.project_label();
        let overlap_project = format!("_overlap:{project_label}");
        let timeline::SemanticSegment {
            repo,
            source_tier,
            kind,
            agent,
            session_id,
            entries,
        } = seg;

        let mut kept = Vec::with_capacity(entries.len());
        // Borrow a per-repo dedup bucket out of the run-wide maps so the
        // inner loop can `.insert(...)` against persistent state for
        // every segment in the same canonical repo.
        let exact_seen_this_run = full_rescan_exact_seen
            .entry(project_label.clone())
            .or_default();
        let overlap_seen_this_run = full_rescan_overlap_seen
            .entry(overlap_project.clone())
            .or_default();

        for entry in entries {
            total_scanned += 1;

            let exact = StateManager::content_hash(
                &entry.agent,
                entry.timestamp.timestamp(),
                &entry.message,
            );
            if full_rescan {
                if !exact_seen_this_run.insert(exact.clone()) {
                    report_dedup_progress(&mut progress, total_scanned - 1, total_entries);
                    continue;
                }
            } else if !state.is_new(&project_label, &exact) {
                report_dedup_progress(&mut progress, total_scanned - 1, total_entries);
                continue;
            }

            let overlap = StateManager::overlap_hash(entry.timestamp.timestamp(), &entry.message);
            if full_rescan {
                if !overlap_seen_this_run.insert(overlap.clone()) {
                    report_dedup_progress(&mut progress, total_scanned - 1, total_entries);
                    continue;
                }
            } else if !state.is_new(&overlap_project, &overlap) {
                report_dedup_progress(&mut progress, total_scanned - 1, total_entries);
                continue;
            }

            if !full_rescan {
                state.mark_seen(&project_label, exact);
                state.mark_seen(&overlap_project, overlap);
            }
            kept.push(entry);
            report_dedup_progress(&mut progress, total_scanned - 1, total_entries);
        }

        if !kept.is_empty() {
            out.push(timeline::SemanticSegment {
                repo,
                source_tier,
                kind,
                agent,
                session_id,
                entries: kept,
            });
        }
    }

    progress(total_entries);
    out
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

fn all_time_cutoff() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(0, 0).expect("Unix epoch timestamp is valid")
}

/// Convert a lookback window (in hours) to a UTC cutoff timestamp.
///
/// Canonical time-window helper for every CLI/MCP path that asks "what is the
/// cutoff for `--hours N`?". One function, one set of semantics.
///
/// - `hours == 0` → [`all_time_cutoff`] (operator convention: 0 means all time).
/// - `hours > 0`  → `Utc::now() - hours`, with the hour count clamped to
///   `[1, i32::MAX]` (~245k years) so a wildly large `u64` cannot silently wrap
///   `as i64` to a negative value and place the cutoff in the future.
fn lookback_cutoff(hours: u64) -> DateTime<Utc> {
    if hours == 0 {
        return all_time_cutoff();
    }
    const MAX_SAFE_HOURS: i64 = i32::MAX as i64;
    let hours_i64 = i64::try_from(hours)
        .unwrap_or(MAX_SAFE_HOURS)
        .clamp(1, MAX_SAFE_HOURS);
    Utc::now() - chrono::Duration::hours(hours_i64)
}

fn lookback_label(hours: u64) -> String {
    if hours == 0 {
        "all time".to_string()
    } else {
        format!("last {hours} hours")
    }
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

    // Hold the state lock across the full read-modify-write cycle so two
    // concurrent runs cannot clobber each other's watermarks or seen hashes.
    let _state_guard = aicx::locks::acquire_exclusive(aicx::locks::state_lock_path()?)?;

    // Load state for incremental/dedup
    let mut state = StateManager::load()?;

    let cutoff = lookback_cutoff(hours);

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

    let structured_emit = matches!(emit, StdoutEmit::Json);
    let reporter = aicx::progress::select_reporter(structured_emit);
    let failures = aicx::progress::FailureLog::new();

    // ──────────────────────────────────────────────────────────────────
    // Extract phase — same phased UX as `run_store` so `aicx all`,
    // `aicx claude`, `aicx codex` etc. don't stall silently for 15-20
    // minutes during a --full-rescan/-H 0 sweep of agent stores.
    // Heartbeat uses exponential backoff (2s → 60s cap) so long
    // single-agent extracts emit a handful of ticks, not hundreds.
    // ──────────────────────────────────────────────────────────────────
    let extract_phase =
        aicx::progress::Phase::start(reporter.clone(), "extract", Some(agents.len() as u64));
    let mut entries = Vec::new();
    let mut agents_done: u64 = 0;
    for &agent in agents {
        let hb = aicx::progress::Heartbeat::spawn_with_backoff(
            extract_phase.clone(),
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(60),
        );
        let agent_entries_result = match agent {
            "claude" => sources::extract_claude(&config),
            "codex" => sources::extract_codex(&config),
            "gemini" => sources::extract_gemini(&config),
            "junie" => sources::extract_junie(&config),
            "codescribe" => sources::extract_codescribe(&config),
            "operator-md" => sources::extract_operator_markdown(&config),
            _ => Ok(Vec::new()),
        };
        hb.stop();
        let agent_entries = match agent_entries_result {
            Ok(entries) => entries,
            Err(e) => {
                let record =
                    extract_phase.finish_err(&e, aicx::progress::recovery_hint_for("extract"));
                failures.record(record);
                let _ = aicx::progress::render_failure_tail(&failures);
                return Err(e);
            }
        };
        eprintln!("  [{}] {} entries", agent, agent_entries.len());
        entries.extend(agent_entries);
        agents_done += 1;
        extract_phase.tick(agents_done);
    }
    extract_phase.finish_ok(format!(
        "{} agents → {} entries",
        agents.len(),
        entries.len()
    ));

    // Sort by timestamp — done early so the watermark capture and
    // segmentation both see entries in canonical order.
    entries.sort_by_key(|a| a.timestamp);

    // ──────────────────────────────────────────────────────────────────
    // Watermark capture (#19): record the latest raw-extract timestamp
    // BEFORE any filtering. The post-filter survivor list cannot be
    // trusted as a watermark source — dedup or self-echo can drop the
    // tail, leaving a watermark that re-extracts the same tail forever.
    // ──────────────────────────────────────────────────────────────────
    let raw_extract_latest: Option<DateTime<Utc>> = entries.last().map(|e| e.timestamp);

    // ──────────────────────────────────────────────────────────────────
    // Redact phase (#6): redaction must happen BEFORE dedup so the hash
    // domain converges across incremental and --full-rescan/force paths.
    // ──────────────────────────────────────────────────────────────────
    if redact_secrets {
        for e in &mut entries {
            e.message = aicx::redact::redact_secrets(&e.message);
        }
    }

    // ──────────────────────────────────────────────────────────────────
    // Segment phase — moved UP so per-canonical-repo dedup (#8) can key
    // on each segment's repo identity. Progress denominator = entry
    // count so operators see real percentage during long rescans.
    // ──────────────────────────────────────────────────────────────────
    let segment_total = entries.len() as u64;
    let segment_phase =
        aicx::progress::Phase::start(reporter.clone(), "segment", Some(segment_total));
    let segments = {
        let hb = aicx::progress::Heartbeat::spawn_with_backoff(
            segment_phase.clone(),
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(60),
        );
        let result = aicx::segmentation::semantic_segments_with_progress(&entries, |processed| {
            hb.raise_floor(processed as u64)
        });
        hb.stop();
        result
    };
    let pre_dedup: usize = segments.iter().map(|s| s.entries.len()).sum();
    let segment_count_pre = segments.len();
    segment_phase.finish_ok(format!(
        "{} entries → {} segments",
        pre_dedup, segment_count_pre
    ));

    // ──────────────────────────────────────────────────────────────────
    // Dedup phase (#8): per-canonical-repo, skipped entirely when
    // `--force` is set. Otherwise it honors `full_rescan` for intra-run
    // dedup vs cross-run state lookup.
    // ──────────────────────────────────────────────────────────────────
    let segments = if force {
        segments
    } else {
        let dedup_phase =
            aicx::progress::Phase::start(reporter.clone(), "dedup", Some(pre_dedup as u64));
        let deduped = dedup_segments_per_repo(segments, &mut state, full_rescan, |scanned| {
            dedup_phase.tick(scanned as u64)
        });
        let post = deduped.iter().map(|s| s.entries.len()).sum::<usize>();
        let skipped = pre_dedup.saturating_sub(post);
        dedup_phase.finish_ok(format!("kept {post} / {pre_dedup} (skipped {skipped})"));
        if skipped > 0 {
            eprintln!("  Dedup: {pre_dedup} → {post} entries (skipped {skipped} seen)");
        }
        deduped
    };

    // ──────────────────────────────────────────────────────────────────
    // Self-echo phase (per-segment): aicx tool-echo entries get dropped
    // within each segment; empty segments are then dropped wholesale.
    // ──────────────────────────────────────────────────────────────────
    let pre_echo: usize = segments.iter().map(|s| s.entries.len()).sum();
    let echo_phase =
        aicx::progress::Phase::start(reporter.clone(), "self_echo", Some(pre_echo as u64));
    let segments = {
        const ECHO_TICK_EVERY: usize = 500;
        let mut scanned: usize = 0;
        let mut out = Vec::with_capacity(segments.len());
        for mut seg in segments {
            seg.entries.retain(|e| {
                scanned += 1;
                if scanned.is_multiple_of(ECHO_TICK_EVERY) {
                    echo_phase.tick(scanned as u64);
                }
                !aicx::sanitize::is_self_echo(&e.message)
            });
            if !seg.entries.is_empty() {
                out.push(seg);
            }
        }
        echo_phase.tick(scanned as u64);
        out
    };
    let post_echo: usize = segments.iter().map(|s| s.entries.len()).sum();
    let echo_filtered = pre_echo.saturating_sub(post_echo);
    echo_phase.finish_ok(format!(
        "kept {post_echo} / {pre_echo} (filtered {echo_filtered})"
    ));
    if echo_filtered > 0 {
        eprintln!("  Filtered {echo_filtered} self-echo entries");
    }

    // Reassemble a flat, timestamp-ordered Vec for downstream formatters
    // (sessions list, markdown/json reports, conversation projection).
    // We clone — segments still own the canonical entries for the
    // store_segments_at call below.
    let mut output_entries: Vec<timeline::TimelineEntry> = segments
        .iter()
        .flat_map(|s| s.entries.iter().cloned())
        .collect();
    output_entries.sort_by_key(|e| e.timestamp);

    let mut sessions: Vec<String> = output_entries
        .iter()
        .map(|e| e.session_id.clone())
        .collect();
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
        sessions,
    };

    let chunker_config = aicx::chunker::ChunkerConfig::default();
    let mut all_written_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut written_empty_body_skipped = 0usize;
    let mut scope_surface = StoreScopeSurface::empty(&project);

    if !output_entries.is_empty() {
        // Chunk phase — segments were prepared upstream (per-canonical-repo
        // dedup + self_echo). Denominator is segments so `current/total`
        // reflects actual write progress.
        let segment_count = segments.len();
        let chunk_phase =
            aicx::progress::Phase::start(reporter.clone(), "chunk", Some(segment_count as u64));
        let store_result = store::store_segments_at(
            &aicx::store::store_base_dir()?,
            &segments,
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
            let projection = sources::to_conversation_with_stats(&output_entries, &project);
            let extract_stats = output::ConversationExtractStats {
                aicx_version: env!("CARGO_PKG_VERSION"),
                redaction_enabled: redact_secrets,
                raw_entries: output_entries.len(),
                conversation_messages: projection.messages.len(),
                conversation_projection: "user_assistant_only",
                exact_short_duplicates_dropped: projection.exact_short_duplicates_dropped,
                harness_noise_dropped: projection.harness_noise_dropped,
            };
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
                output::write_conversation_markdown_with_redaction(
                    &md_path,
                    &projection.messages,
                    &metadata,
                    false,
                )?;
            }
            if out_format == OutputFormat::Json || out_format == OutputFormat::Both {
                let json_path =
                    local_dir.join(format!("{}_conversation_{}.json", prefix, date_str));
                output::write_conversation_json_with_redaction(
                    &json_path,
                    &projection.messages,
                    &metadata,
                    &extract_stats,
                    false,
                )?;
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

    // ──────────────────────────────────────────────────────────────────
    // Watermark write (#19): advance from `raw_extract_latest` captured
    // BEFORE filtering, not from the post-filter survivor list. This
    // closes the self-echo-tail re-extract loop.
    // ──────────────────────────────────────────────────────────────────
    if let Some(latest) = raw_extract_latest {
        state.update_watermark(&source_key, latest);
    }

    // For --force (dedup bypassed) and --full-rescan (dedup uses
    // intra-run only), mark surviving entries under their canonical
    // repo slug so future incremental runs honor what just landed.
    // Incremental (!force, !full_rescan) runs already marked via
    // dedup_segments_per_repo during the filter pass.
    if force || full_rescan {
        for seg in &segments {
            let project_label = seg.project_label();
            let overlap_project = format!("_overlap:{project_label}");
            for e in &seg.entries {
                let exact =
                    StateManager::content_hash(&e.agent, e.timestamp.timestamp(), &e.message);
                let overlap = StateManager::overlap_hash(e.timestamp.timestamp(), &e.message);
                state.mark_seen(&project_label, exact);
                state.mark_seen(&overlap_project, overlap);
            }
        }
    }

    state.record_run(
        output_entries.len(),
        agents.iter().map(|s| s.to_string()).collect(),
    );
    state.prune_old_hashes(50_000);
    state.save()?;

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

    let cutoff = cutoff.unwrap_or_else(|| lookback_cutoff(hours));

    let agents = resolve_store_agents(agent.as_deref())?;

    // Hold the state lock across the full read-modify-write cycle so two
    // concurrent store runs cannot clobber each other's state.
    let _state_guard = aicx::locks::acquire_exclusive(aicx::locks::state_lock_path()?)?;

    let mut state = StateManager::load()?;
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

    let structured_emit = matches!(emit, StdoutEmit::Json);
    let reporter = aicx::progress::select_reporter(structured_emit);
    let failures = aicx::progress::FailureLog::new();

    // ──────────────────────────────────────────────────────────────────
    // Extract phase
    //
    // Each agent's extractor is opaque from the outside (it walks
    // `~/.claude/projects/`, `~/.codex/`, etc. on its own), so we wrap
    // each call in a heartbeat so the operator still sees the spinner
    // and elapsed-time advance during a long `--full-rescan -H 0` run.
    // Per-agent ticks raise the heartbeat floor so the final tick value
    // reflects accumulated entries, not just heartbeat counts.
    // ──────────────────────────────────────────────────────────────────
    let extract_phase =
        aicx::progress::Phase::start(reporter.clone(), "extract", Some(agents.len() as u64));
    let mut all_entries = Vec::new();
    let mut agents_done: u64 = 0;
    for &ag in &agents {
        // Backoff so a long single-agent extract (e.g. ~/.claude/projects/
        // walking thousands of JSONL files) doesn't flood the structured
        // log with one tick every 2s; first few ticks fire fast so the
        // operator sees the spinner come alive, then settle to a 60s cap.
        let hb = aicx::progress::Heartbeat::spawn_with_backoff(
            extract_phase.clone(),
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(60),
        );
        let agent_entries_result = match ag {
            "claude" => sources::extract_claude(&config),
            "codex" => sources::extract_codex(&config),
            "gemini" => sources::extract_gemini(&config),
            "junie" => sources::extract_junie(&config),
            "codescribe" => sources::extract_codescribe(&config),
            "operator-md" => sources::extract_operator_markdown(&config),
            _ => Ok(Vec::new()),
        };
        hb.stop();
        let agent_entries = match agent_entries_result {
            Ok(entries) => entries,
            Err(e) => {
                let record =
                    extract_phase.finish_err(&e, aicx::progress::recovery_hint_for("extract"));
                failures.record(record);
                let _ = aicx::progress::render_failure_tail(&failures);
                return Err(e);
            }
        };
        eprintln!("  [{}] {} entries", ag, agent_entries.len());
        all_entries.extend(agent_entries);
        agents_done += 1;
        extract_phase.tick(agents_done);
    }
    extract_phase.finish_ok(format!(
        "{} agents → {} entries",
        agents.len(),
        all_entries.len()
    ));

    all_entries.sort_by_key(|a| a.timestamp);

    // ──────────────────────────────────────────────────────────────────
    // Watermark capture (#19): record the latest raw-extract timestamp
    // BEFORE any filtering. The post-filter `all_entries.last()` is not
    // a safe watermark source — self-echo or dedup can drop the tail,
    // leaving a watermark that re-extracts the same tail every run.
    // ──────────────────────────────────────────────────────────────────
    let raw_extract_latest: Option<DateTime<Utc>> = all_entries.last().map(|e| e.timestamp);

    // ──────────────────────────────────────────────────────────────────
    // Redact phase (#6): redaction must happen BEFORE dedup so the hash
    // domain converges across incremental and --full-rescan paths. The
    // legacy ordering hashed pre-redact in incremental and post-redact
    // in full_rescan, producing two competing seen_hashes universes.
    // ──────────────────────────────────────────────────────────────────
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

    // ──────────────────────────────────────────────────────────────────
    // Segment phase (moved UP, ahead of dedup): per-canonical-repo dedup
    // (#8) needs the canonical repo identity from each segment, so
    // segmentation runs first. Progress denominator = entry count so
    // operators see real percentage during long rescans (pass-4 UX
    // follow-up to D-2-cluster).
    // ──────────────────────────────────────────────────────────────────
    let segment_total = all_entries.len() as u64;
    let segment_phase =
        aicx::progress::Phase::start(reporter.clone(), "segment", Some(segment_total));
    let segments = {
        let hb = aicx::progress::Heartbeat::spawn_with_backoff(
            segment_phase.clone(),
            std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(60),
        );
        let result =
            aicx::segmentation::semantic_segments_with_progress(&all_entries, |processed| {
                hb.raise_floor(processed as u64)
            });
        hb.stop();
        result
    };
    let segment_count_pre = segments.len();
    segment_phase.finish_ok(format!(
        "{} entries → {} segments",
        all_entries.len(),
        segment_count_pre
    ));

    // ──────────────────────────────────────────────────────────────────
    // Dedup phase (#8): keyed on per-segment canonical repo slug rather
    // than `_global` / `project.join("+")`. Cross-repo content
    // collisions no longer falsely dedup. Legacy buckets in state.json
    // stay as stale and are evicted by prune_old_hashes over time.
    // ──────────────────────────────────────────────────────────────────
    let pre_dedup: usize = segments.iter().map(|s| s.entries.len()).sum();
    let dedup_phase =
        aicx::progress::Phase::start(reporter.clone(), "dedup", Some(pre_dedup as u64));
    let segments = dedup_segments_per_repo(segments, &mut state, full_rescan, |scanned| {
        dedup_phase.tick(scanned as u64)
    });
    let post_dedup: usize = segments.iter().map(|s| s.entries.len()).sum();
    let dedup_skipped = pre_dedup.saturating_sub(post_dedup);
    dedup_phase.finish_ok(format!(
        "kept {post_dedup} / {pre_dedup} (skipped {dedup_skipped})"
    ));
    if dedup_skipped > 0 {
        eprintln!("  Dedup: {pre_dedup} → {post_dedup} entries (skipped {dedup_skipped} seen)");
    }

    // ──────────────────────────────────────────────────────────────────
    // Self-echo phase (per-segment): drop aicx tool-echo entries within
    // each segment, then drop segments that emptied.
    // ──────────────────────────────────────────────────────────────────
    let pre_echo = post_dedup;
    let echo_phase =
        aicx::progress::Phase::start(reporter.clone(), "self_echo", Some(pre_echo as u64));
    let segments = {
        const ECHO_TICK_EVERY: usize = 500;
        let mut scanned: usize = 0;
        let mut out = Vec::with_capacity(segments.len());
        for mut seg in segments {
            seg.entries.retain(|e| {
                scanned += 1;
                if scanned.is_multiple_of(ECHO_TICK_EVERY) {
                    echo_phase.tick(scanned as u64);
                }
                !aicx::sanitize::is_self_echo(&e.message)
            });
            if !seg.entries.is_empty() {
                out.push(seg);
            }
        }
        echo_phase.tick(scanned as u64);
        out
    };
    let post_echo: usize = segments.iter().map(|s| s.entries.len()).sum();
    let echo_filtered = pre_echo.saturating_sub(post_echo);
    echo_phase.finish_ok(format!(
        "kept {post_echo} / {pre_echo} (filtered {echo_filtered})"
    ));
    if echo_filtered > 0 {
        eprintln!("  Filtered {echo_filtered} self-echo entries");
    }

    let mut stored_count = 0;
    let mut all_written_paths = Vec::new();
    let mut scope_surface = StoreScopeSurface::empty(&project);
    let mut skipped_empty_body = 0;
    let mut deduped_chunks = 0;

    if post_echo == 0 {
        eprintln!("No entries found.");
    } else {
        // ──────────────────────────────────────────────────────────────
        // Chunk phase — denominator is segments (not entries), so the
        // `current/total` ratio reflects actual write progress.
        // ──────────────────────────────────────────────────────────────
        let segment_count = segments.len();
        let chunk_phase =
            aicx::progress::Phase::start(reporter.clone(), "chunk", Some(segment_count as u64));
        let store_result = store::store_segments_at(
            &aicx::store::store_base_dir()?,
            &segments,
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

        stored_count = store_summary.total_entries;
        all_written_paths = store_summary.written_paths.clone();
        scope_surface = StoreScopeSurface::from_store_summary(&project, &store_summary);
        skipped_empty_body = store_summary.skipped_empty_body;
        deduped_chunks = store_summary.deduped_chunks;

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
        if store_summary.deduped_chunks > 0 {
            eprintln!(
                "  Deduped {} content-identical chunk(s)",
                store_summary.deduped_chunks
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
    }

    // ──────────────────────────────────────────────────────────────────
    // Watermark write (#19): advance from `raw_extract_latest` captured
    // BEFORE filtering, not from the post-filter survivor list. This
    // closes the self-echo-tail re-extract loop.
    // ──────────────────────────────────────────────────────────────────
    if let Some(latest) = raw_extract_latest {
        state.update_watermark(&source_key, latest);
    }

    // For --full-rescan, dedup_segments_per_repo skips persistent
    // mark_seen during the run; we mark surviving entries now so future
    // incremental runs honor what just landed.
    if full_rescan {
        for seg in &segments {
            let project_label = seg.project_label();
            let overlap_project = format!("_overlap:{project_label}");
            for e in &seg.entries {
                let exact =
                    StateManager::content_hash(&e.agent, e.timestamp.timestamp(), &e.message);
                let overlap = StateManager::overlap_hash(e.timestamp.timestamp(), &e.message);
                state.mark_seen(&project_label, exact);
                state.mark_seen(&overlap_project, overlap);
            }
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
                    "written_empty_body_skipped": skipped_empty_body,
                    "deduped_chunks": deduped_chunks,
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

fn project_scopes(projects: &[String]) -> Vec<Option<&str>> {
    if projects.is_empty() {
        vec![None]
    } else {
        projects.iter().map(String::as_str).map(Some).collect()
    }
}

/// Canonical project resolver shared by `aicx index` and `aicx index
/// status`. Routes raw user `-p` filters through
/// `resolve_project_filters_or_error` (and ultimately
/// `aicx::store::project_filter_matches`) so both commands canonicalize
/// the same filter the same way and therefore agree on the resulting
/// bucket set. Without this single chokepoint, `aicx index status -p X`
/// could compute a bucket like `_codescribe` that `aicx index -p X`
/// never built (bug #36).
///
/// `None` represents the `_all` cross-project bucket; `Some(slug)` is a
/// canonical `<owner>/<repo>` slug exactly as `index` builds buckets for.
fn resolve_index_scopes(projects: &[String]) -> Result<Vec<Option<String>>> {
    let resolved = resolve_project_filters_or_error(projects)?;
    Ok(if resolved.is_empty() {
        vec![None]
    } else {
        resolved.into_iter().map(Some).collect()
    })
}

/// Resolve user `-p` filters into canonical `<owner>/<repo>` slugs by
/// enumerating the on-disk store. Empty input → empty output (caller treats
/// it as "all projects"). Non-empty input that matches zero projects returns
/// an error with the user-visible filter list, so search/index never silently
/// resolve to `_all` after a typo.
fn resolve_project_filters_or_error(projects: &[String]) -> Result<Vec<String>> {
    if projects.is_empty() {
        return Ok(Vec::new());
    }
    let resolved = aicx::store::resolve_filters_to_slugs_or_error(projects)?;
    // Warn (don't fail) when a bare-name filter matched both as an
    // organization AND as a repository — operator likely wanted one or the
    // other. Filter still resolves to the union; this is just a heads-up.
    for filter in projects {
        if let Some((as_org, as_repo)) =
            aicx::store::detect_ambiguous_bare_filter(filter, &resolved)
        {
            let trimmed = filter.trim();
            let org_example = as_org.first().cloned().unwrap_or_default();
            let repo_example = as_repo.first().cloned().unwrap_or_default();
            eprintln!(
                "warning: filter '{trimmed}' matched as both an organization AND a repository name.\n  \
                 as org    -> {trimmed}/* (e.g. {org_example})\n  \
                 as repo   -> {repo_example}\n  \
                 use -p {trimmed}/ for org-only or -p /{trimmed} for repo-only."
            );
        }
    }
    Ok(resolved)
}

fn project_scope_label(projects: &[String]) -> String {
    if projects.is_empty() {
        "all projects".to_string()
    } else {
        projects.join(", ")
    }
}

/// Semantic-first retrieval across the canonical store. Fails fast when
/// semantic preconditions are missing unless `--no-semantic` is explicit.
struct SearchRunArgs<'a> {
    query: &'a str,
    projects: &'a [String],
    hours: u64,
    date: Option<&'a str>,
    json: bool,
    filters: RetrievalFilters,
    kind: Option<&'a str>,
    no_semantic: bool,
}

fn validate_cli_search_limit(limit: usize) -> Result<()> {
    if limit > MAX_CLI_SEARCH_LIMIT {
        anyhow::bail!(
            "search --limit {limit} exceeds the hard cap of {MAX_CLI_SEARCH_LIMIT}; \
             narrow the query/filter or run multiple smaller searches"
        );
    }
    Ok(())
}

fn search_examined_fetch_limit(user_limit: usize, filters_active: bool) -> usize {
    if filters_active {
        user_limit
            .saturating_mul(aicx::search_engine::FILTER_EXAMINED_CAP_RATIO)
            .max(aicx::search_engine::FILTER_EXAMINED_CAP_MIN)
    } else {
        user_limit
    }
}

fn run_search(args: SearchRunArgs<'_>) -> Result<()> {
    let SearchRunArgs {
        query,
        projects,
        hours,
        date,
        json,
        filters,
        kind,
        no_semantic,
    } = args;
    let limit = filters.limit.unwrap_or(DEFAULT_RETRIEVAL_LIMIT);
    validate_cli_search_limit(limit)?;
    let kind_filter = kind.and_then(aicx::timeline::Kind::parse);
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

    // Build the canonical filter pushdown for the retrieval primitive.
    // The explicit date filter wins over `--hours`, matching legacy
    // precedence preserved by the wrapper.
    let (date_lo, date_hi) = if let Some(ref d) = effective_date {
        parse_date_filter(d)?
    } else {
        (filters.since.clone(), filters.until.clone())
    };
    let hours_cutoff = if hours > 0 && date_lo.is_none() && date_hi.is_none() {
        Some(lookback_cutoff(hours).format("%Y-%m-%d").to_string())
    } else {
        None
    };
    let post_filters = aicx::search_engine::SemanticSearchFilters {
        agent: filters.agent.clone(),
        score_min: filters.score,
        date_lo: date_lo.clone(),
        date_hi: date_hi.clone(),
        hours_cutoff: hours_cutoff.clone(),
    };

    let resolved_projects = resolve_project_filters_or_error(projects)?;
    let scopes = project_scopes(&resolved_projects);

    let (mut results, scanned, semantic_status, pushdown_diagnostic) = if no_semantic {
        // Fuzzy path keeps the legacy "fetch then post-filter" shape —
        // `rank::fuzzy_search_store` is not on the hybrid retrieval
        // primitive and is operator-requested explicitly via
        // `--no-semantic`, so we leave it alone.
        let fuzzy_fetch_limit = search_examined_fetch_limit(limit, post_filters.is_active());
        let (mut results, scanned) = rank::fuzzy_search_store(
            &root,
            &search_query,
            fuzzy_fetch_limit,
            &scopes,
            filters.frame_kind.map(Into::into),
        )?;
        if let Some(min_score) = post_filters.score_min {
            results.retain(|r| r.score >= min_score);
        }
        if let Some(ref agent_filter) = post_filters.agent {
            results.retain(|r| r.agent == *agent_filter);
        }
        if post_filters.date_lo.is_some() || post_filters.date_hi.is_some() {
            let lo = post_filters.date_lo.as_deref();
            let hi = post_filters.date_hi.as_deref();
            results.retain(|r| {
                lo.is_none_or(|lo| r.date.as_str() >= lo)
                    && hi.is_none_or(|hi| r.date.as_str() <= hi)
            });
        } else if let Some(ref cutoff) = post_filters.hours_cutoff {
            let cutoff = cutoff.as_str();
            results.retain(|r| r.date.as_str() >= cutoff);
        }
        (results, scanned, None, None)
    } else {
        match aicx::search_engine::try_semantic_search_filtered(
            &root,
            &search_query,
            limit,
            &scopes,
            filters.frame_kind.map(Into::into),
            kind_filter.map(|kind| kind.dir_name()),
            &post_filters,
        ) {
            Ok(filtered) => {
                let aicx::search_engine::FilteredSemanticOutcome {
                    outcome,
                    diagnostic,
                } = filtered;
                let status = (
                    outcome.backend_label,
                    outcome.model_id.clone(),
                    outcome.scanned,
                    outcome.retrieval_status.clone(),
                );
                (outcome.results, outcome.scanned, Some(status), diagnostic)
            }
            Err(err) => {
                let payload = serde_json::json!({
                    "ok": false,
                    "error": "semantic_search_unavailable",
                    "kind": err.kind(),
                    "reason": err.reason(),
                    "recommendation": err.recommendation(),
                    "fallback": {
                        "available": true,
                        "command": format!("aicx search --no-semantic {:?}", query),
                    },
                });
                if json {
                    println!("{}", serde_json::to_string_pretty(&payload)?);
                } else {
                    eprintln!("aicx search: semantic search unavailable.");
                    eprintln!("  kind:           {}", err.kind());
                    eprintln!("  reason:         {}", err.reason());
                    eprintln!("  recommendation: {}", err.recommendation());
                    eprintln!("  fallback:       aicx search --no-semantic {:?}", query);
                }
                std::process::exit(2);
            }
        }
    };

    // Defensive kind retain: the semantic path pushes `kind_filter`
    // into the hybrid query, but we keep the explicit check so a future
    // index regression cannot smuggle off-kind hits past the operator.
    if let Some(kind_filter) = kind_filter {
        results.retain(|r| r.kind == kind_filter.dir_name());
    }

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
    let results: Vec<_> = results.into_iter().take(limit).collect();

    if json {
        let oracle_status = match semantic_status {
            Some((
                _semantic_backend,
                _semantic_model_id,
                _semantic_scanned,
                Some(ref retrieval_status),
            )) => aicx::oracle::OracleStatus::hybrid_rrf(
                &root,
                retrieval_status,
                results.len(),
                aicx::oracle::verify_paths(
                    results
                        .iter()
                        .map(|result| std::path::Path::new(&result.path).to_path_buf()),
                ),
            ),
            Some((_semantic_backend, _semantic_model_id, semantic_scanned, None)) => {
                aicx::oracle::OracleStatus::content_semantic(
                    &root,
                    semantic_scanned,
                    results.len(),
                    aicx::oracle::verify_paths(
                        results
                            .iter()
                            .map(|result| std::path::Path::new(&result.path).to_path_buf()),
                    ),
                )
            }
            None => rank::search_oracle_status(&root, &results, scanned),
        };
        let rendered =
            rank::render_search_json_with_oracle(&root, &results, scanned, oracle_status)?;
        let payload = aicx::search_engine::inject_filter_pushdown_diagnostic(
            &rendered,
            pushdown_diagnostic.as_ref(),
        )?;
        println!("{}", payload);
        return Ok(());
    }

    if results.is_empty() {
        eprintln!("No matches for {:?} (scanned {} chunks).", query, scanned);
        if let Some(ref diag) = pushdown_diagnostic {
            eprintln!(
                "  filter_pushdown: kind={} examined={} matched={} requested_limit={} cap_ratio={}x",
                diag.kind,
                diag.examined,
                diag.matched,
                diag.requested_limit,
                diag.examined_cap_ratio
            );
            eprintln!(
                "  hint: examined the bounded retrieval cap; widen the filter \
                 or rebuild the index if the corpus is expected to satisfy it."
            );
        }
        return Ok(());
    }

    print!(
        "{}",
        rank::render_search_text(&results, io::stdout().is_terminal())
    );
    let _ = io::stdout().flush();

    if io::stderr().is_terminal() {
        let base_line = match semantic_status {
            Some((semantic_backend, semantic_model_id, semantic_scanned, retrieval_status)) => {
                aicx::search_engine::render_semantic_status_line(
                    semantic_backend,
                    &semantic_model_id,
                    results.len(),
                    semantic_scanned,
                    retrieval_status.as_ref(),
                )
            }
            None => format!(
                "{} result(s) from {} scanned chunks. oracle_status: backend=filesystem_fuzzy index=none fallback=operator_requested loctree_scope_safe=false",
                results.len(),
                scanned
            ),
        };
        let suffix = pushdown_diagnostic
            .as_ref()
            .map(|d| {
                format!(
                    " filter_pushdown={} examined={} matched={} requested_limit={}",
                    d.kind, d.examined, d.matched, d.requested_limit
                )
            })
            .unwrap_or_default();
        eprintln!("\n{}{}", base_line, suffix);
    }
    Ok(())
}

/// Build the `IndexEvent` -> sink fanout used by `aicx index`. Always
/// includes a tracing adapter (for log capture / non-TTY runs); adds an
/// `IndicatifSink` with live ETA + rate when stderr is an interactive
/// terminal. Translates `IndexEvent` variants into `ProgressUpdate`s that
/// drive the progress bar position, length, message, and final-state.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn build_index_event_fanout(
    interactive: bool,
) -> std::sync::Arc<aicx::progress::FanOut<aicx_progress_contracts::IndexEvent>> {
    use aicx::progress::{FanOut, IndicatifSink, ProgressUpdate, TracingSink};
    use aicx_progress_contracts::IndexEvent;

    let render = |event: &IndexEvent| -> Option<ProgressUpdate> {
        match event {
            IndexEvent::RunStarted { total_items, .. } => Some(ProgressUpdate {
                position: 0,
                length: Some(*total_items as u64),
                message: Some("embedding chunks".to_string()),
                finished: false,
            }),
            IndexEvent::StatsTick {
                processed,
                total,
                items_per_sec,
                eta_secs,
                failed,
                ..
            } => {
                let eta_label = match eta_secs {
                    Some(secs) if *secs >= 60.0 => {
                        let mins = (secs / 60.0).floor();
                        let rem = secs - mins * 60.0;
                        format!("ETA {mins:.0}m{rem:02.0}s")
                    }
                    Some(secs) => format!("ETA {secs:.0}s"),
                    None => "ETA …".to_string(),
                };
                let err_suffix = if *failed > 0 {
                    format!(" · {failed} failed")
                } else {
                    String::new()
                };
                Some(ProgressUpdate {
                    position: *processed as u64,
                    length: Some(*total as u64),
                    message: Some(format!("{items_per_sec:.1}/s · {eta_label}{err_suffix}")),
                    finished: false,
                })
            }
            IndexEvent::RunCompleted {
                processed,
                indexed,
                failed,
                elapsed,
                ..
            } => Some(ProgressUpdate {
                position: *processed as u64,
                length: Some(*processed as u64),
                message: Some(format!(
                    "done · {indexed} indexed · {failed} failed · {:.1}s",
                    elapsed.as_secs_f64()
                )),
                finished: true,
            }),
            _ => None,
        }
    };

    let mut fan = FanOut::<IndexEvent>::new();
    fan.push(std::sync::Arc::new(IndicatifSink::new(
        0,
        interactive,
        render,
    )));
    fan.push(std::sync::Arc::new(TracingSink));
    std::sync::Arc::new(fan)
}

fn write_index_for_current_build(
    scope: Option<&str>,
    sample: usize,
    interactive: bool,
    full_rescan: bool,
) -> Result<aicx::vector_index::IndexStats> {
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    {
        let fan = build_index_event_fanout(interactive);
        let fan_for_closure = std::sync::Arc::clone(&fan);
        let on_event = move |event: &aicx_progress_contracts::IndexEvent| {
            use aicx::progress::EventSink;
            fan_for_closure.on_event(event);
        };
        let options = aicx::vector_index::IndexBuildOptions { full_rescan };
        aicx::vector_index::write_index_with_options(scope, sample, options, &on_event)
    }

    #[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
    {
        let _ = (scope, sample, interactive, full_rescan);
        anyhow::bail!(
            "aicx index requires a semantic embedder backend; rebuild with \
             --features native-embedder or --features cloud-embedder, or use \
             `aicx index --dry-run` to inspect corpus/index readiness without embedding"
        );
    }
}

/// Build (or preview) the vector index. `dry_run=true` probes the
/// embedder + samples chunks for ETA. `dry_run=false` writes a
/// persistent NDJSON-backed index (Iter 3) that subsequent `aicx search`
/// queries against via cosine similarity.
fn run_index(
    projects: &[String],
    sample: usize,
    json: bool,
    dry_run: bool,
    full_rescan: bool,
) -> Result<()> {
    let resolved_scopes = resolve_index_scopes(projects)?;
    let scopes: Vec<Option<&str>> = resolved_scopes.iter().map(Option::as_deref).collect();

    let interactive = std::io::IsTerminal::is_terminal(&std::io::stderr()) && !json;

    // G-3: announce embedder backend class so the operator can predict perf.
    // Cloud HTTP (~2.5s/req) vs native GGUF (~50ms/req on M-series) matter
    // for ETA expectations; suppressed in --json mode so machine readers
    // get a clean payload.
    if !json {
        #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
        if let Some(label) = aicx::vector_index::probe_backend_label() {
            eprintln!("Backend: {}", label);
        }
    }

    let mut reports = Vec::with_capacity(scopes.len());
    for scope in scopes {
        let stats = if dry_run {
            let _lock = aicx::locks::acquire_exclusive(aicx::locks::lance_lock_path()?)?;
            aicx::vector_index::dry_run_index(scope, sample)?
        } else {
            write_index_for_current_build(scope, sample, interactive, full_rescan)?
        };
        reports.push((scope.map(ToString::to_string), stats));
    }

    if json {
        if reports.len() == 1 {
            println!("{}", aicx::vector_index::render_stats_json(&reports[0].1)?);
        } else {
            let payload = reports
                .iter()
                .map(|(project, stats)| {
                    serde_json::json!({
                        "project": project.as_deref().unwrap_or("_all"),
                        "stats": stats,
                    })
                })
                .collect::<Vec<_>>();
            println!("{}", serde_json::to_string(&payload)?);
        }
    } else {
        for (idx, (project, stats)) in reports.iter().enumerate() {
            if reports.len() > 1 {
                if idx > 0 {
                    eprintln!();
                }
                eprintln!(
                    "scope: {}",
                    project
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .unwrap_or("_all")
                );
            }
            eprint!("{}", aicx::vector_index::render_stats_text(stats));
            if let Some(path) = &stats.index_path {
                eprintln!("\n  index_path:          {}", path.display());
            }
        }
    }
    Ok(())
}

fn run_index_status(projects: &[String], json: bool) -> Result<()> {
    let resolved_scopes = resolve_index_scopes(projects)?;
    let client = aicx::Aicx::from_env()?;

    let mut reports: Vec<(Option<String>, aicx::IndexStatus)> =
        Vec::with_capacity(resolved_scopes.len());
    for scope in &resolved_scopes {
        let status = client.index_status(scope.as_deref())?;
        reports.push((scope.clone(), status));
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&index_status_json_payload(&reports))?
        );
    } else {
        for (idx, (scope, status)) in reports.iter().enumerate() {
            if reports.len() > 1 {
                if idx > 0 {
                    eprintln!();
                }
                eprintln!(
                    "scope: {}",
                    scope
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .unwrap_or("_all")
                );
            }
            print_index_status_text(status);
        }
    }
    Ok(())
}

fn index_status_json_payload(reports: &[(Option<String>, aicx::IndexStatus)]) -> serde_json::Value {
    serde_json::Value::Array(
        reports
            .iter()
            .map(|(scope, status)| {
                serde_json::json!({
                    "project": scope
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .unwrap_or("_all"),
                    "status": status,
                })
            })
            .collect(),
    )
}

fn print_index_status_text(status: &aicx::IndexStatus) {
    eprintln!("aicx index status");
    eprintln!(
        "  readiness:              {}",
        match status.readiness {
            aicx::IndexReadiness::Ready => "ready",
            aicx::IndexReadiness::Pending => "pending (only temp checkpoint)",
            aicx::IndexReadiness::Missing => "missing",
        }
    );
    eprintln!("  backend:                {}", status.backend);
    eprintln!("  project_bucket:         {}", status.project_bucket);
    eprintln!("  canonical_chunks:       {}", status.canonical_chunks);
    eprintln!(
        "  semantic_index_present: {}",
        status.semantic_index_present
    );
    eprintln!(
        "  semantic_index_path:    {}",
        status.semantic_index_path.as_deref().unwrap_or("<none>")
    );
    eprintln!("  semantic_index_rows:    {}", status.semantic_index_rows);
    eprintln!(
        "  committed_at:           {}",
        status.committed_at.as_deref().unwrap_or("<none>")
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
    eprintln!("  temp_index_present:     {}", status.temp_index_present);
    eprintln!(
        "  temp_index_path:        {}",
        status.temp_index_path.as_deref().unwrap_or("<none>")
    );
    eprintln!("  temp_index_rows:        {}", status.temp_index_rows);
    eprintln!(
        "  temp_index_mtime:       {}",
        status.temp_index_mtime.as_deref().unwrap_or("<none>")
    );
    eprintln!(
        "  temp_index_bytes:       {}",
        status
            .temp_index_bytes
            .map(|value| value.to_string())
            .unwrap_or_else(|| "<none>".to_string())
    );
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
    projects: &[String],
    date: Option<&str>,
    json: bool,
    filters: RetrievalFilters,
) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let limit = filters.limit.unwrap_or(DEFAULT_RETRIEVAL_LIMIT);

    let effective_date = date;
    let (date_lo, date_hi) = if let Some(d) = effective_date {
        let bounds = parse_date_filter(d)?;
        (bounds.0, bounds.1)
    } else {
        (filters.since.clone(), filters.until.clone())
    };

    let frame_kind = filters.frame_kind.map(Into::into);
    let scopes = project_scopes(projects);
    let mut metadatas = Vec::new();
    for project in scopes {
        let filter = aicx::steer_index::SteerFilter {
            run_id,
            prompt_id,
            agent: filters.agent.as_deref(),
            kind,
            frame_kind,
            project,
            date_lo: date_lo.as_deref(),
            date_hi: date_hi.as_deref(),
        };
        let mut batch = rt.block_on(aicx::steer_index::search_steer_index(&filter, limit))?;
        metadatas.append(&mut batch);
    }
    dedup_steer_metadata(&mut metadatas);

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
    metadatas.truncate(limit);

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

fn dedup_steer_metadata(metadatas: &mut Vec<serde_json::Value>) {
    let mut seen = BTreeSet::new();
    metadatas.retain(|meta| {
        let key = meta
            .get("path")
            .or_else(|| meta.get("source_chunk"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| meta.to_string());
        seen.insert(key)
    });
}

fn refs_cutoff(hours: u64) -> std::time::SystemTime {
    if hours == 0 {
        std::time::UNIX_EPOCH
    } else {
        std::time::SystemTime::now() - std::time::Duration::from_secs(hours.saturating_mul(3600))
    }
}

/// List chunks in the canonical store, filtered by recency.
fn run_refs(hours: u64, project: Option<String>, emit: RefsEmit, strict: bool) -> Result<()> {
    let cutoff = refs_cutoff(hours);
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
    let _state_guard = aicx::locks::acquire_exclusive(aicx::locks::state_lock_path()?)?;
    let mut state = StateManager::load()?;

    if info {
        // B-P1-13: honor `--project` filter on --info as well as on --reset.
        // When set, narrow the seen-hash listing (and totals) to buckets that
        // match the filter via the canonical project_filter_matches resolver
        // (`<owner>/<repo>` strict, `<owner>/` org wildcard, `/<repo>` repo
        // wildcard, bare `name` cross-org). Watermarks and runs are global
        // and remain unfiltered.
        let filter = project.as_deref().map(str::trim).filter(|s| !s.is_empty());
        eprintln!("=== State Info ===");
        if let Some(f) = filter {
            eprintln!("Filtered by project: {}", f);
        }
        if let Some(f) = filter {
            let matched: Vec<(&String, &aicx::state::SeenHashSet)> = state
                .seen_hashes
                .iter()
                .filter(|(bucket, _)| state_bucket_matches_project_filter(bucket, f))
                .collect();
            let total: usize = matched.iter().map(|(_, set)| set.len()).sum();
            eprintln!("  Total hashes (filtered): {}", total);
            eprintln!("  Projects (filtered):     {}", matched.len());
            for (proj, set) in &matched {
                eprintln!("    {}: {} hashes", proj, set.len());
            }
        } else {
            eprintln!("  Total hashes: {}", state.total_hashes());
            eprintln!("  Projects: {}", state.seen_hashes.len());
            for (proj, set) in &state.seen_hashes {
                eprintln!("    {}: {} hashes", proj, set.len());
            }
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

/// Apply the canonical `project_filter_matches` resolver to a state-store
/// bucket key. Buckets are stored as lowercase `<owner>/<repo>` (see
/// `aicx::state::migration::canonical_state_bucket`); buckets that don't
/// split into exactly two segments are matched against the bare filter
/// only (cross-org name match) and never against the slug or org-wildcard
/// shapes.
fn state_bucket_matches_project_filter(bucket: &str, filter: &str) -> bool {
    let mut parts = bucket.splitn(2, '/');
    match (parts.next(), parts.next()) {
        (Some(org), Some(repo)) if !org.is_empty() && !repo.is_empty() => {
            store::project_filter_matches(org, repo, filter)
        }
        _ => {
            // Legacy / non-slug bucket: treat the whole key as the repo
            // side so a bare `-p <name>` still works.
            store::project_filter_matches("", bucket, filter)
        }
    }
}

struct DashboardServerRunArgs {
    store_root: Option<PathBuf>,
    scope: DashboardScope,
    host: String,
    port: u16,
    no_open: bool,
    bg: bool,
    allow_cors_origins: Option<String>,
    auth_token: Option<String>,
    require_auth: bool,
    allow_no_origin: bool,
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
    let auth_config = aicx::auth::load_auth_config(args.auth_token.as_deref(), args.require_auth)?;
    dashboard_server::validate_dashboard_host_policy(
        host,
        &cors_policy,
        args.allow_cors_origins.is_some(),
        &auth_config,
    )?;
    let artifact_path = args.artifact;

    if args.bg {
        return spawn_dashboard_server_background(DashboardServerBackgroundArgs {
            store_root: root,
            scope: args.scope,
            host,
            port: args.port,
            title: &args.title,
            preview_chars: args.preview_chars,
            allow_cors_origins: args.allow_cors_origins.as_deref(),
            auth_token: args.auth_token.as_deref(),
            require_auth: args.require_auth,
            allow_no_origin: args.allow_no_origin,
        });
    }

    if !host.is_loopback() {
        eprintln!(
            "! Warning: dashboard server is binding beyond loopback on http://{}:{}",
            host, args.port
        );
        eprintln!("  CORS policy: {}", cors_policy.label());
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
        auth: auth_config,
        allow_no_origin: args.allow_no_origin,
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

struct DashboardServerBackgroundArgs<'a> {
    store_root: PathBuf,
    scope: DashboardScope,
    host: std::net::IpAddr,
    port: u16,
    title: &'a str,
    preview_chars: usize,
    allow_cors_origins: Option<&'a str>,
    auth_token: Option<&'a str>,
    require_auth: bool,
    allow_no_origin: bool,
}

fn spawn_dashboard_server_background(args: DashboardServerBackgroundArgs<'_>) -> Result<()> {
    let current_exe = std::env::current_exe().context("Resolve current aicx executable")?;
    let mut command = std::process::Command::new(&current_exe);
    command
        .arg("dashboard")
        .arg("--serve")
        .arg("--no-open")
        .arg("--host")
        .arg(args.host.to_string())
        .arg("--port")
        .arg(args.port.to_string())
        .arg("--store-root")
        .arg(args.store_root.as_os_str());

    if let Some(project) = args.scope.project.as_deref() {
        command.arg("--project").arg(project);
    }
    if let Some(hours) = args.scope.hours {
        command.arg("--hours").arg(hours.to_string());
    }
    if let Some(policy) = args.allow_cors_origins {
        command.arg("--allow-cors-origins").arg(policy);
    }
    if let Some(token) = args.auth_token {
        command.arg("--auth-token").arg(token);
    }
    command
        .arg("--require-auth")
        .arg(if args.require_auth { "true" } else { "false" });
    if args.allow_no_origin {
        command.arg("--allow-no-origin");
    }
    if args.title != DEFAULT_DASHBOARD_TITLE {
        command.arg("--title").arg(args.title);
    }
    if args.preview_chars != 320 {
        command
            .arg("--preview-chars")
            .arg(args.preview_chars.to_string());
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
    eprintln!("  URL: http://{}:{}", args.host, args.port);
    eprintln!("  Store: {}", args.store_root.display());
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
            auth_token: args.auth_token,
            require_auth: args.require_auth,
            allow_no_origin: args.allow_no_origin,
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
        || args.auth_token.is_some()
    {
        return Err(anyhow::anyhow!(
            "--host, --port, --no-open, --bg, --allow-cors-origins, and --auth-token are only valid with --serve."
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
    force: bool,
    deterministic: bool,
}

fn default_reports_output_path() -> Result<PathBuf> {
    Ok(store::store_base_dir()?.join("aicx-reports.html"))
}

fn run_reports_command(args: ReportsArgs) -> Result<()> {
    // Env var hook keeps CI/scripts reproducible without rewiring CLI flags.
    let deterministic = args.deterministic
        || matches!(
            std::env::var("AICX_REPORTS_DETERMINISTIC")
                .ok()
                .as_deref()
                .map(str::trim),
            Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
        );
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
        force: args.force,
        deterministic,
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
        sources::infer_repo_name_from_current_dir()?
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
        deterministic: args.deterministic,
    };

    let artifact = reports_extractor::build_reports_explorer(&config)?;
    write_text_output(
        &args.output,
        &artifact.html,
        "report explorer HTML",
        args.force,
    )?;
    write_text_output(
        &bundle_output,
        &artifact.bundle_json,
        "report explorer JSON bundle",
        args.force,
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

fn write_text_output(path: &Path, content: &str, label: &str, force: bool) -> Result<()> {
    let mut validated = aicx::sanitize::validate_write_path(path)?;
    if let Some(parent) = validated.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output directory: {}", parent.display()))?;
    }
    validated = aicx::sanitize::validate_write_path(&validated)?;
    if !force && validated.exists() {
        return Err(anyhow::anyhow!(
            "Refusing to overwrite existing {label} at {}: pass --force to replace it.",
            validated.display()
        ));
    }
    fs::write(&validated, content)
        .with_context(|| format!("Failed to write {}: {}", label, validated.display()))
}

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
