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

use crate::intents::{self, IntentKind, IntentsConfig};
use crate::oracle::OracleStatus;
use crate::rank;
use crate::store;
use crate::timeline::FrameKind;

// ============================================================================
// Tool parameter & result types
// ============================================================================

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

fn search_project_scopes(projects: &[String]) -> Vec<Option<&str>> {
    if projects.is_empty() {
        vec![None]
    } else {
        projects.iter().map(String::as_str).map(Some).collect()
    }
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
    /// Hours to look back (default: 72)
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
    /// Filter by project (case-insensitive substring)
    pub project: Option<String>,
    /// Optional project filters for cross-project steering.
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

#[derive(Clone)]
pub struct AicxMcpServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
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
        }
    }

    #[tool(
        name = "aicx_search",
        description = "Semantic search over the canonical corpus. Fails fast with kind/reason/recommendation when the index or embedder is not ready."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.query;
        let limit = params.limit.min(50);
        let project = params.project;
        let owned_projects = params
            .projects
            .clone()
            .filter(|projects| !projects.is_empty())
            .unwrap_or_else(|| project.clone().into_iter().collect());
        let project_scopes = search_project_scopes(&owned_projects);
        let score = validate_score_filter(params.score)?;
        let hours = params.hours.unwrap_or(0);
        let date = params.date;
        let frame_kind = params.frame_kind;
        let fetch_limit = if score.is_some() || date.is_some() || hours > 0 {
            limit.saturating_mul(5).max(50)
        } else {
            limit
        };

        let store_root = store::store_base_dir()
            .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;

        // Semantic-only dispatch. No fuzzy fallback. When a precondition
        // is missing (embedder unhydrated, index not built, ...) return
        // a structured McpError carrying the same `kind` + `reason` +
        // `recommendation` triple the CLI fail-fast surface emits, so an
        // MCP caller has the same diagnostic to act on.
        let outcome = match crate::search_engine::try_semantic_search(
            &store_root,
            &query,
            fetch_limit,
            &project_scopes,
            frame_kind,
        ) {
            Ok(outcome) => outcome,
            Err(err) => {
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
        let scanned = outcome.scanned;
        let results = outcome.results;

        let mut results = results;

        if let Some(min_score) = score {
            results.retain(|result| result.score >= min_score);
        }
        if let Some(ref agent_filter) = params.agent {
            results.retain(|r| r.agent == *agent_filter);
        }

        let date_effective = date.or(params.since.clone());
        let (lo, hi) = if let Some(ref date_filter) = date_effective {
            parse_date_filter_mcp(date_filter)
        } else {
            (None, params.until.clone())
        };

        let mut results: Vec<_> = if lo.is_some() || hi.is_some() {
            results
                .into_iter()
                .filter(|result| {
                    lo.as_ref()
                        .is_none_or(|lo| result.date.as_str() >= lo.as_str())
                        && hi
                            .as_ref()
                            .is_none_or(|hi| result.date.as_str() <= hi.as_str())
                })
                .collect()
        } else if hours > 0 {
            let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
            let cutoff_date = cutoff.format("%Y-%m-%d").to_string();
            results
                .into_iter()
                .filter(|result| result.date >= cutoff_date)
                .collect()
        } else {
            results
        };

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

        let json = rank::render_search_json(&store_root, &results, scanned)
            .map_err(|e| McpError::internal_error(format!("Serialize search JSON: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(
        name = "aicx_read",
        description = "Read one canonical AICX chunk by path, file name, or compact reference. Use after aicx_search, aicx_steer, or CLI refs/search output to pull the actual chunk content into context."
    )]
    async fn read_chunk(
        &self,
        Parameters(params): Parameters<ReadParams>,
    ) -> Result<CallToolResult, McpError> {
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
        let project = params.project;
        let hours = params.hours;
        let strict = params.strict;
        let top = params.top;

        let cutoff = std::time::SystemTime::now()
            - std::time::Duration::from_secs(hours.saturating_mul(3600).min(365 * 24 * 3600));
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
            if lo
                .as_ref()
                .is_some_and(|lo| file.date_iso.as_str() < lo.as_str())
                || hi
                    .as_ref()
                    .is_some_and(|hi| file.date_iso.as_str() > hi.as_str())
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
        description = "Retrieve chunks by steering metadata. Supports project/projects, run_id, prompt_id, agent, kind, frame_kind, and date filters."
    )]
    async fn steer(
        &self,
        Parameters(params): Parameters<SteerParams>,
    ) -> Result<CallToolResult, McpError> {
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
        let project_scopes = search_project_scopes(&owned_projects);
        let mut metadatas = Vec::new();

        for project in project_scopes {
            let filter = crate::steer_index::SteerFilter {
                run_id: params.run_id.as_deref(),
                prompt_id: params.prompt_id.as_deref(),
                agent: params.agent.as_deref(),
                kind: params.kind.as_deref(),
                frame_kind: params.frame_kind,
                project,
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
            .unwrap()
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

/// Run MCP server over stdio transport.
pub async fn run_stdio() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

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

/// Run MCP server over streamable HTTP transport on given port.
pub async fn run_http(port: u16) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init()
        .ok();

    let addr = std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), port);

    let config = rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default();
    let service = rmcp::transport::streamable_http_server::StreamableHttpService::new(
        || Ok(AicxMcpServer::new()),
        std::sync::Arc::new(
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager::default(),
        ),
        config,
    );

    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::any(move |req: axum::http::Request<axum::body::Body>| {
            let svc = service.clone();
            async move { svc.handle(req).await }
        }),
    );

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to bind MCP server on {addr}: {e}"))?;

    eprintln!("aicx MCP server running (streamable HTTP)");
    eprintln!("  Endpoint: http://{addr}/mcp");
    eprintln!("  Transport: Streamable HTTP (POST + GET /mcp)");

    axum::serve(listener, app)
        .await
        .map_err(|e| anyhow::anyhow!("MCP HTTP server error: {e}"))
}

/// Legacy compatibility wrapper for callers that still use the old `run_sse` name.
pub async fn run_sse(port: u16) -> anyhow::Result<()> {
    run_http(port).await
}

/// Run the selected MCP transport.
pub async fn run_transport(transport: McpTransport, port: u16) -> anyhow::Result<()> {
    match transport {
        McpTransport::Stdio => run_stdio().await,
        McpTransport::Http => run_http(port).await,
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
        MAX_SCORE_FILTER, McpTransport, RankItem, RankResponse, SearchParams, SteerResponse,
        background_refresh_args, parse_date_filter_mcp, validate_score_filter,
    };
    use crate::oracle::OracleStatus;
    use clap::ValueEnum as _;

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
    fn score_filter_rejects_values_above_max() {
        let err = validate_score_filter(Some(MAX_SCORE_FILTER + 1))
            .expect_err("score above 100 should be rejected");
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
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
