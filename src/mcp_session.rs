//! MCP session chain: list → show/extract → continuity.
//!
//! These functions are the library core behind `aicx_sessions`, `aicx_session`,
//! and `aicx_continuity`. The MCP router in `mcp.rs` stays a thin wrapper.
//! Discovery is live (operator home), project identity is exact and
//! fail-closed, and empty/ambiguous outcomes are numbered — never silent.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use crate::extraction::{self, ConversationMessage};
use crate::legacy_archive::{self, ProjectMatchMode, ProjectResolutionError};
use crate::session_catalog::{self, AgentKind, CatalogError, CatalogIoStats, ResolvedSource};
use crate::sessions::{self, SessionInfo};
use crate::timeline::FrameKind;

const DEFAULT_LIST_HOURS: u64 = 720;
const DEFAULT_LIST_LIMIT: usize = 20;
const DEFAULT_CONTINUITY_HOURS: u64 = 24;
const CONTINUITY_MODE: &str = "context_pack";
const CONTINUITY_BANNER: &str = "_Context pack only. Native provider attach is off unless the operator already supplied a session id._\n\n";

#[derive(Debug)]
pub enum SessionSurfaceError {
    InvalidParams { message: String, payload: Value },
    Internal { message: String },
}

impl std::fmt::Display for SessionSurfaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidParams { message, .. } | Self::Internal { message } => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for SessionSurfaceError {}

impl SessionSurfaceError {
    fn invalid(kind: &str, message: impl Into<String>, extra: Value) -> Self {
        let message = message.into();
        let mut payload = extra;
        if let Some(object) = payload.as_object_mut() {
            object.insert("ok".into(), json!(false));
            object.insert("error".into(), json!(kind));
        }
        Self::InvalidParams { message, payload }
    }

