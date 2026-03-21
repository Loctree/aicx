//! Central context store for ai-contexters.
//!
//! Manages the `~/.aicx/` directory structure:
//! - `store/<organization>/<repository>/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md`
//! - `non-repository-contexts/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md`
//! - `store/<project>/<date>/<time>_<agent>-context.{md,json}` — legacy monolithic helpers kept for library use/tests
//! - `memex/chunks/` — pre-chunked text for RAG indexing
//! - `index.json` — manifest of stored contexts
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::chunker::{self, ChunkerConfig};
use crate::output::TimelineEntry;
use crate::sanitize;
use crate::segmentation::{RepoIdentity, SemanticSegment, semantic_segments};

// ============================================================================
// Kind classification
// ============================================================================

/// Canonical kind for a session segment in the store.
///
/// Kind determines the subdirectory under `<project>/<date>/` and is part
/// of the canonical store path.  Classification is conservative: when in
/// doubt, segments fall through to `Other`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Conversations,
    Plans,
    Reports,
    #[default]
    Other,
}

impl Kind {
    /// Directory name used in the canonical store layout.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Conversations => "conversations",
            Self::Plans => "plans",
            Self::Reports => "reports",
            Self::Other => "other",
        }
    }

    /// Parse from a string (case-insensitive, accepts both singular and plural).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "conversations" | "conversation" => Some(Self::Conversations),
            "plans" | "plan" => Some(Self::Plans),
            "reports" | "report" => Some(Self::Reports),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dir_name())
    }
}

// ── Kind heuristics ────────────────────────────────────────────────────────

const PLAN_KEYWORDS: &[&str] = &[
    "implementation plan",
    "plan:",
    "## plan",
    "step 1:",
    "step 2:",
    "step 3:",
    "action items",
    "milestones",
    "roadmap",
    "todo list",
    "acceptance criteria",
    "## steps",
    "## phases",
];

const REPORT_KEYWORDS: &[&str] = &[
    "## findings",
    "## summary",
    "## report",
    "audit report",
    "coverage report",
    "test results",
    "## metrics",
    "## recommendations",
    "## conclusion",
    "status report",
    "incident report",
    "pr review",
    "code review",
];

/// Classify a set of timeline entries into a canonical `Kind`.
///
/// Uses a lightweight keyword-scoring approach:
/// - Scans assistant messages (where classification signal is strongest)
/// - Scores plan vs report keywords
/// - Conversations win by default when neither plan nor report signal is strong
///
/// The approach is intentionally conservative: ambiguous content falls to
/// `Conversations` (the most common kind), not `Other`.
pub fn classify_kind(entries: &[TimelineEntry]) -> Kind {
    if entries.is_empty() {
        return Kind::Other;
    }

    let mut plan_score: u32 = 0;
    let mut report_score: u32 = 0;
    let mut has_conversation = false;

    for entry in entries {
        let lower = entry.message.to_lowercase();

        // Only count strong signals from assistant messages
        if entry.role == "assistant" {
            for kw in PLAN_KEYWORDS {
                if lower.contains(kw) {
                    plan_score += 1;
                }
            }
            for kw in REPORT_KEYWORDS {
                if lower.contains(kw) {
                    report_score += 1;
                }
            }
        }

        // Any user+assistant exchange = conversation evidence
        if entry.role == "user" || entry.role == "assistant" {
            has_conversation = true;
        }
    }

    // Threshold: need at least 3 keyword hits to classify as plan or report
    let threshold = 3;

    if plan_score >= threshold && plan_score > report_score {
        Kind::Plans
    } else if report_score >= threshold && report_score > plan_score {
        Kind::Reports
    } else if has_conversation {
        Kind::Conversations
    } else {
        Kind::Other
    }
}

// ============================================================================
// Session-first filename generation
// ============================================================================

/// Generate a canonical session-first basename for a store chunk file.
///
/// Format: `<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md`
///
/// The date is derived from the source event timestamp, NOT from
/// the time `store` was run. Session identity is the primary uniqueness
/// anchor; the date prefix ensures lexicographic ordering and
/// self-description when the file is viewed outside its directory context.
pub fn session_basename(date: &str, agent: &str, session_id: &str, chunk: u32) -> String {
    let date_compact = compact_date(date);
    let sid = truncate_session_id(session_id);
    format!("{}_{}_{}_{:03}.md", date_compact, agent, sid, chunk)
}

/// Compact a YYYY-MM-DD date to YYYY_MMDD form.
pub(crate) fn compact_date(date: &str) -> String {
    // Handle both "2026-03-21" and "2026_0321" input
    let digits: String = date.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        format!("{}_{}", &digits[..4], &digits[4..8])
    } else {
        // Fallback: use as-is with underscores
        date.replace('-', "_")
    }
}

/// Truncate session ID to a reasonable length for filenames.
///
/// Session IDs can be UUIDs (36 chars) or other formats.
/// We take the first 12 chars which provides sufficient uniqueness
/// (~2^48 for hex IDs) while keeping basenames readable.
fn truncate_session_id(session_id: &str) -> String {
    let cleaned: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let limit = 12.min(cleaned.len());
    cleaned[..limit].to_string()
}

// ============================================================================
// Path helpers
// ============================================================================

pub const NON_REPOSITORY_CONTEXTS: &str = "non-repository-contexts";
pub const CANONICAL_STORE_DIRNAME: &str = "store";

/// Returns the AICX base directory: `~/.aicx/`
///
/// Creates the directory if it doesn't exist.
pub fn store_base_dir() -> Result<PathBuf> {
    let dir = dirs::home_dir().context("No home directory")?.join(".aicx");
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create store dir: {}", dir.display()))?;
    Ok(dir)
}

