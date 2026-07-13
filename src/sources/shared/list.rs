#![allow(unused_imports)]
use super::*;
use crate::importers::codescribe::CODESCRIBE_AGENT;
use crate::importers::{discover_codescribe_transcripts, discover_operator_markdown};
use crate::sources::UNPROTECTED_SOURCE_WARNING;
use crate::sources::count_codex_sessions;

const JUNIE_EVENTS_FILENAME: &str = "events.jsonl";

fn is_gemini_session_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "json" | "jsonl"))
}

fn source_root_for_protection(path: &Path) -> PathBuf {
    if path.is_file() {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

fn discover_protecting_git_root(path: &Path, home: &Path) -> Option<PathBuf> {
    let source_root = source_root_for_protection(path);
    let mut current = Some(source_root.as_path());

    while let Some(candidate) = current {
        if candidate.join(".git").is_dir() {
            return Some(candidate.to_path_buf());
        }
        if candidate == home {
            break;
        }
        current = candidate.parent();
    }

    None
}

fn git_remote_lines(root: &Path) -> Vec<String> {
    let Ok(output) = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["remote", "-v"])
        .output()
    else {
        return Vec::new();
    };

    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn source_info(
    home: &Path,
    agent: impl Into<String>,
    path: PathBuf,
    sessions: usize,
    size_bytes: u64,
) -> SourceInfo {
    let protection_root = discover_protecting_git_root(&path, home);
    let git_remotes = protection_root
        .as_deref()
        .map(git_remote_lines)
        .unwrap_or_default();
    let protected_by_git = protection_root.is_some();

    SourceInfo {
        agent: agent.into(),
        path,
        sessions,
        size_bytes,
        protected_by_git,
        protection_backend: if protected_by_git {
            "git-local".to_string()
        } else {
            "none".to_string()
        },
        protection_root,
        git_remote_count: git_remotes.len(),
        git_remotes,
        protection_warning: (!protected_by_git).then(|| UNPROTECTED_SOURCE_WARNING.to_string()),
    }
}

/// List available sources with session counts, sizes, and read-only protection status.
pub fn list_available_sources() -> Result<Vec<SourceInfo>> {
    let home = crate::os_user_home().context("No home dir")?;
    let mut sources: Vec<SourceInfo> = Vec::new();

    // Claude
    let claude_dir = home.join(".claude").join("projects");
    if claude_dir.exists() && claude_dir.is_dir() {
        for dir_entry in fs::read_dir(&claude_dir)? {
            let dir_entry = dir_entry?;
            let path = dir_entry.path();
            if !path.is_dir() {
                continue;
            }

            let mut session_count = 0usize;
            let mut total_size = 0u64;

            for file_entry in fs::read_dir(&path)? {
                let file_entry = file_entry?;
                let fp = file_entry.path();
                if fp.extension().is_some_and(|e| e == "jsonl") {
                    session_count += 1;
                    if let Ok(meta) = fs::metadata(&fp) {
                        total_size += meta.len();
                    }
                }
            }

            if session_count > 0 {
                sources.push(source_info(
                    &home,
                    "claude",
                    path,
                    session_count,
                    total_size,
                ));
            }
        }
    }

    // Claude history.jsonl
    let claude_history = home.join(".claude").join("history.jsonl");
    if claude_history.exists() {
        let size = fs::metadata(&claude_history).map(|m| m.len()).unwrap_or(0);
        sources.push(source_info(
            &home,
            "claude-history",
            claude_history,
            1,
            size,
        ));
    }

    // Codex
    let codex_path = home.join(".codex").join("history.jsonl");
    if codex_path.exists() {
        let size = fs::metadata(&codex_path).map(|m| m.len()).unwrap_or(0);
        let sessions = count_codex_sessions(&codex_path).unwrap_or(0);
        sources.push(source_info(&home, "codex", codex_path, sessions, size));
    }

    // Codex sessions: ~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl
    let codex_sessions_dir = home.join(".codex").join("sessions");
    if codex_sessions_dir.exists() && codex_sessions_dir.is_dir() {
        let files = walk_jsonl_files(&codex_sessions_dir);
        let total_size: u64 = files
            .iter()
            .filter_map(|f| fs::metadata(f).ok())
            .map(|m| m.len())
            .sum();
        if !files.is_empty() {
            sources.push(source_info(
                &home,
                "codex-sessions",
                codex_sessions_dir,
                files.len(),
                total_size,
            ));
        }
    }

    // Grok (Codex v1/responses format): ~/.grok/sessions/<project>/<session-uuid>/*.jsonl
    // and also check projects/ for any additional transcripts
    let grok_sessions_dir = home.join(".grok").join("sessions");
    if grok_sessions_dir.exists() && grok_sessions_dir.is_dir() {
        let files = walk_jsonl_files(&grok_sessions_dir);
        let total_size: u64 = files
            .iter()
            .filter_map(|f| fs::metadata(f).ok())
            .map(|m| m.len())
            .sum();
        if !files.is_empty() {
            sources.push(source_info(
                &home,
                "grok-sessions",
                grok_sessions_dir,
                files.len(),
                total_size,
            ));
        }
    }
    // Also surface the projects dir if it has data (Grok stores per-project session dirs under it too)
    let grok_projects_dir = home.join(".grok").join("projects");
    if grok_projects_dir.exists() && grok_projects_dir.is_dir() {
        let files = walk_jsonl_files(&grok_projects_dir);
        let total_size: u64 = files
            .iter()
            .filter_map(|f| fs::metadata(f).ok())
            .map(|m| m.len())
            .sum();
        if !files.is_empty() {
            sources.push(source_info(
                &home,
                "grok-projects",
                grok_projects_dir,
                files.len(),
                total_size,
            ));
        }
    }

    // Gemini CLI: ~/.gemini/tmp/<projectHash>/chats/session-*.json[l]
    let gemini_tmp = home.join(".gemini").join("tmp");
    if gemini_tmp.exists() && gemini_tmp.is_dir() {
        for project_entry in fs::read_dir(&gemini_tmp)? {
            let project_entry = project_entry?;
            let project_path = project_entry.path();

            if !project_path.is_dir() {
                continue;
            }

            let chats_dir = project_path.join("chats");
            if !chats_dir.exists() || !chats_dir.is_dir() {
                continue;
            }

            let mut session_count = 0usize;
            let mut total_size = 0u64;

            for file_entry in fs::read_dir(&chats_dir)? {
                let file_entry = file_entry?;
                let fp = file_entry.path();
                if is_gemini_session_file(&fp) {
                    session_count += 1;
                    if let Ok(meta) = fs::metadata(&fp) {
                        total_size += meta.len();
                    }
                }
            }

            if session_count > 0 {
                sources.push(source_info(
                    &home,
                    "gemini",
                    project_path,
                    session_count,
                    total_size,
                ));
            }
        }
    }

    // Junie sessions: ~/.junie/sessions/session-*/events.jsonl
    let junie_sessions = home.join(".junie").join("sessions");
    if junie_sessions.exists() && junie_sessions.is_dir() {
        let files: Vec<PathBuf> = walk_jsonl_files(&junie_sessions)
            .into_iter()
            .filter(|path| {
                path.file_name().and_then(|name| name.to_str()) == Some(JUNIE_EVENTS_FILENAME)
            })
            .collect();
        let total_size: u64 = files
            .iter()
            .filter_map(|file| fs::metadata(file).ok())
            .map(|metadata| metadata.len())
            .sum();
        if !files.is_empty() {
            sources.push(source_info(
                &home,
                "junie",
                junie_sessions,
                files.len(),
                total_size,
            ));
        }
    }

    // Codescribe transcripts: ~/.codescribe/transcriptions/YYYY-MM-DD/*.{txt,md,json}
    let codescribe_transcripts = discover_codescribe_transcripts(&home);
    if !codescribe_transcripts.is_empty() {
        let total_size: u64 = codescribe_transcripts
            .iter()
            .filter_map(|transcript| fs::metadata(&transcript.path).ok())
            .map(|metadata| metadata.len())
            .sum();
        sources.push(source_info(
            &home,
            CODESCRIBE_AGENT,
            home.join(".codescribe").join("transcriptions"),
            codescribe_transcripts.len(),
            total_size,
        ));
    }

    // Operator markdown: ~/Downloads/*.md and ~/.vibecrafted/inbox/*.md
    let operator_markdown = discover_operator_markdown(&home);
    if !operator_markdown.is_empty() {
        let total_size: u64 = operator_markdown
            .iter()
            .filter_map(|document| fs::metadata(&document.path).ok())
            .map(|metadata| metadata.len())
            .sum();
        sources.push(source_info(
            &home,
            "operator-md",
            home.join("Downloads"),
            operator_markdown.len(),
            total_size,
        ));
    }

    Ok(sources)
}
