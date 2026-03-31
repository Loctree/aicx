//! State management for ai-contexters.
//!
//! Tracks processing watermarks, content hashes for deduplication,
//! and run history. Persists to `~/.aicx/state.json`.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// Default maximum number of stored hashes before pruning.
const DEFAULT_MAX_HASHES: usize = 50_000;

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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateManager {
    /// Per-source watermark: only process entries newer than this timestamp.
    pub last_processed: HashMap<String, DateTime<Utc>>,
    /// Per-project content hashes of already-processed entries (dedup set).
    ///
    /// Key is project name, value is the set of hashes seen for that project.
    /// This prevents cross-project dedup pollution: entries extracted for
    /// project A won't be skipped when extracting project B.
    pub seen_hashes: HashMap<String, HashSet<u64>>,
    /// History of extraction runs.
    pub runs: Vec<RunRecord>,
}

impl StateManager {
    /// Returns the path to the state file: `~/.aicx/state.json`
    fn state_path() -> Result<PathBuf> {
        let base = crate::store::store_base_dir()?;
        Ok(base.join("state.json"))
    }

    /// Load state from disk. Creates a fresh default state if the file
    /// does not exist or cannot be parsed.
    ///
    /// **Migration:** If the file contains the old flat `HashSet<u64>` format
    /// for `seen_hashes`, deserialization will fail and we fall back to
    /// `Default` (empty state). Old hashes are discarded — they have no
    /// project context and would cause cross-project dedup pollution.
    pub fn load() -> Self {
        let path = match Self::state_path() {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };

        if !path.exists() {
            return Self::default();
        }

        let contents = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };

        // Try new format first; fall back to default (migration: old flat hashes discarded)
        serde_json::from_str(&contents).unwrap_or_default()
    }

    /// Persist current state to disk. Creates parent directories if needed.
    pub fn save(&self) -> Result<()> {
        let path = Self::state_path()?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir: {}", parent.display()))?;
        }

        let json = serde_json::to_string_pretty(self).context("Failed to serialize state")?;

        fs::write(&path, json)
            .with_context(|| format!("Failed to write state file: {}", path.display()))?;

        Ok(())
    }

    // ========================================================================
    // Dedup API
    // ========================================================================

    /// Compute a stable content hash from entry fields (exact dedup).
    ///
    /// Uses `DefaultHasher` (SipHash) for fast, collision-resistant hashing.
    /// The hash is deterministic within a single binary build (which is
    /// sufficient for dedup across runs of the same version).
    ///
    /// The (agent, timestamp, message) triple is sufficient for unique
    /// identification. Session ID is excluded because Claude Code stores
    /// the same user message in multiple session JSONL files.
    pub fn content_hash(agent: &str, timestamp: i64, message: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        agent.hash(&mut hasher);
        timestamp.hash(&mut hasher);
        message.hash(&mut hasher);
        hasher.finish()
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
    pub fn overlap_hash(timestamp: i64, message: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        let bucket = timestamp / 60; // 60-second window
        bucket.hash(&mut hasher);
        message.hash(&mut hasher);
        hasher.finish()
    }

    /// Returns `true` if this hash has NOT been seen before for the given project.
    pub fn is_new(&self, project: &str, hash: u64) -> bool {
        self.seen_hashes
            .get(project)
            .is_none_or(|set| !set.contains(&hash))
    }

    /// Mark a hash as seen for the given project.
    pub fn mark_seen(&mut self, project: &str, hash: u64) {
        self.seen_hashes
            .entry(project.to_string())
            .or_default()
            .insert(hash);
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
            if set.len() <= limit {
                continue;
            }
            let excess = set.len() - limit;
            let to_remove: Vec<u64> = set.iter().take(excess).copied().collect();
            for hash in to_remove {
                set.remove(&hash);
            }
        }
    }

    /// Reset hashes for a specific project.
    pub fn reset_project(&mut self, project: &str) {
        self.seen_hashes.remove(project);
    }

    /// Reset all dedup state.
    pub fn reset_all(&mut self) {
        self.seen_hashes.clear();
    }

    /// Total number of hashes across all projects.
    pub fn total_hashes(&self) -> usize {
        self.seen_hashes.values().map(|s| s.len()).sum()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

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
        assert!(state.is_new("projA", hash));
        assert!(state.is_new("projB", hash));

        // Mark seen only in projA
        state.mark_seen("projA", hash);
        assert!(!state.is_new("projA", hash));
        assert!(state.is_new("projB", hash)); // still new for projB

        // Mark seen in projB
        state.mark_seen("projB", hash);
        assert!(!state.is_new("projB", hash));
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
    fn test_prune_old_hashes_below_limit() {
        let mut state = StateManager::default();
        for i in 0..10u64 {
            state.mark_seen("proj", i);
        }

        state.prune_old_hashes(100);
        assert_eq!(state.seen_hashes["proj"].len(), 10);
    }

    #[test]
    fn test_prune_old_hashes_above_limit() {
        let mut state = StateManager::default();
        for i in 0..100u64 {
            state.mark_seen("proj", i);
        }

        state.prune_old_hashes(30);
        assert_eq!(state.seen_hashes["proj"].len(), 30);
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
        state.mark_seen("projA", 1);
        state.mark_seen("projA", 2);
        state.mark_seen("projB", 3);

        state.reset_project("projA");
        assert!(state.is_new("projA", 1));
        assert!(!state.is_new("projB", 3));
    }

    #[test]
    fn test_reset_all() {
        let mut state = StateManager::default();
        state.mark_seen("projA", 1);
        state.mark_seen("projB", 2);

        state.reset_all();
        assert!(state.is_new("projA", 1));
        assert!(state.is_new("projB", 2));
        assert_eq!(state.total_hashes(), 0);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut state = StateManager::default();
        let t = Utc.with_ymd_and_hms(2026, 1, 20, 15, 30, 0).unwrap();

        state.update_watermark("claude:TestProject", t);
        state.mark_seen("myproj", 123456789);
        state.mark_seen("myproj", 987654321);
        state.record_run(5, vec!["claude:TestProject".to_string()]);

        let json = serde_json::to_string_pretty(&state).unwrap();
        let restored: StateManager = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.get_watermark("claude:TestProject"), Some(t));
        assert!(!restored.is_new("myproj", 123456789));
        assert!(!restored.is_new("myproj", 987654321));
        assert!(restored.is_new("myproj", 111111111));
        assert!(restored.is_new("other", 123456789)); // different project
        assert_eq!(restored.runs.len(), 1);
        assert_eq!(restored.runs[0].entries_added, 5);
    }

    #[test]
    fn test_state_path_is_under_store() {
        if let Ok(path) = StateManager::state_path() {
            assert!(path.to_string_lossy().contains(".aicx"));
            assert!(path.to_string_lossy().ends_with("state.json"));
        }
    }
}
