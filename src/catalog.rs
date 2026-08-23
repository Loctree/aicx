//! Durable session catalog — the extract-era store surface.
//!
//! Replaces the per-frame card mill (`~/.aicx/store/**/*.md`) with one
//! compact append-only identity index:
//!
//! ```text
//! ~/.aicx/catalog/sessions.jsonl
//! ```
//!
//! Each line maps `session_id → project, agent, date, cwd, source_path,
//! title, machine`. Content stays in the agent sources (or optional
//! `~/.aicx/extracts/` cache). Rebuild walks source roots only — no card
//! files are written.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::legacy_archive::{self};
use crate::session_catalog::{AgentKind, CatalogIoStats, CatalogSource, SessionCatalog};

pub const CATALOG_DIRNAME: &str = "catalog";
pub const SESSIONS_FILENAME: &str = "sessions.jsonl";
pub const CATALOG_SCHEMA: &str = "aicx.catalog.session.v1";
const REMOTE_MEMO_FILENAME: &str = "remotes.json";
const REMOTE_MEMO_SCHEMA: &str = "aicx.catalog.remotes.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogEntry {
    pub schema: String,
    pub session_id: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub source_path: String,
    /// Live source size at last catalog rebuild (bytes). Part of the
    /// source-change fingerprint so appends invalidate incremental reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_len: Option<u64>,
    /// Live source mtime at last catalog rebuild (unix nanoseconds).
    /// Paired with `source_len` so `aicx index` re-parses changed sessions
    /// instead of treating path-stable catalog rows as frozen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_mtime_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_session_id: Option<String>,
}

