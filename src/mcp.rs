//! MCP (Model Context Protocol) server for aicx.
//!
//! Exposes aicx functionality as MCP tools so any AI agent can query
//! session history, search chunks, rank artifacts, and extract intents.
//!
//! Supports stdio and SSE transports.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use rmcp::schemars::{self, JsonSchema};
use rmcp::{
    ErrorData as McpError, handler::server::tool::ToolRouter, handler::server::wrapper::Parameters,
    model::*, tool, tool_router,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Guard that prevents concurrent background refresh child-process spawns.
static RESCAN_RUNNING: AtomicBool = AtomicBool::new(false);

use crate::rank;
use crate::store;

// ============================================================================
// Tool parameter & result types
// ============================================================================

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Search query text
    pub query: String,
    /// Max results to return (default: 10)
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Optional project filter (case-insensitive substring)
    pub project: Option<String>,
}

fn default_limit() -> usize {
    10
}

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
pub struct RefsParams {
    /// Hours to look back (default: 168)
    #[serde(default = "default_refs_hours")]
    pub hours: u64,
    /// Optional project filter
    pub project: Option<String>,
    /// Exclude noise artifacts
    #[serde(default)]
    pub strict: bool,
}

fn default_refs_hours() -> u64 {
    168
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StoreParams {
    /// Hours to look back (default: 24)
    #[serde(default = "default_store_hours")]
    pub hours: u64,
    /// Optional project filter
    pub project: Option<String>,
}

fn default_store_hours() -> u64 {
    24
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
        description = "Fuzzy search across stored AI session chunks. Returns quality-scored results with matched lines. Supports Polish diacritics normalization and optional project filtering."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let query = params.query;
        let limit = params.limit.min(50);
        let project = params.project;
        let store_root = store::store_base_dir()
            .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;

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

        let (results, scanned) =
            rank::fuzzy_search_store(&store_root, &query, limit, project.as_deref())
                .map_err(|e| McpError::internal_error(format!("Read store: {e}"), None))?;

        let json = serde_json::to_string_pretty(&serde_json::json!({
            "scanned": scanned,
            "results": results.len(),
            "items": results,
        }))
        .unwrap_or_default();

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

        let store_root = store::store_base_dir()
            .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600);

        let proj_dir = store_root.join(&project);
        if !proj_dir.is_dir() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No data for project '{project}'"
            ))]));
        }

        let mut scored: Vec<serde_json::Value> = Vec::new();

        let Ok(dates) = std::fs::read_dir(&proj_dir) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "Cannot read project dir",
            )]));
        };

        for date_entry in dates.filter_map(|e| e.ok()) {
            let date_path = date_entry.path();
            if !date_path.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&date_path) else {
                continue;
            };
            for file_entry in files.filter_map(|e| e.ok()) {
                let fpath = file_entry.path();
                if fpath.extension().is_none_or(|ext| ext != "md") {
                    continue;
                }
                if let Ok(meta) = fpath.metadata()
                    && let Ok(mtime) = meta.modified()
                    && mtime >= cutoff
                {
                    let cs = rank::score_chunk_file(&fpath);
                    if strict && cs.score < 5 {
                        continue;
                    }
                    scored.push(serde_json::json!({
                        "file": fpath.file_name().unwrap_or_default().to_string_lossy(),
                        "date": date_path.file_name().unwrap_or_default().to_string_lossy(),
                        "score": cs.score,
                        "label": cs.label,
                        "signal": cs.signal_lines,
                        "noise": cs.noise_lines,
                        "total": cs.total_lines,
                        "density": format!("{:.0}%", cs.density * 100.0),
                    }));
                }
            }
        }

        scored.sort_by(|a, b| b["score"].as_u64().cmp(&a["score"].as_u64()));

        if let Some(n) = top {
            scored.truncate(n);
        }

        let output = serde_json::to_string_pretty(&serde_json::json!({
            "project": project,
            "hours": hours,
            "strict": strict,
            "chunks": scored.len(),
            "items": scored,
        }))
        .unwrap_or_default();

        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(
        name = "aicx_refs",
        description = "List stored context files from the aicx central store, filtered by recency and optionally by project. Returns file paths."
    )]
    async fn refs(
        &self,
        Parameters(params): Parameters<RefsParams>,
    ) -> Result<CallToolResult, McpError> {
        let hours = params.hours;
        let project = params.project;
        let strict = params.strict;

        let store_root = store::store_base_dir()
            .map_err(|e| McpError::internal_error(format!("Store error: {e}"), None))?;
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600);

        let mut paths: Vec<PathBuf> = Vec::new();

        let project_dirs: Vec<PathBuf> = if let Some(ref p) = project {
            let d = store_root.join(p);
            if d.is_dir() { vec![d] } else { vec![] }
        } else {
            std::fs::read_dir(&store_root)
                .map_err(|e| McpError::internal_error(format!("{e}"), None))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n != "memex"))
                .collect()
        };

        for proj_dir in project_dirs {
            let Ok(dates) = std::fs::read_dir(&proj_dir) else {
                continue;
            };
            for date_entry in dates.filter_map(|e| e.ok()) {
                let date_path = date_entry.path();
                if !date_path.is_dir() {
                    continue;
                }
                let Ok(files) = std::fs::read_dir(&date_path) else {
                    continue;
                };
                for file_entry in files.filter_map(|e| e.ok()) {
                    let fpath = file_entry.path();
                    if fpath
                        .extension()
                        .is_some_and(|ext| ext == "md" || ext == "json")
                        && let Ok(meta) = fpath.metadata()
                        && let Ok(mtime) = meta.modified()
                        && mtime >= cutoff
                    {
                        if strict {
                            let cs = rank::score_chunk_file(&fpath);
                            if cs.score < 5 {
                                continue;
                            }
                        }
                        paths.push(fpath);
                    }
                }
            }
        }

        paths.sort();
        let text = paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(CallToolResult::success(vec![Content::text(
            if text.is_empty() {
                format!("No context files found within last {hours} hours.")
            } else {
                format!("{} files:\n{text}", paths.len())
            },
        )]))
    }

    #[tool(
        name = "aicx_store",
        description = "Trigger a recent incremental rescan across AI agent sessions (Claude, Codex, Gemini) and store any new chunks centrally. Uses watermarks + dedup to skip already-processed history."
    )]
    async fn store_sync(
        &self,
        Parameters(params): Parameters<StoreParams>,
    ) -> Result<CallToolResult, McpError> {
        let hours = params.hours;
        let project = params.project;
        let args = incremental_rescan_args(hours, project.as_deref());

        let output = std::process::Command::new("aicx")
            .args(&args)
            .output()
            .map_err(|e| McpError::internal_error(format!("Failed to run aicx: {e}"), None))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let status = output
                .status
                .code()
                .map_or_else(|| "signal".to_string(), |code| code.to_string());
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Incremental rescan failed (status: {status}).\n{}\n{}",
                stdout.trim(),
                stderr.trim(),
            ))]));
        }

        let scope = project
            .as_deref()
            .map_or_else(|| "all projects".to_string(), |p| format!("project '{p}'"));
        let mut summary =
            format!("Incremental rescan completed for {scope} over the last {hours}h.");
        if !stderr.trim().is_empty() {
            summary.push('\n');
            summary.push_str(stderr.trim());
        }

        Ok(CallToolResult::success(vec![Content::text(summary)]))
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
pub async fn run_sse(port: u16) -> anyhow::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::incremental_rescan_args;

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
}
