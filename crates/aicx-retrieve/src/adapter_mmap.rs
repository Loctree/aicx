// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
//! Exact dense retrieval over a versioned, read-only memory map.
//!
//! One file is one project shard. Callers select the project shard path before
//! opening this adapter; row metadata filters are applied before vector bytes
//! are read. The query path keeps only metadata for the current row plus a
//! bounded `limit` heap. It never materializes the vector payload on the heap.
//! When no filters are set, the scan skips metadata decoding entirely and
//! reads vector bytes straight off the map; full metadata is decoded only for
//! the final top-`limit` rows.

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::mem::size_of;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use tantivy::directory::{Directory, MmapDirectory, OwnedBytes};

use crate::{DenseChunkRef, DenseIndex, Distance, FilterSet, Hit};

pub const MMAP_DENSE_KIND: &str = "exact_mmap_v1";
pub const MMAP_DENSE_MAGIC: [u8; 8] = *b"AICXDMM1";
pub const MMAP_DENSE_SCHEMA_VERSION: u16 = 1;
pub const MMAP_DENSE_HEADER_LEN: usize = 128;
pub const MMAP_METADATA_REF_LEN: usize = 16;

const ENDIAN_MARKER: u32 = 0x0102_0304;
const MAX_METADATA_RECORD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MmapQueryStats {
    /// Rows visited by the scan. Metadata bytes are decoded for these rows
    /// only when the query carries filters; unfiltered scans visit every row
    /// without touching the metadata region.
    pub metadata_examined: usize,
    pub vectors_scored: usize,
}

#[derive(Debug, Clone, Copy)]
struct Header {
    dim: usize,
    distance: Distance,
    count: usize,
    source_hash: [u8; 32],
    refs_offset: usize,
    refs_len: usize,
    vectors_offset: usize,
    vectors_len: usize,
    metadata_offset: usize,
    metadata_len: usize,
}

#[derive(Debug, Clone, Copy)]
struct MetadataRef {
    offset: usize,
    len: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredMetadata {
    chunk_id: String,
    source_path: String,
    metadata: serde_json::Value,
}

/// Filter-pass projection of [`StoredMetadata`]: the hot loop only needs the
/// `metadata` object, so `chunk_id`/`source_path` string allocations are
/// skipped while the scan decides which rows to score.
#[derive(Debug, Deserialize)]
struct FilterMetadata {
    metadata: serde_json::Value,
}

/// A file-backed exact dense index. `data` is an OS-backed read-only mapping
/// supplied by Tantivy's cross-platform mmap directory implementation.
pub struct MmapDenseAdapter {
    path: PathBuf,
    header: Header,
    refs: Vec<MetadataRef>,
    data: OwnedBytes,
    last_query_stats: Cell<MmapQueryStats>,
}

impl MmapDenseAdapter {
    /// Create an unopened shard owner. The first `build` atomically writes the
    /// binary file and replaces this value with a validated read-only mapping.
    pub fn create(
        path: impl Into<PathBuf>,
        dim: usize,
        distance: Distance,
        source_hash: [u8; 32],
    ) -> Self {
        Self {
            path: path.into(),
            header: Header {
                dim,
                distance,
                count: 0,
                source_hash,
                refs_offset: MMAP_DENSE_HEADER_LEN,
                refs_len: 0,
                vectors_offset: MMAP_DENSE_HEADER_LEN,
                vectors_len: 0,
                metadata_offset: MMAP_DENSE_HEADER_LEN,
                metadata_len: 0,
            },
            refs: Vec::new(),
            data: OwnedBytes::empty(),
            last_query_stats: Cell::new(MmapQueryStats::default()),
        }
    }

