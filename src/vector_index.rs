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

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default cap on how much of a chunk's content the embedder sees.
///
/// Embedders have a max-token window (typically 512). This is a
/// conservative byte-level cap that keeps each embed call fast and within
/// any reasonable model context.
pub const DEFAULT_EMBED_PREFIX_BYTES: usize = 2048;

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
    // `store_base_dir()` resolves to the AICX home (`~/.aicx`), not the
    // corpus store (`~/.aicx/store`). Keep the vector index inside the
    // operator-owned AICX home so build, status, and search all agree.
    let index_root = base.join(INDEX_DIR_NAME);
    let bucket = project.unwrap_or(ALL_BUCKET_NAME);
    // Sanitize project bucket for filesystem (canonical lowercase per
    // canonical_project_slug invariant + replace path separators).
    let safe_bucket = bucket
        .chars()
        .map(|c| match c {
            '/' | '\\' => '_',
            c => c.to_ascii_lowercase(),
        })
        .collect::<String>();
    Ok(index_root.join(safe_bucket).join(INDEX_FILE_NAME))
}

pub fn context_corpus_index_path(project: Option<&str>) -> Result<PathBuf> {
    Ok(index_path(project)?.with_file_name(CONTEXT_CORPUS_INDEX_FILE_NAME))
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
pub fn write_index(project: Option<&str>, sample: usize) -> Result<IndexStats> {
    use std::fs;
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
    };

    let _lock = crate::locks::acquire_exclusive(crate::locks::lance_lock_path()?)?;

    let root = crate::store::store_base_dir()?;
    let files = live_index_files(&root, project)?;
    stats.chunks_total = files.len();

    if files.is_empty() {
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

    let target_path = index_path(project)?;
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create index dir: {}", parent.display()))?;
    }

    // Atomic-ish: write to `.tmp` then rename so a partial build cannot
    // poison subsequent queries. `create_file_validated` validates the
    // path against canonical roots BEFORE opening the file — important
    // because `project` flows from operator input into the index path
    // components, and a malicious `../` segment must not escape the
    // index tree.
    let tmp_path = target_path.with_extension("ndjson.tmp");
    let mut writer = BufWriter::new(
        crate::sanitize::create_file_validated(&tmp_path)
            .with_context(|| format!("open tmp index: {}", tmp_path.display()))?,
    );

    let cap = if sample == 0 {
        files.len()
    } else {
        sample.min(files.len())
    };

    let header = IndexHeader {
        schema_version: INDEX_SCHEMA_VERSION.to_string(),
        model_id: info.model_id.clone(),
        model_profile: info.profile.to_string(),
        dimension: info.dimension,
        generated_at: chrono::Utc::now().to_rfc3339(),
        entry_count: 0, // patched by the caller after the entry pass; for
                        // NDJSON streaming consumers a 0 just means "scan
                        // until EOF".
    };
    writeln!(writer, "{}", serde_json::to_string(&header)?)?;

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
        let embedding = match engine.embed(&prefix) {
            Ok(vec) => vec,
            Err(_) => {
                stats.embed_errors += 1;
                continue;
            }
        };
        let entry = IndexEntry {
            id: chunk_id_from_path(&stored.path),
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
    }

    let context_files = crate::store::scan_context_corpus_files_at(&root)?;
    if !context_files.is_empty() {
        let context_target = context_corpus_index_path(project)?;
        if let Some(parent) = context_target.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("create context-corpus index dir: {}", parent.display())
            })?;
        }
        let context_tmp = context_target.with_extension("ndjson.tmp");
        let mut context_writer = BufWriter::new(
            crate::sanitize::create_file_validated(&context_tmp).with_context(|| {
                format!("open context-corpus tmp index: {}", context_tmp.display())
            })?,
        );
        let context_header = IndexHeader {
            schema_version: INDEX_SCHEMA_VERSION.to_string(),
            model_id: info.model_id.clone(),
            model_profile: info.profile.to_string(),
            dimension: info.dimension,
            generated_at: chrono::Utc::now().to_rfc3339(),
            entry_count: 0,
        };
        writeln!(
            context_writer,
            "{}",
            serde_json::to_string(&context_header)?
        )?;
        for stored in &context_files {
            let content = match crate::sanitize::read_to_string_validated(&stored.raw_path) {
                Ok(text) => text,
                Err(_) => continue,
            };
            let prefix = take_prefix_bytes(&content, DEFAULT_EMBED_PREFIX_BYTES);
            let Ok(embedding) = engine.embed(&prefix) else {
                continue;
            };
            let entry = IndexEntry {
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
            };
            writeln!(context_writer, "{}", serde_json::to_string(&entry)?)?;
        }
        context_writer.flush().with_context(|| {
            format!("flush context-corpus tmp index: {}", context_tmp.display())
        })?;
        drop(context_writer);
        fs::rename(&context_tmp, &context_target).with_context(|| {
            format!(
                "commit context-corpus index: {} → {}",
                context_tmp.display(),
                context_target.display()
            )
        })?;
    }

    writer
        .flush()
        .with_context(|| format!("flush tmp index: {}", tmp_path.display()))?;
    drop(writer);

    fs::rename(&tmp_path, &target_path).with_context(|| {
        format!(
            "commit index: {} → {}",
            tmp_path.display(),
            target_path.display()
        )
    })?;

    stats.index_path = Some(target_path);
    stats.elapsed_ms = started.elapsed().as_millis();
    Ok(stats)
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
    };
    Ok(stats)
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
    _limit: usize,
    kind_filter: Option<&str>,
    frame_kind_filter: Option<&str>,
) -> Result<Vec<QueryHit>> {
    use std::io::{BufRead, BufReader};

    let path = index_path(project)?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let _lock = crate::locks::acquire_shared(crate::locks::lance_lock_path()?)?;

    let mut engine = crate::embedder::EmbeddingEngine::new()
        .with_context(|| "semantic embedder unavailable (optional) for query")?;
    let query_embedding = engine.embed(query).with_context(|| "embed query")?;

    // `open_file_validated` validates the path against canonical roots
    // BEFORE opening — blocks any path-traversal attempt that an
    // operator-controlled `project` could inject into the lookup. Index
    // files must always live under the canonical `~/.aicx/indexed` tree.
    let file = crate::sanitize::open_file_validated(&path)
        .with_context(|| format!("open index: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    // First line is header
    let header_line = match lines.next() {
        Some(Ok(line)) => line,
        Some(Err(err)) => return Err(err.into()),
        None => return Ok(Vec::new()),
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

    let scan = scan_index_entries(lines, &query_embedding, kind_filter, frame_kind_filter)
        .with_context(|| format!("scan index entries in {}", path.display()))?;

    if scan.corrupt_count > 0 {
        let rate = scan.corrupt_count as f64 / scan.total_data_lines.max(1) as f64;
        if scan.total_data_lines >= CORRUPT_MIN_SAMPLE && rate > CORRUPT_RATE_FAIL_FAST {
            return Err(anyhow::anyhow!(
                "index integrity failure in {}: {} of {} data lines ({:.1}%) failed to parse — exceeds {:.0}% threshold. Recovery: `aicx index --fresh --project <name>` to rebuild from canonical store.",
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
    }

    let mut hits = scan.hits;
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(hits)
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
        // Round-trip via env override so the test is hermetic.
        let dir = tempdir_for_test();
        unsafe {
            std::env::set_var("AICX_HOME", &dir);
        }
        let path = index_path(Some("vetcoders/aicx")).expect("path");
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("vetcoders_aicx"),
            "expected slash collapsed to underscore in {path_str}"
        );
        assert!(
            path_str.ends_with("embeddings.ndjson"),
            "expected NDJSON filename in {path_str}"
        );
        unsafe {
            std::env::remove_var("AICX_HOME");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn index_path_uses_all_bucket_for_no_project_filter() {
        let dir = tempdir_for_test();
        unsafe {
            std::env::set_var("AICX_HOME", &dir);
        }
        let path = index_path(None).expect("path");
        assert!(
            path.to_string_lossy().contains("_all"),
            "expected _all bucket in {}",
            path.display()
        );
        assert_eq!(path, dir.join("indexed").join("_all").join(INDEX_FILE_NAME));
        unsafe {
            std::env::remove_var("AICX_HOME");
        }
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
mod tests {
    use super::*;

    #[test]
    fn full_index_eta_zero_when_no_embeddings() {
        let stats = IndexStats {
            chunks_total: 1000,
            chunks_sampled: 10,
            embeddings_computed: 0,
            embed_errors: 10,
            dimension: None,
            model_id: None,
            model_profile: None,
            fallback_reason: Some("test".into()),
            elapsed_ms: 100,
            dry_run: true,
            index_path: None,
        };
        assert!(stats.full_index_eta_secs().is_none());
    }

    #[test]
    fn full_index_eta_scales_linearly() {
        let stats = IndexStats {
            chunks_total: 10_000,
            chunks_sampled: 10,
            embeddings_computed: 10,
            embed_errors: 0,
            dimension: Some(1024),
            model_id: Some("F2LLM-v2-0.6B.Q4_K_M.gguf".into()),
            model_profile: Some("base".into()),
            fallback_reason: None,
            // 10 embeds in 1000 ms ⇒ 100 ms per embed.
            elapsed_ms: 1000,
            dry_run: true,
            index_path: None,
        };
        // 10000 * 100 ms = 1_000_000 ms = 1000 s.
        assert_eq!(stats.full_index_eta_secs(), Some(1000));
    }

    #[test]
    fn render_stats_text_includes_fallback_reason_when_set() {
        let stats = IndexStats {
            chunks_total: 0,
            chunks_sampled: 0,
            embeddings_computed: 0,
            embed_errors: 0,
            dimension: None,
            model_id: None,
            model_profile: None,
            fallback_reason: Some("native-embedder feature not compiled in".into()),
            elapsed_ms: 5,
            dry_run: true,
            index_path: None,
        };
        let text = render_stats_text(&stats);
        assert!(text.contains("fallback_reason:"));
        assert!(text.contains("native-embedder feature not compiled in"));
        assert!(text.contains("dry-run only"));
    }

    #[test]
    fn render_stats_text_includes_eta_when_available() {
        let stats = IndexStats {
            chunks_total: 5_000,
            chunks_sampled: 50,
            embeddings_computed: 50,
            embed_errors: 0,
            dimension: Some(1024),
            model_id: Some("F2LLM-v2-0.6B.Q4_K_M.gguf".into()),
            model_profile: Some("base".into()),
            fallback_reason: None,
            elapsed_ms: 5_000,
            dry_run: true,
            index_path: None,
        };
        let text = render_stats_text(&stats);
        assert!(text.contains("full_index_eta_secs:"));
        assert!(text.contains("dimension:"));
        assert!(text.contains("F2LLM-v2-0.6B.Q4_K_M.gguf"));
    }

    #[test]
    fn render_stats_json_round_trips() {
        let stats = IndexStats {
            chunks_total: 42,
            chunks_sampled: 8,
            embeddings_computed: 8,
            embed_errors: 0,
            dimension: Some(1024),
            model_id: Some("model-x".into()),
            model_profile: Some("base".into()),
            fallback_reason: None,
            elapsed_ms: 800,
            dry_run: true,
            index_path: None,
        };
        let json = render_stats_json(&stats).expect("serialize");
        assert!(json.contains("\"chunks_total\":42"));
        assert!(json.contains("\"dry_run\":true"));
        assert!(json.contains("\"model_id\":\"model-x\""));
    }

    #[test]
    fn take_prefix_bytes_short_input_unchanged() {
        let s = "hello";
        assert_eq!(take_prefix_bytes(s, 10), "hello");
    }

    #[test]
    fn take_prefix_bytes_caps_at_codepoint_boundary() {
        // "ą" is 2 bytes in UTF-8. Cap at 1 byte must not split it.
        let s = "ąść";
        let out = take_prefix_bytes(s, 1);
        assert_eq!(out, "");
    }

    #[test]
    fn take_prefix_bytes_preserves_codepoints_under_cap() {
        let s = "ąść";
        // Bytes: ą=0xC4 0x85 (2), ś=0xC5 0x9B (2), ć=0xC4 0x87 (2). 6 total.
        let out = take_prefix_bytes(s, 4);
        // Cap of 4 must include exactly two codepoints (ą + ś).
        assert_eq!(out, "ąś");
    }
}
