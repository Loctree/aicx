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
use std::sync::atomic::{AtomicBool, Ordering};

/// Guard that prevents concurrent background refresh child-process spawns.
static RESCAN_RUNNING: AtomicBool = AtomicBool::new(false);

use crate::rank;
use crate::store;

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
    /// Optional project filter (case-insensitive substring)
    pub project: Option<String>,
    /// Minimum score threshold (0-100)
    pub score: Option<u8>,
    /// Hours to look back (0 = all time)
    pub hours: Option<u64>,
    /// Optional date filter (single day or range)
    pub date: Option<String>,
}

fn default_limit() -> usize {
    10
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
    /// Show only top N bundles
    pub top: Option<usize>,
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
    /// Filter by project (case-insensitive substring)
    pub project: Option<String>,
    /// Filter by date (YYYY-MM-DD, or range like 2026-03-20..2026-03-28)
    pub date: Option<String>,
    /// Max results (default: 20)
    #[serde(default = "default_steer_limit")]
    pub limit: usize,
}

fn default_steer_limit() -> usize {
    20
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
    results: usize,
    items: Vec<serde_json::Value>,
}

fn incremental_rescan_args(hours: u64, project: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "all".to_string(),
        "-H".to_string(),
        hours.to_string(),
        "--incremental".to_string(),
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
        description = "Search stored AI session chunks. Uses memex semantic retrieval when available and otherwise falls back to canonical-store fuzzy search. Returns quality-scored results with matched lines."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.query;
        let limit = params.limit.min(50);
        let project = params.project;
        let score = validate_score_filter(params.score)?;
        let hours = params.hours.unwrap_or(0);
        let date = params.date;
        let fetch_limit = if score.is_some() || date.is_some() || hours > 0 {
            limit.saturating_mul(5).max(50)
        } else {
            limit
        };

        // Non-blocking auto-rescan with rate-limit guard.
        if !RESCAN_RUNNING.swap(true, Ordering::SeqCst) {
            let args = incremental_rescan_args(24, project.as_deref());
            match std::process::Command::new("aicx")
                .args(&args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(_) => {
                    std::thread::spawn(|| {
                        std::thread::sleep(std::time::Duration::from_secs(30));
                        RESCAN_RUNNING.store(false, Ordering::SeqCst);
                    });
                }
                Err(e) => {
                    tracing::warn!("Failed to spawn aicx background refresh: {e}");
                    RESCAN_RUNNING.store(false, Ordering::SeqCst);
                }
            }
        }

        // Try fast search with rmcp_memex first (instant), fallback to brute-force if it fails
        let (results, scanned) =
            match crate::memex::fast_memex_search(&query, fetch_limit, project.as_deref()).await {
                Ok((res, scan)) if !res.is_empty() => (res, scan),
                Err(err) if crate::memex::is_compatibility_error(&err) => {
                    return Err(McpError::internal_error(
                        format!("Search index incompatible: {err}"),
                        None,
                    ));
                }
                _ => {
                    // Fallback to reading all markdown files sequentially (slow)
                    let store_root = store::store_base_dir()
                        .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;
                    rank::fuzzy_search_store(&store_root, &query, fetch_limit, project.as_deref())
                        .map_err(|e| McpError::internal_error(format!("Read store: {e}"), None))?
                }
            };

        let mut results = results;

        if let Some(min_score) = score {
            results.retain(|result| result.score >= min_score);
        }

        let results: Vec<_> = if let Some(ref date_filter) = date {
            let (lo, hi) = parse_date_filter_mcp(date_filter);
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
        let results: Vec<_> = results.into_iter().take(limit).collect();

        let json = rank::render_search_json(&results, scanned)
            .map_err(|e| McpError::internal_error(format!("Serialize search JSON: {e}"), None))?;

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

        let files = store::context_files_since(cutoff, Some(&project))
            .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;

        for file in files {
            if file.path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }
            let cs = rank::score_chunk_file(&file.path);
            if strict && cs.score < 5 {
                continue;
            }
            scored.push(RankItem {
                file: file
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
                project: file.project,
                date: file.date_iso,
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

        scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));

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
        description = "Retrieve stored chunks by steering metadata (frontmatter fields). Filters by run_id, prompt_id, agent, kind, project, and/or date range using sidecar metadata — no filesystem grep needed. Returns chunk paths with their sidecar metadata for selective re-entry."
    )]
    async fn steer(
        &self,
        Parameters(params): Parameters<SteerParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params.limit.min(100);

        let (date_lo, date_hi) = if let Some(ref d) = params.date {
            parse_date_filter_mcp(d)
        } else {
            (None, None)
        };

        let metadatas = crate::steer_index::search_steer_index(
            params.run_id.as_deref(),
            params.prompt_id.as_deref(),
            params.agent.as_deref(),
            params.kind.as_deref(),
            params.project.as_deref(),
            date_lo.as_deref(),
            date_hi.as_deref(),
            limit,
        )
        .await
        .map_err(|e| McpError::internal_error(format!("Index error: {e}"), None))?;

        let json = serde_json::to_string(&SteerResponse {
            results: metadatas.len(),
            items: metadatas,
        })
        .map_err(|e| McpError::internal_error(format!("Serialize steer JSON: {e}"), None))?;

        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

// ============================================================================
// ServerHandler impl
// ============================================================================

#[rmcp::tool_handler]
impl rmcp::handler::server::ServerHandler for AicxMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
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
        incremental_rescan_args, parse_date_filter_mcp, validate_score_filter,
    };
    use clap::ValueEnum as _;

    #[test]
    fn incremental_rescan_args_use_all_incremental_and_quiet_stdout() {
        assert_eq!(
            incremental_rescan_args(24, None),
            vec![
                "all".to_string(),
                "-H".to_string(),
                "24".to_string(),
                "--incremental".to_string(),
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
    fn incremental_rescan_args_include_project_filter() {
        assert_eq!(
            incremental_rescan_args(72, Some("ai-contexters")),
            vec![
                "all".to_string(),
                "-H".to_string(),
                "72".to_string(),
                "--incremental".to_string(),
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
        assert_eq!(payload["items"][0]["path"], "/tmp/chunk.md");
        assert_eq!(payload["items"][0]["agent"], "codex");
    }

    #[test]
    fn search_params_roundtrip_include_new_optional_filters() {
        let params: SearchParams =
            serde_json::from_str(r#"{"query":"dashboard"}"#).expect("search params should parse");
        assert_eq!(params.limit, 10);
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
