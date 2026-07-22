//! Diagnostic checks and the doctor run orchestrator.
//!
//! `run` is the CLI entrypoint and picks between two contracts:
//!
//! - **fast health** (`deep = false`): bounded metadata / lease / manifest
//!   reads plus sampled invariants under an explicit filesystem-call budget
//!   ([`FAST_HEALTH_FS_BUDGET`]) — never a recursive payload scan, so the
//!   first result lands well under two seconds even on a multi-gigabyte
//!   store. Facts the fast pass cannot prove within the budget are reported
//!   as `Unknown` together with the exact deep command; unknown is never
//!   upgraded to healthy.
//! - **deep forensics** (`deep = true`): every integrity check plus the
//!   requested remediations, wrapped in progress phases with heartbeats,
//!   an explicit estimated scope, and cooperative cancellation.
//!
//! `run_at` keeps the historical silent-deep contract for in-crate callers
//! (cleanup orchestration, the API boundary, and tests).

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use crate::progress::{Heartbeat, NoopReporter, Phase, Reporter};
use crate::sanitize;
use crate::steer_index;
use crate::store;
use crate::store::canonical_projection::StageInventoryEntry;
use crate::validation::{is_valid_repo_bucket_name, is_valid_repo_project_slug};

use super::quarantine::{
    apply_empty_body_quarantine, empty_body_report, quarantine_bucket,
    quarantine_bucket_with_timestamp, render_prune_empty_bodies_script,
    render_rebuild_sidecars_script,
};
use super::report::max_severity;
use super::types::{CheckResult, DoctorOptions, DoctorReport, Severity, doctor_home_label};

/// CLI doctor/health entrypoint (only `main.rs` calls this).
///
/// `deep = false` runs the bounded fast health pass; `deep = true` runs the
/// full forensic pass with progress reported through `reporter` and
/// cooperative cancellation via `cancel`. Returns `Ok(None)` when the run
/// was cancelled — every completed phase remains valid, nothing was left in
/// an unrecoverable state (deep checks are read-only; remediations are
/// recoverable quarantine moves that either completed or did not start).
pub async fn run(
    base_override: Option<&Path>,
    opts: &DoctorOptions,
    deep: bool,
    reporter: Arc<dyn Reporter>,
    cancel: Arc<AtomicBool>,
) -> Result<Option<DoctorReport>> {
    let base = match base_override {
        Some(base) => base.to_path_buf(),
        None => store::store_base_dir().context("Failed to resolve aicx store base directory")?,
    };
    if deep {
        run_deep_impl(&base, opts, reporter, cancel).await
    } else {
        let mut budget = FsBudget::new(FAST_HEALTH_FS_BUDGET);
        Ok(Some(run_fast_impl(&base, opts, &mut budget).await))
    }
}

/// Historical silent-deep contract: full checks, no progress surface, not
/// cancellable. Kept stable for cleanup orchestration, `api.rs`, and tests.
pub async fn run_at(base: &Path, opts: &DoctorOptions) -> Result<DoctorReport> {
    let report = run_deep_impl(
        base,
        opts,
        Arc::new(NoopReporter),
        Arc::new(AtomicBool::new(false)),
    )
    .await?;
    Ok(report.expect("doctor run without a cancel source cannot be cancelled"))
}

