use anyhow::Result;
use axum::{
    Json,
    extract::{Query, State, rejection::QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::dashboard::project_matches_filter;

use super::{DashboardServerState, ErrorResponse};

pub(super) const MAX_SCORE_FILTER: u8 = 100;

fn default_search_limit() -> usize {
    20
}

#[derive(Debug, Deserialize)]
pub(super) struct SemanticSearchParams {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
    /// Optional project filter routed through the canonical
    /// `aicx::store::project_filter_matches` helper. Strict semantics: no
    /// substring matching. Prefer `projects` for cross-project search.
    project: Option<String>,
    /// Optional project filters for cross-project search (strict).
    #[serde(default)]
    projects: Vec<String>,
    /// Optional minimum score threshold (0-100)
    score: Option<u8>,
    /// Optional frame/channel filter
    frame_kind: Option<crate::timeline::FrameKind>,
    /// Optional canonical corpus kind filter
    kind: Option<String>,
}

#[derive(Debug, Serialize)]
struct FuzzySearchResult {
    file: String,
    path: String,
    project: String,
    kind: String,
    frame_kind: Option<String>,
    agent: String,
    date: String,
    score: u8,
    label: String,
    signal_density: f32,
    matched_lines: Vec<String>,
    excerpt: String,
}

#[derive(Debug, Serialize)]
struct FuzzySearchResponse {
    ok: bool,
    query: String,
    results: Vec<FuzzySearchResult>,
    total_scanned: usize,
}

#[derive(Debug, Deserialize)]
pub(super) struct SteerSearchParams {
    /// Filter by run_id (exact)
    run_id: Option<String>,
    /// Filter by prompt_id (exact)
    prompt_id: Option<String>,
    /// Filter by agent name
    agent: Option<String>,
    /// Filter by kind
    kind: Option<String>,
    /// Filter by frame/channel
    frame_kind: Option<crate::timeline::FrameKind>,
    /// Filter by project (strict, via `aicx::store::project_filter_matches`)
    project: Option<String>,
    /// Filter by multiple project filters (strict)
    #[serde(default)]
    projects: Vec<String>,
    /// Filter by date (YYYY-MM-DD or range)
    date: Option<String>,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

#[derive(Debug, Serialize)]
struct SteerSearchResult {
    path: String,
    project: String,
    agent: String,
    kind: String,
    frame_kind: Option<String>,
    date: String,
    session_id: String,
    run_id: Option<String>,
    prompt_id: Option<String>,
    agent_model: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    token_usage: Option<u64>,
    findings_count: Option<u32>,
}

#[derive(Debug, Serialize)]
struct SteerSearchResponse {
    ok: bool,
    scanned: usize,
    matched: usize,
    items: Vec<SteerSearchResult>,
}

/// Combine a startup `--project` scope with the request-level `project` /
/// `projects` filters into a single list. The rollup is canonical-equality
/// only: a request filter is kept when it matches the startup scope
/// case-insensitively after trimming. The previous reverse-substring
/// rollup (`request.contains(scope)`) silently collapsed `vista` +
/// `vista-portal` into the same bucket — that's bug #28 and intentionally
/// gone here.
pub(super) fn merge_project_scopes(
    scope: Option<&str>,
    request: Option<String>,
    requests: Vec<String>,
) -> Vec<String> {
    let mut merged = if requests.is_empty() {
        request.into_iter().collect::<Vec<_>>()
    } else {
        requests
    };

    if let Some(scope) = scope {
        if merged.is_empty() {
            merged.push(scope.to_string());
        } else {
            let scope_trimmed = scope.trim();
            merged.retain(|request| request.trim().eq_ignore_ascii_case(scope_trimmed));
            if merged.is_empty() {
                merged.push(scope.to_string());
            }
        }
    }

    merged
}

fn search_project_scopes(projects: &[String]) -> Vec<Option<&str>> {
    if projects.is_empty() {
        vec![None]
    } else {
        projects.iter().map(String::as_str).map(Some).collect()
    }
}

fn project_matches_any_filter(project: &str, filters: &[String]) -> bool {
    filters.is_empty()
        || filters
            .iter()
            .any(|filter| project_matches_filter(project, Some(filter.as_str())))
}

pub(super) fn validate_score_filter(score: Option<u8>) -> Result<Option<u8>, String> {
    match score {
        Some(score) if score > MAX_SCORE_FILTER => {
            Err(format!("score must be between 0 and {MAX_SCORE_FILTER}"))
        }
        _ => Ok(score),
    }
}

/// In-process semantic search against the persistent NDJSON vector
/// index ([`crate::vector_index::query_index`]). No external memex CLI
/// spawn — the dashboard ships with the same `try_semantic_search`
/// dispatch the CLI and MCP surfaces use.
///
/// Fails fast (HTTP 422) with `kind` + `reason` + `recommendation`
/// when a precondition is missing (embedder unhydrated, vector index
/// not built, dimension mismatch). Operators see the same diagnostic
/// they would see from `aicx search` directly.
pub(super) async fn get_semantic_search(
    State(state): State<Arc<DashboardServerState>>,
    params: Result<Query<SemanticSearchParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(q) => q,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    ok: false,
                    error: format!("Invalid query parameters: {rejection}"),
                }),
            )
                .into_response();
        }
    };

    let query = params.q.trim().to_string();
    if query.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                ok: false,
                error: "Query parameter 'q' is required".to_string(),
            }),
        )
            .into_response();
    }

    let limit = params.limit.min(100);
    let store_root = state.config.store_root.clone();
    let scope = state.config.scope.normalized();
    let request_projects = merge_project_scopes(
        scope.project.as_deref(),
        params.project.clone(),
        params.projects.clone(),
    );
    let frame_kind = params.frame_kind;
    let kind_filter = match params.kind.as_deref() {
        Some(kind) => match crate::timeline::Kind::parse(kind) {
            Some(kind) => Some(kind),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ErrorResponse {
                        ok: false,
                        error: format!(
                            "Invalid kind '{kind}'. Expected conversations, plans, reports, or other"
                        ),
                    }),
                )
                    .into_response();
            }
        },
        None => None,
    };
    let score_filter = match validate_score_filter(params.score) {
        Ok(score) => score,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse { ok: false, error }),
            )
                .into_response();
        }
    };
    let query_clone = query.clone();
    let project_owned = request_projects.clone();
    let kind_filter_owned = kind_filter;

    let result = tokio::task::spawn_blocking(move || {
        let project_scopes = search_project_scopes(&project_owned);
        crate::search_engine::try_semantic_search(
            &store_root,
            &query_clone,
            limit,
            &project_scopes,
            frame_kind,
            kind_filter_owned.map(|kind| kind.dir_name()),
        )
    })
    .await;

    match result {
        Ok(Ok(outcome)) => {
            let mut results: Vec<_> = outcome
                .results
                .into_iter()
                .filter(|r| project_matches_any_filter(&r.project, &request_projects))
                .filter(|r| score_filter.is_none_or(|min| r.score >= min))
                .map(|result| {
                    let excerpt = result.matched_lines.join(" ... ");
                    FuzzySearchResult {
                        file: result.file,
                        path: result.path,
                        project: result.project,
                        kind: result.kind,
                        frame_kind: result.frame_kind,
                        agent: result.agent,
                        date: result.date,
                        score: result.score,
                        label: result.label,
                        signal_density: result.density,
                        matched_lines: result.matched_lines,
                        excerpt,
                    }
                })
                .collect();
            results.truncate(limit);
            let total_scanned = outcome.scanned;
            (
                StatusCode::OK,
                Json(FuzzySearchResponse {
                    ok: true,
                    query,
                    results,
                    total_scanned,
                }),
            )
                .into_response()
        }
        Ok(Err(err)) => {
            // Fail-fast with the same kind/reason/recommendation triple
            // that the CLI and MCP surfaces emit. Status 422 to signal
            // "request was valid but a precondition is missing" rather
            // than 500 (server bug).
            let payload = serde_json::json!({
                "ok": false,
                "error": "semantic_search_unavailable",
                "kind": err.kind(),
                "reason": err.reason(),
                "recommendation": err.recommendation(),
            });
            (StatusCode::UNPROCESSABLE_ENTITY, Json(payload)).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("Search task failed: {err}"),
            }),
        )
            .into_response(),
    }
}