    /// Open and validate a shard. Invalid files fail closed; this function never
    /// falls back to the legacy NDJSON reader.
    pub fn open(
        path: impl AsRef<Path>,
        expected_dim: usize,
        expected_distance: Distance,
        expected_source_hash: Option<[u8; 32]>,
    ) -> Result<Self> {
        let path = validate_index_path(path.as_ref())?.to_path_buf();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let file_name = path
            .file_name()
            .ok_or_else(|| anyhow!("mmap dense path has no file name: {}", path.display()))?;
        let directory = MmapDirectory::open(parent)
            .with_context(|| format!("open mmap directory {}", parent.display()))?;
        let data = directory
            .open_read(Path::new(file_name))
            .with_context(|| format!("open mmap dense file {}", path.display()))?
            .read_bytes()
            .with_context(|| format!("map mmap dense file {}", path.display()))?;
        let header = parse_header(&data, &path)?;

        if header.dim != expected_dim {
            bail!(
                "mmap dense dimension mismatch in {}: header={}, expected={expected_dim}",
                path.display(),
                header.dim
            );
        }
        if header.distance != expected_distance {
            bail!(
                "mmap dense distance mismatch in {}: header={:?}, expected={expected_distance:?}",
                path.display(),
                header.distance
            );
        }
        if let Some(expected) = expected_source_hash
            && header.source_hash != expected
        {
            bail!("mmap dense source hash mismatch in {}", path.display());
        }

        let refs = parse_and_validate_refs(&data, &header, &path)?;
        Ok(Self {
            path,
            header,
            refs,
            data,
            last_query_stats: Cell::new(MmapQueryStats::default()),
        })
    }

    pub fn source_hash(&self) -> [u8; 32] {
        self.header.source_hash
    }

    pub fn mapped_len(&self) -> usize {
        self.data.len()
    }

    /// Conservative accounting for adapter-owned heap state. The memory map is
    /// deliberately excluded: its pages are file-backed and not heap-loaded.
    pub fn heap_bytes_upper_bound(&self) -> usize {
        size_of::<Self>() + self.refs.capacity() * size_of::<MetadataRef>()
    }

    pub fn last_query_stats(&self) -> MmapQueryStats {
        self.last_query_stats.get()
    }

    fn metadata_slice(&self, row: usize) -> Result<&[u8]> {
        let reference = self
            .refs
            .get(row)
            .ok_or_else(|| anyhow!("mmap dense metadata row out of range: {row}"))?;
        let start = checked_add(
            self.header.metadata_offset,
            reference.offset,
            "metadata start",
        )?;
        let end = checked_add(start, reference.len, "metadata end")?;
        Ok(&self.data[start..end])
    }

    fn metadata(&self, row: usize) -> Result<StoredMetadata> {
        serde_json::from_slice(self.metadata_slice(row)?)
            .with_context(|| format!("parse mmap dense metadata row {row}"))
    }

    fn metadata_filter_value(&self, row: usize) -> Result<serde_json::Value> {
        serde_json::from_slice::<FilterMetadata>(self.metadata_slice(row)?)
            .map(|stored| stored.metadata)
            .with_context(|| format!("parse mmap dense metadata row {row}"))
    }

    /// Score one stored row. Accumulation stays in the sequential order the
    /// legacy brute-force leg uses, so scores are bit-identical across legs;
    /// `query_norm_sq` is that same sequential self-product, hoisted out of
    /// the scan because it does not vary per row.
    fn vector_score(&self, row: usize, query: &[f32], query_norm_sq: f32) -> Result<f32> {
        let row_bytes = checked_mul(self.header.dim, size_of::<f32>(), "vector row bytes")?;
        let start = checked_add(
            self.header.vectors_offset,
            checked_mul(row, row_bytes, "vector row offset")?,
            "vector start",
        )?;
        let end = checked_add(start, row_bytes, "vector end")?;
        let (components, remainder) = self.data[start..end].as_chunks::<4>();
        debug_assert!(remainder.is_empty());

        let score = match self.header.distance {
            Distance::Cosine => {
                let mut dot = 0.0f32;
                let mut norm_row = 0.0f32;
                for (component, query_component) in components.iter().zip(query.iter().copied()) {
                    let value = f32::from_le_bytes(*component);
                    dot += query_component * value;
                    norm_row += value * value;
                }
                if query_norm_sq == 0.0 || norm_row == 0.0 {
                    0.0
                } else {
                    dot / (query_norm_sq.sqrt() * norm_row.sqrt())
                }
            }
            Distance::Dot => {
                let mut dot = 0.0f32;
                for (component, query_component) in components.iter().zip(query.iter().copied()) {
                    dot += query_component * f32::from_le_bytes(*component);
                }
                dot
            }
            Distance::Euclidean => {
                let mut squared_distance = 0.0f32;
                for (component, query_component) in components.iter().zip(query.iter().copied()) {
                    let delta = query_component - f32::from_le_bytes(*component);
                    squared_distance += delta * delta;
                }
                -squared_distance.sqrt()
            }
        };
        if !score.is_finite() {
            // Fail-closed diagnostics off the hot path: a non-finite score is
            // either a corrupt stored component or an overflow of finite
            // inputs; distinguish them only once scoring has already failed.
            if components
                .iter()
                .any(|component| !f32::from_le_bytes(*component).is_finite())
            {
                bail!("non-finite vector component in mmap dense row {row}");
            }
            bail!("non-finite mmap dense score for row {row}");
        }
        Ok(score)
    }

