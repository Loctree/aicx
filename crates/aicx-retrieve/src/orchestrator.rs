// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};

use crate::{
    ChunkRef, DenseChunkRef, DenseIndex, ExecutedPath, FilterSet, FusionStrategy, Hit,
    LexicalIndex, LexicalQuery, Manifest, RequestedMode, RetrievalEvidence, RetrievalOutcome,
    RetrieveError,
};

pub struct HybridIndex {
    lexical: Box<dyn LexicalIndex>,
    dense: Box<dyn DenseIndex>,
    fusion: Box<dyn FusionStrategy>,
    manifest_dir: PathBuf,
    manifest: Option<Manifest>,
    embedder_fingerprint: EmbedderFingerprint,
}

impl std::fmt::Debug for HybridIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HybridIndex")
            .field("manifest_dir", &self.manifest_dir)
            .field("manifest", &self.manifest)
            .field("embedder_fingerprint", &self.embedder_fingerprint)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedderFingerprint {
    pub model: String,
    pub url_hash: String,
    pub dim: usize,
    pub distance: String,
}

impl EmbedderFingerprint {
    pub fn new(
        model: impl Into<String>,
        endpoint_url: &str,
        dim: usize,
        distance: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            url_hash: hash_endpoint_url(endpoint_url),
            dim,
            distance: distance.into(),
        }
    }

    pub fn from_hash(
        model: impl Into<String>,
        url_hash: impl Into<String>,
        dim: usize,
        distance: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            url_hash: url_hash.into(),
            dim,
            distance: distance.into(),
        }
    }
}

pub struct HybridQueryInput<'a> {
    pub query_text: &'a str,
    pub query_embedding: &'a [f32],
    pub filters: FilterSet,
    pub limit: usize,
}

/// Default hard ceiling for adapters that cannot prove full metadata
/// pushdown. The orchestrator refills deterministically up to this many
/// globally-ranked candidates; reaching the ceiling is explicit saturation,
/// never silent empty/exhaustion.
pub const DEFAULT_FILTER_REFILL_BUDGET: usize = 1_024;

/// Hybrid query result plus the evidence needed to distinguish a complete
/// empty result from bounded under-delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridQueryResult {
    pub hits: Vec<Hit>,
    pub examined_count: usize,
    pub exhausted: bool,
    pub retrieval_outcome: RetrievalOutcome,
}

impl HybridIndex {
    pub fn new(
        lexical: Box<dyn LexicalIndex>,
        dense: Box<dyn DenseIndex>,
        fusion: Box<dyn FusionStrategy>,
        manifest_dir: impl Into<PathBuf>,
        embedder_fingerprint: EmbedderFingerprint,
    ) -> Self {
        Self {
            lexical,
            dense,
            fusion,
            manifest_dir: manifest_dir.into(),
            manifest: None,
            embedder_fingerprint,
        }
    }

    pub fn build_hybrid(
        &mut self,
        lexical_chunks: &[ChunkRef],
        dense_chunks: &[DenseChunkRef],
        source_hash: &str,
    ) -> Result<()> {
        let started = Manifest::now_utc();
        let timer = Instant::now();
        let lexical_commit_id = self.lexical.build(lexical_chunks)?;
        self.dense.build(dense_chunks)?;
        let completed = Manifest::now_utc();

        self.manifest = Some(Manifest {
            schema_version: "2.0".to_string(),
            generation_id: Manifest::fresh_generation_id(),
            source_chunk_count: lexical_chunks.len(),
            source_hash_blake3: source_hash_blake3(source_hash),
            embedder_model: self.embedder_fingerprint.model.clone(),
            embedder_url_hash: self.embedder_fingerprint.url_hash.clone(),
            embedder_dim: self.embedder_fingerprint.dim,
            embedder_distance: self.embedder_fingerprint.distance.clone(),
            dense_count: self.dense.count(),
            dense_kind: self.dense.kind().to_string(),
            lexical_commit_id: lexical_commit_id.0,
            lexical_doc_count: self.lexical.doc_count(),
            build_started_at: started,
            build_completed_at: completed,
            build_wall_seconds: timer.elapsed().as_secs(),
            fusion_algorithm: self.fusion.name().to_string(),
            fusion_k: fusion_k(self.fusion.name()),
        });
        Ok(())
    }