// Cross-namespace memex CLI fork removed: `aicx` no longer shells out to
// `rust-memex` / `rmcp-memex`. AICX is the canonical corpus surface
// (operator decision 2026-05-23). The `/api/search/cross` route is kept
// so a client polling the endpoint gets a clean Gone surface instead of
// a 404 that looks like a routing mistake.
pub(super) async fn cross_search_gone() -> Response {
    (
        StatusCode::GONE,
        Json(serde_json::json!({
            "ok": false,
            "error": "cross_search_removed",
            "reason": "The /api/search/cross endpoint backed by the rust-memex / rmcp-memex CLI fork has been removed. AICX is the canonical corpus; use /api/search/semantic instead.",
        })),
    )
        .into_response()
}

/// Steering-metadata search across stored chunks.
///
/// Filters by sidecar metadata (run_id, prompt_id, agent, kind, project, date)
/// using canonical chunk dates instead of filesystem mtime.
pub(super) async fn steer_search(
    params: Result<Query<SteerSearchParams>, QueryRejection>,
) -> Response {
    let Query(params) = match params {
        Ok(q) => q,
        Err(rejection) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    ok: false,
                    error: format!("Invalid query parameters: {rejection}"),
                }),
            )
                .into_response();
        }
    };

    let limit = params.limit.min(100);

    let result = tokio::task::spawn_blocking(move || run_steer_search(params, limit)).await;

    match result {
        Ok(Ok(response)) => (StatusCode::OK, Json(response)).into_response(),
        Ok(Err(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("{err:#}"),
            }),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                ok: false,
                error: format!("Steer search task failed: {err}"),
            }),
        )
            .into_response(),
    }
}

