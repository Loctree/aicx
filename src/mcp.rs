//! MCP (Model Context Protocol) server for aicx.
//!
//! Exposes aicx functionality as MCP tools so agents can search canonical
//! chunks, rank artifacts, and retrieve steer metadata.
//!
//! Supports stdio and streamable HTTP transports.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use clap::ValueEnum;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{
    ErrorData as McpError, handler::server::tool::ToolRouter, handler::server::wrapper::Parameters,
    model::*, tool, tool_router,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    error::Error,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::api;
use crate::auth::{self, AuthConfig};
use crate::intents::{self, IntentKind, IntentsConfig};
use crate::oracle::OracleStatus;
use crate::rank;
use crate::store;
use crate::timeline::{FrameKind, Kind};

use rmcp::transport::streamable_http_server::session::{
    RestoreOutcome, ServerSseMessage, SessionId, SessionManager,
    local::{LocalSessionManager, LocalSessionManagerError, SessionConfig, SessionTransport},
};

// ============================================================================
// Tool parameter & result types
// ============================================================================

const MCP_SESSION_IDLE_TTL: Duration = Duration::from_secs(30 * 60);
const MCP_SESSION_MAX_SESSIONS: usize = 1000;
const MCP_SESSION_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// D-6: once the embedder fails to load (model not hydrated, cloud creds
/// missing, ...) the MCP server short-circuits subsequent semantic search
/// requests for this long so a flapping endpoint cannot retry-storm the
/// embedder. 5 minutes balances "recover quickly when the operator fixes
/// the config" against "stop hammering the same broken path".
const MCP_EMBEDDER_NEGATIVE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug)]
struct AicxSessionManager {
    inner: LocalSessionManager,
    idle_ttl: Duration,
    max_sessions: usize,
    last_seen: tokio::sync::RwLock<HashMap<SessionId, Instant>>,
}

#[derive(Debug)]
enum AicxSessionManagerError {
    Inner(LocalSessionManagerError),
    SessionLimitExceeded(usize),
}

impl fmt::Display for AicxSessionManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inner(err) => write!(f, "{err}"),
            Self::SessionLimitExceeded(max) => {
                write!(f, "MCP session limit exceeded ({max} active sessions)")
            }
        }
    }
}

impl Error for AicxSessionManagerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Inner(err) => Some(err),
            Self::SessionLimitExceeded(_) => None,
        }
    }
}

impl From<LocalSessionManagerError> for AicxSessionManagerError {
    fn from(err: LocalSessionManagerError) -> Self {
        Self::Inner(err)
    }
}

impl AicxSessionManager {
    fn new(idle_ttl: Duration, max_sessions: usize) -> Self {
        let mut session_config = SessionConfig::default();
        session_config.keep_alive = Some(idle_ttl);
        let mut inner = LocalSessionManager::default();
        inner.session_config = session_config;
        Self {
            // rmcp exposes idle timeout through SessionConfig::keep_alive, but
            // does not expose a max-session knob; this wrapper adds that cap.
            inner,
            idle_ttl,
            max_sessions,
            last_seen: tokio::sync::RwLock::new(HashMap::new()),
        }
    }

    async fn session_count(&self) -> usize {
        self.inner.sessions.read().await.len()
    }

    async fn touch_session(&self, id: &SessionId) {
        self.last_seen
            .write()
            .await
            .insert(id.clone(), Instant::now());
    }

    async fn sweep_idle_sessions(&self) -> Result<usize, AicxSessionManagerError> {
        self.sweep_idle_sessions_at(Instant::now()).await
    }

    async fn sweep_idle_sessions_at(&self, now: Instant) -> Result<usize, AicxSessionManagerError> {
        let expired = {
            let last_seen = self.last_seen.read().await;
            last_seen
                .iter()
                .filter_map(|(id, last)| {
                    let idle_for = now.checked_duration_since(*last).unwrap_or_default();
                    (idle_for >= self.idle_ttl).then(|| id.clone())
                })
                .collect::<Vec<_>>()
        };

        let mut closed = 0usize;
        for id in expired {
            let still_expired = {
                let last_seen = self.last_seen.read().await;
                last_seen.get(&id).is_some_and(|last| {
                    now.checked_duration_since(*last).unwrap_or_default() >= self.idle_ttl
                })
            };
            if still_expired {
                self.close_session(&id).await?;
                closed += 1;
            }
        }
        Ok(closed)
    }

    async fn ensure_session_capacity(&self) -> Result<(), AicxSessionManagerError> {
        self.sweep_idle_sessions().await?;
        let active = self.session_count().await;
        if active >= self.max_sessions {
            return Err(AicxSessionManagerError::SessionLimitExceeded(
                self.max_sessions,
            ));
        }
        Ok(())
    }
}

impl SessionManager for AicxSessionManager {
    type Error = AicxSessionManagerError;
    type Transport = SessionTransport;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        self.ensure_session_capacity().await?;
        let (id, transport) = self.inner.create_session().await?;
        self.touch_session(&id).await;
        Ok((id, transport))
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        let response = self.inner.initialize_session(id, message).await?;
        self.touch_session(id).await;
        Ok(response)
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        self.last_seen.write().await.remove(id);
        self.inner.close_session(id).await?;
        Ok(())
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        let exists = self.inner.has_session(id).await?;
        if exists {
            self.touch_session(id).await;
        } else {
            self.last_seen.write().await.remove(id);
        }
        Ok(exists)
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl futures::Stream<Item = ServerSseMessage> + Send + 'static, Self::Error> {
        let stream = self.inner.create_stream(id, message).await?;
        self.touch_session(id).await;
        Ok(stream)
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        self.inner.accept_message(id, message).await?;
        self.touch_session(id).await;
        Ok(())
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl futures::Stream<Item = ServerSseMessage> + Send + 'static, Self::Error> {
        let stream = self.inner.create_standalone_stream(id).await?;
        self.touch_session(id).await;
        Ok(stream)
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl futures::Stream<Item = ServerSseMessage> + Send + 'static, Self::Error> {
        let stream = self.inner.resume(id, last_event_id).await?;
        self.touch_session(id).await;
        Ok(stream)
    }

    async fn restore_session(
        &self,
        id: SessionId,
    ) -> Result<RestoreOutcome<Self::Transport>, Self::Error> {
        self.ensure_session_capacity().await?;
        let outcome = self.inner.restore_session(id.clone()).await?;
        match &outcome {
            RestoreOutcome::Restored(_) | RestoreOutcome::AlreadyPresent => {
                self.touch_session(&id).await;
            }
            RestoreOutcome::NotSupported => {
                self.last_seen.write().await.remove(&id);
            }
            _ => {}
        }
        Ok(outcome)
    }
}

fn configured_mcp_session_manager() -> Arc<AicxSessionManager> {
    Arc::new(AicxSessionManager::new(
        MCP_SESSION_IDLE_TTL,
        MCP_SESSION_MAX_SESSIONS,
    ))
}

