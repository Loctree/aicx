//! Durable session catalog — the extract-era store surface.
//!
//! Replaces the per-frame card mill (`~/.aicx/store/**/*.md`) with one
//! compact append-only identity index:
//!
//! ```text
//! ~/.aicx/catalog/sessions.jsonl
//! ```
//!
//! Each line maps `session_id → project, agent, date, cwd, source_path,
//! title, machine`. Content stays in the agent sources (or optional
//! `~/.aicx/extracts/` cache). Rebuild walks source roots only — no card
//! files are written.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::session_catalog::{AgentKind, CatalogSource, SessionCatalog};
use crate::store::{self};

pub const CATALOG_DIRNAME: &str = "catalog";
pub const SESSIONS_FILENAME: &str = "sessions.jsonl";
pub const CATALOG_SCHEMA: &str = "aicx.catalog.session.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogEntry {
    pub schema: String,
    pub session_id: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub source_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_session_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RebuildReport {
    pub agents: BTreeMap<String, usize>,
    pub projects: BTreeMap<String, usize>,
    pub total_sessions: usize,
    pub catalog_path: String,
    pub wall_ms: u64,
    pub cards_written: usize,
}

pub fn catalog_dir_for(home: &Path) -> PathBuf {
    home.join(CATALOG_DIRNAME)
}

pub fn sessions_path_for(home: &Path) -> PathBuf {
    catalog_dir_for(home).join(SESSIONS_FILENAME)
}

pub fn sessions_path() -> Result<PathBuf> {
    Ok(sessions_path_for(&store::resolve_aicx_home()?))
}

