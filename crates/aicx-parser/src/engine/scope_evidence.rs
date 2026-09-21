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
    /// Every explicit workdir in the window normalizes to one identity.
    Consistent,
    /// More than one distinct repo identity — mixed/unattributed, no guessing.
    Conflict,
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
/// checkout are one scope). Unresolved paths stay distinct unless nested under
/// a resolved root from the same window. The caller keeps the `turn_context`
/// baseline on [`WindowScope::Baseline`], stamps the [`WindowScope::Consistent`]
/// identity path on the whole window, and marks the window mixed/unattributed
/// on [`WindowScope::Conflict`].
pub fn effective_window_scope(workdirs: &[String]) -> (WindowScope, Option<String>) {
    let identities: Vec<WorkdirIdentity> =
        workdirs.iter().map(|raw| normalize_workdir(raw)).collect();
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
    match distinct.len() {
        0 => (WindowScope::Baseline, None),
        1 => (
            WindowScope::Consistent,
            distinct.first().map(WorkdirIdentity::scope_path),
        ),
        _ => (WindowScope::Conflict, None),
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
        let (scope, path) = effective_window_scope(&workdirs);
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
        let (scope, path) = effective_window_scope(&workdirs);
        assert_eq!(scope, WindowScope::Conflict);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unresolved_paths_stay_conservatively_distinct() {
        let missing_a = "/definitely/missing/aicx-scope-a";
        let missing_b = "/definitely/missing/aicx-scope-b";
        let (scope, _) = effective_window_scope(&[missing_a.to_string(), missing_b.to_string()]);
        assert_eq!(scope, WindowScope::Conflict);
        let (single, path) = effective_window_scope(std::slice::from_ref(&missing_a.to_string()));
        assert_eq!(single, WindowScope::Consistent);
        assert_eq!(path.as_deref(), Some(missing_a));
    }

    #[test]
    fn no_workdirs_keeps_the_baseline() {
        let (scope, path) = effective_window_scope(&[]);
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);
    }
}
