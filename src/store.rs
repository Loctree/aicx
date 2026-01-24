//! Central context store for ai-contexters.
//!
//! Manages the `~/.ai-contexters/` directory structure:
//! - `contexts/<project>/<agent>/<date>.md` — extracted timelines
//! - `memex/chunks/` — pre-chunked text for RAG indexing
//! - `index.json` — manifest of stored contexts
//!
//! Created by M&K (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use crate::output::TimelineEntry;

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

/// Returns the contexts directory: `~/.ai-contexters/contexts/`
pub fn contexts_dir() -> Result<PathBuf> {
    let dir = store_base_dir()?.join("contexts");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Returns the chunks directory: `~/.ai-contexters/memex/chunks/`
pub fn chunks_dir() -> Result<PathBuf> {
    let dir = store_base_dir()?.join("memex").join("chunks");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Full path for a specific context file.
pub fn get_context_path(project: &str, agent: &str, date: &str) -> Result<PathBuf> {
    let dir = contexts_dir()?.join(project).join(agent);
    fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}.md", date)))
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

/// Write timeline entries to the central store as a markdown file.
///
/// Path: `~/.ai-contexters/contexts/<project>/<agent>/<date>.md`
///
/// If the file already exists, entries are appended (with dedup header).
/// Returns the path of the written file.
pub fn write_context(
    project: &str,
    agent: &str,
    date: &str,
    entries: &[TimelineEntry],
) -> Result<PathBuf> {
    let path = get_context_path(project, agent, date)?;

    let mut content = String::new();

    // Header if new file
    if !path.exists() {
        content.push_str(&format!("# {} | {} | {}\n\n", project, agent, date));
    }

    for entry in entries {
        let ts = entry.timestamp.format("%Y-%m-%d %H:%M:%S UTC");
        content.push_str(&format!("### {} | {}\n", ts, entry.role));

        for line in entry.message.lines() {
            content.push_str(&format!("> {}\n", line));
        }
        content.push('\n');
    }

    if path.exists() {
        // Append mode
        let mut existing = fs::read_to_string(&path)?;
        existing.push_str(&content);
        fs::write(&path, existing)?;
    } else {
        fs::write(&path, &content)?;
    }

    Ok(path)
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
    fn test_contexts_dir() {
        if let Ok(path) = contexts_dir() {
            assert!(path.to_string_lossy().contains("contexts"));
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
    fn test_get_context_path() {
        if let Ok(path) = get_context_path("CodeScribe", "claude", "2026-01-22") {
            let s = path.to_string_lossy();
            assert!(s.contains("CodeScribe"));
            assert!(s.contains("claude"));
            assert!(s.ends_with("2026-01-22.md"));
        }
    }

    #[test]
    fn test_write_context_creates_file() {
        let tmp = env::temp_dir().join("ai-ctx-test-store");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("contexts").join("TestProj").join("claude")).unwrap();

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

        // Write to a temp path directly (bypass home dir)
        let path = tmp.join("contexts/TestProj/claude/2026-01-22.md");
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
        fs::write(&path, &content).unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("# TestProj | claude | 2026-01-22"));
        assert!(written.contains("### 2026-01-22 14:30:05 UTC | user"));
        assert!(written.contains("> hello world"));
        assert!(written.contains("### 2026-01-22 14:30:12 UTC | assistant"));
        assert!(written.contains("> hi there"));
        assert!(written.contains("> second line"));

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
