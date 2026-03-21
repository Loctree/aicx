//! Semantic segmentation for canonical store ownership.
//!
//! Reconstructs repository-scoped session segments from content signals rather
//! than weak source-side identifiers.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use crate::sources::TimelineEntry;
use crate::store::Kind;
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoIdentity {
    pub organization: String,
    pub repository: String,
}

impl RepoIdentity {
    pub fn slug(&self) -> String {
        format!("{}/{}", self.organization, self.repository)
    }
}

#[derive(Debug, Clone)]
pub struct SemanticSegment {
    pub repo: Option<RepoIdentity>,
    pub kind: Kind,
    pub agent: String,
    pub session_id: String,
    pub entries: Vec<TimelineEntry>,
}

impl SemanticSegment {
    pub fn project_label(&self) -> String {
        self.repo
            .as_ref()
            .map(RepoIdentity::slug)
            .unwrap_or_else(|| "non-repository-contexts".to_string())
    }
}

pub fn semantic_segments(entries: &[TimelineEntry]) -> Vec<SemanticSegment> {
    let mut sessions: HashMap<(String, String), Vec<TimelineEntry>> = HashMap::new();
    for entry in entries {
        sessions
            .entry((entry.agent.clone(), entry.session_id.clone()))
            .or_default()
            .push(entry.clone());
    }

    let mut ordered = Vec::new();

    for ((agent, session_id), mut session_entries) in sessions {
        session_entries.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));

        let mut current_repo: Option<RepoIdentity> = None;
        let mut current_entries: Vec<TimelineEntry> = Vec::new();

        for entry in session_entries {
            let explicit_repo = infer_repo_identity_from_entry(&entry);

            let split_for_first_truth =
                !current_entries.is_empty() && current_repo.is_none() && explicit_repo.is_some();
            let split_for_context_switch = !current_entries.is_empty()
                && explicit_repo
                    .as_ref()
                    .zip(current_repo.as_ref())
                    .is_some_and(|(next_repo, active_repo)| next_repo != active_repo);

            if split_for_first_truth || split_for_context_switch {
                ordered.push(build_segment(
                    current_repo.take(),
                    &agent,
                    &session_id,
                    std::mem::take(&mut current_entries),
                ));
            }

            if current_entries.is_empty() {
                current_repo = explicit_repo.clone();
            }

            if current_repo.is_none() && explicit_repo.is_some() {
                current_repo = explicit_repo.clone();
            }

            current_entries.push(entry);
        }

        if !current_entries.is_empty() {
            ordered.push(build_segment(
                current_repo,
                &agent,
                &session_id,
                current_entries,
            ));
        }
    }

    ordered.sort_by(|left, right| {
        left.entries
            .first()
            .map(|entry| entry.timestamp)
            .cmp(&right.entries.first().map(|entry| entry.timestamp))
            .then_with(|| left.agent.cmp(&right.agent))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });

    ordered
}

pub fn infer_repo_identity_from_entry(entry: &TimelineEntry) -> Option<RepoIdentity> {
    infer_repo_identity_from_text(&entry.message)
        .or_else(|| infer_repo_identity_from_cwd(entry.cwd.as_deref()))
}

fn build_segment(
    repo: Option<RepoIdentity>,
    agent: &str,
    session_id: &str,
    entries: Vec<TimelineEntry>,
) -> SemanticSegment {
    let kind = classify_segment_kind(&entries);
    SemanticSegment {
        repo,
        kind,
        agent: agent.to_string(),
        session_id: session_id.to_string(),
        entries,
    }
}

fn classify_segment_kind(entries: &[TimelineEntry]) -> Kind {
    if entries.is_empty() {
        return Kind::Other;
    }

    let has_conversation = entries
        .iter()
        .any(|entry| entry.role == "user" || entry.role == "assistant");

    let report_score = entries
        .iter()
        .map(|entry| classify_report_signal(entry.message.as_str()))
        .sum::<u8>();
    let plan_score = entries
        .iter()
        .map(|entry| classify_plan_signal(entry.message.as_str()))
        .sum::<u8>();

    if report_score >= 2 && report_score > plan_score && !has_conversation {
        Kind::Reports
    } else if plan_score >= 2 && plan_score >= report_score {
        Kind::Plans
    } else if has_conversation {
        Kind::Conversations
    } else if report_score > 0 {
        Kind::Reports
    } else {
        Kind::Other
    }
}

