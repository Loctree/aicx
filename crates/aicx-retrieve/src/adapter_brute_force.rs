// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
//! Pure-Rust brute-force `DenseIndex` adapter.
//!
//! This is the Linux-musl-safe fallback dense leg from research synthesis
//! §6 and §8: zero C dependencies, cross-platform clean, ~50 LOC of hot
//! cosine math that we can verify by inspection. It scales well to ~500k
//! 4096-d vectors on a single host before P95 latency becomes the gate
//! (see synthesis §C.1 open question).
//!
//! Persistence is split from the trait surface. The adapter is in-memory
//! during `build`/`insert`/`query`; the orchestration layer (track D / H)
//! decides when to call [`BruteForceAdapter::persist_ndjson`] and
//! [`BruteForceAdapter::load_ndjson`]. This keeps trait semantics pure and
//! lets the brute-force adapter compose into other write protocols later.

use std::fs;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use aicx_parser::sanitize::{MAX_VALIDATED_BYTES, read_line_capped};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::{DenseChunkRef, DenseIndex, Distance, FilterSet, Hit};

/// On-disk row of the brute-force NDJSON store.
///
/// Mirrors `crate::types::DenseChunkRef` flattened: persisting the inner
/// `ChunkRef` as separate fields keeps each line trivial to parse and
/// human-readable for ops debugging.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DenseEntry {
    chunk_id: String,
    source_path: String,
    embedding: Vec<f32>,
    metadata: serde_json::Value,
}

impl DenseEntry {
    fn from_chunk(c: &DenseChunkRef) -> Self {
        Self {
            chunk_id: c.chunk.id.clone(),
            source_path: c.chunk.source_path.clone(),
            embedding: c.embedding.clone(),
            metadata: c.chunk.metadata.clone(),
        }
    }
}

/// Header of the brute-force NDJSON store. Lets `load_ndjson` reject
/// mismatched-dim corpora before scoring (mirrors `vector_index::IndexHeader`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BruteForceHeader {
    schema_version: String,
    dim: usize,
    distance: Distance,
    entry_count: usize,
}

const BRUTE_FORCE_SCHEMA_VERSION: &str = "1.0";

/// Canonical `Manifest::dense_kind` value emitted by this adapter.
///
/// Re-exported at the crate root so manifest orchestration (track D) can
/// reference it without taking a string dependency on the adapter module.
pub const BRUTE_FORCE_KIND: &str = "brute_force_ndjson";

/// In-process dense adapter with NDJSON persistence.
pub struct BruteForceAdapter {
    dim: usize,
    distance: Distance,
    entries: Vec<DenseEntry>,
}

