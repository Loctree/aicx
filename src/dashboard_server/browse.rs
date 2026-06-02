use anyhow::{Context, Result, anyhow};
use axum::{
    Json,
    extract::{Query, State, rejection::QueryRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    dashboard::{DashboardRecord, DashboardStats, project_matches_filter},
    sanitize,
};

use super::{DashboardServerState, ErrorResponse};

/// Lightweight record for browse listing (no search_blob, no detail_text).
#[derive(Debug, Serialize)]
struct BrowseRecord {
    id: usize,
    project: String,
    agent: String,
    date: String,
    time: String,
    kind: String,
    file_name: String,
    relative_path: String,
    absolute_path: String,
    bytes: u64,
    size_human: String,
    modified_utc: String,
    sort_ts: i64,
    entry_count: Option<usize>,
    preview: String,
}

impl From<&DashboardRecord> for BrowseRecord {
    fn from(r: &DashboardRecord) -> Self {
        Self {
            id: r.id,
            project: r.project.clone(),
            agent: r.agent.clone(),
            date: r.date.clone(),
            time: r.time.clone(),
            kind: r.kind.clone(),
            file_name: r.file_name.clone(),
            relative_path: r.relative_path.clone(),
            absolute_path: r.absolute_path.clone(),
            bytes: r.bytes,
            size_human: r.size_human.clone(),
            modified_utc: r.modified_utc.clone(),
            sort_ts: r.sort_ts,
            entry_count: r.entry_count,
            preview: r.preview.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
struct BrowseResponse {
    ok: bool,
    generated_at: String,
    stats: DashboardStats,
    assumptions: Vec<String>,
    projects: Vec<String>,
    agents: Vec<String>,
    kinds: Vec<String>,
    records: Vec<BrowseRecord>,
}

#[derive(Debug, Deserialize)]
pub(super) struct BrowseParams {
    project: Option<String>,
    agent: Option<String>,
    kind: Option<String>,
    #[serde(default = "default_browse_sort")]
    sort: String,
    since: Option<String>,
}

fn default_browse_sort() -> String {
    "newest".to_string()
}

fn parse_relative_time(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let now = Utc::now().timestamp();
    let (num, unit) = if let Some(h) = s.strip_suffix('h') {
        (h.parse::<i64>().ok()?, 3600)
    } else if let Some(d) = s.strip_suffix('d') {
        (d.parse::<i64>().ok()?, 86400)
    } else {
        return None;
    };
    // saturate on overflow rather than panic; user input may be adversarial
    Some(now.saturating_sub(num.saturating_mul(unit)))
}

#[derive(Debug, Deserialize)]
pub(super) struct ChunkParams {
    id: Option<usize>,
    path: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChunkResponse {
    ok: bool,
    id: Option<usize>,
    path: String,
    content: String,
    project: String,
    agent: String,
    kind: String,
    date: String,
    bytes: u64,
}

/// Browse all records with optional server-side filtering.
pub(super) async fn get_browse(
    State(state): State<Arc<DashboardServerState>>,
    params: Result<Query<BrowseParams>, QueryRejection>,
) -> Response {
    let params = match params {
        Ok(Query(p)) => p,
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

    let snapshot = state.snapshot.read().await;
    let since_ts = params.since.as_deref().and_then(parse_relative_time);

    let mut records: Vec<BrowseRecord> = snapshot
        .payload
        .records
        .iter()
        .filter(|r| {
            if let Some(ref p) = params.project
                && !project_matches_filter(&r.project, Some(p))
            {
                return false;
            }
            if let Some(ref a) = params.agent
                && !r.agent.eq_ignore_ascii_case(a)
            {
                return false;
            }
            if let Some(ref k) = params.kind
                && r.kind != *k
            {
                return false;
            }
            if let Some(ts) = since_ts
                && r.sort_ts < ts
            {
                return false;
            }
            true
        })
        .map(BrowseRecord::from)
        .collect();

    match params.sort.as_str() {
        "oldest" => records.sort_by_key(|r| r.sort_ts),
        _ => records.sort_by_key(|b| std::cmp::Reverse(b.sort_ts)),
    }

    (
        StatusCode::OK,
        Json(BrowseResponse {
            ok: true,
            generated_at: snapshot.payload.generated_at.clone(),
            stats: snapshot.stats.clone(),
            assumptions: snapshot.assumptions.clone(),
            projects: snapshot.payload.projects.clone(),
            agents: snapshot.payload.agents.clone(),
            kinds: snapshot.payload.kinds.clone(),
            records,
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct DetailParams {
    id: usize,
}

#[derive(Debug, Serialize)]
struct DetailResponse {
    ok: bool,
    id: usize,
    detail_text: String,
}

/// Fetch detail_text for a single record by id.
pub(super) async fn get_detail(
    State(state): State<Arc<DashboardServerState>>,
    params: Result<Query<DetailParams>, QueryRejection>,
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

    let snapshot = state.snapshot.read().await;
    // Record IDs are 1-based (assigned as idx+1 in scan_store), so look up by
    // matching the id field rather than using it as a raw array index.
    if let Some(record) = snapshot.payload.records.iter().find(|r| r.id == params.id) {
        (
            StatusCode::OK,
            Json(DetailResponse {
                ok: true,
                id: params.id,
                detail_text: record.detail_text.clone(),
            }),
        )
            .into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                ok: false,
                error: format!("Record id {} not found", params.id),
            }),
        )
            .into_response()
    }
}

pub(super) async fn get_chunk(
    State(state): State<Arc<DashboardServerState>>,
    params: Result<Query<ChunkParams>, QueryRejection>,
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

    let snapshot = state.snapshot.read().await;
    let store_root = &state.config.store_root;

    let record = if let Some(id) = params.id {
        snapshot.payload.records.iter().find(|r| r.id == id)
    } else if let Some(ref rel_path) = params.path {
        snapshot
            .payload
            .records
            .iter()
            .find(|r| r.relative_path == *rel_path)
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                ok: false,
                error: "Either 'id' or 'path' parameter is required".to_string(),
            }),
        )
            .into_response();
    };

    let Some(record) = record else {
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                ok: false,
                error: "Chunk not found".to_string(),
            }),
        )
            .into_response();
    };

    let file_path = store_root.join(&record.relative_path);
    let file_path = match resolve_bounded_path(store_root, &file_path) {
        Ok(p) => p,
        Err(err) => {
            return (
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    ok: false,
                    error: format!("Path resolution rejected: {err}"),
                }),
            )
                .into_response();
        }
    };

    let content = match sanitize::read_to_string_validated(&file_path) {
        Ok(c) => c,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    ok: false,
                    error: format!("Failed to read chunk: {err}"),
                }),
            )
                .into_response();
        }
    };

    (
        StatusCode::OK,
        Json(ChunkResponse {
            ok: true,
            id: Some(record.id),
            path: record.relative_path.clone(),
            content,
            project: record.project.clone(),
            agent: record.agent.clone(),
            kind: record.kind.clone(),
            date: record.date.clone(),
            bytes: record.bytes,
        }),
    )
        .into_response()
}

fn resolve_bounded_path(root: &Path, target: &Path) -> Result<PathBuf> {
    let target_str = target.to_string_lossy();
    if target_str.contains("..") {
        return Err(anyhow!("Path contains traversal sequence"));
    }

    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize store root: {}", root.display()))?;

    if !target.exists() {
        return Err(anyhow!("Path does not exist: {}", target.display()));
    }

    let canonical_target = target
        .canonicalize()
        .with_context(|| format!("Cannot canonicalize target: {}", target.display()))?;

    if !canonical_target.starts_with(&canonical_root) {
        return Err(anyhow!(
            "Path escapes store root: {} is not under {}",
            canonical_target.display(),
            canonical_root.display()
        ));
    }

    Ok(canonical_target)
}