    fn write_chunks(&self, chunks: &[DenseChunkRef]) -> Result<()> {
        if self.header.dim == 0 {
            bail!("mmap dense dimension must be non-zero");
        }
        let mut metadata_blobs = Vec::with_capacity(chunks.len());
        let mut metadata_len = 0usize;
        for chunk in chunks {
            if chunk.embedding.len() != self.header.dim {
                bail!(
                    "build: chunk {} has dim {}, adapter dim {}",
                    chunk.chunk.id,
                    chunk.embedding.len(),
                    self.header.dim
                );
            }
            if chunk.embedding.iter().any(|value| !value.is_finite()) {
                bail!(
                    "build: chunk {} contains non-finite embedding",
                    chunk.chunk.id
                );
            }
            let blob = serde_json::to_vec(&StoredMetadata {
                chunk_id: chunk.chunk.id.clone(),
                source_path: chunk.chunk.source_path.clone(),
                metadata: chunk.chunk.metadata.clone(),
            })?;
            if blob.is_empty() || blob.len() > MAX_METADATA_RECORD_BYTES {
                bail!(
                    "build: chunk {} metadata size {} is outside 1..={MAX_METADATA_RECORD_BYTES}",
                    chunk.chunk.id,
                    blob.len()
                );
            }
            metadata_len = checked_add(metadata_len, blob.len(), "metadata payload length")?;
            metadata_blobs.push(blob);
        }

        let count = chunks.len();
        let refs_len = checked_mul(count, MMAP_METADATA_REF_LEN, "metadata refs length")?;
        let vectors_len = checked_mul(
            checked_mul(count, self.header.dim, "vector component count")?,
            size_of::<f32>(),
            "vectors length",
        )?;
        let refs_offset = MMAP_DENSE_HEADER_LEN;
        let vectors_offset = checked_add(refs_offset, refs_len, "vectors offset")?;
        let metadata_offset = checked_add(vectors_offset, vectors_len, "metadata offset")?;
        let file_len = checked_add(metadata_offset, metadata_len, "file length")?;
        let header = Header {
            dim: self.header.dim,
            distance: self.header.distance,
            count,
            source_hash: self.header.source_hash,
            refs_offset,
            refs_len,
            vectors_offset,
            vectors_len,
            metadata_offset,
            metadata_len,
        };

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create mmap dense parent {}", parent.display()))?;
        }
        let tmp_path = temporary_path(&self.path)?;
        let file = File::create(&tmp_path)
            .with_context(|| format!("create mmap dense tmp {}", tmp_path.display()))?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&encode_header(&header, file_len)?)?;