impl BruteForceAdapter {
    /// Create a fresh adapter at the requested dimension. Default distance
    /// is `Cosine` to match aicx's qwen3-embedding:8b production embedder.
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            distance: Distance::Cosine,
            entries: Vec::new(),
        }
    }

    /// Override the distance metric. Build before adding entries.
    pub fn with_distance(mut self, distance: Distance) -> Self {
        self.distance = distance;
        self
    }

    /// Persist current state to `path` as NDJSON: header line + one entry
    /// per data line. Writes to `<path>.tmp` then renames into place so a
    /// partial write cannot corrupt the existing index. Caller is
    /// responsible for ensuring the parent directory exists.
    pub fn persist_ndjson(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create parent dir for {}", path.display()))?;
        }
        let mut tmp_path = path.to_path_buf();
        let tmp_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(name) => format!("{name}.tmp"),
            None => return Err(anyhow!("invalid persist path: {}", path.display())),
        };
        tmp_path.set_file_name(tmp_name);

        let file = create_validated(&tmp_path)
            .with_context(|| format!("create tmp {}", tmp_path.display()))?;
        let mut writer = BufWriter::new(file);

        let header = BruteForceHeader {
            schema_version: BRUTE_FORCE_SCHEMA_VERSION.to_string(),
            dim: self.dim,
            distance: self.distance,
            entry_count: self.entries.len(),
        };
        writeln!(writer, "{}", serde_json::to_string(&header)?)?;
        for entry in &self.entries {
            writeln!(writer, "{}", serde_json::to_string(entry)?)?;
        }
        writer
            .flush()
            .with_context(|| format!("flush tmp {}", tmp_path.display()))?;
        drop(writer);

        fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "commit brute-force index: {} -> {}",
                tmp_path.display(),
                path.display()
            )
        })?;
        Ok(())
    }

    /// Load adapter state from an NDJSON file produced by [`Self::persist_ndjson`].
    ///
    /// Fails fast on dim mismatch between header and stored embeddings,
    /// and propagates the same `corrupt_count` / threshold policy as
    /// `vector_index::scan_index_entries` (corrupt-row count surfaced,
    /// no silent swallow). For the brute-force adapter we apply the
    /// policy here directly because there is no upstream orchestrator
    /// in tests; the production path will route through the manifest
    /// (track D) and may relax this to warn-only.
    pub fn load_ndjson(&mut self, path: &Path) -> Result<LoadStats> {
        let file = open_validated(path)
            .with_context(|| format!("open brute-force index: {}", path.display()))?;
        let mut reader = BufReader::new(file);

        let header_line = read_line_capped(&mut reader, MAX_VALIDATED_BYTES)
            .with_context(|| format!("read brute-force header in {}", path.display()))?
            .ok_or_else(|| anyhow!("empty brute-force index: {}", path.display()))?;
        if header_line.exceeded {
            anyhow::bail!(
                "brute-force header line exceeds {} bytes in {}",
                MAX_VALIDATED_BYTES,
                path.display()
            );
        }
        let header_line = strip_line_ending(header_line.line);
        let header: BruteForceHeader = serde_json::from_str(&header_line)
            .with_context(|| format!("parse header in {}", path.display()))?;

        if header.dim != self.dim {
            return Err(anyhow!(
                "brute-force index dim mismatch in {}: header={}, adapter={}",
                path.display(),
                header.dim,
                self.dim
            ));
        }
        if header.distance != self.distance {
            return Err(anyhow!(
                "brute-force index distance mismatch in {}: header={:?}, adapter={:?}",
                path.display(),
                header.distance,
                self.distance
            ));
        }

        self.entries.clear();
        let mut stats = LoadStats::default();
        let mut line_no = 2usize;
        while let Some(line) = read_line_capped(&mut reader, MAX_VALIDATED_BYTES)
            .with_context(|| format!("read brute-force line {} in {}", line_no, path.display()))?
        {
            let exceeded = line.exceeded;
            let line = strip_line_ending(line.line);
            if line.is_empty() {
                line_no += 1;
                continue;
            }
            stats.total_data_lines += 1;
            if exceeded {
                stats.corrupt_count += 1;
                tracing::warn!(
                    target: "aicx_retrieve::brute_force",
                    line_no,
                    max_bytes = MAX_VALIDATED_BYTES,
                    "oversized NDJSON line in brute-force index skipped"
                );
                line_no += 1;
                continue;
            }
            match serde_json::from_str::<DenseEntry>(&line) {
                Ok(entry) => {
                    if entry.embedding.len() != self.dim {
                        stats.corrupt_count += 1;
                        tracing::warn!(
                            target: "aicx_retrieve::brute_force",
                            chunk_id = %entry.chunk_id,
                            row_dim = entry.embedding.len(),
                            index_dim = self.dim,
                            "skipping row with mismatched embedding dim"
                        );
                        continue;
                    }
                    self.entries.push(entry);
                }
                Err(err) => {
                    stats.corrupt_count += 1;
                    tracing::warn!(
                        target: "aicx_retrieve::brute_force",
                        occurrence = stats.corrupt_count,
                        error = %err,
                        "corrupt NDJSON line in brute-force index"
                    );
                }
            }
            line_no += 1;
        }
        Ok(stats)
    }
}

fn strip_line_ending(mut line: String) -> String {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    line
}

/// Diagnostics from a brute-force NDJSON load. Mirrors the shape
/// `vector_index::ScanResult` exposes so orchestration policy can be
/// shared between legs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadStats {
    pub total_data_lines: usize,
    pub corrupt_count: usize,
}

impl DenseIndex for BruteForceAdapter {
    fn dim(&self) -> usize {
        self.dim
    }

    fn distance(&self) -> Distance {
        self.distance
    }

    fn kind(&self) -> &str {
        BRUTE_FORCE_KIND
    }

    fn build(&mut self, chunks: &[DenseChunkRef]) -> Result<()> {
        self.entries.clear();
        self.entries.reserve(chunks.len());
        for chunk in chunks {
            if chunk.embedding.len() != self.dim {
                return Err(anyhow!(
                    "build: chunk {} has dim {}, adapter dim {}",
                    chunk.chunk.id,
                    chunk.embedding.len(),
                    self.dim
                ));
            }
            self.entries.push(DenseEntry::from_chunk(chunk));
        }
        Ok(())
    }

