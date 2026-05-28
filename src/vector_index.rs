//! Vector index builder for `aicx` semantic search.
//!
//! Goal: take the canonical store ([`crate::store`]) markdown chunks and
//! materialize a vector representation per chunk so `aicx search` can rank
//! by cosine similarity rather than line-overlap fuzzy. The index is
//! configuration-driven so the same command works for the in-process
//! native GGUF embedder ([`aicx_embeddings`]) and the cloud HTTP embed
//! endpoint.
//!
//! Two surfaces:
//! - [`dry_run_index`]: probe the embedder, sample N chunks, embed them,
//!   return stats. Used for ETA estimation before a full rebuild.
//! - [`write_index`] / [`query_index`] (Iter 3): persistent NDJSON-backed
//!   index per project at `~/.aicx/indexed/<bucket>/embeddings.ndjson`,
//!   queryable via in-process cosine similarity.
//!
//! NDJSON over Lance for the MVP: each chunk is one JSON line
//! (`{id, project, agent, date, path, embedding}`). One file per project
//! bucket so per-project rebuilds and crashes do not corrupt others. Lance
//! migration is a separate iteration once we validate the operator-side
//! query patterns; the public API ([`query_index`]) stays the same so the
//! storage swap is transparent to callers.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aicx_progress_contracts::{IndexEvent, RollingRate};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Default cap on how much of a chunk's content the embedder sees.
///
/// Embedders have a max-token window (typically 512). This is a
/// conservative byte-level cap that keeps each embed call fast and within
/// any reasonable model context.
pub const DEFAULT_EMBED_PREFIX_BYTES: usize = 2048;

fn strip_line_ending(mut line: String) -> String {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    line
}

fn oversized_line_error(path: &Path, line_no: usize, context: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{context} line {line_no} exceeds {} bytes in {}",
            crate::sanitize::MAX_VALIDATED_BYTES,
            path.display()
        ),
    )
}

fn read_index_line_capped<R: BufRead>(
    reader: &mut R,
    path: &Path,
    line_no: usize,
    context: &str,
) -> io::Result<Option<String>> {
    let Some(line) =
        crate::sanitize::read_line_capped(reader, crate::sanitize::MAX_VALIDATED_BYTES)?
    else {
        return Ok(None);
    };
    if line.exceeded {
        return Err(oversized_line_error(path, line_no, context));
    }
    Ok(Some(strip_line_ending(line.line)))
}

struct CappedIndexLines<R> {
    reader: R,
    path: PathBuf,
    line_no: usize,
    context: &'static str,
}

fn capped_index_lines<R: BufRead>(
    reader: R,
    path: &Path,
    first_line_no: usize,
    context: &'static str,
) -> CappedIndexLines<R> {
    CappedIndexLines {
        reader,
        path: path.to_path_buf(),
        line_no: first_line_no,
        context,
    }
}

impl<R: BufRead> Iterator for CappedIndexLines<R> {
    type Item = io::Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        let line_no = self.line_no;
        self.line_no += 1;
        read_index_line_capped(&mut self.reader, &self.path, line_no, self.context).transpose()
    }
}

/// Aggregate report emitted by [`dry_run_index`].
///
/// All fields are JSON-serializable so the same struct backs both the
/// human stderr output and the `--json` machine output.
#[derive(Debug, Clone, Serialize)]
pub struct IndexStats {
    /// Total chunk files discovered in the canonical store after the
    /// `project` filter has been applied.
    pub chunks_total: usize,
    /// Number of chunks actually fed to the embedder. Capped by `sample`
    /// from the caller; equals `chunks_total` when `sample == 0`.
    pub chunks_sampled: usize,
    /// Successful embedding computations. Lower than `chunks_sampled`
    /// when individual reads or embeds fail; failures are tracked
    /// separately in `embed_errors`.
    pub embeddings_computed: usize,
    /// Per-chunk read or embed failures encountered during the pass.
    pub embed_errors: usize,
    /// Output dimension of the resolved embedding model (informational
    /// for the operator; the persistent Lance schema will lock to this).
    pub dimension: Option<usize>,
    /// Resolved model identifier (e.g. `"F2LLM-v2-0.6B.Q4_K_M.gguf"`).
    pub model_id: Option<String>,
    /// Resolved embedding profile (`base` / `dev` / `premium`).
    pub model_profile: Option<String>,
    /// Optional fallback reason set when the embedder cannot load.
    pub fallback_reason: Option<String>,
    /// Wall-clock time of the dry-run pass (excluding store scan).
    pub elapsed_ms: u128,
    /// `true` for [`dry_run_index`] (probe-only). `false` for
    /// [`write_index`] (persistent build).
    pub dry_run: bool,
    /// Filesystem path of the materialized index, when [`write_index`]
    /// produced one. `None` for `dry_run_index`. Public so callers can
    /// echo it back to the operator after a build.
    pub index_path: Option<PathBuf>,
    /// Number of already-materialized embeddings reused from a surviving
    /// `<index>.tmp` checkpoint during a resumed full build.
    pub resumed_embeddings: usize,
    /// Checkpoint path used for resume, when a full build continued from
    /// an existing temporary index instead of truncating it.
    pub resume_tmp_path: Option<PathBuf>,
}

/// One row of the persistent NDJSON-backed index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Stable id for this chunk (filesystem-safe slug derived from the
    /// chunk path).
    pub id: String,
    /// Project bucket the chunk belongs to (canonical lowercase).
    pub project: String,
    /// Agent label (`claude` / `codex` / `gemini` / ...).
    pub agent: String,
    /// Compact date (`YYYYMMDD`) the chunk was emitted for.
    pub date: String,
    /// Absolute path to the chunk markdown file.
    pub path: PathBuf,
    /// Canonical corpus kind (`conversations`, `plans`, `reports`, `other`).
    #[serde(default)]
    pub kind: String,
    /// Source session id when known.
    #[serde(default)]
    pub session_id: String,
    /// Timeline frame kind (`user_msg`, `agent_reply`, `internal_thought`, `tool_call`).
    #[serde(default)]
    pub frame_kind: Option<String>,
    /// Working directory when captured by the extractor.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Embedding vector. Dimension is implied by the resolved model
    /// (recorded in the per-file header — see [`IndexHeader`]).
    pub embedding: Vec<f32>,
}

/// First-line header of the NDJSON index file. Captures schema and model
/// metadata so a query can reject mismatched-dimension queries before
/// touching any vectors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexHeader {
    pub schema_version: String,
    pub model_id: String,
    pub model_profile: String,
    pub dimension: usize,
    pub generated_at: String,
    pub entry_count: usize,
}

/// Single semantic-search hit with its cosine score.
#[derive(Debug, Clone, Serialize)]
pub struct QueryHit {
    pub id: String,
    pub project: String,
    pub agent: String,
    pub date: String,
    pub path: PathBuf,
    pub kind: String,
    pub session_id: String,
    pub frame_kind: Option<String>,
    pub cwd: Option<String>,
    /// Cosine similarity in `[-1.0, 1.0]`. Higher = more similar.
    pub score: f32,
}

impl IndexStats {
    /// Project the embedder load time and per-chunk embedding cost into
    /// an ETA for indexing the full corpus, in seconds. Returns `None`
    /// when there is not enough signal to estimate (zero embeddings or
    /// zero elapsed time).
    pub fn full_index_eta_secs(&self) -> Option<u64> {
        if self.embeddings_computed == 0 || self.elapsed_ms == 0 {
            return None;
        }
        let per_ms = self.elapsed_ms as f64 / self.embeddings_computed.max(1) as f64;
        let total_ms = per_ms * self.chunks_total.max(1) as f64;
        Some((total_ms / 1000.0).ceil() as u64)
    }
}

