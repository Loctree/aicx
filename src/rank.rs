//! Per-chunk content quality scoring and fuzzy-search presentation helpers.
//!
//! Scores each chunk file on a 0–10 scale based on signal density,
//! penalizing noise patterns (echoed skill prompts, tool JSON, system
//! reminders) and rewarding actionable content (decisions, TODOs,
//! architecture changes, bug findings).
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use serde::Serialize;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use crate::oracle::OracleStatus;
use crate::sanitize;
use crate::sanitize::normalize_query;
use crate::store;
use crate::timeline::FrameKind;

// ============================================================================
// Noise patterns — lines that inflate chunk size without adding value
// ============================================================================

/// Line prefixes that are always noise (case-insensitive check).
const NOISE_PREFIXES: &[&str] = &[
    "<command-message>",
    "<command-name>",
    "<command-args>",
    "</command-args>",
    "<system-reminder>",
    "</system-reminder>",
    "<available-deferred-tools>",
    "</available-deferred-tools>",
    "base directory for this skill:",
    "arguments:",
    "launching skill:",
    "tool loaded.",
    "human:",
];

/// Substrings that indicate noise anywhere in a line (case-insensitive).
const NOISE_CONTAINS: &[&str] = &[
    "<task-notification>",
    "tool-results/",
    "persisted-output>",
    "output too large",
    "full output saved to:",
    "preview (first",
    "ran command",
    "ran find",
    "called loctree",
    "killed process",
    "background command",
    "task killed",
    "task update",
    "task-notification",
    "mcp__loctree__",
    "mcp__plugin_",
    "mcp__unicode",
    "mcp__youtube",
    "mcp__claude_ai_",
    "antml:invoke",
    "antml:parameter",
    "antml:function_calls",
    "function_results",
    "\"$schema\":",
    "additionalproperties",
];

/// Markdown headers that indicate echoed skill documentation (case-insensitive).
const SKILL_BOILERPLATE_HEADERS: &[&str] = &[
    "## when to use",
    "## anti-patterns",
    "## fallback",
    "## quick reference",
    "## pipeline overview",
    "## notes",
    "## additional resources",
    "## phase gate",
    "## audit sequence",
    "## the undone matrix",
    "## init sequence",
    "## for subagent prompts",
    "## phase skipping",
    "## spawn pattern",
    "## research sources",
    "## query strategy",
    "## required steps",
    "## how to access skills",
    "## platform adaptation",
    "## skill types",
    "## skill priority",
    "## red flags",
    "## the rule",
    "### step 1:",
    "### step 2:",
    "### step 3:",
    "### step 4:",
    "### output:",
    "### phase gate",
    "### required steps",
    "### agent plan template",
    "### review",
    "### research sources",
];

/// Footers/signatures that are boilerplate.
const BOILERPLATE_FOOTERS: &[&str] = &[
    "created by vetcoders",
    "vibecrafted with ai agents",
    "*created by vetcoders",
    "*vibecrafted with",
];

// ============================================================================
// Signal patterns — lines containing actionable content
// ============================================================================

/// Substrings that indicate genuine signal (case-insensitive).
const SIGNAL_CONTAINS: &[&str] = &[
    // Decisions & architecture
    "decision:",
    "[decision]",
    "architecture",
    "breaking change",
    "migration",
    "refactor",
    // Tasks & tracking
    "todo:",
    "fixme:",
    "- [ ]",
    "- [x]",
    // Bugs & errors
    "bug:",
    "error:",
    "fix:",
    "broke",
    "regression",
    "panic",
    "crash",
    " failed",
    "test failed",
    "check failed",
    // Git & deployment
    "git commit",
    " committed",
    "commit ",
    "git merge",
    "merge pr",
    " merged",
    "pr #",
    "deploy",
    "release",
    "tag v",
    "git rm",
    "git push",
    // Quality & scoring
    "score:",
    "p0=",
    "p1=",
    "p2=",
    "/100",
    " passed",
    "tests pass",
    "all pass",
    "check pass",
    "clippy",
    "semgrep",
    "cargo test",
    "cargo check",
    // Outcomes
    "[skill_outcome]",
    "outcome:",
    "validation:",
    "smoke test",
    // User intent (Polish + English)
    "chcę",
    "chce ",
    "zróbmy",
    "zrobmy",
    "proponuję",
    "proponuje",
    "następny krok",
    "nastepny krok",
    "let's",
    "i want",
    "next step",
    "plan:",
];

/// Lines that are signal if they appear as the ONLY content (short, punchy).
const SIGNAL_PREFIXES: &[&str] = &[
    "insight",
    "★ insight",
    "ultrathink",
    "plan mode",
    "accept plan",
    "user accepted",
];

// ============================================================================
// Scoring
// ============================================================================

/// Classification for a single line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineClass {
    Signal,
    Noise,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpeakerRole {
    Operator,
    Assistant,
    Tool,
    Unknown,
}

/// Score result for a single chunk file.
#[derive(Debug, Clone)]
pub struct ChunkScore {
    /// Computed quality score 0–10.
    pub score: u8,
    /// Number of lines classified as signal.
    pub signal_lines: usize,
    /// Number of lines classified as noise.
    pub noise_lines: usize,
    /// Total non-empty lines.
    pub total_lines: usize,
    /// Signal density (signal / total), 0.0–1.0.
    pub density: f32,
    /// Human label.
    pub label: &'static str,
}

/// Candidate threshold for semantic intent clustering. The embedding score is
/// deliberately only a candidate signal: overlay policy still applies its
/// pair-relative negation/contradiction veto before any merge.
pub const SEMANTIC_INTENT_CANDIDATE_THRESHOLD: f32 = 0.82;

/// Rank two intent embeddings using the shared AICX embedding similarity
/// implementation. Keeping the threshold in the rank layer gives search and
/// overlay one explicit scoring owner while leaving merge policy to overlay.
pub fn intent_candidate_similarity(left: &[f32], right: &[f32]) -> f32 {
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    {
        aicx_embeddings::similarity(left, right)
    }
    #[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
    {
        let _ = (left, right);
        0.0
    }
}

/// Shared fuzzy-search result for a stored chunk.
#[derive(Debug, Clone, Serialize)]
pub struct FuzzyResult {
    pub file: String,
    pub path: String,
    pub project: String,
    pub kind: String,
    pub frame_kind: Option<String>,
    pub agent: String,
    pub date: String,
    pub timestamp: Option<String>,
    pub score: u8,
    pub label: String,
    pub density: f32,
    pub matched_lines: Vec<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Serialize)]
struct CompactSearchResponse {
    oracle_status: OracleStatus,
    results: usize,
    scanned: usize,
    items: Vec<CompactSearchItem>,
}

#[derive(Debug, Serialize)]
struct CompactSearchItem {
    score: u8,
    label: String,
    project: String,
    kind: String,
    agent: String,
    date: String,
    timestamp: Option<String>,
    frame_kind: Option<String>,
    session: String,
    session_id: String,
    cwd: String,
    matches: Vec<String>,
    path: String,
}