    pub fn commit(&mut self) -> Result<&Manifest> {
        let prior = self
            .manifest
            .as_ref()
            .ok_or_else(|| anyhow!("cannot commit hybrid index before build_hybrid"))?
            .clone();
        let completed = Manifest::now_utc();
        let wall_seconds = completed
            .signed_duration_since(prior.build_started_at)
            .num_seconds()
            .max(0) as u64;
        self.manifest = Some(Manifest {
            schema_version: prior.schema_version,
            generation_id: Manifest::fresh_generation_id(),
            source_chunk_count: prior.source_chunk_count,
            source_hash_blake3: prior.source_hash_blake3,
            embedder_model: self.embedder_fingerprint.model.clone(),
            embedder_url_hash: self.embedder_fingerprint.url_hash.clone(),
            embedder_dim: self.embedder_fingerprint.dim,
            embedder_distance: self.embedder_fingerprint.distance.clone(),
            dense_count: self.dense.count(),
            dense_kind: self.dense.kind().to_string(),
            lexical_commit_id: self.lexical.commit_id().0.clone(),
            lexical_doc_count: self.lexical.doc_count(),
            build_started_at: prior.build_started_at,
            build_completed_at: completed,
            build_wall_seconds: wall_seconds,
            fusion_algorithm: self.fusion.name().to_string(),
            fusion_k: fusion_k(self.fusion.name()),
        });
        let manifest = self
            .manifest
            .as_ref()
            .ok_or_else(|| anyhow!("cannot commit hybrid index before build_hybrid"))?;
        std::fs::create_dir_all(&self.manifest_dir)?;
        manifest.write_to_path(&self.manifest_path())?;
        Ok(manifest)
    }

    /// Load a published hybrid generation.
    ///
    /// `observed_source_hash` is optional. When present it is blake3-hashed and
    /// compared to the manifest (build-time integrity). When `None`, source-hash
    /// revalidation is skipped — still validating lexical commit, dense count,
    /// dense kind, and embedder fingerprint. Search hot paths pass `None` so a
    /// multi-GB primary NDJSON is never re-hashed on every query.
    pub fn load_from_manifest(
        lexical: Box<dyn LexicalIndex>,
        dense: Box<dyn DenseIndex>,
        fusion: Box<dyn FusionStrategy>,
        manifest_dir: impl Into<PathBuf>,
        embedder_fingerprint: EmbedderFingerprint,
        observed_source_hash: Option<&str>,
    ) -> Result<Self> {
        let manifest_dir = manifest_dir.into();
        let manifest = Manifest::read_from_path(&manifest_dir.join("manifest.json"))?;
        validate_bindings(
            &manifest,
            lexical.as_ref(),
            dense.as_ref(),
            fusion.as_ref(),
            &embedder_fingerprint,
            observed_source_hash,
        )?;
        Ok(Self {
            lexical,
            dense,
            fusion,
            manifest_dir,
            manifest: Some(manifest),
            embedder_fingerprint,
        })
    }

    pub fn query_hybrid(&self, input: HybridQueryInput<'_>) -> Result<Vec<Hit>> {
        Ok(self
            .query_hybrid_with_budget(input, DEFAULT_FILTER_REFILL_BUDGET)?
            .hits)
    }

    /// Execute hybrid retrieval with deterministic bounded refill for the
    /// lexical leg. Dense adapters honor the trait's filter-before-distance
    /// contract directly. Lexical adapters are treated conservatively: with
    /// active filters the orchestrator requests cumulative global windows,
    /// applies every exact metadata predicate locally, and doubles the window
    /// until the requested count, actual exhaustion, or the explicit budget.
    pub fn query_hybrid_with_budget(
        &self,
        input: HybridQueryInput<'_>,
        candidate_budget: usize,
    ) -> Result<HybridQueryResult> {
        self.query_hybrid_with_budget_and_filter(input, candidate_budget, false, |_| true)
    }