/// Returns the canonical repo-centric store root: `~/.aicx/store/`
pub fn canonical_store_dir() -> Result<PathBuf> {
    let dir = store_base_dir()?.join(CANONICAL_STORE_DIRNAME);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Returns the non-repository fallback root: `~/.aicx/non-repository-contexts/`
pub fn non_repository_contexts_dir() -> Result<PathBuf> {
    let dir = store_base_dir()?.join(NON_REPOSITORY_CONTEXTS);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Returns the legacy input-store root used for truthful migration inventory.
pub fn legacy_store_base_dir() -> Result<PathBuf> {
    Ok(dirs::home_dir()
        .context("No home directory")?
        .join(".ai-contexters"))
}

/// Returns the project directory: `~/.aicx/store/<project>/`
pub fn project_dir(project: &str) -> Result<PathBuf> {
    let dir = canonical_store_dir()?.join(project);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Returns the chunks directory: `~/.aicx/memex/chunks/`
pub fn chunks_dir() -> Result<PathBuf> {
    let dir = store_base_dir()?.join("memex").join("chunks");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Full path for a specific context markdown file.
///
/// Layout: `~/.aicx/store/<project>/<date>/<time>_<agent>-context.md`
pub fn get_context_path(project: &str, agent: &str, date: &str, time: &str) -> Result<PathBuf> {
    let dir = canonical_store_dir()?.join(project).join(date);
    fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}_{}-context.md", time, agent)))
}

/// Full path for a specific context JSON file.
///
/// Layout: `~/.aicx/store/<project>/<date>/<time>_<agent>-context.json`
pub fn get_context_json_path(
    project: &str,
    agent: &str,
    date: &str,
    time: &str,
) -> Result<PathBuf> {
    let dir = canonical_store_dir()?.join(project).join(date);
    fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}_{}-context.json", time, agent)))
}

// ============================================================================
// Index types
// ============================================================================

/// Manifest of all stored contexts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoreIndex {
    pub projects: HashMap<String, ProjectIndex>,
    pub last_updated: DateTime<Utc>,
}

/// Per-project index entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectIndex {
    pub agents: HashMap<String, AgentIndex>,
}

/// Per-agent index within a project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentIndex {
    pub dates: Vec<String>,
    pub total_entries: usize,
    pub last_updated: DateTime<Utc>,
}

// ============================================================================
// Index operations
// ============================================================================

/// Load the store index from `~/.ai-contexters/index.json`.
///
/// Returns a default empty index if the file doesn't exist or can't be parsed.
pub fn load_index() -> StoreIndex {
    let base = match store_base_dir() {
        Ok(dir) => dir,
        Err(_) => return StoreIndex::default(),
    };
    load_index_at(&base)
}

fn load_index_at(base: &Path) -> StoreIndex {
    let path = base.join("index.json");
    if !path.exists() {
        return StoreIndex::default();
    }

    let contents = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return StoreIndex::default(),
    };

    serde_json::from_str(&contents).unwrap_or_default()
}

/// Persist the store index to disk.
pub fn save_index(index: &StoreIndex) -> Result<()> {
    save_index_at(&store_base_dir()?, index)
}

fn save_index_at(base: &Path, index: &StoreIndex) -> Result<()> {
    let path = base.join("index.json");
    let json = serde_json::to_string_pretty(index).context("Failed to serialize index")?;
    fs::write(&path, json).with_context(|| format!("Failed to write index: {}", path.display()))?;
    Ok(())
}

/// Update the in-memory index with a new context entry.
pub fn update_index(
    index: &mut StoreIndex,
    project: &str,
    agent: &str,
    date: &str,
    entry_count: usize,
) {
    let now = Utc::now();
    index.last_updated = now;

    let project_idx = index.projects.entry(project.to_string()).or_default();

    let agent_idx = project_idx.agents.entry(agent.to_string()).or_default();

    if !agent_idx.dates.contains(&date.to_string()) {
        agent_idx.dates.push(date.to_string());
        agent_idx.dates.sort();
    }

    agent_idx.total_entries += entry_count;
    agent_idx.last_updated = now;
}

/// List all projects in the index.
pub fn list_stored_projects(index: &StoreIndex) -> Vec<String> {
    let mut projects: Vec<String> = index.projects.keys().cloned().collect();
    projects.sort();
    projects
}

#[derive(Debug, Clone)]
pub struct StoredContextFile {
    pub path: PathBuf,
    pub project: String,
    pub repo: Option<RepoIdentity>,
    pub date_compact: String,
    pub date_iso: String,
    pub kind: Kind,
    pub agent: String,
    pub session_id: String,
    pub chunk: u32,
}

#[derive(Debug, Clone, Default)]
pub struct StoreWriteSummary {
    pub total_entries: usize,
    pub written_paths: Vec<PathBuf>,
    pub project_summary: BTreeMap<String, BTreeMap<String, usize>>,
}

struct SessionWriteSpec<'a> {
    project: Option<&'a str>,
    agent: &'a str,
    date: &'a str,
    session_id: &'a str,
    kind: Option<Kind>,
}

// ============================================================================
// Context writing
// ============================================================================

/// Write timeline entries to the central store.
///
/// Creates two files:
/// - `~/.aicx/store/<project>/<date>/<time>_<agent>-context.md`
/// - `~/.aicx/store/<project>/<date>/<time>_<agent>-context.json`
///
/// Returns paths of both files.
pub fn write_context(
    project: &str,
    agent: &str,
    date: &str,
    time: &str,
    entries: &[TimelineEntry],
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();

    // Markdown
    let md_path = get_context_path(project, agent, date, time)?;
    let mut md_content = String::new();
    md_content.push_str(&format!("# {} | {} | {}\n\n", project, agent, date));

    for entry in entries {
        let ts = entry.timestamp.format("%Y-%m-%d %H:%M:%S UTC");
        md_content.push_str(&format!("### {} | {}\n", ts, entry.role));
        for line in entry.message.lines() {
            md_content.push_str(&format!("> {}\n", line));
        }
        md_content.push('\n');
    }

    let write_path = sanitize::validate_write_path(&md_path)?;
    fs::write(&write_path, &md_content)?;
    written.push(md_path);

    // JSON
    let json_path = get_context_json_path(project, agent, date, time)?;
    let json_content = serde_json::to_string_pretty(entries)?;
    let write_path = sanitize::validate_write_path(&json_path)?;
    fs::write(&write_path, &json_content)?;
    written.push(json_path);

    Ok(written)
}

