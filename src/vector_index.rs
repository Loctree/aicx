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
//!   index per project at `~/.aicx/index/<bucket>/embeddings.ndjson`,
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
    let files = crate::store::scan_context_files_project_at(&root, project)?;
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
            stats.fallback_reason = Some(format!("embedder init failed: {err}"));
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
const INDEX_DIR_NAME: &str = "index";
const ALL_BUCKET_NAME: &str = "_all";

/// Resolve the on-disk path of the persistent vector index for a given
/// project bucket. When `project == None`, returns the cross-project
/// `_all` bucket path so an operator can index every chunk in one file.
pub fn index_path(project: Option<&str>) -> Result<PathBuf> {
    let base = crate::store::store_base_dir()?;
    // The store base is usually `~/.aicx/store`; the index lives next
    // to it as `~/.aicx/index/<bucket>/embeddings.ndjson`.
    let index_root = base.parent().unwrap_or(&base).join(INDEX_DIR_NAME);
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
    let files = crate::store::scan_context_files_project_at(&root, project)?;
    stats.chunks_total = files.len();

    if files.is_empty() {
        stats.fallback_reason = Some("no chunks found in canonical store".to_string());
        stats.elapsed_ms = started.elapsed().as_millis();
        return Ok(stats);
    }

    let mut engine = match crate::embedder::EmbeddingEngine::new() {
        Ok(engine) => engine,
        Err(err) => {
            stats.fallback_reason = Some(format!("embedder init failed: {err}"));
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
    // poison subsequent queries.
    let tmp_path = target_path.with_extension("ndjson.tmp");
    let mut writer = BufWriter::new(
        fs::File::create(&tmp_path)
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
            embedding,
        };
        writeln!(writer, "{}", serde_json::to_string(&entry)?)?;
        stats.embeddings_computed += 1;
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

/// Query the persistent index for the top `limit` chunks most similar to
/// `query`. Returns an empty `Vec` if the index does not exist yet or
/// the embedder cannot load.
///
/// Pure cosine similarity in-process (no SIMD) — adequate for the tens-of-
/// thousands corpus scale aicx targets in v0.7. When the corpus grows past
/// ~100k chunks per bucket, the storage migrates to Lance + ANN search
/// behind the same `query_index` signature.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub fn query_index(project: Option<&str>, query: &str, limit: usize) -> Result<Vec<QueryHit>> {
    use std::fs;
    use std::io::{BufRead, BufReader};

    let path = index_path(project)?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let _lock = crate::locks::acquire_shared(crate::locks::lance_lock_path()?)?;

    let mut engine = crate::embedder::EmbeddingEngine::new()
        .with_context(|| "embedder init failed for query")?;
    let query_embedding = engine.embed(query).with_context(|| "embed query")?;

    let file = fs::File::open(&path).with_context(|| format!("open index: {}", path.display()))?;
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

    let mut hits: Vec<QueryHit> = Vec::new();
    for line in lines {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let entry: IndexEntry = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue, // tolerate corrupt lines without bailing
        };
        let score = cosine_similarity(&query_embedding, &entry.embedding);
        hits.push(QueryHit {
            id: entry.id,
            project: entry.project,
            agent: entry.agent,
            date: entry.date,
            path: entry.path,
            score,
        });
    }

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(limit);
    Ok(hits)
}

/// Query stub for builds without an embedder feature.
#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
pub fn query_index(_project: Option<&str>, _query: &str, _limit: usize) -> Result<Vec<QueryHit>> {
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
    out.push_str("aicx index — dry-run report\n");
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
        out.push_str("  note: dry-run only; persistent Lance write lands in Iter 3.\n");
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
