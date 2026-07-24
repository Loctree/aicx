//! Cleanup orchestration: automated and interactive fix selection,
//! dry-run previews, and apply phases built on top of `checks::run_at`.

use anyhow::{Context, Result, bail};
use std::io::IsTerminal;
use std::path::Path;
use std::time::{Duration, Instant};

use super::checks::{run_at, scan_corpus_buckets};
use super::quarantine::{
    empty_body_quarantine_root, empty_body_quarantine_timestamp, empty_body_report,
};
use super::report::format_report_text;
use super::types::{
    DoctorApplyPhase, DoctorCleanupRunReport, DoctorDryRunPreview, DoctorFixChoice, DoctorFixId,
    DoctorOptions, DoctorReport, Severity,
};

pub async fn run_automated_cleanup_at(
    base: &Path,
    force: bool,
    verbose: bool,
    smoke: bool,
    progress: bool,
) -> Result<DoctorCleanupRunReport> {
    let initial = run_at(base, &base_doctor_options(verbose, smoke)).await?;
    let selected = actionable_fixes(&initial)
        .into_iter()
        .map(|choice| choice.id)
        .collect::<Vec<_>>();
    run_cleanup_actions(
        base,
        selected,
        force,
        progress,
        verbose,
        smoke,
        if force { "force" } else { "yes" },
    )
    .await
}

pub async fn run_interactive_cleanup_at(
    base: &Path,
    verbose: bool,
    smoke: bool,
) -> Result<DoctorCleanupRunReport> {
    let initial = run_at(base, &base_doctor_options(verbose, smoke)).await?;
    let choices = actionable_fixes(&initial);
    if choices.is_empty() {
        return Ok(DoctorCleanupRunReport {
            mode: "interactive".to_string(),
            selected: Vec::new(),
            dry_run: Vec::new(),
            applied: Vec::new(),
            final_report: initial,
        });
    }

    if !std::io::stdin().is_terminal() {
        bail!("interactive doctor requires a TTY; pass --yes or --format json for automation");
    }

    let defaults = (0..choices.len()).collect::<Vec<_>>();
    let selected = inquire::MultiSelect::new("aicx doctor fixes", choices.clone())
        .with_default(&defaults)
        .with_page_size(8)
        .with_help_message(
            "Space toggles, Enter confirms. No data is deleted; cleanup moves files to quarantine.",
        )
        .prompt()
        .context("doctor selection cancelled")?;
    let selected_ids = selected.iter().map(|choice| choice.id).collect::<Vec<_>>();
    let dry_run = selected_ids
        .iter()
        .copied()
        .map(|fix| dry_run_preview(base, fix))
        .collect::<Result<Vec<_>>>()?;

    if selected_ids.is_empty() {
        let final_report = run_at(base, &base_doctor_options(verbose, smoke)).await?;
        return Ok(DoctorCleanupRunReport {
            mode: "interactive".to_string(),
            selected: Vec::new(),
            dry_run,
            applied: Vec::new(),
            final_report,
        });
    }

    eprintln!("{}", format_dry_run_previews(&dry_run));
    eprintln!(
        "Dry run finished. No filesystem changes made.\nNo data will be lost. File movements are reversible from ~/.aicx/quarantine/ via `aicx doctor --restore-quarantine <slug>`."
    );

    let confirm_defaults = (0..selected.len()).collect::<Vec<_>>();
    let confirmed = inquire::MultiSelect::new("Apply cleanup", selected)
        .with_default(&confirm_defaults)
        .with_page_size(8)
        .with_help_message(
            "Uncheck anything to skip it, Enter applies selected cleanup, Esc cancels.",
        )
        .prompt()
        .context("doctor apply confirmation cancelled")?;

    run_cleanup_actions(
        base,
        confirmed.iter().map(|choice| choice.id).collect(),
        true,
        true,
        verbose,
        smoke,
        "interactive",
    )
    .await
    .map(|mut report| {
        report.dry_run = dry_run;
        report
    })
}