        let mut metadata_cursor = 0usize;
        for blob in &metadata_blobs {
            write_u64(
                &mut writer,
                usize_to_u64(metadata_cursor, "metadata offset")?,
            )?;
            write_u32(&mut writer, usize_to_u32(blob.len(), "metadata length")?)?;
            write_u32(&mut writer, 0)?;
            metadata_cursor += blob.len();
        }
        for chunk in chunks {
            for value in &chunk.embedding {
                writer.write_all(&value.to_le_bytes())?;
            }
        }
        for blob in metadata_blobs {
            writer.write_all(&blob)?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&tmp_path, &self.path).with_context(|| {
            format!(
                "commit mmap dense file {} -> {}",
                tmp_path.display(),
                self.path.display()
            )
        })?;
        Ok(())
    }

    fn append_chunk(&mut self, chunk: &DenseChunkRef) -> Result<()> {
        if self.header.count == 0 || self.data.is_empty() {
            return self.build(std::slice::from_ref(chunk));
        }
        if chunk.embedding.len() != self.header.dim {
            bail!(
                "insert: chunk {} has dim {}, adapter dim {}",
                chunk.chunk.id,
                chunk.embedding.len(),
                self.header.dim
            );
        }
        if chunk.embedding.iter().any(|value| !value.is_finite()) {
            bail!(
                "insert: chunk {} contains non-finite embedding",
                chunk.chunk.id
            );
        }
        let metadata = serde_json::to_vec(&StoredMetadata {
            chunk_id: chunk.chunk.id.clone(),
            source_path: chunk.chunk.source_path.clone(),
            metadata: chunk.chunk.metadata.clone(),
        })?;
        if metadata.is_empty() || metadata.len() > MAX_METADATA_RECORD_BYTES {
            bail!(
                "insert: chunk {} metadata size {} is outside 1..={MAX_METADATA_RECORD_BYTES}",
                chunk.chunk.id,
                metadata.len()
            );
        }

        let count = checked_add(self.header.count, 1, "insert count")?;
        let refs_len = checked_mul(count, MMAP_METADATA_REF_LEN, "insert refs length")?;
        let vectors_len = checked_add(
            self.header.vectors_len,
            checked_mul(self.header.dim, 4, "insert vector bytes")?,
            "insert vectors length",
        )?;
        let metadata_len = checked_add(
            self.header.metadata_len,
            metadata.len(),
            "insert metadata length",
        )?;
        let refs_offset = MMAP_DENSE_HEADER_LEN;
        let vectors_offset = checked_add(refs_offset, refs_len, "insert vectors offset")?;
        let metadata_offset = checked_add(vectors_offset, vectors_len, "insert metadata offset")?;
        let file_len = checked_add(metadata_offset, metadata_len, "insert file length")?;
        let next_header = Header {
            dim: self.header.dim,
            distance: self.header.distance,
            count,
            source_hash: self.header.source_hash,
            refs_offset,
            refs_len,
            vectors_offset,
            vectors_len,
            metadata_offset,
            metadata_len,
        };

        let tmp_path = temporary_path(&self.path)?;
        let file = File::create(&tmp_path)
            .with_context(|| format!("create mmap dense tmp {}", tmp_path.display()))?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&encode_header(&next_header, file_len)?)?;
        for reference in &self.refs {
            write_u64(
                &mut writer,
                usize_to_u64(reference.offset, "metadata offset")?,
            )?;
            write_u32(&mut writer, usize_to_u32(reference.len, "metadata length")?)?;
            write_u32(&mut writer, 0)?;
        }
        write_u64(
            &mut writer,
            usize_to_u64(self.header.metadata_len, "metadata offset")?,
        )?;
        write_u32(
            &mut writer,
            usize_to_u32(metadata.len(), "metadata length")?,
        )?;
        write_u32(&mut writer, 0)?;
        let old_vectors_end = checked_add(
            self.header.vectors_offset,
            self.header.vectors_len,
            "old vectors end",
        )?;
        writer.write_all(&self.data[self.header.vectors_offset..old_vectors_end])?;
        for value in &chunk.embedding {
            writer.write_all(&value.to_le_bytes())?;
        }
        let old_metadata_end = checked_add(
            self.header.metadata_offset,
            self.header.metadata_len,
            "old metadata end",
        )?;
        writer.write_all(&self.data[self.header.metadata_offset..old_metadata_end])?;
        writer.write_all(&metadata)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);

        self.data = OwnedBytes::empty();
        self.refs.clear();
        fs::rename(&tmp_path, &self.path).with_context(|| {
            format!(
                "commit mmap dense insert {} -> {}",
                tmp_path.display(),
                self.path.display()
            )
        })?;
        *self = Self::open(
            &self.path,
            next_header.dim,
            next_header.distance,
            Some(next_header.source_hash),
        )?;
        Ok(())
    }
}

