//! Diagnostic and self-healing layer for aicx.
//!
//! `aicx doctor` performs an integrity audit of the canonical store, the
//! steer index (Lance + BM25), state.json, sidecar coverage, and corpus
//! bucket names. With
//! `--fix`, safe corrective actions are applied: corrupted steer indexes
//! are rebuilt from canonical store via `steer_index::rebuild_steer_index_if_needed`.
//! With `--fix-buckets`, suspicious top-level store buckets are moved to
//! timestamped quarantine.
//!
//! The canonical store (`~/.aicx/store/`) is treated as ground truth: doctor
//! never deletes store contents. Bucket quarantine is a rename into
//! `~/.aicx/quarantine/<timestamp>/`, preserving the original payload.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::steer_index;
use crate::store;
use crate::validation::is_valid_repo_bucket_name;

#[derive(Debug, Clone)]
pub struct DoctorOptions {
    pub fix: bool,
    pub fix_buckets: bool,
    pub verbose: bool,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Green,
    Warning,
    Critical,
}

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub severity: Severity,
    pub detail: String,
    pub recommendation: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub canonical_store: CheckResult,
    pub steer_lance: CheckResult,
    pub steer_bm25: CheckResult,
    pub state: CheckResult,
    pub sidecars: CheckResult,
    pub corpus_buckets: CheckResult,
    pub noise_health: CheckResult,
    pub fixes_applied: Vec<String>,
    pub overall: Severity,
}

pub async fn run(opts: &DoctorOptions) -> Result<DoctorReport> {
    let base = store::store_base_dir().context("Failed to resolve aicx store base directory")?;

    let mut canonical_store = check_canonical_store(&base);
    let mut steer_lance = check_steer_lance(&base).await;
    let mut steer_bm25 = check_steer_bm25(&base);
    let mut state = check_state(&base);
    let mut sidecars = check_sidecar_coverage(&base);
    let mut corpus_buckets = check_corpus_buckets(&base);
    let mut noise_health = check_noise_health(&base);

    let mut fixes_applied = Vec::new();

    if opts.fix
        && (steer_lance.severity == Severity::Critical || steer_bm25.severity == Severity::Critical)
    {
        match attempt_steer_rebuild(&base).await {
            Ok(msg) => fixes_applied.push(msg),
            Err(e) => fixes_applied.push(format!("rebuild attempted but failed: {e}")),
        }
    }

    if opts.fix_buckets {
        let store_root = base.join("store");
        match suspicious_corpus_buckets(&store_root) {
            Ok(suspicious) if suspicious.is_empty() => {
                fixes_applied.push("no suspicious corpus buckets to quarantine".to_string());
            }
            Ok(suspicious) => {
                let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
                let single_bucket = suspicious.len() == 1;
                for bucket_name in suspicious {
                    let result = if single_bucket {
                        quarantine_bucket(&store_root, &bucket_name)
                    } else {
                        quarantine_bucket_with_timestamp(&store_root, &bucket_name, &timestamp)
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
            Err(e) => fixes_applied.push(format!("bucket quarantine skipped: {e}")),
        }
    }

    if opts.fix || opts.fix_buckets {
        canonical_store = check_canonical_store(&base);
        steer_lance = check_steer_lance(&base).await;
        steer_bm25 = check_steer_bm25(&base);
        state = check_state(&base);
        sidecars = check_sidecar_coverage(&base);
        corpus_buckets = check_corpus_buckets(&base);
        noise_health = check_noise_health(&base);
    }

    let overall = max_severity(&[
        canonical_store.severity,
        steer_lance.severity,
        steer_bm25.severity,
        state.severity,
        sidecars.severity,
        corpus_buckets.severity,
        noise_health.severity,
    ]);

    Ok(DoctorReport {
        canonical_store,
        steer_lance,
        steer_bm25,
        state,
        sidecars,
        corpus_buckets,
        noise_health,
        fixes_applied,
        overall,
    })
}

fn check_canonical_store(base: &Path) -> CheckResult {
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
    CheckResult {
        name: "canonical_store".to_string(),
        severity: Severity::Green,
        detail: format!("{} chunk files indexed", files.len()),
        recommendation: None,
    }
}

async fn check_steer_lance(base: &Path) -> CheckResult {
    let lance_dir = base.join("steer_db").join("mcp_documents.lance");
    if !lance_dir.exists() {
        return CheckResult {
            name: "steer_lance".to_string(),
            severity: Severity::Warning,
            detail: "Lance steer table does not exist (will be created on next sync)".to_string(),
            recommendation: None,
        };
    }
    match steer_index::query_steer_index().await {
        Ok(docs) => CheckResult {
            name: "steer_lance".to_string(),
            severity: Severity::Green,
            detail: format!("Lance steer table healthy, {} documents", docs.len()),
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
                    "Run `aicx doctor --fix` to delete and rebuild from canonical store".to_string()
                } else {
                    "Investigate logs; persistent issues may need manual `rm -rf ~/.aicx/steer_db`"
                        .to_string()
                }),
            }
        }
    }
}