/// Size + mtime-ns for a path. Returns `None` when the file is unreadable.
pub fn live_source_fingerprint(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::metadata(path).ok()?;
    let mtime_ns = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    // u64 covers unix nanos until year ~2554; truncation is intentional.
    Some((metadata.len(), mtime_ns as u64))
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RebuildReport {
    pub agents: BTreeMap<String, usize>,
    pub projects: BTreeMap<String, usize>,
    pub total_sessions: usize,
    pub catalog_path: String,
    pub wall_ms: u64,
    pub cards_written: usize,
    /// Sessions the search index has not yet absorbed. Rebuild never
    /// drains chunks unless the caller asked `--with-chunks`.
    #[serde(default)]
    pub pending_chunks: usize,
}

/// Granular catalog vs live-source readiness for operator tooling.
///
/// Orthogonal to `aicx index status` (index vs catalog). This surface answers:
/// will the next rebuild admit new sessions, and which rows are already stale?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogReadiness {
    /// No durable catalog file yet.
    Missing,
    /// Catalog empty and no live sources discovered.
    Empty,
    /// Every catalog row matches live fingerprints; no unadmitted live sources.
    Fresh,
    /// Live sources exist that are not in the catalog, and/or fingerprints drifted.
    NeedsRebuild,
    /// Catalog has rows but every live source path is missing (sync/path problem).
    SourcesMissing,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StalenessCounts {
    /// Catalog row fingerprint matches live source.
    pub current: usize,
    /// Catalog row exists but live size/mtime differs (append/edit).
    pub stale: usize,
    /// Live primary source not present in durable catalog.
    pub unadmitted: usize,
    /// Catalog row whose source_path is gone or unreadable.
    pub missing_source: usize,
    /// Catalog row lacks fingerprint and live stats could not be read.
    pub fingerprint_unknown: usize,
}

impl StalenessCounts {
    pub fn total_catalog_classified(&self) -> usize {
        self.current + self.stale + self.missing_source + self.fingerprint_unknown
    }

    pub fn rebuild_pressure(&self) -> usize {
        self.stale + self.unadmitted + self.missing_source
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StalenessSample {
    pub agent: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    pub source_path: String,
    pub class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_mtime_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_mtime_ns: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogStatusReport {
    pub schema: String,
    pub readiness: CatalogReadiness,
    pub catalog_path: String,
    pub catalog_present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_mtime: Option<String>,
    pub catalog_sessions: usize,
    pub live_sessions: usize,
    pub counts: StalenessCounts,
    pub by_agent: BTreeMap<String, StalenessCounts>,
    /// Hostnames stamped into catalog rows at last rebuild (identity only).
    pub by_machine: BTreeMap<String, usize>,
    pub samples: Vec<StalenessSample>,
    pub recommendations: Vec<String>,
    pub notes: Vec<String>,
    pub wall_ms: u64,
}

pub const CATALOG_STATUS_SCHEMA: &str = "aicx.catalog.status.v1";
const STATUS_SAMPLE_CAP: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildStage {
    Preparing,
    ScanningSources,
    EnrichingSessions,
    SnapshottingRuntimeRuns,
    Serializing,
    Writing,
    Complete,
}

#[derive(Debug, Clone)]
pub struct RebuildProgress {
    pub stage: RebuildStage,
    pub agent: Option<&'static str>,
    pub agent_index: usize,
    pub agent_total: usize,
    pub io: CatalogIoStats,
    pub sessions: usize,
    pub elapsed_ms: u64,
}

impl RebuildProgress {
    pub fn preparing() -> Self {
        Self {
            stage: RebuildStage::Preparing,
            agent: None,
            agent_index: 0,
            agent_total: 5,
            io: CatalogIoStats::default(),
            sessions: 0,
            elapsed_ms: 0,
        }
    }
}

pub fn catalog_dir_for(home: &Path) -> PathBuf {
    home.join(CATALOG_DIRNAME)
}

pub fn sessions_path_for(home: &Path) -> PathBuf {
    catalog_dir_for(home).join(SESSIONS_FILENAME)
}

pub fn sessions_path() -> Result<PathBuf> {
    Ok(sessions_path_for(&crate::aicx_home::resolve()?))
}

pub fn read_entries_at(home: &Path) -> Result<Vec<CatalogEntry>> {
    let path = sessions_path_for(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    // Containment: catalog must resolve under the AICX home allowlist.
    let file = crate::source_path::open_under_aicx_home(home, &path)
        .with_context(|| format!("open catalog {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read catalog line {}", line_no + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        entries.push(serde_json::from_str(&line).with_context(|| {
            format!("parse catalog line {} in {}", line_no + 1, path.display())
        })?);
    }
    Ok(entries)
}

/// Project identities already attributed in the durable catalog (if any).
pub fn project_identities_from_catalog_at(aicx_home: &Path) -> Result<Vec<String>> {
    let path = sessions_path_for(aicx_home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    // Containment: catalog must resolve under the AICX home allowlist.
    let file = crate::source_path::open_under_aicx_home(aicx_home, &path)
        .with_context(|| format!("open catalog {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut identities = BTreeMap::new();
    for line in reader.lines() {
        let line = line.with_context(|| format!("read catalog line {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<CatalogEntry>(&line) else {
            continue;
        };
        if let Some(project) = entry.project {
            let project = project.trim();
            if !project.is_empty() {
                identities
                    .entry(project.to_ascii_lowercase())
                    .or_insert_with(|| project.to_string());
            }
        }
    }
    Ok(identities.into_values().collect())
}

/// Rebuild the durable catalog from live agent source roots.
///
/// Walks claude / codex / gemini / grok / junie roots via
/// [`SessionCatalog`], enriches with [`crate::sessions`] discovery for
/// cwd/project/title when available, and writes one jsonl line per
/// session. Never creates per-frame card files under `store/`.
pub fn rebuild(home: &Path, user_home: &Path) -> Result<RebuildReport> {
    rebuild_with_progress(home, user_home, |_| {})
}

pub fn rebuild_with_progress(
    home: &Path,
    user_home: &Path,
    mut on_progress: impl FnMut(&RebuildProgress),
) -> Result<RebuildReport> {
    let started = Instant::now();
    let mut progress = RebuildProgress::preparing();
    on_progress(&progress);

    let by_id = scan_live_entries_with_progress(home, user_home, started, &mut on_progress);

    progress.stage = RebuildStage::Serializing;
    progress.sessions = by_id.len();
    progress.elapsed_ms = started.elapsed().as_millis() as u64;
    on_progress(&progress);
    let catalog_path = sessions_path_for(home);
    fs::create_dir_all(catalog_dir_for(home))
        .with_context(|| format!("create catalog dir {}", catalog_dir_for(home).display()))?;

    let mut agents: BTreeMap<String, usize> = BTreeMap::new();
    let mut projects: BTreeMap<String, usize> = BTreeMap::new();
    let mut body = String::new();
    for entry in by_id.values() {
        *agents.entry(entry.agent.clone()).or_default() += 1;
        if let Some(ref project) = entry.project {
            *projects.entry(project.clone()).or_default() += 1;
        }
        body.push_str(&serde_json::to_string(entry)?);
        body.push('\n');
    }
    progress.stage = RebuildStage::Writing;
    progress.sessions = by_id.len();
    progress.elapsed_ms = started.elapsed().as_millis() as u64;
    on_progress(&progress);
    let _catalog_guard = crate::locks::acquire_exclusive(home.join("locks").join("catalog.lock"))?;
    legacy_archive::atomic_write::atomic_write(&catalog_path, body.as_bytes())
        .with_context(|| format!("write catalog {}", catalog_path.display()))?;

    let report = RebuildReport {
        total_sessions: by_id.len(),
        agents,
        projects,
        catalog_path: catalog_path.display().to_string(),
        wall_ms: started.elapsed().as_millis() as u64,
        cards_written: 0,
        pending_chunks: 0,
    };
    progress.stage = RebuildStage::Complete;
    progress.sessions = report.total_sessions;
    progress.elapsed_ms = report.wall_ms;
    on_progress(&progress);
    Ok(report)
}

/// Compare durable catalog rows to live agent source roots without rewriting.
///
/// Classes:
/// - `current` — catalog fingerprint matches live size+mtime
/// - `stale` — same session id/path, live fingerprint drifted (append/edit)
/// - `unadmitted` — live primary source not yet in catalog
/// - `missing_source` — catalog path no longer readable on this host
/// - `fingerprint_unknown` — no catalog fingerprint and live stats unavailable
///
/// This does **not** inspect the search index. After rebuild pressure drops to
/// zero, run `aicx index status` / `aicx index` for CURRENT freshness.
pub fn status(home: &Path, user_home: &Path) -> Result<CatalogStatusReport> {
    let started = Instant::now();
    let catalog_path = sessions_path_for(home);
    let catalog_present = catalog_path.is_file();
    let catalog_mtime = if catalog_present {
        fs::metadata(&catalog_path)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|mtime| {
                let secs = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
                chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.to_rfc3339())
            })
    } else {
        None
    };
    let catalog_entries = read_entries_at(home)?;
    let live = scan_live_entries(home, user_home);

    let mut counts = StalenessCounts::default();
    let mut by_agent: BTreeMap<String, StalenessCounts> = BTreeMap::new();
    let mut by_machine: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples = Vec::new();

    let mut live_keys: BTreeSet<(String, String)> = BTreeSet::new();
    for entry in live.values() {
        live_keys.insert((entry.agent.clone(), entry.session_id.clone()));
    }

    let mut catalog_keys: BTreeSet<(String, String)> = BTreeSet::new();
    for entry in &catalog_entries {
        let key = (entry.agent.clone(), entry.session_id.clone());
        catalog_keys.insert(key.clone());
        if let Some(machine) = entry.machine.as_deref().filter(|m| !m.is_empty()) {
            *by_machine.entry(machine.to_string()).or_default() += 1;
        }
        let agent_counts = by_agent.entry(entry.agent.clone()).or_default();
        let live_entry = live.get(&key);
        let live_fp = live_entry
            .map(|e| Path::new(&e.source_path))
            .and_then(live_source_fingerprint);

        match (live_fp, entry.source_len, entry.source_mtime_ns) {
            (Some((live_len, live_mtime)), Some(cat_len), Some(cat_mtime))
                if live_len == cat_len && live_mtime == cat_mtime =>
            {
                counts.current += 1;
                agent_counts.current += 1;
            }
            (Some((live_len, live_mtime)), Some(cat_len), Some(cat_mtime)) => {
                counts.stale += 1;
                agent_counts.stale += 1;
                push_sample(
                    &mut samples,
                    entry,
                    "stale",
                    Some(cat_len),
                    Some(live_len),
                    Some(cat_mtime),
                    Some(live_mtime),
                );
            }
            (Some((live_len, live_mtime)), _, _) => {
                // Catalog lacked fingerprint — treat as stale so rebuild admits stats.
                counts.stale += 1;
                agent_counts.stale += 1;
                push_sample(
                    &mut samples,
                    entry,
                    "stale",
                    entry.source_len,
                    Some(live_len),
                    entry.source_mtime_ns,
                    Some(live_mtime),
                );
            }
            (None, _, _) if live_entry.is_some() => {
                counts.fingerprint_unknown += 1;
                agent_counts.fingerprint_unknown += 1;
                push_sample(
                    &mut samples,
                    entry,
                    "fingerprint_unknown",
                    entry.source_len,
                    None,
                    entry.source_mtime_ns,
                    None,
                );
            }
            (None, _, _) => {
                counts.missing_source += 1;
                agent_counts.missing_source += 1;
                push_sample(
                    &mut samples,
                    entry,
                    "missing_source",
                    entry.source_len,
                    None,
                    entry.source_mtime_ns,
                    None,
                );
            }
        }
    }

    for key in &live_keys {
        if catalog_keys.contains(key) {
            continue;
        }
        let Some(entry) = live.get(key) else {
            continue;
        };
        counts.unadmitted += 1;
        by_agent.entry(entry.agent.clone()).or_default().unadmitted += 1;
        let live_fp = live_source_fingerprint(Path::new(&entry.source_path));
        push_sample(
            &mut samples,
            entry,
            "unadmitted",
            None,
            live_fp.map(|(len, _)| len),
            None,
            live_fp.map(|(_, mtime)| mtime),
        );
    }

    let readiness = classify_readiness(catalog_present, catalog_entries.len(), live.len(), &counts);
    let recommendations = recommendations_for(readiness, &counts);
    let notes = multi_host_notes(&by_machine, &counts);

    Ok(CatalogStatusReport {
        schema: CATALOG_STATUS_SCHEMA.to_string(),
        readiness,
        catalog_path: catalog_path.display().to_string(),
        catalog_present,
        catalog_mtime,
        catalog_sessions: catalog_entries.len(),
        live_sessions: live.len(),
        counts,
        by_agent,
        by_machine,
        samples,
        recommendations,
        notes,
        wall_ms: started.elapsed().as_millis() as u64,
    })
}

/// Hot-window live delta: sessions present on disk that the durable catalog
/// census does not admit yet. `newest_live_mtime_ns` spans ALL live sessions
/// (lag honesty), while `unadmitted` carries only the sessions a hot query
/// must parse ad-hoc. Same discovery + enrichment as `rebuild`, no writes.
#[derive(Debug, Clone, Default)]
pub struct LiveDelta {
    pub unadmitted: Vec<CatalogEntry>,
    /// Hot-window rows that are new or whose live fingerprint changed.
    ///
    /// This is the bounded input for [`refresh_hot`]. It deliberately omits
    /// cold catalog rows so an interactive refresh never becomes a full walk.
    pub changed: Vec<CatalogEntry>,
    pub live_sessions: usize,
    pub newest_live_mtime_ns: Option<u64>,
    pub wall_ms: u64,
}

pub const CATALOG_REFRESH_SCHEMA: &str = "aicx.catalog.refresh.v1";

#[derive(Debug, Clone, Serialize)]
pub struct HotRefreshReport {
    pub schema: String,
    pub catalog_path: String,
    pub catalog_present: bool,
    pub scanned_live_sessions: usize,
    pub changed_sessions: usize,
    pub admitted_sessions: usize,
    pub reattributed_sessions: usize,
    pub total_sessions: usize,
    pub wall_ms: u64,
    pub recommendation: Option<String>,
}

/// One command run (or one MCP burst) should pay for a single source-root
/// walk even when several extraction lanes ask for the delta back-to-back.
/// 30 s stays comfortably inside the ≤60 s live-window freshness SLA.
const LIVE_DELTA_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Extraction lanes compute their cutoff from independent `Utc::now()` calls;
/// treat cutoffs within a minute as the same window for cache purposes.
const LIVE_DELTA_CUTOFF_TOLERANCE_NS: u128 = 60 * 1_000_000_000;

#[allow(clippy::type_complexity)]
static LIVE_DELTA_CACHE: std::sync::Mutex<Option<(Instant, PathBuf, PathBuf, u128, LiveDelta)>> =
    std::sync::Mutex::new(None);

pub fn live_delta(home: &Path, user_home: &Path, cutoff_unix_ns: u128) -> Result<LiveDelta> {
    if let Ok(guard) = LIVE_DELTA_CACHE.lock()
        && let Some((stamp, cached_home, cached_user_home, cached_cutoff, delta)) = guard.as_ref()
        && stamp.elapsed() < LIVE_DELTA_CACHE_TTL
        && cached_home == home
        && cached_user_home == user_home
        && cached_cutoff.abs_diff(cutoff_unix_ns) <= LIVE_DELTA_CUTOFF_TOLERANCE_NS
    {
        return Ok(delta.clone());
    }
    let delta = live_delta_uncached(home, user_home, cutoff_unix_ns)?;
    if let Ok(mut guard) = LIVE_DELTA_CACHE.lock() {
        *guard = Some((
            Instant::now(),
            home.to_path_buf(),
            user_home.to_path_buf(),
            cutoff_unix_ns,
            delta.clone(),
        ));
    }
    Ok(delta)
}

/// Seed the live-delta cache so unit tests exercise the intents live window
/// without walking the developer's real agent roots.
#[cfg(test)]
pub(crate) fn prime_live_delta_cache_for_tests(
    home: &Path,
    user_home: &Path,
    cutoff_unix_ns: u128,
    delta: LiveDelta,
) {
    if let Ok(mut guard) = LIVE_DELTA_CACHE.lock() {
        *guard = Some((
            Instant::now(),
            home.to_path_buf(),
            user_home.to_path_buf(),
            cutoff_unix_ns,
            delta,
        ));
    }
}

fn live_delta_uncached(home: &Path, user_home: &Path, cutoff_unix_ns: u128) -> Result<LiveDelta> {
    let started = Instant::now();
    let catalog_by_key: BTreeMap<(String, String), CatalogEntry> = read_entries_at(home)?
        .into_iter()
        .map(|entry| ((entry.agent.clone(), entry.session_id.clone()), entry))
        .collect();

    // Sources the census already holds at this exact fingerprint. Probing
    // them again would open a bounded header per file — thousands of reads
    // per call on a real root — only to re-derive the identity already on
    // disk. The cost of that trust: path-derived fields (cwd, project guess)
    // of an untouched source are not re-derived when the derivation itself
    // improves; `aicx catalog rebuild` is the pass that does.
    let known_fingerprints: BTreeMap<&str, (u64, u128)> = catalog_by_key
        .values()
        .filter_map(|entry| {
            Some((
                entry.source_path.as_str(),
                (entry.source_len?, entry.source_mtime_ns? as u128),
            ))
        })
        .collect();
    let is_known = |path: &Path, fingerprint: &crate::session_catalog::SourceFingerprint| {
        known_fingerprints
            .get(path.to_string_lossy().as_ref())
            .is_some_and(|(len, modified)| {
                *len == fingerprint.len && *modified == fingerprint.modified_unix_nanos
            })
    };

    let mut live_sessions = 0usize;
    let mut newest_live_mtime_ns: Option<u64> = None;
    let mut fresh: BTreeMap<(String, String), CatalogEntry> = BTreeMap::new();
    let agents = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Gemini,
        AgentKind::Grok,
        AgentKind::Junie,
    ];
    for agent in agents {
        let root = agent_source_root(agent, user_home);
        if !root.exists() {
            continue;
        }
        let Ok(catalog) = SessionCatalog::new(agent, &root) else {
            continue;
        };
        let Ok(scan) = catalog.scan_hot_window_skipping(cutoff_unix_ns, &is_known) else {
            continue;
        };
        live_sessions += scan.total_candidates;
        if let Some(newest) = scan.newest_modified_unix_nanos {
            let newest = newest.min(u64::MAX as u128) as u64;
            newest_live_mtime_ns = Some(newest_live_mtime_ns.map_or(newest, |max| max.max(newest)));
        }
        for source in scan.fresh_sources {
            if !is_primary_catalog_source(agent, &source.path) {
                continue;
            }
            let entry = entry_from_source(agent, &source);
            fresh.insert((entry.agent.clone(), entry.session_id.clone()), entry);
        }
    }
    // Runtime-run transcripts are a bounded tree — the vibecrafted lane of
    // the live window stays in.
    enrich_runtime_runs(&mut fresh, user_home);

    // Reattribution — not the path guess — is what finally decides a
    // session's identity, so the delta has to compare post-reattribution
    // values on both sides. Comparing a fresh path guess (`vibecrafted-suite/
    // vc-slack-agent`, read off the directory layout) against a cataloged
    // origin slug (`vetcoders/vc-slack`) marked ~270 untouched sessions as
    // changed on every single call: the whole catalog was rewritten, the
    // rewrite reattributed the rows straight back, and the next call found
    // the same difference again. A fixed point was unreachable by
    // construction.
    let mut memo = RemoteMemo::load(home);
    reattribute_catalog_entries(&mut fresh, &mut memo);
    memo.persist();

    let unadmitted = fresh
        .iter()
        .filter(|(key, _)| !catalog_by_key.contains_key(*key))
        .map(|(_, entry)| entry.clone())
        .collect();
    let changed = fresh
        .into_iter()
        .filter(|(key, entry)| {
            catalog_by_key.get(key).is_none_or(|cataloged| {
                cataloged.source_path != entry.source_path
                    || cataloged.source_len != entry.source_len
                    || cataloged.source_mtime_ns != entry.source_mtime_ns
                    || cataloged.project != entry.project
                    || cataloged.cwd != entry.cwd
            })
        })
        .map(|(_, entry)| entry)
        .collect();
    Ok(LiveDelta {
        unadmitted,
        changed,
        live_sessions,
        newest_live_mtime_ns,
        wall_ms: started.elapsed().as_millis() as u64,
    })
}

/// Admit only new or fingerprint-changed sessions inside a hot time window.
///
/// The first durable census remains an explicit full rebuild: creating a
/// catalog from a bounded window would falsely present a partial inventory as
/// complete. Once `sessions.jsonl` exists, this path is safe for interactive
/// continuity, wizard, and dashboard entry because it merges hot rows under
/// the same catalog write lock and never deletes cold rows.
pub fn refresh_hot(
    home: &Path,
    user_home: &Path,
    cutoff_unix_ns: u128,
) -> Result<HotRefreshReport> {
    let started = Instant::now();
    let catalog_path = sessions_path_for(home);
    if !catalog_path.is_file() {
        return Ok(HotRefreshReport {
            schema: CATALOG_REFRESH_SCHEMA.to_string(),
            catalog_path: catalog_path.display().to_string(),
            catalog_present: false,
            scanned_live_sessions: 0,
            changed_sessions: 0,
            admitted_sessions: 0,
            reattributed_sessions: 0,
            total_sessions: 0,
            wall_ms: started.elapsed().as_millis() as u64,
            recommendation: Some(
                "Run `aicx catalog rebuild` once to establish the durable census; hot refresh will maintain it afterwards."
                    .to_string(),
            ),
        });
    }

    let delta = live_delta_uncached(home, user_home, cutoff_unix_ns)?;
    let scanned_live_sessions = delta.live_sessions;
    let changed_sessions = delta.changed.len();
    let admitted_sessions = delta.unadmitted.len();
    let mut preview_catalog: BTreeMap<(String, String), CatalogEntry> = read_entries_at(home)?
        .into_iter()
        .map(|entry| ((entry.agent.clone(), entry.session_id.clone()), entry))
        .collect();
    let mut memo = RemoteMemo::load(home);
    let reattributed_sessions = reattribute_catalog_entries(&mut preview_catalog, &mut memo);
    memo.persist();
    if changed_sessions == 0 && reattributed_sessions == 0 {
        return Ok(HotRefreshReport {
            schema: CATALOG_REFRESH_SCHEMA.to_string(),
            catalog_path: catalog_path.display().to_string(),
            catalog_present: true,
            scanned_live_sessions,
            changed_sessions,
            admitted_sessions,
            reattributed_sessions,
            total_sessions: read_entries_at(home)?.len(),
            wall_ms: started.elapsed().as_millis() as u64,
            recommendation: None,
        });
    }

    let lock_path = home.join("locks").join("catalog.lock");
    let _guard = crate::locks::acquire_exclusive(&lock_path)?;
    let mut catalog: BTreeMap<(String, String), CatalogEntry> = read_entries_at(home)?
        .into_iter()
        .map(|entry| ((entry.agent.clone(), entry.session_id.clone()), entry))
        .collect();
    for entry in delta.changed {
        catalog.insert((entry.agent.clone(), entry.session_id.clone()), entry);
    }
    let reattributed_sessions = reattribute_catalog_entries(&mut catalog, &mut memo);
    memo.persist();
    let mut body = String::new();
    for entry in catalog.values() {
        body.push_str(&serde_json::to_string(entry)?);
        body.push('\n');
    }
    legacy_archive::atomic_write::atomic_write(&catalog_path, body.as_bytes())
        .with_context(|| format!("write hot-refreshed catalog {}", catalog_path.display()))?;
    if let Ok(mut cache) = LIVE_DELTA_CACHE.lock() {
        *cache = None;
    }

    Ok(HotRefreshReport {
        schema: CATALOG_REFRESH_SCHEMA.to_string(),
        catalog_path: catalog_path.display().to_string(),
        catalog_present: true,
        scanned_live_sessions,
        changed_sessions,
        admitted_sessions,
        reattributed_sessions,
        total_sessions: catalog.len(),
        wall_ms: started.elapsed().as_millis() as u64,
        recommendation: None,
    })
}

fn scan_live_entries(home: &Path, user_home: &Path) -> BTreeMap<(String, String), CatalogEntry> {
    scan_live_entries_with_progress(home, user_home, Instant::now(), &mut |_| {})
}

fn scan_live_entries_with_progress(
    home: &Path,
    user_home: &Path,
    started: Instant,
    on_progress: &mut impl FnMut(&RebuildProgress),
) -> BTreeMap<(String, String), CatalogEntry> {
    let mut by_id: BTreeMap<(String, String), CatalogEntry> = BTreeMap::new();
    let mut progress = RebuildProgress::preparing();
    on_progress(&progress);

    let agents = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Gemini,
        AgentKind::Grok,
        AgentKind::Junie,
    ];
    for (agent_offset, agent) in agents.into_iter().enumerate() {
        progress.stage = RebuildStage::ScanningSources;
        progress.agent = Some(agent.as_str());
        progress.agent_index = agent_offset + 1;
        progress.io = CatalogIoStats::default();
        progress.elapsed_ms = started.elapsed().as_millis() as u64;
        on_progress(&progress);

        let root = agent_source_root(agent, user_home);
        if !root.exists() {
            continue;
        }
        let catalog = match SessionCatalog::new(agent, &root) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let scan = catalog.scan_with_stats_and_progress(|io| {
            progress.io = io.clone();
            progress.sessions = by_id.len();
            progress.elapsed_ms = started.elapsed().as_millis() as u64;
            on_progress(&progress);
        });
        let sources = match scan.result {
            Ok(s) => s,
            Err(_) => continue,
        };
        for source in sources {
            if !is_primary_catalog_source(agent, &source.path) {
                continue;
            }
            let entry = entry_from_source(agent, &source);
            by_id.insert((entry.agent.clone(), entry.session_id.clone()), entry);
        }
        progress.io = scan.stats;
        progress.sessions = by_id.len();
        progress.elapsed_ms = started.elapsed().as_millis() as u64;
        on_progress(&progress);
    }

    progress.stage = RebuildStage::EnrichingSessions;
    progress.agent = None;
    progress.sessions = by_id.len();
    progress.elapsed_ms = started.elapsed().as_millis() as u64;
    on_progress(&progress);
    enrich_from_sessions_discovery(&mut by_id, user_home);

    progress.stage = RebuildStage::SnapshottingRuntimeRuns;
    progress.sessions = by_id.len();
    progress.elapsed_ms = started.elapsed().as_millis() as u64;
    on_progress(&progress);
    enrich_runtime_runs(&mut by_id, user_home);
    let mut memo = RemoteMemo::load(home);
    reattribute_catalog_entries(&mut by_id, &mut memo);
    memo.persist();

    by_id
}

fn push_sample(
    samples: &mut Vec<StalenessSample>,
    entry: &CatalogEntry,
    class: &str,
    catalog_len: Option<u64>,
    live_len: Option<u64>,
    catalog_mtime_ns: Option<u64>,
    live_mtime_ns: Option<u64>,
) {
    if samples.len() >= STATUS_SAMPLE_CAP {
        return;
    }
    samples.push(StalenessSample {
        agent: entry.agent.clone(),
        session_id: entry.session_id.clone(),
        project: entry.project.clone(),
        machine: entry.machine.clone(),
        source_path: entry.source_path.clone(),
        class: class.to_string(),
        catalog_len,
        live_len,
        catalog_mtime_ns,
        live_mtime_ns,
    });
}

fn classify_readiness(
    catalog_present: bool,
    catalog_sessions: usize,
    live_sessions: usize,
    counts: &StalenessCounts,
) -> CatalogReadiness {
    if !catalog_present {
        return CatalogReadiness::Missing;
    }
    if catalog_sessions == 0 && live_sessions == 0 {
        return CatalogReadiness::Empty;
    }
    if counts.missing_source > 0
        && counts.current == 0
        && counts.stale == 0
        && counts.unadmitted == 0
        && live_sessions == 0
    {
        return CatalogReadiness::SourcesMissing;
    }
    if counts.rebuild_pressure() > 0 || counts.fingerprint_unknown > 0 {
        return CatalogReadiness::NeedsRebuild;
    }
    CatalogReadiness::Fresh
}

fn recommendations_for(readiness: CatalogReadiness, counts: &StalenessCounts) -> Vec<String> {
    let mut out = Vec::new();
    match readiness {
        CatalogReadiness::Missing => {
            out.push("Run `aicx catalog rebuild` to create ~/.aicx/catalog/sessions.jsonl.".into());
            out.push(
                "Then `aicx index` (optionally `--cache-extracts`) to publish CURRENT.".into(),
            );
        }
        CatalogReadiness::Empty => {
            out.push(
                "No agent session sources found under ~/.claude|codex|gemini|grok|junie or vibecrafted runtime_runs."
                    .into(),
            );
            out.push(
                "Sync JSONL into those roots on this host, or set AICX_HOME only after sources resolve here."
                    .into(),
            );
        }
        CatalogReadiness::Fresh => {
            out.push("Catalog fingerprints match live sources.".into());
            out.push(
                "Check search lag with `aicx index status` — catalog fresh ≠ index CURRENT fresh."
                    .into(),
            );
        }
        CatalogReadiness::NeedsRebuild => {
            if counts.unadmitted > 0 {
                out.push(format!(
                    "{} live session(s) not in catalog — `aicx catalog rebuild` admits them.",
                    counts.unadmitted
                ));
            }
            if counts.stale > 0 {
                out.push(format!(
                    "{} catalog row(s) have drifted size/mtime — rebuild refreshes fingerprints (index still re-parses on live fingerprint even without rebuild).",
                    counts.stale
                ));
            }
            if counts.missing_source > 0 {
                out.push(format!(
                    "{} catalog row(s) point at missing paths — path must resolve on the indexing host (absolute paths; sync sources, not only sessions.jsonl).",
                    counts.missing_source
                ));
            }
            if counts.fingerprint_unknown > 0 {
                out.push(format!(
                    "{} row(s) lack usable fingerprints — rebuild to stamp source_len/source_mtime_ns.",
                    counts.fingerprint_unknown
                ));
            }
            out.push("After rebuild: `aicx index status` then `aicx index` if readiness is stale_index/pending.".into());
        }
        CatalogReadiness::SourcesMissing => {
            out.push(
                "Catalog rows exist but no live sources resolve — this host cannot index content until JSONL lands under agent roots with the same absolute paths, or you rebuild on the machine that owns the sources."
                    .into(),
            );
            out.push(
                "Do not co-locate dense 0.6b and 8b generations as one CURRENT; dimension/model mismatch is fail-closed. Prefer one index owner host."
                    .into(),
            );
        }
    }
    out
}

fn multi_host_notes(by_machine: &BTreeMap<String, usize>, counts: &StalenessCounts) -> Vec<String> {
    let mut notes = vec![
        "Catalog discovers only local agent source roots on the host running rebuild/status.".into(),
        "Alternative store drop dirs are not scanned; put JSONL under ~/.claude/projects, ~/.codex/sessions, ~/.gemini/tmp, ~/.grok/sessions, ~/.junie/sessions, or ~/.vibecrafted/control_plane/runtime_runs.".into(),
        "AICX_HOME / [storage].home relocates the whole home (catalog+index+extracts), not a second session intake path.".into(),
        "Dense indexes are model+dimension locked. Laptop 0.6b vectors must not merge into the owner's 8b CURRENT — lexical Tantivy can be rebuilt on the owner host from shared sources.".into(),
        "Remote agents: `aicx serve --transport http` with Bearer token (not OAuth). Prefer one index owner and point remotes at its streamable HTTP + embedder URL.".into(),
    ];
    if by_machine.len() > 1 {
        notes.push(format!(
            "Catalog already stamps {} machine identity bucket(s): {} — identity only; paths still must resolve here.",
            by_machine.len(),
            by_machine
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if counts.missing_source > 0 {
        notes.push(
            "missing_source is the usual multi-machine failure mode: catalog copied without matching source trees/paths."
                .into(),
        );
    }
    notes
}

/// Resolve a session_id → source_path from the durable catalog (exact id match).
pub fn resolve_session(home: &Path, session_id: &str) -> Result<Option<CatalogEntry>> {
    let needle = session_id.trim();
    if needle.is_empty() {
        return Ok(None);
    }
    let entries = read_entries_at(home)?;
    for entry in &entries {
        if entry.session_id == needle
            || entry
                .logical_session_id
                .as_deref()
                .is_some_and(|id| id == needle)
        {
            return Ok(Some(entry.clone()));
        }
    }
    let mut prefixes: Vec<_> = entries
        .into_iter()
        .filter(|entry| {
            entry.session_id.starts_with(needle)
                || entry
                    .logical_session_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with(needle))
        })
        .collect();
    prefixes.sort_by(|left, right| {
        left.agent
            .cmp(&right.agent)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    prefixes
        .dedup_by(|left, right| left.agent == right.agent && left.session_id == right.session_id);
    match prefixes.len() {
        0 => Ok(None),
        1 => Ok(prefixes.pop()),
        count => anyhow::bail!(
            "session prefix `{needle}` is ambiguous across {count} catalog entries; use the full id"
        ),
    }
}

fn agent_source_root(agent: AgentKind, user_home: &Path) -> PathBuf {
    match agent {
        AgentKind::Claude => user_home.join(".claude").join("projects"),
        AgentKind::Codex => user_home.join(".codex").join("sessions"),
        AgentKind::Gemini => user_home.join(".gemini").join("tmp"),
        // Grok sessions live under `~/.grok/sessions/<cwd-encoded>/…`
        // (not the bare `~/.grok` tree, which also holds config noise).
        AgentKind::Grok => user_home.join(".grok").join("sessions"),
        AgentKind::Junie => user_home.join(".junie").join("sessions"),
    }
}

fn is_primary_catalog_source(agent: AgentKind, path: &Path) -> bool {
    agent != AgentKind::Grok
        || path.file_name().and_then(|name| name.to_str()) == Some("chat_history.jsonl")
}

fn entry_from_source(agent: AgentKind, source: &CatalogSource) -> CatalogEntry {
    let session_id = if agent == AgentKind::Grok {
        grok_session_id_from_path(&source.path).unwrap_or_else(|| source.source_id.clone())
    } else {
        source.source_id.clone()
    };
    let cwd = infer_cwd_from_path(agent, &source.path);
    let project = cwd
        .as_deref()
        .and_then(project_from_cwd)
        .or_else(|| infer_project_from_path(agent, &source.path))
        .map(|slug| canonicalize_project_slug(&slug));
    let date = source
        .fingerprint
        .modified_unix_nanos
        .checked_div(1_000_000_000)
        .and_then(|secs| {
            chrono::DateTime::from_timestamp(secs as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
        });
    CatalogEntry {
        schema: CATALOG_SCHEMA.to_string(),
        session_id: session_id.clone(),
        agent: agent.as_str().to_string(),
        project,
        date,
        cwd,
        source_path: source.path.display().to_string(),
        source_len: Some(source.fingerprint.len),
        source_mtime_ns: Some(source.fingerprint.modified_unix_nanos as u64),
        title: None,
        machine: hostname(),
        logical_session_id: if agent == AgentKind::Grok {
            Some(session_id)
        } else {
            source.logical_session_id.clone()
        },
    }
}

fn enrich_from_sessions_discovery(
    by_id: &mut BTreeMap<(String, String), CatalogEntry>,
    user_home: &Path,
) {
    let claude_root = user_home.join(".claude").join("projects");
    if claude_root.is_dir() {
        for info in crate::sessions::discover_claude_sessions(&claude_root, None, None) {
            merge_session_info(by_id, &info);
        }
    }
    let codex_root = user_home.join(".codex").join("sessions");
    if codex_root.is_dir() {
        for info in crate::sessions::discover_codex_sessions(&codex_root, None) {
            merge_session_info(by_id, &info);
        }
    }
    let gemini_root = user_home.join(".gemini").join("tmp");
    if gemini_root.is_dir() {
        for info in crate::sessions::discover_gemini_sessions(&gemini_root, None, None) {
            merge_session_info(by_id, &info);
        }
    }
    let junie_root = user_home.join(".junie").join("sessions");
    if junie_root.is_dir() {
        for info in crate::sessions::discover_junie_sessions(&junie_root, None) {
            merge_session_info(by_id, &info);
        }
    }
}

fn merge_session_info(
    by_id: &mut BTreeMap<(String, String), CatalogEntry>,
    info: &crate::sessions::SessionInfo,
) {
    let key = (info.agent.clone(), info.session_id.clone());
    let date = info
        .updated_at
        .or(info.started_at)
        .map(|dt| dt.format("%Y-%m-%d").to_string());
    let (source_len, source_mtime_ns) = live_source_fingerprint(&info.source_path)
        .map(|(len, mtime)| (Some(len), Some(mtime)))
        .unwrap_or((None, None));
    let entry = by_id.entry(key).or_insert_with(|| CatalogEntry {
        schema: CATALOG_SCHEMA.to_string(),
        session_id: info.session_id.clone(),
        agent: info.agent.clone(),
        project: info.project.clone(),
        date: date.clone(),
        cwd: info.repo_path.clone(),
        source_path: info.source_path.display().to_string(),
        source_len,
        source_mtime_ns,
        title: info.title.clone(),
        machine: hostname(),
        logical_session_id: None,
    });
    if let Some(repo_path) = info.repo_path.as_deref() {
        entry.cwd = Some(repo_path.to_string());
        if let Some(remote_project) = project_from_git_remote(repo_path) {
            entry.project = Some(remote_project);
        } else if entry.project.is_none() {
            entry.project = info.project.as_deref().map(canonicalize_project_slug);
        }
    } else if entry.project.is_none() {
        entry.project = info.project.as_deref().map(canonicalize_project_slug);
    }
    if entry.title.is_none() {
        entry.title = info.title.clone();
    }
    if entry.date.is_none() {
        entry.date = date;
    }
    if entry.source_path.is_empty() {
        entry.source_path = info.source_path.display().to_string();
    }
    // Refresh fingerprint whenever discovery sees the live file — rebuild must
    // admit source appends even when session id/path are unchanged.
    if let Some((len, mtime)) = live_source_fingerprint(&info.source_path) {
        entry.source_len = Some(len);
        entry.source_mtime_ns = Some(mtime);
    }
}

fn enrich_runtime_runs(by_id: &mut BTreeMap<(String, String), CatalogEntry>, user_home: &Path) {
    let runs = user_home
        .join(".vibecrafted")
        .join("control_plane")
        .join("runtime_runs");
    if !runs.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(&runs) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let run_id = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if run_id.is_empty() {
            continue;
        }
        let transcript = path.join("transcript.log");
        if !transcript.is_file() {
            continue;
        }
        let (source_len, source_mtime_ns) = live_source_fingerprint(&transcript)
            .map(|(len, mtime)| (Some(len), Some(mtime)))
            .unwrap_or((None, None));
        let date = source_mtime_ns.and_then(|ns| {
            let secs = (ns / 1_000_000_000) as i64;
            chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.format("%Y-%m-%d").to_string())
        });
        let key = ("vibecrafted".to_string(), run_id.clone());
        let entry = by_id.entry(key).or_insert_with(|| CatalogEntry {
            schema: CATALOG_SCHEMA.to_string(),
            session_id: run_id,
            agent: "vibecrafted".to_string(),
            project: Some("vetcoders/vibecrafted".to_string()),
            date,
            cwd: None,
            source_path: transcript.display().to_string(),
            source_len,
            source_mtime_ns,
            title: Some("runtime_run transcript".to_string()),
            machine: hostname(),
            logical_session_id: None,
        });
        if let Some((len, mtime)) = live_source_fingerprint(&transcript) {
            entry.source_len = Some(len);
            entry.source_mtime_ns = Some(mtime);
        }
    }
}

fn infer_cwd_from_path(agent: AgentKind, path: &Path) -> Option<String> {
    match agent {
        AgentKind::Claude => {
            // Ground truth first: Claude session events carry `cwd` verbatim.
            // The directory slug is lossy — every `-` inside a real path
            // component ("vc-workspace", "vibecrafted-suite") decodes into a
            // bogus `/`, which fabricates identities like `suite/vibecrafted`
            // and a cwd no reattribution can ever `git -C` into.
            sniff_claude_cwd(path).or_else(|| {
                // ~/.claude/projects/<encoded-cwd>/<session>.jsonl — lossy
                // last resort for unreadable/headless files.
                path.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|encoded| encoded.replace('-', "/"))
            })
        }
        AgentKind::Grok => {
            // ~/.grok/sessions/<cwd-encoded>/<session>/...
            let encoded_cwd = path.ancestors().find(|ancestor| {
                ancestor
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    == Some("sessions")
            })?;
            encoded_cwd
                .file_name()
                .and_then(|name| name.to_str())
                .map(crate::sessions::decode_percent_encoded_path)
        }
        _ => None,
    }
}

/// Read the working directory out of a Claude session head.
///
/// Leaf/summary records at the top of a JSONL carry no `cwd`; the first
/// real event does. The scan is bounded (lines and bytes) so a session
/// whose head is one enormous pasted line cannot stall catalog admission.
fn sniff_claude_cwd(path: &Path) -> Option<String> {
    use std::io::{BufRead, Read};
    const MAX_LINES: usize = 64;
    const MAX_BYTES: u64 = 256 * 1024;

    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file.take(MAX_BYTES));
    let mut line = String::new();
    for _ in 0..MAX_LINES {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(cwd) = value.get("cwd").and_then(|v| v.as_str())
            && !cwd.is_empty()
        {
            return Some(cwd.to_string());
        }
    }
    None
}

fn grok_session_id_from_path(path: &Path) -> Option<String> {
    path.parent()?
        .file_name()?
        .to_str()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn infer_project_from_path(agent: AgentKind, path: &Path) -> Option<String> {
    let cwd = infer_cwd_from_path(agent, path)?;
    project_from_cwd(&cwd)
}

fn project_from_cwd(cwd: &str) -> Option<String> {
    let seg = cwd
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())?;
    // Prefer owner/repo when two trailing segments look like a git path.
    let parts: Vec<&str> = cwd
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .filter(|s| !s.is_empty())
        .take(2)
        .collect();
    if parts.len() == 2 {
        let repo = parts[0];
        let owner = parts[1];
        if owner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && repo
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            && owner.len() >= 2
            && repo.len() >= 2
        {
            return Some(canonicalize_project_slug(&format!("{owner}/{repo}")));
        }
    }
    Some(canonicalize_project_slug(seg))
}

fn reattribute_catalog_entries(
    entries: &mut BTreeMap<(String, String), CatalogEntry>,
    memo: &mut RemoteMemo,
) -> usize {
    let mut changed = 0usize;
    for entry in entries.values_mut() {
        let Some(cwd) = entry.cwd.as_deref().filter(|cwd| !cwd.is_empty()) else {
            continue;
        };
        if let Some(project) = memo.project_for(cwd) {
            let project = canonicalize_project_slug(&project);
            if entry.project.as_deref() != Some(project.as_str()) {
                entry.project = Some(project);
                changed += 1;
            }
        }
    }
    changed
}

/// Memoized `origin` resolution, keyed by checkout path.
///
/// Reattribution runs over every catalog row on every hot refresh, and the
/// owner host carries ~500 distinct checkouts. One `git remote get-url`
/// subprocess per checkout costs ~10 s, paid again by each `continuity`,
/// `dashboard`, and wizard call. The memo lives next to the catalog and is
/// invalidated by the mtime of the checkout's git metadata, so a re-pointed
/// `origin` still lands on the next refresh without a spawn per row.
struct RemoteMemo {
    path: PathBuf,
    entries: BTreeMap<String, RemoteMemoEntry>,
    dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RemoteMemoEntry {
    /// Git metadata entry discovered for this checkout (`.git` directory, or
    /// the link file of a worktree/submodule).
    git_path: String,
    git_mtime_ns: u64,
    /// mtime of `<git>/config` when the checkout owns a real git directory —
    /// that file is where `origin` actually lives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config_mtime_ns: Option<u64>,
    /// Resolved `owner/repo`. Absent means git was asked and had no origin;
    /// that answer is cached too, so unremoted checkouts stop costing spawns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RemoteMemoFile {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    entries: BTreeMap<String, RemoteMemoEntry>,
}

impl RemoteMemo {
    fn load(home: &Path) -> Self {
        let path = catalog_dir_for(home).join(REMOTE_MEMO_FILENAME);
        let entries = fs::read_to_string(&path)
            .ok()
            .and_then(|body| serde_json::from_str::<RemoteMemoFile>(&body).ok())
            .filter(|file| file.schema == REMOTE_MEMO_SCHEMA)
            .map(|file| file.entries)
            .unwrap_or_default();
        Self {
            path,
            entries,
            dirty: false,
        }
    }

    fn project_for(&mut self, cwd: &str) -> Option<String> {
        let path = Path::new(cwd);
        // A relative cwd (`.` shows up in older rows) resolves against
        // whichever directory the process happens to run in, so any answer
        // would be an accident of invocation — and memoizing it would make
        // that accident stick.
        if !path.is_absolute() {
            return None;
        }
        let stamp = git_metadata_stamp(path)?;
        if let Some(hit) = self.entries.get(cwd)
            && hit.git_path == stamp.git_path
            && hit.git_mtime_ns == stamp.git_mtime_ns
            && hit.config_mtime_ns == stamp.config_mtime_ns
        {
            return hit.project.clone();
        }
        let project = project_from_git_remote(cwd);
        self.entries.insert(
            cwd.to_string(),
            RemoteMemoEntry {
                project: project.clone(),
                ..stamp
            },
        );
        self.dirty = true;
        project
    }

    /// Best-effort persist. A missing or unwritable memo only costs speed, so
    /// a failure here must never fail the catalog operation that owns it.
    fn persist(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let file = RemoteMemoFile {
            schema: REMOTE_MEMO_SCHEMA.to_string(),
            entries: self.entries.clone(),
        };
        let Ok(body) = serde_json::to_vec(&file) else {
            return;
        };
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = legacy_archive::atomic_write::atomic_write(&self.path, &body);
    }
}

/// Fingerprint the git metadata that decides a checkout's `origin`.
///
/// Walks up like git itself does, so a session whose cwd sits inside a
/// subdirectory of a repository resolves the same way `git -C` would.
fn git_metadata_stamp(cwd: &Path) -> Option<RemoteMemoEntry> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        let git = dir.join(".git");
        if let Ok(meta) = fs::symlink_metadata(&git) {
            let config_mtime_ns = if meta.is_dir() {
                fs::metadata(git.join("config"))
                    .ok()
                    .and_then(|meta| mtime_unix_ns(&meta))
            } else {
                None
            };
            return Some(RemoteMemoEntry {
                git_path: git.display().to_string(),
                git_mtime_ns: mtime_unix_ns(&meta).unwrap_or(0),
                config_mtime_ns,
                project: None,
            });
        }
        current = dir.parent();
    }
    None
}

fn mtime_unix_ns(meta: &fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|since| since.as_nanos().min(u64::MAX as u128) as u64)
}

fn project_from_git_remote(cwd: &str) -> Option<String> {
    let path = Path::new(cwd);
    if !path.is_dir() {
        return None;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    project_slug_from_remote(String::from_utf8_lossy(&output.stdout).trim())
}

pub fn project_slug_from_remote(remote: &str) -> Option<String> {
    let trimmed = remote
        .trim()
        .split(['?', '#'])
        .next()?
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let path = if let Some((_, rest)) = trimmed.split_once("://") {
        rest.split_once('/')?.1
    } else if let Some((_, rest)) = trimmed.rsplit_once(':') {
        rest
    } else {
        trimmed
    };
    let mut parts = path.split('/').filter(|part| !part.is_empty()).rev();
    let repository = parts.next()?.trim();
    let organization = parts.next()?.trim();
    if organization.is_empty() || repository.is_empty() {
        return None;
    }
    Some(canonicalize_project_slug(&format!(
        "{organization}/{repository}"
    )))
}

/// Case-fold and normalize separators so catalog admission does not mint
/// parallel buckets (`VetCoders/vibecrafted` vs `vetcoders/vibecrafted`).
/// Hyphens inside a segment stay; only path separators are folded.
pub(crate) fn canonicalize_project_slug(raw: &str) -> String {
    raw.replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| segment.trim().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let output = std::process::Command::new("hostname").output().ok()?;
            if !output.status.success() {
                return None;
            }
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if name.is_empty() { None } else { Some(name) }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("aicx-catalog-{label}-{nanos}-{n}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn live_delta_reports_unadmitted_until_rebuild_admits_them() {
        let dir = test_root("live-delta");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        fs::create_dir_all(user.join(".claude").join("projects").join("proj")).unwrap();
        let session = user
            .join(".claude")
            .join("projects")
            .join("proj")
            .join("bbbbbbbb-cccc-dddd-eeee-ffffffffffff.jsonl");
        let mut f = File::create(&session).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","sessionId":"bbbbbbbb-cccc-dddd-eeee-ffffffffffff","message":{{"content":"live window probe"}}}}"#
        )
        .unwrap();

        // No durable catalog yet: the whole live surface is unadmitted.
        let before = live_delta_uncached(&home, &user, 0).unwrap();
        assert_eq!(before.live_sessions, 1);
        assert_eq!(before.unadmitted.len(), 1);
        assert!(before.newest_live_mtime_ns.is_some());
        assert_eq!(before.unadmitted[0].agent, "claude");

        // Rebuild admits the session — the delta must drain to zero.
        rebuild(&home, &user).unwrap();
        let after = live_delta_uncached(&home, &user, 0).unwrap();
        assert_eq!(after.live_sessions, 1);
        assert!(
            after.unadmitted.is_empty(),
            "admitted session still reported unadmitted: {:?}",
            after.unadmitted
        );
    }

    #[test]
    fn claude_cwd_prefers_session_event_truth_over_lossy_slug() {
        let dir = test_root("cwd-sniff");
        // Slug whose dashes are NOT all separators: naive decode fabricates
        // `/Volumes/vc/workspace/.../suite/vibecrafted`.
        let project_dir = dir.join("-Volumes-vc-workspace-vetcoders-vibecrafted-suite-vibecrafted");
        fs::create_dir_all(&project_dir).unwrap();
        let session = project_dir.join("aaaa.jsonl");
        let mut f = File::create(&session).unwrap();
        writeln!(f, r#"{{"type":"summary","leafUuid":"x"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"attachment","cwd":"/Volumes/vc-workspace/vetcoders/vibecrafted-suite/vibecrafted"}}"#
        )
        .unwrap();

        let cwd = infer_cwd_from_path(AgentKind::Claude, &session).unwrap();
        assert_eq!(
            cwd,
            "/Volumes/vc-workspace/vetcoders/vibecrafted-suite/vibecrafted"
        );

        // Head without cwd → lossy slug decode stays as the last resort.
        let bare = project_dir.join("bbbb.jsonl");
        fs::write(&bare, "{\"type\":\"summary\"}\n").unwrap();
        let cwd = infer_cwd_from_path(AgentKind::Claude, &bare).unwrap();
        assert_eq!(
            cwd,
            "/Volumes/vc/workspace/vetcoders/vibecrafted/suite/vibecrafted"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_roundtrip_writes_zero_cards() {
        let dir = test_root("roundtrip");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        fs::create_dir_all(user.join(".claude").join("projects").join("proj")).unwrap();
        let session = user
            .join(".claude")
            .join("projects")
            .join("proj")
            .join("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
        let mut f = File::create(&session).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","sessionId":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","message":{{"content":"hi"}}}}"#
        )
        .unwrap();
        let report = rebuild(&home, &user).unwrap();
        assert_eq!(report.cards_written, 0);
        assert!(Path::new(&report.catalog_path).exists());
        assert!(!home.join("store").exists());
        let resolved = resolve_session(&home, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
            .unwrap()
            .expect("session in catalog");
        assert_eq!(resolved.agent, "claude");
        assert!(resolved.source_path.contains("aaaaaaaa-bbbb"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_identities_reads_catalog() {
        let home = test_root("identities");
        fs::create_dir_all(catalog_dir_for(&home)).unwrap();
        let entry = CatalogEntry {
            schema: CATALOG_SCHEMA.to_string(),
            session_id: "s1".into(),
            agent: "claude".into(),
            project: Some("vetcoders/mlx-lm".into()),
            date: Some("2026-07-22".into()),
            cwd: None,
            source_path: "/tmp/x".into(),
            source_len: None,
            source_mtime_ns: None,
            title: None,
            machine: None,
            logical_session_id: None,
        };
        let mut case_variant = entry.clone();
        case_variant.session_id = "s2".into();
        case_variant.project = Some("vetcoders/mlx-lm".into());
        fs::write(
            sessions_path_for(&home),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&entry).unwrap(),
                serde_json::to_string(&case_variant).unwrap()
            ),
        )
        .unwrap();
        let ids = project_identities_from_catalog_at(&home).unwrap();
        assert_eq!(ids, vec!["vetcoders/mlx-lm".to_string()]);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_session_rejects_ambiguous_prefix() {
        let home = test_root("ambiguous-prefix");
        fs::create_dir_all(catalog_dir_for(&home)).unwrap();
        let entries = ["abcdef-111", "abcdef-222"]
            .into_iter()
            .map(|session_id| CatalogEntry {
                schema: CATALOG_SCHEMA.to_string(),
                session_id: session_id.to_string(),
                agent: "codex".to_string(),
                project: None,
                date: None,
                cwd: None,
                source_path: format!("/tmp/{session_id}.jsonl"),
                source_len: None,
                source_mtime_ns: None,
                title: None,
                machine: None,
                logical_session_id: None,
            })
            .map(|entry| serde_json::to_string(&entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(sessions_path_for(&home), format!("{entries}\n")).unwrap();
        let error = resolve_session(&home, "abcdef").unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
        assert!(resolve_session(&home, "abcdef-111").unwrap().is_some());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn grok_catalog_keeps_chat_history_and_decodes_cwd() {
        let path = Path::new(
            "/Users/test/.grok/sessions/%2FVolumes%2Fvc-workspace%2Fvetcoders%2Fvibecrafted/\
             019f5407-5b0c-7363-b210-1093f26a41f7/chat_history.jsonl",
        );
        assert!(is_primary_catalog_source(AgentKind::Grok, path));
        assert!(!is_primary_catalog_source(
            AgentKind::Grok,
            &path.with_file_name("events.jsonl")
        ));
        assert_eq!(
            infer_cwd_from_path(AgentKind::Grok, path).as_deref(),
            Some("/Volumes/vc-workspace/vetcoders/vibecrafted")
        );
        assert_eq!(
            infer_project_from_path(AgentKind::Grok, path).as_deref(),
            Some("vetcoders/vibecrafted")
        );
        assert_eq!(
            grok_session_id_from_path(path).as_deref(),
            Some("019f5407-5b0c-7363-b210-1093f26a41f7")
        );
    }

    #[test]
    fn status_reports_missing_catalog_and_unadmitted_live() {
        let dir = test_root("status-unadmitted");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        let project = user.join(".claude").join("projects").join("proj");
        fs::create_dir_all(&project).unwrap();
        let session = project.join("bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
        let mut f = File::create(&session).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","sessionId":"bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee","message":{{"content":"hi"}}}}"#
        )
        .unwrap();

        let report = status(&home, &user).unwrap();
        assert_eq!(report.readiness, CatalogReadiness::Missing);
        assert_eq!(report.counts.unadmitted, 1);
        assert!(
            report
                .recommendations
                .iter()
                .any(|r| r.contains("catalog rebuild"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_marks_stale_when_live_fingerprint_drifts() {
        let dir = test_root("status-stale");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        let project = user.join(".claude").join("projects").join("proj");
        fs::create_dir_all(&project).unwrap();
        let session_id = "cccccccc-bbbb-cccc-dddd-eeeeeeeeeeee";
        let session = project.join(format!("{session_id}.jsonl"));
        let mut f = File::create(&session).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","sessionId":"{session_id}","message":{{"content":"v1"}}}}"#
        )
        .unwrap();
        rebuild(&home, &user).unwrap();

        // Append so size+mtime change.
        let mut f = fs::OpenOptions::new().append(true).open(&session).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","sessionId":"{session_id}","message":{{"content":"v2-append"}}}}"#
        )
        .unwrap();

        let report = status(&home, &user).unwrap();
        assert_eq!(report.readiness, CatalogReadiness::NeedsRebuild);
        assert_eq!(report.counts.stale, 1);
        assert_eq!(report.counts.unadmitted, 0);
        assert!(
            report
                .samples
                .iter()
                .any(|s| s.class == "stale" && s.session_id == session_id)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_marks_fresh_after_rebuild() {
        let dir = test_root("status-fresh");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        let project = user.join(".claude").join("projects").join("proj");
        fs::create_dir_all(&project).unwrap();
        let session = project.join("dddddddd-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
        let mut f = File::create(&session).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","sessionId":"dddddddd-bbbb-cccc-dddd-eeeeeeeeeeee","message":{{"content":"hi"}}}}"#
        )
        .unwrap();
        rebuild(&home, &user).unwrap();
        let report = status(&home, &user).unwrap();
        assert_eq!(report.readiness, CatalogReadiness::Fresh);
        assert_eq!(report.counts.current, 1);
        assert_eq!(report.counts.rebuild_pressure(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_marks_missing_source_when_path_gone() {
        let dir = test_root("status-missing-source");
        let home = dir.join(".aicx");
        fs::create_dir_all(catalog_dir_for(&home)).unwrap();
        let entry = CatalogEntry {
            schema: CATALOG_SCHEMA.to_string(),
            session_id: "ghost-session".into(),
            agent: "claude".into(),
            project: Some("Loctree/aicx".into()),
            date: Some("2026-07-26".into()),
            cwd: None,
            source_path: dir.join("does-not-exist.jsonl").display().to_string(),
            source_len: Some(10),
            source_mtime_ns: Some(1),
            title: None,
            machine: Some("laptop".into()),
            logical_session_id: None,
        };
        fs::write(
            sessions_path_for(&home),
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();
        let user = dir.join("user");
        fs::create_dir_all(&user).unwrap();
        let report = status(&home, &user).unwrap();
        assert_eq!(report.counts.missing_source, 1);
        assert_eq!(report.readiness, CatalogReadiness::SourcesMissing);
        assert!(report.by_machine.get("laptop").copied() == Some(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_does_not_stat_catalog_paths_outside_live_agent_roots() {
        let dir = test_root("status-untrusted-catalog-path");
        let home = dir.join(".aicx");
        fs::create_dir_all(catalog_dir_for(&home)).unwrap();
        let outside = dir.join("outside-agent-roots.jsonl");
        fs::write(
            &outside,
            "catalog data must not authorize filesystem access",
        )
        .unwrap();
        let (source_len, source_mtime_ns) = live_source_fingerprint(&outside).unwrap();
        let entry = CatalogEntry {
            schema: CATALOG_SCHEMA.to_string(),
            session_id: "untrusted-path".into(),
            agent: "claude".into(),
            project: Some("Loctree/aicx".into()),
            date: Some("2026-07-27".into()),
            cwd: None,
            source_path: outside.display().to_string(),
            source_len: Some(source_len),
            source_mtime_ns: Some(source_mtime_ns),
            title: None,
            machine: Some("laptop".into()),
            logical_session_id: None,
        };
        fs::write(
            sessions_path_for(&home),
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();
        let user = dir.join("user");
        fs::create_dir_all(&user).unwrap();

        let report = status(&home, &user).unwrap();

        assert_eq!(report.counts.current, 0);
        assert_eq!(report.counts.missing_source, 1);
        assert_eq!(report.readiness, CatalogReadiness::SourcesMissing);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remote_slug_parser_accepts_https_and_scp_shapes() {
        assert_eq!(
            project_slug_from_remote("https://github.com/Loctree/aicx.git"),
            Some("loctree/aicx".to_string())
        );
        assert_eq!(
            project_slug_from_remote("https://github.com/vetcoders/vibecrafted.git"),
            Some("vetcoders/vibecrafted".to_string())
        );
        assert_eq!(
            project_slug_from_remote("git@github.com:vetcoders/pensieve.git"),
            Some("vetcoders/pensieve".to_string())
        );
    }

    #[test]
    fn hot_refresh_reattributes_existing_path_guess_from_git_origin() {
        let dir = test_root("refresh-reattribute");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        let repo = dir.join("Git").join("pensieve");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&user).unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["init", "--quiet"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args([
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/vetcoders/pensieve.git",
                ])
                .status()
                .unwrap()
                .success()
        );
        fs::create_dir_all(catalog_dir_for(&home)).unwrap();
        let source = repo.join("session.jsonl");
        fs::write(&source, "{\"type\":\"user\"}\n").unwrap();
        let entry = CatalogEntry {
            schema: CATALOG_SCHEMA.to_string(),
            session_id: "identity-session".into(),
            agent: "codex".into(),
            project: Some("Git/pensieve".into()),
            date: Some("2026-07-30".into()),
            cwd: Some(repo.display().to_string()),
            source_path: source.display().to_string(),
            source_len: None,
            source_mtime_ns: None,
            title: None,
            machine: None,
            logical_session_id: None,
        };
        fs::write(
            sessions_path_for(&home),
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();

        let report = refresh_hot(&home, &user, 0).unwrap();
        assert_eq!(report.reattributed_sessions, 1);
        let refreshed = read_entries_at(&home).unwrap();
        assert_eq!(refreshed[0].project.as_deref(), Some("vetcoders/pensieve"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_slug_canonicalizes_case_and_separators() {
        assert_eq!(
            canonicalize_project_slug("VetCoders/vibecrafted"),
            "vetcoders/vibecrafted"
        );
        assert_eq!(
            canonicalize_project_slug(r"VetCoders\CodeScribe"),
            "vetcoders/codescribe"
        );
        assert_eq!(canonicalize_project_slug("/vibecrafted/"), "vibecrafted");
    }
}
