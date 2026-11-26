// 𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by Vetcoders (c)2024-2026 LibraxisAI

use anyhow::{Result, anyhow, ensure};
use serde::Deserialize;
use serde_json::json;

use super::{checked_query, config::Endpoint, document, http};
use crate::rank::FuzzyResult;

#[derive(Deserialize)]
struct Ranked {
    index: usize,
    score: Option<f64>,
    relevance_score: Option<f64>,
}

#[derive(Deserialize)]
struct Response {
    results: Option<Vec<Ranked>>,
    data: Option<Vec<Ranked>>,
}

pub(super) async fn reorder(
    endpoint: &Endpoint,
    query: &str,
    pool: &mut [FuzzyResult],
) -> Result<()> {
    let documents: Vec<_> = pool
        .iter()
        .map(|hit| document(hit, endpoint.max_doc_bytes))
        .collect::<Result<Vec<_>>>()?;
    let response: Response = http::post(
        endpoint,
        &json!({
            "model": endpoint.model, "query": checked_query(query)?,
            // Reranker servers use both names for the same requested limit.
            // MLX ignores top_n and otherwise silently returns only ten rows.
            "documents": documents, "top_n": pool.len(), "top_k": pool.len(),
        }),
    )
    .await?;
    let rows = response
        .results
        .or(response.data)
        .ok_or_else(|| anyhow!("missing rerank results"))?;
    ensure!(
        rows.len() == pool.len(),
        "reranker returned an incomplete permutation"
    );
    let mut scores = vec![None; pool.len()];
    for row in rows {
        let score = row
            .relevance_score
            .or(row.score)
            .filter(|s| s.is_finite())
            .ok_or_else(|| anyhow!("missing or invalid rerank score"))?;
        let slot = scores
            .get_mut(row.index)
            .ok_or_else(|| anyhow!("rerank index out of range"))?;
        ensure!(slot.replace(score).is_none(), "duplicate rerank index");
    }

    // Stable ties follow the original local order, even if the server shuffled its rows.
    // Validate the complete mapping before moving a single candidate.
    let mut order: Vec<_> = (0..pool.len()).collect();
    order.sort_by(|&a, &b| {
        scores[b]
            .partial_cmp(&scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut destination = vec![0; pool.len()];
    for (rank, index) in order.into_iter().enumerate() {
        destination[index] = rank;
    }
    for i in 0..pool.len() {
        while destination[i] != i {
            let j = destination[i];
            pool.swap(i, j);
            destination.swap(i, j);
        }
    }
    Ok(())
}
