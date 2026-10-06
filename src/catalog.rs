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
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::legacy_archive::{self};
use crate::session_catalog::{
    self, AgentKind, CatalogError, CatalogIoStats, CatalogSource, ScopedChildIdentity,
    SessionCatalog, SourceFingerprint, is_uuid,
};

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
    /// Independent evidence for optional source artifacts. Legacy rows lack
    /// this receipt and are refreshed when a bundled source is next admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_bundle_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_session_id: Option<String>,
    /// Structural subagent provenance (e.g. `subagent:guardian`); travels from
    /// session discovery so harness-wrapper prompts are never mistaken for
    /// operator utterances downstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
}

/// Size + mtime-ns for a path. Returns `None` when the file is unreadable.
pub fn live_source_fingerprint(path: &Path) -> Option<(u64, u64)> {
    let fingerprint = live_source_bundle_fingerprint(path)?;
    // u64 covers unix nanos until year ~2554; truncation is intentional.
    Some((fingerprint.len, fingerprint.modified_unix_nanos as u64))
}

/// Live source evidence with distinct artifact identity, including Copilot's
/// optional workspace sidecar. Size and mtime remain physical measurements.
pub fn live_source_bundle_fingerprint(path: &Path) -> Option<session_catalog::SourceFingerprint> {
    session_catalog::source_bundle_fingerprint(path).ok()
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
            agent_total: 7,
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
/// Walks registered agent roots via
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

    let by_id = scan_live_entries_with_progress(home, user_home, started, &mut on_progress)?;

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
    let live = scan_live_entries(home, user_home)?;

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
                if live_len == cat_len
                    && live_mtime == cat_mtime
                    && live_entry.and_then(|live| live.source_bundle_fingerprint.as_ref())
                        == entry.source_bundle_fingerprint.as_ref() =>
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

// A synthetic test delta must survive cache invalidation by other tests and
// must never fall through to the developer's real session roots.
#[cfg(test)]
thread_local! {
    #[allow(clippy::type_complexity)]
    static TEST_LIVE_DELTA_CACHE: std::cell::RefCell<Option<(PathBuf, u128, LiveDelta)>> = const {
        std::cell::RefCell::new(None)
    };
}

pub fn live_delta(home: &Path, user_home: &Path, cutoff_unix_ns: u128) -> Result<LiveDelta> {
    #[cfg(test)]
    if let Some(delta) = TEST_LIVE_DELTA_CACHE.with(|fixture| {
        fixture
            .borrow()
            .as_ref()
            .and_then(|(fixture_home, cutoff, delta)| {
                (fixture_home == home
                    && cutoff.abs_diff(cutoff_unix_ns) <= LIVE_DELTA_CUTOFF_TOLERANCE_NS)
                    .then(|| delta.clone())
            })
    }) {
        return Ok(delta);
    }
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

/// Supply a thread-local delta so tests exercise the intents live window
/// without walking real agent roots or racing the process-global cache.
#[cfg(test)]
pub(crate) fn prime_live_delta_cache_for_tests(
    home: &Path,
    _user_home: &Path,
    cutoff_unix_ns: u128,
    delta: LiveDelta,
) {
    TEST_LIVE_DELTA_CACHE.with(|fixture| {
        *fixture.borrow_mut() = Some((home.to_path_buf(), cutoff_unix_ns, delta));
    });
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
    let known_fingerprints: BTreeMap<&str, (u64, u128, Option<&str>)> = catalog_by_key
        .values()
        .filter_map(|entry| {
            Some((
                entry.source_path.as_str(),
                (
                    entry.source_len?,
                    entry.source_mtime_ns? as u128,
                    entry.source_bundle_fingerprint.as_deref(),
                ),
            ))
        })
        .collect();
    let is_known = |path: &Path, fingerprint: &crate::session_catalog::SourceFingerprint| {
        known_fingerprints
            .get(path.to_string_lossy().as_ref())
            .is_some_and(|(len, modified, bundle)| {
                *len == fingerprint.len
                    && *modified == fingerprint.modified_unix_nanos
                    && *bundle == fingerprint.bundle_fingerprint.as_deref()
            })
    };

    let mut live_sessions = 0usize;
    let mut newest_live_mtime_ns: Option<u64> = None;
    let mut fresh: BTreeMap<(String, String), CatalogEntry> = BTreeMap::new();
    let agents = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Cursor,
        AgentKind::Gemini,
        AgentKind::Grok,
        AgentKind::Junie,
        AgentKind::Kimi,
        AgentKind::Copilot,
    ];
    for agent in agents {
        let root = agent_source_root(agent, user_home);
        if !(if agent == AgentKind::Copilot {
            root.try_exists()
                .with_context(|| format!("inspect copilot root {}", root.display()))?
        } else {
            root.exists()
        }) {
            continue;
        }
        let catalog = match SessionCatalog::new(agent, &root) {
            Ok(catalog) => catalog,
            Err(error) if agent == AgentKind::Copilot => return Err(error.into()),
            Err(_) => continue,
        };
        let scan = match catalog.scan_hot_window_skipping(cutoff_unix_ns, &is_known) {
            Ok(scan) => scan,
            Err(error) if agent == AgentKind::Copilot => return Err(error.into()),
            Err(_) => continue,
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
                    || cataloged.source_bundle_fingerprint != entry.source_bundle_fingerprint
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

fn scan_live_entries(
    home: &Path,
    user_home: &Path,
) -> Result<BTreeMap<(String, String), CatalogEntry>> {
    scan_live_entries_with_progress(home, user_home, Instant::now(), &mut |_| {})
}

fn scan_live_entries_with_progress(
    home: &Path,
    user_home: &Path,
    started: Instant,
    on_progress: &mut impl FnMut(&RebuildProgress),
) -> Result<BTreeMap<(String, String), CatalogEntry>> {
    let mut by_id: BTreeMap<(String, String), CatalogEntry> = BTreeMap::new();
    let mut progress = RebuildProgress::preparing();
    on_progress(&progress);

    let agents = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Cursor,
        AgentKind::Gemini,
        AgentKind::Grok,
        AgentKind::Junie,
        AgentKind::Kimi,
        AgentKind::Copilot,
    ];
    for (agent_offset, agent) in agents.into_iter().enumerate() {
        progress.stage = RebuildStage::ScanningSources;
        progress.agent = Some(agent.as_str());
        progress.agent_index = agent_offset + 1;
        progress.io = CatalogIoStats::default();
        progress.elapsed_ms = started.elapsed().as_millis() as u64;
        on_progress(&progress);

        let root = agent_source_root(agent, user_home);
        if !(if agent == AgentKind::Copilot {
            root.try_exists()
                .with_context(|| format!("inspect copilot root {}", root.display()))?
        } else {
            root.exists()
        }) {
            continue;
        }
        let catalog = match SessionCatalog::new(agent, &root) {
            Ok(c) => c,
            Err(error) if agent == AgentKind::Copilot => return Err(error.into()),
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
            Err(error) if agent == AgentKind::Copilot => return Err(error.into()),
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

    Ok(by_id)
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
                "No agent session sources found under ~/.claude|codex|gemini|grok|junie|kimi-code|cursor, ~/.copilot/session-state or vibecrafted runtime_runs."
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
        "Alternative store drop dirs are not scanned; put JSONL under ~/.claude/projects, ~/.codex/sessions, ~/.cursor/projects, ~/.gemini/tmp, ~/.grok/sessions, ~/.junie/sessions, ~/.kimi-code/sessions, ~/.copilot/session-state, or ~/.vibecrafted/control_plane/runtime_runs.".into(),
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

/// Resolve a session id through the canonical live-catalog matching policy.
///
/// The durable JSONL remains the source of rows, but it no longer owns a
/// second exact/prefix algorithm. Any non-canonical match is refused with the
/// loud substitution receipt produced by `session_catalog`.
pub fn resolve_session(home: &Path, session_id: &str) -> Result<Option<CatalogEntry>> {
    let needle = session_id.trim();
    if needle.is_empty() {
        return Ok(None);
    }
    let entries = read_entries_at(home)?;
    let mut hits = Vec::new();
    for agent in AgentKind::ALL {
        let sources = entries
            .iter()
            .filter(|entry| AgentKind::parse(&entry.agent) == Some(agent))
            .map(|entry| CatalogSource {
                agent,
                source_id: entry.session_id.clone(),
                logical_session_id: entry.logical_session_id.clone(),
                aliases: Vec::new(),
                filename_aliases: Vec::new(),
                scoped_children: Vec::<ScopedChildIdentity>::new(),
                path: PathBuf::from(&entry.source_path),
                identity_inferred: entry.logical_session_id.is_none(),
                fingerprint: SourceFingerprint {
                    len: entry.source_len.unwrap_or_default(),
                    modified_unix_nanos: entry.source_mtime_ns.unwrap_or_default() as u128,
                    physical_identity: Vec::new(),
                    bundle_fingerprint: entry.source_bundle_fingerprint.clone(),
                },
                header_truncated: false,
            })
            .collect::<Vec<_>>();
        if sources.is_empty() {
            continue;
        }
        match session_catalog::resolve_from_sources(agent, needle.to_owned(), sources) {
            Ok(resolved) => hits.push(resolved),
            Err(CatalogError::Missing { .. }) => {}
            Err(error) => return Err(anyhow::Error::new(error)),
        }
    }
    match hits.len() {
        0 => Ok(None),
        1 => {
            let resolved = hits.pop().expect("one resolver hit");
            if let Some(notice) = resolved.substitution_notice {
                // Loud, not fatal (W2-T12 → W4 recovery): a unique prefix or
                // alias resolving to one catalog id is the documented way to
                // name a session; the operator sees the substitution on
                // stderr and gets the session. Ambiguity stays an error.
                eprintln!("{notice}");
                crate::diagnostics::log_describe(&format!("catalog_resolve {notice}"));
            }
            Ok(entries.into_iter().find(|entry| {
                AgentKind::parse(&entry.agent) == Some(resolved.source.agent)
                    && entry.session_id == resolved.source.source_id
                    && entry.source_path.as_str() == resolved.source.path.to_string_lossy().as_ref()
            }))
        }
        count => anyhow::bail!(
            "session `{needle}` is ambiguous across {count} catalog entries; use the full id and agent"
        ),
    }
}

fn agent_source_root(agent: AgentKind, user_home: &Path) -> PathBuf {
    match agent {
        AgentKind::Claude => user_home.join(".claude").join("projects"),
        AgentKind::Codex => user_home.join(".codex").join("sessions"),
        // Cursor transcripts live under `~/.cursor/projects/<slug>/agent-transcripts/<uuid>/`
        // (the projects tree also holds non-transcript dirs).
        AgentKind::Cursor => user_home.join(".cursor").join("projects"),
        AgentKind::Gemini => user_home.join(".gemini").join("tmp"),
        // Grok sessions live under `~/.grok/sessions/<cwd-encoded>/…`
        // (not the bare `~/.grok` tree, which also holds config noise).
        AgentKind::Grok => user_home.join(".grok").join("sessions"),
        AgentKind::Junie => user_home.join(".junie").join("sessions"),
        AgentKind::Kimi => user_home.join(".kimi-code").join("sessions"),
        AgentKind::Copilot => session_catalog::copilot_session_root(user_home),
    }
}

fn is_primary_catalog_source(agent: AgentKind, path: &Path) -> bool {
    match agent {
        AgentKind::Grok => {
            path.file_name().and_then(|name| name.to_str()) == Some("chat_history.jsonl")
        }
        AgentKind::Cursor => {
            let has_transcripts_component = path
                .components()
                .any(|component| component.as_os_str() == "agent-transcripts");
            let stem = path.file_stem().and_then(|name| name.to_str());
            let parent = path
                .parent()
                .and_then(|dir| dir.file_name())
                .and_then(|name| name.to_str());
            // The layout contract is `<uuid>/<same uuid>.jsonl` — without the
            // UUID shape check a state file like `metadata/metadata.jsonl`
            // would acquire session identity and get indexed/extracted.
            has_transcripts_component
                && stem.is_some()
                && stem == parent
                && stem.is_some_and(is_uuid)
        }
        AgentKind::Copilot => session_catalog::is_copilot_source_file(path),
        _ => true,
    }
}

fn entry_from_source(agent: AgentKind, source: &CatalogSource) -> CatalogEntry {
    // Only selected sources are enriched. Reading a changed Copilot stream
    // through EOF is necessary because session.resume may occur well beyond
    // the catalog's bounded identity header and move the working repository.
    let copilot = (agent == AgentKind::Copilot)
        .then(|| crate::sessions::scan_copilot_session_file(&source.path, &source.source_id))
        .flatten();
    let session_id = if agent == AgentKind::Grok {
        grok_session_id_from_path(&source.path).unwrap_or_else(|| source.source_id.clone())
    } else {
        source.source_id.clone()
    };
    let codex_metadata = (agent == AgentKind::Codex)
        .then(|| crate::sessions::codex_session_metadata_from_source(&source.path))
        .flatten();
    let cwd = copilot
        .as_ref()
        .and_then(|metadata| metadata.repo_path.clone())
        .or_else(|| {
            codex_metadata
                .as_ref()
                .filter(|metadata| {
                    metadata.session_id.as_deref().is_some_and(|id| {
                        id.eq_ignore_ascii_case(
                            source
                                .logical_session_id
                                .as_deref()
                                .unwrap_or(&source.source_id),
                        )
                    })
                })
                .and_then(|metadata| metadata.cwd.clone())
        })
        .or_else(|| infer_cwd_from_path(agent, &source.path));
    let project = copilot
        .as_ref()
        .and_then(|metadata| metadata.project.clone())
        .or_else(|| {
            cwd.as_deref().and_then(|cwd| {
                if agent == AgentKind::Codex {
                    project_from_git_remote(cwd).or_else(|| project_from_cwd(cwd))
                } else {
                    project_from_cwd(cwd)
                }
            })
        })
        .or_else(|| infer_project_from_path(agent, &source.path))
        .map(|slug| canonicalize_project_slug(&slug));
    let date = copilot
        .as_ref()
        .and_then(|metadata| metadata.updated_at.or(metadata.started_at))
        .map(|timestamp| timestamp.format("%Y-%m-%d").to_string())
        .or_else(|| {
            if agent == AgentKind::Codex {
                codex_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.date.clone())
            } else {
                source
                    .fingerprint
                    .modified_unix_nanos
                    .checked_div(1_000_000_000)
                    .and_then(|secs| {
                        chrono::DateTime::from_timestamp(secs as i64, 0)
                            .map(|dt| dt.format("%Y-%m-%d").to_string())
                    })
            }
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
        source_bundle_fingerprint: source.fingerprint.bundle_fingerprint.clone(),
        title: copilot.and_then(|metadata| metadata.title),
        machine: hostname(),
        logical_session_id: if agent == AgentKind::Grok {
            Some(session_id)
        } else {
            source.logical_session_id.clone()
        },
        session_kind: codex_metadata.and_then(|metadata| metadata.session_kind),
    }
}

/// Recover old null scope columns from bounded, allowlisted Codex root
/// metadata. This is an in-memory view: identity, clocks and catalog stay put.
pub(crate) fn recover_catalog_scope_at(
    aicx_home: &Path,
    entry: &CatalogEntry,
) -> Result<CatalogEntry> {
    let mut recovered = entry.clone();
    if entry.agent != "codex" || (entry.cwd.is_some() && entry.project.is_some()) {
        return Ok(recovered);
    }
    let user_home =
        crate::os_user_home().context("resolve user home for catalog scope recovery")?;
    let source = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home)
        .resolve_file(entry.source_path.as_str())?;
    let metadata = crate::sessions::codex_session_metadata_from_source(&source)
        .context("read Codex metadata for catalog scope recovery")?;
    let Some(id) = metadata.session_id.as_deref() else {
        return Ok(recovered);
    };
    // A filename UUID is physical authority; its logical root may differ.
    // Cached logical identity, when present, must still match in full.
    let physical_matches = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| {
            if is_uuid(stem) {
                return Some(stem);
            }
            let start = stem.len().checked_sub(36)?;
            let candidate = stem.get(start..)?;
            (is_uuid(candidate)
                && start.checked_sub(1).is_some_and(|boundary| {
                    matches!(stem.as_bytes()[boundary], b'-' | b'_' | b'.')
                }))
            .then_some(candidate)
        })
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&entry.session_id));
    let identity_matches = entry.logical_session_id.as_deref().map_or_else(
        || physical_matches || id.eq_ignore_ascii_case(&entry.session_id),
        |logical| logical.eq_ignore_ascii_case(id),
    );
    if !identity_matches {
        return Ok(recovered);
    }
    if recovered.cwd.is_none() {
        recovered.cwd = metadata.cwd;
    }
    if recovered.project.is_none() {
        recovered.project = recovered.cwd.as_deref().and_then(|cwd| {
            project_from_git_remote(cwd)
                .or_else(|| project_from_cwd(cwd))
                .map(|project| canonicalize_project_slug(&project))
        });
    }
    if recovered.session_kind.is_none() {
        recovered.session_kind = metadata.session_kind;
    }
    Ok(recovered)
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
    let cursor_root = user_home.join(".cursor").join("projects");
    if cursor_root.is_dir() {
        for info in crate::sessions::discover_cursor_sessions(&cursor_root, None, None) {
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
    let fingerprint = live_source_bundle_fingerprint(&info.source_path);
    let source_len = fingerprint.as_ref().map(|fingerprint| fingerprint.len);
    let source_mtime_ns = fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.modified_unix_nanos as u64);
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
        source_bundle_fingerprint: fingerprint
            .and_then(|fingerprint| fingerprint.bundle_fingerprint),
        title: info.title.clone(),
        machine: hostname(),
        logical_session_id: None,
        session_kind: info.session_kind.clone(),
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
    if entry.session_kind.is_none() {
        entry.session_kind = info.session_kind.clone();
    }
    if entry.date.is_none() {
        entry.date = date;
    }
    if entry.source_path.is_empty() {
        entry.source_path = info.source_path.display().to_string();
    }
    // Refresh fingerprint whenever discovery sees the live file — rebuild must
    // admit source appends even when session id/path are unchanged.
    if let Some(fingerprint) = live_source_bundle_fingerprint(&info.source_path) {
        entry.source_len = Some(fingerprint.len);
        entry.source_mtime_ns = Some(fingerprint.modified_unix_nanos as u64);
        entry.source_bundle_fingerprint = fingerprint.bundle_fingerprint;
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
            source_bundle_fingerprint: None,
            title: Some("runtime_run transcript".to_string()),
            machine: hostname(),
            logical_session_id: None,
            session_kind: None,
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
        AgentKind::Cursor => {
            // ~/.cursor/projects/<slug>/agent-transcripts/<uuid>/<uuid>.jsonl —
            // the slug is the only cwd evidence on disk (rows carry none).
            // Same lossy idiom as the Claude fallback: every `-` inside a real
            // path component decodes into a bogus `/`.
            let slug = path
                .ancestors()
                .find(|ancestor| {
                    ancestor
                        .parent()
                        .and_then(Path::file_name)
                        .and_then(|name| name.to_str())
                        == Some("projects")
                })
                .and_then(|ancestor| ancestor.file_name().map(|name| name.to_owned()))?
                .into_string()
                .ok()?;
            Some(format!("/{}", slug.replace('-', "/")))
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
    let output = crate::git_env::git_command_isolated()
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

    #[test]
    fn copilot_broken_provider_root_fails_catalog_operations_and_preserves_prior_catalog() {
        let dir = test_root("copilot-provider-error");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        let root = user.join(".copilot").join("session-state");
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::write(&root, "not a directory").unwrap();
        fs::create_dir_all(catalog_dir_for(&home)).unwrap();
        let before = "existing durable catalog\n";
        fs::write(sessions_path_for(&home), before).unwrap();
        assert!(rebuild(&home, &user).is_err());
        assert_eq!(
            fs::read_to_string(sessions_path_for(&home)).unwrap(),
            before
        );
        assert!(live_delta_uncached(&home, &user, 0).is_err());
        fs::remove_file(&root).unwrap();
        fs::remove_file(sessions_path_for(&home)).unwrap();
        assert!(rebuild(&home, &user).unwrap().total_sessions == 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copilot_named_sdk_session_is_admitted_into_durable_catalog() {
        let dir = test_root("copilot-named-session");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        let id = "user-123-task-456";
        let directory = user.join(".copilot").join("session-state").join(id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("events.jsonl"), format!("{{\"type\":\"session.start\",\"timestamp\":\"2026-06-01T12:00:00Z\",\"data\":{{\"sessionId\":\"{id}\",\"context\":{{\"cwd\":\"/owner/repo\",\"repository\":\"owner/repo\"}}}}}}\n")).unwrap();
        rebuild(&home, &user).unwrap();
        let rows = read_entries_at(&home).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, id);
        assert_eq!(
            resolve_session(&home, "user-123")
                .unwrap()
                .unwrap()
                .session_id,
            id
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copilot_catalog_hot_refresh_observes_sidecar_metadata_only_change() {
        let dir = test_root("copilot-sidecar-refresh");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        let id = "12345678-1234-1234-1234-123456789abc";
        let session = user.join(".copilot").join("session-state").join(id);
        fs::create_dir_all(&session).unwrap();
        let events = session.join("events.jsonl");
        fs::write(&events, format!("{{\"type\":\"session.start\",\"timestamp\":\"2026-06-01T12:00:00Z\",\"data\":{{\"sessionId\":\"{id}\"}}}}\n")).unwrap();
        let sidecar = session.join("workspace.yaml");
        fs::write(
            &sidecar,
            "cwd: /repo/first\nrepository: owner/first\nname: first title\n",
        )
        .unwrap();
        let report = rebuild(&home, &user).unwrap();
        assert_eq!(report.agents.get("copilot"), Some(&1));
        let entry = read_entries_at(&home).unwrap().pop().unwrap();
        assert_eq!(entry.session_id, id);
        assert_eq!(entry.project.as_deref(), Some("owner/first"));
        assert_eq!(entry.title.as_deref(), Some("first title"));
        assert!(
            live_delta_uncached(&home, &user, 0)
                .unwrap()
                .changed
                .is_empty()
        );
        fs::write(
            &sidecar,
            "cwd: /repo/other\nrepository: owner/other\nname: other title\n",
        )
        .unwrap();
        filetime::set_file_mtime(
            &sidecar,
            filetime::FileTime::from_unix_time(
                (entry.source_mtime_ns.unwrap() / 1_000_000_000) as i64 + 2,
                0,
            ),
        )
        .unwrap();
        let delta = live_delta_uncached(&home, &user, 0).unwrap();
        assert_eq!(delta.changed.len(), 1);
        assert_eq!(delta.changed[0].project.as_deref(), Some("owner/other"));
        assert_eq!(delta.changed[0].title.as_deref(), Some("other title"));
        refresh_hot(&home, &user, 0).unwrap();
        let entry = read_entries_at(&home).unwrap().pop().unwrap();
        assert_eq!(entry.cwd.as_deref(), Some("/repo/other"));
        assert_eq!(entry.project.as_deref(), Some("owner/other"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copilot_catalog_status_and_hot_admission_notice_sidecar_edits_below_event_mtime() {
        let dir = test_root("copilot-independent-artifacts");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        let session = user.join(".copilot/session-state/user-123-task-456");
        fs::create_dir_all(&session).unwrap();
        let events = session.join("events.jsonl");
        fs::write(&events, "{\"type\":\"session.start\",\"timestamp\":\"2026-06-01T12:00:00Z\",\"data\":{\"sessionId\":\"user-123-task-456\"}}\n").unwrap();
        filetime::set_file_mtime(
            &events,
            filetime::FileTime::from_unix_time(2_000_000_000, 0),
        )
        .unwrap();
        let sidecar = session.join("workspace.yaml");
        fs::write(&sidecar, "cwd: /repo/first\nname: first title\n").unwrap();
        filetime::set_file_mtime(
            &sidecar,
            filetime::FileTime::from_unix_time(1_700_000_000, 0),
        )
        .unwrap();
        rebuild(&home, &user).unwrap();
        let before = read_entries_at(&home).unwrap().pop().unwrap();
        assert!(before.source_bundle_fingerprint.is_some());
        fs::write(&sidecar, "cwd: /repo/other\nname: other title\n").unwrap();
        filetime::set_file_mtime(
            &sidecar,
            filetime::FileTime::from_unix_time(1_700_000_001, 0),
        )
        .unwrap();
        let live = live_source_bundle_fingerprint(&events).unwrap();
        assert_eq!(before.source_len, Some(live.len));
        assert_eq!(
            before.source_mtime_ns,
            Some(live.modified_unix_nanos as u64)
        );
        assert_ne!(before.source_bundle_fingerprint, live.bundle_fingerprint);
        assert_eq!(status(&home, &user).unwrap().counts.stale, 1);
        let delta = live_delta_uncached(&home, &user, 0).unwrap();
        assert_eq!(delta.changed.len(), 1);
        assert_eq!(delta.changed[0].title.as_deref(), Some("other title"));
        refresh_hot(&home, &user, 0).unwrap();
        let after = read_entries_at(&home).unwrap().pop().unwrap();
        assert_eq!(after.cwd.as_deref(), Some("/repo/other"));
        assert_eq!(after.source_bundle_fingerprint, live.bundle_fingerprint);
        assert!(
            live_delta_uncached(&home, &user, 0)
                .unwrap()
                .changed
                .is_empty()
        );
        assert_eq!(status(&home, &user).unwrap().counts.current, 1);
        // Old catalog JSON has no bundle receipt and must be admitted once.
        let mut legacy = serde_json::to_value(after).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("source_bundle_fingerprint");
        fs::write(sessions_path_for(&home), format!("{legacy}\n")).unwrap();
        assert_eq!(
            live_delta_uncached(&home, &user, 0).unwrap().changed.len(),
            1
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copilot_hot_catalog_uses_resume_metadata_beyond_identity_header() {
        let dir = test_root("copilot-resume-refresh");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        let id = "12345678-1234-1234-1234-123456789abc";
        let session = user.join(".copilot").join("session-state").join(id);
        fs::create_dir_all(&session).unwrap();
        let mut file = File::create(session.join("events.jsonl")).unwrap();
        writeln!(file, r#"{{"type":"session.start","timestamp":"2026-06-01T12:00:00Z","data":{{"sessionId":"{id}","context":{{"cwd":"/repo/old","repository":"owner/old"}}}}}}"#).unwrap();
        for _ in 0..session_catalog::MAX_HEADER_LINES {
            writeln!(file, r#"{{"type":"hook.end","data":{{}}}}"#).unwrap();
        }
        writeln!(file, r#"{{"type":"session.resume","timestamp":"2026-06-02T12:00:00Z","data":{{"context":{{"cwd":"/repo/current","repository":"owner/current"}}}}}}"#).unwrap();
        let delta = live_delta_uncached(&home, &user, 0).unwrap();
        assert_eq!(delta.changed.len(), 1);
        assert_eq!(delta.changed[0].session_id, id);
        assert_eq!(delta.changed[0].cwd.as_deref(), Some("/repo/current"));
        assert_eq!(delta.changed[0].project.as_deref(), Some("owner/current"));
        assert_eq!(delta.changed[0].date.as_deref(), Some("2026-06-02"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copilot_hot_catalog_uses_direct_context_change_beyond_identity_header() {
        let dir = test_root("copilot-context-change-refresh");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        let id = "user-123-task-456";
        let session = user.join(".copilot").join("session-state").join(id);
        fs::create_dir_all(&session).unwrap();
        let mut file = File::create(session.join("events.jsonl")).unwrap();
        writeln!(file, r#"{{"type":"session.start","timestamp":"2026-06-01T12:00:00Z","data":{{"sessionId":"{id}","context":{{"cwd":"/repo/old","repository":"owner/old"}}}}}}"#).unwrap();
        for _ in 0..session_catalog::MAX_HEADER_LINES {
            writeln!(file, r#"{{"type":"hook.end","data":{{}}}}"#).unwrap();
        }
        writeln!(file, r#"{{"type":"session.context_changed","timestamp":"2026-06-02T12:00:00Z","data":{{"cwd":"/repo/current","repository":"owner/current"}}}}"#).unwrap();
        let delta = live_delta_uncached(&home, &user, 0).unwrap();
        assert_eq!(delta.changed.len(), 1);
        assert_eq!(delta.changed[0].session_id, id);
        assert_eq!(delta.changed[0].cwd.as_deref(), Some("/repo/current"));
        assert_eq!(delta.changed[0].project.as_deref(), Some("owner/current"));
        assert_eq!(delta.changed[0].date.as_deref(), Some("2026-06-02"));
        fs::remove_dir_all(dir).unwrap();
    }

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
            source_bundle_fingerprint: None,
            title: None,
            machine: None,
            logical_session_id: None,
            session_kind: None,
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
                source_bundle_fingerprint: None,
                title: None,
                machine: None,
                logical_session_id: None,
                session_kind: None,
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
    fn cursor_catalog_requires_uuid_transcript_identity() {
        let legit = Path::new(
            "/Users/test/.cursor/projects/proj/agent-transcripts/\
             019f5407-5b0c-7363-b210-1093f26a41f7/019f5407-5b0c-7363-b210-1093f26a41f7.jsonl",
        );
        assert!(is_primary_catalog_source(AgentKind::Cursor, legit));
        // State files that happen to mirror their parent dir name must not
        // acquire session identity (Copilot review on PR #81).
        let state_file = Path::new(
            "/Users/test/.cursor/projects/proj/agent-transcripts/metadata/metadata.jsonl",
        );
        assert!(!is_primary_catalog_source(AgentKind::Cursor, state_file));
        let mismatched = Path::new(
            "/Users/test/.cursor/projects/proj/agent-transcripts/\
             019f5407-5b0c-7363-b210-1093f26a41f7/other.jsonl",
        );
        assert!(!is_primary_catalog_source(AgentKind::Cursor, mismatched));
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
            source_bundle_fingerprint: None,
            title: None,
            machine: Some("laptop".into()),
            logical_session_id: None,
            session_kind: None,
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
            source_bundle_fingerprint: None,
            title: None,
            machine: Some("laptop".into()),
            logical_session_id: None,
            session_kind: None,
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
            crate::git_env::git_command_isolated()
                .arg("-C")
                .arg(&repo)
                .args(["init", "--quiet"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            crate::git_env::git_command_isolated()
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
            source_bundle_fingerprint: None,
            title: None,
            machine: None,
            logical_session_id: None,
            session_kind: None,
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
    #[test]
    fn continuity_contract_hot_codex_keeps_recorded_header_scope() {
        let dir = test_root("continuity-hot-header");
        let user = dir.join("user");
        let home = dir.join(".aicx");
        let root = user.join(".codex/sessions/2026/10/02");
        fs::create_dir_all(&root).unwrap();
        let id = "11111111-2222-3333-4444-555555555555";
        let path = root.join(format!("rollout-2026-10-02T12-00-00-{id}.jsonl"));
        fs::write(&path, format!("{}\n", serde_json::json!({"type":"session_meta","timestamp":"2026-10-02T12:00:00Z","payload":{"id":id,"cwd":"/fixtures/vetcoders/codescribe"}}))).unwrap();
        let delta = live_delta_uncached(&home, &user, 0).unwrap();
        assert_eq!(delta.unadmitted.len(), 1);
        assert_eq!(
            delta.unadmitted[0].cwd.as_deref(),
            Some("/fixtures/vetcoders/codescribe")
        );
        assert_eq!(
            delta.unadmitted[0].project.as_deref(),
            Some("vetcoders/codescribe")
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