impl DenseIndex for MmapDenseAdapter {
    fn dim(&self) -> usize {
        self.header.dim
    }

    fn distance(&self) -> Distance {
        self.header.distance
    }

    fn kind(&self) -> &str {
        MMAP_DENSE_KIND
    }

    fn build(&mut self, chunks: &[DenseChunkRef]) -> Result<()> {
        // Drop an existing mapping before atomic replacement. This matters on
        // Windows, where a mapped file cannot be renamed over in place.
        self.data = OwnedBytes::empty();
        self.refs.clear();
        self.header.count = 0;
        self.write_chunks(chunks)?;
        *self = Self::open(
            &self.path,
            self.header.dim,
            self.header.distance,
            Some(self.header.source_hash),
        )?;
        Ok(())
    }

    fn insert(&mut self, chunk: &DenseChunkRef) -> Result<()> {
        self.append_chunk(chunk)
    }

    fn query(&self, embedding: &[f32], limit: usize, filters: &FilterSet) -> Result<Vec<Hit>> {
        if embedding.len() != self.header.dim {
            bail!(
                "query: embedding dim {} != adapter dim {}",
                embedding.len(),
                self.header.dim
            );
        }
        if embedding.iter().any(|value| !value.is_finite()) {
            bail!("query embedding contains non-finite components");
        }
        if limit == 0 {
            self.last_query_stats.set(MmapQueryStats::default());
            return Ok(Vec::new());
        }

        let mut query_norm_sq = 0.0f32;
        for query_component in embedding.iter().copied() {
            query_norm_sq += query_component * query_component;
        }

        let apply_filters = !filters.values.is_empty();
        let mut stats = MmapQueryStats::default();
        let mut heap = BinaryHeap::with_capacity(limit.saturating_add(1));
        for row in 0..self.header.count {
            stats.metadata_examined += 1;
            if apply_filters && !filter_matches(&self.metadata_filter_value(row)?, filters) {
                continue;
            }
            let score = self.vector_score(row, embedding, query_norm_sq)?;
            stats.vectors_scored += 1;
            let candidate = Candidate { score, row };
            if heap.len() < limit {
                heap.push(candidate);
            } else if heap.peek().is_some_and(|worst| candidate < *worst) {
                heap.pop();
                heap.push(candidate);
            }
        }
        self.last_query_stats.set(stats);

        let mut top = heap.into_vec();
        top.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.row.cmp(&right.row))
        });
        top.into_iter()
            .enumerate()
            .map(|(rank, candidate)| {
                let stored = self.metadata(candidate.row)?;
                Ok(Hit {
                    chunk_id: stored.chunk_id,
                    score: candidate.score,
                    rank,
                    source: MMAP_DENSE_KIND.to_string(),
                    metadata: stored.metadata,
                })
            })
            .collect()
    }

    fn count(&self) -> usize {
        self.header.count
    }
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    score: f32,
    row: usize,
}

impl PartialEq for Candidate {
    fn eq(&self, other: &Self) -> bool {
        self.score.to_bits() == other.score.to_bits() && self.row == other.row
    }
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.row.cmp(&other.row))
    }
}

