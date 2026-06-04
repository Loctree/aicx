// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};

use crate::{
    ChunkRef, DenseChunkRef, DenseIndex, FilterSet, FusionStrategy, Hit, LexicalIndex,
    LexicalQuery, Manifest, RetrieveError,
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

    pub fn load_from_manifest(
        lexical: Box<dyn LexicalIndex>,
        dense: Box<dyn DenseIndex>,
        fusion: Box<dyn FusionStrategy>,
        manifest_dir: impl Into<PathBuf>,
        embedder_fingerprint: EmbedderFingerprint,
        observed_source_hash: &str,
    ) -> Result<Self> {
        let manifest_dir = manifest_dir.into();
        let manifest = Manifest::read_from_path(&manifest_dir.join("manifest.json"))?;
        validate_bindings(
            &manifest,
            lexical.as_ref(),
            dense.as_ref(),
            fusion.as_ref(),
            &embedder_fingerprint,
            Some(observed_source_hash),
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
        let lex_hits = self.lexical.query(&LexicalQuery {
            text: input.query_text.to_string(),
            limit: input.limit,
            filters: input.filters.clone(),
        })?;
        let dense_hits = self
            .dense
            .query(input.query_embedding, input.limit, &input.filters)?;
        Ok(self.fusion.fuse(lex_hits, dense_hits, input.limit))
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
        BruteForceAdapter, ChunkRef, DenseChunkRef, Distance, ReciprocalRankFusion, TantivyAdapter,
    };
    use serde_json::json;
    use tempfile::TempDir;

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

    fn built_hybrid(
        manifest_dir: &std::path::Path,
    ) -> (
        Manifest,
        Box<dyn LexicalIndex>,
        Box<dyn DenseIndex>,
        Box<dyn FusionStrategy>,
        EmbedderFingerprint,
    ) {
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
}
