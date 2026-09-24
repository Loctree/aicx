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

/// Resolve one workdir string to an absolute spelling, lexically normalized.
///
/// A relative workdir (`.`, `packages/api`) belongs to the TURN's cwd, never to
/// wherever `aicx` happens to be running: resolving it against the process cwd
/// would let the caller's own checkout adopt a historical rollout's messages.
/// Without a baseline there is nothing to resolve it against, so it stays
/// unknown.
///
/// Absoluteness is a property of the RECORDED spelling, not of the host doing
/// the parsing: a Windows rollout replayed on macOS still has `C:\repo` as an
/// absolute baseline. Asking `Path::is_absolute` made every relative workdir
/// of such a rollout unresolvable, and the window unattributed.
fn resolve_candidate(path: &str, baseline: Option<&str>) -> Option<String> {
    let path = path.trim();
    if absolute_anywhere(path) {
        return Some(lexically_normalized(path));
    }
    let base = baseline?.trim();
    if !absolute_anywhere(base) {
        return None;
    }
    let separator = separator_of(base);
    Some(lexically_normalized(&format!(
        "{}{separator}{path}",
        base.trim_end_matches(['/', '\\'])
    )))
}

/// Is `path` absolute on ANY platform a rollout can come from? `/unix`,
/// `C:\` or `C:/` drive roots, and `\`-rooted or UNC (`\\server\share`)
/// Windows paths. A drive-relative `C:repo` is not.
fn absolute_anywhere(path: &str) -> bool {
    path.starts_with(['/', '\\']) || windows_drive_root(path)
}

fn windows_drive_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

/// Is this a Windows spelling, where `\` is a separator rather than a legal
/// filename character?
fn windows_shaped(path: &str) -> bool {
    path.starts_with('\\')
        || (path.len() >= 2
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':')
}

/// The separator a path is spelled with, so a join or a rewrite keeps it.
fn separator_of(path: &str) -> char {
    if windows_shaped(path) && (path.contains('\\') || !path.contains('/')) {
        '\\'
    } else {
        '/'
    }
}

/// Only a path absolute on THIS host may be probed on its filesystem. Any
/// other spelling — a Windows path on Unix, a Unix path on Windows — would be
/// resolved against the process cwd, which is exactly the leak the relative
/// case refuses.
fn host_path(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    path.is_absolute().then_some(path)
}

/// Does the readable head of an over-cap record look like a tool-call
/// envelope? The record is truncated (never valid JSON), so this reads the
/// visible prefix only: the envelope and payload discriminators are written
/// before the oversized `arguments`/`input` body, so they survive the cap.
pub fn truncated_record_is_tool_call(prefix: &str) -> bool {
    // Key order inside a record is not a contract, so this scans the whole
    // visible prefix rather than a fixed head: the discriminator can sit
    // after a megabyte of `arguments`. Over-cap records are rare and their
    // bytes are already in hand.
    let discriminators = record_type_discriminators(prefix);
    // `function_call_output` / `mcp_tool_call_end` are RESULTS, not calls:
    // they carry no workdir, so an oversized one is not lost evidence. Exact
    // equality against the parsed value keeps them out.
    if discriminators.iter().any(|value| {
        matches!(
            value.as_str(),
            "function_call" | "custom_tool_call" | "tool_call" | "mcp_tool_call"
        )
    }) {
        return true;
    }
    // Otherwise the question is whether the PAYLOAD discriminator was
    // readable at all. Key order inside a record is not a contract: an
    // envelope whose own `type` is visible can still be truncated before its
    // payload type, hiding a tool call behind a megabyte of `arguments`.
    // Seeing one discriminator (the envelope's) or none is not evidence that
    // nothing was lost, so it fails closed.
    discriminators.len() < 2
}