const SEARCH_MATCH_MAX_CHARS: usize = 200;
const SEARCH_METADATA_PREFIX: &str = "[metadata]";
const SEARCH_GENERATED_METADATA_PREFIXES: &[&str] = &[
    "[frame_kind:",
    "source project:",
    "canonical test project:",
    "tb artifact:",
    "round status:",
];
const METADATA_CANDIDATE_FLOOR: usize = 200;
const METADATA_CANDIDATE_MULTIPLIER: usize = 100;

pub fn search_oracle_status(root: &Path, results: &[FuzzyResult], scanned: usize) -> OracleStatus {
    OracleStatus::filesystem_fuzzy(
        root,
        scanned,
        results.len(),
        crate::oracle::verify_paths(
            results
                .iter()
                .map(|result| Path::new(&result.path).to_path_buf()),
        ),
    )
}

pub fn render_search_json(
    root: &Path,
    results: &[FuzzyResult],
    scanned: usize,
) -> serde_json::Result<String> {
    render_search_json_with_oracle(
        root,
        results,
        scanned,
        search_oracle_status(root, results, scanned),
    )
}

pub fn render_search_json_with_oracle(
    _root: &Path,
    results: &[FuzzyResult],
    scanned: usize,
    oracle_status: OracleStatus,
) -> serde_json::Result<String> {
    let items = results
        .iter()
        .map(|result| CompactSearchItem {
            score: result.score,
            label: result.label.clone(),
            project: result.project.clone(),
            kind: result.kind.clone(),
            agent: result.agent.clone(),
            date: result.date.clone(),
            timestamp: result.timestamp.clone(),
            frame_kind: result.frame_kind.clone(),
            session: result.session_id.clone().unwrap_or_else(|| "-".to_string()),
            session_id: result.session_id.clone().unwrap_or_else(|| "-".to_string()),
            cwd: result.cwd.clone().unwrap_or_else(|| "-".to_string()),
            matches: display_search_matches(result),
            path: result.path.clone(),
        })
        .collect();

    serde_json::to_string(&CompactSearchResponse {
        oracle_status,
        results: results.len(),
        scanned,
        items,
    })
}

pub fn render_search_text(results: &[FuzzyResult], color: bool) -> String {
    let mut out = String::new();

    for result in results {
        let session_str = result.session_id.as_deref().unwrap_or("-");
        let cwd_str = result.cwd.as_deref().unwrap_or("-");
        let frame_str = result.frame_kind.as_deref().unwrap_or("-");
        let matches = display_search_matches(result);

        if color {
            let score_color = match result.label.as_str() {
                "HIGH" => "\x1b[1;32m",
                "MEDIUM" => "\x1b[1;33m",
                _ => "\x1b[1;31m",
            };
            let _ = writeln!(
                out,
                "{score_color}[{}/100 {}]\x1b[0m \x1b[1;36m{}\x1b[0m | \x1b[35m{}\x1b[0m | \x1b[90m{}\x1b[0m",
                result.score, result.label, result.project, result.agent, result.date
            );
            let _ = writeln!(out, "session(s): \x1b[90m{session_str}\x1b[0m");
            let _ = writeln!(out, "cwd: \x1b[90m{cwd_str}\x1b[0m");
            let _ = writeln!(out, "frame_kind: \x1b[90m{frame_str}\x1b[0m");
            let _ = writeln!(out, "search result:");
            for line in &matches {
                let _ = writeln!(out, "  \x1b[90m>\x1b[0m \x1b[90m{}\x1b[0m", line);
            }
            let _ = writeln!(out, "source file(s):");
            let _ = writeln!(out, "\x1b[90;4m{}\x1b[0m", result.path);
            let _ = writeln!(out);
        } else {
            let _ = writeln!(
                out,
                "[{}/100 {}] {} | {} | {}",
                result.score, result.label, result.project, result.agent, result.date
            );
            let _ = writeln!(out, "session(s): {session_str}");
            let _ = writeln!(out, "cwd: {cwd_str}");
            let _ = writeln!(out, "frame_kind: {frame_str}");
            let _ = writeln!(out, "search result:");
            for line in &matches {
                let _ = writeln!(out, "  > {}", line);
            }
            let _ = writeln!(out, "source file(s):");
            let _ = writeln!(out, "{}", result.path);
            let _ = writeln!(out);
        }
    }

    out
}

fn display_search_matches(result: &FuzzyResult) -> Vec<String> {
    let mut lines = result
        .matched_lines
        .iter()
        .filter(|line| !is_search_metadata_line(line))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines = result.matched_lines.iter().collect();
    }
    lines
        .into_iter()
        .map(|line| truncate_search_match(line, SEARCH_MATCH_MAX_CHARS))
        .collect()
}

fn is_search_metadata_line(line: &str) -> bool {
    let trimmed = line.trim();
    let lower = trimmed.to_lowercase();
    crate::card_header::is_bracket_header_line(trimmed)
        || trimmed.starts_with(SEARCH_METADATA_PREFIX)
        || SEARCH_GENERATED_METADATA_PREFIXES
            .iter()
            .any(|prefix| lower.starts_with(prefix))
}

fn truncate_search_match(line: &str, max_chars: usize) -> String {
    let mut truncated: String = line.chars().take(max_chars).collect();
    if line.chars().count() > max_chars {
        truncated.push_str(" ...");
    }
    truncated
}

fn select_search_candidates(
    files: Vec<store::StoredContextFile>,
    query_terms: &[&str],
    limit: usize,
) -> Vec<store::StoredContextFile> {
    if query_terms.is_empty() {
        return files;
    }

    let mut scored = files
        .iter()
        .enumerate()
        .filter_map(|(idx, file)| {
            let score = metadata_match_count(file, query_terms);
            (score > 0).then_some((idx, score))
        })
        .collect::<Vec<_>>();

    if scored.is_empty() {
        return files;
    }

    let metadata_only_query = query_terms
        .iter()
        .any(|term| is_generic_metadata_query_term(term));

    scored.sort_by(|(left_idx, left_score), (right_idx, right_score)| {
        right_score
            .cmp(left_score)
            .then_with(|| {
                files[*right_idx]
                    .date_compact
                    .cmp(&files[*left_idx].date_compact)
            })
            .then_with(|| files[*right_idx].chunk.cmp(&files[*left_idx].chunk))
    });

    let cap = if metadata_only_query {
        limit
            .saturating_mul(METADATA_CANDIDATE_MULTIPLIER)
            .max(METADATA_CANDIDATE_FLOOR)
    } else {
        files.len()
    };
    scored
        .into_iter()
        .take(cap)
        .map(|(idx, _)| files[idx].clone())
        .collect()
}

fn metadata_match_count(file: &store::StoredContextFile, query_terms: &[&str]) -> usize {
    let metadata = metadata_search_text(file);
    query_terms
        .iter()
        .filter(|term| metadata.contains(**term))
        .count()
}

fn metadata_search_text(file: &store::StoredContextFile) -> String {
    normalize_query(&format!(
        "{} {} {} {} {} {}",
        file.project,
        file.agent,
        file.kind.dir_name(),
        file.date_iso,
        file.path.file_name().unwrap_or_default().to_string_lossy(),
        file.path.display()
    ))
}

