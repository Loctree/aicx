//! Central context store for ai-contexters.
//!
//! Manages the `~/.aicx/` directory structure:
//! - `store/<organization>/<repository>/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md`
//! - `non-repository-contexts/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md`
//! - `store/<project>/<date>/<time>_<agent>-context.{md,json}` — legacy monolithic helpers kept for library use/tests
//! - `memex/sync_state.json` — sync bookkeeping for the semantic index add-on
//! - `index.json` — manifest of stored contexts
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, TimeZone, Utc};
use globset::{Glob, GlobMatcher};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::chunker::{self, ChunkerConfig};
use crate::output::TimelineEntry;
use crate::sanitize;
use crate::segmentation::semantic_segments;
use crate::sources::{self, ExtractionConfig};
pub use crate::types::Kind;
use crate::types::{RepoIdentity, SemanticSegment};

// ============================================================================
// Kind classification
// ============================================================================

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
pub const LEGACY_SALVAGE_DIRNAME: &str = "legacy-store";
pub const AICX_IGNORE_FILENAME: &str = ".aicxignore";
const MIGRATION_DIRNAME: &str = "migration";
const MIGRATION_MANIFEST_FILENAME: &str = "manifest.json";
const MIGRATION_REPORT_FILENAME: &str = "report.md";

#[derive(Debug, Clone)]
struct IgnoreRule {
    negate: bool,
    matcher: GlobMatcher,
}

#[derive(Debug, Clone, Default)]
pub struct StoreIgnoreMatcher {
    base: PathBuf,
    rules: Vec<IgnoreRule>,
}

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

fn legacy_salvage_dir(base: &Path) -> PathBuf {
    base.join(LEGACY_SALVAGE_DIRNAME)
}

fn migration_dir(base: &Path) -> PathBuf {
    base.join(MIGRATION_DIRNAME)
}

fn migration_manifest_path(base: &Path) -> PathBuf {
    migration_dir(base).join(MIGRATION_MANIFEST_FILENAME)
}

fn migration_report_path(base: &Path) -> PathBuf {
    migration_dir(base).join(MIGRATION_REPORT_FILENAME)
}

impl StoreIgnoreMatcher {
    fn load(base: &Path) -> Result<Self> {
        let path = base.join(AICX_IGNORE_FILENAME);
        if !path.exists() {
            return Ok(Self {
                base: base.to_path_buf(),
                rules: Vec::new(),
            });
        }

        let raw = sanitize::read_to_string_validated(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let mut rules = Vec::new();

        for (line_no, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let negate = trimmed.starts_with('!');
            let pattern = trimmed.trim_start_matches('!').trim();
            if pattern.is_empty() {
                continue;
            }

            let normalized = normalize_aicx_ignore_pattern(pattern);
            let matcher = Glob::new(&normalized)
                .with_context(|| {
                    format!(
                        "Invalid {} pattern at line {}: {}",
                        path.display(),
                        line_no + 1,
                        trimmed
                    )
                })?
                .compile_matcher();

            rules.push(IgnoreRule { negate, matcher });
        }

        Ok(Self {
            base: base.to_path_buf(),
            rules,
        })
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        if self.rules.is_empty() {
            return false;
        }

        let Ok(relative) = path.strip_prefix(&self.base) else {
            return false;
        };
        let relative = normalize_relative_store_path(relative);
        if relative.is_empty() {
            return false;
        }

        let mut ignored = false;
        for rule in &self.rules {
            if rule.matcher.is_match(&relative) {
                ignored = !rule.negate;
            }
        }
        ignored
    }
}

fn normalize_relative_store_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_aicx_ignore_pattern(pattern: &str) -> String {
    let mut normalized = pattern
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .replace('\\', "/");

    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }

    if normalized.ends_with('/') {
        normalized.push_str("**");
    }

    normalized
}

pub fn load_ignore_matcher_at(base: &Path) -> Result<StoreIgnoreMatcher> {
    StoreIgnoreMatcher::load(base)
}

pub fn filter_ignored_paths_at<P>(base: &Path, paths: &[P]) -> Result<(Vec<PathBuf>, usize)>
where
    P: AsRef<Path>,
{
    let matcher = load_ignore_matcher_at(base)?;
    if matcher.rules.is_empty() {
        return Ok((
            paths
                .iter()
                .map(|path| path.as_ref().to_path_buf())
                .collect(),
            0,
        ));
    }

    let mut kept = Vec::with_capacity(paths.len());
    let mut ignored = 0usize;

    for path in paths {
        let path = path.as_ref();
        if matcher.is_ignored(path) {
            ignored += 1;
        } else {
            kept.push(path.to_path_buf());
        }
    }

    Ok((kept, ignored))
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

/// Load the store index from `~/.aicx/index.json`.
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

/// Write timeline entries to the canonical store.
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

/// Write timeline entries as agent-friendly chunks to the canonical store.
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
        write_chunk_sidecar(&path, chunk)?;
        written.push(path);
    }

    Ok(written)
}

fn write_chunk_sidecar(path: &Path, chunk: &chunker::Chunk) -> Result<()> {
    let sidecar_path = path.with_extension("meta.json");
    let write_path = sanitize::validate_write_path(&sidecar_path)?;
    let sidecar = chunker::ChunkMetadataSidecar::from(chunk);
    fs::write(&write_path, serde_json::to_vec_pretty(&sidecar)?)?;
    Ok(())
}

pub fn store_semantic_segments(
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
) -> Result<StoreWriteSummary> {
    store_semantic_segments_with_progress(entries, chunker_config, |_, _| {})
}

pub fn store_semantic_segments_with_progress<F>(
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
    progress: F,
) -> Result<StoreWriteSummary>
where
    F: FnMut(usize, usize),
{
    store_semantic_segments_at(&store_base_dir()?, entries, chunker_config, progress)
}

