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
use sha2::{Digest, Sha256};

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
    Kimi,
    Cursor,
    Copilot,
}

impl AgentKind {
    pub const ALL: [Self; 8] = [
        Self::Claude,
        Self::Codex,
        Self::Gemini,
        Self::Junie,
        Self::Grok,
        Self::Kimi,
        Self::Cursor,
        Self::Copilot,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Junie => "junie",
            Self::Grok => "grok",
            Self::Kimi => "kimi",
            Self::Cursor => "cursor",
            Self::Copilot => "copilot",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "gemini" | "gemini-antigravity" => Some(Self::Gemini),
            "junie" => Some(Self::Junie),
            "grok" => Some(Self::Grok),
            "kimi" => Some(Self::Kimi),
            "cursor" | "cursor-agent" => Some(Self::Cursor),
            "copilot" | "copilot-cli" | "github-copilot" | "github-copilot-cli" => {
                Some(Self::Copilot)
            }
            _ => None,
        }
    }

    /// On-disk session root for this agent under the operator home.
    pub fn session_root(self, home: &Path) -> PathBuf {
        match self {
            Self::Claude => home.join(".claude").join("projects"),
            Self::Codex => home.join(".codex").join("sessions"),
            Self::Gemini => home.join(".gemini").join("tmp"),
            Self::Grok => home.join(".grok").join("sessions"),
            Self::Junie => home.join(".junie").join("sessions"),
            Self::Kimi => home.join(".kimi-code").join("sessions"),
            Self::Cursor => home.join(".cursor").join("projects"),
            Self::Copilot => copilot_session_root(home),
        }
    }

    pub const fn parser_kind(self) -> aicx_parser::engine::AgentKind {
        match self {
            Self::Claude => aicx_parser::engine::AgentKind::Claude,
            Self::Codex => aicx_parser::engine::AgentKind::Codex,
            Self::Gemini => aicx_parser::engine::AgentKind::Gemini,
            Self::Grok => aicx_parser::engine::AgentKind::Grok,
            Self::Junie => aicx_parser::engine::AgentKind::Junie,
            Self::Kimi => aicx_parser::engine::AgentKind::Kimi,
            Self::Cursor => aicx_parser::engine::AgentKind::Cursor,
            Self::Copilot => aicx_parser::engine::AgentKind::Copilot,
        }
    }

    fn accepts_extension(self, extension: Option<&str>) -> bool {
        match self {
            Self::Gemini => matches!(extension, Some("json" | "jsonl")),
            Self::Claude
            | Self::Codex
            | Self::Junie
            | Self::Grok
            | Self::Kimi
            | Self::Cursor
            | Self::Copilot => extension == Some("jsonl"),
        }
    }

    /// Grok session dirs carry multiple JSONL streams (chat, events, updates,
    /// hunks, rewind). Only `chat_history.jsonl` is conversation content;
    /// telemetry streams must not become catalog identity.
    ///
    /// Gemini CLI keeps a conversation only under `<project>/chats/`: either
    /// `chats/session-*.json[l]` or, for resumed sessions, `chats/<uuid>/<id>.json`.
    /// The same project directory also holds `logs.json`, `checkpoint-*.json`,
    /// `.extraction-state.json` and `formatted_context.json`, which are JSON but
    /// were never a session. Measured on a real tree (2026-09-10) they were 32
    /// of 397 candidates and every one of them reached the adapter as an
    /// `unknown_payload_type` refusal. Discovery decides here, by shape of the
    /// path, so the adapter is never asked about them.
    ///
    /// Kimi session dirs hold one `wire.jsonl` per agent lane
    /// (`session_<uuid>/agents/<agentId>/wire.jsonl`) plus non-conversation
    /// material (`state.json`, `logs/`, `tasks/`, `file-history/`). Only the
    /// per-lane wire file is a session source.
    ///
    /// Cursor project dirs hold the conversation only under
    /// `agent-transcripts/<uuid>/<uuid>.jsonl`; siblings (`agent-tools/`,
    /// `terminals/`, `repo.json`, `worker.log`) are never a session. The
    /// shape check keeps any future non-transcript `.jsonl` from becoming
    /// catalog identity.
    fn is_primary_source_file(self, path: &Path) -> bool {
        match self {
            Self::Grok => {
                path.file_name().and_then(|name| name.to_str()) == Some("chat_history.jsonl")
            }
            Self::Cursor => {
                let stem_owns_session_dir = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .zip(
                        path.parent()
                            .and_then(|dir| dir.file_name())
                            .and_then(|name| name.to_str()),
                    )
                    .is_some_and(|(stem, dir)| stem == dir && is_uuid(stem));
                stem_owns_session_dir
                    && path
                        .ancestors()
                        .nth(2)
                        .and_then(|dir| dir.file_name())
                        .and_then(|name| name.to_str())
                        == Some("agent-transcripts")
            }
            Self::Gemini => path
                .ancestors()
                .skip(1)
                .take(2)
                .any(|dir| dir.file_name().and_then(|name| name.to_str()) == Some("chats")),
            Self::Kimi => path.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl"),
            Self::Copilot => is_copilot_source_file(path),
            Self::Claude | Self::Codex | Self::Junie => true,
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
    /// Cheap physical identity on Unix (dev/inode/ctime/mode/owner); on
    /// platforms without that proof this includes a streaming content digest.
    pub physical_identity: Vec<u64>,
    /// Separate artifact evidence for multi-file sources. Size and freshness
    /// retain their physical meanings; neither encodes this digest.
    pub bundle_fingerprint: Option<String>,
}

impl SourceFingerprint {
    fn from_path(path: &Path, metadata: &fs::Metadata) -> std::io::Result<Self> {
        let _ = path;
        let modified_unix_nanos = metadata
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        #[cfg(unix)]
        let physical_identity = {
            use std::os::unix::fs::MetadataExt;
            vec![
                metadata.dev(),
                metadata.ino(),
                metadata.ctime() as u64,
                metadata.ctime_nsec() as u64,
                metadata.mode() as u64,
                metadata.uid() as u64,
                metadata.gid() as u64,
            ]
        };
        #[cfg(not(unix))]
        let physical_identity = {
            let mut file = sanitize::open_file_validated(path).map_err(std::io::Error::other)?;
            let mut hasher = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            hasher
                .finalize()
                .chunks_exact(8)
                .map(|bytes| u64::from_le_bytes(bytes.try_into().expect("eight-byte digest chunk")))
                .collect()
        };
        Ok(Self {
            len: metadata.len(),
            modified_unix_nanos,
            physical_identity,
            bundle_fingerprint: None,
        })
    }
}

/// GitHub Copilot CLI and SDK honor COPILOT_HOME for their configuration and
/// persisted sessions. Storage.home/AICX_HOME do not move provider sources.
pub fn copilot_session_root(home: &Path) -> PathBuf {
    let configured = std::env::var_os("COPILOT_HOME");
    copilot_session_root_from(home, configured.as_deref())
}

fn copilot_session_root_from(home: &Path, configured: Option<&std::ffi::OsStr>) -> PathBuf {
    configured
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".copilot"))
        .join("session-state")
}

