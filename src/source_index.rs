//! Source-driven lexical index.
//!
//! The durable corpus is the session catalog plus live source files. This
//! module parses each cataloged source once, keeps only user/assistant signal,
//! writes at most one readable extract per session, and publishes Tantivy
//! directly. It never reads or writes per-frame store cards or embedding
//! NDJSON intermediates.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use aicx_parser::engine::scope_evidence::{
    WindowScope, WorkdirEvidence, effective_window_scope, normalize_workdir, recorded_workdir,
    tool_call_workdirs,
};

use crate::catalog::CatalogEntry;
use crate::progress::{Heartbeat, NoopReporter, Phase, Reporter};
use crate::timeline::{FrameKind, TimelineEntry};

const MAX_MESSAGE_CHARS: usize = 256 * 1024;
const MAX_EXTRACT_CHARS: usize = 4 * 1024 * 1024;
const MAX_UNBROKEN_TOKEN_CHARS: usize = 4096;
const MAX_FULL_PARSE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSONL_RECORD_BYTES: usize = 2 * 1024 * 1024;

/// Bump whenever signal filtering or extract body shaping changes.
///
/// The catalog fingerprint alone is not enough: a CURRENT generation built
/// before thought-token stripping still matched the same catalog bytes and
/// short-circuited forever, leaving search previews full of
/// `{"type":"thought","data":"..."}` spam. Including this constant forces a
/// one-shot rebuild so index truth tracks filter truth.
pub(crate) const SIGNAL_FILTER_VERSION: &str = "signal-v6-scope-fails-closed";

const PARSE_STATE_SCHEMA: &str = "aicx.source_parse_state.v1";
const PARSE_STATE_RELPATH: &str = "indexed/_all/source_parse_state.v1.json";

#[derive(Debug, Clone, Serialize)]
pub struct SourceIndexReport {
    pub catalog_path: String,
    pub sources_total: usize,
    pub sources_parsed: usize,
    /// Sessions whose extract was reused without re-parsing the live source.
    pub sources_reused: usize,
    pub sources_skipped: usize,
    pub raw_frames: usize,
    pub signal_frames: usize,
    pub filtered_frames: usize,
    pub extracts_written: usize,
    /// Documents this run materialized with the card.v3 distill block
    /// (W2-02). Reused cached extracts stay v2 until re-parsed, so this is
    /// the incremental coverage delta, not the corpus total.
    #[serde(default)]
    pub distill_docs: usize,
    pub lexical_docs: usize,
    /// Dense vectors published when `semantic` was requested; 0 for lexical-only.
    #[serde(default)]
    pub dense_docs: usize,
    /// `optional_not_built` | `exact_mmap_v1` | …
    #[serde(default)]
    pub dense_kind: String,
    /// Whether this run requested `aicx index --semantic`.
    #[serde(default)]
    pub semantic_requested: bool,
    pub unchanged: bool,
    pub wall_ms: u64,
    pub manifest_path: Option<String>,
    pub skipped_by_agent: BTreeMap<String, usize>,
}

/// Honest corpus coverage attached to every CLI search surface.
///
/// `scanned_sessions` is the number of catalog sessions present in the
/// published CURRENT lexical generation. `total_sessions` is the durable
/// catalog size. Missing rows are grouped by extractor so an empty result
/// cannot silently hide an unreadable agent source.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SearchCoverage {
    pub scanned_sessions: usize,
    pub total_sessions: usize,
    pub skipped: BTreeMap<String, usize>,
}

impl SearchCoverage {
    pub fn single_session(scanned: bool, agent: &str) -> Self {
        let mut skipped = BTreeMap::new();
        if !scanned {
            skipped.insert(format!("{}_unreadable", coverage_agent_key(agent)), 1);
        }
        Self {
            scanned_sessions: usize::from(scanned),
            total_sessions: 1,
            skipped,
        }
    }

