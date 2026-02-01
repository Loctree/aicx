//! Integration with rmcp-memex for vector memory indexing.
//!
//! Shells out to the `rmcp-memex` CLI binary for:
//! - Batch indexing of chunk files (`rmcp-memex index`)
//! - Single chunk upsert (`rmcp-memex upsert`)
//! - Semantic search (`rmcp-memex search`)
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sanitize;

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for memex integration.
#[derive(Debug, Clone)]
pub struct MemexConfig {
    /// Namespace in vector store (default: "ai-contexts")
    pub namespace: String,
    /// Override LanceDB path if needed
    pub db_path: Option<PathBuf>,
    /// Use batch `index` command (true) or per-chunk `upsert` (false)
    pub batch_mode: bool,
}

impl Default for MemexConfig {
    fn default() -> Self {
        Self {
            namespace: "ai-contexts".to_string(),
            db_path: None,
            batch_mode: true,
        }
    }
}

// ============================================================================
// Sync state
// ============================================================================

/// Persistent state tracking what's been synced to memex.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemexSyncState {
    /// Last time a sync was performed.
    pub last_synced: Option<DateTime<Utc>>,
    /// Set of chunk IDs already pushed to memex.
    pub synced_chunks: HashSet<String>,
    /// Total number of pushes across all syncs.
    pub total_pushes: usize,
}

/// Result of a sync operation.
#[derive(Debug, Default)]
pub struct SyncResult {
    /// Number of chunks successfully pushed.
    pub chunks_pushed: usize,
    /// Number of chunks skipped (already synced or dedup).
    pub chunks_skipped: usize,
    /// Errors encountered during sync.
    pub errors: Vec<String>,
}

// ============================================================================
// Sync state persistence
// ============================================================================

/// Path to sync state file: `~/.ai-contexters/memex/sync_state.json`
fn sync_state_path() -> Result<PathBuf> {
    let dir = crate::store::store_base_dir()?.join("memex");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("sync_state.json"))
}

/// Load sync state from disk. Returns default if missing or unparseable.
pub fn load_sync_state() -> MemexSyncState {
    let path = match sync_state_path() {
        Ok(p) => p,
        Err(_) => return MemexSyncState::default(),
    };

    if !path.exists() {
        return MemexSyncState::default();
    }

    let contents = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return MemexSyncState::default(),
    };

    serde_json::from_str(&contents).unwrap_or_default()
}

/// Persist sync state to disk.
pub fn save_sync_state(state: &MemexSyncState) -> Result<()> {
    let path = sync_state_path()?;
    let json = serde_json::to_string_pretty(state).context("Failed to serialize sync state")?;
    fs::write(&path, json)?;
    Ok(())
}

// ============================================================================
// Availability check
// ============================================================================

/// Check if the `rmcp-memex` binary is available in PATH.
pub fn check_memex_available() -> bool {
    Command::new("rmcp-memex")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

// ============================================================================
// Batch sync (primary method)
// ============================================================================

/// Sync all chunk files in a directory to memex using batch `index` command.
///
/// Runs: `rmcp-memex index <chunks_dir> -n <namespace> -s flat --dedup true`
///
/// This is the fastest method — rmcp-memex handles deduplication internally
/// via content hashing.
pub fn sync_chunks_batch(chunks_dir: &Path, config: &MemexConfig) -> Result<SyncResult> {
    if !check_memex_available() {
        bail!("rmcp-memex not found in PATH. Install with: cargo install rmcp-memex");
    }

    if !chunks_dir.exists() || !chunks_dir.is_dir() {
        return Ok(SyncResult::default());
    }

    let validated_dir = sanitize::validate_dir_path(chunks_dir)?;

    // SECURITY: dir sanitized via validate_dir_path (traversal + canonicalize + allowlist)
    let file_count = fs::read_dir(&validated_dir)? // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "txt"))
        .count();

    if file_count == 0 {
        return Ok(SyncResult::default());
    }

    let mut cmd = Command::new("rmcp-memex");
    cmd.arg("index")
        .arg(chunks_dir)
        .arg("-n")
        .arg(&config.namespace)
        .arg("-s")
        .arg("flat")
        .arg("--dedup")
        .arg("true");

    if let Some(ref db_path) = config.db_path {
        cmd.arg("--db-path").arg(db_path);
    }

    let output = cmd.output().context("Failed to run rmcp-memex index")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("rmcp-memex index failed: {}", stderr.trim());
    }

    // Parse output for stats
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}\n{}", stdout, stderr_str);

    let chunks_pushed = parse_indexed_count(&combined).unwrap_or(file_count);

    Ok(SyncResult {
        chunks_pushed,
        chunks_skipped: file_count.saturating_sub(chunks_pushed),
        errors: vec![],
    })
}

