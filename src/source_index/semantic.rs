//! Complete semantic passages and retained, corpus-independent embedding work.

#![cfg_attr(
    not(any(feature = "native-embedder", feature = "cloud-embedder")),
    allow(dead_code, unused_imports)
)]

use super::*;
use std::io::{BufRead, BufWriter, Seek, SeekFrom, Write};

pub const SOURCE_DENSE_CHECKPOINT_SCHEMA: &str = "aicx.source_dense_checkpoint.v2";
pub const SOURCE_DENSE_CHECKPOINT_DIR: &str = "dense-checkpoints";
pub(super) const PASSAGE_INPUT_VERSION: &str = "signal-passages-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDenseCheckpointHeader {
    pub schema: String,
    pub input_contract: String,
    pub embedder_model: String,
    pub embedder_url_hash: String,
    pub dimension: usize,
    pub distance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceDenseCheckpointRow {
    pub chunk_id: String,
    pub content_hash_blake3: String,
    pub embedding: Vec<f32>,
    pub checksum_blake3: String,
}

pub fn source_chunk_content_hash(chunk: &aicx_retrieve::ChunkRef) -> String {
    blake3::hash(chunk.text.as_bytes()).to_hex().to_string()
}

/// Corpus membership is deliberately absent. The model and the complete input
/// contract identify reusable work, both before and after successful admission.
pub fn source_dense_checkpoint_path(
    hybrid_root: &Path,
    input_contract: &str,
    fingerprint: &aicx_retrieve::EmbedderFingerprint,
) -> PathBuf {
    let header = cache_header(input_contract, fingerprint);
    let identity = serde_json::to_vec(&header).expect("serialize cache identity");
    hybrid_root
        .join(SOURCE_DENSE_CHECKPOINT_DIR)
        .join(format!("{}.ndjson", blake3::hash(&identity).to_hex()))
}

fn cache_header(
    contract: &str,
    fp: &aicx_retrieve::EmbedderFingerprint,
) -> SourceDenseCheckpointHeader {
    SourceDenseCheckpointHeader {
        schema: SOURCE_DENSE_CHECKPOINT_SCHEMA.into(),
        input_contract: contract.into(),
        embedder_model: fp.model.clone(),
        embedder_url_hash: fp.url_hash.clone(),
        dimension: fp.dim,
        distance: fp.distance.clone(),
    }
}

impl SourceDenseCheckpointRow {
    fn new(passage: &SemanticPassage<'_>, embedding: Vec<f32>) -> Self {
        let mut row = Self {
            chunk_id: passage.id.clone(),
            content_hash_blake3: passage.content_hash.clone(),
            embedding,
            checksum_blake3: String::new(),
        };
        row.checksum_blake3 = row.checksum();
        row
    }
    fn checksum(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(self.chunk_id.as_bytes());
        h.update(&[0]);
        h.update(self.content_hash_blake3.as_bytes());
        h.update(&[0]);
        for v in &self.embedding {
            h.update(&v.to_le_bytes());
        }
        h.finalize().to_hex().to_string()
    }
    fn valid(&self, dim: usize) -> bool {
        self.embedding.len() == dim
            && self.embedding.iter().all(|v| v.is_finite())
            && self.embedding.iter().any(|v| *v != 0.0)
            && self.checksum() == self.checksum_blake3
    }
}

struct CheckpointLocation {
    offset: u64,
    content_hash: String,
}

fn load_cache(
    path: &Path,
    header: &SourceDenseCheckpointHeader,
) -> Result<HashMap<String, CheckpointLocation>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let mut reader = BufReader::with_capacity(128 * 1024, fs::File::open(path)?);
    let mut line = String::new();
    let mut offset = reader.read_line(&mut line)? as u64;
    let observed: SourceDenseCheckpointHeader = serde_json::from_str(&line).with_context(|| {
        format!(
            "invalid semantic cache header: {}; cache retained",
            path.display()
        )
    })?;
    anyhow::ensure!(
        &observed == header,
        "semantic cache identity mismatch; cache retained"
    );
    let mut rows = HashMap::new();
    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        if !line.ends_with('\n') {
            // Only the unfinished final write is repaired. Complete corrupt
            // rows are cache misses, never trusted vectors or a fresh header.
            drop(reader);
            let file = fs::OpenOptions::new().write(true).open(path)?;
            file.set_len(offset)?;
            file.sync_all()?;
            break;
        }
        if let Ok(row) = serde_json::from_str::<SourceDenseCheckpointRow>(&line)
            && row.valid(header.dimension)
        {
            rows.insert(
                row.chunk_id,
                CheckpointLocation {
                    offset,
                    content_hash: row.content_hash_blake3,
                },
            );
        }
        offset += bytes as u64;
    }
    Ok(rows)
}

