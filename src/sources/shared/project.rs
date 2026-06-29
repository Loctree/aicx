#![allow(unused_imports)]
use super::*;
use crate::sources::UNPROTECTED_SOURCE_WARNING;

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

/// Detect Windows-style absolute paths so Tier 1 canonical resolution
/// also fires for `C:\Users\...` / `C:/Users/...` / UNC `\\server\share`
/// shapes. Without this, Windows local checkouts where the canonical
/// `(owner, repository)` lives only in `.git/config` (not in path
/// segments) silently fall through to Tier 3 segment-adjacency matching
/// and `-p owner/repo` filters cannot reach them.
///
/// Surfaced by `chatgpt-codex-connector` P1 on PR #8 at src/sources.rs:1069.
pub(crate) fn is_windows_absolute_path(s: &str) -> bool {
    let bytes = s.as_bytes();
    // Drive-letter form: `C:\…` or `C:/…` (`X:` for any ASCII letter).
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    // UNC form: `\\server\share…`.
    s.starts_with("\\\\")
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

/// Check if any project filter matches the given path or canonical identity.
///
/// Resolution tiers (in order):
///
/// 1. **Canonical identity from `aicx_parser::segmentation::infer_tiered_identity_from_cwd`**
///    — consults `cwd`'s `.git/config` remote URL, then known-layout
///    heuristics, then URL-shape inference. This is the **ground truth**
///    resolver, the same one `semantic_segments` uses to bucket entries.
/// 2. **Literal `owner/repo` canonical string** — for inputs that are
///    already a slash-shaped slug (not a path).
/// 3. **Strict adjacent path segments** — `owner/repo` requires both
///    segments to appear adjacent in the path. `owner/` matches any
///    occurrence of `owner`. `/repo` matches any occurrence of `repo`.
///    Bare `name` matches any segment equal to `name`.
///
/// The previous "tier 3 relax" (path's last segment equals filter's
/// repo, even when the owner is absent from the path) is **gone**.
/// It caused the cross-org leak flagged by `chatgpt-codex-connector` P1
/// on PR #8 (filter `Loctree/aicx` matched `/.../Vetcoders/aicx`).
/// The original bug #14 workflow (`-p Loctree/aicx` against
/// `/Users/user/Git/aicx`) now travels through Tier 1: when the local
/// path has a `.git/config` pointing at `github.com/Loctree/aicx`, the
/// canonical resolver returns `(Loctree, aicx)` and the filter matches
/// honestly — by ground truth, not by path-noise heuristic. For paths
/// with no resolvable canonical identity (no `.git`, no URL shape, no
/// known layout), `-p owner/repo` requires strict adjacency.
pub(crate) fn project_filter_matches_path(cwd: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }

    // Tier 1: ground-truth canonical resolver (git remote → known layout
    // → URL shape). Same machinery `semantic_segments` uses to bucket
    // entries, so the filter and the bucketer cannot disagree.
    //
    // We only invoke Tier 1 when the cwd LOOKS LIKE A REAL CWD — an
    // absolute path, a `~/...` home-relative path, or a URL shape. For
    // anything else (relative path strings, encoded Claude dir names,
    // bare segments) Tier 1's internal `discover_git_root` would walk
    // ancestors that resolve relative to the *test runner's own cwd*,
    // spuriously locking onto the running process's `.git` and
    // reporting THAT remote's identity. Guarding here is cheaper and
    // honester than patching `discover_git_root` at the parser layer.
    let cwd_trimmed = cwd.trim();
    // Windows-shape admission is gated behind `cfg!(windows)`. On
    // non-Windows runners, `Path::ancestors()` over a `C:\...` string
    // resolves the empty trailing component against the test process's
    // own cwd and spuriously locks onto the running git remote — the
    // same Tier-1 leak `resolvable_shape` was designed to prevent for
    // unanchored relative paths. Gating by compile-time platform keeps
    // Windows local-checkout matching (codex P1 src/sources.rs:1069)
    // honest on Windows runners while non-Windows runners cleanly fall
    // through to Tier 3 segment matching.
    let resolvable_shape = cwd_trimmed.starts_with('/')
        || cwd_trimmed.starts_with('~')
        || cwd_trimmed.starts_with("http://")
        || cwd_trimmed.starts_with("https://")
        || cwd_trimmed.starts_with("git@")
        || cwd_trimmed.starts_with("ssh://")
        || cwd_trimmed.starts_with("git://")
        || (cfg!(windows) && is_windows_absolute_path(cwd_trimmed));
    if resolvable_shape
        && let Some(tiered) = aicx_parser::segmentation::infer_tiered_identity_from_cwd(Some(cwd))
    {
        return project_filter_matches_identity(
            &tiered.identity.organization,
            &tiered.identity.repository,
            filters,
        );
    }

    // Tier 2: cwd is already a literal `owner/repo` slug (not a path).
    if let Some((organization, repository)) = canonical_project_parts(cwd) {
        return project_filter_matches_identity(&organization, &repository, filters);
    }

    // Tier 3: strict path-segment matching, no last-segment relax.
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
            // Strict adjacency — no last-segment relax. Cross-org leak
            // (filter `Loctree/aicx` matching `/.../Vetcoders/aicx`)
            // is impossible in this branch. For a local checkout to
            // match by canonical identity, Tier 1 must resolve it via
            // `.git/config` remote URL.
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
/// per-entry `cwd` filter inside `extract_claude` (sources.rs ~1587,
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
pub(crate) fn claude_project_dir_matches_filter(dir_name: &str, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }

    filters.iter().any(|filter| {
        let filter = filter.trim();
        if filter.is_empty() {
            return false;
        }

        if filter.contains('/') {
            // owner/repo or /repo: defer to the strict path matcher on a
            // best-effort decoded form. Decode is lossy (hyphens vs
            // slashes ambiguous) but the path matcher's adjacency
            // semantics tolerate that; the per-entry cwd filter
            // downstream catches the strict case.
            let decoded = decode_claude_project_path(dir_name);
            return project_filter_matches_path(&decoded, &[filter.to_string()]);
        }

        let needle_lower = filter.to_ascii_lowercase();
        let dir_lower = dir_name.to_ascii_lowercase();

        // Shape 1: exact (no leading dash, single-component).
        if dir_lower == needle_lower {
            return true;
        }
        // Shape 2: leading-dash exact (single-component as absolute).
        let dash_exact = format!("-{needle_lower}");
        if dir_lower == dash_exact {
            return true;
        }
        // Shape 3: last-`-`-chunk match (plausible last cwd segment).
        dir_lower.ends_with(&dash_exact)
    })
}

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