/// Try to parse the number of indexed documents from rmcp-memex output.
fn parse_indexed_count(output: &str) -> Option<usize> {
    // Look for patterns like "Indexed 42 documents" or "42 chunks indexed"
    for line in output.lines() {
        let lower = line.to_lowercase();
        if lower.contains("index") || lower.contains("chunk") || lower.contains("document") {
            // Try to find a number in this line
            for word in line.split_whitespace() {
                if let Ok(n) = word.parse::<usize>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

// ============================================================================
// Single chunk sync
// ============================================================================

/// Push a single chunk to memex using the `upsert` command.
///
/// Runs: `rmcp-memex upsert -n <ns> -i <id> -t <text> -m <metadata>`
pub fn sync_chunk_single(
    chunk_id: &str,
    text: &str,
    metadata: &serde_json::Value,
    config: &MemexConfig,
) -> Result<()> {
    if !check_memex_available() {
        bail!("rmcp-memex not found in PATH");
    }

    let meta_str = serde_json::to_string(metadata)?;

    let mut cmd = Command::new("rmcp-memex");
    cmd.arg("upsert")
        .arg("-n")
        .arg(&config.namespace)
        .arg("-i")
        .arg(chunk_id)
        .arg("-t")
        .arg(text)
        .arg("-m")
        .arg(&meta_str);

    if let Some(ref db_path) = config.db_path {
        cmd.arg("--db-path").arg(db_path);
    }

    let output = cmd
        .output()
        .with_context(|| format!("Failed to upsert chunk: {}", chunk_id))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "rmcp-memex upsert failed for {}: {}",
            chunk_id,
            stderr.trim()
        );
    }

    Ok(())
}

// ============================================================================
// High-level sync
// ============================================================================

/// Sync only new chunks (not previously synced) to memex.
///
/// Loads sync state, determines which chunk files are new,
/// syncs them via batch mode, and updates state.
pub fn sync_new_chunks(chunks_dir: &Path, config: &MemexConfig) -> Result<SyncResult> {
    let mut state = load_sync_state();

    if !chunks_dir.exists() {
        return Ok(SyncResult::default());
    }

    let validated_dir = sanitize::validate_dir_path(chunks_dir)?;

    // SECURITY: dir sanitized via validate_dir_path (traversal + canonicalize + allowlist)
    let all_files: Vec<PathBuf> = fs::read_dir(&validated_dir)? // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "txt"))
        .collect();

    let new_files: Vec<&PathBuf> = all_files
        .iter()
        .filter(|p| {
            let id = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            !state.synced_chunks.contains(&id)
        })
        .collect();

    if new_files.is_empty() {
        return Ok(SyncResult {
            chunks_pushed: 0,
            chunks_skipped: all_files.len(),
            errors: vec![],
        });
    }

    // For batch mode: sync entire directory (rmcp-memex dedup handles already-indexed)
    let result = if config.batch_mode {
        sync_chunks_batch(chunks_dir, config)?
    } else {
        // Per-file upsert mode
        let mut result = SyncResult::default();
        for file in &new_files {
            let id = file
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let validated_file = match sanitize::validate_read_path(file) {
                Ok(p) => p,
                Err(e) => {
                    result.errors.push(format!("{}: {}", id, e));
                    continue;
                }
            };
            let text = match fs::read_to_string(&validated_file) {
                Ok(t) => t,
                Err(e) => {
                    result.errors.push(format!("{}: {}", id, e));
                    continue;
                }
            };

            let metadata = serde_json::json!({
                "source": "ai-contexters",
                "chunk_id": id,
            });

            match sync_chunk_single(&id, &text, &metadata, config) {
                Ok(()) => result.chunks_pushed += 1,
                Err(e) => result.errors.push(format!("{}: {}", id, e)),
            }
        }
        result
    };

    // Update sync state with newly synced chunks
    for file in &new_files {
        let id = file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        state.synced_chunks.insert(id);
    }
    state.last_synced = Some(Utc::now());
    state.total_pushes += result.chunks_pushed;
    save_sync_state(&state)?;

    Ok(result)
}

// ============================================================================
// Search (utility)
// ============================================================================

/// Search memex for relevant chunks. Utility for testing/debugging.
///
/// Runs: `rmcp-memex search -n <namespace> -q <query>`
pub fn search_memex(query: &str, namespace: &str) -> Result<String> {
    if !check_memex_available() {
        bail!("rmcp-memex not found in PATH");
    }

    let output = Command::new("rmcp-memex")
        .arg("search")
        .arg("-n")
        .arg(namespace)
        .arg("-q")
        .arg(query)
        .output()
        .context("Failed to run rmcp-memex search")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("rmcp-memex search failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memex_config_default() {
        let config = MemexConfig::default();
        assert_eq!(config.namespace, "ai-contexts");
        assert!(config.db_path.is_none());
        assert!(config.batch_mode);
    }

    #[test]
    fn test_sync_state_serialization() {
        let mut synced_chunks = HashSet::new();
        synced_chunks.insert("chunk_001".to_string());
        synced_chunks.insert("chunk_002".to_string());

        let state = MemexSyncState {
            last_synced: Some(Utc::now()),
            synced_chunks,
            total_pushes: 42,
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let restored: MemexSyncState = serde_json::from_str(&json).unwrap();

        assert!(restored.last_synced.is_some());
        assert_eq!(restored.synced_chunks.len(), 2);
        assert!(restored.synced_chunks.contains("chunk_001"));
        assert!(restored.synced_chunks.contains("chunk_002"));
        assert_eq!(restored.total_pushes, 42);
    }

    #[test]
    fn test_sync_state_tracks_chunks() {
        let mut state = MemexSyncState::default();
        assert!(state.synced_chunks.is_empty());

        state.synced_chunks.insert("a".to_string());
        assert!(state.synced_chunks.contains("a"));
        assert!(!state.synced_chunks.contains("b"));

        state.synced_chunks.insert("b".to_string());
        assert_eq!(state.synced_chunks.len(), 2);
    }

    #[test]
    fn test_sync_result_default() {
        let result = SyncResult::default();
        assert_eq!(result.chunks_pushed, 0);
        assert_eq!(result.chunks_skipped, 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_parse_indexed_count() {
        assert_eq!(parse_indexed_count("Indexed 42 documents"), Some(42));
        assert_eq!(
            parse_indexed_count("Processing... 10 chunks indexed"),
            Some(10)
        );
        assert_eq!(parse_indexed_count("no numbers here"), None);
        assert_eq!(parse_indexed_count(""), None);
    }

    #[test]
    fn test_check_memex_available() {
        // Just verify it doesn't panic — actual result depends on system
        let _ = check_memex_available();
    }
}
