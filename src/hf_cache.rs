//! HuggingFace cache utilities for aicx's runtime embedder fallback.
//!
//! Shared with `build.rs` conceptually: both look for the newest snapshot of a
//! given HF repository in a list of cache directories. The runtime copy also
//! supports operator overrides via `AICX_EMBEDDER_PATH`.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Find a snapshot for `repo` that contains every file in `required_all` and at
/// least one file from `required_any`. Returns `None` if no cache entry matches.
pub fn find_snapshot_with_any(
    repo: &str,
    required_all: &[&str],
    required_any: &[&str],
) -> Option<PathBuf> {
    for base in cache_bases() {
        if let Some(snapshot) = find_snapshot_in_base(&base, repo, required_all, required_any) {
            return Some(snapshot);
        }
    }
    None
}

fn cache_bases() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(path) = env::var("AICX_HF_CACHE") {
        out.push(PathBuf::from(path));
    }
    if let Ok(path) = env::var("HUGGINGFACE_HUB_CACHE") {
        out.push(PathBuf::from(path));
    }
    if let Ok(path) = env::var("HF_HUB_CACHE") {
        out.push(PathBuf::from(path));
    }
    if let Ok(path) = env::var("HF_HOME") {
        out.push(PathBuf::from(path).join("hub"));
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".cache").join("huggingface").join("hub"));
        out.push(home.join(".aicx").join("embeddings"));
        out.push(home.join(".aicx").join("embeddings").join("hub"));
    }
    out.sort();
    out.dedup();
    out
}

fn find_snapshot_in_base(
    base: &Path,
    repo: &str,
    required_all: &[&str],
    required_any: &[&str],
) -> Option<PathBuf> {
    let repo_dir = base.join(format!("models--{}", repo.replace('/', "--")));
    let snapshots_dir = repo_dir.join("snapshots");

    let snapshots_dir = if snapshots_dir.exists() {
        snapshots_dir
    } else {
        let target = repo.to_ascii_lowercase();
        let mut matched: Option<PathBuf> = None;
        if let Ok(entries) = fs::read_dir(base) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("models--") {
                    continue;
                }
                let repo_id = name
                    .strip_prefix("models--")
                    .unwrap_or("")
                    .replace("--", "/");
                if repo_id.to_ascii_lowercase() == target {
                    matched = Some(entry.path().join("snapshots"));
                    break;
                }
            }
        }
        matched?
    };

    let entries = fs::read_dir(&snapshots_dir).ok()?;
    let mut best: Option<(SystemTime, PathBuf)> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !required_all.iter().all(|f| path.join(f).exists()) {
            continue;
        }
        if !required_any.is_empty() && !required_any.iter().any(|f| path.join(f).exists()) {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        match &best {
            Some((best_time, _)) if *best_time >= modified => {}
            _ => best = Some((modified, path)),
        }
    }

    best.map(|(_, p)| p)
}