    pub fn render_line(&self) -> String {
        let skipped = if self.skipped.is_empty() {
            "none".to_string()
        } else {
            self.skipped
                .iter()
                .map(|(reason, count)| format!("{reason}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "scanned {} of {} sessions; skipped: {}",
            self.scanned_sessions, self.total_sessions, skipped
        )
    }
}

/// One stable, source-ordered passage from a catalog session.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionPassage {
    pub passage: usize,
    pub line_span: LineSpan,
    pub match_lines: Vec<usize>,
    pub text: String,
    pub source_path: String,
    pub document_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LineSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionPassageReport {
    pub session_id: String,
    pub agent: String,
    pub query: String,
    pub mode: &'static str,
    pub context: usize,
    pub cache_hit: bool,
    pub passages: Vec<SessionPassage>,
    pub coverage: SearchCoverage,
}

#[derive(Debug)]
struct SessionDocument {
    body: String,
    source_path: String,
    document_path: PathBuf,
    cache_hit: bool,
}

/// Durable per-session parse ledger for true incremental index builds.
///
/// Whole-catalog fingerprint short-circuit covers the no-op case. When the
/// catalog grows (new sessions) this ledger lets the indexer re-parse only the
/// changed rows and reuse cached extracts for the rest — the audit failure was
/// "+28 sessions → full multi-ten-minute reparse".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SourceParseState {
    schema: String,
    signal_filter_version: String,
    /// Hash of normalized checkout prefixes from `$AICX_HOME/.aicxignore`.
    /// Old ledgers deserialize to empty and are deliberately not reusable.
    #[serde(default)]
    repo_path_ignore_fingerprint: String,
    sessions: BTreeMap<String, SessionParseRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionParseRecord {
    source_path: String,
    /// Live source size when this extract was produced (bytes).
    #[serde(default)]
    source_len: u64,
    /// Live source mtime when this extract was produced (unix nanoseconds).
    #[serde(default)]
    source_mtime_ns: u64,
    extract_relpath: String,
    extract_sha256: String,
    raw_frames: usize,
    signal_frames: usize,
    filtered_frames: usize,
    project: Option<String>,
    date: Option<String>,
    cwd: Option<String>,
    /// Whole-session mixed-workstream verdict at parse time; reused chunks
    /// re-stamp it into metadata so the intents index lane can fall back to
    /// the census for mixed sessions.
    #[serde(default)]
    scope_conflict: bool,
    /// Any frame carried unresolved foreign workdir evidence; reused chunks
    /// re-stamp it so the index lane falls back to the census lane, which
    /// applies the do-not-inherit filter per frame.
    #[serde(default)]
    scope_unattributed: bool,
    /// Subagent provenance (e.g. `subagent:guardian`); reused chunks re-stamp
    /// it so the intents index lane can exclude control-plane sessions.
    #[serde(default)]
    session_kind: Option<String>,
    /// Identity of the FILESYSTEM the scope verdicts above were computed
    /// against (see [`scope_environment_fingerprint`]).
    ///
    /// Those verdicts resolve repository identity on this host, so they can
    /// go stale while the source bytes and the catalog row stay byte-identical
    /// — a nested checkout created or removed, a `.gitmodules` edited. Old
    /// ledgers deserialize to empty, which never matches and so never reuses.
    #[serde(default)]
    scope_environment: String,
    /// Distinct cwds observed in this session's frames, which is what the
    /// fingerprint above is recomputed from at reuse time.
    #[serde(default)]
    scope_cwds: Vec<String>,
}

/// Identity of the repository layout that a session's scope verdicts depend on.
///
/// Scope resolution reads the local filesystem: which `.git` ancestor a path
/// has, how symlinks resolve, what `.gitmodules` declares. The reuse gate
/// otherwise keys only on source bytes and catalog fields, so creating or
/// removing a nested checkout — or editing `.gitmodules` — left a cached
/// extract servable under verdicts the current filesystem no longer supports.
///
/// Deliberately bounded to what can be recomputed without re-parsing the
/// source: the baseline's repo root, the submodule declarations at that root,
/// and how each recorded frame cwd resolves today. A change the parser would
/// see but this cannot is a re-parse away in any case, because it takes a
/// source change to produce one.
fn scope_environment_fingerprint(baseline: Option<&str>, scope_cwds: &[String]) -> String {
    use aicx_parser::engine::{WorkdirIdentity, normalize_workdir};

    /// How one path resolves today, as bytes, plus the repo root when it has
    /// one.
    fn resolution(value: &str) -> (Vec<u8>, Option<std::path::PathBuf>) {
        match normalize_workdir(value, None) {
            WorkdirIdentity::Resolved(root) => {
                let mut bytes = b"resolved\0".to_vec();
                bytes.extend_from_slice(root.to_string_lossy().as_bytes());
                bytes.push(0);
                (bytes, Some(root))
            }
            WorkdirIdentity::Unresolved(path) => {
                let mut bytes = b"unresolved\0".to_vec();
                bytes.extend_from_slice(path.as_bytes());
                bytes.push(0);
                (bytes, None)
            }
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(b"aicx.scope_environment.v1\0");

    match baseline.map(str::trim).filter(|value| !value.is_empty()) {
        Some(base) => {
            let (bytes, root) = resolution(base);
            hasher.update(&bytes);
            if let Some(root) = root {
                // Submodule declarations decide whether a vanished path is a
                // repository of its own, so their content is part of the
                // environment, not merely their presence.
                match fs::read(root.join(".gitmodules")) {
                    Ok(body) => {
                        hasher.update(b"gitmodules\0");
                        hasher.update(sha256_hex(&body).as_bytes());
                    }
                    Err(_) => hasher.update(b"no-gitmodules\0"),
                }
            }
        }
        None => hasher.update(b"no-baseline\0"),
    }

    let mut cwds: Vec<&str> = scope_cwds.iter().map(String::as_str).collect();
    cwds.sort_unstable();
    cwds.dedup();
    for cwd in cwds {
        hasher.update(b"cwd\0");
        hasher.update(cwd.as_bytes());
        hasher.update([0]);
        hasher.update(&resolution(cwd).0);
    }
    hex::encode(hasher.finalize())
}

/// Build or preview the global lexical index from the durable catalog.
///
/// Incremental truth is bounded by catalog rows that carry live source
/// fingerprints (size + mtime-ns). When the catalog snapshot and every
/// selected source fingerprint match the CURRENT generation, reuse is free.
/// A catalog rebuild that re-stats sources admits appends/edits; per-session
/// parse state reuses only extracts whose source fingerprint still matches.
/// Silent form of [`build_with_reporter`] for callers with no terminal (MCP
/// auto-refresh, scheduler): progress goes nowhere, the report is identical.
pub fn build(
    aicx_home: &Path,
    project_filters: &[String],
    dry_run: bool,
    full_rescan: bool,
    semantic: bool,
) -> Result<SourceIndexReport> {
    build_with_reporter(
        aicx_home,
        project_filters,
        dry_run,
        full_rescan,
        semantic,
        Arc::new(NoopReporter),
    )
}

/// Build the source-driven index and narrate it through `reporter`: an
/// `index_parse` phase ticking once per cataloged source (heartbeat keeps a
/// slow parse visibly alive) and an `index_publish` phase for the generation
/// flip. A full pass over ~14k sessions is a six-to-twelve minute job; before
/// this the command printed nothing between the mutation note and the summary.
///
/// Extracts and the reuse ledger are always written: without the ledger every
/// run re-parsed the whole catalog (`reused=0`), which is what made `index`
/// feel broken on every laptop.
pub fn build_with_reporter(
    aicx_home: &Path,
    project_filters: &[String],
    dry_run: bool,
    full_rescan: bool,
    semantic: bool,
    reporter: Arc<dyn Reporter>,
) -> Result<SourceIndexReport> {
    let started = Instant::now();
    if !dry_run && !project_filters.is_empty() {
        anyhow::bail!(
            "project-scoped index publishing is retired; run `aicx index` once for the global \
             catalog, then filter queries with `aicx search -p <project>` (use `aicx index -p \
             <project> --dry-run` only to inspect a project slice)"
        );
    }
    if semantic && dry_run {
        anyhow::bail!(
            "`aicx index --semantic --dry-run` cannot preview dense embedding without writing; \
             drop --dry-run to embed, or run lexical `aicx index --dry-run` alone"
        );
    }
    let catalog_path = crate::catalog::sessions_path_for(aicx_home);
    let entries = crate::catalog::read_entries_at(aicx_home)?;
    if entries.is_empty() {
        anyhow::bail!(
            "session catalog is empty at {}; no agent sessions were discovered under the known \
             roots (see `aicx sources`)",
            catalog_path.display()
        );
    }

    let selected: Vec<CatalogEntry> = entries
        .into_iter()
        .filter(|entry| project_selected(entry.project.as_deref(), project_filters))
        .collect();
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let ignore = crate::legacy_archive::load_repo_path_ignore(aicx_home, &user_home)?;
    let repo_path_ignore_fingerprint = ignore.fingerprint();
    let source_allow = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home);
    // Digest includes LIVE size+mtime so appends without catalog rebuild still
    // move the generation fingerprint (source-change incremental).
    let source_fingerprint = source_fingerprint(
        aicx_home,
        &catalog_path,
        &selected,
        &source_allow,
        &repo_path_ignore_fingerprint,
    )?;
    // Incremental short-circuit applies to both publish and dry-run. A matching
    // live+catalog digest means CURRENT already reflects this snapshot —
    // re-parsing ~10k sources on every `index --dry-run` recreated the mill
    // latency the extracts-store cut was meant to kill. Use `--full-rescan` to
    // force a walk. Live source drift (append without catalog rebuild) moves
    // the digest so recent frames cannot stay invisible forever.
    //
    // `--semantic` refuses the lexical-only short-circuit when dense is absent
    // so operators can attach dense to an otherwise current corpus without
    // `--full-rescan`.
    let dense_missing = crate::vector_index::current_dense_not_built().unwrap_or(true);
    if !full_rescan
        && project_filters.is_empty()
        && crate::vector_index::source_lexical_generation_matches(&source_fingerprint)?
        && !(semantic && dense_missing)
    {
        let (dense_kind, dense_docs) = current_dense_stats();
        return Ok(SourceIndexReport {
            catalog_path: catalog_path.display().to_string(),
            sources_total: selected.len(),
            sources_parsed: 0,
            sources_reused: 0,
            sources_skipped: 0,
            raw_frames: 0,
            signal_frames: 0,
            filtered_frames: 0,
            extracts_written: 0,
            distill_docs: 0,
            lexical_docs: crate::vector_index::current_lexical_doc_count()?.unwrap_or(0),
            dense_docs,
            dense_kind,
            semantic_requested: semantic,
            unchanged: true,
            wall_ms: started.elapsed().as_millis() as u64,
            manifest_path: crate::vector_index::hybrid_manifest_path(None)
                .ok()
                .map(|path| path.display().to_string()),
            skipped_by_agent: BTreeMap::new(),
        });
    }

    let mut chunks = Vec::with_capacity(selected.len());
    let mut sources_parsed = 0usize;
    let mut sources_reused = 0usize;
    let mut sources_skipped = 0usize;
    let mut raw_frames = 0usize;
    let mut signal_frames = 0usize;
    let mut filtered_frames = 0usize;
    let mut extracts_written = 0usize;
    let mut distill_docs = 0usize;
    let mut skipped_by_agent = BTreeMap::new();
    let mut next_state = SourceParseState {
        schema: PARSE_STATE_SCHEMA.to_string(),
        signal_filter_version: SIGNAL_FILTER_VERSION.to_string(),
        repo_path_ignore_fingerprint: repo_path_ignore_fingerprint.clone(),
        sessions: BTreeMap::new(),
    };

    let prior_state = if full_rescan {
        SourceParseState::default()
    } else {
        load_parse_state(aicx_home, &repo_path_ignore_fingerprint)
    };

    let parse_phase = Phase::start(reporter.clone(), "index_parse", Some(selected.len() as u64));
    let parse_hb = Heartbeat::spawn_with_backoff(
        parse_phase.clone(),
        Duration::from_secs(2),
        Duration::from_secs(15),
    );
    for entry in &selected {
        parse_phase.tick((sources_parsed + sources_reused + sources_skipped) as u64);
        let session_key = session_state_key(&entry.agent, &entry.session_id);

        // True incremental: reuse a prior extract only when the source
        // fingerprint (path + size + mtime) still matches and extract bytes
        // have not been tampered with.
        if let Some(chunk) = try_reuse_cached_extract(
            aicx_home,
            entry,
            &prior_state,
            &session_key,
            &source_allow,
            &repo_path_ignore_fingerprint,
        ) {
            let record = prior_state
                .sessions
                .get(&session_key)
                .expect("reuse requires prior record");
            raw_frames += record.raw_frames;
            signal_frames += record.signal_frames;
            filtered_frames += record.filtered_frames;
            sources_reused += 1;
            next_state.sessions.insert(session_key, record.clone());
            chunks.push(chunk);
            continue;
        }

        // Resolve under approved roots before any parse/open.
        // Pass the catalog string through AsRef<Path> so Path::new lives only
        // inside the allowlist resolver (canonicalize + containment).
        let source_path = match source_allow.resolve_file(entry.source_path.as_str()) {
            Ok(path) => path,
            Err(error) => {
                crate::diagnostics::log_describe(&format!(
                    "source_index_skip agent={} session_id={} path={} error={error:#}",
                    entry.agent, entry.session_id, entry.source_path
                ));
                sources_skipped += 1;
                *skipped_by_agent.entry(entry.agent.clone()).or_default() += 1;
                continue;
            }
        };
        let parsed_source = match parse_catalog_source(entry, &source_path, &source_allow) {
            Ok(parsed) => parsed,
            Err(error) => {
                crate::diagnostics::log_describe(&format!(
                    "source_index_skip agent={} session_id={} path={} error={error:#}",
                    entry.agent,
                    entry.session_id,
                    source_path.display()
                ));
                sources_skipped += 1;
                *skipped_by_agent.entry(entry.agent.clone()).or_default() += 1;
                continue;
            }
        };
        let ParsedCatalogSource {
            mut frames,
            distill,
        } = parsed_source;
        sources_parsed += 1;
        let raw_count = frames.len();
        raw_frames += raw_count;
        frames.sort_by_key(|frame| frame.timestamp);
        let before = frames.len();
        frames.retain(is_signal_frame);
        for frame in &mut frames {
            frame.message = clean_message(&frame.message);
        }
        frames.retain(|frame| !frame.message.trim().is_empty());
        // Same one step as the intent lane: whatever `.aicxignore` hides is
        // counted into the report here, or this chunk's scope silently forgets
        // the session's own baseline.
        let scope = scope_report_excluding_ignored(&mut frames, &ignore);
        let signal_count = frames.len();
        signal_frames += signal_count;
        let filtered_count = before.saturating_sub(frames.len());
        filtered_frames += filtered_count;
        if frames.is_empty() {
            continue;
        }

        let extract = render_extract(entry, &frames);
        if extract.trim().is_empty() {
            continue;
        }
        let extract_path = extract_path_for(aicx_home, &entry.agent, &entry.session_id);
        if !dry_run && write_if_changed(aicx_home, &extract_path, extract.as_bytes())? {
            extracts_written += 1;
        }
        let indexed_path = if !dry_run {
            extract_path.clone()
        } else {
            source_path.to_path_buf()
        };
        let date = frames
            .last()
            .map(|frame| frame.timestamp.format("%Y-%m-%d").to_string())
            .or_else(|| entry.date.clone())
            .unwrap_or_default();
        let project = entry
            .project
            .clone()
            .unwrap_or_else(|| "_unknown".to_string());
        // Whole-session chunks cannot express per-frame scope, so this flag
        // routes the session to the per-frame census lane. It must mean
        // "more than one scope", not "MixedCandidate": a same-repo branch
        // switch is mixed by status and perfectly servable as one bucket.
        // It must also catch the homogeneous case: every frame re-scoped to
        // ONE foreign checkout is a consistent session that belongs to another
        // repository, and serving it whole would stamp all of it with this
        // catalog row's project.
        let session_mixed = scope.scope_foreign_to(entry.cwd.as_deref());
        let session_unattributed = frames.iter().any(|frame| frame.scope_unattributed);
        let scope_cwds: Vec<String> = scope.cwds.to_vec();
        // Frames carry the provenance resolved at the source, which is the
        // only lane that can see it for a catalog row cataloged before the
        // column existed.
        let resolved_kind = frames
            .iter()
            .find_map(|frame| frame.session_kind.clone())
            .or_else(|| entry.session_kind.clone());
        let mut metadata = serde_json::json!({
            "source_path": indexed_path.to_string_lossy(),
            "project": project,
            "agent": entry.agent,
            "date": date,
            "kind": "conversations",
            "session_id": entry.session_id,
            "logical_session_id": entry.logical_session_id,
            "frame_kind": "conversation",
            "cwd": entry.cwd,
            "source_catalog_path": entry.source_path,
            "preview_lines": extract_preview_lines(&frames),
            "scope_conflict": session_mixed,
            "scope_unattributed": session_unattributed,
            "session_kind": resolved_kind.clone(),
        });
        if let Some(distill) = &distill {
            // card.v3 (W2-02): distill block + flat filter scalars. Reused
            // cached extracts skip this branch, so their documents stay v2
            // until re-parsed — incremental coverage, reported not faked.
            distill.merge_into(&mut metadata);
            distill_docs += 1;
        }
        chunks.push(aicx_retrieve::ChunkRef {
            id: format!("{}:{}", entry.agent, entry.session_id),
            source_path: indexed_path.display().to_string(),
            text: extract.clone(),
            metadata,
        });

        let rel = extract_path
            .strip_prefix(aicx_home)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| extract_path.clone());
        // Always stamp LIVE stats into the parse ledger so reuse survives
        // catalog lag (append without catalog rebuild still reuses later
        // once CURRENT has the new extract).
        let (source_len, source_mtime_ns) = crate::catalog::live_source_fingerprint(&source_path)
            .unwrap_or_else(|| resolve_entry_fingerprint(entry, &source_path));
        next_state.sessions.insert(
            session_key,
            SessionParseRecord {
                source_path: entry.source_path.clone(),
                source_len,
                source_mtime_ns,
                extract_relpath: rel.to_string_lossy().replace('\\', "/"),
                extract_sha256: sha256_hex(extract.as_bytes()),
                raw_frames: raw_count,
                signal_frames: signal_count,
                filtered_frames: filtered_count,
                project: entry.project.clone(),
                date: entry.date.clone().or(Some(date)),
                cwd: entry.cwd.clone(),
                scope_conflict: session_mixed,
                scope_unattributed: session_unattributed,
                session_kind: resolved_kind.clone(),
                scope_environment: scope_environment_fingerprint(entry.cwd.as_deref(), &scope_cwds),
                scope_cwds,
            },
        );
    }
    parse_hb.stop();
    parse_phase.finish_ok(format!(
        "parsed={sources_parsed} reused={sources_reused} skipped={sources_skipped}"
    ));

    if chunks.is_empty() {
        anyhow::bail!(
            "source-driven index produced zero signal extracts from {} cataloged source(s)",
            selected.len()
        );
    }

    let publish_phase = Phase::start(reporter.clone(), "index_publish", None);
    let publish_hb = Heartbeat::spawn_with_backoff(
        publish_phase.clone(),
        Duration::from_secs(2),
        Duration::from_secs(15),
    );
    let (manifest_path, dense_docs, dense_kind) = if dry_run {
        (
            None,
            0usize,
            if semantic {
                "would_build_semantic".to_string()
            } else {
                "optional_not_built".to_string()
            },
        )
    } else if semantic {
        let (dense_chunks, fingerprint) = embed_chunks_for_semantic(&chunks)?;
        let manifest = crate::vector_index::publish_source_hybrid_generation(
            &chunks,
            &dense_chunks,
            &source_fingerprint,
            &fingerprint,
        )?;
        if project_filters.is_empty() {
            write_parse_state(aicx_home, &next_state)?;
        }
        let path = crate::vector_index::hybrid_manifest_path(None)?
            .display()
            .to_string();
        (
            Some(path).filter(|_| manifest.lexical_doc_count == chunks.len()),
            manifest.dense_count,
            manifest.dense_kind,
        )
    } else {
        let manifest =
            crate::vector_index::publish_source_lexical_generation(&chunks, &source_fingerprint)?;
        // Persist parse state only after a successful publish so a killed build
        // cannot claim sessions are current when CURRENT never flipped.
        if project_filters.is_empty() {
            write_parse_state(aicx_home, &next_state)?;
        }
        let path = crate::vector_index::hybrid_manifest_path(None)?
            .display()
            .to_string();
        (
            Some(path).filter(|_| manifest.lexical_doc_count == chunks.len()),
            0,
            "optional_not_built".to_string(),
        )
    };
    publish_hb.stop();
    publish_phase.finish_ok(format!(
        "lexical_docs={} dense_docs={dense_docs}{}",
        chunks.len(),
        if dry_run { " (dry run)" } else { "" }
    ));

    Ok(SourceIndexReport {
        catalog_path: catalog_path.display().to_string(),
        sources_total: selected.len(),
        sources_parsed,
        sources_reused,
        sources_skipped,
        raw_frames,
        signal_frames,
        filtered_frames,
        extracts_written,
        distill_docs,
        lexical_docs: chunks.len(),
        dense_docs,
        dense_kind,
        semantic_requested: semantic,
        unchanged: false,
        wall_ms: started.elapsed().as_millis() as u64,
        manifest_path,
        skipped_by_agent,
    })
}

fn current_dense_stats() -> (String, usize) {
    let Ok(path) = crate::vector_index::hybrid_manifest_path(None) else {
        return ("missing".to_string(), 0);
    };
    if !path.is_file() {
        return ("missing".to_string(), 0);
    }
    match aicx_retrieve::Manifest::read_from_path(&path) {
        Ok(m) => (m.dense_kind, m.dense_count),
        Err(_) => ("unreadable".to_string(), 0),
    }
}

/// Embed session extracts for `--semantic` using the configured cloud/native engine.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn embed_chunks_for_semantic(
    chunks: &[aicx_retrieve::ChunkRef],
) -> Result<(
    Vec<aicx_retrieve::DenseChunkRef>,
    aicx_retrieve::EmbedderFingerprint,
)> {
    let mut engine = crate::embedder::EmbeddingEngine::new().with_context(|| {
        "initialize embedder for `aicx index --semantic` (configure ~/.aicx/config.toml \
         [embedder.cloud] or native GGUF, then `aicx warmup`)"
            .to_string()
    })?;
    let info = engine.info().clone();
    let fingerprint = crate::vector_index::hybrid_embedder_fingerprint(&info);
    let batch_size = engine.embed_batch_size().max(1);
    let mut dense_chunks = Vec::with_capacity(chunks.len());
    let total = chunks.len();
    for (batch_idx, batch) in chunks.chunks(batch_size).enumerate() {
        let texts: Vec<String> = batch
            .iter()
            .map(|chunk| {
                // Bound embed payload: first ~8k chars keeps signal, avoids multi-MB HTTP.
                let text = chunk.text.as_str();
                if text.len() > 8_192 {
                    text.chars().take(8_192).collect()
                } else {
                    text.to_string()
                }
            })
            .collect();
        let vectors = engine.embed_batch(&texts).with_context(|| {
            format!(
                "embed batch {}/{} ({} texts) for index --semantic",
                batch_idx + 1,
                total.div_ceil(batch_size),
                texts.len()
            )
        })?;
        if vectors.len() != batch.len() {
            anyhow::bail!(
                "embedder returned {} vectors for {} texts in batch {}",
                vectors.len(),
                batch.len(),
                batch_idx + 1
            );
        }
        for (chunk, embedding) in batch.iter().zip(vectors) {
            if embedding.len() != fingerprint.dim {
                anyhow::bail!(
                    "embedder returned dim {} for chunk {}; config expects {}",
                    embedding.len(),
                    chunk.id,
                    fingerprint.dim
                );
            }
            dense_chunks.push(aicx_retrieve::DenseChunkRef {
                chunk: chunk.clone(),
                embedding,
            });
        }
        if batch_idx == 0 || (batch_idx + 1) % 10 == 0 || (batch_idx + 1) * batch_size >= total {
            eprintln!(
                "aicx index --semantic · embedded {}/{} session document(s)",
                dense_chunks.len(),
                total
            );
        }
    }
    Ok((dense_chunks, fingerprint))
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
fn embed_chunks_for_semantic(
    _chunks: &[aicx_retrieve::ChunkRef],
) -> Result<(
    Vec<aicx_retrieve::DenseChunkRef>,
    aicx_retrieve::EmbedderFingerprint,
)> {
    anyhow::bail!(
        "`aicx index --semantic` requires a build with `cloud-embedder` and/or `native-embedder` \
         features (default release builds include both)"
    )
}

/// Compare the durable catalog with the document identities actually committed
/// to CURRENT. This intentionally scans the exact id term dictionary rather
/// than trusting only the manifest count: the count alone cannot say which
/// extractor has holes.
pub fn current_search_coverage(aicx_home: &Path) -> SearchCoverage {
    let entries = match crate::catalog::read_entries_at(aicx_home) {
        Ok(entries) => entries,
        Err(_) => {
            return SearchCoverage {
                skipped: BTreeMap::from([("catalog_unreadable".to_string(), 1)]),
                ..SearchCoverage::default()
            };
        }
    };
    let total_sessions = entries.len();
    let indexed = match current_indexed_session_keys() {
        Ok(indexed) => indexed,
        Err(_) => {
            let mut skipped = BTreeMap::new();
            for entry in &entries {
                *skipped
                    .entry(format!(
                        "{}_index_unreadable",
                        coverage_agent_key(entry.agent.as_str())
                    ))
                    .or_default() += 1;
            }
            return SearchCoverage {
                scanned_sessions: 0,
                total_sessions,
                skipped,
            };
        }
    };
    let mut skipped = BTreeMap::new();
    let mut scanned_sessions = 0usize;

    for entry in &entries {
        if indexed.contains(&session_state_key(&entry.agent, &entry.session_id)) {
            scanned_sessions += 1;
        } else {
            *skipped
                .entry(format!(
                    "{}_unindexed",
                    coverage_agent_key(entry.agent.as_str())
                ))
                .or_default() += 1;
        }
    }
    let indexed_orphans = indexed.len().saturating_sub(scanned_sessions);
    if indexed_orphans > 0 {
        skipped.insert("index_orphaned".to_string(), indexed_orphans);
    }

    SearchCoverage {
        scanned_sessions,
        total_sessions,
        skipped,
    }
}

fn current_indexed_session_keys() -> Result<HashSet<String>> {
    let hybrid_root = crate::vector_index::hybrid_root_dir(None)?;
    let generation = crate::vector_index::resolve_hybrid_generation_dir(&hybrid_root);
    if !generation.join("manifest.json").is_file() {
        return Ok(HashSet::new());
    }
    let lexical = aicx_retrieve::TantivyAdapter::new(generation)?;
    Ok(lexical.scan_chunk_ids()?.into_iter().collect())
}

fn coverage_agent_key(agent: &str) -> String {
    let normalized: String = agent
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let normalized = normalized.trim_matches('_');
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized.to_string()
    }
}

/// Search every matching passage inside one catalog session.
///
/// Cached extracts are read first. When no cache exists, the source is parsed
/// through the same signal-only parser and renderer used by `aicx index`; this
/// fallback is read-only and never materializes a cache file.
pub fn search_session_passages(
    aicx_home: &Path,
    session: &str,
    query: &str,
    context: usize,
    literal: bool,
) -> Result<SessionPassageReport> {
    if query.trim().is_empty() {
        anyhow::bail!("session passage query must not be empty");
    }
    let entry = crate::catalog::resolve_session(aicx_home, session)?.ok_or_else(|| {
        anyhow::anyhow!(
            "session `{session}` not in durable catalog; run `aicx catalog rebuild` first"
        )
    })?;
    let document = read_session_document(aicx_home, &entry)?;
    let hit_lines = matching_line_numbers(&document.body, query, literal)?;
    let spans = merge_context_spans(&hit_lines, document.body.lines().count(), context);
    let lines: Vec<&str> = document.body.lines().collect();
    let source_path = document.source_path;
    let document_path = document.document_path.display().to_string();
    let passages = spans
        .into_iter()
        .enumerate()
        .map(|(index, (start, end, match_lines))| SessionPassage {
            passage: index + 1,
            line_span: LineSpan { start, end },
            match_lines,
            text: lines[start - 1..end].join("\n"),
            source_path: source_path.clone(),
            document_path: document_path.clone(),
        })
        .collect();

    Ok(SessionPassageReport {
        session_id: entry.session_id,
        agent: entry.agent.clone(),
        query: query.to_string(),
        mode: if literal { "literal" } else { "token" },
        context,
        cache_hit: document.cache_hit,
        passages,
        coverage: SearchCoverage::single_session(true, &entry.agent),
    })
}

fn read_session_document(aicx_home: &Path, entry: &CatalogEntry) -> Result<SessionDocument> {
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let ignore = crate::legacy_archive::load_repo_path_ignore(aicx_home, &user_home)?;
    let repo_path_ignore_fingerprint = ignore.fingerprint();
    let cache_path = extract_path_for(aicx_home, &entry.agent, &entry.session_id);
    let source_allow = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home);
    let prior_state = load_parse_state(aicx_home, &repo_path_ignore_fingerprint);
    let session_key = session_state_key(&entry.agent, &entry.session_id);
    if let Some(chunk) = try_reuse_cached_extract(
        aicx_home,
        entry,
        &prior_state,
        &session_key,
        &source_allow,
        &repo_path_ignore_fingerprint,
    ) {
        return Ok(SessionDocument {
            body: chunk.text,
            source_path: entry.source_path.clone(),
            document_path: cache_path,
            cache_hit: true,
        });
    }
    let source_path = source_allow
        .resolve_file(entry.source_path.as_str())
        .with_context(|| {
            format!(
                "resolve catalog source agent={} session_id={}",
                entry.agent, entry.session_id
            )
        })?;
    let mut frames = parse_catalog_source(entry, &source_path, &source_allow)?.frames;
    frames.sort_by_key(|frame| frame.timestamp);
    frames.retain(is_signal_frame);
    for frame in &mut frames {
        frame.message = clean_message(&frame.message);
    }
    frames.retain(|frame| !frame.message.trim().is_empty());
    let _ = scope_report_excluding_ignored(&mut frames, &ignore);
    let body = render_extract(entry, &frames);
    Ok(SessionDocument {
        body,
        source_path: entry.source_path.clone(),
        document_path: source_path,
        cache_hit: false,
    })
}