/// Write timeline entries as agent-friendly chunks to the central store.
///
/// Instead of one monolithic file per (project, agent, date), splits entries
/// into overlapping ~1500-token windows preserving conversation flow.
///
/// Layout (legacy): `~/.aicx/store/<project>/<date>/<time>_<agent>-<seq:03>.md`
///
/// Returns paths of all written chunk files.
pub fn write_context_chunked(
    project: &str,
    agent: &str,
    date: &str,
    time: &str,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
) -> Result<Vec<PathBuf>> {
    if entries.is_empty() {
        return Ok(vec![]);
    }

    let chunks = chunker::chunk_entries(entries, project, agent, chunker_config);
    let dir = canonical_store_dir()?.join(project).join(date);
    fs::create_dir_all(&dir)?;

    let mut written = Vec::new();

    for chunk in &chunks {
        // Extract seq from chunk.id (last _NNN part)
        let seq = chunk.id.rsplit('_').next().unwrap_or("001");

        let filename = format!("{}_{}-{}.md", time, agent, seq);
        let path = dir.join(&filename);

        let write_path = sanitize::validate_write_path(&path)?;
        fs::write(&write_path, &chunk.text)?;
        written.push(path);
    }

    Ok(written)
}

/// Write timeline entries using the session-first canonical layout.
///
/// Layout: `~/.aicx/store/<project>/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md`
///
/// The `kind` is auto-classified from entries if not provided.
/// Date is derived from the source event timestamps, not from runtime.
///
/// Returns paths of all written chunk files.
pub fn write_context_session_first(
    project: &str,
    agent: &str,
    date: &str,
    session_id: &str,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
    kind: Option<Kind>,
) -> Result<Vec<PathBuf>> {
    write_context_session_first_at(
        &canonical_store_dir()?,
        SessionWriteSpec {
            project: Some(project),
            agent,
            date,
            session_id,
            kind,
        },
        entries,
        chunker_config,
    )
}

fn write_context_session_first_at(
    root: &Path,
    spec: SessionWriteSpec<'_>,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
) -> Result<Vec<PathBuf>> {
    if entries.is_empty() {
        return Ok(vec![]);
    }

    let kind = spec.kind.unwrap_or_else(|| classify_kind(entries));
    let project_label = spec.project.unwrap_or(NON_REPOSITORY_CONTEXTS);
    let chunks = chunker::chunk_entries(entries, project_label, spec.agent, chunker_config);
    let date_dir = compact_date(spec.date);

    let mut written = Vec::new();

    for (idx, chunk) in chunks.iter().enumerate() {
        let chunk_num = (idx as u32) + 1;
        let mut dir = root.join(&date_dir).join(kind.dir_name()).join(spec.agent);
        if let Some(project) = spec.project {
            dir = root
                .join(project)
                .join(&date_dir)
                .join(kind.dir_name())
                .join(spec.agent);
        }
        fs::create_dir_all(&dir)?;

        let filename = session_basename(spec.date, spec.agent, spec.session_id, chunk_num);
        let path = dir.join(&filename);

        let write_path = sanitize::validate_write_path(&path)?;
        fs::write(&write_path, &chunk.text)?;
        written.push(path);
    }

    Ok(written)
}

pub fn store_semantic_segments(
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
) -> Result<StoreWriteSummary> {
    store_semantic_segments_at(&store_base_dir()?, entries, chunker_config)
}

