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
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::oracle::OracleReadiness;
use crate::steer_index;
use crate::store;
use crate::validation::{is_valid_repo_bucket_name, is_valid_repo_project_slug};

#[derive(Debug, Clone)]
pub struct DoctorOptions {
    pub fix: bool,
    pub fix_buckets: bool,
    pub rebuild_sidecars: bool,
    pub prune_empty_bodies: bool,
    pub verbose: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Green,
    #[default]
    Warning,
    Critical,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub severity: Severity,
    pub detail: String,
    pub recommendation: Option<String>,
}

impl Default for CheckResult {
    fn default() -> Self {
        Self {
            name: "unknown".to_string(),
            severity: Severity::Warning,
            detail: "not checked".to_string(),
            recommendation: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DoctorReport {
    pub canonical_store: CheckResult,
    pub steer_lance: CheckResult,
    pub steer_bm25: CheckResult,
    pub state: CheckResult,
    pub sidecars: CheckResult,
    pub corpus_buckets: CheckResult,
    pub noise_health: CheckResult,
    #[serde(default)]
    pub semantic_health: CheckResult,
    #[serde(default)]
    pub index_freshness: CheckResult,
    #[serde(default)]
    pub sidecar_coverage: CheckResult,
    #[serde(default)]
    pub embedder_warmth: CheckResult,
    #[serde(default)]
    pub empty_body_chunks: CheckResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebuild_sidecars_script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prune_empty_bodies_script: Option<String>,
    pub fixes_applied: Vec<String>,
    pub overall: Severity,
}

#[derive(Debug, Serialize)]
pub struct OracleReadinessReport {
    pub readiness: OracleReadiness,
    pub readiness_label: &'static str,
    pub canonical_corpus_health: Severity,
    pub metadata_steer_index_health: Severity,
    pub content_semantic_index_health: Severity,
    pub dashboard_semantic_route_health: Severity,
    pub loctree_oracle_readiness: OracleReadiness,
    pub reason: String,
}

pub async fn run(opts: &DoctorOptions) -> Result<DoctorReport> {
    let base = store::store_base_dir().context("Failed to resolve aicx store base directory")?;
    run_at(&base, opts).await
}

pub async fn run_at(base: &Path, opts: &DoctorOptions) -> Result<DoctorReport> {
    let mut canonical_store = check_canonical_store(base);
    let mut steer_lance = check_steer_lance(base).await;
    let mut steer_bm25 = check_steer_bm25(base);
    let mut state = check_state(base);
    let mut sidecars = check_sidecar_coverage(base);
    let mut corpus_buckets = check_corpus_buckets(base);
    let mut noise_health = check_noise_health(base);
    let mut semantic_health = check_semantic_health();
    let mut index_freshness = check_index_freshness(base);
    let mut embedder_warmth = check_embedder_warmth();
    let mut empty_body_chunks = check_empty_body_chunks(base);

    let mut fixes_applied = Vec::new();
    let rebuild_sidecars_script = if opts.rebuild_sidecars {
        Some(render_rebuild_sidecars_script(base)?)
    } else {
        None
    };
    let prune_empty_bodies_script = if opts.prune_empty_bodies {
        Some(render_prune_empty_bodies_script(base)?)
    } else {
        None
    };

    if opts.fix
        && (steer_lance.severity == Severity::Critical || steer_bm25.severity == Severity::Critical)
    {
        match attempt_steer_rebuild(base).await {
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
        canonical_store = check_canonical_store(base);
        steer_lance = check_steer_lance(base).await;
        steer_bm25 = check_steer_bm25(base);
        state = check_state(base);
        sidecars = check_sidecar_coverage(base);
        corpus_buckets = check_corpus_buckets(base);
        noise_health = check_noise_health(base);
        semantic_health = check_semantic_health();
        index_freshness = check_index_freshness(base);
        embedder_warmth = check_embedder_warmth();
        empty_body_chunks = check_empty_body_chunks(base);
    }

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
        embedder_warmth.severity,
        empty_body_chunks.severity,
    ]);

    Ok(DoctorReport {
        canonical_store,
        steer_lance,
        steer_bm25,
        state,
        sidecars,
        corpus_buckets,
        noise_health,
        semantic_health,
        index_freshness,
        sidecar_coverage: check_sidecar_coverage(base),
        embedder_warmth,
        empty_body_chunks,
        rebuild_sidecars_script,
        prune_empty_bodies_script,
        fixes_applied,
        overall,
    })
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn check_semantic_health() -> CheckResult {
    let cfg = crate::embedder::EmbeddingConfig::from_env();
    match cfg.backend {
        crate::embedder::BackendPreference::Cloud => {
            let Some(cloud) = cfg.cloud.as_ref() else {
                return CheckResult {
                    name: "semantic_health".to_string(),
                    severity: Severity::Warning,
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
            CheckResult {
                name: "semantic_health".to_string(),
                severity: Severity::Green,
                detail: format!("cloud backend configured: {} ({})", cloud.url, cloud.model),
                recommendation: None,
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
                CheckResult {
                    name: "semantic_health".to_string(),
                    severity: Severity::Green,
                    detail: format!("native model available at {}", path.display()),
                    recommendation: None,
                }
            } else {
                CheckResult {
                    name: "semantic_health".to_string(),
                    severity: Severity::Warning,
                    detail: format!(
                        "native model not found: {}/{}",
                        resolved.repo, resolved.filename
                    ),
                    recommendation: Some(
                        "Hydrate the model cache or switch [embedder].backend = \"cloud\""
                            .to_string(),
                    ),
                }
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

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
fn check_semantic_health() -> CheckResult {
    CheckResult {
        name: "semantic_health".to_string(),
        severity: Severity::Critical,
        detail: "binary built without embedder features".to_string(),
        recommendation: Some("Rebuild with cloud-embedder or native-embedder".to_string()),
    }
}

fn check_index_freshness(base: &Path) -> CheckResult {
    let newest_chunk = newest_mtime(&base.join("store"))
        .into_iter()
        .chain(newest_mtime(&base.join("non-repository-contexts")))
        .max();
    let index_mtime = newest_mtime(&base.join("steer_db"))
        .into_iter()
        .chain(newest_mtime(&base.join("steer_bm25")))
        .max();

    match (newest_chunk, index_mtime) {
        (None, _) => CheckResult {
            name: "index_freshness".to_string(),
            severity: Severity::Green,
            detail: "no canonical chunks found; no index lag".to_string(),
            recommendation: None,
        },
        (Some(_), None) => CheckResult {
            name: "index_freshness".to_string(),
            severity: Severity::Critical,
            detail: "canonical chunks exist but no semantic/steer index mtime was found"
                .to_string(),
            recommendation: Some(
                "Run `aicx index --dry-run` to probe, then rebuild steer metadata".to_string(),
            ),
        },
        (Some(chunk), Some(index)) => {
            let lag = chunk.duration_since(index).unwrap_or(Duration::ZERO);
            let severity = if lag > Duration::from_secs(72 * 3600) {
                Severity::Critical
            } else if lag > Duration::from_secs(24 * 3600) {
                Severity::Warning
            } else {
                Severity::Green
            };
            CheckResult {
                name: "index_freshness".to_string(),
                severity,
                detail: format!("semantic lag: {} seconds", lag.as_secs()),
                recommendation: if severity == Severity::Green {
                    None
                } else {
                    Some(
                        "Run `aicx index --dry-run` and refresh the materialized index".to_string(),
                    )
                },
            }
        }
    }
}

fn newest_mtime(root: &Path) -> Option<SystemTime> {
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
fn check_embedder_warmth() -> CheckResult {
    let cfg = crate::embedder::EmbeddingConfig::from_env();
    if cfg.backend == crate::embedder::BackendPreference::Cloud {
        let local = cfg
            .cloud
            .as_ref()
            .is_some_and(|cloud| is_local_embedder_url(&cloud.url));
        if !local {
            return CheckResult {
                name: "embedder_warmth".to_string(),
                severity: Severity::Green,
                detail: "remote cloud backend: warmth probe skipped to avoid paid/noisy calls"
                    .to_string(),
                recommendation: None,
            };
        }
    }

    CheckResult {
        name: "embedder_warmth".to_string(),
        severity: Severity::Warning,
        detail: "local embedder warmth probe available via `aicx warmup`".to_string(),
        recommendation: Some(
            "Run `aicx warmup` before one-shot semantic search after idle".to_string(),
        ),
    }
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
fn check_embedder_warmth() -> CheckResult {
    CheckResult {
        name: "embedder_warmth".to_string(),
        severity: Severity::Critical,
        detail: "binary built without embedder features".to_string(),
        recommendation: None,
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn is_local_embedder_url(url: &str) -> bool {
    url.contains("localhost:")
        || url.contains("127.0.0.1:")
        || url.contains("0.0.0.0:")
        || url.contains("[::1]:")
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

fn check_empty_body_chunks(base: &Path) -> CheckResult {
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
                "Run `aicx doctor --prune-empty-bodies` to emit a reviewable cleanup script"
                    .to_string(),
            )
        } else {
            None
        },
    }
}

#[derive(Debug, Default)]
struct EmptyBodyReport {
    total: usize,
    empty: usize,
    empty_paths: Vec<PathBuf>,
    sample_paths: Vec<PathBuf>,
    by_frame_kind: BTreeMap<String, usize>,
}

fn empty_body_report(base: &Path) -> EmptyBodyReport {
    let files = store::scan_context_files_at(base).unwrap_or_default();
    let mut report = EmptyBodyReport {
        total: files.len(),
        ..Default::default()
    };

    for file in files {
        if file.path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&file.path) else {
            continue;
        };
        if !chunk_body_is_empty(&content) && chunk_body_after_header(&content).trim().len() >= 50 {
            continue;
        }

        report.empty += 1;
        report.empty_paths.push(file.path.clone());
        if report.sample_paths.len() < 20 {
            report.sample_paths.push(file.path.clone());
        }
        let frame_kind = store::load_sidecar(&file.path)
            .and_then(|sidecar| sidecar.frame_kind.map(|kind| kind.as_str().to_string()))
            .unwrap_or_else(|| "unknown".to_string());
        *report.by_frame_kind.entry(frame_kind).or_insert(0) += 1;
    }

    report
}

fn chunk_body_after_header(content: &str) -> &str {
    let Some(rest) = content.strip_prefix("[project:") else {
        return content;
    };
    let Some((_, body)) = rest.split_once('\n') else {
        return "";
    };
    body.trim_start_matches(['\r', '\n'])
}

fn chunk_body_is_empty(content: &str) -> bool {
    !chunk_body_after_header(content)
        .lines()
        .any(chunk_line_has_signal)
}

fn chunk_line_has_signal(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    if let Some((_, rest)) = line.split_once("] ")
        && let Some((_, message)) = rest.split_once(':')
    {
        return !message.trim().is_empty();
    }
    true
}

pub fn render_prune_empty_bodies_script(base: &Path) -> Result<String> {
    let report = empty_body_report(base);
    let mut out = String::from("#!/usr/bin/env bash\nset -euo pipefail\n\n");
    out.push_str("# Review before running. Generated by `aicx doctor --prune-empty-bodies`.\n");
    for path in report.empty_paths {
        out.push_str("rm -f -- ");
        out.push_str(&shell_quote_path(&path));
        out.push(' ');
        out.push_str(&shell_quote_path(&path.with_extension("meta.json")));
        out.push('\n');
    }
    if !out.contains("rm -f --") {
        out.push_str("# No empty-body chunks detected.\n");
    }
    Ok(out)
}

pub fn render_rebuild_sidecars_script(base: &Path) -> Result<String> {
    let files = store::scan_context_files_at(base).unwrap_or_default();
    let mut out = String::from("#!/usr/bin/env bash\nset -euo pipefail\n\n");
    out.push_str(
        "# Review before running. Rebuilds missing sidecars by forcing a corpus rescan.\n",
    );
    let missing = files
        .iter()
        .filter(|file| !file.path.with_extension("meta.json").exists())
        .take(20)
        .map(|file| file.path.display().to_string())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        out.push_str("# No missing sidecars detected.\n");
    } else {
        for path in missing {
            out.push_str(&format!("# missing: {path}\n"));
        }
        out.push_str("aicx store --full-rescan\n");
    }
    Ok(out)
}

fn shell_quote_path(path: &Path) -> String {
    let value = path.display().to_string();
    format!("'{}'", value.replace('\'', "'\\''"))
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
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).context("create nested quarantine parent")?;
    }
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
        &report.semantic_health,
        &report.index_freshness,
        &report.sidecar_coverage,
        &report.embedder_warmth,
        &report.empty_body_chunks,
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
    if let Some(script) = &report.rebuild_sidecars_script {
        out.push_str("\nRebuild sidecars script:\n");
        out.push_str(script);
    }
    if let Some(script) = &report.prune_empty_bodies_script {
        out.push_str("\nPrune empty bodies script:\n");
        out.push_str(script);
    }
    out
}

pub fn oracle_readiness(report: &DoctorReport) -> OracleReadinessReport {
    let canonical = report.canonical_store.severity;
    let metadata = max_severity(&[report.steer_lance.severity, report.steer_bm25.severity]);
    let content = Severity::Critical;
    let dashboard = Severity::Warning;

    let readiness = if canonical == Severity::Critical
        || report.sidecars.severity == Severity::Critical
        || content == Severity::Critical
    {
        OracleReadiness::UnsafeForLoctreeScope
    } else if metadata != Severity::Green || dashboard != Severity::Green {
        OracleReadiness::Degraded
    } else {
        OracleReadiness::Ready
    };

    let reason = match readiness {
        OracleReadiness::Ready => "canonical corpus, metadata steer index, and semantic route are healthy".to_string(),
        OracleReadiness::Degraded => "oracle usable with explicit degradation; metadata or dashboard route needs attention".to_string(),
        OracleReadiness::UnsafeForLoctreeScope => "content semantic index is unavailable or corpus health is unsafe; Loctree must not use AICX to narrow scope".to_string(),
    };

    OracleReadinessReport {
        readiness,
        readiness_label: match readiness {
            OracleReadiness::Ready => "ready",
            OracleReadiness::Degraded => "degraded",
            OracleReadiness::UnsafeForLoctreeScope => "unsafe_for_loctree_scope",
        },
        canonical_corpus_health: canonical,
        metadata_steer_index_health: metadata,
        content_semantic_index_health: content,
        dashboard_semantic_route_health: dashboard,
        loctree_oracle_readiness: readiness,
        reason,
    }
}

pub fn format_oracle_readiness_text(report: &OracleReadinessReport) -> String {
    format!(
        "canonical corpus health: {:?}\nmetadata steer index health: {:?}\ncontent semantic index health: {:?}\ndashboard semantic route health: {:?}\nLoctree oracle readiness: {}\nreason: {}\n",
        report.canonical_corpus_health,
        report.metadata_steer_index_health,
        report.content_semantic_index_health,
        report.dashboard_semantic_route_health,
        report.readiness_label,
        report.reason
    )
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
    fn oracle_readiness_is_unsafe_without_content_index() {
        let report = DoctorReport {
            canonical_store: CheckResult {
                name: "canonical".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            steer_lance: CheckResult {
                name: "metadata_steer_index lance".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            steer_bm25: CheckResult {
                name: "metadata_steer_index bm25".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            state: CheckResult {
                name: "state".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            sidecars: CheckResult {
                name: "sidecars".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            corpus_buckets: CheckResult {
                name: "buckets".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            noise_health: CheckResult {
                name: "noise".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            semantic_health: CheckResult {
                name: "semantic".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            index_freshness: CheckResult {
                name: "freshness".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            sidecar_coverage: CheckResult {
                name: "sidecar_coverage".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            embedder_warmth: CheckResult {
                name: "warmth".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            empty_body_chunks: CheckResult {
                name: "empty_body_chunks".to_string(),
                severity: Severity::Green,
                detail: "ok".to_string(),
                recommendation: None,
            },
            rebuild_sidecars_script: None,
            prune_empty_bodies_script: None,
            fixes_applied: Vec::new(),
            overall: Severity::Green,
        };

        let readiness = oracle_readiness(&report);
        assert_eq!(readiness.readiness_label, "unsafe_for_loctree_scope");
        assert_eq!(readiness.content_semantic_index_health, Severity::Critical);
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
        std::fs::create_dir_all(store.join("VetCoders").join("vibecrafted.git`")).unwrap();
        std::fs::create_dir_all(store.join("VetCoders").join("loctree\n\n**AICX")).unwrap();
        std::fs::create_dir_all(store.join("{target_owner}")).unwrap();
        std::fs::create_dir_all(store.join("...")).unwrap();

        let result = check_corpus_buckets(&tmp);
        assert_eq!(result.severity, Severity::Warning);
        assert!(result.detail.contains("{target_owner}"));
        assert!(result.detail.contains("..."));
        assert!(result.detail.contains("VetCoders/vibecrafted.git`"));
        assert!(result.detail.contains("VetCoders/loctree"));

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
    fn quarantine_moves_nested_repo_bucket_atomically() {
        let tmp = unique_test_dir("quarantine-nested-move");
        let store = tmp.join("store");
        let bad = store.join("VetCoders").join("vc-skills.git\"><span");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("test.md"), "content").unwrap();

        let dest = quarantine_bucket(&store, "VetCoders/vc-skills.git\"><span").unwrap();
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

    #[test]
    fn empty_body_chunks_red_when_over_threshold_and_script_is_reviewable() {
        let tmp = unique_test_dir("empty-bodies");
        let dir = tmp
            .join("store")
            .join("VetCoders")
            .join("aicx")
            .join("2026_0506")
            .join("conversations")
            .join("claude");
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("2026_0506_claude_sess-empty_001.md");
        let full = dir.join("2026_0506_claude_sess-full_001.md");
        std::fs::write(
            &empty,
            "[project: VetCoders/aicx | agent: claude | date: 2026-05-06 | frame_kind: internal_thought]\n\n",
        )
        .unwrap();
        std::fs::write(
            &full,
            "[project: VetCoders/aicx | agent: claude | date: 2026-05-06]\n\nThis chunk carries enough real body content to avoid the empty-body threshold.",
        )
        .unwrap();

        let check = check_empty_body_chunks(&tmp);
        assert_eq!(check.severity, Severity::Critical);
        assert!(check.detail.contains("1 empty-body"));

        let script = render_prune_empty_bodies_script(&tmp).unwrap();
        assert!(script.starts_with("#!/usr/bin/env bash"));
        assert!(script.contains("rm -f --"));
        assert!(script.contains("sess-empty"));
        assert!(!script.contains("sess-full"));
        assert!(empty.exists(), "script generation must not delete files");

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