    /// Bounded refill variant for supported range/quality metadata predicates
    /// that cannot be represented by [`FilterSet`]'s exact equality map.
    pub fn query_hybrid_with_budget_and_filter<F>(
        &self,
        input: HybridQueryInput<'_>,
        candidate_budget: usize,
        extra_filter_active: bool,
        extra_filter: F,
    ) -> Result<HybridQueryResult>
    where
        F: Fn(&serde_json::Value) -> bool,
    {
        let limit = input.limit;
        if limit == 0 {
            return Ok(HybridQueryResult {
                hits: Vec::new(),
                examined_count: 0,
                exhausted: true,
                retrieval_outcome: RetrievalOutcome::from_evidence(
                    RequestedMode::Hybrid,
                    RetrievalEvidence {
                        executed_path: Some(ExecutedPath::HybridFusion),
                        examined_count: Some(0),
                        matched_count: 0,
                        fallback_reason: None,
                        stale_evidence: false,
                    },
                ),
            });
        }

        let filters_active = !input.filters.values.is_empty() || extra_filter_active;
        let budget = candidate_budget.max(limit).max(1);
        let mut window = limit.min(budget).max(1);
        let mut lexical_hits;
        let lexical_examined;
        let lexical_exhausted;
        let lexical_saturated;

        loop {
            let raw = self.lexical.query(&LexicalQuery {
                text: input.query_text.to_string(),
                limit: window,
                // Do not hand an unproven adapter a filter: legacy adapters
                // may turn it into a full-doc collection. The orchestrator's
                // cumulative window is the bounded fallback contract.
                filters: FilterSet::default(),
            })?;
            let raw_len = raw.len();
            lexical_hits = if filters_active {
                raw.into_iter()
                    .filter(|hit| {
                        metadata_matches(&hit.metadata, &input.filters)
                            && extra_filter(&hit.metadata)
                    })
                    .collect()
            } else {
                raw
            };

            if lexical_hits.len() >= limit {
                lexical_examined = raw_len;
                lexical_exhausted = false;
                lexical_saturated = false;
                break;
            }
            if raw_len < window {
                lexical_examined = raw_len;
                lexical_exhausted = true;
                lexical_saturated = false;
                break;
            }
            if window >= budget {
                lexical_examined = raw_len;
                lexical_exhausted = false;
                lexical_saturated = true;
                break;
            }
            window = window.saturating_mul(2).min(budget);
        }

        lexical_hits.truncate(limit);
        for (rank, hit) in lexical_hits.iter_mut().enumerate() {
            hit.rank = rank;
        }

        let mut dense_window = limit.min(budget).max(1);
        let mut dense_hits;
        let dense_examined;
        let dense_exhausted;
        let dense_saturated;
        loop {
            let raw = self
                .dense
                .query(input.query_embedding, dense_window, &input.filters)?;
            let raw_len = raw.len();
            dense_hits = if extra_filter_active {
                raw.into_iter()
                    .filter(|hit| extra_filter(&hit.metadata))
                    .collect()
            } else {
                raw
            };
            if dense_hits.len() >= limit {
                dense_examined = raw_len;
                dense_exhausted = false;
                dense_saturated = false;
                break;
            }
            if raw_len < dense_window {
                dense_examined = raw_len;
                dense_exhausted = true;
                dense_saturated = false;
                break;
            }
            if dense_window >= budget {
                dense_examined = raw_len;
                dense_exhausted = false;
                dense_saturated = true;
                break;
            }
            dense_window = dense_window.saturating_mul(2).min(budget);
        }
        dense_hits.truncate(limit);
        for (rank, hit) in dense_hits.iter_mut().enumerate() {
            hit.rank = rank;
        }
        let hits = self.fusion.fuse(lexical_hits, dense_hits, limit);
        let under_delivered = hits.len() < limit;
        let saturated = under_delivered && (lexical_saturated || dense_saturated);
        let retrieval_outcome = RetrievalOutcome::from_evidence(
            RequestedMode::Hybrid,
            RetrievalEvidence {
                executed_path: Some(ExecutedPath::HybridFusion),
                examined_count: Some(lexical_examined.max(dense_examined)),
                matched_count: hits.len(),
                fallback_reason: None,
                stale_evidence: false,
            },
        );
        let retrieval_outcome = if saturated {
            retrieval_outcome.mark_partial()
        } else {
            retrieval_outcome
        };

        Ok(HybridQueryResult {
            hits,
            examined_count: lexical_examined.max(dense_examined),
            exhausted: under_delivered && lexical_exhausted && dense_exhausted,
            retrieval_outcome,
        })
    }

