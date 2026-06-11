//! Session discovery surface for AICX.
//!
//! First-class enumeration of agent sessions on disk, so an agent standing in a
//! repository can ask "show me the relevant sessions for this repo" without
//! remembering session ids. This is the data core behind the `aicx sessions
//! list` / `aicx session show` CLI surface.
//!
//! P0 temporal discipline (vc-intents MASTER): every [`SessionInfo`] carries
//! ABSOLUTE time — `started_at` / `updated_at` are full `DateTime<Utc>`
//! (RFC3339 / ISO-8601 with offset on serialize), never a bare `HH:MM:SS`. When
//! a session exposes no parseable timestamp the record says so explicitly via
//! [`TemporalConfidence::None`] rather than silently presenting partial time as
//! truth.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// True if `path`'s mtime is older than `after` — a cheap metadata-only
/// pre-filter (no file read) so a scan can skip ancient session files before
/// the expensive full parse. A missing/unreadable mtime is treated as "keep".
fn older_than(path: &Path, after: Option<SystemTime>) -> bool {
    let Some(after) = after else {
        return false;
    };
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|mtime| mtime < after)
        .unwrap_or(false)
}

/// True when `child` equals `parent` or sits strictly under it with `sep` as
/// the segment boundary right after the prefix. The boundary check is what
/// prevents `/a/repo-backup` from matching `/a/repo` (and the `-`-encoded
/// equivalent in the Claude project-dir space).
fn nests_under(child: &str, parent: &str, sep: char) -> bool {
    child
        .strip_prefix(parent)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(sep))
}

/// True when `here` and `repo` are the same path or one nests inside the other —
/// the "is this session relevant to my cwd?" test, shared by the pre-read dir
/// prune and the post-discovery [`select_sessions`] filter. Matching is on
/// whole path segments: after a shared prefix the next char must be `/` (or
/// the end), so `repo` never matches `repo-backup`.
fn cwd_nests(here: &str, repo: &str) -> bool {
    // Case-insensitive: macOS filesystems are case-insensitive, and the same repo
    // is recorded under mixed casing (e.g. `vetcoders` vs `VetCoders`).
    let (here, repo) = (here.to_lowercase(), repo.to_lowercase());
    let (here, repo) = (here.trim_end_matches('/'), repo.trim_end_matches('/'));
    nests_under(here, repo, '/') || nests_under(repo, here, '/')
}

/// ENCODED-space variant of [`cwd_nests`] for [`Association::Inferred`]
/// sessions: the repo path was decoded from a Claude project dir name (every
/// `-` became `/`, lossy), so comparing decoded strings would mis-match real
/// hyphenated paths. Instead encode `here` the same way (`/` -> `-`) and nest
/// with `-` as the boundary, mirroring the pre-read dir prune.
fn cwd_nests_encoded(here: &str, repo: &str) -> bool {
    let here = here.trim_end_matches('/').replace('/', "-").to_lowercase();
    let repo = repo.trim_end_matches('/').replace('/', "-").to_lowercase();
    nests_under(&here, &repo, '-') || nests_under(&repo, &here, '-')
}

/// How a session was associated with a project/repo: directly read from the
/// session's own `cwd`, or inferred from the on-disk directory encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Association {
    /// Repo/project came from the session's own recorded `cwd`.
    Exact,
    /// Repo/project was inferred (e.g. decoded from the directory name).
    Inferred,
    /// No repo/project association could be established.
    Unknown,
}

/// Whether the session exposed full absolute time, partial, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalConfidence {
    /// At least one full `DateTime<Utc>` (date + year + time) was parsed.
    Full,
    /// Some time signal exists but is incomplete (no usable absolute stamp).
    Partial,
    /// No usable timestamp at all — recency cannot be judged.
    None,
}

/// A discovered agent session with absolute temporal metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    /// First message timestamp — absolute (RFC3339 on serialize).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// Last message timestamp — absolute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    pub message_count: usize,
    pub user_message_count: usize,
    pub agent_message_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub source_path: PathBuf,
    pub association: Association,
    pub temporal_confidence: TemporalConfidence,
}

/// Decode a Claude project directory name back into a cwd path.
///
/// Claude encodes the cwd by replacing `/` with `-`, e.g.
/// `-Users-silver-Git-transcript-builder`. The encoding is lossy (real hyphens
/// are indistinguishable from separators), so this is a best-effort INFERENCE,
/// not exact truth — callers mark the association as [`Association::Inferred`]
/// and any path comparison against such a decoded value must happen in the
/// ENCODED space (see [`cwd_nests_encoded`]), never on the decoded string. The
/// session's own `cwd` field, when present, is always preferred over this
/// decode.
fn decode_claude_project_dir(dir_name: &str) -> Option<String> {
    if !dir_name.starts_with('-') {
        return None;
    }
    Some(dir_name.replace('-', "/"))
}

/// Derive a short project label (last path segment) from a cwd.
fn project_label_from_cwd(cwd: &str) -> Option<String> {
    cwd.trim_end_matches('/')
        .rsplit('/')
        .find(|seg| !seg.is_empty())
        .map(str::to_string)
}

/// Discover Claude Code sessions under a `projects` root
/// (`~/.claude/projects`). Each `<encoded-cwd>/<session-id>.jsonl` becomes one
/// [`SessionInfo`]. Tolerant: unparseable lines are skipped, unreadable files
/// are omitted rather than aborting the scan.
pub fn discover_claude_sessions(
    projects_root: &Path,
    modified_after: Option<SystemTime>,
    cwd_filter: Option<&str>,
) -> Vec<SessionInfo> {
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let Ok(dirs) = fs::read_dir(projects_root) else {
        return out;
    };
    for dir_entry in dirs.flatten() {
        let dir_path = dir_entry.path();
        if !dir_path.is_dir() {
            continue;
        }
        let dir_name = dir_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let decoded_cwd = decode_claude_project_dir(&dir_name);

        // Claude encodes the cwd in the project dir name, so a `--cwd` filter can
        // prune entire non-matching project dirs BEFORE reading any session file
        // — the key to a fast `--cwd` list over a large history. Match by
        // ENCODING the target ('/' -> '-') and comparing against the raw dir name,
        // NOT by decoding: the decode is lossy (a real hyphen like `vc-workspace`
        // is indistinguishable from a separator) and would mis-prune those paths.
        if let Some(want) = cwd_filter {
            // Lowercase: macOS is case-insensitive and the same repo appears under
            // mixed casing (`vetcoders` vs `VetCoders`). The prefix match enforces
            // a `-` segment boundary (encoded separator) so `-a-repo` does not
            // keep `-a-repository`; an encoded subdir like `-a-repo-backup` stays
            // (it could be `/a/repo/backup` — conservative keep, the post-discovery
            // filter on the recorded cwd settles it).
            let want_enc = want.replace('/', "-").to_lowercase();
            let dir_lc = dir_name.to_lowercase();
            if !nests_under(&dir_lc, &want_enc, '-') && !nests_under(&want_enc, &dir_lc, '-') {
                continue;
            }
        }

        let Ok(files) = fs::read_dir(&dir_path) else {
            continue;
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if older_than(&path, modified_after) {
                continue;
            }
            match scan_claude_session_file(&path, decoded_cwd.as_deref()) {
                Some(info) => out.push(info),
                None => skipped += 1,
            }
        }
    }
    if skipped > 0 {
        eprintln!("aicx: sessions: skipped {skipped} unreadable file(s) (claude)");
    }
    out
}