fn metadata_matched_lines(
    file: &store::StoredContextFile,
    metadata_text: &str,
    query_terms: &[&str],
) -> Vec<String> {
    if !query_terms.iter().any(|term| metadata_text.contains(*term)) {
        return Vec::new();
    }

    metadata_line(file)
}

fn metadata_line(file: &store::StoredContextFile) -> Vec<String> {
    vec![format!(
        "[metadata] project: {} | agent: {} | date: {} | kind: {} | path: {}",
        file.project,
        file.agent,
        file.date_iso,
        file.kind.dir_name(),
        file.path.display()
    )]
}

fn metadata_covers_query(metadata_text: &str, query_terms: &[&str]) -> bool {
    let required_terms = query_terms
        .iter()
        .filter(|term| !is_generic_metadata_query_term(term))
        .collect::<Vec<_>>();
    !required_terms.is_empty()
        && required_terms
            .iter()
            .all(|term| metadata_text.contains(**term))
}

fn metadata_only_result(
    stored_file: store::StoredContextFile,
    metadata_text: &str,
    query_terms: &[&str],
) -> FuzzyResult {
    let matched_lines = {
        let lines = metadata_matched_lines(&stored_file, metadata_text, query_terms);
        if lines.is_empty() {
            metadata_line(&stored_file)
        } else {
            lines
        }
    };
    FuzzyResult {
        file: stored_file
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        path: stored_file.path.display().to_string(),
        project: stored_file.project,
        kind: stored_file.kind.dir_name().to_string(),
        frame_kind: None,
        agent: stored_file.agent,
        date: stored_file.date_iso,
        timestamp: None,
        score: 90,
        label: "HIGH".to_string(),
        density: 1.0,
        matched_lines,
        session_id: Some(stored_file.session_id),
        cwd: None,
    }
}

fn infer_project_filter_from_query(store_root: &Path, query_terms: &[&str]) -> Option<String> {
    let tokens = project_hint_tokens(query_terms);
    if tokens.is_empty() {
        return None;
    }

    let canonical_root = store_root.join(store::CANONICAL_STORE_DIRNAME);
    let mut scores: HashMap<String, u8> = HashMap::new();

    let Ok(org_entries) = fs::read_dir(canonical_root) else {
        return None;
    };

    for org_entry in org_entries.flatten() {
        let org_path = org_entry.path();
        if !org_path.is_dir() {
            continue;
        }
        let org = org_entry.file_name().to_string_lossy().to_string();
        let Ok(repo_entries) = fs::read_dir(&org_path) else {
            continue;
        };
        for repo_entry in repo_entries.flatten() {
            let repo_path = repo_entry.path();
            if !repo_path.is_dir() {
                continue;
            }
            let repo = repo_entry.file_name().to_string_lossy().to_string();
            let slug = format!("{org}/{repo}");
            let haystacks = [
                normalize_query(&org),
                normalize_query(&repo),
                normalize_query(&slug),
            ];
            let compact_haystacks = haystacks
                .iter()
                .map(|value| compact_project_token(value))
                .collect::<Vec<_>>();

            let mut best_score = 0u8;
            for token in &tokens {
                let compact_token = compact_project_token(token);
                for haystack in &haystacks {
                    if haystack == token {
                        best_score = best_score.max(4);
                    } else if token.len() >= 5 && haystack.contains(token) {
                        best_score = best_score.max(2);
                    }
                }
                for haystack in &compact_haystacks {
                    if haystack == &compact_token {
                        best_score = best_score.max(3);
                    } else if compact_token.len() >= 5 && haystack.contains(&compact_token) {
                        best_score = best_score.max(1);
                    }
                }
            }

            if best_score > 0 {
                scores
                    .entry(slug)
                    .and_modify(|score| *score = (*score).max(best_score))
                    .or_insert(best_score);
            }
        }
    }

    let max_score = scores.values().copied().max()?;
    let mut best = scores
        .into_iter()
        .filter(|(_, score)| *score == max_score)
        .map(|(slug, _)| slug)
        .collect::<Vec<_>>();
    best.sort();

    if best.len() == 1 {
        best.into_iter().next()
    } else {
        tokens.into_iter().next()
    }
}

fn project_hint_tokens(query_terms: &[&str]) -> Vec<String> {
    query_terms
        .iter()
        .map(|term| term.trim())
        .filter(|term| term.len() >= 4 && !is_generic_metadata_query_term(term))
        .map(ToString::to_string)
        .collect()
}

fn is_generic_metadata_query_term(term: &str) -> bool {
    let generic = [
        "path", "file", "files", "repo", "project", "store", "chunk", "chunks", "context",
    ];
    generic.contains(&term)
}

fn compact_project_token(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

/// Fuzzy-search stored chunk files with normalized matching and quality scoring.
pub fn fuzzy_search_store(
    store_root: &Path,
    query: &str,
    limit: usize,
    project_filters: &[Option<&str>],
    frame_kind_filter: Option<FrameKind>,
) -> io::Result<(Vec<FuzzyResult>, usize)> {
    let scopes = if project_filters.is_empty() {
        vec![None]
    } else {
        project_filters.to_vec()
    };
    let mut merged = Vec::new();
    let mut scanned = 0usize;
    for scope in scopes {
        let (mut results, scope_scanned) =
            fuzzy_search_store_one(store_root, query, limit, scope, frame_kind_filter)?;
        scanned += scope_scanned;
        merged.append(&mut results);
    }
    merged.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));
    merged.truncate(limit);
    Ok((merged, scanned))
}

