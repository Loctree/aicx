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

fn validate_bindings(
    manifest: &Manifest,
    lexical: &dyn LexicalIndex,
    dense: &dyn DenseIndex,
    fusion: &dyn FusionStrategy,
    fingerprint: &EmbedderFingerprint,
    observed_source_hash: &str,
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
        source_hash_blake3: source_hash_blake3(observed_source_hash),
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