/// Values of every `"type"` key that can be a RECORD discriminator in the
/// readable head of a truncated record, in order.
///
/// A truncated record is never valid JSON, so this is a byte scanner rather
/// than a parse. Two things make it structural rather than a substring count:
///
/// * it tracks JSON string state, so `"type":` written inside an argument
///   body is text, not a key; and
/// * it tracks nesting depth and keeps only the envelope (depth 1) and its
///   payload (depth 2), which is where a Codex record's discriminators live.
///
/// Both matter for the same reason: the threshold below decides whether an
/// unreadable record is allowed to keep its window attributed. A count that
/// payload CONTENT can raise — an `arguments` object carrying its own
/// `"type"` field — hands that decision to the very bytes we could not read.
fn record_type_discriminators(prefix: &str) -> Vec<String> {
    /// Deepest nesting level at which a `"type"` key is still the record's
    /// own discriminator: 1 is the envelope, 2 its `payload`.
    const DISCRIMINATOR_DEPTH: usize = 2;

    let bytes = prefix.as_bytes();
    // Reads the string token starting at `bytes[at] == b'"'`, returning its
    // contents and the index just past the closing quote. An unterminated
    // string (the truncation itself) ends the scan.
    let read_string = |at: usize| -> Option<(String, usize)> {
        let mut out = String::new();
        let mut cursor = at + 1;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'\\' => cursor += 2,
                b'"' => return Some((out, cursor + 1)),
                byte => {
                    out.push(byte as char);
                    cursor += 1;
                }
            }
        }
        None
    };

    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut idx = 0usize;
    while idx < bytes.len() {
        match bytes[idx] {
            b'{' | b'[' => {
                depth += 1;
                idx += 1;
                continue;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                idx += 1;
                continue;
            }
            b'"' => {}
            _ => {
                idx += 1;
                continue;
            }
        }
        let Some((token, after)) = read_string(idx) else {
            break;
        };
        idx = after;
        if token != "type" || depth > DISCRIMINATOR_DEPTH {
            continue;
        }
        // A `"type"` token is only a discriminator when it is used as a KEY.
        let mut cursor = idx;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() || bytes[cursor] != b':' {
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        // A discriminator truncated before its value is still a discriminator
        // that was there: counting it keeps the fail-closed threshold honest.
        match bytes.get(cursor) {
            Some(b'"') => match read_string(cursor) {
                Some((value, after)) => {
                    found.push(value);
                    idx = after;
                }
                None => {
                    found.push(String::new());
                    break;
                }
            },
            _ => found.push(String::new()),
        }
    }
    found
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
    let Some(candidate) = resolve_candidate(path, baseline) else {
        // Nothing to resolve a bare relative token against; it stays as-is.
        return WorkdirIdentity::Unresolved(trim_path(path).to_string());
    };
    if let Some(on_host) = host_path(&candidate)
        && on_host.exists()
        && let Some(root) = discover_git_root(on_host)
    {
        // Canonical form, so one checkout reached through a symlink or a
        // `/var` vs `/private/var` spelling is one identity.
        return WorkdirIdentity::Resolved(std::fs::canonicalize(&root).unwrap_or(root));
    }
    // Unresolvable, but not unknown: a replayed session whose checkout is gone
    // from this machine still told us `.` or `packages/api` RELATIVE TO its
    // baseline. Discarding that join and keeping the bare token would compare
    // `.` against `/old/repo` and throw away turns that are plainly in scope.
    WorkdirIdentity::Unresolved(candidate)
}

/// The workdir as the rollout RECORDED it, made comparable without asking the
/// filesystem anything: a relative workdir is joined onto the window's
/// baseline, `.`/`..` are collapsed, trailing separators dropped.
///
/// This is the host-independent form of the evidence. [`normalize_workdir`]
/// answers "which checkout is this ON THIS HOST", which changes when a
/// nested checkout appears; this answers "which path did the session name",
/// which never does. Privacy filters and cache identities need the latter:
/// both must see the raw evidence, not what survived one host's reduction.
pub fn recorded_workdir(path: &str, baseline: Option<&str>) -> String {
    match resolve_candidate(path, baseline) {
        Some(candidate) => trim_path(&candidate).to_string(),
        None => trim_path(path).to_string(),
    }
}

