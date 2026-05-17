#![cfg(feature = "e2e-aicx")]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Deserialize)]
struct QueriesConfig {
    queries: Vec<QueryConfig>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct QueryConfig {
    id: String,
    category: String,
    query: String,
    expected_top_3_paths: Vec<String>,
    expected_minimum_recall_at_5: f64,
    notes: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
struct BaselineMetric {
    recall_at_5: f64,
    ndcg_at_10: f64,
    latency_ms: u128,
}

#[derive(Serialize, Deserialize, Default)]
struct BaselineData {
    metadata: String,
    aggregate: BaselineMetric,
    queries: HashMap<String, BaselineMetric>,
}

fn calculate_ndcg(results: &[aicx::rank::FuzzyResult], expected: &[String], k: usize) -> f64 {
    let mut dcg = 0.0;
    for (i, hit) in results.iter().take(k).enumerate() {
        let path_str = hit.path.clone();
        if expected
            .iter()
            .any(|e| path_str.contains(e) || e.contains(&path_str))
        {
            let rel = 1.0;
            dcg += rel / ((i + 2) as f64).log2();
        }
    }

    let mut idcg = 0.0;
    let ideal_hits = std::cmp::min(k, expected.len());
    for i in 0..ideal_hits {
        let rel = 1.0;
        idcg += rel / ((i + 2) as f64).log2();
    }

    if idcg == 0.0 {
        return 0.0;
    }
    dcg / idcg
}

#[test]
fn eval_harness() {
    let queries_path = PathBuf::from("tests/retrieval_eval/queries.toml");
    let content = fs::read_to_string(&queries_path).expect("failed to read queries.toml");
    let config: QueriesConfig = toml::from_str(&content).expect("failed to parse TOML");

    assert_eq!(config.queries.len(), 50, "Expected exactly 50 queries");

    let baseline_path = PathBuf::from("tests/retrieval_eval/baseline.json");
    let previous_baseline: Option<BaselineData> = if baseline_path.exists() {
        let content = fs::read_to_string(&baseline_path).unwrap();
        Some(serde_json::from_str(&content).unwrap())
    } else {
        None
    };

    let mut current_data = BaselineData {
        metadata: "Baseline measured: production hybrid_rrf retrieval".to_string(),
        aggregate: BaselineMetric::default(),
        queries: HashMap::new(),
    };

    let mut total_recall = 0.0;
    let mut total_ndcg = 0.0;
    let mut latencies = Vec::new();

    for q in &config.queries {
        let start = Instant::now();
        let results = aicx::search_engine::try_semantic_search(
            std::path::Path::new(""),
            &q.query,
            10,
            &[None],
            None,
            None,
        )
        .map(|outcome| {
            assert_eq!(
                outcome.backend_label, "hybrid_rrf",
                "retrieval eval must exercise the production hybrid path"
            );
            assert!(
                outcome.retrieval_status.is_some(),
                "retrieval eval must observe a committed hybrid manifest"
            );
            outcome.results
        })
        .unwrap_or_else(|err| {
            panic!(
                "retrieval eval requires a live hybrid index: kind={} reason={}; recommendation={}",
                err.kind(),
                err.reason(),
                err.recommendation()
            )
        });
        let latency = start.elapsed().as_millis();

        let mut hits = 0;
        for expected in &q.expected_top_3_paths {
            let found = results.iter().take(5).any(|hit| {
                let p = hit.path.clone();
                p.contains(expected) || expected.contains(&p)
            });
            if found {
                hits += 1;
            }
        }

        let recall = if q.expected_top_3_paths.is_empty() {
            0.0
        } else {
            hits as f64 / q.expected_top_3_paths.len() as f64
        };

        let ndcg = calculate_ndcg(&results, &q.expected_top_3_paths, 10);

        current_data.queries.insert(
            q.id.clone(),
            BaselineMetric {
                recall_at_5: recall,
                ndcg_at_10: ndcg,
                latency_ms: latency,
            },
        );

        total_recall += recall;
        total_ndcg += ndcg;
        latencies.push(latency);
    }

    latencies.sort_unstable();
    let p95_idx = (latencies.len() as f64 * 0.95) as usize;
    let p95_latency = latencies.get(p95_idx).copied().unwrap_or(0);

    let n = config.queries.len() as f64;
    current_data.aggregate.recall_at_5 = total_recall / n;
    current_data.aggregate.ndcg_at_10 = total_ndcg / n;
    current_data.aggregate.latency_ms = p95_latency;

    if let Some(prev) = previous_baseline {
        let drop = prev.aggregate.recall_at_5 - current_data.aggregate.recall_at_5;
        if drop > 0.05 {
            panic!(
                "Regression detected! Recall dropped from {:.3} to {:.3}",
                prev.aggregate.recall_at_5, current_data.aggregate.recall_at_5
            );
        }
        println!(
            "Pass: Recall {:.3} (prev: {:.3}), NDCG {:.3}, p95 {:.3}ms",
            current_data.aggregate.recall_at_5,
            prev.aggregate.recall_at_5,
            current_data.aggregate.ndcg_at_10,
            p95_latency
        );
    } else {
        println!("No previous baseline. Establishing new baseline.");
    }

    let json = serde_json::to_string_pretty(&current_data).unwrap();
    fs::write(baseline_path, json).unwrap();
}