fn fuzzy_search_store_one(
    store_root: &Path,
    query: &str,
    limit: usize,
    project_filter: Option<&str>,
    frame_kind_filter: Option<FrameKind>,
) -> io::Result<(Vec<FuzzyResult>, usize)> {
    let normalized_query = normalize_query(query);
    let query_terms: Vec<&str> = normalized_query.split_whitespace().collect();

    let mut results = Vec::new();
    let mut total_scanned = 0usize;

    let inferred_project_filter = if project_filter.is_none() {
        infer_project_filter_from_query(store_root, &query_terms)
    } else {
        None
    };
    let effective_project_filter = project_filter.or(inferred_project_filter.as_deref());

    let stored_files = store::scan_context_files_project_at(store_root, effective_project_filter)
        .map_err(io::Error::other)?;
    let stored_files = select_search_candidates(stored_files, &query_terms, limit);
    for stored_file in stored_files {
        if stored_file.path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }

        // Strict canonical project filter: split the stored `<owner>/<repo>`
        // slug and delegate to `store::project_filter_matches` so the rank
        // fallback fuzzy path agrees with store / dashboard / steer / mcp.
        // Substring fallback (`-p vista` matching `vista-portal`, etc.) is
        // intentionally removed — Bug #38.
        if let Some(filter) = project_filter {
            let (organization, repository) = stored_file
                .project
                .split_once('/')
                .unwrap_or(("", stored_file.project.as_str()));
            if !store::project_filter_matches(organization, repository, filter) {
                continue;
            }
        }

        total_scanned += 1;
        let metadata_text = metadata_search_text(&stored_file);
        let metadata_matches = metadata_matched_lines(&stored_file, &metadata_text, &query_terms);

        if metadata_covers_query(&metadata_text, &query_terms) {
            results.push(metadata_only_result(
                stored_file,
                &metadata_text,
                &query_terms,
            ));
            continue;
        }

        let Ok(content) = sanitize::read_to_string_validated(&stored_file.path) else {
            continue;
        };

        // Header-agnostic: drop the card header (bracket or frontmatter)
        // structurally before line-level filtering, so frontmatter meta
        // lines never surface as search matches.
        let all_lines: Vec<&str> = crate::card_header::card_body(&content).lines().collect();
        let without_aicx = strip_aicx_read_blocks(all_lines);
        let signal_lines: Vec<&str> = without_aicx
            .into_iter()
            .filter(|line| !is_search_boilerplate(line))
            .collect();
        let signal_text = signal_lines
            .iter()
            .map(|line| normalize_query(line))
            .collect::<Vec<_>>()
            .join(" ");

        let matched_terms = query_terms
            .iter()
            .filter(|term| signal_text.contains(**term) || metadata_text.contains(**term))
            .count();

        if matched_terms == 0 {
            continue;
        }

        let mut matched_lines: Vec<String> = signal_lines
            .iter()
            .filter(|line| {
                let normalized_line = normalize_query(line);
                query_terms
                    .iter()
                    .any(|term| normalized_line.contains(term))
            })
            .take(5)
            .map(|line| line.trim().to_string())
            .collect();
        if matched_lines.is_empty() && !metadata_matches.is_empty() {
            matched_lines = metadata_matches;
        }
        if matched_lines.is_empty() && metadata_match_count(&stored_file, &query_terms) > 0 {
            matched_lines = metadata_line(&stored_file);
        }
        matched_lines.truncate(5);

        let sidecar_path = stored_file.path.with_extension("meta.json");
        let (session_id, cwd, timestamp, frame_kind, speaker_hint) = if sidecar_path.exists() {
            sanitize::read_to_string_validated(&sidecar_path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .map(|v| {
                    (
                        v.get("session_id")
                            .and_then(|s| s.as_str())
                            .map(String::from),
                        v.get("cwd").and_then(|s| s.as_str()).map(String::from),
                        v.get("started_at")
                            .and_then(|s| s.as_str())
                            .map(String::from)
                            .or_else(|| {
                                v.get("timestamp")
                                    .and_then(|s| s.as_str())
                                    .map(String::from)
                            }),
                        v.get("frame_kind")
                            .and_then(|s| s.as_str())
                            .and_then(FrameKind::parse)
                            .map(|kind| kind.to_string()),
                        v.get("speaker_hint")
                            .or_else(|| v.get("speaker"))
                            .or_else(|| v.get("role"))
                            .and_then(|s| s.as_str())
                            .map(String::from),
                    )
                })
                .unwrap_or((None, None, None, None, None))
        } else {
            (None, None, None, None, None)
        };

        let chunk_score = score_chunk_content_with_context(
            &content,
            frame_kind.as_deref(),
            speaker_hint.as_deref(),
        );
        let match_ratio = if query_terms.is_empty() {
            1.0
        } else {
            matched_terms as f32 / query_terms.len() as f32
        };
        let final_score = combine_match_quality_score(match_ratio, chunk_score.score);

        if let Some(expected) = frame_kind_filter
            && frame_kind.as_deref() != Some(expected.as_str())
        {
            continue;
        }

        let final_timestamp = timestamp.or_else(|| {
            stored_file
                .path
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(chrono::DateTime::<chrono::Utc>::from)
                .map(|d| d.to_rfc3339())
        });

        results.push(FuzzyResult {
            file: stored_file
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            path: stored_file.path.display().to_string(),
            project: stored_file.project,
            kind: stored_file.kind.dir_name().to_string(),
            frame_kind,
            agent: stored_file.agent,
            date: stored_file.date_iso,
            timestamp: final_timestamp,
            score: final_score,
            label: if final_score >= 80 {
                "HIGH".to_string()
            } else if final_score >= 60 {
                "MEDIUM".to_string()
            } else {
                "LOW".to_string()
            },
            density: chunk_score.density,
            matched_lines,
            session_id,
            cwd,
        });
    }

    results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));

    let mut seen_hashes = std::collections::HashSet::new();
    results.retain(|result| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        result.matched_lines.hash(&mut h);
        result.file.hash(&mut h);
        seen_hashes.insert(h.finish())
    });

    let mut best_per_session: HashMap<String, usize> = HashMap::new();
    for (idx, result) in results.iter().enumerate() {
        let session_key = session_key_for_result(result);
        best_per_session
            .entry(session_key)
            .and_modify(|prev| {
                if result.score > results[*prev].score {
                    *prev = idx;
                }
            })
            .or_insert(idx);
    }
    let keep: std::collections::HashSet<usize> = best_per_session.values().copied().collect();
    let mut deduped = Vec::with_capacity(keep.len());
    for (idx, result) in results.into_iter().enumerate() {
        if keep.contains(&idx) {
            deduped.push(result);
        }
    }

    if deduped.len() >= 5 {
        let threshold = (deduped.len() as f32 * 0.15).ceil() as usize;
        let mut line_freq: HashMap<String, usize> = HashMap::new();
        for result in &deduped {
            let mut seen_in_result = std::collections::HashSet::new();
            for line in &result.matched_lines {
                let key = normalize_query(line);
                if seen_in_result.insert(key.clone()) {
                    *line_freq.entry(key).or_insert(0) += 1;
                }
            }
        }
        for result in &mut deduped {
            result.matched_lines.retain(|line| {
                line_freq.get(&normalize_query(line)).copied().unwrap_or(0) < threshold
            });
        }
    }

    deduped.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));
    deduped.truncate(limit);

    Ok((deduped, total_scanned))
}

fn extract_session_key(filename: &str) -> String {
    let stem = filename.strip_suffix(".md").unwrap_or(filename);
    if let Some(key) = canonical_session_key(stem) {
        return key;
    }
    tracing::warn!(
        target: "aicx::rank",
        filename = %filename,
        "legacy_non_canonical_session_filename"
    );
    stem.to_string()
}

fn session_key_for_result(result: &FuzzyResult) -> String {
    let stem = result.file.strip_suffix(".md").unwrap_or(&result.file);
    if let Some(key) = canonical_session_key(stem) {
        return key;
    }
    let legacy_key = extract_session_key(&result.file);
    format!("legacy:{}:{legacy_key}", result.path)
}

fn canonical_session_key(stem: &str) -> Option<String> {
    let pos = stem.rfind('_')?;
    let suffix = &stem[pos + 1..];
    if suffix.len() <= 3 && suffix.chars().all(|c| c.is_ascii_digit()) {
        Some(stem[..pos].to_string())
    } else {
        None
    }
}

/// Score a chunk file's content quality.
///
/// Returns a `ChunkScore` with a 0–10 rating based on:
/// - Signal density (actionable lines / total lines)
/// - Presence of high-value patterns (decisions, bugs, outcomes)
/// - Penalty for boilerplate-heavy content
pub fn score_chunk_content(content: &str) -> ChunkScore {
    score_chunk_content_with_context(content, None, None)
}