/// Collapse `.` and `..` without touching the filesystem, in the path's own
/// spelling.
///
/// The lexical comparison below is the last resort for paths that do not exist
/// here, so it must at least compare like with like: `/old/repo` joined with
/// `.` is `/old/repo`, not `/old/repo/.`. It works on the string rather than on
/// `Path` components because components are the HOST's grammar: on Unix a
/// Windows path is one opaque component, so `C:\repo\..\other` never
/// collapsed. `\` separates only in a Windows spelling; in a Unix path it is a
/// legal filename character and stays one.
fn lexically_normalized(path: &str) -> String {
    let path = path.trim();
    let windows = windows_shaped(path);
    let separator = separator_of(path);
    let is_separator = |c: char| c == '/' || (windows && c == '\\');
    let root_len = if windows_drive_root(path) {
        3
    } else if windows && (path.starts_with("\\\\") || path.starts_with("//")) {
        2
    } else if path.starts_with(is_separator) {
        1
    } else if windows {
        // Drive-relative `C:repo`: the drive is the only root there is.
        2
    } else {
        0
    };
    let (root, rest) = path.split_at(root_len);
    let root: String = root
        .chars()
        .map(|c| if is_separator(c) { separator } else { c })
        .collect();
    let mut parts: Vec<&str> = Vec::new();
    for part in rest.split(is_separator) {
        match part {
            "" | "." => {}
            ".." => match parts.last() {
                Some(last) if *last != ".." => {
                    parts.pop();
                }
                // Above a root there is nothing to climb to; a relative path
                // keeps the `..` it cannot resolve.
                _ if root.is_empty() => parts.push(".."),
                _ => {}
            },
            part => parts.push(part),
        }
    }
    let body = parts.join(separator.to_string().as_str());
    if root.is_empty() && body.is_empty() {
        ".".to_owned()
    } else {
        format!("{root}{body}")
    }
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
///
/// Both sides are compared lexically normalized, so a scope written with a
/// trailing `.` or `..` still contains what it names.
fn lexically_within(candidate: &str, scope_root: &str) -> bool {
    let candidate = lexically_normalized(candidate);
    let scope = lexically_normalized(scope_root);
    let windows = windows_shaped(&scope);
    let candidate = trim_path(&candidate);
    let scope = trim_path(&scope);
    candidate == scope
        || candidate
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with('/') || (windows && rest.starts_with('\\')))
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
        // Unknowable path. The declared scope is the only evidence there is,
        // but it is evidence ONLY where the scope root is equally unknowable.
        // A scope root that resolves here cannot adopt a path it cannot prove:
        // a removed nested checkout or submodule sits lexically below its
        // parent and is still a different repository, so lexical containment
        // would re-open exactly the leak identity checking exists to close.
        WorkdirIdentity::Unresolved(candidate) => match normalize_workdir(scope_root, None) {
            WorkdirIdentity::Resolved(_) => false,
            WorkdirIdentity::Unresolved(scope) => lexically_within(&candidate, &scope),
        },
    }
}

/// Is `candidate` plausibly INSIDE `scope_root`, for the purpose of counting
/// how many repository identities a turn window touched?
///
/// Deliberately more tolerant than [`workdir_within_scope`], because the two
/// answer different questions. Membership asks "may this frame be served as
/// project P?", where an unprovable claim must fail closed. Reduction asks "is
/// this a SECOND repository?", where an unprovable claim must fail OPEN: a
/// window judged [`WindowScope::Unattributed`] has its frames dropped outright
/// by the project filter, so calling every vanished path a second identity
/// silently deletes ordinary operator evidence — a deleted `target/`, a
/// removed temp dir, a worktree that has been cleaned up.
///
/// The one case where a missing path IS provably its own repository is a
/// declared submodule: the parent's `.gitmodules` still names it after the
/// working tree is gone. That is read here, bounded and filesystem-only, so
/// the common case keeps its evidence and the provable case keeps its
/// identity. A bare nested checkout that was deleted leaves no such trace and
/// is genuinely indistinguishable from a deleted directory; it is absorbed,
/// and that trade is deliberate.
fn plausibly_within_scope(candidate: &str, scope_root: &str) -> bool {
    match normalize_workdir(candidate, Some(scope_root)) {
        // Provable on both sides: identity decides, exactly as for membership.
        WorkdirIdentity::Resolved(_) => workdir_within_scope(candidate, scope_root),
        WorkdirIdentity::Unresolved(path) => {
            if !lexically_within(&path, trim_path(scope_root)) {
                return false;
            }
            !declared_submodule(scope_root, &path)
        }
    }
}

/// The directory whose `.gitmodules` declares the submodules around `scope`.
///
/// `.gitmodules` lives at the CHECKOUT root and spells its paths relative to
/// it, while a scope is only a working directory — frequently a subdirectory.
/// Reading it where the session happened to stand would miss every
/// declaration and silently absorb the submodule into its parent.
/// `discover_git_root` walks ancestors without canonicalizing, so the root it
/// returns stays comparable with a candidate spelled the same way.
fn submodule_declarations_root(scope: &Path) -> PathBuf {
    discover_git_root(scope).unwrap_or_else(|| scope.to_path_buf())
}

