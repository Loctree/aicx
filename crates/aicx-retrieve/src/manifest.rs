// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
use std::fs;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::ManifestError;

/// Machine-readable description of the exact mmap dense artifact referenced by
/// `Manifest::dense_kind == "exact_mmap_v1"`.
///
/// The binary layout is little-endian and contiguous:
/// `header[128] | metadata_refs[count * 16] | vectors[count * dim * f32] |
/// metadata_json`. Each reference is `(offset: u64, len: u32, reserved: u32)`
/// relative to the metadata region. The header carries magic, schema version,
/// endian marker, dimension, distance, count, source BLAKE3 bytes, every region
/// offset/length, and the exact file length.
/// Canonical file name of the single dense vector payload inside a hybrid
/// generation directory. One generation materializes vectors exactly once,
/// into this artifact; the legacy `dense_brute_force.ndjson` twin is a
/// migration read input only and is never written by new builds.
pub const MMAP_DENSE_PAYLOAD_FILE_NAME: &str = "dense.exact_mmap_v1.bin";

/// Blake3 digest bytes of an observed source hash string. The hex form of the
/// returned bytes equals [`crate::source_hash_blake3`] for the same input, so
/// the manifest's `source_hash_blake3` field and the 32-byte source hash
/// embedded in the mmap dense payload share one derivation.
pub fn source_hash_bytes(observed_source_hash: &str) -> [u8; 32] {
    *blake3::hash(observed_source_hash.as_bytes()).as_bytes()
}