fn classify_plan_signal(message: &str) -> u8 {
    let lower = message.to_ascii_lowercase();
    u8::from(lower.contains("goal:"))
        + u8::from(lower.contains("acceptance:"))
        + u8::from(lower.contains("test gate:"))
        + u8::from(lower.contains("- [ ]"))
        + u8::from(lower.contains("plan:"))
        + u8::from(lower.contains("migration plan"))
}

fn classify_report_signal(message: &str) -> u8 {
    let lower = message.to_ascii_lowercase();
    u8::from(lower.contains("recovery report"))
        + u8::from(lower.contains("audit report"))
        + u8::from(lower.contains("coverage report"))
        + u8::from(lower.contains("status report"))
        + u8::from(lower.contains("summary"))
}

fn infer_repo_identity_from_cwd(cwd: Option<&str>) -> Option<RepoIdentity> {
    let cwd = cwd?.trim();
    if cwd.is_empty() || looks_like_weak_source_identifier(cwd) {
        return None;
    }

    if let Some(repo) = infer_repo_identity_from_remote_like(cwd) {
        return Some(repo);
    }

    let path = expand_home(cwd);
    infer_repo_identity_from_path(&path)
}

fn infer_repo_identity_from_text(text: &str) -> Option<RepoIdentity> {
    if let Some(repo) = infer_repo_identity_from_remote_like(text) {
        return Some(repo);
    }

    let path_re = Regex::new(r"(/[A-Za-z0-9._~\-]+(?:/[A-Za-z0-9._~\-]+)+)").ok()?;
    for capture in path_re.captures_iter(text) {
        let path = capture.get(1)?.as_str();
        if let Some(repo) = infer_repo_identity_from_path(&PathBuf::from(path)) {
            return Some(repo);
        }
    }

    None
}

fn infer_repo_identity_from_path(path: &Path) -> Option<RepoIdentity> {
    if let Some(repo) = infer_repo_identity_from_local_git(path) {
        return Some(repo);
    }

    infer_repo_identity_from_known_layout(path)
}

fn infer_repo_identity_from_local_git(path: &Path) -> Option<RepoIdentity> {
    let repo_root = discover_git_root(path)?;
    infer_repo_identity_from_git_remote(&repo_root)
        .or_else(|| infer_repo_identity_from_known_layout(&repo_root))
        .or_else(|| {
            repo_root.file_name().map(|name| RepoIdentity {
                organization: "local".to_string(),
                repository: name.to_string_lossy().to_string(),
            })
        })
}

fn discover_git_root(path: &Path) -> Option<PathBuf> {
    let seed = if path.is_file() {
        path.parent()?.to_path_buf()
    } else {
        path.to_path_buf()
    };

    seed.ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
}

fn infer_repo_identity_from_git_remote(repo_root: &Path) -> Option<RepoIdentity> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let remote = String::from_utf8_lossy(&output.stdout);
    infer_repo_identity_from_remote_like(remote.trim())
}

fn infer_repo_identity_from_known_layout(path: &Path) -> Option<RepoIdentity> {
    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect();

    for marker in ["hosted", "repos", "repositories", "github", "git"] {
        let marker_index = components
            .iter()
            .position(|component| component == marker)?;
        if components.len() > marker_index + 2 {
            let organization = components[marker_index + 1].clone();
            let repository = components[marker_index + 2].clone();
            if is_probably_repo_name(&organization) && is_probably_repo_name(&repository) {
                return Some(RepoIdentity {
                    organization,
                    repository,
                });
            }
        }
    }

    None
}

fn infer_repo_identity_from_remote_like(raw: &str) -> Option<RepoIdentity> {
    for token in raw.split_whitespace() {
        let trimmed = token
            .trim_matches(|ch: char| matches!(ch, '"' | '\'' | ',' | '.' | ')' | '(' | '[' | ']'));
        for prefix in [
            "https://github.com/",
            "http://github.com/",
            "https://gitlab.com/",
            "http://gitlab.com/",
            "git@github.com:",
            "git@gitlab.com:",
        ] {
            if let Some(rest) = trimmed.strip_prefix(prefix)
                && let Some(repo) = repo_identity_from_remote_path(rest)
            {
                return Some(repo);
            }
        }
    }

    None
}

fn repo_identity_from_remote_path(path: &str) -> Option<RepoIdentity> {
    let mut parts = path.split('/');
    let organization = parts.next()?.trim();
    let repository = parts.next()?.trim().trim_end_matches(".git");
    if !is_probably_repo_name(organization) || !is_probably_repo_name(repository) {
        return None;
    }

    Some(RepoIdentity {
        organization: organization.to_string(),
        repository: repository.to_string(),
    })
}