    fn insert(&mut self, chunk: &DenseChunkRef) -> Result<()> {
        if chunk.embedding.len() != self.dim {
            return Err(anyhow!(
                "insert: chunk {} has dim {}, adapter dim {}",
                chunk.chunk.id,
                chunk.embedding.len(),
                self.dim
            ));
        }
        self.entries.push(DenseEntry::from_chunk(chunk));
        Ok(())
    }

    fn query(&self, embedding: &[f32], limit: usize, filters: &FilterSet) -> Result<Vec<Hit>> {
        if embedding.len() != self.dim {
            return Err(anyhow!(
                "query: embedding dim {} != adapter dim {}",
                embedding.len(),
                self.dim
            ));
        }
        // Filter pre-pass: apply `filters` BEFORE scoring so we do not
        // spend cosine math on rows that the orchestrator will discard
        // anyway. This is the discipline from research §C.5 (filter
        // pre-pass over post-filter on top-K) baked into the leg.
        let mut scored: Vec<(f32, &DenseEntry)> = self
            .entries
            .iter()
            .filter(|e| filter_matches(&e.metadata, filters))
            .map(|e| (score_distance(embedding, &e.embedding, self.distance), e))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        Ok(scored
            .into_iter()
            .enumerate()
            .map(|(rank, (score, e))| Hit {
                chunk_id: e.chunk_id.clone(),
                score,
                rank,
                source: BRUTE_FORCE_KIND.to_string(),
                metadata: e.metadata.clone(),
            })
            .collect())
    }

    fn count(&self) -> usize {
        self.entries.len()
    }
}