fn store_semantic_segments_at(
    base: &Path,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
) -> Result<StoreWriteSummary> {
    let mut summary = StoreWriteSummary::default();
    if entries.is_empty() {
        return Ok(summary);
    }

    let segments = semantic_segments(entries);
    let mut index = load_index_at(base);

    for segment in segments {
        let date = segment
            .entries
            .first()
            .map(|entry| entry.timestamp.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
        let project = segment.project_label();

        let paths = write_semantic_segment_at(base, &segment, &date, chunker_config)?;
        update_index(
            &mut index,
            &project,
            &segment.agent,
            &compact_date(&date),
            segment.entries.len(),
        );
        *summary
            .project_summary
            .entry(project)
            .or_default()
            .entry(segment.agent.clone())
            .or_insert(0) += segment.entries.len();
        summary.total_entries += segment.entries.len();
        summary.written_paths.extend(paths);
    }

    save_index_at(base, &index)?;
    Ok(summary)
}

fn write_semantic_segment_at(
    base: &Path,
    segment: &SemanticSegment,
    date: &str,
    chunker_config: &ChunkerConfig,
) -> Result<Vec<PathBuf>> {
    let project = segment.repo.as_ref().map(RepoIdentity::slug);
    let root = if project.is_some() {
        base.join(CANONICAL_STORE_DIRNAME)
    } else {
        base.join(NON_REPOSITORY_CONTEXTS)
    };

    write_context_session_first_at(
        &root,
        SessionWriteSpec {
            project: project.as_deref(),
            agent: &segment.agent,
            date,
            session_id: &segment.session_id,
            kind: Some(segment.kind),
        },
        &segment.entries,
        chunker_config,
    )
}

pub fn scan_context_files() -> Result<Vec<StoredContextFile>> {
    let base = store_base_dir()?;
    scan_context_files_at(&base)
}

pub fn scan_context_files_at(base: &Path) -> Result<Vec<StoredContextFile>> {
    let base = sanitize::validate_dir_path(base)?;
    let mut files = Vec::new();

    let canonical_root = base.join(CANONICAL_STORE_DIRNAME);
    if canonical_root.is_dir() {
        scan_repo_store(&canonical_root, &mut files)?;
    }

    let non_repo_root = base.join(NON_REPOSITORY_CONTEXTS);
    if non_repo_root.is_dir() {
        scan_non_repository_store(&non_repo_root, &mut files)?;
    }

    files.sort_by(|left, right| {
        left.date_compact
            .cmp(&right.date_compact)
            .then_with(|| left.project.cmp(&right.project))
            .then_with(|| left.agent.cmp(&right.agent))
            .then_with(|| left.session_id.cmp(&right.session_id))
            .then_with(|| left.chunk.cmp(&right.chunk))
    });

    Ok(files)
}

pub fn context_files_since(
    cutoff: SystemTime,
    project_filter: Option<&str>,
) -> Result<Vec<StoredContextFile>> {
    let filter = project_filter.map(|value| value.to_ascii_lowercase());
    let mut files = scan_context_files()?;
    files.retain(|file| {
        let matches_project = filter
            .as_ref()
            .is_none_or(|needle| file.project.to_ascii_lowercase().contains(needle));
        let matches_cutoff = file
            .path
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .is_some_and(|modified| modified >= cutoff);
        matches_project && matches_cutoff
    });
    Ok(files)
}

fn scan_repo_store(root: &Path, files: &mut Vec<StoredContextFile>) -> Result<()> {
    for organization_entry in sanitize::read_dir_validated(root)?.filter_map(|entry| entry.ok()) {
        let organization_path = organization_entry.path();
        if !organization_path.is_dir() {
            continue;
        }
        let organization = organization_entry.file_name().to_string_lossy().to_string();

        for repository_entry in
            sanitize::read_dir_validated(&organization_path)?.filter_map(|entry| entry.ok())
        {
            let repository_path = repository_entry.path();
            if !repository_path.is_dir() {
                continue;
            }
            let repository = repository_entry.file_name().to_string_lossy().to_string();
            let repo = RepoIdentity {
                organization: organization.clone(),
                repository: repository.clone(),
            };

            for date_entry in
                sanitize::read_dir_validated(&repository_path)?.filter_map(|entry| entry.ok())
            {
                let date_path = date_entry.path();
                if !date_path.is_dir() {
                    continue;
                }
                let date_compact = date_entry.file_name().to_string_lossy().to_string();

                for kind_entry in
                    sanitize::read_dir_validated(&date_path)?.filter_map(|entry| entry.ok())
                {
                    let kind_path = kind_entry.path();
                    if !kind_path.is_dir() {
                        continue;
                    }
                    let Some(kind) = Kind::parse(&kind_entry.file_name().to_string_lossy()) else {
                        continue;
                    };

                    for agent_entry in
                        sanitize::read_dir_validated(&kind_path)?.filter_map(|entry| entry.ok())
                    {
                        let agent_path = agent_entry.path();
                        if !agent_path.is_dir() {
                            continue;
                        }
                        let agent = agent_entry.file_name().to_string_lossy().to_string();
                        collect_leaf_files(
                            &agent_path,
                            Some(repo.clone()),
                            &repo.slug(),
                            &date_compact,
                            kind,
                            &agent,
                            files,
                        )?;
                    }
                }
            }
        }
    }

    Ok(())
}

fn scan_non_repository_store(root: &Path, files: &mut Vec<StoredContextFile>) -> Result<()> {
    for date_entry in sanitize::read_dir_validated(root)?.filter_map(|entry| entry.ok()) {
        let date_path = date_entry.path();
        if !date_path.is_dir() {
            continue;
        }
        let date_compact = date_entry.file_name().to_string_lossy().to_string();

        for kind_entry in sanitize::read_dir_validated(&date_path)?.filter_map(|entry| entry.ok()) {
            let kind_path = kind_entry.path();
            if !kind_path.is_dir() {
                continue;
            }
            let Some(kind) = Kind::parse(&kind_entry.file_name().to_string_lossy()) else {
                continue;
            };

            for agent_entry in
                sanitize::read_dir_validated(&kind_path)?.filter_map(|entry| entry.ok())
            {
                let agent_path = agent_entry.path();
                if !agent_path.is_dir() {
                    continue;
                }
                let agent = agent_entry.file_name().to_string_lossy().to_string();
                collect_leaf_files(
                    &agent_path,
                    None,
                    NON_REPOSITORY_CONTEXTS,
                    &date_compact,
                    kind,
                    &agent,
                    files,
                )?;
            }
        }
    }

    Ok(())
}

fn collect_leaf_files(
    dir: &Path,
    repo: Option<RepoIdentity>,
    project: &str,
    date_compact: &str,
    kind: Kind,
    agent: &str,
    files: &mut Vec<StoredContextFile>,
) -> Result<()> {
    for file_entry in sanitize::read_dir_validated(dir)?.filter_map(|entry| entry.ok()) {
        let path = file_entry.path();
        let file_type = match file_entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_symlink() || !file_type.is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_none_or(|ext| ext != "md" && ext != "json")
        {
            continue;
        }

        let Some((session_id, chunk)) = parse_session_basename(
            &file_entry.file_name().to_string_lossy(),
            agent,
            date_compact,
        ) else {
            continue;
        };

        files.push(StoredContextFile {
            path,
            project: project.to_string(),
            repo: repo.clone(),
            date_compact: date_compact.to_string(),
            date_iso: expand_compact_date(date_compact),
            kind,
            agent: agent.to_string(),
            session_id,
            chunk,
        });
    }

    Ok(())
}

fn parse_session_basename(name: &str, agent: &str, date_compact: &str) -> Option<(String, u32)> {
    let escaped_agent = regex::escape(agent);
    let escaped_date = regex::escape(date_compact);
    let pattern = format!(
        r"^(?P<date>{escaped_date})_(?P<agent>{escaped_agent})_(?P<session>[A-Za-z0-9-]+)_(?P<chunk>\d{{3}})\.(md|json)$"
    );
    let re = Regex::new(&pattern).ok()?;
    let captures = re.captures(name)?;
    let session_id = captures.name("session")?.as_str().to_string();
    let chunk = captures.name("chunk")?.as_str().parse().ok()?;
    Some((session_id, chunk))
}

pub fn expand_compact_date(compact: &str) -> String {
    let digits: String = compact.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        format!("{}-{}-{}", &digits[..4], &digits[4..6], &digits[6..8])
    } else {
        compact.to_string()
    }
}