/// Parse a single Claude session `.jsonl` into a [`SessionInfo`].
fn scan_claude_session_file(path: &Path, decoded_cwd: Option<&str>) -> Option<SessionInfo> {
    let content = fs::read_to_string(path).ok()?;
    let session_id = path.file_stem().and_then(|s| s.to_str())?.to_string();

    let mut started_at: Option<DateTime<Utc>> = None;
    let mut updated_at: Option<DateTime<Utc>> = None;
    let mut message_count = 0usize;
    let mut user_message_count = 0usize;
    let mut agent_message_count = 0usize;
    let mut recorded_cwd: Option<String> = None;
    let mut title: Option<String> = None;
    // Set only after a line actually parses: a file holding nothing but garbage
    // has no time signal at all and must report TemporalConfidence::None, not
    // Partial.
    let mut saw_parsable_line = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        saw_parsable_line = true;

        if recorded_cwd.is_none()
            && let Some(cwd) = value.get("cwd").and_then(|v| v.as_str())
            && !cwd.trim().is_empty()
        {
            recorded_cwd = Some(cwd.to_string());
        }

        if let Some(ts) = value
            .get("timestamp")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
        {
            started_at = Some(started_at.map_or(ts, |cur| cur.min(ts)));
            updated_at = Some(updated_at.map_or(ts, |cur| cur.max(ts)));
        }

        let role = value.get("type").and_then(|v| v.as_str()).or_else(|| {
            value
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(|v| v.as_str())
        });
        match role {
            Some("user") => {
                message_count += 1;
                user_message_count += 1;
                if title.is_none()
                    && let Some(text) = value
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                    && !text.trim().is_empty()
                {
                    title = Some(short_title(text));
                }
            }
            Some("assistant") => {
                message_count += 1;
                agent_message_count += 1;
            }
            _ => {}
        }
    }

    let (repo_path, association) = match (recorded_cwd, decoded_cwd) {
        (Some(cwd), _) => (Some(cwd), Association::Exact),
        (None, Some(decoded)) => (Some(decoded.to_string()), Association::Inferred),
        (None, None) => (None, Association::Unknown),
    };
    let project = repo_path.as_deref().and_then(project_label_from_cwd);

    let temporal_confidence = if started_at.is_some() {
        TemporalConfidence::Full
    } else if saw_parsable_line {
        TemporalConfidence::Partial
    } else {
        TemporalConfidence::None
    };

    Some(SessionInfo {
        session_id,
        agent: "claude".to_string(),
        project,
        repo_path,
        started_at,
        updated_at,
        message_count,
        user_message_count,
        agent_message_count,
        title,
        source_path: path.to_path_buf(),
        association,
        temporal_confidence,
    })
}

/// Depth cap for the codex `YYYY/MM/DD` rollout walk: the real layout is 3
/// levels, 16 leaves generous headroom while still terminating on pathological
/// (e.g. cyclic-looking) trees.
const MAX_CODEX_SCAN_DEPTH: usize = 16;

/// Discover Codex CLI sessions under a sessions root (`~/.codex/sessions`),
/// which nests rollouts by date (`YYYY/MM/DD/rollout-*.jsonl`). Walks the tree
/// without following symlinks (the entry's own file type decides) and with a
/// [`MAX_CODEX_SCAN_DEPTH`] cap.
pub fn discover_codex_sessions(
    sessions_root: &Path,
    modified_after: Option<SystemTime>,
) -> Vec<SessionInfo> {
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let mut stack = vec![(sessions_root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            // entry.file_type() reports the symlink itself (no follow), unlike
            // path.is_dir() which would traverse it.
            let Ok(file_type) = entry.file_type() else {
                skipped += 1;
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if depth < MAX_CODEX_SCAN_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if file_type.is_file()
                && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                && !older_than(&path, modified_after)
            {
                match scan_codex_session_file(&path) {
                    Some(info) => out.push(info),
                    None => skipped += 1,
                }
            }
        }
    }
    if skipped > 0 {
        eprintln!("aicx: sessions: skipped {skipped} unreadable file(s) (codex)");
    }
    out
}

/// Parse a single Codex rollout `.jsonl` into a [`SessionInfo`]. Canonical id +
/// cwd come from the `session_meta` line; the conversation count uses only the
/// `response_item` message stream by role (developer/system/tool rows are not
/// conversation — consistent with the meta->SystemNote classification).
fn scan_codex_session_file(path: &Path) -> Option<SessionInfo> {
    let content = fs::read_to_string(path).ok()?;
    let mut session_id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut updated_at: Option<DateTime<Utc>> = None;
    let mut user_message_count = 0usize;
    let mut agent_message_count = 0usize;
    let mut title: Option<String> = None;
    // Set only after a line actually parses (see scan_claude_session_file): a
    // garbage-only file reports TemporalConfidence::None, not Partial.
    let mut saw_parsable_line = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        saw_parsable_line = true;

        if let Some(ts) = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
        {
            started_at = Some(started_at.map_or(ts, |c| c.min(ts)));
            updated_at = Some(updated_at.map_or(ts, |c| c.max(ts)));
        }

        let typ = v.get("type").and_then(|t| t.as_str());
        let payload = v.get("payload");
        if typ == Some("session_meta") {
            if session_id.is_none() {
                session_id = payload
                    .and_then(|p| p.get("id"))
                    .and_then(|i| i.as_str())
                    .map(String::from);
            }
            if cwd.is_none() {
                cwd = payload
                    .and_then(|p| p.get("cwd"))
                    .and_then(|c| c.as_str())
                    .map(String::from);
            }
            continue;
        }
        if typ == Some("response_item")
            && payload.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("message")
        {
            match payload.and_then(|p| p.get("role")).and_then(|r| r.as_str()) {
                Some("user") => {
                    user_message_count += 1;
                    if title.is_none() {
                        title = codex_message_text(payload).map(|t| short_title(&t));
                    }
                }
                Some("assistant") => agent_message_count += 1,
                _ => {}
            }
        }
    }

    let session_id = session_id.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string()
    });
    let project = cwd.as_deref().and_then(project_label_from_cwd);
    let (repo_path, association) = match cwd {
        Some(c) => (Some(c), Association::Exact),
        None => (None, Association::Unknown),
    };
    let temporal_confidence = if started_at.is_some() {
        TemporalConfidence::Full
    } else if saw_parsable_line {
        TemporalConfidence::Partial
    } else {
        TemporalConfidence::None
    };

    Some(SessionInfo {
        session_id,
        agent: "codex".to_string(),
        project,
        repo_path,
        started_at,
        updated_at,
        message_count: user_message_count + agent_message_count,
        user_message_count,
        agent_message_count,
        title,
        source_path: path.to_path_buf(),
        association,
        temporal_confidence,
    })
}

/// Best-effort text of a codex message payload (`content` can be a string or an
/// array of `{text}` / `{input_text}` parts).
fn codex_message_text(payload: Option<&serde_json::Value>) -> Option<String> {
    let content = payload?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    content.as_array()?.iter().find_map(|part| {
        part.get("text")
            .and_then(|t| t.as_str())
            .filter(|t| !t.trim().is_empty())
            .map(String::from)
    })
}

/// Discover Gemini CLI sessions under a tmp root (`~/.gemini/tmp`). Each project
/// lives in `<tmp>/<project-or-hash>/chats/session-*.json` as a single whole-file
/// JSON document with `sessionId`, `startTime`, `lastUpdated`, and `messages[]`.
/// Gemini records no cwd — only a project hash — so the repo association is the
/// directory name (the project basename) when it is not an opaque hash.
pub fn discover_gemini_sessions(
    tmp_root: &Path,
    modified_after: Option<SystemTime>,
    cwd_filter: Option<&str>,
) -> Vec<SessionInfo> {
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let Ok(dirs) = fs::read_dir(tmp_root) else {
        return out;
    };
    // Gemini has no cwd, only the project basename == the dir name; match --cwd
    // against the current dir's last path segment.
    let want_base = cwd_filter
        .and_then(project_label_from_cwd)
        .map(|s| s.to_lowercase());
    for dir_entry in dirs.flatten() {
        let proj_dir = dir_entry.path();
        if !proj_dir.is_dir() {
            continue;
        }
        let dir_name = proj_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if let Some(want) = &want_base
            && dir_name.to_lowercase() != *want
        {
            continue;
        }
        let chats = proj_dir.join("chats");
        let Ok(files) = fs::read_dir(&chats) else {
            continue;
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            let ext = path.extension().and_then(|e| e.to_str());
            if (ext != Some("json") && ext != Some("jsonl")) || older_than(&path, modified_after) {
                continue;
            }
            match scan_gemini_session_file(&path, &dir_name) {
                Some(info) => out.push(info),
                None => skipped += 1,
            }
        }
    }
    if skipped > 0 {
        eprintln!("aicx: sessions: skipped {skipped} unreadable file(s) (gemini)");
    }
    out
}