fn store_semantic_segments_at<F>(
    base: &Path,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
    mut progress: F,
) -> Result<StoreWriteSummary>
where
    F: FnMut(usize, usize),
{
    let mut summary = StoreWriteSummary::default();
    if entries.is_empty() {
        return Ok(summary);
    }

    let segments = semantic_segments(entries);
    let total_segments = segments.len();
    let mut index = load_index_at(base);

    for (segment_idx, segment) in segments.into_iter().enumerate() {
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
        progress(segment_idx + 1, total_segments);
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
    // Only assertable identities (Primary/Secondary) earn canonical store placement.
    // Fallback/Opaque/None route to non-repository-contexts.
    let project = if segment.has_assertable_identity() {
        segment.repo.as_ref().map(RepoIdentity::slug)
    } else {
        None
    };
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

pub fn scan_context_files_raw() -> Result<Vec<StoredContextFile>> {
    let base = store_base_dir()?;
    scan_context_files_raw_at(&base)
}

pub fn scan_context_files_at(base: &Path) -> Result<Vec<StoredContextFile>> {
    let base = sanitize::validate_dir_path(base)?;
    let ignore = load_ignore_matcher_at(&base)?;
    scan_context_files_with_ignore(&base, &ignore)
}

pub fn scan_context_files_raw_at(base: &Path) -> Result<Vec<StoredContextFile>> {
    let base = sanitize::validate_dir_path(base)?;
    let ignore = StoreIgnoreMatcher {
        base: base.clone(),
        rules: Vec::new(),
    };
    scan_context_files_with_ignore(&base, &ignore)
}

fn scan_context_files_with_ignore(
    base: &Path,
    ignore: &StoreIgnoreMatcher,
) -> Result<Vec<StoredContextFile>> {
    let mut files = Vec::new();

    let canonical_root = base.join(CANONICAL_STORE_DIRNAME);
    if canonical_root.is_dir() {
        scan_repo_store(&canonical_root, ignore, &mut files)?;
    }

    let non_repo_root = base.join(NON_REPOSITORY_CONTEXTS);
    if non_repo_root.is_dir() {
        scan_non_repository_store(&non_repo_root, ignore, &mut files)?;
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
    context_files_since_at(&store_base_dir()?, cutoff, project_filter)
}

fn context_files_since_at(
    base: &Path,
    cutoff: SystemTime,
    project_filter: Option<&str>,
) -> Result<Vec<StoredContextFile>> {
    let filter = project_filter.map(|value| value.to_ascii_lowercase());
    let cutoff_date = DateTime::<Utc>::from(cutoff).format("%Y-%m-%d").to_string();
    let mut files = scan_context_files_at(base)?;
    files.retain(|file| {
        let matches_project = filter
            .as_ref()
            .is_none_or(|needle| file.project.to_ascii_lowercase().contains(needle));
        // Discovery recency is anchored to the canonical chunk date encoded in the
        // store layout, not filesystem mtime which can drift during migration/copy.
        let matches_cutoff = file.date_iso >= cutoff_date;
        matches_project && matches_cutoff
    });
    Ok(files)
}

/// Load the metadata sidecar for a context file, if it exists.
pub fn load_sidecar(chunk_path: &Path) -> Option<chunker::ChunkMetadataSidecar> {
    let sidecar_path = chunk_path.with_extension("meta.json");
    let sidecar_path = sanitize::validate_read_path(&sidecar_path).ok()?;
    let content = fs::read_to_string(&sidecar_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Find stored chunks whose sidecar metadata matches a run ID.
pub fn chunks_by_run_id(run_id: &str, project: Option<&str>) -> Result<Vec<StoredContextFile>> {
    let cutoff = SystemTime::now() - std::time::Duration::from_secs(7 * 24 * 3600);
    chunks_by_run_id_at(&store_base_dir()?, run_id, project, cutoff)
}

fn chunks_by_run_id_at(
    base: &Path,
    run_id: &str,
    project: Option<&str>,
    cutoff: SystemTime,
) -> Result<Vec<StoredContextFile>> {
    let filter = project.map(|value| value.to_ascii_lowercase());
    let cutoff_date = DateTime::<Utc>::from(cutoff).format("%Y-%m-%d").to_string();
    let mut matched = Vec::new();

    for file in scan_context_files_at(base)? {
        let matches_project = filter
            .as_ref()
            .is_none_or(|needle| file.project.to_ascii_lowercase().contains(needle));
        let matches_cutoff = file.date_iso >= cutoff_date;

        if !matches_project || !matches_cutoff {
            continue;
        }

        if load_sidecar(&file.path)
            .and_then(|sidecar| sidecar.run_id)
            .as_deref()
            == Some(run_id)
        {
            matched.push(file);
        }
    }

    Ok(matched)
}

fn scan_repo_store(
    root: &Path,
    ignore: &StoreIgnoreMatcher,
    files: &mut Vec<StoredContextFile>,
) -> Result<()> {
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
                        let repo_slug = repo.slug();
                        let ctx = LeafScanContext {
                            repo: Some(repo.clone()),
                            project: &repo_slug,
                            date_compact: &date_compact,
                            kind,
                            agent: &agent,
                        };
                        collect_leaf_files(&agent_path, &ctx, ignore, files)?;
                    }
                }
            }
        }
    }

    Ok(())
}

fn scan_non_repository_store(
    root: &Path,
    ignore: &StoreIgnoreMatcher,
    files: &mut Vec<StoredContextFile>,
) -> Result<()> {
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
                let ctx = LeafScanContext {
                    repo: None,
                    project: NON_REPOSITORY_CONTEXTS,
                    date_compact: &date_compact,
                    kind,
                    agent: &agent,
                };
                collect_leaf_files(&agent_path, &ctx, ignore, files)?;
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
struct LeafScanContext<'a> {
    repo: Option<RepoIdentity>,
    project: &'a str,
    date_compact: &'a str,
    kind: Kind,
    agent: &'a str,
}

fn collect_leaf_files(
    dir: &Path,
    ctx: &LeafScanContext<'_>,
    ignore: &StoreIgnoreMatcher,
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
        if ignore.is_ignored(&path) {
            continue;
        }

        let Some((session_id, chunk)) = parse_session_basename(
            &file_entry.file_name().to_string_lossy(),
            ctx.agent,
            ctx.date_compact,
        ) else {
            continue;
        };

        files.push(StoredContextFile {
            path,
            project: ctx.project.to_string(),
            repo: ctx.repo.clone(),
            date_compact: ctx.date_compact.to_string(),
            date_iso: expand_compact_date(ctx.date_compact),
            kind: ctx.kind,
            agent: ctx.agent.to_string(),
            session_id,
            chunk,
        });
    }

    Ok(())
}