fn parse_header(data: &[u8], path: &Path) -> Result<Header> {
    if data.len() < MMAP_DENSE_HEADER_LEN {
        bail!(
            "mmap dense file {} is truncated before the {MMAP_DENSE_HEADER_LEN}-byte header",
            path.display()
        );
    }
    if data[..8] != MMAP_DENSE_MAGIC {
        bail!("mmap dense magic mismatch in {}", path.display());
    }
    let schema_version = read_u16(data, 8)?;
    if schema_version != MMAP_DENSE_SCHEMA_VERSION {
        bail!(
            "unsupported mmap dense schema version {schema_version} in {}",
            path.display()
        );
    }
    if read_u32(data, 10)? != ENDIAN_MARKER {
        bail!("mmap dense endian marker mismatch in {}", path.display());
    }
    if usize::from(read_u16(data, 14)?) != MMAP_DENSE_HEADER_LEN {
        bail!("mmap dense header length mismatch in {}", path.display());
    }
    let dim = usize::try_from(read_u32(data, 16)?)?;
    if dim == 0 {
        bail!("mmap dense dimension is zero in {}", path.display());
    }
    let distance = decode_distance(data[20])?;
    if data[21..24].iter().any(|byte| *byte != 0) || data[120..128].iter().any(|byte| *byte != 0) {
        bail!(
            "mmap dense reserved header bytes are non-zero in {}",
            path.display()
        );
    }
    let count = u64_to_usize(read_u64(data, 24)?, "count")?;
    let mut source_hash = [0u8; 32];
    source_hash.copy_from_slice(&data[32..64]);
    let refs_offset = u64_to_usize(read_u64(data, 64)?, "refs offset")?;
    let refs_len = u64_to_usize(read_u64(data, 72)?, "refs length")?;
    let vectors_offset = u64_to_usize(read_u64(data, 80)?, "vectors offset")?;
    let vectors_len = u64_to_usize(read_u64(data, 88)?, "vectors length")?;
    let metadata_offset = u64_to_usize(read_u64(data, 96)?, "metadata offset")?;
    let metadata_len = u64_to_usize(read_u64(data, 104)?, "metadata length")?;
    let file_len = u64_to_usize(read_u64(data, 112)?, "file length")?;

    let expected_refs_len = checked_mul(count, MMAP_METADATA_REF_LEN, "refs length")?;
    let expected_vectors_len = checked_mul(
        checked_mul(count, dim, "vector component count")?,
        4,
        "vectors length",
    )?;
    let refs_end = checked_add(refs_offset, refs_len, "refs end")?;
    let vectors_end = checked_add(vectors_offset, vectors_len, "vectors end")?;
    let metadata_end = checked_add(metadata_offset, metadata_len, "metadata end")?;
    if refs_offset != MMAP_DENSE_HEADER_LEN
        || refs_len != expected_refs_len
        || vectors_offset != refs_end
        || vectors_len != expected_vectors_len
        || metadata_offset != vectors_end
        || metadata_end != file_len
        || file_len != data.len()
    {
        bail!(
            "mmap dense structural offsets are corrupt in {}",
            path.display()
        );
    }

    Ok(Header {
        dim,
        distance,
        count,
        source_hash,
        refs_offset,
        refs_len,
        vectors_offset,
        vectors_len,
        metadata_offset,
        metadata_len,
    })
}

fn parse_and_validate_refs(data: &[u8], header: &Header, path: &Path) -> Result<Vec<MetadataRef>> {
    let mut refs = Vec::with_capacity(header.count);
    let mut expected_offset = 0usize;
    for row in 0..header.count {
        let start = checked_add(
            header.refs_offset,
            checked_mul(row, MMAP_METADATA_REF_LEN, "metadata ref offset")?,
            "metadata ref start",
        )?;
        let offset = u64_to_usize(read_u64(data, start)?, "metadata ref offset")?;
        let len = usize::try_from(read_u32(data, start + 8)?)?;
        let reserved = read_u32(data, start + 12)?;
        if reserved != 0 || offset != expected_offset || len == 0 || len > MAX_METADATA_RECORD_BYTES
        {
            bail!(
                "invalid mmap dense metadata reference {row} in {}",
                path.display()
            );
        }
        let end = checked_add(offset, len, "metadata reference end")?;
        if end > header.metadata_len {
            bail!(
                "mmap dense metadata reference {row} exceeds region in {}",
                path.display()
            );
        }
        let absolute_start =
            checked_add(header.metadata_offset, offset, "metadata absolute start")?;
        let absolute_end = checked_add(absolute_start, len, "metadata absolute end")?;
        serde_json::from_slice::<StoredMetadata>(&data[absolute_start..absolute_end])
            .with_context(|| {
                format!(
                    "validate mmap dense metadata row {row} in {}",
                    path.display()
                )
            })?;
        refs.push(MetadataRef { offset, len });
        expected_offset = end;
    }
    if expected_offset != header.metadata_len {
        bail!(
            "mmap dense metadata references do not cover region in {}",
            path.display()
        );
    }
    Ok(refs)
}

