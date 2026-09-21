//! Turn-level effective scope evidence (W2-R1 follow-up).
//!
//! A Codex session's declared cwd (`session_meta` / `turn_context`) is only the
//! turn baseline: agents run executable tool calls with an explicit `workdir`
//! that can point into a different repository without any `turn_context` drift.
//! This module extracts that stronger runtime evidence and reduces it to a
//! per-turn-window verdict at REPO-ROOT identity granularity — two subdirs of
//! one checkout are one scope, two real checkouts are a conflict.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::segmentation::discover_git_root;

/// Repo-level identity of one explicit tool-call working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkdirIdentity {
    /// Path resolves to a git checkout; the identity is the repo root.
    Resolved(PathBuf),
    /// Path does not exist locally or has no `.git` ancestor. Conservative:
    /// never guess a repo — each distinct unresolved path is its own identity.
    Unresolved(String),
}

impl WorkdirIdentity {
    /// Path representing this identity for frame stamping: the repo root when
    /// resolved, the original workdir otherwise.
    pub fn scope_path(&self) -> String {
        match self {
            Self::Resolved(root) => root.to_string_lossy().into_owned(),
            Self::Unresolved(path) => path.clone(),
        }
    }
}

/// Effective scope of one turn window, from its explicit workdir evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowScope {
    /// No explicit workdir evidence — keep the `turn_context` baseline.
    Baseline,
    /// Every explicit workdir in the window normalizes to one resolved repo
    /// identity — positive attribution at the repo root.
    Consistent,
    /// Two or more distinct resolved repo identities — proven divergence.
    Conflict,
    /// Workdir evidence exists but does not resolve to a git checkout
    /// (historical/deleted/foreign path). "I don't know" — never a positive
    /// attribution, and not proof of divergence either.
    Unattributed,
}

