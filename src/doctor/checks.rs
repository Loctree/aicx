//! Diagnostic checks and the doctor run orchestrator.
//!
//! `run` / `run_at` execute every integrity check, apply requested
//! remediations (steer rebuild, bucket quarantine, empty-body
//! quarantine), then re-check and aggregate an overall severity.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::sanitize;
use crate::steer_index;
use crate::store;
use crate::validation::{is_valid_repo_bucket_name, is_valid_repo_project_slug};

use super::quarantine::{
    apply_empty_body_quarantine, empty_body_report, quarantine_bucket,
    quarantine_bucket_with_timestamp, render_prune_empty_bodies_script,
    render_rebuild_sidecars_script,
};
use super::report::max_severity;
use super::types::{CheckResult, DoctorOptions, DoctorReport, Severity, doctor_home_label};

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
    let mut semantic_health = check_semantic_health(opts);
    let mut index_freshness = check_index_freshness(base);
    let mut index_consistency = check_index_consistency(base);
    let mut embedder_warmth = check_embedder_warmth(opts);
    let mut empty_body_chunks = check_empty_body_chunks(base);
    let mut content_dedup = if opts.check_dedup {
        check_content_dedup(base)
    } else {
        CheckResult {
            name: "content_dedup".to_string(),
            severity: Severity::Green,
            detail: "not requested".to_string(),
            recommendation: None,
        }
    };
    let mut context_corpus = check_context_corpus(base);

    let mut fixes_applied = Vec::new();
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

    if opts.rebuild_steer_index || opts.fix_buckets || apply_empty_bodies {
        canonical_store = check_canonical_store(base);
        steer_lance = check_steer_lance(base).await;
        steer_bm25 = check_steer_bm25(base);
        state = check_state(base);
        sidecars = check_sidecar_coverage(base);
        corpus_buckets = check_corpus_buckets(base);
        noise_health = check_noise_health(base);
        semantic_health = check_semantic_health(opts);
        index_freshness = check_index_freshness(base);
        index_consistency = check_index_consistency(base);
        embedder_warmth = check_embedder_warmth(opts);
        empty_body_chunks = check_empty_body_chunks(base);
        content_dedup = if opts.check_dedup {
            check_content_dedup(base)
        } else {
            content_dedup
        };
        context_corpus = check_context_corpus(base);
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

    Ok(DoctorReport {
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
        rebuild_sidecars_script,
        prune_empty_bodies_script,
        fixes_applied,
        overall,
    })
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
    CheckResult {
        name: "canonical_store".to_string(),
        severity: Severity::Green,
        detail: format!("{} chunk files indexed", files.len()),
        recommendation: None,
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
/// legitimate chunks across `LibraxisAI/`, `VetCoders/`, `Loctree/`,
/// `Szowesgad/`. Relaxing the validator (not adding canonicalization
/// magic) was the correct response.
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