fn parse_session_basename(name: &str, agent: &str, date_compact: &str) -> Option<(String, u32)> {
    let ext = if name.ends_with(".md") {
        ".md"
    } else if name.ends_with(".json") {
        ".json"
    } else {
        return None;
    };

    let stem = name.strip_suffix(ext)?;
    let prefix = format!("{date_compact}_{agent}_");
    let remainder = stem.strip_prefix(&prefix)?;
    let (session_id, chunk_str) = remainder.rsplit_once('_')?;

    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return None;
    }

    if chunk_str.len() != 3 || !chunk_str.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    let chunk = chunk_str.parse().ok()?;
    Some((session_id.to_string(), chunk))
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LegacyItemKind {
    ContextBundle,
    LooseFile,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationAction {
    Rebuild,
    RebuildAndSalvage,
    Salvage,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationExecution {
    Planned,
    Executed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MigrationTotals {
    pub total_items: usize,
    pub total_legacy_files: usize,
    pub rebuild_items: usize,
    pub rebuild_and_salvage_items: usize,
    pub salvage_items: usize,
    pub unclassified_items: usize,
    pub resolved_sources: usize,
    pub missing_source_hints: usize,
    pub ambiguous_source_hints: usize,
    pub rebuilt_items: usize,
    pub salvaged_items: usize,
    pub rebuilt_paths: usize,
    pub salvaged_paths: usize,
    pub failed_items: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationManifest {
    pub generated_at: DateTime<Utc>,
    pub legacy_root: String,
    pub store_root: String,
    pub manifest_path: String,
    pub report_path: String,
    pub dry_run: bool,
    pub totals: MigrationTotals,
    pub items: Vec<MigrationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationItem {
    pub item_id: String,
    pub legacy_kind: LegacyItemKind,
    pub legacy_group: String,
    pub legacy_files: Vec<String>,
    pub agent_hint: Option<String>,
    pub date_hint: Option<String>,
    pub source_hints: Vec<String>,
    pub existing_sources: Vec<String>,
    pub missing_sources: Vec<String>,
    pub ambiguous_sources: Vec<String>,
    pub action: MigrationAction,
    pub action_reason: String,
    pub execution: MigrationExecution,
    pub canonical_paths: Vec<String>,
    pub salvage_paths: Vec<String>,
    pub errors: Vec<String>,
}

impl MigrationItem {
    fn from_plan(plan: &LegacyItemPlan) -> Self {
        let mut legacy_files: Vec<String> = plan
            .legacy_files
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        legacy_files.sort();

        let mut existing_sources: Vec<String> = plan
            .resolved_sources
            .iter()
            .map(|source| source.path.display().to_string())
            .collect();
        existing_sources.sort();

        let mut canonical_paths: Vec<String> = plan
            .canonical_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        canonical_paths.sort();

        let mut salvage_paths: Vec<String> = plan
            .salvage_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        salvage_paths.sort();

        Self {
            item_id: plan.item_id.clone(),
            legacy_kind: plan.legacy_kind,
            legacy_group: plan.legacy_group.clone(),
            legacy_files,
            agent_hint: plan.agent_hint.clone(),
            date_hint: plan.date_hint.clone(),
            source_hints: plan.source_hints.clone(),
            existing_sources,
            missing_sources: plan.missing_sources.clone(),
            ambiguous_sources: plan.ambiguous_sources.clone(),
            action: plan.action,
            action_reason: plan.action_reason.clone(),
            execution: plan.execution,
            canonical_paths,
            salvage_paths,
            errors: plan.errors.clone(),
        }
    }
}

impl MigrationTotals {
    fn from_items(items: &[MigrationItem]) -> Self {
        Self {
            total_items: items.len(),
            total_legacy_files: items.iter().map(|item| item.legacy_files.len()).sum(),
            rebuild_items: items
                .iter()
                .filter(|item| item.action == MigrationAction::Rebuild)
                .count(),
            rebuild_and_salvage_items: items
                .iter()
                .filter(|item| item.action == MigrationAction::RebuildAndSalvage)
                .count(),
            salvage_items: items
                .iter()
                .filter(|item| item.action == MigrationAction::Salvage)
                .count(),
            unclassified_items: items
                .iter()
                .filter(|item| is_unclassified_item(item))
                .count(),
            resolved_sources: items.iter().map(|item| item.existing_sources.len()).sum(),
            missing_source_hints: items.iter().map(|item| item.missing_sources.len()).sum(),
            ambiguous_source_hints: items.iter().map(|item| item.ambiguous_sources.len()).sum(),
            rebuilt_items: items
                .iter()
                .filter(|item| !item.canonical_paths.is_empty())
                .count(),
            salvaged_items: items
                .iter()
                .filter(|item| !item.salvage_paths.is_empty())
                .count(),
            rebuilt_paths: items.iter().map(|item| item.canonical_paths.len()).sum(),
            salvaged_paths: items.iter().map(|item| item.salvage_paths.len()).sum(),
            failed_items: items.iter().filter(|item| !item.errors.is_empty()).count(),
        }
    }
}

#[derive(Debug, Clone)]
struct LegacyItemPlan {
    item_id: String,
    legacy_kind: LegacyItemKind,
    legacy_group: String,
    legacy_files: Vec<PathBuf>,
    agent_hint: Option<String>,
    date_hint: Option<String>,
    source_hints: Vec<String>,
    resolved_sources: Vec<ResolvedSource>,
    missing_sources: Vec<String>,
    ambiguous_sources: Vec<String>,
    action: MigrationAction,
    action_reason: String,
    execution: MigrationExecution,
    canonical_paths: Vec<PathBuf>,
    salvage_paths: Vec<PathBuf>,
    errors: Vec<String>,
}

#[derive(Debug, Clone)]
struct LegacyBundleDescriptor {
    bundle_key: PathBuf,
    agent_hint: Option<String>,
    date_hint: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum SourceFormat {
    Claude,
    Codex,
    Gemini,
    GeminiAntigravity,
}

#[derive(Debug, Clone)]
struct ResolvedSource {
    path: PathBuf,
    format: SourceFormat,
}

#[derive(Debug, Clone, Default)]
struct SourceResolution {
    source_hints: Vec<String>,
    resolved_sources: Vec<ResolvedSource>,
    missing_sources: Vec<String>,
    ambiguous_sources: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct SourceProcessingOutcome {
    canonical_paths: Vec<PathBuf>,
    error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct SourceLocator {
    index: HashMap<String, Vec<PathBuf>>,
}

#[derive(Debug, Clone)]
enum SourceLookupOutcome {
    Missing,
    Unique(PathBuf),
    Ambiguous(Vec<PathBuf>),
}

pub fn run_migration(dry_run: bool) -> Result<MigrationManifest> {
    run_migration_with_paths(dry_run, None, None)
}

pub fn run_migration_with_paths(
    dry_run: bool,
    legacy_root: Option<PathBuf>,
    store_root: Option<PathBuf>,
) -> Result<MigrationManifest> {
    let legacy_root = legacy_root.unwrap_or(legacy_store_base_dir()?);
    let store_root = store_root.unwrap_or(store_base_dir()?);
    let locator = SourceLocator::from_home();
    let manifest = run_migration_at(&legacy_root, &store_root, dry_run, &locator)?;

    print_migration_summary(&manifest);
    if dry_run {
        println!(
            "[DRY RUN] Would write migration manifest to {}",
            manifest.manifest_path
        );
        println!(
            "[DRY RUN] Would write migration report to {}",
            manifest.report_path
        );
    } else {
        println!("Wrote migration manifest to {}", manifest.manifest_path);
        println!("Wrote migration report to {}", manifest.report_path);
    }

    Ok(manifest)
}

fn run_migration_at(
    legacy_root: &Path,
    store_root: &Path,
    dry_run: bool,
    locator: &SourceLocator,
) -> Result<MigrationManifest> {
    let normalized_legacy_root = if legacy_root.exists() {
        sanitize::validate_dir_path(legacy_root)?
    } else {
        legacy_root.to_path_buf()
    };
    let mut items = collect_legacy_items(&normalized_legacy_root, locator)?;

    if !dry_run {
        execute_migration_items(&normalized_legacy_root, store_root, &mut items)?;
    }

    let manifest =
        build_migration_manifest_at(&normalized_legacy_root, store_root, dry_run, &items);
    if !dry_run {
        write_migration_artifacts(&manifest)?;
    }

    Ok(manifest)
}

fn build_migration_manifest_at(
    legacy_root: &Path,
    store_root: &Path,
    dry_run: bool,
    items: &[LegacyItemPlan],
) -> MigrationManifest {
    let items: Vec<MigrationItem> = items.iter().map(MigrationItem::from_plan).collect();
    let totals = MigrationTotals::from_items(&items);

    MigrationManifest {
        generated_at: Utc::now(),
        legacy_root: legacy_root.display().to_string(),
        store_root: store_root.display().to_string(),
        manifest_path: migration_manifest_path(store_root).display().to_string(),
        report_path: migration_report_path(store_root).display().to_string(),
        dry_run,
        totals,
        items,
    }
}

fn write_migration_artifacts(manifest: &MigrationManifest) -> Result<()> {
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let manifest_path = PathBuf::from(&manifest.manifest_path);
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let manifest_json = serde_json::to_string_pretty(manifest)?;
    let manifest_path = sanitize::validate_write_path(&manifest_path)?;
    fs::write(&manifest_path, manifest_json)?;

    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    let report_path = PathBuf::from(&manifest.report_path);
    if let Some(parent) = report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let report = render_migration_report(manifest);
    let report_path = sanitize::validate_write_path(&report_path)?;
    fs::write(&report_path, report)?;

    Ok(())
}

fn render_migration_report(manifest: &MigrationManifest) -> String {
    let mut report = String::new();
    report.push_str("# AICX Migration Report\n\n");
    report.push_str(&format!(
        "- Generated at: `{}`\n",
        manifest.generated_at.to_rfc3339()
    ));
    report.push_str(&format!("- Dry run: `{}`\n", manifest.dry_run));
    report.push_str(&format!("- Legacy root: `{}`\n", manifest.legacy_root));
    report.push_str(&format!("- Store root: `{}`\n", manifest.store_root));
    report.push_str(&format!("- Manifest: `{}`\n", manifest.manifest_path));
    report.push_str(&format!("- Report: `{}`\n\n", manifest.report_path));

    report.push_str("## Summary\n\n");
    report.push_str(&format!(
        "- Swept `{}` migration item(s) from `{}` legacy file(s)\n",
        manifest.totals.total_items, manifest.totals.total_legacy_files
    ));
    report.push_str(&format!(
        "- Planned rebuild-only items: `{}`\n",
        manifest.totals.rebuild_items
    ));
    report.push_str(&format!(
        "- Planned rebuild+salvage items: `{}`\n",
        manifest.totals.rebuild_and_salvage_items
    ));
    report.push_str(&format!(
        "- Planned salvage-only items: `{}`\n",
        manifest.totals.salvage_items
    ));
    report.push_str(&format!(
        "- Unclassified legacy items: `{}`\n",
        manifest.totals.unclassified_items
    ));
    report.push_str(&format!(
        "- Resolved source matches: `{}`\n",
        manifest.totals.resolved_sources
    ));
    report.push_str(&format!(
        "- Missing source hints: `{}`\n",
        manifest.totals.missing_source_hints
    ));
    report.push_str(&format!(
        "- Ambiguous source hints: `{}`\n",
        manifest.totals.ambiguous_source_hints
    ));
    report.push_str(&format!(
        "- Rebuilt items: `{}` (`{}` canonical path(s))\n",
        manifest.totals.rebuilt_items, manifest.totals.rebuilt_paths
    ));
    report.push_str(&format!(
        "- Salvaged items: `{}` (`{}` preserved path(s))\n",
        manifest.totals.salvaged_items, manifest.totals.salvaged_paths
    ));
    report.push_str(&format!(
        "- Items with execution errors: `{}`\n\n",
        manifest.totals.failed_items
    ));

    push_report_section(
        &mut report,
        if manifest.dry_run {
            "Planned Rebuild"
        } else {
            "Rebuilt"
        },
        manifest.items.iter().filter(|item| {
            matches!(
                item.action,
                MigrationAction::Rebuild | MigrationAction::RebuildAndSalvage
            )
        }),
    );
    push_report_section(
        &mut report,
        if manifest.dry_run {
            "Planned Salvage"
        } else {
            "Salvaged"
        },
        manifest.items.iter().filter(|item| {
            item.action == MigrationAction::Salvage
                || item.action == MigrationAction::RebuildAndSalvage
                || !item.salvage_paths.is_empty()
        }),
    );
    push_report_section(
        &mut report,
        "Unclassified Legacy Items",
        manifest
            .items
            .iter()
            .filter(|item| is_unclassified_item(item)),
    );

    report
}

fn push_report_section<'a, I>(report: &mut String, title: &str, items: I)
where
    I: Iterator<Item = &'a MigrationItem>,
{
    report.push_str(&format!("## {}\n\n", title));
    let mut wrote = false;

    for item in items {
        wrote = true;
        report.push_str(&format!(
            "- `{}` [{}]\n",
            item.legacy_group, item.action_reason
        ));
        if !item.existing_sources.is_empty() {
            report.push_str(&format!(
                "  sources: `{}`\n",
                item.existing_sources.join("`, `")
            ));
        }
        if !item.canonical_paths.is_empty() {
            report.push_str(&format!(
                "  canonical: `{}`\n",
                item.canonical_paths.join("`, `")
            ));
        }
        if !item.salvage_paths.is_empty() {
            report.push_str(&format!(
                "  legacy: `{}`\n",
                item.salvage_paths.join("`, `")
            ));
        }
        if !item.missing_sources.is_empty() {
            report.push_str(&format!(
                "  missing: `{}`\n",
                item.missing_sources.join("`, `")
            ));
        }
        if !item.ambiguous_sources.is_empty() {
            report.push_str(&format!(
                "  ambiguous: `{}`\n",
                item.ambiguous_sources.join("`, `")
            ));
        }
        if !item.errors.is_empty() {
            report.push_str(&format!("  errors: `{}`\n", item.errors.join("`, `")));
        }
    }

    if !wrote {
        report.push_str("- none\n");
    }

    report.push('\n');
}

fn print_migration_summary(manifest: &MigrationManifest) {
    println!(
        "Legacy sweep: {} item(s) from {} file(s).",
        manifest.totals.total_items, manifest.totals.total_legacy_files
    );
    println!("Legacy root: {}", manifest.legacy_root);
    println!("Store root: {}", manifest.store_root);
    println!(
        "Plan: {} rebuild-only, {} rebuild+salvage, {} salvage-only, {} unclassified.",
        manifest.totals.rebuild_items,
        manifest.totals.rebuild_and_salvage_items,
        manifest.totals.salvage_items,
        manifest.totals.unclassified_items
    );
    println!(
        "Source hints: {} resolved, {} missing, {} ambiguous.",
        manifest.totals.resolved_sources,
        manifest.totals.missing_source_hints,
        manifest.totals.ambiguous_source_hints
    );

    if !manifest.dry_run {
        println!(
            "Executed: {} rebuilt item(s) -> {} canonical path(s); {} salvaged item(s) -> {} preserved path(s); {} item(s) with errors.",
            manifest.totals.rebuilt_items,
            manifest.totals.rebuilt_paths,
            manifest.totals.salvaged_items,
            manifest.totals.salvaged_paths,
            manifest.totals.failed_items
        );
    }
}

fn collect_legacy_items(
    legacy_root: &Path,
    locator: &SourceLocator,
) -> Result<Vec<LegacyItemPlan>> {
    let mut files = Vec::new();
    collect_legacy_files(legacy_root, &mut files)?;

    let mut bundles: BTreeMap<String, (LegacyBundleDescriptor, Vec<PathBuf>)> = BTreeMap::new();
    let mut loose_files = Vec::new();

    for file in files {
        let relative = match file.strip_prefix(legacy_root) {
            Ok(relative) => relative.to_path_buf(),
            Err(_) => continue,
        };

        if let Some(descriptor) = legacy_bundle_descriptor(&relative) {
            bundles
                .entry(descriptor.bundle_key.display().to_string())
                .or_insert_with(|| (descriptor.clone(), Vec::new()))
                .1
                .push(file);
        } else {
            loose_files.push(file);
        }
    }

    let mut items = Vec::new();

    for (_, (descriptor, mut bundle_files)) in bundles {
        bundle_files.sort();
        items.push(build_context_bundle_plan(
            &bundle_files,
            &descriptor,
            locator,
        )?);
    }

    loose_files.sort();
    for file in loose_files {
        let relative = file
            .strip_prefix(legacy_root)
            .unwrap_or(file.as_path())
            .display()
            .to_string();
        items.push(LegacyItemPlan {
            item_id: relative.clone(),
            legacy_kind: LegacyItemKind::LooseFile,
            legacy_group: relative,
            legacy_files: vec![file],
            agent_hint: None,
            date_hint: None,
            source_hints: Vec::new(),
            resolved_sources: Vec::new(),
            missing_sources: Vec::new(),
            ambiguous_sources: Vec::new(),
            action: MigrationAction::Salvage,
            action_reason: "non_context_legacy_file".to_string(),
            execution: MigrationExecution::Planned,
            canonical_paths: Vec::new(),
            salvage_paths: Vec::new(),
            errors: Vec::new(),
        });
    }

    items.sort_by(|left, right| left.legacy_group.cmp(&right.legacy_group));
    Ok(items)
}

fn collect_legacy_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() || !root.is_dir() {
        return Ok(());
    }

    for entry in sanitize::read_dir_validated(root)?.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            collect_legacy_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }

    Ok(())
}

fn legacy_bundle_descriptor(relative: &Path) -> Option<LegacyBundleDescriptor> {
    let file_name = relative.file_name()?.to_str()?;
    let stem = Path::new(file_name).file_stem()?.to_str()?;
    let parent = relative.parent()?;
    let date_hint = parent.file_name()?.to_str()?.to_string();
    if !looks_like_iso_date(&date_hint) {
        return None;
    }

    let bundle_re =
        Regex::new(r"^(?P<time>\d{6})_(?P<agent>[A-Za-z0-9_]+)(?:-(?P<tail>\d{3}|context))?$")
            .expect("legacy bundle regex should compile");
    let captures = bundle_re.captures(stem)?;
    let bundle_name = format!("{}_{}", &captures["time"], &captures["agent"]);

    Some(LegacyBundleDescriptor {
        bundle_key: parent.join(bundle_name),
        agent_hint: Some(captures["agent"].to_string()),
        date_hint: Some(date_hint),
    })
}

fn build_context_bundle_plan(
    bundle_files: &[PathBuf],
    descriptor: &LegacyBundleDescriptor,
    locator: &SourceLocator,
) -> Result<LegacyItemPlan> {
    let resolution =
        resolve_sources_for_legacy_files(bundle_files, descriptor.agent_hint.as_deref(), locator);
    let has_resolved = !resolution.resolved_sources.is_empty();
    let has_unresolved =
        !resolution.missing_sources.is_empty() || !resolution.ambiguous_sources.is_empty();

    let (action, action_reason) = if has_resolved && has_unresolved {
        (
            MigrationAction::RebuildAndSalvage,
            "partial_source_recovery".to_string(),
        )
    } else if has_resolved {
        (MigrationAction::Rebuild, "rebuild_from_source".to_string())
    } else if !resolution.ambiguous_sources.is_empty() {
        (
            MigrationAction::Salvage,
            "ambiguous_source_hints".to_string(),
        )
    } else if !resolution.source_hints.is_empty() {
        (MigrationAction::Salvage, "missing_source".to_string())
    } else {
        (MigrationAction::Salvage, "no_source_hints".to_string())
    };

    Ok(LegacyItemPlan {
        item_id: descriptor.bundle_key.display().to_string(),
        legacy_kind: LegacyItemKind::ContextBundle,
        legacy_group: descriptor.bundle_key.display().to_string(),
        legacy_files: bundle_files.to_vec(),
        agent_hint: descriptor.agent_hint.clone(),
        date_hint: descriptor.date_hint.clone(),
        source_hints: resolution.source_hints,
        resolved_sources: resolution.resolved_sources,
        missing_sources: resolution.missing_sources,
        ambiguous_sources: resolution.ambiguous_sources,
        action,
        action_reason,
        execution: MigrationExecution::Planned,
        canonical_paths: Vec::new(),
        salvage_paths: Vec::new(),
        errors: Vec::new(),
    })
}

fn resolve_sources_for_legacy_files(
    bundle_files: &[PathBuf],
    agent_hint: Option<&str>,
    locator: &SourceLocator,
) -> SourceResolution {
    let mut direct_candidates = BTreeSet::new();
    let mut lookup_hints = BTreeSet::new();
    let mut source_hints = BTreeSet::new();

    for file in bundle_files {
        let content = sanitize::read_to_string_validated(file).unwrap_or_default();
        collect_source_hints_from_text(
            &content,
            agent_hint,
            &mut direct_candidates,
            &mut lookup_hints,
            &mut source_hints,
        );
    }

    let mut resolved_sources = BTreeMap::new();
    let mut missing_sources = BTreeSet::new();
    let mut ambiguous_sources = BTreeSet::new();
    let mut handled_lookup_hints = BTreeSet::new();

    for direct in direct_candidates {
        source_hints.insert(direct.display().to_string());

        if direct.exists() {
            if let Some(format) = source_format_hint(&direct, agent_hint) {
                register_lookup_keys(&direct, &mut handled_lookup_hints);
                resolved_sources.insert(
                    direct.clone(),
                    ResolvedSource {
                        path: direct,
                        format,
                    },
                );
            } else {
                missing_sources.insert(format!("unsupported: {}", direct.display()));
            }
            continue;
        }

        let lookup_key = direct
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_ascii_lowercase());
        match lookup_key
            .as_deref()
            .map(|key| locator.lookup(key))
            .unwrap_or(SourceLookupOutcome::Missing)
        {
            SourceLookupOutcome::Unique(path) => {
                if let Some(format) = source_format_hint(&path, agent_hint) {
                    register_lookup_keys(&direct, &mut handled_lookup_hints);
                    resolved_sources.insert(path.clone(), ResolvedSource { path, format });
                } else {
                    missing_sources.insert(format!("unsupported: {}", direct.display()));
                }
            }
            SourceLookupOutcome::Ambiguous(paths) => {
                ambiguous_sources.insert(format!(
                    "{} -> {}",
                    direct.display(),
                    display_paths(&paths)
                ));
            }
            SourceLookupOutcome::Missing => {
                missing_sources.insert(direct.display().to_string());
            }
        }
    }

    for hint in lookup_hints {
        if handled_lookup_hints.contains(&hint) {
            continue;
        }
        source_hints.insert(hint.clone());
        match locator.lookup(&hint) {
            SourceLookupOutcome::Unique(path) => {
                if let Some(format) = source_format_hint(&path, agent_hint) {
                    resolved_sources.insert(path.clone(), ResolvedSource { path, format });
                } else {
                    missing_sources.insert(format!("unsupported: {}", hint));
                }
            }
            SourceLookupOutcome::Ambiguous(paths) => {
                ambiguous_sources.insert(format!("{} -> {}", hint, display_paths(&paths)));
            }
            SourceLookupOutcome::Missing => {
                missing_sources.insert(hint);
            }
        }
    }

    SourceResolution {
        source_hints: source_hints.into_iter().collect(),
        resolved_sources: resolved_sources.into_values().collect(),
        missing_sources: missing_sources.into_iter().collect(),
        ambiguous_sources: ambiguous_sources.into_iter().collect(),
    }
}

fn collect_source_hints_from_text(
    text: &str,
    agent_hint: Option<&str>,
    direct_candidates: &mut BTreeSet<PathBuf>,
    lookup_hints: &mut BTreeSet<String>,
    source_hints: &mut BTreeSet<String>,
) {
    let absolute_path_re = Regex::new(
        r"(?:(?:file://)?(/(?:[A-Za-z0-9._~\-]+/)*[A-Za-z0-9._~\-]+(?:\.[A-Za-z0-9._~-]+)?))",
    )
    .expect("absolute legacy source hint regex should compile");
    let tilde_path_re =
        Regex::new(r"(~/(?:[A-Za-z0-9._~\-]+/)*[A-Za-z0-9._~\-]+(?:\.[A-Za-z0-9._~-]+)?)")
            .expect("tilde legacy source hint regex should compile");

    for capture in absolute_path_re.captures_iter(text) {
        if let Some(raw) = capture.get(1) {
            let candidate = PathBuf::from(raw.as_str());
            if source_format_hint(&candidate, agent_hint).is_some() {
                source_hints.insert(candidate.display().to_string());
                direct_candidates.insert(candidate);
            }
        }
    }

    for capture in tilde_path_re.captures_iter(text) {
        if let Some(raw) = capture.get(1) {
            let expanded = expand_tilde(raw.as_str());
            if source_format_hint(&expanded, agent_hint).is_some() {
                source_hints.insert(raw.as_str().to_string());
                direct_candidates.insert(expanded);
            }
        }
    }

    for hint_re in [
        Regex::new(r"\brollout-[A-Za-z0-9T._:-]+\.jsonl\b")
            .expect("codex rollout hint regex should compile"),
        Regex::new(r"\bsession-[A-Za-z0-9._-]+\.json\b")
            .expect("gemini session hint regex should compile"),
        Regex::new(r"\b[0-9a-fA-F-]{16,}\.pb\b").expect("antigravity pb hint regex should compile"),
        Regex::new(r"\b[0-9a-fA-F-]{16,}\.jsonl\b")
            .expect("claude jsonl hint regex should compile"),
    ] {
        for capture in hint_re.captures_iter(text) {
            if let Some(raw) = capture.get(0) {
                let hint = raw.as_str().to_ascii_lowercase();
                source_hints.insert(raw.as_str().to_string());
                lookup_hints.insert(hint);
            }
        }
    }
}

fn source_format_hint(path: &Path, agent_hint: Option<&str>) -> Option<SourceFormat> {
    let path_str = path.to_string_lossy().to_ascii_lowercase();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase())
        .unwrap_or_default();
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());

    match agent_hint {
        Some("claude") => {
            if extension.as_deref() == Some("jsonl")
                || extension.as_deref() == Some("output")
                || path_str.contains("/.claude/")
            {
                return Some(SourceFormat::Claude);
            }
            return None;
        }
        Some("codex") => {
            if extension.as_deref() == Some("jsonl")
                || file_name == "history.jsonl"
                || file_name.starts_with("rollout-")
                || path_str.contains("/.codex/")
            {
                return Some(SourceFormat::Codex);
            }
            return None;
        }
        Some("gemini") => {
            if extension.as_deref() == Some("pb")
                || path_str.contains("/antigravity/brain/")
                || path_str.contains("/antigravity/conversations/")
            {
                return Some(SourceFormat::GeminiAntigravity);
            }
            if extension.as_deref() == Some("json")
                && (file_name.starts_with("session-") || path_str.contains("/.gemini/tmp/"))
            {
                return Some(SourceFormat::Gemini);
            }
            return None;
        }
        Some(_) => return None,
        None => {}
    }

    if extension.as_deref() == Some("pb")
        || path_str.contains("/antigravity/brain/")
        || path_str.contains("/antigravity/conversations/")
    {
        return Some(SourceFormat::GeminiAntigravity);
    }

    if extension.as_deref() == Some("json")
        && (file_name.starts_with("session-") || path_str.contains("/.gemini/tmp/"))
    {
        return Some(SourceFormat::Gemini);
    }

    if extension.as_deref() == Some("jsonl")
        && (file_name.starts_with("rollout-")
            || file_name == "history.jsonl"
            || path_str.contains("/.codex/"))
    {
        return Some(SourceFormat::Codex);
    }

    if (extension.as_deref() == Some("jsonl") || extension.as_deref() == Some("output"))
        && path_str.contains("/.claude/")
    {
        return Some(SourceFormat::Claude);
    }

    None
}

fn execute_migration_items(
    legacy_root: &Path,
    store_root: &Path,
    items: &mut [LegacyItemPlan],
) -> Result<()> {
    let chunker_config = ChunkerConfig::default();
    let mut source_cache: HashMap<PathBuf, SourceProcessingOutcome> = HashMap::new();

    for item in items {
        item.execution = MigrationExecution::Executed;

        if matches!(
            item.action,
            MigrationAction::Rebuild | MigrationAction::RebuildAndSalvage
        ) {
            for source in item.resolved_sources.clone() {
                let outcome = source_cache.entry(source.path.clone()).or_insert_with(|| {
                    rebuild_source_into_store(store_root, &source, &chunker_config)
                });

                for path in &outcome.canonical_paths {
                    push_unique_path(&mut item.canonical_paths, path.clone());
                }
                if let Some(error) = &outcome.error {
                    item.errors
                        .push(format!("{}: {}", source.path.display(), error));
                }
            }
        }

        if item.action == MigrationAction::Rebuild && item.canonical_paths.is_empty() {
            item.errors
                .push("No canonical paths were written from resolved sources.".to_string());
        }

        let should_salvage = matches!(
            item.action,
            MigrationAction::Salvage | MigrationAction::RebuildAndSalvage
        ) || !item.errors.is_empty()
            || (item.action == MigrationAction::Rebuild && item.canonical_paths.is_empty());

        if should_salvage {
            let salvaged = preserve_legacy_item(legacy_root, store_root, item)?;
            item.salvage_paths = salvaged;
        }
    }

    Ok(())
}

fn rebuild_source_into_store(
    store_root: &Path,
    source: &ResolvedSource,
    chunker_config: &ChunkerConfig,
) -> SourceProcessingOutcome {
    match rebuild_source_into_store_impl(store_root, source, chunker_config) {
        Ok(canonical_paths) => SourceProcessingOutcome {
            canonical_paths,
            error: None,
        },
        Err(error) => SourceProcessingOutcome {
            canonical_paths: Vec::new(),
            error: Some(error.to_string()),
        },
    }
}

fn rebuild_source_into_store_impl(
    store_root: &Path,
    source: &ResolvedSource,
    chunker_config: &ChunkerConfig,
) -> Result<Vec<PathBuf>> {
    let entries = extract_entries_from_source(source)?;
    if entries.is_empty() {
        anyhow::bail!("source produced no timeline entries");
    }

    let summary = store_semantic_segments_at(store_root, &entries, chunker_config, |_, _| {})?;
    Ok(summary.written_paths)
}

fn extract_entries_from_source(source: &ResolvedSource) -> Result<Vec<TimelineEntry>> {
    let config = ExtractionConfig {
        project_filter: Vec::new(),
        cutoff: Utc
            .timestamp_opt(0, 0)
            .single()
            .expect("unix epoch should be representable"),
        include_assistant: true,
        watermark: None,
    };

    match source.format {
        SourceFormat::Claude => sources::extract_claude_file(&source.path, &config),
        SourceFormat::Codex => sources::extract_codex_file(&source.path, &config),
        SourceFormat::Gemini => sources::extract_gemini_file(&source.path, &config),
        SourceFormat::GeminiAntigravity => {
            sources::extract_gemini_antigravity_file(&source.path, &config)
        }
    }
}

fn preserve_legacy_item(
    legacy_root: &Path,
    store_root: &Path,
    item: &LegacyItemPlan,
) -> Result<Vec<PathBuf>> {
    let salvage_root = legacy_salvage_dir(store_root);
    fs::create_dir_all(&salvage_root)?;

    let mut preserved = Vec::new();
    for legacy_file in &item.legacy_files {
        let legacy_file = sanitize::validate_read_path(legacy_file)?;
        let relative = legacy_file
            .strip_prefix(legacy_root)
            .unwrap_or(legacy_file.as_path());
        let destination = salvage_root.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let destination = sanitize::validate_write_path(&destination)?;
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        fs::copy(legacy_file, &destination)?;
        preserved.push(destination);
    }

    let provenance_path = provenance_path_for_item(&salvage_root, legacy_root, item);
    if let Some(parent) = provenance_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let provenance_path = sanitize::validate_write_path(&provenance_path)?;
    let provenance = serde_json::json!({
        "generated_at": Utc::now().to_rfc3339(),
        "item_id": item.item_id.clone(),
        "legacy_group": item.legacy_group.clone(),
        "action": item.action,
        "action_reason": item.action_reason.clone(),
        "legacy_files": item.legacy_files.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "source_hints": item.source_hints.clone(),
        "existing_sources": item.resolved_sources.iter().map(|source| source.path.display().to_string()).collect::<Vec<_>>(),
        "missing_sources": item.missing_sources.clone(),
        "ambiguous_sources": item.ambiguous_sources.clone(),
        "errors": item.errors.clone(),
    });
    fs::write(&provenance_path, serde_json::to_string_pretty(&provenance)?)?;
    preserved.push(provenance_path);

    Ok(preserved)
}

fn provenance_path_for_item(
    salvage_root: &Path,
    legacy_root: &Path,
    item: &LegacyItemPlan,
) -> PathBuf {
    let anchor = if item.legacy_kind == LegacyItemKind::ContextBundle {
        PathBuf::from(&item.legacy_group)
    } else {
        item.legacy_files
            .first()
            .and_then(|path| path.strip_prefix(legacy_root).ok())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(&item.legacy_group))
    };

    let parent = anchor.parent().map(Path::to_path_buf).unwrap_or_default();
    let file_name = anchor
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "legacy-item".to_string());

    salvage_root
        .join(parent)
        .join(format!("{}.migration-provenance.json", file_name))
}

fn push_unique_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.iter().any(|path| path == &candidate) {
        paths.push(candidate);
    }
}

