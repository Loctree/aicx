//! Deterministic locate-before-parse catalog for physical agent sessions.
//!
//! The catalog deliberately stops at bounded identity headers. It never parses
//! conversation bodies and never lets directory traversal order choose a
//! winner. A physical filename UUID is the stable source identity; when there
//! is no filename UUID, the first valid root-record id owns the source. Later
//! root ids are aliases, while explicitly scoped child/subagent ids remain
//! children and cannot replace the source identity.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aicx_parser::sanitize;
use serde_json::Value;

/// Maximum physical bytes read from one candidate while cataloging identity.
pub const MAX_HEADER_BYTES: usize = 256 * 1024;
/// Maximum JSON/JSONL records inspected from one candidate.
pub const MAX_HEADER_LINES: usize = 128;
/// Maximum bytes retained from any individual header record.
pub const MAX_HEADER_LINE_BYTES: usize = 64 * 1024;
const MAX_SCAN_DEPTH: usize = 12;
const MAX_ID_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AgentKind {
    Claude,
    Codex,
    Gemini,
    Junie,
    Grok,
}

impl AgentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Junie => "junie",
            Self::Grok => "grok",
        }
    }

    fn accepts_extension(self, extension: Option<&str>) -> bool {
        match self {
            Self::Gemini => matches!(extension, Some("json" | "jsonl")),
            Self::Claude | Self::Codex | Self::Junie | Self::Grok => extension == Some("jsonl"),
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceFingerprint {
    pub len: u64,
    pub modified_unix_nanos: u128,
}

impl SourceFingerprint {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        let modified_unix_nanos = metadata
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Self {
            len: metadata.len(),
            modified_unix_nanos,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopedChildIdentity {
    pub id: String,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSource {
    pub agent: AgentKind,
    /// Stable identity of the physical source, independent of logical drift.
    pub source_id: String,
    /// First valid top-level record id, if the source asserted one.
    pub logical_session_id: Option<String>,
    /// Ordered later top-level ids observed within the bounded header.
    pub aliases: Vec<String>,
    /// Validated aliases derived from the physical filename.
    pub filename_aliases: Vec<String>,
    /// Child identities never promoted into source/logical identity.
    pub scoped_children: Vec<ScopedChildIdentity>,
    pub path: PathBuf,
    /// True when no valid logical root id exists and identity came from source
    /// coordinates (UUID/filename) only.
    pub identity_inferred: bool,
    pub fingerprint: SourceFingerprint,
    pub header_truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    ExactSourceId,
    ExactLogicalId,
    ExactAlias,
    ExactFilenameAlias,
    UuidSuffix,
    UniquePrefix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    pub query: String,
    pub matched_by: MatchKind,
    pub source: CatalogSource,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CatalogCandidateSummary {
    pub source_id: String,
    pub logical_session_id: Option<String>,
    pub path: PathBuf,
    pub identity_inferred: bool,
}

impl From<&CatalogSource> for CatalogCandidateSummary {
    fn from(source: &CatalogSource) -> Self {
        Self {
            source_id: source.source_id.clone(),
            logical_session_id: source.logical_session_id.clone(),
            path: source.path.clone(),
            identity_inferred: source.identity_inferred,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    InvalidQuery(String),
    Io {
        path: PathBuf,
        message: String,
    },
    Missing {
        query: String,
        agent: AgentKind,
        candidates_scanned: usize,
    },
    Ambiguous {
        query: String,
        candidates: Vec<CatalogCandidateSummary>,
    },
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQuery(query) => write!(formatter, "invalid session reference `{query}`"),
            Self::Io { path, message } => {
                write!(
                    formatter,
                    "session catalog I/O error at {}: {message}",
                    path.display()
                )
            }
            Self::Missing {
                query,
                agent,
                candidates_scanned,
            } => write!(
                formatter,
                "no {agent} session matched `{query}` ({candidates_scanned} candidate source(s))"
            ),
            Self::Ambiguous { query, candidates } => {
                write!(formatter, "session reference `{query}` is ambiguous:")?;
                for candidate in candidates {
                    write!(
                        formatter,
                        "\n- {} ({})",
                        candidate.source_id,
                        candidate.path.display()
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for CatalogError {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogIoStats {
    pub directories_visited: usize,
    pub metadata_candidates: usize,
    pub files_opened: usize,
    pub header_lines_read: usize,
    pub header_bytes_read: usize,
    /// Kept explicit so tests and callers can prove locate-before-parse.
    pub body_reads: usize,
    pub rejected_paths: usize,
}

#[derive(Debug)]
pub struct CatalogLookup {
    pub result: Result<ResolvedSource, CatalogError>,
    pub stats: CatalogIoStats,
}

#[derive(Debug)]
pub struct CatalogScan {
    pub result: Result<Vec<CatalogSource>, CatalogError>,
    pub stats: CatalogIoStats,
}

#[derive(Debug, Clone)]
pub struct SessionCatalog {
    agent: AgentKind,
    root: PathBuf,
    max_depth: usize,
}

#[derive(Debug, Clone)]
struct CandidatePath {
    path: PathBuf,
    filename_aliases: Vec<String>,
    filename_uuid: Option<String>,
    fingerprint: SourceFingerprint,
}

impl SessionCatalog {
    pub fn new(agent: AgentKind, root: impl AsRef<Path>) -> Result<Self, CatalogError> {
        let requested = root.as_ref();
        let root = sanitize::validate_dir_path(requested).map_err(|error| CatalogError::Io {
            path: requested.to_path_buf(),
            message: error.to_string(),
        })?;
        Ok(Self {
            agent,
            root,
            max_depth: MAX_SCAN_DEPTH,
        })
    }

    pub fn resolve(&self, query: &str) -> Result<ResolvedSource, CatalogError> {
        self.resolve_with_stats(query).result
    }

    pub fn resolve_with_stats(&self, query: &str) -> CatalogLookup {
        let mut stats = CatalogIoStats::default();
        let result = self.resolve_inner(query, &mut stats);
        CatalogLookup { result, stats }
    }

    /// Rebuild the catalog from current directory metadata and bounded headers.
    /// No cache is retained, so add/remove/rename/content changes are observed
    /// on every call and cached state can never become correctness authority.
    pub fn scan_with_stats(&self) -> CatalogScan {
        let mut stats = CatalogIoStats::default();
        let result = self.scan_sources(&mut stats);
        CatalogScan { result, stats }
    }

    fn resolve_inner(
        &self,
        query: &str,
        stats: &mut CatalogIoStats,
    ) -> Result<ResolvedSource, CatalogError> {
        let query = validate_identity(query)
            .ok_or_else(|| CatalogError::InvalidQuery(query.to_string()))?;
        let candidates = self.collect_candidate_paths(stats)?;

        // A filename UUID is the physical source authority. Exact UUID lookup
        // can therefore open only the matching header and avoid touching an
        // arbitrarily large unrelated corpus.
        let uuid_matches: Vec<&CandidatePath> = candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .filename_uuid
                    .as_deref()
                    .is_some_and(|id| identity_eq(id, &query))
            })
            .collect();
        if uuid_matches.len() > 1 {
            let mut summaries = uuid_matches
                .into_iter()
                .map(|candidate| CatalogCandidateSummary {
                    source_id: candidate.filename_uuid.clone().unwrap_or_default(),
                    logical_session_id: None,
                    path: candidate.path.clone(),
                    identity_inferred: true,
                })
                .collect::<Vec<_>>();
            summaries.sort();
            return Err(CatalogError::Ambiguous {
                query,
                candidates: summaries,
            });
        }
        if let Some(candidate) = uuid_matches.first() {
            let source =
                self.probe_candidate(candidate, stats)?
                    .ok_or_else(|| CatalogError::Missing {
                        query: query.clone(),
                        agent: self.agent,
                        candidates_scanned: candidates.len(),
                    })?;
            return Ok(ResolvedSource {
                query,
                matched_by: MatchKind::ExactSourceId,
                source,
            });
        }

        let sources = self.probe_candidates(&candidates, stats)?;
        resolve_from_sources(self.agent, query, sources)
    }

    fn scan_sources(&self, stats: &mut CatalogIoStats) -> Result<Vec<CatalogSource>, CatalogError> {
        let candidates = self.collect_candidate_paths(stats)?;
        self.probe_candidates(&candidates, stats)
    }

    fn collect_candidate_paths(
        &self,
        stats: &mut CatalogIoStats,
    ) -> Result<Vec<CandidatePath>, CatalogError> {
        let mut pending = vec![(self.root.clone(), 0usize)];
        let mut paths = BTreeSet::new();

        while let Some((directory, depth)) = pending.pop() {
            stats.directories_visited += 1;
            let entries =
                sanitize::read_dir_validated(&directory).map_err(|error| CatalogError::Io {
                    path: directory.clone(),
                    message: error.to_string(),
                })?;
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    stats.rejected_paths += 1;
                    continue;
                };
                let path = entry.path();
                if file_type.is_symlink() {
                    stats.rejected_paths += 1;
                    continue;
                }
                if file_type.is_dir() {
                    if depth < self.max_depth {
                        pending.push((path, depth + 1));
                    }
                    continue;
                }
                if !file_type.is_file()
                    || !self
                        .agent
                        .accepts_extension(path.extension().and_then(|ext| ext.to_str()))
                {
                    continue;
                }
                paths.insert(path);
            }
        }

        let mut candidates = Vec::with_capacity(paths.len());
        for path in paths {
            let Ok(metadata) = fs::metadata(&path) else {
                stats.rejected_paths += 1;
                continue;
            };
            let filename_aliases = filename_aliases(&path);
            if filename_aliases.is_empty() {
                stats.rejected_paths += 1;
                continue;
            }
            let filename_uuid = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(uuid_from_filename)
                .map(str::to_ascii_lowercase);
            candidates.push(CandidatePath {
                path,
                filename_aliases,
                filename_uuid,
                fingerprint: SourceFingerprint::from_metadata(&metadata),
            });
        }
        candidates.sort_by(|left, right| left.path.cmp(&right.path));
        stats.metadata_candidates = candidates.len();
        Ok(candidates)
    }

    fn probe_candidates(
        &self,
        candidates: &[CandidatePath],
        stats: &mut CatalogIoStats,
    ) -> Result<Vec<CatalogSource>, CatalogError> {
        let mut sources = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            match self.probe_candidate(candidate, stats) {
                Ok(Some(source)) => sources.push(source),
                Ok(None) | Err(CatalogError::Io { .. }) => stats.rejected_paths += 1,
                Err(error) => return Err(error),
            }
        }
        sources.sort_by(|left, right| {
            left.source_id
                .cmp(&right.source_id)
                .then_with(|| left.path.cmp(&right.path))
        });
        Ok(sources)
    }

    fn probe_candidate(
        &self,
        candidate: &CandidatePath,
        stats: &mut CatalogIoStats,
    ) -> Result<Option<CatalogSource>, CatalogError> {
        let file =
            sanitize::open_file_validated(&candidate.path).map_err(|error| CatalogError::Io {
                path: candidate.path.clone(),
                message: error.to_string(),
            })?;
        stats.files_opened += 1;
        let mut reader = BufReader::new(file.take(MAX_HEADER_BYTES as u64));
        let mut root_ids = Vec::new();
        let mut children = Vec::new();
        let mut json_prefix = String::new();
        let mut header_truncated = false;

        for _ in 0..MAX_HEADER_LINES {
            let Some(line) = sanitize::read_line_capped(&mut reader, MAX_HEADER_LINE_BYTES)
                .map_err(|error| CatalogError::Io {
                    path: candidate.path.clone(),
                    message: error.to_string(),
                })?
            else {
                break;
            };
            stats.header_lines_read += 1;
            if line.exceeded {
                header_truncated = true;
                continue;
            }
            if json_prefix.len() + line.line.len() <= MAX_HEADER_BYTES {
                json_prefix.push_str(&line.line);
            }
            if let Ok(value) = serde_json::from_str::<Value>(line.line.trim()) {
                observe_record(self.agent, &value, &mut root_ids, &mut children);
            }
        }
        let bytes_read = MAX_HEADER_BYTES as u64 - reader.get_ref().limit();
        stats.header_bytes_read += bytes_read as usize;
        if bytes_read as usize == MAX_HEADER_BYTES {
            header_truncated = true;
        }

        if root_ids.is_empty()
            && let Ok(value) = serde_json::from_str::<Value>(&json_prefix)
        {
            observe_record(self.agent, &value, &mut root_ids, &mut children);
        }

        root_ids.retain(|id| validate_identity(id).is_some());
        children.retain(|child| validate_identity(&child.id).is_some());
        dedupe_ordered(&mut root_ids);
        dedupe_children_ordered(&mut children);

        let logical_session_id = root_ids.first().cloned();
        let stem_identity = candidate
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(validate_identity);
        let Some(source_id) = candidate
            .filename_uuid
            .clone()
            .or_else(|| logical_session_id.clone())
            .or(stem_identity)
        else {
            return Ok(None);
        };
        let aliases = root_ids.into_iter().skip(1).collect();

        Ok(Some(CatalogSource {
            agent: self.agent,
            source_id,
            logical_session_id: logical_session_id.clone(),
            aliases,
            filename_aliases: candidate.filename_aliases.clone(),
            scoped_children: children,
            path: candidate.path.clone(),
            identity_inferred: logical_session_id.is_none(),
            fingerprint: candidate.fingerprint.clone(),
            header_truncated,
        }))
    }
}

fn resolve_from_sources(
    agent: AgentKind,
    query: String,
    sources: Vec<CatalogSource>,
) -> Result<ResolvedSource, CatalogError> {
    for (kind, predicate) in [
        (
            MatchKind::ExactSourceId,
            exact_source_id as fn(&CatalogSource, &str) -> bool,
        ),
        (MatchKind::ExactLogicalId, exact_logical_id),
        (MatchKind::ExactAlias, exact_alias),
        (MatchKind::ExactFilenameAlias, exact_filename_alias),
    ] {
        let matches = sources
            .iter()
            .filter(|source| predicate(source, &query))
            .collect::<Vec<_>>();
        if !matches.is_empty() {
            return one_or_ambiguous(query, kind, matches);
        }
    }

    let uuid_suffix_matches = sources
        .iter()
        .filter(|source| {
            is_uuid(&source.source_id)
                && query.len() >= 8
                && identity_ends_with(&source.source_id, &query)
        })
        .collect::<Vec<_>>();
    if !uuid_suffix_matches.is_empty() {
        return one_or_ambiguous(query, MatchKind::UuidSuffix, uuid_suffix_matches);
    }

    let matches = sources
        .iter()
        .filter(|source| source_matches_prefix(source, &query))
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(CatalogError::Missing {
            query,
            agent,
            candidates_scanned: sources.len(),
        });
    }
    one_or_ambiguous(query, MatchKind::UniquePrefix, matches)
}

fn one_or_ambiguous(
    query: String,
    kind: MatchKind,
    matches: Vec<&CatalogSource>,
) -> Result<ResolvedSource, CatalogError> {
    if matches.len() == 1 {
        return Ok(ResolvedSource {
            query,
            matched_by: kind,
            source: matches[0].clone(),
        });
    }
    let mut candidates = matches
        .into_iter()
        .map(CatalogCandidateSummary::from)
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    Err(CatalogError::Ambiguous { query, candidates })
}

fn exact_source_id(source: &CatalogSource, query: &str) -> bool {
    identity_eq(&source.source_id, query)
}

fn exact_logical_id(source: &CatalogSource, query: &str) -> bool {
    source
        .logical_session_id
        .as_deref()
        .is_some_and(|id| identity_eq(id, query))
}

fn exact_alias(source: &CatalogSource, query: &str) -> bool {
    source.aliases.iter().any(|id| identity_eq(id, query))
}

fn exact_filename_alias(source: &CatalogSource, query: &str) -> bool {
    source
        .filename_aliases
        .iter()
        .any(|id| identity_eq(id, query))
}

fn source_matches_prefix(source: &CatalogSource, query: &str) -> bool {
    identity_starts_with(&source.source_id, query)
        || source
            .logical_session_id
            .as_deref()
            .is_some_and(|id| identity_starts_with(id, query))
        || source
            .aliases
            .iter()
            .any(|id| identity_starts_with(id, query))
        || source
            .filename_aliases
            .iter()
            .any(|id| identity_starts_with(id, query))
}

fn observe_record(
    agent: AgentKind,
    value: &Value,
    root_ids: &mut Vec<String>,
    children: &mut Vec<ScopedChildIdentity>,
) {
    if let Some(items) = value.as_array() {
        for item in items {
            observe_record(agent, item, root_ids, children);
        }
        return;
    }
    let Some(object) = value.as_object() else {
        return;
    };

    if matches!(agent, AgentKind::Codex | AgentKind::Grok)
        && object.get("type").and_then(Value::as_str) == Some("session_meta")
    {
        if let Some(id) = object
            .get("payload")
            .and_then(|payload| payload.get("id"))
            .and_then(Value::as_str)
            .and_then(validate_identity)
        {
            root_ids.push(id);
        }
        return;
    }

    let id = object
        .get("sessionId")
        .or_else(|| object.get("session_id"))
        .and_then(Value::as_str)
        .and_then(validate_identity);
    if let Some(id) = id {
        if record_is_scoped_child(object) {
            let parent_id = object
                .get("parentSessionId")
                .or_else(|| object.get("parent_session_id"))
                .and_then(Value::as_str)
                .and_then(validate_identity);
            children.push(ScopedChildIdentity { id, parent_id });
        } else {
            root_ids.push(id);
        }
    }

    for key in ["subagent", "childSession", "child_session"] {
        let Some(child) = object.get(key).and_then(Value::as_object) else {
            continue;
        };
        let Some(id) = child
            .get("sessionId")
            .or_else(|| child.get("session_id"))
            .or_else(|| child.get("id"))
            .and_then(Value::as_str)
            .and_then(validate_identity)
        else {
            continue;
        };
        let parent_id = child
            .get("parentSessionId")
            .or_else(|| child.get("parent_session_id"))
            .and_then(Value::as_str)
            .and_then(validate_identity);
        children.push(ScopedChildIdentity { id, parent_id });
    }
}

fn record_is_scoped_child(object: &serde_json::Map<String, Value>) -> bool {
    object
        .get("isSidechain")
        .or_else(|| object.get("is_sidechain"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || object.contains_key("agentId")
        || object.contains_key("subagentId")
        || object.contains_key("parentSessionId")
        || object.contains_key("parent_session_id")
}

fn filename_aliases(path: &Path) -> Vec<String> {
    let mut aliases = Vec::new();
    if let Some(filename) = path.file_name().and_then(|value| value.to_str())
        && let Some(filename) = validate_identity(filename)
    {
        aliases.push(filename);
    }
    if let Some(stem) = path.file_stem().and_then(|value| value.to_str())
        && let Some(stem) = validate_identity(stem)
    {
        aliases.push(stem.clone());
        if let Some(uuid) = uuid_from_filename(&stem) {
            aliases.push(uuid.to_ascii_lowercase());
        }
    }
    dedupe_ordered(&mut aliases);
    aliases
}

fn uuid_from_filename(stem: &str) -> Option<&str> {
    if is_uuid(stem) {
        return Some(stem);
    }
    let suffix = stem.get(stem.len().checked_sub(36)?..)?;
    let boundary = stem.len().checked_sub(37)?;
    (is_uuid(suffix)
        && stem
            .as_bytes()
            .get(boundary)
            .is_some_and(|byte| matches!(*byte, b'-' | b'_' | b'.')))
    .then_some(suffix)
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.as_bytes().iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn validate_identity(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || value == "."
        || value == ".."
        || value.contains("..")
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@' | b'+')
        })
    {
        return None;
    }
    Some(value.to_string())
}

fn identity_eq(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn identity_starts_with(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

fn identity_ends_with(value: &str, suffix: &str) -> bool {
    value
        .get(value.len().saturating_sub(suffix.len())..)
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(suffix))
}

fn dedupe_ordered(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.to_ascii_lowercase()));
}

fn dedupe_children_ordered(values: &mut Vec<ScopedChildIdentity>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.id.to_ascii_lowercase()));
}