pub fn format_cleanup_run_text(report: &DoctorCleanupRunReport) -> String {
    let mut out = String::new();
    if report.selected.is_empty() {
        out.push_str("aicx doctor: no actionable cleanup findings.\n");
        out.push_str(&format_report_text(&report.final_report, false));
        return out;
    }
    if !report.dry_run.is_empty() {
        out.push_str(&format_dry_run_previews(&report.dry_run));
        out.push('\n');
    }
    out.push_str("Apply complete.\n\n");
    for phase in &report.applied {
        out.push_str(&format!(
            "  {} {}: {} ({:.2}s)\n",
            if phase.status == "ok" { "OK" } else { "FAIL" },
            phase.title,
            phase.detail,
            phase.elapsed_ms as f64 / 1000.0
        ));
    }
    out.push_str("\nVerify quarantine:\n  ls ~/.aicx/quarantine/\n\n");
    out.push_str("Restore if needed:\n  aicx doctor --restore-quarantine <slug>\n");
    out
}

pub(crate) fn base_doctor_options(verbose: bool, smoke: bool) -> DoctorOptions {
    DoctorOptions {
        rebuild_steer_index: false,
        fix_buckets: false,
        dry_run: false,
        rebuild_sidecars: false,
        prune_empty_bodies: false,
        apply_prune_empty_bodies: false,
        migrate_identities: false,
        apply_migrate_identities: false,
        check_dedup: false,
        verbose,
        smoke,
    }
}

pub(crate) fn actionable_fixes(report: &DoctorReport) -> Vec<DoctorFixChoice> {
    let mut choices = Vec::new();
    if matches!(
        report.steer_lance.severity,
        Severity::Critical | Severity::Warning
    ) || matches!(
        report.steer_bm25.severity,
        Severity::Critical | Severity::Warning
    ) || matches!(report.index_freshness.severity, Severity::Critical)
    {
        choices.push(DoctorFixChoice {
            id: DoctorFixId::RebuildSteerIndex,
            title: DoctorFixId::RebuildSteerIndex.title().to_string(),
            detail: "derived Lance/BM25 metadata, rebuilt from canonical chunks".to_string(),
        });
    }
    if matches!(
        report.corpus_buckets.severity,
        Severity::Critical | Severity::Warning
    ) {
        choices.push(DoctorFixChoice {
            id: DoctorFixId::QuarantineBuckets,
            title: DoctorFixId::QuarantineBuckets.title().to_string(),
            detail: "recoverable move of suspicious bucket paths".to_string(),
        });
    }
    if matches!(
        report.empty_body_chunks.severity,
        Severity::Critical | Severity::Warning
    ) {
        choices.push(DoctorFixChoice {
            id: DoctorFixId::QuarantineEmptyBodies,
            title: DoctorFixId::QuarantineEmptyBodies.title().to_string(),
            detail: "recoverable move to empty-body quarantine".to_string(),
        });
    }
    choices
}

pub(crate) async fn run_cleanup_actions(
    base: &Path,
    selected: Vec<DoctorFixId>,
    skip_dry_run: bool,
    progress: bool,
    verbose: bool,
    smoke: bool,
    mode: &str,
) -> Result<DoctorCleanupRunReport> {
    let dry_run = if skip_dry_run {
        Vec::new()
    } else {
        selected
            .iter()
            .copied()
            .map(|fix| dry_run_preview(base, fix))
            .collect::<Result<Vec<_>>>()?
    };
    let mut applied = Vec::new();
    for fix in &selected {
        let started = Instant::now();
        let bar = if progress {
            let bar = indicatif::ProgressBar::new_spinner();
            bar.enable_steady_tick(Duration::from_millis(120));
            bar.set_message(fix.title());
            Some(bar)
        } else {
            None
        };
        let result = apply_cleanup_action(base, *fix, verbose, smoke).await;
        if let Some(bar) = bar {
            match &result {
                Ok(_) => bar.finish_with_message(format!("{} done", fix.title())),
                Err(err) => bar.finish_with_message(format!("{} failed: {err}", fix.title())),
            }
            if result.is_ok() {
                eprintln!();
            }
        }
        match result {
            Ok(detail) => applied.push(DoctorApplyPhase {
                fix: *fix,
                title: fix.title().to_string(),
                status: "ok".to_string(),
                detail,
                elapsed_ms: started.elapsed().as_millis(),
            }),
            Err(err) => applied.push(DoctorApplyPhase {
                fix: *fix,
                title: fix.title().to_string(),
                status: "failed".to_string(),
                detail: err.to_string(),
                elapsed_ms: started.elapsed().as_millis(),
            }),
        }
    }
    let final_report = run_at(base, &base_doctor_options(verbose, smoke)).await?;
    Ok(DoctorCleanupRunReport {
        mode: mode.to_string(),
        selected,
        dry_run,
        applied,
        final_report,
    })
}