fn check_steer_bm25(base: &Path) -> CheckResult {
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
            Severity::Green
        } else {
            Severity::Warning
        },
        detail: format!("BM25 index has {} entries on disk", entries),
        recommendation: if entries == 0 {
            Some("BM25 dir is empty; reindex with `aicx store -H 168 --full-rescan`".to_string())
        } else {
            None
        },
    }
}

fn check_state(base: &Path) -> CheckResult {
    let state_path = base.join("state.json");
    if !state_path.exists() {
        return CheckResult {
            name: "state".to_string(),
            severity: Severity::Warning,
            detail: "state.json does not exist (no extraction watermarks)".to_string(),
            recommendation: Some("Will be created on first `aicx store` run".to_string()),
        };
    }
    let raw = match std::fs::read_to_string(&state_path) {
        Ok(s) => s,
        Err(e) => {
            return CheckResult {
                name: "state".to_string(),
                severity: Severity::Critical,
                detail: format!("Failed to read state.json: {e}"),
                recommendation: Some(
                    "Check filesystem permissions on ~/.aicx/state.json".to_string(),
                ),
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
            recommendation: Some(
                "Backup and remove ~/.aicx/state.json; will rebuild on next run".to_string(),
            ),
        },
    }
}

fn check_sidecar_coverage(base: &Path) -> CheckResult {
    let files = store::scan_context_files_at(base).unwrap_or_default();
    let total = files.len();
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
        let sidecar = f.path.with_extension("meta.json");
        if !sidecar.exists() {
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
fn check_noise_health(base: &Path) -> CheckResult {
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

fn check_corpus_buckets(base: &Path) -> CheckResult {
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

    match suspicious_corpus_buckets(&store_root) {
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
                suspicious.join(", ")
            ),
            recommendation: Some(
                "Run `aicx doctor --fix-buckets` to move them to $HOME/.aicx/quarantine/"
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

fn count_corpus_buckets(store_root: &Path) -> Result<usize> {
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

fn suspicious_corpus_buckets(store_root: &Path) -> Result<Vec<String>> {
    if !store_root.exists() {
        return Ok(Vec::new());
    }

    let mut suspicious = Vec::new();
    for entry in
        crate::sanitize::read_dir_validated(store_root).context("read corpus store root")?
    {
        let entry = entry.context("read corpus store entry")?;
        if !entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !is_valid_repo_bucket_name(&name) {
            suspicious.push(name);
        }
    }
    suspicious.sort();
    Ok(suspicious)
}

fn quarantine_bucket(store_root: &Path, bucket_name: &str) -> Result<PathBuf> {
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    quarantine_bucket_with_timestamp(store_root, bucket_name, &timestamp)
}

fn quarantine_bucket_with_timestamp(
    store_root: &Path,
    bucket_name: &str,
    timestamp: &str,
) -> Result<PathBuf> {
    let quarantine_root = store_root
        .parent()
        .context("store root has no parent for quarantine")?
        .join("quarantine")
        .join(timestamp);
    std::fs::create_dir_all(&quarantine_root).context("create quarantine root")?;
    let src = store_root.join(bucket_name);
    let dst = quarantine_root.join(bucket_name);
    std::fs::rename(&src, &dst).with_context(|| {
        format!(
            "rename corpus bucket {} to {}",
            src.display(),
            dst.display()
        )
    })?;
    Ok(dst)
}

async fn attempt_steer_rebuild(base: &Path) -> Result<String> {
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

fn max_severity(items: &[Severity]) -> Severity {
    let mut max = Severity::Green;
    for s in items {
        match s {
            Severity::Critical => return Severity::Critical,
            Severity::Warning if max == Severity::Green => max = Severity::Warning,
            _ => {}
        }
    }
    max
}

pub fn format_report_text(report: &DoctorReport, verbose: bool) -> String {
    let mut out = String::new();
    out.push_str("aicx doctor report\n");
    out.push_str(&format!("Overall: {:?}\n\n", report.overall));
    let checks = [
        &report.canonical_store,
        &report.steer_lance,
        &report.steer_bm25,
        &report.state,
        &report.sidecars,
        &report.corpus_buckets,
        &report.noise_health,
    ];
    for check in checks {
        out.push_str(&format!(
            "[{:?}] {}: {}\n",
            check.severity, check.name, check.detail
        ));
        if let Some(rec) = &check.recommendation
            && (verbose || check.severity != Severity::Green)
        {
            out.push_str(&format!("    -> {}\n", rec));
        }
    }
    if !report.fixes_applied.is_empty() {
        out.push_str("\nFixes applied:\n");
        for fix in &report.fixes_applied {
            out.push_str(&format!("  + {}\n", fix));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_severity_promotes_critical() {
        assert_eq!(
            max_severity(&[Severity::Green, Severity::Warning, Severity::Critical]),
            Severity::Critical
        );
        assert_eq!(
            max_severity(&[Severity::Green, Severity::Warning]),
            Severity::Warning
        );
        assert_eq!(max_severity(&[Severity::Green]), Severity::Green);
    }

    #[test]
    fn check_canonical_store_warns_when_missing() {
        let tmp = std::env::temp_dir().join(format!("aicx-doctor-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let result = check_canonical_store(&tmp);
        assert_eq!(result.severity, Severity::Warning);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn check_corpus_buckets_green_when_only_valid_names() {
        let tmp = unique_test_dir("valid-buckets");
        let store = tmp.join("store");
        std::fs::create_dir_all(store.join("VetCoders")).unwrap();
        std::fs::create_dir_all(store.join("LibraxisAI")).unwrap();
        std::fs::create_dir_all(store.join("local")).unwrap();

        let result = check_corpus_buckets(&tmp);
        assert_eq!(result.severity, Severity::Green);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn check_corpus_buckets_flags_template_literals() {
        let tmp = unique_test_dir("bad-buckets");
        let store = tmp.join("store");
        std::fs::create_dir_all(store.join("VetCoders")).unwrap();
        std::fs::create_dir_all(store.join("{target_owner}")).unwrap();
        std::fs::create_dir_all(store.join("...")).unwrap();

        let result = check_corpus_buckets(&tmp);
        assert_eq!(result.severity, Severity::Warning);
        assert!(result.detail.contains("{target_owner}"));
        assert!(result.detail.contains("..."));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn quarantine_moves_bucket_atomically() {
        let tmp = unique_test_dir("quarantine-move");
        let store = tmp.join("store");
        let bad = store.join("{x}");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("test.md"), "content").unwrap();

        let dest = quarantine_bucket(&store, "{x}").unwrap();
        assert!(dest.exists());
        assert!(!bad.exists());
        assert!(dest.join("test.md").exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn quarantine_skips_when_no_buckets_match() {
        let tmp = unique_test_dir("quarantine-noop");
        let store = tmp.join("store");
        std::fs::create_dir_all(store.join("VetCoders")).unwrap();
        std::fs::create_dir_all(store.join("local")).unwrap();

        let suspicious = suspicious_corpus_buckets(&store).unwrap();
        assert!(suspicious.is_empty());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn unique_test_dir(label: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "aicx-doctor-{label}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }
}