fn append_rows(path: &Path, rows: &[SourceDenseCheckpointRow]) -> Result<Vec<u64>> {
    let file = fs::OpenOptions::new().append(true).open(path)?;
    let mut position = file.metadata()?.len();
    let mut writer = BufWriter::with_capacity(128 * 1024, file);
    let mut line = Vec::new();
    let mut offsets = Vec::with_capacity(rows.len());
    for row in rows {
        line.clear();
        serde_json::to_writer(&mut line, row)?;
        line.push(b'\n');
        writer.write_all(&line)?;
        offsets.push(position);
        position += line.len() as u64;
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(offsets)
}

pub(super) struct SemanticPayload {
    pub fingerprint: aicx_retrieve::EmbedderFingerprint,
    pub contract: String,
    pub reused: usize,
    pub embedded: usize,
    path: PathBuf,
    header: SourceDenseCheckpointHeader,
    locations: HashMap<String, CheckpointLocation>,
    input_byte_budget: usize,
    _lock: crate::locks::LockHandle,
}

impl SemanticPayload {
    fn checkpoint_rows<'a>(
        &'a self,
        parents: &'a [aicx_retrieve::ChunkRef],
    ) -> Result<
        impl ExactSizeIterator<Item = Result<(SemanticPassage<'a>, SourceDenseCheckpointRow)>> + 'a,
    > {
        let mut reader = BufReader::with_capacity(128 * 1024, fs::File::open(&self.path)?);
        let mut line = String::new();
        let mut position = 0;
        Ok(semantic_passages(parents, self.input_byte_budget)
            .into_iter()
            .map(move |passage| {
                let location = self
                    .locations
                    .get(&passage.id)
                    .context("dense passage is missing from checkpoint")?;
                if position != location.offset {
                    reader.seek(SeekFrom::Start(location.offset))?;
                    position = location.offset;
                }
                line.clear();
                position += reader.read_line(&mut line)? as u64;
                let row: SourceDenseCheckpointRow = serde_json::from_str(&line)?;
                anyhow::ensure!(
                    line.ends_with('\n')
                        && row.chunk_id == passage.id
                        && row.content_hash_blake3 == passage.content_hash
                        && row.valid(self.header.dimension),
                    "dense passage checkpoint changed before publication"
                );
                Ok((passage, row))
            }))
    }

    pub(super) fn dense_rows<'a>(
        &'a self,
        parents: &'a [aicx_retrieve::ChunkRef],
    ) -> Result<impl ExactSizeIterator<Item = Result<aicx_retrieve::DenseChunkRef>> + 'a> {
        Ok(self.checkpoint_rows(parents)?.map(|row| {
            let (passage, row) = row?;
            Ok(aicx_retrieve::DenseChunkRef {
                chunk: passage.into_chunk(),
                embedding: row.embedding,
            })
        }))
    }

    /// Prune deleted/changed inputs only after the current source/privacy
    /// snapshot was checked. It is safe to retain this work if publication fails.
    pub fn compact(&self, parents: &[aicx_retrieve::ChunkRef]) -> Result<()> {
        write_snapshot(
            &self.path,
            &self.header,
            self.checkpoint_rows(parents)?
                .map(|row| row.map(|(_, value)| value)),
        )
    }
}