    pub fn manifest(&self) -> Option<&Manifest> {
        self.manifest.as_ref()
    }

    pub fn generation_id(&self) -> Option<&str> {
        self.manifest
            .as_ref()
            .map(|manifest| manifest.generation_id.as_str())
    }

    fn manifest_path(&self) -> PathBuf {
        self.manifest_dir.join("manifest.json")
    }
}

fn metadata_matches(metadata: &serde_json::Value, filters: &FilterSet) -> bool {
    filters
        .values
        .iter()
        .all(|(key, expected)| metadata.get(key) == Some(expected))
}

pub fn validate_live_bindings_for_refresh(
    manifest: &Manifest,
    lexical: &dyn LexicalIndex,
    dense: &dyn DenseIndex,
    fusion: &dyn FusionStrategy,
    fingerprint: &EmbedderFingerprint,
) -> std::result::Result<(), RetrieveError> {
    validate_bindings(manifest, lexical, dense, fusion, fingerprint, None)
}

fn validate_bindings(
    manifest: &Manifest,
    lexical: &dyn LexicalIndex,
    dense: &dyn DenseIndex,
    fusion: &dyn FusionStrategy,
    fingerprint: &EmbedderFingerprint,
    observed_source_hash: Option<&str>,
) -> std::result::Result<(), RetrieveError> {
    if manifest.dense_kind != dense.kind() {
        return Err(RetrieveError::GenerationMismatch {
            lexical_gen: manifest.dense_kind.clone(),
            dense_gen: dense.kind().to_string(),
        });
    }
    if manifest.dense_count != dense.count() {
        return Err(RetrieveError::DenseCountMismatch {
            expected: manifest.dense_count,
            actual: dense.count(),
        });
    }
    if manifest.lexical_commit_id != lexical.commit_id().0 {
        return Err(RetrieveError::LexicalCommitMismatch {
            expected: manifest.lexical_commit_id.clone(),
            actual: lexical.commit_id().0.clone(),
        });
    }
    if manifest.lexical_doc_count != lexical.doc_count() {
        return Err(RetrieveError::LexicalDocCountMismatch {
            expected: manifest.lexical_doc_count,
            actual: lexical.doc_count(),
        });
    }
    if manifest.fusion_algorithm != fusion.name() {
        return Err(RetrieveError::GenerationMismatch {
            lexical_gen: manifest.fusion_algorithm.clone(),
            dense_gen: fusion.name().to_string(),
        });
    }
    if manifest.embedder_url_hash != fingerprint.url_hash {
        return Err(RetrieveError::EmbedderModelDrift {
            manifest_model: manifest.embedder_url_hash.clone(),
            query_model: fingerprint.url_hash.clone(),
        });
    }
    if manifest.embedder_distance != fingerprint.distance {
        return Err(RetrieveError::EmbedderModelDrift {
            manifest_model: manifest.embedder_distance.clone(),
            query_model: fingerprint.distance.clone(),
        });
    }
    if manifest.embedder_dim != dense.dim() {
        return Err(RetrieveError::DimMismatch {
            expected: manifest.embedder_dim,
            actual: dense.dim(),
        });
    }

    let observed = Manifest {
        schema_version: manifest.schema_version.clone(),
        generation_id: manifest.generation_id.clone(),
        source_chunk_count: manifest.source_chunk_count,
        source_hash_blake3: observed_source_hash
            .map(source_hash_blake3)
            .unwrap_or_else(|| manifest.source_hash_blake3.clone()),
        embedder_model: fingerprint.model.clone(),
        embedder_url_hash: fingerprint.url_hash.clone(),
        embedder_dim: fingerprint.dim,
        embedder_distance: fingerprint.distance.clone(),
        dense_count: dense.count(),
        dense_kind: dense.kind().to_string(),
        lexical_commit_id: lexical.commit_id().0.clone(),
        lexical_doc_count: lexical.doc_count(),
        build_started_at: manifest.build_started_at,
        build_completed_at: manifest.build_completed_at,
        build_wall_seconds: manifest.build_wall_seconds,
        fusion_algorithm: fusion.name().to_string(),
        fusion_k: manifest.fusion_k,
    };
    manifest.validate_against(&observed)
}