pub fn score_chunk_content_with_context(
    content: &str,
    frame_kind: Option<&str>,
    speaker_hint: Option<&str>,
) -> ChunkScore {
    let mut signal = 0usize;
    let mut noise = 0usize;
    let mut total = 0usize;
    let mut weighted_signal = 0.0f32;
    let mut in_skill_boilerplate = false;
    let mut in_code_block = false;
    let mut consecutive_noise = 0usize;
    let mut has_high_value = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        total += 1;

        // Track code blocks (``` ... ```) — skill docs often contain them
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            if in_skill_boilerplate {
                noise += 1;
                consecutive_noise += 1;
                continue;
            }
        }

        // Code inside skill boilerplate = noise
        if in_code_block && in_skill_boilerplate {
            noise += 1;
            consecutive_noise += 1;
            continue;
        }

        let class = classify_line(trimmed, in_skill_boilerplate);

        // Detect entry into skill boilerplate sections
        let lower = trimmed.to_lowercase();
        if !in_skill_boilerplate && is_skill_boilerplate_header(&lower) {
            in_skill_boilerplate = true;
        }
        // Exit boilerplate on signals block or actual conversation lines
        if in_skill_boilerplate
            && (lower.starts_with("[signals]")
                || lower.starts_with("[/signals]")
                || is_conversation_line(trimmed))
        {
            in_skill_boilerplate = false;
        }

        match class {
            LineClass::Signal => {
                consecutive_noise = 0;
                let role = speaker_role(frame_kind, speaker_hint, trimmed);
                let weight = signal_weight_for_role(&lower, role);
                weighted_signal += weight;
                if weight >= 0.5 {
                    signal += 1;
                }
                // High-value markers get extra weight
                if is_high_value_signal(&lower) && high_value_bonus_allowed(role) {
                    has_high_value = true;
                }
            }
            LineClass::Noise => {
                noise += 1;
                consecutive_noise += 1;
            }
            LineClass::Neutral => {
                consecutive_noise = 0;
            }
        }

        // Long runs of consecutive noise indicate boilerplate sections
        if consecutive_noise > 10 && !in_skill_boilerplate {
            in_skill_boilerplate = true;
        }
    }

    if total == 0 {
        return ChunkScore {
            score: 0,
            signal_lines: 0,
            noise_lines: 0,
            total_lines: 0,
            density: 0.0,
            label: "EMPTY",
        };
    }

    let density = weighted_signal / total as f32;
    let noise_ratio = noise as f32 / total as f32;

    // Base score from signal density (0–6 points)
    let density_score = (density * 10.0).min(6.0);

    // Bonus for high-value signals (+2)
    let high_value_bonus = if has_high_value { 2.0 } else { 0.0 };

    // Penalty for high noise ratio (-3 max)
    let noise_penalty = if noise_ratio > 0.7 {
        3.0
    } else if noise_ratio > 0.5 {
        2.0
    } else if noise_ratio > 0.3 {
        1.0
    } else {
        0.0
    };

    // Bonus for sufficient signal volume (+2 max)
    let volume_bonus = if signal >= 15 {
        2.0
    } else if signal >= 8 {
        1.0
    } else {
        0.0
    };

    let raw = density_score + high_value_bonus + volume_bonus - noise_penalty;
    let score = raw.clamp(0.0, 10.0).round() as u8;

    let label = match score {
        0..=2 => "NOISE",
        3..=4 => "LOW",
        5..=7 => "MEDIUM",
        _ => "HIGH",
    };

    ChunkScore {
        score,
        signal_lines: signal,
        noise_lines: noise,
        total_lines: total,
        density,
        label,
    }
}

/// Score a chunk file by path.
pub fn score_chunk_file(path: &Path) -> ChunkScore {
    match sanitize::read_to_string_validated(path) {
        Ok(content) => score_chunk_content(&content),
        Err(_) => ChunkScore {
            score: 0,
            signal_lines: 0,
            noise_lines: 0,
            total_lines: 0,
            density: 0.0,
            label: "UNREADABLE",
        },
    }
}

// ============================================================================
// Line classification
// ============================================================================

fn classify_line(line: &str, in_boilerplate: bool) -> LineClass {
    let lower = line.to_lowercase();

    // Explicit noise checks first (fast path)
    if is_noise_line(&lower) {
        return LineClass::Noise;
    }

    // Inside boilerplate section — treat as noise unless it's clearly signal
    if in_boilerplate {
        if is_signal_line(&lower) {
            return LineClass::Signal;
        }
        return LineClass::Noise;
    }

    // Signal checks
    if is_signal_line(&lower) {
        return LineClass::Signal;
    }

    // Skill boilerplate headers (even outside detected sections)
    if is_skill_boilerplate_header(&lower) {
        return LineClass::Noise;
    }

    // Boilerplate footers
    for pat in BOILERPLATE_FOOTERS {
        if lower.contains(pat) {
            return LineClass::Noise;
        }
    }

    LineClass::Neutral
}

fn combine_match_quality_score(match_ratio: f32, quality_score: u8) -> u8 {
    let quality_weight = if match_ratio >= 0.5 { 4.0 } else { 2.0 };
    ((60.0 * match_ratio + quality_score as f32 * quality_weight) as u8).min(100)
}

fn is_noise_line(lower: &str) -> bool {
    for prefix in NOISE_PREFIXES {
        if lower.starts_with(prefix) {
            return true;
        }
    }
    for substr in NOISE_CONTAINS {
        if lower.contains(substr) {
            return true;
        }
    }
    false
}

fn is_signal_line(lower: &str) -> bool {
    for substr in SIGNAL_CONTAINS {
        if signal_contains_matches(lower, substr) {
            return true;
        }
    }
    for prefix in SIGNAL_PREFIXES {
        if lower.starts_with(prefix) {
            return true;
        }
    }
    false
}

fn signal_contains_matches(lower: &str, needle: &str) -> bool {
    match needle {
        "deploy" | "release" => contains_word(lower, needle),
        _ => lower.contains(needle),
    }
}

fn speaker_role(frame_kind: Option<&str>, speaker_hint: Option<&str>, line: &str) -> SpeakerRole {
    if matches!(frame_kind, Some("user_msg")) {
        return SpeakerRole::Operator;
    }
    if matches!(
        frame_kind,
        Some("internal_thought" | "tool_call" | "tool_result")
    ) {
        return SpeakerRole::Tool;
    }
    if matches!(frame_kind, Some("agent_reply")) {
        return SpeakerRole::Assistant;
    }

    let hint = speaker_hint.or_else(|| conversation_speaker(line));
    match hint.map(|hint| hint.trim().to_ascii_lowercase()) {
        Some(hint) if matches!(hint.as_str(), "user" | "human" | "operator") => {
            SpeakerRole::Operator
        }
        Some(hint)
            if matches!(
                hint.as_str(),
                "assistant" | "agent" | "claude" | "codex" | "gemini" | "junie"
            ) =>
        {
            SpeakerRole::Assistant
        }
        Some(hint) if matches!(hint.as_str(), "tool" | "system" | "internal_thought") => {
            SpeakerRole::Tool
        }
        _ => SpeakerRole::Unknown,
    }
}

