#![allow(unused_imports)]
use super::*;
use crate::extraction::UNPROTECTED_SOURCE_WARNING;

fn project_filter_matches_identity(
    organization: &str,
    repository: &str,
    filters: &[String],
) -> bool {
    filters.is_empty()
        || filters
            .iter()
            .any(|filter| project_filter_matches(organization, repository, filter))
}

fn canonical_project_parts(value: &str) -> Option<(String, String)> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || trimmed.contains(":\\")
    {
        return None;
    }

    let mut parts = trimmed.split(['/', '\\']);
    let organization = parts
        .next()
        .map(str::trim)
        .filter(|part| !part.is_empty())?;
    let repository = parts
        .next()
        .map(str::trim)
        .filter(|part| !part.is_empty())?;
    if parts.next().is_some() {
        return None;
    }

    Some((organization.to_string(), repository.to_string()))
}

/// Check an ingest-time path hint or already-persisted canonical identity.
///
/// Resolution tiers (in order):
///
/// 1. **Literal `owner/repo` canonical string** — for inputs that are
///    already a slash-shaped slug (not a path).
/// 2. **Strict adjacent path segments** — `owner/repo` requires both
///    segments to appear adjacent in the path. `owner/` matches any
///    occurrence of `owner`. `/repo` matches any occurrence of `repo`.
///    Bare `name` matches any segment equal to `name`.
///
/// The previous last-segment relax (path's last segment equals filter's
/// repo, even when the owner is absent from the path) is **gone**.
/// It caused the cross-org leak flagged by `chatgpt-codex-connector` P1
/// on PR #8 (filter `Loctree/aicx` matched `/.../Vetcoders/aicx`). Query
/// paths do not call this helper: their ground truth is the project identity
/// persisted when the card was ingested. In particular, this function never
/// opens `.git/config` or re-derives historical identity from a live checkout.
pub(crate) fn project_filter_matches_path(cwd: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }

    // Tier 1: the caller already has a persisted `owner/repo` slug.
    if let Some((organization, repository)) = canonical_project_parts(cwd) {
        return project_filter_matches_identity(&organization, &repository, filters);
    }

    // Tier 2: legacy ingest path fallback, strict and filesystem-free.
    let path_segments: Vec<String> = cwd
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_ascii_lowercase())
        .collect();
    if path_segments.is_empty() {
        return false;
    }
    filters.iter().any(|filter| {
        let filter = filter.trim().to_ascii_lowercase();
        if filter.is_empty() {
            return false;
        }

        if let Some(repo_only) = filter.strip_prefix('/') {
            return !repo_only.is_empty()
                && !repo_only.contains('/')
                && path_segments.iter().any(|segment| segment == repo_only);
        }

        if let Some(owner_only) = filter.strip_suffix('/') {
            return !owner_only.is_empty()
                && !owner_only.contains('/')
                && path_segments.iter().any(|segment| segment == owner_only);
        }

        if filter.contains('/') {
            let mut parts = filter.split('/');
            let Some(owner) = parts.next().filter(|part| !part.is_empty()) else {
                return false;
            };
            let Some(repo) = parts.next().filter(|part| !part.is_empty()) else {
                return false;
            };
            if parts.next().is_some() {
                return false;
            }
            // Strict adjacency — no last-segment relax. Cross-org leakage
            // (filter `Loctree/aicx` matching `/.../Vetcoders/aicx`) is
            // impossible and no live remote is consulted.
            return path_segments
                .windows(2)
                .any(|pair| pair[0] == owner && pair[1] == repo);
        }

        path_segments.iter().any(|segment| segment == &filter)
    })
}

