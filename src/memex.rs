//! Integration with rmcp-memex for semantic indexing.
//!
//! Shells out to the `rmcp-memex` CLI binary for:
//! - Batch import of canonical chunk records (`rmcp-memex import`)
//! - Single chunk upsert (`rmcp-memex upsert`)
//! - Semantic search (`rmcp-memex search`)
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use dirs;
use rmcp_memex::compute_content_hash;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::Write;
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
    /// Use batch JSONL import (true) or per-chunk `upsert` (false)
    pub batch_mode: bool,
    /// Compatibility flag for legacy `rmcp-memex index` callers.
    ///
    /// Live AICX sync paths use JSONL import so metadata stays aligned with the
    /// canonical sidecar contract. The legacy `index` shim still honors this.
    pub preprocess: bool,
}

impl Default for MemexConfig {
    fn default() -> Self {
        Self {
            namespace: "ai-contexts".to_string(),
            db_path: None,
            batch_mode: true,
            preprocess: true,
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

#[derive(Debug, Serialize)]
struct ImportRecord {
    id: String,
    text: String,
    metadata: serde_json::Value,
    content_hash: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ImportStats {
    imported: usize,
    skipped: usize,
    errors: usize,
}

// ============================================================================
// Sync state persistence
// ============================================================================

/// Path to sync state file: `~/.aicx/memex/sync_state.json`
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

/// Legacy shim: sync all chunk files in a directory via `rmcp-memex index`.
///
/// Live AICX flows use `sync_chunks_import` so batch and per-chunk sync share
/// the same metadata contract. This helper remains only as a compatibility
/// boundary for callers that still want raw recursive indexing.
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
        .filter(|e| {
            let path = e.path();
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            ext == "txt" || ext == "md"
        })
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
        .arg("-r") // Recursive support for nested canonical store
        .arg("--dedup")
        .arg("true");

    if config.preprocess {
        cmd.arg("--preprocess");
    }

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

/// Sync a specific list of chunk files to memex using a temporary JSONL import.
/// This ensures metadata parity between batch and per-chunk sync.
pub fn sync_chunks_import(chunk_paths: &[PathBuf], config: &MemexConfig) -> Result<SyncResult> {
    if !check_memex_available() {
        bail!("rmcp-memex not found in PATH. Install with: cargo install rmcp-memex");
    }

    if chunk_paths.is_empty() {
        return Ok(SyncResult::default());
    }

    let tmp_jsonl = std::env::temp_dir().join(format!("aicx-sync-{}.jsonl", std::process::id()));
    let mut file = fs::File::create(&tmp_jsonl)?;

    let mut count = 0;
    for path in chunk_paths {
        let validated_path = sanitize::validate_read_path(path)?;
        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let text = sanitize::read_to_string_validated(&validated_path)?;
        let line = chunk_import_record(&validated_path, &id, &text);
        serde_json::to_writer(&file, &line)?;
        writeln!(&mut file)?;
        count += 1;
    }

    let mut cmd = Command::new("rmcp-memex");
    cmd.arg("import")
        .arg("-n")
        .arg(&config.namespace)
        .arg("-i")
        .arg(&tmp_jsonl)
        .arg("--skip-existing");

    if let Some(ref db_path) = config.db_path {
        cmd.arg("--db-path").arg(db_path);
    }

    let output = cmd
        .output()
        .context("Failed to run rmcp-memex import via external dependency 'rmcp-memex'")?;
    let _ = fs::remove_file(&tmp_jsonl);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("rmcp-memex import failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stats = parse_import_stats(&format!("{}\n{}", stdout, stderr));

    Ok(SyncResult {
        chunks_pushed: if stats == ImportStats::default() {
            count
        } else {
            stats.imported
        },
        chunks_skipped: stats.skipped,
        errors: if stats.errors > 0 {
            vec![format!(
                "rmcp-memex import reported {} record error(s); inspect rmcp-memex stderr for details",
                stats.errors
            )]
        } else {
            vec![]
        },
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

fn parse_import_stats(output: &str) -> ImportStats {
    let mut stats = ImportStats::default();

    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("Imported:") {
            stats.imported = value
                .split_whitespace()
                .find_map(|word| word.parse::<usize>().ok())
                .unwrap_or(0);
        } else if let Some(value) = trimmed.strip_prefix("Skipped:") {
            stats.skipped = value
                .split_whitespace()
                .find_map(|word| word.parse::<usize>().ok())
                .unwrap_or(0);
        } else if let Some(value) = trimmed.strip_prefix("Errors:") {
            stats.errors = value
                .split_whitespace()
                .find_map(|word| word.parse::<usize>().ok())
                .unwrap_or(0);
        }
    }

    stats
}

#[cfg(test)]
fn chunk_sidecar_path(chunk_path: &Path) -> PathBuf {
    chunk_path.with_extension("meta.json")
}

fn insert_optional_string(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<String>,
) {
    if let Some(value) = value {
        metadata.insert(key.to_string(), serde_json::Value::String(value));
    }
}

fn insert_optional_u64(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<u64>,
) {
    if let Some(value) = value {
        metadata.insert(key.to_string(), serde_json::Value::from(value));
    }
}

fn insert_optional_u32(
    metadata: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<u32>,
) {
    if let Some(value) = value {
        metadata.insert(key.to_string(), serde_json::Value::from(value));
    }
}

fn chunk_metadata_from_header(text: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut metadata = serde_json::Map::new();
    let Some(first_line) = text.lines().next() else {
        return metadata;
    };

    if !first_line.starts_with('[') || !first_line.ends_with(']') {
        return metadata;
    }

    let inner = &first_line[1..first_line.len() - 1];
    for part in inner.split('|') {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        metadata.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    metadata
}

fn chunk_metadata_for_upsert(chunk_path: &Path, chunk_id: &str, text: &str) -> serde_json::Value {
    let content_hash = compute_content_hash(text);
    let mut metadata = serde_json::Map::from_iter([
        (
            "source".to_string(),
            serde_json::Value::String("ai-contexters".to_string()),
        ),
        (
            "chunk_id".to_string(),
            serde_json::Value::String(chunk_id.to_string()),
        ),
        (
            "path".to_string(),
            serde_json::Value::String(chunk_path.to_string_lossy().to_string()),
        ),
        (
            "content_hash".to_string(),
            serde_json::Value::String(content_hash),
        ),
    ]);

    let sidecar = crate::store::load_sidecar(chunk_path);

    if let Some(sidecar) = sidecar {
        metadata.insert(
            "project".to_string(),
            serde_json::Value::String(sidecar.project),
        );
        metadata.insert(
            "agent".to_string(),
            serde_json::Value::String(sidecar.agent),
        );
        metadata.insert("date".to_string(), serde_json::Value::String(sidecar.date));
        metadata.insert(
            "session_id".to_string(),
            serde_json::Value::String(sidecar.session_id),
        );
        metadata.insert(
            "kind".to_string(),
            serde_json::Value::String(sidecar.kind.dir_name().to_string()),
        );
        insert_optional_string(&mut metadata, "cwd", sidecar.cwd);
        insert_optional_string(&mut metadata, "run_id", sidecar.run_id);
        insert_optional_string(&mut metadata, "prompt_id", sidecar.prompt_id);
        insert_optional_string(&mut metadata, "agent_model", sidecar.agent_model);
        insert_optional_string(&mut metadata, "started_at", sidecar.started_at);
        insert_optional_string(&mut metadata, "completed_at", sidecar.completed_at);
        insert_optional_u64(&mut metadata, "token_usage", sidecar.token_usage);
        insert_optional_u32(&mut metadata, "findings_count", sidecar.findings_count);
        insert_optional_string(&mut metadata, "workflow_phase", sidecar.workflow_phase);
        insert_optional_string(&mut metadata, "mode", sidecar.mode);
        insert_optional_string(&mut metadata, "skill_code", sidecar.skill_code);
        insert_optional_string(
            &mut metadata,
            "framework_version",
            sidecar.framework_version,
        );
    } else {
        metadata.extend(chunk_metadata_from_header(text));
    }

    serde_json::Value::Object(metadata)
}

fn chunk_import_record(chunk_path: &Path, chunk_id: &str, text: &str) -> ImportRecord {
    ImportRecord {
        id: chunk_id.to_string(),
        text: text.to_string(),
        metadata: chunk_metadata_for_upsert(chunk_path, chunk_id, text),
        content_hash: compute_content_hash(text),
    }
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
pub fn sync_new_chunk_paths(chunk_paths: &[PathBuf], config: &MemexConfig) -> Result<SyncResult> {
    let mut state = load_sync_state();

    let all_files: Vec<PathBuf> = chunk_paths
        .iter()
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext == "md" || ext == "txt")
        })
        .cloned()
        .collect();

    let new_files: Vec<PathBuf> = all_files
        .iter()
        .filter(|p| {
            let id = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            !state.synced_chunks.contains(&id)
        })
        .cloned()
        .collect();

    if new_files.is_empty() {
        return Ok(SyncResult {
            chunks_pushed: 0,
            chunks_skipped: all_files.len(),
            errors: vec![],
        });
    }

    let (result, synced_files): (SyncResult, Vec<PathBuf>) = if config.batch_mode {
        let result = sync_chunks_import(&new_files, config)?;
        let can_advance_state = result.errors.is_empty()
            && result.chunks_pushed + result.chunks_skipped == new_files.len();
        let synced_files = if can_advance_state {
            new_files.clone()
        } else {
            Vec::new()
        };
        (result, synced_files)
    } else {
        let mut result = SyncResult::default();
        let mut synced_files = Vec::new();
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

            let metadata = chunk_metadata_for_upsert(&validated_file, &id, &text);

            match sync_chunk_single(&id, &text, &metadata, config) {
                Ok(()) => {
                    result.chunks_pushed += 1;
                    synced_files.push(file.clone());
                }
                Err(e) => result.errors.push(format!("{}: {}", id, e)),
            }
        }
        (result, synced_files)
    };

    if let Ok(rt) = tokio::runtime::Runtime::new() {
        let path_refs: Vec<&PathBuf> = new_files.iter().collect();
        if let Err(e) = rt.block_on(crate::steer_index::sync_steer_index(&path_refs)) {
            tracing::warn!("Failed to sync steer index: {}", e);
        }
    }

    for file in &synced_files {
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

pub fn sync_new_chunks(chunks_dir: &Path, config: &MemexConfig) -> Result<SyncResult> {
    if !chunks_dir.exists() {
        return Ok(SyncResult::default());
    }

    let validated_dir = sanitize::validate_dir_path(chunks_dir)?;

    // SECURITY: dir sanitized via validate_dir_path (traversal + canonicalize + allowlist)
    let all_files: Vec<PathBuf> = fs::read_dir(&validated_dir)? // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            ext == "txt" || ext == "md"
        })
        .collect();
    sync_new_chunk_paths(&all_files, config)
}

use crate::rank::{FuzzyResult, score_chunk_content};

// ============================================================================
// High-level fast search via rmcp-memex library
// ============================================================================

/// Compatibility shim for `rmcp-memex` BM25 search tuples.
///
/// The local checkout currently returns `(id, namespace, score)`, while the
/// published crate used during `cargo publish --dry-run` still returns
/// `(id, score)`. We keep the seam inside AICX so the transport boundary stays
/// explicit and ship verification does not depend on matching local checkout
/// state.
trait Bm25SearchHit {
    fn into_hit(self) -> (String, f32);
}

impl Bm25SearchHit for (String, f32) {
    fn into_hit(self) -> (String, f32) {
        self
    }
}

impl Bm25SearchHit for (String, String, f32) {
    fn into_hit(self) -> (String, f32) {
        let (id, _namespace, score) = self;
        (id, score)
    }
}

/// Fast semantic/keyword search using `rmcp_memex`'s embedded LanceDB and Tantivy index.
pub async fn fast_memex_search(
    query: &str,
    limit: usize,
    project_filter: Option<&str>,
) -> Result<(Vec<FuzzyResult>, usize)> {
    use rmcp_memex::search::{BM25Config, BM25Index};
    use rmcp_memex::storage::StorageManager;

    let config = BM25Config::default();
    let index = BM25Index::new(&config).context("Failed to load BM25 index")?;

    let raw_results = index.search(query, Some("ai-contexts"), limit * 5)?;
    let total_scanned = raw_results.len(); // Approximate

    let default_db_path = dirs::home_dir()
        .map(|h| h.join(".rmcp-servers/rmcp-memex/lancedb"))
        .unwrap_or_else(|| PathBuf::from("/tmp/rmcp-memex/lancedb"));

    let db_path = if default_db_path.exists() {
        default_db_path
    } else {
        crate::store::store_base_dir()?.join("steer_db")
    };

    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy())
        .await
        .context("Failed to open LanceDB")?;

    let mut results = Vec::new();
    let project_lower = project_filter.map(|s| s.to_lowercase());

    for raw_result in raw_results {
        if results.len() >= limit {
            break;
        }

        let (id, score) = raw_result.into_hit();

        if let Ok(Some(doc)) = storage.get_document("ai-contexts", &id).await {
            // Apply project filter if any
            let doc_project = doc
                .metadata
                .get("project")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if let Some(ref pf) = project_lower {
                if !doc_project.to_lowercase().contains(pf) {
                    continue;
                }
            }

            let kind = doc
                .metadata
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let agent = doc
                .metadata
                .get("agent")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let date = doc
                .metadata
                .get("date")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let session_id = doc
                .metadata
                .get("session_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let cwd = doc
                .metadata
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // Score content to get density and matched lines
            let chunk_score = score_chunk_content(&doc.document);

            // Extract matching lines
            let query_terms: Vec<&str> = query.split_whitespace().collect();
            let matched_lines: Vec<String> = doc
                .document
                .lines()
                .filter(|line| {
                    let lower = line.to_lowercase();
                    query_terms
                        .iter()
                        .any(|&term| lower.contains(&term.to_lowercase()))
                })
                .take(5)
                .map(|s| s.trim().to_string())
                .collect();

            // Calculate final score using BM25 score and signal density
            // BM25 score usually > 0. The higher the better.
            let final_score = ((chunk_score.score as f32 * 5.0 + score * 10.0) as u8).min(100);

            results.push(FuzzyResult {
                file: format!("{}.md", id),
                path: doc
                    .metadata
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&format!("{}.md", id))
                    .to_string(),
                project: doc_project,
                kind,
                agent,
                date,
                score: final_score,
                label: if final_score >= 80 {
                    "HIGH".to_string()
                } else if final_score >= 60 {
                    "MEDIUM".to_string()
                } else {
                    "LOW".to_string()
                },
                density: chunk_score.density,
                matched_lines,
                session_id,
                cwd,
            });
        }
    }

    // Sort by score
    results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));

    Ok((results, total_scanned))
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
        assert!(config.preprocess);
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

    #[test]
    fn test_chunk_metadata_from_header() {
        let text = "[project: prview-rs | agent: claude | date: 2026-03-24]\n\nhello";
        let metadata = chunk_metadata_from_header(text);

        assert_eq!(metadata.get("project").unwrap(), "prview-rs");
        assert_eq!(metadata.get("agent").unwrap(), "claude");
        assert_eq!(metadata.get("date").unwrap(), "2026-03-24");
    }

    #[test]
    fn test_chunk_metadata_for_upsert_prefers_sidecar() {
        let tmp = std::env::temp_dir().join(format!("ai-ctx-memex-sidecar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let chunk_path = tmp.join("chunk.txt");
        fs::write(
            &chunk_path,
            "[project: wrong | agent: wrong | date: 2026-01-01]\n\nbody",
        )
        .unwrap();
        fs::write(
            chunk_sidecar_path(&chunk_path),
            serde_json::to_vec_pretty(&crate::chunker::ChunkMetadataSidecar {
                id: "chunk".to_string(),
                project: "prview-rs".to_string(),
                agent: "claude".to_string(),
                date: "2026-03-24".to_string(),
                session_id: "sess-1".to_string(),
                cwd: Some("/Users/tester/workspaces/prview-rs".to_string()),
                kind: crate::store::Kind::Conversations,
                run_id: Some("mrbl-001".to_string()),
                prompt_id: Some("api-redesign_20260327".to_string()),
                agent_model: Some("gpt-5.4".to_string()),
                started_at: Some("2026-03-27T10:00:00Z".to_string()),
                completed_at: Some("2026-03-27T10:01:00Z".to_string()),
                token_usage: Some(1234),
                findings_count: Some(4),
                workflow_phase: Some("implement".to_string()),
                mode: Some("session-first".to_string()),
                skill_code: Some("vc-workflow".to_string()),
                framework_version: Some("2026-03".to_string()),
            })
            .unwrap(),
        )
        .unwrap();

        let metadata = chunk_metadata_for_upsert(&chunk_path, "chunk", "body");
        let object = metadata.as_object().unwrap();

        assert_eq!(object.get("project").unwrap(), "prview-rs");
        assert_eq!(object.get("agent").unwrap(), "claude");
        assert_eq!(object.get("date").unwrap(), "2026-03-24");
        assert_eq!(object.get("session_id").unwrap(), "sess-1");
        assert_eq!(object.get("kind").unwrap(), "conversations");
        assert_eq!(
            object.get("cwd").unwrap(),
            "/Users/tester/workspaces/prview-rs"
        );
        assert_eq!(object.get("run_id").unwrap(), "mrbl-001");
        assert_eq!(object.get("prompt_id").unwrap(), "api-redesign_20260327");
        assert_eq!(object.get("agent_model").unwrap(), "gpt-5.4");
        assert_eq!(object.get("started_at").unwrap(), "2026-03-27T10:00:00Z");
        assert_eq!(object.get("completed_at").unwrap(), "2026-03-27T10:01:00Z");
        assert_eq!(object.get("token_usage").unwrap(), 1234);
        assert_eq!(object.get("findings_count").unwrap(), 4);
        assert_eq!(object.get("workflow_phase").unwrap(), "implement");
        assert_eq!(object.get("mode").unwrap(), "session-first");
        assert_eq!(object.get("skill_code").unwrap(), "vc-workflow");
        assert_eq!(object.get("framework_version").unwrap(), "2026-03");
        assert_eq!(
            object.get("path").unwrap(),
            &serde_json::Value::String(chunk_path.to_string_lossy().to_string())
        );
        assert_eq!(
            object.get("content_hash").unwrap(),
            &serde_json::Value::String(compute_content_hash("body"))
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_chunk_import_record_includes_content_hash() {
        let chunk_path = PathBuf::from("/tmp/ctx/chunk.md");
        let record = chunk_import_record(&chunk_path, "chunk", "body");

        assert_eq!(record.id, "chunk");
        assert_eq!(record.content_hash, compute_content_hash("body"));
    }

    #[test]
    fn test_parse_import_stats() {
        let stats = parse_import_stats(
            "Import complete:\n  Imported: 2 documents\n  Skipped:  1 (already exist)\n  Errors:   3",
        );

        assert_eq!(
            stats,
            ImportStats {
                imported: 2,
                skipped: 1,
                errors: 3,
            }
        );
    }

    #[test]
    fn test_bm25_search_hit_supports_published_tuple_shape() {
        let hit = ("chunk-1".to_string(), 0.42_f32);
        assert_eq!(hit.into_hit(), ("chunk-1".to_string(), 0.42_f32));
    }

    #[test]
    fn test_bm25_search_hit_supports_checkout_tuple_shape() {
        let hit = ("chunk-1".to_string(), "ai-contexts".to_string(), 0.42_f32);
        assert_eq!(hit.into_hit(), ("chunk-1".to_string(), 0.42_f32));
    }
}