pub(crate) async fn apply_cleanup_action(
    base: &Path,
    fix: DoctorFixId,
    verbose: bool,
    smoke: bool,
) -> Result<String> {
    let opts = match fix {
        DoctorFixId::RebuildSteerIndex => DoctorOptions {
            rebuild_steer_index: true,
            ..base_doctor_options(verbose, smoke)
        },
        DoctorFixId::QuarantineBuckets => DoctorOptions {
            fix_buckets: true,
            ..base_doctor_options(verbose, smoke)
        },
        DoctorFixId::QuarantineEmptyBodies => DoctorOptions {
            prune_empty_bodies: true,
            apply_prune_empty_bodies: true,
            ..base_doctor_options(verbose, smoke)
        },
    };
    let report = run_at(base, &opts).await?;
    Ok(if report.fixes_applied.is_empty() {
        "no changes needed".to_string()
    } else {
        report.fixes_applied.join("; ")
    })
}

pub(crate) fn dry_run_preview(base: &Path, fix: DoctorFixId) -> Result<DoctorDryRunPreview> {
    let summary = match fix {
        DoctorFixId::RebuildSteerIndex => {
            let candidates = ["steer_db", "steer_bm25", "steer_index_meta.json"]
                .iter()
                .map(|name| base.join(name))
                .filter(|path| path.exists())
                .map(|path| format!("Would remove derived path: {}", path.display()))
                .collect::<Vec<_>>();
            let mut summary = if candidates.is_empty() {
                vec!["No existing steer index paths found; rebuild will materialize from canonical chunks.".to_string()]
            } else {
                candidates
            };
            summary
                .push("Would rebuild Lance/BM25 steer metadata from canonical store.".to_string());
            summary
        }
        DoctorFixId::QuarantineBuckets => {
            let suspicious = scan_corpus_buckets(&base.join("store"))?;
            if suspicious.is_empty() {
                vec!["No suspicious corpus buckets found.".to_string()]
            } else {
                suspicious
                    .iter()
                    .take(20)
                    .map(|bucket| format!("Would move bucket to quarantine: {bucket}"))
                    .collect()
            }
        }
        DoctorFixId::QuarantineEmptyBodies => {
            let report = empty_body_report(base);
            let timestamp = empty_body_quarantine_timestamp();
            let mut summary = vec![
                format!("Would move {} empty-body chunk(s).", report.empty),
                format!(
                    "Would write quarantine manifest under {}",
                    empty_body_quarantine_root(base, &timestamp).display()
                ),
            ];
            for (kind, count) in report.by_frame_kind.iter().take(8) {
                summary.push(format!("frame_kind {kind}: {count}"));
            }
            summary
        }
    };
    Ok(DoctorDryRunPreview {
        fix,
        title: fix.title().to_string(),
        summary,
    })
}

pub(crate) fn format_dry_run_previews(previews: &[DoctorDryRunPreview]) -> String {
    let mut out = format!(
        "You picked {} fix(es). Running dry-run preview:\n",
        previews.len()
    );
    for (idx, preview) in previews.iter().enumerate() {
        out.push_str(&format!(
            "\n  [{}/{}] {}\n",
            idx + 1,
            previews.len(),
            preview.title
        ));
        for line in &preview.summary {
            out.push_str(&format!("    {line}\n"));
        }
    }
    out.push_str("\nDry run finished. No filesystem changes made.\n");
    out
}
