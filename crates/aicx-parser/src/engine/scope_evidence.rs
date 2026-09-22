//! Turn-level effective scope evidence (W2-R1 follow-up).
//!
//! A Codex session's declared cwd (`session_meta` / `turn_context`) is only the
//! turn baseline: agents run executable tool calls with an explicit `workdir`
//! that can point into a different repository without any `turn_context` drift.
//! This module extracts that stronger runtime evidence and reduces it to a
//! per-turn-window verdict at REPO-ROOT identity granularity — two subdirs of
//! one checkout are one scope, two real checkouts are a conflict.
//!
//! Repo identity — never a lexical path prefix — is the single membership
//! test in this module and in every downstream project filter. A nested
//! checkout or submodule lives lexically below its parent and is still a
//! different repository; deciding membership by string prefix is exactly the
//! cross-repo leak this module exists to close.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::segmentation::discover_git_root;

/// One piece of explicit workdir evidence observed inside a turn window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkdirEvidence {
    /// A readable explicit `workdir` from an executable tool call.
    Explicit(String),
    /// A tool-call envelope was observed but its payload could not be read
    /// (a record over the bounded reader's per-record cap). Evidence exists,
    /// its value does not. Fail-closed: the window can never be attributed
    /// from evidence it could not read.
    Opaque,
}

impl WorkdirEvidence {
    /// The explicit path, when this evidence carries one.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Explicit(path) => Some(path.as_str()),
            Self::Opaque => None,
        }
    }
}

/// Repo-level identity of one explicit tool-call working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkdirIdentity {
    /// Path resolves to a git checkout; the identity is the canonical repo
    /// root, so two spellings of one checkout (symlink, `/var` vs
    /// `/private/var`) are one identity rather than a phantom conflict.
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

    fn is_resolved(&self) -> bool {
        matches!(self, Self::Resolved(_))
    }
}

/// Effective scope of one turn window, from its explicit workdir evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowScope {
    /// No explicit workdir evidence, or all of it names the baseline checkout
    /// — keep the `turn_context` baseline.
    Baseline,
    /// Every explicit workdir in the window normalizes to one resolved repo
    /// identity — positive attribution at the repo root.
    Consistent,
    /// Two or more distinct resolved repo identities — proven divergence.
    Conflict,
    /// Workdir evidence exists but does not resolve to a git checkout
    /// (historical/deleted/foreign path), or could not be read at all.
    /// "I don't know" — never a positive attribution, and not proof of
    /// divergence either.
    Unattributed,
}

/// Every explicit `workdir` in an executable tool call payload, in order.
///
/// Two real shapes exist in Codex rollouts: JSON `function_call.arguments`
/// (`{"cmd": "...", "workdir": "/abs/path"}`) and a JavaScript literal inside
/// `custom_tool_call.input` (`tools.exec_command({cmd:"...",workdir:'/abs'})`).
///
/// ALL occurrences are returned, not the first: one `input` can orchestrate
/// several `tools.exec_command` calls, and reading only the first hides a
/// later hop into another repository behind a baseline-looking opener. It also
/// keeps a `workdir:`-shaped string inside a shell command from deciding the
/// window on its own — an extra reading makes the window unattributed or
/// conflicted, never confidently wrong.
pub fn tool_call_workdirs(payload: &Value) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut push = |value: String| {
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() && !found.contains(&trimmed) {
            found.push(trimmed);
        }
    };
    if let Some(arguments) = payload.get("arguments") {
        let parsed = match arguments {
            Value::Object(_) => Some(arguments.clone()),
            Value::String(raw) => serde_json::from_str::<Value>(raw).ok(),
            _ => None,
        };
        match parsed.as_ref().and_then(|body| body.get("workdir")) {
            Some(Value::String(workdir)) => push(workdir.clone()),
            // A JS literal can also arrive as the raw `arguments` string; scan
            // it rather than declaring no evidence because JSON parsing failed.
            _ => {
                if let Value::String(raw) = arguments {
                    for workdir in workdirs_in_literal(raw) {
                        push(workdir);
                    }
                }
            }
        }
    }
    if let Some(input) = payload.get("input").and_then(Value::as_str) {
        for workdir in workdirs_in_literal(input) {
            push(workdir);
        }
    }
    found
}

