use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::Path;

use crate::timeline::TimelineEntry;

/// Determine the project/repo name for a given entry.
///
/// 1. If a single project filter is active, it unconditionally becomes the project name.
/// 2. If multiple filters are active, uses the first one matching the `cwd`.
/// 3. Otherwise, tries to walk up the `cwd` path to find a `.git` root.
/// 4. Fallback: last path component of `cwd`.
pub fn repo_name_from_cwd(cwd: Option<&str>, project_filter: &[String]) -> String {
    if !project_filter.is_empty() {
        if project_filter.len() == 1 {
            return canonical_repo_label(&project_filter[0]);
        } else if let Some(c) = cwd {
            for p in project_filter {
                // Use the same word-boundary path match as the project
                // filter itself, so a label cannot be picked via a raw
                // substring (`--project test` against `/tmp/fastest-project`)
                // even though the filter step rejects that match.
                if super::project_filter_matches_path(c, std::slice::from_ref(p)) {
                    return canonical_repo_label(p);
                }
            }
        }
    }

    let cwd_str = match cwd {
        Some(c) if !c.is_empty() => c,
        _ => return "unknown".to_string(),
    };

    let path = Path::new(cwd_str);
    let mut current = Some(path);

    while let Some(p) = current {
        if !p.as_os_str().is_empty()
            && p.join(".git").is_dir()
            && let Some(name) = p.file_name()
        {
            return canonical_repo_label(&name.to_string_lossy());
        }
        current = p.parent();
    }

    path.file_name()
        .map(|n| canonical_repo_label(&n.to_string_lossy()))
        .unwrap_or_else(|| "unknown".to_string())
}

fn canonical_repo_label(value: &str) -> String {
    value
        .split('/')
        .map(|segment| segment.trim().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

/// Derive canonical repo labels from extracted entries.
pub fn repo_labels_from_entries(
    entries: &[TimelineEntry],
    project_filter: &[String],
) -> Vec<String> {
    let mut labels = BTreeSet::new();

    for entry in entries {
        let repo = repo_name_from_cwd(entry.cwd.as_deref(), project_filter);
        if repo != "unknown" {
            labels.insert(repo);
        }
    }

    labels.into_iter().collect()
}

/// Infer the current repo name with an error if neither git root nor cwd is usable.
pub fn infer_repo_name_from_current_dir() -> Result<String> {
    let cwd = std::env::current_dir().context("Cannot determine current directory")?;
    let mut probe = cwd.as_path();
    loop {
        if probe.join(".git").exists() {
            let repo = probe
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .map(canonical_repo_label)
                .ok_or_else(|| anyhow::anyhow!("Could not infer --repo from git root"))?;
            return Ok(repo);
        }
        let Some(parent) = probe.parent() else {
            break;
        };
        probe = parent;
    }

    let repo = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(canonical_repo_label)
        .ok_or_else(|| anyhow::anyhow!("Could not infer --repo from the current directory"))?;
    Ok(repo)
}

/// Detect project name from current working directory.
///
/// Strategy: git repo root dirname -> cwd dirname -> "unknown".
pub fn detect_project_name() -> String {
    infer_repo_name_from_current_dir().unwrap_or_else(|_| "unknown".to_string())
}

/// Decode a Claude project path from the encoded directory name.
///
/// Claude's project-dir encoding is lossy: `-` may be either a literal
/// repository-name character or an encoded `/`. Replacing every dash with `/`
/// turns hyphenated repos into fake path segments, so this helper preserves the
/// stored name and only removes the leading absolute-path sentinel.
///
/// Example: `-Users-maciejgad-hosted-VetCoders-CodeScribe`
///       -> `Users-maciejgad-hosted-VetCoders-CodeScribe`
pub fn decode_claude_project_path(encoded: &str) -> String {
    let stripped = encoded.strip_prefix('-').unwrap_or(encoded);
    stripped.to_string()
}
