// 𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
//! Optional read-only stages on the filtered, published search result pool.

mod config;
mod decision;
mod http;
mod rerank;

pub use config::QualityConfig;

use std::{path::Path, time::Instant};

use aicx_retrieve::{RequestedMode, RetrievalEvidence, RetrievalOutcome};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    evidence::{EvidenceReport, build_evidence_report},
    rank::FuzzyResult,
    search_engine::{PublishedHits, PublishedSearch, finalize_fuzzy_results},
};
use config::Setting;

const QUERY_BYTES: usize = 4096;

fn bounded(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn document(hit: &FuzzyResult, bytes: usize) -> String {
    let filename = Path::new(&hit.file)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let mut text = String::with_capacity(bytes);
    let metadata = [
        hit.project.as_str(),
        hit.kind.as_str(),
        hit.date.as_str(),
        filename,
        hit.label.as_str(),
    ];
    let lines = hit.matched_lines.iter().map(String::as_str).chain(metadata);
    for line in lines.filter(|line| !line.is_empty()) {
        if !text.is_empty() {
            text.push('\n');
        }
        let part = bounded(line, bytes - text.len());
        text.push_str(part);
        if part.len() < line.len() || text.len() == bytes {
            break;
        }
    }
    text
}

#[derive(Serialize)]
struct Stage {
    status: &'static str,
    model: Option<String>,
    elapsed_ms: u128,
    candidate_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_detail: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    assessment: Option<decision::Assessment>,
}

impl Stage {
    fn new(setting: &Setting) -> Self {
        let (status, model, detail) = match setting {
            Setting::Disabled => ("disabled", None, None),
            Setting::LocalReranker => ("skipped", None, None),
            Setting::Configured(e) => ("skipped", Some(e.model.clone()), None),
            Setting::Invalid(msg) => ("failed", None, Some(msg.clone())),
        };
        Self {
            status,
            model,
            elapsed_ms: 0,
            candidate_count: 0,
            failure_detail: detail,
            assessment: None,
        }
    }

    fn skip(&mut self, reason: &str) {
        if self.status == "skipped" {
            self.failure_detail = Some(reason.to_owned());
        }
    }

    fn finish<T>(&mut self, start: Instant, result: anyhow::Result<T>) -> Option<T> {
        self.elapsed_ms = start.elapsed().as_millis();
        match result {
            Ok(value) => {
                self.status = "applied";
                Some(value)
            }
            Err(err) => {
                self.status = "failed";
                self.failure_detail = Some(err.to_string());
                None
            }
        }
    }
}

async fn rank(setting: &Setting, query: &str, hits: &mut [FuzzyResult]) -> Stage {
    let mut stage = Stage::new(setting);
    if let Setting::Configured(endpoint) = setting {
        stage.candidate_count = hits.len().min(endpoint.max_candidates);
        if stage.candidate_count > 0 {
            let start = Instant::now();
            let result = rerank::reorder(endpoint, query, &mut hits[..stage.candidate_count]).await;
            let _ = stage.finish(start, result);
        }
    }
    stage
}

#[derive(Serialize)]
struct QualityReport {
    original_query: String,
    executed_query: String,
    attempts: u8,
    rerank: Stage,
    decision: Stage,
    // The decision cites the first pass even when the returned results came from refinement.
    served_evidence: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_rerank: Option<Stage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_dense_failure: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refinement_failure: Option<String>,
}

impl QualityReport {
    fn new(config: &QualityConfig, query: &str) -> Self {
        Self {
            original_query: query.to_owned(),
            executed_query: query.to_owned(),
            attempts: 0,
            rerank: Stage::new(&config.reranker),
            decision: Stage::new(&config.decision),
            served_evidence: Vec::new(),
            initial_rerank: None,
            initial_dense_failure: None,
            refinement_failure: None,
        }
    }
}

/// Both renderers use this envelope. With no diagnostics, JSON passes through verbatim.
pub struct SearchTelemetry {
    quality: Option<QualityReport>,
    dense_failure: Option<&'static str>,
}

impl SearchTelemetry {
    pub fn new(config: &QualityConfig, query: &str) -> Self {
        Self {
            quality: config
                .has_explicit_config()
                .then(|| QualityReport::new(config, query)),
            dense_failure: None,
        }
    }

    pub fn record_base(&mut self, hits: &PublishedHits) {
        self.dense_failure = hits.dense_failure;
    }

    pub fn skip(&mut self, reason: &str) {
        if let Some(q) = self.quality.as_mut() {
            q.rerank.skip(reason);
            q.decision.skip(reason);
        }
    }

    pub async fn apply(
        &mut self,
        config: &QualityConfig,
        query: &str,
        scope: &PublishedSearch,
        base: &mut PublishedHits,
    ) {
        self.record_base(base);
        let dense = base.used_semantic();
        if !dense && !config.rerank_requested && !config.decision_requested {
            self.skip("serving published Tantivy results");
            return;
        }
        let hits = &mut base.filtered.outcome.results;
        if !config.active() || (hits.is_empty() && self.quality.is_none()) {
            return;
        }
        let q = self
            .quality
            .get_or_insert_with(|| QualityReport::new(config, query));
        let local =
            if dense && !hits.is_empty() && matches!(config.reranker, Setting::LocalReranker) {
                Some(match http::discover_local_reranker().await {
                    Ok(endpoint) => Setting::Configured(endpoint),
                    Err(error) => Setting::Invalid(error.to_string()),
                })
            } else {
                None
            };
        let reranker = if dense || config.rerank_requested {
            let reranker = local.as_ref().unwrap_or(&config.reranker);
            q.rerank = rank(reranker, query, hits).await;
            reranker
        } else {
            q.rerank.skip("serving published Tantivy results");
            &Setting::Disabled
        };
        if !config.decision_requested && q.rerank.status != "applied" {
            q.decision.skip("reranking did not complete");
            return;
        }
        let Setting::Configured(endpoint) = &config.decision else {
            return;
        };
        let candidates = &hits[..hits.len().min(endpoint.max_candidates)];
        q.decision.candidate_count = candidates.len();
        q.served_evidence = candidates
            .iter()
            .enumerate()
            .map(|(i, hit)| {
                json!({
                    "id": decision::evidence_id(i), "path": hit.path, "label": hit.label
                })
            })
            .collect();
        let start = Instant::now();
        let answer = decision::assess(endpoint, query, candidates).await;
        q.decision.assessment = q.decision.finish(start, answer);
        if candidates.is_empty() {
            // Local evidence is conclusive only about the absence of candidates.
            q.decision.status = "skipped";
        }
        let refined = q
            .decision
            .assessment
            .as_ref()
            .and_then(|a| a.refined_query.as_deref());
        let Some(refined) = refined.filter(|_| endpoint.max_refinements > 0) else {
            return;
        };
        // Only the frozen retrieval and rerank stages run on the second pass.
        q.attempts = 1;
        let refined_query = refined.trim().to_owned();
        let request = refined_query.clone();
        let mut frozen_scope = scope.clone();
        // An unavailable embedder already failed on this request. Refine on
        // the same published lexical backend without paying its timeout twice.
        frozen_scope.embedder_cached_unavailable |=
            base.dense_failure == Some("embedder_unavailable");
        let result = tokio::task::spawn_blocking(move || frozen_scope.retrieve(&request)).await;
        let mut next = match result {
            Ok(Ok(next)) if !next.filtered.outcome.results.is_empty() => next,
            Ok(Ok(_)) => {
                q.refinement_failure = Some("refinement returned no matching candidates".into());
                return;
            }
            Ok(Err(error)) => {
                q.refinement_failure = Some(format!("refinement failed ({})", error.kind()));
                return;
            }
            Err(_) => {
                q.refinement_failure = Some("refinement worker failed".into());
                return;
            }
        };
        if dense && !next.used_semantic() {
            q.refinement_failure = Some(format!(
                "refinement served Tantivy ({})",
                next.dense_failure
                    .unwrap_or("semantic retrieval unavailable")
            ));
            return;
        }
        let next_rank = rank(reranker, &refined_query, &mut next.filtered.outcome.results).await;
        if q.rerank.status == "applied" && next_rank.status != "applied" {
            q.refinement_failure = next_rank.failure_detail;
            return;
        }
        q.initial_rerank = Some(std::mem::replace(&mut q.rerank, next_rank));
        q.initial_dense_failure = self.dense_failure;
        self.dense_failure = match (self.dense_failure, next.dense_failure) {
            (Some("embedder_unavailable"), Some("embedder_negative_cache")) => {
                Some("embedder_unavailable")
            }
            (_, current) => current,
        };
        q.executed_query = refined_query;
        *base = next;
    }

    pub fn attach(&self, payload: String) -> serde_json::Result<String> {
        if self.quality.is_none() && self.dense_failure.is_none() {
            return Ok(payload);
        }
        let mut value: Value = serde_json::from_str(&payload)?;
        if let Some(quality) = &self.quality {
            value["retrieval_quality"] = serde_json::to_value(quality)?;
        }
        if let Some(kind) = self.dense_failure {
            value["dense_fallback"] =
                json!({"status": "failed", "failure_detail": kind, "backend": "lexical_tantivy"});
        }
        serde_json::to_string(&value)
    }

    pub fn retrieval(&self, base: RetrievalOutcome) -> RetrievalOutcome {
        let Some(kind) = self.dense_failure else {
            return base;
        };
        RetrievalOutcome::from_evidence(
            RequestedMode::Hybrid,
            RetrievalEvidence {
                executed_path: Some(base.executed_path),
                examined_count: Some(base.examined_count),
                matched_count: base.matched_count,
                stale_evidence: base.stale_evidence,
                fallback_reason: Some(format!("{kind}: served published Tantivy results")),
            },
        )
    }

    pub fn print_summary(&self) {
        if let Some(kind) = self.dense_failure {
            eprintln!("search: dense retrieval failed ({kind}); serving published Tantivy results");
        }
        if let Some(q) = &self.quality {
            let details: Vec<_> = [
                &q.rerank.failure_detail,
                &q.decision.failure_detail,
                &q.refinement_failure,
            ]
            .into_iter()
            .filter_map(|s| s.as_deref())
            .collect();
            let verdict = q
                .decision
                .assessment
                .as_ref()
                .map(|a| match a.verdict {
                    decision::Verdict::Sufficient => "sufficient",
                    decision::Verdict::Insufficient => "insufficient",
                    decision::Verdict::Refine => "refine",
                })
                .unwrap_or("unassessed");
            eprintln!(
                "retrieval quality: rerank={} decision={} verdict={} refinements={} {}",
                q.rerank.status,
                q.decision.status,
                verdict,
                q.attempts,
                details.join(" / ")
            );
        }
    }

    fn reranked(&self) -> bool {
        self.quality
            .as_ref()
            .is_some_and(|q| q.rerank.status == "applied")
    }

    pub fn finalize(
        &self,
        mut hits: Vec<FuzzyResult>,
        kind: Option<&str>,
        sort: Option<&str>,
        limit: usize,
    ) -> Vec<FuzzyResult> {
        if self.reranked() && matches!(sort, None | Some("score")) {
            hits.retain(|hit| kind.is_none_or(|k| hit.kind == k));
            hits.truncate(limit);
            hits
        } else {
            finalize_fuzzy_results(hits, kind, sort, limit)
        }
    }

    pub fn evidence(
        &self,
        query: &str,
        mut hits: Vec<FuzzyResult>,
        limit: usize,
    ) -> EvidenceReport {
        let executed = self
            .quality
            .as_ref()
            .map_or(query, |q| q.executed_query.as_str());
        if !self.reranked() {
            return build_evidence_report(executed, hits, limit);
        }
        // Apply evidence scoring/dedup to the selected cross-encoder hits.
        // Keep evidence labels and scores, then restore the admitted order.
        hits.truncate(limit.max(1));
        let order: std::collections::HashMap<_, _> = hits
            .iter()
            .enumerate()
            .map(|(i, h)| (h.path.clone(), i))
            .collect();
        let mut report = build_evidence_report(executed, hits, limit);
        report
            .items
            .sort_by_key(|item| order.get(&item.path).copied().unwrap_or(usize::MAX));
        report
    }
}
