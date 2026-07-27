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
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::catalog::CatalogEntry;
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
const SIGNAL_FILTER_VERSION: &str = "signal-v2-vibecrafted-thought-strip";

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
    pub lexical_docs: usize,
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
}

/// Build or preview the global lexical index from the durable catalog.
///
/// Incremental truth is bounded by catalog rows that carry live source
/// fingerprints (size + mtime-ns). When the catalog snapshot and every
/// selected source fingerprint match the CURRENT generation, reuse is free.
/// A catalog rebuild that re-stats sources admits appends/edits; per-session
/// parse state reuses only extracts whose source fingerprint still matches.
pub fn build(
    aicx_home: &Path,
    project_filters: &[String],
    dry_run: bool,
    full_rescan: bool,
    cache_extracts: bool,
) -> Result<SourceIndexReport> {
    let started = Instant::now();
    if !dry_run && !project_filters.is_empty() {
        anyhow::bail!(
            "project-scoped index publishing is retired; run `aicx index` once for the global \
             catalog, then filter queries with `aicx search -p <project>` (use `aicx index -p \
             <project> --dry-run` only to inspect a project slice)"
        );
    }
    let catalog_path = crate::catalog::sessions_path_for(aicx_home);
    let entries = crate::catalog::read_entries_at(aicx_home)?;
    if entries.is_empty() {
        anyhow::bail!(
            "session catalog is empty at {}; run `aicx catalog rebuild` first",
            catalog_path.display()
        );
    }

    let selected: Vec<CatalogEntry> = entries
        .into_iter()
        .filter(|entry| project_selected(entry.project.as_deref(), project_filters))
        .collect();
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let source_allow = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home);
    // Digest includes LIVE size+mtime so appends without catalog rebuild still
    // move the generation fingerprint (source-change incremental).
    let source_fingerprint =
        source_fingerprint(aicx_home, &catalog_path, &selected, &source_allow)?;
    // Incremental short-circuit applies to both publish and dry-run. A matching
    // live+catalog digest means CURRENT already reflects this snapshot —
    // re-parsing ~10k sources on every `index --dry-run` recreated the mill
    // latency the extracts-store cut was meant to kill. Use `--full-rescan` to
    // force a walk. Live source drift (append without catalog rebuild) moves
    // the digest so recent frames cannot stay invisible forever.
    if !full_rescan
        && project_filters.is_empty()
        && crate::vector_index::source_lexical_generation_matches(&source_fingerprint)?
    {
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
            lexical_docs: crate::vector_index::current_lexical_doc_count()?.unwrap_or(0),
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
    let mut skipped_by_agent = BTreeMap::new();
    let mut next_state = SourceParseState {
        schema: PARSE_STATE_SCHEMA.to_string(),
        signal_filter_version: SIGNAL_FILTER_VERSION.to_string(),
        sessions: BTreeMap::new(),
    };

    let prior_state = if full_rescan {
        SourceParseState::default()
    } else {
        load_parse_state(aicx_home)
    };

    for entry in &selected {
        let session_key = session_state_key(&entry.agent, &entry.session_id);

        // True incremental: reuse a prior extract only when the source
        // fingerprint (path + size + mtime) still matches and extract bytes
        // have not been tampered with.
        if let Some(chunk) =
            try_reuse_cached_extract(aicx_home, entry, &prior_state, &session_key, &source_allow)
        {
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
        let mut frames = match parse_catalog_source(entry, &source_path, &source_allow) {
            Ok(frames) => frames,
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
        if !dry_run
            && cache_extracts
            && write_if_changed(aicx_home, &extract_path, extract.as_bytes())?
        {
            extracts_written += 1;
        }
        let indexed_path = if !dry_run && cache_extracts {
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
        let metadata = serde_json::json!({
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
        });
        chunks.push(aicx_retrieve::ChunkRef {
            id: format!("{}:{}", entry.agent, entry.session_id),
            source_path: indexed_path.display().to_string(),
            text: extract.clone(),
            metadata,
        });

        if cache_extracts {
            let rel = extract_path
                .strip_prefix(aicx_home)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| extract_path.clone());
            // Always stamp LIVE stats into the parse ledger so reuse survives
            // catalog lag (append without catalog rebuild still reuses later
            // once CURRENT has the new extract).
            let (source_len, source_mtime_ns) =
                crate::catalog::live_source_fingerprint(&source_path)
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
                },
            );
        }
    }

    if chunks.is_empty() {
        anyhow::bail!(
            "source-driven index produced zero signal extracts from {} cataloged source(s)",
            selected.len()
        );
    }

    let manifest_path = if dry_run {
        None
    } else {
        let manifest =
            crate::vector_index::publish_source_lexical_generation(&chunks, &source_fingerprint)?;
        // Persist parse state only after a successful publish so a killed build
        // cannot claim sessions are current when CURRENT never flipped.
        if cache_extracts && project_filters.is_empty() {
            write_parse_state(aicx_home, &next_state)?;
        }
        Some(
            crate::vector_index::hybrid_manifest_path(None)?
                .display()
                .to_string(),
        )
        .filter(|_| manifest.lexical_doc_count == chunks.len())
    };

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
        lexical_docs: chunks.len(),
        unchanged: false,
        wall_ms: started.elapsed().as_millis() as u64,
        manifest_path,
        skipped_by_agent,
    })
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
    let cache_path = extract_path_for(aicx_home, &entry.agent, &entry.session_id);
    if cache_path.is_file() {
        let body = crate::source_path::read_under_aicx_home(aicx_home, &cache_path)
            .with_context(|| format!("read session extract cache {}", cache_path.display()))?;
        return Ok(SessionDocument {
            body,
            source_path: entry.source_path.clone(),
            document_path: cache_path,
            cache_hit: true,
        });
    }

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
    let mut frames = parse_catalog_source(entry, &source_path, &source_allow)?;
    frames.sort_by_key(|frame| frame.timestamp);
    frames.retain(is_signal_frame);
    for frame in &mut frames {
        frame.message = clean_message(&frame.message);
    }
    frames.retain(|frame| !frame.message.trim().is_empty());
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