fn parse_date_bounds(date: &str) -> (Option<String>, Option<String>) {
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

fn run_steer_search(params: SteerSearchParams, limit: usize) -> Result<SteerSearchResponse> {
    let rt = tokio::runtime::Runtime::new()?;

    let (date_lo, date_hi) = if let Some(ref d) = params.date {
        parse_date_bounds(d)
    } else {
        (None, None)
    };

    let project_filters = merge_project_scopes(None, params.project.clone(), params.projects);
    let mut metadatas = Vec::new();
    for project in search_project_scopes(&project_filters) {
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
        let mut batch = rt.block_on(crate::steer_index::search_steer_index(&filter, limit))?;
        metadatas.append(&mut batch);
    }
    dedup_steer_metadata(&mut metadatas);
    metadatas.truncate(limit);

    let mut items = Vec::new();

    for meta in metadatas {
        let path = meta
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let project = meta
            .get("project")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let agent = meta
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let kind = meta
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let date = meta
            .get("date")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let session_id = meta
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let run_id = meta
            .get("run_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let prompt_id = meta
            .get("prompt_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let agent_model = meta
            .get("agent_model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let started_at = meta
            .get("started_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let completed_at = meta
            .get("completed_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let token_usage = meta.get("token_usage").and_then(|v| v.as_u64());
        let findings_count = meta
            .get("findings_count")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32);

        items.push(SteerSearchResult {
            path,
            project,
            agent,
            kind,
            frame_kind: meta
                .get("frame_kind")
                .and_then(|v| v.as_str())
                .map(String::from),
            date,
            session_id,
            run_id,
            prompt_id,
            agent_model,
            started_at,
            completed_at,
            token_usage,
            findings_count,
        });
    }

    Ok(SteerSearchResponse {
        ok: true,
        scanned: 0,
        matched: items.len(),
        items,
    })
}

fn dedup_steer_metadata(metadatas: &mut Vec<serde_json::Value>) {
    let mut seen = std::collections::BTreeSet::new();
    metadatas.retain(|meta| {
        let key = meta
            .get("path")
            .or_else(|| meta.get("source_chunk"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| meta.to_string());
        seen.insert(key)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bug #28 regression: `merge_project_scopes` must roll up by canonical
    // case-insensitive equality, not by reverse substring. `vista` no longer
    // collapses `vista-portal` into the same bucket.
    #[test]
    fn merge_project_scopes_rejects_substring_rollup() {
        let merged = merge_project_scopes(
            Some("vista"),
            None,
            vec!["vista-portal".to_string(), "vetcoders/vista".to_string()],
        );
        assert_eq!(
            merged,
            vec!["vista".to_string()],
            "reverse-substring rollup leaked: vista-portal / vetcoders/vista must NOT be retained when scope is `vista`"
        );
    }

    #[test]
    fn merge_project_scopes_keeps_canonical_match_case_insensitive() {
        let merged = merge_project_scopes(
            Some("Vetcoders/Vista"),
            None,
            vec![
                "vetcoders/vista".to_string(),
                "vetcoders/vista-portal".to_string(),
            ],
        );
        assert_eq!(
            merged,
            vec!["vetcoders/vista".to_string()],
            "canonical-equality rollup must keep only the exact match (case-insensitive)"
        );
    }

    #[test]
    fn merge_project_scopes_falls_back_to_scope_when_no_overlap() {
        let merged = merge_project_scopes(Some("vista"), None, vec!["vista-portal".to_string()]);
        assert_eq!(
            merged,
            vec!["vista".to_string()],
            "when no request matches the scope canonically, fall back to scope alone"
        );
    }

    #[test]
    fn score_filter_rejects_values_above_max() {
        let err = validate_score_filter(Some(MAX_SCORE_FILTER + 1))
            .expect_err("score above 100 should be rejected");
        assert_eq!(err, "score must be between 0 and 100");
    }
}
