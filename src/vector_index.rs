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

use std::io::{self, BufRead};
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

/// Report for deriving one project-scoped semantic bucket from the
/// cross-project `_all` bucket without re-embedding.
#[derive(Debug, Clone, Serialize)]
pub struct DerivedProjectIndexStats {
    pub project: String,
    pub source_index_path: PathBuf,
    pub index_path: PathBuf,
    pub entries_written: usize,
    pub elapsed_ms: u128,
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
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).with_context(|| format!("read {}", path.display()))?;
    Ok(format!("{:x}", hasher.finalize()))
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
    let mut hybrid_delta_chunks = Vec::new();

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
        if incremental_baseline.is_some() {
            let metadata = serde_json::json!({
                "source_path": stored.path.to_string_lossy(),
                "project": stored.project,
                "agent": stored.agent,
                "date": stored.date_iso,
                "kind": stored.kind.dir_name(),
                "session_id": stored.session_id,
                "frame_kind": chunk_frame_kind(&stored.path),
                "cwd": chunk_cwd(&stored.path),
            });
            hybrid_delta_chunks.push(aicx_retrieve::DenseChunkRef {
                chunk: aicx_retrieve::ChunkRef {
                    id: entry_id.clone(),
                    source_path: stored.path.to_string_lossy().to_string(),
                    text: content,
                    metadata,
                },
                embedding: entry.embedding.clone(),
            });
        }
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

    // Bug A1: `materialize_hybrid_index` does a full, destructive tantivy
    // rebuild on every call. Skip it when this incremental run added nothing
    // and the committed manifest still matches the embedder — keeps the
    // last-good hybrid index queryable and avoids the 98%-CPU / 12-min
    // rebuild pathology. A dimension/model migration flips the manifest match
    // to false and rebuilds regardless.
    let manifest_matches_pre_commit_source =
        incremental_baseline.as_ref().is_some_and(|baseline| {
            hybrid_manifest_matches_committed_source(
                project,
                baseline.source_chunk_count,
                &baseline.source_hash_blake3,
            )
        });
    let hybrid_mode = decide_hybrid_materialization(
        incremental_baseline.is_some(),
        indexed,
        failed,
        hybrid_manifest_matches_embedder(project, &info),
        manifest_matches_pre_commit_source,
        has_existing_hybrid_artifacts(project),
    );
    let hybrid_result = match hybrid_mode {
        HybridMaterializationMode::Skip => {
            eprintln!(
                "[aicx][phase=index event=hybrid_skip reason=no_op_incremental_manifest_match indexed=0 failed=0]"
            );
            Ok(None)
        }
        HybridMaterializationMode::IncrementalInsert => {
            let committed_source_hash = observed_source_hash_for_index_path(&target_path)?;
            match incremental_materialize_hybrid(
                project,
                &info,
                &hybrid_delta_chunks,
                total_indexed,
                &committed_source_hash,
            ) {
                Ok(manifest) => {
                    eprintln!(
                        "[aicx][phase=index event=hybrid_incremental indexed={indexed} failed={failed} dense_count={}]",
                        manifest.dense_count
                    );
                    Ok(Some(manifest))
                }
                Err(err) => {
                    eprintln!(
                        "[aicx][phase=index event=hybrid_incremental_fallback reason=incremental_failed error={err:#}]"
                    );
                    materialize_hybrid_index(&target_path, project, &info).map(Some)
                }
            }
        }
        HybridMaterializationMode::FullRebuild => {
            materialize_hybrid_index(&target_path, project, &info).map(Some)
        }
    };
    if let Err(err) = hybrid_result {
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
    let mut skipped_missing_source_count = 0usize;
    let mut skipped_missing_source_examples: Vec<PathBuf> = Vec::new();

    for entry in entries {
        let text = match crate::sanitize::read_to_string_validated(&entry.path) {
            Ok(text) => text,
            Err(_err) if !entry.path.exists() => {
                skipped_missing_source_count += 1;
                if skipped_missing_source_examples.len() < 5 {
                    skipped_missing_source_examples.push(entry.path.clone());
                }
                continue;
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("read chunk for hybrid index: {}", entry.path.display())
                });
            }
        };
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
    if skipped_missing_source_count > 0 {
        let examples = skipped_missing_source_examples
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" | ");
        eprintln!(
            "[aicx][phase=index event=hybrid_skip_missing_sources skipped={} examples={}]",
            skipped_missing_source_count, examples
        );
    }
    if dense_chunks.is_empty() && skipped_missing_source_count > 0 {
        anyhow::bail!(
            "hybrid build has no live source chunks after skipping {} stale semantic row(s)",
            skipped_missing_source_count
        );
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

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn incremental_materialize_hybrid(
    project: Option<&str>,
    info: &crate::embedder::EmbeddingModelInfo,
    delta_chunks: &[aicx_retrieve::DenseChunkRef],
    source_chunk_count: usize,
    source_hash: &str,
) -> Result<aicx_retrieve::Manifest> {
    use aicx_retrieve::{
        DenseIndex, Distance, FusionStrategy, LexicalIndex, Manifest, ReciprocalRankFusion,
        TantivyAdapter, validate_live_bindings_for_refresh,
    };

    if delta_chunks.is_empty() {
        anyhow::bail!("incremental hybrid materialize requires at least one delta chunk");
    }

    let manifest_dir = hybrid_index_dir(project)?;
    let manifest_path = hybrid_manifest_path(project)?;
    let dense_path = hybrid_dense_path(project)?;
    let manifest = Manifest::read_from_path(&manifest_path)?;
    let mut lexical = TantivyAdapter::new(manifest_dir)?;
    let mut dense = aicx_retrieve::load_from_ndjson(&dense_path, info.dimension, Distance::Cosine)?;
    let fusion = ReciprocalRankFusion::default();
    let fingerprint = hybrid_embedder_fingerprint(info);

    validate_live_bindings_for_refresh(&manifest, &lexical, &dense, &fusion, &fingerprint)
        .map_err(|err| anyhow::anyhow!("incremental hybrid validate existing artifacts: {err}"))?;

    let build_started_at = Manifest::now_utc();
    for delta in delta_chunks {
        lexical.insert(&delta.chunk)?;
        dense.insert(delta)?;
    }
    dense.persist_ndjson(&dense_path)?;
    let build_completed_at = Manifest::now_utc();
    let refreshed = Manifest {
        schema_version: manifest.schema_version,
        generation_id: Manifest::fresh_generation_id(),
        source_chunk_count,
        source_hash_blake3: aicx_retrieve::source_hash_blake3(source_hash),
        embedder_model: fingerprint.model,
        embedder_url_hash: fingerprint.url_hash,
        embedder_dim: fingerprint.dim,
        embedder_distance: fingerprint.distance,
        dense_count: dense.count(),
        dense_kind: dense.kind().to_string(),
        lexical_commit_id: lexical.commit_id().0.clone(),
        lexical_doc_count: lexical.doc_count(),
        build_started_at,
        build_completed_at,
        build_wall_seconds: build_completed_at
            .signed_duration_since(build_started_at)
            .num_seconds()
            .max(0) as u64,
        fusion_algorithm: fusion.name().to_string(),
        fusion_k: aicx_retrieve::RRF_K_DEFAULT,
    };
    refreshed.write_to_path(&manifest_path)?;
    Ok(refreshed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HybridMaterializationMode {
    Skip,
    IncrementalInsert,
    FullRebuild,
}

pub(crate) fn decide_hybrid_materialization(
    is_incremental: bool,
    indexed: usize,
    failed: usize,
    manifest_matches_embedder: bool,
    manifest_matches_committed_source: bool,
    has_existing_hybrid: bool,
) -> HybridMaterializationMode {
    let hybrid_is_current =
        manifest_matches_embedder && manifest_matches_committed_source && has_existing_hybrid;
    if is_incremental && indexed == 0 && failed == 0 && hybrid_is_current {
        HybridMaterializationMode::Skip
    } else if is_incremental && indexed > 0 && failed == 0 && hybrid_is_current {
        HybridMaterializationMode::IncrementalInsert
    } else {
        HybridMaterializationMode::FullRebuild
    }
}

/// Pure decision: should `materialize_hybrid_index` be SKIPPED on this run?
///
/// Bug A1: `materialize_hybrid_index` triggers a full tantivy lexical rebuild
/// (`remove_dir_all` + reindex of every doc) and is otherwise called
/// unconditionally after each dense commit — so a no-op incremental run
/// (zero new chunks) needlessly burns CPU and tears down the last-good
/// lexical index. Skip it when nothing changed.
///
/// Skip ONLY when ALL hold:
/// - `is_incremental` — not a `--full-rescan` (which must always rebuild),
/// - `indexed == 0` — no new/changed rows materialized,
/// - `failed == 0` — no embed failures to reconcile,
/// - `manifest_matches_embedder` — the committed hybrid manifest still matches
///   the current embedder. A dimension/model change (operator's F2LLM 2048 ->
///   qwen3 4096 migration) flips this to false and FORCES a rebuild even on a
///   no-op incremental, so search never keeps serving a stale-model hybrid.
/// - `manifest_matches_committed_source` — the existing hybrid still points at
///   the same committed semantic corpus the incremental delta is based on.
/// - `has_existing_hybrid` — manifest + persisted dense + Tantivy lexical
///   artifacts exist.
#[allow(dead_code)]
pub(crate) fn should_skip_hybrid_rebuild(
    is_incremental: bool,
    indexed: usize,
    failed: usize,
    manifest_matches_embedder: bool,
    manifest_matches_committed_source: bool,
    has_existing_hybrid: bool,
) -> bool {
    decide_hybrid_materialization(
        is_incremental,
        indexed,
        failed,
        manifest_matches_embedder,
        manifest_matches_committed_source,
        has_existing_hybrid,
    ) == HybridMaterializationMode::Skip
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn has_existing_hybrid_artifacts(project: Option<&str>) -> bool {
    let Ok(manifest_path) = hybrid_manifest_path(project) else {
        return false;
    };
    let Ok(dense_path) = hybrid_dense_path(project) else {
        return false;
    };
    let Ok(hybrid_dir) = hybrid_index_dir(project) else {
        return false;
    };
    let lexical_meta = hybrid_dir
        .join(aicx_retrieve::TANTIVY_INDEX_DIR)
        .join("meta.json");
    manifest_path.exists() && dense_path.exists() && lexical_meta.exists()
}

/// Does the committed hybrid manifest still match the CURRENT embedder?
///
/// Reads the on-disk manifest and compares its recorded dimension + model id
/// against the live embedder. A missing/unreadable manifest, or a
/// dimension/model change (F2LLM 2048 -> qwen3 4096 migration), returns
/// `false` — which forces [`should_skip_hybrid_rebuild`] to rebuild rather
/// than skip, so search never serves a stale-model hybrid.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn hybrid_manifest_matches_embedder(
    project: Option<&str>,
    info: &crate::embedder::EmbeddingModelInfo,
) -> bool {
    let Ok(manifest_path) = hybrid_manifest_path(project) else {
        return false;
    };
    if !manifest_path.exists() {
        return false;
    }
    match aicx_retrieve::Manifest::read_from_path(&manifest_path) {
        Ok(manifest) => {
            manifest.embedder_dim == info.dimension && manifest.embedder_model == info.model_id
        }
        Err(_) => false,
    }
}

/// Rebuild the hybrid lexical + dense + manifest artifacts from the EXISTING
/// committed semantic index, WITHOUT re-embedding (the `aicx repair` recovery
/// path). The committed index already holds every chunk's embedding, so
/// `materialize_hybrid_index` reconstructs the full retrieval surface from it
/// — turning an unqueryable build (dense present, hybrid missing/stale) into a
/// queryable one in seconds instead of a multi-hour re-embed.
///
/// The embedder is loaded only for its model/dimension fingerprint (recorded
/// in the manifest and checked against the committed index); it never
/// re-embeds chunk content.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn repair_hybrid_from_committed(project: Option<&str>) -> Result<aicx_retrieve::Manifest> {
    let index_path = index_path(project)?;
    if !index_path.exists() {
        anyhow::bail!(
            "no committed semantic index at {} — run `aicx index` first (repair only rebuilds \
             hybrid artifacts from existing embeddings; it does not embed)",
            index_path.display()
        );
    }
    let engine = crate::embedder::EmbeddingEngine::new().context(
        "repair needs the embedder for the model/dimension fingerprint (it does NOT re-embed chunks)",
    )?;
    let info = engine.info().clone();
    materialize_hybrid_index(&index_path, project, &info)
}

/// Build-disabled stub for binaries compiled without any embedder feature.
#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
pub fn repair_hybrid_from_committed(_project: Option<&str>) -> Result<aicx_retrieve::Manifest> {
    anyhow::bail!(
        "repair requires an embedder feature — rebuild with `--features native-embedder` or `--features cloud-embedder`"
    )
}

/// Materialize `indexed/<project>/embeddings.ndjson` from the existing
/// cross-project `_all` semantic index without invoking the embedder.
///
/// The `_all` index already stores full [`IndexEntry`] rows including each
/// vector. For project-scoped fast paths we can stream-filter those rows into
/// a project bucket, rewrite the header with the truthful row count, and then
/// let [`repair_hybrid_from_committed`] build lexical+dense hybrid artifacts
/// from that committed project index. This is a repair/derivation path, not a
/// semantic refresh: any chunks missing from `_all` are necessarily missing
/// from the derived bucket too.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn derive_project_index_from_all(project: &str) -> Result<DerivedProjectIndexStats> {
    use std::fs;
    use std::io::{BufReader, BufWriter, Write};

    if project.trim().is_empty() {
        anyhow::bail!("project is required; refusing to derive the cross-project _all bucket");
    }

    let started = Instant::now();
    let source_index_path = index_path(None)?;
    let index_path = index_path(Some(project))?;
    if source_index_path == index_path {
        anyhow::bail!(
            "source and target index paths are identical: {}",
            index_path.display()
        );
    }
    if !source_index_path.exists() {
        anyhow::bail!(
            "no cross-project semantic index at {} — build `_all` first with `aicx index`",
            source_index_path.display()
        );
    }

    let _lock = crate::locks::acquire_exclusive(crate::locks::lance_lock_path()?)?;
    let Some(parent) = index_path.parent() else {
        anyhow::bail!("index path has no parent: {}", index_path.display());
    };
    fs::create_dir_all(parent)
        .with_context(|| format!("create derived project index dir: {}", parent.display()))?;

    let tmp_path = index_path.with_extension("ndjson.tmp");
    let final_tmp_path = index_path.with_extension("ndjson.commit.tmp");
    let _ = fs::remove_file(&tmp_path);
    let _ = fs::remove_file(&final_tmp_path);

    let source = crate::sanitize::open_file_validated(&source_index_path)
        .with_context(|| format!("open _all semantic index: {}", source_index_path.display()))?;
    let mut reader = BufReader::new(source);
    let header_line = read_index_line_capped(
        &mut reader,
        &source_index_path,
        1,
        "cross-project semantic header",
    )
    .with_context(|| format!("read _all header: {}", source_index_path.display()))?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "empty cross-project semantic index: {}",
            source_index_path.display()
        )
    })?;
    let source_header = serde_json::from_str::<IndexHeader>(&header_line)
        .with_context(|| format!("parse _all header: {}", source_index_path.display()))?;

    let tmp = crate::sanitize::create_file_validated(&tmp_path)
        .with_context(|| format!("create derived project tmp index: {}", tmp_path.display()))?;
    let mut writer = BufWriter::new(tmp);
    writeln!(
        writer,
        "{}",
        serde_json::to_string(&IndexHeader {
            schema_version: source_header.schema_version.clone(),
            model_id: source_header.model_id.clone(),
            model_profile: source_header.model_profile.clone(),
            dimension: source_header.dimension,
            generated_at: chrono::Utc::now().to_rfc3339(),
            entry_count: 0,
        })?
    )
    .with_context(|| format!("write placeholder derived header: {}", tmp_path.display()))?;

    let project_json = serde_json::to_string(project)?;
    let project_needle = format!("\"project\":{project_json}");
    let mut entries_written = 0usize;
    for (idx, line) in
        capped_index_lines(reader, &source_index_path, 2, "cross-project semantic data").enumerate()
    {
        let line = line.with_context(|| {
            format!(
                "read _all semantic index line {}: {}",
                idx + 2,
                source_index_path.display()
            )
        })?;
        if line.trim().is_empty() || !line.contains(&project_needle) {
            continue;
        }
        writeln!(writer, "{line}")
            .with_context(|| format!("write derived project row: {}", tmp_path.display()))?;
        entries_written += 1;
    }
    writer
        .flush()
        .with_context(|| format!("flush derived project tmp index: {}", tmp_path.display()))?;

    if entries_written == 0 {
        let _ = fs::remove_file(&tmp_path);
        anyhow::bail!(
            "no entries for project `{project}` found in {}",
            source_index_path.display()
        );
    }

    let truthful_header = IndexHeader {
        schema_version: source_header.schema_version,
        model_id: source_header.model_id,
        model_profile: source_header.model_profile,
        dimension: source_header.dimension,
        generated_at: chrono::Utc::now().to_rfc3339(),
        entry_count: entries_written,
    };
    rewrite_index_with_truthful_header(&tmp_path, &final_tmp_path, &truthful_header)
        .with_context(|| format!("rewrite derived project header: {}", tmp_path.display()))?;
    let _ = fs::remove_file(&tmp_path);
    fs::rename(&final_tmp_path, &index_path).with_context(|| {
        format!(
            "commit derived project index: {} -> {}",
            final_tmp_path.display(),
            index_path.display()
        )
    })?;

    Ok(DerivedProjectIndexStats {
        project: project.to_string(),
        source_index_path,
        index_path,
        entries_written,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
pub fn derive_project_index_from_all(_project: &str) -> Result<DerivedProjectIndexStats> {
    anyhow::bail!(
        "derive requires an embedder feature — rebuild with `--features native-embedder` or `--features cloud-embedder`"
    )
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
pub(crate) fn read_committed_index_entries(path: &Path) -> Result<(IndexHeader, Vec<IndexEntry>)> {
    read_committed_index_entries_matching_project(path, None)
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub(crate) fn read_committed_index_entries_matching_project(
    path: &Path,
    project_filter: Option<&str>,
) -> Result<(IndexHeader, Vec<IndexEntry>)> {
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
    let project_needle = project_filter
        .map(|project| {
            serde_json::to_string(project).map(|project_json| format!("\"project\":{project_json}"))
        })
        .transpose()?;
    for (idx, line) in capped_index_lines(reader, path, 2, "committed semantic data").enumerate() {
        let line = line.with_context(|| format!("read line {} in {}", idx + 2, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(needle) = project_needle.as_deref() {
            // The persistent IndexEntry JSON is compact and includes a
            // top-level `"project":"owner/repo"` field. For `_all` fallback
            // queries this cheap textual guard avoids deserializing large
            // 4096-float embeddings from unrelated projects before the dense
            // leg can apply its exact FilterSet.
            if !line.contains(needle) {
                continue;
            }
        }
        let entry = serde_json::from_str::<IndexEntry>(&line)
            .with_context(|| format!("parse line {} in {}", idx + 2, path.display()))?;
        entries.push(entry);
    }
    Ok((header, entries))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub(crate) fn index_entry_metadata_json(entry: &IndexEntry) -> serde_json::Value {
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
    pub(crate) source_chunk_count: usize,
    pub(crate) source_hash_blake3: String,
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

    let source_hash = observed_source_hash_for_index_path(path)
        .with_context(|| format!("hash committed incremental index: {}", path.display()))?;

    Ok(Some(IncrementalBaseline {
        cutoff: cutoff_st,
        embedded_ids,
        source_chunk_count: data_rows,
        source_hash_blake3: aicx_retrieve::source_hash_blake3(&source_hash),
    }))
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn hybrid_manifest_matches_committed_source(
    project: Option<&str>,
    source_chunk_count: usize,
    source_hash_blake3: &str,
) -> bool {
    let Ok(path) = hybrid_manifest_path(project) else {
        return false;
    };
    let Ok(manifest) = aicx_retrieve::Manifest::read_from_path(&path) else {
        return false;
    };
    manifest.source_chunk_count == source_chunk_count
        && manifest.source_hash_blake3 == source_hash_blake3
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
mod iter3_tests;

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