fn write_snapshot(
    path: &Path,
    header: &SourceDenseCheckpointHeader,
    rows: impl IntoIterator<Item = Result<SourceDenseCheckpointRow>>,
) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let result: Result<()> = (|| {
        let file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)?;
        let mut writer = BufWriter::with_capacity(128 * 1024, file);
        serde_json::to_writer(&mut writer, header)?;
        writer.write_all(b"\n")?;
        for row in rows {
            serde_json::to_writer(&mut writer, &row?)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        fs::rename(&tmp, path)?;
        #[cfg(unix)]
        fs::File::open(path.parent().expect("cache parent"))?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Partition every admitted byte, on message and whitespace boundaries. Byte
/// offsets remain exact UTF-8 boundaries, including for a long unbroken line.
struct SemanticPassage<'a> {
    parent: &'a aicx_retrieve::ChunkRef,
    id: String,
    content_hash: String,
    text: &'a str,
    start_line: usize,
    end_line: usize,
}

impl SemanticPassage<'_> {
    fn into_chunk(self) -> aicx_retrieve::ChunkRef {
        let mut metadata = serde_json::Map::new();
        if let Some(parent) = self.parent.metadata.as_object() {
            for (key, value) in parent {
                let value = if key == "preview_lines" {
                    let lines: Vec<String> = value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(serde_json::Value::as_str)
                        .take(6)
                        .map(|line| line.chars().take(240).collect())
                        .collect();
                    serde_json::json!(lines)
                } else {
                    value.clone()
                };
                metadata.insert(key.clone(), value);
            }
        }
        metadata.insert("parent_chunk_id".into(), self.parent.id.clone().into());
        metadata.insert(
            "semantic_passage_lines".into(),
            serde_json::json!([self.text]),
        );
        metadata.insert("passage_start_line".into(), self.start_line.into());
        metadata.insert("passage_end_line".into(), self.end_line.into());
        aicx_retrieve::ChunkRef {
            id: self.id,
            source_path: self.parent.source_path.clone(),
            text: self.text.to_string(),
            metadata: metadata.into(),
        }
    }
}

