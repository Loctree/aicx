// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
use aicx_retrieve::{FusionStrategy, Hit, RRF_K_DEFAULT, ReciprocalRankFusion};
use serde_json::json;

fn hit(id: &str, rank: usize, source: &str) -> Hit {
    Hit {
        chunk_id: id.to_string(),
        score: 100.0 - rank as f32,
        rank,
        source: source.to_string(),
        metadata: json!({ "source": source, "rank": rank }),
    }
}

fn rrf(k: u32, rank: usize) -> f32 {
    1.0 / (k as f32 + (rank + 1) as f32)
}

#[test]
fn rrf_canonical_formula() {
    let lex = vec![hit("a", 0, "lex"), hit("b", 1, "lex"), hit("c", 2, "lex")];
    let dense = vec![
        hit("b", 0, "dense"),
        hit("c", 1, "dense"),
        hit("d", 2, "dense"),
    ];

    let fused = ReciprocalRankFusion::default().fuse(lex, dense, 10);
    let score = |id: &str| fused.iter().find(|hit| hit.chunk_id == id).unwrap().score;

    assert!((score("b") - (rrf(RRF_K_DEFAULT, 1) + rrf(RRF_K_DEFAULT, 0))).abs() < 1e-7);
    assert!((score("c") - (rrf(RRF_K_DEFAULT, 2) + rrf(RRF_K_DEFAULT, 1))).abs() < 1e-7);
    assert!((score("a") - rrf(RRF_K_DEFAULT, 0)).abs() < 1e-7);
    assert!((score("d") - rrf(RRF_K_DEFAULT, 2)).abs() < 1e-7);
    assert_eq!(fused[0].chunk_id, "b");
    assert_eq!(fused[0].rank, 0);
    assert_eq!(fused[0].source, "rrf");
}

#[test]
fn rrf_k_sensitivity() {
    let lex = vec![hit("a", 0, "lex"), hit("b", 1, "lex"), hit("c", 2, "lex")];
    let dense = vec![
        hit("a", 0, "dense"),
        hit("b", 1, "dense"),
        hit("c", 2, "dense"),
    ];

    let low_k = ReciprocalRankFusion::with_k(10).fuse(lex.clone(), dense.clone(), 3);
    let default_k = ReciprocalRankFusion::with_k(60).fuse(lex, dense, 3);

    assert_eq!(
        low_k.iter().map(|hit| &hit.chunk_id).collect::<Vec<_>>(),
        default_k
            .iter()
            .map(|hit| &hit.chunk_id)
            .collect::<Vec<_>>()
    );
    assert!(low_k[0].score > default_k[0].score);
}

#[test]
fn rrf_disjoint_inputs() {
    let lex = vec![hit("a", 0, "lex"), hit("b", 1, "lex"), hit("c", 2, "lex")];
    let dense = vec![
        hit("d", 0, "dense"),
        hit("e", 1, "dense"),
        hit("f", 2, "dense"),
    ];

    let fused = ReciprocalRankFusion::default().fuse(lex, dense, 10);

    assert_eq!(fused.len(), 6);
    assert_eq!(fused[0].chunk_id, "a");
    assert_eq!(fused[1].chunk_id, "d");
    assert!(fused[0].score >= fused[1].score);
    assert_eq!(
        fused.iter().map(|hit| hit.rank).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5]
    );
}