/// Copilot stores one conversation at `session-state/<id>/events.jsonl`.
/// Global usage events and JSONL inside checkpoints/tool artifacts are not
/// conversation sources, even when they reuse the same filename.
pub(crate) fn is_copilot_source_file(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(is_copilot_source_id)
        && path
            .ancestors()
            .nth(2)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("session-state")
}

fn is_copilot_source_id(value: &str) -> bool {
    !value.starts_with('.')
        && !value.contains(':')
        && validate_identity(value).is_some_and(|id| id == value)
}

/// Metadata fingerprint of the finite source bundle. Copilot's optional
/// workspace sidecar participates because an unchanged event stream can gain
/// a repository association or title when that sidecar changes.
pub(crate) fn source_bundle_fingerprint(path: &Path) -> std::io::Result<SourceFingerprint> {
    let mut fingerprint = SourceFingerprint::from_path(path, &fs::metadata(path)?)?;
    if is_copilot_source_file(path) {
        let mut bundle = Sha256::new();
        bundle.update(b"copilot-source-bundle.v1\0events.jsonl\0");
        bundle.update(fingerprint.len.to_le_bytes());
        bundle.update(fingerprint.modified_unix_nanos.to_le_bytes());
        bundle.update(b"workspace.yaml\0");
        if let Some(sidecar) = path.parent().map(|parent| parent.join("workspace.yaml"))
            && let Ok(metadata) = fs::symlink_metadata(&sidecar)
            && metadata.is_file()
        {
            let companion = SourceFingerprint::from_path(&sidecar, &metadata)?;
            bundle.update(b"present\0");
            bundle.update(companion.len.to_le_bytes());
            bundle.update(companion.modified_unix_nanos.to_le_bytes());
            // Native workspace metadata is small. Hash its bytes inside the
            // same bounded metadata budget, catching preserved-mtime edits
            // too; oversized sidecars still retain independent size+mtime.
            if companion.len <= MAX_HEADER_BYTES as u64 {
                let file =
                    sanitize::open_file_validated(&sidecar).map_err(std::io::Error::other)?;
                let mut bytes = Vec::new();
                file.take(MAX_HEADER_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                bundle.update(b"content\0");
                bundle.update(Sha256::digest(&bytes));
            } else {
                bundle.update(b"oversized-metadata\0");
            }
            fingerprint.len = fingerprint.len.saturating_add(companion.len);
            fingerprint.modified_unix_nanos = fingerprint
                .modified_unix_nanos
                .max(companion.modified_unix_nanos);
        } else {
            bundle.update(b"absent\0");
        }
        fingerprint.bundle_fingerprint = Some(hex::encode(bundle.finalize()));
    }
    Ok(fingerprint)
}

#[derive(Debug, Default)]
pub(crate) struct CopilotMetadata {
    pub cwd: Option<String>,
    pub repository: Option<String>,
    pub title: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// Bounded metadata probe, with the event header taking precedence over the
/// optional YAML sidecar. A malformed sidecar never hides valid events.
pub(crate) fn copilot_metadata(path: &Path) -> CopilotMetadata {
    let mut metadata = CopilotMetadata::default();
    if let Ok(file) = sanitize::open_file_validated(path) {
        let mut reader = BufReader::new(file.take(MAX_HEADER_BYTES as u64));
        for _ in 0..MAX_HEADER_LINES {
            let Ok(Some(line)) = sanitize::read_line_capped(&mut reader, MAX_HEADER_LINE_BYTES)
            else {
                break;
            };
            if line.exceeded {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(line.line.trim()) else {
                continue;
            };
            let event_type = value.get("type").and_then(Value::as_str);
            if !matches!(event_type, Some("session.start" | "session.resume")) {
                continue;
            }
            let data = &value["data"];
            if let Some(cwd) = nonempty_string(&data["context"]["cwd"])
                .or_else(|| nonempty_string(&data["context"]["gitRoot"]))
            {
                metadata.cwd = Some(cwd);
            }
            if let Some(repository) = nonempty_string(&data["context"]["repository"]) {
                metadata.repository = Some(repository);
            }
            if event_type == Some("session.start") && metadata.created_at.is_none() {
                metadata.created_at = nonempty_string(&data["startTime"])
                    .or_else(|| nonempty_string(&value["timestamp"]));
            }
            if event_type == Some("session.resume") {
                metadata.updated_at = nonempty_string(&value["timestamp"]);
            }
        }
    }
    if let Some(sidecar) = path.parent().map(|parent| parent.join("workspace.yaml"))
        && fs::symlink_metadata(&sidecar).is_ok_and(|metadata| metadata.is_file())
        && let Ok(file) = sanitize::open_file_validated(&sidecar)
    {
        let mut bytes = Vec::new();
        if file
            .take(MAX_HEADER_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .is_ok()
            && bytes.len() <= MAX_HEADER_BYTES
            && let Ok(value) = serde_yaml::from_slice::<Value>(&bytes)
        {
            metadata.cwd = metadata.cwd.or_else(|| {
                nonempty_string(&value["cwd"]).or_else(|| nonempty_string(&value["git_root"]))
            });
            metadata.repository = metadata
                .repository
                .or_else(|| nonempty_string(&value["repository"]));
            metadata.title = nonempty_string(&value["name"]);
            metadata.created_at = metadata
                .created_at
                .or_else(|| nonempty_string(&value["created_at"]));
            metadata.updated_at = nonempty_string(&value["updated_at"]).or(metadata.updated_at);
        }
    }
    metadata
}

fn nonempty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopedChildIdentity {
    pub id: String,
    pub parent_id: Option<String>,
}

/// Result of [`SessionCatalog::scan_hot_window`]: walk-only totals plus fully
/// probed sources for the fresh (in-window) candidates only.
#[derive(Debug, Clone)]
pub struct HotWindowScan {
    pub total_candidates: usize,
    pub newest_modified_unix_nanos: Option<u128>,
    pub fresh_sources: Vec<CatalogSource>,
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
    /// Loud identity drift receipt for every non-canonical resolver match.
    /// Consumers must render this or refuse; silently discarding it recreates
    /// the alias-substitution bug this catalog exists to prevent.
    pub substitution_notice: Option<String>,
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
            max_depth: if agent == AgentKind::Copilot {
                1
            } else {
                MAX_SCAN_DEPTH
            },
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
        self.scan_with_stats_and_progress(|_| {})
    }

    /// Rebuild the catalog while exposing live I/O counters to a progress sink.
    ///
    /// The callback is synchronous and must stay cheap. It is invoked after
    /// each visited directory and each probed source so callers can throttle
    /// display updates without guessing whether the scan is still alive.
    pub fn scan_with_stats_and_progress(
        &self,
        mut on_progress: impl FnMut(&CatalogIoStats),
    ) -> CatalogScan {
        let mut stats = CatalogIoStats::default();
        on_progress(&stats);
        let result = self.scan_sources_with_progress(&mut stats, &mut on_progress);
        on_progress(&stats);
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
        //
        // Cursor stores that same filename UUID once per project the
        // conversation touched. Those copies are one session. Every other
        // agent still treats two physical files with one filename UUID as
        // ambiguity — a shared catalog must not collapse unrelated sessions.
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
            if self.agent == AgentKind::Cursor && is_uuid(&query) {
                return self.resolve_cursor_same_id_project_copies(
                    query,
                    &uuid_matches,
                    candidates.len(),
                    stats,
                );
            }
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
                substitution_notice: None,
            });
        }

        let sources = self.probe_candidates(&candidates, stats)?;
        resolve_from_sources(self.agent, query, sources)
    }

    /// Cursor writes `<project>/agent-transcripts/<uuid>/<uuid>.jsonl` for
    /// every project a conversation touched. The filename UUID is one session
    /// id, so an exact full-id query keeps the newest write
    /// (`modified_unix_nanos`, then the lexicographically smaller path when
    /// mtimes tie) and says so. Prefix collisions of different ids never
    /// reach this function.
    fn resolve_cursor_same_id_project_copies(
        &self,
        query: String,
        matches: &[&CandidatePath],
        candidates_scanned: usize,
        stats: &mut CatalogIoStats,
    ) -> Result<ResolvedSource, CatalogError> {
        let copies = matches.len();
        let mut ranked = matches.to_vec();
        ranked.sort_by(|left, right| {
            right
                .fingerprint
                .modified_unix_nanos
                .cmp(&left.fingerprint.modified_unix_nanos)
                .then_with(|| left.path.cmp(&right.path))
        });
        let chosen = ranked[0];
        let source = self
            .probe_candidate(chosen, stats)?
            .ok_or_else(|| CatalogError::Missing {
                query: query.clone(),
                agent: self.agent,
                candidates_scanned,
            })?;
        let kept = source.path.display().to_string();
        Ok(ResolvedSource {
            query: query.clone(),
            matched_by: MatchKind::ExactSourceId,
            source,
            substitution_notice: Some(format!(
                "substituted: {query} kept newest of {copies} cursor project copies → {kept}"
            )),
        })
    }

    fn scan_sources_with_progress(
        &self,
        stats: &mut CatalogIoStats,
        on_progress: &mut dyn FnMut(&CatalogIoStats),
    ) -> Result<Vec<CatalogSource>, CatalogError> {
        let candidates = self.collect_candidate_paths_with_progress(stats, on_progress)?;
        self.probe_candidates_with_progress(&candidates, stats, on_progress)
    }

    fn collect_candidate_paths(
        &self,
        stats: &mut CatalogIoStats,
    ) -> Result<Vec<CandidatePath>, CatalogError> {
        self.collect_candidate_paths_with_progress(stats, &mut |_| {})
    }

    fn collect_candidate_paths_with_progress(
        &self,
        stats: &mut CatalogIoStats,
        on_progress: &mut dyn FnMut(&CatalogIoStats),
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
                    || !self.agent.is_primary_source_file(&path)
                {
                    continue;
                }
                paths.insert(path);
            }
            stats.metadata_candidates = paths.len();
            on_progress(stats);
        }

        let mut candidates = Vec::with_capacity(paths.len());
        for path in paths {
            let fingerprint = match source_bundle_fingerprint(&path) {
                Ok(fingerprint) => fingerprint,
                Err(error) if self.agent == AgentKind::Copilot => {
                    return Err(CatalogError::Io {
                        path,
                        message: error.to_string(),
                    });
                }
                Err(_) => {
                    stats.rejected_paths += 1;
                    continue;
                }
            };
            let mut filename_aliases = if self.agent == AgentKind::Copilot {
                // Every Copilot source is called events.jsonl; exposing that
                // stem as an alias would make unrelated sessions ambiguous.
                Vec::new()
            } else {
                filename_aliases(&path)
            };
            let filename_uuid = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(uuid_from_filename)
                .map(str::to_ascii_lowercase)
                .or_else(|| {
                    // Grok layout: `…/<cwd-encoded>/<session-uuid>/chat_history.jsonl`.
                    // The physical session id lives on the parent directory, not the
                    // chat filename stem — surface it so ExactSourceId resolves.
                    if self.agent == AgentKind::Grok {
                        path.parent()
                            .and_then(|parent| parent.file_name())
                            .and_then(|name| name.to_str())
                            .filter(|name| is_uuid(name))
                            .map(|name| name.to_ascii_lowercase())
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    (self.agent == AgentKind::Copilot)
                        .then(|| {
                            path.parent()
                                .and_then(Path::file_name)
                                .and_then(|name| name.to_str())
                                .filter(|name| is_copilot_source_id(name))
                                .map(str::to_owned)
                        })
                        .flatten()
                })
                .or_else(|| {
                    // Kimi layout: `…/wd_<slug>_<hex>/session_<uuid>/agents/<agentId>/wire.jsonl`.
                    // The session uuid lives three directories up and every lane
                    // shares it, so the main lane claims the bare uuid while
                    // subagent lanes take the scoped `<uuid>:<agentId>` form —
                    // an ExactSourceId query for the session uuid lands on the
                    // operator conversation, never ambiguous across lanes.
                    (self.agent == AgentKind::Kimi)
                        .then(|| kimi_source_identity(&path))
                        .flatten()
                })
                .or_else(|| {
                    // Junie layout: `…/sessions/session-<id>/events.jsonl`. The
                    // session id lives on the parent directory behind a
                    // `session-` prefix, and the bare `<id>` is what junie
                    // tooling and `aicx sessions list` print — so it must
                    // resolve as ExactSourceId (round-trip contract: every id
                    // the catalog surface prints is accepted back).
                    (self.agent == AgentKind::Junie)
                        .then(|| junie_source_identity(&path))
                        .flatten()
                });
            if let Some(ref uuid) = filename_uuid {
                filename_aliases.push(uuid.clone());
            }
            if self.agent == AgentKind::Junie
                && let Some(session_dir) = path
                    .parent()
                    .and_then(|parent| parent.file_name())
                    .and_then(|name| name.to_str())
                    .filter(|name| name.starts_with("session-"))
                    .and_then(validate_identity)
            {
                // The prefixed directory name stays a paste-friendly alias.
                filename_aliases.push(session_dir);
            }
            dedupe_ordered(&mut filename_aliases);
            if filename_aliases.is_empty() {
                stats.rejected_paths += 1;
                continue;
            }
            candidates.push(CandidatePath {
                path,
                filename_aliases,
                filename_uuid,
                fingerprint,
            });
            on_progress(stats);
        }
        candidates.sort_by(|left, right| left.path.cmp(&right.path));
        stats.metadata_candidates = candidates.len();
        on_progress(stats);
        Ok(candidates)
    }

    fn probe_candidates(
        &self,
        candidates: &[CandidatePath],
        stats: &mut CatalogIoStats,
    ) -> Result<Vec<CatalogSource>, CatalogError> {
        self.probe_candidates_with_progress(candidates, stats, &mut |_| {})
    }

    /// Hot-window scan: walk + stat every candidate (cheap), but open bounded
    /// headers ONLY for candidates whose mtime falls inside the window. The
    /// full probe pass costs minutes on real roots; a hot query cannot pay it.
    pub fn scan_hot_window(&self, cutoff_unix_ns: u128) -> Result<HotWindowScan, CatalogError> {
        self.scan_hot_window_skipping(cutoff_unix_ns, &|_, _| false)
    }

    /// The same walk, with the caller allowed to declare a candidate already
    /// known — a path whose fingerprint the census already holds unchanged.
    ///
    /// Freshness alone is not a reason to re-read: on a warm census most of
    /// the window is sessions nothing has touched since the last pass, and
    /// their bounded header re-derives byte-identical identity. Skipping them
    /// is what separates a hot refresh from a rebuild. They still count into
    /// `total_candidates` — the walk did see them.
    pub fn scan_hot_window_skipping(
        &self,
        cutoff_unix_ns: u128,
        is_known: &dyn Fn(&Path, &SourceFingerprint) -> bool,
    ) -> Result<HotWindowScan, CatalogError> {
        let mut stats = CatalogIoStats::default();
        let candidates = self.collect_candidate_paths(&mut stats)?;
        let total_candidates = candidates.len();
        let newest_modified_unix_nanos = candidates
            .iter()
            .map(|candidate| candidate.fingerprint.modified_unix_nanos)
            .max();
        let fresh: Vec<CandidatePath> = candidates
            .into_iter()
            .filter(|candidate| candidate.fingerprint.modified_unix_nanos >= cutoff_unix_ns)
            .filter(|candidate| !is_known(&candidate.path, &candidate.fingerprint))
            .collect();
        let fresh_sources = self.probe_candidates(&fresh, &mut stats)?;
        Ok(HotWindowScan {
            total_candidates,
            newest_modified_unix_nanos,
            fresh_sources,
        })
    }

    fn probe_candidates_with_progress(
        &self,
        candidates: &[CandidatePath],
        stats: &mut CatalogIoStats,
        on_progress: &mut dyn FnMut(&CatalogIoStats),
    ) -> Result<Vec<CatalogSource>, CatalogError> {
        let mut sources = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            match self.probe_candidate(candidate, stats) {
                Ok(Some(source)) => sources.push(source),
                Err(error @ CatalogError::Io { .. }) if self.agent == AgentKind::Copilot => {
                    return Err(error);
                }
                Ok(None) | Err(CatalogError::Io { .. }) => stats.rejected_paths += 1,
                Err(error) => return Err(error),
            }
            on_progress(stats);
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

pub(crate) fn resolve_from_sources(
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
        let source = matches[0].clone();
        let substitution_notice = matches!(
            kind,
            MatchKind::ExactAlias
                | MatchKind::ExactFilenameAlias
                | MatchKind::UuidSuffix
                | MatchKind::UniquePrefix
        )
        .then(|| format!("substituted: {query} → {}", source.source_id));
        return Ok(ResolvedSource {
            query,
            matched_by: kind,
            source,
            substitution_notice,
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

    if agent == AgentKind::Copilot {
        // Event ids and parentId form an event chain, not session identity.
        // Only session.start may assert the logical root id.
        if object.get("type").and_then(Value::as_str) == Some("session.start")
            && let Some(id) = object
                .get("data")
                .and_then(|data| data.get("sessionId"))
                .and_then(Value::as_str)
                .and_then(validate_identity)
        {
            root_ids.push(id);
        }
        return;
    }

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

/// Physical source identity for a Kimi `wire.jsonl`: the session uuid from
/// the `session_<uuid>` grandparent directory, scoped by the agent lane from
/// the parent directory. `agents/main` is the operator conversation and owns
/// the bare uuid; every other lane keeps its own append-only wire and gets
/// the `<uuid>:<agentId>` identity.
/// Physical source identity for a Junie session stream: the `<id>` from the
/// `session-<id>` parent directory. Junie addresses sessions by that bare id
/// (its own directory prefix is decoration), so the catalog resolves it as
/// ExactSourceId instead of demanding an id no surface ever prints.
fn junie_source_identity(path: &Path) -> Option<String> {
    let session_dir = path.parent()?.file_name()?.to_str()?;
    validate_identity(session_dir.strip_prefix("session-")?)
}

fn kimi_source_identity(path: &Path) -> Option<String> {
    // `…/wd_<slug>_<hex>/session_<uuid>/agents/<agentId>/wire.jsonl` — the
    // session uuid lives three directories up, behind a mandatory `agents/`
    // level that separates the operator lane (`main`) from subagent lanes.
    let agent_dir = path.parent()?.file_name()?.to_str()?;
    let agents_dir = path.parent()?.parent()?.file_name()?.to_str()?;
    if agents_dir != "agents" {
        return None;
    }
    let session_dir = path.parent()?.parent()?.parent()?.file_name()?.to_str()?;
    let uuid = session_dir.strip_prefix("session_")?;
    if !is_uuid(uuid) || validate_identity(agent_dir).is_none_or(|id| id != agent_dir) {
        return None;
    }
    let uuid = uuid.to_ascii_lowercase();
    if agent_dir == "main" {
        Some(uuid)
    } else {
        Some(format!("{uuid}:{agent_dir}"))
    }
}

pub(crate) use crate::uuid_shape::is_uuid;

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

#[cfg(test)]
mod copilot_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const ID: &str = "12345678-1234-1234-1234-123456789abc";

    fn root() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir()
            .join(format!(
                "aicx-copilot-catalog-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ))
            .join(".copilot")
            .join("session-state");
        fs::create_dir_all(root.join(ID)).unwrap();
        root
    }

    fn event(root: &Path) -> PathBuf {
        let path = root.join(ID).join("events.jsonl");
        fs::write(&path, format!("{{\"type\":\"session.start\",\"id\":\"event-id\",\"data\":{{\"sessionId\":\"{ID}\",\"context\":{{\"cwd\":\"/repo/current\"}}}}}}\n")).unwrap();
        path
    }

    #[test]
    fn copilot_catalog_admits_only_direct_conversation_events() {
        let root = root();
        let events = event(&root);
        let nested = root.join(ID).join("checkpoints").join(ID);
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("events.jsonl"), "{}\n").unwrap();
        fs::write(root.join(ID).join("tool.jsonl"), "{}\n").unwrap();
        let global = root.parent().unwrap().join("events.jsonl");
        fs::write(&global, "{}\n").unwrap();
        assert!(!is_copilot_source_file(&global));
        let scan = SessionCatalog::new(AgentKind::Copilot, &root)
            .unwrap()
            .scan_with_stats();
        let sources = scan.result.unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].path, events.canonicalize().unwrap());
        assert_eq!(sources[0].source_id, ID);
        assert_eq!(sources[0].logical_session_id.as_deref(), Some(ID));
        assert!(
            !sources[0]
                .filename_aliases
                .iter()
                .any(|alias| alias == "events" || alias == "events.jsonl")
        );
        assert_eq!(scan.stats.body_reads, 0);
        assert_eq!(scan.stats.files_opened, 1);
        fs::remove_dir_all(root.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn copilot_directory_identity_resolves_exact_and_prefix_despite_logical_drift() {
        let root = root();
        let events = event(&root);
        fs::write(&events, "{\"type\":\"session.start\",\"id\":\"event-id\",\"data\":{\"sessionId\":\"logical-alias\"}}\n").unwrap();
        let catalog = SessionCatalog::new(AgentKind::Copilot, &root).unwrap();
        let exact = catalog.resolve(ID).unwrap();
        assert_eq!(exact.matched_by, MatchKind::ExactSourceId);
        assert_eq!(exact.source.source_id, ID);
        assert_eq!(
            catalog.resolve("12345678").unwrap().matched_by,
            MatchKind::UniquePrefix
        );
        assert_eq!(
            catalog.resolve("logical-alias").unwrap().matched_by,
            MatchKind::ExactLogicalId
        );
        assert!(matches!(
            catalog.resolve("events"),
            Err(CatalogError::Missing { .. })
        ));
        fs::remove_dir_all(root.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn copilot_optional_sidecar_falls_back_and_malformed_yaml_preserves_events() {
        let root = root();
        let events = event(&root);
        let sidecar = events.parent().unwrap().join("workspace.yaml");
        fs::write(&sidecar, "cwd: /repo/stale\nname: session title\nrepository: owner/repo\ncreated_at: '2026-06-01T12:00:00Z'\n").unwrap();
        let metadata = copilot_metadata(&events);
        assert_eq!(metadata.cwd.as_deref(), Some("/repo/current"));
        assert_eq!(metadata.repository.as_deref(), Some("owner/repo"));
        assert_eq!(metadata.title.as_deref(), Some("session title"));
        fs::write(&sidecar, "cwd: [unterminated\n").unwrap();
        let metadata = copilot_metadata(&events);
        assert_eq!(metadata.cwd.as_deref(), Some("/repo/current"));
        assert!(metadata.title.is_none());
        assert_eq!(
            SessionCatalog::new(AgentKind::Copilot, &root)
                .unwrap()
                .scan_with_stats()
                .result
                .unwrap()
                .len(),
            1
        );
        fs::remove_dir_all(root.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn copilot_bundle_fingerprint_observes_sidecar_edit_and_removal() {
        let root = root();
        let events = event(&root);
        let only_events = source_bundle_fingerprint(&events).unwrap();
        let sidecar = events.parent().unwrap().join("workspace.yaml");
        fs::write(&sidecar, "name: first\n").unwrap();
        let before = source_bundle_fingerprint(&events).unwrap();
        assert_ne!(before, only_events);
        fs::write(&sidecar, "name: other\n").unwrap();
        filetime::set_file_mtime(
            &sidecar,
            filetime::FileTime::from_unix_time(
                (before.modified_unix_nanos / 1_000_000_000) as i64 + 2,
                0,
            ),
        )
        .unwrap();
        let after = source_bundle_fingerprint(&events).unwrap();
        assert_eq!(before.len, after.len);
        assert_ne!(before, after);
        fs::remove_file(&sidecar).unwrap();
        assert_eq!(source_bundle_fingerprint(&events).unwrap(), only_events);
        fs::remove_dir_all(root.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn copilot_bundle_retains_sidecar_evidence_under_future_event_mtime() {
        let root = root();
        let events = event(&root);
        filetime::set_file_mtime(
            &events,
            filetime::FileTime::from_unix_time(2_000_000_000, 0),
        )
        .unwrap();
        let sidecar = events.parent().unwrap().join("workspace.yaml");
        fs::write(&sidecar, "name: first\n").unwrap();
        filetime::set_file_mtime(
            &sidecar,
            filetime::FileTime::from_unix_time(1_700_000_000, 0),
        )
        .unwrap();
        let before = source_bundle_fingerprint(&events).unwrap();
        fs::write(&sidecar, "name: other\n").unwrap();
        filetime::set_file_mtime(
            &sidecar,
            filetime::FileTime::from_unix_time(1_700_000_001, 0),
        )
        .unwrap();
        let after = source_bundle_fingerprint(&events).unwrap();
        assert_eq!(
            before.len, after.len,
            "size is the physical total, not an encoded hash"
        );
        assert_eq!(
            before.modified_unix_nanos, after.modified_unix_nanos,
            "recency remains the newest artifact timestamp"
        );
        assert_ne!(before.bundle_fingerprint, after.bundle_fingerprint);
        fs::write(&sidecar, "name: third\n").unwrap();
        filetime::set_file_mtime(
            &sidecar,
            filetime::FileTime::from_unix_time(1_700_000_001, 0),
        )
        .unwrap();
        let preserved_mtime = source_bundle_fingerprint(&events).unwrap();
        assert_eq!(after.len, preserved_mtime.len);
        assert_eq!(
            after.modified_unix_nanos,
            preserved_mtime.modified_unix_nanos
        );
        assert_ne!(
            after.bundle_fingerprint, preserved_mtime.bundle_fingerprint,
            "bounded workspace content hash also observes a preserved-mtime edit"
        );
        fs::remove_dir_all(root.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn physical_identity_detects_same_size_restored_mtime_and_atomic_replace() {
        let root = root();
        let events = event(&root);
        fs::write(&events, b"aaaa\n").unwrap();
        let before = source_bundle_fingerprint(&events).unwrap();
        let pinned_mtime =
            filetime::FileTime::from_last_modification_time(&fs::metadata(&events).unwrap());

        fs::write(&events, b"bbbb\n").unwrap();
        filetime::set_file_mtime(&events, pinned_mtime).unwrap();
        let rewritten = source_bundle_fingerprint(&events).unwrap();
        assert_eq!(before.len, rewritten.len);
        assert_eq!(before.modified_unix_nanos, rewritten.modified_unix_nanos);
        assert_ne!(
            before.physical_identity, rewritten.physical_identity,
            "same-size restored-mtime rewrite must move physical identity"
        );

        #[cfg(unix)]
        {
            let replacement = events.with_extension("replacement");
            fs::write(&replacement, b"bbbb\n").unwrap();
            filetime::set_file_mtime(&replacement, pinned_mtime).unwrap();
            fs::rename(&replacement, &events).unwrap();
            let replaced = source_bundle_fingerprint(&events).unwrap();
            assert_eq!(rewritten.len, replaced.len);
            assert_eq!(rewritten.modified_unix_nanos, replaced.modified_unix_nanos);
            assert_ne!(
                rewritten.physical_identity, replaced.physical_identity,
                "atomic replacement must move inode/ctime identity"
            );
        }

        fs::remove_dir_all(root.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn copilot_missing_selected_source_is_io_error_while_empty_root_is_empty() {
        let root = root();
        let events = event(&root);
        let catalog = SessionCatalog::new(AgentKind::Copilot, &root).unwrap();
        let mut stats = CatalogIoStats::default();
        let candidates = catalog.collect_candidate_paths(&mut stats).unwrap();
        fs::remove_file(events).unwrap();
        assert!(matches!(
            catalog.probe_candidates(&candidates, &mut stats),
            Err(CatalogError::Io { .. })
        ));
        assert!(catalog.scan_with_stats().result.unwrap().is_empty());
        fs::remove_dir_all(root.parent().unwrap().parent().unwrap()).unwrap();
    }

    #[test]
    fn copilot_provider_roots_and_aliases_are_canonical() {
        for alias in [
            "copilot",
            "copilot-cli",
            "github-copilot",
            "github-copilot-cli",
        ] {
            assert_eq!(AgentKind::parse(alias), Some(AgentKind::Copilot));
        }
        assert_eq!(
            copilot_session_root_from(Path::new("/home/user"), None),
            PathBuf::from("/home/user/.copilot/session-state")
        );
        assert_eq!(
            copilot_session_root_from(Path::new("/home/user"), Some(std::ffi::OsStr::new(""))),
            PathBuf::from("/home/user/.copilot/session-state")
        );
        assert_eq!(
            copilot_session_root_from(
                Path::new("/home/user"),
                Some(std::ffi::OsStr::new("/custom/copilot-home"))
            ),
            PathBuf::from("/custom/copilot-home/session-state")
        );
        assert!(AgentKind::ALL.contains(&AgentKind::Copilot));
    }

    #[test]
    fn copilot_sdk_named_session_id_is_physical_identity_for_exact_and_prefix_lookup() {
        let root = root();
        let named = "user-123-task-456";
        let directory = root.join(named);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("events.jsonl"),
            format!("{{\"type\":\"session.start\",\"data\":{{\"sessionId\":\"{named}\"}}}}\n"),
        )
        .unwrap();
        let catalog = SessionCatalog::new(AgentKind::Copilot, &root).unwrap();
        let exact = catalog.resolve(named).unwrap();
        assert_eq!(exact.source.source_id, named);
        assert_eq!(exact.matched_by, MatchKind::ExactSourceId);
        assert_eq!(catalog.resolve("user-123").unwrap().source.source_id, named);
        assert!(is_copilot_source_file(&directory.join("events.jsonl")));
        assert!(!is_copilot_source_file(
            &root.join("unsafe:id").join("events.jsonl")
        ));
        assert!(!is_copilot_source_file(
            &root.join(".hidden").join("events.jsonl")
        ));
        fs::remove_dir_all(root.parent().unwrap().parent().unwrap()).unwrap();
    }
}