fn matching_line_numbers(body: &str, query: &str, literal: bool) -> Result<Vec<usize>> {
    if literal {
        return Ok(body
            .lines()
            .enumerate()
            .filter(|(_, line)| contains_boundary_literal(line, query))
            .map(|(index, _)| index + 1)
            .collect());
    }

    let query_tokens = lexical_tokens(query);
    if query_tokens.is_empty() {
        anyhow::bail!("token query must contain at least one letter or digit");
    }
    Ok(body
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let line_tokens: HashSet<String> = lexical_tokens(line).into_iter().collect();
            query_tokens.iter().any(|token| line_tokens.contains(token))
        })
        .map(|(index, _)| index + 1)
        .collect())
}

fn lexical_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

fn contains_boundary_literal(line: &str, query: &str) -> bool {
    let Some(first) = query.chars().next() else {
        return false;
    };
    let Some(last) = query.chars().next_back() else {
        return false;
    };
    line.match_indices(query).any(|(start, matched)| {
        let end = start + matched.len();
        let before = line[..start].chars().next_back();
        let after = line[end..].chars().next();
        (!is_identifier_char(first) || before.is_none_or(|ch| !is_identifier_char(ch)))
            && (!is_identifier_char(last) || after.is_none_or(|ch| !is_identifier_char(ch)))
    })
}

