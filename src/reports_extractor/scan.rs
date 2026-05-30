use super::support::{
    build_detail_text, build_preview, build_record_key, collapse_ws, contains_case_insensitive,
    derive_agent, derive_lane_and_workflow, derive_status, derive_title, file_modified,
    format_date_window, format_modified_utc, matches_date_filter, normalize_date_bucket,
    normalized_eq, path_contains_segment, path_string_without_suffix, pick_sort_ts, read_markdown,
    read_meta, relative_components, resolve_artifact_reference, validate_artifact_dir,
    validate_artifact_file,
};
use super::types::{
    Candidate, DateFilter, ReportsExplorerPayload, ReportsExplorerRecord, ReportsExplorerStats,
    ReportsExtractorConfig,
};
use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub(super) fn scan_reports(
    repo_root: &Path,
    artifacts_root: &Path,
    config: &ReportsExtractorConfig,
) -> Result<ReportsExplorerPayload> {
    let mut assumptions = vec![
        "Scans Vibecrafted markdown plans/reports plus optional .meta.json companions under the central artifacts tree.".to_string(),
        "Meta-only or transcript-backed runs are surfaced honestly instead of being dropped from the explorer.".to_string(),
        "Standalone HTML includes an embedded JSON payload and can merge additional bundle files client-side.".to_string(),
    ];
    assumptions
        .push("Legacy artifacts are skipped by default in this first explorer pass.".to_string());
    if let Some(workflow) = config.workflow.as_ref() {
        assumptions.push(format!(
            "Workflow filter applied during extraction: {}",
            workflow
        ));
    }
    if config.date_from.is_some() || config.date_to.is_some() {
        assumptions.push(format!(
            "Date window applied during extraction: {} .. {}",
            config
                .date_from
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "open".to_string()),
            config
                .date_to
                .map(|date| date.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "open".to_string())
        ));
    }

    let date_filter = DateFilter {
        start: config.date_from,
        end: config.date_to,
    };
    let mut candidates = BTreeMap::<String, Candidate>::new();
    collect_candidates(repo_root, repo_root, Path::new(""), &mut candidates)?;

    let mut records = Vec::<ReportsExplorerRecord>::new();
    let mut workflows = BTreeSet::<String>::new();
    let mut agents = BTreeSet::<String>::new();
    let mut statuses = BTreeSet::<String>::new();
    let mut lanes = BTreeSet::<String>::new();
    let mut days = BTreeSet::<String>::new();
    let mut duration_total = 0.0_f64;
    let mut duration_count = 0_u64;

    for candidate in candidates.values() {
        let record = finalize_candidate(candidate, repo_root, config, &date_filter)?;
        let Some(record) = record else {
            continue;
        };

        workflows.insert(record.workflow.clone());
        agents.insert(record.agent.clone());
        statuses.insert(record.status.clone());
        lanes.insert(record.lane.clone());
        days.insert(record.date_iso.clone());
        if let Some(duration) = record.duration_s {
            duration_total += duration;
            duration_count += 1;
        }
        records.push(record);
    }

    records.sort_by(|left, right| {
        right
            .sort_ts
            .cmp(&left.sort_ts)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    for (idx, record) in records.iter_mut().enumerate() {
        record.id = idx + 1;
    }

    let total_reports = records
        .iter()
        .filter(|record| record.record_kind != "plan")
        .count();
    let total_plans = records
        .iter()
        .filter(|record| record.record_kind == "plan")
        .count();
    let total_meta_only = records
        .iter()
        .filter(|record| record.has_meta && !record.has_markdown)
        .count();
    let total_transcript_backed = records
        .iter()
        .filter(|record| record.has_transcript)
        .count();
    let completed_records = records
        .iter()
        .filter(|record| normalized_eq(&record.status, "completed"))
        .count();
    let incomplete_records = records.len().saturating_sub(completed_records);

    let stats = ReportsExplorerStats {
        total_records: records.len(),
        total_reports,
        total_plans,
        total_meta_only,
        total_transcript_backed,
        completed_records,
        incomplete_records,
        total_days: days.len(),
        total_workflows: workflows.len(),
        total_agents: agents.len(),
        avg_duration_s: if duration_count > 0 {
            Some(duration_total / duration_count as f64)
        } else {
            None
        },
    };

    let generated_at = if config.deterministic {
        // Pick the latest record sort timestamp so the same artifact tree
        // produces the same `generated_at` across runs. Empty tree falls back
        // to the Unix epoch sentinel rather than `Utc::now()`.
        records
            .iter()
            .map(|record| record.sort_ts)
            .max()
            .and_then(|ts| DateTime::<Utc>::from_timestamp(ts, 0))
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| "1970-01-01T00:00:00+00:00".to_string())
    } else {
        Utc::now().to_rfc3339()
    };

    Ok(ReportsExplorerPayload {
        schema_version: 1,
        generated_at,
        artifacts_root: artifacts_root.display().to_string(),
        resolved_org: config.org.clone(),
        resolved_repo: config.repo.clone(),
        scan_root: repo_root.display().to_string(),
        selected_date: format_date_window(config.date_from, config.date_to),
        selected_workflow: config.workflow.clone(),
        stats,
        assumptions,
        workflows: workflows.into_iter().collect(),
        agents: agents.into_iter().collect(),
        statuses: statuses.into_iter().collect(),
        lanes: lanes.into_iter().collect(),
        days: days.into_iter().collect(),
        records,
    })
}