/// Parse a single Gemini whole-file session JSON into a [`SessionInfo`].
fn scan_gemini_session_file(path: &Path, dir_name: &str) -> Option<SessionInfo> {
    let content = fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;

    let session_id = v
        .get("sessionId")
        .and_then(|s| s.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string()
        });
    let parse_ts = |key: &str| {
        v.get(key)
            .and_then(|s| s.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
    };
    let started_at = parse_ts("startTime");
    let updated_at = parse_ts("lastUpdated").or(started_at);

    let mut user_message_count = 0usize;
    let mut agent_message_count = 0usize;
    let mut title = None;
    // Partial-time signal: a per-message timestamp exists even though the
    // header times are missing.
    let mut messages_have_time = false;
    if let Some(msgs) = v.get("messages").and_then(|m| m.as_array()) {
        for m in msgs {
            if !messages_have_time
                && m.get("timestamp")
                    .and_then(|t| t.as_str())
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .is_some()
            {
                messages_have_time = true;
            }
            match m.get("type").and_then(|t| t.as_str()) {
                Some("user") => {
                    user_message_count += 1;
                    if title.is_none()
                        && let Some(t) = m
                            .get("content")
                            .and_then(|c| c.as_str())
                            .filter(|t| !t.trim().is_empty())
                    {
                        title = Some(short_title(t));
                    }
                }
                Some("gemini") | Some("model") | Some("assistant") => agent_message_count += 1,
                _ => {}
            }
        }
    }

    let is_hash = dir_name.len() == 64 && dir_name.chars().all(|c| c.is_ascii_hexdigit());
    let project = if is_hash || dir_name.is_empty() {
        None
    } else {
        Some(dir_name.to_string())
    };
    let association = if project.is_some() {
        Association::Inferred
    } else {
        Association::Unknown
    };
    // Consistent with claude/codex: no startTime, no lastUpdated and no
    // per-message time means NO time signal at all -> None, never Partial.
    let temporal_confidence = if started_at.is_some() {
        TemporalConfidence::Full
    } else if updated_at.is_some() || messages_have_time {
        TemporalConfidence::Partial
    } else {
        TemporalConfidence::None
    };

    Some(SessionInfo {
        session_id,
        agent: "gemini".to_string(),
        project,
        // Gemini records only a project hash, never the cwd.
        repo_path: None,
        started_at,
        updated_at,
        message_count: user_message_count + agent_message_count,
        user_message_count,
        agent_message_count,
        title,
        source_path: path.to_path_buf(),
        association,
        temporal_confidence,
    })
}

/// Junie session directory prefix (`~/.junie/sessions/session-*`). Mirrors
/// `JUNIE_SESSION_DIR_PREFIX` in `sources/providers/junie.rs` (private to the
/// extractor) — keep the two in sync.
const JUNIE_SESSION_DIR_PREFIX: &str = "session-";

/// Junie events log filename inside a session directory.
const JUNIE_EVENTS_FILENAME: &str = "events.jsonl";

/// Parse Junie's compact `YYMMDD` + `HHMMSS` timestamp pair (used in session
/// dir names `session-260408-214715-abcd` and request ids
/// `prompt-260408-214823-br8l`). Mirrors `parse_compact_junie_timestamp` in
/// `sources/providers/junie.rs` (private to the extractor) — keep in sync.
/// Junie writes these as UTC wall-clock, consistent with the extractor.
fn parse_compact_junie_timestamp(compact_date: &str, compact_time: &str) -> Option<DateTime<Utc>> {
    if compact_date.len() != 6 || compact_time.len() != 6 {
        return None;
    }
    // Byte-index slicing below is only UTF-8-safe for ASCII.
    if !compact_date.is_ascii() || !compact_time.is_ascii() {
        return None;
    }
    let year = 2000 + compact_date[0..2].parse::<i32>().ok()?;
    let month = compact_date[2..4].parse::<u32>().ok()?;
    let day = compact_date[4..6].parse::<u32>().ok()?;
    let hour = compact_time[0..2].parse::<u32>().ok()?;
    let minute = compact_time[2..4].parse::<u32>().ok()?;
    let second = compact_time[4..6].parse::<u32>().ok()?;
    let naive =
        chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)?;
    Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

/// Parse a compact timestamp out of a `<prefix>-YYMMDD-HHMMSS[-suffix]` string
/// (Junie session-dir suffix or `prompt-*` request id).
fn parse_junie_compact_pair(s: &str) -> Option<DateTime<Utc>> {
    let mut parts = s.split('-');
    let compact_date = parts.next()?;
    let compact_time = parts.next()?;
    parse_compact_junie_timestamp(compact_date, compact_time)
}

/// Discover Junie sessions under a sessions root (`~/.junie/sessions`). Each
/// session is `session-<YYMMDD>-<HHMMSS>-<suffix>/events.jsonl`; the session
/// id is the directory name minus the `session-` prefix — the same id
/// `aicx extract --agent junie` reports. Absolute time comes from the compact
/// timestamps Junie embeds in the dir name and in `prompt-*` request ids
/// (events carry no per-line RFC3339 stamp). Tolerant: unreadable files are
/// counted and skipped, never abort the scan.
pub fn discover_junie_sessions(
    sessions_root: &Path,
    modified_after: Option<SystemTime>,
) -> Vec<SessionInfo> {
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let Ok(dirs) = fs::read_dir(sessions_root) else {
        return out;
    };
    for dir_entry in dirs.flatten() {
        let dir_path = dir_entry.path();
        if !dir_path.is_dir() {
            continue;
        }
        let Some(session_id) = dir_path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix(JUNIE_SESSION_DIR_PREFIX))
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let events = dir_path.join(JUNIE_EVENTS_FILENAME);
        if !events.is_file() || older_than(&events, modified_after) {
            continue;
        }
        match scan_junie_session_file(&events, session_id) {
            Some(info) => out.push(info),
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        eprintln!("aicx: sessions: skipped {skipped} unreadable file(s) (junie)");
    }
    out
}

/// Parse a single Junie `events.jsonl` into a [`SessionInfo`].
///
/// Event shapes (same contract the extractor in
/// `sources/providers/junie.rs` consumes):
/// - `{"kind":"UserPromptEvent","requestId":"prompt-YYMMDD-HHMMSS-..","prompt":..}`
///   — a user message; prompts carrying `PlanAttachment` /
///   `ContinueTaskAttachment` are harness meta, not operator messages.
/// - `{"kind":"UserResponseEvent","prompt":..}` — a user choice/reply.
/// - `{"kind":"SessionA2uxEvent","event":{"agentEvent":{"kind":..}}}` —
///   nested agent events; `ResultBlockUpdatedEvent` is the agent reply
///   surface (deduped per stepId like the extractor),
///   `CurrentDirectoryUpdatedEvent` carries the recorded cwd.
fn scan_junie_session_file(path: &Path, session_id: &str) -> Option<SessionInfo> {
    let content = fs::read_to_string(path).ok()?;

    // Anchor: the session dir name embeds the start time.
    let anchor = parse_junie_compact_pair(session_id);
    let mut started_at: Option<DateTime<Utc>> = anchor;
    let mut updated_at: Option<DateTime<Utc>> = anchor;
    let mut user_message_count = 0usize;
    let mut agent_message_count = 0usize;
    let mut title: Option<String> = None;
    let mut recorded_cwd: Option<String> = None;
    // Garbage-only files report TemporalConfidence::None (consistent with
    // claude/codex) — unless the dir-name anchor already gave absolute time.
    let mut saw_parsable_line = false;
    // Junie streams ResultBlockUpdatedEvent snapshots (repeats per stepId);
    // count a reply only when the rendered text changes, like the extractor.
    let mut last_result_render: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        saw_parsable_line = true;

        match v.get("kind").and_then(|k| k.as_str()) {
            Some("UserPromptEvent") => {
                let prompt = v
                    .get("prompt")
                    .and_then(|p| p.as_str())
                    .map(str::trim)
                    .filter(|t| !t.is_empty());
                if let Some(ts) = v
                    .get("requestId")
                    .and_then(|r| r.as_str())
                    .and_then(|r| r.strip_prefix("prompt-"))
                    .and_then(parse_junie_compact_pair)
                {
                    started_at = Some(started_at.map_or(ts, |c| c.min(ts)));
                    updated_at = Some(updated_at.map_or(ts, |c| c.max(ts)));
                }
                // Plan/continue attachments are injected harness meta, not an
                // operator message — same rule as the extractor's SystemNote.
                let is_meta = v
                    .get("customAttachments")
                    .and_then(|a| a.as_array())
                    .is_some_and(|attachments| {
                        attachments.iter().any(|attachment| {
                            matches!(
                                attachment.get("kind").and_then(|k| k.as_str()),
                                Some("PlanAttachment" | "ContinueTaskAttachment")
                            )
                        })
                    });
                if !is_meta && let Some(text) = prompt {
                    user_message_count += 1;
                    if title.is_none() {
                        title = Some(short_title(text));
                    }
                }
            }
            Some("UserResponseEvent") => {
                if v.get("prompt")
                    .and_then(|p| p.as_str())
                    .is_some_and(|t| !t.trim().is_empty())
                {
                    user_message_count += 1;
                }
            }
            _ => {
                let Some(agent_event) = v.get("event").and_then(|e| e.get("agentEvent")) else {
                    continue;
                };
                match agent_event.get("kind").and_then(|k| k.as_str()) {
                    Some("CurrentDirectoryUpdatedEvent") => {
                        if recorded_cwd.is_none()
                            && let Some(cwd) = agent_event
                                .get("currentDirectory")
                                .and_then(|c| c.as_str())
                                .map(str::trim)
                                .filter(|c| !c.is_empty())
                        {
                            recorded_cwd = Some(cwd.to_string());
                        }
                    }
                    Some("ResultBlockUpdatedEvent") => {
                        let Some(result) = agent_event
                            .get("result")
                            .and_then(|r| r.as_str())
                            .map(str::trim)
                            .filter(|r| !r.is_empty())
                        else {
                            continue;
                        };
                        let step_id = agent_event
                            .get("stepId")
                            .and_then(|s| s.as_str())
                            .unwrap_or("(no-step)")
                            .to_string();
                        if last_result_render
                            .get(&step_id)
                            .is_some_and(|prev| prev == result)
                        {
                            continue;
                        }
                        last_result_render.insert(step_id, result.to_string());
                        agent_message_count += 1;
                    }
                    _ => {}
                }
            }
        }
    }

    let project = recorded_cwd.as_deref().and_then(project_label_from_cwd);
    let (repo_path, association) = match recorded_cwd {
        Some(c) => (Some(c), Association::Exact),
        None => (None, Association::Unknown),
    };
    // Full when the dir name or a request id yielded an absolute stamp;
    // Partial when the file parsed but exposed no time signal; None for
    // garbage-only content (consistent with claude/codex/gemini).
    let temporal_confidence = if started_at.is_some() {
        TemporalConfidence::Full
    } else if saw_parsable_line {
        TemporalConfidence::Partial
    } else {
        TemporalConfidence::None
    };

    Some(SessionInfo {
        session_id: session_id.to_string(),
        agent: "junie".to_string(),
        project,
        repo_path,
        started_at,
        updated_at,
        message_count: user_message_count + agent_message_count,
        user_message_count,
        agent_message_count,
        title,
        source_path: path.to_path_buf(),
        association,
        temporal_confidence,
    })
}

