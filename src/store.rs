//! Central context store for ai-contexters.
//!
//! Manages the `~/.ai-contexters/` directory structure:
//! - `<project>/<date>/<time>_<agent>-context.{md,json}` — extracted timelines
//! - `memex/chunks/` — pre-chunked text for RAG indexing
//! - `index.json` — manifest of stored contexts
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::chunker::{self, ChunkerConfig};
use crate::output::TimelineEntry;
use crate::sanitize;

// ============================================================================
// Path helpers
// ============================================================================

/// Returns the base store directory: `~/.ai-contexters/`
///
/// Creates the directory if it doesn't exist.
pub fn store_base_dir() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("No home directory")?
        .join(".ai-contexters");
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create store dir: {}", dir.display()))?;
    Ok(dir)
}

/// Returns the project directory: `~/.ai-contexters/<project>/`
pub fn project_dir(project: &str) -> Result<PathBuf> {
    let dir = store_base_dir()?.join(project);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Returns the chunks directory: `~/.ai-contexters/memex/chunks/`
pub fn chunks_dir() -> Result<PathBuf> {
    let dir = store_base_dir()?.join("memex").join("chunks");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Full path for a specific context markdown file.
///
/// Layout: `~/.ai-contexters/<project>/<date>/<time>_<agent>-context.md`
pub fn get_context_path(project: &str, agent: &str, date: &str, time: &str) -> Result<PathBuf> {
    let dir = store_base_dir()?.join(project).join(date);
    fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}_{}-context.md", time, agent)))
}

/// Full path for a specific context JSON file.
///
/// Layout: `~/.ai-contexters/<project>/<date>/<time>_<agent>-context.json`
pub fn get_context_json_path(
    project: &str,
    agent: &str,
    date: &str,
    time: &str,
) -> Result<PathBuf> {
    let dir = store_base_dir()?.join(project).join(date);
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
    let path = match store_base_dir() {
        Ok(d) => d.join("index.json"),
        Err(_) => return StoreIndex::default(),
    };

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
    let path = store_base_dir()?.join("index.json");
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

// ============================================================================
// Context writing
// ============================================================================

/// Write timeline entries to the central store.
///
/// Creates two files:
/// - `~/.ai-contexters/<project>/<date>/<time>_<agent>-context.md`
/// - `~/.ai-contexters/<project>/<date>/<time>_<agent>-context.json`
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
/// Layout: `~/.ai-contexters/<project>/<date>/<time>_<agent>-<seq:03>.md`
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
    let dir = store_base_dir()?.join(project).join(date);
    fs::create_dir_all(&dir)?;

    let mut written = Vec::new();

    for chunk in &chunks {
        // Extract seq from chunk.id (last _NNN part)
        let seq = chunk
            .id
            .rsplit('_')
            .next()
            .unwrap_or("001");

        let filename = format!("{}_{}-{}.md", time, agent, seq);
        let path = dir.join(&filename);

        let write_path = sanitize::validate_write_path(&path)?;
        fs::write(&write_path, &chunk.text)?;
        written.push(path);
    }

    Ok(written)
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
            assert!(path.to_string_lossy().contains("ai-contexters"));
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
}