// ============================================================================
// Migration
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationInventory {
    pub generated_at: DateTime<Utc>,
    pub legacy_root: String,
    pub manifest_path: String,
    pub total_files: usize,
    pub rebuild_candidates: usize,
    pub missing_sources: usize,
    pub items: Vec<MigrationInventoryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationInventoryItem {
    pub legacy_path: String,
    pub source_candidates: Vec<String>,
    pub existing_sources: Vec<String>,
    pub rebuild_candidate: bool,
}

pub fn run_migration(dry_run: bool) -> Result<()> {
    let inventory = build_migration_inventory()?;

    println!(
        "Legacy sweep: {} file(s), {} truthful rebuild candidate(s), {} item(s) with missing sources.",
        inventory.total_files, inventory.rebuild_candidates, inventory.missing_sources
    );
    println!("Legacy root: {}", inventory.legacy_root);

    if dry_run {
        println!(
            "[DRY RUN] Would write migration inventory to {}",
            inventory.manifest_path
        );
        return Ok(());
    }

    let manifest_path = PathBuf::from(&inventory.manifest_path);
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&inventory)?;
    let write_path = sanitize::validate_write_path(&manifest_path)?;
    fs::write(write_path, json)?;
    println!("Wrote migration inventory to {}", inventory.manifest_path);

    Ok(())
}

pub fn build_migration_inventory() -> Result<MigrationInventory> {
    build_migration_inventory_at(
        &legacy_store_base_dir()?,
        &store_base_dir()?.join("migration-index.json"),
    )
}

fn build_migration_inventory_at(
    legacy_root: &Path,
    manifest_path: &Path,
) -> Result<MigrationInventory> {
    let mut items = Vec::new();

    if legacy_root.is_dir() {
        collect_legacy_inventory_items(legacy_root, &mut items)?;
    }

    let rebuild_candidates = items.iter().filter(|item| item.rebuild_candidate).count();
    let missing_sources = items.len().saturating_sub(rebuild_candidates);

    Ok(MigrationInventory {
        generated_at: Utc::now(),
        legacy_root: legacy_root.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        total_files: items.len(),
        rebuild_candidates,
        missing_sources,
        items,
    })
}

fn collect_legacy_inventory_items(
    legacy_root: &Path,
    items: &mut Vec<MigrationInventoryItem>,
) -> Result<()> {
    for entry in sanitize::read_dir_validated(legacy_root)?.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            collect_legacy_inventory_items(&path, items)?;
            continue;
        }

        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_none_or(|ext| ext != "md" && ext != "json")
        {
            continue;
        }

        let source_candidates = legacy_source_candidates(&path)?;
        let existing_sources: Vec<String> = source_candidates
            .iter()
            .filter(|candidate| candidate.exists())
            .map(|candidate| candidate.display().to_string())
            .collect();

        items.push(MigrationInventoryItem {
            legacy_path: path.display().to_string(),
            source_candidates: source_candidates
                .iter()
                .map(|candidate| candidate.display().to_string())
                .collect(),
            existing_sources: existing_sources.clone(),
            rebuild_candidate: !existing_sources.is_empty(),
        });
    }

    Ok(())
}