/// Poll-based cancellable wrapper for a blocking, read-only check. The check
/// runs on a helper thread; on cancellation the thread is abandoned (it
/// finishes in the background — checks never write, so abandonment cannot
/// corrupt state) and `None` is returned immediately.
fn run_check_cancellable<T: Send + 'static>(
    cancel: &AtomicBool,
    f: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let handle = std::thread::spawn(f);
    loop {
        if handle.is_finished() {
            return match handle.join() {
                Ok(value) => Some(value),
                Err(panic) => std::panic::resume_unwind(panic),
            };
        }
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Bounded estimate of canonical-chunk scope from `index.json` alone (one
/// file read, no store traversal). `None` when the manifest is missing or
/// unreadable — callers must report "unknown", never guess.
fn index_tuple_count(base: &Path) -> Option<usize> {
    let raw = sanitize::read_to_string_validated(&base.join("index.json")).ok()?;
    let index = serde_json::from_str::<store::StoreIndex>(&raw).ok()?;
    let mut tuples = 0usize;
    for (_, project_index) in index.projects {
        for (_, agent_index) in project_index.agents {
            tuples += agent_index.dates.len();
        }
    }
    Some(tuples)
}

async fn run_deep_impl(
    base: &Path,
    opts: &DoctorOptions,
    reporter: Arc<dyn Reporter>,
    cancel: Arc<AtomicBool>,
) -> Result<Option<DoctorReport>> {
    let estimated_tuples = index_tuple_count(base);

    // Local shorthand: run one blocking check inside the current phase or
    // bail out of the whole deep run on cancellation, closing the phase and
    // stopping its heartbeat first.
    macro_rules! check {
        ($phase:expr, $hb:expr, $f:expr) => {
            match run_check_cancellable(cancel.as_ref(), $f) {
                Some(value) => value,
                None => {
                    $hb.stop();
                    $phase.finish_err("cancelled by operator", None);
                    return Ok(None);
                }
            }
        };
    }

    // ---- doctor_quick: bounded checks (no store traversal) ----
    let phase = Phase::start(reporter.clone(), "doctor_quick", Some(9));
    let hb = Heartbeat::spawn_with_backoff(
        phase.clone(),
        Duration::from_millis(500),
        Duration::from_secs(5),
    );
    let aicx_home = check_aicx_home(base);
    phase.tick(1);
    let mut state = {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_state(&base))
    };
    phase.tick(2);
    if cancel.load(Ordering::Relaxed) {
        hb.stop();
        phase.finish_err("cancelled by operator", None);
        return Ok(None);
    }
    let mut steer_lance = check_steer_lance(base).await;
    phase.tick(3);
    let mut steer_bm25 = {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_steer_bm25(&base))
    };
    phase.tick(4);
    let mut semantic_health = {
        let opts = opts.clone();
        check!(phase, hb, move || check_semantic_health(&opts))
    };
    phase.tick(5);
    let mut embedder_warmth = {
        let opts = opts.clone();
        check!(phase, hb, move || check_embedder_warmth(&opts))
    };
    phase.tick(6);
    let mut corpus_buckets = {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_corpus_buckets(&base))
    };
    phase.tick(7);
    let binary_pair = check!(phase, hb, check_binary_pair);
    phase.tick(8);
    let http_auth_token = check_http_auth_token();
    phase.tick(9);
    hb.stop();
    phase.finish_ok("bounded checks complete");

    // ---- doctor_store_scan: recursive canonical-store inventory ----
    let scope_summary = match estimated_tuples {
        Some(tuples) => format!("estimated scope: {tuples} chunk tuple(s) per index.json"),
        None => "estimated scope: unknown (no readable index.json)".to_string(),
    };
    let phase = Phase::start(
        reporter.clone(),
        "doctor_store_scan",
        estimated_tuples.map(|t| t as u64),
    );
    let hb = Heartbeat::spawn_with_backoff(
        phase.clone(),
        Duration::from_millis(500),
        Duration::from_secs(10),
    );
    let mut canonical_store = {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_canonical_store(&base))
    };
    hb.stop();
    phase.finish_ok(format!("{}; {scope_summary}", canonical_store.detail));

    // ---- doctor_index_scan: semantic index freshness + consistency ----
    let phase = Phase::start(reporter.clone(), "doctor_index_scan", Some(2));
    let hb = Heartbeat::spawn_with_backoff(
        phase.clone(),
        Duration::from_millis(500),
        Duration::from_secs(10),
    );
    let mut index_freshness = {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_index_freshness(&base))
    };
    phase.tick(1);
    let mut index_consistency = {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_index_consistency(&base))
    };
    phase.tick(2);
    hb.stop();
    phase.finish_ok("index scans complete");

    // ---- doctor_content_scan: sidecars, noise, payload-level checks ----
    let phase = Phase::start(reporter.clone(), "doctor_content_scan", Some(5));
    let hb = Heartbeat::spawn_with_backoff(
        phase.clone(),
        Duration::from_millis(500),
        Duration::from_secs(10),
    );
    let mut sidecars = {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_sidecar_coverage(&base))
    };
    phase.tick(1);
    let mut noise_health = {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_noise_health(&base))
    };
    phase.tick(2);
    let mut empty_body_chunks = {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_empty_body_chunks(&base))
    };
    phase.tick(3);
    let mut content_dedup = if opts.check_dedup {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_content_dedup(&base))
    } else {
        CheckResult {
            name: "content_dedup".to_string(),
            severity: Severity::Green,
            detail: "not requested".to_string(),
            recommendation: None,
        }
    };
    phase.tick(4);
    let mut context_corpus = {
        let base = base.to_path_buf();
        check!(phase, hb, move || check_context_corpus(&base))
    };
    phase.tick(5);
    hb.stop();
    phase.finish_ok("content scans complete");
    let mut fixes_applied = Vec::new();
    let wants_fixes = opts.rebuild_steer_index
        || opts.fix_buckets
        || opts.migrate_identities
        || (opts.prune_empty_bodies && opts.apply_prune_empty_bodies);
    let mut fix_phase = if wants_fixes {
        let phase = Phase::start(reporter.clone(), "doctor_fix", None);
        let hb = Heartbeat::spawn_with_backoff(
            phase.clone(),
            Duration::from_millis(500),
            Duration::from_secs(10),
        );
        Some((phase, hb))
    } else {
        None
    };
    // Cooperative cancel point between remediation blocks: each block is a
    // recoverable quarantine/rebuild that either completed or did not start.
    macro_rules! fix_cancel_point {
        () => {
            if cancel.load(Ordering::Relaxed) {
                if let Some((phase, hb)) = fix_phase.take() {
                    hb.stop();
                    phase.finish_err("cancelled by operator", None);
                }
                return Ok(None);
            }
        };
    }
    fix_cancel_point!();
    let rebuild_sidecars_script = if opts.rebuild_sidecars {
        Some(render_rebuild_sidecars_script(base)?)
    } else {
        None
    };
    let prune_empty_bodies_script = if opts.prune_empty_bodies && !opts.apply_prune_empty_bodies {
        Some(render_prune_empty_bodies_script(base)?)
    } else {
        None
    };
    let apply_empty_bodies = opts.prune_empty_bodies && opts.apply_prune_empty_bodies;

    if opts.rebuild_steer_index
        && (steer_lance.severity == Severity::Critical || steer_bm25.severity == Severity::Critical)
    {
        match attempt_steer_rebuild(base).await {
            Ok(msg) => fixes_applied.push(msg),
            Err(e) => fixes_applied.push(format!("rebuild attempted but failed: {e}")),
        }
    }

    if opts.fix_buckets {
        let store_root = base.join("store");
        let dry = opts.dry_run;
        let prefix = if dry { "[dry-run] " } else { "" };
        match scan_corpus_buckets(&store_root) {
            Ok(suspicious) if suspicious.is_empty() => {
                fixes_applied.push("no suspicious corpus buckets to quarantine".to_string());
            }
            Ok(suspicious) => {
                let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
                let single_bucket = suspicious.len() == 1;
                for bucket_name in &suspicious {
                    if dry {
                        fixes_applied.push(format!(
                            "{prefix}would quarantine corpus bucket `{bucket_name}`"
                        ));
                        continue;
                    }
                    let result = if single_bucket {
                        quarantine_bucket(&store_root, bucket_name)
                    } else {
                        quarantine_bucket_with_timestamp(&store_root, bucket_name, &timestamp)
                    };
                    match result {
                        Ok(dst) => fixes_applied.push(format!(
                            "quarantined corpus bucket `{bucket_name}` to {}",
                            dst.display()
                        )),
                        Err(e) => fixes_applied.push(format!(
                            "failed to quarantine corpus bucket `{bucket_name}`: {e}"
                        )),
                    }
                }
            }
            Err(e) => fixes_applied.push(format!("{prefix}bucket scan skipped: {e}")),
        }

        // Same recoverable-quarantine contract for dead canonical-projection
        // stages: rename into `<base>/quarantine/projection-stages-<ts>/`
        // with a restore manifest. Live/stale/unproven owners are left in
        // place — doctor never force-kills and never deletes.
        match super::quarantine::quarantine_projection_stages_at(base, dry) {
            Ok(stage_report) => {
                if stage_report.moved.is_empty()
                    && stage_report.failures.is_empty()
                    && stage_report.left_in_place.is_empty()
                {
                    fixes_applied.push("no canonical-projection stages to quarantine".to_string());
                } else {
                    fixes_applied.extend(stage_report.moved);
                    for line in stage_report.left_in_place {
                        fixes_applied.push(format!("left projection stage in place — {line}"));
                    }
                    for line in stage_report.failures {
                        fixes_applied
                            .push(format!("failed to quarantine projection stage — {line}"));
                    }
                    if let Some(manifest_path) = stage_report.manifest_path {
                        fixes_applied.push(format!(
                            "wrote projection-stage quarantine manifest {}",
                            manifest_path.display()
                        ));
                    }
                }
            }
            Err(e) => {
                fixes_applied.push(format!("{prefix}projection-stage quarantine skipped: {e}"));
            }
        }
    }

    if opts.migrate_identities {
        let apply_identities = opts.apply_migrate_identities;
        match crate::store::migration::run_identity_migration_at(base, apply_identities) {
            Ok(outcome) => {
                let prefix = if outcome.applied { "" } else { "[dry-run] " };
                fixes_applied.push(format!(
                    "{prefix}identity migration: {} index key rename(s), {} store dir rename(s), {} card alias(es) [annotate-only], {} typo-twin pair(s) [report-only], {} conflict(s)",
                    outcome.manifest.index_key_renames.len(),
                    outcome.manifest.dir_renames.len(),
                    outcome.manifest.card_aliases.len(),
                    outcome.manifest.typo_twins.len(),
                    outcome.manifest.conflicts.len(),
                ));
                fixes_applied.push(format!(
                    "identity migration manifest: {}",
                    outcome.manifest_path.display()
                ));
                fixes_applied.push(format!(
                    "identity migration report: {}",
                    outcome.report_path.display()
                ));
                if !apply_identities {
                    fixes_applied.push(
                        "run `aicx doctor --migrate-identities --apply` to execute the planned renames"
                            .to_string(),
                    );
                }
            }
            Err(e) => fixes_applied.push(format!("identity migration skipped: {e}")),
        }
    }

    if apply_empty_bodies {
        match apply_empty_body_quarantine(base) {
            Ok(report) if report.moved_chunks == 0 && report.failures.is_empty() => {
                fixes_applied.push("no empty-body chunks to quarantine".to_string());
            }
            Ok(report) => {
                if report.moved_chunks > 0 {
                    let quarantine_root = report
                        .quarantine_root
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "unknown quarantine root".to_string());
                    fixes_applied.push(format!(
                        "quarantined {} empty-body chunk(s) and {} sidecar(s) to {}",
                        report.moved_chunks, report.moved_sidecars, quarantine_root
                    ));
                    if let Some(manifest_path) = report.manifest_path {
                        fixes_applied.push(format!(
                            "wrote quarantine manifest {}",
                            manifest_path.display()
                        ));
                    }
                }
                for failure in report.failures.iter().take(10) {
                    fixes_applied.push(format!("failed to quarantine empty-body chunk: {failure}"));
                }
                if report.failures.len() > 10 {
                    fixes_applied.push(format!(
                        "additional empty-body quarantine failures omitted: {}",
                        report.failures.len() - 10
                    ));
                }
            }
            Err(e) => fixes_applied.push(format!("empty-body quarantine skipped: {e}")),
        }
    }

    if let Some((phase, hb)) = fix_phase.take() {
        hb.stop();
        phase.finish_ok(format!("{} remediation line(s)", fixes_applied.len()));
    }

    if opts.rebuild_steer_index
        || opts.fix_buckets
        || apply_empty_bodies
        || (opts.migrate_identities && opts.apply_migrate_identities)
    {
        let phase = Phase::start(reporter.clone(), "doctor_recheck", Some(13));
        let hb = Heartbeat::spawn_with_backoff(
            phase.clone(),
            Duration::from_millis(500),
            Duration::from_secs(10),
        );
        canonical_store = {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_canonical_store(&base))
        };
        phase.tick(1);
        if cancel.load(Ordering::Relaxed) {
            hb.stop();
            phase.finish_err("cancelled by operator", None);
            return Ok(None);
        }
        steer_lance = check_steer_lance(base).await;
        phase.tick(2);
        steer_bm25 = {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_steer_bm25(&base))
        };
        phase.tick(3);
        state = {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_state(&base))
        };
        phase.tick(4);
        sidecars = {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_sidecar_coverage(&base))
        };
        phase.tick(5);
        corpus_buckets = {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_corpus_buckets(&base))
        };
        phase.tick(6);
        noise_health = {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_noise_health(&base))
        };
        phase.tick(7);
        semantic_health = {
            let opts = opts.clone();
            check!(phase, hb, move || check_semantic_health(&opts))
        };
        phase.tick(8);
        index_freshness = {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_index_freshness(&base))
        };
        phase.tick(9);
        index_consistency = {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_index_consistency(&base))
        };
        phase.tick(10);
        embedder_warmth = {
            let opts = opts.clone();
            check!(phase, hb, move || check_embedder_warmth(&opts))
        };
        phase.tick(11);
        empty_body_chunks = {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_empty_body_chunks(&base))
        };
        phase.tick(12);
        content_dedup = if opts.check_dedup {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_content_dedup(&base))
        } else {
            content_dedup
        };
        context_corpus = {
            let base = base.to_path_buf();
            check!(phase, hb, move || check_context_corpus(&base))
        };
        phase.tick(13);
        hb.stop();
        phase.finish_ok("post-fix recheck complete");
    }

    let sidecar_coverage = sidecars.clone();

    let overall = max_severity(&[
        canonical_store.severity,
        steer_lance.severity,
        steer_bm25.severity,
        state.severity,
        sidecars.severity,
        corpus_buckets.severity,
        noise_health.severity,
        semantic_health.severity,
        index_freshness.severity,
        index_consistency.severity,
        embedder_warmth.severity,
        empty_body_chunks.severity,
        content_dedup.severity,
        context_corpus.severity,
    ]);

    Ok(Some(DoctorReport {
        schema_version: 2,
        canonical_store,
        steer_lance,
        steer_bm25,
        state,
        sidecars,
        corpus_buckets,
        noise_health,
        semantic_health,
        index_freshness,
        index_consistency,
        sidecar_coverage,
        embedder_warmth,
        empty_body_chunks,
        content_dedup,
        context_corpus,
        aicx_home,
        binary_pair,
        http_auth_token,
        rebuild_sidecars_script,
        prune_empty_bodies_script,
        fixes_applied,
        overall,
    }))
}

