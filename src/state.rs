//! State management for ai-contexters.
//!
//! Tracks processing watermarks, content hashes for deduplication,
//! and run history. Persists to `~/.aicx/state.json`.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use crate::store::atomic_write::atomic_write;

pub mod migration;

/// Default maximum number of stored hashes before pruning.
const DEFAULT_MAX_HASHES: usize = 50_000;

/// Per-project dedup hashes with insertion/LRU order preserved.
#[derive(Debug, Clone, Default)]
pub struct SeenHashSet {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl SeenHashSet {
    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub fn contains(&self, hash: &str) -> bool {
        self.set.contains(hash)
    }

    pub fn insert(&mut self, hash: String) {
        if self.set.remove(&hash) {
            self.order.retain(|existing| *existing != hash);
        }
        self.set.insert(hash.clone());
        self.order.push_back(hash);
    }

    pub(crate) fn extend_from(&mut self, other: SeenHashSet) {
        for hash in other.order {
            self.insert(hash);
        }
    }

    pub fn prune_oldest(&mut self, limit: usize) {
        while self.set.len() > limit {
            let Some(hash) = self.order.pop_front() else {
                break;
            };
            self.set.remove(&hash);
        }
    }
}

impl Serialize for SeenHashSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.order.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SeenHashSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hashes = Vec::<String>::deserialize(deserializer)?;
        let mut out = SeenHashSet::default();
        for hash in hashes {
            out.insert(hash);
        }
        Ok(out)
    }
}

/// Record of a single extraction run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    /// When this run was executed.
    pub timestamp: DateTime<Utc>,
    /// Number of new entries added during this run.
    pub entries_added: usize,
    /// Sources processed (e.g., "claude:CodeScribe", "codex:global").
    pub sources: Vec<String>,
}

/// Persistent state for incremental processing and deduplication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateManager {
    /// Per-source watermark: only process entries newer than this timestamp.
    pub last_processed: HashMap<String, DateTime<Utc>>,
    /// Per-project content hashes of already-processed entries (dedup set).
    ///
    /// Key is project name, value is the set of hashes seen for that project.
    /// This prevents cross-project dedup pollution: entries extracted for
    /// project A won't be skipped when extracting project B.
    pub seen_hashes: HashMap<String, SeenHashSet>,
    /// History of extraction runs.
    pub runs: Vec<RunRecord>,
    /// Stable algorithm used for persistent `seen_hashes` values.
    ///
    /// Missing or different values mean the old `u64` hashes cannot be trusted
    /// across toolchain changes and must be rebuilt by the next extraction run.
    #[serde(default)]
    pub hash_algorithm: String,
}

#[derive(Debug, Deserialize)]
struct LegacySiphashStateManager {
    #[serde(default)]
    last_processed: HashMap<String, DateTime<Utc>>,
    #[serde(default)]
    seen_hashes: HashMap<String, Vec<u64>>,
    #[serde(default)]
    runs: Vec<RunRecord>,
    hash_algorithm: String,
}

impl LegacySiphashStateManager {
    fn into_current_state(self) -> StateManager {
        let seen_hashes = self
            .seen_hashes
            .into_iter()
            .map(|(project, hashes)| {
                let mut set = SeenHashSet::default();
                for hash in hashes {
                    set.insert(hash.to_string());
                }
                (project, set)
            })
            .collect();

        StateManager {
            last_processed: self.last_processed,
            seen_hashes,
            runs: self.runs,
            hash_algorithm: self.hash_algorithm,
        }
    }
}

impl Default for StateManager {
    fn default() -> Self {
        Self {
            last_processed: HashMap::new(),
            seen_hashes: HashMap::new(),
            runs: Vec::new(),
            hash_algorithm: migration::BLAKE3_128_ALGORITHM.to_string(),
        }
    }
}

impl StateManager {
    /// Returns the path to the state file: `~/.aicx/state.json`
    fn state_path() -> Result<PathBuf> {
        let base = crate::store::store_base_dir()?;
        Ok(base.join("state.json"))
    }