fn collect_candidates(
    scan_root: &Path,
    dir: &Path,
    relative: &Path,
    candidates: &mut BTreeMap<String, Candidate>,
) -> Result<()> {
    let dir = validate_artifact_dir(scan_root, dir)?;
    let mut entries = crate::sanitize::read_dir_validated(&dir)
        .with_context(|| format!("Failed to read artifact directory: {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("Failed to iterate artifact directory: {}", dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let rel = relative.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_candidates(scan_root, &path, &rel, candidates)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let path = match validate_artifact_file(scan_root, &path) {
            Ok(path) => path,
            Err(_) => continue,
        };

        let rel_string = rel.to_string_lossy();
        let contains_lane = rel_string.contains("/reports/") || rel_string.contains("/plans/");
        if !contains_lane && !rel_string.ends_with("/reports") && !rel_string.ends_with("/plans") {
            continue;
        }

        let file_name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name,
            None => continue,
        };

        if file_name.ends_with(".meta.json") {
            let key = path_string_without_suffix(&path, ".meta.json");
            candidates.entry(key).or_default().meta_path = Some(path);
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            let key = path_string_without_suffix(&path, ".md");
            candidates.entry(key).or_default().md_path = Some(path);
        }
    }

    Ok(())
}

fn finalize_candidate(
    candidate: &Candidate,
    repo_root: &Path,
    config: &ReportsExtractorConfig,
    date_filter: &DateFilter,
) -> Result<Option<ReportsExplorerRecord>> {
    let primary_path = candidate
        .md_path
        .as_ref()
        .or(candidate.meta_path.as_ref())
        .ok_or_else(|| anyhow!("artifact candidate without markdown or metadata path"))?;
    let relative = primary_path
        .strip_prefix(repo_root)
        .with_context(|| {
            format!(
                "Failed to resolve relative artifact path for {}",
                primary_path.display()
            )
        })?
        .to_path_buf();

    let path_parts = relative_components(&relative);
    if path_parts.is_empty() {
        return Ok(None);
    }
    let date_bucket = path_parts[0].clone();
    if date_bucket == "legacy" {
        return Ok(None);
    }

    let date_iso = normalize_date_bucket(&date_bucket).unwrap_or_else(|| date_bucket.clone());
    if !matches_date_filter(&date_iso, date_filter) {
        return Ok(None);
    }

    let meta = if let Some(meta_path) = candidate.meta_path.as_ref() {
        Some(read_meta(repo_root, meta_path)?)
    } else {
        None
    };

    let markdown = if let Some(md_path) = candidate.md_path.as_ref() {
        Some(read_markdown(repo_root, md_path)?)
    } else {
        None
    };

    let title = derive_title(
        markdown.as_ref().map(|item| item.body.as_str()),
        primary_path,
        "day-root",
        meta.as_ref(),
    );
    let (lane, workflow) = derive_lane_and_workflow(
        &path_parts,
        primary_path,
        &title,
        markdown.as_ref(),
        meta.as_ref(),
    );
    if let Some(filter) = config.workflow.as_ref() {
        let haystack = format!("{workflow} {lane} {}", relative.display());
        if !contains_case_insensitive(&haystack, filter) {
            return Ok(None);
        }
    }

    let agent = derive_agent(&title, &path_parts, markdown.as_ref(), meta.as_ref());

    let status = derive_status(&lane, markdown.as_ref(), meta.as_ref());

    let transcript_path = meta
        .as_ref()
        .and_then(|item| item.transcript.as_ref())
        .and_then(|path| {
            candidate
                .meta_path
                .as_ref()
                .and_then(|origin| resolve_artifact_reference(repo_root, origin, path))
        });
    let has_transcript = transcript_path.is_some();

    let detail_text = build_detail_text(
        repo_root,
        markdown.as_ref(),
        transcript_path.as_deref(),
        meta.as_ref(),
    );
    let preview = build_preview(
        markdown.as_ref(),
        detail_text.as_str(),
        config.preview_chars,
        &status,
        &title,
    );

    let headings = markdown
        .as_ref()
        .map(|item| item.headings.clone())
        .unwrap_or_default();

    let meta_path_string = candidate
        .meta_path
        .as_ref()
        .map(|path| path.display().to_string());
    let absolute_path = candidate
        .md_path
        .as_ref()
        .map(|path| path.display().to_string())
        .or_else(|| meta.as_ref().and_then(|item| item.report.clone()))
        .unwrap_or_else(|| primary_path.display().to_string());
    let relative_path = relative.display().to_string();

    let run_id = markdown
        .as_ref()
        .and_then(|item| item.frontmatter.report.telemetry.run_id.clone())
        .or_else(|| meta.as_ref().and_then(|item| item.run_id.clone()));
    let prompt_id = markdown
        .as_ref()
        .and_then(|item| item.frontmatter.report.telemetry.prompt_id.clone())
        .or_else(|| meta.as_ref().and_then(|item| item.prompt_id.clone()));
    let skill_code = markdown
        .as_ref()
        .and_then(|item| item.frontmatter.report.steering.skill_code.clone())
        .or_else(|| meta.as_ref().and_then(|item| item.skill_code.clone()));
    let mode = markdown
        .as_ref()
        .and_then(|item| item.frontmatter.report.steering.mode.clone())
        .or_else(|| meta.as_ref().and_then(|item| item.mode.clone()));
    let completed_at = meta
        .as_ref()
        .and_then(|item| item.completed_at.clone())
        .or_else(|| {
            markdown
                .as_ref()
                .and_then(|item| item.frontmatter.created.clone())
        });
    let updated_at = meta
        .as_ref()
        .and_then(|item| item.updated_at.clone())
        .or_else(|| Some(format_modified_utc(file_modified(primary_path))));
    let duration_s = meta.as_ref().and_then(|item| item.duration_s);
    let loop_nr = meta.as_ref().and_then(|item| item.loop_nr);
    let session_id = meta.as_ref().and_then(|item| item.session_id.clone());

    let search_blob = collapse_ws(&format!(
        "{} {} {} {} {} {} {} {} {} {} {} {}",
        title,
        workflow,
        lane,
        status,
        agent,
        skill_code.clone().unwrap_or_default(),
        run_id.clone().unwrap_or_default(),
        prompt_id.clone().unwrap_or_default(),
        relative_path,
        headings.join(" "),
        preview,
        detail_text
    ));

    let sort_ts = pick_sort_ts(
        completed_at.as_deref(),
        updated_at.as_deref(),
        file_modified(primary_path),
    );

    Ok(Some(ReportsExplorerRecord {
        id: 0,
        key: build_record_key(
            run_id.as_deref(),
            &absolute_path,
            &relative_path,
            meta_path_string.as_deref(),
        ),
        org: config.org.clone(),
        repo: config.repo.clone(),
        workflow,
        lane,
        record_kind: if path_contains_segment(&path_parts, "plans") {
            "plan".to_string()
        } else {
            "report".to_string()
        },
        status,
        agent,
        skill_code,
        mode,
        run_id,
        prompt_id,
        session_id,
        date_bucket,
        date_iso,
        title,
        file_name: primary_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact")
            .to_string(),
        relative_path,
        absolute_path,
        meta_path: meta_path_string,
        transcript_path: transcript_path
            .as_ref()
            .map(|path| path.display().to_string()),
        input_path: meta.as_ref().and_then(|item| item.input.clone()),
        launcher_path: meta.as_ref().and_then(|item| item.launcher.clone()),
        updated_at,
        completed_at,
        duration_s,
        loop_nr,
        headings,
        preview,
        detail_text,
        search_blob,
        has_markdown: candidate.md_path.is_some(),
        has_meta: candidate.meta_path.is_some(),
        has_transcript,
        sort_ts,
    }))
}