/// Filesystem-call budget for the fast health pass. Every directory listing,
/// metadata probe, and bounded file read charges the budget; once exhausted
/// the fast pass stops touching the filesystem and reports what it could not
/// prove as `Unknown`. The cap is sized so a worst-case fast run stays well
/// under the two-second latency bound on cold caches.
pub(crate) const FAST_HEALTH_FS_BUDGET: usize = 8192;

/// Sidecar sample ceiling for the fast pass when the bounded walk could not
/// finish the whole store within budget.
const FAST_SIDECAR_SAMPLE: usize = 64;

/// Instrumented filesystem-call budget. `charge` returns `false` once the
/// budget is exhausted so callers can stop traversing instead of scanning on.
pub(crate) struct FsBudget {
    used: usize,
    limit: usize,
}

impl FsBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self { used: 0, limit }
    }

    fn charge(&mut self, n: usize) -> bool {
        self.used = self.used.saturating_add(n);
        self.used <= self.limit
    }

    fn exhausted(&self) -> bool {
        self.used > self.limit
    }

    #[cfg(test)]
    pub(crate) fn used(&self) -> usize {
        self.used
    }
}

/// Outcome of the bounded store walk: chunk files found so far and whether
/// the walk covered the whole tree before hitting the budget.
struct BoundedWalk {
    chunk_files: Vec<PathBuf>,
    complete: bool,
}