fn conversation_speaker(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once("] ")?;
    rest.split_once(':').map(|(speaker, _)| speaker.trim())
}

fn signal_weight_for_role(lower: &str, role: SpeakerRole) -> f32 {
    match role {
        SpeakerRole::Operator | SpeakerRole::Unknown => 1.0,
        SpeakerRole::Tool => 0.0,
        SpeakerRole::Assistant if is_assistant_scaffolding_signal(lower) => 0.25,
        SpeakerRole::Assistant => 0.75,
    }
}

fn is_assistant_scaffolding_signal(lower: &str) -> bool {
    lower.contains("todo:")
        || lower.contains("- [ ]")
        || lower.contains("- [x]")
        || lower.contains("decision:")
        || lower.contains("[decision]")
        || lower.contains("plan:")
        || lower.contains("outcome:")
        || lower.contains("[skill_outcome]")
        || lower.contains("p0=")
        || lower.contains("p1=")
        || lower.contains("p2=")
        || lower.contains("score:")
        || lower.contains("/100")
        || contains_word(lower, "deploy")
        || contains_word(lower, "release")
}

fn high_value_bonus_allowed(role: SpeakerRole) -> bool {
    !matches!(role, SpeakerRole::Assistant | SpeakerRole::Tool)
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut start = 0usize;
    while let Some(offset) = haystack[start..].find(needle) {
        let pos = start + offset;
        let end = pos + needle.len();
        let before = haystack[..pos].chars().next_back();
        let after = haystack[end..].chars().next();
        if before.is_none_or(|ch| !is_word_char(ch)) && after.is_none_or(|ch| !is_word_char(ch)) {
            return true;
        }
        start = end;
    }
    false
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

/// Lines that are generic preamble/boilerplate — should not contribute to search matching.
const SEARCH_BOILERPLATE: &[&str] = &["created by vetcoders", "vibecrafted with ai agents"];

/// Sentinel brackets for aicx read blocks. Content between these markers
/// is injected context from aicx tools — not original session signal.
const AICX_READ_BEGIN: &str = "【aicx:read】";
const AICX_READ_END: &str = "【/aicx:read】";

fn is_search_boilerplate(line: &str) -> bool {
    if is_search_metadata_line(line) {
        return true;
    }
    let lower = line.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    for pat in SEARCH_BOILERPLATE {
        if lower.contains(pat) {
            return true;
        }
    }
    is_skill_boilerplate_header(&lower)
}

/// Filter out lines inside 【aicx:read】...【/aicx:read】 blocks.
fn strip_aicx_read_blocks(lines: Vec<&str>) -> Vec<&str> {
    let mut out = Vec::with_capacity(lines.len());
    let mut inside = false;
    for line in lines {
        if line.contains(AICX_READ_BEGIN) {
            inside = true;
            continue;
        }
        if line.contains(AICX_READ_END) {
            inside = false;
            continue;
        }
        if !inside {
            out.push(line);
        }
    }
    out
}

fn is_skill_boilerplate_header(lower: &str) -> bool {
    for header in SKILL_BOILERPLATE_HEADERS {
        if lower.starts_with(header) {
            return true;
        }
    }
    false
}

/// Detect actual conversation lines like `[HH:MM:SS] role: ...`
fn is_conversation_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('[')
        && trimmed.len() > 12
        && trimmed.as_bytes().get(3) == Some(&b':')
        && trimmed.as_bytes().get(6) == Some(&b':')
        && trimmed.as_bytes().get(9) == Some(&b']')
}