/// Sample the canonical store, embed up to `sample` chunks, return stats.
///
/// `sample == 0` means "embed every discovered chunk" (the operator
/// signals they want a full ETA, not a quick smoke test).
pub fn dry_run_index(project: Option<&str>, sample: usize) -> Result<IndexStats> {
    let started = Instant::now();
    let mut stats = IndexStats {
        chunks_total: 0,
        chunks_sampled: 0,
        embeddings_computed: 0,
        embed_errors: 0,
        dimension: None,
        model_id: None,
        model_profile: None,
        fallback_reason: None,
        elapsed_ms: 0,
        dry_run: true,
        index_path: None,
        resumed_embeddings: 0,
        resume_tmp_path: None,
    };

    let root = crate::store::store_base_dir()?;
    let files = live_index_files(&root, project)?;
    stats.chunks_total = files.len();

    // `sample` is consumed inside the embedder-enabled cfg branch below;
    // bind it to `_` here so the no-embedder build does not warn about an
    // unused argument while keeping the public signature stable.
    let _ = sample;

    if files.is_empty() {
        stats.elapsed_ms = started.elapsed().as_millis();
        Ok(stats)
    } else {
        #[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
        {
            stats.fallback_reason =
                Some("native-embedder feature not compiled in this binary".to_string());
            stats.elapsed_ms = started.elapsed().as_millis();
            Ok(stats)
        }

        #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
        {
            run_native_pass(&files, sample, &mut stats);
            stats.elapsed_ms = started.elapsed().as_millis();
            Ok(stats)
        }
    }
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn run_native_pass(
    files: &[crate::store::StoredContextFile],
    sample: usize,
    stats: &mut IndexStats,
) {
    let mut engine = match crate::embedder::EmbeddingEngine::new() {
        Ok(engine) => engine,
        Err(err) => {
            stats.fallback_reason =
                Some(format!("semantic embedder unavailable (optional): {err}"));
            return;
        }
    };

    let info = engine.info().clone();
    stats.dimension = Some(info.dimension);
    stats.model_id = Some(info.model_id.clone());
    stats.model_profile = Some(info.profile.to_string());

    let cap = if sample == 0 {
        files.len()
    } else {
        sample.min(files.len())
    };

    for stored in files.iter().take(cap) {
        stats.chunks_sampled += 1;
        let content = match crate::sanitize::read_to_string_validated(&stored.path) {
            Ok(text) => text,
            Err(_) => {
                stats.embed_errors += 1;
                continue;
            }
        };
        let prefix = take_prefix_bytes(&content, DEFAULT_EMBED_PREFIX_BYTES);
        match engine.embed(&prefix) {
            Ok(_vec) => {
                stats.embeddings_computed += 1;
            }
            Err(_) => {
                stats.embed_errors += 1;
            }
        }
    }
}

// ============================================================================
// Iter 3 — persistent NDJSON-backed index
// ============================================================================

const INDEX_SCHEMA_VERSION: &str = "1.0";
const INDEX_FILE_NAME: &str = "embeddings.ndjson";
const CONTEXT_CORPUS_INDEX_FILE_NAME: &str = "context-corpus.embeddings.ndjson";
const INDEX_DIR_NAME: &str = "indexed";
const ALL_BUCKET_NAME: &str = "_all";

// Index integrity thresholds for `query_index` scan.
//
// A live tail can race with a query and produce a single truncated line at
// the tip; tolerating one corrupt row in a healthy index keeps queries
// answering. But when the corrupt ratio crosses `CORRUPT_RATE_FAIL_FAST` on
// an index large enough for the ratio to be meaningful
// (`CORRUPT_MIN_SAMPLE`), we surface a fail-fast error with a recovery hint
// instead of silently degrading recall.
const CORRUPT_RATE_FAIL_FAST: f64 = 0.05;
const CORRUPT_MIN_SAMPLE: usize = 20;
const CORRUPT_WARN_HEAD: usize = 5;

/// Resolve the on-disk path of the persistent vector index for a given
/// project bucket. When `project == None`, returns the cross-project
/// `_all` bucket path so an operator can index every chunk in one file.
pub fn index_path(project: Option<&str>) -> Result<PathBuf> {
    let base = crate::store::store_base_dir()?;
    Ok(index_path_for(&base, project))
}

fn index_path_for(base: &Path, project: Option<&str>) -> PathBuf {
    // `store_base_dir()` resolves to the AICX home (`~/.aicx`), not the
    // corpus store (`~/.aicx/store`). Keep the vector index inside the
    // operator-owned AICX home so build, status, and search all agree.
    let index_root = base.join(INDEX_DIR_NAME);
    index_root
        .join(index_bucket_name(project))
        .join(INDEX_FILE_NAME)
}

pub fn context_corpus_index_path(project: Option<&str>) -> Result<PathBuf> {
    Ok(index_path(project)?.with_file_name(CONTEXT_CORPUS_INDEX_FILE_NAME))
}

pub fn index_bucket_name(project: Option<&str>) -> String {
    let bucket = project.unwrap_or(ALL_BUCKET_NAME);
    // Sanitize project bucket for filesystem (canonical lowercase per
    // canonical_project_slug invariant + replace path separators).
    bucket
        .chars()
        .map(|c| match c {
            '/' | '\\' => '_',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn hybrid_index_dir(project: Option<&str>) -> Result<PathBuf> {
    let path = index_path(project)?;
    path.parent()
        .map(|parent| parent.join("hybrid"))
        .ok_or_else(|| anyhow::anyhow!("index path has no parent: {}", path.display()))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn hybrid_manifest_path(project: Option<&str>) -> Result<PathBuf> {
    Ok(hybrid_index_dir(project)?.join("manifest.json"))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn hybrid_dense_path(project: Option<&str>) -> Result<PathBuf> {
    Ok(aicx_retrieve::default_ndjson_path(&hybrid_index_dir(
        project,
    )?))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn observed_source_hash_for_index_path(path: &Path) -> Result<String> {
    let mut file = crate::sanitize::open_file_validated(path)
        .with_context(|| format!("open {}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn hybrid_embedder_fingerprint(
    info: &crate::embedder::EmbeddingModelInfo,
) -> aicx_retrieve::EmbedderFingerprint {
    let source = match &info.source {
        crate::embedder::NativeEmbeddingSource::HfCache {
            repo,
            filename,
            path,
        } => format!("hf-cache:{repo}:{filename}:{}", path.display()),
        crate::embedder::NativeEmbeddingSource::ExplicitPath(path) => {
            format!("explicit-path:{}", path.display())
        }
        crate::embedder::NativeEmbeddingSource::CloudEndpoint(url) => {
            format!("cloud-endpoint:{url}")
        }
    };
    aicx_retrieve::EmbedderFingerprint::new(
        info.model_id.clone(),
        &source,
        info.dimension,
        "cosine",
    )
}

fn live_index_files(
    root: &std::path::Path,
    project: Option<&str>,
) -> Result<Vec<crate::store::StoredContextFile>> {
    let mut files = crate::store::scan_context_files_project_at(root, project)?;
    files.retain(|file| {
        !crate::store::load_sidecar(&file.path).is_some_and(|sidecar| {
            sidecar.artifact_family.as_deref() == Some(crate::store::LOCT_CONTEXT_PACK_FAMILY)
                || sidecar
                    .truth_status
                    .as_ref()
                    .is_some_and(|status| status.role == crate::chunker::TruthRole::Example)
        })
    });
    Ok(files)
}

/// Build (or rebuild) the persistent NDJSON-backed index for `project`.
///
/// Iter 3 surface: scans the canonical store, embeds every chunk via the
/// configured embedder ([`crate::embedder::EmbeddingEngine`]), and writes
/// a single NDJSON file per project bucket. First line is an
/// [`IndexHeader`] for schema/model metadata; subsequent lines are
/// [`IndexEntry`] rows.
///
/// `sample == 0` indexes every discovered chunk (the operator wants a
/// full build). Non-zero `sample` caps the embed loop — useful for fast
/// integration tests against a small subset.
///
/// The lance-resource lock ([`crate::locks::lance_lock_path`]) is
/// acquired for the duration of the write so concurrent CLI / MCP
/// processes serialize their rebuilds.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[allow(clippy::too_many_arguments)]
fn maybe_emit_stats_tick(
    on_event: &dyn Fn(&IndexEvent),
    rolling: &RollingRate,
    last_tick: &mut Instant,
    interval: Duration,
    processed: usize,
    indexed: usize,
    skipped: usize,
    failed: usize,
    total: usize,
) {
    if last_tick.elapsed() < interval {
        return;
    }
    let rate = rolling.rate_per_sec();
    let remaining = total.saturating_sub(processed);
    let eta = rolling.eta_secs(remaining);
    on_event(&IndexEvent::StatsTick {
        processed,
        indexed,
        skipped,
        failed,
        total,
        items_per_sec: rate,
        eta_secs: eta,
        total_chunks: indexed,
        in_flight: 1,
    });
    *last_tick = Instant::now();
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn write_index(project: Option<&str>, sample: usize) -> Result<IndexStats> {
    write_index_with_progress(project, sample, &|_| {})
}

/// G-3 build-mode options for [`write_index_with_options`].
///
/// Default is incremental: only sidecars whose mtime is newer than the
/// committed index `header.generated_at` are re-embedded, and their rows
/// are appended to the existing `embeddings.ndjson`. `full_rescan: true`
/// restores the pre-G-3 from-zero rebuild.
#[derive(Debug, Clone, Copy, Default)]
pub struct IndexBuildOptions {
    /// `true` to embed every chunk from scratch. `false` (default) to walk
    /// only chunks newer than the committed index header.
    pub full_rescan: bool,
}

/// Short label for the currently-configured embedder backend
/// (`"cloud"` / `"gguf"`). Returns `None` if no embedder can be loaded
/// — caller is free to skip printing rather than surfacing a noisy error
/// before the same backend init runs again inside [`write_index`].
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn probe_backend_label() -> Option<&'static str> {
    let engine = crate::embedder::EmbeddingEngine::new().ok()?;
    Some(backend_label_from_info(engine.info()))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn backend_label_from_info(info: &crate::embedder::EmbeddingModelInfo) -> &'static str {
    match info.source {
        crate::embedder::NativeEmbeddingSource::CloudEndpoint(_) => "cloud",
        crate::embedder::NativeEmbeddingSource::HfCache { .. }
        | crate::embedder::NativeEmbeddingSource::ExplicitPath(_) => "gguf",
    }
}

/// Same as [`write_index`] but emits [`IndexEvent`]s into the supplied sink
/// for every embedded chunk plus a rate-limited [`IndexEvent::StatsTick`].
///
/// `aicx index` builds a `FanOut<IndexEvent>` over an `IndicatifSink` (live
/// TTY bar) plus a tracing adapter and passes the resulting closure here so
/// the operator can see the embedding pipeline breathe instead of staring
/// at a 75-minute blank stdout. Internal rebuild paths (`aicx all`, library
/// callers) still call the thin [`write_index`] wrapper above and pay zero
/// observability cost.
///
/// Defaults to **incremental** since G-3 (only sidecars newer than the
/// committed `header.generated_at` are re-embedded). Callers that need a
/// from-zero rebuild use [`write_index_with_options`] with
/// `full_rescan: true`.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn write_index_with_progress(
    project: Option<&str>,
    sample: usize,
    on_event: &dyn Fn(&IndexEvent),
) -> Result<IndexStats> {
    write_index_with_options(project, sample, IndexBuildOptions::default(), on_event)
}

/// Build (or incrementally update) the persistent NDJSON-backed index.
///
/// `options.full_rescan` controls the walk strategy — see
/// [`IndexBuildOptions`].
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn write_index_with_options(
    project: Option<&str>,
    sample: usize,
    options: IndexBuildOptions,
    on_event: &dyn Fn(&IndexEvent),
) -> Result<IndexStats> {
    use std::collections::HashSet;
    use std::fs::{self, OpenOptions};
    use std::io::{BufWriter, Write};

    let started = Instant::now();
    let mut stats = IndexStats {
        chunks_total: 0,
        chunks_sampled: 0,
        embeddings_computed: 0,
        embed_errors: 0,
        dimension: None,
        model_id: None,
        model_profile: None,
        fallback_reason: None,
        elapsed_ms: 0,
        dry_run: false,
        index_path: None,
        resumed_embeddings: 0,
        resume_tmp_path: None,
    };

    let target_path = index_path(project)?;
    let tmp_path = target_path.with_extension("ndjson.tmp");
    if sample != 0 && tmp_path.exists() {
        return Err(anyhow::anyhow!(
            "refusing to overwrite existing semantic index checkpoint: {}. Run `aicx index --sample 0` to resume the full build, or move the checkpoint aside deliberately.",
            tmp_path.display()
        ));
    }

    let _lock = crate::locks::acquire_exclusive(crate::locks::lance_lock_path()?)?;

    let root = crate::store::store_base_dir()?;
    let all_files = live_index_files(&root, project)?;
    stats.chunks_total = all_files.len();

    if all_files.is_empty() {
        stats.fallback_reason = Some("no chunks found in canonical store".to_string());
        stats.elapsed_ms = started.elapsed().as_millis();
        return Ok(stats);
    }

    let mut engine = match crate::embedder::EmbeddingEngine::new() {
        Ok(engine) => engine,
        Err(err) => {
            stats.fallback_reason =
                Some(format!("semantic embedder unavailable (optional): {err}"));
            stats.elapsed_ms = started.elapsed().as_millis();
            return Ok(stats);
        }
    };

    let info = engine.info().clone();
    stats.dimension = Some(info.dimension);
    stats.model_id = Some(info.model_id.clone());
    stats.model_profile = Some(info.profile.to_string());

    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create index dir: {}", parent.display()))?;
    }

    // G-3: decide build mode. `--full-rescan` always rebuilds from zero.
    // `sample != 0` is a deterministic-subset diagnostic mode, also full.
    // Otherwise look for a compatible committed index — if absent or
    // incompatible (dim/model/profile drift), fall back to full so the
    // operator does not silently mix model outputs.
    let incremental_baseline = if options.full_rescan || sample != 0 {
        None
    } else {
        load_incremental_baseline(&target_path, &info)?
    };

    // Files to walk through the embed loop. Incremental keeps only those
    // newer than the baseline `generated_at` AND not already embedded
    // (id-set diff covers crash-recovery cases where mtime ≤ generated_at
    // but the row never made it into the committed index).
    let files: Vec<crate::store::StoredContextFile> = match incremental_baseline.as_ref() {
        Some(baseline) => partition_incremental_files(&all_files, baseline),
        None => all_files.clone(),
    };
    let cap = if sample == 0 {
        files.len()
    } else {
        sample.min(files.len())
    };
    // Resume-from-checkpoint only applies to a full rebuild; an
    // incremental walk never writes to `ndjson.tmp` so there is nothing
    // to resume from.
    let resume = if sample == 0 && incremental_baseline.is_none() {
        load_resume_tmp_index(&tmp_path, &info)?
    } else {
        None
    };
    let resumed_ids: HashSet<String> = resume
        .as_ref()
        .map(|state| state.ids.clone())
        .unwrap_or_default();

    // Atomic-ish: write to `.tmp` then rename so a partial build cannot
    // poison subsequent queries. Three flows feed the same tmp file shape:
    //   1. Resumed full build → append onto the surviving `.tmp` checkpoint.
    //   2. Incremental walk (G-3) → seed tmp with the committed body so the
    //      embed loop only writes the genuinely new rows.
    //   3. Fresh full build → empty tmp + placeholder header, embed loop
    //      writes every chunk.
    let mut writer = if let Some(state) = &resume {
        stats.resumed_embeddings = state.rows;
        stats.resume_tmp_path = Some(tmp_path.clone());
        let mut file = OpenOptions::new()
            .append(true)
            .open(&tmp_path)
            .with_context(|| format!("open tmp index for resume: {}", tmp_path.display()))?;
        if state.needs_newline {
            file.write_all(b"\n")
                .with_context(|| format!("repair tmp trailing newline: {}", tmp_path.display()))?;
        }
        BufWriter::new(file)
    } else {
        let mut writer = BufWriter::new(
            crate::sanitize::create_file_validated(&tmp_path)
                .with_context(|| format!("open tmp index: {}", tmp_path.display()))?,
        );
        // Placeholder count is rewritten with the truthful `indexed` total
        // once the embed loop ends but before the atomic rename to the final
        // committed path (see D-2 — `entry_count` is truthful for new builds).
        // Streaming consumers that scan until EOF still work; readers that
        // want a constant-time count now have one.
        let header = IndexHeader {
            schema_version: INDEX_SCHEMA_VERSION.to_string(),
            model_id: info.model_id.clone(),
            model_profile: info.profile.to_string(),
            dimension: info.dimension,
            generated_at: chrono::Utc::now().to_rfc3339(),
            entry_count: 0,
        };
        writeln!(writer, "{}", serde_json::to_string(&header)?)?;
        // G-3: incremental seed — copy every data line from the committed
        // index into tmp before the embed loop appends new rows. The
        // truthful-header rewrite at commit time then sees existing-plus-new
        // and renames atomically onto the target. `stats.resumed_embeddings`
        // doubles as the "already in the file" count so the D-2 entry_count
        // math below stays accurate.
        if incremental_baseline.is_some() {
            stats.resumed_embeddings = copy_committed_body_into(&mut writer, &target_path)
                .with_context(|| {
                    format!(
                        "seed incremental tmp from committed index: {}",
                        target_path.display()
                    )
                })?;
        }
        writer
    };

    let run_started = Instant::now();
    let total_items = cap;
    on_event(&IndexEvent::RunStarted {
        total_items,
        namespace: "semantic_index".to_string(),
        source_label: target_path.to_string_lossy().to_string(),
        parallelism: 1,
        started_at: chrono::Utc::now(),
    });

    let mut rolling = RollingRate::new(Duration::from_secs(10));
    let mut last_tick = Instant::now();
    let mut processed = 0usize;
    let mut indexed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let tick_interval = Duration::from_secs(1);

    for (item_index, stored) in files.iter().take(cap).enumerate() {
        let entry_id = chunk_id_from_path(&stored.path);
        if resumed_ids.contains(&entry_id) {
            skipped += 1;
            processed += 1;
            on_event(&IndexEvent::ItemSkipped {
                item_index,
                label: entry_id,
                reason: "resumed from checkpoint".to_string(),
                content_hash: None,
            });
            maybe_emit_stats_tick(
                on_event,
                &rolling,
                &mut last_tick,
                tick_interval,
                processed,
                indexed,
                skipped,
                failed,
                total_items,
            );
            continue;
        }
        stats.chunks_sampled += 1;
        let item_started = Instant::now();
        let content = match crate::sanitize::read_to_string_validated(&stored.path) {
            Ok(text) => text,
            Err(err) => {
                stats.embed_errors += 1;
                failed += 1;
                processed += 1;
                on_event(&IndexEvent::ItemFailed {
                    item_index,
                    label: entry_id,
                    error: format!("read failed: {err}"),
                });
                continue;
            }
        };
        let prefix = take_prefix_bytes(&content, DEFAULT_EMBED_PREFIX_BYTES);
        let embedder_started = Instant::now();
        let embedding = match engine.embed(&prefix) {
            Ok(vec) => vec,
            Err(err) => {
                stats.embed_errors += 1;
                failed += 1;
                processed += 1;
                on_event(&IndexEvent::ItemFailed {
                    item_index,
                    label: entry_id,
                    error: format!("embed failed: {err}"),
                });
                continue;
            }
        };
        let embedder_ms = embedder_started.elapsed().as_millis() as u64;
        let duration_ms = item_started.elapsed().as_millis() as u64;
        let entry = IndexEntry {
            id: entry_id.clone(),
            project: stored.project.clone(),
            agent: stored.agent.clone(),
            date: stored.date_iso.clone(),
            path: stored.path.clone(),
            kind: stored.kind.dir_name().to_string(),
            session_id: stored.session_id.clone(),
            frame_kind: chunk_frame_kind(&stored.path),
            cwd: chunk_cwd(&stored.path),
            embedding,
        };
        writeln!(writer, "{}", serde_json::to_string(&entry)?)?;
        stats.embeddings_computed += 1;
        indexed += 1;
        processed += 1;
        rolling.record(1);
        on_event(&IndexEvent::ItemIndexed {
            item_index,
            label: entry_id,
            chunks_indexed: 1,
            duration_ms,
            embedder_ms: Some(embedder_ms),
            tokens_estimated: None,
            content_hash: None,
        });
        maybe_emit_stats_tick(
            on_event,
            &rolling,
            &mut last_tick,
            tick_interval,
            processed,
            indexed,
            skipped,
            failed,
            total_items,
        );
    }

    // Emit completion only after the final atomic commit lands on disk so the
    // event truthfully reflects "semantic index ready to query". The embed
    // loop is done at this point; the rest is filesystem commit.

    // Primary commits FIRST (D-3). If the process crashes between primary
    // and context-corpus, readers querying the primary index still get
    // correct semantics; an absent context-corpus is a graceful degrade,
    // never a stale-ahead-of-primary inconsistency.

    if let Err(err) = writer
        .flush()
        .with_context(|| format!("flush tmp index: {}", tmp_path.display()))
    {
        on_event(&IndexEvent::RunFailed {
            error: format!("{err:#}"),
            processed_before_failure: processed,
        });
        return Err(err);
    }
    drop(writer);

    // D-2: rewrite the placeholder header so `entry_count` reflects the
    // truthful row total before the atomic rename. Done by streaming the
    // tmp file into a fresh `commit-tmp` file (header swapped, entries
    // copied verbatim) so resumed checkpoints with the older placeholder
    // format are upgraded transparently.
    let final_tmp_path = target_path.with_extension("ndjson.commit-tmp");
    let total_indexed = stats
        .resumed_embeddings
        .saturating_add(indexed)
        .saturating_add(skipped);
    let truthful_header = IndexHeader {
        schema_version: INDEX_SCHEMA_VERSION.to_string(),
        model_id: info.model_id.clone(),
        model_profile: info.profile.to_string(),
        dimension: info.dimension,
        generated_at: chrono::Utc::now().to_rfc3339(),
        entry_count: total_indexed,
    };
    if let Err(err) =
        rewrite_index_with_truthful_header(&tmp_path, &final_tmp_path, &truthful_header)
    {
        on_event(&IndexEvent::RunFailed {
            error: format!("{err:#}"),
            processed_before_failure: processed,
        });
        return Err(err);
    }
    let _ = fs::remove_file(&tmp_path);

    if let Err(err) = fs::rename(&final_tmp_path, &target_path).with_context(|| {
        format!(
            "commit index: {} → {}",
            final_tmp_path.display(),
            target_path.display()
        )
    }) {
        on_event(&IndexEvent::RunFailed {
            error: format!("{err:#}"),
            processed_before_failure: processed,
        });
        return Err(err);
    }

    // Context-corpus commits AFTER primary. Tiny corpus; collect entries
    // in-memory so the header carries the truthful `entry_count` from the
    // first byte written (no rewrite needed).
    let context_files = crate::store::scan_context_corpus_files_at(&root)?;
    if !context_files.is_empty() {
        let context_target = context_corpus_index_path(project)?;
        if let Some(parent) = context_target.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("create context-corpus index dir: {}", parent.display())
            })?;
        }
        let mut context_entries: Vec<IndexEntry> = Vec::with_capacity(context_files.len());
        for stored in &context_files {
            let content = match crate::sanitize::read_to_string_validated(&stored.raw_path) {
                Ok(text) => text,
                Err(_) => continue,
            };
            let prefix = take_prefix_bytes(&content, DEFAULT_EMBED_PREFIX_BYTES);
            let Ok(embedding) = engine.embed(&prefix) else {
                continue;
            };
            context_entries.push(IndexEntry {
                id: stored.sidecar.id.clone(),
                project: stored.sidecar.project.clone(),
                agent: stored.sidecar.agent.clone(),
                date: stored.sidecar.date.clone(),
                path: stored.raw_path.clone(),
                kind: "context-corpus".to_string(),
                session_id: stored.sidecar.id.clone(),
                frame_kind: None,
                cwd: None,
                embedding,
            });
        }

        let context_tmp = context_target.with_extension("ndjson.tmp");
        let context_header = IndexHeader {
            schema_version: INDEX_SCHEMA_VERSION.to_string(),
            model_id: info.model_id.clone(),
            model_profile: info.profile.to_string(),
            dimension: info.dimension,
            generated_at: chrono::Utc::now().to_rfc3339(),
            entry_count: context_entries.len(),
        };
        {
            let mut context_writer = BufWriter::new(
                crate::sanitize::create_file_validated(&context_tmp).with_context(|| {
                    format!("open context-corpus tmp index: {}", context_tmp.display())
                })?,
            );
            writeln!(
                context_writer,
                "{}",
                serde_json::to_string(&context_header)?
            )?;
            for entry in &context_entries {
                writeln!(context_writer, "{}", serde_json::to_string(entry)?)?;
            }
            if let Err(err) = context_writer.flush().with_context(|| {
                format!("flush context-corpus tmp index: {}", context_tmp.display())
            }) {
                on_event(&IndexEvent::RunFailed {
                    error: format!("{err:#}"),
                    processed_before_failure: processed,
                });
                return Err(err);
            }
        }
        if let Err(err) = fs::rename(&context_tmp, &context_target).with_context(|| {
            format!(
                "commit context-corpus index: {} → {}",
                context_tmp.display(),
                context_target.display()
            )
        }) {
            on_event(&IndexEvent::RunFailed {
                error: format!("{err:#}"),
                processed_before_failure: processed,
            });
            return Err(err);
        }
    }

    if let Err(err) = materialize_hybrid_index(&target_path, project, &info) {
        on_event(&IndexEvent::RunFailed {
            error: format!("{err:#}"),
            processed_before_failure: processed,
        });
        return Err(err.context("materialize hybrid retrieval index"));
    }

    // Final atomic commit succeeded. Only now is the semantic index queryable
    // at its canonical final path — emit RunCompleted so downstream consumers
    // (Loctree bridge, MCP `aicx_index_status`) can trust the readiness claim.
    on_event(&IndexEvent::RunCompleted {
        processed,
        indexed,
        skipped,
        failed,
        total_chunks: indexed,
        elapsed: run_started.elapsed(),
        stopped_early: false,
    });

    stats.index_path = Some(target_path);
    stats.elapsed_ms = started.elapsed().as_millis();
    Ok(stats)
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn materialize_hybrid_index(
    index_path: &Path,
    project: Option<&str>,
    info: &crate::embedder::EmbeddingModelInfo,
) -> Result<aicx_retrieve::Manifest> {
    use aicx_retrieve::{
        BruteForceAdapter, ChunkRef, DenseChunkRef, Distance, HybridIndex, ReciprocalRankFusion,
        TantivyAdapter,
    };

    let (header, entries) = read_committed_index_entries(index_path)?;
    if header.dimension != info.dimension {
        anyhow::bail!(
            "hybrid build dim mismatch: committed index has {}, embedder has {}",
            header.dimension,
            info.dimension
        );
    }

    let manifest_dir = hybrid_index_dir(project)?;
    let source_hash = observed_source_hash_for_index_path(index_path)?;
    let mut lexical_chunks = Vec::with_capacity(entries.len());
    let mut dense_chunks = Vec::with_capacity(entries.len());

    for entry in entries {
        let text = crate::sanitize::read_to_string_validated(&entry.path)
            .with_context(|| format!("read chunk for hybrid index: {}", entry.path.display()))?;
        let metadata = index_entry_metadata_json(&entry);
        let chunk = ChunkRef {
            id: entry.id.clone(),
            source_path: entry.path.to_string_lossy().to_string(),
            text,
            metadata,
        };
        lexical_chunks.push(chunk.clone());
        dense_chunks.push(DenseChunkRef {
            chunk,
            embedding: entry.embedding,
        });
    }

    let lexical = Box::new(TantivyAdapter::new(manifest_dir.clone())?);
    let dense = Box::new(BruteForceAdapter::new(header.dimension).with_distance(Distance::Cosine));
    let fusion = Box::new(ReciprocalRankFusion::default());
    let fingerprint = hybrid_embedder_fingerprint(info);
    let mut hybrid = HybridIndex::new(lexical, dense, fusion, manifest_dir, fingerprint);
    hybrid.build_hybrid(&lexical_chunks, &dense_chunks, &source_hash)?;
    let manifest = hybrid.commit()?.clone();

    let mut dense_persist =
        BruteForceAdapter::new(header.dimension).with_distance(Distance::Cosine);
    aicx_retrieve::DenseIndex::build(&mut dense_persist, &dense_chunks)?;
    dense_persist.persist_ndjson(&hybrid_dense_path(project)?)?;

    Ok(manifest)
}

/// Rewrite the placeholder header in `tmp_path` with the truthful one and
/// stream the remaining entries into `final_tmp_path`. Caller renames
/// `final_tmp_path` onto the committed target after this succeeds.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn rewrite_index_with_truthful_header(
    tmp_path: &Path,
    final_tmp_path: &Path,
    header: &IndexHeader,
) -> Result<()> {
    use std::io::{BufReader, BufWriter, Write};

    let src = crate::sanitize::open_file_validated(tmp_path)
        .with_context(|| format!("open tmp index for header rewrite: {}", tmp_path.display()))?;
    let mut src_reader = BufReader::new(src);
    let placeholder = read_index_line_capped(&mut src_reader, tmp_path, 1, "tmp index header")
        .with_context(|| format!("read placeholder header: {}", tmp_path.display()))?;
    if placeholder.is_none() {
        anyhow::bail!("tmp index unexpectedly empty: {}", tmp_path.display());
    }

    let dst = crate::sanitize::create_file_validated(final_tmp_path)
        .with_context(|| format!("create commit-tmp index: {}", final_tmp_path.display()))?;
    let mut dst_writer = BufWriter::new(dst);
    writeln!(dst_writer, "{}", serde_json::to_string(header)?).with_context(|| {
        format!(
            "write truthful header to commit-tmp: {}",
            final_tmp_path.display()
        )
    })?;
    std::io::copy(&mut src_reader, &mut dst_writer).with_context(|| {
        format!(
            "copy entries from {} → {}",
            tmp_path.display(),
            final_tmp_path.display()
        )
    })?;
    dst_writer
        .flush()
        .with_context(|| format!("flush commit-tmp index: {}", final_tmp_path.display()))?;
    Ok(())
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn read_committed_index_entries(path: &Path) -> Result<(IndexHeader, Vec<IndexEntry>)> {
    use std::io::BufReader;

    let file = crate::sanitize::open_file_validated(path)
        .with_context(|| format!("open committed semantic index: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let header_line = read_index_line_capped(&mut reader, path, 1, "committed semantic header")
        .with_context(|| format!("read header in {}", path.display()))?
        .ok_or_else(|| anyhow::anyhow!("empty committed semantic index: {}", path.display()))?;
    let header = serde_json::from_str::<IndexHeader>(&header_line)
        .with_context(|| format!("parse header in {}", path.display()))?;
    let mut entries = Vec::new();
    for (idx, line) in capped_index_lines(reader, path, 2, "committed semantic data").enumerate() {
        let line = line.with_context(|| format!("read line {} in {}", idx + 2, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let entry = serde_json::from_str::<IndexEntry>(&line)
            .with_context(|| format!("parse line {} in {}", idx + 2, path.display()))?;
        entries.push(entry);
    }
    Ok((header, entries))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn index_entry_metadata_json(entry: &IndexEntry) -> serde_json::Value {
    serde_json::json!({
        "source_path": entry.path.to_string_lossy(),
        "project": entry.project,
        "agent": entry.agent,
        "date": entry.date,
        "kind": entry.kind,
        "session_id": entry.session_id,
        "frame_kind": entry.frame_kind,
        "cwd": entry.cwd,
    })
}

/// Build-disabled stub for binaries compiled without any embedder feature.
#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
pub fn write_index(project: Option<&str>, _sample: usize) -> Result<IndexStats> {
    let _ = project;
    let mut stats = IndexStats {
        chunks_total: 0,
        chunks_sampled: 0,
        embeddings_computed: 0,
        embed_errors: 0,
        dimension: None,
        model_id: None,
        model_profile: None,
        fallback_reason: Some(
            "no embedder feature compiled in (rebuild with --features native-embedder or cloud-embedder)".to_string(),
        ),
        elapsed_ms: 0,
        dry_run: false,
        index_path: None,
        resumed_embeddings: 0,
        resume_tmp_path: None,
    };
    Ok(stats)
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[derive(Debug, Clone)]
struct ResumeTmpIndex {
    ids: std::collections::HashSet<String>,
    rows: usize,
    needs_newline: bool,
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn load_resume_tmp_index(
    path: &std::path::Path,
    info: &crate::embedder::EmbeddingModelInfo,
) -> Result<Option<ResumeTmpIndex>> {
    use std::collections::HashSet;
    use std::io::{BufReader, Read, Seek, SeekFrom};

    if !path.exists() {
        return Ok(None);
    }

    let file = crate::sanitize::open_file_validated(path)
        .with_context(|| format!("open tmp index checkpoint: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let header_line =
        match read_index_line_capped(&mut reader, path, 1, "tmp index checkpoint header")
            .with_context(|| format!("read tmp header: {}", path.display()))?
        {
            Some(line) => line,
            None => return Ok(None),
        };
    let header: IndexHeader = serde_json::from_str(&header_line)
        .with_context(|| format!("parse tmp header: {}", path.display()))?;
    let profile = info.profile.to_string();
    if header.schema_version != INDEX_SCHEMA_VERSION
        || header.model_id != info.model_id
        || header.model_profile != profile
        || header.dimension != info.dimension
    {
        return Err(anyhow::anyhow!(
            "tmp index checkpoint at {path} is incompatible with the active embedder.\n  \
             checkpoint: schema={cks} model={ckm} profile={ckp} dim={ckd}\n  \
             current   : schema={cur_s} model={cur_m} profile={cur_p} dim={cur_d}\n  \
             they cannot resume each other. fix one of:\n    \
             - rebuild the partial index with the original embedder, or\n    \
             - remove the stale checkpoint and start fresh:\n        \
                 rm {path}",
            path = path.display(),
            cks = header.schema_version,
            ckm = header.model_id,
            ckp = header.model_profile,
            ckd = header.dimension,
            cur_s = INDEX_SCHEMA_VERSION,
            cur_m = info.model_id,
            cur_p = profile,
            cur_d = info.dimension,
        ));
    }

    #[derive(Deserialize)]
    struct ResumeEntry {
        id: String,
    }

    let mut ids = HashSet::new();
    let mut rows = 0usize;
    for (idx, line) in capped_index_lines(reader, path, 2, "tmp index checkpoint data").enumerate()
    {
        let line = line.with_context(|| {
            format!(
                "read tmp index checkpoint line {}: {}",
                idx + 2,
                path.display()
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let entry: ResumeEntry = serde_json::from_str(&line).with_context(|| {
            format!(
                "parse tmp index checkpoint line {}: {}",
                idx + 2,
                path.display()
            )
        })?;
        ids.insert(entry.id);
        rows += 1;
    }

    let mut tail = crate::sanitize::open_file_validated(path)
        .with_context(|| format!("open tmp index tail: {}", path.display()))?;
    let len = tail
        .metadata()
        .with_context(|| format!("stat tmp index: {}", path.display()))?
        .len();
    let needs_newline = if len == 0 {
        false
    } else {
        tail.seek(SeekFrom::End(-1))
            .with_context(|| format!("seek tmp index tail: {}", path.display()))?;
        let mut byte = [0u8; 1];
        tail.read_exact(&mut byte)
            .with_context(|| format!("read tmp index tail: {}", path.display()))?;
        byte[0] != b'\n'
    };

    Ok(Some(ResumeTmpIndex {
        ids,
        rows,
        needs_newline,
    }))
}

/// G-3 incremental baseline parsed from the existing committed index.
///
/// `embedded_ids` captures the row IDs that already live in the
/// committed file. A chunk whose id is already in the committed body
/// never re-embeds under the incremental walk (`--full-rescan` is the
/// explicit refresh path).
///
/// `cutoff` is the `header.generated_at` lifted into a comparable
/// [`std::time::SystemTime`]. It is retained as parsed-and-validated
/// metadata so the loader can still reject a committed file with a
/// malformed timestamp; the partition logic itself no longer consults
/// mtime now that the missing-id rule is authoritative.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[derive(Debug, Clone)]
pub(crate) struct IncrementalBaseline {
    #[allow(dead_code)] // validated at parse time; kept for diagnostics & future reconcile mode
    pub(crate) cutoff: std::time::SystemTime,
    pub(crate) embedded_ids: std::collections::HashSet<String>,
}

/// Read the committed index at `path`, validate that its header matches
/// the active embedder, and emit an [`IncrementalBaseline`] the caller
/// can hand to [`partition_incremental_files`].
///
/// Returns `Ok(None)` when the committed index does not exist or has no
/// data rows — both cases degrade to a full rebuild, since incremental
/// math against "zero entries with stale generated_at" would silently
/// skip every sidecar older than the cutoff and produce an empty result.
///
/// Header dim / model / profile mismatch returns an `Err` with a
/// recovery hint pointing at `--full-rescan`. Embedder swaps must be
/// explicit, never silent.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn load_incremental_baseline(
    path: &Path,
    info: &crate::embedder::EmbeddingModelInfo,
) -> Result<Option<IncrementalBaseline>> {
    use std::io::BufReader;

    if !path.exists() {
        return Ok(None);
    }

    let file = crate::sanitize::open_file_validated(path)
        .with_context(|| format!("open committed index for incremental: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let header_line =
        match read_index_line_capped(&mut reader, path, 1, "committed incremental header")
            .with_context(|| format!("read committed header: {}", path.display()))?
        {
            Some(line) => line,
            None => return Ok(None),
        };
    let header: IndexHeader = serde_json::from_str(&header_line)
        .with_context(|| format!("parse committed header: {}", path.display()))?;
    let profile = info.profile.to_string();
    if header.schema_version != INDEX_SCHEMA_VERSION
        || header.model_id != info.model_id
        || header.model_profile != profile
        || header.dimension != info.dimension
    {
        return Err(anyhow::anyhow!(
            "committed semantic index at {path} is incompatible with the active embedder.\n  \
             committed: schema={cks} model={ckm} profile={ckp} dim={ckd}\n  \
             current  : schema={cur_s} model={cur_m} profile={cur_p} dim={cur_d}\n  \
             incremental walk requires a matching embedder. Rebuild from scratch:\n    \
                 aicx index --full-rescan",
            path = path.display(),
            cks = header.schema_version,
            ckm = header.model_id,
            ckp = header.model_profile,
            ckd = header.dimension,
            cur_s = INDEX_SCHEMA_VERSION,
            cur_m = info.model_id,
            cur_p = profile,
            cur_d = info.dimension,
        ));
    }

    let cutoff = chrono::DateTime::parse_from_rfc3339(&header.generated_at)
        .with_context(|| {
            format!(
                "parse header.generated_at ({}) in {}",
                header.generated_at,
                path.display()
            )
        })?
        .with_timezone(&chrono::Utc);
    let cutoff_st: std::time::SystemTime = cutoff.into();

    #[derive(Deserialize)]
    struct IdOnly {
        id: String,
    }

    let mut embedded_ids = std::collections::HashSet::new();
    let mut data_rows = 0usize;
    for (idx, line) in capped_index_lines(reader, path, 2, "committed incremental data").enumerate()
    {
        let line =
            line.with_context(|| format!("read committed line {}: {}", idx + 2, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        data_rows += 1;
        match serde_json::from_str::<IdOnly>(&line) {
            Ok(row) => {
                embedded_ids.insert(row.id);
            }
            Err(_) => {
                // A corrupt row is rare here (writer commits whole rows or
                // not at all). Tolerate it; query_index will surface the
                // ratio at read time.
            }
        }
    }

    if data_rows == 0 {
        // Header alone, no body — treat as never-built so the operator
        // does not get a silent empty index after the first --full-rescan.
        return Ok(None);
    }

    Ok(Some(IncrementalBaseline {
        cutoff: cutoff_st,
        embedded_ids,
    }))
}

/// Return the subset of `files` that need to be embedded under the
/// incremental walk.
///
/// **Rule: missing-id always wins.** A chunk whose id is not in the
/// committed body is re-embedded regardless of mtime — this is the
/// shape produced by backup / rsync / quarantine restore, where a real
/// chunk may surface with an mtime older than `generated_at`. Without
/// this rule incremental indexing would silently drop those chunks and
/// only `--full-rescan` could recover them.
///
/// **Crash-recovery guard.** Any id already present in the committed
/// body is skipped here. Refreshing an already-embedded chunk is the
/// explicit job of `--full-rescan`; doing it silently under the
/// incremental walk would risk crash-loop double writes after a
/// partial rebuild.
///
/// Pure function so it can be exercised against a synthetic corpus in
/// tests.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn partition_incremental_files(
    files: &[crate::store::StoredContextFile],
    baseline: &IncrementalBaseline,
) -> Vec<crate::store::StoredContextFile> {
    files
        .iter()
        .filter(|stored| {
            let entry_id = chunk_id_from_path(&stored.path);
            // Missing-id always wins. A chunk restored from backup /
            // rsync / quarantine may carry an mtime older than the
            // committed `header.generated_at`, but if its id is absent
            // from the committed body we MUST re-embed it — otherwise
            // Layer 2 semantic search silently drifts incomplete and
            // operators are forced into `--full-rescan` to recover.
            // Note: mtime is intentionally not consulted for this case;
            // restored chunks may have any clock value.
            if !baseline.embedded_ids.contains(&entry_id) {
                return true;
            }
            // Crash-recovery guard: id is already in the committed body
            // -> never re-embed under the incremental walk. Genuine
            // refresh / reconcile of an already-embedded chunk is the
            // job of `--full-rescan`, not silent drift here.
            false
        })
        .cloned()
        .collect()
}

/// G-3 incremental seed: copy every data row from the committed index at
/// `target_path` into the open tmp writer. Skips the header line because
/// the caller has already written its own placeholder header into tmp.
/// Returns the number of rows copied so the caller can fold it into
/// `stats.resumed_embeddings` (the D-2 entry_count math relies on it).
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn copy_committed_body_into(
    writer: &mut std::io::BufWriter<std::fs::File>,
    target_path: &Path,
) -> Result<usize> {
    use std::io::{BufReader, Write};

    let src = crate::sanitize::open_file_validated(target_path)
        .with_context(|| format!("open committed index for seed: {}", target_path.display()))?;
    let mut reader = BufReader::new(src);
    // Discard the header — caller has already emitted a new placeholder
    // for the tmp file. Done by consuming one capped line.
    let _ = read_index_line_capped(&mut reader, target_path, 1, "committed seed header")
        .with_context(|| format!("read committed seed header: {}", target_path.display()))?;
    let mut rows = 0usize;
    for line in capped_index_lines(reader, target_path, 2, "committed seed data") {
        let line =
            line.with_context(|| format!("read committed body row: {}", target_path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        writeln!(writer, "{}", line).with_context(|| {
            format!(
                "seed incremental tmp with committed row: {}",
                target_path.display()
            )
        })?;
        rows += 1;
    }
    Ok(rows)
}

fn chunk_id_from_path(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.trim_end_matches(".md").to_string())
        .unwrap_or_else(|| {
            let s = path.to_string_lossy();
            s.replace('/', "_").trim_end_matches(".md").to_string()
        })
}

struct IndexEntryMetadata {
    kind: String,
    session_id: String,
    frame_kind: Option<String>,
    cwd: Option<String>,
}

fn indexed_metadata(entry: &IndexEntry) -> IndexEntryMetadata {
    IndexEntryMetadata {
        kind: if entry.kind.is_empty() {
            infer_kind_from_path(&entry.path)
        } else {
            entry.kind.clone()
        },
        session_id: if entry.session_id.is_empty() {
            infer_session_id_from_path(&entry.path)
        } else {
            entry.session_id.clone()
        },
        frame_kind: entry
            .frame_kind
            .clone()
            .or_else(|| chunk_frame_kind(&entry.path)),
        cwd: entry.cwd.clone().or_else(|| chunk_cwd(&entry.path)),
    }
}

fn infer_kind_from_path(path: &std::path::Path) -> String {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .find_map(|part| crate::timeline::Kind::parse(part).map(|kind| kind.dir_name().to_string()))
        .unwrap_or_else(|| "other".to_string())
}

fn infer_session_id_from_path(path: &std::path::Path) -> String {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return "-".to_string();
    };
    let parts = stem.split('_').collect::<Vec<_>>();
    if parts.len() >= 5 {
        parts[3..parts.len() - 1].join("_")
    } else {
        "-".to_string()
    }
}

fn chunk_frame_kind(path: &std::path::Path) -> Option<String> {
    first_metadata_field(path, "frame_kind")
}

fn chunk_cwd(path: &std::path::Path) -> Option<String> {
    first_metadata_field(path, "cwd")
}

fn first_metadata_field(path: &std::path::Path, key: &str) -> Option<String> {
    let content = crate::sanitize::read_to_string_validated(path).ok()?;
    let first = content.lines().next()?.trim();
    if !(first.starts_with('[') && first.ends_with(']')) {
        return None;
    }
    first
        .trim_matches(|ch| ch == '[' || ch == ']')
        .split('|')
        .filter_map(|part| part.trim().split_once(':'))
        .find_map(|(field, value)| {
            (field.trim() == key)
                .then(|| value.trim().to_string())
                .filter(|value| !value.is_empty() && value != "-")
        })
}

/// Query the persistent index for the top `limit` chunks most similar to
/// `query`. Returns an empty `Vec` if the index does not exist yet or
/// the embedder cannot load.
///
/// Pure cosine similarity in-process (no SIMD) — adequate for the tens-of-
/// thousands corpus scale aicx targets in v0.7. When the corpus grows past
/// ~100k chunks per bucket, the storage migrates to Lance + ANN search
/// behind the same `query_index` signature.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn query_index(
    project: Option<&str>,
    query: &str,
    limit: usize,
    kind_filter: Option<&str>,
    frame_kind_filter: Option<&str>,
) -> Result<Vec<QueryHit>> {
    let path = index_path(project)?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    // D-5: embedder init + query embed runs OUTSIDE the lance lock. A
    // cloud-backend embed can take hundreds of ms on a slow link; a GGUF
    // init can take seconds on cold caches. Holding the shared lance lock
    // across that window blocks concurrent rebuilds (exclusive) and other
    // readers for no good reason — query embeddings do not touch any
    // lance-resource. The lock is re-acquired only for the index file read.
    let mut engine = crate::embedder::EmbeddingEngine::new()
        .with_context(|| "semantic embedder unavailable (optional) for query")?;
    let query_embedding = engine.embed(query).with_context(|| "embed query")?;
    drop(engine);

    query_index_with_embedding(
        &path,
        &query_embedding,
        limit,
        kind_filter,
        frame_kind_filter,
    )
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn query_index_with_embedding(
    path: &Path,
    query_embedding: &[f32],
    limit: usize,
    kind_filter: Option<&str>,
    frame_kind_filter: Option<&str>,
) -> Result<Vec<QueryHit>> {
    use std::io::BufReader;

    let _lock = crate::locks::acquire_shared(crate::locks::lance_lock_path()?)?;

    // `open_file_validated` validates the path against canonical roots
    // BEFORE opening — blocks any path-traversal attempt that an
    // operator-controlled `project` could inject into the lookup. Index
    // files must always live under the canonical `~/.aicx/indexed` tree.
    let file = crate::sanitize::open_file_validated(path)
        .with_context(|| format!("open index: {}", path.display()))?;
    let mut reader = BufReader::new(file);

    // First line is header
    let header_line = match read_index_line_capped(&mut reader, path, 1, "query index header") {
        Ok(Some(line)) => line,
        Ok(None) => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let header: IndexHeader = serde_json::from_str(&header_line)
        .with_context(|| format!("parse header in {}", path.display()))?;
    if header.dimension != query_embedding.len() {
        return Err(anyhow::anyhow!(
            "dimension mismatch: index has {}, query embedder produced {}",
            header.dimension,
            query_embedding.len()
        ));
    }

    let scan = scan_index_entries(
        capped_index_lines(reader, path, 2, "query index data"),
        query_embedding,
        kind_filter,
        frame_kind_filter,
    )
    .with_context(|| format!("scan index entries in {}", path.display()))?;

    enforce_index_integrity(path, &scan)?;

    Ok(finalize_query_hits(scan.hits, limit))
}

fn enforce_index_integrity(path: &Path, scan: &ScanResult) -> Result<()> {
    if scan.corrupt_count == 0 {
        return Ok(());
    }

    let rate = scan.corrupt_count as f64 / scan.total_data_lines.max(1) as f64;
    if scan.total_data_lines >= CORRUPT_MIN_SAMPLE && rate > CORRUPT_RATE_FAIL_FAST {
        return Err(anyhow::anyhow!(
            "index integrity failure in {}: {} of {} data lines ({:.1}%) failed to parse — exceeds {:.0}% threshold. Recovery: `aicx index --full-rescan --project <name>` to rebuild from canonical store.",
            path.display(),
            scan.corrupt_count,
            scan.total_data_lines,
            rate * 100.0,
            CORRUPT_RATE_FAIL_FAST * 100.0,
        ));
    }

    tracing::warn!(
        target: "aicx::vector_index",
        corrupt = scan.corrupt_count,
        total = scan.total_data_lines,
        rate_pct = rate * 100.0,
        threshold_pct = CORRUPT_RATE_FAIL_FAST * 100.0,
        index = %path.display(),
        "index integrity: corrupt NDJSON lines tolerated below fail-fast threshold"
    );

    Ok(())
}

/// Sort `hits` by cosine score descending and cap at `limit`.
///
/// Pure post-scan tail of [`query_index`]. Extracted so the limit contract
/// (bug #32: caller-supplied `limit` is honored — never returns > `limit`
/// rows) can be unit-tested without standing up an embedder.
///
/// Safe to truncate here because the scan filters (`kind` / `frame_kind`)
/// are already pushed down into [`scan_index_entries`]: the pool fed in
/// is filter-saturated, so the truncate cannot re-introduce bug #31's
/// silent-empty pathology on the legacy path.
pub fn finalize_query_hits(mut hits: Vec<QueryHit>, limit: usize) -> Vec<QueryHit> {
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);
    hits
}

/// Result of scanning the data-line region of a persistent NDJSON index.
///
/// Returned by [`scan_index_entries`] so the caller (`query_index`) can
/// apply integrity policy (warn vs fail-fast on corrupt rows) once, at the
/// orchestration layer, instead of scattering threshold decisions through
/// the row loop.
#[derive(Debug, Default)]
pub struct ScanResult {
    /// Hits accumulated from successfully parsed entries, filter-applied
    /// (kind / frame_kind). Not yet sorted or truncated.
    pub hits: Vec<QueryHit>,
    /// Total non-empty lines observed after the header. Includes lines
    /// that failed to parse (counted in `corrupt_count`).
    pub total_data_lines: usize,
    /// Count of lines that failed `serde_json::from_str` into
    /// [`IndexEntry`]. A non-zero value here is the operator signal that
    /// the index has live-tail race damage or a writer crashed mid-flush.
    pub corrupt_count: usize,
}

/// Scan the data-line region of an opened NDJSON index, score each entry
/// against `query_embedding`, and surface a count of unparseable rows.
///
/// This is the pure core of the query path — no filesystem, no embedder,
/// no lock acquisition — so it can be exercised in unit tests with
/// synthetic inputs. The caller is responsible for header validation
/// (schema_version, dimension) before invoking; this function trusts that
/// gate.
///
/// On `Err`, the IO read itself failed (corrupt OS-level state, not
/// per-row parse failure). Per-row JSON parse errors are folded into
/// `ScanResult.corrupt_count` so the caller can apply policy
/// (`CORRUPT_RATE_FAIL_FAST`) once, at the orchestration layer.
pub fn scan_index_entries(
    lines: impl Iterator<Item = std::io::Result<String>>,
    query_embedding: &[f32],
    kind_filter: Option<&str>,
    frame_kind_filter: Option<&str>,
) -> Result<ScanResult> {
    let mut result = ScanResult::default();
    for line in lines {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        result.total_data_lines += 1;
        let entry: IndexEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(err) => {
                result.corrupt_count += 1;
                if result.corrupt_count <= CORRUPT_WARN_HEAD {
                    tracing::warn!(
                        target: "aicx::vector_index",
                        occurrence = result.corrupt_count,
                        error = %err,
                        "corrupt NDJSON line in index"
                    );
                }
                continue;
            }
        };
        let metadata = indexed_metadata(&entry);
        if let Some(kind) = kind_filter
            && metadata.kind != kind
        {
            continue;
        }
        if let Some(frame_kind) = frame_kind_filter
            && metadata.frame_kind.as_deref() != Some(frame_kind)
        {
            continue;
        }
        let score = cosine_similarity(query_embedding, &entry.embedding);
        result.hits.push(QueryHit {
            id: entry.id,
            project: entry.project,
            agent: entry.agent,
            date: entry.date,
            path: entry.path,
            kind: metadata.kind,
            session_id: metadata.session_id,
            frame_kind: metadata.frame_kind,
            cwd: metadata.cwd,
            score,
        });
    }
    Ok(result)
}

/// Query stub for builds without an embedder feature.
#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
pub fn query_index(
    _project: Option<&str>,
    _query: &str,
    _limit: usize,
    _kind_filter: Option<&str>,
    _frame_kind_filter: Option<&str>,
) -> Result<Vec<QueryHit>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod iter3_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn cosine_orthogonal_vectors_are_zero() {
        let a = [1.0f32, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn cosine_identical_vectors_are_one() {
        let a = [0.5f32, 0.3, 0.8];
        let b = [0.5f32, 0.3, 0.8];
        let s = cosine_similarity(&a, &b);
        assert!((s - 1.0).abs() < 1e-6, "expected ~1.0, got {}", s);
    }

    #[test]
    fn cosine_zero_vector_is_safely_zero() {
        let a = [0.0f32, 0.0, 0.0];
        let b = [1.0f32, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_dimension_mismatch_returns_zero() {
        let a = [1.0f32, 2.0];
        let b = [1.0f32, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn chunk_id_strips_md_extension() {
        let path = Path::new("/tmp/store/foo/bar/baz_001.md");
        assert_eq!(chunk_id_from_path(path), "baz_001");
    }

    #[test]
    fn index_path_collapses_slashes_to_underscores() {
        let dir = tempdir_for_test();
        let path = index_path_for(&dir, Some("vetcoders/aicx"));
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("vetcoders_aicx"),
            "expected slash collapsed to underscore in {path_str}"
        );
        assert!(
            path_str.ends_with("embeddings.ndjson"),
            "expected NDJSON filename in {path_str}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn partition_incremental_files_keeps_only_unembedded_and_fresh() {
        // G-3 partition logic on a synthetic corpus. The committed
        // baseline knows about ids `a` and `b`; chunk `c` is brand new.
        // `a` is older than cutoff AND committed -> skip; `b` is newer
        // than cutoff but already committed -> skip (crash-recovery
        // guard); `c` is the only survivor (new id wins regardless of
        // mtime, plus its mtime is fresh here anyway).
        use std::collections::HashSet;

        let dir = tempdir_for_test();
        let cutoff_dt = chrono::DateTime::parse_from_rfc3339("2026-05-15T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let chunks = [
            ("a", "2026-05-14T00:00:00Z"), // older than cutoff
            ("b", "2026-05-16T00:00:00Z"), // newer than cutoff
            ("c", "2026-05-16T00:00:00Z"), // newer than cutoff
        ];
        let mut files: Vec<crate::store::StoredContextFile> = Vec::new();
        for (id, mtime_rfc) in chunks {
            let path = dir.join(format!("{id}.md"));
            std::fs::write(&path, format!("# chunk {id}")).unwrap();
            let ts: std::time::SystemTime = chrono::DateTime::parse_from_rfc3339(mtime_rfc)
                .unwrap()
                .with_timezone(&chrono::Utc)
                .into();
            filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(ts)).unwrap();
            files.push(crate::store::StoredContextFile {
                path,
                project: "test".into(),
                repo: None,
                date_compact: "20260516".into(),
                date_iso: "2026-05-16".into(),
                kind: crate::timeline::Kind::Other,
                agent: "claude".into(),
                session_id: id.into(),
                chunk: 0,
            });
        }

        let mut embedded_ids = HashSet::new();
        embedded_ids.insert("a".to_string());
        embedded_ids.insert("b".to_string());
        let baseline = IncrementalBaseline {
            cutoff: cutoff_dt.into(),
            embedded_ids,
        };

        let to_embed = partition_incremental_files(&files, &baseline);
        let ids: Vec<String> = to_embed
            .iter()
            .map(|stored| chunk_id_from_path(&stored.path))
            .collect();
        assert_eq!(ids, vec!["c".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn partition_incremental_files_reembeds_missing_id_with_old_mtime() {
        // Regression for PR #6 follow-up: a chunk restored from backup /
        // rsync / quarantine restore may have an mtime OLDER than the
        // committed `header.generated_at`, but if its id is not in the
        // committed body the incremental walk MUST still pick it up.
        // Otherwise Layer 2 semantic search silently drifts incomplete
        // and operators are forced into `--full-rescan` to recover.
        use std::collections::HashSet;

        let dir = tempdir_for_test();
        let cutoff_dt = chrono::DateTime::parse_from_rfc3339("2026-05-15T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        // Chunk `restored` has an old mtime (way before the cutoff) and
        // no entry in the committed embedded_ids set — exactly the
        // backup-restore shape we want to cover.
        let restored_path = dir.join("restored.md");
        std::fs::write(&restored_path, "# restored chunk").unwrap();
        let old_ts: std::time::SystemTime =
            chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
                .into();
        filetime::set_file_mtime(&restored_path, filetime::FileTime::from_system_time(old_ts))
            .unwrap();

        let files = vec![crate::store::StoredContextFile {
            path: restored_path,
            project: "test".into(),
            repo: None,
            date_compact: "20260101".into(),
            date_iso: "2026-01-01".into(),
            kind: crate::timeline::Kind::Other,
            agent: "claude".into(),
            session_id: "restored".into(),
            chunk: 0,
        }];

        // Committed baseline mentions other ids, never `restored`.
        let mut embedded_ids = HashSet::new();
        embedded_ids.insert("a".to_string());
        embedded_ids.insert("b".to_string());
        let baseline = IncrementalBaseline {
            cutoff: cutoff_dt.into(),
            embedded_ids,
        };

        let to_embed = partition_incremental_files(&files, &baseline);
        let ids: Vec<String> = to_embed
            .iter()
            .map(|stored| chunk_id_from_path(&stored.path))
            .collect();
        assert_eq!(
            ids,
            vec!["restored".to_string()],
            "missing-id chunk with old mtime must be re-embedded"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn copy_committed_body_into_streams_data_rows_only() {
        // G-3 incremental seed: existing committed index has 5 data rows
        // plus a header. The seed helper must write exactly those 5 rows
        // into the tmp writer (header dropped because the caller writes a
        // fresh placeholder of its own).
        let dir = tempdir_for_test();
        let target = dir.join("embeddings.ndjson");
        let tmp = dir.join("embeddings.ndjson.tmp");

        let header = IndexHeader {
            schema_version: "v0-test".into(),
            model_id: "test-model".into(),
            model_profile: "base".into(),
            dimension: 4,
            generated_at: "2026-05-15T12:00:00Z".into(),
            entry_count: 5,
        };
        let mut body = serde_json::to_string(&header).unwrap();
        body.push('\n');
        for i in 0..5 {
            body.push_str(&format!(
                r#"{{"id":"row-{i}","embedding":[0.1,0.2,0.3,0.4]}}"#
            ));
            body.push('\n');
        }
        std::fs::write(&target, &body).unwrap();

        {
            let mut writer = std::io::BufWriter::new(std::fs::File::create(&tmp).unwrap());
            use std::io::Write;
            // Caller writes its own placeholder first.
            writeln!(writer, "{{\"placeholder\":true}}").unwrap();
            let rows = copy_committed_body_into(&mut writer, &target).unwrap();
            assert_eq!(rows, 5, "seed must surface row count for D-2 math");
        }

        let content = std::fs::read_to_string(&tmp).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 6, "placeholder header + 5 data rows");
        assert_eq!(lines[0], "{\"placeholder\":true}");
        for (i, expected_id) in (0..5).map(|i| format!("row-{i}")).enumerate() {
            assert!(
                lines[i + 1].contains(&expected_id),
                "row {} preserved verbatim",
                i
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn incremental_round_trip_appends_only_new_rows() {
        // End-to-end shape: first committed index has 3 rows. Operator
        // adds 5 more chunks. Incremental walk + seed + rewrite produces
        // a final file with exactly 8 rows and a truthful entry_count.
        let dir = tempdir_for_test();
        let target = dir.join("embeddings.ndjson");
        let tmp = dir.join("embeddings.ndjson.tmp");
        let commit_tmp = dir.join("embeddings.ndjson.commit-tmp");

        // 1. Seed the canonical committed file with 3 rows.
        let old_header = IndexHeader {
            schema_version: INDEX_SCHEMA_VERSION.into(),
            model_id: "test-model".into(),
            model_profile: "base".into(),
            dimension: 4,
            generated_at: "2026-05-15T12:00:00Z".into(),
            entry_count: 3,
        };
        let mut body = serde_json::to_string(&old_header).unwrap();
        body.push('\n');
        for i in 0..3 {
            body.push_str(&format!(
                r#"{{"id":"old-{i}","project":"test","agent":"claude","date":"20260515","path":"/tmp/old-{i}.md","kind":"other","session_id":"old-{i}","frame_kind":null,"cwd":null,"embedding":[0.1,0.2,0.3,0.4]}}"#
            ));
            body.push('\n');
        }
        std::fs::write(&target, &body).unwrap();

        // 2. Simulate incremental write: tmp = placeholder + copy of old
        //    body + 5 brand-new rows. Mirrors the production sequencing
        //    inside `write_index_with_options` without invoking the
        //    embedder (the unit-of-work the test is asserting is the
        //    file-level math, not the embed step itself).
        {
            let mut writer = std::io::BufWriter::new(std::fs::File::create(&tmp).unwrap());
            use std::io::Write;
            let placeholder = IndexHeader {
                entry_count: 0,
                generated_at: "2026-05-16T12:00:00Z".into(),
                ..old_header.clone()
            };
            writeln!(writer, "{}", serde_json::to_string(&placeholder).unwrap()).unwrap();
            copy_committed_body_into(&mut writer, &target).unwrap();
            for i in 0..5 {
                writeln!(
                    writer,
                    r#"{{"id":"new-{i}","project":"test","agent":"claude","date":"20260516","path":"/tmp/new-{i}.md","kind":"other","session_id":"new-{i}","frame_kind":null,"cwd":null,"embedding":[0.5,0.6,0.7,0.8]}}"#
                )
                .unwrap();
            }
        }

        // 3. Truthful header rewrite (D-2 contract) + atomic rename
        //    onto the canonical target.
        let truthful = IndexHeader {
            entry_count: 8,
            generated_at: "2026-05-16T12:00:00Z".into(),
            ..old_header
        };
        rewrite_index_with_truthful_header(&tmp, &commit_tmp, &truthful).unwrap();
        let _ = std::fs::remove_file(&tmp);
        std::fs::rename(&commit_tmp, &target).unwrap();

        // 4. Assertions: header.entry_count == 8, body has 3 old + 5 new
        //    rows, no full re-embed of the originals.
        let final_content = std::fs::read_to_string(&target).unwrap();
        let lines: Vec<&str> = final_content.lines().collect();
        assert_eq!(lines.len(), 9, "header + 3 old + 5 new = 9 lines");
        let final_header: IndexHeader = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(
            final_header.entry_count, 8,
            "D-2 entry_count truthful after incremental append"
        );
        let old_count = (1..=3).filter(|i| lines[*i].contains("old-")).count();
        let new_count = (4..=8).filter(|i| lines[*i].contains("new-")).count();
        assert_eq!(old_count, 3, "3 original rows preserved verbatim");
        assert_eq!(new_count, 5, "exactly 5 new rows appended");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn rewrite_index_with_truthful_header_replaces_placeholder_and_preserves_entries() {
        // D-2: rewrite swap-and-rename helper produces a file whose first
        // line carries the truthful entry_count and whose data lines are
        // byte-for-byte identical to the placeholder tmp.
        let dir = tempdir_for_test();
        let tmp_path = dir.join("test.ndjson.tmp");
        let final_tmp = dir.join("test.ndjson.commit-tmp");

        let placeholder = IndexHeader {
            schema_version: "v0-test".to_string(),
            model_id: "test-model".to_string(),
            model_profile: "base".to_string(),
            dimension: 4,
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            entry_count: 0,
        };
        let entries = [
            r#"{"id":"a","embedding":[0.1,0.2,0.3,0.4]}"#,
            r#"{"id":"b","embedding":[0.5,0.6,0.7,0.8]}"#,
            r#"{"id":"c","embedding":[0.9,1.0,1.1,1.2]}"#,
        ];
        let mut tmp_bytes = serde_json::to_string(&placeholder).unwrap();
        tmp_bytes.push('\n');
        for entry in &entries {
            tmp_bytes.push_str(entry);
            tmp_bytes.push('\n');
        }
        std::fs::write(&tmp_path, &tmp_bytes).unwrap();

        let truthful = IndexHeader {
            entry_count: entries.len(),
            ..placeholder.clone()
        };
        rewrite_index_with_truthful_header(&tmp_path, &final_tmp, &truthful)
            .expect("rewrite must succeed");

        let lines: Vec<String> = std::fs::read_to_string(&final_tmp)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        let header: IndexHeader = serde_json::from_str(&lines[0]).expect("header parses");
        assert_eq!(
            header.entry_count,
            entries.len(),
            "rewritten header must carry truthful entry_count"
        );
        assert_eq!(&lines[1..], entries);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_path_uses_all_bucket_for_no_project_filter() {
        let dir = tempdir_for_test();
        let path = index_path_for(&dir, None);
        assert!(
            path.to_string_lossy().contains("_all"),
            "expected _all bucket in {}",
            path.display()
        );
        assert_eq!(path, dir.join("indexed").join("_all").join(INDEX_FILE_NAME));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tempdir_for_test() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aicx-vector-index-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Build a synthetic NDJSON data-line for an `IndexEntry`. Mirrors the
    /// real `write_index` row shape without going through filesystem.
    fn make_entry_line(id: &str, embedding: Vec<f32>) -> String {
        let entry = IndexEntry {
            id: id.to_string(),
            project: "test".to_string(),
            agent: "claude".to_string(),
            date: "20260515".to_string(),
            path: std::path::PathBuf::from(format!("/tmp/aicx-test/{id}.md")),
            kind: "session".to_string(),
            session_id: id.to_string(),
            frame_kind: None,
            cwd: None,
            embedding,
        };
        serde_json::to_string(&entry).expect("serialize synthetic entry")
    }

    fn ok_lines(
        lines: impl IntoIterator<Item = String>,
    ) -> impl Iterator<Item = std::io::Result<String>> {
        lines.into_iter().map(Ok::<_, std::io::Error>)
    }

    #[test]
    fn capped_index_lines_error_on_oversized_and_advance_to_next_line() {
        let next = make_entry_line("after-oversized", vec![1.0]);
        let mut input = "x".repeat(crate::sanitize::MAX_VALIDATED_BYTES + 1);
        input.push('\n');
        input.push_str(&next);
        input.push('\n');

        let reader = std::io::BufReader::new(std::io::Cursor::new(input.into_bytes()));
        let mut lines = capped_index_lines(
            reader,
            Path::new("/tmp/aicx-vector-index-oversized.ndjson"),
            2,
            "test index data",
        );
        let err = lines
            .next()
            .expect("first oversized line is observed")
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds"));
        let second = lines
            .next()
            .expect("reader advances to following line")
            .expect("second line is valid");
        assert_eq!(second, next);
    }

    #[test]
    fn scan_index_entries_no_corrupt_returns_all_hits() {
        let q = vec![1.0f32, 0.0, 0.0];
        let lines = vec![
            make_entry_line("a", vec![1.0, 0.0, 0.0]),
            make_entry_line("b", vec![0.0, 1.0, 0.0]),
            make_entry_line("c", vec![0.5, 0.5, 0.0]),
        ];
        let scan = scan_index_entries(ok_lines(lines), &q, None, None).expect("scan");
        assert_eq!(scan.total_data_lines, 3);
        assert_eq!(scan.corrupt_count, 0);
        assert_eq!(scan.hits.len(), 3);
    }

    #[test]
    fn scan_index_entries_counts_corrupt_lines_below_threshold() {
        // 1 corrupt out of 10 = 10% — above the 5% threshold ratio, but
        // policy lives in `query_index`. The helper itself only reports.
        let q = vec![1.0f32, 0.0];
        let mut lines: Vec<String> = (0..9)
            .map(|i| make_entry_line(&format!("ok-{i}"), vec![1.0, 0.0]))
            .collect();
        lines.push("{not valid json".to_string());

        let scan = scan_index_entries(ok_lines(lines), &q, None, None).expect("scan");
        assert_eq!(scan.total_data_lines, 10);
        assert_eq!(scan.corrupt_count, 1);
        assert_eq!(scan.hits.len(), 9, "valid entries still parsed and scored");
    }

    fn make_hit(id: &str, score: f32) -> QueryHit {
        QueryHit {
            id: id.to_string(),
            project: "test".to_string(),
            agent: "claude".to_string(),
            date: "20260524".to_string(),
            path: std::path::PathBuf::from(format!("/tmp/aicx-test/{id}.md")),
            kind: "session".to_string(),
            session_id: id.to_string(),
            frame_kind: None,
            cwd: None,
            score,
        }
    }

    #[test]
    fn finalize_query_hits_truncates_to_requested_limit() {
        // Bug #32 regression. The legacy `query_index` accepted a `limit`
        // arg but did not honor it — the parameter was prefixed `_limit`
        // and the post-scan tail returned every hit. This locks the
        // contract that the returned vec has `len() <= limit`.
        let hits: Vec<QueryHit> = (0..50)
            .map(|i| make_hit(&format!("h-{i}"), (50 - i) as f32 / 50.0))
            .collect();
        let out = finalize_query_hits(hits, 10);
        assert_eq!(out.len(), 10, "limit honored: returns exactly 10 hits");
        // Top score is the highest (1.0); confirm score-desc sort holds
        // after truncate so the kept 10 are the BEST 10, not a random
        // slice.
        assert!(
            out.windows(2).all(|w| w[0].score >= w[1].score),
            "hits remain sorted score-desc after truncate"
        );
        assert_eq!(out[0].id, "h-0", "highest-scoring hit retained at head");
    }

    #[test]
    fn finalize_query_hits_returns_full_pool_when_limit_exceeds_pool() {
        // Pool shorter than limit ⇒ return everything (no padding, no
        // panic). Documents the "fewer if pool exhausted" half of the
        // bug #32 contract.
        let hits: Vec<QueryHit> = (0..3)
            .map(|i| make_hit(&format!("h-{i}"), i as f32 / 3.0))
            .collect();
        let out = finalize_query_hits(hits, 100);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn finalize_query_hits_zero_limit_returns_empty() {
        // `limit == 0` is a legal request for "no hits, just confirm the
        // scan ran". The legacy code ignored `_limit` entirely; the fix
        // honors it strictly, including the degenerate case.
        let hits: Vec<QueryHit> = (0..5)
            .map(|i| make_hit(&format!("h-{i}"), i as f32))
            .collect();
        let out = finalize_query_hits(hits, 0);
        assert!(out.is_empty(), "limit=0 returns empty vec");
    }

    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    #[test]
    fn query_index_recovery_hint_uses_full_rescan_not_fresh() {
        // Bug #33 regression. Exercise the same post-embedder query helper
        // that opens the on-disk index, scans NDJSON data rows, and surfaces
        // the operator-facing recovery hint when corruption exceeds policy.
        let root = tempdir_for_test();
        let path = index_path_for(&root, Some("recovery-hint"));
        std::fs::create_dir_all(path.parent().expect("index parent")).unwrap();

        let header = IndexHeader {
            schema_version: INDEX_SCHEMA_VERSION.to_string(),
            model_id: "test-model".to_string(),
            model_profile: "base".to_string(),
            dimension: 2,
            generated_at: "2026-05-24T18:13:11Z".to_string(),
            entry_count: 20,
        };
        let mut body = serde_json::to_string(&header).expect("serialize header");
        body.push('\n');
        for i in 0..18 {
            body.push_str(&make_entry_line(&format!("ok-{i}"), vec![1.0, 0.0]));
            body.push('\n');
        }
        body.push_str("{not valid json\n");
        body.push_str("{still not valid json\n");
        std::fs::write(&path, body).expect("write corrupt fixture index");

        let err = query_index_with_embedding(&path, &[1.0, 0.0], 10, None, None)
            .expect_err("corrupt fixture should fail-fast");
        let message = format!("{err:#}");
        assert!(
            message.contains("--full-rescan"),
            "query_index recovery hint must reference the canonical rescan flag"
        );
        let stale_flag = format!("--{}", "fresh");
        assert!(
            !message.contains(&stale_flag),
            "stale rescan flag hint must not appear in the recovery message"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_index_entries_empty_lines_are_skipped_not_counted() {
        let q = vec![1.0f32, 0.0];
        let lines = vec![
            make_entry_line("a", vec![1.0, 0.0]),
            "".to_string(),
            make_entry_line("b", vec![0.0, 1.0]),
        ];
        let scan = scan_index_entries(ok_lines(lines), &q, None, None).expect("scan");
        assert_eq!(scan.total_data_lines, 2, "empty line does not count");
        assert_eq!(scan.corrupt_count, 0);
        assert_eq!(scan.hits.len(), 2);
    }

    #[test]
    fn scan_index_entries_majority_corrupt_still_returns_ok_caller_enforces_policy() {
        // 6 corrupt out of 10 = 60%. The helper does NOT fail-fast — that
        // is `query_index`'s job. Helper only surfaces the count so the
        // caller can apply `CORRUPT_RATE_FAIL_FAST` policy with `path`
        // context for the operator-facing error message.
        let q = vec![1.0f32, 0.0];
        let mut lines: Vec<String> = (0..4)
            .map(|i| make_entry_line(&format!("ok-{i}"), vec![1.0, 0.0]))
            .collect();
        for _ in 0..6 {
            lines.push("{garbage".to_string());
        }
        let scan = scan_index_entries(ok_lines(lines), &q, None, None).expect("scan");
        assert_eq!(scan.total_data_lines, 10);
        assert_eq!(scan.corrupt_count, 6);
        assert_eq!(scan.hits.len(), 4);

        let rate = scan.corrupt_count as f64 / scan.total_data_lines as f64;
        assert!(
            scan.total_data_lines >= CORRUPT_MIN_SAMPLE.saturating_sub(11)
                && rate > CORRUPT_RATE_FAIL_FAST,
            "rate {} should exceed threshold {}",
            rate,
            CORRUPT_RATE_FAIL_FAST
        );
    }

    #[test]
    fn scan_index_entries_kind_filter_excludes_non_matching() {
        let q = vec![1.0f32];
        let lines = vec![
            make_entry_line("keep-1", vec![1.0]),
            make_entry_line("keep-2", vec![1.0]),
        ];
        // make_entry_line defaults `kind = "session"`. Asking for "report"
        // should drop everything.
        let scan =
            scan_index_entries(ok_lines(lines.clone()), &q, Some("report"), None).expect("scan");
        assert_eq!(scan.total_data_lines, 2);
        assert_eq!(scan.corrupt_count, 0);
        assert_eq!(scan.hits.len(), 0);

        let scan2 = scan_index_entries(ok_lines(lines), &q, Some("session"), None).expect("scan");
        assert_eq!(scan2.hits.len(), 2);
    }
}

/// Cosine similarity between two equal-length vectors. Returns `0.0`
/// when either vector is the zero vector (avoids NaN).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot: f32 = 0.0;
    let mut norm_a: f32 = 0.0;
    let mut norm_b: f32 = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a.sqrt() * norm_b.sqrt())
    }
}

/// Take the first `max_bytes` bytes of `s`, but never split a UTF-8
/// codepoint. Returns owned `String` with no truncation marker.
///
/// Distinct from [`aicx_parser::chunker`]'s display-oriented truncate
/// (which appends `"...[truncated]"`) — this one returns raw bytes only,
/// which is what embedders want: the marker would just become more
/// tokens consuming context-window budget for zero retrieval value.
///
/// Public so downstream lib consumers (loctree, loct-io binary bundle)
/// reuse the same codepoint-safe truncation logic when feeding the
/// embedder, instead of each crate rolling its own slice + boundary
/// loop.
pub fn take_prefix_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Render a human-friendly summary of [`IndexStats`] for stderr.
pub fn render_stats_text(stats: &IndexStats) -> String {
    let mut out = String::new();
    if stats.dry_run {
        out.push_str("aicx index — dry-run report\n");
    } else {
        out.push_str("aicx index — materialization report\n");
    }
    out.push_str(&format!("  chunks_total:        {}\n", stats.chunks_total));
    out.push_str(&format!(
        "  chunks_sampled:      {}\n",
        stats.chunks_sampled
    ));
    out.push_str(&format!(
        "  embeddings_computed: {}\n",
        stats.embeddings_computed
    ));
    if stats.resumed_embeddings > 0 {
        out.push_str(&format!(
            "  resumed_embeddings:  {}\n",
            stats.resumed_embeddings
        ));
    }
    out.push_str(&format!("  embed_errors:        {}\n", stats.embed_errors));
    if let Some(dim) = stats.dimension {
        out.push_str(&format!("  dimension:           {}\n", dim));
    }
    if let Some(model) = stats.model_id.as_deref() {
        out.push_str(&format!("  model:               {}\n", model));
    }
    if let Some(profile) = stats.model_profile.as_deref() {
        out.push_str(&format!("  profile:             {}\n", profile));
    }
    out.push_str(&format!("  elapsed_ms:          {}\n", stats.elapsed_ms));
    if let Some(eta) = stats.full_index_eta_secs() {
        out.push_str(&format!("  full_index_eta_secs: {} (estimated)\n", eta));
    }
    if let Some(reason) = stats.fallback_reason.as_deref() {
        out.push_str(&format!("  fallback_reason:     {}\n", reason));
    }
    if let Some(path) = stats.resume_tmp_path.as_deref() {
        out.push_str(&format!("  resume_tmp_path:     {}\n", path.display()));
    }
    if stats.dry_run {
        out.push_str("  note: dry-run only; omit `--dry-run` to materialize the semantic index.\n");
    }
    out
}

/// Render the same stats as a single JSON object for machine consumers.
pub fn render_stats_json(stats: &IndexStats) -> Result<String> {
    Ok(serde_json::to_string(stats)?)
}

#[cfg(test)]
mod tests;
