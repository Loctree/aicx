//! Central context store for ai-contexters.
//!
//! Manages the `~/.aicx/` directory structure:
//! - `store/<organization>/<repository>/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md`
//! - `non-repository-contexts/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md`
//! - `store/<project>/<date>/<time>_<agent>-context.{md,json}` — legacy monolithic helpers kept for library use/tests
//! - `chunks/` — the base location for chunk content
//! - `index.json` — manifest of stored contexts
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub(crate) mod atomic_write;
use atomic_write::atomic_write;

pub mod canonical_projection;
pub use canonical_projection::{
    CANONICAL_PROJECTION_DIRNAME, CanonicalStoreManifest, read_canonical_projection_at,
    write_canonical_projection_at,
};

use crate::chunker::{self, ChunkerConfig};
use crate::sanitize;
use crate::segmentation::semantic_segments;
use crate::timeline::{RepoIdentity, SemanticSegment, TimelineEntry};
pub use aicx_parser::{classify_kind, timeline::Kind};

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
/// UUIDv7 IDs share a time-dominated prefix; truncating to 12 hex chars
/// makes basename collisions between near-in-time sessions plausible. We
/// keep up to 20 cleaned chars, and append a 6-hex SipHash-1-3 suffix of
/// the original ID when truncation actually drops information so the
/// basename remains collision-resistant.
fn truncate_session_id(session_id: &str) -> String {
    let cleaned: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    const LIMIT: usize = 20;
    if cleaned.len() <= LIMIT {
        return cleaned;
    }
    format!("{}-h{}", &cleaned[..LIMIT], siphash13_hex6(session_id))
}

/// Stable 6-char hex of `input` via SipHash-1-3 with default (zero) key.
/// 24 bits of disambiguation — collision probability ~2^-24 for unrelated
/// inputs, sufficient for basename suffix disambiguation.
fn siphash13_hex6(input: &str) -> String {
    use siphasher::sip::SipHasher13;
    use std::hash::{Hash, Hasher};
    let mut hasher = SipHasher13::new();
    input.hash(&mut hasher);
    format!("{:06x}", (hasher.finish() & 0x00FF_FFFF) as u32)
}

fn chunk_sequence_from_id(id: &str) -> Option<u32> {
    id.rsplit('_').next().and_then(parse_chunk_component)
}

// ============================================================================
// Path helpers
// ============================================================================

pub(crate) mod dedupe;
pub(crate) mod ignore;
pub(crate) mod paths;
pub(crate) mod sidecar;

pub use dedupe::content_sha256_exists_in_dir;
use dedupe::{DirShaCache, content_sha256, sha256_of_file};

pub use ignore::{
    AICX_IGNORE_FILENAME, StoreIgnoreMatcher, filter_ignored_paths_at, load_ignore_matcher_at,
};
use paths::aicx_context_corpus_dir_for;
pub(crate) use paths::canonical_project_slug;
use paths::validated_store_project_dir;
pub use paths::{
    CANONICAL_STORE_DIRNAME, CONTEXT_CORPUS_DIRNAME, CONTEXT_CORPUS_SCHEMA_VERSION,
    LEGACY_SALVAGE_DIRNAME, LOCT_CONTEXT_PACK_FAMILY, NON_REPOSITORY_CONTEXTS,
    OWNERLESS_PROJECT_ORGANIZATION, aicx_context_corpus_dir, canonical_store_dir, chunks_dir,
    chunks_dir_for, context_corpus_root_dir, get_context_json_path, get_context_path,
    legacy_store_base_dir, non_repository_contexts_dir, project_dir, resolve_aicx_home,
    store_base_dir, store_base_dir_for,
};
use sidecar::load_sidecar_from_path;
pub use sidecar::{is_context_corpus_sidecar, load_sidecar, sidecar_path_for_chunk};

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
/// Returns a default empty index if the file doesn't exist. If the file
/// exists but cannot be read or parsed (and no `.bak` sibling rescues it),
/// emits a `tracing::warn!` and still returns default to preserve the
/// public `StoreIndex` API; callers that need fail-fast semantics on a
/// corrupt index should call `load_index_at` directly.
pub fn load_index() -> StoreIndex {
    let base = match store_base_dir() {
        Ok(dir) => dir,
        Err(_) => return StoreIndex::default(),
    };
    let lock_path = match crate::locks::index_lock_path() {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!("failed to resolve index lock path: {err}");
            return StoreIndex::default();
        }
    };
    let _lock = match crate::locks::acquire_shared(lock_path) {
        Ok(lock) => lock,
        Err(err) => {
            tracing::warn!("failed to acquire shared index lock: {err}");
            return StoreIndex::default();
        }
    };
    match load_index_at(&base) {
        Ok(idx) => idx,
        Err(err) => {
            tracing::warn!("failed to load store index (returning empty default): {err:#}");
            StoreIndex::default()
        }
    }
}

fn load_index_at(base: &Path) -> Result<StoreIndex> {
    let path = base.join("index.json");
    if !path.exists() {
        return Ok(StoreIndex::default());
    }

    match read_and_parse_index(&path) {
        Ok(idx) => Ok(idx),
        Err(primary_err) => {
            let bak_path = path.with_extension("json.bak");
            tracing::warn!(
                path = %path.display(),
                bak = %bak_path.display(),
                "store index corrupt or unreadable ({primary_err:#}); attempting .bak recovery"
            );
            if bak_path.exists() {
                match read_and_parse_index(&bak_path) {
                    Ok(idx) => {
                        tracing::warn!("recovered store index from {}", bak_path.display());
                        return Ok(idx);
                    }
                    Err(bak_err) => {
                        return Err(anyhow!(
                            "store index unreadable and .bak fallback also failed (primary: {primary_err:#}; bak: {bak_err:#})"
                        ));
                    }
                }
            }
            Err(primary_err.context(format!(
                "store index unreadable and no .bak sibling at {}",
                bak_path.display()
            )))
        }
    }
}

