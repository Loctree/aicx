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

use chrono::{DateTime, Utc};
use serde::Serialize;

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
/// not exact truth — callers mark the association accordingly. The session's own
/// `cwd` field, when present, is always preferred over this decode.
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
pub fn discover_claude_sessions(projects_root: &Path) -> Vec<SessionInfo> {
    let mut out = Vec::new();
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

        let Ok(files) = fs::read_dir(&dir_path) else {
            continue;
        };
        for file_entry in files.flatten() {
            let path = file_entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(info) = scan_claude_session_file(&path, decoded_cwd.as_deref()) {
                out.push(info);
            }
        }
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
    let mut saw_any_line = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        saw_any_line = true;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

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
    } else if saw_any_line {
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
    limit: usize,
) -> Vec<SessionInfo> {
    if let Some(agent) = agent {
        sessions.retain(|s| s.agent == agent);
    }
    if let Some(here) = here {
        sessions.retain(|s| {
            s.repo_path
                .as_deref()
                .is_some_and(|p| here == p || here.starts_with(p) || p.starts_with(here))
        });
    }
    sessions.sort_by(|a, b| {
        let ta = a.updated_at.or(a.started_at);
        let tb = b.updated_at.or(b.started_at);
        tb.cmp(&ta)
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

        let sessions = discover_claude_sessions(&root);
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
        let sessions = discover_claude_sessions(&root);
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
        let sessions = discover_claude_sessions(&root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(sessions[0].temporal_confidence, TemporalConfidence::Partial);
        assert!(sessions[0].started_at.is_none());
        // partial repo association inferred from the directory encoding
        assert_eq!(sessions[0].association, Association::Inferred);
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
        let got = select_sessions(sessions.clone(), Some(here), None, 0);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].session_id, "bbb", "newest first");
        assert_eq!(got[1].session_id, "aaa");

        // limit clips after sort
        let limited = select_sessions(sessions.clone(), Some(here), None, 1);
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].session_id, "bbb");

        // no cwd filter -> all three, ccc (2026-06-09) newest
        let all = select_sessions(sessions, None, None, 0);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].session_id, "ccc");
    }
}