    /// Load state from disk. Creates a fresh default state only when the file
    /// does not exist.
    ///
    /// Malformed JSON is never silently reset to default. We recover from a
    /// valid rolling backup when available; otherwise the caller gets an error.
    pub fn load() -> Result<Self> {
        Self::load_from_path(&Self::state_path()?)
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        Self::load_from_path_with_legacy_warning(path, |message| eprintln!("{message}"))
    }

    fn load_from_path_with_legacy_warning<W>(path: &Path, mut warn_legacy: W) -> Result<Self>
    where
        W: FnMut(&str),
    {
        if !path.exists() {
            return Ok(Self::default());
        }

        let backup_path = Self::backup_path(path);
        let contents = fs::read_to_string(path) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
            .with_context(|| format!("Failed to read state file: {}", path.display()))?;

        let state: Self =
            match Self::deserialize_and_migrate_contents(path, &contents, &mut warn_legacy) {
                Ok(state) => state,
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "state.json parse failed"
                    );
                    if backup_path.exists() {
                        let backup = fs::read_to_string(&backup_path) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
                            .with_context(|| {
                                format!("Failed to read state backup: {}", backup_path.display())
                            })?;
                        let recovered = Self::deserialize_and_migrate_contents(
                            &backup_path,
                            &backup,
                            &mut warn_legacy,
                        )
                        .map_err(|backup_err| {
                            anyhow!(
                                "state.json malformed AND backup unreadable: {err} / {backup_err}"
                            )
                        })?;
                        recovered.save_recovered_backup_to_primary(path)?;
                        recovered
                    } else {
                        return Err(anyhow!(
                            "state.json corrupted, no backup; manual recovery needed: {}",
                            path.display()
                        ));
                    }
                }
            };
        Ok(state)
    }

    fn save_recovered_backup_to_primary(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir: {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("Failed to serialize state")?;
        atomic_write(path, json.as_bytes()).with_context(|| {
            format!(
                "Failed to self-heal state file from backup: {}",
                path.display()
            )
        })?;
        Ok(())
    }

    fn deserialize_and_migrate_contents<W>(
        path: &Path,
        contents: &str,
        warn_legacy: &mut W,
    ) -> std::result::Result<Self, serde_json::Error>
    where
        W: FnMut(&str),
    {
        let (mut state, loaded_legacy_u64_shape) =
            Self::deserialize_current_or_legacy_siphash(contents)?;
        let previous_hash_algorithm = state.hash_algorithm.clone();
        let report = state.apply_load_migrations();
        Self::emit_load_migration_warning(
            path,
            &previous_hash_algorithm,
            loaded_legacy_u64_shape,
            &report,
            warn_legacy,
        );
        Ok(state)
    }

    fn deserialize_current_or_legacy_siphash(
        contents: &str,
    ) -> std::result::Result<(Self, bool), serde_json::Error> {
        let value: serde_json::Value = serde_json::from_str(contents)?;
        match serde_json::from_value::<Self>(value.clone()) {
            Ok(state) => Ok((state, false)),
            Err(strict_err) => {
                if !Self::is_legacy_siphash_state_value(&value) {
                    return Err(strict_err);
                }

                serde_json::from_value::<LegacySiphashStateManager>(value)
                    .map(|legacy| (legacy.into_current_state(), true))
                    .map_err(|_| strict_err)
            }
        }
    }

    fn is_legacy_siphash_state_value(value: &serde_json::Value) -> bool {
        value
            .get("hash_algorithm")
            .and_then(|algorithm| algorithm.as_str())
            .is_some_and(migration::is_legacy_siphash13_algorithm)
    }

    fn emit_load_migration_warning<W>(
        path: &Path,
        previous_hash_algorithm: &str,
        loaded_legacy_u64_shape: bool,
        report: &migration::StateMigrationReport,
        warn_legacy: &mut W,
    ) where
        W: FnMut(&str),
    {
        if !report.hash_algorithm_changed {
            return;
        }

        tracing::warn!(
            path = %path.display(),
            previous_hash_algorithm = %previous_hash_algorithm,
            current_hash_algorithm = %migration::BLAKE3_128_ALGORITHM,
            cleared_seen_hashes = report.cleared_seen_hashes,
            legacy_u64_shape = loaded_legacy_u64_shape,
            "state.json migrated from legacy hash algorithm"
        );

        let previous = if previous_hash_algorithm.trim().is_empty() {
            "missing"
        } else {
            previous_hash_algorithm.trim()
        };
        warn_legacy(&format!(
            "Warning: state.json migrated from legacy hash algorithm {previous} to {}; cleared {} legacy seen_hashes",
            migration::BLAKE3_128_ALGORITHM,
            report.cleared_seen_hashes
        ));
    }

    /// Persist current state to disk. Creates parent directories if needed.
    pub fn save(&self) -> Result<()> {
        let path = Self::state_path()?;
        self.save_to_path(&path)
    }

    fn save_to_path(&self, path: &Path) -> Result<()> {
        self.save_to_path_with_writer(path, atomic_write)
    }

    fn save_to_path_with_writer<W>(&self, path: &Path, write_atomic: W) -> Result<()>
    where
        W: Fn(&Path, &[u8]) -> std::io::Result<()>,
    {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir: {}", parent.display()))?;
        }

        // Note: caller is responsible for holding `state_lock_path()` exclusive
        // around the full read-modify-write cycle (see run_store/run_state/etc.
        // in main.rs). Re-acquiring here would deadlock.

        let json = serde_json::to_string_pretty(self).context("Failed to serialize state")?;

        if path.exists() {
            let previous = fs::read(path) // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
                .with_context(|| format!("Failed to read state file: {}", path.display()))?;
            let backup_path = Self::backup_path(path);
            write_atomic(&backup_path, &previous).with_context(|| {
                format!("Failed to write state backup: {}", backup_path.display())
            })?;
        }

        write_atomic(path, json.as_bytes())
            .with_context(|| format!("Failed to write state file: {}", path.display()))?;
        Ok(())
    }

    fn backup_path(path: &Path) -> PathBuf {
        path.with_file_name("state.json.bak")
    }

    // ========================================================================
    // Dedup API
    // ========================================================================

    /// Compute a stable content hash from entry fields (exact dedup).
    ///
    /// Uses explicitly pinned BLAKE3-128 for fast, stable hashing.
    ///
    /// The (agent, timestamp, message) triple is sufficient for unique
    /// identification. Session ID is excluded because Claude Code stores
    /// the same user message in multiple session JSONL files.
    pub fn content_hash(agent: &str, timestamp: i64, message: &str) -> String {
        let mut data = Vec::new();
        data.extend_from_slice(agent.as_bytes());
        data.extend_from_slice(&timestamp.to_le_bytes());
        data.extend_from_slice(message.as_bytes());
        migration::stable_blake3_128(&data)
    }

    /// Compute an overlap hash for cross-agent dedup.
    ///
    /// When the same prompt is broadcast to multiple agents simultaneously
    /// (e.g., 8 parallel Claude sessions), each gets an identical message
    /// within a narrow time window. The exact hash treats these as distinct
    /// because `agent` differs.
    ///
    /// The overlap hash ignores `agent` and buckets timestamps into 60-second
    /// windows, so identical messages arriving within the same minute from
    /// different agents collapse into one.
    pub fn overlap_hash(timestamp: i64, message: &str) -> String {
        let bucket = timestamp / 60; // 60-second window
        let mut data = Vec::new();
        data.extend_from_slice(&bucket.to_le_bytes());
        data.extend_from_slice(message.as_bytes());
        migration::stable_blake3_128(&data)
    }

    /// Returns `true` if this hash has NOT been seen before for the given project.
    pub fn is_new(&self, project: &str, hash: &str) -> bool {
        let project = migration::canonical_state_bucket(project);
        self.seen_hashes
            .get(&project)
            .is_none_or(|set| !set.contains(hash))
    }

    /// Mark a hash as seen for the given project.
    pub fn mark_seen(&mut self, project: &str, hash: String) {
        let project = migration::canonical_state_bucket(project);
        self.seen_hashes.entry(project).or_default().insert(hash);
    }

    // ========================================================================
    // Watermark API
    // ========================================================================

    /// Get the watermark timestamp for a given source.
    ///
    /// Returns `None` if this source has never been processed.
    pub fn get_watermark(&self, source: &str) -> Option<DateTime<Utc>> {
        self.last_processed.get(source).copied()
    }

    /// Carry a watermark forward from legacy source-key generations into the
    /// canonical key so adding/removing extractor agents does not reset ingest.
    pub fn migrate_watermark_aliases(&mut self, canonical: &str, aliases: &[String]) -> bool {
        if self.last_processed.contains_key(canonical) {
            return false;
        }

        let migrated = aliases
            .iter()
            .filter_map(|alias| self.last_processed.get(alias).copied())
            .max();

        if let Some(ts) = migrated {
            self.last_processed.insert(canonical.to_string(), ts);
            true
        } else {
            false
        }
    }

    /// Update the watermark for a source, but only if the new timestamp
    /// is strictly newer than the existing one.
    pub fn update_watermark(&mut self, source: &str, ts: DateTime<Utc>) {
        let entry = self.last_processed.entry(source.to_string()).or_insert(ts);
        if ts > *entry {
            *entry = ts;
        }
    }

    // ========================================================================
    // Run tracking
    // ========================================================================

    /// Record a completed extraction run.
    pub fn record_run(&mut self, entries: usize, sources: Vec<String>) {
        self.runs.push(RunRecord {
            timestamp: Utc::now(),
            entries_added: entries,
            sources,
        });
    }

    // ========================================================================
    // Cleanup API
    // ========================================================================

    /// Prune hash sets per-project to prevent unbounded growth.
    ///
    /// Each project's set is capped at `max_per_project` entries.
    /// Pass `0` to use the default maximum (`50_000`).
    pub fn prune_old_hashes(&mut self, max_per_project: usize) {
        let limit = if max_per_project == 0 {
            DEFAULT_MAX_HASHES
        } else {
            max_per_project
        };

        for set in self.seen_hashes.values_mut() {
            set.prune_oldest(limit);
        }
    }

    /// Reset hashes for a specific project.
    pub fn reset_project(&mut self, project: &str) {
        self.seen_hashes
            .remove(&migration::canonical_state_bucket(project));
    }

    /// Reset all dedup state.
    pub fn reset_all(&mut self) {
        self.seen_hashes.clear();
    }

    /// Total number of hashes across all projects.
    pub fn total_hashes(&self) -> usize {
        self.seen_hashes.values().map(|s| s.len()).sum()
    }

    fn apply_load_migrations(&mut self) -> migration::StateMigrationReport {
        migration::migrate_loaded_state(self)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_state_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("aicx-state-{label}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join("state.json")
    }

    fn cleanup_state_path(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    fn ts(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).single().unwrap()
    }

    fn state_with_marker(source: &str, watermark_seconds: i64, project_hash: u64) -> StateManager {
        let mut state = StateManager::default();
        state.update_watermark(source, ts(watermark_seconds));
        state.mark_seen("project", project_hash.to_string());
        state
    }

    #[test]
    fn test_default_state_is_empty() {
        let state = StateManager::default();
        assert!(state.last_processed.is_empty());
        assert!(state.seen_hashes.is_empty());
        assert!(state.runs.is_empty());
    }

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = StateManager::content_hash("claude", 1700000000, "hello world");
        let h2 = StateManager::content_hash("claude", 1700000000, "hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_varies_with_input() {
        let h1 = StateManager::content_hash("claude", 1700000000, "hello");
        let h2 = StateManager::content_hash("claude", 1700000000, "world");
        assert_ne!(h1, h2, "different message → different hash");

        let h3 = StateManager::content_hash("codex", 1700000000, "hello");
        assert_ne!(h1, h3, "different agent → different hash");

        // session_id is intentionally excluded: same message from different
        // sessions within the same project is a semantic duplicate
        let h5 = StateManager::content_hash("claude", 1700000001, "hello");
        assert_ne!(h1, h5, "different timestamp → different hash");
    }

    #[test]
    fn test_overlap_hash_ignores_agent() {
        let prompt = "Deploy the new auth module to staging and run integration tests";
        let ts = 1700000000i64;

        let h_claude = StateManager::overlap_hash(ts, prompt);
        let h_codex = StateManager::overlap_hash(ts, prompt);
        assert_eq!(
            h_claude, h_codex,
            "same message + same bucket → SAME overlap hash"
        );
    }

    #[test]
    fn test_overlap_hash_buckets_60s() {
        let prompt = "Identical broadcast prompt";

        // Pick a timestamp that's cleanly at a bucket boundary
        let base = 1700000040i64; // bucket = 28333334
        let same_bucket = base + 19; // 1700000059 → bucket 28333334 (still same)

        let h1 = StateManager::overlap_hash(base, prompt);
        let h2 = StateManager::overlap_hash(same_bucket, prompt);
        assert_eq!(h1, h2, "within same 60s bucket → SAME hash");

        // Next bucket starts at base rounded up to next 60
        let next_bucket = base - (base % 60) + 60; // 1700000040 - 40 + 60 = 1700000060
        let h3 = StateManager::overlap_hash(next_bucket, prompt);
        assert_ne!(h1, h3, "different 60s bucket → different hash");
    }

    #[test]
    fn test_overlap_hash_different_message() {
        let ts = 1700000000i64;
        let h1 = StateManager::overlap_hash(ts, "prompt A");
        let h2 = StateManager::overlap_hash(ts, "prompt B");
        assert_ne!(h1, h2, "different message → different overlap hash");
    }

    #[test]
    fn test_is_new_and_mark_seen_per_project() {
        let mut state = StateManager::default();
        let hash = StateManager::content_hash("claude", 100, "msg");

        // New for both projects
        assert!(state.is_new("projA", &hash));
        assert!(state.is_new("projB", &hash));

        // Mark seen only in projA
        state.mark_seen("projA", hash.clone());
        assert!(!state.is_new("projA", &hash));
        assert!(!state.is_new("proja", &hash));
        assert!(state.is_new("projB", &hash)); // still new for projB

        // Mark seen in projB
        state.mark_seen("projB", hash.clone());
        assert!(!state.is_new("projB", &hash));
    }

    #[test]
    fn test_watermark_none_for_unknown_source() {
        let state = StateManager::default();
        assert_eq!(state.get_watermark("nonexistent"), None);
    }

    #[test]
    fn test_watermark_update_only_if_newer() {
        let mut state = StateManager::default();

        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap();

        state.update_watermark("claude:CodeScribe", t1);
        assert_eq!(state.get_watermark("claude:CodeScribe"), Some(t1));

        // Newer timestamp updates
        state.update_watermark("claude:CodeScribe", t2);
        assert_eq!(state.get_watermark("claude:CodeScribe"), Some(t2));

        // Older timestamp does NOT update
        state.update_watermark("claude:CodeScribe", t0);
        assert_eq!(state.get_watermark("claude:CodeScribe"), Some(t2));
    }

    #[test]
    fn test_record_run() {
        let mut state = StateManager::default();
        assert!(state.runs.is_empty());

        state.record_run(
            42,
            vec!["claude:Proj".to_string(), "codex:global".to_string()],
        );

        assert_eq!(state.runs.len(), 1);
        assert_eq!(state.runs[0].entries_added, 42);
        assert_eq!(state.runs[0].sources, vec!["claude:Proj", "codex:global"]);
    }

    #[test]
    fn test_save_uses_atomic_write() {
        let path = unique_state_path("atomic-write");
        let old = state_with_marker("claude:test", 10, 101);
        old.save_to_path(&path).unwrap();
        let old_contents = fs::read(&path).unwrap();

        let new = state_with_marker("claude:test", 20, 202);
        let target_path = path.clone();
        let err = new
            .save_to_path_with_writer(&path, move |target, content| {
                if target == target_path.as_path() {
                    return Err(std::io::Error::other("mock atomic_write failure"));
                }
                crate::store::atomic_write::atomic_write(target, content)
            })
            .expect_err("mocked final atomic write should fail");

        assert!(err.to_string().contains("Failed to write state file"));
        assert_eq!(fs::read(&path).unwrap(), old_contents);
        let loaded = StateManager::load_from_path(&path).unwrap();
        assert_eq!(loaded.get_watermark("claude:test"), Some(ts(10)));
        assert!(!loaded.is_new("project", "101"));

        cleanup_state_path(&path);
    }

    #[test]
    fn test_load_malformed_returns_error_not_default() {
        let path = unique_state_path("malformed");
        fs::write(&path, b"{ this is not json").unwrap();

        let err = StateManager::load_from_path(&path)
            .expect_err("malformed state without backup must not default");

        assert!(err.to_string().contains("state.json corrupted"));
        cleanup_state_path(&path);
    }

    #[test]
    fn test_load_recovers_from_backup_when_main_corrupt() {
        let path = unique_state_path("backup-recovery");
        let backup_path = StateManager::backup_path(&path);
        let backup_state = state_with_marker("claude:test", 20, 202);
        fs::write(&path, b"{ this is not json").unwrap();
        fs::write(
            &backup_path,
            serde_json::to_string_pretty(&backup_state).unwrap(),
        )
        .unwrap();

        let loaded = StateManager::load_from_path(&path).unwrap();

        assert_eq!(loaded.get_watermark("claude:test"), Some(ts(20)));
        assert!(!loaded.is_new("project", "202"));
        let repaired_primary: StateManager =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(repaired_primary.get_watermark("claude:test"), Some(ts(20)));
        assert!(!repaired_primary.is_new("project", "202"));
        let repaired = StateManager::load_from_path(&path).unwrap();
        assert_eq!(repaired.get_watermark("claude:test"), Some(ts(20)));
        assert!(!repaired.is_new("project", "202"));
        cleanup_state_path(&path);
    }

    #[test]
    fn test_load_migrates_legacy_siphash_u64_state() {
        let path = unique_state_path("legacy-siphash-u64");
        let legacy_state = serde_json::json!({
            "last_processed": {
                "claude:test": "2026-05-20T12:00:00Z"
            },
            "seen_hashes": {
                "Vista": [101_u64, 202_u64]
            },
            "runs": [
                {
                    "timestamp": "2026-05-20T12:30:00Z",
                    "entries_added": 2,
                    "sources": ["claude:test"]
                }
            ],
            "hash_algorithm": migration::SIPHASH13_ALGORITHM
        });
        fs::write(&path, serde_json::to_vec_pretty(&legacy_state).unwrap()).unwrap();

        let mut warnings = Vec::new();
        let loaded = StateManager::load_from_path_with_legacy_warning(&path, |message| {
            warnings.push(message.to_string());
        })
        .unwrap();

        assert_eq!(loaded.hash_algorithm, migration::BLAKE3_128_ALGORITHM);
        assert!(loaded.seen_hashes.is_empty());
        assert_eq!(loaded.total_hashes(), 0);
        assert_eq!(loaded.get_watermark("claude:test"), Some(ts(1779278400)));
        assert_eq!(loaded.runs.len(), 1);
        assert_eq!(loaded.runs[0].entries_added, 2);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("migrated from legacy hash algorithm"));
        assert!(warnings[0].contains(migration::SIPHASH13_ALGORITHM));
        assert!(warnings[0].contains(migration::BLAKE3_128_ALGORITHM));
        assert!(warnings[0].contains("cleared 2 legacy seen_hashes"));

        loaded.save_to_path(&path).unwrap();
        let persisted: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            persisted["hash_algorithm"],
            serde_json::Value::String(migration::BLAKE3_128_ALGORITHM.to_string())
        );
        assert_eq!(persisted["seen_hashes"].as_object().unwrap().len(), 0);

        cleanup_state_path(&path);
    }

    #[test]
    fn test_load_rejects_non_legacy_schema_mismatch() {
        let path = unique_state_path("non-legacy-schema-mismatch");
        let invalid_current_state = serde_json::json!({
            "last_processed": {},
            "seen_hashes": {
                "Vista": [101_u64]
            },
            "runs": [],
            "hash_algorithm": migration::BLAKE3_128_ALGORITHM
        });
        fs::write(
            &path,
            serde_json::to_vec_pretty(&invalid_current_state).unwrap(),
        )
        .unwrap();

        let mut warnings = Vec::new();
        let err = StateManager::load_from_path_with_legacy_warning(&path, |message| {
            warnings.push(message.to_string());
        })
        .expect_err("non-legacy u64 hashes must still be rejected");

        assert!(err.to_string().contains("state.json corrupted"));
        assert!(warnings.is_empty());
        cleanup_state_path(&path);
    }

    #[test]
    fn test_save_creates_backup_before_overwrite() {
        let path = unique_state_path("backup-overwrite");
        let backup_path = StateManager::backup_path(&path);
        let old = state_with_marker("claude:test", 10, 101);
        old.save_to_path(&path).unwrap();
        let old_contents = fs::read_to_string(&path).unwrap();

        let new = state_with_marker("claude:test", 20, 202);
        new.save_to_path(&path).unwrap();

        assert_eq!(fs::read_to_string(&backup_path).unwrap(), old_contents);
        let loaded = StateManager::load_from_path(&path).unwrap();
        assert_eq!(loaded.get_watermark("claude:test"), Some(ts(20)));
        assert!(!loaded.is_new("project", "202"));
        cleanup_state_path(&path);
    }

    #[test]
    fn test_prune_old_hashes_below_limit() {
        let mut state = StateManager::default();
        for i in 0..10u64 {
            state.mark_seen("proj", i.to_string());
        }

        state.prune_old_hashes(100);
        assert_eq!(state.seen_hashes["proj"].len(), 10);
    }

    #[test]
    fn test_prune_old_hashes_above_limit() {
        let mut state = StateManager::default();
        for i in 0..100u64 {
            state.mark_seen("proj", i.to_string());
        }

        state.prune_old_hashes(30);
        assert_eq!(state.seen_hashes["proj"].len(), 30);
    }

    #[test]
    fn lru_evicts_oldest_first() {
        let mut state = StateManager::default();
        for i in 0..10u64 {
            state.mark_seen("proj", i.to_string());
        }

        state.prune_old_hashes(5);

        for old in 0..5u64 {
            assert!(
                state.is_new("proj", &old.to_string()),
                "old hash {old} should be evicted"
            );
        }
        for fresh in 5..10u64 {
            assert!(
                !state.is_new("proj", &fresh.to_string()),
                "fresh hash {fresh} should remain"
            );
        }
    }

    #[test]
    fn watermark_migration_carries_timestamp_forward() {
        let mut state = StateManager::default();
        let ts = Utc.with_ymd_and_hms(2026, 5, 6, 11, 0, 0).unwrap();
        state.update_watermark("claude+codex+gemini:all", ts);

        let migrated = state.migrate_watermark_aliases(
            "claude+codex+gemini+junie:all",
            &["claude+codex+gemini:all".to_string()],
        );

        assert!(migrated);
        assert_eq!(
            state.get_watermark("claude+codex+gemini+junie:all"),
            Some(ts)
        );
    }

    #[test]
    fn test_prune_old_hashes_default_limit() {
        let mut state = StateManager::default();
        state.prune_old_hashes(0);
        assert_eq!(state.total_hashes(), 0);
    }

    #[test]
    fn test_reset_project() {
        let mut state = StateManager::default();
        state.mark_seen("projA", "1".to_string());
        state.mark_seen("projA", "2".to_string());
        state.mark_seen("projB", "3".to_string());

        state.reset_project("projA");
        assert!(state.is_new("projA", "1"));
        assert!(!state.is_new("projB", "3"));
    }

    #[test]
    fn test_reset_all() {
        let mut state = StateManager::default();
        state.mark_seen("projA", "1".to_string());
        state.mark_seen("projB", "2".to_string());

        state.reset_all();
        assert!(state.is_new("projA", "1"));
        assert!(state.is_new("projB", "2"));
        assert_eq!(state.total_hashes(), 0);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut state = StateManager::default();
        let t = Utc.with_ymd_and_hms(2026, 1, 20, 15, 30, 0).unwrap();

        state.update_watermark("claude:TestProject", t);
        state.mark_seen("myproj", "123456789".to_string());
        state.mark_seen("myproj", "987654321".to_string());
        state.record_run(5, vec!["claude:TestProject".to_string()]);

        let json = serde_json::to_string_pretty(&state).unwrap();
        let restored: StateManager = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.get_watermark("claude:TestProject"), Some(t));
        assert!(!restored.is_new("myproj", "123456789"));
        assert!(!restored.is_new("myproj", "987654321"));
        assert!(restored.is_new("myproj", "111111111"));
        assert!(restored.is_new("other", "123456789")); // different project
        assert_eq!(restored.runs.len(), 1);
        assert_eq!(restored.runs[0].entries_added, 5);
    }

    #[test]
    fn pre_siphash_state_clears_seen_hashes_once() {
        let mut state = StateManager::default();
        state.hash_algorithm.clear();
        state
            .seen_hashes
            .entry("Vista".to_string())
            .or_default()
            .insert("42".to_string());

        let report = migration::migrate_loaded_state(&mut state);

        assert!(report.hash_algorithm_changed);
        assert_eq!(report.cleared_seen_hashes, 1);
        assert_eq!(state.hash_algorithm, migration::BLAKE3_128_ALGORITHM);
        assert_eq!(state.total_hashes(), 0);
    }

    #[test]
    fn current_state_lowercases_and_merges_seen_hash_buckets() {
        let mut state = StateManager::default();
        state
            .seen_hashes
            .entry("Vista".to_string())
            .or_default()
            .insert("1".to_string());
        state
            .seen_hashes
            .entry("vista".to_string())
            .or_default()
            .insert("2".to_string());

        let report = migration::migrate_loaded_state(&mut state);

        assert!(!report.hash_algorithm_changed);
        assert_eq!(report.lowercased_seen_hash_buckets, 1);
        assert_eq!(report.merged_seen_hash_buckets, 1);
        assert_eq!(state.seen_hashes.len(), 1);
        assert!(!state.is_new("vista", "1"));
        assert!(!state.is_new("Vista", "2"));
    }

    #[test]
    fn case_bucket_merge_script_is_reviewable_and_lowercase_targeted() {
        let root = std::env::temp_dir().join(format!(
            "aicx-case-bucket-script-{}",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("VetCoders").join("Vista")).unwrap();
        std::fs::create_dir_all(root.join("vetcoders").join("vista")).unwrap();

        let plan = migration::generate_case_bucket_merge_script(&root).unwrap();

        assert_eq!(plan.merges.len(), 2);
        assert!(plan.script.contains("Review before running"));
        assert!(plan.script.contains("vetcoders"));
        assert!(plan.script.contains("VetCoders"));
        assert!(root.join("VetCoders").join("Vista").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_state_path_is_under_store() {
        if let Ok(path) = StateManager::state_path() {
            assert!(path.to_string_lossy().contains(".aicx"));
            assert!(path.to_string_lossy().ends_with("state.json"));
        }
    }
}
