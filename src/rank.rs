//! Per-chunk content quality scoring for `aicx rank`.
//!
//! Scores each chunk file on a 0–10 scale based on signal density,
//! penalizing noise patterns (echoed skill prompts, tool JSON, system
//! reminders) and rewarding actionable content (decisions, TODOs,
//! architecture changes, bug findings).
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use serde::Serialize;
use std::io;
use std::path::Path;

use crate::sanitize;
use crate::sanitize::normalize_query;
use crate::store;

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
    "created by m&k",
    "vibecrafted with ai agents",
    "*created by m&k",
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

/// Shared fuzzy-search result for a stored chunk.
#[derive(Debug, Clone, Serialize)]
pub struct FuzzyResult {
    pub file: String,
    pub path: String,
    pub project: String,
    pub kind: String,
    pub agent: String,
    pub date: String,
    pub score: u8,
    pub label: String,
    pub density: f32,
    pub matched_lines: Vec<String>,
}

/// Fuzzy-search stored chunk files with normalized AND-matching and quality scoring.
pub fn fuzzy_search_store(
    store_root: &Path,
    query: &str,
    limit: usize,
    project_filter: Option<&str>,
) -> std::io::Result<(Vec<FuzzyResult>, usize)> {
    let normalized_query = normalize_query(query);
    let query_terms: Vec<&str> = normalized_query.split_whitespace().collect();
    let project_filter_lower = project_filter.map(|filter| filter.to_lowercase());

    let mut results = Vec::new();
    let mut total_scanned = 0usize;

    let stored_files = store::scan_context_files_at(store_root).map_err(io::Error::other)?;
    for stored_file in stored_files {
        if stored_file.path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }

        if let Some(ref filter) = project_filter_lower
            && !stored_file.project.to_lowercase().contains(filter)
        {
            continue;
        }

        total_scanned += 1;

        let Ok(content) = sanitize::read_to_string_validated(&stored_file.path) else {
            continue;
        };

        let content_normalized = normalize_query(&content);
        if !query_terms
            .iter()
            .all(|term| content_normalized.contains(term))
        {
            continue;
        }

        let matched_lines = content
            .lines()
            .filter(|line| {
                let normalized_line = normalize_query(line);
                query_terms
                    .iter()
                    .any(|term| normalized_line.contains(term))
            })
            .take(5)
            .map(|line| line.trim().to_string())
            .collect();

        let chunk_score = score_chunk_content(&content);
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
            agent: stored_file.agent,
            date: stored_file.date_iso,
            score: chunk_score.score,
            label: chunk_score.label.to_string(),
            density: chunk_score.density,
            matched_lines,
        });
    }

    results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.date.cmp(&a.date)));
    results.truncate(limit);

    Ok((results, total_scanned))
}

/// Score a chunk file's content quality.
///
/// Returns a `ChunkScore` with a 0–10 rating based on:
/// - Signal density (actionable lines / total lines)
/// - Presence of high-value patterns (decisions, bugs, outcomes)
/// - Penalty for boilerplate-heavy content
pub fn score_chunk_content(content: &str) -> ChunkScore {
    let mut signal = 0usize;
    let mut noise = 0usize;
    let mut total = 0usize;
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
                signal += 1;
                consecutive_noise = 0;
                // High-value markers get extra weight
                if is_high_value_signal(&lower) {
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

    let density = signal as f32 / total as f32;
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
        if lower.contains(substr) {
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
        || lower.contains("deploy")
        || lower.contains("release")
        || lower.contains("breaking change")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
        content.push_str("```bash\naicx all -p project --incremental\n```\n");

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

[14:30:00] assistant: Decision: rewrite auth middleware for compliance
[14:31:00] assistant: Outcome: P0=0, P1=0, P2=0, Score: 100/100
[14:32:00] assistant: Deploy to vistacare.ai complete
[14:33:00] assistant: Release v0.8.16 tagged
"#;
        let score = score_chunk_content(content);
        assert!(
            score.score >= 8,
            "High-value signals should score >=8, got {}",
            score.score
        );
    }

    // ================================================================
    // Repo-centric fuzzy search retrieval tests
    // ================================================================

    use chrono::Utc;
    use std::fs;
    use std::path::PathBuf;

    fn search_test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "aicx-rank-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    fn write_chunk(path: &PathBuf, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn fuzzy_search_returns_repo_centric_metadata() {
        let root = search_test_root("fuzzy-repo");
        let _ = fs::remove_dir_all(&root);

        // Create a repo-centric chunk with searchable signal content
        let chunk_path = root
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0321")
            .join("conversations")
            .join("claude")
            .join("2026_0321_claude_sess-search1_001.md");
        write_chunk(
            &chunk_path,
            "Decision: adopt repo-centric store layout for session recovery",
        );

        let (results, scanned) =
            fuzzy_search_store(&root, "repo-centric store", 10, None).expect("search should work");

        assert!(scanned > 0, "should scan at least one file");
        assert_eq!(results.len(), 1, "should find the matching chunk");

        let result = &results[0];
        assert_eq!(result.project, "VetCoders/ai-contexters");
        assert_eq!(result.kind, "conversations");
        assert_eq!(result.agent, "claude");
        assert_eq!(result.date, "2026-03-21");
        assert!(!result.path.is_empty(), "path should be populated");
        assert!(
            result.path.contains("store/VetCoders/ai-contexters"),
            "path should contain repo-centric structure"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fuzzy_search_returns_non_repository_metadata() {
        let root = search_test_root("fuzzy-nonrepo");
        let _ = fs::remove_dir_all(&root);

        // Create a non-repository chunk
        let chunk_path = root
            .join("non-repository-contexts")
            .join("2026_0321")
            .join("plans")
            .join("codex")
            .join("2026_0321_codex_sess-plan01_001.md");
        write_chunk(
            &chunk_path,
            "Migration plan: adopt repo-centric layout for all agents",
        );

        let (results, scanned) =
            fuzzy_search_store(&root, "migration plan", 10, None).expect("search should work");

        assert!(scanned > 0);
        assert_eq!(results.len(), 1);

        let result = &results[0];
        assert_eq!(result.project, "non-repository-contexts");
        assert_eq!(result.kind, "plans");
        assert_eq!(result.agent, "codex");
        assert!(
            result.path.contains("non-repository-contexts"),
            "path should reference non-repository root"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fuzzy_search_filters_by_repo_project() {
        let root = search_test_root("fuzzy-filter");
        let _ = fs::remove_dir_all(&root);

        // Two repos with the same keyword
        let chunk1 = root
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0321")
            .join("conversations")
            .join("claude")
            .join("2026_0321_claude_sess-a1_001.md");
        write_chunk(&chunk1, "Decision: adopt the new architecture");

        let chunk2 = root
            .join("store")
            .join("VetCoders")
            .join("loctree")
            .join("2026_0321")
            .join("conversations")
            .join("claude")
            .join("2026_0321_claude_sess-b1_001.md");
        write_chunk(&chunk2, "Decision: adopt scanner improvements");

        // Unfiltered: both match
        let (all, _) =
            fuzzy_search_store(&root, "decision adopt", 10, None).expect("unfiltered search");
        assert_eq!(all.len(), 2);

        // Filter by ai-contexters: only one match
        let (filtered, _) = fuzzy_search_store(&root, "decision adopt", 10, Some("ai-contexters"))
            .expect("filtered search");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].project, "VetCoders/ai-contexters");

        let _ = fs::remove_dir_all(&root);
    }
}