/// Extract an explicit `workdir` from an executable tool call payload.
///
/// Two real shapes exist in Codex rollouts: JSON `function_call.arguments`
/// (`{"cmd": "...", "workdir": "/abs/path"}`) and a JavaScript literal inside
/// `custom_tool_call.input` (`tools.exec_command({cmd:"...",workdir:"/abs"})`).
/// Only non-empty trimmed values count.
pub fn tool_call_workdir(payload: &Value) -> Option<String> {
    if let Some(arguments) = payload.get("arguments") {
        let parsed = match arguments {
            Value::Object(_) => Some(arguments.clone()),
            Value::String(raw) => serde_json::from_str::<Value>(raw).ok(),
            _ => None,
        };
        if let Some(value) = parsed.and_then(|body| {
            body.get("workdir")
                .and_then(Value::as_str)
                .map(str::to_owned)
        }) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    let input = payload.get("input").and_then(Value::as_str)?;
    workdir_in_js_literal(input)
}

/// `workdir` key inside a JS object literal: `workdir:"/p"`, `"workdir": "/p"`.
fn workdir_in_js_literal(input: &str) -> Option<String> {
    static WORKDIR_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = WORKDIR_RE
        .get_or_init(|| regex::Regex::new(r#""?workdir"?\s*:\s*"([^"]+)""#).expect("valid regex"));
    let value = re.captures(input)?.get(1)?.as_str().trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Normalize one explicit workdir to its repo-level identity.
///
/// Filesystem-only and bounded (ancestor walk to the first `.git`); no
/// subprocess and no remote lookup — the root path is identity enough.
pub fn normalize_workdir(path: &str) -> WorkdirIdentity {
    let candidate = Path::new(path);
    if let Some(root) = discover_git_root(candidate) {
        return WorkdirIdentity::Resolved(root);
    }
    WorkdirIdentity::Unresolved(path.to_string())
}

/// Reduce a window's explicit workdirs to one effective-scope verdict.
///
/// A resolved repo root absorbs every workdir nested under it (subdirs of one
/// checkout are one scope). An unresolvable workdir that matches the
/// `baseline` (same path or nested under it) is baseline evidence — the
/// window's `turn_context` already says the same thing, so nothing changes.
/// An unresolvable workdir pointing ELSEWHERE (historical or foreign-machine
/// path) is [`WindowScope::Unattributed`]: a durable "evidence exists but says
/// nothing" state — never a positive attribution, never proof of divergence,
/// and never eligible for bucket inheritance downstream. Two or more resolved
/// repo identities are [`WindowScope::Conflict`].
pub fn effective_window_scope(
    workdirs: &[String],
    baseline: Option<&str>,
) -> (WindowScope, Option<String>) {
    let baseline = baseline.map(str::trim).filter(|value| !value.is_empty());
    let matches_baseline = |path: &str| {
        let Some(baseline) = baseline else {
            return false;
        };
        let candidate = path.trim_end_matches(['/', '\\']);
        let base = baseline.trim_end_matches(['/', '\\']);
        candidate == base
            || candidate
                .strip_prefix(base)
                .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('\\'))
    };
    let identities: Vec<WorkdirIdentity> = workdirs
        .iter()
        .map(|raw| raw.as_str())
        .filter(|raw| !matches_baseline(raw))
        .map(normalize_workdir)
        .collect();
    let roots: Vec<PathBuf> = identities
        .iter()
        .filter_map(|identity| match identity {
            WorkdirIdentity::Resolved(root) => Some(root.clone()),
            WorkdirIdentity::Unresolved(_) => None,
        })
        .collect();
    let absorbed = |identity: &WorkdirIdentity| -> bool {
        let own_path = match identity {
            WorkdirIdentity::Resolved(root) => root.as_path(),
            WorkdirIdentity::Unresolved(path) => Path::new(path),
        };
        roots
            .iter()
            .any(|root| root != own_path && own_path.starts_with(root))
    };
    let mut distinct: Vec<WorkdirIdentity> = Vec::new();
    for identity in identities {
        if absorbed(&identity) {
            continue;
        }
        if !distinct.contains(&identity) {
            distinct.push(identity);
        }
    }
    let resolved_count = distinct
        .iter()
        .filter(|identity| matches!(identity, WorkdirIdentity::Resolved(_)))
        .count();
    if resolved_count >= 2 {
        return (WindowScope::Conflict, None);
    }
    if distinct
        .iter()
        .any(|identity| matches!(identity, WorkdirIdentity::Unresolved(_)))
    {
        return (WindowScope::Unattributed, None);
    }
    match distinct.len() {
        0 => (WindowScope::Baseline, None),
        1 => (
            WindowScope::Consistent,
            distinct.first().map(WorkdirIdentity::scope_path),
        ),
        _ => unreachable!("two distinct identities with fewer than two resolved"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workdir_from_json_arguments_string() {
        let payload: Value = serde_json::from_str(
            r#"{"type":"function_call","name":"shell","arguments":"{\"cmd\":\"cargo test\",\"workdir\":\"/repo/a\"}"}"#,
        )
        .expect("fixture payload");
        assert_eq!(tool_call_workdir(&payload).as_deref(), Some("/repo/a"));
    }

    #[test]
    fn workdir_from_js_literal_input() {
        let payload: Value = serde_json::from_str(
            r#"{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({cmd:\"npm test\",\"workdir\":\"/repo/b\",\"yield_time_ms\":30000});"}"#,
        )
        .expect("fixture payload");
        assert_eq!(tool_call_workdir(&payload).as_deref(), Some("/repo/b"));
    }

    #[test]
    fn no_workdir_anywhere_is_none() {
        let payload: Value = serde_json::from_str(
            r#"{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({cmd:\"pwd\"});"}"#,
        )
        .expect("fixture payload");
        assert_eq!(tool_call_workdir(&payload), None);
    }

    #[test]
    fn same_repo_subdirs_are_one_scope() {
        let root =
            std::env::temp_dir().join(format!("aicx-scope-same-repo-{}", std::process::id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        std::fs::create_dir_all(repo.join("packages/a")).expect("pkg a");
        std::fs::create_dir_all(repo.join("packages/b")).expect("pkg b");
        let workdirs = vec![
            repo.join("packages/a").to_string_lossy().into_owned(),
            repo.join("packages/b").to_string_lossy().into_owned(),
        ];
        let (scope, path) = effective_window_scope(&workdirs, None);
        assert_eq!(scope, WindowScope::Consistent);
        assert_eq!(path.as_deref(), Some(repo.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_real_repo_roots_are_a_conflict() {
        let root =
            std::env::temp_dir().join(format!("aicx-scope-two-repos-{}", std::process::id()));
        let repo_a = root.join("repo-a");
        let repo_b = root.join("repo-b");
        std::fs::create_dir_all(repo_a.join(".git")).expect("git dir a");
        std::fs::create_dir_all(repo_b.join(".git")).expect("git dir b");
        let workdirs = vec![
            repo_a.to_string_lossy().into_owned(),
            repo_b.to_string_lossy().into_owned(),
        ];
        let (scope, path) = effective_window_scope(&workdirs, None);
        assert_eq!(scope, WindowScope::Conflict);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unresolved_paths_are_unattributed_never_positive_never_conflict() {
        let missing_a = "/definitely/missing/aicx-scope-a";
        let missing_b = "/definitely/missing/aicx-scope-b";
        // One unresolved workdir: not a positive attribution (no scope path
        // stamped), and not proof of divergence either — just "unknown".
        let (single, path) =
            effective_window_scope(std::slice::from_ref(&missing_a.to_string()), None);
        assert_eq!(single, WindowScope::Unattributed);
        assert_eq!(path, None);
        // Two distinct unresolved paths: still unknown, not a proven conflict.
        let (scope, path) =
            effective_window_scope(&[missing_a.to_string(), missing_b.to_string()], None);
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
    }

    #[test]
    fn resolved_plus_unresolved_is_unattributed_not_conflict() {
        let root = std::env::temp_dir().join(format!("aicx-scope-mixed-{}", std::process::id()));
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        let workdirs = vec![
            repo.to_string_lossy().into_owned(),
            "/definitely/missing/aicx-scope-elsewhere".to_string(),
        ];
        let (scope, path) = effective_window_scope(&workdirs, None);
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_workdirs_keeps_the_baseline() {
        let (scope, path) = effective_window_scope(&[], None);
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);
    }

    #[test]
    fn unresolved_workdir_matching_baseline_is_baseline_evidence() {
        // The codescribe-golden shape: the explicit workdir IS the
        // turn_context cwd (or nests under it) but does not exist on this
        // machine. Nothing foreign happened — the window stays baseline.
        let (scope, path) = effective_window_scope(
            &["/Volumes/vc-workspace/vetcoders/codescribe".to_string()],
            Some("/Volumes/vc-workspace/vetcoders/codescribe"),
        );
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);
        let (nested, _) = effective_window_scope(
            &["/Volumes/vc-workspace/vetcoders/codescribe/site".to_string()],
            Some("/Volumes/vc-workspace/vetcoders/codescribe"),
        );
        assert_eq!(nested, WindowScope::Baseline);
    }

    #[test]
    fn unresolved_workdir_foreign_to_baseline_is_unattributed() {
        // The 60b7 shape under a missing checkout: turn_context says vista,
        // the explicit workdir points at a fleet-bus path that does not
        // resolve here. Durable do-not-inherit, never a positive vista stamp.
        let (scope, path) =
            effective_window_scope(&["/missing/fleet-bus".to_string()], Some("/present/vista"));
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
    }
}