fn semantic_passages(
    chunks: &[aicx_retrieve::ChunkRef],
    budget: usize,
) -> Vec<SemanticPassage<'_>> {
    let budget = budget.max(4);
    let mut out = Vec::new();
    for parent in chunks {
        let text = parent.text.as_str();
        // The extract preamble is provenance, not conversational evidence.
        let start = if text.starts_with("# AICX session extract\n") {
            text.find("\n## ").map_or(0, |n| n + 1)
        } else {
            0
        };
        let body = &text[start..];
        let mut boundaries = vec![start];
        if body.len() > budget {
            boundaries.extend(body.match_indices("\n## ").map(|(n, _)| start + n + 1));
        }
        boundaries.push(text.len());
        let mut seen = HashMap::<String, usize>::new();
        let mut next_line = text[..start].bytes().filter(|b| *b == b'\n').count() + 1;
        for pair in boundaries.windows(2) {
            let mut lo = pair[0];
            while lo < pair[1] {
                let mut hi = (lo + budget).min(pair[1]);
                while !text.is_char_boundary(hi) {
                    hi -= 1;
                }
                if hi < pair[1] {
                    let window = &text[lo..hi];
                    if let Some((n, c)) = window
                        .char_indices()
                        .rev()
                        .find(|(n, c)| *n > window.len() / 2 && c.is_whitespace())
                    {
                        hi = lo + n + c.len_utf8();
                    }
                }
                let passage = &text[lo..hi];
                let start_line = next_line;
                next_line += passage.bytes().filter(|b| *b == b'\n').count();
                if !passage.trim().is_empty() {
                    let hash = blake3::hash(passage.as_bytes()).to_hex().to_string();
                    let ordinal = seen.entry(hash.clone()).or_default();
                    out.push(SemanticPassage {
                        parent,
                        id: format!("{}:passage:{hash}:{}", parent.id, *ordinal),
                        content_hash: hash,
                        text: passage,
                        start_line,
                        end_line: next_line,
                    });
                    *ordinal += 1;
                }
                lo = hi;
            }
        }
    }
    out
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub(super) fn input_contract(engine: &crate::embedder::EmbeddingEngine) -> Result<String> {
    let mut h = blake3::Hasher::new();
    h.update(PASSAGE_INPUT_VERSION.as_bytes());
    h.update(&engine.input_byte_budget().to_le_bytes());
    // Native identity includes actual weights, not merely a mutable file name.
    let path = match engine.source() {
        crate::embedder::NativeEmbeddingSource::HfCache { path, .. }
        | crate::embedder::NativeEmbeddingSource::ExplicitPath(path) => Some(path),
        crate::embedder::NativeEmbeddingSource::CloudEndpoint(_) => None,
    };
    if let Some(path) = path {
        h.update(b"native-weights-blake3");
        h.update(&aicx_retrieve::verified_file_digest(path)?);
    }
    Ok(format!("{PASSAGE_INPUT_VERSION}:{}", h.finalize().to_hex()))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub(super) fn embed_chunks_for_semantic(
    _aicx_home: &Path,
    _source_fingerprint: &str,
    chunks: &[aicx_retrieve::ChunkRef],
    reporter: Arc<dyn Reporter>,
    prepared_engine: Option<crate::embedder::EmbeddingEngine>,
) -> Result<SemanticPayload> {
    use aicx_progress_contracts::{EventSink, IndexEvent};
    let prepare = Phase::start(reporter.clone(), "index_embed_prepare", None);
    let prepare_hb = Heartbeat::spawn_with_backoff(
        prepare.clone(),
        Duration::from_secs(2),
        Duration::from_secs(15),
    );
    let prepared: Result<_> = (|| {
        let engine = prepared_engine
            .map_or_else(crate::embedder::EmbeddingEngine::new, Ok)
            .context("initialize embedder for aicx index --semantic")?;
        let contract = input_contract(&engine)?;
        let fingerprint = crate::vector_index::hybrid_embedder_fingerprint(engine.info());
        let chunks = semantic_passages(chunks, engine.input_byte_budget());
        anyhow::ensure!(
            !chunks.is_empty(),
            "semantic projection produced no passages"
        );
        Ok((engine, contract, fingerprint, chunks))
    })();
    prepare_hb.stop();
    let (mut engine, contract, fingerprint, chunks) = match prepared {
        Ok(value) => {
            prepare.finish_ok("model identity and complete passage projection prepared");
            value
        }
        Err(error) => {
            prepare.finish_err(&error, Some("check embedder configuration and retry"));
            return Err(error);
        }
    };
    let phase = Phase::start(reporter, "index_embed", Some(chunks.len() as u64));
    let heartbeat = Heartbeat::spawn_with_backoff(
        phase.clone(),
        Duration::from_secs(2),
        Duration::from_secs(15),
    );
    let progress = aicx_monitor::IndexProgressMonitor::default();
    progress.on_event(&IndexEvent::RunStarted {
        total_items: chunks.len(),
        namespace: "index_embed".into(),
        source_label: "complete source passages".into(),
        parallelism: 1,
        started_at: chrono::Utc::now(),
    });
    let indexed = |index, chunk: &SemanticPassage<'_>, duration_ms| {
        progress.on_event(&IndexEvent::ItemStarted {
            item_index: index,
            label: chunk.id.clone(),
            size_bytes: Some(chunk.text.len() as u64),
        });
        progress.on_event(&IndexEvent::ItemIndexed {
            item_index: index,
            label: chunk.id.clone(),
            chunks_indexed: 1,
            duration_ms,
            embedder_ms: None,
            tokens_estimated: None,
            content_hash: Some(chunk.content_hash.clone()),
        });
    };
    let result: Result<_> = (|| {
        let root = crate::vector_index::hybrid_root_dir(None)?;
        let path = source_dense_checkpoint_path(&root, &contract, &fingerprint);
        fs::create_dir_all(path.parent().expect("cache parent"))?;
        let lock = crate::locks::acquire_exclusive(root.join("semantic-cache.lock"))?;
        let header = cache_header(&contract, &fingerprint);
        let mut locations = load_cache(&path, &header)?;
        if !path.exists() {
            write_snapshot(&path, &header, [])?;
        }
        let mut reused = 0;
        let mut missing = Vec::new();
        for (index, chunk) in chunks.iter().enumerate() {
            if locations
                .get(&chunk.id)
                .is_some_and(|row| row.content_hash == chunk.content_hash)
            {
                reused += 1;
                indexed(index, chunk, 0);
            } else {
                missing.push((index, chunk));
            }
        }
        if reused > 0 {
            phase.tick(reused as u64);
        }
        let mut embedded = 0;
        let embedding_start = Instant::now();
        for batch in missing.chunks(engine.embed_batch_size()) {
            let at = Instant::now();
            let texts: Vec<_> = batch.iter().map(|(_, c)| c.text).collect();
            let vectors = engine.embed_batch(&texts)?;
            anyhow::ensure!(
                vectors.len() == batch.len(),
                "embedder returned the wrong number of vectors"
            );
            let rows: Vec<_> = batch
                .iter()
                .zip(vectors)
                .map(|((_, c), v)| SourceDenseCheckpointRow::new(c, v))
                .collect();
            // fsync before progress claims the batch can be resumed.
            let offsets = append_rows(&path, &rows)?;
            for (index, chunk) in batch {
                indexed(*index, chunk, at.elapsed().as_millis() as u64);
            }
            for (row, offset) in rows.into_iter().zip(offsets) {
                locations.insert(
                    row.chunk_id,
                    CheckpointLocation {
                        offset,
                        content_hash: row.content_hash_blake3,
                    },
                );
                embedded += 1;
            }
            phase.tick((reused + embedded) as u64);
            let rate = embedded as f64 / embedding_start.elapsed().as_secs_f64();
            let eta = chunks.len().saturating_sub(reused + embedded) as f64 / rate;
            eprintln!(
                "aicx index --semantic · passages {}/{} · reused={reused} embedded={embedded} · {rate:.2} passages/s · ETA {eta:.0}s",
                reused + embedded,
                chunks.len(),
            );
        }
        Ok(SemanticPayload {
            fingerprint,
            contract,
            reused,
            embedded,
            path,
            header,
            locations,
            input_byte_budget: engine.input_byte_budget(),
            _lock: lock,
        })
    })();
    heartbeat.stop();
    match &result {
        Ok(payload) => {
            let snapshot = progress.snapshot();
            progress.on_event(&IndexEvent::RunCompleted {
                processed: snapshot.processed,
                indexed: snapshot.indexed,
                skipped: snapshot.skipped,
                failed: snapshot.failed,
                total_chunks: snapshot.total_chunks,
                elapsed: phase.started_at.elapsed(),
                stopped_early: false,
            });
            phase.finish_ok(format!(
                "reused={} embedded={}",
                payload.reused, payload.embedded
            ));
        }
        Err(error) => {
            progress.on_event(&IndexEvent::RunFailed {
                error: format!("{error:#}"),
                processed_before_failure: progress.snapshot().processed,
            });
            phase.finish_err(
                error,
                Some("retry aicx index --semantic; completed passages remain checkpointed"),
            );
        }
    }
    result
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
pub(super) fn embed_chunks_for_semantic(
    _home: &Path,
    _source: &str,
    _chunks: &[aicx_retrieve::ChunkRef],
    _reporter: Arc<dyn Reporter>,
) -> Result<SemanticPayload> {
    anyhow::bail!("aicx index --semantic requires cloud-embedder and/or native-embedder features")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semantic_passages_bound_legacy_preview_lines() {
        // Warm parse caches predating bounded previews can retain an entire
        // single-line message here. Repeating it in every passage is quadratic.
        let text = "ż".repeat(32 * 1024);
        let parent = aicx_retrieve::ChunkRef {
            id: "codex:legacy-preview".into(),
            source_path: "extract.md".into(),
            text: text.clone(),
            metadata: serde_json::json!({"preview_lines": [text]}),
        };
        let passages = semantic_passages(std::slice::from_ref(&parent), 504);
        assert!(passages.len() > 100);
        assert_eq!(
            passages.iter().map(|c| c.text).collect::<String>(),
            parent.text
        );
        for passage in passages.into_iter().map(SemanticPassage::into_chunk) {
            let preview = passage.metadata["preview_lines"].as_array().unwrap();
            assert!(preview.len() <= 6);
            assert!(
                preview
                    .iter()
                    .all(|line| line.as_str().unwrap().chars().count() <= 240),
                "passage inherited an unbounded legacy preview"
            );
            assert!(serde_json::to_vec(&passage.metadata).unwrap().len() < 2_000);
        }
        assert!(parent.metadata["preview_lines"][0].as_str().unwrap().len() > 60_000);
    }

    #[test]
    fn passage_partition_keeps_every_utf8_byte_and_final_message() {
        let text = format!(
            "## first · user\n\n{}\n## last · assistant\n\nKONIEC",
            "Zażółć 🦀 gęślą. ".repeat(250)
        );
        let parent = aicx_retrieve::ChunkRef {
            id: "codex:s".into(),
            source_path: "extract.md".into(),
            text: text.clone(),
            metadata: serde_json::json!({}),
        };
        let chunks = semantic_passages(std::slice::from_ref(&parent), 127);
        assert!(chunks.iter().all(|c| c.text.len() <= 127));
        assert_eq!(chunks.iter().map(|c| c.text).collect::<String>(), text);
        assert!(chunks.last().unwrap().text.contains("KONIEC"));
    }
}
