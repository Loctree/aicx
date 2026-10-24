//! `/api/intents` reuses the CLI/MCP display filters. It does not scan again
//! with a second pipeline.

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::dashboard::project_matches_filter;
use crate::intents::{
    IntentDisplayFilters, IntentRecord, IntentSourceFilter, IntentsConfig, apply_display_filters,
    extract_intents_from_root_at_for_projects_with_stats_filtered, intent_display_body,
    intent_display_header,
};
use crate::result_format::RESULT_BODY_PREVIEW_CHARS;

use super::{DashboardServerState, ErrorResponse};

fn default_hours() -> u64 {
    720
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Deserialize)]
pub(super) struct IntentsQuery {
    project: Option<String>,
    agent: Option<String>,
    #[serde(default = "default_hours")]
    hours: u64,
    #[serde(default = "default_limit")]
    limit: usize,
    /// Axum does not parse the query string `"1"` as `bool`.
    full: Option<String>,
    session_id: Option<String>,
    source_chunk: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct IntentApiSelect {
    pub project: Option<String>,
    pub agent: Option<String>,
    pub hours: u64,
    pub limit: Option<usize>,
    pub full: bool,
    pub session_id: Option<String>,
    pub source_chunk: Option<String>,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct IntentApiItem {
    pub header: String,
    pub body: String,
    pub body_chars_total: usize,
    pub body_truncated: bool,
    pub project: String,
    pub agent: String,
    pub kind: String,
    pub date: String,
    pub session_id: String,
    pub source_chunk: String,
    pub preview: String,
    pub surface: &'static str,
}

#[derive(Debug, Serialize)]
struct IntentsResponse {
    ok: bool,
    body_preview_chars: usize,
    items: Vec<IntentApiItem>,
}

pub(super) fn truthy(value: &Option<String>) -> bool {
    matches!(
        value.as_deref().map(str::trim),
        Some("1" | "true" | "yes" | "TRUE" | "True" | "YES")
    )
}

pub(super) fn hours_date_lo(hours: u64, now: DateTime<Utc>) -> Option<String> {
    if hours == 0 {
        return None;
    }
    let hours = i64::try_from(hours).unwrap_or(i64::MAX);
    Some(
        (now - Duration::hours(hours))
            .format("%Y-%m-%d")
            .to_string(),
    )
}

pub(super) fn select_intent_api_items(
    records: Vec<IntentRecord>,
    query: &IntentApiSelect,
) -> Vec<IntentApiItem> {
    let records = records
        .into_iter()
        .filter(|record| project_matches_filter(&record.project, query.project.as_deref()))
        .collect();
    let filters = IntentDisplayFilters {
        agent: query.agent.clone(),
        date_lo: hours_date_lo(query.hours, query.now),
        limit: query.limit,
        ..IntentDisplayFilters::default()
    };
    apply_display_filters(records, &filters)
        .into_iter()
        .filter(|record| {
            query
                .session_id
                .as_ref()
                .is_none_or(|session| record.session_id == *session)
        })
        .filter(|record| {
            query
                .source_chunk
                .as_ref()
                .is_none_or(|chunk| record.source_chunk == *chunk)
        })
        .map(|record| {
            let shown = intent_display_body(&record, query.full);
            IntentApiItem {
                header: intent_display_header(&record),
                preview: shown.body.clone(),
                body: shown.body,
                body_chars_total: shown.body_chars_total,
                body_truncated: shown.body_truncated,
                project: record.project,
                agent: record.agent,
                kind: record.kind.heading().to_string(),
                date: record.date,
                session_id: record.session_id,
                source_chunk: record.source_chunk,
                surface: "intent",
            }
        })
        .collect()
}

pub(super) async fn get_intents(
    State(state): State<Arc<DashboardServerState>>,
    Query(query): Query<IntentsQuery>,
) -> Response {
    let now = Utc::now();
    let projects = query
        .project
        .clone()
        .filter(|project| !project.trim().is_empty())
        .into_iter()
        .collect::<Vec<_>>();
    let date_lo = hours_date_lo(query.hours, now);
    let config = IntentsConfig {
        project: projects.first().cloned().unwrap_or_default(),
        hours: query.hours,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
        live: IntentsConfig::auto_live(query.hours),
    };
    let source_filter = IntentSourceFilter {
        agent: query.agent.clone(),
        date_lo,
        date_hi: None,
        full_source_scan: false,
    };
    let home = state.config.aicx_home.clone();
    let extracted = tokio::task::spawn_blocking(move || {
        extract_intents_from_root_at_for_projects_with_stats_filtered(
            &config,
            &projects,
            &source_filter,
            &home,
            now,
        )
    })
    .await;

    let extraction = match extracted {
        Ok(Ok(extraction)) => extraction,
        Ok(Err(error)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    ok: false,
                    error: error.to_string(),
                }),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    ok: false,
                    error: format!("intent extraction task failed: {error}"),
                }),
            )
                .into_response();
        }
    };

    let items = select_intent_api_items(
        extraction.records,
        &IntentApiSelect {
            project: query.project,
            agent: query.agent,
            hours: query.hours,
            limit: Some(query.limit),
            full: truthy(&query.full),
            session_id: query.session_id,
            source_chunk: query.source_chunk,
            now,
        },
    );
    (
        StatusCode::OK,
        Json(IntentsResponse {
            ok: true,
            body_preview_chars: RESULT_BODY_PREVIEW_CHARS,
            items,
        }),
    )
        .into_response()
}
