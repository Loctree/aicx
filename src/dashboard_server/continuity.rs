//! Browser adapter for the CLI continuity pack. No corpus or renderer fork.
use super::*;
use crate::{legacy_archive, mcp_session::ContinuityRequest};
use axum::extract::{Query, rejection::QueryRejection};
use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct DashboardContinuityQuery {
    project: String,
    #[serde(rename = "as")]
    addressee: Option<String>,
    #[serde(default = "crate::mcp_session::default_continuity_hours")]
    hours: u64,
    #[serde(default)]
    inject: bool,
    until: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct Section {
    title: String,
    /// Includes the heading and blank lines; all sections sum to total_lines.
    lines: usize,
    markdown: String,
}

pub(super) async fn get_continuity(
    State(state): State<Arc<DashboardServerState>>,
    query: Result<Query<DashboardContinuityQuery>, QueryRejection>,
) -> Response {
    let result = match query {
        Err(error) => Err((StatusCode::BAD_REQUEST, error.body_text())),
        Ok(Query(query)) => {
            let home = state.config.aicx_home.clone();
            // Live parsing and addressed bus reads must not hold an async worker.
            tokio::task::spawn_blocking(move || build_response(&home, query))
                .await
                .unwrap_or_else(|error| Err((StatusCode::INTERNAL_SERVER_ERROR, error.to_string())))
        }
    };
    let mut response = match result {
        Ok(payload) => Json(payload).into_response(),
        Err((status, error)) => (status, Json(ErrorResponse { ok: false, error })).into_response(),
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn build_response(
    home: &std::path::Path,
    query: DashboardContinuityQuery,
) -> Result<serde_json::Value, (StatusCode, String)> {
    let bad_request = |error: String| (StatusCode::BAD_REQUEST, error);
    if query.project.trim().is_empty() {
        return Err(bad_request(
            "project is required; pass an exact owner/repo".into(),
        ));
    }
    if query
        .addressee
        .as_deref()
        .is_some_and(|name| name.trim().is_empty() || name.contains('\0'))
    {
        return Err(bad_request(
            "addressed name must not be empty or contain NUL".into(),
        ));
    }
    let until = query.until.unwrap_or_else(Utc::now);
    // Reuse the session surface request and its defaults. Resolve strictly as
    // the CLI does: MCP intentionally permits unknown shaped project buckets.
    let request = ContinuityRequest {
        aicx_home: home,
        project: Some(&query.project),
        projects: &[],
        project_match: legacy_archive::ProjectMatchMode::Exact,
        hours: query.hours,
        for_inject: query.inject,
        until: Some(until),
        distill_mixed: false,
    };
    let internal = |error: anyhow::Error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string());
    let corpus =
        legacy_archive::project_identities_for_search_at(request.aicx_home).map_err(internal)?;
    let filters = vec![query.project.clone()];
    let resolution =
        legacy_archive::require_project_resolution(&filters, &corpus, request.project_match)
            .map_err(|error| bad_request(error.to_string()))?;
    let pack = crate::continuity::build_with_scope_at(
        request.aicx_home,
        &resolution.selected,
        request.hours,
        request.distill_mixed,
        until,
    )
    .map_err(internal)?;
    let markdown =
        crate::continuity::render_as(&pack, request.for_inject, query.addressee.as_deref())
            .map_err(internal)?;
    let sections = split_sections(&markdown);
    // Pin the window end even when omitted by the browser. --no-refresh avoids
    // changing the catalog between this read-only request and its CLI replay.
    let mut args = vec![
        "aicx".into(),
        "continuity".into(),
        "show".into(),
        "-p".into(),
        query.project.clone(),
        "-H".into(),
        request.hours.to_string(),
        "--until".into(),
        until.to_rfc3339(),
        "--no-refresh".into(),
    ];
    if let Some(name) = query.addressee {
        args.extend(["--as".into(), name]);
    }
    if request.for_inject {
        args.push("--for-inject".into());
    }
    let cli_command = args
        .iter()
        .map(|arg| shlex::try_quote(arg).map(|s| s.into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| bad_request(error.to_string()))?
        .join(" ");
    Ok(serde_json::json!({
        "ok": true, "markdown": markdown, "sections": sections,
        "total_lines": markdown.lines().count(), "cli_command": cli_command,
        "until": until.to_rfc3339(), "projects": resolution.selected,
        "hours": request.hours, "inject": request.for_inject,
    }))
}

fn split_sections(markdown: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut section = Section {
        title: "Continuity pack".into(),
        lines: 0,
        markdown: String::new(),
    };
    for line in markdown.split_inclusive('\n') {
        if let Some(title) = line.strip_prefix("## ") {
            if !section.markdown.is_empty() {
                sections.push(section);
            }
            section = Section {
                title: title.trim().into(),
                lines: 0,
                markdown: String::new(),
            };
        }
        section.lines += 1;
        section.markdown.push_str(line);
    }
    if !section.markdown.is_empty() {
        sections.push(section);
    }
    sections
}