fn is_identifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn merge_context_spans(
    hit_lines: &[usize],
    total_lines: usize,
    context: usize,
) -> Vec<(usize, usize, Vec<usize>)> {
    let mut spans: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    for &line in hit_lines {
        let start = line.saturating_sub(context).max(1);
        let end = line.saturating_add(context).min(total_lines);
        if let Some((_, previous_end, previous_hits)) = spans.last_mut()
            && start <= previous_end.saturating_add(1)
        {
            *previous_end = (*previous_end).max(end);
            previous_hits.push(line);
        } else {
            spans.push((start, end, vec![line]));
        }
    }
    spans
}

/// One parsed catalog source: the signal timeline plus, when the source went
/// through the full session parser, the card.v3 distillate materialization
/// (W2-02). Signal-only fast paths (vibecrafted transcripts, oversized codex
/// rollouts) carry no model, so they stay v2-coverage — reported, not faked.
struct ParsedCatalogSource {
    frames: Vec<TimelineEntry>,
    distill: Option<crate::extraction::distill::materialize::IndexDistillate>,
}

fn parse_catalog_source(
    entry: &CatalogEntry,
    path: &Path,
    allow: &crate::source_path::SourceAllowlist,
) -> Result<ParsedCatalogSource> {
    // Canonicalize + prove containment under approved source roots before any open.
    let path = allow
        .resolve_file(path)
        .with_context(|| format!("resolve catalog source {}", path.display()))?;
    // Provenance is established where the source is opened — the catalog
    // column only caches it, and cold rows never get refreshed by a hot-window
    // pass. Bounded header read, and only for rows that lack the column.
    let session_kind =
        crate::sessions::resolve_session_kind(&entry.agent, entry.session_kind.as_deref(), &path);

    if entry.agent == "vibecrafted" {
        let body = allow
            .read_to_string(&path)
            .with_context(|| format!("read runtime transcript {}", path.display()))?;
        // Token-stream runtime_runs logs interleave thought fragments with
        // visible text. Indexing the raw body made search surface
        // `{"type":"thought","data":"The"}` spam over real operator answers.
        let message = vibecrafted_signal_body(&body);
        if message.trim().is_empty() {
            return Ok(ParsedCatalogSource {
                frames: Vec::new(),
                distill: None,
            });
        }
        let timestamp = fs::metadata(&path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(chrono::DateTime::<chrono::Utc>::from)
            .unwrap_or_else(chrono::Utc::now);
        return Ok(ParsedCatalogSource {
            distill: None,
            frames: vec![TimelineEntry {
                timestamp,
                agent: entry.agent.clone(),
                session_id: entry.session_id.clone(),
                role: "assistant".to_string(),
                message,
                frame_class: None,
                lineage_origin: None,
                frame_kind: Some(FrameKind::AgentReply),
                branch: None,
                cwd: entry.cwd.clone(),
                scope_conflict: false,
                scope_unattributed: false,
                scope_workdirs: Vec::new(),
                session_kind: session_kind.clone(),
                timestamp_source: Some("source_mtime".to_string()),
                source_path: Some(entry.source_path.clone()),
                source_sha256: None,
                source_line_span: None,
            }],
        });
    }

    let source_bytes = fs::metadata(&path)
        .with_context(|| format!("stat source {}", path.display()))?
        .len();
    if entry.agent == "codex" && source_bytes > MAX_FULL_PARSE_BYTES {
        return Ok(ParsedCatalogSource {
            frames: parse_large_codex_signal(entry, &path, allow, session_kind.as_deref())?,
            distill: None,
        });
    }
    if source_bytes > MAX_FULL_PARSE_BYTES {
        anyhow::bail!(
            "source is {} bytes (bounded full-parser limit is {} bytes)",
            source_bytes,
            MAX_FULL_PARSE_BYTES
        );
    }

    let agent = match entry.agent.as_str() {
        "claude" => aicx_parser::engine::AgentKind::Claude,
        "codex" => aicx_parser::engine::AgentKind::Codex,
        "cursor" => aicx_parser::engine::AgentKind::Cursor,
        "gemini" => aicx_parser::engine::AgentKind::Gemini,
        "grok" => aicx_parser::engine::AgentKind::Grok,
        "junie" => aicx_parser::engine::AgentKind::Junie,
        "kimi" => aicx_parser::engine::AgentKind::Kimi,
        other => anyhow::bail!("unsupported catalog agent `{other}`"),
    };
    let parsed = crate::parser_dispatch::parse_file(
        agent,
        &entry.session_id,
        entry.logical_session_id.clone(),
        &path,
    )?;
    let distillates = crate::extraction::distill::materialize::session_distillates(parsed.model());
    let distill = Some(crate::extraction::distill::materialize::index_metadata(
        &distillates,
    ));
    let mut frames = crate::output::timeline_entries_from_model(parsed.model());
    for frame in &mut frames {
        frame.session_kind = session_kind.clone();
    }
    Ok(ParsedCatalogSource { frames, distill })
}

/// Read one cataloged session through the same allowlisted, signal-only parser
/// used by the lexical index.
///
/// Intent retrieval uses this path directly instead of reconstructing evidence
/// from retired per-frame cards. The returned path is canonical and proven to
/// live under the operator allowlist.
/// The requested frames PLUS the scope report of the whole session.
///
/// Scope is a property of the session, not of one role: the kind filter drops
/// the opposite role, so a report computed after it can see at most half the
/// evidence. A session whose assistant turns ran in a foreign checkout while
/// its user turns carry the baseline would then look homogeneous to both
/// passes — unreported as mixed, and cwd-less frames inheriting the catalog
/// project on the strength of evidence that was filtered away.
///
/// `.aicxignore` narrows the same evidence one layer deeper, so the count of
/// scopes it hid travels with the report: an ignored BASELINE would otherwise
/// leave a foreign cwd looking like the session's only scope.
pub(crate) fn read_catalog_signal_with_scope_at(
    aicx_home: &Path,
    entry: &CatalogEntry,
    frame_kind: FrameKind,
) -> Result<(
    PathBuf,
    Vec<TimelineEntry>,
    crate::extraction::conversation::ScopeReport,
)> {
    // The report arrives already built, on the whole session and on the
    // evidence the privacy filter removed; there is nothing here to re-attach.
    let (source_path, mut frames, scope) = read_catalog_conversation_at(aicx_home, entry)?;
    frames.retain(|frame| frame_matches_kind(frame, frame_kind));
    Ok((source_path, frames, scope))
}

/// Read one cataloged session as the clean user/assistant conversation used by
/// current operator surfaces. System prompts, tool payloads, and thought
/// frames are removed before callers build previews or retrieval records.
pub(crate) fn read_catalog_conversation_at(
    aicx_home: &Path,
    entry: &CatalogEntry,
) -> Result<(
    PathBuf,
    Vec<TimelineEntry>,
    crate::extraction::conversation::ScopeReport,
)> {
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let source_allow = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home);
    let source_path = source_allow
        .resolve_file(entry.source_path.as_str())
        .with_context(|| {
            format!(
                "resolve catalog source agent={} session_id={}",
                entry.agent, entry.session_id
            )
        })?;
    let mut frames = parse_catalog_source(entry, &source_path, &source_allow)?.frames;
    frames.sort_by_key(|frame| frame.timestamp);
    frames.retain(is_signal_frame);
    for frame in &mut frames {
        frame.message = clean_message(&frame.message);
    }
    frames.retain(|frame| !frame.message.trim().is_empty());
    let ignore = crate::legacy_archive::load_repo_path_ignore(aicx_home, &user_home)?;
    let scope = scope_report_excluding_ignored(&mut frames, &ignore);
    Ok((source_path, frames, scope))
}

/// Drop frames whose cwd the operator hid, and report the session's scope.
///
/// The privacy filter and the scope report are deliberately ONE step. A report
/// built after the filter has already lost the hidden evidence, and a caller
/// that has to remember to re-attach it is a caller that can forget: that is
/// precisely how a session whose baseline is hidden comes back looking
/// homogeneous, letting its remaining frames inherit the cataloged project.
///
/// A frame is judged on every checkout its turn window touched, not only on
/// its `cwd`: the window's recorded workdirs are tested too. A conflict window
/// has no `cwd` to test at all, and a window absorbed into its baseline still
/// ran inside the paths it names — judging `cwd` alone published exactly the
/// denied checkout that made the window mixed.
///
/// The hidden repositories are counted, never named — scope honesty must not
/// re-publish a path `.aicxignore` exists to hide. Each denied path counts as
/// the checkout it resolves to here, so one repository reached through its
/// root and a subdirectory is one hidden scope, not two.
fn scope_report_excluding_ignored(
    frames: &mut Vec<TimelineEntry>,
    ignore: &crate::legacy_archive::RepoPathIgnoreMatcher,
) -> crate::extraction::conversation::ScopeReport {
    if ignore.is_empty() {
        return crate::extraction::conversation::scope_report_for_entries(frames);
    }
    let mut identities: BTreeMap<String, String> = BTreeMap::new();
    let mut hidden = std::collections::BTreeSet::new();
    frames.retain(|frame| {
        let denied: Vec<&str> = frame
            .cwd
            .as_deref()
            .into_iter()
            .chain(frame.scope_workdirs.iter().map(String::as_str))
            .map(str::trim)
            .filter(|path| !path.is_empty() && ignore.ignores_cwd(Some(path)))
            .collect();
        for path in &denied {
            let identity = identities
                .entry((*path).to_owned())
                .or_insert_with(|| normalize_workdir(path, None).scope_path());
            hidden.insert(identity.clone());
        }
        denied.is_empty()
    });
    let mut report = crate::extraction::conversation::scope_report_for_entries(frames);
    report.hidden_scopes = hidden.len();
    report
}

fn frame_matches_kind(frame: &TimelineEntry, requested: FrameKind) -> bool {
    frame.frame_kind.unwrap_or(match frame.role.as_str() {
        "user" => FrameKind::UserMsg,
        "assistant" => FrameKind::AgentReply,
        _ => FrameKind::SystemNote,
    }) == requested
}