fn is_high_value_signal(lower: &str) -> bool {
    lower.contains("[decision]")
        || lower.contains("decision:")
        || lower.contains("[skill_outcome]")
        || lower.contains("outcome:")
        || lower.contains("p0=")
        || lower.contains("p1=")
        || lower.contains("p2=")
        || lower.contains("/100")
        || contains_word(lower, "deploy")
        || contains_word(lower, "release")
        || lower.contains("breaking change")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_intent_candidate_uses_shared_cosine_ranker() {
        let close = intent_candidate_similarity(&[1.0, 0.0], &[0.99, 0.01]);
        let far = intent_candidate_similarity(&[1.0, 0.0], &[0.0, 1.0]);
        assert!(close >= SEMANTIC_INTENT_CANDIDATE_THRESHOLD);
        assert!(far < SEMANTIC_INTENT_CANDIDATE_THRESHOLD);
    }

    #[test]
    fn test_empty_content() {
        let score = score_chunk_content("");
        assert_eq!(score.score, 0);
        assert_eq!(score.label, "EMPTY");
    }

    #[test]
    fn test_pure_noise() {
        let content = r#"[project: test | agent: claude | date: 2026-03-14]

<command-message>vetcoders-init</command-message>
<command-name>/vetcoders-init</command-name>
<command-args>some args</command-args>
Base directory for this skill: /some/path

## When To Use
Execute at the start of every session.
## Anti-Patterns
- Starting implementation without running init

## Fallback
If aicx unavailable: skip memory steps.
"#;
        let score = score_chunk_content(content);
        assert!(
            score.score <= 3,
            "Pure noise should score <=3, got {}",
            score.score
        );
        assert_eq!(score.label, "NOISE");
    }

    #[test]
    fn test_pure_signal() {
        let content = r#"[project: test | agent: claude | date: 2026-03-14]

[signals]
Decision: Use per-chunk scoring instead of bundle-level
- [ ] Implement rank.rs module
- [x] Read existing code
[/signals]

[14:30:00] user: Decision: we need to fix the ranking
[14:31:00] assistant: Plan: refactor run_rank to use content scoring
[14:32:00] assistant: TODO: add --strict flag
[14:33:00] user: Deploy to production after merge
[14:34:00] assistant: Score: 92/100, P0=0, P1=0, P2=1
"#;
        let score = score_chunk_content(content);
        assert!(
            score.score >= 7,
            "Pure signal should score >=7, got {}",
            score.score
        );
        assert!(score.label == "HIGH" || score.label == "MEDIUM");
    }

    #[test]
    fn test_mixed_content_noisy() {
        // 2 signal lines, 4 noise lines, 3 neutral — leans noisy
        let content = r#"[project: test | agent: claude | date: 2026-03-14]

[14:30:00] user: Fix the login regression
[14:31:00] assistant: Found the bug in auth middleware
[14:32:00] assistant: This is just some neutral conversation
[14:33:00] assistant: More neutral stuff here
<command-message>some-skill</command-message>
Base directory for this skill: /foo

## When To Use
Some boilerplate text.
"#;
        let score = score_chunk_content(content);
        assert!(
            score.score <= 4,
            "Noisy mixed content should score <=4, got {}",
            score.score
        );
    }

    #[test]
    fn test_mixed_content_signal_heavy() {
        // More signal than noise — should score medium
        let content = r#"[project: test | agent: claude | date: 2026-03-14]

[14:30:00] user: Fix the login regression
[14:31:00] assistant: Found the bug in auth middleware - commit pending
[14:32:00] assistant: TODO: add test for edge case
[14:33:00] assistant: Architecture decision: split into modules
[14:34:00] user: Let's deploy after merge
[14:35:00] assistant: Plan: run cargo test then merge PR #42
[14:36:00] assistant: Some neutral observation
"#;
        let score = score_chunk_content(content);
        assert!(
            score.score >= 4,
            "Signal-heavy mixed content should score >=4, got {}",
            score.score
        );
    }

    #[test]
    fn test_skill_echo_is_noise() {
        // Simulates a chunk that's mostly echoed skill prompt
        let mut content = String::from("[project: test | agent: claude | date: 2026-03-14]\n\n");
        content.push_str("[14:30:00] user: /vetcoders-init\n");
        content.push_str(
            "Base directory for this skill: /Users/test/.claude/skills/vetcoders-init\n\n",
        );
        content.push_str("# vetcoders-init — Memory + Eyes for AI Agents\n\n");
        content.push_str("## When To Use\n");
        for i in 0..20 {
            content.push_str(&format!(
                "Line {} of skill documentation that adds no value.\n",
                i
            ));
        }
        content.push_str("## Anti-Patterns\n");
        content.push_str("- Starting implementation without running init\n");
        content.push_str("## Fallback\n");
        content.push_str("If aicx unavailable: skip memory steps.\n");
        content.push_str("```bash\naicx all -p project\n```\n");

        let score = score_chunk_content(&content);
        assert!(
            score.score <= 4,
            "Echoed skill prompt should score <=4, got {}",
            score.score
        );
    }

    #[test]
    fn test_conversation_line_detection() {
        assert!(is_conversation_line("[14:30:00] user: hello"));
        assert!(is_conversation_line("[08:06:37] assistant: Starting init"));
        assert!(!is_conversation_line("## When To Use"));
        assert!(!is_conversation_line("[signals]"));
        assert!(!is_conversation_line("just some text"));
    }

    #[test]
    fn test_high_value_signals_boost() {
        let content = r#"[project: test | agent: claude | date: 2026-03-14]

    [14:30:00] user: Decision: rewrite auth middleware for compliance
    [14:31:00] user: Outcome: P0=0, P1=0, P2=0, Score: 100/100
    [14:32:00] user: Deploy to vistacare.ai complete
    [14:33:00] user: Release v0.8.16 tagged
"#;
        let score = score_chunk_content(content);
        assert!(
            score.score >= 8,
            "High-value signals should score >=8, got {}",
            score.score
        );
    }

    #[test]
    fn assistant_todo_scaffolding_is_discounted() {
        let assistant = r#"[project: test | agent: codex | date: 2026-05-20]

    [14:30:00] assistant: TODO: add tests
    [14:31:00] assistant: Decision: run cargo test
    [14:32:00] assistant: Outcome: P0=0, P1=0, P2=0, Score: 100/100
"#;
        let user = r#"[project: test | agent: codex | date: 2026-05-20]

    [14:30:00] user: TODO: fix search ranking
    [14:31:00] user: Decision: rank user-authored TODO above agent checklist
    [14:32:00] user: Release is blocked until ranking is fixed
"#;

        let assistant_score = score_chunk_content(assistant);
        let user_score = score_chunk_content(user);

        assert!(user_score.score > assistant_score.score);
        assert!(assistant_score.score <= 4, "got {}", assistant_score.score);
    }

    #[test]
    fn deploy_and_release_require_word_boundaries() {
        assert!(is_high_value_signal("release v0.8.1"));
        assert!(is_high_value_signal("deploy after tests"));
        assert!(!is_high_value_signal("predeployment checklist"));
        assert!(!is_high_value_signal("releasedocs draft"));
    }

    #[test]
    fn partial_match_high_signal_does_not_beat_full_lexical_match() {
        let partial_high_signal = combine_match_quality_score(1.0 / 3.0, 10);
        let full_low_signal = combine_match_quality_score(1.0, 0);
        assert!(full_low_signal > partial_high_signal);
    }

    #[test]
    fn extract_session_key_strips_canonical_suffix() {
        assert_eq!(
            extract_session_key("2026_0405_codex_sess1_001.md"),
            "2026_0405_codex_sess1"
        );
    }

    #[test]
    fn legacy_session_key_for_result_uses_path_to_avoid_global_collision() {
        let result_a = FuzzyResult {
            file: "chunk.md".to_string(),
            path: "/tmp/a/chunk.md".to_string(),
            project: "p".to_string(),
            kind: "conversations".to_string(),
            frame_kind: None,
            agent: "codex".to_string(),
            date: "2026-05-20".to_string(),
            timestamp: None,
            score: 1,
            label: "LOW".to_string(),
            density: 0.0,
            matched_lines: Vec::new(),
            session_id: None,
            cwd: None,
        };
        let mut result_b = result_a.clone();
        result_b.path = "/tmp/b/chunk.md".to_string();

        assert_ne!(
            session_key_for_result(&result_a),
            session_key_for_result(&result_b)
        );
    }

    // ================================================================
    // Repo-centric fuzzy search retrieval tests
    // ================================================================

    fn unique_rank_test_store_root(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("aicx-rank-{label}-{}-{nanos}", std::process::id()))
    }

    fn write_canonical_search_fixture(
        root: &Path,
        organization: &str,
        repository: &str,
        session_id: &str,
        body: &str,
    ) -> std::path::PathBuf {
        let dir = root
            .join(store::CANONICAL_STORE_DIRNAME)
            .join(organization)
            .join(repository)
            .join("2026_0524")
            .join("conversations")
            .join("codex");
        fs::create_dir_all(&dir).expect("fixture directory should be created");
        let path = dir.join(format!("2026_0524_codex_{session_id}_001.md"));
        fs::write(&path, body).expect("fixture chunk should be written");
        path
    }

    #[test]
    fn fuzzy_search_store_one_applies_strict_project_filter_for_bare_repo_name() {
        let root = unique_rank_test_store_root("strict-project-filter");
        fs::create_dir_all(&root).expect("fixture root should be created");

        let vista_path = write_canonical_search_fixture(
            &root,
            "Vetcoders",
            "Vista",
            "sessvista",
            "[project: Vetcoders/Vista | agent: codex | date: 2026-05-24]\n\nDecision: strictneedle belongs to the exact Vista repository.\n",
        );
        write_canonical_search_fixture(
            &root,
            "Vetcoders",
            "vista-portal",
            "sessportal",
            "[project: Vetcoders/vista-portal | agent: codex | date: 2026-05-24]\n\nDecision: strictneedle must not leak through a bare vista filter.\n",
        );

        let (results, scanned) =
            fuzzy_search_store_one(&root, "strictneedle", 10, Some("vista"), None)
                .expect("fixture fuzzy search should succeed");

        assert_eq!(scanned, 1, "bare `-p vista` must scan only exact Vista");
        assert_eq!(results.len(), 1, "vista-portal must not match `-p vista`");
        assert_eq!(results[0].project, "Vetcoders/Vista");
        assert_eq!(
            results[0].path,
            fs::canonicalize(&vista_path)
                .expect("fixture path should canonicalize")
                .display()
                .to_string()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fuzzy_search_store_one_prefers_content_matches_over_metadata_lines() {
        let root = unique_rank_test_store_root("content-before-metadata");
        fs::create_dir_all(&root).expect("fixture root should be created");

        write_canonical_search_fixture(
            &root,
            "Vetcoders",
            "aicx",
            "sessaicx",
            "User asked:\nWhy did we move embeddings to Sztudio?\n\nAgent answered:\nDecision: foundationneedle belongs in the content match, not in a metadata banner.\n",
        );

        let (results, scanned) =
            fuzzy_search_store_one(&root, "aicx foundationneedle", 10, None, None)
                .expect("fixture fuzzy search should succeed");

        assert_eq!(scanned, 1);
        assert_eq!(results.len(), 1);
        assert!(
            results[0].matched_lines[0].contains("foundationneedle"),
            "content evidence should be first"
        );
        assert!(
            !results[0]
                .matched_lines
                .iter()
                .any(|line| is_search_metadata_line(line)),
            "metadata lines should not be kept when content evidence exists"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fuzzy_search_tolerates_frontmatter_card_header() {
        let root = unique_rank_test_store_root("frontmatter-header");
        fs::create_dir_all(&root).expect("fixture root should be created");

        write_canonical_search_fixture(
            &root,
            "Vetcoders",
            "Vista",
            "sessfront",
            "---\nproject: Vetcoders/Vista\nagent: codex\ndate: 2026-05-24\nframe_kind: agent_reply\n---\n\nDecision: frontneedle lives only in the body.\n",
        );

        let (results, scanned) =
            fuzzy_search_store_one(&root, "frontneedle", 10, Some("Vetcoders/Vista"), None)
                .expect("fixture fuzzy search should succeed");

        assert_eq!(scanned, 1);
        assert_eq!(results.len(), 1, "frontmatter card must stay searchable");
        let matches = &results[0].matched_lines;
        assert!(
            matches.iter().any(|line| line.contains("frontneedle")),
            "body evidence must surface: {matches:?}"
        );
        assert!(
            matches.iter().all(|line| {
                !line.starts_with("---")
                    && !line.starts_with("project:")
                    && !line.starts_with("agent:")
                    && !line.starts_with("frame_kind:")
            }),
            "frontmatter meta lines must not surface as matches: {matches:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn render_search_json_matches_cli_surface_fields() {
        let long_line = "x".repeat(205);
        let json = render_search_json(
            Path::new("/tmp/aicx"),
            &[FuzzyResult {
                file: "chunk.md".to_string(),
                path: "/tmp/chunk.md".to_string(),
                project: "Vetcoders/ai-contexters".to_string(),
                kind: "reports".to_string(),
                frame_kind: None,
                agent: "codex".to_string(),
                date: "2026-03-31".to_string(),
                timestamp: None,
                score: 88,
                label: "HIGH".to_string(),
                density: 0.8,
                matched_lines: vec![
                    "[project: test | agent: codex | date: 2026-03-31]".to_string(),
                    "[metadata] project: test | agent: codex | date: 2026-03-31 | kind: reports | path: /tmp/chunk.md".to_string(),
                    "[frame_kind: agent_reply | cwd: /repo]".to_string(),
                    "Source project: aicx".to_string(),
                    "Canonical test project: tb14d-rounds/aicx".to_string(),
                    "TB artifact: spotlight_rounds".to_string(),
                    "Round status: answered".to_string(),
                    long_line.clone(),
                    "decision: align MCP search JSON with CLI".to_string(),
                ],
                session_id: Some("sess-123".to_string()),
                cwd: Some("/repo".to_string()),
            }],
            127,
        )
        .expect("search JSON should serialize");

        assert!(!json.contains('\n'));

        let payload: serde_json::Value =
            serde_json::from_str(&json).expect("search JSON should parse");

        assert_eq!(payload["results"], 1);
        assert_eq!(payload["scanned"], 127);
        assert_eq!(payload["oracle_status"]["backend"], "filesystem_fuzzy");
        assert_eq!(payload["oracle_status"]["index_kind"], "none");
        assert_eq!(
            payload["oracle_status"]["source_layer"],
            "layer_1_canonical_corpus"
        );
        assert_eq!(
            payload["oracle_status"]["derived_view"],
            "none_filesystem_scan"
        );
        assert_eq!(
            payload["oracle_status"]["fallback_reason"],
            "fallback_filesystem_fuzzy: content index unavailable"
        );
        assert_eq!(payload["oracle_status"]["scanned_count"], 127);
        assert_eq!(payload["oracle_status"]["candidate_count"], 1);
        assert_eq!(payload["oracle_status"]["stale_or_unknown"], true);
        assert_eq!(payload["oracle_status"]["loctree_scope_safe"], false);
        assert!(
            payload["oracle_status"]["loctree_scope_note"]
                .as_str()
                .unwrap()
                .contains("unsafe_for_scope_narrowing")
        );
        assert_eq!(payload["items"][0]["score"], 88);
        assert_eq!(payload["items"][0]["label"], "HIGH");
        assert_eq!(payload["items"][0]["project"], "Vetcoders/ai-contexters");
        assert_eq!(payload["items"][0]["kind"], "reports");
        assert_eq!(payload["items"][0]["agent"], "codex");
        assert_eq!(payload["items"][0]["date"], "2026-03-31");
        assert_eq!(payload["items"][0]["session"], "sess-123");
        assert_eq!(payload["items"][0]["session_id"], "sess-123");
        assert_eq!(payload["items"][0]["cwd"], "/repo");
        assert_eq!(payload["items"][0]["path"], "/tmp/chunk.md");
        assert_eq!(payload["items"][0]["matches"].as_array().unwrap().len(), 2);
        assert_eq!(
            payload["items"][0]["matches"][1],
            "decision: align MCP search JSON with CLI"
        );
        assert!(
            payload["items"][0]["matches"][0]
                .as_str()
                .unwrap()
                .ends_with(" ...")
        );
    }

    #[test]
    fn render_search_json_keeps_metadata_when_it_is_the_only_match() {
        let json = render_search_json(
            Path::new("/tmp/aicx"),
            &[FuzzyResult {
                file: "chunk.md".to_string(),
                path: "/tmp/chunk.md".to_string(),
                project: "Vetcoders/aicx".to_string(),
                kind: "conversations".to_string(),
                frame_kind: None,
                agent: "codex".to_string(),
                date: "2026-06-19".to_string(),
                timestamp: None,
                score: 90,
                label: "HIGH".to_string(),
                density: 1.0,
                matched_lines: vec![
                    "[metadata] project: Vetcoders/aicx | agent: codex | date: 2026-06-19 | kind: conversations | path: /tmp/chunk.md".to_string(),
                ],
                session_id: Some("sess-456".to_string()),
                cwd: None,
            }],
            1,
        )
        .expect("search JSON should serialize");

        let payload: serde_json::Value =
            serde_json::from_str(&json).expect("search JSON should parse");
        let matches = payload["items"][0]["matches"].as_array().unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].as_str().unwrap().starts_with("[metadata]"));
    }
}