pub fn hash_endpoint_url(endpoint_url: &str) -> String {
    hex::encode(Sha256::digest(endpoint_url.as_bytes()))
}

pub fn source_hash_blake3(source_hash: &str) -> String {
    blake3::hash(source_hash.as_bytes()).to_hex().to_string()
}

fn fusion_k(name: &str) -> u32 {
    if name == crate::RRF_NAME {
        crate::RRF_K_DEFAULT
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BruteForceAdapter, ChunkRef, DenseChunkRef, Distance, LexicalCommitId,
        ReciprocalRankFusion, TantivyAdapter,
    };
    use serde_json::json;
    use tempfile::TempDir;

    struct RankedLexical {
        hits: Vec<Hit>,
        commit_id: LexicalCommitId,
    }

    impl LexicalIndex for RankedLexical {
        fn schema_version(&self) -> &str {
            "ranked-test-v1"
        }

        fn build(&mut self, _chunks: &[ChunkRef]) -> Result<LexicalCommitId> {
            Ok(self.commit_id.clone())
        }

        fn insert(&mut self, _chunk: &ChunkRef) -> Result<()> {
            Ok(())
        }

        fn query(&self, query: &LexicalQuery) -> Result<Vec<Hit>> {
            Ok(self.hits.iter().take(query.limit).cloned().collect())
        }

        fn commit_id(&self) -> &LexicalCommitId {
            &self.commit_id
        }

        fn doc_count(&self) -> usize {
            self.hits.len()
        }
    }

    struct EmptyDense;

    impl DenseIndex for EmptyDense {
        fn dim(&self) -> usize {
            2
        }

        fn distance(&self) -> Distance {
            Distance::Cosine
        }

        fn kind(&self) -> &str {
            "empty-test-dense"
        }

        fn build(&mut self, _chunks: &[DenseChunkRef]) -> Result<()> {
            Ok(())
        }

        fn insert(&mut self, _chunk: &DenseChunkRef) -> Result<()> {
            Ok(())
        }

        fn query(
            &self,
            _embedding: &[f32],
            _limit: usize,
            _filters: &FilterSet,
        ) -> Result<Vec<Hit>> {
            Ok(Vec::new())
        }

        fn count(&self) -> usize {
            0
        }
    }

    fn ranked_hit(rank: usize, project: &str) -> Hit {
        Hit {
            chunk_id: format!("rank-{rank:03}"),
            score: 1.0 - rank as f32 / 1_000.0,
            rank,
            source: "ranked-test".to_string(),
            metadata: json!({"project": project}),
        }
    }

    fn refill_test_index(foreign: usize, target: usize) -> HybridIndex {
        let hits = (0..foreign)
            .map(|rank| ranked_hit(rank, "foreign/project"))
            .chain((0..target).map(|offset| ranked_hit(foreign + offset, "target/project")))
            .collect();
        HybridIndex::new(
            Box::new(RankedLexical {
                hits,
                commit_id: LexicalCommitId("ranked-test-commit".to_string()),
            }),
            Box::new(EmptyDense),
            Box::new(ReciprocalRankFusion::default()),
            "/tmp/aicx-refill-test",
            fingerprint(),
        )
    }

    fn target_filter() -> FilterSet {
        FilterSet {
            values: [("project".to_string(), json!("target/project"))]
                .into_iter()
                .collect(),
        }
    }

    fn chunk(id: &str, text: &str) -> ChunkRef {
        ChunkRef {
            id: id.to_string(),
            source_path: format!("/tmp/{id}.md"),
            text: text.to_string(),
            metadata: json!({
                "agent": "codex",
                "date": "20260603",
                "project": "vetcoders/Vista",
            }),
        }
    }

    fn dense(chunk: &ChunkRef, embedding: Vec<f32>) -> DenseChunkRef {
        DenseChunkRef {
            chunk: chunk.clone(),
            embedding,
        }
    }

    fn fingerprint() -> EmbedderFingerprint {
        EmbedderFingerprint::new("test-model", "http://example.invalid/embed", 2, "cosine")
    }

    type BuiltHybrid = (
        Manifest,
        Box<dyn LexicalIndex>,
        Box<dyn DenseIndex>,
        Box<dyn FusionStrategy>,
        EmbedderFingerprint,
    );

    fn built_hybrid(manifest_dir: &std::path::Path) -> BuiltHybrid {
        let chunk_a = chunk("a", "alpha");
        let chunk_b = chunk("b", "bravo");
        let dense_a = dense(&chunk_a, vec![1.0, 0.0]);
        let dense_b = dense(&chunk_b, vec![0.0, 1.0]);

        let lexical = Box::new(TantivyAdapter::new(manifest_dir.to_path_buf()).expect("lexical"));
        let dense = Box::new(BruteForceAdapter::new(2).with_distance(Distance::Cosine));
        let fusion = Box::new(ReciprocalRankFusion::default());
        let fingerprint = fingerprint();
        let mut hybrid =
            HybridIndex::new(lexical, dense, fusion, manifest_dir, fingerprint.clone());
        hybrid
            .build_hybrid(&[chunk_a, chunk_b], &[dense_a, dense_b], "source-v1")
            .expect("build");
        let manifest = hybrid.commit().expect("commit").clone();
        let HybridIndex {
            lexical,
            dense,
            fusion,
            ..
        } = hybrid;
        (manifest, lexical, dense, fusion, fingerprint)
    }

    #[test]
    fn refresh_validation_rejects_embedder_model_drift_without_source_hash() {
        let tmp = TempDir::new().expect("tempdir");
        let (mut manifest, lexical, dense, fusion, fingerprint) = built_hybrid(tmp.path());
        manifest.embedder_model = "old-model".to_string();

        let err = validate_live_bindings_for_refresh(
            &manifest,
            lexical.as_ref(),
            dense.as_ref(),
            fusion.as_ref(),
            &fingerprint,
        )
        .expect_err("model drift must fail refresh validation");
        assert!(
            matches!(err, RetrieveError::EmbedderModelDrift { .. }),
            "expected model drift error, got {err:?}"
        );
    }

    #[test]
    fn refresh_validation_rejects_unsupported_schema_without_source_hash() {
        let tmp = TempDir::new().expect("tempdir");
        let (mut manifest, lexical, dense, fusion, fingerprint) = built_hybrid(tmp.path());
        manifest.schema_version = "1.0".to_string();

        let err = validate_live_bindings_for_refresh(
            &manifest,
            lexical.as_ref(),
            dense.as_ref(),
            fusion.as_ref(),
            &fingerprint,
        )
        .expect_err("unsupported schema must fail refresh validation");
        assert!(
            matches!(err, RetrieveError::SchemaVersionUnsupported(_)),
            "expected unsupported schema error, got {err:?}"
        );
    }

    #[test]
    fn commit_refreshes_manifest_after_delta_insert() {
        let tmp = TempDir::new().expect("tempdir");
        let manifest_dir = tmp.path().join("hybrid");

        let chunk_a = chunk("a", "alpha");
        let chunk_b = chunk("b", "bravo");
        let chunk_c = chunk("c", "charlie");
        let dense_a = dense(&chunk_a, vec![1.0, 0.0]);
        let dense_b = dense(&chunk_b, vec![0.0, 1.0]);
        let dense_c = dense(&chunk_c, vec![0.8, 0.2]);

        let lexical = Box::new(TantivyAdapter::new(manifest_dir.clone()).expect("lexical"));
        let dense = Box::new(BruteForceAdapter::new(2).with_distance(Distance::Cosine));
        let fusion = Box::new(ReciprocalRankFusion::default());
        let mut hybrid = HybridIndex::new(lexical, dense, fusion, &manifest_dir, fingerprint());

        hybrid
            .build_hybrid(
                &[chunk_a.clone(), chunk_b.clone()],
                &[dense_a.clone(), dense_b.clone()],
                "source-v1",
            )
            .expect("build");
        let initial = hybrid.commit().expect("initial commit").clone();

        hybrid.lexical.insert(&chunk_c).expect("lexical insert");
        hybrid.dense.insert(&dense_c).expect("dense insert");
        {
            let manifest = hybrid.manifest.as_mut().expect("manifest");
            manifest.source_chunk_count = 3;
            manifest.source_hash_blake3 = source_hash_blake3("source-v2");
        }

        let refreshed = hybrid.commit().expect("refresh commit").clone();
        assert_eq!(refreshed.source_chunk_count, 3);
        assert_eq!(refreshed.dense_count, 3);
        assert_eq!(refreshed.lexical_doc_count, 3);
        assert_ne!(refreshed.generation_id, initial.generation_id);
    }

    #[test]
    fn filter_refill_recovers_project_hits_beyond_former_top_50() {
        let index = refill_test_index(100, 5);
        let outcome = index
            .query_hybrid_with_budget(
                HybridQueryInput {
                    query_text: "needle",
                    query_embedding: &[1.0, 0.0],
                    filters: target_filter(),
                    limit: 5,
                },
                128,
            )
            .expect("bounded refill should reach target-project hits");

        assert_eq!(outcome.hits.len(), 5);
        assert!(
            outcome
                .hits
                .iter()
                .all(|hit| { hit.metadata.get("project") == Some(&json!("target/project")) })
        );
        assert_eq!(
            outcome.retrieval_outcome.completeness,
            crate::RetrievalCompleteness::Complete
        );
    }

    #[test]
    fn tiny_refill_budget_surfaces_saturation_instead_of_empty_exhaustion() {
        let index = refill_test_index(100, 5);
        let outcome = index
            .query_hybrid_with_budget(
                HybridQueryInput {
                    query_text: "needle",
                    query_embedding: &[1.0, 0.0],
                    filters: target_filter(),
                    limit: 5,
                },
                50,
            )
            .expect("bounded refill should return typed boundary evidence");

        assert!(outcome.hits.is_empty());
        assert_eq!(outcome.examined_count, 50);
        assert_eq!(
            outcome.retrieval_outcome.completeness,
            crate::RetrievalCompleteness::Partial
        );
        assert_eq!(outcome.retrieval_outcome.examined_count, 50);
        assert_eq!(outcome.retrieval_outcome.matched_count, 0);
        assert!(!outcome.exhausted);
    }

    #[test]
    fn short_refill_pool_is_true_exhaustion_not_saturation() {
        let index = refill_test_index(20, 0);
        let outcome = index
            .query_hybrid_with_budget(
                HybridQueryInput {
                    query_text: "needle",
                    query_embedding: &[1.0, 0.0],
                    filters: target_filter(),
                    limit: 5,
                },
                50,
            )
            .expect("short pool should exhaust cleanly");

        assert!(outcome.hits.is_empty());
        assert_eq!(outcome.examined_count, 20);
        assert_eq!(
            outcome.retrieval_outcome.completeness,
            crate::RetrievalCompleteness::Complete
        );
        assert!(outcome.exhausted);
    }
}