/// Token-aware "does this body mention `repo` as a whole word?" check.
///
/// Splits the (already-lowercased) body on runs of non-identifier
/// characters — alphanumerics, `-`, and `_` count as in-word, everything
/// else is a separator. Returns true iff some resulting token equals
/// `repo_lower` byte-for-byte.
///
/// This is used by codescribe project-hint inference (sources.rs ~4943):
/// the prior `lower_body.contains(&repo)` substring match was flagged in
/// the gemini-code-assist MEDIUM PR #8 review for re-introducing a
/// suffix-leak (`-p vista` matched bodies mentioning `vista-portal`).
/// Token-equality is consistent with the rest of pass-4's strict
/// identity matchers (e.g. `project_filter_matches`).
pub(crate) fn body_mentions_repo_token(body_lower: &str, repo_lower: &str) -> bool {
    if repo_lower.is_empty() {
        return false;
    }
    body_lower
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .any(|token| token == repo_lower)
}

/// Soft pre-filter on Claude project directory names.
///
/// The Claude on-disk encoding is the original cwd with `/` replaced by
/// `-` (optionally prefixed with a leading `-` for absolute paths). The
/// encoding is **inherently lossy** — given `-Users-user-Git-vista` we
/// cannot recover whether the original was `/Users/user/Git/vista`
/// (single-segment repo `vista`) or `/Users/user/Git/nextra-docs-vista`
/// or `/Users-user/Git/vista`, etc.
///
/// This function therefore acts as a **permissive pre-filter** at the
/// directory-listing stage. Three legal match shapes for a bare `repo`
/// filter:
///
///   1. Exact: `dir_name == filter` (single-component repo, no prefix).
///   2. Leading-dash exact: `dir_name == "-{filter}"` (single-component
///      repo encoded as absolute path).
///   3. Last-chunk: `dir_name.ends_with("-{filter}")` (filter looks like
///      a plausible last cwd segment).
///
/// For `owner/repo` filters (containing `/`), we defer to the strict
/// path-shaped matcher on a best-effort decoded form.
///
/// Case 3 reintroduces a controlled ambiguity — `-p vista` will match
/// both `-Users-user-Git-vista` (correct) **and**
/// `-Users-user-Git-nextra-docs-vista` (likely not what the operator
/// meant). That ambiguity is **inherent** to the Claude encoding and
/// cannot be resolved from the directory name alone. The strict
/// per-entry `cwd` filter inside the Claude extraction pipeline
/// `entry.cwd.as_deref().is_some_and(|c| project_filter_matches_path(c, ...))`)
/// resolves it precisely when the entry carries a `cwd` field — the
/// common case. Sessions without `cwd` were previously dropped by the
/// over-restrictive equality check; this relaxation keeps them reachable
/// so the strict per-entry pass can decide.
///
/// History: the previous `decoded.eq_ignore_ascii_case(filter) ||
/// dir_name.eq_ignore_ascii_case(filter)` required the filter to be the
/// **whole** encoded dir name (e.g. `-p Users-user-Git-aicx`) instead
/// of just the repo name. That broke every existing `-p reponame`
/// invocation against absolute-path Claude dirs and was the regression
/// flagged by gemini-code-assist HIGH + chatgpt-codex-connector P1
/// comments on PR #8.
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
                if project_filter_matches_path(c, std::slice::from_ref(p)) {
                    return canonical_repo_label(p);
                }
            }
        }
    }

    let cwd_str = match cwd {
        Some(c) if !c.is_empty() => c,
        _ => return "unknown".to_string(),
    };

    let path = std::path::Path::new(cwd_str);
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

/// Infer the `--repo` label by walking up from the current directory to the
/// nearest `.git` root, falling back to the cwd's own basename. Canonicalized
/// through [`canonical_repo_label`] so the result matches stored slugs.
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

/// Detect project name from current working directory.
///
/// Strategy: git repo root dirname → cwd dirname → "unknown".
pub fn detect_project_name() -> String {
    // Try git repo root
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        && output.status.success()
    {
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(name) = std::path::Path::new(&s).file_name() {
            return canonical_repo_label(&name.to_string_lossy());
        }
    }

    // Fallback: cwd dirname
    if let Ok(cwd) = std::env::current_dir()
        && let Some(name) = cwd.file_name()
    {
        return canonical_repo_label(&name.to_string_lossy());
    }

    "unknown".to_string()
}

pub fn decode_claude_project_path(encoded: &str) -> String {
    let stripped = encoded.strip_prefix('-').unwrap_or(encoded);
    stripped.to_string()
}