/// Every `workdir` key inside an object literal, quote style agnostic:
/// `workdir:"/p"`, `"workdir": "/p"`, `'workdir': '/p'`.
///
/// Codex writes `custom_tool_call.input` as model-authored JavaScript, so the
/// quote style is whatever the model emitted. Accepting only JSON-style double
/// quotes silently dropped real evidence and left the window on its baseline
/// project — the leak this module exists to close.
///
/// A value runs to the closing quote and may contain backslashes: a Windows
/// workdir is `C:\repo\crate`, and stopping the capture at the first backslash
/// would reduce it to the drive letter.
fn workdirs_in_literal(input: &str) -> Vec<String> {
    static WORKDIR_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = WORKDIR_RE.get_or_init(|| {
        regex::Regex::new(r#"["']?workdir["']?\s*:\s*["']([^"']+)"#).expect("valid regex")
    });
    re.captures_iter(input)
        .filter_map(|capture| capture.get(1))
        .map(|value| value.as_str().trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

/// Resolve one workdir string to a filesystem candidate.
///
/// A relative workdir (`.`, `packages/api`) belongs to the TURN's cwd, never to
/// wherever `aicx` happens to be running: resolving it against the process cwd
/// would let the caller's own checkout adopt a historical rollout's messages.
/// Without a baseline there is nothing to resolve it against, so it stays
/// unknown.
fn resolve_candidate(path: &str, baseline: Option<&str>) -> Option<PathBuf> {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Some(candidate.to_path_buf());
    }
    let base = Path::new(baseline?.trim());
    base.is_absolute().then(|| base.join(candidate))
}

/// Normalize one explicit workdir to its repo-level identity.
///
/// Filesystem-only and bounded (ancestor walk to the first `.git`); no
/// subprocess and no remote lookup — the canonical root path is identity
/// enough. A path that does not exist here resolves to nothing: an ancestor's
/// `.git` is not evidence about a directory that was never there, and treating
/// `/repo-b/deleted-fleet` as `/repo-b` would stamp positive scope on a
/// workdir the contract calls unattributable.
pub fn normalize_workdir(path: &str, baseline: Option<&str>) -> WorkdirIdentity {
    if let Some(candidate) = resolve_candidate(path, baseline)
        && candidate.exists()
        && let Some(root) = discover_git_root(&candidate)
    {
        // Canonical form, so one checkout reached through a symlink or a
        // `/var` vs `/private/var` spelling is one identity.
        return WorkdirIdentity::Resolved(std::fs::canonicalize(&root).unwrap_or(root));
    }
    WorkdirIdentity::Unresolved(path.to_string())
}

fn trim_path(path: &str) -> &str {
    let trimmed = path.trim().trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        path.trim()
    } else {
        trimmed
    }
}

/// Lexical containment — the fallback used ONLY where repo identity cannot be
/// established on this machine (neither path resolves to a checkout).
fn lexically_within(candidate: &str, scope_root: &str) -> bool {
    let candidate = trim_path(candidate);
    let scope_root = trim_path(scope_root);
    candidate == scope_root
        || candidate
            .strip_prefix(scope_root)
            .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('\\'))
}

/// Does `candidate` belong to the same checkout as `scope_root`?
///
/// The single membership predicate for scope decisions. Repo identity wins
/// over path shape: a nested checkout or submodule sits lexically below its
/// parent and is still a different repository, so a resolved candidate must
/// match the scope's resolved repo ROOT, not merely live under its path.
/// Lexical containment survives only where identity is genuinely unknowable —
/// a workdir that does not exist on this machine (historical rollouts,
/// foreign-machine paths), where the declared scope is the only evidence
/// there is.
pub fn workdir_within_scope(candidate: &str, scope_root: &str) -> bool {
    match normalize_workdir(candidate, Some(scope_root)) {
        // The candidate is a real checkout here: only root identity counts.
        // A nested checkout resolves to ITSELF, so it is never absorbed by
        // the enclosing path.
        WorkdirIdentity::Resolved(candidate_root) => match normalize_workdir(scope_root, None) {
            WorkdirIdentity::Resolved(scope) => candidate_root == scope,
            WorkdirIdentity::Unresolved(scope) => {
                trim_path(&candidate_root.to_string_lossy()) == trim_path(&scope)
            }
        },
        // Unknowable path: the declared scope is all the evidence there is.
        WorkdirIdentity::Unresolved(candidate) => lexically_within(&candidate, scope_root),
    }
}