/// Everything the scope verdicts in this module read from THIS host's
/// filesystem about one recorded path, as bytes for a cache key.
///
/// A verdict is a pure function of the recorded evidence plus two reads: how
/// the path resolves ([`normalize_workdir`]: does it exist, which `.git`
/// ancestor is nearest) and which submodules its checkout declares
/// ([`declared_submodule`] reads `.gitmodules` at that checkout root). A cache
/// keyed on source bytes alone therefore serves stale verdicts after a nested
/// checkout appears or a `.gitmodules` is edited, although no source byte
/// moved. Hashing this for every path a session recorded makes the layout
/// part of the cache identity. It lives beside the reads it mirrors, so a
/// verdict that starts reading something new has one place to declare it.
pub fn scope_layout_evidence(path: &str) -> Vec<u8> {
    let mut evidence = Vec::new();
    match normalize_workdir(path, None) {
        WorkdirIdentity::Resolved(root) => {
            evidence.extend_from_slice(b"resolved\0");
            evidence.extend_from_slice(root.to_string_lossy().as_bytes());
        }
        WorkdirIdentity::Unresolved(candidate) => {
            evidence.extend_from_slice(b"unresolved\0");
            evidence.extend_from_slice(candidate.as_bytes());
        }
    }
    evidence.push(0);
    // Only a path spelled for this host has a `.gitmodules` to read, which is
    // exactly the gate `declared_submodule` applies.
    if let Some(scope) = host_path(trim_path(path)) {
        match std::fs::read(submodule_declarations_root(scope).join(".gitmodules")) {
            Ok(body) => {
                evidence.extend_from_slice(b"gitmodules\0");
                evidence.extend_from_slice(&(body.len() as u64).to_le_bytes());
                evidence.extend_from_slice(&body);
            }
            Err(_) => evidence.extend_from_slice(b"no-gitmodules\0"),
        }
    }
    evidence
}