/// Extract the trailing session-id segment from a codex rollout filename stem,
/// shaped `rollout-<YYYY-MM-DDThh-mm-ss>-<id>`. Returns `None` when the stem
/// does not follow that shape (callers then fall back to whole-stem matching).
fn codex_rollout_id_segment(stem: &str) -> Option<&str> {
    let rest = stem.strip_prefix("rollout-")?;
    // The timestamp block is a fixed 19-char `YYYY-MM-DDThh-mm-ss`.
    let ts = rest.get(..19)?;
    if rest.as_bytes().get(19) != Some(&b'-')
        || !ts
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == 'T')
    {
        return None;
    }
    rest.get(20..).filter(|id| !id.is_empty())
}

/// Locate a single session by id (or unique prefix) for `aicx session show`.
/// Fast for claude/codex/junie (the id is in the file/dir name, so no file is
/// read until a name matches); gemini falls back to a header scan (its id
/// lives inside the file). Returns the first match, trying
/// claude -> codex -> gemini -> junie. Within every branch an ambiguous
/// prefix resolves deterministically: candidates are sorted and a warning
/// names the match count before the first one wins.
pub fn find_session_by_id(home: &Path, id: &str) -> Option<SessionInfo> {
    // Claude: file stem IS the session id. Candidates are collected and
    // sorted so an ambiguous prefix resolves deterministically (warn + first
    // by path) instead of in fs::read_dir order — mirrors the codex/junie
    // branches.
    let claude_root = home.join(".claude").join("projects");
    if let Ok(dirs) = fs::read_dir(&claude_root) {
        let mut candidates: Vec<(PathBuf, Option<String>)> = Vec::new();
        for d in dirs.flatten() {
            let dp = d.path();
            if !dp.is_dir() {
                continue;
            }
            let decoded = decode_claude_project_dir(
                dp.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
            );
            let Ok(files) = fs::read_dir(&dp) else {
                continue;
            };
            for f in files.flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                if stem.starts_with(id) {
                    candidates.push((p, decoded.clone()));
                }
            }
        }
        candidates.sort();
        if candidates.len() > 1 {
            eprintln!(
                "aicx: session: id '{id}' matches {} claude sessions; using the first (sorted)",
                candidates.len()
            );
        }
        for (p, decoded) in candidates {
            if let Some(info) = scan_claude_session_file(&p, decoded.as_deref()) {
                return Some(info);
            }
        }
    }

    // Codex: the filename embeds the uuid as the trailing segment of
    // `rollout-<timestamp>-<id>`; match a prefix of that segment (so an id
    // prefix never collides with the timestamp block), falling back to a
    // whole-stem prefix match for non-rollout-shaped names. Walk without
    // following symlinks and with the same depth cap as discovery.
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut stack = vec![(home.join(".codex").join("sessions"), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for e in read.flatten() {
            let Ok(file_type) = e.file_type() else {
                continue;
            };
            let p = e.path();
            if file_type.is_dir() {
                if depth < MAX_CODEX_SCAN_DEPTH {
                    stack.push((p, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() || p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            let matched = codex_rollout_id_segment(stem)
                .map_or_else(|| stem.starts_with(id), |seg| seg.starts_with(id));
            if matched {
                candidates.push(p);
            }
        }
    }
    // Deterministic on ambiguity: warn, then take the lexicographically first.
    candidates.sort();
    if candidates.len() > 1 {
        eprintln!(
            "aicx: session: id '{id}' matches {} codex rollouts; using the first (sorted)",
            candidates.len()
        );
    }
    for p in candidates {
        if let Some(info) = scan_codex_session_file(&p) {
            return Some(info);
        }
    }

    // Gemini: id lives in the header; scan (few files). Matches are collected
    // and sorted (session id, then path) so an ambiguous prefix resolves
    // deterministically with a warning — mirrors the codex/junie branches.
    let gemini_root = home.join(".gemini").join("tmp");
    if let Ok(dirs) = fs::read_dir(&gemini_root) {
        let mut candidates: Vec<SessionInfo> = Vec::new();
        for d in dirs.flatten() {
            let dir_name = d
                .path()
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let Ok(files) = fs::read_dir(d.path().join("chats")) else {
                continue;
            };
            for f in files.flatten() {
                let p = f.path();
                let ext = p.extension().and_then(|x| x.to_str());
                if ext != Some("json") && ext != Some("jsonl") {
                    continue;
                }
                if let Some(info) = scan_gemini_session_file(&p, &dir_name)
                    && info.session_id.starts_with(id)
                {
                    candidates.push(info);
                }
            }
        }
        candidates.sort_by(|a, b| {
            a.session_id
                .cmp(&b.session_id)
                .then_with(|| a.source_path.cmp(&b.source_path))
        });
        if candidates.len() > 1 {
            eprintln!(
                "aicx: session: id '{id}' matches {} gemini sessions; using the first (sorted)",
                candidates.len()
            );
        }
        if let Some(info) = candidates.into_iter().next() {
            return Some(info);
        }
    }

    // Junie: the session id is the dir name minus the `session-` prefix; match
    // a prefix of that segment, no file read until a name matches. Sorted for
    // a deterministic pick on ambiguous prefixes (mirrors the codex branch).
    let junie_root = home.join(".junie").join("sessions");
    if let Ok(dirs) = fs::read_dir(&junie_root) {
        let mut candidates: Vec<(String, PathBuf)> = Vec::new();
        for d in dirs.flatten() {
            let dp = d.path();
            if !dp.is_dir() {
                continue;
            }
            let Some(sid) = dp
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix(JUNIE_SESSION_DIR_PREFIX))
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            if !sid.starts_with(id) {
                continue;
            }
            let events = dp.join(JUNIE_EVENTS_FILENAME);
            if events.is_file() {
                candidates.push((sid.to_string(), events));
            }
        }
        candidates.sort();
        if candidates.len() > 1 {
            eprintln!(
                "aicx: session: id '{id}' matches {} junie sessions; using the first (sorted)",
                candidates.len()
            );
        }
        for (sid, events) in candidates {
            if let Some(info) = scan_junie_session_file(&events, &sid) {
                return Some(info);
            }
        }
    }

    None
}

fn short_title(text: &str) -> String {
    let first_line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let trimmed = first_line.trim();
    if trimmed.chars().count() <= 80 {
        trimmed.to_string()
    } else {
        let clipped: String = trimmed.chars().take(77).collect();
        format!("{clipped}...")
    }
}

/// Select sessions for `aicx sessions list`: optional agent filter, optional
/// cwd-association filter (keep sessions whose repo_path nests with `here`),
/// newest-first sort (by `updated_at`, falling back to `started_at`), then an
/// optional `limit` (0 = all). Pure so the list policy is testable without the
/// filesystem.
pub fn select_sessions(
    mut sessions: Vec<SessionInfo>,
    here: Option<&str>,
    agent: Option<&str>,
    since: Option<DateTime<Utc>>,
    limit: usize,
) -> Vec<SessionInfo> {
    if let Some(agent) = agent {
        sessions.retain(|s| s.agent == agent);
    }
    if let Some(since) = since {
        // A session with NO timestamp survives the since-window: it is marked
        // "(no timestamp)" in the table (main.rs), never silently dated out —
        // aligns with COMMANDS.md "marked, never silently dated".
        sessions.retain(|s| s.updated_at.or(s.started_at).is_none_or(|t| t >= since));
    }
    if let Some(here) = here {
        sessions.retain(|s| {
            s.repo_path.as_deref().is_some_and(|p| {
                // Inferred repo paths were decoded from a lossy dir encoding
                // ('-' -> '/'), so compare those in the ENCODED space; exact
                // recorded cwds compare as real paths.
                if s.association == Association::Inferred {
                    cwd_nests_encoded(here, p)
                } else {
                    cwd_nests(here, p)
                }
            })
        });
    }
    sessions.sort_by(|a, b| {
        let ta = a.updated_at.or(a.started_at);
        let tb = b.updated_at.or(b.started_at);
        // Tie-break on session_id so equal timestamps order deterministically
        // (a stable result under `limit`).
        tb.cmp(&ta).then_with(|| a.session_id.cmp(&b.session_id))
    });
    if limit > 0 {
        sessions.truncate(limit);
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_session(dir: &Path, name: &str, lines: &[&str]) {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    fn temp_root(tag: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("aicx_sessions_test_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn discovers_claude_session_with_absolute_time_and_counts() {
        let root = temp_root("basic");
        let proj = root.join("-Users-silver-Git-transcript-builder");
        fs::create_dir_all(&proj).unwrap();
        write_session(
            &proj,
            "0eb1a73c-1234.jsonl",
            &[
                r#"{"type":"user","cwd":"/Users/silver/Git/transcript-builder","sessionId":"0eb1a73c","message":{"role":"user","content":"hej claude, pomóż mi"},"timestamp":"2026-06-08T01:42:13.000Z"}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"jasne"}]},"timestamp":"2026-06-08T01:42:23.000Z"}"#,
                r#"{"type":"user","message":{"role":"user","content":"dzięki"},"timestamp":"2026-06-08T01:45:00.000Z"}"#,
            ],
        );

        let sessions = discover_claude_sessions(&root, None, None);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.session_id, "0eb1a73c-1234");
        assert_eq!(s.agent, "claude");
        // absolute time: full date + year, not bare HH:MM:SS
        assert_eq!(s.temporal_confidence, TemporalConfidence::Full);
        assert_eq!(
            s.started_at.unwrap().to_rfc3339(),
            "2026-06-08T01:42:13+00:00"
        );
        assert_eq!(
            s.updated_at.unwrap().to_rfc3339(),
            "2026-06-08T01:45:00+00:00"
        );
        // exact association from the recorded cwd
        assert_eq!(s.association, Association::Exact);
        assert_eq!(
            s.repo_path.as_deref(),
            Some("/Users/silver/Git/transcript-builder")
        );
        assert_eq!(s.project.as_deref(), Some("transcript-builder"));
        // counts split user vs agent
        assert_eq!(s.message_count, 3);
        assert_eq!(s.user_message_count, 2);
        assert_eq!(s.agent_message_count, 1);
        assert_eq!(s.title.as_deref(), Some("hej claude, pomóż mi"));
    }

    #[test]
    fn serialized_session_exposes_rfc3339_timestamps() {
        let root = temp_root("serialize");
        let proj = root.join("-tmp-proj");
        fs::create_dir_all(&proj).unwrap();
        write_session(
            &proj,
            "s1.jsonl",
            &[
                r#"{"type":"user","message":{"role":"user","content":"x"},"timestamp":"2026-01-23T09:00:00.000Z"}"#,
            ],
        );
        let sessions = discover_claude_sessions(&root, None, None);
        let _ = fs::remove_dir_all(&root);
        let json = serde_json::to_string(&sessions[0]).unwrap();
        // year + full ISO date present in machine output (RFC3339, UTC as `Z`),
        // so recency is unambiguous — never a bare HH:MM:SS.
        assert!(json.contains("2026-01-23T09:00:00Z"), "json: {json}");
    }

    #[test]
    fn session_without_timestamps_is_marked_not_silently_full() {
        let root = temp_root("notime");
        let proj = root.join("-tmp-proj2");
        fs::create_dir_all(&proj).unwrap();
        write_session(
            &proj,
            "s2.jsonl",
            &[r#"{"type":"user","message":{"role":"user","content":"no time here"}}"#],
        );
        let sessions = discover_claude_sessions(&root, None, None);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(sessions[0].temporal_confidence, TemporalConfidence::Partial);
        assert!(sessions[0].started_at.is_none());
        // partial repo association inferred from the directory encoding
        assert_eq!(sessions[0].association, Association::Inferred);
    }

    #[test]
    fn cwd_prune_keeps_hyphenated_path_dirs() {
        // The project dir encodes cwd with '/'->'-'; a real hyphen (vc-workspace)
        // must NOT be mis-pruned by a lossy decode when matching --cwd.
        let root = temp_root("cwdprune");
        let proj = root.join("-Users-me-vc-workspace-aicx");
        fs::create_dir_all(&proj).unwrap();
        write_session(
            &proj,
            "h1.jsonl",
            &[
                r#"{"type":"user","cwd":"/Users/me/vc-workspace/aicx","message":{"role":"user","content":"x"},"timestamp":"2026-06-08T10:00:00.000Z"}"#,
            ],
        );

        // matching cwd (with the hyphen) keeps the session
        let kept = discover_claude_sessions(&root, None, Some("/Users/me/vc-workspace/aicx"));
        assert_eq!(kept.len(), 1, "hyphenated cwd must not be mis-pruned");

        // a different repo prunes the dir without reading
        let pruned = discover_claude_sessions(&root, None, Some("/Users/me/other-repo"));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(pruned.len(), 0);
    }

    #[test]
    fn find_session_by_id_locates_claude_by_filename() {
        let home = temp_root("findsession");
        let proj = home.join(".claude").join("projects").join("-tmp-proj");
        fs::create_dir_all(&proj).unwrap();
        write_session(
            &proj,
            "abc12345-dead-beef.jsonl",
            &[
                r#"{"type":"user","cwd":"/tmp/proj","message":{"role":"user","content":"hi"},"timestamp":"2026-06-08T10:00:00.000Z"}"#,
            ],
        );

        // located by a prefix, no other agent roots present (tolerated)
        let found = find_session_by_id(&home, "abc12345");
        let _ = fs::remove_dir_all(&home);
        let s = found.expect("session found by id prefix");
        assert_eq!(s.session_id, "abc12345-dead-beef");
        assert_eq!(s.agent, "claude");

        // unknown id -> None
        let home2 = temp_root("findsession2");
        fs::create_dir_all(&home2).unwrap();
        let none = find_session_by_id(&home2, "nope");
        let _ = fs::remove_dir_all(&home2);
        assert!(none.is_none());
    }

    #[test]
    fn discovers_gemini_session_from_whole_file_json() {
        let root = temp_root("gemini");
        let chats = root.join("myproj").join("chats");
        fs::create_dir_all(&chats).unwrap();
        let doc = r#"{
          "sessionId":"116b0791-gemini",
          "projectHash":"abc",
          "startTime":"2026-03-22T20:43:32.318Z",
          "lastUpdated":"2026-03-22T20:43:53.023Z",
          "messages":[
            {"id":"1","type":"user","content":"zrób raport"},
            {"id":"2","type":"gemini","content":"ok"},
            {"id":"3","type":"gemini","content":"gotowe"}
          ]
        }"#;
        fs::write(chats.join("session-2026-03-22T20-43.json"), doc).unwrap();

        let sessions = discover_gemini_sessions(&root, None, None);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.agent, "gemini");
        assert_eq!(s.session_id, "116b0791-gemini");
        assert_eq!(s.project.as_deref(), Some("myproj"));
        // type=user vs type=gemini split
        assert_eq!(s.user_message_count, 1);
        assert_eq!(s.agent_message_count, 2);
        assert_eq!(s.message_count, 3);
        assert_eq!(s.title.as_deref(), Some("zrób raport"));
        assert_eq!(s.temporal_confidence, TemporalConfidence::Full);
        assert_eq!(
            s.started_at.unwrap().to_rfc3339(),
            "2026-03-22T20:43:32.318+00:00"
        );
        // gemini records only a project hash, never the cwd
        assert!(s.repo_path.is_none());
    }

    #[test]
    fn discovers_codex_session_from_meta_and_message_stream() {
        let root = temp_root("codex");
        let day = root.join("2026").join("01").join("29");
        fs::create_dir_all(&day).unwrap();
        write_session(
            &day,
            "rollout-2026-01-29T13-58-09-019c09d5.jsonl",
            &[
                r#"{"timestamp":"2026-01-29T12:58:09.421Z","type":"session_meta","payload":{"id":"019c09d5-codex","cwd":"/Users/me/hosted/VetCoders"}}"#,
                r#"{"timestamp":"2026-01-29T12:58:10.000Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"text":"bootstrap"}]}}"#,
                r#"{"timestamp":"2026-01-29T12:59:00.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"zrób to"}]}}"#,
                r#"{"timestamp":"2026-01-29T13:00:00.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"text":"robię"}]}}"#,
            ],
        );
        let sessions = discover_codex_sessions(&root, None);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.agent, "codex");
        // canonical id from session_meta.payload.id, not the filename
        assert_eq!(s.session_id, "019c09d5-codex");
        assert_eq!(s.project.as_deref(), Some("VetCoders"));
        assert_eq!(s.association, Association::Exact);
        // developer row is NOT conversation — only user + assistant counted
        assert_eq!(s.user_message_count, 1);
        assert_eq!(s.agent_message_count, 1);
        assert_eq!(s.message_count, 2);
        assert_eq!(s.title.as_deref(), Some("zrób to"));
        assert_eq!(s.temporal_confidence, TemporalConfidence::Full);
        assert_eq!(
            s.started_at.unwrap().to_rfc3339(),
            "2026-01-29T12:58:09.421+00:00"
        );
    }

    fn mk_info(id: &str, repo: Option<&str>, updated: &str) -> SessionInfo {
        SessionInfo {
            session_id: id.to_string(),
            agent: "claude".to_string(),
            project: repo.and_then(project_label_from_cwd),
            repo_path: repo.map(str::to_string),
            started_at: None,
            updated_at: Some(
                DateTime::parse_from_rfc3339(updated)
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            message_count: 1,
            user_message_count: 1,
            agent_message_count: 0,
            title: None,
            source_path: PathBuf::from(format!("/x/{id}.jsonl")),
            association: Association::Exact,
            temporal_confidence: TemporalConfidence::Full,
        }
    }

    #[test]
    fn select_sessions_filters_by_cwd_and_sorts_newest_first() {
        let here = "/Users/me/repo-a";
        let sessions = vec![
            mk_info("aaa", Some("/Users/me/repo-a"), "2026-06-01T10:00:00Z"),
            mk_info("bbb", Some("/Users/me/repo-a"), "2026-06-08T10:00:00Z"),
            mk_info("ccc", Some("/Users/me/repo-b"), "2026-06-09T10:00:00Z"),
        ];

        // cwd filter keeps only repo-a, newest first
        let got = select_sessions(sessions.clone(), Some(here), None, None, 0);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].session_id, "bbb", "newest first");
        assert_eq!(got[1].session_id, "aaa");

        // limit clips after sort
        let limited = select_sessions(sessions.clone(), Some(here), None, None, 1);
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].session_id, "bbb");

        // no cwd filter -> all three, ccc (2026-06-09) newest
        let all = select_sessions(sessions.clone(), None, None, None, 0);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].session_id, "ccc");

        // since filter drops anything updated before the bound
        let recent = select_sessions(
            sessions,
            None,
            None,
            Some(
                DateTime::parse_from_rfc3339("2026-06-08T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            0,
        );
        assert_eq!(recent.len(), 2, "only bbb (06-08) and ccc (06-09) survive");
        assert!(recent.iter().all(|s| s.session_id != "aaa"));
    }

    #[test]
    fn cwd_nests_requires_segment_boundary() {
        // sibling with a shared prefix must NOT nest (both directions)
        assert!(!cwd_nests("/a/repo", "/a/repo-backup"));
        assert!(!cwd_nests("/a/repo-backup", "/a/repo"));
        assert!(!cwd_nests("/a/repository", "/a/repo"));
        // genuine nesting still works (both directions)
        assert!(cwd_nests("/a/repo/sub", "/a/repo"));
        assert!(cwd_nests("/a/repo", "/a/repo/sub"));
        // equality, case-insensitivity, trailing slash
        assert!(cwd_nests("/a/Repo", "/a/repo"));
        assert!(cwd_nests("/a/repo/", "/a/repo"));
    }

    #[test]
    fn cwd_prune_requires_encoded_segment_boundary() {
        // A dir whose encoded name merely shares a string prefix with the
        // encoded --cwd must be pruned: `repo` is not `repository`.
        let root = temp_root("cwdprune_boundary");
        let sibling = root.join("-Users-me-repository");
        fs::create_dir_all(&sibling).unwrap();
        write_session(
            &sibling,
            "x1.jsonl",
            &[
                r#"{"type":"user","cwd":"/Users/me/repository","message":{"role":"user","content":"x"},"timestamp":"2026-06-08T10:00:00.000Z"}"#,
            ],
        );
        let nested = root.join("-Users-me-repo-sub");
        fs::create_dir_all(&nested).unwrap();
        write_session(
            &nested,
            "x2.jsonl",
            &[
                r#"{"type":"user","cwd":"/Users/me/repo/sub","message":{"role":"user","content":"y"},"timestamp":"2026-06-08T11:00:00.000Z"}"#,
            ],
        );

        let got = discover_claude_sessions(&root, None, Some("/Users/me/repo"));
        let _ = fs::remove_dir_all(&root);
        assert_eq!(got.len(), 1, "repository pruned, repo/sub kept");
        assert_eq!(got[0].session_id, "x2");
    }

    #[test]
    fn select_sessions_keeps_sessions_without_timestamp_in_since_window() {
        // "marked, never silently dated": a no-timestamp session survives
        // --since (main.rs renders it as "(no timestamp)").
        let mut no_time = mk_info("notime", Some("/Users/me/repo-a"), "2026-06-01T10:00:00Z");
        no_time.updated_at = None;
        no_time.started_at = None;
        no_time.temporal_confidence = TemporalConfidence::None;
        let sessions = vec![
            no_time,
            mk_info("old", Some("/Users/me/repo-a"), "2026-06-01T10:00:00Z"),
            mk_info("new", Some("/Users/me/repo-a"), "2026-06-08T10:00:00Z"),
        ];
        let got = select_sessions(
            sessions,
            None,
            None,
            Some(
                DateTime::parse_from_rfc3339("2026-06-07T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            0,
        );
        let ids: Vec<&str> = got.iter().map(|s| s.session_id.as_str()).collect();
        assert!(ids.contains(&"notime"), "no-timestamp session survives");
        assert!(ids.contains(&"new"));
        assert!(!ids.contains(&"old"), "dated-out session is dropped");
        // no-timestamp sorts after dated sessions (None < Some, newest first)
        assert_eq!(ids.last(), Some(&"notime"));
    }

    #[test]
    fn select_sessions_sort_is_deterministic_on_equal_timestamps() {
        let sessions = vec![
            mk_info("zzz", None, "2026-06-08T10:00:00Z"),
            mk_info("aaa", None, "2026-06-08T10:00:00Z"),
            mk_info("mmm", None, "2026-06-08T10:00:00Z"),
        ];
        let got = select_sessions(sessions, None, None, None, 0);
        let ids: Vec<&str> = got.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, vec!["aaa", "mmm", "zzz"], "session_id tie-break");
        // limit now clips a stable head
        let sessions = vec![
            mk_info("zzz", None, "2026-06-08T10:00:00Z"),
            mk_info("aaa", None, "2026-06-08T10:00:00Z"),
        ];
        let one = select_sessions(sessions, None, None, None, 1);
        assert_eq!(one[0].session_id, "aaa");
    }

    #[test]
    fn select_sessions_matches_inferred_repo_in_encoded_space() {
        // Inferred repo_path comes from the lossy dir decode: the real cwd
        // /Users/me/vc-workspace/aicx decodes to /Users/me/vc/workspace/aicx.
        // Encoded-space matching must still associate it with --cwd.
        let mut inferred = mk_info(
            "inf",
            Some("/Users/me/vc/workspace/aicx"),
            "2026-06-08T10:00:00Z",
        );
        inferred.association = Association::Inferred;
        let got = select_sessions(
            vec![inferred.clone()],
            Some("/Users/me/vc-workspace/aicx"),
            None,
            None,
            0,
        );
        assert_eq!(got.len(), 1, "inferred match happens in encoded space");
        // and a non-matching cwd still drops it
        let none = select_sessions(vec![inferred], Some("/Users/me/other"), None, None, 0);
        assert!(none.is_empty());
    }

    #[test]
    fn garbage_only_file_reports_temporal_confidence_none() {
        // claude: no parsable record + no timestamp -> None, not Partial
        let root = temp_root("garbage_claude");
        let proj = root.join("-tmp-garbage");
        fs::create_dir_all(&proj).unwrap();
        write_session(&proj, "g1.jsonl", &["this is not json", "neither is this"]);
        let sessions = discover_claude_sessions(&root, None, None);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].temporal_confidence, TemporalConfidence::None);

        // codex: same rule
        let root = temp_root("garbage_codex");
        let day = root.join("2026").join("01").join("29");
        fs::create_dir_all(&day).unwrap();
        write_session(&day, "rollout-garbage.jsonl", &["{{{ not json"]);
        let sessions = discover_codex_sessions(&root, None);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].temporal_confidence, TemporalConfidence::None);
    }

    #[test]
    fn gemini_session_without_any_time_signal_reports_none() {
        let root = temp_root("gemini_notime");
        let chats = root.join("proj").join("chats");
        fs::create_dir_all(&chats).unwrap();
        // no startTime, no lastUpdated, no per-message timestamps -> None
        let no_time =
            r#"{"sessionId":"g-1","messages":[{"id":"1","type":"user","content":"hej"}]}"#;
        fs::write(chats.join("session-a.json"), no_time).unwrap();
        // only a per-message timestamp -> Partial (some signal, not absolute header time)
        let msg_time = r#"{"sessionId":"g-2","messages":[{"id":"1","type":"user","content":"hej","timestamp":"2026-06-08T10:00:00.000Z"}]}"#;
        fs::write(chats.join("session-b.json"), msg_time).unwrap();

        let mut sessions = discover_gemini_sessions(&root, None, None);
        let _ = fs::remove_dir_all(&root);
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "g-1");
        assert_eq!(sessions[0].temporal_confidence, TemporalConfidence::None);
        assert_eq!(sessions[1].session_id, "g-2");
        assert_eq!(sessions[1].temporal_confidence, TemporalConfidence::Partial);
    }

    #[test]
    fn find_session_by_id_codex_matches_id_segment_not_timestamp() {
        let home = temp_root("codexfind");
        let day = home.join(".codex").join("sessions").join("2026").join("01");
        fs::create_dir_all(&day).unwrap();
        write_session(
            &day,
            "rollout-2026-01-29T13-58-09-019c1111-aaaa.jsonl",
            &[
                r#"{"timestamp":"2026-01-29T12:58:09.421Z","type":"session_meta","payload":{"id":"019c1111-aaaa","cwd":"/tmp/a"}}"#,
            ],
        );
        write_session(
            &day,
            "rollout-2026-01-29T14-00-00-019c2222-bbbb.jsonl",
            &[
                r#"{"timestamp":"2026-01-29T13:00:00.000Z","type":"session_meta","payload":{"id":"019c2222-bbbb","cwd":"/tmp/b"}}"#,
            ],
        );

        // exact id-segment prefix resolves the right rollout
        let found = find_session_by_id(&home, "019c2222");
        assert_eq!(
            found.expect("found by id segment").session_id,
            "019c2222-bbbb"
        );
        // a timestamp-shaped prefix must NOT match (old `contains` would have)
        assert!(find_session_by_id(&home, "2026").is_none());
        // ambiguous prefix resolves deterministically (sorted, first wins)
        let ambiguous = find_session_by_id(&home, "019c");
        assert_eq!(
            ambiguous
                .expect("ambiguous prefix still resolves")
                .session_id,
            "019c1111-aaaa"
        );
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn discovers_junie_session_with_compact_timestamps_and_counts() {
        let root = temp_root("junie");
        let session_dir = root.join("session-260408-214715-abcd");
        fs::create_dir_all(&session_dir).unwrap();
        write_session(
            &session_dir,
            "events.jsonl",
            &[
                r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"CurrentDirectoryUpdatedEvent","currentDirectory":"/tmp/repo"}}}"#,
                r#"{"kind":"UserPromptEvent","requestId":"prompt-260408-214823-br8l","prompt":"vc-init","presentablePrompt":"vc-init"}"#,
                r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"step-1","result":"Initial plan"}}}"#,
                r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"step-1","result":"Initial plan"}}}"#,
                r#"{"kind":"UserResponseEvent","prompt":"jedziemy","isChoice":true}"#,
                r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"step-1","result":"Refined plan"}}}"#,
            ],
        );

        let sessions = discover_junie_sessions(&root, None);
        let _ = fs::remove_dir_all(&root);

        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.agent, "junie");
        // id = dir name minus the `session-` prefix (extractor contract)
        assert_eq!(s.session_id, "260408-214715-abcd");
        // absolute time from dir-name anchor + prompt request id
        assert_eq!(s.temporal_confidence, TemporalConfidence::Full);
        assert_eq!(
            s.started_at.unwrap().to_rfc3339(),
            "2026-04-08T21:47:15+00:00"
        );
        assert_eq!(
            s.updated_at.unwrap().to_rfc3339(),
            "2026-04-08T21:48:23+00:00"
        );
        // cwd from CurrentDirectoryUpdatedEvent
        assert_eq!(s.repo_path.as_deref(), Some("/tmp/repo"));
        assert_eq!(s.project.as_deref(), Some("repo"));
        assert_eq!(s.association, Association::Exact);
        // duplicate streaming Result snapshot deduped; changed text counted
        assert_eq!(s.user_message_count, 2);
        assert_eq!(s.agent_message_count, 2);
        assert_eq!(s.message_count, 4);
        assert_eq!(s.title.as_deref(), Some("vc-init"));
    }

    #[test]
    fn junie_meta_prompt_is_not_an_operator_message_or_title() {
        let root = temp_root("junie_meta");
        let session_dir = root.join("session-260605-183024-junie");
        fs::create_dir_all(&session_dir).unwrap();
        write_session(
            &session_dir,
            "events.jsonl",
            &[
                r#"{"kind":"UserPromptEvent","requestId":"prompt-260605-183101-meta1","prompt":"Implement the suggested plan","customAttachments":[{"kind":"PlanAttachment"}]}"#,
                r#"{"kind":"UserPromptEvent","requestId":"prompt-260605-183102-real1","prompt":"real operator question"}"#,
            ],
        );
        let sessions = discover_junie_sessions(&root, None);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.user_message_count, 1, "meta prompt not counted");
        assert_eq!(s.title.as_deref(), Some("real operator question"));
    }

    #[test]
    fn junie_garbage_only_file_reports_temporal_none_without_anchor() {
        let root = temp_root("junie_garbage");
        // No parseable compact timestamp in the dir suffix, garbage content.
        let session_dir = root.join("session-opaque");
        fs::create_dir_all(&session_dir).unwrap();
        write_session(&session_dir, "events.jsonl", &["{{{ not json"]);
        let sessions = discover_junie_sessions(&root, None);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].temporal_confidence, TemporalConfidence::None);
        assert!(sessions[0].started_at.is_none());
    }

    #[test]
    fn find_session_by_id_locates_junie_by_dir_name() {
        let home = temp_root("junie_find");
        let session_dir = home
            .join(".junie")
            .join("sessions")
            .join("session-260408-214715-abcd");
        fs::create_dir_all(&session_dir).unwrap();
        write_session(
            &session_dir,
            "events.jsonl",
            &[
                r#"{"kind":"UserPromptEvent","requestId":"prompt-260408-214823-br8l","prompt":"hej"}"#,
            ],
        );
        let found = find_session_by_id(&home, "260408-214715");
        let _ = fs::remove_dir_all(&home);
        let s = found.expect("junie session found by id prefix");
        assert_eq!(s.agent, "junie");
        assert_eq!(s.session_id, "260408-214715-abcd");
    }

    #[test]
    fn find_session_by_id_claude_ambiguous_prefix_resolves_deterministically() {
        let home = temp_root("claude_ambiguous");
        // Two sessions sharing the `abc1` prefix in DIFFERENT project dirs:
        // fs::read_dir order is unspecified, so without sorting the winner
        // would be arbitrary. Sorted by path, `-proj-a` wins.
        let proj_a = home.join(".claude").join("projects").join("-proj-a");
        let proj_b = home.join(".claude").join("projects").join("-proj-b");
        fs::create_dir_all(&proj_a).unwrap();
        fs::create_dir_all(&proj_b).unwrap();
        write_session(
            &proj_a,
            "abc1zzzz-in-a.jsonl",
            &[
                r#"{"type":"user","cwd":"/proj/a","message":{"role":"user","content":"a"},"timestamp":"2026-06-08T10:00:00.000Z"}"#,
            ],
        );
        write_session(
            &proj_b,
            "abc1aaaa-in-b.jsonl",
            &[
                r#"{"type":"user","cwd":"/proj/b","message":{"role":"user","content":"b"},"timestamp":"2026-06-08T11:00:00.000Z"}"#,
            ],
        );

        let found = find_session_by_id(&home, "abc1");
        let _ = fs::remove_dir_all(&home);
        let s = found.expect("ambiguous prefix still resolves");
        // Lexicographically first PATH wins (-proj-a sorts before -proj-b),
        // independent of read_dir enumeration order.
        assert_eq!(s.session_id, "abc1zzzz-in-a");
        assert_eq!(s.repo_path.as_deref(), Some("/proj/a"));
    }

    #[test]
    fn find_session_by_id_gemini_ambiguous_prefix_resolves_deterministically() {
        let home = temp_root("gemini_ambiguous");
        let chats = home.join(".gemini").join("tmp").join("proj").join("chats");
        fs::create_dir_all(&chats).unwrap();
        // Same `g-abc1` prefix; sorted by session id, `g-abc1-aaa` must win
        // even though its file name sorts AFTER the other one.
        fs::write(
            chats.join("session-1.json"),
            r#"{"sessionId":"g-abc1-zzz","messages":[{"id":"1","type":"user","content":"z"}]}"#,
        )
        .unwrap();
        fs::write(
            chats.join("session-2.json"),
            r#"{"sessionId":"g-abc1-aaa","messages":[{"id":"1","type":"user","content":"a"}]}"#,
        )
        .unwrap();

        let found = find_session_by_id(&home, "g-abc1");
        let _ = fs::remove_dir_all(&home);
        let s = found.expect("ambiguous prefix still resolves");
        assert_eq!(s.session_id, "g-abc1-aaa", "smallest session id wins");
    }

    /// Make `path` unreadable (mode 000). Returns false when permissions are
    /// not enforced (e.g. running as root) — callers then skip the assertion.
    #[cfg(unix)]
    fn make_unreadable(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
        fs::read_to_string(path).is_err()
    }

    #[cfg(unix)]
    #[test]
    fn claude_unreadable_file_is_skipped_not_fatal() {
        let root = temp_root("unreadable_claude");
        let proj = root.join("-tmp-proj");
        fs::create_dir_all(&proj).unwrap();
        write_session(
            &proj,
            "good.jsonl",
            &[
                r#"{"type":"user","message":{"role":"user","content":"ok"},"timestamp":"2026-06-08T10:00:00.000Z"}"#,
            ],
        );
        write_session(&proj, "locked.jsonl", &["{}"]);
        if !make_unreadable(&proj.join("locked.jsonl")) {
            let _ = fs::remove_dir_all(&root);
            return; // root: permissions not enforced, nothing to verify
        }
        let sessions = discover_claude_sessions(&root, None, None);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(sessions.len(), 1, "unreadable file skipped, scan survives");
        assert_eq!(sessions[0].session_id, "good");
    }

    #[cfg(unix)]
    #[test]
    fn codex_unreadable_file_is_skipped_not_fatal() {
        let root = temp_root("unreadable_codex");
        let day = root.join("2026").join("01").join("29");
        fs::create_dir_all(&day).unwrap();
        write_session(
            &day,
            "rollout-good.jsonl",
            &[
                r#"{"timestamp":"2026-01-29T12:58:09.421Z","type":"session_meta","payload":{"id":"good-codex","cwd":"/tmp/a"}}"#,
            ],
        );
        write_session(&day, "rollout-locked.jsonl", &["{}"]);
        if !make_unreadable(&day.join("rollout-locked.jsonl")) {
            let _ = fs::remove_dir_all(&root);
            return;
        }
        let sessions = discover_codex_sessions(&root, None);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(sessions.len(), 1, "unreadable file skipped, scan survives");
        assert_eq!(sessions[0].session_id, "good-codex");
    }

    #[cfg(unix)]
    #[test]
    fn gemini_unreadable_file_is_skipped_not_fatal() {
        let root = temp_root("unreadable_gemini");
        let chats = root.join("proj").join("chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(
            chats.join("session-good.json"),
            r#"{"sessionId":"g-good","messages":[{"id":"1","type":"user","content":"hej"}]}"#,
        )
        .unwrap();
        fs::write(chats.join("session-locked.json"), "{}").unwrap();
        if !make_unreadable(&chats.join("session-locked.json")) {
            let _ = fs::remove_dir_all(&root);
            return;
        }
        let sessions = discover_gemini_sessions(&root, None, None);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(sessions.len(), 1, "unreadable file skipped, scan survives");
        assert_eq!(sessions[0].session_id, "g-good");
    }

    #[cfg(unix)]
    #[test]
    fn junie_unreadable_events_file_is_skipped_not_fatal() {
        let root = temp_root("unreadable_junie");
        let good = root.join("session-260408-214715-good");
        fs::create_dir_all(&good).unwrap();
        write_session(
            &good,
            "events.jsonl",
            &[
                r#"{"kind":"UserPromptEvent","requestId":"prompt-260408-214823-br8l","prompt":"hej"}"#,
            ],
        );
        let locked = root.join("session-260408-220000-lock");
        fs::create_dir_all(&locked).unwrap();
        write_session(&locked, "events.jsonl", &["{}"]);
        if !make_unreadable(&locked.join("events.jsonl")) {
            let _ = fs::remove_dir_all(&root);
            return;
        }
        let sessions = discover_junie_sessions(&root, None);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(sessions.len(), 1, "unreadable file skipped, scan survives");
        assert_eq!(sessions[0].session_id, "260408-214715-good");
    }

    #[cfg(unix)]
    #[test]
    fn codex_walk_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;
        let home = temp_root("codex_symlink_home");
        let root = home.join(".codex").join("sessions");
        fs::create_dir_all(&root).unwrap();
        // A real rollout OUTSIDE the scan root, reachable only via symlinks.
        let outside = temp_root("codex_symlink_outside");
        write_session(
            &outside,
            "rollout-2026-01-29T13-58-09-019c09d5.jsonl",
            &[
                r#"{"timestamp":"2026-01-29T12:58:09.421Z","type":"session_meta","payload":{"id":"019c09d5-ext","cwd":"/tmp/x"}}"#,
            ],
        );
        // Cyclic dir symlink: following it would loop until the depth cap.
        symlink(&root, root.join("loop")).unwrap();
        // Dir symlink escaping the root + direct file symlink to a .jsonl.
        symlink(&outside, root.join("ext")).unwrap();
        symlink(
            outside.join("rollout-2026-01-29T13-58-09-019c09d5.jsonl"),
            root.join("rollout-2026-01-29T13-58-09-019c09d5.jsonl"),
        )
        .unwrap();

        // Discovery terminates and collects NOTHING through a symlink.
        let sessions = discover_codex_sessions(&root, None);
        assert!(
            sessions.is_empty(),
            "no session may arrive through a symlink: {sessions:?}"
        );
        // The find-by-id walk obeys the same rule.
        assert!(find_session_by_id(&home, "019c09d5").is_none());
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn codex_walk_caps_depth() {
        let home = temp_root("codex_depth_home");
        let root = home.join(".codex").join("sessions");
        // A rollout at the deepest scanned level (MAX_CODEX_SCAN_DEPTH dirs
        // below the root) is still found...
        let mut in_cap = root.clone();
        for i in 0..MAX_CODEX_SCAN_DEPTH {
            in_cap = in_cap.join(format!("d{i}"));
        }
        fs::create_dir_all(&in_cap).unwrap();
        write_session(
            &in_cap,
            "rollout-2026-01-29T13-58-09-incap1111.jsonl",
            &[
                r#"{"timestamp":"2026-01-29T12:58:09.421Z","type":"session_meta","payload":{"id":"incap1111","cwd":"/tmp/a"}}"#,
            ],
        );
        // ...one level deeper is beyond the cap and must NOT be found.
        let mut beyond = root.clone();
        for i in 0..(MAX_CODEX_SCAN_DEPTH + 1) {
            beyond = beyond.join(format!("e{i}"));
        }
        fs::create_dir_all(&beyond).unwrap();
        write_session(
            &beyond,
            "rollout-2026-01-29T14-00-00-beyond2222.jsonl",
            &[
                r#"{"timestamp":"2026-01-29T13:00:00.000Z","type":"session_meta","payload":{"id":"beyond2222","cwd":"/tmp/b"}}"#,
            ],
        );

        let sessions = discover_codex_sessions(&root, None);
        assert_eq!(sessions.len(), 1, "only the in-cap rollout is discovered");
        assert_eq!(sessions[0].session_id, "incap1111");
        // The find-by-id walk applies the same cap.
        assert!(find_session_by_id(&home, "incap1111").is_some());
        assert!(find_session_by_id(&home, "beyond2222").is_none());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_rollout_id_segment_parses_rollout_shape() {
        assert_eq!(
            codex_rollout_id_segment("rollout-2026-01-29T13-58-09-019c09d5-aaaa"),
            Some("019c09d5-aaaa")
        );
        // non-rollout shapes fall back to None (whole-stem match applies)
        assert_eq!(codex_rollout_id_segment("rollout-garbage"), None);
        assert_eq!(codex_rollout_id_segment("some-other-file"), None);
        assert_eq!(
            codex_rollout_id_segment("rollout-2026-01-29T13-58-09-"),
            None
        );
    }
}