/// Budgeted, depth-capped walk over the canonical store collecting chunk
/// files (`*.md`). Dot-prefixed directories (projection stages, hidden
/// scratch) are skipped — stage inventory has its own bounded reader. The
/// walk stops the moment the budget is exhausted and reports `complete =
/// false`; it never degrades into the recursive full scan.
fn bounded_store_walk(root: &Path, budget: &mut FsBudget) -> BoundedWalk {
    const MAX_DEPTH: usize = 8;
    let mut chunk_files = Vec::new();
    if !budget.charge(1) || !root.is_dir() {
        return BoundedWalk {
            chunk_files,
            complete: !budget.exhausted(),
        };
    }
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if !budget.charge(1) {
            return BoundedWalk {
                chunk_files,
                complete: false,
            };
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !budget.charge(1) {
                return BoundedWalk {
                    chunk_files,
                    complete: false,
                };
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let path = entry.path();
            let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            if is_dir {
                if name.starts_with('.') {
                    continue;
                }
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if path.extension().is_some_and(|ext| ext == "md") {
                chunk_files.push(path);
            }
        }
    }
    BoundedWalk {
        chunk_files,
        complete: true,
    }
}

/// Fast-pass placeholder for a check whose truth requires a recursive scan.
/// Unknown is a hard floor: fast health never upgrades it to healthy, and
/// the recommendation carries the exact deep command.
fn unmeasured_in_fast(name: &str, what: &str) -> CheckResult {
    CheckResult {
        name: name.to_string(),
        severity: Severity::Unknown,
        detail: format!("{what} not measured by fast health (bounded run)"),
        recommendation: Some("Run `aicx doctor --deep` for the full recursive scan".to_string()),
    }
}

/// Bounded canonical-store check: projection-stage lease inventory (metadata
/// only) plus a chunk inventory that is either proven by a completed bounded
/// walk or taken from `index.json`; when neither source answers, the check
/// reports unknown with the deep command instead of guessing.
fn check_canonical_store_fast(
    base: &Path,
    walk: &BoundedWalk,
    budget: &mut FsBudget,
) -> CheckResult {
    let store_root = base.join("store");
    budget.charge(1);
    if !store_root.exists() {
        return CheckResult {
            name: "canonical_store".to_string(),
            severity: Severity::Warning,
            detail: format!("Canonical store does not exist at {}", store_root.display()),
            recommendation: Some("Run `aicx store -H 168` to populate".to_string()),
        };
    }
    let stages = crate::store::canonical_projection::inspect_projection_stages_at(&store_root);
    budget.charge(1 + stages.len().saturating_mul(4));
    let (inventory, inventory_known) = if walk.complete {
        (
            format!(
                "{} chunk file(s) counted by bounded walk",
                walk.chunk_files.len()
            ),
            true,
        )
    } else {
        budget.charge(1);
        match index_tuple_count(base) {
            Some(tuples) => (
                format!(
                    "index.json lists {tuples} project/agent/date tuple(s) (payload not scanned)"
                ),
                true,
            ),
            None => (
                "chunk inventory unknown (bounded walk hit budget, index.json unreadable)"
                    .to_string(),
                false,
            ),
        }
    };
    let stage_summary = summarize_projection_stages(&stages);
    let (severity, recommendation) = match &stage_summary {
        Some(summary) => (summary.severity, summary.recommendation.clone()),
        None if inventory_known => (Severity::Green, None),
        None => (
            Severity::Unknown,
            Some("Run `aicx doctor --deep` for the full store scan".to_string()),
        ),
    };
    let detail = match &stage_summary {
        Some(summary) => format!(
            "{inventory}; {} projection stage(s): {}; sample: {}",
            summary.count, summary.breakdown, summary.sample
        ),
        None => inventory,
    };
    CheckResult {
        name: "canonical_store".to_string(),
        severity,
        detail,
        recommendation,
    }
}

/// Bounded sidecar check: full verification when the bounded walk finished
/// the store within budget, otherwise a sampled invariant. A missing sidecar
/// in the sample degrades to Warning (fail-closed upward); a clean sample
/// stays `Unknown` — a sample is not coverage proof.
fn check_sidecars_fast(walk: &BoundedWalk, budget: &mut FsBudget) -> CheckResult {
    let mut checked = 0usize;
    let mut missing = 0usize;
    let mut verification_complete = walk.complete;
    for chunk in &walk.chunk_files {
        if !walk.complete && checked >= FAST_SIDECAR_SAMPLE {
            break;
        }
        if !budget.charge(1) {
            verification_complete = false;
            break;
        }
        checked += 1;
        if !store::sidecar_path_for_chunk(chunk).exists() {
            missing += 1;
        }
    }
    if missing > 0 {
        return CheckResult {
            name: "sidecars".to_string(),
            severity: Severity::Warning,
            detail: format!(
                "{missing}/{checked} sampled chunk(s) missing sidecars (fast sample)"
            ),
            recommendation: Some(
                "Run `aicx doctor --deep` for full coverage, then `aicx store --full-rescan` to backfill"
                    .to_string(),
            ),
        };
    }
    if verification_complete && checked == walk.chunk_files.len() {
        return CheckResult {
            name: "sidecars".to_string(),
            severity: Severity::Green,
            detail: if checked == 0 {
                "no chunks to check".to_string()
            } else {
                format!("{checked}/{checked} chunks have sidecars (verified within fast budget)")
            },
            recommendation: None,
        };
    }
    CheckResult {
        name: "sidecars".to_string(),
        severity: Severity::Unknown,
        detail: if checked == 0 {
            "no chunks sampled (budget exhausted before any chunk)".to_string()
        } else {
            format!("sampled {checked} chunk(s): all sidecars present (sample, not coverage)")
        },
        recommendation: Some(
            "Run `aicx doctor --deep` for the full sidecar coverage scan".to_string(),
        ),
    }
}

/// Bounded context-corpus check: existence-level truth only; a populated
/// corpus tree requires the deep batch walk.
fn check_context_corpus_fast(base: &Path, budget: &mut FsBudget) -> CheckResult {
    let corpus_root = base.join(store::CONTEXT_CORPUS_DIRNAME);
    budget.charge(1);
    if !corpus_root.exists() {
        return CheckResult {
            name: "context_corpus".to_string(),
            severity: Severity::Green,
            detail: format!(
                "context-corpus: empty (will be created on first `aicx ingest --source loct-context-pack`) at {}",
                corpus_root.display()
            ),
            recommendation: None,
        };
    }
    unmeasured_in_fast("context_corpus", "context-corpus batch walk")
}

/// The bounded fast health pass. Reads metadata, leases, manifests, lock
/// state, and sampled invariants under `budget`; never a recursive payload
/// scan. Exposed with an explicit budget for the instrumented tests.
pub(crate) async fn run_fast_impl(
    base: &Path,
    opts: &DoctorOptions,
    budget: &mut FsBudget,
) -> DoctorReport {
    let store_root = base.join("store");
    let walk = bounded_store_walk(&store_root, budget);

    let canonical_store = check_canonical_store_fast(base, &walk, budget);
    let steer_lance = check_steer_lance(base).await;
    let steer_bm25 = check_steer_bm25(base);
    let state = check_state(base);
    let sidecars = check_sidecars_fast(&walk, budget);
    let corpus_buckets = check_corpus_buckets(base);
    let noise_health = unmeasured_in_fast("noise_health", "sidecar noise aggregation");
    let semantic_health = check_semantic_health(opts);
    let index_freshness = unmeasured_in_fast("index_freshness", "recursive store mtime scan");
    let index_consistency =
        unmeasured_in_fast("index_consistency", "index.json vs store reconciliation");
    let embedder_warmth = check_embedder_warmth(opts);
    let empty_body_chunks = unmeasured_in_fast("empty_body_chunks", "chunk payload scan");
    let content_dedup = if opts.check_dedup {
        unmeasured_in_fast("content_dedup", "content hash sweep")
    } else {
        CheckResult {
            name: "content_dedup".to_string(),
            severity: Severity::Green,
            detail: "not requested".to_string(),
            recommendation: None,
        }
    };
    let context_corpus = check_context_corpus_fast(base, budget);
    let aicx_home = check_aicx_home(base);
    let binary_pair = check_binary_pair();
    let http_auth_token = check_http_auth_token();

    let sidecar_coverage = sidecars.clone();
    let overall = max_severity(&[
        canonical_store.severity,
        steer_lance.severity,
        steer_bm25.severity,
        state.severity,
        sidecars.severity,
        corpus_buckets.severity,
        noise_health.severity,
        semantic_health.severity,
        index_freshness.severity,
        index_consistency.severity,
        embedder_warmth.severity,
        empty_body_chunks.severity,
        content_dedup.severity,
        context_corpus.severity,
    ]);

    DoctorReport {
        schema_version: 2,
        canonical_store,
        steer_lance,
        steer_bm25,
        state,
        sidecars,
        corpus_buckets,
        noise_health,
        semantic_health,
        index_freshness,
        index_consistency,
        sidecar_coverage,
        embedder_warmth,
        empty_body_chunks,
        content_dedup,
        context_corpus,
        aicx_home,
        binary_pair,
        http_auth_token,
        rebuild_sidecars_script: None,
        prune_empty_bodies_script: None,
        fixes_applied: Vec::new(),
        overall,
    }
}

/// Shared projection-stage rollup used by both the deep canonical-store
/// check and the fast variant, so their severity/wording cannot drift.
struct StageSummary {
    severity: Severity,
    count: usize,
    breakdown: String,
    sample: String,
    recommendation: Option<String>,
}

fn summarize_projection_stages(stages: &[StageInventoryEntry]) -> Option<StageSummary> {
    if stages.is_empty() {
        return None;
    }
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for stage in stages {
        *counts.entry(stage.class.as_str()).or_insert(0) += 1;
    }
    let breakdown = counts
        .iter()
        .map(|(class, count)| format!("{class}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sample = stages
        .iter()
        .take(3)
        .map(|stage| {
            format!(
                "{} [{}: {}]",
                stage.path.display(),
                stage.class.as_str(),
                stage.reason
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let quarantine_eligible = stages
        .iter()
        .filter(|stage| stage.class.quarantine_eligible())
        .count();
    let unproven = stages.iter().any(|stage| {
        matches!(
            stage.class,
            crate::store::canonical_projection::StageClass::Stale
                | crate::store::canonical_projection::StageClass::UnknownOwner
        )
    });

    let severity = if quarantine_eligible > 0 || unproven {
        Severity::Warning
    } else {
        Severity::Green
    };
    let recommendation = if quarantine_eligible > 0 {
        Some(
            "Dead/drifted canonical-projection stage(s) hold unpromoted payload. Run `aicx doctor --fix-buckets --dry-run` to preview, then `aicx doctor --fix-buckets` for a recoverable quarantine move; final deletion stays with the operator via the quarantine manifest."
                .to_string(),
        )
    } else if unproven {
        Some(
            "Projection stage ownership is stale or cannot be proven; the stage is left in place. Inspect the stage lease (stage.json) manually before acting."
                .to_string(),
        )
    } else {
        None
    };

    Some(StageSummary {
        severity,
        count: stages.len(),
        breakdown,
        sample,
        recommendation,
    })
}

/// Informational: report the resolved AICX_HOME, whether it is pinned via the
/// environment, and whether the canonical store / semantic index live under
/// it. Diagnostic only — `canonical_store` owns the health gate; this exists so
/// an operator can see *where* aicx is looking without spelunking.
pub(crate) fn check_aicx_home(base: &Path) -> CheckResult {
    let env_pin = std::env::var_os("AICX_HOME").filter(|value| !value.is_empty());
    let resolved = base.display().to_string();
    let store_present = base.join("store").exists();
    let indexed_present = base.join("indexed").exists();
    let source = match &env_pin {
        Some(value) => format!("pinned via AICX_HOME={}", PathBuf::from(value).display()),
        None => "default (bootstrap config or ~/.aicx)".to_string(),
    };
    let detail = format!(
        "resolved home: {resolved} [{source}]; store/ {}, indexed/ {}",
        if store_present { "present" } else { "missing" },
        if indexed_present {
            "present"
        } else {
            "missing"
        },
    );
    let (severity, recommendation) = if store_present {
        (Severity::Green, None)
    } else {
        (
            Severity::Warning,
            Some(format!(
                "No canonical store under {resolved}. If your corpus lives elsewhere, set AICX_HOME to that path before running aicx (default is ~/.aicx)."
            )),
        )
    };
    CheckResult {
        name: "aicx_home".to_string(),
        severity,
        detail,
        recommendation,
    }
}

/// Informational: compare the running aicx CLI version against the aicx-mcp
/// companion resolved on PATH. Surfaces the "fresh CLI, stale MCP" drift class
/// where a long-running MCP service answers health checks while serving older
/// search behavior. Diagnostic only — not part of `overall`.
pub(crate) fn check_binary_pair() -> CheckResult {
    let cli_version = crate::BUILD_VERSION;
    let probed = std::process::Command::new("aicx-mcp")
        .arg("--version")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    match probed {
        None => CheckResult {
            name: "binary_pair".to_string(),
            severity: Severity::Warning,
            detail: format!("aicx CLI {cli_version}; aicx-mcp not found on PATH"),
            recommendation: Some(
                "Install the matching aicx-mcp so `aicx serve` MCP behavior tracks the CLI."
                    .to_string(),
            ),
        },
        Some(version_line) => {
            // `aicx-mcp --version` prints "aicx-mcp <semver>"; pick the first
            // token that starts with a digit so a trailing build suffix
            // ("0.9.4 (abc)") or a different binary-name prefix cannot corrupt
            // the comparison.
            let mcp_version = version_line
                .split_whitespace()
                .find(|token| token.chars().next().is_some_and(|c| c.is_ascii_digit()))
                .unwrap_or(version_line.as_str());
            if mcp_version == cli_version {
                CheckResult {
                    name: "binary_pair".to_string(),
                    severity: Severity::Green,
                    detail: format!(
                        "aicx CLI {cli_version} matches aicx-mcp {mcp_version} on PATH"
                    ),
                    recommendation: None,
                }
            } else {
                CheckResult {
                    name: "binary_pair".to_string(),
                    severity: Severity::Warning,
                    detail: format!(
                        "version drift: aicx CLI {cli_version} vs aicx-mcp {mcp_version} on PATH"
                    ),
                    recommendation: Some(
                        "Reinstall so both binaries match; a stale aicx-mcp service can serve old search behavior while looking healthy."
                            .to_string(),
                    ),
                }
            }
        }
    }
}

/// Informational: report where the HTTP auth token resolves from, without
/// reading the token value or generating one. Diagnostic only.
pub(crate) fn check_http_auth_token() -> CheckResult {
    let probe = crate::auth::probe_token_source();
    CheckResult {
        name: "http_auth_token".to_string(),
        severity: Severity::Green,
        detail: format!("HTTP auth token source: {}", probe.describe()),
        recommendation: None,
    }
}

pub(crate) fn check_context_corpus(base: &Path) -> CheckResult {
    let corpus_root = base.join(store::CONTEXT_CORPUS_DIRNAME);
    if !corpus_root.exists() {
        return CheckResult {
            name: "context_corpus".to_string(),
            severity: Severity::Green,
            detail: format!(
                "context-corpus: empty (will be created on first `aicx ingest --source loct-context-pack`) at {}",
                corpus_root.display()
            ),
            recommendation: None,
        };
    }
    let files = match store::scan_context_corpus_files_at(base) {
        Ok(files) => files,
        Err(err) => {
            return CheckResult {
                name: "context_corpus".to_string(),
                severity: Severity::Warning,
                detail: format!(
                    "context-corpus: scan failed at {}: {err}",
                    corpus_root.display()
                ),
                recommendation: Some(format!(
                    "Inspect {}/context-corpus/ for permission or filesystem issues",
                    doctor_home_label()
                )),
            };
        }
    };
    if files.is_empty() {
        return CheckResult {
            name: "context_corpus".to_string(),
            severity: Severity::Green,
            detail: format!(
                "context-corpus: empty (no batches yet; tree exists at {})",
                corpus_root.display()
            ),
            recommendation: None,
        };
    }
    let mut batches: BTreeSet<PathBuf> = BTreeSet::new();
    let mut repos: BTreeSet<PathBuf> = BTreeSet::new();
    for file in &files {
        if let Some(batch_dir) = file.raw_path.parent().and_then(|p| p.parent()) {
            batches.insert(batch_dir.to_path_buf());
            // batch_dir layout: <repo>/<date>/loct-context-pack/<batch>
            // strip <date>/loct-context-pack/<batch> to find <org>/<repo>
            if let Some(repo_dir) = batch_dir.ancestors().nth(3).map(|p| p.to_path_buf())
                && repo_dir != corpus_root
                && repo_dir.starts_with(&corpus_root)
            {
                repos.insert(repo_dir);
            }
        }
    }
    CheckResult {
        name: "context_corpus".to_string(),
        severity: Severity::Green,
        detail: format!(
            "context-corpus: {} chunks across {} batch(es) / {} repo(s) at {}",
            files.len(),
            batches.len(),
            repos.len(),
            corpus_root.display()
        ),
        recommendation: None,
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub(crate) fn check_semantic_health(opts: &DoctorOptions) -> CheckResult {
    let cfg = crate::embedder::EmbeddingConfig::from_env();
    match cfg.backend {
        crate::embedder::BackendPreference::Cloud => {
            let Some(cloud) = cfg.cloud.as_ref() else {
                return CheckResult {
                    name: "semantic_health".to_string(),
                    severity: Severity::NotConfigured,
                    detail: "cloud backend selected but [embedder.cloud] is not configured"
                        .to_string(),
                    recommendation: Some(
                        "Run `aicx config init` and set provider details".to_string(),
                    ),
                };
            };
            if let Some(env_name) = cloud.api_key_env.as_deref()
                && std::env::var(env_name).is_err()
            {
                return CheckResult {
                    name: "semantic_health".to_string(),
                    severity: Severity::Warning,
                    detail: format!(
                        "cloud backend configured for {}, but ${env_name} is unset",
                        cloud.url
                    ),
                    recommendation: Some(format!("export {env_name}=<provider-api-key>")),
                };
            }
            if !opts.smoke {
                return CheckResult {
                    name: "semantic_health".to_string(),
                    severity: Severity::Green,
                    detail: format!(
                        "cloud backend configured for {} (pass --smoke for real HTTP probe)",
                        cloud.url
                    ),
                    recommendation: None,
                };
            }
            match crate::embedder::cloud::probe(&cloud.url, &cloud.model) {
                Ok(detail) => CheckResult {
                    name: "semantic_health".to_string(),
                    severity: Severity::Green,
                    detail: format!(
                        "cloud backend reachable: {} ({}) - {detail}",
                        cloud.url, cloud.model
                    ),
                    recommendation: None,
                },
                Err(failure) => CheckResult {
                    name: "semantic_health".to_string(),
                    severity: Severity::Critical,
                    detail: format!(
                        "cloud backend URL probe failed for {}: {}",
                        cloud.url, failure
                    ),
                    recommendation: Some(
                        "Start the embedder service or correct [embedder.cloud].url".to_string(),
                    ),
                },
            }
        }
        crate::embedder::BackendPreference::Gguf | crate::embedder::BackendPreference::Auto => {
            let resolved = cfg.resolved_model();
            let found = cfg
                .model_path
                .as_ref()
                .filter(|path| path.exists())
                .cloned()
                .or_else(|| {
                    crate::embedder::find_cached_model_file(&resolved.repo, &resolved.filename)
                });
            if let Some(path) = found {
                match native_embedder_ping() {
                    Ok(detail) => CheckResult {
                        name: "semantic_health".to_string(),
                        severity: Severity::Green,
                        detail: format!("native model available at {}; {detail}", path.display()),
                        recommendation: None,
                    },
                    Err(err) => CheckResult {
                        name: "semantic_health".to_string(),
                        severity: Severity::Warning,
                        detail: format!(
                            "native model available at {}, but embedder info probe failed: {err}",
                            path.display()
                        ),
                        recommendation: Some(
                            "Run `aicx warmup` for a full embedder probe".to_string(),
                        ),
                    },
                }
            } else {
                CheckResult {
                    name: "semantic_health".to_string(),
                    severity: Severity::Warning,
                    detail: format!(
                        "native semantic model not found (optional capability): {}/{}",
                        resolved.repo, resolved.filename
                    ),
                    recommendation: Some(
                        "Hydrate the model cache or switch [embedder].backend = \"cloud\". Semantic features will be skipped until available."
                            .to_string(),
                    ),                }
            }
        }
        crate::embedder::BackendPreference::Candle => CheckResult {
            name: "semantic_health".to_string(),
            severity: Severity::Critical,
            detail: "legacy candle backend is not supported by this build".to_string(),
            recommendation: Some("Use backend = \"cloud\" or backend = \"gguf\"".to_string()),
        },
    }
}

#[cfg(feature = "native-embedder")]
pub(crate) fn native_embedder_ping() -> std::result::Result<String, String> {
    crate::embedder::EmbeddingEngine::new()
        .map(|engine| format!("native engine info responded: {}", engine.info().model_id))
        .map_err(|err| err.to_string())
}

#[cfg(not(feature = "native-embedder"))]
pub(crate) fn native_embedder_ping() -> std::result::Result<String, String> {
    Ok("native engine info probe skipped; native-embedder feature is disabled".to_string())
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
pub(crate) fn check_semantic_health(_opts: &DoctorOptions) -> CheckResult {
    CheckResult {
        name: "semantic_health".to_string(),
        severity: Severity::Critical,
        detail: "binary built without embedder features".to_string(),
        recommendation: Some("Rebuild with cloud-embedder or native-embedder".to_string()),
    }
}

/// Enumerate per-bucket subdirectories under `<aicx_home>/indexed/`.
///
/// Each bucket holds a `embeddings.ndjson` (atomically committed) and
/// optionally a `embeddings.ndjson.tmp` checkpoint. Buckets are either
/// `_all` (cross-project query target) or a `canonical_bucket_name`
/// derived from `<owner>/<repo>` per `api::semantic_index_path_for_bucket`.
pub(crate) fn list_indexed_buckets(indexed_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(indexed_root) else {
        return Vec::new();
    };
    let mut buckets: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter_map(|entry| entry.file_name().to_str().map(String::from))
        .collect();
    buckets.sort();
    buckets
}

/// Bug #37: doctor freshness must inspect the SEMANTIC index that
/// `aicx index` actually writes — `<aicx_home>/indexed/<bucket>/embeddings.ndjson`
/// (see `api::semantic_index_path_for_bucket`). The legacy check used
/// `steer_db` / `steer_bm25` mtimes (metadata steer indexes), so it
/// reported "fresh" while the real semantic corpus was missing or stale.
///
/// Recovery hint uses the post-A-1 canonical flag set: `aicx index` for
/// incremental refresh, `aicx index --full-rescan` for a from-zero
/// rebuild. Path interpolation flows through `doctor_home_label` so the
/// recommendation tracks `$AICX_HOME` like the other 8 doctor strings.
pub(crate) fn check_index_freshness(base: &Path) -> CheckResult {
    let indexed_root = base.join("indexed");
    let newest_chunk = newest_mtime(&base.join("store"))
        .into_iter()
        .chain(newest_mtime(&base.join("non-repository-contexts")))
        .max();

    let Some(newest_chunk) = newest_chunk else {
        return CheckResult {
            name: "index_freshness".to_string(),
            severity: Severity::Green,
            detail: "no canonical chunks found; no index lag".to_string(),
            recommendation: None,
        };
    };

    let buckets = list_indexed_buckets(&indexed_root);
    if buckets.is_empty() {
        return CheckResult {
            name: "index_freshness".to_string(),
            severity: Severity::Critical,
            detail: format!(
                "canonical chunks exist but no semantic index buckets under {}/indexed/",
                doctor_home_label()
            ),
            recommendation: Some(format!(
                "Run `aicx index` to materialize {}/indexed/<bucket>/embeddings.ndjson \
                 (use `aicx index --full-rescan` for a from-zero rebuild)",
                doctor_home_label()
            )),
        };
    }

    let mut missing: Vec<String> = Vec::new();
    let mut max_lag = Duration::ZERO;
    let mut stale_count = 0usize;
    let mut fresh_count = 0usize;

    for bucket in &buckets {
        let index_path = indexed_root.join(bucket).join("embeddings.ndjson");
        match index_path.metadata().and_then(|m| m.modified()) {
            Err(_) => missing.push(bucket.clone()),
            Ok(index_mtime) => {
                let lag = newest_chunk
                    .duration_since(index_mtime)
                    .unwrap_or(Duration::ZERO);
                if lag > Duration::ZERO {
                    stale_count += 1;
                    if lag > max_lag {
                        max_lag = lag;
                    }
                } else {
                    fresh_count += 1;
                }
            }
        }
    }

    if !missing.is_empty() {
        return CheckResult {
            name: "index_freshness".to_string(),
            severity: Severity::Critical,
            detail: format!(
                "semantic index missing for {} bucket(s): {}",
                missing.len(),
                missing.join(", ")
            ),
            recommendation: Some(format!(
                "Run `aicx index` to materialize {}/indexed/<bucket>/embeddings.ndjson \
                 (use `aicx index --full-rescan` for a from-zero rebuild)",
                doctor_home_label()
            )),
        };
    }

    if stale_count > 0 {
        let severity = if max_lag > Duration::from_secs(72 * 3600) {
            Severity::Critical
        } else {
            Severity::Warning
        };
        return CheckResult {
            name: "index_freshness".to_string(),
            severity,
            detail: format!(
                "semantic index stale in {stale_count} bucket(s); max lag {} seconds",
                max_lag.as_secs()
            ),
            recommendation: Some(format!(
                "Run `aicx index` to refresh {}/indexed/<bucket>/embeddings.ndjson \
                 (use `aicx index --full-rescan` to rebuild from zero)",
                doctor_home_label()
            )),
        };
    }

    CheckResult {
        name: "index_freshness".to_string(),
        severity: Severity::Green,
        detail: format!("semantic index fresh across {fresh_count} bucket(s)"),
        recommendation: None,
    }
}

pub(crate) fn check_index_consistency(base: &Path) -> CheckResult {
    let files = store::scan_context_files_at(base).unwrap_or_default();
    let chunk_keys = files
        .iter()
        .map(|file| {
            (
                file.project.clone(),
                file.agent.clone(),
                file.date_compact.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let index_path = base.join("index.json");
    if !index_path.exists() {
        return CheckResult {
            name: "index_consistency".to_string(),
            severity: if chunk_keys.is_empty() {
                Severity::Green
            } else {
                Severity::Warning
            },
            detail: format!(
                "index.json missing; {} store project/agent/date tuple(s) discovered",
                chunk_keys.len()
            ),
            recommendation: if chunk_keys.is_empty() {
                None
            } else {
                Some(format!(
                    "Run `aicx store --full-rescan` to rebuild {}/index.json",
                    doctor_home_label()
                ))
            },
        };
    }
    let raw = match sanitize::read_to_string_validated(&index_path) {
        Ok(raw) => raw,
        Err(err) => {
            return CheckResult {
                name: "index_consistency".to_string(),
                severity: Severity::Critical,
                detail: format!("Failed to read index.json: {err}"),
                recommendation: Some(format!(
                    "Check filesystem permissions on {}/index.json",
                    doctor_home_label()
                )),
            };
        }
    };
    let index = match serde_json::from_str::<store::StoreIndex>(&raw) {
        Ok(index) => index,
        Err(err) => {
            return CheckResult {
                name: "index_consistency".to_string(),
                severity: Severity::Critical,
                detail: format!("index.json is malformed JSON: {err}"),
                recommendation: Some(format!(
                    "Backup and rebuild {}/index.json via `aicx store --full-rescan`",
                    doctor_home_label()
                )),
            };
        }
    };
    let mut index_keys = BTreeSet::new();
    for (project, project_index) in index.projects {
        for (agent, agent_index) in project_index.agents {
            for date in agent_index.dates {
                index_keys.insert((project.clone(), agent.clone(), date));
            }
        }
    }
    let orphaned = index_keys
        .difference(&chunk_keys)
        .cloned()
        .collect::<Vec<_>>();
    let missing = chunk_keys
        .difference(&index_keys)
        .cloned()
        .collect::<Vec<_>>();
    let sample = |items: &[(String, String, String)]| {
        items
            .iter()
            .take(3)
            .map(|(project, agent, date)| format!("{project}/{agent}/{date}"))
            .collect::<Vec<_>>()
            .join(", ")
    };

    CheckResult {
        name: "index_consistency".to_string(),
        severity: if orphaned.is_empty() && missing.is_empty() {
            Severity::Green
        } else {
            Severity::Warning
        },
        detail: if orphaned.is_empty() && missing.is_empty() {
            format!(
                "index.json matches {} store project/agent/date tuple(s)",
                chunk_keys.len()
            )
        } else {
            format!(
                "{} orphaned index tuple(s), {} missing index tuple(s); orphaned sample: {}; missing sample: {}",
                orphaned.len(),
                missing.len(),
                if orphaned.is_empty() {
                    "none".to_string()
                } else {
                    sample(&orphaned)
                },
                if missing.is_empty() {
                    "none".to_string()
                } else {
                    sample(&missing)
                }
            )
        },
        recommendation: if orphaned.is_empty() && missing.is_empty() {
            None
        } else {
            Some(format!(
                "Run `aicx store --full-rescan` to reconcile {}/index.json with the canonical store",
                doctor_home_label()
            ))
        },
    }
}

pub(crate) fn newest_mtime(root: &Path) -> Option<SystemTime> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut newest = root.metadata().ok().and_then(|meta| meta.modified().ok());
    for entry in entries.flatten() {
        let path = entry.path();
        let candidate = if path.is_dir() {
            newest_mtime(&path)
        } else {
            entry.metadata().ok().and_then(|meta| meta.modified().ok())
        };
        newest = newest.into_iter().chain(candidate).max();
    }
    newest
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub(crate) fn check_embedder_warmth(opts: &DoctorOptions) -> CheckResult {
    let cfg = crate::embedder::EmbeddingConfig::from_env();
    if cfg.backend == crate::embedder::BackendPreference::Cloud {
        let local = cfg
            .cloud
            .as_ref()
            .is_some_and(|cloud| is_local_embedder_url(&cloud.url));
        if !local && !opts.smoke {
            return CheckResult {
                name: "embedder_warmth".to_string(),
                severity: Severity::Skipped,
                detail: "warmth probe skipped; pass --smoke to enable".to_string(),
                recommendation: None,
            };
        }

        if opts.smoke
            && let Some(cloud) = cfg.cloud.as_ref()
        {
            let start = std::time::Instant::now();
            match crate::embedder::cloud::probe(&cloud.url, &cloud.model) {
                Ok(_) => {
                    let elapsed = start.elapsed();
                    let severity = if elapsed < Duration::from_millis(500) {
                        Severity::Green
                    } else if elapsed < Duration::from_secs(3) {
                        Severity::Warning
                    } else {
                        Severity::Critical
                    };
                    return CheckResult {
                        name: "embedder_warmth".to_string(),
                        severity,
                        detail: format!("cloud embedder replied in {}ms", elapsed.as_millis()),
                        recommendation: None,
                    };
                }
                Err(e) => {
                    return CheckResult {
                        name: "embedder_warmth".to_string(),
                        severity: Severity::Critical,
                        detail: format!("cloud warmth probe failed: {e}"),
                        recommendation: None,
                    };
                }
            }
        }
    } else if opts.smoke {
        #[cfg(feature = "native-embedder")]
        {
            let start = std::time::Instant::now();
            match crate::embedder::EmbeddingEngine::new() {
                Ok(mut engine) => {
                    let _ = engine.embed_batch(&["aicx doctor probe".to_string()]);
                    let elapsed = start.elapsed();
                    let severity = if elapsed < Duration::from_millis(500) {
                        Severity::Green
                    } else if elapsed < Duration::from_secs(3) {
                        Severity::Warning
                    } else {
                        Severity::Critical
                    };
                    return CheckResult {
                        name: "embedder_warmth".to_string(),
                        severity,
                        detail: format!("native embedder replied in {}ms", elapsed.as_millis()),
                        recommendation: None,
                    };
                }
                Err(e) => {
                    return CheckResult {
                        name: "embedder_warmth".to_string(),
                        severity: Severity::Critical,
                        detail: format!("native warmth probe failed: {e}"),
                        recommendation: None,
                    };
                }
            }
        }
    }

    CheckResult {
        name: "embedder_warmth".to_string(),
        severity: if opts.smoke {
            Severity::Green
        } else {
            Severity::Skipped
        },
        detail: if opts.smoke {
            "warmth probe ran".to_string()
        } else {
            "warmth probe skipped".to_string()
        },
        recommendation: Some(
            "Run `aicx warmup` before one-shot semantic search after idle".to_string(),
        ),
    }
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
pub(crate) fn check_embedder_warmth(_opts: &DoctorOptions) -> CheckResult {
    CheckResult {
        name: "embedder_warmth".to_string(),
        severity: Severity::NotConfigured,
        detail: "binary built without embedder features".to_string(),
        recommendation: None,
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub(crate) fn is_local_embedder_url(url: &str) -> bool {
    url.contains("localhost:")
        || url.contains("127.0.0.1:")
        || url.contains("0.0.0.0:")
        || url.contains("[::1]:")
}

pub(crate) fn check_canonical_store(base: &Path) -> CheckResult {
    let store_root = base.join("store");
    if !store_root.exists() {
        return CheckResult {
            name: "canonical_store".to_string(),
            severity: Severity::Warning,
            detail: format!("Canonical store does not exist at {}", store_root.display()),
            recommendation: Some("Run `aicx store -H 168` to populate".to_string()),
        };
    }
    let files = store::scan_context_files_at(base).unwrap_or_default();
    // Fast inventory of in-flight/orphaned canonical-projection stages:
    // lease metadata only, payload is never read. A dead stage can retain
    // tens of gigabytes with no owner — surface it, never delete it.
    let stages = crate::store::canonical_projection::inspect_projection_stages_at(&store_root);
    let Some(summary) = summarize_projection_stages(&stages) else {
        return CheckResult {
            name: "canonical_store".to_string(),
            severity: Severity::Green,
            detail: format!("{} chunk files indexed", files.len()),
            recommendation: None,
        };
    };

    CheckResult {
        name: "canonical_store".to_string(),
        severity: summary.severity,
        detail: format!(
            "{} chunk files indexed; {} projection stage(s): {}; sample: {}",
            files.len(),
            summary.count,
            summary.breakdown,
            summary.sample,
        ),
        recommendation: summary.recommendation,
    }
}

#[cfg(not(feature = "lance"))]
pub(crate) async fn check_steer_lance(_base: &Path) -> CheckResult {
    CheckResult {
        name: "steer_lance".to_string(),
        severity: Severity::NotConfigured,
        detail: "steer_index (Lance/BM25) feature is disabled".to_string(),
        recommendation: None,
    }
}

#[cfg(feature = "lance")]
pub(crate) async fn check_steer_lance(base: &Path) -> CheckResult {
    let lance_dir = base.join("steer_db").join("mcp_documents.lance");
    if !lance_dir.exists() {
        return CheckResult {
            name: "steer_lance".to_string(),
            severity: Severity::Warning,
            detail: "Lance steer table does not exist (will be created on next sync)".to_string(),
            recommendation: None,
        };
    }
    match steer_index::query_steer_index_count().await {
        Ok(count) => CheckResult {
            name: "steer_lance".to_string(),
            severity: Severity::Skipped,
            detail: format!(
                "Lance steer table exists ({} documents); real query not run",
                count
            ),
            recommendation: None,
        },
        Err(e) => {
            let msg = e.to_string();
            let critical = msg.contains("Not found")
                || msg.contains("manifest")
                || msg.contains("_deletions")
                || msg.contains("LanceError");
            CheckResult {
                name: "steer_lance".to_string(),
                severity: if critical {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                detail: format!("Lance probe failed: {msg}"),
                recommendation: Some(if critical {
                    "Run `aicx doctor --rebuild-steer-index` to delete and rebuild from canonical store".to_string()
                } else {
                    format!(
                        "Investigate logs; persistent issues may need manual `rm -rf {}/steer_db`",
                        doctor_home_label()
                    )
                }),
            }
        }
    }
}

#[cfg(not(feature = "lance"))]
pub(crate) fn check_steer_bm25(_base: &Path) -> CheckResult {
    CheckResult {
        name: "steer_bm25".to_string(),
        severity: Severity::NotConfigured,
        detail: "steer_index (Lance/BM25) feature is disabled".to_string(),
        recommendation: None,
    }
}

#[cfg(feature = "lance")]
pub(crate) fn check_steer_bm25(base: &Path) -> CheckResult {
    let bm25_dir = base.join("steer_bm25");
    if !bm25_dir.exists() {
        return CheckResult {
            name: "steer_bm25".to_string(),
            severity: Severity::Warning,
            detail: "BM25 steer index does not exist".to_string(),
            recommendation: Some("Will be created on next `aicx store` run".to_string()),
        };
    }
    let entries = std::fs::read_dir(&bm25_dir)
        .map(|it| it.flatten().count())
        .unwrap_or(0);
    CheckResult {
        name: "steer_bm25".to_string(),
        severity: if entries > 0 {
            Severity::Skipped
        } else {
            Severity::Warning
        },
        detail: format!(
            "BM25 index has {} entries on disk; real query not run",
            entries
        ),
        recommendation: if entries == 0 {
            Some("BM25 dir is empty; reindex with `aicx store -H 168 --full-rescan`".to_string())
        } else {
            None
        },
    }
}

pub(crate) fn check_state(base: &Path) -> CheckResult {
    let state_path = base.join("state.json");
    if !state_path.exists() {
        return CheckResult {
            name: "state".to_string(),
            severity: Severity::Warning,
            detail: "state.json does not exist (no extraction watermarks)".to_string(),
            recommendation: Some("Will be created on first `aicx store` run".to_string()),
        };
    }
    // state.json has a dedicated 128 MiB cap (see sanitize::MAX_STATE_JSON_BYTES);
    // the generic 8 MiB validated reader rejects realistic dedup histories
    // (200k+ chunks → state.json ~25 MB). Use the state-specific reader.
    let raw = match sanitize::read_state_json_validated(&state_path) {
        Ok(s) => s,
        Err(e) => {
            return CheckResult {
                name: "state".to_string(),
                severity: Severity::Critical,
                detail: format!("Failed to read state.json: {e}"),
                recommendation: Some(format!(
                    "Check filesystem permissions on {}/state.json",
                    doctor_home_label()
                )),
            };
        }
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(_) => CheckResult {
            name: "state".to_string(),
            severity: Severity::Green,
            detail: format!("state.json parses cleanly ({} bytes)", raw.len()),
            recommendation: None,
        },
        Err(e) => CheckResult {
            name: "state".to_string(),
            severity: Severity::Critical,
            detail: format!("state.json is malformed JSON: {e}"),
            recommendation: Some(format!(
                "Backup and remove {}/state.json; will rebuild on next run",
                doctor_home_label()
            )),
        },
    }
}

pub(crate) fn check_sidecar_coverage(base: &Path) -> CheckResult {
    let files = store::scan_context_files_at(base).unwrap_or_default();
    let context_corpus_files = store::scan_context_corpus_files_at(base).unwrap_or_default();
    let total = files.len() + context_corpus_files.len();
    if total == 0 {
        return CheckResult {
            name: "sidecars".to_string(),
            severity: Severity::Green,
            detail: "no chunks to check".to_string(),
            recommendation: None,
        };
    }
    let mut missing = 0usize;
    for f in &files {
        let sidecar = store::sidecar_path_for_chunk(&f.path);
        if !sidecar.exists() {
            missing += 1;
        }
    }
    for f in &context_corpus_files {
        if !f.sidecar_path.exists() {
            missing += 1;
        }
    }
    let coverage_pct = (((total - missing) as f64 / total as f64) * 100.0) as u32;
    let severity = if missing == 0 {
        Severity::Green
    } else if missing < total / 10 {
        Severity::Warning
    } else {
        Severity::Critical
    };
    CheckResult {
        name: "sidecars".to_string(),
        severity,
        detail: format!(
            "{}/{} chunks have sidecars ({}%)",
            total - missing,
            total,
            coverage_pct
        ),
        recommendation: if missing > 0 {
            Some(format!(
                "{} chunks missing sidecars; run `aicx store --full-rescan` to backfill",
                missing
            ))
        } else {
            None
        },
    }
}

pub(crate) fn check_content_dedup(base: &Path) -> CheckResult {
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in store::scan_context_files_at(base).unwrap_or_default() {
        if let Some(sidecar) = store::load_sidecar(&file.path)
            && let Some(hash) = sidecar.content_sha256
        {
            seen.entry(hash)
                .or_default()
                .push(file.path.display().to_string());
        }
    }
    for file in store::scan_context_corpus_files_at(base).unwrap_or_default() {
        if let Some(hash) = file.sidecar.content_sha256 {
            seen.entry(hash)
                .or_default()
                .push(file.raw_path.display().to_string());
        }
    }
    let duplicates: Vec<_> = seen.values().filter(|paths| paths.len() > 1).collect();
    let duplicate_chunks: usize = duplicates.iter().map(|paths| paths.len()).sum();
    let duplicate_groups = duplicates.len();
    CheckResult {
        name: "content_dedup".to_string(),
        severity: if duplicate_groups == 0 {
            Severity::Green
        } else {
            Severity::Warning
        },
        detail: format!("{duplicate_groups} duplicate hash group(s), {duplicate_chunks} chunk(s)"),
        recommendation: if duplicate_groups > 0 {
            Some("Run `aicx store --full-rescan` only after pruning duplicate chunks or let content-hash dedup skip future writes".to_string())
        } else {
            None
        },
    }
}

pub(crate) fn check_empty_body_chunks(base: &Path) -> CheckResult {
    let report = empty_body_report(base);
    let total = report.total;
    if total == 0 {
        return CheckResult {
            name: "empty_body_chunks".to_string(),
            severity: Severity::Green,
            detail: "no chunks to inspect".to_string(),
            recommendation: None,
        };
    }

    let pct = (report.empty as f64 / total as f64) * 100.0;
    let severity = if pct > 5.0 {
        Severity::Critical
    } else if pct >= 0.5 {
        Severity::Warning
    } else {
        Severity::Green
    };
    let top_frames = report
        .by_frame_kind
        .iter()
        .take(5)
        .map(|(kind, count)| format!("{kind}:{count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sample = report
        .sample_paths
        .iter()
        .take(3)
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    CheckResult {
        name: "empty_body_chunks".to_string(),
        severity,
        detail: format!(
            "{} empty-body candidate(s) / {} chunks ({pct:.2}%); frame_kind: {}; sample: {}",
            report.empty,
            total,
            if top_frames.is_empty() {
                "none"
            } else {
                &top_frames
            },
            if sample.is_empty() { "none" } else { &sample }
        ),
        recommendation: if report.empty > 0 {
            Some(
                "Run `aicx doctor --prune-empty-bodies --apply` to move empty-body chunks to quarantine, or omit `--apply` for a reviewable script"
                    .to_string(),
            )
        } else {
            None
        },
    }
}

/// Aggregate noise filter activity across all sidecars in the canonical
/// store. Surfaces upstream emitters that produce excessive structural
/// scaffolding. Sidecars without `noise_lines_dropped` (older than commit
/// `ffe288a`) contribute `0` and are counted under `pre_filter_chunks`.
///
/// Severity policy:
/// - `Green` when no chunks have any dropped noise (clean corpus or
///   pre-filter-only).
/// - `Warning` when >50% of post-filter chunks recorded >10 dropped noise
///   lines — operator should investigate which agents/runs produced the
///   scaffolding.
/// - `Green` for any milder signal (filter doing its job invisibly).
pub(crate) fn check_noise_health(base: &Path) -> CheckResult {
    let files = store::scan_context_files_at(base).unwrap_or_default();
    let total = files.len();
    if total == 0 {
        return CheckResult {
            name: "noise_health".to_string(),
            severity: Severity::Green,
            detail: "no chunks to inspect".to_string(),
            recommendation: None,
        };
    }

    let mut total_dropped: u64 = 0;
    let mut chunks_with_drops: usize = 0;
    let mut chunks_with_heavy_drops: usize = 0;
    let mut sidecars_read: usize = 0;
    let mut sidecars_pre_filter: usize = 0;

    for f in &files {
        let sidecar_path = f.path.with_extension("meta.json");
        let Ok(bytes) = std::fs::read(&sidecar_path) else {
            continue;
        };
        sidecars_read += 1;
        let Ok(sidecar) = serde_json::from_slice::<aicx_parser::ChunkMetadataSidecar>(&bytes)
        else {
            continue;
        };
        if sidecar.noise_lines_dropped == 0 {
            // Either pre-filter-era sidecar (field absent → 0) or genuinely
            // clean chunk. Indistinguishable here without a probe; surface
            // as informational.
            sidecars_pre_filter += 1;
            continue;
        }
        total_dropped += sidecar.noise_lines_dropped as u64;
        chunks_with_drops += 1;
        if sidecar.noise_lines_dropped > 10 {
            chunks_with_heavy_drops += 1;
        }
    }

    let post_filter_chunks = sidecars_read.saturating_sub(sidecars_pre_filter);
    let heavy_pct = if post_filter_chunks > 0 {
        (chunks_with_heavy_drops as f64 / post_filter_chunks as f64) * 100.0
    } else {
        0.0
    };

    let severity = if post_filter_chunks > 0 && heavy_pct > 50.0 {
        Severity::Warning
    } else {
        Severity::Green
    };

    let detail = format!(
        "{total_dropped} noise lines dropped across {chunks_with_drops}/{post_filter_chunks} post-filter chunks ({heavy_pct:.0}% heavy >10 lines); {sidecars_pre_filter} pre-filter sidecars"
    );

    let recommendation = if matches!(severity, Severity::Warning) {
        Some(
            "Heavy structural scaffolding in upstream emitters. Investigate which agents/runs produced the noise (check `aicx doctor --verbose` and ingestion sources).".to_string(),
        )
    } else {
        None
    };

    CheckResult {
        name: "noise_health".to_string(),
        severity,
        detail,
        recommendation,
    }
}

pub(crate) fn check_corpus_buckets(base: &Path) -> CheckResult {
    let store_root = base.join("store");
    if !store_root.exists() {
        return CheckResult {
            name: "corpus_buckets".to_string(),
            severity: Severity::Green,
            detail: format!("No corpus store exists at {}", store_root.display()),
            recommendation: None,
        };
    }

    let bucket_count = match count_corpus_buckets(&store_root) {
        Ok(count) => count,
        Err(e) => {
            return CheckResult {
                name: "corpus_buckets".to_string(),
                severity: Severity::Critical,
                detail: format!("Failed to read corpus buckets: {e}"),
                recommendation: Some(format!(
                    "Check filesystem permissions on {}",
                    store_root.display()
                )),
            };
        }
    };

    match scan_corpus_buckets(&store_root) {
        Ok(suspicious) if suspicious.is_empty() => CheckResult {
            name: "corpus_buckets".to_string(),
            severity: Severity::Green,
            detail: format!("All {bucket_count} top-level org buckets pass schema check"),
            recommendation: None,
        },
        Ok(suspicious) => CheckResult {
            name: "corpus_buckets".to_string(),
            severity: Severity::Warning,
            detail: format!(
                "{} suspicious bucket(s): {}",
                suspicious.len(),
                suspicious
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            recommendation: Some(
                "Run `aicx doctor --fix-buckets --dry-run` to preview the quarantine plan, \
                 then `aicx doctor --fix-buckets` to move text-extracted-junk and template \
                 placeholder buckets to $HOME/.aicx/quarantine/<timestamp>/"
                    .to_string(),
            ),
        },
        Err(e) => CheckResult {
            name: "corpus_buckets".to_string(),
            severity: Severity::Critical,
            detail: format!("Failed to read corpus buckets: {e}"),
            recommendation: Some(format!(
                "Check filesystem permissions on {}",
                store_root.display()
            )),
        },
    }
}

pub(crate) fn count_corpus_buckets(store_root: &Path) -> Result<usize> {
    let mut count = 0usize;
    for entry in
        crate::sanitize::read_dir_validated(store_root).context("read corpus store root")?
    {
        let entry = entry.context("read corpus store entry")?;
        if entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false) {
            count += 1;
        }
    }
    Ok(count)
}

/// Scan top-level corpus buckets and return only the **suspicious** ones —
/// folder names that fail [`is_valid_repo_bucket_name`] / [`is_valid_repo_project_slug`].
///
/// The validator is intentionally permissive (case-preserving CamelCase,
/// dot-prefix `.aicx`/`.github`/`.scripts`, underscore-prefix `_internal`)
/// after the 2026-05-12 relax. What remains "suspicious" is real
/// extractor-bug evidence: template-placeholder leaks (`${RELEASE_REPO}`,
/// `<owner>`, `{target_owner}`), and mid-segment shell-metacharacter /
/// newline / quote garbage (`loctree.git\ncd`,
/// `vc-skills.git\"><span`).
///
/// Pre-2026-05-12 the validator also rejected CamelCase orgs and
/// dot-prefixed names, which on 2026-05-09 mass-quarantined ~89k
/// legitimate chunks across `LibraxisAI/`, `Vetcoders/`, `Loctree/`,
/// and other org buckets. Relaxing the validator (not adding
/// canonicalization magic) was the correct response.
pub(crate) fn scan_corpus_buckets(store_root: &Path) -> Result<Vec<String>> {
    let mut suspicious = Vec::new();
    if !store_root.exists() {
        return Ok(suspicious);
    }

    for org_entry in
        crate::sanitize::read_dir_validated(store_root).context("read corpus store root")?
    {
        let org_entry = org_entry.context("read corpus store entry")?;
        if !org_entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(org) = org_entry.file_name().into_string() else {
            continue;
        };

        if !is_valid_repo_bucket_name(&org) {
            suspicious.push(org);
            continue;
        }

        // Org folder is valid → check each repo subfolder.
        for repo_entry in crate::sanitize::read_dir_validated(&org_entry.path())
            .with_context(|| format!("read corpus org bucket `{org}`"))?
        {
            let repo_entry = repo_entry.context("read corpus repo entry")?;
            if !repo_entry
                .file_type()
                .map(|ty| ty.is_dir())
                .unwrap_or(false)
            {
                continue;
            }
            let Ok(repo) = repo_entry.file_name().into_string() else {
                continue;
            };
            let slug = format!("{org}/{repo}");
            if !is_valid_repo_project_slug(&slug) {
                suspicious.push(slug);
            }
        }
    }

    suspicious.sort();
    Ok(suspicious)
}

pub(crate) async fn attempt_steer_rebuild(base: &Path) -> Result<String> {
    let steer_db = base.join("steer_db");
    let steer_bm25 = base.join("steer_bm25");
    let steer_meta = base.join("steer_index_meta.json");

    let mut removed = Vec::new();
    if steer_db.exists() {
        std::fs::remove_dir_all(&steer_db).context("remove steer_db")?;
        removed.push("steer_db");
    }
    if steer_bm25.exists() {
        std::fs::remove_dir_all(&steer_bm25).context("remove steer_bm25")?;
        removed.push("steer_bm25");
    }
    if steer_meta.exists() {
        std::fs::remove_file(&steer_meta).context("remove steer_index_meta.json")?;
        removed.push("steer_index_meta.json");
    }

    steer_index::rebuild_steer_index_if_needed()
        .await
        .context("rebuild after corruption removal")?;

    Ok(format!(
        "removed {} and rebuilt steer index from canonical store",
        if removed.is_empty() {
            "nothing".to_string()
        } else {
            removed.join(", ")
        }
    ))
}

#[cfg(test)]
mod fast_health_budget_tests {
    use super::*;

    fn opts() -> DoctorOptions {
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
            verbose: false,
            smoke: false,
        }
    }

    fn fixture(tag: &str, dirs: usize, files_per_dir: usize) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "aicx-doctor-budget-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for d in 0..dirs {
            let dir = base
                .join("store")
                .join("sampleorg")
                .join(format!("repo-{d}"));
            std::fs::create_dir_all(&dir).unwrap();
            for f in 0..files_per_dir {
                let chunk = dir.join(format!("chunk-{f}.md"));
                std::fs::write(&chunk, "# chunk\n").unwrap();
                std::fs::write(chunk.with_extension("meta.json"), "{}").unwrap();
            }
        }
        base
    }

    #[test]
    fn fast_pass_respects_the_fs_call_budget_and_degrades_to_unknown() {
        // W2-04 red-first instrumentation: with a budget far below the tree
        // size the fast pass must stop traversing (bounded call count) and
        // refuse to certify — unknown, never a silent full scan.
        let base = fixture("tiny-budget", 6, 6);
        let limit = 10usize;
        let mut budget = FsBudget::new(limit);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let report = rt.block_on(run_fast_impl(&base, &opts(), &mut budget));

        assert!(
            budget.used() <= limit + 8,
            "fast pass kept charging after exhaustion: used {} vs limit {limit}",
            budget.used()
        );
        assert_eq!(
            report.canonical_store.severity,
            Severity::Unknown,
            "with no walk and no index.json the store inventory is unknown: {:?}",
            report.canonical_store
        );
        assert!(
            report
                .canonical_store
                .recommendation
                .as_deref()
                .is_some_and(|rec| rec.contains("aicx doctor --deep")),
            "unknown inventory must point at the deep command"
        );
        assert_eq!(report.index_freshness.severity, Severity::Unknown);
        assert_eq!(report.sidecars.severity, Severity::Unknown);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn fast_pass_fully_verifies_small_stores_within_budget() {
        let base = fixture("small", 2, 3);
        let mut budget = FsBudget::new(FAST_HEALTH_FS_BUDGET);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let report = rt.block_on(run_fast_impl(&base, &opts(), &mut budget));

        assert_eq!(report.canonical_store.severity, Severity::Green);
        assert!(
            report
                .canonical_store
                .detail
                .contains("6 chunk file(s) counted by bounded walk"),
            "completed bounded walk must yield the exact chunk count: {}",
            report.canonical_store.detail
        );
        assert_eq!(report.sidecars.severity, Severity::Green);
        assert!(budget.used() <= FAST_HEALTH_FS_BUDGET);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn fast_pass_missing_sampled_sidecar_fails_closed_to_warning() {
        let base = fixture("missing-sidecar", 1, 3);
        // Remove one sidecar: the sampled invariant must degrade, not shrug.
        let victim = base
            .join("store")
            .join("sampleorg")
            .join("repo-0")
            .join("chunk-1.meta.json");
        std::fs::remove_file(&victim).unwrap();
        let mut budget = FsBudget::new(FAST_HEALTH_FS_BUDGET);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let report = rt.block_on(run_fast_impl(&base, &opts(), &mut budget));
        assert_eq!(report.sidecars.severity, Severity::Warning);
        let _ = std::fs::remove_dir_all(&base);
    }
}