pub fn read_entries_at(home: &Path) -> Result<Vec<CatalogEntry>> {
    let path = sessions_path_for(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path).with_context(|| format!("open catalog {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read catalog line {}", line_no + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        entries.push(serde_json::from_str(&line).with_context(|| {
            format!("parse catalog line {} in {}", line_no + 1, path.display())
        })?);
    }
    Ok(entries)
}

/// Project identities already attributed in the durable catalog (if any).
pub fn project_identities_from_catalog_at(store_root: &Path) -> Result<Vec<String>> {
    let path = sessions_path_for(store_root);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path).with_context(|| format!("open catalog {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut identities = BTreeSet::new();
    for line in reader.lines() {
        let line = line.with_context(|| format!("read catalog line {}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<CatalogEntry>(&line) else {
            continue;
        };
        if let Some(project) = entry.project {
            let project = project.trim();
            if !project.is_empty() {
                identities.insert(project.to_string());
            }
        }
    }
    Ok(identities.into_iter().collect())
}

/// Rebuild the durable catalog from live agent source roots.
///
/// Walks claude / codex / gemini / grok / junie roots via
/// [`SessionCatalog`], enriches with [`crate::sessions`] discovery for
/// cwd/project/title when available, and writes one jsonl line per
/// session. Never creates per-frame card files under `store/`.
pub fn rebuild(home: &Path, user_home: &Path) -> Result<RebuildReport> {
    let started = Instant::now();
    let mut by_id: BTreeMap<(String, String), CatalogEntry> = BTreeMap::new();

    for agent in [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Gemini,
        AgentKind::Grok,
        AgentKind::Junie,
    ] {
        let root = agent_source_root(agent, user_home);
        if !root.exists() {
            continue;
        }
        let catalog = match SessionCatalog::new(agent, &root) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let scan = catalog.scan_with_stats();
        let sources = match scan.result {
            Ok(s) => s,
            Err(_) => continue,
        };
        for source in sources {
            if !is_primary_catalog_source(agent, &source.path) {
                continue;
            }
            let entry = entry_from_source(agent, &source);
            by_id.insert((entry.agent.clone(), entry.session_id.clone()), entry);
        }
    }

    // Enrich with sessions discovery (cwd / project / title / dates).
    enrich_from_sessions_discovery(&mut by_id, user_home);

    // vibecrafted runtime_runs — snapshot identity before collector GC.
    enrich_runtime_runs(&mut by_id, user_home);

    let catalog_path = sessions_path_for(home);
    fs::create_dir_all(catalog_dir_for(home))
        .with_context(|| format!("create catalog dir {}", catalog_dir_for(home).display()))?;

    let mut agents: BTreeMap<String, usize> = BTreeMap::new();
    let mut projects: BTreeMap<String, usize> = BTreeMap::new();
    let mut body = String::new();
    for entry in by_id.values() {
        *agents.entry(entry.agent.clone()).or_default() += 1;
        if let Some(ref project) = entry.project {
            *projects.entry(project.clone()).or_default() += 1;
        }
        body.push_str(&serde_json::to_string(entry)?);
        body.push('\n');
    }
    store::atomic_write::atomic_write(&catalog_path, body.as_bytes())
        .with_context(|| format!("write catalog {}", catalog_path.display()))?;

    Ok(RebuildReport {
        total_sessions: by_id.len(),
        agents,
        projects,
        catalog_path: catalog_path.display().to_string(),
        wall_ms: started.elapsed().as_millis() as u64,
        cards_written: 0,
    })
}

/// Resolve a session_id → source_path from the durable catalog (exact id match).
pub fn resolve_session(home: &Path, session_id: &str) -> Result<Option<CatalogEntry>> {
    let needle = session_id.trim();
    if needle.is_empty() {
        return Ok(None);
    }
    let entries = read_entries_at(home)?;
    for entry in &entries {
        if entry.session_id == needle
            || entry
                .logical_session_id
                .as_deref()
                .is_some_and(|id| id == needle)
        {
            return Ok(Some(entry.clone()));
        }
    }
    let mut prefixes: Vec<_> = entries
        .into_iter()
        .filter(|entry| {
            entry.session_id.starts_with(needle)
                || entry
                    .logical_session_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with(needle))
        })
        .collect();
    prefixes.sort_by(|left, right| {
        left.agent
            .cmp(&right.agent)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    prefixes
        .dedup_by(|left, right| left.agent == right.agent && left.session_id == right.session_id);
    match prefixes.len() {
        0 => Ok(None),
        1 => Ok(prefixes.pop()),
        count => anyhow::bail!(
            "session prefix `{needle}` is ambiguous across {count} catalog entries; use the full id"
        ),
    }
}

fn agent_source_root(agent: AgentKind, user_home: &Path) -> PathBuf {
    match agent {
        AgentKind::Claude => user_home.join(".claude").join("projects"),
        AgentKind::Codex => user_home.join(".codex").join("sessions"),
        AgentKind::Gemini => user_home.join(".gemini").join("tmp"),
        // Grok sessions live under `~/.grok/sessions/<cwd-encoded>/…`
        // (not the bare `~/.grok` tree, which also holds config noise).
        AgentKind::Grok => user_home.join(".grok").join("sessions"),
        AgentKind::Junie => user_home.join(".junie").join("sessions"),
    }
}

fn is_primary_catalog_source(agent: AgentKind, path: &Path) -> bool {
    agent != AgentKind::Grok
        || path.file_name().and_then(|name| name.to_str()) == Some("chat_history.jsonl")
}

fn entry_from_source(agent: AgentKind, source: &CatalogSource) -> CatalogEntry {
    let session_id = if agent == AgentKind::Grok {
        grok_session_id_from_path(&source.path).unwrap_or_else(|| source.source_id.clone())
    } else {
        source.source_id.clone()
    };
    let cwd = infer_cwd_from_path(agent, &source.path);
    let project = cwd
        .as_deref()
        .and_then(project_from_cwd)
        .or_else(|| infer_project_from_path(agent, &source.path));
    let date = source
        .fingerprint
        .modified_unix_nanos
        .checked_div(1_000_000_000)
        .and_then(|secs| {
            chrono::DateTime::from_timestamp(secs as i64, 0)
                .map(|dt| dt.format("%Y-%m-%d").to_string())
        });
    CatalogEntry {
        schema: CATALOG_SCHEMA.to_string(),
        session_id: session_id.clone(),
        agent: agent.as_str().to_string(),
        project,
        date,
        cwd,
        source_path: source.path.display().to_string(),
        title: None,
        machine: hostname(),
        logical_session_id: if agent == AgentKind::Grok {
            Some(session_id)
        } else {
            source.logical_session_id.clone()
        },
    }
}

fn enrich_from_sessions_discovery(
    by_id: &mut BTreeMap<(String, String), CatalogEntry>,
    user_home: &Path,
) {
    let claude_root = user_home.join(".claude").join("projects");
    if claude_root.is_dir() {
        for info in crate::sessions::discover_claude_sessions(&claude_root, None, None) {
            merge_session_info(by_id, &info);
        }
    }
    let codex_root = user_home.join(".codex").join("sessions");
    if codex_root.is_dir() {
        for info in crate::sessions::discover_codex_sessions(&codex_root, None) {
            merge_session_info(by_id, &info);
        }
    }
    let gemini_root = user_home.join(".gemini").join("tmp");
    if gemini_root.is_dir() {
        for info in crate::sessions::discover_gemini_sessions(&gemini_root, None, None) {
            merge_session_info(by_id, &info);
        }
    }
    let junie_root = user_home.join(".junie").join("sessions");
    if junie_root.is_dir() {
        for info in crate::sessions::discover_junie_sessions(&junie_root, None) {
            merge_session_info(by_id, &info);
        }
    }
}

fn merge_session_info(
    by_id: &mut BTreeMap<(String, String), CatalogEntry>,
    info: &crate::sessions::SessionInfo,
) {
    let key = (info.agent.clone(), info.session_id.clone());
    let date = info
        .updated_at
        .or(info.started_at)
        .map(|dt| dt.format("%Y-%m-%d").to_string());
    let entry = by_id.entry(key).or_insert_with(|| CatalogEntry {
        schema: CATALOG_SCHEMA.to_string(),
        session_id: info.session_id.clone(),
        agent: info.agent.clone(),
        project: info.project.clone(),
        date: date.clone(),
        cwd: info.repo_path.clone(),
        source_path: info.source_path.display().to_string(),
        title: info.title.clone(),
        machine: hostname(),
        logical_session_id: None,
    });
    if entry.project.is_none() {
        entry.project = info.project.clone();
    }
    if entry.cwd.is_none() {
        entry.cwd = info.repo_path.clone();
    }
    if entry.title.is_none() {
        entry.title = info.title.clone();
    }
    if entry.date.is_none() {
        entry.date = date;
    }
    if entry.source_path.is_empty() {
        entry.source_path = info.source_path.display().to_string();
    }
}

fn enrich_runtime_runs(by_id: &mut BTreeMap<(String, String), CatalogEntry>, user_home: &Path) {
    let runs = user_home
        .join(".vibecrafted")
        .join("control_plane")
        .join("runtime_runs");
    if !runs.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(&runs) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let run_id = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if run_id.is_empty() {
            continue;
        }
        let transcript = path.join("transcript.log");
        if !transcript.is_file() {
            continue;
        }
        let meta = fs::metadata(&transcript).ok();
        let date = meta.and_then(|m| m.modified().ok()).and_then(|t| {
            let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
            chrono::DateTime::from_timestamp(secs, 0).map(|dt| dt.format("%Y-%m-%d").to_string())
        });
        let key = ("vibecrafted".to_string(), run_id.clone());
        by_id.entry(key).or_insert_with(|| CatalogEntry {
            schema: CATALOG_SCHEMA.to_string(),
            session_id: run_id,
            agent: "vibecrafted".to_string(),
            project: Some("VetCoders/vibecrafted".to_string()),
            date,
            cwd: None,
            source_path: transcript.display().to_string(),
            title: Some("runtime_run transcript".to_string()),
            machine: hostname(),
            logical_session_id: None,
        });
    }
}

fn infer_cwd_from_path(agent: AgentKind, path: &Path) -> Option<String> {
    match agent {
        AgentKind::Claude => {
            // ~/.claude/projects/<encoded-cwd>/<session>.jsonl
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|encoded| encoded.replace('-', "/"))
        }
        AgentKind::Grok => {
            // ~/.grok/sessions/<cwd-encoded>/<session>/...
            let encoded_cwd = path.ancestors().find(|ancestor| {
                ancestor
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    == Some("sessions")
            })?;
            encoded_cwd
                .file_name()
                .and_then(|name| name.to_str())
                .map(decode_grok_cwd)
        }
        _ => None,
    }
}

fn decode_grok_cwd(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] == b'%'
            && cursor + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_nibble(bytes[cursor + 1]), hex_nibble(bytes[cursor + 2]))
        {
            decoded.push((high << 4) | low);
            cursor += 3;
        } else {
            decoded.push(bytes[cursor]);
            cursor += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn grok_session_id_from_path(path: &Path) -> Option<String> {
    path.parent()?
        .file_name()?
        .to_str()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn infer_project_from_path(agent: AgentKind, path: &Path) -> Option<String> {
    let cwd = infer_cwd_from_path(agent, path)?;
    project_from_cwd(&cwd)
}

fn project_from_cwd(cwd: &str) -> Option<String> {
    let seg = cwd
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|s| !s.is_empty())?;
    // Prefer owner/repo when two trailing segments look like a git path.
    let parts: Vec<&str> = cwd
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .filter(|s| !s.is_empty())
        .take(2)
        .collect();
    if parts.len() == 2 {
        let repo = parts[0];
        let owner = parts[1];
        if owner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && repo
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            && owner.len() >= 2
            && repo.len() >= 2
        {
            return Some(format!("{owner}/{repo}"));
        }
    }
    Some(seg.to_string())
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let output = std::process::Command::new("hostname").output().ok()?;
            if !output.status.success() {
                return None;
            }
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if name.is_empty() { None } else { Some(name) }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_root(label: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("aicx-catalog-{label}-{nanos}-{n}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn catalog_roundtrip_writes_zero_cards() {
        let dir = test_root("roundtrip");
        let home = dir.join(".aicx");
        let user = dir.join("user");
        fs::create_dir_all(user.join(".claude").join("projects").join("proj")).unwrap();
        let session = user
            .join(".claude")
            .join("projects")
            .join("proj")
            .join("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
        let mut f = File::create(&session).unwrap();
        writeln!(
            f,
            r#"{{"type":"user","sessionId":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","message":{{"content":"hi"}}}}"#
        )
        .unwrap();
        let report = rebuild(&home, &user).unwrap();
        assert_eq!(report.cards_written, 0);
        assert!(Path::new(&report.catalog_path).exists());
        assert!(!home.join("store").exists());
        let resolved = resolve_session(&home, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee")
            .unwrap()
            .expect("session in catalog");
        assert_eq!(resolved.agent, "claude");
        assert!(resolved.source_path.contains("aaaaaaaa-bbbb"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_identities_reads_catalog() {
        let home = test_root("identities");
        fs::create_dir_all(catalog_dir_for(&home)).unwrap();
        let entry = CatalogEntry {
            schema: CATALOG_SCHEMA.to_string(),
            session_id: "s1".into(),
            agent: "claude".into(),
            project: Some("VetCoders/mlx-lm".into()),
            date: Some("2026-07-22".into()),
            cwd: None,
            source_path: "/tmp/x".into(),
            title: None,
            machine: None,
            logical_session_id: None,
        };
        fs::write(
            sessions_path_for(&home),
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();
        let ids = project_identities_from_catalog_at(&home).unwrap();
        assert_eq!(ids, vec!["VetCoders/mlx-lm".to_string()]);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn resolve_session_rejects_ambiguous_prefix() {
        let home = test_root("ambiguous-prefix");
        fs::create_dir_all(catalog_dir_for(&home)).unwrap();
        let entries = ["abcdef-111", "abcdef-222"]
            .into_iter()
            .map(|session_id| CatalogEntry {
                schema: CATALOG_SCHEMA.to_string(),
                session_id: session_id.to_string(),
                agent: "codex".to_string(),
                project: None,
                date: None,
                cwd: None,
                source_path: format!("/tmp/{session_id}.jsonl"),
                title: None,
                machine: None,
                logical_session_id: None,
            })
            .map(|entry| serde_json::to_string(&entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(sessions_path_for(&home), format!("{entries}\n")).unwrap();
        let error = resolve_session(&home, "abcdef").unwrap_err();
        assert!(error.to_string().contains("ambiguous"));
        assert!(resolve_session(&home, "abcdef-111").unwrap().is_some());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn grok_catalog_keeps_chat_history_and_decodes_cwd() {
        let path = Path::new(
            "/Users/test/.grok/sessions/%2FVolumes%2Fvc-workspace%2Fvetcoders%2Fvibecrafted/\
             019f5407-5b0c-7363-b210-1093f26a41f7/chat_history.jsonl",
        );
        assert!(is_primary_catalog_source(AgentKind::Grok, path));
        assert!(!is_primary_catalog_source(
            AgentKind::Grok,
            &path.with_file_name("events.jsonl")
        ));
        assert_eq!(
            infer_cwd_from_path(AgentKind::Grok, path).as_deref(),
            Some("/Volumes/vc-workspace/vetcoders/vibecrafted")
        );
        assert_eq!(
            infer_project_from_path(AgentKind::Grok, path).as_deref(),
            Some("vetcoders/vibecrafted")
        );
        assert_eq!(
            grok_session_id_from_path(path).as_deref(),
            Some("019f5407-5b0c-7363-b210-1093f26a41f7")
        );
    }
}