    pub fn into_mcp(self) -> rmcp::ErrorData {
        match self {
            Self::InvalidParams { message, payload } => {
                rmcp::ErrorData::invalid_params(message, Some(payload))
            }
            Self::Internal { message } => rmcp::ErrorData::internal_error(message, None),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionListItem {
    pub session_id: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub live: bool,
    pub message_count: usize,
    pub user_message_count: usize,
    pub agent_message_count: usize,
    pub association: sessions::Association,
    pub temporal_confidence: sessions::TemporalConfidence,
    pub source_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionListPayload {
    pub ok: bool,
    pub empty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub projects: Vec<String>,
    pub scanned: usize,
    pub matched: usize,
    pub limit: usize,
    pub hours: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub sessions: Vec<SessionListItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationPayload {
    pub message_count: usize,
    pub harness_noise_dropped: usize,
    pub messages: Vec<ConversationMessage>,
    pub markdown: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionShowPayload {
    pub ok: bool,
    pub session: SessionListItem,
    pub matched_by: String,
    pub catalog_candidates: usize,
    pub catalog_files_opened: usize,
    pub catalog_body_reads: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ConversationPayload>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContinuityPayload {
    pub ok: bool,
    pub mode: &'static str,
    pub native_resume: bool,
    pub project: String,
    pub projects: Vec<String>,
    pub hours: u64,
    pub live_sessions: usize,
    pub markdown: String,
    pub warnings: Vec<String>,
}

pub struct ListSessionsRequest<'a> {
    pub user_home: &'a Path,
    pub aicx_home: &'a Path,
    pub project: Option<&'a str>,
    pub projects: &'a [String],
    pub project_match: ProjectMatchMode,
    pub agent: Option<&'a str>,
    pub hours: u64,
    pub since: Option<&'a str>,
    pub limit: usize,
}

pub struct ShowSessionRequest<'a> {
    pub user_home: &'a Path,
    pub session: &'a str,
    pub agent: Option<&'a str>,
    pub conversation: bool,
    pub user_only: bool,
}

pub struct ContinuityRequest<'a> {
    pub aicx_home: &'a Path,
    pub project: Option<&'a str>,
    pub projects: &'a [String],
    pub project_match: ProjectMatchMode,
    pub hours: u64,
    pub for_inject: bool,
}

pub fn default_list_hours() -> u64 {
    DEFAULT_LIST_HOURS
}

pub fn default_list_limit() -> usize {
    DEFAULT_LIST_LIMIT
}

pub fn default_continuity_hours() -> u64 {
    DEFAULT_CONTINUITY_HOURS
}

pub fn list_sessions(
    req: ListSessionsRequest<'_>,
) -> Result<SessionListPayload, SessionSurfaceError> {
    let agent = parse_optional_agent(req.agent)?;
    let filters = collect_project_filters(req.project, req.projects);
    let since_dt = lower_bound(req.hours, req.since)?;
    let modified_after = since_dt.map(datetime_to_system_time);
    let discovered = sessions::discover_sessions_at(
        req.user_home,
        modified_after,
        None,
        agent.map(AgentKind::as_str),
    );
    let scanned = discovered.len();
    let filters_to_apply =
        resolve_session_project_filters(&filters, &discovered, req.aicx_home, req.project_match)?;
    let live_paths = live_source_paths(req.aicx_home, req.user_home, since_dt);
    let mut selected = if filters_to_apply.is_empty() {
        discovered
    } else {
        discovered
            .into_iter()
            .filter(|session| session_matches_project(session, &filters_to_apply))
            .collect()
    };
    selected = sessions::select_sessions(
        selected,
        None,
        agent.map(AgentKind::as_str),
        since_dt,
        req.limit,
    );
    let items: Vec<SessionListItem> = selected
        .into_iter()
        .map(|session| session_item(session, &live_paths))
        .collect();
    let matched = items.len();
    let (empty_kind, warning) = list_empty_signal(scanned, matched, !filters.is_empty());
    Ok(SessionListPayload {
        ok: true,
        empty: matched == 0,
        empty_kind,
        warning,
        project: req.project.map(str::to_string),
        projects: filters,
        scanned,
        matched,
        limit: req.limit,
        hours: req.hours,
        agent: req.agent.map(str::to_string),
        sessions: items,
    })
}

pub fn show_session(
    req: ShowSessionRequest<'_>,
) -> Result<SessionShowPayload, SessionSurfaceError> {
    if req.session.trim().is_empty() {
        return Err(SessionSurfaceError::invalid(
            "invalid_session_reference",
            "session id is empty",
            json!({ "session": req.session }),
        ));
    }
    let (agent, resolved, stats) = resolve_session(req.user_home, req.session, req.agent)?;
    let info = session_info_for_resolved(agent, &resolved);
    let live_paths = BTreeSet::new();
    let mut warnings = Vec::new();
    let conversation = if req.conversation {
        match extract_conversation(&resolved, agent, req.user_only) {
            Ok(payload) => Some(payload),
            Err(error) => {
                warnings.push(error);
                None
            }
        }
    } else {
        None
    };
    if conversation.is_none() && req.conversation {
        warnings.push(format!(
            "session `{}` resolved but conversation extract failed; metadata is still returned",
            resolved.source.source_id
        ));
    }
    Ok(SessionShowPayload {
        ok: true,
        session: session_item(info, &live_paths),
        matched_by: format!("{:?}", resolved.matched_by),
        catalog_candidates: stats.metadata_candidates,
        catalog_files_opened: stats.files_opened,
        catalog_body_reads: stats.body_reads,
        conversation,
        warnings,
    })
}

pub fn continuity_pack(
    req: ContinuityRequest<'_>,
) -> Result<ContinuityPayload, SessionSurfaceError> {
    let filters = collect_project_filters(req.project, req.projects);
    if filters.is_empty() {
        return Err(SessionSurfaceError::invalid(
            "project_required",
            "aicx_continuity requires an exact project filter; MCP cannot infer the operator checkout",
            json!({
                "hint": "pass project=\"owner/repo\" or project=\"/repo\"",
            }),
        ));
    }
    let selected =
        resolve_session_project_filters(&filters, &[], req.aicx_home, req.project_match)?;
    let pack = crate::continuity::build(req.aicx_home, &selected, req.hours).map_err(|error| {
        SessionSurfaceError::Internal {
            message: format!("continuity pack: {error}"),
        }
    })?;
    let mut markdown = crate::continuity::render(&pack, req.for_inject);
    if let Some(idx) = markdown.find('\n') {
        markdown.insert(idx + 1, '\n');
        markdown.insert_str(idx + 2, CONTINUITY_BANNER);
    } else {
        markdown.push('\n');
        markdown.push_str(CONTINUITY_BANNER);
    }
    let mut warnings = Vec::new();
    if pack.live_sessions == 0 {
        warnings.push(format!(
            "continuity window {}h for {} produced 0 live sessions; empty NOW/PEERS is not proof of a quiet window",
            req.hours,
            selected.join(", ")
        ));
    }
    if pack.index_health.pending > 0 || pack.index_health.sessions_newer_than_chunks > 0 {
        warnings.push(format!(
            "index lag pending={} sessions_newer_than_chunks={}",
            pack.index_health.pending, pack.index_health.sessions_newer_than_chunks
        ));
    }
    Ok(ContinuityPayload {
        ok: true,
        mode: CONTINUITY_MODE,
        native_resume: false,
        project: selected.join(", "),
        projects: selected,
        hours: req.hours,
        live_sessions: pack.live_sessions,
        markdown,
        warnings,
    })
}

fn collect_project_filters(project: Option<&str>, projects: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(project) = project {
        let trimmed = project.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
    }
    for project in projects {
        let trimmed = project.trim();
        if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn parse_optional_agent(agent: Option<&str>) -> Result<Option<AgentKind>, SessionSurfaceError> {
    match agent {
        None => Ok(None),
        Some(value) => AgentKind::parse(value).map(Some).ok_or_else(|| {
            SessionSurfaceError::invalid(
                "invalid_agent",
                format!("unknown agent `{value}`; expected claude, codex, gemini, junie, or grok"),
                json!({
                    "agent": value,
                    "expected": ["claude", "codex", "gemini", "junie", "grok"],
                }),
            )
        }),
    }
}

fn lower_bound(
    hours: u64,
    since: Option<&str>,
) -> Result<Option<DateTime<Utc>>, SessionSurfaceError> {
    if let Some(since) = since {
        return parse_since_date(since).map(Some);
    }
    if hours == 0 {
        return Ok(None);
    }
    Ok(Some(
        Utc::now() - chrono::Duration::hours(hours.min(i64::MAX as u64) as i64),
    ))
}

fn parse_since_date(value: &str) -> Result<DateTime<Utc>, SessionSurfaceError> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| {
        SessionSurfaceError::invalid(
            "invalid_since",
            format!("since `{value}` is not YYYY-MM-DD"),
            json!({ "since": value }),
        )
    })?;
    date.and_hms_opt(0, 0, 0)
        .map(|naive| Utc.from_utc_datetime(&naive))
        .ok_or_else(|| {
            SessionSurfaceError::invalid(
                "invalid_since",
                format!("since `{value}` is not a valid date"),
                json!({ "since": value }),
            )
        })
}

fn datetime_to_system_time(dt: DateTime<Utc>) -> std::time::SystemTime {
    let secs = dt.timestamp().max(0) as u64;
    std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
}

pub fn resolve_session_project_filters(
    filters: &[String],
    sessions: &[SessionInfo],
    aicx_home: &Path,
    match_mode: ProjectMatchMode,
) -> Result<Vec<String>, SessionSurfaceError> {
    if filters.is_empty() {
        return Ok(Vec::new());
    }
    let corpus = identity_corpus(sessions, aicx_home);
    let resolution = match legacy_archive::resolve_project_identities(filters, &corpus, match_mode)
    {
        Ok(resolution) => resolution,
        Err(error) => return Err(project_error(error)),
    };
    if !resolution.unresolved_filters.is_empty() {
        let only_shaped = resolution
            .unresolved_filters
            .iter()
            .all(|filter| filter.contains('/'));
        if only_shaped {
            return Ok(resolution
                .unresolved_filters
                .into_iter()
                .chain(resolution.selected)
                .collect());
        }
        return Err(project_error(ProjectResolutionError::NoMatch {
            filters: resolution.unresolved_filters,
        }));
    }
    Ok(if resolution.selected.is_empty() {
        filters.to_vec()
    } else {
        resolution.selected
    })
}

fn identity_corpus(sessions: &[SessionInfo], aicx_home: &Path) -> Vec<String> {
    let mut identities = BTreeSet::new();
    if let Ok(from_index) = legacy_archive::project_identities_for_search_at(aicx_home) {
        identities.extend(from_index);
    }
    for session in sessions {
        if let Some(path) = session.repo_path.as_deref()
            && let Some(identity) = identity_from_repo_path(path)
        {
            identities.insert(identity);
        }
        if let Some(project) = session.project.as_deref()
            && project.contains('/')
        {
            identities.insert(project.to_string());
        }
    }
    identities.into_iter().collect()
}

fn identity_from_repo_path(path: &str) -> Option<String> {
    let segments: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect();
    let repo = segments.last()?;
    let owner = segments.get(segments.len().checked_sub(2)?)?;
    if owner.eq_ignore_ascii_case("Users")
        || owner.eq_ignore_ascii_case("home")
        || owner.eq_ignore_ascii_case("tmp")
        || owner.eq_ignore_ascii_case("var")
    {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

pub fn session_matches_project(session: &SessionInfo, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    if let Some(path) = session.repo_path.as_deref()
        && extraction::project_filter_matches_path(path, filters)
    {
        return true;
    }
    if let Some(project) = session.project.as_deref()
        && extraction::project_filter_matches_path(project, filters)
    {
        return true;
    }
    false
}

fn live_source_paths(
    aicx_home: &Path,
    user_home: &Path,
    since: Option<DateTime<Utc>>,
) -> BTreeSet<PathBuf> {
    let cutoff_ns = since
        .and_then(|dt| dt.timestamp_nanos_opt())
        .map(|nanos| nanos.max(0) as u128)
        .unwrap_or(0);
    crate::catalog::live_delta(aicx_home, user_home, cutoff_ns)
        .map(|delta| {
            delta
                .unadmitted
                .into_iter()
                .map(|entry| PathBuf::from(entry.source_path))
                .collect()
        })
        .unwrap_or_default()
}

fn session_item(session: SessionInfo, live_paths: &BTreeSet<PathBuf>) -> SessionListItem {
    let live = live_paths.contains(&session.source_path);
    SessionListItem {
        session_id: session.session_id,
        agent: session.agent,
        project: session.project,
        repo_path: session.repo_path,
        title: session.title,
        started_at: session.started_at.map(|ts| ts.to_rfc3339()),
        updated_at: session.updated_at.map(|ts| ts.to_rfc3339()),
        live,
        message_count: session.message_count,
        user_message_count: session.user_message_count,
        agent_message_count: session.agent_message_count,
        association: session.association,
        temporal_confidence: session.temporal_confidence,
        source_path: session.source_path.display().to_string(),
    }
}

fn list_empty_signal(
    scanned: usize,
    matched: usize,
    project_filtered: bool,
) -> (Option<&'static str>, Option<String>) {
    if matched > 0 {
        return (None, None);
    }
    if scanned == 0 {
        return (
            Some("no_sources_discovered"),
            Some(
                "scanned=0 session files under the operator home; this is not a project miss — no agent sources were readable"
                    .to_string(),
            ),
        );
    }
    if project_filtered {
        return (
            Some("no_sessions_for_project"),
            Some(format!(
                "scanned={scanned} session(s); matched=0 for this exact project filter — the project has no sessions in the window"
            )),
        );
    }
    (
        Some("no_sessions_in_window"),
        Some(format!(
            "scanned={scanned} session(s); matched=0 after agent/time filters (no project filter was set)"
        )),
    )
}

fn project_error(error: ProjectResolutionError) -> SessionSurfaceError {
    SessionSurfaceError::invalid(
        error.kind(),
        error.to_string(),
        json!({
            "filter": error.filter(),
            "candidates": error.candidates(),
        }),
    )
}

fn resolve_session(
    user_home: &Path,
    query: &str,
    agent: Option<&str>,
) -> Result<(AgentKind, ResolvedSource, CatalogIoStats), SessionSurfaceError> {
    let requested = parse_optional_agent(agent)?;
    let agents: Vec<AgentKind> = requested
        .map(|agent| vec![agent])
        .unwrap_or_else(|| AgentKind::ALL.to_vec());
    let mut hits = Vec::new();
    let mut scanned = 0usize;
    let mut last_missing: Option<(AgentKind, CatalogError)> = None;
    for agent in agents {
        let root = agent.session_root(user_home);
        if !root.is_dir() {
            continue;
        }
        let catalog = session_catalog::SessionCatalog::new(agent, &root)
            .map_err(|error| catalog_error(agent, error, 0))?;
        let lookup = catalog.resolve_with_stats(query);
        scanned += lookup.stats.metadata_candidates;
        match lookup.result {
            Ok(resolved) => hits.push((agent, resolved, lookup.stats)),
            Err(CatalogError::Missing { .. }) => {
                last_missing = Some((
                    agent,
                    CatalogError::Missing {
                        query: query.to_string(),
                        agent,
                        candidates_scanned: lookup.stats.metadata_candidates,
                    },
                ));
            }
            Err(CatalogError::Ambiguous { query, candidates }) => {
                return Err(catalog_error(
                    agent,
                    CatalogError::Ambiguous { query, candidates },
                    lookup.stats.metadata_candidates,
                ));
            }
            Err(error) => {
                return Err(catalog_error(
                    agent,
                    error,
                    lookup.stats.metadata_candidates,
                ));
            }
        }
    }
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => {
            if let Some((agent, error)) = last_missing {
                return Err(catalog_error(agent, error, scanned));
            }
            Err(SessionSurfaceError::invalid(
                "session_not_found",
                format!("no session matched `{query}` ({scanned} candidate source(s) scanned)"),
                json!({
                    "session": query,
                    "candidates_scanned": scanned,
                    "agent": agent,
                }),
            ))
        }
        _ => Err(SessionSurfaceError::invalid(
            "session_ambiguous",
            format!(
                "session `{query}` matched {} sources across agents; use the full id and/or agent",
                hits.len()
            ),
            json!({
                "session": query,
                "candidates": hits.iter().map(|(agent, resolved, _)| json!({
                    "agent": agent.as_str(),
                    "session_id": resolved.source.source_id,
                    "path": resolved.source.path.display().to_string(),
                })).collect::<Vec<_>>(),
            }),
        )),
    }
}

fn catalog_error(agent: AgentKind, error: CatalogError, scanned: usize) -> SessionSurfaceError {
    match error {
        CatalogError::InvalidQuery(query) => SessionSurfaceError::invalid(
            "invalid_session_reference",
            format!("session reference `{query}` is not a valid session identity"),
            json!({ "session": query, "agent": agent.as_str() }),
        ),
        CatalogError::Missing {
            query,
            candidates_scanned,
            ..
        } => SessionSurfaceError::invalid(
            "session_not_found",
            format!(
                "no {} session matched `{query}` ({} candidate source(s) scanned)",
                agent.as_str(),
                if scanned > 0 {
                    scanned
                } else {
                    candidates_scanned
                }
            ),
            json!({
                "session": query,
                "agent": agent.as_str(),
                "candidates_scanned": if scanned > 0 { scanned } else { candidates_scanned },
            }),
        ),
        CatalogError::Ambiguous { query, candidates } => SessionSurfaceError::invalid(
            "session_ambiguous",
            format!(
                "session `{query}` matched {} {} source(s); use the full id",
                candidates.len(),
                agent.as_str()
            ),
            json!({
                "session": query,
                "agent": agent.as_str(),
                "candidates": candidates.iter().map(|candidate| json!({
                    "session_id": candidate.source_id,
                    "path": candidate.path.display().to_string(),
                })).collect::<Vec<_>>(),
            }),
        ),
        CatalogError::Io { path, message } => SessionSurfaceError::Internal {
            message: format!("session catalog I/O at {}: {message}", path.display()),
        },
    }
}

fn session_info_for_resolved(agent: AgentKind, resolved: &ResolvedSource) -> SessionInfo {
    let path = &resolved.source.path;
    let fallback = SessionInfo {
        session_id: resolved
            .source
            .logical_session_id
            .clone()
            .unwrap_or_else(|| resolved.source.source_id.clone()),
        agent: agent.as_str().to_string(),
        project: None,
        repo_path: None,
        started_at: None,
        updated_at: None,
        message_count: 0,
        user_message_count: 0,
        agent_message_count: 0,
        title: None,
        source_path: path.clone(),
        association: sessions::Association::Unknown,
        temporal_confidence: sessions::TemporalConfidence::None,
    };
    sessions::find_session_by_id(&guess_user_home(path, agent), &fallback.session_id)
        .unwrap_or(fallback)
}

fn guess_user_home(source: &Path, agent: AgentKind) -> PathBuf {
    let marker = match agent {
        AgentKind::Claude => ".claude",
        AgentKind::Codex => ".codex",
        AgentKind::Gemini => ".gemini",
        AgentKind::Grok => ".grok",
        AgentKind::Junie => ".junie",
    };
    let mut current = source;
    while let Some(parent) = current.parent() {
        if current.file_name().and_then(|name| name.to_str()) == Some(marker) {
            return parent.to_path_buf();
        }
        current = parent;
    }
    source.parent().unwrap_or(Path::new("/")).to_path_buf()
}

fn extract_conversation(
    resolved: &ResolvedSource,
    agent: AgentKind,
    user_only: bool,
) -> Result<ConversationPayload, String> {
    let parsed = crate::parser_dispatch::parse_file(
        agent.parser_kind(),
        &resolved.source.source_id,
        resolved.source.logical_session_id.clone(),
        &resolved.source.path,
    )
    .map_err(|error| {
        format!(
            "unsupported or unreadable source {}: {error}",
            resolved.source.path.display()
        )
    })?;
    let mut entries = crate::output::timeline_entries_from_model(parsed.model());
    if user_only {
        entries
            .retain(|entry| entry.role == "user" || entry.frame_kind == Some(FrameKind::UserMsg));
    }
    if entries.is_empty() {
        return Err(format!(
            "resolved `{}` but no extractable conversation turns were present",
            resolved.source.source_id
        ));
    }
    let projection = extraction::to_conversation_with_stats(&entries, &[]);
    let markdown = render_conversation_markdown(
        &resolved.source.source_id,
        agent.as_str(),
        &projection.messages,
    );
    Ok(ConversationPayload {
        message_count: projection.messages.len(),
        harness_noise_dropped: projection.harness_noise_dropped,
        messages: projection.messages,
        markdown,
    })
}

fn render_conversation_markdown(
    session_id: &str,
    agent: &str,
    messages: &[ConversationMessage],
) -> String {
    let mut out = String::new();
    out.push_str("# Session extract\n\n");
    out.push_str(&format!("- agent: {agent}\n"));
    out.push_str(&format!("- session: {session_id}\n"));
    out.push_str(&format!("- messages: {}\n\n", messages.len()));
    for message in messages {
        out.push_str(&format!(
            "**[{}] {}:**\n\n{}\n\n",
            message.timestamp.format("%Y-%m-%dT%H:%M:%SZ"),
            message.role,
            message.message
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_home(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aicx-mcp-session-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("home");
        root
    }

    fn write_claude(home: &Path, encoded_cwd: &str, session_id: &str, cwd: &str, user: &str) {
        let dir = home.join(".claude").join("projects").join(encoded_cwd);
        fs::create_dir_all(&dir).expect("claude project");
        let path = dir.join(format!("{session_id}.jsonl"));
        let mut file = fs::File::create(path).expect("session");
        writeln!(
            file,
            r#"{{"type":"user","cwd":"{cwd}","sessionId":"{session_id}","message":{{"role":"user","content":"{user}"}},"timestamp":"2026-08-16T12:00:00.000Z"}}"#
        )
        .expect("write user");
        writeln!(
            file,
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"ok"}}]}},"timestamp":"2026-08-16T12:00:01.000Z"}}"#
        )
        .expect("write assistant");
    }

    #[test]
    fn list_exact_project_is_loud_empty_not_foreign_sessions() {
        let home = temp_home("list");
        let aicx_home = home.join(".aicx");
        fs::create_dir_all(&aicx_home).expect("aicx home");
        write_claude(
            &home,
            "-Volumes-vc-workspace-Loctree-aicx",
            "aaaaaaaa-1111-2222-3333-444444444444",
            "/Volumes/vc-workspace/Loctree/aicx",
            "work on aicx mcp",
        );
        write_claude(
            &home,
            "-Users-someone-Downloads-ChatGPT-export",
            "bbbbbbbb-1111-2222-3333-444444444444",
            "/Users/tester/Downloads/ChatGPT-export",
            "chatgpt dump",
        );

        let aicx = list_sessions(ListSessionsRequest {
            user_home: &home,
            aicx_home: &aicx_home,
            project: Some("/aicx"),
            projects: &[],
            project_match: ProjectMatchMode::Exact,
            agent: None,
            hours: 0,
            since: None,
            limit: 20,
        })
        .expect("list /aicx");
        assert!(!aicx.empty, "{aicx:?}");
        assert_eq!(aicx.matched, 1);
        assert_eq!(aicx.scanned, 2);
        assert_eq!(
            aicx.sessions[0].session_id,
            "aaaaaaaa-1111-2222-3333-444444444444"
        );
        assert!(
            aicx.sessions[0]
                .repo_path
                .as_deref()
                .is_some_and(|path| path.ends_with("Loctree/aicx"))
        );

        let empty = list_sessions(ListSessionsRequest {
            user_home: &home,
            aicx_home: &aicx_home,
            project: Some("vetcoders/vibecrafted"),
            projects: &[],
            project_match: ProjectMatchMode::Exact,
            agent: None,
            hours: 0,
            since: None,
            limit: 20,
        })
        .expect("list missing project");
        assert!(empty.empty);
        assert_eq!(empty.empty_kind, Some("no_sessions_for_project"));
        assert_eq!(empty.scanned, 2);
        assert_eq!(empty.matched, 0);
        assert!(
            empty
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains("scanned=2"))
        );

        let unknown = list_sessions(ListSessionsRequest {
            user_home: &home,
            aicx_home: &aicx_home,
            project: Some("not-a-real-bucket"),
            projects: &[],
            project_match: ProjectMatchMode::Exact,
            agent: None,
            hours: 0,
            since: None,
            limit: 20,
        });
        let err = unknown.expect_err("bare unknown project must fail closed");
        match err {
            SessionSurfaceError::InvalidParams { payload, .. } => {
                assert_eq!(payload["error"], "project_not_found");
            }
            other => panic!("expected project_not_found, got {other:?}"),
        }

        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn list_percent_encoded_grok_matches_cross_org_repo_filter() {
        let home = temp_home("grok-filter");
        let aicx_home = home.join(".aicx");
        fs::create_dir_all(&aicx_home).expect("aicx home");
        let dir = home
            .join(".grok")
            .join("sessions")
            .join("%2FVolumes%2Fvc-workspace%2FLoctree%2Faicx")
            .join("01a00c9a-2344-70f1-8446-c31b4ca80d0e");
        fs::create_dir_all(&dir).expect("grok session");
        fs::write(
            dir.join("chat_history.jsonl"),
            concat!(
                r#"{"type":"user","content":[{"type":"text","text":"fix /aicx filter"}]}"#,
                "\n",
                r#"{"type":"assistant","content":"ok"}"#,
                "\n"
            ),
        )
        .expect("write grok");

        let listed = list_sessions(ListSessionsRequest {
            user_home: &home,
            aicx_home: &aicx_home,
            project: Some("/aicx"),
            projects: &[],
            project_match: ProjectMatchMode::Exact,
            agent: Some("grok"),
            hours: 0,
            since: None,
            limit: 20,
        })
        .expect("list /aicx grok");
        assert!(!listed.empty, "{listed:?}");
        assert_eq!(listed.matched, 1);
        assert_eq!(
            listed.sessions[0].session_id,
            "01a00c9a-2344-70f1-8446-c31b4ca80d0e"
        );
        assert_eq!(
            listed.sessions[0].repo_path.as_deref(),
            Some("/Volumes/vc-workspace/Loctree/aicx")
        );
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn show_extracts_conversation_and_ambiguous_prefix_is_loud() {
        let home = temp_home("show");
        write_claude(
            &home,
            "-Volumes-vc-workspace-Loctree-aicx",
            "cccccccc-1111-2222-3333-444444444444",
            "/Volumes/vc-workspace/Loctree/aicx",
            "open the session chain",
        );
        write_claude(
            &home,
            "-Volumes-vc-workspace-Loctree-aicx",
            "cccccccc-aaaa-2222-3333-444444444444",
            "/Volumes/vc-workspace/Loctree/aicx",
            "second same prefix",
        );

        let shown = show_session(ShowSessionRequest {
            user_home: &home,
            session: "cccccccc-1111-2222-3333-444444444444",
            agent: Some("claude"),
            conversation: true,
            user_only: false,
        })
        .expect("show exact");
        assert_eq!(
            shown.session.session_id,
            "cccccccc-1111-2222-3333-444444444444"
        );
        let conversation = shown.conversation.expect("conversation");
        assert!(conversation.message_count >= 1);
        assert!(conversation.markdown.contains("open the session chain"));
        assert!(
            !conversation
                .markdown
                .to_ascii_lowercase()
                .contains("recover previous session")
        );

        let ambiguous = show_session(ShowSessionRequest {
            user_home: &home,
            session: "cccccccc",
            agent: Some("claude"),
            conversation: false,
            user_only: false,
        });
        let err = ambiguous.expect_err("prefix must not auto-pick");
        match err {
            SessionSurfaceError::InvalidParams { payload, .. } => {
                assert_eq!(payload["error"], "session_ambiguous");
            }
            other => panic!("expected session_ambiguous, got {other:?}"),
        }

        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn continuity_pack_is_context_only() {
        let home = temp_home("continuity");
        let aicx_home = home.join(".aicx");
        fs::create_dir_all(&aicx_home).expect("aicx home");
        let pack = continuity_pack(ContinuityRequest {
            aicx_home: &aicx_home,
            project: Some("Loctree/aicx"),
            projects: &[],
            project_match: ProjectMatchMode::Exact,
            hours: 24,
            for_inject: false,
        })
        .expect("continuity");
        assert!(pack.ok);
        assert!(!pack.native_resume);
        assert_eq!(pack.mode, "context_pack");
        assert!(pack.markdown.contains("## NOW"));
        assert!(pack.markdown.contains("## PEERS"));
        assert!(pack.markdown.contains("## DECISIONS"));
        assert!(pack.markdown.contains("## TASKS"));
        assert!(pack.markdown.contains("## SOURCES"));
        assert!(pack.markdown.contains("## INDEX HEALTH"));
        assert!(pack.markdown.contains("Context pack only"));
        let lower = pack.markdown.to_ascii_lowercase();
        assert!(!lower.contains("recover previous session"));
        assert!(!lower.contains("continue that session"));
        assert!(!lower.contains("claude --resume"));

        let missing_project = continuity_pack(ContinuityRequest {
            aicx_home: &aicx_home,
            project: None,
            projects: &[],
            project_match: ProjectMatchMode::Exact,
            hours: 24,
            for_inject: false,
        });
        let err = missing_project.expect_err("project required");
        match err {
            SessionSurfaceError::InvalidParams { payload, .. } => {
                assert_eq!(payload["error"], "project_required");
            }
            other => panic!("expected project_required, got {other:?}"),
        }
        let _ = fs::remove_dir_all(home);
    }
}