/// Bounded signal-only reader for oversized Codex rollouts.
///
/// Historical rollouts can exceed hundreds of MB because tool results and
/// pasted artifacts share the JSONL. The full canonical projection pays for
/// all of that noise. This path drains over-cap records without allocating
/// them and deserializes only bounded message records.
fn parse_large_codex_signal(
    entry: &CatalogEntry,
    path: &Path,
    allow: &crate::source_path::SourceAllowlist,
    session_kind: Option<&str>,
) -> Result<Vec<TimelineEntry>> {
    // `path` is already resolve_file'd by the caller; open through the allowlist.
    let file = allow
        .open_file(path)
        .with_context(|| format!("open source {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut frames = Vec::new();
    let mut line_no = 0u64;
    // Turn-window effective scope: message frames buffer per window between
    // `turn_context` records; explicit tool-call workdirs collected inside the
    // window can re-scope the whole window (never per-frame flip-flop).
    let mut baseline_cwd = entry.cwd.clone();
    let mut window_frames: Vec<TimelineEntry> = Vec::new();
    let mut window_workdirs: Vec<WorkdirEvidence> = Vec::new();
    while let Some(record) = crate::sanitize::read_line_capped(&mut reader, MAX_JSONL_RECORD_BYTES)?
    {
        line_no += 1;
        if record.exceeded {
            // An over-cap record is drained, never parsed. When its visible
            // head says it was a tool-call envelope, the window MIGHT have
            // moved repos and this reader will never know: record unreadable
            // evidence so the window fails closed to unattributed instead of
            // silently keeping the baseline project.
            if aicx_parser::engine::truncated_record_is_tool_call(&record.line)
                && !window_workdirs.contains(&WorkdirEvidence::Opaque)
            {
                window_workdirs.push(WorkdirEvidence::Opaque);
            }
            continue;
        }
        if record.line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&record.line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) == Some("turn_context") {
            flush_scope_window(
                &mut window_frames,
                &mut window_workdirs,
                baseline_cwd.as_deref(),
                &mut frames,
            );
            if let Some(cwd) = value
                .get("payload")
                .and_then(|payload| payload.get("cwd"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
            {
                baseline_cwd = Some(cwd.to_string());
            }
            continue;
        }
        // Tool calls arrive in BOTH Codex envelopes: `response_item` and
        // `event_msg`. The full adapter accepts both (`push_tool_turn` is
        // reached from either), so a bounded reader that only looked at
        // `response_item` lost every workdir of an `event_msg`-shaped rollout
        // and left its foreign turns inheriting the baseline project.
        let record_type = value.get("type").and_then(serde_json::Value::as_str);
        if !matches!(record_type, Some("response_item") | Some("event_msg")) {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        let payload_type = payload.get("type").and_then(serde_json::Value::as_str);
        if is_tool_call_payload_type(payload_type) {
            for workdir in tool_call_workdirs(payload) {
                let evidence = WorkdirEvidence::Explicit(workdir);
                if !window_workdirs.contains(&evidence) {
                    window_workdirs.push(evidence);
                }
            }
            continue;
        }
        // Only `response_item.message` carries chat text in this reader; the
        // `event_msg` envelope is footprint (the dual-envelope rule the Codex
        // adapter applies) and would double every turn.
        if record_type != Some("response_item") || payload_type != Some("message") {
            continue;
        }
        let Some(role) = payload.get("role").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let frame_kind = match role {
            "user" => FrameKind::UserMsg,
            "assistant" => FrameKind::AgentReply,
            _ => continue,
        };
        let message = payload
            .get("content")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let kind = item.get("type")?.as_str()?;
                if !matches!(kind, "input_text" | "output_text" | "text") {
                    return None;
                }
                item.get("text")?.as_str()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if message.trim().is_empty() {
            continue;
        }
        let timestamp = value
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);
        window_frames.push(TimelineEntry {
            timestamp,
            agent: entry.agent.clone(),
            session_id: entry.session_id.clone(),
            role: role.to_string(),
            message,
            frame_class: None,
            lineage_origin: None,
            frame_kind: Some(frame_kind),
            branch: None,
            cwd: baseline_cwd.clone(),
            scope_conflict: false,
            scope_unattributed: false,
            scope_workdirs: Vec::new(),
            session_kind: session_kind.map(str::to_owned),
            timestamp_source: Some("record".to_string()),
            source_path: Some(entry.source_path.clone()),
            source_sha256: None,
            source_line_span: Some((line_no, line_no)),
        });
    }
    flush_scope_window(
        &mut window_frames,
        &mut window_workdirs,
        baseline_cwd.as_deref(),
        &mut frames,
    );
    Ok(frames)
}

/// Codex tool-call payload types that can carry an explicit `workdir`.
/// Mirrors the set the full adapter routes into `push_tool_turn`.
fn is_tool_call_payload_type(payload_type: Option<&str>) -> bool {
    matches!(
        payload_type,
        Some("function_call")
            | Some("custom_tool_call")
            | Some("tool_call")
            | Some("mcp_tool_call")
    )
}

/// Stamp a buffered turn window with its effective scope and drain it into the
/// session frame list: one consistent foreign repo re-scopes the whole window
/// (including messages before the first tool call); proven-divergent evidence
/// marks every frame conflicted, while unresolved foreign evidence leaves a
/// durable do-not-inherit mark on every frame of the window. Every frame also
/// carries the window's recorded workdirs, whatever the verdict, so the
/// `.aicxignore` filter judges each checkout the window touched.
fn flush_scope_window(
    window_frames: &mut Vec<TimelineEntry>,
    window_workdirs: &mut Vec<WorkdirEvidence>,
    baseline: Option<&str>,
    frames: &mut Vec<TimelineEntry>,
) {
    if !window_frames.is_empty() {
        let recorded: Vec<String> = window_workdirs
            .iter()
            .filter_map(WorkdirEvidence::path)
            .map(|workdir| recorded_workdir(workdir, baseline))
            .fold(Vec::new(), |mut acc, workdir| {
                if !acc.contains(&workdir) {
                    acc.push(workdir);
                }
                acc
            });
        for frame in window_frames.iter_mut() {
            frame.scope_workdirs.clone_from(&recorded);
        }
        let (scope, path) = effective_window_scope(window_workdirs, baseline);
        match scope {
            WindowScope::Consistent => {
                if let Some(path) = path {
                    for frame in window_frames.iter_mut() {
                        frame.cwd = Some(path.clone());
                    }
                }
            }
            WindowScope::Conflict => {
                for frame in window_frames.iter_mut() {
                    frame.cwd = None;
                    frame.scope_conflict = true;
                }
            }
            // Unresolved foreign evidence: durable do-not-inherit mark. The
            // baseline cwd stays for structure, but the frame never counts as
            // positive project evidence downstream.
            WindowScope::Unattributed => {
                for frame in window_frames.iter_mut() {
                    frame.scope_unattributed = true;
                }
            }
            WindowScope::Baseline => {}
        }
        frames.append(window_frames);
    }
    window_workdirs.clear();
}

fn is_signal_frame(frame: &TimelineEntry) -> bool {
    let signal_kind = match frame.frame_kind {
        Some(FrameKind::UserMsg | FrameKind::AgentReply) => true,
        Some(FrameKind::ToolCall | FrameKind::InternalThought | FrameKind::SystemNote) => false,
        None => matches!(frame.role.as_str(), "user" | "assistant"),
    };
    signal_kind
        && !crate::extraction::is_harness_injected_noise(&frame.role, &frame.message)
        && !looks_like_binary_payload(&frame.message)
}

/// Collapse a vibecrafted `runtime_runs/*/transcript.log` into indexable text.
///
/// Keeps visible `text` tokens and nested `agent_message` bodies; drops pure
/// `thought` token streams. Non-JSON lines (plain markdown transcripts) pass
/// through unchanged.
fn vibecrafted_signal_body(body: &str) -> String {
    let mut out = String::new();
    let mut saw_json_line = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            if !saw_json_line {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        };
        saw_json_line = true;
        let Some(ty) = value.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        match ty {
            "thought" => continue,
            "text" => {
                if let Some(data) = value.get("data").and_then(serde_json::Value::as_str) {
                    out.push_str(data);
                }
            }
            "item.completed" => {
                if let Some(item) = value.get("item") {
                    let item_ty = item.get("type").and_then(serde_json::Value::as_str);
                    if matches!(item_ty, Some("agent_message") | Some("message"))
                        && let Some(text) = item.get("text").and_then(serde_json::Value::as_str)
                    {
                        if !out.is_empty() && !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push_str(text);
                        out.push('\n');
                    }
                }
            }
            "agent_message" | "message" => {
                if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(text);
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    clean_message(&out)
}

fn looks_like_binary_payload(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    if lower.contains("data:image/") && lower.contains(";base64,") {
        return true;
    }
    message
        .split_whitespace()
        .any(|token| token.chars().count() > MAX_UNBROKEN_TOKEN_CHARS)
}

fn clean_message(message: &str) -> String {
    let message = strip_known_harness_blocks(message);
    let mut cleaned = String::new();
    for line in message.lines() {
        if line.chars().count() > MAX_UNBROKEN_TOKEN_CHARS
            || (line.to_ascii_lowercase().contains("base64")
                && line.chars().count() > MAX_UNBROKEN_TOKEN_CHARS / 2)
        {
            continue;
        }
        if cleaned.chars().count() + line.chars().count() + 1 > MAX_MESSAGE_CHARS {
            cleaned.push_str("\n[message truncated by source index]\n");
            break;
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }
    cleaned.trim().to_string()
}

fn strip_known_harness_blocks(message: &str) -> String {
    let mut cleaned = message.to_string();
    for tag in ["user_info", "git_status"] {
        let opening = format!("<{tag}>");
        let closing = format!("</{tag}>");
        while let Some(start) = cleaned.find(&opening) {
            let content_start = start + opening.len();
            let Some(relative_end) = cleaned[content_start..].find(&closing) else {
                break;
            };
            let end = content_start + relative_end + closing.len();
            cleaned.replace_range(start..end, "");
        }
    }
    cleaned
}

fn render_extract(entry: &CatalogEntry, frames: &[TimelineEntry]) -> String {
    let mut out = format!(
        "# AICX session extract\n\n- session: `{}`\n- agent: `{}`\n- project: `{}`\n- source: `{}`\n\n",
        entry.session_id,
        entry.agent,
        entry.project.as_deref().unwrap_or("_unknown"),
        entry.source_path
    );
    for frame in frames {
        let role = if frame.role == "user" {
            "user"
        } else {
            "assistant"
        };
        let header = format!(
            "## {} · {}\n\n",
            frame.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
            role
        );
        if out.chars().count() + header.chars().count() + frame.message.chars().count()
            > MAX_EXTRACT_CHARS
        {
            out.push_str("\n[session extract truncated by source index]\n");
            break;
        }
        out.push_str(&header);
        out.push_str(frame.message.trim());
        out.push_str("\n\n");
    }
    out
}

fn extract_preview_lines(frames: &[TimelineEntry]) -> Vec<String> {
    frames
        .iter()
        .flat_map(|frame| frame.message.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(6)
        .map(|line| line.chars().take(240).collect())
        .collect()
}

fn project_selected(project: Option<&str>, filters: &[String]) -> bool {
    filters.is_empty()
        || project.is_some_and(|project| {
            filters
                .iter()
                .any(|filter| project.eq_ignore_ascii_case(filter))
        })
}

fn session_state_key(agent: &str, session_id: &str) -> String {
    format!("{agent}:{session_id}")
}

fn parse_state_path(aicx_home: &Path) -> PathBuf {
    aicx_home.join(PARSE_STATE_RELPATH)
}

fn load_parse_state(aicx_home: &Path, repo_path_ignore_fingerprint: &str) -> SourceParseState {
    let path = parse_state_path(aicx_home);
    if !path.is_file() {
        return SourceParseState::default();
    }
    let Ok(raw) = crate::source_path::read_under_aicx_home(aicx_home, &path) else {
        return SourceParseState::default();
    };
    let Ok(state) = serde_json::from_str::<SourceParseState>(&raw) else {
        return SourceParseState::default();
    };
    if state.schema != PARSE_STATE_SCHEMA
        || state.signal_filter_version != SIGNAL_FILTER_VERSION
        || state.repo_path_ignore_fingerprint != repo_path_ignore_fingerprint
    {
        return SourceParseState::default();
    }
    state
}

fn write_parse_state(aicx_home: &Path, state: &SourceParseState) -> Result<()> {
    let path = parse_state_path(aicx_home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parse-state dir {}", parent.display()))?;
    }
    let body = serde_json::to_vec_pretty(state).context("serialize source parse state")?;
    let write_path = crate::sanitize::validate_write_path(&path)
        .with_context(|| format!("validate parse-state path {}", path.display()))?;
    let tmp = write_path.with_extension("json.tmp");
    let tmp_write = crate::sanitize::validate_write_path(&tmp)
        .with_context(|| format!("validate parse-state tmp {}", tmp.display()))?;
    fs::write(&tmp_write, body)
        .with_context(|| format!("write parse-state tmp {}", tmp_write.display()))?;
    fs::rename(&tmp_write, &write_path).with_context(|| {
        format!(
            "publish parse-state {} -> {}",
            tmp_write.display(),
            write_path.display()
        )
    })?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn try_reuse_cached_extract(
    aicx_home: &Path,
    entry: &CatalogEntry,
    prior: &SourceParseState,
    session_key: &str,
    source_allow: &crate::source_path::SourceAllowlist,
    repo_path_ignore_fingerprint: &str,
) -> Option<aicx_retrieve::ChunkRef> {
    if prior.signal_filter_version != SIGNAL_FILTER_VERSION
        || prior.repo_path_ignore_fingerprint != repo_path_ignore_fingerprint
    {
        return None;
    }
    let record = prior.sessions.get(session_key)?;
    if record.source_path != entry.source_path {
        return None;
    }
    // The stored scope verdicts were computed against the catalog cwd of the
    // parse that produced them, and this path re-stamps them verbatim. An
    // unchanged source under a MOVED catalog cwd is a different scope
    // question, so reusing those flags would serve a now-foreign session
    // unflagged and bypass per-frame filtering entirely.
    if record.cwd != entry.cwd {
        return None;
    }
    // The verdicts also depend on the repository layout on this host, which
    // can change without the source or the catalog row changing at all.
    if record.scope_environment
        != scope_environment_fingerprint(entry.cwd.as_deref(), &record.scope_cwds)
    {
        return None;
    }
    // Zeroed legacy records (pre-fingerprint schema) never reuse.
    if record.source_len == 0 || record.source_mtime_ns == 0 {
        return None;
    }
    // LIVE source fingerprint is the reuse gate. Catalog-embedded size/mtime
    // lag until rebuild; requiring them to match the ledger forced full
    // reparse after a source-change index that already stamped live stats.
    if let Ok(live_path) = source_allow.resolve_file(entry.source_path.as_str()) {
        let (live_len, live_mtime) = crate::catalog::live_source_fingerprint(&live_path)?;
        if live_len != record.source_len || live_mtime != record.source_mtime_ns {
            return None;
        }
    } else {
        // Unreadable path: fall back to catalog-admitted fields only.
        if entry.source_len != Some(record.source_len)
            || entry.source_mtime_ns != Some(record.source_mtime_ns)
        {
            return None;
        }
    }
    let extract_path = aicx_home.join(&record.extract_relpath);
    let body = crate::source_path::read_under_aicx_home(aicx_home, &extract_path).ok()?;
    if sha256_hex(body.as_bytes()) != record.extract_sha256 {
        return None;
    }
    if body.trim().is_empty() {
        return None;
    }
    let project = entry
        .project
        .clone()
        .or_else(|| record.project.clone())
        .unwrap_or_else(|| "_unknown".to_string());
    let date = entry
        .date
        .clone()
        .or_else(|| record.date.clone())
        .unwrap_or_default();
    let preview_lines: Vec<String> = body
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .take(6)
        .map(str::to_string)
        .collect();
    let metadata = serde_json::json!({
        "source_path": extract_path.to_string_lossy(),
        "project": project,
        "agent": entry.agent,
        "date": date,
        "kind": "conversations",
        "session_id": entry.session_id,
        "frame_kind": "conversation",
        "cwd": entry.cwd.clone().or_else(|| record.cwd.clone()),
        "source_catalog_path": entry.source_path,
        "preview_lines": preview_lines,
        "incremental_reuse": true,
        "scope_conflict": record.scope_conflict,
        "scope_unattributed": record.scope_unattributed,
        "session_kind": record.session_kind.clone().or_else(|| entry.session_kind.clone()),
    });
    Some(aicx_retrieve::ChunkRef {
        id: format!("{}:{}", entry.agent, entry.session_id),
        source_path: extract_path.display().to_string(),
        text: body,
        metadata,
    })
}

fn source_fingerprint(
    aicx_home: &Path,
    catalog_path: &Path,
    entries: &[CatalogEntry],
    source_allow: &crate::source_path::SourceAllowlist,
    repo_path_ignore_fingerprint: &str,
) -> Result<String> {
    let mut hasher = Sha256::new();
    // Filter generation first so a catalog-identical CURRENT cannot hide a
    // pre-filter corpus after signal-body rules change.
    hasher.update(SIGNAL_FILTER_VERSION.as_bytes());
    hasher.update([0]);
    // Privacy rules are part of corpus identity. A rule edit must invalidate
    // CURRENT even when the catalog and every source file are unchanged.
    hasher.update(repo_path_ignore_fingerprint.as_bytes());
    hasher.update([0]);
    // Catalog membership (session ids + project attribution) is part of the
    // digest so new rows always admit a rebuild even when live stats match.
    hasher.update(
        crate::source_path::read_bytes_under_aicx_home(aicx_home, catalog_path)
            .with_context(|| format!("read catalog {}", catalog_path.display()))?,
    );
    // Per-source live size+mtime is the change detector. Catalog-embedded
    // fingerprints lag until rebuild; hashing LIVE stats means an append to
    // an existing JSONL moves the generation digest without inventing a new
    // session id (audit P0: source-CHANGE incremental, not only session-ADD).
    for entry in entries {
        hasher.update(entry.agent.as_bytes());
        hasher.update([0]);
        hasher.update(entry.session_id.as_bytes());
        hasher.update([0]);
        hasher.update(entry.source_path.as_bytes());
        hasher.update([0]);
        let (len, mtime) = live_or_catalog_fingerprint(entry, source_allow);
        hasher.update(len.to_le_bytes());
        hasher.update(mtime.to_le_bytes());
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Prefer live size+mtime; fall back to catalog-admitted values when the file
/// is temporarily unreadable (quarantine races, transient IO).
fn live_or_catalog_fingerprint(
    entry: &CatalogEntry,
    source_allow: &crate::source_path::SourceAllowlist,
) -> (u64, u64) {
    if let Ok(path) = source_allow.resolve_file(entry.source_path.as_str())
        && let Some(live) = crate::catalog::live_source_fingerprint(&path)
    {
        return live;
    }
    (
        entry.source_len.unwrap_or(0),
        entry.source_mtime_ns.unwrap_or(0),
    )
}

fn resolve_entry_fingerprint(entry: &CatalogEntry, source_path: &Path) -> (u64, u64) {
    if let (Some(len), Some(mtime)) = (entry.source_len, entry.source_mtime_ns) {
        return (len, mtime);
    }
    crate::catalog::live_source_fingerprint(source_path).unwrap_or((0, 0))
}

fn extract_path_for(aicx_home: &Path, agent: &str, session_id: &str) -> PathBuf {
    let mut safe: String = session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    safe = safe.trim_matches(['.', '_']).to_string();
    if safe.is_empty() || safe.len() > 180 {
        let digest = Sha256::digest(session_id.as_bytes());
        safe = format!("session-{}", &hex::encode(digest)[..16]);
    }
    aicx_home
        .join("extracts")
        .join(agent)
        .join(format!("{safe}_conversation.md"))
}

fn write_if_changed(aicx_home: &Path, path: &Path, bytes: &[u8]) -> Result<bool> {
    // Extracts live under aicx_home/extracts — prove containment before any IO.
    let allow = crate::source_path::SourceAllowlist::from_roots([aicx_home.to_path_buf()]);
    if path.exists() {
        let existing = allow.read_bytes(path).ok();
        if existing.as_deref() == Some(bytes) {
            return Ok(false);
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("extract path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create extract dir {}", parent.display()))?;
    let tmp = path.with_extension("md.tmp");
    // Write path is derived solely from aicx_home + agent + sanitized session id
    // (see extract_path_for); validate the final destination under aicx_home.
    let write_path = crate::sanitize::validate_write_path(path)
        .with_context(|| format!("validate extract write path {}", path.display()))?;
    let tmp_write = crate::sanitize::validate_write_path(&tmp)
        .with_context(|| format!("validate extract tmp path {}", tmp.display()))?;
    fs::write(&tmp_write, bytes)
        .with_context(|| format!("write extract tmp {}", tmp_write.display()))?;
    fs::rename(&tmp_write, &write_path).with_context(|| {
        format!(
            "publish extract {} -> {}",
            tmp_write.display(),
            write_path.display()
        )
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_project_slice_cannot_replace_the_global_generation() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-project-safety-{}",
            std::process::id()
        ));
        let error = build(
            &root,
            &["vetcoders/vibecrafted".to_string()],
            false,
            false,
            false,
        )
        .expect_err("project-scoped publish must fail before touching the index");

        assert!(
            error
                .to_string()
                .contains("project-scoped index publishing is retired")
        );
    }

    #[test]
    fn vibecrafted_signal_body_drops_thought_tokens_and_keeps_visible_text() {
        let raw = r#"{"type":"thought","data":"The"}
{"type":"thought","data":" user"}
{"type":"text","data":"I'll"}
{"type":"text","data":" start"}
{"type":"text","data":" with"}
{"type":"text","data":" catalog"}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"Routing strzałek is W2-B-4c."}}
"#;
        let cleaned = vibecrafted_signal_body(raw);
        assert!(
            cleaned.contains("I'll start with catalog"),
            "visible text tokens must reassemble; got {cleaned:?}"
        );
        assert!(
            cleaned.contains("Routing strzałek is W2-B-4c."),
            "agent_message bodies must survive; got {cleaned:?}"
        );
        assert!(
            !cleaned.contains("thought") && !cleaned.contains("\"data\":\"The\""),
            "thought token streams must not enter the index; got {cleaned:?}"
        );
    }

    #[test]
    fn vibecrafted_signal_body_keeps_plain_markdown_transcripts() {
        let raw = "# implement report\n\nRouting strzałek taby landed in W2-B-4c.\n";
        let cleaned = vibecrafted_signal_body(raw);
        assert!(cleaned.contains("Routing strzałek taby landed in W2-B-4c."));
    }

    #[test]
    fn clean_message_strips_workspace_bootstrap_blocks() {
        let raw = "<user_info>\nOS Version: macos\n</user_info>\n\
                   <git_status>\nM src/main.rs\n</git_status>\n\
                   Build the live continuity path.";
        let cleaned = clean_message(raw);
        assert_eq!(cleaned, "Build the live continuity path.");
    }

    #[test]
    fn signal_filter_version_is_non_empty_and_stable_for_this_cut() {
        // Guard against accidental empty version (would collapse fingerprints
        // across filter generations without meaning to).
        assert!(!SIGNAL_FILTER_VERSION.is_empty());
        assert!(SIGNAL_FILTER_VERSION.starts_with("signal-v"));
    }

    #[test]
    fn large_codex_signal_tracks_turn_context_cwd_per_frame() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-large-codex-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source_path = root.join("rollout.jsonl");
        let body = concat!(
            "{\"timestamp\":\"2026-08-22T00:00:00Z\",\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/repo/public\"}}\n",
            "{\"timestamp\":\"2026-08-22T00:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"public marker\"}]}}\n",
            "{\"timestamp\":\"2026-08-22T00:00:02Z\",\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/repo/private\"}}\n",
            "{\"timestamp\":\"2026-08-22T00:00:03Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"private marker\"}]}}\n"
        );
        fs::write(&source_path, body).unwrap();
        let entry = CatalogEntry {
            schema: crate::catalog::CATALOG_SCHEMA.to_string(),
            session_id: "large-codex".to_string(),
            agent: "codex".to_string(),
            project: Some("owner/repo".to_string()),
            date: Some("2026-08-22".to_string()),
            cwd: Some("/repo/initial".to_string()),
            source_path: source_path.display().to_string(),
            source_len: None,
            source_mtime_ns: None,
            title: None,
            machine: None,
            logical_session_id: None,
            session_kind: None,
        };
        let allow = crate::source_path::SourceAllowlist::from_roots([root.clone()]);

        let frames = parse_large_codex_signal(&entry, &source_path, &allow, None).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].cwd.as_deref(), Some("/repo/public"));
        assert_eq!(frames[1].cwd.as_deref(), Some("/repo/private"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn source_fingerprint_changes_with_checkout_deny_list() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-ignore-fingerprint-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("catalog")).unwrap();
        let catalog_path = root.join("catalog/sessions.jsonl");
        fs::write(&catalog_path, "catalog snapshot\n").unwrap();
        let allow = crate::source_path::SourceAllowlist::from_roots([root.clone()]);

        let before = source_fingerprint(&root, &catalog_path, &[], &allow, "deny-a").unwrap();
        let after = source_fingerprint(&root, &catalog_path, &[], &allow, "deny-b").unwrap();
        assert_ne!(before, after);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn checkout_deny_list_drops_only_matching_frames() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-ignore-frames-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let aicx_home = root.join(".aicx");
        fs::create_dir_all(&aicx_home).unwrap();
        fs::write(
            aicx_home.join(crate::legacy_archive::AICX_IGNORE_FILENAME),
            "/repo/private\n",
        )
        .unwrap();
        let ignore = crate::legacy_archive::load_repo_path_ignore(&aicx_home, &root).unwrap();
        let frame = |message: &str, cwd: &str| TimelineEntry {
            timestamp: chrono::Utc::now(),
            agent: "codex".to_string(),
            session_id: "multi-root".to_string(),
            role: "user".to_string(),
            message: message.to_string(),
            frame_class: None,
            lineage_origin: None,
            frame_kind: Some(FrameKind::UserMsg),
            branch: None,
            cwd: Some(cwd.to_string()),
            scope_conflict: false,
            scope_unattributed: false,
            scope_workdirs: Vec::new(),
            session_kind: None,
            timestamp_source: Some("record".to_string()),
            source_path: None,
            source_sha256: None,
            source_line_span: None,
        };
        let mut frames = vec![
            frame("keep public", "/repo/public"),
            frame("drop private", "/repo/private/nested"),
            frame("keep public again", "/repo/public"),
        ];

        let report = scope_report_excluding_ignored(&mut frames, &ignore);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].message, "keep public");
        assert_eq!(frames[1].message, "keep public again");
        // The hidden checkout is counted, and its path is not in the report.
        assert_eq!(report.hidden_scopes, 1);
        assert_eq!(report.cwds, vec!["/repo/public".to_string()]);
        assert!(
            report.scope_mixed(),
            "a session is not homogeneous just because the other scope was hidden"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// A conflict window has no `cwd` to test, so the deny list must see the
    /// workdirs that made it a conflict. Two real checkouts, one of them in
    /// `.aicxignore`, both named by one turn window: the window's frames are
    /// dropped on BOTH the full-parse and the bounded path, and the hidden
    /// checkout is counted without being named.
    #[test]
    fn conflict_window_workdirs_reach_the_checkout_deny_list() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-ignore-conflict-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let repo_public = root.join("repo-public");
        let repo_private = root.join("repo-private");
        fs::create_dir_all(repo_public.join(".git")).unwrap();
        fs::create_dir_all(repo_private.join(".git")).unwrap();
        let aicx_home = root.join(".aicx");
        fs::create_dir_all(&aicx_home).unwrap();
        fs::write(
            aicx_home.join(crate::legacy_archive::AICX_IGNORE_FILENAME),
            format!("{}\n", repo_private.display()),
        )
        .unwrap();
        let ignore = crate::legacy_archive::load_repo_path_ignore(&aicx_home, &root).unwrap();

        let tool_call = |call_id: &str, workdir: &Path| {
            serde_json::json!({
                "timestamp": "2026-08-22T00:00:04Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "call_id": call_id,
                    "arguments": serde_json::json!({
                        "cmd": "ls",
                        "workdir": workdir.display().to_string(),
                    })
                    .to_string(),
                },
            })
        };
        let body = jsonl(&[
            serde_json::json!({"timestamp": "2026-08-22T00:00:00Z", "type": "session_meta",
                "payload": {"id": "conflict-deny", "cwd": "/sessions/vista"}}),
            serde_json::json!({"timestamp": "2026-08-22T00:00:01Z", "type": "turn_context",
                "payload": {"cwd": "/sessions/vista"}}),
            serde_json::json!({"timestamp": "2026-08-22T00:00:02Z", "type": "response_item",
                "payload": {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "baseline question"}]}}),
            serde_json::json!({"timestamp": "2026-08-22T00:00:03Z", "type": "turn_context",
                "payload": {"cwd": "/sessions/vista"}}),
            tool_call("c1", &repo_public),
            tool_call("c2", &repo_private),
            serde_json::json!({"timestamp": "2026-08-22T00:00:06Z", "type": "response_item",
                "payload": {"type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "private checkout secret"}]}}),
        ]);
        let source_path = root.join("rollout.jsonl");
        fs::write(&source_path, body).unwrap();
        let mut entry = bounded_entry(&source_path);
        entry.session_id = "conflict-deny".to_string();
        entry.cwd = Some("/sessions/vista".to_string());
        let allow = crate::source_path::SourceAllowlist::from_roots([root.clone()]);

        let full = parse_catalog_source(&entry, &source_path, &allow)
            .unwrap()
            .frames;
        let bounded = parse_large_codex_signal(&entry, &source_path, &allow, None).unwrap();
        for (label, mut frames) in [("full", full), ("bounded", bounded)] {
            assert!(
                frames
                    .iter()
                    .any(|frame| frame.message.contains("private checkout secret")),
                "{label}: the fixture must actually produce the conflict frame"
            );
            let report = scope_report_excluding_ignored(&mut frames, &ignore);
            assert!(
                frames
                    .iter()
                    .all(|frame| !frame.message.contains("private checkout secret")),
                "{label}: a frame whose window ran in a denied checkout survived"
            );
            assert!(
                frames
                    .iter()
                    .any(|frame| frame.message.contains("baseline question")),
                "{label}: frames outside the denied window must survive"
            );
            assert_eq!(report.hidden_scopes, 1, "{label}");
            let rendered = format!("{report:?}");
            assert!(
                !rendered.contains("repo-private"),
                "{label}: the hidden checkout must be counted, never named: {rendered}"
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_state_reuse_requires_matching_source_path_and_extract_hash() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-reuse-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("extracts/claude")).unwrap();
        let extract_rel = "extracts/claude/session_conversation.md";
        let extract_path = root.join(extract_rel);
        let body = "# claude session\n\nuser: hello routing\nassistant: arrows vc-frame landed\n";
        fs::write(&extract_path, body).unwrap();

        // Real source file under a root the allowlist accepts (HOME + aicx_home).
        let source_path = root
            .join(".claude")
            .join("projects")
            .join("x")
            .join("session.jsonl");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "{\"type\":\"user\",\"text\":\"hello\"}\n").unwrap();
        let (source_len, source_mtime_ns) =
            crate::catalog::live_source_fingerprint(&source_path).expect("source fingerprint");

        let entry = CatalogEntry {
            schema: crate::catalog::CATALOG_SCHEMA.to_string(),
            session_id: "session".to_string(),
            agent: "claude".to_string(),
            project: Some("vetcoders/vibecrafted".to_string()),
            date: Some("2026-07-22".to_string()),
            cwd: Some("/tmp/work".to_string()),
            source_path: source_path.display().to_string(),
            source_len: Some(source_len),
            source_mtime_ns: Some(source_mtime_ns),
            title: Some("routing".to_string()),
            machine: None,
            logical_session_id: None,
            session_kind: None,
        };
        let mut prior = SourceParseState {
            schema: PARSE_STATE_SCHEMA.to_string(),
            signal_filter_version: SIGNAL_FILTER_VERSION.to_string(),
            repo_path_ignore_fingerprint: "ignore-fingerprint".to_string(),
            sessions: BTreeMap::new(),
        };
        prior.sessions.insert(
            session_state_key("claude", "session"),
            SessionParseRecord {
                source_path: entry.source_path.clone(),
                source_len,
                source_mtime_ns,
                extract_relpath: extract_rel.to_string(),
                extract_sha256: sha256_hex(body.as_bytes()),
                raw_frames: 4,
                signal_frames: 2,
                filtered_frames: 2,
                project: entry.project.clone(),
                date: entry.date.clone(),
                cwd: entry.cwd.clone(),
                scope_conflict: false,
                scope_unattributed: false,
                session_kind: None,
                scope_environment: scope_environment_fingerprint(entry.cwd.as_deref(), &[]),
                scope_cwds: Vec::new(),
            },
        );

        let key = session_state_key("claude", "session");
        // Allowlist roots: treat test root as both HOME and aicx_home.
        let allow = crate::source_path::SourceAllowlist::for_operator(&root, &root);
        let reused =
            try_reuse_cached_extract(&root, &entry, &prior, &key, &allow, "ignore-fingerprint")
                .expect("matching source fingerprint+hash must reuse");
        assert!(reused.text.contains("arrows vc-frame"));
        assert_eq!(reused.id, "claude:session");

        // The cached record carries scope verdicts computed against the
        // catalog cwd of its parse. A row whose cwd MOVED is a different
        // scope question: re-stamping the old flags would serve a now-foreign
        // session unflagged, straight past per-frame filtering.
        let mut rescoped = entry.clone();
        rescoped.cwd = Some("/repos/fleet-bus".to_string());
        assert!(
            try_reuse_cached_extract(&root, &rescoped, &prior, &key, &allow, "ignore-fingerprint")
                .is_none(),
            "a moved catalog cwd must force a reparse, not reuse stale scope flags"
        );

        // The verdicts also depend on the repository layout on this host,
        // which moves without the source or the catalog row moving at all.
        // An old ledger states no layout, which is not the current one.
        let mut stale_layout = prior.clone();
        stale_layout
            .sessions
            .get_mut(&key)
            .unwrap()
            .scope_environment = String::new();
        assert!(
            try_reuse_cached_extract(
                &root,
                &entry,
                &stale_layout,
                &key,
                &allow,
                "ignore-fingerprint"
            )
            .is_none(),
            "a cached extract whose layout identity no longer matches must reparse"
        );

        // Source path drift invalidates reuse.
        let mut drifted = entry.clone();
        drifted.source_path = "/elsewhere/session.jsonl".to_string();
        assert!(
            try_reuse_cached_extract(&root, &drifted, &prior, &key, &allow, "ignore-fingerprint")
                .is_none()
        );

        // Catalog-only size drift must NOT invalidate when live file is unchanged
        // (catalog lag after a live-stamped reparse is expected).
        let mut catalog_lag = entry.clone();
        catalog_lag.source_len = Some(source_len + 64);
        assert!(
            try_reuse_cached_extract(
                &root,
                &catalog_lag,
                &prior,
                &key,
                &allow,
                "ignore-fingerprint"
            )
            .is_some(),
            "stale catalog size alone must not force reparse when live matches"
        );

        // Live source growth (append) invalidates reuse.
        fs::write(
            &source_path,
            "{\"type\":\"user\",\"text\":\"hello\"}\n{\"type\":\"user\",\"text\":\"more\"}\n",
        )
        .unwrap();
        // Coarse FS mtime: touch content size always changes here.
        assert!(
            try_reuse_cached_extract(&root, &entry, &prior, &key, &allow, "ignore-fingerprint")
                .is_none(),
            "live append must invalidate reuse"
        );
        // Restore live bytes so later checks use the original fingerprint.
        fs::write(&source_path, "{\"type\":\"user\",\"text\":\"hello\"}\n").unwrap();
        // mtime may have moved; refresh ledger to match restored content.
        let (restored_len, restored_mtime) =
            crate::catalog::live_source_fingerprint(&source_path).expect("restored fp");
        prior.sessions.get_mut(&key).unwrap().source_len = restored_len;
        prior.sessions.get_mut(&key).unwrap().source_mtime_ns = restored_mtime;

        // Zeroed legacy records never reuse (forces reparse after upgrade).
        prior.sessions.get_mut(&key).unwrap().source_len = 0;
        assert!(
            try_reuse_cached_extract(&root, &entry, &prior, &key, &allow, "ignore-fingerprint")
                .is_none()
        );
        prior.sessions.get_mut(&key).unwrap().source_len = restored_len;

        assert!(
            try_reuse_cached_extract(
                &root,
                &entry,
                &prior,
                &key,
                &allow,
                "changed-ignore-fingerprint"
            )
            .is_none(),
            "deny-list drift must invalidate cached extracts"
        );

        // Corrupt extract bytes invalidate reuse.
        fs::write(&extract_path, "tampered").unwrap();
        assert!(
            try_reuse_cached_extract(&root, &entry, &prior, &key, &allow, "ignore-fingerprint")
                .is_none()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bounded_reader_stamps_turn_window_effective_scope() {
        let root = std::env::temp_dir().join(format!(
            "aicx-bounded-scope-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let fleet = root.join("fleet-bus");
        let other = root.join("other-repo");
        fs::create_dir_all(fleet.join(".git")).unwrap();
        fs::create_dir_all(other.join(".git")).unwrap();
        let source_path = root
            .join(".codex")
            .join("sessions")
            .join("2026")
            .join("rollout.jsonl");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        let template = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"s1","cwd":"/sessions/vista"}}
{"timestamp":"2026-01-01T00:01:00Z","type":"turn_context","payload":{"cwd":"/sessions/vista"}}
{"timestamp":"2026-01-01T00:01:10Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"vista opening"}]}}
{"timestamp":"2026-01-01T00:02:00Z","type":"turn_context","payload":{"cwd":"/sessions/vista"}}
{"timestamp":"2026-01-01T00:02:05Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Biore fleet task"}]}}
{"timestamp":"2026-01-01T00:02:10Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c1","input":"const r = await tools.exec_command({cmd:\"npm test\",\"workdir\":\"@FLEET@\"});"}}
{"timestamp":"2026-01-01T00:03:00Z","type":"turn_context","payload":{"cwd":"/sessions/vista"}}
{"timestamp":"2026-01-01T00:03:05Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c2","input":"const a = await tools.exec_command({cmd:\"ls\",\"workdir\":\"@FLEET@\"});"}}
{"timestamp":"2026-01-01T00:03:10Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c3","input":"const b = await tools.exec_command({cmd:\"ls\",\"workdir\":\"@OTHER@\"});"}}
{"timestamp":"2026-01-01T00:03:20Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"conflicted turn"}]}}
"#;
        let body = template
            .replace("@FLEET@", &json_path(&fleet))
            .replace("@OTHER@", &json_path(&other));
        fs::write(&source_path, body).unwrap();

        let entry = CatalogEntry {
            schema: crate::catalog::CATALOG_SCHEMA.to_string(),
            session_id: "s1".to_string(),
            agent: "codex".to_string(),
            project: Some("vista".to_string()),
            date: Some("2026-01-01".to_string()),
            cwd: Some("/sessions/vista".to_string()),
            source_path: source_path.display().to_string(),
            source_len: None,
            source_mtime_ns: None,
            title: None,
            machine: None,
            logical_session_id: None,
            session_kind: None,
        };
        let allow = crate::source_path::SourceAllowlist::for_operator(&root, &root);
        let resolved = allow.resolve_file(&source_path).expect("resolve rollout");
        let frames =
            parse_large_codex_signal(&entry, &resolved, &allow, None).expect("bounded parse");

        assert_eq!(frames.len(), 3, "{frames:?}");
        assert_eq!(frames[0].message, "vista opening");
        assert_eq!(frames[0].cwd.as_deref(), Some("/sessions/vista"));
        assert!(!frames[0].scope_conflict);
        assert_eq!(frames[1].message, "Biore fleet task");
        assert_eq!(
            frames[1].cwd.as_deref(),
            Some(canonical(&fleet).as_str()),
            "message before the first tool call shares the window scope"
        );
        assert!(!frames[1].scope_conflict);
        assert_eq!(frames[2].message, "conflicted turn");
        assert_eq!(frames[2].cwd, None);
        assert!(frames[2].scope_conflict);

        let _ = fs::remove_dir_all(&root);
    }

    /// Substitute a filesystem path into a JSONL fixture: a Windows path is
    /// `C:\Users\…`, and pasting it raw into a JSON string literal produces
    /// invalid escapes (`\U`), so the record silently fails to parse and the
    /// window loses the only workdir evidence it had.
    fn json_path(path: &Path) -> String {
        let quoted = serde_json::Value::String(path.display().to_string()).to_string();
        quoted[1..quoted.len() - 1].to_string()
    }

    /// Identities are canonical, so a scratch repo under a symlinked temp
    /// dir compares equal to the scope this reader stamps.
    fn canonical(path: &Path) -> String {
        fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    }

    fn bounded_scope_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aicx-bounded-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn bounded_entry(source_path: &Path) -> CatalogEntry {
        CatalogEntry {
            schema: crate::catalog::CATALOG_SCHEMA.to_string(),
            session_id: "s1".to_string(),
            agent: "codex".to_string(),
            project: Some("vista".to_string()),
            date: Some("2026-01-01".to_string()),
            cwd: Some("/sessions/vista".to_string()),
            source_path: source_path.display().to_string(),
            source_len: None,
            source_mtime_ns: None,
            title: None,
            machine: None,
            logical_session_id: None,
            session_kind: None,
        }
    }

    fn jsonl(rows: &[serde_json::Value]) -> String {
        rows.iter().fold(String::new(), |mut body, row| {
            body.push_str(&row.to_string());
            body.push('\n');
            body
        })
    }

    fn codex_meta(cwd: &str) -> serde_json::Value {
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00Z",
            "type": "session_meta",
            "payload": {"id": "s1", "cwd": cwd},
        })
    }

    fn codex_turn_context(cwd: &str) -> serde_json::Value {
        serde_json::json!({
            "timestamp": "2026-01-01T00:01:00Z",
            "type": "turn_context",
            "payload": {"cwd": cwd},
        })
    }

    fn codex_user_message(text: &str) -> serde_json::Value {
        serde_json::json!({
            "timestamp": "2026-01-01T00:01:10Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": text}],
            },
        })
    }

    /// Codex emits tool calls in BOTH envelopes. The full adapter routes
    /// `event_msg` function/tool/mcp calls into the same handler as
    /// `response_item`; a bounded reader that only accepted `response_item`
    /// collected zero workdir evidence from an `event_msg`-shaped rollout and
    /// left every foreign turn inheriting the baseline project.
    #[test]
    fn bounded_reader_reads_workdirs_from_event_msg_tool_calls() {
        let root = bounded_scope_root("event-msg");
        let fleet = root.join("fleet-bus");
        fs::create_dir_all(fleet.join(".git")).unwrap();
        let source_path = root.join(".codex").join("sessions").join("rollout.jsonl");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        // Real shape: `arguments` is a JSON STRING, not an object.
        let arguments = serde_json::to_string(&serde_json::json!({
            "cmd": "ls",
            "workdir": fleet.display().to_string(),
        }))
        .unwrap();
        let body = jsonl(&[
            codex_meta("/sessions/vista"),
            codex_turn_context("/sessions/vista"),
            codex_user_message("biore fleet task"),
            serde_json::json!({
                "timestamp": "2026-01-01T00:01:20Z",
                "type": "event_msg",
                "payload": {
                    "type": "function_call",
                    "name": "shell",
                    "call_id": "c1",
                    "arguments": arguments,
                },
            }),
        ]);
        fs::write(&source_path, body).unwrap();

        let entry = bounded_entry(&source_path);
        let allow = crate::source_path::SourceAllowlist::for_operator(&root, &root);
        let resolved = allow.resolve_file(&source_path).expect("resolve rollout");
        let frames =
            parse_large_codex_signal(&entry, &resolved, &allow, None).expect("bounded parse");

        assert_eq!(frames.len(), 1, "{frames:?}");
        assert_eq!(frames[0].message, "biore fleet task");
        assert_eq!(
            frames[0].cwd.as_deref(),
            Some(canonical(&fleet).as_str()),
            "an event_msg tool call re-scopes its window like a response_item one"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// An over-cap record is drained, never parsed. When it was a tool-call
    /// envelope, the window's only foreign workdir can be inside it — keeping
    /// the baseline there is a silent cross-repo leak, so unreadable evidence
    /// fails closed to unattributed.
    #[test]
    fn oversized_tool_call_fails_closed_to_unattributed() {
        // Real rollout key order: the envelope and payload discriminators are
        // written BEFORE the oversized body, so they survive the cap.
        let filler = "x".repeat(MAX_JSONL_RECORD_BYTES + 4096);
        let oversized_window = |label: &str, oversized: String| {
            let root = bounded_scope_root(label);
            let source_path = root.join(".codex").join("sessions").join("rollout.jsonl");
            fs::create_dir_all(source_path.parent().unwrap()).unwrap();
            let mut body = jsonl(&[
                codex_meta("/sessions/vista"),
                codex_turn_context("/sessions/vista"),
                codex_user_message("biore task"),
            ]);
            body.push_str(&oversized);
            body.push('\n');
            fs::write(&source_path, body).unwrap();
            let entry = bounded_entry(&source_path);
            let allow = crate::source_path::SourceAllowlist::for_operator(&root, &root);
            let resolved = allow.resolve_file(&source_path).expect("resolve rollout");
            let frames =
                parse_large_codex_signal(&entry, &resolved, &allow, None).expect("bounded parse");
            let _ = fs::remove_dir_all(&root);
            frames
        };

        let call_frames = oversized_window(
            "oversized-call",
            format!(
                r#"{{"timestamp":"2026-01-01T00:01:20Z","type":"response_item","payload":{{"type":"function_call","name":"shell","call_id":"c1","arguments":"{{\"cmd\":\"{filler}\"}}"}}}}"#
            ),
        );
        assert_eq!(call_frames.len(), 1, "{call_frames:?}");
        assert!(
            call_frames[0].scope_unattributed,
            "a tool call this reader could not read must not leave the window on its baseline"
        );

        // A merely oversized RESULT carries no workdir, so it is not lost
        // evidence and must not poison an otherwise clean window.
        let result_frames = oversized_window(
            "oversized-result",
            format!(
                r#"{{"timestamp":"2026-01-01T00:01:20Z","type":"response_item","payload":{{"type":"function_call_output","call_id":"c1","output":"{filler}"}}}}"#
            ),
        );
        assert_eq!(result_frames.len(), 1, "{result_frames:?}");
        assert!(
            !result_frames[0].scope_unattributed,
            "an oversized tool RESULT carries no workdir and is not lost evidence"
        );
        assert_eq!(result_frames[0].cwd.as_deref(), Some("/sessions/vista"));

        // A recognised envelope whose PAYLOAD discriminator sits after the
        // oversized body: key order is not a contract, and seeing only the
        // envelope's own `type` proves nothing about what was lost.
        let late_discriminator = oversized_window(
            "oversized-late-type",
            format!(
                r#"{{"timestamp":"2026-01-01T00:01:20Z","type":"response_item","payload":{{"arguments":"{filler}","type":"function_call"}}}}"#
            ),
        );
        assert_eq!(late_discriminator.len(), 1, "{late_discriminator:?}");
        assert!(
            late_discriminator[0].scope_unattributed,
            "an envelope truncated before its payload type must fail closed"
        );

        // Pathological writer: nothing identifiable survives the cap. The
        // record could have been a tool call, so the window cannot claim the
        // baseline it did not verify.
        let opaque_frames = oversized_window(
            "oversized-opaque",
            format!(r#"{{"payload":{{"arguments":"{filler}"}}}}"#),
        );
        assert_eq!(opaque_frames.len(), 1, "{opaque_frames:?}");
        assert!(
            opaque_frames[0].scope_unattributed,
            "an over-cap record this reader could not classify is not proof of the baseline"
        );
    }
    /// Finding: the scope verdicts stored in the parse ledger are resolved
    /// against the local filesystem, but the reuse gate keyed only on source
    /// bytes and catalog fields. Creating or removing a nested checkout, or
    /// editing `.gitmodules`, therefore left a cached extract servable under
    /// verdicts the current layout no longer supports.
    #[test]
    fn the_repository_layout_is_part_of_a_cached_extracts_identity() {
        let root = std::env::temp_dir().join(format!(
            "aicx-scope-env-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let parent = root.join("vista");
        let nested = parent.join("vendor").join("fleet-bus");
        fs::create_dir_all(parent.join(".git")).unwrap();
        fs::create_dir_all(&nested).unwrap();

        let baseline = parent.to_string_lossy().into_owned();
        let cwds = vec![nested.to_string_lossy().into_owned()];
        let before = scope_environment_fingerprint(Some(&baseline), &cwds);

        // A nested checkout appears: that path is now a repository of its own
        // and the frames under it stop belonging to the parent.
        fs::create_dir_all(nested.join(".git")).unwrap();
        let with_nested = scope_environment_fingerprint(Some(&baseline), &cwds);
        assert_ne!(
            before, with_nested,
            "a new nested checkout changes what the stored verdicts mean"
        );

        // And it disappears again.
        fs::remove_dir_all(nested.join(".git")).unwrap();
        assert_eq!(
            before,
            scope_environment_fingerprint(Some(&baseline), &cwds),
            "the same layout is the same identity"
        );

        // `.gitmodules` decides whether a vanished path is its own
        // repository, so its CONTENT is part of the environment.
        fs::write(
            parent.join(".gitmodules"),
            "[submodule \"fleet-bus\"]\n\tpath = vendor/fleet-bus\n",
        )
        .unwrap();
        let declared = scope_environment_fingerprint(Some(&baseline), &cwds);
        assert_ne!(before, declared, "a new submodule declaration is a change");
        fs::write(
            parent.join(".gitmodules"),
            "[submodule \"other\"]\n\tpath = vendor/other\n",
        )
        .unwrap();
        assert_ne!(
            declared,
            scope_environment_fingerprint(Some(&baseline), &cwds),
            "and so is editing one"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