/// Do these two paths resolve to two DIFFERENT checkouts on this machine?
///
/// Only true when both sides are proven, so callers can distinguish "provably
/// another repository" from "cannot tell". A downstream project filter needs
/// that distinction: its path-segment fallback would otherwise re-admit a
/// nested checkout whose path happens to spell the requested project.
pub fn distinct_repo_identity(candidate: &str, scope_root: &str) -> bool {
    match (
        normalize_workdir(candidate, Some(scope_root)),
        normalize_workdir(scope_root, None),
    ) {
        (WorkdirIdentity::Resolved(left), WorkdirIdentity::Resolved(right)) => left != right,
        _ => false,
    }
}

/// Reduce a window's explicit workdir evidence to one effective-scope verdict.
///
/// Evidence naming the baseline checkout counts as an observed identity, not
/// as noise to drop: a window that ran tools in BOTH the baseline and another
/// checkout has two proven identities and must not be re-scoped wholesale to
/// the foreign one. Unresolvable paths nested under a proven root are absorbed
/// by it, but a resolved root never absorbs another resolved root — a nested
/// checkout is a repository of its own.
///
/// An unresolvable workdir pointing away from the baseline (historical or
/// foreign-machine path), and evidence that could not be read at all
/// ([`WorkdirEvidence::Opaque`]), are [`WindowScope::Unattributed`]: a durable
/// "evidence exists but says nothing" state — never a positive attribution,
/// never proof of divergence, and never eligible for bucket inheritance
/// downstream.
pub fn effective_window_scope(
    workdirs: &[WorkdirEvidence],
    baseline: Option<&str>,
) -> (WindowScope, Option<String>) {
    let baseline = baseline.map(str::trim).filter(|value| !value.is_empty());
    // Unreadable evidence can never be proven to agree with anything else in
    // the window, so it decides the verdict on its own.
    if workdirs.contains(&WorkdirEvidence::Opaque) {
        return (WindowScope::Unattributed, None);
    }

    let mut saw_baseline = false;
    let mut foreign: Vec<WorkdirIdentity> = Vec::new();
    for raw in workdirs.iter().filter_map(WorkdirEvidence::path) {
        if baseline.is_some_and(|base| workdir_within_scope(raw, base)) {
            saw_baseline = true;
            continue;
        }
        let identity = normalize_workdir(raw, baseline);
        if !foreign.contains(&identity) {
            foreign.push(identity);
        }
    }

    // A proven root absorbs unresolvable paths beneath it (one checkout, one
    // scope). It never absorbs another proven root.
    let roots: Vec<PathBuf> = foreign
        .iter()
        .filter_map(|identity| match identity {
            WorkdirIdentity::Resolved(root) => Some(root.clone()),
            WorkdirIdentity::Unresolved(_) => None,
        })
        .collect();
    foreign.retain(|identity| match identity {
        WorkdirIdentity::Resolved(_) => true,
        WorkdirIdentity::Unresolved(path) => {
            !roots.iter().any(|root| Path::new(path).starts_with(root))
        }
    });

    let resolved: Vec<&WorkdirIdentity> = foreign
        .iter()
        .filter(|identity| identity.is_resolved())
        .collect();
    if resolved.len() >= 2 {
        return (WindowScope::Conflict, None);
    }
    if resolved.len() == 1 && saw_baseline {
        // The window ran in the baseline AND in another checkout. Proven
        // divergence when the baseline itself is a checkout here; otherwise
        // the second identity is unproven, which is "unknown", not a verdict.
        let baseline_resolved = baseline
            .map(|base| normalize_workdir(base, None).is_resolved())
            .unwrap_or(false);
        return if baseline_resolved {
            (WindowScope::Conflict, None)
        } else {
            (WindowScope::Unattributed, None)
        };
    }
    if foreign.iter().any(|identity| !identity.is_resolved()) {
        return (WindowScope::Unattributed, None);
    }
    match foreign.first() {
        None => (WindowScope::Baseline, None),
        Some(identity) => (WindowScope::Consistent, Some(identity.scope_path())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn explicit(paths: &[&str]) -> Vec<WorkdirEvidence> {
        paths
            .iter()
            .map(|path| WorkdirEvidence::Explicit((*path).to_string()))
            .collect()
    }

    /// Identities are canonical, so a scratch root under a symlinked temp dir
    /// compares equal to what `normalize_workdir` returns.
    fn canonical(path: &Path) -> String {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    }

    fn scratch(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aicx-scope-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn first_workdir(payload: &Value) -> Option<String> {
        tool_call_workdirs(payload).into_iter().next()
    }

    #[test]
    fn workdir_from_json_arguments_string() {
        let payload: Value = serde_json::from_str(
            r#"{"type":"function_call","name":"shell","arguments":"{\"cmd\":\"cargo test\",\"workdir\":\"/repo/a\"}"}"#,
        )
        .expect("fixture payload");
        assert_eq!(first_workdir(&payload).as_deref(), Some("/repo/a"));
    }

    #[test]
    fn workdir_from_js_literal_input() {
        let payload: Value = serde_json::from_str(
            r#"{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({cmd:\"npm test\",\"workdir\":\"/repo/b\",\"yield_time_ms\":30000});"}"#,
        )
        .expect("fixture payload");
        assert_eq!(first_workdir(&payload).as_deref(), Some("/repo/b"));
    }

    #[test]
    fn workdir_from_single_quoted_js_literal() {
        // Codex writes `custom_tool_call.input` as model-authored JavaScript:
        // the quote style is whatever the model emitted. A double-quote-only
        // reader silently dropped this evidence and kept the baseline project.
        let payload: Value = serde_json::from_str(
            r#"{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({cmd: 'npm test', workdir: '/foreign/repo'});"}"#,
        )
        .expect("fixture payload");
        assert_eq!(first_workdir(&payload).as_deref(), Some("/foreign/repo"));

        let mixed: Value = serde_json::from_str(
            r#"{"type":"custom_tool_call","name":"exec","input":"tools.exec_command({'workdir': \"/foreign/other\"})"}"#,
        )
        .expect("fixture payload");
        assert_eq!(first_workdir(&mixed).as_deref(), Some("/foreign/other"));
    }

    /// A Windows workdir is `C:\repo\crate`. A capture that stops at the first
    /// backslash reduces it to `C:` — no evidence, and the window silently
    /// keeps a baseline it never verified.
    #[test]
    fn workdir_with_backslashes_survives_the_literal_scanner() {
        let payload: Value = serde_json::json!({
            "type": "custom_tool_call",
            "name": "exec",
            "input": r#"tools.exec_command({cmd:"cargo test",workdir:"C:\Users\runner\fleet-bus"})"#,
        });
        assert_eq!(
            first_workdir(&payload).as_deref(),
            Some(r"C:\Users\runner\fleet-bus")
        );
    }

    /// One `input` can orchestrate several `exec_command` calls. Reading only
    /// the first hides a later hop into another repository behind a
    /// baseline-looking opener.
    #[test]
    fn every_workdir_in_an_orchestrated_tool_call_is_collected() {
        let payload: Value = serde_json::json!({
            "type": "custom_tool_call",
            "name": "exec",
            "input": "await tools.exec_command({cmd:\"ls\",workdir:\"/repo/vista\"});\n\
                      await tools.exec_command({cmd:\"ls\",workdir:'/repo/fleet-bus'});",
        });
        assert_eq!(
            tool_call_workdirs(&payload),
            vec!["/repo/vista".to_string(), "/repo/fleet-bus".to_string()],
            "a later hop must not hide behind the first call"
        );
    }

    #[test]
    fn no_workdir_anywhere_is_none() {
        let payload: Value = serde_json::from_str(
            r#"{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({cmd:\"pwd\"});"}"#,
        )
        .expect("fixture payload");
        assert!(tool_call_workdirs(&payload).is_empty());
    }

    #[test]
    fn same_repo_subdirs_are_one_scope() {
        let root = scratch("same-repo");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        std::fs::create_dir_all(repo.join("packages/a")).expect("pkg a");
        std::fs::create_dir_all(repo.join("packages/b")).expect("pkg b");
        let workdirs = explicit(&[
            repo.join("packages/a").to_string_lossy().as_ref(),
            repo.join("packages/b").to_string_lossy().as_ref(),
        ]);
        let (scope, path) = effective_window_scope(&workdirs, None);
        assert_eq!(scope, WindowScope::Consistent);
        assert_eq!(path.as_deref(), Some(canonical(&repo).as_str()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn two_real_repo_roots_are_a_conflict() {
        let root = scratch("two-repos");
        let repo_a = root.join("repo-a");
        let repo_b = root.join("repo-b");
        std::fs::create_dir_all(repo_a.join(".git")).expect("git dir a");
        std::fs::create_dir_all(repo_b.join(".git")).expect("git dir b");
        let workdirs = explicit(&[
            repo_a.to_string_lossy().as_ref(),
            repo_b.to_string_lossy().as_ref(),
        ]);
        let (scope, path) = effective_window_scope(&workdirs, None);
        assert_eq!(scope, WindowScope::Conflict);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nested_checkout_under_the_baseline_is_not_baseline_scope() {
        // Submodule / vendored checkout: lexically below the parent, its own
        // repository. A path-prefix baseline test swallowed it and kept the
        // window in the parent's project bucket.
        let root = scratch("nested-checkout");
        let parent = root.join("vista");
        let nested = parent.join("vendor/fleet-bus");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::create_dir_all(nested.join(".git")).expect("nested git dir");
        std::fs::create_dir_all(parent.join("crates/core")).expect("parent subdir");
        let baseline = parent.to_string_lossy().into_owned();

        assert!(!workdir_within_scope(
            nested.to_string_lossy().as_ref(),
            &baseline
        ));
        assert!(workdir_within_scope(
            parent.join("crates/core").to_string_lossy().as_ref(),
            &baseline
        ));
        assert!(distinct_repo_identity(
            nested.to_string_lossy().as_ref(),
            &baseline
        ));

        let (scope, path) = effective_window_scope(
            &explicit(&[nested.to_string_lossy().as_ref()]),
            Some(&baseline),
        );
        assert_eq!(
            scope,
            WindowScope::Consistent,
            "the nested checkout is its own repo identity, not the parent's"
        );
        assert_eq!(path.as_deref(), Some(canonical(&nested).as_str()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two proven roots in one window are a conflict even when one of them is
    /// the enclosing checkout: a resolved root never absorbs another resolved
    /// root by lexical containment.
    #[test]
    fn parent_and_nested_checkout_together_are_a_conflict() {
        let root = scratch("parent-plus-nested");
        let parent = root.join("vista");
        let nested = parent.join("vendor/fleet-bus");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::create_dir_all(nested.join(".git")).expect("nested git dir");

        // No baseline: both are ordinary evidence.
        let (scope, path) = effective_window_scope(
            &explicit(&[
                parent.to_string_lossy().as_ref(),
                nested.to_string_lossy().as_ref(),
            ]),
            None,
        );
        assert_eq!(scope, WindowScope::Conflict, "two proven roots, one window");
        assert_eq!(path, None);

        // With the parent as the declared baseline, the window still ran in
        // two checkouts and must not be re-scoped wholesale to either.
        let (scoped, scoped_path) = effective_window_scope(
            &explicit(&[
                parent.to_string_lossy().as_ref(),
                nested.to_string_lossy().as_ref(),
            ]),
            Some(parent.to_string_lossy().as_ref()),
        );
        assert_eq!(scoped, WindowScope::Conflict);
        assert_eq!(scoped_path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An explicit baseline workdir is observed evidence, not noise: dropping
    /// it before reduction let one foreign root re-scope the whole window,
    /// including the turns that really did run in the baseline checkout.
    #[test]
    fn baseline_plus_foreign_checkout_is_a_conflict_not_a_rescope() {
        let root = scratch("baseline-plus-foreign");
        let vista = root.join("vista");
        let fleet = root.join("fleet-bus");
        std::fs::create_dir_all(vista.join(".git")).expect("vista git dir");
        std::fs::create_dir_all(fleet.join(".git")).expect("fleet git dir");

        let (scope, path) = effective_window_scope(
            &explicit(&[
                vista.to_string_lossy().as_ref(),
                fleet.to_string_lossy().as_ref(),
            ]),
            Some(vista.to_string_lossy().as_ref()),
        );
        assert_eq!(scope, WindowScope::Conflict);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A relative workdir belongs to the turn's cwd. Resolving it against the
    /// process cwd would let whichever checkout `aicx` runs from adopt a
    /// historical rollout's messages.
    #[test]
    fn relative_workdirs_resolve_against_the_baseline_not_the_process() {
        let root = scratch("relative-workdir");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        std::fs::create_dir_all(repo.join("packages/api")).expect("pkg");
        let baseline = repo.to_string_lossy().into_owned();

        assert_eq!(
            normalize_workdir("packages/api", Some(&baseline)),
            WorkdirIdentity::Resolved(
                std::fs::canonicalize(&repo).unwrap_or_else(|_| repo.clone())
            )
        );
        // Without a baseline there is nothing to resolve against — never the
        // process cwd.
        assert_eq!(
            normalize_workdir("packages/api", None),
            WorkdirIdentity::Unresolved("packages/api".to_string())
        );
        let (scope, _) = effective_window_scope(&explicit(&["packages/api"]), Some(&baseline));
        assert_eq!(scope, WindowScope::Baseline);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An ancestor's `.git` says nothing about a directory that is not there.
    /// Treating `/repo/deleted-subdir` as `/repo` stamps positive scope on a
    /// workdir the contract calls unattributable.
    #[test]
    fn missing_path_under_a_real_checkout_stays_unresolved() {
        let root = scratch("missing-under-repo");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        let missing = repo.join("deleted-fleet");

        assert_eq!(
            normalize_workdir(missing.to_string_lossy().as_ref(), None),
            WorkdirIdentity::Unresolved(missing.to_string_lossy().into_owned())
        );
        let (scope, path) =
            effective_window_scope(&explicit(&[missing.to_string_lossy().as_ref()]), None);
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unresolved_paths_are_unattributed_never_positive_never_conflict() {
        let missing_a = "/definitely/missing/aicx-scope-a";
        let missing_b = "/definitely/missing/aicx-scope-b";
        // One unresolved workdir: not a positive attribution (no scope path
        // stamped), and not proof of divergence either — just "unknown".
        let (single, path) = effective_window_scope(&explicit(&[missing_a]), None);
        assert_eq!(single, WindowScope::Unattributed);
        assert_eq!(path, None);
        // Two distinct unresolved paths: still unknown, not a proven conflict.
        let (scope, path) = effective_window_scope(&explicit(&[missing_a, missing_b]), None);
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
    }

    #[test]
    fn resolved_plus_unresolved_is_unattributed_not_conflict() {
        let root = scratch("mixed");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        let workdirs = explicit(&[
            repo.to_string_lossy().as_ref(),
            "/definitely/missing/aicx-scope-elsewhere",
        ]);
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
    fn unreadable_tool_call_evidence_fails_closed_to_unattributed() {
        // The bounded reader drops records over its per-record cap. A tool
        // call whose payload it could not read is still evidence that the
        // window MIGHT have moved: it must never keep the baseline.
        let (scope, path) =
            effective_window_scope(&[WorkdirEvidence::Opaque], Some("/present/vista"));
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);

        let root = scratch("opaque-plus-resolved");
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git dir");
        let mut workdirs = explicit(&[repo.to_string_lossy().as_ref()]);
        workdirs.push(WorkdirEvidence::Opaque);
        // Even a window that otherwise looks consistent cannot be proven so
        // while one of its tool calls is unreadable.
        let (scope, path) = effective_window_scope(&workdirs, None);
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unresolved_workdir_matching_baseline_is_baseline_evidence() {
        // The codescribe-golden shape: the explicit workdir IS the
        // turn_context cwd (or nests under it) but does not exist on this
        // machine. Nothing foreign happened — the window stays baseline.
        let (scope, path) = effective_window_scope(
            &explicit(&["/Volumes/vc-workspace/vetcoders/codescribe"]),
            Some("/Volumes/vc-workspace/vetcoders/codescribe"),
        );
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);
        let (nested, _) = effective_window_scope(
            &explicit(&["/Volumes/vc-workspace/vetcoders/codescribe/site"]),
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
            effective_window_scope(&explicit(&["/missing/fleet-bus"]), Some("/present/vista"));
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);
    }
}