fn is_unclassified_item(item: &MigrationItem) -> bool {
    item.legacy_kind == LegacyItemKind::LooseFile
        || item.action_reason == "no_source_hints"
        || item.action_reason == "non_context_legacy_file"
}

fn looks_like_iso_date(value: &str) -> bool {
    value.len() == 10
        && value.chars().enumerate().all(|(idx, ch)| match idx {
            4 | 7 => ch == '-',
            _ => ch.is_ascii_digit(),
        })
}

fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }

    PathBuf::from(raw)
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn register_lookup_keys(path: &Path, handled_lookup_hints: &mut BTreeSet<String>) {
    if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
        handled_lookup_hints.insert(file_name.to_ascii_lowercase());
    }
    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
        handled_lookup_hints.insert(stem.to_ascii_lowercase());
    }
}

impl SourceLocator {
    fn from_home() -> Self {
        let Some(home) = dirs::home_dir() else {
            return Self::default();
        };

        let mut locator = Self::default();
        locator.index_recursive(home.join(".claude").join("projects"), |path| {
            matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("jsonl" | "output")
            )
        });
        locator.index_recursive(home.join(".codex").join("sessions"), |path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        });
        locator.index_file(home.join(".codex").join("history.jsonl"));
        locator.index_recursive(home.join(".gemini").join("tmp"), |path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("session-"))
        });
        locator.index_recursive(
            home.join(".gemini")
                .join("antigravity")
                .join("conversations"),
            |path| path.extension().and_then(|ext| ext.to_str()) == Some("pb"),
        );
        locator.index_directories(home.join(".gemini").join("antigravity").join("brain"));
        locator
    }

    fn lookup(&self, hint: &str) -> SourceLookupOutcome {
        let key = hint.to_ascii_lowercase();
        let Some(paths) = self.index.get(&key) else {
            return SourceLookupOutcome::Missing;
        };

        let unique: Vec<PathBuf> = paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        match unique.as_slice() {
            [] => SourceLookupOutcome::Missing,
            [only] => SourceLookupOutcome::Unique(only.clone()),
            many => SourceLookupOutcome::Ambiguous(many.to_vec()),
        }
    }

    fn index_recursive<F>(&mut self, root: PathBuf, include: F)
    where
        F: Fn(&Path) -> bool + Copy,
    {
        if !root.exists() {
            return;
        }

        let Ok(read_dir) = fs::read_dir(&root) else {
            return;
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.index_recursive(path, include);
                continue;
            }

            if include(&path) {
                self.add_path(&path);
            }
        }
    }

    fn index_directories(&mut self, root: PathBuf) {
        if !root.exists() {
            return;
        }

        let Ok(read_dir) = fs::read_dir(&root) else {
            return;
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.add_path(&path);
            }
        }
    }

    fn index_file(&mut self, path: PathBuf) {
        if path.exists() {
            self.add_path(&path);
        }
    }

    fn add_path(&mut self, path: &Path) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return;
        };

        let lower_name = name.to_ascii_lowercase();
        self.index
            .entry(lower_name.clone())
            .or_default()
            .push(path.to_path_buf());

        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            let lower_stem = stem.to_ascii_lowercase();
            if lower_stem != lower_name {
                self.index
                    .entry(lower_stem)
                    .or_default()
                    .push(path.to_path_buf());
            }
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use filetime::{FileTime, set_file_mtime};
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

    #[test]
    fn canonical_store_writes_sidecar_with_frontmatter_telemetry() {
        let root = retrieval_test_root("telemetry-sidecar");
        let _ = fs::remove_dir_all(&root);

        let entries = vec![TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 3, 27, 10, 0, 0).unwrap(),
            agent: "codex".to_string(),
            session_id: "sess-telemetry".to_string(),
            role: "assistant".to_string(),
            message: "---\nrun_id: mrbl-001\nprompt_id: api-redesign_20260327\nmodel: gpt-5.4\nstarted_at: 2026-03-27T10:00:00Z\ncompleted_at: 2026-03-27T10:01:00Z\ntoken_usage: 1234\nfindings_count: 4\nphase: implement\nmode: session-first\nskill_code: vc-workflow\nframework_version: 2026-03\n---\n## Findings\nTelemetry wiring landed.\n".to_string(),
            branch: None,
            cwd: None,
        }];

        let written = write_context_session_first_at(
            &root.join("store"),
            SessionWriteSpec {
                project: Some("VetCoders/ai-contexters"),
                agent: "codex",
                date: "2026-03-27",
                session_id: "sess-telemetry",
                kind: Some(Kind::Reports),
            },
            &entries,
            &ChunkerConfig::default(),
        )
        .expect("write canonical context");

        assert_eq!(written.len(), 1);
        let chunk_path = &written[0];
        assert!(chunk_path.exists());

        let content = fs::read_to_string(chunk_path).expect("read stored chunk");
        assert!(content.contains("## Findings"));
        assert!(!content.contains("run_id: mrbl-001"));
        assert!(!content.contains("mode: session-first"));

        let sidecar_path = chunk_path.with_extension("meta.json");
        assert!(sidecar_path.exists());

        let sidecar = load_sidecar(chunk_path).expect("load sidecar");
        assert_eq!(sidecar.run_id.as_deref(), Some("mrbl-001"));
        assert_eq!(sidecar.prompt_id.as_deref(), Some("api-redesign_20260327"));
        assert_eq!(sidecar.agent_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(sidecar.started_at.as_deref(), Some("2026-03-27T10:00:00Z"));
        assert_eq!(
            sidecar.completed_at.as_deref(),
            Some("2026-03-27T10:01:00Z")
        );
        assert_eq!(sidecar.token_usage, Some(1234));
        assert_eq!(sidecar.findings_count, Some(4));
        assert_eq!(sidecar.workflow_phase.as_deref(), Some("implement"));
        assert_eq!(sidecar.mode.as_deref(), Some("session-first"));
        assert_eq!(sidecar.skill_code.as_deref(), Some("vc-workflow"));
        assert_eq!(sidecar.framework_version.as_deref(), Some("2026-03"));

        let scanned = scan_context_files_at(&root).expect("scan canonical store");
        assert_eq!(scanned.len(), 1, "sidecar files must not scan as chunks");

        let matched = chunks_by_run_id_at(
            &root,
            "mrbl-001",
            Some("ai-contexters"),
            SystemTime::UNIX_EPOCH,
        )
        .expect("query by run id");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].path.file_name(), chunk_path.file_name());

        let _ = fs::remove_dir_all(&root);
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

        let summary =
            store_semantic_segments_at(&root, &entries, &ChunkerConfig::default(), |_, _| {})
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
    fn test_store_semantic_segments_reports_progress_per_segment() {
        let root = retrieval_test_root("segmentation-progress");
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

        let mut progress_updates = Vec::new();
        let summary = store_semantic_segments_at(
            &root,
            &entries,
            &ChunkerConfig::default(),
            |done, total| progress_updates.push((done, total)),
        )
        .expect("store semantic segments");

        assert_eq!(summary.total_entries, 4);
        assert_eq!(progress_updates, vec![(1, 3), (2, 3), (3, 3)]);

        let _ = fs::remove_dir_all(&root);
    }

    // ================================================================
    // Repo-centric retrieval tests
    // ================================================================

    fn retrieval_test_root(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "aicx-retrieval-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    fn write_chunk_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn set_mtime(path: &Path, unix_seconds: i64) {
        set_file_mtime(path, FileTime::from_unix_time(unix_seconds, 0)).unwrap();
    }

    #[test]
    fn scan_retrieves_repo_centric_files_with_correct_metadata() {
        let root = retrieval_test_root("repo-scan");
        let _ = fs::remove_dir_all(&root);

        // Create canonical repo-centric layout:
        // store/VetCoders/ai-contexters/2026_0321/conversations/claude/<file>.md
        let chunk_dir = root
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0321")
            .join("conversations")
            .join("claude");
        write_chunk_file(
            &chunk_dir.join("2026_0321_claude_sess-abc123_001.md"),
            "Decision: use repo-centric store layout",
        );

        let scanned = scan_context_files_at(&root).expect("scan should succeed");
        assert_eq!(scanned.len(), 1);

        let file = &scanned[0];
        assert_eq!(file.project, "VetCoders/ai-contexters");
        assert_eq!(file.agent, "claude");
        assert_eq!(file.kind, Kind::Conversations);
        assert_eq!(file.date_compact, "2026_0321");
        assert_eq!(file.date_iso, "2026-03-21");
        assert_eq!(file.session_id, "sess-abc123");
        assert_eq!(file.chunk, 1);
        assert!(file.repo.is_some());
        assert_eq!(
            file.repo.as_ref().unwrap().slug(),
            "VetCoders/ai-contexters"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_retrieves_non_repository_files_with_explicit_project_label() {
        let root = retrieval_test_root("non-repo-scan");
        let _ = fs::remove_dir_all(&root);

        // Create non-repository layout:
        // non-repository-contexts/2026_0321/plans/codex/<file>.md
        let chunk_dir = root
            .join("non-repository-contexts")
            .join("2026_0321")
            .join("plans")
            .join("codex");
        write_chunk_file(
            &chunk_dir.join("2026_0321_codex_sess-xyz789_001.md"),
            "Migration plan before repo identity is known",
        );

        let scanned = scan_context_files_at(&root).expect("scan should succeed");
        assert_eq!(scanned.len(), 1);

        let file = &scanned[0];
        assert_eq!(file.project, NON_REPOSITORY_CONTEXTS);
        assert_eq!(file.agent, "codex");
        assert_eq!(file.kind, Kind::Plans);
        assert!(file.repo.is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_retrieves_both_repo_and_non_repo_files_together() {
        let root = retrieval_test_root("combined-scan");
        let _ = fs::remove_dir_all(&root);

        // Repo-centric file
        let repo_dir = root
            .join("store")
            .join("VetCoders")
            .join("loctree")
            .join("2026_0320")
            .join("reports")
            .join("gemini");
        write_chunk_file(
            &repo_dir.join("2026_0320_gemini_sess-rpt001_001.md"),
            "## Report\nCoverage report for loctree scanner",
        );

        // Non-repo file
        let non_repo_dir = root
            .join("non-repository-contexts")
            .join("2026_0321")
            .join("other")
            .join("claude");
        write_chunk_file(
            &non_repo_dir.join("2026_0321_claude_sess-misc01_001.md"),
            "Unscoped brainstorm notes",
        );

        let scanned = scan_context_files_at(&root).expect("scan should succeed");
        assert_eq!(scanned.len(), 2);

        let repo_file = scanned.iter().find(|f| f.project == "VetCoders/loctree");
        let non_repo_file = scanned
            .iter()
            .find(|f| f.project == NON_REPOSITORY_CONTEXTS);

        assert!(repo_file.is_some(), "repo-centric file must be found");
        assert!(non_repo_file.is_some(), "non-repository file must be found");

        let repo_file = repo_file.unwrap();
        assert_eq!(repo_file.kind, Kind::Reports);
        assert_eq!(repo_file.agent, "gemini");
        assert!(repo_file.repo.is_some());

        let non_repo_file = non_repo_file.unwrap();
        assert_eq!(non_repo_file.kind, Kind::Other);
        assert_eq!(non_repo_file.agent, "claude");
        assert!(non_repo_file.repo.is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn context_files_since_uses_canonical_chunk_date_not_mtime() {
        let root = retrieval_test_root("context-files-since-date");
        let _ = fs::remove_dir_all(&root);

        let recent = root
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0331")
            .join("reports")
            .join("claude")
            .join("2026_0331_claude_sess-new_001.md");
        let old = root
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0328")
            .join("reports")
            .join("claude")
            .join("2026_0328_claude_sess-old_001.md");

        write_chunk_file(&recent, "Fresh canonical chunk");
        write_chunk_file(&old, "Stale canonical chunk");

        // Reverse the mtimes to prove recency follows the canonical store date.
        set_mtime(&recent, 1);
        set_mtime(&old, 2_000_000_000);

        let cutoff: SystemTime = Utc.with_ymd_and_hms(2026, 3, 30, 0, 0, 0).unwrap().into();
        let files = context_files_since_at(&root, cutoff, Some("ai-contexters"))
            .expect("context file filtering should succeed");

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].date_iso, "2026-03-31");
        assert_eq!(
            files[0]
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap(),
            "2026_0331_claude_sess-new_001.md"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_context_files_respects_aicxignore_and_negation() {
        let root = retrieval_test_root("context-files-ignore");
        let _ = fs::remove_dir_all(&root);

        let ignored = root
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0331")
            .join("reports")
            .join("codex")
            .join("2026_0331_codex_sess-rpt_001.md");
        let kept = root
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0331")
            .join("conversations")
            .join("codex")
            .join("2026_0331_codex_sess-conv_001.md");

        write_chunk_file(&ignored, "## Report\nIgnore this chunk");
        write_chunk_file(&kept, "Conversation that should remain visible");
        fs::write(
            root.join(AICX_IGNORE_FILENAME),
            "store/VetCoders/ai-contexters/**\n!store/VetCoders/ai-contexters/**/conversations/**\n",
        )
        .unwrap();

        let scanned = scan_context_files_at(&root).expect("ignore-aware scan should succeed");
        assert_eq!(scanned.len(), 1);
        assert_eq!(
            scanned[0]
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap(),
            "2026_0331_codex_sess-conv_001.md"
        );

        let raw = scan_context_files_raw_at(&root).expect("raw scan should succeed");
        assert_eq!(raw.len(), 2);

        let (filtered, ignored_count) =
            filter_ignored_paths_at(&root, &[ignored.clone(), kept.clone()])
                .expect("ignore filter should succeed");
        assert_eq!(ignored_count, 1);
        assert_eq!(filtered, vec![kept]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_ignore_matcher_rejects_traversal_base() {
        let root = retrieval_test_root("context-files-ignore-traversal");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join(AICX_IGNORE_FILENAME), "store/**\n").unwrap();

        let traversal_base = root.join("nested").join("..");
        let err = load_ignore_matcher_at(&traversal_base)
            .expect_err("traversal base should be rejected by validated read");
        let message = err.to_string().to_lowercase();
        assert!(message.contains("traversal"), "unexpected error: {err:#}");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migration_rebuilds_existing_sources_into_canonical_store() {
        let root = migration_test_root("rebuild-canonical");
        let legacy_root = root.join("legacy");
        let store_root = root.join("aicx");
        let repo_root = root.join("hosted").join("VetCoders").join("ai-contexters");
        let source = root
            .join("sources")
            .join("rollout-rebuild-canonical-019be5e4.jsonl");
        let _ = fs::remove_dir_all(&root);

        fs::create_dir_all(&repo_root).unwrap();
        fs::create_dir_all(repo_root.join(".git")).unwrap();
        fs::create_dir_all(legacy_root.join("demo").join("2026-03-21")).unwrap();
        write_codex_history(
            &source,
            "sess-rebuild",
            Some(repo_root.to_string_lossy().as_ref()),
            &[
                ("user", 1_742_560_000, "Please inspect the migration seam."),
                (
                    "assistant",
                    1_742_560_060,
                    "Reviewing the repo-centric store now.",
                ),
            ],
        );

        write_text(
            &legacy_root
                .join("demo")
                .join("2026-03-21")
                .join("101045_codex-001.md"),
            &format!("input: {}\n", source.display()),
        );

        let manifest =
            run_migration_at(&legacy_root, &store_root, false, &SourceLocator::default())
                .expect("run migration");

        assert_eq!(manifest.totals.rebuild_items, 1);
        assert_eq!(manifest.totals.rebuilt_items, 1);
        assert_eq!(manifest.totals.salvaged_items, 0);
        assert!(manifest.items.iter().any(|item| {
            item.canonical_paths.iter().any(|path| {
                path.contains("/store/VetCoders/ai-contexters/2025_0321/conversations/codex/")
            })
        }));
        assert!(
            store_root
                .join("store")
                .join("VetCoders")
                .join("ai-contexters")
                .join("2025_0321")
                .join("conversations")
                .join("codex")
                .is_dir()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migration_salvages_legacy_bundle_when_source_is_missing() {
        let root = migration_test_root("salvage-missing");
        let legacy_root = root.join("legacy");
        let store_root = root.join("aicx");
        let missing_source = root
            .join("sources")
            .join("rollout-missing-source-019be5e4.jsonl");
        let _ = fs::remove_dir_all(&root);

        write_text(
            &legacy_root
                .join("demo")
                .join("2026-03-21")
                .join("101045_codex-001.md"),
            &format!("input: {}\n", missing_source.display()),
        );

        let manifest =
            run_migration_at(&legacy_root, &store_root, false, &SourceLocator::default())
                .expect("run migration");
        let item = manifest.items.first().expect("migration item");

        assert_eq!(item.action, MigrationAction::Salvage);
        assert_eq!(item.action_reason, "missing_source");
        assert!(item.canonical_paths.is_empty());
        assert!(
            item.salvage_paths
                .iter()
                .any(|path| { path.contains("/legacy-store/demo/2026-03-21/101045_codex-001.md") })
        );
        assert!(item.salvage_paths.iter().any(|path| {
            path.contains("/legacy-store/demo/2026-03-21/101045_codex.migration-provenance.json")
        }));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migration_writes_manifest_report_and_non_repo_rebuilds() {
        let root = migration_test_root("manifest-report");
        let legacy_root = root.join("legacy");
        let store_root = root.join("aicx");
        let source = root.join("sources").join("rollout-non-repo-019be5e4.jsonl");
        let _ = fs::remove_dir_all(&root);

        write_codex_history(
            &source,
            "sess-non-repo",
            None,
            &[
                (
                    "user",
                    1_742_560_000,
                    "Draft a migration plan before we know the repo.",
                ),
                (
                    "assistant",
                    1_742_560_060,
                    "Working in non-repository mode for now.",
                ),
            ],
        );
        write_text(
            &legacy_root
                .join("demo")
                .join("2026-03-21")
                .join("101045_codex-001.md"),
            &format!("input: {}\n", source.display()),
        );
        write_text(&legacy_root.join("state.json"), "{\"seen_hashes\":[]}");

        let manifest =
            run_migration_at(&legacy_root, &store_root, false, &SourceLocator::default())
                .expect("run migration");
        let report = fs::read_to_string(&manifest.report_path).expect("read report");
        let manifest_json =
            fs::read_to_string(&manifest.manifest_path).expect("read manifest json");

        assert!(manifest.items.iter().any(|item| {
            item.canonical_paths.iter().any(|path| {
                path.contains("/non-repository-contexts/2025_0321/conversations/codex/")
            })
        }));
        assert!(manifest.items.iter().any(|item| {
            item.action_reason == "non_context_legacy_file"
                && item
                    .salvage_paths
                    .iter()
                    .any(|path| path.contains("/legacy-store/state.json"))
        }));
        assert!(report.contains("## Rebuilt"));
        assert!(report.contains("## Unclassified Legacy Items"));
        assert!(report.contains("non_context_legacy_file"));
        assert!(manifest_json.contains("\"report_path\""));
        assert!(PathBuf::from(&manifest.report_path).exists());
        assert!(PathBuf::from(&manifest.manifest_path).exists());

        let _ = fs::remove_dir_all(&root);
    }

    fn migration_test_root(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "aicx-migration-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    fn write_text(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn write_codex_history(
        path: &Path,
        session_id: &str,
        cwd: Option<&str>,
        records: &[(&str, i64, &str)],
    ) {
        let mut lines = Vec::new();
        for (role, ts, text) in records {
            lines.push(
                serde_json::json!({
                    "session_id": session_id,
                    "text": text,
                    "ts": ts,
                    "role": role,
                    "cwd": cwd,
                })
                .to_string(),
            );
        }

        write_text(path, &lines.join("\n"));
    }
}