/// Is `path` a submodule the checkout at `scope_root` declares?
///
/// Bounded and offline: one `.gitmodules` read, no subprocess, no network. A
/// missing or unreadable file simply means "not declared".
fn declared_submodule(scope_root: &str, path: &str) -> bool {
    // Both sides stay in the spelling the caller used: the candidate could not
    // be canonicalized (it does not exist), so canonicalizing only the root
    // would leave the two incomparable on any host where the scope path runs
    // through a symlink.
    let Some(scope) = host_path(trim_path(scope_root)) else {
        // A scope spelled for another platform has no `.gitmodules` on this
        // host; reading one relative to the process cwd would be a guess.
        return false;
    };
    let root = submodule_declarations_root(scope);
    let Ok(raw) = std::fs::read_to_string(root.join(".gitmodules")) else {
        return false;
    };
    let root = root.as_path();
    let relative = match Path::new(trim_path(path)).strip_prefix(root) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => return false,
    };
    let relative = trim_path(&relative);
    gitmodules_paths(&raw)
        .iter()
        .map(|declared| trim_path(declared))
        .filter(|declared| !declared.is_empty())
        // A submodule's DESCENDANTS are the submodule's repository too. A
        // vanished `vendor/fleet-bus/src` is still inside the declared
        // `vendor/fleet-bus`, so exact equality alone would let the parent
        // checkout absorb it. The boundary is a path component, never a
        // character prefix: `vendor/fleet-bus-old` is a different directory.
        .any(|declared| {
            relative == declared
                || relative
                    .strip_prefix(declared)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// Every `submodule.<name>.path` value in a `.gitmodules` body, read with
/// git-config syntax: section and key names are case-insensitive (`Path =`),
/// values may be double-quoted (`"vendor/fleet bus"`), `;` and `#` start a
/// comment outside quotes, `\` escapes a quote, a backslash or a newline
/// (continuation), and surrounding whitespace is dropped while inner
/// whitespace is kept.
///
/// A declaration this reader misses lets the parent absorb a vanished
/// submodule — the fail-open direction — so malformed input is read
/// leniently, never discarded: an unterminated quote runs to the end of the
/// line and an unknown escape keeps its character.
fn gitmodules_paths(raw: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut in_submodule = false;
    let mut chars = raw.chars().peekable();
    loop {
        while chars.next_if(|c| *c != '\n' && c.is_whitespace()).is_some() {}
        match chars.peek().copied() {
            None => break,
            Some('\n') => {
                chars.next();
            }
            Some('#' | ';') => skip_config_line(&mut chars),
            Some('[') => {
                chars.next();
                let mut header = String::new();
                while let Some(c) = chars.next_if(|c| *c != ']' && *c != '\n') {
                    header.push(c);
                }
                if chars.next_if_eq(&']').is_none() {
                    in_submodule = false;
                    continue;
                }
                // `[submodule "name"]`, or the legacy `[submodule.name]`.
                let section = header
                    .trim_start()
                    .split(|c: char| c.is_whitespace() || c == '"' || c == '.')
                    .next()
                    .unwrap_or_default();
                in_submodule = section.eq_ignore_ascii_case("submodule");
                // A key may follow the header on the same line.
            }
            Some(_) => {
                let mut key = String::new();
                while let Some(c) = chars.next_if(|c| c.is_ascii_alphanumeric() || *c == '-') {
                    key.push(c);
                }
                while chars.next_if(|c| *c == ' ' || *c == '\t').is_some() {}
                if key.is_empty() || chars.next_if_eq(&'=').is_none() {
                    // A bare boolean key, or noise: nothing to read.
                    skip_config_line(&mut chars);
                    continue;
                }
                let value = config_value(&mut chars);
                if in_submodule && key.eq_ignore_ascii_case("path") {
                    paths.push(value);
                }
            }
        }
    }
    paths
}

fn skip_config_line(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for c in chars.by_ref() {
        if c == '\n' {
            break;
        }
    }
}

/// One git-config value, consuming through the end of its (possibly
/// continued) line. Mirrors git's `parse_value`: leading whitespace skipped,
/// trailing unquoted whitespace trimmed, inner whitespace verbatim.
fn config_value(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut value = String::new();
    let mut quoted = false;
    let mut comment = false;
    // Length of `value` before a run of unquoted whitespace, so a run that
    // turns out to be trailing can be cut off.
    let mut trim_len: Option<usize> = None;
    while let Some(c) = chars.next() {
        if c == '\n' {
            break;
        }
        if comment {
            continue;
        }
        if c.is_whitespace() && !quoted {
            if trim_len.is_none() {
                trim_len = Some(value.len());
            }
            if !value.is_empty() {
                value.push(c);
            }
            continue;
        }
        if !quoted && (c == ';' || c == '#') {
            comment = true;
            continue;
        }
        trim_len = None;
        match c {
            '\\' => match chars.next() {
                Some('\n') => {}
                Some('\r') if chars.next_if_eq(&'\n').is_some() => {}
                Some('n') => value.push('\n'),
                Some('t') => value.push('\t'),
                Some('b') => value.push('\u{8}'),
                Some(other) => value.push(other),
                None => break,
            },
            '"' => quoted = !quoted,
            other => value.push(other),
        }
    }
    if let Some(len) = trim_len {
        value.truncate(len);
    }
    value
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
/// the foreign one. A proven root never absorbs another proven root — a nested
/// checkout is a repository of its own. It DOES absorb a path that merely no
/// longer exists inside it, unless `.gitmodules` still declares that path a
/// submodule: see [`plausibly_within_scope`] for why reduction fails open
/// where membership fails closed.
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
        if baseline.is_some_and(|base| plausibly_within_scope(raw, base)) {
            saw_baseline = true;
            continue;
        }
        let identity = normalize_workdir(raw, baseline);
        if !foreign.contains(&identity) {
            foreign.push(identity);
        }
    }

    // A proven foreign root absorbs unprovable paths that plausibly sit inside
    // it, for the same reason the baseline does: a vanished directory is not
    // evidence of a second repository. It never absorbs another proven root.
    let roots: Vec<String> = foreign
        .iter()
        .filter_map(|identity| match identity {
            WorkdirIdentity::Resolved(root) => Some(root.to_string_lossy().into_owned()),
            WorkdirIdentity::Unresolved(_) => None,
        })
        .collect();
    foreign.retain(|identity| match identity {
        WorkdirIdentity::Resolved(_) => true,
        WorkdirIdentity::Unresolved(path) => {
            !roots.iter().any(|root| plausibly_within_scope(path, root))
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

    /// Membership and identity-counting are different questions, and this pins
    /// the split deliberately rather than letting it fall out.
    ///
    /// A path under a live checkout that no longer exists cannot be PROVEN to
    /// belong to it, so the project filter must not serve it (fail closed).
    /// It equally cannot be proven to be a second repository, and the window
    /// reduction must not call it one (fail open) — `retain_frames_for_project`
    /// drops `scope_unattributed` frames outright, so counting every deleted
    /// `target/` or cleaned-up worktree as a foreign repo would silently
    /// delete ordinary operator evidence.
    #[test]
    fn a_vanished_path_fails_closed_for_membership_and_open_for_identity() {
        let root = scratch("vanished-under-live");
        let parent = root.join("vista");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        let base = parent.to_string_lossy().into_owned();
        let gone = parent.join("target").join("tmp-build");
        let gone = gone.to_string_lossy().into_owned();

        assert!(
            !workdir_within_scope(&gone, &base),
            "membership: a proven checkout must not claim a path it cannot prove"
        );
        let (scope, path) = effective_window_scope(&explicit(&[&gone]), Some(&base));
        assert_eq!(
            scope,
            WindowScope::Baseline,
            "identity: a vanished directory is not a second repository"
        );
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The one vanished path that IS provably its own repository: the parent
    /// still declares it in `.gitmodules` after the working tree is gone. A
    /// bare nested checkout leaves no such trace and is absorbed — that half
    /// of the trade is stated in `plausibly_within_scope`, not hidden.
    #[test]
    fn a_vanished_declared_submodule_is_still_its_own_repository() {
        let root = scratch("vanished-submodule");
        let parent = root.join("vista");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet-bus\"]\n\tpath = vendor/fleet-bus\n\turl = https://example.invalid/fleet-bus.git\n",
        )
        .expect("write .gitmodules");
        let base = parent.to_string_lossy().into_owned();
        let gone = parent.join("vendor").join("fleet-bus");
        let gone = gone.to_string_lossy().into_owned();

        assert!(!workdir_within_scope(&gone, &base));
        let (scope, path) = effective_window_scope(&explicit(&[&gone]), Some(&base));
        assert_eq!(
            scope,
            WindowScope::Unattributed,
            "a declared submodule keeps its identity once the tree is gone"
        );
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The baseline is a working directory, not the checkout root. Reading
    /// `.gitmodules` where the session happened to stand misses the
    /// declaration entirely and absorbs the vanished submodule into its
    /// parent — the exact leak the declaration exists to prevent.
    #[test]
    fn a_declared_submodule_is_found_from_a_subdirectory_baseline() {
        let root = scratch("submodule-subdir-baseline");
        let parent = root.join("vista");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::create_dir_all(parent.join("packages").join("api")).expect("baseline dir");
        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet-bus\"]\n\tpath = packages/api/vendor/fleet-bus\n\turl = https://example.invalid/fleet-bus.git\n",
        )
        .expect("write .gitmodules");

        // The session stood in a subdirectory; the submodule tree is gone.
        let base = parent.join("packages").join("api");
        let base = base.to_string_lossy().into_owned();
        let gone = parent
            .join("packages")
            .join("api")
            .join("vendor")
            .join("fleet-bus");
        let gone = gone.to_string_lossy().into_owned();

        let (scope, path) = effective_window_scope(&explicit(&[&gone]), Some(&base));
        assert_eq!(
            scope,
            WindowScope::Unattributed,
            "a declared submodule keeps its identity even when the baseline is a subdirectory"
        );
        assert_eq!(path, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding: a submodule's DESCENDANTS belong to the submodule, not to the
    /// parent that declares it. Exact `.gitmodules` matching absorbed a
    /// vanished `vendor/fleet-bus/src` into the parent checkout and let the
    /// window keep the parent's positive attribution.
    #[test]
    fn a_vanished_path_inside_a_declared_submodule_is_not_the_parent() {
        let root = scratch("submodule-descendant");
        let parent = root.join("vista");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet-bus\"]\n\tpath = vendor/fleet-bus\n\turl = https://example.invalid/fleet-bus.git\n",
        )
        .expect("write .gitmodules");
        let base = parent.to_string_lossy().into_owned();

        let inside = parent.join("vendor").join("fleet-bus").join("src");
        let (scope, path) =
            effective_window_scope(&explicit(&[&inside.to_string_lossy()]), Some(&base));
        assert_eq!(
            scope,
            WindowScope::Unattributed,
            "a path under a declared submodule must not inherit the parent checkout"
        );
        assert_eq!(path, None);

        // The boundary is a path COMPONENT: a sibling that merely starts with
        // the declared spelling is an ordinary vanished directory.
        let sibling = parent.join("vendor").join("fleet-bus-old");
        let (scope, _) =
            effective_window_scope(&explicit(&[&sibling.to_string_lossy()]), Some(&base));
        assert_eq!(
            scope,
            WindowScope::Baseline,
            "`fleet-bus-old` is not inside the declared `fleet-bus`"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding (P1-06): `.gitmodules` is git-config, not `path = x` lines. A
    /// reader that matched only a lowercase unquoted `path` let the parent
    /// absorb every submodule declared as `Path =` or with a quoted value.
    #[test]
    fn gitmodules_are_read_with_git_config_syntax() {
        let raw = concat!(
            "# vendored checkouts\n",
            "[Submodule \"fleet-bus\"]\n",
            "\tPath = \"vendor/fleet-bus\" ; moved in 2025\n",
            "\turl = https://example.invalid/fleet-bus.git\n",
            "[submodule \"spaced\"]\n",
            "\tpath=\"vendor/fleet bus\"\n",
            "[submodule \"commented\"]\n",
            "\tPATH = vendor/plain   # trailing comment\n",
            "[submodule \"hash\"]\n",
            "\tpath = \"vendor/with#hash\"\r\n",
            "[submodule \"continued\"]\n",
            "\tpath = vendor/cont\\\n",
            "inued\n",
            "[submodule \"escaped\"]\n",
            "\tpath = \"vendor/q\\\"uote\"\n",
            "[remote \"origin\"]\n",
            "\tpath = not/a/submodule\n",
            "[submodule.legacy] path = vendor/legacy\n",
            "[submodule \"broken\"]\n",
            "\tpath = \"vendor/unterminated\n",
        );
        assert_eq!(
            gitmodules_paths(raw),
            vec![
                "vendor/fleet-bus",
                "vendor/fleet bus",
                "vendor/plain",
                "vendor/with#hash",
                "vendor/continued",
                "vendor/q\"uote",
                "vendor/legacy",
                "vendor/unterminated",
            ]
        );
    }

    /// The same finding through the verdict: a submodule declared with a
    /// capitalised key and a quoted, spaced value keeps its identity once its
    /// tree is gone, and the component boundary still holds.
    #[test]
    fn a_quoted_submodule_declaration_is_not_absorbed_by_its_parent() {
        let root = scratch("quoted-submodule");
        let parent = root.join("vista");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet bus\"]\n\tPath = \"vendor/fleet bus\" ; moved\n",
        )
        .expect("write .gitmodules");
        let base = parent.to_string_lossy().into_owned();

        let gone = parent.join("vendor").join("fleet bus").join("src");
        let (scope, path) =
            effective_window_scope(&explicit(&[&gone.to_string_lossy()]), Some(&base));
        assert_eq!(scope, WindowScope::Unattributed);
        assert_eq!(path, None);

        let sibling = parent.join("vendor").join("fleet bus-old");
        let (scope, _) =
            effective_window_scope(&explicit(&[&sibling.to_string_lossy()]), Some(&base));
        assert_eq!(scope, WindowScope::Baseline);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding (P1-01/02): the index cache keyed verdicts on source bytes, so
    /// a nested checkout appearing or a `.gitmodules` edit left stale verdicts
    /// servable. The layout evidence of a recorded path must move whenever a
    /// verdict about it can — including when the path is only a subdirectory
    /// of the checkout that declares the submodule — and must hold still when
    /// nothing the verdicts read has changed.
    #[test]
    fn layout_evidence_moves_exactly_when_a_verdict_can() {
        let root = scratch("layout-evidence");
        let parent = root.join("vista");
        let app = parent.join("app");
        let nested = app.join("tools").join("fleet-bus");
        std::fs::create_dir_all(parent.join(".git")).expect("parent git dir");
        std::fs::create_dir_all(nested.join("src")).expect("nested dir");
        let base = app.to_string_lossy().into_owned();
        let inside = nested.join("src").to_string_lossy().into_owned();
        let vanished = app
            .join("vendor")
            .join("fleet")
            .to_string_lossy()
            .into_owned();
        let verdict = |workdir: &str| effective_window_scope(&explicit(&[workdir]), Some(&base)).0;

        let before = (scope_layout_evidence(&base), scope_layout_evidence(&inside));
        assert_eq!(
            (scope_layout_evidence(&base), scope_layout_evidence(&inside)),
            before,
            "nothing moved, so neither may the evidence"
        );
        assert_eq!(verdict(&inside), WindowScope::Baseline);
        assert_eq!(verdict(&vanished), WindowScope::Baseline);

        std::fs::create_dir_all(nested.join(".git")).expect("nested git dir");
        assert_eq!(verdict(&inside), WindowScope::Consistent);
        assert_ne!(scope_layout_evidence(&inside), before.1);
        assert_eq!(
            scope_layout_evidence(&base),
            before.0,
            "the baseline's own layout did not move"
        );

        std::fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet\"]\n\tpath = app/vendor/fleet\n",
        )
        .expect("write .gitmodules");
        assert_eq!(verdict(&vanished), WindowScope::Unattributed);
        assert_ne!(scope_layout_evidence(&base), before.0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Finding (P2-01): absoluteness was the HOST's question. A Windows
    /// rollout replayed on Unix (or the reverse) had its baseline called
    /// relative, so every relative workdir became unresolvable and the whole
    /// window unattributed. The recorded spelling decides now, on every host.
    #[test]
    fn a_foreign_platform_baseline_still_anchors_relative_workdirs() {
        let windows = r"C:\Users\dev\repo";
        assert_eq!(recorded_workdir(".", Some(windows)), windows);
        assert_eq!(
            recorded_workdir(r"packages\api", Some(windows)),
            r"C:\Users\dev\repo\packages\api"
        );
        assert_eq!(
            recorded_workdir("packages/api", Some(windows)),
            r"C:\Users\dev\repo\packages\api"
        );
        assert_eq!(
            recorded_workdir(r"..\other", Some(windows)),
            r"C:\Users\dev\other"
        );
        assert_eq!(
            recorded_workdir(r"\\server\share\repo", Some(windows)),
            r"\\server\share\repo"
        );
        let (scope, path) = effective_window_scope(
            &explicit(&[".", r"packages\api", r"C:\Users\dev\repo\src"]),
            Some(windows),
        );
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);
        let (scope, _) = effective_window_scope(&explicit(&[r"..\other"]), Some(windows));
        assert_eq!(
            scope,
            WindowScope::Unattributed,
            "leaving the baseline is still foreign evidence"
        );

        // The Unix spelling reads the same on a Windows host.
        let unix = "/sessions/replayed";
        assert_eq!(recorded_workdir("./api/..", Some(unix)), unix);
        let (scope, _) = effective_window_scope(&explicit(&[".", "api"]), Some(unix));
        assert_eq!(scope, WindowScope::Baseline);

        // In a Unix spelling `\` is a filename character, not a separator.
        assert_eq!(
            recorded_workdir(r"odd\name", Some(unix)),
            r"/sessions/replayed/odd\name"
        );
        // A drive-relative token is not absolute anywhere.
        assert!(!absolute_anywhere("C:repo"));
    }

    /// Finding: the fail-closed threshold for an over-cap record counted raw
    /// `"type":` substrings anywhere in the visible prefix. An `arguments`
    /// object carrying its own `type` field therefore raised the count past
    /// the threshold and bought the record a not-a-tool-call verdict — the
    /// unreadable payload deciding whether it had to be treated as opaque.
    #[test]
    fn argument_content_cannot_talk_an_over_cap_record_out_of_being_a_tool_call() {
        // Envelope discriminator readable, payload discriminator truncated
        // away, and the visible argument body carries a nested `type` key.
        let prefix = concat!(
            r#"{"timestamp":"2026-09-22T00:00:00Z","type":"response_item","payload":{"#,
            r#""arguments":{"cmd":"deploy","env":{"type":"noise"},"workdir":"/other/checkout"#
        );
        assert!(
            truncated_record_is_tool_call(prefix),
            "payload content must never satisfy the discriminator threshold"
        );

        // The same text inside a STRING body is equally inert.
        let quoted = concat!(
            r#"{"timestamp":"2026-09-22T00:00:00Z","type":"response_item","payload":{"#,
            r#""arguments":"{\"cmd\":\"rg 'type': src\",\"workdir\":\"/other/checkout"#
        );
        assert!(
            truncated_record_is_tool_call(quoted),
            "argument text must never satisfy the discriminator threshold"
        );

        // A plainly readable non-call record still keeps its evidence: both
        // real discriminators survived the cap, so nothing was lost.
        let readable = concat!(
            r#"{"timestamp":"2026-09-22T00:00:00Z","type":"response_item","payload":{"#,
            r#""type":"message","role":"assistant","content":[{"#
        );
        assert!(
            !truncated_record_is_tool_call(readable),
            "two structural discriminators, neither a call"
        );
    }

    /// Finding: replaying a session whose checkout is gone from this machine.
    /// Nothing resolves, so lexical comparison is the only evidence there is —
    /// and it has to compare like with like: the baseline-joined `.`, not the
    /// bare token, or every turn of the session is discarded as foreign.
    #[test]
    fn a_relative_workdir_in_a_vanished_checkout_still_belongs_to_it() {
        let baseline = "/nonexistent-aicx-scope/old/repo";

        assert_eq!(
            normalize_workdir(".", Some(baseline)),
            WorkdirIdentity::Unresolved(baseline.to_string()),
            "a relative workdir keeps the baseline it was recorded against"
        );
        assert!(workdir_within_scope(".", baseline));
        assert!(workdir_within_scope("packages/api", baseline));
        assert!(!workdir_within_scope("../sibling", baseline));

        let (scope, path) =
            effective_window_scope(&explicit(&[".", "packages/api"]), Some(baseline));
        assert_eq!(scope, WindowScope::Baseline);
        assert_eq!(path, None);
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