/// Decode a manifest `source_hash_blake3` hex string back into the 32-byte
/// form the mmap dense payload embeds. Fails closed on malformed input.
pub fn decode_source_hash_blake3(hex_hash: &str) -> Result<[u8; 32]> {
    let decoded = hex::decode(hex_hash)
        .with_context(|| format!("decode manifest source hash hex: {hex_hash}"))?;
    <[u8; 32]>::try_from(decoded.as_slice())
        .map_err(|_| anyhow::anyhow!("manifest source hash must be 32 bytes: {hex_hash}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MmapDenseFormatSchema {
    pub schema: String,
    pub magic_ascii: String,
    pub byte_order: String,
    pub header_bytes: usize,
    pub metadata_reference_bytes: usize,
    pub vector_element: String,
    pub layout: String,
}

impl Default for MmapDenseFormatSchema {
    fn default() -> Self {
        Self {
            schema: "aicx.dense.exact_mmap.v1".to_string(),
            magic_ascii: "AICXDMM1".to_string(),
            byte_order: "little_endian".to_string(),
            header_bytes: 128,
            metadata_reference_bytes: 16,
            vector_element: "ieee754_f32".to_string(),
            layout: "header|metadata_refs|fixed_width_vectors|metadata_json".to_string(),
        }
    }
}

/// Retrieval build manifest for split lexical + dense index artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: String,
    pub generation_id: String,
    pub source_chunk_count: usize,
    pub source_hash_blake3: String,
    pub embedder_model: String,
    pub embedder_url_hash: String,
    pub embedder_dim: usize,
    pub embedder_distance: String,
    pub dense_count: usize,
    pub dense_kind: String,
    pub lexical_commit_id: String,
    pub lexical_doc_count: usize,
    pub build_started_at: DateTime<Utc>,
    pub build_completed_at: DateTime<Utc>,
    pub build_wall_seconds: u64,
    pub fusion_algorithm: String,
    pub fusion_k: u32,
}

impl Manifest {
    pub fn write_to_path(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create manifest parent {}", parent.display()))?;
        }
        let mut tmp_path = path.to_path_buf();
        let tmp_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}.tmp"))
            .unwrap_or_else(|| "manifest.json.tmp".to_string());
        tmp_path.set_file_name(tmp_name);

        let mut file = create_validated(&tmp_path)
            .with_context(|| format!("create tmp manifest {}", tmp_path.display()))?;
        serde_json::to_writer_pretty(&mut file, self).context("serialize retrieval manifest")?;
        file.write_all(b"\n")
            .with_context(|| format!("finish tmp manifest {}", tmp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync tmp manifest {}", tmp_path.display()))?;
        drop(file);

        fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "commit retrieval manifest: {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })?;
        Ok(())
    }

    pub fn read_from_path(path: &Path) -> Result<Manifest> {
        let bytes =
            read_validated(path).with_context(|| format!("read manifest {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parse manifest {}", path.display()))
    }

    pub fn fresh_generation_id() -> String {
        let mut bytes = [0u8; 4];
        rand::thread_rng().fill_bytes(&mut bytes);
        format!(
            "g-{}-{}",
            Self::now_utc().format("%Y-%m-%dT%H:%M:%SZ"),
            hex::encode(bytes)
        )
    }

    pub fn now_utc() -> DateTime<Utc> {
        Utc::now()
    }

    /// Validate that two retrieval artifacts belong to the same generation.
    pub fn validate_against(&self, other: &Manifest) -> Result<(), ManifestError> {
        const SUPPORTED_SCHEMA_VERSION: &str = "2.0";

        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(ManifestError::SchemaVersionUnsupported(
                self.schema_version.clone(),
            ));
        }

        if other.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(ManifestError::SchemaVersionUnsupported(
                other.schema_version.clone(),
            ));
        }

        if self.embedder_dim != other.embedder_dim {
            return Err(ManifestError::DimMismatch {
                expected: self.embedder_dim,
                actual: other.embedder_dim,
            });
        }

        if self.embedder_model != other.embedder_model {
            return Err(ManifestError::EmbedderModelDrift {
                manifest_model: self.embedder_model.clone(),
                query_model: other.embedder_model.clone(),
            });
        }

        if self.source_hash_blake3 != other.source_hash_blake3 {
            return Err(ManifestError::SourceHashDrift {
                manifest_hash: self.source_hash_blake3.clone(),
                observed_hash: other.source_hash_blake3.clone(),
            });
        }

        if self.embedder_distance != other.embedder_distance {
            return Err(ManifestError::EmbedderModelDrift {
                manifest_model: self.embedder_distance.clone(),
                query_model: other.embedder_distance.clone(),
            });
        }

        if self.lexical_commit_id != other.lexical_commit_id {
            return Err(ManifestError::LexicalCommitMismatch {
                expected: self.lexical_commit_id.clone(),
                actual: other.lexical_commit_id.clone(),
            });
        }

        // Partial-build drift: artifacts that claim the same generation must
        // agree on payload kind and row counts, or an interrupted build could
        // masquerade as complete.
        if self.dense_kind != other.dense_kind {
            return Err(ManifestError::GenerationMismatch {
                lexical_gen: self.dense_kind.clone(),
                dense_gen: other.dense_kind.clone(),
            });
        }

        if self.dense_count != other.dense_count {
            return Err(ManifestError::DenseCountMismatch {
                expected: self.dense_count,
                actual: other.dense_count,
            });
        }

        if self.lexical_doc_count != other.lexical_doc_count {
            return Err(ManifestError::LexicalDocCountMismatch {
                expected: self.lexical_doc_count,
                actual: other.lexical_doc_count,
            });
        }

        Ok(())
    }
}

fn validate_manifest_path(path: &Path) -> Result<&Path> {
    let path_str = path.to_string_lossy();
    if path_str.contains('\0') || path_str.contains('\n') || path_str.contains('\r') {
        anyhow::bail!("invalid retrieval manifest path: {}", path.display());
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        anyhow::bail!(
            "retrieval manifest path must not contain traversal components: {}",
            path.display()
        );
    }
    Ok(path)
}

fn create_validated(path: &Path) -> Result<File> {
    let path = validate_manifest_path(path)?;
    fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))
}

fn read_validated(path: &Path) -> Result<Vec<u8>> {
    let path = validate_manifest_path(path)?;
    let mut bytes = Vec::new();
    let mut file = fs::OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    file.read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(bytes)
}
