//! Per-chunk content quality scoring and fuzzy-search presentation helpers.
//!
//! Scores each chunk file on a 0–10 scale based on signal density,
//! penalizing noise patterns (echoed skill prompts, tool JSON, system
//! reminders) and rewarding actionable content (decisions, TODOs,
//! architecture changes, bug findings).
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use serde::Serialize;
use std::fmt::Write as _;
use std::path::Path;

use crate::oracle::OracleStatus;
use crate::sanitize;

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
    agent: String,
    date: String,
    timestamp: Option<String>,
    frame_kind: Option<String>,
    session: String,
    cwd: String,
    matches: Vec<String>,
    path: String,
}

const SEARCH_MATCH_MAX_CHARS: usize = 200;
const SEARCH_META_PREFIX: &str = "[project:";

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
    let items = results
        .iter()
        .map(|result| CompactSearchItem {
            score: result.score,
            label: result.label.clone(),
            project: result.project.clone(),
            agent: result.agent.clone(),
            date: result.date.clone(),
            timestamp: result.timestamp.clone(),
            frame_kind: result.frame_kind.clone(),
            session: result.session_id.clone().unwrap_or_else(|| "-".to_string()),
            cwd: result.cwd.clone().unwrap_or_else(|| "-".to_string()),
            matches: display_search_matches(result),
            path: result.path.clone(),
        })
        .collect();

    serde_json::to_string(&CompactSearchResponse {
        oracle_status: search_oracle_status(root, results, scanned),
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
    result
        .matched_lines
        .iter()
        .filter(|line| !line.trim().starts_with(SEARCH_META_PREFIX))
        .map(|line| truncate_search_match(line, SEARCH_MATCH_MAX_CHARS))
        .collect()
}

fn truncate_search_match(line: &str, max_chars: usize) -> String {
    let mut truncated: String = line.chars().take(max_chars).collect();
    if line.chars().count() > max_chars {
        truncated.push_str(" ...");
    }
    truncated
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

    #[test]
    fn render_search_json_matches_cli_surface_fields() {
        let long_line = "x".repeat(205);
        let json = render_search_json(
            Path::new("/tmp/aicx"),
            &[FuzzyResult {
                file: "chunk.md".to_string(),
                path: "/tmp/chunk.md".to_string(),
                project: "VetCoders/ai-contexters".to_string(),
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
        assert_eq!(payload["items"][0]["project"], "VetCoders/ai-contexters");
        assert_eq!(payload["items"][0]["agent"], "codex");
        assert_eq!(payload["items"][0]["date"], "2026-03-31");
        assert_eq!(payload["items"][0]["session"], "sess-123");
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
}
