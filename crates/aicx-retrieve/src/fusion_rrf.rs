// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
use std::collections::BTreeMap;

use crate::{FusionStrategy, Hit};

pub const RRF_K_DEFAULT: u32 = 60;
pub const RRF_NAME: &str = "rrf";

/// Reciprocal Rank Fusion over lexical and dense results.
///
/// When the same chunk appears in both rankers, metadata maps are merged with
/// lexical metadata taking precedence on key conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReciprocalRankFusion {
    pub k: u32,
}

impl ReciprocalRankFusion {
    pub fn with_k(k: u32) -> Self {
        Self { k }
    }
}

impl Default for ReciprocalRankFusion {
    fn default() -> Self {
        Self { k: RRF_K_DEFAULT }
    }
}

impl FusionStrategy for ReciprocalRankFusion {
    fn fuse(&self, lex: Vec<Hit>, dense: Vec<Hit>, limit: usize) -> Vec<Hit> {
        if limit == 0 {
            return Vec::new();
        }

        let mut fused: BTreeMap<String, FusedHit> = BTreeMap::new();

        for hit in dense {
            add_hit(&mut fused, hit, self.k, false);
        }
        for hit in lex {
            add_hit(&mut fused, hit, self.k, true);
        }

        let mut hits: Vec<_> = fused
            .into_values()
            .map(|hit| Hit {
                chunk_id: hit.chunk_id,
                score: hit.score,
                rank: 0,
                source: RRF_NAME.to_string(),
                metadata: hit.metadata,
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.chunk_id.cmp(&b.chunk_id))
        });
        hits.truncate(limit);
        for (rank, hit) in hits.iter_mut().enumerate() {
            hit.rank = rank;
        }
        hits
    }

    fn name(&self) -> &str {
        RRF_NAME
    }
}

struct FusedHit {
    chunk_id: String,
    score: f32,
    metadata: serde_json::Value,
}

fn add_hit(fused: &mut BTreeMap<String, FusedHit>, hit: Hit, k: u32, lexical_precedence: bool) {
    let contribution = 1.0 / (k as f32 + (hit.rank + 1) as f32);
    let entry = fused
        .entry(hit.chunk_id.clone())
        .or_insert_with(|| FusedHit {
            chunk_id: hit.chunk_id.clone(),
            score: 0.0,
            metadata: hit.metadata.clone(),
        });
    entry.score += contribution;
    if lexical_precedence {
        entry.metadata = merge_metadata_lexical_first(hit.metadata, entry.metadata.clone());
    }
}

fn merge_metadata_lexical_first(
    lexical: serde_json::Value,
    existing: serde_json::Value,
) -> serde_json::Value {
    match (existing, lexical) {
        (serde_json::Value::Object(mut dense), serde_json::Value::Object(lex)) => {
            for (key, value) in lex {
                dense.insert(key, value);
            }
            serde_json::Value::Object(dense)
        }
        (_, lexical) => lexical,
    }
}
