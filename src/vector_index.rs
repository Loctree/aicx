//! Vector index builder for `aicx` semantic search (Iter 2: dry-run only).
//!
//! Goal: take the canonical store ([`crate::store`]) markdown chunks and
//! materialize a vector representation per chunk so `aicx search` can rank
//! by cosine similarity rather than line-overlap fuzzy. The index is
//! configuration-driven so the same command works for the in-process
//! native GGUF embedder ([`aicx_embeddings`]) and, in a later iteration,
//! a cloud HTTP embed endpoint.
//!
//! This iteration ships **dry-run** only: probe the embedder, sample N
//! chunks, embed them, return stats. No persistent Lance write yet; that
//! comes in Iter 3 once the schema and the rebuild flow are validated
//! against this stats surface.
//!
//! Why ship dry-run first: it is the smallest unit of evidence that
//! end-to-end pipeline works (model load, content read, embed, dimension
//! verify). It also gives operators an honest ETA before they commit to a
//! full re-index — embedding 11k chunks on CPU is real time and they
//! should know how much before the persistent write lands.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::time::Instant;

use anyhow::Result;
use serde::Serialize;

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
    /// Always `true` for Iter 2; reserved for the persistent path.
    pub dry_run: bool,
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

/// Take the first `max_bytes` bytes of `s`, but never split a UTF-8
/// codepoint. Returns owned `String`.
///
/// Cfg-gated with the same predicate as its sole caller `run_native_pass`
/// so a no-embedder build does not warn about a dead helper. Tests below
/// run under `#[cfg(test)]` which always picks up workspace defaults
/// (where the embedder feature is enabled), so the helper stays
/// reachable for unit coverage.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn take_prefix_bytes(s: &str, max_bytes: usize) -> String {
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