fn read_and_parse_index(path: &Path) -> Result<StoreIndex> {
    let contents = sanitize::read_to_string_validated(path)
        .with_context(|| format!("read failed: {}", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("parse failed: {}", path.display()))
}

/// Persist the store index to disk.
pub fn save_index(index: &StoreIndex) -> Result<()> {
    let base = store_base_dir()?;
    let lock = crate::locks::acquire_exclusive(crate::locks::index_lock_path()?)?;
    let result = save_index_at(&base, index);
    crate::locks::release(lock);
    result
}

fn save_index_at(base: &Path, index: &StoreIndex) -> Result<()> {
    let path = base.join("index.json");
    let json = serde_json::to_string_pretty(index).context("Failed to serialize index")?;

    // Best-effort: snapshot the previous index to `.bak` BEFORE the swap so a
    // crash mid-write still leaves a recoverable copy. Open the source once
    // and stream from that FD to avoid path re-resolution between exists/copy.
    let bak = path.with_extension("json.bak");
    match fs::OpenOptions::new().read(true).open(&path) {
        Ok(mut src) => {
            let copy_result: Result<u64> = (|| {
                let mut dst = sanitize::create_file_validated(&bak)?;
                std::io::copy(&mut src, &mut dst)
                    .with_context(|| format!("copy {} -> {}", path.display(), bak.display()))
            })();
            if let Err(err) = copy_result {
                tracing::warn!(
                    src = %path.display(),
                    dst = %bak.display(),
                    "failed to snapshot index to .bak before save: {err}"
                );
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(
                src = %path.display(),
                dst = %bak.display(),
                "failed to open index before .bak snapshot: {err}"
            );
        }
    }

    atomic_write(&path, json.as_bytes())
        .with_context(|| format!("Failed to write index: {}", path.display()))?;
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

    let project_idx = index
        .projects
        .entry(canonical_project_slug(project))
        .or_default();

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

#[derive(Debug, Clone, Serialize)]
pub struct ReadContextChunk {
    pub path: PathBuf,
    pub relative_path: String,
    pub project: String,
    pub date: String,
    pub kind: String,
    pub agent: String,
    pub session_id: String,
    pub chunk: u32,
    pub bytes: u64,
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkRefSpec {
    Path(PathBuf),
    Id(String),
    LegacyCompact(String),
}

impl ChunkRefSpec {
    pub fn parse(reference: &str) -> Result<Self> {
        let reference = reference.trim();
        if reference.is_empty() {
            return Err(anyhow!("chunk reference is required"));
        }

        if let Some(id) = reference.strip_prefix("chunk:") {
            let id = id.trim();
            if !is_chunk_id_prefix(id) {
                return Err(anyhow!("invalid chunk reference id: {reference}"));
            }
            return Ok(Self::Id(id.to_ascii_lowercase()));
        }

        if is_chunk_id_prefix(reference) {
            return Ok(Self::Id(reference.to_ascii_lowercase()));
        }

        if reference.contains('|') {
            return Ok(Self::LegacyCompact(reference.to_string()));
        }

        Ok(Self::Path(PathBuf::from(reference)))
    }
}

#[derive(Debug, Clone, Default)]
pub struct StoreWriteSummary {
    pub total_entries: usize,
    pub written_paths: Vec<PathBuf>,
    pub skipped_empty_body: usize,
    pub deduped_chunks: usize,
    pub project_summary: BTreeMap<String, BTreeMap<String, usize>>,
}

#[derive(Debug, Clone, Default)]
struct SessionWriteOutcome {
    written_paths: Vec<PathBuf>,
    written_date_counts: BTreeMap<String, usize>,
    skipped_empty_body: usize,
    deduped_chunks: usize,
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
///
/// When the card mill is disabled (default operator binary), returns an empty
/// vector and creates nothing — dual-body silence for the catalog era.
pub fn write_context(
    project: &str,
    agent: &str,
    date: &str,
    time: &str,
    entries: &[TimelineEntry],
) -> Result<Vec<PathBuf>> {
    if !card_mill_writes_enabled() {
        return Ok(Vec::new());
    }
    let project = canonical_project_slug(project);
    let mut written = Vec::new();

    // Markdown
    let md_path = get_context_path(&project, agent, date, time)?;
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
    atomic_write(&write_path, md_content.as_bytes())?;
    written.push(md_path);

    // JSON
    let json_path = get_context_json_path(&project, agent, date, time)?;
    let json_content = serde_json::to_string_pretty(entries)?;
    let write_path = sanitize::validate_write_path(&json_path)?;
    atomic_write(&write_path, json_content.as_bytes())?;
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
///
/// When the card mill is disabled (default operator binary), returns an empty
/// vector and creates nothing.
pub fn write_context_chunked(
    project: &str,
    agent: &str,
    date: &str,
    time: &str,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
) -> Result<Vec<PathBuf>> {
    if !card_mill_writes_enabled() {
        return Ok(vec![]);
    }
    if entries.is_empty() {
        return Ok(vec![]);
    }

    let project = canonical_project_slug(project);
    let chunks = chunker::chunk_entries(entries, &project, agent, chunker_config);
    let dir = validated_store_project_dir(&canonical_store_dir()?, &project)?.join(date);
    fs::create_dir_all(&dir)?;

    let mut written = Vec::new();

    for chunk in &chunks {
        // Extract seq from chunk.id (last _NNN part)
        let seq = chunk.id.rsplit('_').next().unwrap_or("001");

        let filename = format!("{}_{}-{}.md", time, agent, seq);
        let path = dir.join(&filename);

        let write_path = sanitize::validate_write_path(&path)?;
        atomic_write(&write_path, chunk.text.as_bytes())?;
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
///
/// When the card mill is disabled (default operator binary), returns an empty
/// vector and creates nothing. Migration/salvage must use
/// [`store_semantic_segments_at_forced`] instead of this public helper.
pub fn write_context_session_first(
    project: &str,
    agent: &str,
    date: &str,
    session_id: &str,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
    kind: Option<Kind>,
) -> Result<Vec<PathBuf>> {
    if !card_mill_writes_enabled() {
        return Ok(Vec::new());
    }
    let mut sha_cache = DirShaCache::default();
    Ok(write_context_session_first_outcome_at(
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
        &mut sha_cache,
    )?
    .written_paths)
}

#[cfg(all(test, feature = "app"))]
fn write_context_session_first_at(
    root: &Path,
    spec: SessionWriteSpec<'_>,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
) -> Result<Vec<PathBuf>> {
    let mut sha_cache = DirShaCache::default();
    Ok(
        write_context_session_first_outcome_at(
            root,
            spec,
            entries,
            chunker_config,
            &mut sha_cache,
        )?
        .written_paths,
    )
}

fn write_context_session_first_outcome_at(
    root: &Path,
    spec: SessionWriteSpec<'_>,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
    sha_cache: &mut DirShaCache,
) -> Result<SessionWriteOutcome> {
    if entries.is_empty() {
        return Ok(SessionWriteOutcome::default());
    }

    let kind = spec.kind.unwrap_or_else(|| classify_kind(entries));
    let project_label = spec
        .project
        .map(canonical_project_slug)
        .unwrap_or_else(|| NON_REPOSITORY_CONTEXTS.to_string());
    let chunks = chunker::chunk_entries(entries, &project_label, spec.agent, chunker_config);

    let mut outcome = SessionWriteOutcome::default();

    for (idx, chunk) in chunks.iter().enumerate() {
        if chunk_body_is_empty(&chunk.text) {
            outcome.skipped_empty_body += 1;
            continue;
        }
        let chunk_date = if chunk.date.trim().is_empty() {
            spec.date
        } else {
            chunk.date.as_str()
        };
        let date_dir = compact_date(chunk_date);
        let chunk_num = chunk_sequence_from_id(&chunk.id).unwrap_or((idx as u32) + 1);
        let mut dir = root.join(&date_dir).join(kind.dir_name()).join(spec.agent);
        if spec.project.is_some() {
            dir = validated_store_project_dir(root, &project_label)?
                .join(&date_dir)
                .join(kind.dir_name())
                .join(spec.agent);
        }
        fs::create_dir_all(&dir)?;

        let filename = session_basename(chunk_date, spec.agent, spec.session_id, chunk_num);
        let path = dir.join(&filename);
        let content_sha256 = content_sha256(&chunk.text);
        if sha_cache.contains(&dir, &content_sha256)? {
            outcome.deduped_chunks += 1;
            continue;
        }

        // Basename collision precheck. UUIDv7 prefix sessions can land on the
        // same `session_basename` even after siphash suffix in pathological
        // cases (different inputs, same suffix). If the target already exists
        // with a different `content_sha256`, disambiguate via a `-c{hex}`
        // suffix derived from the new content hash so the existing chunk is
        // never silently overwritten.
        //
        // Orphan handling (#20): if the `.md` is present but its `.meta.json`
        // sidecar is missing, the prior two-phase write was killed between the
        // two renames. The prior policy silently spawned a `-c<hash>` shadow
        // and left the orphan in place forever, so the canonical basename was
        // permanently shadowed and operators saw duplicate-looking chunks.
        // Now we either reclaim the orphan (its on-disk body already matches
        // the new chunk — just write the missing sidecar) or quarantine it
        // (different body — move under `dir/quarantine/` so the canonical
        // slot is free for the new pair).
        let target_path = if path.exists() {
            let existing_sidecar = path.with_extension("meta.json");
            if !existing_sidecar.exists() {
                let orphan_sha = sha256_of_file(&path)?;
                if orphan_sha == content_sha256 {
                    let mut sidecar = chunker::ChunkMetadataSidecar::from(chunk);
                    sidecar.content_sha256 = Some(content_sha256.clone());
                    let sidecar_bytes = serde_json::to_vec_pretty(&sidecar)?;
                    let sidecar_write = sanitize::validate_write_path(&existing_sidecar)?;
                    atomic_write(&sidecar_write, &sidecar_bytes)?;
                    sha_cache.insert(&dir, content_sha256);
                    tracing::info!(
                        target: "aicx::store",
                        orphan = %path.display(),
                        "reclaimed orphan chunk by writing missing sidecar"
                    );
                    outcome.deduped_chunks += 1;
                    continue;
                }
                let quarantine_dir = dir.join("quarantine");
                fs::create_dir_all(&quarantine_dir)?;
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("chunk");
                let quar_path = quarantine_dir.join(format!("{}-orphan-{}.md", stem, stamp));
                fs::rename(&path, &quar_path).with_context(|| {
                    format!(
                        "Failed to quarantine orphan {} -> {}",
                        path.display(),
                        quar_path.display()
                    )
                })?;
                atomic_write::parent_fsync(&path);
                atomic_write::parent_fsync(&quar_path);
                tracing::warn!(
                    target: "aicx::store",
                    orphan = %path.display(),
                    quarantine = %quar_path.display(),
                    orphan_sha = %orphan_sha,
                    new_sha = %content_sha256,
                    "quarantined orphan .md (sidecar missing, body mismatch) to free canonical slot"
                );
                path
            } else {
                let existing_sha =
                    load_sidecar_from_path(&existing_sidecar).and_then(|s| s.content_sha256);
                if existing_sha.as_deref() == Some(content_sha256.as_str()) {
                    outcome.deduped_chunks += 1;
                    continue;
                }
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("chunk");
                let disambig =
                    dir.join(format!("{}-c{}.md", stem, siphash13_hex6(&content_sha256)));
                tracing::warn!(
                    target: "aicx::store",
                    existing = %path.display(),
                    disambiguated = %disambig.display(),
                    existing_sha = ?existing_sha,
                    "session-first chunk basename collision; writing under disambiguated path"
                );
                disambig
            }
        } else {
            path
        };

        let write_path = sanitize::validate_write_path(&target_path)?;
        let sidecar_path = target_path.with_extension("meta.json");
        let sidecar_write_path = sanitize::validate_write_path(&sidecar_path)?;

        let mut sidecar = chunker::ChunkMetadataSidecar::from(chunk);
        sidecar.content_sha256 = Some(content_sha256.clone());
        let sidecar_bytes = serde_json::to_vec_pretty(&sidecar)?;

        // Two-phase commit: stage both tempfiles, then rename in order
        // (.md first, .meta.json second). A crash between renames leaves an
        // orphan .md without sidecar — detectable and recoverable — instead
        // of an orphan .md with a stale or absent sidecar.
        let chunk_tmp = atomic_write::stage_tempfile(&write_path, chunk.text.as_bytes())?;
        let sidecar_tmp = match atomic_write::stage_tempfile(&sidecar_write_path, &sidecar_bytes) {
            Ok(tmp) => tmp,
            Err(err) => {
                atomic_write::discard_tempfile(&chunk_tmp);
                return Err(err.into());
            }
        };
        if let Err(err) = atomic_write::commit_tempfile(&chunk_tmp, &write_path) {
            atomic_write::discard_tempfile(&chunk_tmp);
            atomic_write::discard_tempfile(&sidecar_tmp);
            return Err(err.into());
        }
        if let Err(err) = atomic_write::commit_tempfile(&sidecar_tmp, &sidecar_write_path) {
            atomic_write::discard_tempfile(&sidecar_tmp);
            return Err(err.into());
        }
        // Mirror `atomic_write`'s parent-dir fsync (#21): the two-phase
        // rename above goes through `commit_tempfile` directly, so
        // `atomic_write::atomic_write` never gets to run its own
        // post-rename sync. Without this, chunk + sidecar persistence on
        // power-loss-sensitive filesystems was weaker than the contract
        // used by every single-file `atomic_write` call. The two rename
        // targets live in the same parent dir, so one fsync covers both;
        // we add a defensive second call when paths diverge in unusual
        // tests.
        atomic_write::parent_fsync(&write_path);
        if write_path.parent() != sidecar_write_path.parent() {
            atomic_write::parent_fsync(&sidecar_write_path);
        }
        sha_cache.insert(&dir, content_sha256);
        *outcome
            .written_date_counts
            .entry(date_dir.clone())
            .or_default() += 1;
        outcome.written_paths.push(target_path);
    }

    Ok(outcome)
}

pub(crate) fn chunk_body_is_empty(content: &str) -> bool {
    !crate::card_header::card_body(content)
        .lines()
        .any(chunk_line_has_signal)
}

fn chunk_line_has_signal(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    if let Some((_, rest)) = line.split_once("] ")
        && let Some((_, message)) = rest.split_once(':')
    {
        return !message.trim().is_empty();
    }
    true
}

/// Whether the per-frame card mill may write under a store root.
///
/// **Always false for the operator binary.** The mill concept is deleted:
/// identity is catalog + extracts + source-driven index. Library unit tests
/// (`cfg(test)`) still exercise the write helpers so historical contracts can
/// be migrated off the mill without reopening an operator salvage env.
///
/// There is no `AICX_ALLOW_CARD_MILL` escape hatch.
pub fn card_mill_writes_enabled() -> bool {
    cfg!(test)
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

pub fn store_semantic_segments_at<F>(
    base: &Path,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
    progress: F,
) -> Result<StoreWriteSummary>
where
    F: FnMut(usize, usize),
{
    if entries.is_empty() {
        return Ok(StoreWriteSummary::default());
    }
    let segments = semantic_segments(entries);
    store_segments_at(base, &segments, chunker_config, progress)
}

/// Legacy force-write entry used by migration tests only.
///
/// Outside `cfg(test)` this is identical to [`store_semantic_segments_at`]:
/// the card mill does not write. There is no operator salvage path.
pub fn store_semantic_segments_at_forced<F>(
    base: &Path,
    entries: &[TimelineEntry],
    chunker_config: &ChunkerConfig,
    progress: F,
) -> Result<StoreWriteSummary>
where
    F: FnMut(usize, usize),
{
    store_semantic_segments_at(base, entries, chunker_config, progress)
}

/// Write pre-computed [`SemanticSegment`]s to the canonical store. This
/// is the underlying primitive — callers that already paid for
/// segmentation (e.g. the CLI's phased pipeline that emits a
/// `segment`-phase heartbeat before the first `.md` write) reuse those
/// segments here instead of re-segmenting from raw entries.
///
/// When the card mill is disabled (default), returns an empty summary and
/// creates no directories — dual-body silence for catalog-era operators.
pub fn store_segments_at<F>(
    base: &Path,
    segments: &[SemanticSegment],
    chunker_config: &ChunkerConfig,
    progress: F,
) -> Result<StoreWriteSummary>
where
    F: FnMut(usize, usize),
{
    if !card_mill_writes_enabled() {
        return Ok(StoreWriteSummary::default());
    }
    store_segments_at_impl(base, segments, chunker_config, progress)
}

/// Legacy force-write entry. Outside tests the mill is off; see
/// [`store_segments_at`].
pub fn store_segments_at_forced<F>(
    base: &Path,
    segments: &[SemanticSegment],
    chunker_config: &ChunkerConfig,
    progress: F,
) -> Result<StoreWriteSummary>
where
    F: FnMut(usize, usize),
{
    store_segments_at(base, segments, chunker_config, progress)
}

fn store_segments_at_impl<F>(
    base: &Path,
    segments: &[SemanticSegment],
    chunker_config: &ChunkerConfig,
    mut progress: F,
) -> Result<StoreWriteSummary>
where
    F: FnMut(usize, usize),
{
    let mut summary = StoreWriteSummary::default();
    if segments.is_empty() {
        return Ok(summary);
    }

    let _lock = crate::locks::acquire_exclusive(base.join("locks").join("index.lock"))?;
    let total_segments = segments.len();
    // Save-on-drop RAII guard (#26): `index.json` used to be persisted
    // only at the end of the loop, so Ctrl+C / panic between the first
    // segment write and the loop tail left the on-disk index out of sync
    // with newly-stored chunks. The guard wraps the in-memory index and
    // calls `save_index_at` on every code path — successful completion
    // sets `persisted = true` so `Drop` becomes a no-op, and any early
    // return (`?`) or panic fires `Drop`, which writes the index
    // opportunistically before the surrounding lock is released.
    let mut guard = IndexSaveGuard {
        base,
        index: load_index_at(base)?,
        persisted: false,
    };
    let mut sha_cache = DirShaCache::default();

    for (segment_idx, segment) in segments.iter().enumerate() {
        let date = segment
            .entries
            .first()
            .map(|entry| entry.timestamp.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
        let project = canonical_project_slug(&segment.project_label());

        let outcome =
            write_semantic_segment_at(base, segment, &date, chunker_config, &mut sha_cache)?;
        summary.skipped_empty_body += outcome.skipped_empty_body;
        summary.deduped_chunks += outcome.deduped_chunks;

        // Two separate counters with two separate semantics:
        //
        // 1. `summary.total_entries` and `summary.project_summary` are
        //    "this run processed N entries through the pipeline" —
        //    used by CLI/JSON output that operators (and the
        //    `runtime_cli_store_contract` test) expect to reflect the
        //    full pipeline cost, regardless of whether the chunks
        //    landed on disk or were dedup-skipped.
        //
        // 2. `update_index(...)` writes the on-disk-truth counter to
        //    `index.json`. THAT one is proportional to chunks actually
        //    written, so a `--full-rescan` over an already-stored
        //    corpus doesn't pump the index counter on every run when
        //    `write_context_session_first_outcome_at` short-circuits
        //    every chunk on content_sha256 dedup. This is the
        //    bug #1 fix from PR #7 — index reflects what's on disk,
        //    not what the pipeline touched.
        //
        // Earlier in PR #7 these two semantics were collapsed (both
        // proportional) which broke the contract test
        // `store_cli_defaults_to_incremental_and_full_rescan_recovers_backfill`.
        let chunks_written = outcome.written_paths.len();
        let chunks_total = chunks_written + outcome.deduped_chunks + outcome.skipped_empty_body;
        let entries_committed_to_disk = if chunks_total == 0 || chunks_written == 0 {
            0
        } else {
            // Round-half-up integer division so a one-chunk-written
            // segment doesn't truncate to 0 entries.
            (segment.entries.len() * chunks_written + chunks_total / 2) / chunks_total
        };

        // Pipeline-processed counter (full segment entry count) —
        // operator-facing CLI/JSON output + project_summary breakdown.
        *summary
            .project_summary
            .entry(project.clone())
            .or_default()
            .entry(segment.agent.clone())
            .or_insert(0) += segment.entries.len();
        summary.total_entries += segment.entries.len();

        // On-disk-truth counter (proportional to chunks actually
        // written) — `index.json` only.
        if entries_committed_to_disk > 0 {
            if outcome.written_date_counts.is_empty() {
                update_index(
                    &mut guard.index,
                    &project,
                    &segment.agent,
                    &compact_date(&date),
                    entries_committed_to_disk,
                );
            } else {
                let total_written: usize = outcome.written_date_counts.values().sum();
                let mut remaining_entries = entries_committed_to_disk;
                let mut remaining_dates = outcome.written_date_counts.len();
                for (date, chunks_for_date) in &outcome.written_date_counts {
                    let entry_count = if remaining_dates == 1 {
                        remaining_entries
                    } else {
                        let proportional =
                            entries_committed_to_disk * chunks_for_date / total_written;
                        let count = proportional.max(1).min(remaining_entries);
                        remaining_entries = remaining_entries.saturating_sub(count);
                        remaining_dates -= 1;
                        count
                    };
                    update_index(
                        &mut guard.index,
                        &project,
                        &segment.agent,
                        date,
                        entry_count,
                    );
                }
            }
        }
        summary.written_paths.extend(outcome.written_paths);
        progress(segment_idx + 1, total_segments);
    }

    save_index_at(base, &guard.index)?;
    guard.persisted = true;
    Ok(summary)
}

/// RAII save-on-drop guard for the in-memory store index (#26).
///
/// Holds the index by value while `store_segments_at` mutates it. On
/// successful completion the caller sets `persisted = true` after a
/// regular `save_index_at` and `Drop` becomes a no-op; on any early
/// return (error `?`) or panic the `Drop` impl persists the index
/// opportunistically so Ctrl+C / mid-loop failure does not leave disk
/// out of sync. Write errors during `Drop` are logged (best-effort);
/// `Drop` cannot itself return a `Result`.
struct IndexSaveGuard<'a> {
    base: &'a Path,
    index: StoreIndex,
    persisted: bool,
}

impl Drop for IndexSaveGuard<'_> {
    fn drop(&mut self) {
        if self.persisted {
            return;
        }
        match save_index_at(self.base, &self.index) {
            Ok(()) => {
                tracing::warn!(
                    target: "aicx::store",
                    base = %self.base.display(),
                    "store_segments_at returned early; index.json persisted opportunistically via IndexSaveGuard::drop"
                );
            }
            Err(err) => {
                // `Drop` cannot return; tracing may itself be torn down
                // during a panic so we also fall back to stderr.
                tracing::error!(
                    target: "aicx::store",
                    base = %self.base.display(),
                    "IndexSaveGuard::drop failed to persist index.json: {err:#}"
                );
                eprintln!(
                    "aicx: IndexSaveGuard::drop failed to persist index.json at {}: {err:#}",
                    self.base.display()
                );
            }
        }
    }
}

fn write_semantic_segment_at(
    base: &Path,
    segment: &SemanticSegment,
    date: &str,
    chunker_config: &ChunkerConfig,
    sha_cache: &mut DirShaCache,
) -> Result<SessionWriteOutcome> {
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

    write_context_session_first_outcome_at(
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
        sha_cache,
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

pub fn scan_context_files_project_at(
    base: &Path,
    project_filter: Option<&str>,
) -> Result<Vec<StoredContextFile>> {
    let base = sanitize::validate_dir_path(base)?;
    let Some(filter) = project_filter
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
    else {
        return scan_context_files_at(&base);
    };

    let filter = filter.to_lowercase();
    let ignore = load_ignore_matcher_at(&base)?;
    let mut files = Vec::new();

    let canonical_root = base.join(CANONICAL_STORE_DIRNAME);
    if canonical_root.is_dir() {
        scan_repo_store_filtered(&canonical_root, &ignore, &filter, &mut files)?;
    }

    let non_repo_root = base.join(NON_REPOSITORY_CONTEXTS);
    if non_repo_root.is_dir() && NON_REPOSITORY_CONTEXTS.contains(&filter) {
        scan_non_repository_store(&non_repo_root, &ignore, &mut files)?;
    }

    sort_context_files(&mut files);
    Ok(files)
}

pub fn scan_context_files_raw_at(base: &Path) -> Result<Vec<StoredContextFile>> {
    let base = sanitize::validate_dir_path(base)?;
    let ignore = StoreIgnoreMatcher::empty_at(&base);
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

    sort_context_files(&mut files);

    Ok(files)
}

fn sort_context_files(files: &mut [StoredContextFile]) {
    files.sort_by(|left, right| {
        left.date_compact
            .cmp(&right.date_compact)
            .then_with(|| left.project.cmp(&right.project))
            .then_with(|| left.agent.cmp(&right.agent))
            .then_with(|| left.session_id.cmp(&right.session_id))
            .then_with(|| left.chunk.cmp(&right.chunk))
    });
}

pub fn context_files_since(
    cutoff: SystemTime,
    project_filter: Option<&str>,
) -> Result<Vec<StoredContextFile>> {
    context_files_since_at(&store_base_dir()?, cutoff, project_filter)
}

fn read_store_dir(path: &Path) -> Result<fs::ReadDir> {
    // Shared sanitizer: validate_dir_path + re-canonicalize before open.
    sanitize::read_dir_validated(path)
        .with_context(|| format!("Failed to read store dir {}", path.display()))
}

/// Read one canonical chunk by absolute path, store-relative path, file name,
/// or compact chunk reference.
pub fn read_context_chunk(reference: &str, max_chars: Option<usize>) -> Result<ReadContextChunk> {
    read_context_chunk_at(&store_base_dir()?, reference, max_chars)
}

pub fn read_context_chunk_at(
    base: &Path,
    reference: &str,
    max_chars: Option<usize>,
) -> Result<ReadContextChunk> {
    let base = sanitize::validate_dir_path(base)?;
    let spec = ChunkRefSpec::parse(reference)?;

    let files = scan_context_files_at(&base)?;
    let file = resolve_context_chunk_file(&base, files, &spec)?;

    let relative_path = file
        .path
        .strip_prefix(&base)
        .unwrap_or(&file.path)
        .to_string_lossy()
        // Canonical store keys are forward-slash on every OS so the same
        // conversation resolves to the same relative path on Windows and Unix.
        .replace('\\', "/");
    let path = sanitize::validate_read_path(&file.path)?;
    let bytes = path.metadata().map(|meta| meta.len()).unwrap_or(0);
    let content = sanitize::read_to_string_validated(&path)?;
    let (content, truncated) = truncate_chars(content, max_chars);

    Ok(ReadContextChunk {
        path,
        relative_path,
        project: file.project,
        date: file.date_iso,
        kind: file.kind.dir_name().to_string(),
        agent: file.agent,
        session_id: file.session_id,
        chunk: file.chunk,
        bytes,
        content,
        truncated,
    })
}

fn resolve_context_chunk_file(
    base: &Path,
    files: Vec<StoredContextFile>,
    spec: &ChunkRefSpec,
) -> Result<StoredContextFile> {
    match spec {
        ChunkRefSpec::Id(id) => resolve_context_chunk_id(files, id),
        ChunkRefSpec::Path(path) => {
            let reference = path.to_string_lossy();
            files
                .into_iter()
                .find(|file| stored_file_matches_reference(base, file, &reference))
                .ok_or_else(|| anyhow!("chunk not found: {reference}"))
        }
        ChunkRefSpec::LegacyCompact(reference) => files
            .into_iter()
            .find(|file| stored_file_matches_reference(base, file, reference))
            .ok_or_else(|| anyhow!("chunk not found: {reference}")),
    }
}

fn resolve_context_chunk_id(files: Vec<StoredContextFile>, id: &str) -> Result<StoredContextFile> {
    let mut matches = Vec::new();
    for file in files {
        let path_id = chunk_path_ref_id(&file);
        if path_id.starts_with(id) {
            matches.push((path_id, file));
        }
    }

    match matches.len() {
        0 => Err(anyhow!("chunk not found for id: chunk:{id}")),
        1 => Ok(matches.remove(0).1),
        _ => {
            let candidates = matches
                .iter()
                .map(|(candidate_id, file)| format!("chunk:{candidate_id} {}", file.path.display()))
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow!(
                "ambiguous chunk id chunk:{id}; candidates: {candidates}"
            ))
        }
    }
}

fn chunk_path_ref_id(file: &StoredContextFile) -> String {
    let path = file.path.to_string_lossy();
    let hash = content_sha256(path.as_ref());
    hash.chars().take(8).collect()
}

fn is_chunk_id_prefix(value: &str) -> bool {
    (4..=64).contains(&value.len()) && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn stored_file_matches_reference(base: &Path, file: &StoredContextFile, reference: &str) -> bool {
    let path = file.path.to_string_lossy();
    if path == reference {
        return true;
    }

    let reference_path = Path::new(reference);
    if reference_path.is_absolute() && reference_path == file.path {
        return true;
    }

    if file
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == reference)
    {
        return true;
    }

    if file
        .path
        .strip_prefix(base)
        .ok()
        // Canonical references are forward-slash; normalise the OS-native
        // relative path so a `store/org/repo/...` ref matches on Windows too.
        .is_some_and(|relative| relative.to_string_lossy().replace('\\', "/") == reference)
    {
        return true;
    }

    let compact_ref = format!(
        "{}|{}|{}|{}|{}|{:03}",
        file.project,
        file.date_iso,
        file.kind.dir_name(),
        file.agent,
        file.session_id,
        file.chunk
    );
    compact_ref == reference
}

fn truncate_chars(content: String, max_chars: Option<usize>) -> (String, bool) {
    let Some(max_chars) = max_chars else {
        return (content, false);
    };
    let mut iter = content.chars();
    let truncated: String = iter.by_ref().take(max_chars).collect();
    let was_truncated = iter.next().is_some();
    (truncated, was_truncated)
}

fn context_files_since_at(
    base: &Path,
    cutoff: SystemTime,
    project_filter: Option<&str>,
) -> Result<Vec<StoredContextFile>> {
    // Strict project filter via `project_filter_matches` (same
    // semantics as `aicx search`, `aicx store -p ...` etc.) so the
    // `refs`/MCP/since paths don't leak `-p vista` into `vista-portal`,
    // `vista-datasets`, etc. `StoredContextFile.project` is the
    // canonical `<org>/<repo>` slug (or the non-repo bucket name for
    // entries without a resolved repo identity); split on '/' to feed
    // the org+repo pair into the matcher.
    let filter = project_filter
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let cutoff_date = DateTime::<Utc>::from(cutoff).format("%Y-%m-%d").to_string();
    let mut files = scan_context_files_at(base)?;
    files.retain(|file| {
        let matches_project = match filter {
            None => true,
            Some(f) => {
                let (org, repo) = file
                    .project
                    .split_once('/')
                    .unwrap_or(("", file.project.as_str()));
                project_filter_matches(org, repo, f)
            }
        };
        // Discovery recency is anchored to the canonical chunk date encoded in the
        // store layout, not filesystem mtime which can drift during migration/copy.
        let matches_cutoff = file.date_iso >= cutoff_date;
        matches_project && matches_cutoff
    });
    Ok(files)
}

#[derive(Debug, Clone)]
pub struct ContextCorpusFile {
    pub raw_path: PathBuf,
    pub sidecar_path: PathBuf,
    pub sidecar: chunker::ChunkMetadataSidecar,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ContextCorpusIngestSummary {
    pub target_dir: PathBuf,
    pub raw_written: usize,
    pub sidecars_written: usize,
    pub deduped_chunks: usize,
    pub index_path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
struct ContextCorpusIndexRow {
    id: String,
    path: String,
    artifact_family: Option<String>,
    schema_version: Option<String>,
    truth_status_role: Option<String>,
    keywords: Option<Vec<String>>,
    band: Option<String>,
    content_sha256: Option<String>,
}

pub fn ingest_loct_context_pack(pack_dir: &Path) -> Result<ContextCorpusIngestSummary> {
    ingest_loct_context_pack_into(pack_dir, None)
}

fn ingest_loct_context_pack_into(
    pack_dir: &Path,
    home: Option<&Path>,
) -> Result<ContextCorpusIngestSummary> {
    let pack_dir = sanitize::validate_dir_path(pack_dir)?;
    let raw_dir = pack_dir.join("raw");
    let sidecars_dir = pack_dir.join("sidecars");
    let raw_dir = sanitize::validate_dir_path(&raw_dir)
        .with_context(|| format!("loct context pack missing raw/: {}", raw_dir.display()))?;
    let sidecars_dir = sanitize::validate_dir_path(&sidecars_dir).with_context(|| {
        format!(
            "loct context pack missing sidecars/: {}",
            sidecars_dir.display()
        )
    })?;

    let mut items = Vec::new();
    for entry in read_store_dir(&raw_dir)?.filter_map(|entry| entry.ok()) {
        let raw_path = entry.path();
        if raw_path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = raw_path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let sidecar_path = sidecars_dir.join(format!("{stem}.json"));
        let mut sidecar = load_sidecar_from_path(&sidecar_path)
            .with_context(|| format!("missing or invalid sidecar: {}", sidecar_path.display()))?;
        sidecar.artifact_family = Some(LOCT_CONTEXT_PACK_FAMILY.to_string());
        if sidecar.truth_status.is_none() {
            sidecar.truth_status = Some(chunker::TruthStatus {
                role: chunker::TruthRole::Example,
                runtime_authoritative: false,
                stale_against_current_head: false,
                current_head_when_ingested: None,
            });
        }
        let raw = sanitize::read_to_string_validated(&raw_path)?;
        let hash = content_sha256(&raw);
        sidecar.content_sha256 = Some(hash);
        items.push((raw_path, sidecar_path, sidecar));
    }

    if items.is_empty() {
        anyhow::bail!("loct context pack contains no raw/*.md chunks");
    }

    // Bug #34: reject mixed-project packs before any chunk lands on disk.
    // The legacy code took (org, repo) from the FIRST sidecar and assumed
    // every other record belonged there; a packaging mistake silently
    // routed records into the wrong project bucket.
    let (org, repo) = context_corpus_repo_from_sidecar(&items[0].2)?;
    let first_sidecar_path = items[0].1.clone();
    if let Some((offender_path, offender_org, offender_repo)) =
        items.iter().skip(1).find_map(|(_, sidecar_path, sidecar)| {
            context_corpus_repo_from_sidecar(sidecar)
                .ok()
                .and_then(|(other_org, other_repo)| {
                    (other_org != org || other_repo != repo).then_some((
                        sidecar_path.clone(),
                        other_org,
                        other_repo,
                    ))
                })
        })
    {
        anyhow::bail!(
            "loct context pack {} mixes projects: first sidecar {} declares {}/{}, but sidecar {} declares {}/{}",
            pack_dir.display(),
            first_sidecar_path.display(),
            org,
            repo,
            offender_path.display(),
            offender_org,
            offender_repo,
        );
    }
    let date = items[0].2.date.clone();
    let batch = pack_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("batch");
    let target = match home {
        Some(home) => aicx_context_corpus_dir_for(home, &org, &repo, &date, batch)?,
        None => aicx_context_corpus_dir(&org, &repo, &date, batch)?,
    };
    let target_raw = target.join("raw");
    let target_sidecars = target.join("sidecars");
    let index_path = target.join("index.jsonl");

    let mut seen_hashes = context_corpus_hashes_in_dir(&target_sidecars)?;

    // Bug #35: index.jsonl was unconditionally truncated on re-ingest,
    // erasing rows for chunks the second pack didn't re-present. Load
    // the existing manifest, then merge new rows by id so the on-disk
    // index always contains the union of previously-stored + newly-
    // ingested chunks.
    let mut index_rows = read_context_corpus_index_rows(&index_path)?;
    let mut id_to_pos: HashMap<String, usize> = index_rows
        .iter()
        .enumerate()
        .map(|(idx, row)| (row.id.clone(), idx))
        .collect();

    let mut summary = ContextCorpusIngestSummary {
        target_dir: target.clone(),
        index_path: index_path.clone(),
        ..ContextCorpusIngestSummary::default()
    };

    for (raw_path, _source_sidecar_path, sidecar) in items {
        let hash = sidecar.content_sha256.clone().unwrap_or_default();
        if !hash.is_empty() && seen_hashes.contains_key(&hash) {
            summary.deduped_chunks += 1;
            continue;
        }
        if !hash.is_empty() {
            seen_hashes.insert(hash.clone(), sidecar.id.clone());
        }

        let file_name = raw_path
            .file_name()
            .ok_or_else(|| anyhow!("raw chunk missing filename: {}", raw_path.display()))?;
        let raw_target = target_raw.join(file_name);
        let sidecar_target = target_sidecars.join(format!(
            "{}.json",
            raw_target
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(&sidecar.id)
        ));

        let mut raw_src = sanitize::open_file_validated(&raw_path)?;
        let mut raw_dst = sanitize::create_file_validated(&raw_target)?;
        io::copy(&mut raw_src, &mut raw_dst)?;
        raw_dst.flush()?;
        raw_dst.sync_all()?;
        let mut file = sanitize::create_file_validated(&sidecar_target)?;
        file.write_all(serde_json::to_vec_pretty(&sidecar)?.as_slice())?;
        summary.raw_written += 1;
        summary.sidecars_written += 1;

        let row = ContextCorpusIndexRow {
            id: sidecar.id.clone(),
            path: raw_target.display().to_string(),
            artifact_family: sidecar.artifact_family.clone(),
            schema_version: Some(CONTEXT_CORPUS_SCHEMA_VERSION.to_string()),
            truth_status_role: sidecar
                .truth_status
                .as_ref()
                .map(|status| match status.role {
                    chunker::TruthRole::Live => "live".to_string(),
                    chunker::TruthRole::Example => "example".to_string(),
                }),
            keywords: sidecar.keywords.clone(),
            band: sidecar.frame_kind.map(|kind| kind.as_str().to_string()),
            content_sha256: sidecar.content_sha256.clone(),
        };
        match id_to_pos.get(&row.id).copied() {
            Some(idx) => index_rows[idx] = row,
            None => {
                id_to_pos.insert(row.id.clone(), index_rows.len());
                index_rows.push(row);
            }
        }
    }

    write_context_corpus_index(&index_path, &index_rows)?;
    Ok(summary)
}

pub fn scan_context_corpus_files_at(base: &Path) -> Result<Vec<ContextCorpusFile>> {
    let base = sanitize::validate_dir_path(base)?;
    let root = base.join(CONTEXT_CORPUS_DIRNAME);
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    scan_context_corpus_files_recursive(&root, &mut out)?;
    out.sort_by(|left, right| left.raw_path.cmp(&right.raw_path));
    Ok(out)
}

fn scan_context_corpus_files_recursive(dir: &Path, out: &mut Vec<ContextCorpusFile>) -> Result<()> {
    for entry in read_store_dir(dir)?.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) == Some("raw") {
                collect_context_corpus_raw_dir(&path, out)?;
            } else {
                scan_context_corpus_files_recursive(&path, out)?;
            }
        }
    }
    Ok(())
}

fn collect_context_corpus_raw_dir(raw_dir: &Path, out: &mut Vec<ContextCorpusFile>) -> Result<()> {
    let Some(pack_dir) = raw_dir.parent() else {
        return Ok(());
    };
    let sidecars_dir = pack_dir.join("sidecars");
    if !sidecars_dir.is_dir() {
        return Ok(());
    }
    for entry in read_store_dir(raw_dir)?.filter_map(|entry| entry.ok()) {
        let raw_path = entry.path();
        if raw_path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = raw_path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let sidecar_path = sidecars_dir.join(format!("{stem}.json"));
        let Some(sidecar) = load_sidecar_from_path(&sidecar_path) else {
            continue;
        };
        out.push(ContextCorpusFile {
            raw_path,
            sidecar_path,
            sidecar,
        });
    }
    Ok(())
}

fn context_corpus_repo_from_sidecar(
    sidecar: &chunker::ChunkMetadataSidecar,
) -> Result<(String, String)> {
    let project = sidecar.project.trim();
    if let Some((org, repo)) = project.split_once('/') {
        return Ok((org.to_string(), repo.to_string()));
    }
    Ok(("unknown".to_string(), project.to_string()))
}

fn context_corpus_hashes_in_dir(sidecars_dir: &Path) -> Result<HashMap<String, String>> {
    let mut hashes = HashMap::new();
    if !sidecars_dir.exists() {
        return Ok(hashes);
    }
    for entry in read_store_dir(sidecars_dir)?.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(sidecar) = load_sidecar_from_path(&path) else {
            continue;
        };
        if let Some(hash) = sidecar.content_sha256 {
            hashes.insert(hash, sidecar.id);
        }
    }
    Ok(hashes)
}

fn write_context_corpus_index(path: &Path, rows: &[ContextCorpusIndexRow]) -> Result<()> {
    let mut buf = Vec::with_capacity(rows.len() * 256);
    for row in rows {
        serde_json::to_writer(&mut buf, row)?;
        buf.push(b'\n');
    }
    // Atomic rename keeps the manifest crash-consistent: readers either
    // see the prior full index or the new full index, never a partial
    // truncation. Required by bug #35's preservation contract.
    atomic_write(path, &buf)
        .map_err(|err| anyhow!("write context corpus index {}: {}", path.display(), err))?;
    Ok(())
}

fn read_context_corpus_index_rows(path: &Path) -> Result<Vec<ContextCorpusIndexRow>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = sanitize::read_to_string_validated(path)?;
    let mut rows = Vec::new();
    for (line_no, raw_line) in content.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let row: ContextCorpusIndexRow = serde_json::from_str(trimmed).with_context(|| {
            format!(
                "parse context corpus index row at {}:{}",
                path.display(),
                line_no + 1
            )
        })?;
        rows.push(row);
    }
    Ok(rows)
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
    let project_filter = project.map(str::trim).filter(|value| !value.is_empty());
    let cutoff_date = DateTime::<Utc>::from(cutoff).format("%Y-%m-%d").to_string();
    let mut matched = Vec::new();

    for file in scan_context_files_at(base)? {
        let matches_project = match project_filter {
            None => true,
            Some(f) => {
                let (org, repo) = file
                    .project
                    .split_once('/')
                    .unwrap_or(("", file.project.as_str()));
                project_filter_matches(org, repo, f)
            }
        };
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
    for organization_entry in read_store_dir(root)?.filter_map(|entry| entry.ok()) {
        let organization_path = organization_entry.path();
        if !organization_path.is_dir() {
            continue;
        }
        let organization = organization_entry.file_name().to_string_lossy().to_string();

        // Pre-owner store layouts wrote `store/<repository>/<date>/...`.
        // Keep that physical layout immutable and expose a virtual `_/repo`
        // identity at read time so the bucket is discoverable and queryable.
        if is_ownerless_repository_root(&organization_path)? {
            let repo = RepoIdentity {
                organization: OWNERLESS_PROJECT_ORGANIZATION.to_string(),
                repository: organization,
            };
            let repo_slug = repo.slug();
            scan_single_repo_store(&organization_path, ignore, &repo, &repo_slug, files)?;
            continue;
        }

        for repository_entry in read_store_dir(&organization_path)?.filter_map(|entry| entry.ok()) {
            let repository_path = repository_entry.path();
            if !repository_path.is_dir() {
                continue;
            }
            let repository = repository_entry.file_name().to_string_lossy().to_string();
            let repo = RepoIdentity {
                organization: organization.clone(),
                repository,
            };
            let repo_slug = repo.slug();
            scan_single_repo_store(&repository_path, ignore, &repo, &repo_slug, files)?;
        }
    }

    Ok(())
}

/// Decide whether `<organization>/<repository>` matches a single `-p` filter.
///
/// This is intentionally public: integration tests and downstream callers use
/// it as the canonical project-filter contract, so signature or semantic changes
/// are public API changes.
///
/// Semantics (case-insensitive throughout):
/// - `-p owner/repo` → strict `<owner>/<repo>` slug equality.
/// - `-p owner/` → every repo under this owner (org wildcard).
/// - `-p /repo` → every `*/repo` across all owners (repo wildcard).
/// - `-p name` → match `name` as organization OR repository (cross-org).
///
/// Substring matching (old behavior) is intentionally removed: `-p vista`
/// no longer matched `vista-portal`, `VistaBrain`, `vista-datasets`, etc.
/// Operators get the same effect with `-p vetcoders/Vista -p vetcoders/vista-portal …`
/// when they really mean a list.
pub fn project_filter_matches(organization: &str, repository: &str, filter: &str) -> bool {
    let filter = filter.trim();
    if filter.is_empty() {
        return false;
    }

    // `-p /repo` → cross-org exact repo-name match
    if let Some(repo_only) = filter.strip_prefix('/') {
        if repo_only.is_empty() || repo_only.contains('/') {
            return false;
        }
        return repository.eq_ignore_ascii_case(repo_only);
    }

    // `-p owner/` → org wildcard (all repos under this owner)
    if let Some(org_only) = filter.strip_suffix('/') {
        if org_only.is_empty() || org_only.contains('/') {
            return false;
        }
        return organization.eq_ignore_ascii_case(org_only);
    }

    // `-p owner/repo` → strict slug equality
    if filter.contains('/') {
        let slug = format!("{organization}/{repository}");
        return slug.eq_ignore_ascii_case(filter);
    }

    // `-p name` → cross-org match on organization OR repository
    organization.eq_ignore_ascii_case(filter) || repository.eq_ignore_ascii_case(filter)
}

/// Stable query address for a legacy bucket stored without an owner directory.
pub fn ownerless_project_address(repository: &str) -> String {
    format!("{OWNERLESS_PROJECT_ORGANIZATION}/{}", repository.trim())
}

pub fn is_ownerless_project_address(identity: &str) -> bool {
    identity
        .split_once('/')
        .is_some_and(|(organization, repository)| {
            organization == OWNERLESS_PROJECT_ORGANIZATION && !repository.is_empty()
        })
}

/// Project identity matching is exact by default. Family/substring matching
/// exists only as an explicit opt-in mode on CLI/MCP surfaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMatchMode {
    #[default]
    Exact,
    Fuzzy,
}

impl ProjectMatchMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fuzzy => "fuzzy",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProjectIdentityResolution {
    pub selected: Vec<String>,
    pub candidates: Vec<String>,
    pub unresolved_filters: Vec<String>,
    pub match_mode: ProjectMatchMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectResolutionError {
    Ambiguous {
        filter: String,
        candidates: Vec<String>,
    },
    NoMatch {
        filters: Vec<String>,
    },
}

impl ProjectResolutionError {
    pub fn candidates(&self) -> &[String] {
        match self {
            Self::Ambiguous { candidates, .. } => candidates,
            Self::NoMatch { .. } => &[],
        }
    }

    pub fn filter(&self) -> Option<&str> {
        match self {
            Self::Ambiguous { filter, .. } => Some(filter),
            Self::NoMatch { .. } => None,
        }
    }

    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Ambiguous { .. } => "ambiguous_project",
            Self::NoMatch { .. } => "project_not_found",
        }
    }
}

impl std::fmt::Display for ProjectResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ambiguous { filter, candidates } => write!(
                formatter,
                "project filter {filter:?} is ambiguous; candidates:\n  - {}\nUse one exact bucket with -p owner/repo.",
                candidates.join("\n  - ")
            ),
            Self::NoMatch { filters } => write!(
                formatter,
                "no project matches filter(s): {}\n  accepted forms (case-insensitive): owner/repo (strict), owner/ (org wildcard), /repo (cross-org repo), name (unique exact org or repo); use --project-fuzzy for explicit family matching",
                filters
                    .iter()
                    .map(|filter| format!("{filter:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for ProjectResolutionError {}

/// Resolve project filters against a complete, topology-independent identity
/// corpus. Both CLI and MCP pass their discovered identities through this one
/// function; callers do not select a bucket before ambiguity is assessed.
///
/// Exact-mode rules:
/// - `owner/repo` is case-insensitive slug equality;
/// - `owner/` and `/repo` are explicit multi-project wildcards;
/// - a bare name must identify exactly one owner-or-repository identity;
/// - two or more bare-name candidates fail closed with the full sorted list.
pub fn resolve_project_identities(
    filters: &[String],
    corpus: &[String],
    match_mode: ProjectMatchMode,
) -> std::result::Result<ProjectIdentityResolution, ProjectResolutionError> {
    let identities: Vec<String> = corpus
        .iter()
        .map(|identity| identity.trim())
        .filter(|identity| !identity.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut selected = BTreeSet::new();
    let mut candidates = BTreeSet::new();
    let mut unresolved_filters = Vec::new();

    for raw_filter in filters {
        let filter = raw_filter.trim();
        if filter.is_empty() {
            unresolved_filters.push(raw_filter.clone());
            continue;
        }
        let matches: Vec<String> = identities
            .iter()
            .filter(|identity| project_identity_matches(identity, filter, match_mode))
            .cloned()
            .collect();
        candidates.extend(matches.iter().cloned());

        if matches.is_empty() {
            unresolved_filters.push(raw_filter.clone());
            continue;
        }
        if match_mode == ProjectMatchMode::Exact && !filter.contains('/') && matches.len() > 1 {
            return Err(ProjectResolutionError::Ambiguous {
                filter: raw_filter.clone(),
                candidates: matches,
            });
        }
        selected.extend(matches);
    }

    Ok(ProjectIdentityResolution {
        selected: selected.into_iter().collect(),
        candidates: candidates.into_iter().collect(),
        unresolved_filters,
        match_mode,
    })
}

pub fn require_project_resolution(
    filters: &[String],
    corpus: &[String],
    match_mode: ProjectMatchMode,
) -> std::result::Result<ProjectIdentityResolution, ProjectResolutionError> {
    let resolution = resolve_project_identities(filters, corpus, match_mode)?;
    if !filters.is_empty() && !resolution.unresolved_filters.is_empty() {
        return Err(ProjectResolutionError::NoMatch {
            filters: resolution.unresolved_filters,
        });
    }
    Ok(resolution)
}

fn project_identity_matches(identity: &str, filter: &str, match_mode: ProjectMatchMode) -> bool {
    let (organization, repository) = identity.split_once('/').unwrap_or(("", identity));
    match match_mode {
        ProjectMatchMode::Exact => project_filter_matches(organization, repository, filter),
        ProjectMatchMode::Fuzzy => {
            let needle = filter.trim_matches('/').trim().to_ascii_lowercase();
            if needle.is_empty() {
                return false;
            }
            identity.to_ascii_lowercase().contains(&needle)
                || organization.to_ascii_lowercase().contains(&needle)
                || repository.to_ascii_lowercase().contains(&needle)
        }
    }
}

/// Resolve user-supplied `-p` filters into canonical `<owner>/<repo>` slugs
/// by enumerating the on-disk canonical store. Used by `aicx search` and
/// `aicx index` so a single short name like `-p spotlight-convo-pipeline-v2`
/// expands to `example-org/spotlight-convo-pipeline-v2` before downstream
/// index path / search engine lookup.
///
/// Returns:
/// - empty input → empty output (treat as "search all projects")
/// - non-empty input → union of canonical slugs that match any filter
/// - matched zero projects → empty vec (caller decides: error or all)
pub fn resolve_filters_to_slugs(filters: &[String]) -> Result<Vec<String>> {
    let base = store_base_dir()?;
    let canonical_root = base.join(CANONICAL_STORE_DIRNAME);
    resolve_filters_to_slugs_at(&canonical_root, filters)
}

pub fn resolve_filters_to_slugs_or_error(filters: &[String]) -> Result<Vec<String>> {
    let base = store_base_dir()?;
    let canonical_root = base.join(CANONICAL_STORE_DIRNAME);
    resolve_filters_to_slugs_at_or_error(&canonical_root, filters)
}

pub fn resolve_filters_to_slugs_at(
    canonical_root: &Path,
    filters: &[String],
) -> Result<Vec<String>> {
    if filters.is_empty() {
        return Ok(Vec::new());
    }
    if !canonical_root.is_dir() {
        return Ok(Vec::new());
    }

    let mut slugs: Vec<String> = Vec::new();
    for organization_entry in read_store_dir(canonical_root)?.filter_map(|entry| entry.ok()) {
        let organization_path = organization_entry.path();
        if !organization_path.is_dir() {
            continue;
        }
        let organization = organization_entry.file_name().to_string_lossy().to_string();

        for repository_entry in read_store_dir(&organization_path)?.filter_map(|entry| entry.ok()) {
            let repository_path = repository_entry.path();
            if !repository_path.is_dir() {
                continue;
            }
            let repository = repository_entry.file_name().to_string_lossy().to_string();

            let slug = format!("{organization}/{repository}");
            if !slugs.iter().any(|existing| existing == &slug) {
                slugs.push(slug);
            }
        }
    }

    slugs.sort();
    Ok(resolve_project_identities(filters, &slugs, ProjectMatchMode::Exact)?.selected)
}

pub fn resolve_filters_to_slugs_at_or_error(
    canonical_root: &Path,
    filters: &[String],
) -> Result<Vec<String>> {
    if filters.is_empty() {
        return Ok(Vec::new());
    }
    let corpus = canonical_project_identities_at(canonical_root)?;
    Ok(require_project_resolution(filters, &corpus, ProjectMatchMode::Exact)?.selected)
}

pub fn resolve_filters_to_store_or_index_slugs_at_or_error(
    store_root: &Path,
    filters: &[String],
) -> Result<Vec<String>> {
    if filters.is_empty() {
        return Ok(Vec::new());
    }

    let corpus = project_identities_in_store_or_index_at(store_root)?;
    Ok(require_project_resolution(filters, &corpus, ProjectMatchMode::Exact)?.selected)
}

pub fn canonical_project_identities_at(canonical_root: &Path) -> Result<Vec<String>> {
    if !canonical_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut identities = BTreeSet::new();
    for organization_entry in read_store_dir(canonical_root)?.filter_map(|entry| entry.ok()) {
        let organization_path = organization_entry.path();
        if !organization_path.is_dir() {
            continue;
        }
        let organization = organization_entry.file_name().to_string_lossy().to_string();
        if is_ownerless_repository_root(&organization_path)? {
            identities.insert(ownerless_project_address(&organization));
            continue;
        }
        for repository_entry in read_store_dir(&organization_path)?.filter_map(|entry| entry.ok()) {
            if repository_entry.path().is_dir() {
                identities.insert(format!(
                    "{organization}/{}",
                    repository_entry.file_name().to_string_lossy()
                ));
            }
        }
    }
    Ok(identities.into_iter().collect())
}

pub fn project_identities_in_store_at(store_root: &Path) -> Result<Vec<String>> {
    let canonical_root = store_root.join(CANONICAL_STORE_DIRNAME);
    canonical_project_identities_at(&canonical_root)
}

pub fn project_identities_in_store_or_index_at(store_root: &Path) -> Result<Vec<String>> {
    let mut identities: BTreeSet<String> = project_identities_in_store_at(store_root)?
        .into_iter()
        .collect();
    identities.extend(project_identities_from_index_at(
        &store_root.join("indexed"),
    )?);
    Ok(identities.into_iter().collect())
}

/// Fast corpus for search `-p` resolution: shallow store dirs + indexed
/// bucket directory names. Never opens `embeddings.ndjson` (multi-GB).
///
/// Doctrine 2026-07-23: project is a metadata filter on the hybrid
/// generation, not a precondition that may stream 19 GB of retired NDJSON.
pub fn project_identities_for_search_at(store_root: &Path) -> Result<Vec<String>> {
    let mut identities: BTreeSet<String> = project_identities_in_store_at(store_root)?
        .into_iter()
        .collect();
    identities.extend(project_identities_from_index_dirs_at(
        &store_root.join("indexed"),
    )?);
    // Durable catalog (when present) carries topical project attribution
    // that cwd-derived store layout may miss. Slim (`loctree-consumer`)
    // builds compile without the catalog module and skip this widening.
    #[cfg(feature = "app")]
    {
        if let Ok(catalog_ids) = crate::catalog::project_identities_from_catalog_at(store_root) {
            identities.extend(catalog_ids);
        }
    }
    Ok(identities.into_iter().collect())
}

#[cfg(test)]
fn resolve_filters_to_index_slugs_at(
    indexed_root: &Path,
    filters: &[String],
) -> Result<Vec<String>> {
    let corpus = project_identities_from_index_at(indexed_root)?;
    Ok(resolve_project_identities(filters, &corpus, ProjectMatchMode::Exact)?.selected)
}

/// Directory-name identities under `~/.aicx/indexed/` (no NDJSON reads).
/// Bucket dirs use underscore form (`vetcoders_vista`); bare names stay bare.
fn project_identities_from_index_dirs_at(indexed_root: &Path) -> Result<Vec<String>> {
    if !indexed_root.exists() {
        return Ok(Vec::new());
    }
    let mut identities = BTreeSet::new();
    for entry in sanitize::read_dir_validated(indexed_root)
        .with_context(|| format!("read indexed root {}", indexed_root.display()))?
    {
        let entry =
            entry.with_context(|| format!("read indexed entry in {}", indexed_root.display()))?;
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // Internal / retired buckets — not project filters.
        if name == "_all" || name.starts_with('.') {
            continue;
        }
        if let Some((owner, repo)) = name.split_once('_')
            && !owner.is_empty()
            && !repo.is_empty()
            && !owner.starts_with('_')
        {
            // Best-effort: `loctree_aicx` → `loctree/aicx`. Multi-underscore
            // repos keep the first split only (search also accepts the bare
            // bucket name below).
            identities.insert(format!("{owner}/{repo}"));
        }
        identities.insert(name);
    }
    Ok(identities.into_iter().collect())
}

/// Legacy path kept for tests that still call through `*_or_index_at`.
/// Does **not** stream `embeddings.ndjson` — directory names only.
fn project_identities_from_index_at(indexed_root: &Path) -> Result<Vec<String>> {
    project_identities_from_index_dirs_at(indexed_root)
}

fn scan_repo_store_filtered(
    root: &Path,
    ignore: &StoreIgnoreMatcher,
    project_filter: &str,
    files: &mut Vec<StoredContextFile>,
) -> Result<()> {
    for organization_entry in read_store_dir(root)?.filter_map(|entry| entry.ok()) {
        let organization_path = organization_entry.path();
        if !organization_path.is_dir() {
            continue;
        }
        let organization = organization_entry.file_name().to_string_lossy().to_string();

        if is_ownerless_repository_root(&organization_path)? {
            if project_filter_matches(
                OWNERLESS_PROJECT_ORGANIZATION,
                &organization,
                project_filter,
            ) {
                let repo = RepoIdentity {
                    organization: OWNERLESS_PROJECT_ORGANIZATION.to_string(),
                    repository: organization,
                };
                let repo_slug = repo.slug();
                scan_single_repo_store(&organization_path, ignore, &repo, &repo_slug, files)?;
            }
            continue;
        }

        for repository_entry in read_store_dir(&organization_path)?.filter_map(|entry| entry.ok()) {
            let repository_path = repository_entry.path();
            if !repository_path.is_dir() {
                continue;
            }
            let repository = repository_entry.file_name().to_string_lossy().to_string();
            if !project_filter_matches(&organization, &repository, project_filter) {
                continue;
            }
            let repo = RepoIdentity {
                organization: organization.clone(),
                repository: repository.clone(),
            };
            let repo_slug = repo.slug();
            scan_single_repo_store(&repository_path, ignore, &repo, &repo_slug, files)?;
        }
    }

    Ok(())
}

/// Detect the legacy `store/<repository>/<date>/<kind>/...` shape without
/// confusing a canonical `store/<organization>/<repository>/...` directory
/// for an ownerless repository. A direct child containing a known `Kind`
/// directory is the distinguishing structural signal.
fn is_ownerless_repository_root(path: &Path) -> Result<bool> {
    for date_entry in read_store_dir(path)?.filter_map(|entry| entry.ok()) {
        let date_path = date_entry.path();
        if !date_path.is_dir() {
            continue;
        }
        for kind_entry in read_store_dir(&date_path)?.filter_map(|entry| entry.ok()) {
            if kind_entry.path().is_dir()
                && Kind::parse(&kind_entry.file_name().to_string_lossy()).is_some()
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn scan_single_repo_store(
    repository_path: &Path,
    ignore: &StoreIgnoreMatcher,
    repo: &RepoIdentity,
    repo_slug: &str,
    files: &mut Vec<StoredContextFile>,
) -> Result<()> {
    for date_entry in read_store_dir(repository_path)?.filter_map(|entry| entry.ok()) {
        let date_path = date_entry.path();
        if !date_path.is_dir() {
            continue;
        }
        let date_compact = date_entry.file_name().to_string_lossy().to_string();

        for kind_entry in read_store_dir(&date_path)?.filter_map(|entry| entry.ok()) {
            let kind_path = kind_entry.path();
            if !kind_path.is_dir() {
                continue;
            }
            let Some(kind) = Kind::parse(&kind_entry.file_name().to_string_lossy()) else {
                continue;
            };

            for agent_entry in read_store_dir(&kind_path)?.filter_map(|entry| entry.ok()) {
                let agent_path = agent_entry.path();
                if !agent_path.is_dir() {
                    continue;
                }
                let agent = agent_entry.file_name().to_string_lossy().to_string();
                let ctx = LeafScanContext {
                    repo: Some(repo.clone()),
                    project: repo_slug,
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

fn scan_non_repository_store(
    root: &Path,
    ignore: &StoreIgnoreMatcher,
    files: &mut Vec<StoredContextFile>,
) -> Result<()> {
    for date_entry in read_store_dir(root)?.filter_map(|entry| entry.ok()) {
        let date_path = date_entry.path();
        if !date_path.is_dir() {
            continue;
        }
        let date_compact = date_entry.file_name().to_string_lossy().to_string();

        for kind_entry in read_store_dir(&date_path)?.filter_map(|entry| entry.ok()) {
            let kind_path = kind_entry.path();
            if !kind_path.is_dir() {
                continue;
            }
            let Some(kind) = Kind::parse(&kind_entry.file_name().to_string_lossy()) else {
                continue;
            };

            for agent_entry in read_store_dir(&kind_path)?.filter_map(|entry| entry.ok()) {
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
    for file_entry in read_store_dir(dir)?.filter_map(|entry| entry.ok()) {
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

    let chunk = parse_chunk_component(chunk_str)?;
    Some((session_id.to_string(), chunk))
}

fn parse_chunk_component(value: &str) -> Option<u32> {
    let digits = match value.split_once("-c") {
        Some((digits, suffix))
            if suffix.len() == 6 && suffix.chars().all(|ch| ch.is_ascii_hexdigit()) =>
        {
            digits
        }
        Some(_) => return None,
        None => value,
    };

    if digits.len() < 3 || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    digits.parse().ok()
}

pub fn expand_compact_date(compact: &str) -> String {
    let digits: String = compact.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        format!("{}-{}-{}", &digits[..4], &digits[4..6], &digits[6..8])
    } else {
        compact.to_string()
    }
}

pub(crate) mod migration;
pub use migration::{
    CardsV2Action, CardsV2Item, CardsV2Manifest, CardsV2Totals, LegacyItemKind, MigrationAction,
    MigrationExecution, MigrationItem, MigrationManifest, MigrationTotals, run_cards_v2_migration,
    run_migration, run_migration_with_paths,
};
#[cfg(all(test, feature = "app"))]
pub(crate) use migration::{SourceLocator, run_migration_at};

// ============================================================================
// Tests
// ============================================================================

#[cfg(all(test, feature = "app"))]
mod tests;
