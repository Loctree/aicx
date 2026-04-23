//! Diagnostic and self-healing layer for aicx.
//!
//! `aicx doctor` performs an integrity audit of the canonical store, the
//! steer index (Lance + BM25), state.json, and sidecar coverage. With
//! `--fix`, safe corrective actions are applied: corrupted steer indexes
//! are rebuilt from canonical store via `steer_index::rebuild_steer_index_if_needed`.
//!
//! The canonical store (`~/.aicx/store/`) is treated as ground truth and
//! never modified. Derived views (steer_db, steer_bm25) are rebuildable
//! and may be deleted under `--fix`.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

use crate::steer_index;
use crate::store;

#[derive(Debug, Clone)]
pub struct DoctorOptions {
    pub fix: bool,
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
    pub fixes_applied: Vec<String>,
    pub overall: Severity,
}

pub async fn run(opts: &DoctorOptions) -> Result<DoctorReport> {
    let base = store::store_base_dir().context("Failed to resolve aicx store base directory")?;

    let canonical_store = check_canonical_store(&base);
    let steer_lance = check_steer_lance(&base).await;
    let steer_bm25 = check_steer_bm25(&base);
    let state = check_state(&base);
    let sidecars = check_sidecar_coverage(&base);

    let mut fixes_applied = Vec::new();

    if opts.fix
        && (steer_lance.severity == Severity::Critical || steer_bm25.severity == Severity::Critical)
    {
        match attempt_steer_rebuild(&base).await {
            Ok(msg) => fixes_applied.push(msg),
            Err(e) => fixes_applied.push(format!("rebuild attempted but failed: {e}")),
        }
    }

    let overall = max_severity(&[
        canonical_store.severity,
        steer_lance.severity,
        steer_bm25.severity,
        state.severity,
        sidecars.severity,
    ]);

    Ok(DoctorReport {
        canonical_store,
        steer_lance,
        steer_bm25,
        state,
        sidecars,
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
}