fn parse_catalog_source(
    entry: &CatalogEntry,
    path: &Path,
    allow: &crate::source_path::SourceAllowlist,
) -> Result<Vec<TimelineEntry>> {
    // Canonicalize + prove containment under approved source roots before any open.
    let path = allow
        .resolve_file(path)
        .with_context(|| format!("resolve catalog source {}", path.display()))?;

    if entry.agent == "vibecrafted" {
        let body = allow
            .read_to_string(&path)
            .with_context(|| format!("read runtime transcript {}", path.display()))?;
        // Token-stream runtime_runs logs interleave thought fragments with
        // visible text. Indexing the raw body made search surface
        // `{"type":"thought","data":"The"}` spam over real operator answers.
        let message = vibecrafted_signal_body(&body);
        if message.trim().is_empty() {
            return Ok(Vec::new());
        }
        let timestamp = fs::metadata(&path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(chrono::DateTime::<chrono::Utc>::from)
            .unwrap_or_else(chrono::Utc::now);
        return Ok(vec![TimelineEntry {
            timestamp,
            agent: entry.agent.clone(),
            session_id: entry.session_id.clone(),
            role: "assistant".to_string(),
            message,
            frame_kind: Some(FrameKind::AgentReply),
            branch: None,
            cwd: entry.cwd.clone(),
            timestamp_source: Some("source_mtime".to_string()),
            source_path: Some(entry.source_path.clone()),
            source_sha256: None,
            source_line_span: None,
        }]);
    }

    let source_bytes = fs::metadata(&path)
        .with_context(|| format!("stat source {}", path.display()))?
        .len();
    if entry.agent == "codex" && source_bytes > MAX_FULL_PARSE_BYTES {
        return parse_large_codex_signal(entry, &path, allow);
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
        "gemini" => aicx_parser::engine::AgentKind::Gemini,
        "grok" => aicx_parser::engine::AgentKind::Grok,
        "junie" => aicx_parser::engine::AgentKind::Junie,
        other => anyhow::bail!("unsupported catalog agent `{other}`"),
    };
    let parsed = crate::parser_dispatch::parse_file(
        agent,
        &entry.session_id,
        entry.logical_session_id.clone(),
        &path,
    )?;
    Ok(crate::output::timeline_entries_from_model(parsed.model()))
}

/// Read one cataloged session through the same allowlisted, signal-only parser
/// used by the lexical index.
///
/// Intent retrieval uses this path directly instead of reconstructing evidence
/// from retired per-frame cards. The returned path is canonical and proven to
/// live below one of the operator-approved source roots before any source open.
pub(crate) fn read_catalog_signal_at(
    aicx_home: &Path,
    entry: &CatalogEntry,
    frame_kind: FrameKind,
) -> Result<(PathBuf, Vec<TimelineEntry>)> {
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
    let mut frames = parse_catalog_source(entry, &source_path, &source_allow)?;
    frames.sort_by_key(|frame| frame.timestamp);
    frames.retain(is_signal_frame);
    frames.retain(|frame| frame_matches_kind(frame, frame_kind));
    for frame in &mut frames {
        frame.message = clean_message(&frame.message);
    }
    frames.retain(|frame| !frame.message.trim().is_empty());
    Ok((source_path, frames))
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
) -> Result<Vec<TimelineEntry>> {
    // `path` is already resolve_file'd by the caller; open through the allowlist.
    let file = allow
        .open_file(path)
        .with_context(|| format!("open source {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut frames = Vec::new();
    let mut line_no = 0u64;
    while let Some(record) = crate::sanitize::read_line_capped(&mut reader, MAX_JSONL_RECORD_BYTES)?
    {
        line_no += 1;
        if record.exceeded || record.line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&record.line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(serde_json::Value::as_str) != Some("message") {
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
        frames.push(TimelineEntry {
            timestamp,
            agent: entry.agent.clone(),
            session_id: entry.session_id.clone(),
            role: role.to_string(),
            message,
            frame_kind: Some(frame_kind),
            branch: None,
            cwd: entry.cwd.clone(),
            timestamp_source: Some("record".to_string()),
            source_path: Some(entry.source_path.clone()),
            source_sha256: None,
            source_line_span: Some((line_no, line_no)),
        });
    }
    Ok(frames)
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

fn load_parse_state(aicx_home: &Path) -> SourceParseState {
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
    if state.schema != PARSE_STATE_SCHEMA || state.signal_filter_version != SIGNAL_FILTER_VERSION {
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
) -> Option<aicx_retrieve::ChunkRef> {
    if prior.signal_filter_version != SIGNAL_FILTER_VERSION {
        return None;
    }
    let record = prior.sessions.get(session_key)?;
    if record.source_path != entry.source_path {
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
) -> Result<String> {
    let mut hasher = Sha256::new();
    // Filter generation first so a catalog-identical CURRENT cannot hide a
    // pre-filter corpus after signal-body rules change.
    hasher.update(SIGNAL_FILTER_VERSION.as_bytes());
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
    fn signal_filter_version_is_non_empty_and_stable_for_this_cut() {
        // Guard against accidental empty version (would collapse fingerprints
        // across filter generations without meaning to).
        assert!(!SIGNAL_FILTER_VERSION.is_empty());
        assert!(SIGNAL_FILTER_VERSION.starts_with("signal-v"));
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
        };
        let mut prior = SourceParseState {
            schema: PARSE_STATE_SCHEMA.to_string(),
            signal_filter_version: SIGNAL_FILTER_VERSION.to_string(),
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
            },
        );

        let key = session_state_key("claude", "session");
        // Allowlist roots: treat test root as both HOME and aicx_home.
        let allow = crate::source_path::SourceAllowlist::for_operator(&root, &root);
        let reused = try_reuse_cached_extract(&root, &entry, &prior, &key, &allow)
            .expect("matching source fingerprint+hash must reuse");
        assert!(reused.text.contains("arrows vc-frame"));
        assert_eq!(reused.id, "claude:session");

        // Source path drift invalidates reuse.
        let mut drifted = entry.clone();
        drifted.source_path = "/elsewhere/session.jsonl".to_string();
        assert!(try_reuse_cached_extract(&root, &drifted, &prior, &key, &allow).is_none());

        // Catalog-only size drift must NOT invalidate when live file is unchanged
        // (catalog lag after a live-stamped reparse is expected).
        let mut catalog_lag = entry.clone();
        catalog_lag.source_len = Some(source_len + 64);
        assert!(
            try_reuse_cached_extract(&root, &catalog_lag, &prior, &key, &allow).is_some(),
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
            try_reuse_cached_extract(&root, &entry, &prior, &key, &allow).is_none(),
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
        assert!(try_reuse_cached_extract(&root, &entry, &prior, &key, &allow).is_none());
        prior.sessions.get_mut(&key).unwrap().source_len = restored_len;

        // Corrupt extract bytes invalidate reuse.
        fs::write(&extract_path, "tampered").unwrap();
        assert!(try_reuse_cached_extract(&root, &entry, &prior, &key, &allow).is_none());

        let _ = fs::remove_dir_all(&root);
    }
}