fn encode_header(header: &Header, file_len: usize) -> Result<[u8; MMAP_DENSE_HEADER_LEN]> {
    let mut bytes = [0u8; MMAP_DENSE_HEADER_LEN];
    bytes[..8].copy_from_slice(&MMAP_DENSE_MAGIC);
    bytes[8..10].copy_from_slice(&MMAP_DENSE_SCHEMA_VERSION.to_le_bytes());
    bytes[10..14].copy_from_slice(&ENDIAN_MARKER.to_le_bytes());
    bytes[14..16].copy_from_slice(&(MMAP_DENSE_HEADER_LEN as u16).to_le_bytes());
    bytes[16..20].copy_from_slice(&usize_to_u32(header.dim, "dimension")?.to_le_bytes());
    bytes[20] = encode_distance(header.distance);
    bytes[24..32].copy_from_slice(&usize_to_u64(header.count, "count")?.to_le_bytes());
    bytes[32..64].copy_from_slice(&header.source_hash);
    for (offset, value) in [
        (64, header.refs_offset),
        (72, header.refs_len),
        (80, header.vectors_offset),
        (88, header.vectors_len),
        (96, header.metadata_offset),
        (104, header.metadata_len),
        (112, file_len),
    ] {
        bytes[offset..offset + 8]
            .copy_from_slice(&usize_to_u64(value, "header offset")?.to_le_bytes());
    }
    Ok(bytes)
}

fn filter_matches(metadata: &serde_json::Value, filters: &FilterSet) -> bool {
    filters
        .values
        .iter()
        .all(|(key, expected)| metadata.get(key) == Some(expected))
}

fn validate_index_path(path: &Path) -> Result<&Path> {
    let rendered = path.to_string_lossy();
    if rendered.contains(['\0', '\n', '\r']) {
        bail!("invalid mmap dense path: {}", path.display());
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!(
            "mmap dense path must not contain traversal: {}",
            path.display()
        );
    }
    Ok(path)
}

fn temporary_path(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid mmap dense path: {}", path.display()))?;
    let mut tmp = path.to_path_buf();
    tmp.set_file_name(format!("{name}.tmp"));
    Ok(tmp)
}

fn encode_distance(distance: Distance) -> u8 {
    match distance {
        Distance::Cosine => 1,
        Distance::Euclidean => 2,
        Distance::Dot => 3,
    }
}

fn decode_distance(value: u8) -> Result<Distance> {
    match value {
        1 => Ok(Distance::Cosine),
        2 => Ok(Distance::Euclidean),
        3 => Ok(Distance::Dot),
        _ => bail!("unsupported mmap dense distance code {value}"),
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or_else(|| anyhow!("truncated u16 at byte {offset}"))?
            .try_into()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(|| anyhow!("truncated u32 at byte {offset}"))?
            .try_into()?,
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or_else(|| anyhow!("truncated u64 at byte {offset}"))?
            .try_into()?,
    ))
}

fn write_u32(writer: &mut impl Write, value: u32) -> Result<()> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn write_u64(writer: &mut impl Write, value: u64) -> Result<()> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn checked_add(left: usize, right: usize, label: &str) -> Result<usize> {
    left.checked_add(right)
        .ok_or_else(|| anyhow!("mmap dense {label} overflow"))
}

fn checked_mul(left: usize, right: usize, label: &str) -> Result<usize> {
    left.checked_mul(right)
        .ok_or_else(|| anyhow!("mmap dense {label} overflow"))
}

fn usize_to_u32(value: usize, label: &str) -> Result<u32> {
    u32::try_from(value).with_context(|| format!("mmap dense {label} exceeds u32"))
}

fn usize_to_u64(value: usize, label: &str) -> Result<u64> {
    u64::try_from(value).with_context(|| format!("mmap dense {label} exceeds u64"))
}

fn u64_to_usize(value: u64, label: &str) -> Result<usize> {
    usize::try_from(value).with_context(|| format!("mmap dense {label} exceeds usize"))
}