fn spawn_mcp_session_cleanup(manager: Arc<AicxSessionManager>, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if let Err(err) = manager.sweep_idle_sessions().await {
                tracing::warn!("MCP session cleanup failed: {err}");
            }
        }
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum McpTransport {
    Stdio,
    #[value(alias = "sse")]
    Http,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Search query text
    pub query: String,
    /// Max results to return (default: 10)
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Optional project filter (case-insensitive substring). Prefer
    /// `projects` for cross-project search.
    pub project: Option<String>,
    /// Optional project filters for cross-project search.
    pub projects: Option<Vec<String>>,
    /// Minimum score threshold (0-100)
    pub score: Option<u8>,
    /// Hours to look back (0 = all time)
    pub hours: Option<u64>,
    /// Optional agent filter
    pub agent: Option<String>,
    /// Optional date filter (single day or range)
    pub date: Option<String>,
    /// Optional lower date bound or single-day shorthand
    pub since: Option<String>,
    /// Optional upper date bound
    pub until: Option<String>,
    /// Optional sort order: newest, oldest, score
    pub sort: Option<String>,
    /// Optional frame/channel filter: user_msg, agent_reply, internal_thought, tool_call
    pub frame_kind: Option<FrameKind>,
    /// Optional canonical corpus kind filter: conversations, plans, reports, other
    pub kind: Option<String>,
    /// Return only metadata without full snippet content
    #[serde(default = "default_true")]
    pub slim: bool,
    /// Return snippet + full evidence
    #[serde(default)]
    pub verbose: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadParams {
    /// Absolute path, store-relative path, file name, or compact chunk reference
    pub reference: String,
    /// Truncate chunk content to this many UTF-8 characters
    pub max_chars: Option<usize>,
}

fn default_limit() -> usize {
    20
}

fn default_true() -> bool {
    true
}

fn search_project_scopes(
    store_root: &std::path::Path,
    projects: &[String],
) -> anyhow::Result<Vec<Option<String>>> {
    if projects.is_empty() {
        return Ok(vec![None]);
    }
    let resolved =
        store::resolve_filters_to_store_or_index_slugs_at_or_error(store_root, projects)?;
    Ok(resolved.into_iter().map(Some).collect())
}

fn dedup_metadata_by_path(items: &mut Vec<serde_json::Value>) {
    let mut seen = std::collections::BTreeSet::new();
    items.retain(|item| {
        let key = item
            .get("path")
            .or_else(|| item.get("source_chunk"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| item.to_string());
        seen.insert(key)
    });
}

const MAX_SCORE_FILTER: u8 = 100;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RankParams {
    /// Project name (required)
    pub project: String,
    /// Hours to look back (default: 72, 0 = all time)
    #[serde(default = "default_rank_hours")]
    pub hours: u64,
    /// Only show chunks scoring >= 5
    #[serde(default)]
    pub strict: bool,
    /// Optional agent filter
    pub agent: Option<String>,
    /// Optional lower date bound or single-day shorthand
    pub since: Option<String>,
    /// Optional upper date bound
    pub until: Option<String>,
    /// Optional sort order: newest, oldest, score
    pub sort: Option<String>,
    /// Show only top N bundles
    pub top: Option<usize>,
    /// Max results to return
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Return only metadata without full snippet content
    #[serde(default = "default_true")]
    pub slim: bool,
    /// Return snippet + full evidence
    #[serde(default)]
    pub verbose: bool,
}

fn default_rank_hours() -> u64 {
    72
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SteerParams {
    /// Filter by run_id (exact match against sidecar metadata)
    pub run_id: Option<String>,
    /// Filter by prompt_id (exact match against sidecar metadata)
    pub prompt_id: Option<String>,
    /// Filter by agent name: claude, codex, gemini (case-insensitive)
    pub agent: Option<String>,
    /// Filter by kind: conversations, plans, reports, other
    pub kind: Option<String>,
    /// Filter by frame/channel: user_msg, agent_reply, internal_thought, tool_call
    pub frame_kind: Option<FrameKind>,
    /// Filter by project using strict canonical `<organization>/<repository>`
    /// matching (case-insensitive). Bare names match either an organization
    /// or a repository token; `owner/` matches every repo under that owner;
    /// `/repo` matches that repo across all owners. Substring matching is
    /// intentionally not supported — `vista` does NOT match `vista-portal`.
    pub project: Option<String>,
    /// Optional project filters for cross-project steering. Each entry uses
    /// the same strict canonical semantics as `project`.
    pub projects: Option<Vec<String>>,
    /// Filter by date (YYYY-MM-DD, or range like 2026-03-20..2026-03-28)
    pub date: Option<String>,
    /// Max results (default: 20)
    #[serde(default = "default_steer_limit")]
    pub limit: usize,
    /// Minimum score threshold (0-100)
    pub score: Option<u8>,
    /// Sort order (newest, oldest, score)
    pub sort: Option<String>,
    /// Date boundary
    pub since: Option<String>,
    /// Date boundary
    pub until: Option<String>,
    /// Return only metadata without full snippet content
    #[serde(default = "default_true")]
    pub slim: bool,
    /// Return snippet + full evidence
    #[serde(default)]
    pub verbose: bool,
}

fn default_steer_limit() -> usize {
    20
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IntentsParams {
    /// Optional project filter (case-insensitive substring; empty/None = all projects)
    #[serde(default)]
    pub project: Option<String>,
    /// Optional project filters for cross-project intent extraction.
    pub projects: Option<Vec<String>>,
    /// Hours to look back (default: 720 = 30 days, 0 = all time). Matches CLI default.
    #[serde(default = "default_intents_hours")]
    pub hours: u64,
    /// Strict mode: only emit high-confidence intents (default: false)
    #[serde(default)]
    pub strict: bool,
    /// Optional kind filter: decision, intent, outcome, task
    pub kind: Option<String>,
    /// Optional frame/channel filter: user_msg, agent_reply, internal_thought, tool_call
    pub frame_kind: Option<FrameKind>,
    /// Filter to intent entries lacking a matching outcome in the same session
    #[serde(default)]
    pub unresolved: bool,
    /// Collapse multiple intents from the same session into one entry with count
    #[serde(default)]
    pub collapse_session: bool,
    /// Optional agent filter (claude, codex, gemini, junie)
    pub agent: Option<String>,
    /// Optional lower date bound (YYYY-MM-DD or single-day shorthand like 2026-04-23..)
    pub since: Option<String>,
    /// Optional upper date bound (YYYY-MM-DD)
    pub until: Option<String>,
    /// Sort order: newest (default), oldest
    pub sort: Option<String>,
    /// Max records to return (default: 20, capped at 500)
    #[serde(default = "default_intents_limit")]
    pub limit: usize,
    /// Output format: json, markdown (default). Matches CLI `emit` naming.
    #[serde(default = "default_intents_emit")]
    pub emit: String,
    /// Return only metadata without full snippet content
    #[serde(default = "default_true")]
    pub slim: bool,
    /// Return snippet + full evidence
    #[serde(default)]
    pub verbose: bool,
}

fn default_intents_hours() -> u64 {
    720
}

fn default_intents_limit() -> usize {
    20
}

fn default_intents_emit() -> String {
    "markdown".to_string()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IndexStatusParams {
    /// Optional project bucket filter. Omit (or pass null) to query the
    /// cross-project `_all` bucket. Matches the same canonical lowercase
    /// slug rules the index writer uses on disk.
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Serialize)]
struct RankResponse {
    project: String,
    hours: u64,
    strict: bool,
    results: usize,
    items: Vec<RankItem>,
}

#[derive(Debug, Serialize)]
struct RankItem {
    file: String,
    project: String,
    date: String,
    timestamp: Option<String>,
    kind: String,
    agent: String,
    score: u8,
    label: String,
    signal: usize,
    noise: usize,
    total: usize,
    density: String,
}

#[derive(Debug, Serialize)]
struct SteerResponse {
    oracle_status: OracleStatus,
    results: usize,
    items: Vec<serde_json::Value>,
}

#[cfg(test)]
fn background_refresh_args(hours: u64, project: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "all".to_string(),
        "-H".to_string(),
        hours.to_string(),
        "--emit".to_string(),
        "none".to_string(),
    ];

    if let Some(project) = project {
        args.push("-p".to_string());
        args.push(project.to_string());
    }

    args
}

// ============================================================================
// MCP Server
// ============================================================================

fn validate_string_len(val: Option<&str>, max: usize, field: &str) -> Result<(), McpError> {
    if let Some(v) = val
        && v.len() > max
    {
        return Err(McpError::invalid_params(
            format!("Field '{}' exceeds max length {} bytes", field, max),
            None,
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct McpSemanticFilterBuild {
    post_filters: crate::search_engine::SemanticSearchFilters,
}

fn build_mcp_semantic_filters(
    params: &SearchParams,
    score: Option<u8>,
    now: chrono::DateTime<chrono::Utc>,
) -> McpSemanticFilterBuild {
    // Date filter (explicit `date` or shorthand `since`) wins over
    // `--hours`, preserving the legacy precedence.
    let date_effective = params.date.clone().or(params.since.clone());
    let (date_lo, date_hi) = if let Some(ref date_filter) = date_effective {
        parse_date_filter_mcp(date_filter)
    } else {
        (None, params.until.clone())
    };
    let hours = params.hours.unwrap_or(0);
    let hours_cutoff = if hours > 0 && date_lo.is_none() && date_hi.is_none() {
        let cutoff = now - chrono::Duration::hours(hours as i64);
        Some(cutoff.format("%Y-%m-%d").to_string())
    } else {
        None
    };

    McpSemanticFilterBuild {
        post_filters: crate::search_engine::SemanticSearchFilters {
            agent: params.agent.clone(),
            score_min: score,
            date_lo,
            date_hi,
            hours_cutoff,
        },
    }
}

fn inject_mcp_filter_pushdown_payload(
    rendered: &str,
    diagnostic: Option<&crate::search_engine::FilterPushdownDiagnostic>,
) -> Result<String, McpError> {
    crate::search_engine::inject_filter_pushdown_diagnostic(rendered, diagnostic)
        .map_err(|e| McpError::internal_error(format!("Inject filter_pushdown JSON: {e}"), None))
}

#[derive(Clone)]
pub struct AicxMcpServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    embedder_unavailable_until: Arc<std::sync::Mutex<Option<Instant>>>,
}

impl Default for AicxMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl AicxMcpServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            embedder_unavailable_until: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn embedder_unavailable_guard(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
        self.embedder_unavailable_until.lock().expect(
            "embedder_unavailable_until mutex poisoned; negative-cache state is only mutated by AicxMcpServer",
        )
    }

    /// D-6: returns remaining negative-cache TTL when the embedder was
    /// flagged unavailable. Side effect: lazily clears the entry when it
    /// has expired so a subsequent retry hits the embedder again.
    fn embedder_negative_cache_remaining(&self, now: Instant) -> Option<Duration> {
        let mut guard = self.embedder_unavailable_guard();
        match *guard {
            Some(until) if until > now => Some(until.duration_since(now)),
            Some(_) => {
                *guard = None;
                None
            }
            None => None,
        }
    }

    /// D-6: arm the negative cache for `ttl` from `now`. Called after a
    /// real embedder failure so subsequent requests fail-fast.
    fn mark_embedder_unavailable(&self, now: Instant, ttl: Duration) {
        let mut guard = self.embedder_unavailable_guard();
        *guard = Some(now + ttl);
    }

    #[tool(
        name = "aicx_search",
        description = "Semantic search over the canonical corpus. Fails fast with kind/reason/recommendation when the index or embedder is not ready. Filter pushdown (agent/score/date/hours) iterates a bounded retrieval pool (up to 10x the requested limit) so a corpus whose top-N raw hits all sit outside the filter window still surfaces inside-window matches further down the ranking instead of returning silent-empty. When the cap is examined without satisfying the limit, the response carries a `filter_pushdown` payload with `kind=\"filter_yielded_partial\"` so callers can distinguish bounded under-delivery from a genuinely empty corpus."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        // F-P3-18: audit log records tool-name entry only. NEVER include
        // arguments (query/project/agent/...) — they may carry PII or
        // operator secrets and the audit log is intended to be safe to
        // archive long-term.
        tracing::info!(target: "mcp.audit", tool_name = "aicx_search", "mcp tool invoked");

        // D-6: short-circuit while the embedder is in the negative-cache
        // window. Returns a structured error the MCP caller can act on
        // without further round-trips to the embedder.
        let now = Instant::now();
        if let Some(remaining) = self.embedder_negative_cache_remaining(now) {
            let payload = serde_json::json!({
                "ok": false,
                "error": "semantic_search_unavailable",
                "kind": "embedder_unavailable",
                "reason": format!(
                    "embedder negative cache active for another {}s — last embed attempt failed",
                    remaining.as_secs()
                ),
                "recommendation": "run `aicx doctor` to inspect embedder health; the cache clears automatically after the TTL elapses",
            });
            return Err(McpError::invalid_params(
                format!(
                    "semantic search unavailable [embedder_unavailable]: negative cache active for {}s",
                    remaining.as_secs()
                ),
                Some(payload),
            ));
        }

        validate_string_len(Some(params.query.as_str()), 4096, "query")?;
        validate_string_len(params.project.as_deref(), 4096, "project")?;
        validate_string_len(params.agent.as_deref(), 4096, "agent")?;
        validate_string_len(params.date.as_deref(), 4096, "date")?;
        validate_string_len(params.since.as_deref(), 4096, "since")?;
        validate_string_len(params.until.as_deref(), 4096, "until")?;
        validate_string_len(params.sort.as_deref(), 4096, "sort")?;
        if let Some(projects) = &params.projects {
            for (i, p) in projects.iter().enumerate() {
                validate_string_len(Some(p), 4096, &format!("projects[{}]", i))?;
            }
        }

        let score = validate_score_filter(params.score)?;
        let filter_build = build_mcp_semantic_filters(&params, score, chrono::Utc::now());
        let frame_kind = params.frame_kind;
        let kind_filter = match params.kind.as_deref() {
            Some(kind) => match Kind::parse(kind) {
                Some(kind) => Some(kind),
                None => {
                    let payload = serde_json::json!({
                        "ok": false,
                        "error": "invalid_kind_filter",
                        "kind": kind,
                        "expected": ["conversations", "plans", "reports", "other"],
                    });
                    return Err(McpError::invalid_params(
                        format!(
                            "invalid kind filter `{kind}`; expected conversations, plans, reports, or other"
                        ),
                        Some(payload),
                    ));
                }
            },
            None => None,
        };
        let query = params.query;
        let limit = params.limit.min(50);
        let project = params.project;
        let owned_projects = params
            .projects
            .clone()
            .filter(|projects| !projects.is_empty())
            .unwrap_or_else(|| project.clone().into_iter().collect());
        let store_root = store::store_base_dir()
            .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;
        let project_scopes_owned = search_project_scopes(&store_root, &owned_projects)
            .map_err(|e| McpError::invalid_params(format!("Project filter: {e}"), None))?;
        let project_scopes: Vec<Option<&str>> = project_scopes_owned
            .iter()
            .map(|scope| scope.as_deref())
            .collect();

        let post_filters = filter_build.post_filters;

        // Semantic-only dispatch with filter pushdown. No fuzzy fallback.
        // The wrapper iterates a bounded retrieval pool (`FILTER_EXAMINED_CAP_RATIO`
        // x `limit`) so the canonical pathology — top-N raw hits sit
        // outside the filter window while valid hits exist below — does
        // not surface as silent-empty. When a precondition is missing
        // (embedder unhydrated, index not built, ...) the wrapper still
        // returns a structured McpError carrying the same `kind` +
        // `reason` + `recommendation` triple the CLI fail-fast surface
        // emits.
        let filtered = match crate::search_engine::try_semantic_search_filtered(
            &store_root,
            &query,
            limit,
            &project_scopes,
            frame_kind,
            kind_filter.map(|kind| kind.dir_name()),
            &post_filters,
        ) {
            Ok(filtered) => filtered,
            Err(err) => {
                // D-6: arm the negative cache on real embedder failures so
                // subsequent requests fail-fast for MCP_EMBEDDER_NEGATIVE_TTL
                // instead of re-running the same broken bootstrap. Other
                // kinds (index missing, dim mismatch, ...) remain
                // synchronously retryable.
                if err.kind() == "embedder_unavailable" {
                    self.mark_embedder_unavailable(Instant::now(), MCP_EMBEDDER_NEGATIVE_TTL);
                }
                let payload = serde_json::json!({
                    "ok": false,
                    "error": "semantic_search_unavailable",
                    "kind": err.kind(),
                    "reason": err.reason(),
                    "recommendation": err.recommendation(),
                });
                return Err(McpError::invalid_params(
                    format!(
                        "semantic search unavailable [{}]: {} — recommendation: {}",
                        err.kind(),
                        err.reason(),
                        err.recommendation()
                    ),
                    Some(payload),
                ));
            }
        };
        let crate::search_engine::FilteredSemanticOutcome {
            outcome,
            diagnostic: pushdown_diagnostic,
        } = filtered;
        let scanned = outcome.scanned;
        let retrieval_status = outcome.retrieval_status.clone();
        let mut results = outcome.results;

        if let Some(sort_order) = params.sort.as_deref() {
            results.sort_by(|a, b| {
                let t_a = a.timestamp.as_deref().unwrap_or(a.date.as_str());
                let t_b = b.timestamp.as_deref().unwrap_or(b.date.as_str());
                match sort_order {
                    "newest" => t_b.cmp(t_a),
                    "oldest" => t_a.cmp(t_b),
                    "score" => b.score.cmp(&a.score).then(t_b.cmp(t_a)),
                    _ => t_b.cmp(t_a),
                }
            });
        } else {
            results.sort_by_key(|b| std::cmp::Reverse(b.score));
        }

        let results: Vec<_> = results.into_iter().take(limit).collect();

        let source_paths_verified = crate::oracle::verify_paths(
            results
                .iter()
                .map(|result| std::path::Path::new(&result.path).to_path_buf()),
        );
        let oracle_status = if let Some(ref retrieval_status) = retrieval_status {
            OracleStatus::hybrid_rrf(
                &store_root,
                retrieval_status,
                results.len(),
                source_paths_verified,
            )
        } else {
            OracleStatus::content_semantic(
                &store_root,
                scanned,
                results.len(),
                source_paths_verified,
            )
        };
        let rendered =
            rank::render_search_json_with_oracle(&store_root, &results, scanned, oracle_status)
                .map_err(|e| {
                    McpError::internal_error(format!("Serialize search JSON: {e}"), None)
                })?;
        let payload = inject_mcp_filter_pushdown_payload(&rendered, pushdown_diagnostic.as_ref())?;

        Ok(CallToolResult::success(vec![Content::text(payload)]))
    }

    #[tool(
        name = "aicx_read",
        description = "Read one canonical AICX chunk by path, file name, or compact reference. Use after aicx_search, aicx_steer, or CLI refs/search output to pull the actual chunk content into context."
    )]
    async fn read_chunk(
        &self,
        Parameters(params): Parameters<ReadParams>,
    ) -> Result<CallToolResult, McpError> {
        tracing::info!(target: "mcp.audit", tool_name = "aicx_read", "mcp tool invoked");
        let chunk = store::read_context_chunk(&params.reference, params.max_chars)
            .map_err(|e| McpError::internal_error(format!("Read chunk: {e}"), None))?;
        let json = serde_json::to_string(&chunk)
            .map_err(|e| McpError::internal_error(format!("Serialize chunk JSON: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(
        name = "aicx_rank",
        description = "Rank stored AI session chunks by content quality. Shows signal density, noise ratio, and quality labels (HIGH/MEDIUM/LOW/NOISE) per chunk. Use --strict to filter noise."
    )]
    async fn rank_artifacts(
        &self,
        Parameters(params): Parameters<RankParams>,
    ) -> Result<CallToolResult, McpError> {
        tracing::info!(target: "mcp.audit", tool_name = "aicx_rank", "mcp tool invoked");
        validate_string_len(Some(params.project.as_str()), 4096, "project")?;
        validate_string_len(params.agent.as_deref(), 4096, "agent")?;
        validate_string_len(params.since.as_deref(), 4096, "since")?;
        validate_string_len(params.until.as_deref(), 4096, "until")?;
        validate_string_len(params.sort.as_deref(), 4096, "sort")?;

        let project = params.project;
        let hours = params.hours;
        let strict = params.strict;
        const MAX_TOP: usize = 1000;
        let top = params.top.map(|t| t.min(MAX_TOP));

        let cutoff = if hours == 0 {
            std::time::UNIX_EPOCH
        } else {
            std::time::SystemTime::now()
                - std::time::Duration::from_secs(hours.saturating_mul(3600).min(365 * 24 * 3600))
        };
        let mut scored = Vec::new();

        let (lo, hi) = if let Some(ref d) = params.since {
            parse_date_filter_mcp(d)
        } else {
            (None, params.until.clone())
        };

        let files = store::context_files_since(cutoff, Some(&project))
            .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;

        for file in files {
            if file.path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            if let Some(ref agent_filter) = params.agent
                && file.agent != *agent_filter
            {
                continue;
            }
            if lo.as_deref().is_some_and(|lo| file.date_iso.as_str() < lo)
                || hi.as_deref().is_some_and(|hi| file.date_iso.as_str() > hi)
            {
                continue;
            }

            let cs = rank::score_chunk_file(&file.path);
            if strict && cs.score < 5 {
                continue;
            }

            let sidecar_path = file.path.with_extension("meta.json");
            let timestamp = if sidecar_path.exists() {
                crate::sanitize::read_to_string_validated(&sidecar_path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                    .and_then(|v| {
                        v.get("started_at")
                            .and_then(|s| s.as_str())
                            .map(String::from)
                            .or_else(|| {
                                v.get("timestamp")
                                    .and_then(|s| s.as_str())
                                    .map(String::from)
                            })
                    })
            } else {
                None
            };
            let final_timestamp = timestamp.or_else(|| {
                file.path
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(chrono::DateTime::<chrono::Utc>::from)
                    .map(|d| d.to_rfc3339())
            });

            scored.push(RankItem {
                file: file
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                project: file.project,
                date: file.date_iso,
                timestamp: final_timestamp,
                kind: file.kind.dir_name().to_string(),
                agent: file.agent,
                score: cs.score,
                label: cs.label.to_string(),
                signal: cs.signal_lines,
                noise: cs.noise_lines,
                total: cs.total_lines,
                density: format!("{:.0}%", cs.density * 100.0),
            });
        }

        if let Some(sort_order) = params.sort.as_deref() {
            scored.sort_by(|a, b| {
                let t_a = a.timestamp.as_deref().unwrap_or(a.date.as_str());
                let t_b = b.timestamp.as_deref().unwrap_or(b.date.as_str());
                match sort_order {
                    "newest" => t_b.cmp(t_a),
                    "oldest" => t_a.cmp(t_b),
                    "score" => b.score.cmp(&a.score).then(t_b.cmp(t_a)),
                    _ => t_b.cmp(t_a),
                }
            });
        } else {
            scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));
        }

        if let Some(n) = top {
            scored.truncate(n);
        }

        let json = serde_json::to_string(&RankResponse {
            project,
            hours,
            strict,
            results: scored.len(),
            items: scored,
        })
        .map_err(|e| McpError::internal_error(format!("Serialize rank JSON: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(
        name = "aicx_steer",
        description = "Retrieve chunks by steering metadata. Supports project/projects (strict canonical <organization>/<repository> matching — `vista` does NOT match `vista-portal`; use `owner/repo`, `owner/`, or `/repo` wildcards for explicit scopes), run_id, prompt_id, agent, kind, frame_kind, and date filters."
    )]
    async fn steer(
        &self,
        Parameters(params): Parameters<SteerParams>,
    ) -> Result<CallToolResult, McpError> {
        tracing::info!(target: "mcp.audit", tool_name = "aicx_steer", "mcp tool invoked");
        validate_string_len(params.run_id.as_deref(), 4096, "run_id")?;
        validate_string_len(params.prompt_id.as_deref(), 4096, "prompt_id")?;
        validate_string_len(params.agent.as_deref(), 4096, "agent")?;
        validate_string_len(params.kind.as_deref(), 4096, "kind")?;
        validate_string_len(params.project.as_deref(), 4096, "project")?;
        validate_string_len(params.date.as_deref(), 4096, "date")?;
        validate_string_len(params.since.as_deref(), 4096, "since")?;
        validate_string_len(params.until.as_deref(), 4096, "until")?;
        if let Some(projects) = &params.projects {
            for (i, p) in projects.iter().enumerate() {
                validate_string_len(Some(p), 4096, &format!("projects[{}]", i))?;
            }
        }

        let limit = params.limit.min(100);

        let date_effective = params.date.or(params.since.clone());
        let (date_lo, date_hi) = if let Some(ref d) = date_effective {
            parse_date_filter_mcp(d)
        } else {
            (None, params.until.clone())
        };

        let owned_projects = params
            .projects
            .clone()
            .filter(|projects| !projects.is_empty())
            .unwrap_or_else(|| params.project.clone().into_iter().collect());
        let store_root = store::store_base_dir()
            .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;
        let project_scopes = search_project_scopes(&store_root, &owned_projects)
            .map_err(|e| McpError::invalid_params(format!("Project filter: {e}"), None))?;
        let mut metadatas = Vec::new();

        for project in project_scopes {
            let filter = crate::steer_index::SteerFilter {
                run_id: params.run_id.as_deref(),
                prompt_id: params.prompt_id.as_deref(),
                agent: params.agent.as_deref(),
                kind: params.kind.as_deref(),
                frame_kind: params.frame_kind,
                project: project.as_deref(),
                date_lo: date_lo.as_deref(),
                date_hi: date_hi.as_deref(),
            };
            let mut batch = crate::steer_index::search_steer_index(&filter, limit)
                .await
                .map_err(|e| McpError::internal_error(format!("Index error: {e}"), None))?;
            metadatas.append(&mut batch);
        }
        dedup_metadata_by_path(&mut metadatas);

        if let Some(min_score) = params.score {
            metadatas.retain(|m| {
                let score = m.get("score").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                score >= min_score
            });
        }

        if let Some(sort_order) = params.sort.as_deref() {
            metadatas.sort_by(|a, b| {
                let t_a = a
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .or_else(|| a.get("date").and_then(|v| v.as_str()))
                    .unwrap_or("");
                let t_b = b
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .or_else(|| b.get("date").and_then(|v| v.as_str()))
                    .unwrap_or("");
                match sort_order {
                    "newest" => t_b.cmp(t_a),
                    "oldest" => t_a.cmp(t_b),
                    "score" => {
                        let s_a = a.get("score").and_then(|v| v.as_u64()).unwrap_or(0);
                        let s_b = b.get("score").and_then(|v| v.as_u64()).unwrap_or(0);
                        s_b.cmp(&s_a).then(t_b.cmp(t_a))
                    }
                    _ => t_b.cmp(t_a),
                }
            });
        }
        metadatas.truncate(limit);

        let store_root = store::store_base_dir()
            .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;
        let oracle_status = OracleStatus::metadata_steer(
            &store_root,
            metadatas.len(),
            metadatas.len(),
            crate::oracle::verify_paths(metadatas.iter().filter_map(|m| {
                m.get("path")
                    .or_else(|| m.get("source_chunk"))
                    .and_then(|value| value.as_str())
                    .map(std::path::PathBuf::from)
            })),
        );

        let json = if params.slim && !params.verbose {
            let items: Vec<_> = metadatas.iter().map(|m| {
                serde_json::json!({
                    "path": m.get("path").or_else(|| m.get("source_chunk")).unwrap_or(&serde_json::Value::Null),
                    "agent": m.get("agent").unwrap_or(&serde_json::Value::Null),
                    "date": m.get("date").unwrap_or(&serde_json::Value::Null),
                    "kind": m.get("kind").unwrap_or(&serde_json::Value::Null),
                    "score": m.get("score").unwrap_or(&serde_json::Value::Null),
                    "snippet_preview": "",
                })
            }).collect();
            serde_json::to_string(&SteerResponse {
                oracle_status: oracle_status.clone(),
                results: items.len(),
                items,
            })
            .map_err(|e| McpError::internal_error(format!("Serialize steer JSON: {e}"), None))?
        } else {
            serde_json::to_string(&SteerResponse {
                oracle_status,
                results: metadatas.len(),
                items: metadatas,
            })
            .map_err(|e| McpError::internal_error(format!("Serialize steer JSON: {e}"), None))?
        };

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(
        name = "aicx_intents",
        description = "Retrieve structured intents, decisions, outcomes, and tasks from the canonical corpus. Supports project/projects and source_chunk back-references."
    )]
    async fn intents(
        &self,
        Parameters(params): Parameters<IntentsParams>,
    ) -> Result<CallToolResult, McpError> {
        tracing::info!(target: "mcp.audit", tool_name = "aicx_intents", "mcp tool invoked");
        let kind_filter = match params.kind.as_deref() {
            None => None,
            Some(s) => match s.to_lowercase().as_str() {
                "decision" => Some(IntentKind::Decision),
                "intent" => Some(IntentKind::Intent),
                "outcome" => Some(IntentKind::Outcome),
                "task" => Some(IntentKind::Task),
                other => {
                    return Err(McpError::invalid_params(
                        format!(
                            "Unknown intent kind: '{other}'. Expected one of: decision, intent, outcome, task"
                        ),
                        None,
                    ));
                }
            },
        };

        let sort = match params.sort.as_deref() {
            None => None,
            Some(s) => match s.to_lowercase().as_str() {
                "newest" => Some(intents::IntentSortOrder::Newest),
                "oldest" => Some(intents::IntentSortOrder::Oldest),
                other => {
                    return Err(McpError::invalid_params(
                        format!("Unknown sort order: '{other}'. Expected: newest, oldest"),
                        None,
                    ));
                }
            },
        };

        let owned_projects = params
            .projects
            .clone()
            .filter(|projects| !projects.is_empty())
            .unwrap_or_else(|| params.project.clone().into_iter().collect());

        let config = IntentsConfig {
            project: owned_projects.first().cloned().unwrap_or_default(),
            hours: params.hours,
            strict: params.strict,
            kind_filter,
            frame_kind: params.frame_kind,
        };

        let extraction = intents::extract_intents_with_stats_for_projects(&config, &owned_projects)
            .map_err(|e| McpError::internal_error(format!("Extract intents: {e}"), None))?;
        let records = extraction.records;

        let limit_capped = params.limit.min(500);

        let display_filters = intents::IntentDisplayFilters {
            unresolved: params.unresolved,
            collapse_session: params.collapse_session,
            agent: params.agent,
            date_lo: params.since,
            date_hi: params.until,
            sort,
            limit: Some(limit_capped),
        };

        let records = intents::apply_display_filters(records, &display_filters);

        let body = match params.emit.as_str() {
            "markdown" | "md" => intents::format_intents_markdown(&records),
            _ => {
                let store_root = store::store_base_dir()
                    .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;
                let oracle_status = OracleStatus::canonical_corpus_scan(
                    &store_root,
                    extraction.stats.scanned_count,
                    extraction.stats.candidate_count,
                    extraction.stats.source_paths_verified,
                );
                intents::format_intents_oracle_json(&records, oracle_status).map_err(|e| {
                    McpError::internal_error(format!("Serialize intents JSON: {e}"), None)
                })?
            }
        };

        Ok(CallToolResult::success(vec![Content::text(body)]))
    }

    #[tool(
        name = "aicx_index_status",
        description = "Report the truthful state of the AICX semantic vector index for a project bucket. Returns `readiness` (ready/pending/missing), backend, project_bucket, committed_at, pending_chunks, and the final + temp checkpoint paths. `ready` is set only when the atomically committed `embeddings.ndjson` is present; a lone `.ndjson.tmp` checkpoint surfaces as `pending` so Loctree and other oracles can refuse to trust semantic retrieval before commit."
    )]
    async fn index_status(
        &self,
        Parameters(params): Parameters<IndexStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        tracing::info!(target: "mcp.audit", tool_name = "aicx_index_status", "mcp tool invoked");
        let store_root = store::store_base_dir()
            .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;
        let status = api::index_status_at(&store_root, params.project.as_deref())
            .map_err(|e| McpError::internal_error(format!("index status: {e}"), None))?;
        let json = serde_json::to_string(&status).map_err(|e| {
            McpError::internal_error(format!("Serialize index status JSON: {e}"), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

// ============================================================================
// ServerHandler impl
// ============================================================================

#[rmcp::tool_handler]
impl rmcp::handler::server::ServerHandler for AicxMcpServer {
    fn get_info(&self) -> ServerInfo {
        // `tool_router` is read by the `#[tool_handler]`-expanded `call_tool`,
        // `list_tools`, and `get_tool` methods; rust 1.95 dead_code analysis
        // doesn't traverse macro expansions, so anchor the read here.
        let _ = &self.tool_router;
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new("aicx-mcp", env!("CARGO_PKG_VERSION")))
    }
}

// ============================================================================
// Server runners
// ============================================================================

/// Names of all tool handlers wired into `AicxMcpServer`. Used at startup
/// to log the audit surface (F-P3-18) and in tests to confirm the list
/// stays in sync with the actual `#[tool]` annotations on the impl block.
pub const MCP_TOOL_SURFACE: &[&str] = &[
    "aicx_search",
    "aicx_read",
    "aicx_rank",
    "aicx_steer",
    "aicx_intents",
    "aicx_index_status",
];

/// Run MCP server over stdio transport.
pub async fn run_stdio() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

    // F-P3-18: stdio bypasses HTTP auth (no network surface) — log it so
    // the audit trail captures the security posture explicitly.
    tracing::info!(
        target: "mcp.audit",
        auth = "stdio_no_network",
        tools = ?MCP_TOOL_SURFACE,
        "mcp server starting (stdio)"
    );

    let server = AicxMcpServer::new();
    let service = rmcp::ServiceExt::serve(server, rmcp::transport::io::stdio())
        .await
        .map_err(|e| anyhow::anyhow!("MCP stdio serve failed: {e}"))?;

    eprintln!("aicx MCP server running (stdio)");
    service
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP server error: {e}"))?;
    Ok(())
}

/// Run MCP server over streamable HTTP transport on given port with the given auth state.
pub async fn run_http(port: u16, auth_config: AuthConfig) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

    let addr = std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);

    let auth_source_label = auth_config.source.describe();
    let auth_enforced = auth_config.is_enforced();

    // F-P3-18: emit the truthful auth posture + tool surface as a single
    // tracing event so security review can audit boot configuration
    // without parsing free-form eprintln! lines.
    tracing::info!(
        target: "mcp.audit",
        auth_enabled = auth_enforced,
        auth_source = %auth_source_label,
        tools = ?MCP_TOOL_SURFACE,
        port = port,
        "mcp server starting (http)"
    );

    let config = rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default();
    let session_manager = configured_mcp_session_manager();
    spawn_mcp_session_cleanup(session_manager.clone(), MCP_SESSION_SWEEP_INTERVAL);
    let service = rmcp::transport::streamable_http_server::StreamableHttpService::new(
        || Ok(AicxMcpServer::new()),
        session_manager,
        config,
    );

    let mcp_router = axum::Router::new().route(
        "/mcp",
        axum::routing::any(move |req: axum::http::Request<axum::body::Body>| {
            let svc = service.clone();
            async move { svc.handle(req).await }
        }),
    );

    let app = auth::require_auth_layer(mcp_router, auth_config);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to bind MCP server on {addr}: {e}"))?;

    eprintln!("aicx MCP server running (streamable HTTP)");
    eprintln!("  Endpoint: http://{addr}/mcp");
    eprintln!("  Transport: Streamable HTTP (POST + GET /mcp)");
    if auth_enforced {
        eprintln!("  Auth: enabled (source: {auth_source_label})");
    } else {
        eprintln!(
            "  Auth: DISABLED ({auth_source_label}) — anyone who reaches {addr} can invoke MCP tools"
        );
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("MCP HTTP server error: {e}"))
}

/// Legacy compatibility wrapper for callers that still use the old `run_sse` name.
pub async fn run_sse(port: u16, auth_config: AuthConfig) -> anyhow::Result<()> {
    run_http(port, auth_config).await
}

/// Run the selected MCP transport. Stdio bypasses HTTP auth (no network surface).
pub async fn run_transport(
    transport: McpTransport,
    port: u16,
    auth_config: AuthConfig,
) -> anyhow::Result<()> {
    match transport {
        McpTransport::Stdio => run_stdio().await,
        McpTransport::Http => run_http(port, auth_config).await,
    }
}

/// Parse a date filter string into (optional_low, optional_high) bounds.
///
/// Accepted formats:
/// - `2026-03-28` → exact day
/// - `2026-03-20..2026-03-28` → inclusive range
/// - `2026-03-20..` → open-ended (from date onward)
/// - `..2026-03-28` → open-ended (up to date)
fn parse_date_filter_mcp(date: &str) -> (Option<String>, Option<String>) {
    if let Some((lo, hi)) = date.split_once("..") {
        let lo = if lo.is_empty() {
            None
        } else {
            Some(lo.to_string())
        };
        let hi = if hi.is_empty() {
            None
        } else {
            Some(hi.to_string())
        };
        (lo, hi)
    } else {
        (Some(date.to_string()), Some(date.to_string()))
    }
}

// `inject_filter_pushdown_diagnostic` extracted to `aicx::search_engine`
// — shared with the CLI path in `src/main.rs` (gemini-code-assist
// review on PR #9: DRY).

fn validate_score_filter(score: Option<u8>) -> Result<Option<u8>, McpError> {
    match score {
        Some(score) if score > MAX_SCORE_FILTER => Err(McpError::invalid_params(
            format!("score must be between 0 and {MAX_SCORE_FILTER}"),
            None,
        )),
        _ => Ok(score),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AicxMcpServer, AicxSessionManager, AicxSessionManagerError, MAX_SCORE_FILTER,
        MCP_EMBEDDER_NEGATIVE_TTL, MCP_SESSION_IDLE_TTL, MCP_SESSION_MAX_SESSIONS,
        MCP_SESSION_SWEEP_INTERVAL, McpTransport, RankItem, RankResponse, SearchParams,
        SteerResponse, background_refresh_args, build_mcp_semantic_filters,
        configured_mcp_session_manager, inject_mcp_filter_pushdown_payload, parse_date_filter_mcp,
        spawn_mcp_session_cleanup, validate_score_filter, validate_string_len,
    };
    use crate::oracle::OracleStatus;
    use clap::ValueEnum as _;
    use rmcp::transport::streamable_http_server::session::SessionManager as _;
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    fn minimal_search_params() -> SearchParams {
        SearchParams {
            query: "dashboard".to_string(),
            limit: 20,
            project: None,
            projects: None,
            score: None,
            hours: None,
            agent: None,
            date: None,
            since: None,
            until: None,
            sort: None,
            frame_kind: None,
            kind: None,
            slim: true,
            verbose: false,
        }
    }

    fn fixed_utc_now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-05-24T12:00:00Z")
            .expect("fixed timestamp")
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn background_refresh_args_use_all_and_quiet_stdout() {
        assert_eq!(
            background_refresh_args(24, None),
            vec![
                "all".to_string(),
                "-H".to_string(),
                "24".to_string(),
                "--emit".to_string(),
                "none".to_string(),
            ]
        );
    }

    #[test]
    fn parse_date_filter_mcp_exact_day() {
        let (lo, hi) = parse_date_filter_mcp("2026-03-28");
        assert_eq!(lo.as_deref(), Some("2026-03-28"));
        assert_eq!(hi.as_deref(), Some("2026-03-28"));
    }

    #[test]
    fn parse_date_filter_mcp_range() {
        let (lo, hi) = parse_date_filter_mcp("2026-03-20..2026-03-28");
        assert_eq!(lo.as_deref(), Some("2026-03-20"));
        assert_eq!(hi.as_deref(), Some("2026-03-28"));
    }

    #[test]
    fn parse_date_filter_mcp_open_ended() {
        let (lo, hi) = parse_date_filter_mcp("2026-03-20..");
        assert_eq!(lo.as_deref(), Some("2026-03-20"));
        assert!(hi.is_none());

        let (lo, hi) = parse_date_filter_mcp("..2026-03-28");
        assert!(lo.is_none());
        assert_eq!(hi.as_deref(), Some("2026-03-28"));
    }

    #[test]
    fn background_refresh_args_include_project_filter() {
        assert_eq!(
            background_refresh_args(72, Some("ai-contexters")),
            vec![
                "all".to_string(),
                "-H".to_string(),
                "72".to_string(),
                "--emit".to_string(),
                "none".to_string(),
                "-p".to_string(),
                "ai-contexters".to_string(),
            ]
        );
    }

    #[test]
    fn rank_response_serializes_as_compact_json() {
        let json = serde_json::to_string(&RankResponse {
            project: "VetCoders/ai-contexters".to_string(),
            hours: 72,
            strict: true,
            results: 1,
            items: vec![RankItem {
                file: "chunk.md".to_string(),
                project: "VetCoders/ai-contexters".to_string(),
                date: "2026-03-31".to_string(),
                timestamp: Some("2026-03-31T10:00:00Z".to_string()),
                kind: "reports".to_string(),
                agent: "codex".to_string(),
                score: 8,
                label: "HIGH".to_string(),
                signal: 14,
                noise: 2,
                total: 20,
                density: "70%".to_string(),
            }],
        })
        .expect("rank response should serialize");

        assert!(!json.contains('\n'));

        let payload: serde_json::Value =
            serde_json::from_str(&json).expect("rank JSON should parse");
        assert_eq!(payload["results"], 1);
        assert_eq!(payload["items"][0]["score"], 8);
        assert_eq!(payload["items"][0]["label"], "HIGH");
    }

    #[test]
    fn steer_response_serializes_as_compact_json() {
        let json = serde_json::to_string(&SteerResponse {
            oracle_status: OracleStatus::metadata_steer(std::path::Path::new("/tmp"), 1, 1, true),
            results: 1,
            items: vec![serde_json::json!({
                "path": "/tmp/chunk.md",
                "project": "VetCoders/ai-contexters",
                "agent": "codex",
                "kind": "reports",
            })],
        })
        .expect("steer response should serialize");

        assert!(!json.contains('\n'));

        let payload: serde_json::Value =
            serde_json::from_str(&json).expect("steer JSON should parse");
        assert_eq!(payload["results"], 1);
        assert_eq!(payload["oracle_status"]["backend"], "steer_metadata");
        assert_eq!(payload["oracle_status"]["index_kind"], "metadata_steer");
        assert_eq!(payload["oracle_status"]["loctree_scope_safe"], true);
        assert_eq!(payload["items"][0]["path"], "/tmp/chunk.md");
        assert_eq!(payload["items"][0]["agent"], "codex");
    }

    #[test]
    fn steer_response_with_nan_score_serializes_without_panic() {
        let json = serde_json::to_string(&SteerResponse {
            oracle_status: OracleStatus::metadata_steer(std::path::Path::new("/tmp"), 1, 1, true),
            results: 1,
            items: vec![serde_json::json!({
                "path": "/tmp/chunk.md",
                "score": f32::NAN,
            })],
        })
        .expect("serde_json normalizes non-finite Value numbers instead of panicking");

        let payload: serde_json::Value =
            serde_json::from_str(&json).expect("steer JSON should parse");
        assert!(payload["items"][0]["score"].is_null());
    }

    #[test]
    fn search_params_roundtrip_include_new_optional_filters() {
        let params: SearchParams =
            serde_json::from_str(r#"{"query":"dashboard"}"#).expect("search params should parse");
        assert_eq!(params.limit, 20);
        assert!(params.project.is_none());
        assert!(params.score.is_none());
        assert!(params.hours.is_none());
        assert!(params.date.is_none());
    }

    #[test]
    fn mcp_search_filter_build_uses_hours_when_no_date_bounds_exist() {
        let mut params = minimal_search_params();
        params.hours = Some(48);

        let built = build_mcp_semantic_filters(&params, None, fixed_utc_now());

        assert_eq!(
            built.post_filters.hours_cutoff.as_deref(),
            Some("2026-05-22")
        );
        assert!(built.post_filters.date_lo.is_none());
        assert!(built.post_filters.date_hi.is_none());
    }

    #[test]
    fn mcp_search_filter_build_date_range_wins_over_hours() {
        let mut params = minimal_search_params();
        params.hours = Some(168);
        params.date = Some("2026-03-20..2026-03-28".to_string());
        params.agent = Some("codex".to_string());

        let built = build_mcp_semantic_filters(&params, Some(80), fixed_utc_now());

        assert_eq!(built.post_filters.date_lo.as_deref(), Some("2026-03-20"));
        assert_eq!(built.post_filters.date_hi.as_deref(), Some("2026-03-28"));
        assert!(built.post_filters.hours_cutoff.is_none());
        assert_eq!(built.post_filters.agent.as_deref(), Some("codex"));
        assert_eq!(built.post_filters.score_min, Some(80));
    }

    #[test]
    fn mcp_search_filter_build_until_bound_wins_over_hours() {
        let mut params = minimal_search_params();
        params.hours = Some(168);
        params.until = Some("2026-05-20".to_string());

        let built = build_mcp_semantic_filters(&params, None, fixed_utc_now());

        assert!(built.post_filters.date_lo.is_none());
        assert_eq!(built.post_filters.date_hi.as_deref(), Some("2026-05-20"));
        assert!(built.post_filters.hours_cutoff.is_none());
    }

    #[test]
    fn mcp_search_payload_includes_filter_pushdown_diagnostic() {
        let diagnostic = crate::search_engine::FilterPushdownDiagnostic {
            kind: "filter_yielded_partial",
            examined: 50,
            matched: 2,
            requested_limit: 10,
            examined_cap_ratio: 10,
        };

        let payload = inject_mcp_filter_pushdown_payload(
            r#"{"oracle_status":{"backend":"hybrid_rrf"},"results":2,"items":[]}"#,
            Some(&diagnostic),
        )
        .expect("diagnostic payload should inject");
        let json: serde_json::Value =
            serde_json::from_str(&payload).expect("payload should stay valid JSON");

        assert_eq!(json["results"], 2);
        assert_eq!(json["filter_pushdown"]["kind"], "filter_yielded_partial");
        assert_eq!(json["filter_pushdown"]["examined"], 50);
        assert_eq!(json["filter_pushdown"]["matched"], 2);
        assert_eq!(json["filter_pushdown"]["requested_limit"], 10);
        assert_eq!(json["filter_pushdown"]["examined_cap_ratio"], 10);
    }

    #[test]
    fn mcp_search_payload_invalid_json_maps_to_internal_error() {
        let diagnostic = crate::search_engine::FilterPushdownDiagnostic {
            kind: "filter_yielded_partial",
            examined: 50,
            matched: 2,
            requested_limit: 10,
            examined_cap_ratio: 10,
        };

        let err = inject_mcp_filter_pushdown_payload("{not json", Some(&diagnostic))
            .expect_err("invalid rendered JSON should map to MCP internal error");

        assert_eq!(err.code, rmcp::model::ErrorCode::INTERNAL_ERROR);
        assert!(
            err.message.contains("Inject filter_pushdown JSON"),
            "unexpected error message: {}",
            err.message
        );
        assert!(err.data.is_none());
    }

    #[test]
    fn string_len_validation_rejects_oversized_fields() {
        let err = validate_string_len(Some("abcd"), 3, "query")
            .expect_err("oversized field should be rejected");

        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(
            err.message
                .contains("Field 'query' exceeds max length 3 bytes"),
            "unexpected error message: {}",
            err.message
        );
        assert!(err.data.is_none());
    }

    #[test]
    fn score_filter_rejects_values_above_max() {
        let err = validate_score_filter(Some(MAX_SCORE_FILTER + 1))
            .expect_err("score above 100 should be rejected");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn mcp_tool_surface_lists_all_handlers() {
        // F-P3-18: any new #[tool] handler MUST add itself to MCP_TOOL_SURFACE
        // so the startup audit log includes it. This test fails when a new
        // tool is added without updating the surface list.
        let expected = [
            "aicx_search",
            "aicx_read",
            "aicx_rank",
            "aicx_steer",
            "aicx_intents",
            "aicx_index_status",
        ];
        assert_eq!(
            super::MCP_TOOL_SURFACE.len(),
            expected.len(),
            "MCP_TOOL_SURFACE drifted from expected handler set: {:?} vs {expected:?}",
            super::MCP_TOOL_SURFACE
        );
        for name in expected {
            assert!(
                super::MCP_TOOL_SURFACE.contains(&name),
                "MCP_TOOL_SURFACE missing handler `{name}`"
            );
        }
    }

    #[test]
    fn embedder_negative_ttl_is_five_minutes() {
        // D-6: production TTL is five minutes; a regression to the test-only
        // 30s value would let a flapping endpoint retry-storm an unhealthy
        // embedder six times faster than intended.
        assert_eq!(MCP_EMBEDDER_NEGATIVE_TTL, Duration::from_secs(5 * 60));
    }

    #[test]
    fn mark_embedder_unavailable_arms_cache() {
        // D-6: mark + check cycle works end-to-end without external state.
        let server = AicxMcpServer::new();
        let now = Instant::now();
        assert!(server.embedder_negative_cache_remaining(now).is_none());
        server.mark_embedder_unavailable(now, Duration::from_secs(120));
        let remaining = server
            .embedder_negative_cache_remaining(now)
            .expect("cache should be armed after mark");
        assert!(remaining.as_secs() >= 119 && remaining.as_secs() <= 120);
        // After TTL elapsed, cache lazily clears.
        assert!(
            server
                .embedder_negative_cache_remaining(now + Duration::from_secs(121))
                .is_none()
        );
    }

    #[test]
    fn embedder_negative_cache_expires_by_ttl() {
        let server = AicxMcpServer::new();
        let now = Instant::now();
        {
            let mut guard = server.embedder_unavailable_guard();
            *guard = Some(now + MCP_EMBEDDER_NEGATIVE_TTL);
        }

        let remaining = server
            .embedder_negative_cache_remaining(now + Duration::from_secs(10))
            .expect("cache should still be active");
        assert!(remaining.as_secs() >= MCP_EMBEDDER_NEGATIVE_TTL.as_secs() - 11);

        assert!(
            server
                .embedder_negative_cache_remaining(
                    now + MCP_EMBEDDER_NEGATIVE_TTL + Duration::from_secs(1)
                )
                .is_none()
        );
    }

    #[test]
    fn test_mcp_session_manager_configures_idle_ttl_and_cap() {
        let manager = configured_mcp_session_manager();

        assert_eq!(
            manager.inner.session_config.keep_alive,
            Some(MCP_SESSION_IDLE_TTL)
        );
        assert_eq!(manager.max_sessions, MCP_SESSION_MAX_SESSIONS);
        assert_eq!(MCP_SESSION_SWEEP_INTERVAL, Duration::from_secs(60));
    }

    #[test]
    fn test_mcp_session_count_capped() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let manager = AicxSessionManager::new(Duration::from_secs(30), 1);
            let (_id, _transport) = manager.create_session().await.expect("first session");
            let err = match manager.create_session().await {
                Ok(_) => panic!("second session must exceed cap"),
                Err(err) => err,
            };

            assert!(matches!(
                err,
                AicxSessionManagerError::SessionLimitExceeded(1)
            ));
        });
    }

    #[test]
    fn test_mcp_session_idle_ttl_cleans_up() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let manager = AicxSessionManager::new(Duration::from_millis(10), 1000);
            let (id, _transport) = manager.create_session().await.expect("create session");
            assert_eq!(manager.session_count().await, 1);

            let closed = manager
                .sweep_idle_sessions_at(Instant::now() + Duration::from_millis(11))
                .await
                .expect("sweep idle sessions");

            assert_eq!(closed, 1);
            assert_eq!(manager.session_count().await, 0);
            assert!(!manager.has_session(&id).await.expect("has_session"));
        });
    }

    #[test]
    fn test_mcp_session_cleanup_task_can_be_spawned() {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let manager = Arc::new(AicxSessionManager::new(Duration::from_millis(1), 1000));
            spawn_mcp_session_cleanup(manager, Duration::from_secs(60));
        });
    }

    #[test]
    fn mcp_transport_prefers_http_but_accepts_legacy_sse_alias() {
        let possible = McpTransport::value_variants()
            .iter()
            .map(|variant| {
                variant
                    .to_possible_value()
                    .expect("possible value")
                    .get_name()
                    .to_string()
            })
            .collect::<Vec<_>>();

        assert_eq!(possible, vec!["stdio".to_string(), "http".to_string()]);
        assert_eq!(McpTransport::from_str("http", true), Ok(McpTransport::Http));
        assert_eq!(McpTransport::from_str("sse", true), Ok(McpTransport::Http));
    }
}