fn score_distance(a: &[f32], b: &[f32], distance: Distance) -> f32 {
    match distance {
        Distance::Cosine => cosine(a, b),
        Distance::Dot => dot(a, b),
        // Euclidean is a distance (smaller is better); we negate so the
        // ranking semantics ("higher score = better") stay uniform across
        // distance variants.
        Distance::Euclidean => -euclidean(a, b),
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot_p: f32 = 0.0;
    let mut norm_a: f32 = 0.0;
    let mut norm_b: f32 = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot_p += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot_p / (norm_a.sqrt() * norm_b.sqrt())
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn euclidean(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return f32::INFINITY;
    }
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f32>()
        .sqrt()
}

fn filter_matches(metadata: &serde_json::Value, filters: &FilterSet) -> bool {
    for (key, expected) in &filters.values {
        match metadata.get(key) {
            Some(actual) if actual == expected => continue,
            _ => return false,
        }
    }
    true
}

/// Convenience constructor for the orchestration layer: build a
/// `BruteForceAdapter` and load its on-disk NDJSON in one call.
pub fn load_from_ndjson(path: &Path, dim: usize, distance: Distance) -> Result<BruteForceAdapter> {
    let mut adapter = BruteForceAdapter::new(dim).with_distance(distance);
    adapter.load_ndjson(path)?;
    Ok(adapter)
}

/// Default on-disk filename for the brute-force NDJSON store inside a
/// manifest-managed bucket directory (e.g.
/// `<bucket>/dense_brute_force.ndjson`). Centralizing the name here keeps
/// the manifest layer and the adapter agreed on the canonical path.
pub const DEFAULT_NDJSON_FILE_NAME: &str = "dense_brute_force.ndjson";

/// Canonical relative path for the brute-force NDJSON store within a
/// manifest-managed bucket directory.
pub fn default_ndjson_path(base_dir: &Path) -> PathBuf {
    base_dir.join(DEFAULT_NDJSON_FILE_NAME)
}

fn validate_index_path(path: &Path) -> Result<&Path> {
    let path_str = path.to_string_lossy();
    if path_str.contains('\0') || path_str.contains('\n') || path_str.contains('\r') {
        return Err(anyhow!(
            "invalid brute-force index path: {}",
            path.display()
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(anyhow!(
            "brute-force index path must not contain traversal components: {}",
            path.display()
        ));
    }
    Ok(path)
}

fn create_validated(path: &Path) -> Result<File> {
    let path = validate_index_path(path)?;
    fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))
}

fn open_validated(path: &Path) -> Result<File> {
    let path = validate_index_path(path)?;
    fs::OpenOptions::new()
        .read(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChunkRef;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_chunk(id: &str, agent: &str, embedding: Vec<f32>) -> DenseChunkRef {
        DenseChunkRef {
            chunk: ChunkRef {
                id: id.to_string(),
                source_path: format!("/tmp/aicx/{id}.md"),
                text: format!("chunk body {id}"),
                metadata: json!({ "agent": agent, "date": "20260515" }),
            },
            embedding,
        }
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aicx-bf-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn build_then_query_returns_ranked_hits() {
        let mut adapter = BruteForceAdapter::new(3);
        let chunks = vec![
            make_chunk("a", "claude", vec![1.0, 0.0, 0.0]),
            make_chunk("b", "claude", vec![0.0, 1.0, 0.0]),
            make_chunk("c", "claude", vec![0.5, 0.5, 0.0]),
        ];
        adapter.build(&chunks).expect("build");
        assert_eq!(adapter.count(), 3);

        let hits = adapter
            .query(&[1.0, 0.0, 0.0], 3, &FilterSet::default())
            .expect("query");
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].chunk_id, "a", "exact match must rank first");
        assert_eq!(hits[0].rank, 0);
        assert!(hits[0].score > hits[1].score, "ranking must be descending");
        assert_eq!(hits[0].source, BRUTE_FORCE_KIND);
    }

    #[test]
    fn build_dim_mismatch_fails_with_chunk_id() {
        let mut adapter = BruteForceAdapter::new(3);
        let chunks = vec![make_chunk("bad", "claude", vec![1.0, 0.0])];
        let err = adapter.build(&chunks).unwrap_err().to_string();
        assert!(err.contains("bad"), "error must surface chunk id: {err}");
        assert!(
            err.contains("dim 2"),
            "error must surface actual dim: {err}"
        );
    }

    #[test]
    fn query_dim_mismatch_fails_fast() {
        let mut adapter = BruteForceAdapter::new(3);
        adapter
            .build(&[make_chunk("a", "claude", vec![1.0, 0.0, 0.0])])
            .unwrap();
        let err = adapter
            .query(&[1.0, 0.0], 5, &FilterSet::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("dim 2"));
        assert!(err.contains("adapter dim 3"));
    }

    #[test]
    fn filter_pre_pass_drops_non_matching_rows_before_scoring() {
        let mut adapter = BruteForceAdapter::new(2);
        adapter
            .build(&[
                make_chunk("c1", "claude", vec![1.0, 0.0]),
                make_chunk("c2", "codex", vec![1.0, 0.0]),
                make_chunk("c3", "claude", vec![0.9, 0.1]),
                make_chunk("c4", "gemini", vec![1.0, 0.0]),
            ])
            .unwrap();

        let mut filters = FilterSet::default();
        filters.values.insert("agent".to_string(), json!("claude"));
        let hits = adapter.query(&[1.0, 0.0], 10, &filters).expect("query");

        assert_eq!(hits.len(), 2);
        for hit in &hits {
            assert_eq!(hit.metadata.get("agent").unwrap(), &json!("claude"));
        }
    }

    #[test]
    fn empty_filter_set_returns_all_scored_rows() {
        let mut adapter = BruteForceAdapter::new(2);
        adapter
            .build(&[
                make_chunk("a", "claude", vec![1.0, 0.0]),
                make_chunk("b", "codex", vec![0.0, 1.0]),
            ])
            .unwrap();
        let hits = adapter
            .query(&[1.0, 0.0], 10, &FilterSet::default())
            .expect("query");
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn cosine_and_euclidean_rank_differently() {
        // Cosine ignores magnitude; Euclidean penalizes it.
        let mut cos_adapter = BruteForceAdapter::new(2);
        cos_adapter
            .build(&[
                make_chunk("near_dir", "claude", vec![5.0, 0.0]),
                make_chunk("near_mag", "claude", vec![1.0, 0.1]),
            ])
            .unwrap();
        let cos_hits = cos_adapter
            .query(&[1.0, 0.0], 2, &FilterSet::default())
            .unwrap();
        // (5, 0) has cosine 1.0 with (1, 0); (1, 0.1) has cosine ~0.995.
        assert_eq!(cos_hits[0].chunk_id, "near_dir");

        let mut euc_adapter = BruteForceAdapter::new(2).with_distance(Distance::Euclidean);
        euc_adapter
            .build(&[
                make_chunk("near_dir", "claude", vec![5.0, 0.0]),
                make_chunk("near_mag", "claude", vec![1.0, 0.1]),
            ])
            .unwrap();
        let euc_hits = euc_adapter
            .query(&[1.0, 0.0], 2, &FilterSet::default())
            .unwrap();
        // Euclidean distance: (5, 0) is 4.0 away; (1, 0.1) is 0.1 away.
        // We negate so larger score = closer.
        assert_eq!(euc_hits[0].chunk_id, "near_mag");
    }

    #[test]
    fn persist_then_load_round_trip_preserves_entries() {
        let dir = tempdir();
        let path = dir.join("brute.ndjson");

        let mut writer = BruteForceAdapter::new(3);
        writer
            .build(&[
                make_chunk("a", "claude", vec![1.0, 0.0, 0.0]),
                make_chunk("b", "codex", vec![0.0, 1.0, 0.0]),
                make_chunk("c", "gemini", vec![0.0, 0.0, 1.0]),
            ])
            .unwrap();
        writer.persist_ndjson(&path).expect("persist");

        let mut reader = BruteForceAdapter::new(3);
        let stats = reader.load_ndjson(&path).expect("load");
        assert_eq!(stats.total_data_lines, 3);
        assert_eq!(stats.corrupt_count, 0);
        assert_eq!(reader.count(), 3);

        let hits = reader
            .query(&[0.0, 1.0, 0.0], 1, &FilterSet::default())
            .unwrap();
        assert_eq!(hits[0].chunk_id, "b");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_dim_mismatch_in_header_fails_fast() {
        let dir = tempdir();
        let path = dir.join("bad-dim.ndjson");

        let mut writer = BruteForceAdapter::new(2);
        writer
            .build(&[make_chunk("a", "claude", vec![1.0, 0.0])])
            .unwrap();
        writer.persist_ndjson(&path).unwrap();

        let mut reader = BruteForceAdapter::new(3); // wrong dim
        let err = reader.load_ndjson(&path).unwrap_err().to_string();
        assert!(err.contains("dim mismatch"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_tolerates_corrupt_rows_and_reports_count() {
        let dir = tempdir();
        let path = dir.join("with-corrupt.ndjson");

        // Build a valid file, then append a corrupt row by hand.
        let mut writer = BruteForceAdapter::new(2);
        writer
            .build(&[
                make_chunk("a", "claude", vec![1.0, 0.0]),
                make_chunk("b", "claude", vec![0.0, 1.0]),
            ])
            .unwrap();
        writer.persist_ndjson(&path).unwrap();

        // Append a corrupt line directly. Rewriting the file would lose
        // the canonical header; appending tests the corrupt-tolerance
        // path that a partially-written live-tail would actually hit.
        use std::io::Write as _;
        let mut handle = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(handle, "{{not json").unwrap();
        drop(handle);

        let mut reader = BruteForceAdapter::new(2);
        let stats = reader.load_ndjson(&path).expect("load with corrupt row");
        assert_eq!(stats.total_data_lines, 3);
        assert_eq!(stats.corrupt_count, 1);
        assert_eq!(reader.count(), 2, "only valid rows loaded");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_skips_oversized_row_and_reads_following_row() {
        let dir = tempdir();
        let path = dir.join("with-oversized.ndjson");
        let header = BruteForceHeader {
            schema_version: BRUTE_FORCE_SCHEMA_VERSION.to_string(),
            dim: 2,
            distance: Distance::Cosine,
            entry_count: 2,
        };
        let valid =
            DenseEntry::from_chunk(&make_chunk("after-oversized", "claude", vec![1.0, 0.0]));

        let mut contents = serde_json::to_string(&header).unwrap();
        contents.push('\n');
        contents.push_str(&"x".repeat(MAX_VALIDATED_BYTES + 1));
        contents.push('\n');
        contents.push_str(&serde_json::to_string(&valid).unwrap());
        contents.push('\n');
        fs::write(&path, contents).unwrap();

        let mut reader = BruteForceAdapter::new(2);
        let stats = reader.load_ndjson(&path).expect("load with oversized row");
        assert_eq!(stats.total_data_lines, 2);
        assert_eq!(stats.corrupt_count, 1);
        assert_eq!(reader.count(), 1);

        let hits = reader
            .query(&[1.0, 0.0], 1, &FilterSet::default())
            .expect("query valid row after oversized row");
        assert_eq!(hits[0].chunk_id, "after-oversized");

        let _ = fs::remove_dir_all(&dir);
    }
}