fn looks_like_weak_source_identifier(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.len() >= 16
        && trimmed.chars().all(|ch| ch.is_ascii_hexdigit())
        && !trimmed.contains('/')
        && !trimmed.contains(':')
}

fn expand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }

    PathBuf::from(raw)
}

fn is_probably_repo_name(value: &str) -> bool {
    !value.is_empty()
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "tmp" | "temp" | "src" | "app" | "lib" | "docs" | "workspace" | "workspaces"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::fs;

    fn entry(
        ts: (i32, u32, u32, u32, u32, u32),
        session_id: &str,
        role: &str,
        message: &str,
        cwd: Option<&str>,
    ) -> TimelineEntry {
        TimelineEntry {
            timestamp: Utc
                .with_ymd_and_hms(ts.0, ts.1, ts.2, ts.3, ts.4, ts.5)
                .unwrap(),
            agent: "claude".to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            message: message.to_string(),
            branch: None,
            cwd: cwd.map(ToOwned::to_owned),
        }
    }

    fn mk_tmp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ai-contexters-segmentation-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    #[test]
    fn repo_signal_segmentation_splits_one_session_across_multiple_repositories() {
        let entries = vec![
            entry(
                (2026, 3, 21, 9, 0, 0),
                "sess-1",
                "user",
                "Please inspect https://github.com/VetCoders/ai-contexters before editing.",
                None,
            ),
            entry(
                (2026, 3, 21, 9, 1, 0),
                "sess-1",
                "assistant",
                "I found the store seam in ai-contexters.",
                None,
            ),
            entry(
                (2026, 3, 21, 9, 2, 0),
                "sess-1",
                "user",
                "Switch now to https://github.com/VetCoders/loctree and review the scanner.",
                None,
            ),
            entry(
                (2026, 3, 21, 9, 3, 0),
                "sess-1",
                "assistant",
                "I am reviewing loctree next.",
                None,
            ),
        ];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].project_label(), "VetCoders/ai-contexters");
        assert_eq!(segments[1].project_label(), "VetCoders/loctree");
    }

    #[test]
    fn repo_signal_segmentation_keeps_unknown_prefix_honest() {
        let entries = vec![
            entry(
                (2026, 3, 21, 9, 0, 0),
                "sess-2",
                "user",
                "Need a migration plan but I have not named the repo yet.",
                None,
            ),
            entry(
                (2026, 3, 21, 9, 1, 0),
                "sess-2",
                "assistant",
                "Drafting a migration plan with acceptance criteria.",
                None,
            ),
            entry(
                (2026, 3, 21, 9, 2, 0),
                "sess-2",
                "user",
                "The actual repo is https://github.com/VetCoders/ai-contexters.",
                None,
            ),
        ];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 2);
        assert!(segments[0].repo.is_none());
        assert_eq!(segments[0].kind, Kind::Plans);
        assert_eq!(segments[1].project_label(), "VetCoders/ai-contexters");
    }

    #[test]
    fn repo_signal_segmentation_ignores_gemini_hash_like_cwd() {
        let entry = entry(
            (2026, 3, 21, 9, 0, 0),
            "sess-3",
            "user",
            "No trustworthy repo here.",
            Some("57cfd37b3a72d995c4f2d018ebf9d5a2"),
        );

        assert!(infer_repo_identity_from_entry(&entry).is_none());
        let segments = semantic_segments(&[entry]);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].repo.is_none());
    }

    #[test]
    fn repo_signal_segmentation_uses_local_git_remote_when_available() {
        let root = mk_tmp_dir("git-remote");
        let repo = root.join("hosted").join("VetCoders").join("ai-contexters");
        fs::create_dir_all(&repo).unwrap();

        Command::new("git")
            .arg("init")
            .arg(&repo)
            .output()
            .expect("git init should run");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:VetCoders/ai-contexters.git",
            ])
            .output()
            .expect("git remote add should run");

        let entry = entry(
            (2026, 3, 21, 9, 0, 0),
            "sess-4",
            "user",
            "Inspect the repo on disk.",
            Some(repo.to_string_lossy().as_ref()),
        );

        let repo_identity = infer_repo_identity_from_entry(&entry).expect("repo identity");
        assert_eq!(repo_identity.slug(), "VetCoders/ai-contexters");

        let _ = fs::remove_dir_all(&root);
    }
}