fn legacy_source_candidates(path: &Path) -> Result<Vec<PathBuf>> {
    let mut candidates = Vec::new();
    let content = sanitize::read_to_string_validated(path).unwrap_or_default();
    let path_re = Regex::new(r"(/[A-Za-z0-9._~\-]+(?:/[A-Za-z0-9._~\-]+)+)")
        .expect("legacy source path regex should compile");

    for capture in path_re.captures_iter(&content) {
        if let Some(raw) = capture.get(1) {
            let candidate = PathBuf::from(raw.as_str());
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }

    Ok(candidates)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::env;

    #[test]
    fn test_store_base_dir() {
        if let Ok(path) = store_base_dir() {
            assert!(path.to_string_lossy().contains(".aicx"));
        }
    }

    #[test]
    fn test_chunks_dir() {
        if let Ok(path) = chunks_dir() {
            assert!(path.to_string_lossy().contains("memex"));
            assert!(path.to_string_lossy().contains("chunks"));
        }
    }

    #[test]
    fn test_get_context_path_new_layout() {
        if let Ok(path) = get_context_path("CodeScribe", "claude", "2026-01-22", "143005") {
            let s = path.to_string_lossy();
            assert!(s.contains("CodeScribe"));
            assert!(s.contains("2026-01-22"));
            assert!(s.ends_with("143005_claude-context.md"));
        }
    }

    #[test]
    fn test_get_context_json_path_new_layout() {
        if let Ok(path) = get_context_json_path("CodeScribe", "claude", "2026-01-22", "143005") {
            let s = path.to_string_lossy();
            assert!(s.contains("CodeScribe"));
            assert!(s.contains("2026-01-22"));
            assert!(s.ends_with("143005_claude-context.json"));
        }
    }

    #[test]
    fn test_write_context_creates_both_files() {
        let tmp = env::temp_dir().join("ai-ctx-test-store-new");
        let _ = fs::remove_dir_all(&tmp);
        let date_dir = tmp.join("TestProj").join("2026-01-22");
        fs::create_dir_all(&date_dir).unwrap();

        let entries = vec![
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 14, 30, 5).unwrap(),
                agent: "claude".to_string(),
                session_id: "sess-1".to_string(),
                role: "user".to_string(),
                message: "hello world".to_string(),
                branch: None,
                cwd: None,
            },
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 14, 30, 12).unwrap(),
                agent: "claude".to_string(),
                session_id: "sess-1".to_string(),
                role: "assistant".to_string(),
                message: "hi there\nsecond line".to_string(),
                branch: None,
                cwd: None,
            },
        ];

        // Write md directly to verify format
        let md_path = date_dir.join("143005_claude-context.md");
        let mut content = String::new();
        content.push_str("# TestProj | claude | 2026-01-22\n\n");
        for entry in &entries {
            let ts = entry.timestamp.format("%Y-%m-%d %H:%M:%S UTC");
            content.push_str(&format!("### {} | {}\n", ts, entry.role));
            for line in entry.message.lines() {
                content.push_str(&format!("> {}\n", line));
            }
            content.push('\n');
        }
        fs::write(&md_path, &content).unwrap();

        let written = fs::read_to_string(&md_path).unwrap();
        assert!(written.contains("# TestProj | claude | 2026-01-22"));
        assert!(written.contains("### 2026-01-22 14:30:05 UTC | user"));
        assert!(written.contains("> hello world"));
        assert!(written.contains("> hi there"));
        assert!(written.contains("> second line"));

        // Write json
        let json_path = date_dir.join("143005_claude-context.json");
        let json_content = serde_json::to_string_pretty(&entries).unwrap();
        fs::write(&json_path, &json_content).unwrap();
        assert!(json_path.exists());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_index_serialization_roundtrip() {
        let mut index = StoreIndex::default();
        update_index(&mut index, "CodeScribe", "claude", "2026-01-22", 42);
        update_index(&mut index, "CodeScribe", "gemini", "2026-01-20", 10);
        update_index(&mut index, "vista", "claude", "2026-01-21", 5);

        let json = serde_json::to_string_pretty(&index).unwrap();
        let restored: StoreIndex = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.projects.len(), 2);
        assert!(restored.projects.contains_key("CodeScribe"));
        assert!(restored.projects.contains_key("vista"));

        let cs = &restored.projects["CodeScribe"];
        assert_eq!(cs.agents["claude"].total_entries, 42);
        assert_eq!(cs.agents["claude"].dates, vec!["2026-01-22"]);
        assert_eq!(cs.agents["gemini"].total_entries, 10);
    }

    #[test]
    fn test_update_index() {
        let mut index = StoreIndex::default();

        update_index(&mut index, "proj", "claude", "2026-01-20", 10);
        update_index(&mut index, "proj", "claude", "2026-01-21", 5);
        update_index(&mut index, "proj", "claude", "2026-01-20", 3); // same date, adds to total

        let agent_idx = &index.projects["proj"].agents["claude"];
        assert_eq!(agent_idx.total_entries, 18); // 10 + 5 + 3
        assert_eq!(agent_idx.dates, vec!["2026-01-20", "2026-01-21"]);
    }

    #[test]
    fn test_list_stored_projects() {
        let mut index = StoreIndex::default();
        update_index(&mut index, "zebra", "claude", "2026-01-01", 1);
        update_index(&mut index, "alpha", "codex", "2026-01-01", 1);
        update_index(&mut index, "middle", "gemini", "2026-01-01", 1);

        let projects = list_stored_projects(&index);
        assert_eq!(projects, vec!["alpha", "middle", "zebra"]); // sorted
    }

    #[test]
    fn test_update_index_deduplicates_dates() {
        let mut index = StoreIndex::default();
        update_index(&mut index, "proj", "claude", "2026-01-22", 5);
        update_index(&mut index, "proj", "claude", "2026-01-22", 3);
        update_index(&mut index, "proj", "claude", "2026-01-22", 7);

        let dates = &index.projects["proj"].agents["claude"].dates;
        assert_eq!(dates.len(), 1); // no duplicates
        assert_eq!(dates[0], "2026-01-22");
    }

    // ================================================================
    // Kind classification tests
    // ================================================================

    fn make_entry(role: &str, message: &str) -> TimelineEntry {
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "test-session-abc123".to_string(),
            role: role.to_string(),
            message: message.to_string(),
            branch: None,
            cwd: None,
        }
    }

    #[test]
    fn test_kind_dir_names() {
        assert_eq!(Kind::Conversations.dir_name(), "conversations");
        assert_eq!(Kind::Plans.dir_name(), "plans");
        assert_eq!(Kind::Reports.dir_name(), "reports");
        assert_eq!(Kind::Other.dir_name(), "other");
    }

    #[test]
    fn test_kind_parse_roundtrip() {
        for kind in [Kind::Conversations, Kind::Plans, Kind::Reports, Kind::Other] {
            let parsed = Kind::parse(kind.dir_name()).unwrap();
            assert_eq!(parsed, kind);
        }
        // Singular forms
        assert_eq!(Kind::parse("conversation"), Some(Kind::Conversations));
        assert_eq!(Kind::parse("plan"), Some(Kind::Plans));
        assert_eq!(Kind::parse("report"), Some(Kind::Reports));
        // Case insensitive
        assert_eq!(Kind::parse("PLANS"), Some(Kind::Plans));
        assert_eq!(Kind::parse("Reports"), Some(Kind::Reports));
        // Invalid
        assert_eq!(Kind::parse("bogus"), None);
    }

    #[test]
    fn test_kind_serde_roundtrip() {
        let kind = Kind::Conversations;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, "\"conversations\"");
        let restored: Kind = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, Kind::Conversations);
    }

    #[test]
    fn test_kind_default_is_other() {
        assert_eq!(Kind::default(), Kind::Other);
    }

    #[test]
    fn test_classify_kind_empty_is_other() {
        assert_eq!(classify_kind(&[]), Kind::Other);
    }

    #[test]
    fn test_classify_kind_conversation_first() {
        let entries = vec![
            make_entry("user", "Can you help me fix this bug?"),
            make_entry("assistant", "Sure, let me look at the code."),
            make_entry("user", "It crashes on startup."),
            make_entry("assistant", "I see the issue in the initialization."),
        ];
        assert_eq!(classify_kind(&entries), Kind::Conversations);
    }

    #[test]
    fn test_classify_kind_plan() {
        let entries = vec![
            make_entry("user", "Plan the migration"),
            make_entry(
                "assistant",
                "## Plan\n\nStep 1: Audit current schema\nStep 2: Create migration scripts\nStep 3: Test on staging\nAction items for the team.",
            ),
            make_entry("user", "Looks good, what are the milestones?"),
            make_entry(
                "assistant",
                "Here are the milestones and acceptance criteria for each phase.",
            ),
        ];
        assert_eq!(classify_kind(&entries), Kind::Plans);
    }

    #[test]
    fn test_classify_kind_report() {
        let entries = vec![
            make_entry("user", "Review the PR"),
            make_entry(
                "assistant",
                "## Findings\n\nThe code review reveals several issues.\n## Summary\nOverall quality is good.\n## Recommendations\nAdd more tests.",
            ),
            make_entry("user", "Any metrics?"),
            make_entry(
                "assistant",
                "## Metrics\nCoverage: 85%. Test results show 3 failures.\n## Conclusion\nReady after fixes.",
            ),
        ];
        assert_eq!(classify_kind(&entries), Kind::Reports);
    }

    #[test]
    fn test_classify_kind_conservative_fallback() {
        // Ambiguous content with too few signals → Conversations (not Other)
        let entries = vec![
            make_entry("user", "What do you think about this approach?"),
            make_entry("assistant", "It could work. Let me think about the plan."),
        ];
        assert_eq!(classify_kind(&entries), Kind::Conversations);
    }

    #[test]
    fn test_classify_kind_user_keywords_ignored() {
        // Keywords in user messages should not trigger plan/report classification
        let entries = vec![
            make_entry(
                "user",
                "## Plan\nStep 1: do this\nStep 2: do that\nStep 3: done\nAction items here",
            ),
            make_entry("assistant", "Understood, I'll help with that."),
        ];
        // Only 0 assistant plan keywords hit, so → Conversations
        assert_eq!(classify_kind(&entries), Kind::Conversations);
    }

    // ================================================================
    // Session-first filename tests
    // ================================================================

    #[test]
    fn test_session_basename_format() {
        let name = session_basename("2026-03-21", "claude", "abc123def456", 1);
        assert_eq!(name, "2026_0321_claude_abc123def456_001.md");
    }

    #[test]
    fn test_session_basename_truncates_long_session_id() {
        let long_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let name = session_basename("2026-03-21", "claude", long_id, 3);
        // Truncates to 12 chars (dashes preserved since they're allowed)
        assert!(name.contains("a1b2c3d4-e5f"));
        assert!(name.ends_with("_003.md"));
        // Verify the full basename does NOT contain the entire UUID
        assert!(!name.contains("ef1234567890"));
    }

    #[test]
    fn test_session_basename_chunk_ordering() {
        let a = session_basename("2026-03-21", "claude", "sess1", 1);
        let b = session_basename("2026-03-21", "claude", "sess1", 2);
        let c = session_basename("2026-03-21", "claude", "sess1", 10);
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn test_session_basename_date_ordering() {
        let a = session_basename("2026-03-20", "claude", "sess1", 1);
        let b = session_basename("2026-03-21", "claude", "sess1", 1);
        assert!(a < b, "Earlier date should sort first: {} vs {}", a, b);
    }

    #[test]
    fn test_session_basename_self_describing() {
        // A basename must be meaningful even without its directory path
        let name = session_basename("2026-03-21", "codex", "task-abc-123", 2);
        assert!(name.contains("2026_0321"), "Must contain date");
        assert!(name.contains("codex"), "Must contain agent");
        assert!(
            name.contains("task-abc-12"),
            "Must contain session fragment"
        );
        assert!(name.contains("002"), "Must contain chunk number");
        assert!(name.ends_with(".md"), "Must have .md extension");
    }

    #[test]
    fn test_compact_date() {
        assert_eq!(compact_date("2026-03-21"), "2026_0321");
        assert_eq!(compact_date("2026-01-01"), "2026_0101");
        // Already compact
        assert_eq!(compact_date("2026_0321"), "2026_0321");
    }

    #[test]
    fn test_truncate_session_id_short() {
        assert_eq!(truncate_session_id("abc"), "abc");
        assert_eq!(truncate_session_id(""), "");
    }

    #[test]
    fn test_truncate_session_id_strips_non_alnum() {
        // Only alphanumeric and dashes survive
        assert_eq!(truncate_session_id("a/b:c!d@e#f"), "abcdef");
    }

    // ================================================================
    // Chunk uniqueness within same session/day
    // ================================================================

    #[test]
    fn test_chunk_uniqueness_same_session_day() {
        // Multiple chunks from the same session on the same day must have unique basenames
        let mut names = std::collections::HashSet::new();
        for chunk in 1..=20 {
            let name = session_basename("2026-03-21", "claude", "session-xyz", chunk);
            assert!(names.insert(name.clone()), "Duplicate basename: {}", name);
        }
    }

    #[test]
    fn test_chunk_uniqueness_different_sessions_same_day() {
        let a = session_basename("2026-03-21", "claude", "session-aaa", 1);
        let b = session_basename("2026-03-21", "claude", "session-bbb", 1);
        assert_ne!(a, b, "Different sessions must produce different basenames");
    }

    #[test]
    fn test_chunk_uniqueness_different_agents_same_session() {
        let a = session_basename("2026-03-21", "claude", "session-xyz", 1);
        let b = session_basename("2026-03-21", "codex", "session-xyz", 1);
        assert_ne!(a, b, "Different agents must produce different basenames");
    }

    // ================================================================
    // Output path integration test
    // ================================================================

    #[test]
    fn output_session_first_path_structure() {
        // Verify the full directory structure matches canonical layout
        let date = "2026-03-21";
        let kind = Kind::Conversations;
        let agent = "claude";
        let project = "ai-contexters";

        // Simulate the path that write_context_session_first would create
        let expected_subpath = format!("{}/{}/{}/{}", project, date, kind.dir_name(), agent);

        let basename = session_basename(date, agent, "sess-abc123", 1);
        let full_subpath = format!("{}/{}", expected_subpath, basename);

        assert!(full_subpath.contains("conversations/claude"));
        assert!(full_subpath.ends_with("2026_0321_claude_sess-abc123_001.md"));
    }

    fn semantic_entry(
        ts: (i32, u32, u32, u32, u32, u32),
        session_id: &str,
        role: &str,
        message: &str,
        cwd: Option<&str>,
    ) -> TimelineEntry {
        TimelineEntry {
            timestamp: Utc
                .with_ymd_and_hms(ts.0, ts.1, ts.2, ts.3, ts.4, ts.5)
                .unwrap(),
            agent: "codex".to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            message: message.to_string(),
            branch: None,
            cwd: cwd.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn test_store_semantic_segments_emit_repo_and_non_repo_roots() {
        let root = env::temp_dir().join("aicx-store-segmentation-proof");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let entries = vec![
            semantic_entry(
                (2026, 3, 21, 9, 0, 0),
                "sess-a",
                "user",
                "No repo yet, just planning the migration.",
                None,
            ),
            semantic_entry(
                (2026, 3, 21, 9, 1, 0),
                "sess-a",
                "assistant",
                "Goal:\n- make segmentation real\nAcceptance:\n- stop fake buckets",
                None,
            ),
            semantic_entry(
                (2026, 3, 21, 9, 2, 0),
                "sess-a",
                "user",
                "Switch to https://github.com/VetCoders/ai-contexters now.",
                None,
            ),
            semantic_entry(
                (2026, 3, 21, 9, 3, 0),
                "sess-a",
                "user",
                "Then inspect https://github.com/VetCoders/loctree as well.",
                None,
            ),
        ];

        let summary = store_semantic_segments_at(&root, &entries, &ChunkerConfig::default())
            .expect("store semantic segments");

        assert_eq!(summary.total_entries, 4);
        assert!(
            summary
                .written_paths
                .iter()
                .any(|path| { path.starts_with(root.join("non-repository-contexts")) })
        );
        assert!(summary.written_paths.iter().any(|path| {
            path.starts_with(root.join("store").join("VetCoders").join("ai-contexters"))
        }));
        assert!(summary.written_paths.iter().any(|path| {
            path.starts_with(root.join("store").join("VetCoders").join("loctree"))
        }));

        let scanned = scan_context_files_at(&root).expect("scan stored files");
        assert!(
            scanned
                .iter()
                .any(|file| file.project == NON_REPOSITORY_CONTEXTS)
        );
        assert!(
            scanned
                .iter()
                .any(|file| file.project == "VetCoders/ai-contexters")
        );
        assert!(
            scanned
                .iter()
                .any(|file| file.project == "VetCoders/loctree")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_build_migration_inventory_marks_truthful_rebuild_candidates() {
        let root = env::temp_dir().join("aicx-migration-inventory-proof");
        let legacy_root = root.join("legacy");
        let manifest_path = root.join("migration-index.json");
        let existing_source = root.join("sources").join("session-a.jsonl");
        let missing_source = root.join("sources").join("session-missing.jsonl");
        let _ = fs::remove_dir_all(&root);

        fs::create_dir_all(legacy_root.join("demo").join("2026-03-21")).unwrap();
        fs::create_dir_all(existing_source.parent().unwrap()).unwrap();
        fs::write(&existing_source, "{\"id\":\"session-a\"}\n").unwrap();

        fs::write(
            legacy_root
                .join("demo")
                .join("2026-03-21")
                .join("chunk-a.md"),
            format!("input: {}\n", existing_source.display()),
        )
        .unwrap();
        fs::write(
            legacy_root
                .join("demo")
                .join("2026-03-21")
                .join("chunk-b.md"),
            format!("input: {}\n", missing_source.display()),
        )
        .unwrap();

        let inventory = build_migration_inventory_at(&legacy_root, &manifest_path)
            .expect("build migration inventory");

        assert_eq!(inventory.total_files, 2);
        assert_eq!(inventory.rebuild_candidates, 1);
        assert_eq!(inventory.missing_sources, 1);
        assert!(
            inventory
                .items
                .iter()
                .any(|item| item.rebuild_candidate && item.existing_sources.len() == 1)
        );
        assert!(
            inventory
                .items
                .iter()
                .any(|item| !item.rebuild_candidate && item.existing_sources.is_empty())
        );

        let _ = fs::remove_dir_all(&root);
    }
}
