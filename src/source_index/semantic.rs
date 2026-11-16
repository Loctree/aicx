//! Complete semantic passages and retained, corpus-independent embedding work.

#![cfg_attr(
    not(any(feature = "native-embedder", feature = "cloud-embedder")),
    allow(dead_code, unused_imports)
)]

use super::*;
use std::io::{BufRead, Seek, Write};

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
    fn new(chunk: &aicx_retrieve::ChunkRef, embedding: Vec<f32>) -> Self {
        let mut row = Self {
            chunk_id: chunk.id.clone(),
            content_hash_blake3: source_chunk_content_hash(chunk),
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

fn load_cache(
    path: &Path,
    header: &SourceDenseCheckpointHeader,
) -> Result<HashMap<String, SourceDenseCheckpointRow>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let mut reader = BufReader::new(fs::File::open(path)?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
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
    let mut offset = reader.stream_position()?;
    let mut rows = HashMap::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
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
            rows.insert(row.chunk_id.clone(), row);
        }
        offset = reader.stream_position()?;
    }
    Ok(rows)
}

fn append_rows(path: &Path, rows: &[SourceDenseCheckpointRow]) -> Result<()> {
    let mut file = fs::OpenOptions::new().append(true).open(path)?;
    for row in rows {
        serde_json::to_writer(&mut file, row)?;
        file.write_all(b"\n")?;
    }
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

pub(super) struct SemanticPayload {
    pub chunks: Vec<aicx_retrieve::DenseChunkRef>,
    pub fingerprint: aicx_retrieve::EmbedderFingerprint,
    pub contract: String,
    pub reused: usize,
    pub embedded: usize,
    path: PathBuf,
    header: SourceDenseCheckpointHeader,
    _lock: crate::locks::LockHandle,
}

impl SemanticPayload {
    /// Prune deleted/changed inputs only after the current source/privacy
    /// snapshot was checked. It is safe to retain this work if publication fails.
    pub fn compact(&self) -> Result<()> {
        write_snapshot(
            &self.path,
            &self.header,
            self.chunks
                .iter()
                .map(|d| SourceDenseCheckpointRow::new(&d.chunk, d.embedding.clone())),
        )
    }
}

fn write_snapshot(
    path: &Path,
    header: &SourceDenseCheckpointHeader,
    rows: impl IntoIterator<Item = SourceDenseCheckpointRow>,
) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let result: Result<()> = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)?;
        serde_json::to_writer(&mut file, header)?;
        file.write_all(b"\n")?;
        for row in rows {
            serde_json::to_writer(&mut file, &row)?;
            file.write_all(b"\n")?;
        }
        file.sync_all()?;
        drop(file);
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
pub(super) fn semantic_passages(
    chunks: &[aicx_retrieve::ChunkRef],
    budget: usize,
) -> Vec<aicx_retrieve::ChunkRef> {
    let budget = budget.max(4);
    let mut out = Vec::new();
    for parent in chunks {
        // Older parse caches kept whole preview lines. Normalize once per
        // parent before cloning metadata, including already admitted lexical
        // documents reused without reparsing. Passage text remains complete.
        let mut metadata = parent.metadata.clone();
        if let Some(preview) = metadata.get_mut("preview_lines") {
            let lines: Vec<String> = preview
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .take(6)
                .map(|line| line.chars().take(240).collect())
                .collect();
            *preview = serde_json::json!(lines);
        }
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
                    let mut chunk = aicx_retrieve::ChunkRef {
                        id: format!("{}:passage:{hash}:{}", parent.id, *ordinal),
                        source_path: parent.source_path.clone(),
                        text: passage.to_string(),
                        metadata: metadata.clone(),
                    };
                    *ordinal += 1;
                    chunk.metadata["parent_chunk_id"] = parent.id.clone().into();
                    chunk.metadata["semantic_passage_lines"] = serde_json::json!([passage]);
                    chunk.metadata["passage_start_line"] = start_line.into();
                    chunk.metadata["passage_end_line"] = next_line.into();
                    out.push(chunk);
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
    let indexed = |index, chunk: &aicx_retrieve::ChunkRef, duration_ms| {
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
            content_hash: Some(source_chunk_content_hash(chunk)),
        });
    };
    let result: Result<_> = (|| {
        let root = crate::vector_index::hybrid_root_dir(None)?;
        let path = source_dense_checkpoint_path(&root, &contract, &fingerprint);
        fs::create_dir_all(path.parent().expect("cache parent"))?;
        let lock = crate::locks::acquire_exclusive(root.join("semantic-cache.lock"))?;
        let header = cache_header(&contract, &fingerprint);
        let mut cache = load_cache(&path, &header)?;
        if !path.exists() {
            write_snapshot(&path, &header, [])?;
        }
        let mut ready = HashMap::new();
        for (index, chunk) in chunks.iter().enumerate() {
            if let Some(row) = cache.remove(&chunk.id)
                && row.content_hash_blake3 == source_chunk_content_hash(chunk)
            {
                ready.insert(chunk.id.clone(), row.embedding);
                indexed(index, chunk, 0);
            }
        }
        let reused = ready.len();
        if reused > 0 {
            phase.tick(reused as u64);
        }
        let missing: Vec<_> = chunks
            .iter()
            .enumerate()
            .filter(|(_, c)| !ready.contains_key(&c.id))
            .collect();
        let mut embedded = 0;
        for batch in missing.chunks(engine.embed_batch_size()) {
            let at = Instant::now();
            let texts: Vec<_> = batch.iter().map(|(_, c)| c.text.as_str()).collect();
            let vectors = engine.embed_batch(&texts)?;
            let rows: Vec<_> = batch
                .iter()
                .zip(vectors)
                .map(|((_, c), v)| SourceDenseCheckpointRow::new(c, v))
                .collect();
            // fsync before progress claims the batch can be resumed.
            append_rows(&path, &rows)?;
            for (index, chunk) in batch {
                indexed(*index, chunk, at.elapsed().as_millis() as u64);
            }
            for row in rows {
                ready.insert(row.chunk_id, row.embedding);
                embedded += 1;
            }
            phase.tick((reused + embedded) as u64);
            let snapshot = progress.snapshot();
            eprintln!(
                "aicx index --semantic · passages {}/{} · reused={reused} embedded={embedded} · {:.2} passages/s · ETA {}",
                reused + embedded,
                chunks.len(),
                snapshot.items_per_sec,
                snapshot
                    .eta_secs
                    .map_or_else(|| "unknown".into(), |s| format!("{s:.0}s"))
            );
        }
        let dense = chunks
            .into_iter()
            .map(|chunk| {
                let embedding = ready
                    .remove(&chunk.id)
                    .expect("every passage durably embedded");
                aicx_retrieve::DenseChunkRef { chunk, embedding }
            })
            .collect();
        Ok(SemanticPayload {
            chunks: dense,
            fingerprint,
            contract,
            reused,
            embedded,
            path,
            header,
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
            passages.iter().map(|c| c.text.as_str()).collect::<String>(),
            parent.text
        );
        for passage in &passages {
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
        let chunks = semantic_passages(&[parent], 127);
        assert!(chunks.iter().all(|c| c.text.len() <= 127));
        assert_eq!(
            chunks.iter().map(|c| c.text.as_str()).collect::<String>(),
            text
        );
        assert!(chunks.last().unwrap().text.contains("KONIEC"));
    }
}
