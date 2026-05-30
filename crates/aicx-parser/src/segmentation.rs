//! Semantic segmentation for canonical store ownership.
//!
//! Reconstructs repository-scoped session segments from content signals rather
//! than weak source-side identifiers.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use crate::sanitize;
use crate::timeline::{Kind, RepoIdentity, SemanticSegment, SourceTier, TimelineEntry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

// ============================================================================
// Source trust model
// ============================================================================

/// A repo identity paired with the trust tier of the signal that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TieredIdentity {
    pub identity: RepoIdentity,
    pub tier: SourceTier,
}

/// Explicit source used when assigning an entry/session to a bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BucketingSource {
    OperatorOverride,
    CwdGitRemote,
    CwdGitRoot,
    KnownLayout,
    Frontmatter,
    ContentMention,
    Unclassified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketResolution {
    pub bucket: String,
    pub source: BucketingSource,
    pub identity: Option<RepoIdentity>,
}

// ============================================================================
// Gemini projectHash registry
// ============================================================================

/// Registry mapping Gemini `projectHash` values to known repo roots.
///
/// The mapping lives in `~/.aicx/gemini-project-map.json` and must be
/// maintained by the user or by `aicx init`. A projectHash that is not
/// in this file cannot resolve to a repo — it stays Opaque.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectHashRegistry {
    /// Maps `projectHash` (hex string) → absolute path to project root.
    #[serde(default)]
    pub mappings: HashMap<String, String>,
}

impl ProjectHashRegistry {
    /// Load from the default location. Honors `AICX_HOME` (operator's
    /// explicit store-root override) before falling back to
    /// `~/.aicx/gemini-project-map.json`. Returns an empty registry if
    /// the file doesn't exist or can't be parsed — callers can rely on
    /// this method without an existence check.
    pub fn load_default() -> Self {
        let base = std::env::var_os("AICX_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".aicx")));
        let Some(base) = base else {
            return Self::default();
        };
        let path = base.join("gemini-project-map.json");
        Self::load_from(&path)
    }

    /// Load from a specific path.
    pub fn load_from(path: &Path) -> Self {
        sanitize::read_to_string_validated(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// Resolve a projectHash to a `TieredIdentity` by looking up the mapped
    /// path and then inferring repo identity from that path.
    pub fn resolve(&self, project_hash: &str) -> Option<TieredIdentity> {
        let root_path = self.mappings.get(project_hash)?;
        let path = PathBuf::from(root_path);
        let identity = infer_repo_identity_from_path(&path)?;
        Some(TieredIdentity {
            identity,
            tier: SourceTier::Secondary,
        })
    }
}

pub fn semantic_segments(entries: &[TimelineEntry]) -> Vec<SemanticSegment> {
    // Load the project-hash registry from disk (AICX_HOME or ~/.aicx),
    // not the empty `Default`. Without this, Gemini sessions whose
    // identity arrives as a `projectHash` always resolved to Opaque
    // even when the operator's `gemini-project-map.json` was sitting
    // right there — the helper existed but no hot path called it.
    semantic_segments_with_registry(entries, &ProjectHashRegistry::load_default())
}

/// Same as [`semantic_segments`] but emits per-session cumulative
/// entry-processed counts to `progress`, so callers can pin a
/// `Heartbeat` floor to real work done (rather than ticking blind).
/// Pass-4 follow-up: the segment phase moved AHEAD of dedup, so for
/// large corpora the progress bar needs a real denominator to stay
/// honest.
pub fn semantic_segments_with_progress(
    entries: &[TimelineEntry],
    progress: impl FnMut(usize),
) -> Vec<SemanticSegment> {
    semantic_segments_with_registry_and_progress(
        entries,
        &ProjectHashRegistry::load_default(),
        progress,
    )
}

pub fn semantic_segments_with_registry(
    entries: &[TimelineEntry],
    registry: &ProjectHashRegistry,
) -> Vec<SemanticSegment> {
    semantic_segments_with_registry_and_progress(entries, registry, |_| {})
}

pub fn semantic_segments_with_registry_and_progress(
    entries: &[TimelineEntry],
    registry: &ProjectHashRegistry,
    mut progress: impl FnMut(usize),
) -> Vec<SemanticSegment> {
    let mut sessions: HashMap<(String, String), Vec<TimelineEntry>> = HashMap::new();
    for entry in entries {
        sessions
            .entry((entry.agent.clone(), entry.session_id.clone()))
            .or_default()
            .push(entry.clone());
    }

    let mut ordered = Vec::new();
    let mut processed_entries: usize = 0;

    for ((agent, session_id), mut session_entries) in sessions {
        let session_len = session_entries.len();
        session_entries.sort_by_key(|left| left.timestamp);

        let mut current_tiered: Option<TieredIdentity> = None;
        let mut current_entries: Vec<TimelineEntry> = Vec::new();

        for entry in session_entries {
            let explicit = infer_tiered_identity_from_entry(&entry, registry);

            let explicit_repo = explicit.as_ref().map(|t| &t.identity);
            let current_repo = current_tiered.as_ref().map(|t| &t.identity);

            let split_for_first_truth =
                !current_entries.is_empty() && current_repo.is_none() && explicit_repo.is_some();
            let split_for_context_switch = !current_entries.is_empty()
                && explicit_repo
                    .zip(current_repo)
                    .is_some_and(|(next_repo, active_repo)| next_repo != active_repo);

            if split_for_first_truth || split_for_context_switch {
                let tier = current_tiered.as_ref().map(|t| t.tier);
                ordered.push(build_segment(
                    current_tiered.take().map(|t| t.identity),
                    tier,
                    &agent,
                    &session_id,
                    std::mem::take(&mut current_entries),
                ));
            }

            if current_entries.is_empty() {
                current_tiered = explicit.clone();
            }

            if current_tiered.is_none() && explicit.is_some() {
                current_tiered = explicit.clone();
            }

            current_entries.push(entry);
        }

        if !current_entries.is_empty() {
            let tier = current_tiered.as_ref().map(|t| t.tier);
            ordered.push(build_segment(
                current_tiered.map(|t| t.identity),
                tier,
                &agent,
                &session_id,
                current_entries,
            ));
        }

        processed_entries += session_len;
        progress(processed_entries);
    }

    ordered.sort_by(|left, right| {
        left.entries
            .first()
            .map(|entry| entry.timestamp)
            .cmp(&right.entries.first().map(|entry| entry.timestamp))
            .then_with(|| left.agent.cmp(&right.agent))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });

    ordered
}

pub fn infer_repo_identity_from_entry(entry: &TimelineEntry) -> Option<RepoIdentity> {
    infer_tiered_identity_from_entry(entry, &ProjectHashRegistry::default()).map(|t| t.identity)
}

pub fn resolve_bucket(entry: &TimelineEntry, registry: &ProjectHashRegistry) -> BucketResolution {
    if let Some(tiered) = infer_tiered_identity_from_cwd(entry.cwd.as_deref()) {
        let source = match tiered.tier {
            SourceTier::Primary => BucketingSource::CwdGitRemote,
            SourceTier::Secondary => BucketingSource::CwdGitRoot,
            SourceTier::Fallback => BucketingSource::KnownLayout,
            SourceTier::Opaque => BucketingSource::Unclassified,
        };
        return BucketResolution {
            bucket: tiered.identity.slug(),
            source,
            identity: Some(tiered.identity),
        };
    }

    if let Some(cwd) = entry.cwd.as_deref()
        && looks_like_weak_source_identifier(cwd)
        && let Some(tiered) = registry.resolve(cwd)
    {
        return BucketResolution {
            bucket: tiered.identity.slug(),
            source: BucketingSource::KnownLayout,
            identity: Some(tiered.identity),
        };
    }

    if let Some(tiered) = infer_tiered_identity_from_text(&entry.message) {
        return BucketResolution {
            bucket: tiered.identity.slug(),
            source: BucketingSource::ContentMention,
            identity: Some(tiered.identity),
        };
    }

    BucketResolution {
        bucket: "unclassified".to_string(),
        source: BucketingSource::Unclassified,
        identity: None,
    }
}

/// Infer repo identity with explicit trust tier from source-side signals.
///
/// Signal precedence (highest to lowest):
/// 1. CWD that resolves via local git + remote -> Primary
/// 2. CWD that resolves via local git + known layout/basename -> Secondary
/// 3. CWD via known layout (no .git) -> Fallback
/// 4. ProjectHash resolved through registry (Gemini) -> Secondary
///
/// Text mentions are deliberately NOT a fallback here. A chunk may legitimately
/// quote URLs or paths from other repos as discussion material — promoting any
/// of them to entry identity would (a) split a single-owner session across
/// `current_repo != explicit_repo` segments and (b) smear `segment.repo` with
/// non-ownership signals downstream consumers (search, dashboard) may grow to
/// trust. Standalone callers that want "what does this entry mention?" can use
/// [`resolve_bucket`] with `BucketingSource::ContentMention`.
pub fn infer_tiered_identity_from_entry(
    entry: &TimelineEntry,
    registry: &ProjectHashRegistry,
) -> Option<TieredIdentity> {
    if let Some(tiered) = infer_tiered_identity_from_cwd(entry.cwd.as_deref()) {
        return Some(tiered);
    }

    // Last resort: try projectHash registry for Gemini sessions.
    // The cwd field for Gemini sessions is often the projectHash itself.
    if let Some(cwd) = entry.cwd.as_deref()
        && looks_like_weak_source_identifier(cwd)
    {
        return registry.resolve(cwd);
    }

    None
}

/// Classify a raw CWD string into a source tier without resolving identity.
pub fn classify_cwd_tier(cwd: Option<&str>) -> SourceTier {
    let Some(raw) = cwd else {
        return SourceTier::Opaque;
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return SourceTier::Opaque;
    }
    if looks_like_weak_source_identifier(trimmed) {
        return SourceTier::Opaque;
    }
    let path = expand_home(trimmed);
    if discover_git_root(&path).is_some() {
        return SourceTier::Secondary;
    }
    if infer_repo_identity_from_known_layout(&path).is_some() {
        return SourceTier::Fallback;
    }
    SourceTier::Opaque
}

fn build_segment(
    repo: Option<RepoIdentity>,
    source_tier: Option<SourceTier>,
    agent: &str,
    session_id: &str,
    entries: Vec<TimelineEntry>,
) -> SemanticSegment {
    let kind = classify_segment_kind(&entries);
    SemanticSegment {
        repo,
        source_tier,
        kind,
        agent: agent.to_string(),
        session_id: session_id.to_string(),
        entries,
    }
}

fn classify_segment_kind(entries: &[TimelineEntry]) -> Kind {
    if entries.is_empty() {
        return Kind::Other;
    }

    let has_conversation = entries
        .iter()
        .any(|entry| entry.role == "user" || entry.role == "assistant");

    let report_score = entries
        .iter()
        .map(|entry| classify_report_signal(entry.message.as_str()))
        .sum::<u8>();
    let plan_score = entries
        .iter()
        .map(|entry| classify_plan_signal(entry.message.as_str()))
        .sum::<u8>();

    if report_score >= 2 && report_score > plan_score && !has_conversation {
        Kind::Reports
    } else if plan_score >= 2 && plan_score >= report_score {
        Kind::Plans
    } else if has_conversation {
        Kind::Conversations
    } else if report_score > 0 {
        Kind::Reports
    } else {
        Kind::Other
    }
}

fn classify_plan_signal(message: &str) -> u8 {
    let lower = message.to_ascii_lowercase();
    u8::from(lower.contains("goal:"))
        + u8::from(lower.contains("acceptance:"))
        + u8::from(lower.contains("test gate:"))
        + u8::from(lower.contains("- [ ]"))
        + u8::from(lower.contains("plan:"))
        + u8::from(lower.contains("migration plan"))
}

fn classify_report_signal(message: &str) -> u8 {
    let lower = message.to_ascii_lowercase();
    u8::from(lower.contains("recovery report"))
        + u8::from(lower.contains("audit report"))
        + u8::from(lower.contains("coverage report"))
        + u8::from(lower.contains("status report"))
        + u8::from(lower.contains("summary"))
}

fn infer_repo_identity_from_path(path: &Path) -> Option<RepoIdentity> {
    if let Some(repo) = infer_repo_identity_from_local_git(path) {
        return Some(repo);
    }

    infer_repo_identity_from_known_layout(path)
}

// ── Tiered inference helpers ──────────────────────────────────────────────

fn infer_tiered_identity_from_text(text: &str) -> Option<TieredIdentity> {
    // Content mentions are tag-only signals — never assertable. URL mentions
    // map to Fallback identity for search hinting. FS-resolved path mentions
    // were removed: chunks can quote any path on disk, and walking the FS to
    // validate them leaks ownership from unrelated local repos through
    // `git remote get-url origin` (see fix/segmentation-identity-leak).
    infer_repo_identity_from_remote_like(text).map(|identity| TieredIdentity {
        identity,
        tier: SourceTier::Fallback,
    })
}

/// Resolve a canonical `(organization, repository)` identity from a
/// cwd string by consulting ground-truth signals — git remote URL,
/// then known-layout heuristics, finally URL-shape inference.
///
/// Returns `None` when no canonical identity can be honestly resolved.
/// Made `pub` so `src/sources.rs::project_filter_matches_path` can use
/// the same canonical resolver instead of the prior path-segment
/// heuristic (which leaked cross-org per `chatgpt-codex-connector`
/// P1 review on PR #8; see Wave F-2 follow-up commit body).
pub fn infer_tiered_identity_from_cwd(cwd: Option<&str>) -> Option<TieredIdentity> {
    let cwd = cwd?.trim();
    if cwd.is_empty() || looks_like_weak_source_identifier(cwd) {
        return None;
    }

    // Remote-like CWD → Primary
    if let Some(identity) = infer_repo_identity_from_remote_like(cwd) {
        return Some(TieredIdentity {
            identity,
            tier: SourceTier::Primary,
        });
    }

    let path = expand_home(cwd);
    infer_tiered_identity_from_path(&path)
}

fn infer_tiered_identity_from_path(path: &Path) -> Option<TieredIdentity> {
    // Local git with remote → Primary
    if let Some(repo_root) = discover_git_root(path) {
        if let Some(identity) = infer_repo_identity_from_git_remote(&repo_root) {
            return Some(TieredIdentity {
                identity,
                tier: SourceTier::Primary,
            });
        }
        // Local git with known layout → Secondary
        if let Some(identity) = infer_repo_identity_from_known_layout(&repo_root) {
            return Some(TieredIdentity {
                identity,
                tier: SourceTier::Secondary,
            });
        }
        // Local git, basename only → Secondary
        if let Some(name) = repo_root.file_name() {
            return Some(TieredIdentity {
                identity: RepoIdentity {
                    organization: "local".to_string(),
                    repository: name.to_string_lossy().to_string(),
                },
                tier: SourceTier::Secondary,
            });
        }
    }

    // Known layout without .git → Fallback
    if let Some(identity) = infer_repo_identity_from_known_layout(path) {
        return Some(TieredIdentity {
            identity,
            tier: SourceTier::Fallback,
        });
    }

    None
}

fn infer_repo_identity_from_local_git(path: &Path) -> Option<RepoIdentity> {
    let repo_root = discover_git_root(path)?;
    infer_repo_identity_from_git_remote(&repo_root)
        .or_else(|| infer_repo_identity_from_known_layout(&repo_root))
        .or_else(|| {
            repo_root.file_name().map(|name| RepoIdentity {
                organization: "local".to_string(),
                repository: name.to_string_lossy().to_string(),
            })
        })
}

fn discover_git_root(path: &Path) -> Option<PathBuf> {
    let seed = if path.is_file() {
        path.parent()?.to_path_buf()
    } else {
        path.to_path_buf()
    };

    seed.ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
}

fn infer_repo_identity_from_git_remote(repo_root: &Path) -> Option<RepoIdentity> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let remote = String::from_utf8_lossy(&output.stdout);
    infer_repo_identity_from_remote_like(remote.trim())
}

fn infer_repo_identity_from_known_layout(path: &Path) -> Option<RepoIdentity> {
    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect();

    for marker in ["hosted", "repos", "repositories", "github", "git"] {
        let Some(marker_index) = components
            .iter()
            .position(|component| component.eq_ignore_ascii_case(marker))
        else {
            continue;
        };
        if components.len() > marker_index + 2 {
            let organization = components[marker_index + 1].clone();
            let repository = components[marker_index + 2].clone();
            if is_probably_repo_name(&organization) && is_probably_repo_name(&repository) {
                return Some(RepoIdentity {
                    organization,
                    repository,
                });
            }
        }
    }

    None
}

fn infer_repo_identity_from_remote_like(raw: &str) -> Option<RepoIdentity> {
    for token in raw.split_whitespace() {
        let trimmed = token
            .trim_matches(|ch: char| matches!(ch, '"' | '\'' | ',' | '.' | ')' | '(' | '[' | ']'));
        for prefix in [
            "https://github.com/",
            "http://github.com/",
            "https://gitlab.com/",
            "http://gitlab.com/",
            "git@github.com:",
            "git@gitlab.com:",
        ] {
            if let Some(rest) = trimmed.strip_prefix(prefix)
                && let Some(repo) = repo_identity_from_remote_path(rest)
            {
                return Some(repo);
            }
        }
    }

    None
}

fn repo_identity_from_remote_path(path: &str) -> Option<RepoIdentity> {
    let mut parts = path.split('/');
    let organization = parts.next()?.trim();
    let repository = parts.next()?.trim().trim_end_matches(".git");

    if is_probably_repo_name(organization) && is_probably_repo_name(repository) {
        return Some(RepoIdentity {
            organization: organization.to_string(),
            repository: repository.to_string(),
        });
    }

    Some(RepoIdentity {
        organization: "local".to_string(),
        repository: local_repo_fallback(repository),
    })
}

fn local_repo_fallback(repository: &str) -> String {
    if is_probably_repo_name(repository) {
        repository.to_string()
    } else {
        "unknown".to_string()
    }
}

fn looks_like_weak_source_identifier(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.len() >= 16
        && trimmed.chars().all(|ch| ch.is_ascii_hexdigit())
        && !trimmed.contains('/')
        && !trimmed.contains(':')
}

fn expand_home(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
    {
        return home.join(rest);
    }

    PathBuf::from(raw)
}

fn is_probably_repo_name(value: &str) -> bool {
    if value.is_empty() || value.len() > 64 {
        return false;
    }

    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_')) {
        return false;
    }

    let lower = value.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "." | ".."
            | "..."
            | "local"
            | "tmp"
            | "temp"
            | "src"
            | "app"
            | "lib"
            | "docs"
            | "workspace"
            | "workspaces"
    ) {
        return false;
    }

    let dot_count = value.chars().filter(|ch| *ch == '.').count();
    if dot_count > value.chars().count() / 2 {
        return false;
    }

    // Date-shaped strings (`2026-01-22`, `2026_01_22`, `2026_0122`) sneak past
    // the alphanumeric+`.-_` filter and have landed in the canonical store as
    // pseudo-repos before. Treat them as not-a-repo so layout inference and
    // segmentation never accept a folder dated like a session-bucket.
    if looks_like_date_pattern(value) {
        return false;
    }

    true
}

/// Returns true if `value` is shaped like a calendar date (no other content).
///
/// Recognized: `YYYY-MM-DD`, `YYYY_MM_DD`, `YYYY_MMDD`. The check is
/// shape-only (digits + separator placement); we do not validate that the
/// month/day fall within a real calendar — `2026-99-99` is still rejected as
/// "repo-like" because the *intent* is clearly a date bucket, not a repo.
/// The `YYYY_MMDD` arm is intentionally aligned with state migration's
/// compact store date-dir skip heuristic.
fn looks_like_date_pattern(value: &str) -> bool {
    let bytes = value.as_bytes();
    match bytes.len() {
        // YYYY-MM-DD or YYYY_MM_DD
        10 => {
            bytes[..4].iter().all(u8::is_ascii_digit)
                && matches!(bytes[4], b'-' | b'_')
                && bytes[5..7].iter().all(u8::is_ascii_digit)
                && bytes[7] == bytes[4]
                && bytes[8..10].iter().all(u8::is_ascii_digit)
        }
        // YYYY_MMDD (compact form used by the canonical store layout)
        9 => {
            bytes[..4].iter().all(u8::is_ascii_digit)
                && bytes[4] == b'_'
                && bytes[5..9].iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::fs;

    fn entry(
        ts: (i32, u32, u32, u32, u32, u32),
        session_id: &str,
        role: &str,
        message: &str,
        cwd: Option<&str>,
    ) -> TimelineEntry {
        TimelineEntry {
            timestamp: Utc
                .with_ymd_and_hms(ts.0, ts.1, ts.2, ts.3, ts.4, ts.5)
                .unwrap(),
            agent: "claude".to_string(),
            session_id: session_id.to_string(),
            role: role.to_string(),
            message: message.to_string(),
            branch: None,
            cwd: cwd.map(ToOwned::to_owned),
            timestamp_source: None,
            frame_kind: None,
        }
    }

    fn mk_tmp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ai-contexters-segmentation-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    #[test]
    fn repo_signal_segmentation_splits_one_session_across_multiple_repositories() {
        // Multi-repo split must come from a source-of-truth signal (cwd
        // switch between two real git repos), NOT from text URL mentions
        // in chunks. Pre-round-2 a session that merely *mentioned* repo B
        // mid-conversation would split into a second non-assertable
        // segment with B's URL as identity; that smear is gone.
        let root = mk_tmp_dir("multi-repo-cwd-switch");
        let repo_a = root.join("hosted").join("VetCoders").join("ai-contexters");
        let repo_b = root.join("hosted").join("VetCoders").join("loctree");
        for r in [&repo_a, &repo_b] {
            fs::create_dir_all(r).unwrap();
            Command::new("git").arg("init").arg(r).output().unwrap();
        }

        let cwd_a = repo_a.to_string_lossy().to_string();
        let cwd_b = repo_b.to_string_lossy().to_string();
        let entries = vec![
            entry(
                (2026, 3, 21, 9, 0, 0),
                "sess-1",
                "user",
                "Please inspect ai-contexters before editing.",
                Some(&cwd_a),
            ),
            entry(
                (2026, 3, 21, 9, 1, 0),
                "sess-1",
                "assistant",
                "I found the store seam.",
                Some(&cwd_a),
            ),
            entry(
                (2026, 3, 21, 9, 2, 0),
                "sess-1",
                "user",
                "Switch to loctree now.",
                Some(&cwd_b),
            ),
            entry(
                (2026, 3, 21, 9, 3, 0),
                "sess-1",
                "assistant",
                "Reviewing loctree next.",
                Some(&cwd_b),
            ),
        ];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].project_label(), "VetCoders/ai-contexters");
        assert_eq!(segments[1].project_label(), "VetCoders/loctree");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn repo_signal_segmentation_keeps_unknown_prefix_honest() {
        // First half of the session has no cwd (no identity); the user then
        // starts working in a real on-disk repo. Segmentation must split:
        // a no-identity prefix (preserved as plan/setup talk) followed by
        // a properly-owned segment once cwd lands. Previously this was
        // exercised by a text URL mention; that path is no longer a signal.
        let root = mk_tmp_dir("unknown-prefix-then-cwd");
        let repo = root.join("hosted").join("VetCoders").join("ai-contexters");
        fs::create_dir_all(&repo).unwrap();
        Command::new("git").arg("init").arg(&repo).output().unwrap();

        let cwd = repo.to_string_lossy().to_string();
        let entries = vec![
            entry(
                (2026, 3, 21, 9, 0, 0),
                "sess-2",
                "user",
                "Need a migration plan but I have not named the repo yet.",
                None,
            ),
            entry(
                (2026, 3, 21, 9, 1, 0),
                "sess-2",
                "assistant",
                "Drafting a migration plan with acceptance criteria.",
                None,
            ),
            entry(
                (2026, 3, 21, 9, 2, 0),
                "sess-2",
                "user",
                "Now working in the repo on disk.",
                Some(&cwd),
            ),
        ];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 2);
        assert!(segments[0].repo.is_none());
        assert_eq!(segments[0].kind, Kind::Plans);
        assert_eq!(segments[1].project_label(), "VetCoders/ai-contexters");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn repo_signal_segmentation_ignores_gemini_hash_like_cwd() {
        let entry = entry(
            (2026, 3, 21, 9, 0, 0),
            "sess-3",
            "user",
            "No trustworthy repo here.",
            Some("57cfd37b3a72d995c4f2d018ebf9d5a2"),
        );

        assert!(infer_repo_identity_from_entry(&entry).is_none());
        let segments = semantic_segments(&[entry]);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].repo.is_none());
    }

    #[test]
    fn repo_signal_segmentation_uses_local_git_remote_when_available() {
        let root = mk_tmp_dir("git-remote");
        let repo = root.join("hosted").join("VetCoders").join("ai-contexters");
        fs::create_dir_all(&repo).unwrap();

        Command::new("git")
            .arg("init")
            .arg(&repo)
            .output()
            .expect("git init should run");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:VetCoders/ai-contexters.git",
            ])
            .output()
            .expect("git remote add should run");

        let entry = entry(
            (2026, 3, 21, 9, 0, 0),
            "sess-4",
            "user",
            "Inspect the repo on disk.",
            Some(repo.to_string_lossy().as_ref()),
        );

        let repo_identity = infer_repo_identity_from_entry(&entry).expect("repo identity");
        assert_eq!(repo_identity.slug(), "VetCoders/ai-contexters");

        let _ = fs::remove_dir_all(&root);
    }

    // ================================================================
    // Source tier tests
    // ================================================================

    // (Removed `source_tier_github_url_is_primary` — superseded by
    //  `entry_identity_ignores_text_url_mentions` /
    //  `resolve_bucket_still_surfaces_text_url_mentions` below.)

    #[test]
    fn cwd_git_identity_wins_over_content_mentions() {
        let root = mk_tmp_dir("cwd-wins");
        let repo = root.join("Git").join("vista");
        fs::create_dir_all(&repo).unwrap();

        Command::new("git")
            .arg("init")
            .arg(&repo)
            .output()
            .expect("git init");

        let e = entry(
            (2026, 5, 6, 10, 0, 0),
            "sess-cwd-wins",
            "user",
            "We need to inspect https://github.com/RustCrypto/RSA while working locally.",
            Some(repo.to_string_lossy().as_ref()),
        );

        let resolution = resolve_bucket(&e, &ProjectHashRegistry::default());
        assert_eq!(resolution.bucket, "local/vista");
        assert_eq!(resolution.source, BucketingSource::CwdGitRoot);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_template_literals() {
        assert!(!is_probably_repo_name("{target_owner}"));
        assert!(!is_probably_repo_name("<YOUR_USERNAME>"));
        assert!(!is_probably_repo_name("${RELEASE_REPO}"));
        assert!(!is_probably_repo_name("$REPO"));
        assert!(!is_probably_repo_name("{org}"));
    }

    #[test]
    fn rejects_dot_only_and_traversal_strings() {
        assert!(!is_probably_repo_name("..."));
        assert!(!is_probably_repo_name(".."));
        assert!(!is_probably_repo_name("."));
        assert!(!is_probably_repo_name(".../"));
        assert!(!is_probably_repo_name("..hidden"));
    }

    #[test]
    fn rejects_control_chars_and_separators() {
        assert!(!is_probably_repo_name("foo/bar"));
        assert!(!is_probably_repo_name("foo\\bar"));
        assert!(!is_probably_repo_name("foo\nbar"));
        assert!(!is_probably_repo_name("foo bar"));
        assert!(!is_probably_repo_name(""));
    }

    #[test]
    fn accepts_real_repo_names() {
        assert!(is_probably_repo_name("vibecrafted"));
        assert!(is_probably_repo_name("rust-memex"));
        assert!(is_probably_repo_name("ai-contexters"));
        assert!(is_probably_repo_name("vc-runtime"));
        assert!(is_probably_repo_name("CodeScribe"));
        assert!(is_probably_repo_name("starship"));
        assert!(is_probably_repo_name("01mf02"));
        assert!(is_probably_repo_name("a"));
    }

    #[test]
    fn fallback_routes_invalid_remote_owner_to_local_bucket() {
        // After round-2, text URLs no longer feed segment identity, so this
        // test exercises the lower-level `resolve_bucket` API instead. The
        // malformed-owner → `local/<repo>` rule still belongs to the parser
        // (anything routing through `infer_repo_identity_from_remote_like`
        // must not silently materialize a `{placeholder}/...` org).
        let e = entry(
            (2026, 3, 22, 10, 0, 0),
            "sess-local-fallback",
            "user",
            "Clone https://github.com/{target_owner}/vibecrafted.git before release.",
            None,
        );
        let resolution = resolve_bucket(&e, &ProjectHashRegistry::default());
        assert_eq!(resolution.source, BucketingSource::ContentMention);
        assert_eq!(resolution.bucket, "local/vibecrafted");
        assert_ne!(resolution.bucket, "{target_owner}/vibecrafted");

        // Segment pipeline ignores text mentions entirely → non-assertable.
        let segments = semantic_segments(&[e]);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].repo.is_none());
        assert!(!segments[0].has_assertable_identity());
    }

    #[test]
    fn fallback_routes_invalid_remote_repo_to_unknown_local_bucket() {
        let identity = infer_repo_identity_from_remote_like(
            "https://github.com/VetCoders/${RELEASE_REPO}.git",
        )
        .expect("malformed repository should resolve to local unknown fallback");

        assert_eq!(identity.slug(), "local/unknown");
    }

    #[test]
    fn source_tier_git_remote_cwd_is_primary() {
        let root = mk_tmp_dir("tier-git-remote");
        let repo = root.join("hosted").join("VetCoders").join("loctree");
        fs::create_dir_all(&repo).unwrap();

        Command::new("git")
            .arg("init")
            .arg(&repo)
            .output()
            .expect("git init");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:VetCoders/loctree.git",
            ])
            .output()
            .expect("git remote add");

        let e = entry(
            (2026, 3, 22, 10, 0, 0),
            "sess-tier-git",
            "user",
            "Working in the repo.",
            Some(repo.to_string_lossy().as_ref()),
        );

        let tiered = infer_tiered_identity_from_entry(&e, &ProjectHashRegistry::default())
            .expect("should resolve");
        assert_eq!(tiered.tier, SourceTier::Primary);
        assert_eq!(tiered.identity.slug(), "VetCoders/loctree");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn source_tier_known_layout_without_git_is_fallback() {
        let e = entry(
            (2026, 3, 22, 10, 0, 0),
            "sess-tier-layout",
            "user",
            "Working at /nonexistent/hosted/SomeOrg/SomeRepo",
            None,
        );
        let tiered = infer_tiered_identity_from_entry(&e, &ProjectHashRegistry::default());
        // Path in message text resolved via known layout (no .git) → Fallback
        if let Some(t) = tiered {
            assert_eq!(t.tier, SourceTier::Fallback);
            assert!(!t.tier.is_assertable());
        }
        // It's also OK if it returns None (path doesn't exist on disk)
    }

    #[test]
    fn source_tier_hex_hash_cwd_is_opaque_without_registry() {
        let e = entry(
            (2026, 3, 22, 10, 0, 0),
            "sess-tier-hash",
            "user",
            "Hello from Gemini.",
            Some("fef6ad02174d592d21e7f8a6143564388027ec0c"),
        );
        let tiered = infer_tiered_identity_from_entry(&e, &ProjectHashRegistry::default());
        assert!(
            tiered.is_none(),
            "hex hash without registry must not resolve"
        );
    }

    #[test]
    fn source_tier_hex_hash_resolves_through_registry() {
        let root = mk_tmp_dir("tier-registry");
        let repo = root.join("hosted").join("VetCoders").join("ai-contexters");
        fs::create_dir_all(&repo).unwrap();

        Command::new("git")
            .arg("init")
            .arg(&repo)
            .output()
            .expect("git init");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:VetCoders/ai-contexters.git",
            ])
            .output()
            .expect("git remote add");

        let mut registry = ProjectHashRegistry::default();
        registry.mappings.insert(
            "fef6ad02174d592d21e7f8a6143564388027ec0c".to_string(),
            repo.to_string_lossy().to_string(),
        );

        let e = entry(
            (2026, 3, 22, 10, 0, 0),
            "sess-tier-reg",
            "user",
            "Hello from Gemini.",
            Some("fef6ad02174d592d21e7f8a6143564388027ec0c"),
        );

        let tiered =
            infer_tiered_identity_from_entry(&e, &registry).expect("registry should resolve");
        assert_eq!(tiered.tier, SourceTier::Secondary);
        assert_eq!(tiered.identity.slug(), "VetCoders/ai-contexters");
        assert!(tiered.tier.is_assertable());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn source_tier_registry_with_unknown_hash_returns_none() {
        let registry = ProjectHashRegistry::default();
        let e = entry(
            (2026, 3, 22, 10, 0, 0),
            "sess-tier-unknown",
            "user",
            "Hello from Gemini.",
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        );
        let tiered = infer_tiered_identity_from_entry(&e, &registry);
        assert!(
            tiered.is_none(),
            "unknown hash must not resolve even with empty registry"
        );
    }

    #[test]
    fn source_tier_classify_cwd_empty_is_opaque() {
        assert_eq!(classify_cwd_tier(None), SourceTier::Opaque);
        assert_eq!(classify_cwd_tier(Some("")), SourceTier::Opaque);
    }

    #[test]
    fn source_tier_classify_cwd_hex_is_opaque() {
        assert_eq!(
            classify_cwd_tier(Some("57cfd37b3a72d995c4f2d018ebf9d5a2")),
            SourceTier::Opaque
        );
    }

    #[test]
    fn segments_carry_source_tier() {
        // Source-tier propagation is now demonstrated through cwd-derived
        // identity (the only entry-level identity path). Text URL mentions
        // no longer produce a Fallback-tier segment.
        let root = mk_tmp_dir("segments-carry-tier");
        let repo = root.join("hosted").join("VetCoders").join("ai-contexters");
        fs::create_dir_all(&repo).unwrap();
        Command::new("git").arg("init").arg(&repo).output().unwrap();

        let cwd = repo.to_string_lossy().to_string();
        let entries = vec![
            entry(
                (2026, 3, 22, 10, 0, 0),
                "sess-st",
                "user",
                "Working on ai-contexters.",
                Some(&cwd),
            ),
            entry(
                (2026, 3, 22, 10, 1, 0),
                "sess-st",
                "assistant",
                "Reviewing now.",
                Some(&cwd),
            ),
        ];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].source_tier, Some(SourceTier::Secondary));
        assert!(segments[0].has_assertable_identity());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn segments_without_repo_have_no_tier() {
        let entries = vec![entry(
            (2026, 3, 22, 10, 0, 0),
            "sess-none",
            "user",
            "Just chatting, no repo context.",
            None,
        )];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].repo.is_none());
        assert!(segments[0].source_tier.is_none());
        assert!(!segments[0].has_assertable_identity());
    }

    #[test]
    fn segments_opaque_cwd_routes_to_non_repo() {
        let entries = vec![entry(
            (2026, 3, 22, 10, 0, 0),
            "sess-opaque",
            "user",
            "Gemini session with opaque hash only.",
            Some("fef6ad02174d592d21e7f8a6143564388027ec0c"),
        )];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].repo.is_none());
        assert_eq!(segments[0].project_label(), "non-repository-contexts");
    }

    #[test]
    fn segments_opaque_cwd_resolves_with_registry() {
        let root = mk_tmp_dir("seg-registry");
        let repo = root.join("hosted").join("VetCoders").join("ai-contexters");
        fs::create_dir_all(&repo).unwrap();

        Command::new("git")
            .arg("init")
            .arg(&repo)
            .output()
            .expect("git init");
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:VetCoders/ai-contexters.git",
            ])
            .output()
            .expect("git remote add");

        let mut registry = ProjectHashRegistry::default();
        registry.mappings.insert(
            "fef6ad02174d592d21e7f8a6143564388027ec0c".to_string(),
            repo.to_string_lossy().to_string(),
        );

        let entries = vec![entry(
            (2026, 3, 22, 10, 0, 0),
            "sess-reg",
            "user",
            "Gemini session with mapped hash.",
            Some("fef6ad02174d592d21e7f8a6143564388027ec0c"),
        )];

        let segments = semantic_segments_with_registry(&entries, &registry);
        assert_eq!(segments.len(), 1);
        assert!(segments[0].repo.is_some());
        assert_eq!(segments[0].source_tier, Some(SourceTier::Secondary));
        assert_eq!(segments[0].project_label(), "VetCoders/ai-contexters");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn project_hash_registry_roundtrip() {
        let root = mk_tmp_dir("registry-roundtrip");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("gemini-project-map.json");

        let mut registry = ProjectHashRegistry::default();
        registry.mappings.insert(
            "abc123".to_string(),
            "/home/user/repos/my-project".to_string(),
        );

        let json = serde_json::to_string_pretty(&registry).unwrap();
        fs::write(&path, &json).unwrap();

        let loaded = ProjectHashRegistry::load_from(&path);
        assert_eq!(loaded.mappings.len(), 1);
        assert_eq!(
            loaded.mappings.get("abc123").map(String::as_str),
            Some("/home/user/repos/my-project")
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn project_hash_registry_missing_file_returns_empty() {
        let registry = ProjectHashRegistry::load_from(Path::new("/nonexistent/path.json"));
        assert!(registry.mappings.is_empty());
    }

    #[test]
    fn source_tier_ordering() {
        assert!(SourceTier::Primary < SourceTier::Secondary);
        assert!(SourceTier::Secondary < SourceTier::Fallback);
        assert!(SourceTier::Fallback < SourceTier::Opaque);
    }

    #[test]
    fn source_tier_assertable_boundaries() {
        assert!(SourceTier::Primary.is_assertable());
        assert!(SourceTier::Secondary.is_assertable());
        assert!(!SourceTier::Fallback.is_assertable());
        assert!(!SourceTier::Opaque.is_assertable());
    }

    #[test]
    fn infer_repo_identity_from_known_layout_matches_hosted() {
        let path = Path::new("/Users/x/hosted/MyOrg/my-repo/src/lib.rs");
        let id = infer_repo_identity_from_known_layout(path).expect("hosted match");
        assert_eq!(id.organization, "MyOrg");
        assert_eq!(id.repository, "my-repo");
    }

    #[test]
    fn infer_repo_identity_from_known_layout_matches_repos() {
        // Previously dead code: `?` inside the loop returned from the function
        // on the first missing marker, so only "hosted" was ever tried.
        let path = Path::new("/Users/x/repos/MyOrg/my-repo/src/lib.rs");
        let id = infer_repo_identity_from_known_layout(path).expect("repos match");
        assert_eq!(id.organization, "MyOrg");
        assert_eq!(id.repository, "my-repo");
    }

    #[test]
    fn infer_repo_identity_from_known_layout_matches_repositories() {
        let path = Path::new("/Users/x/repositories/MyOrg/my-repo/file.txt");
        let id = infer_repo_identity_from_known_layout(path).expect("repositories match");
        assert_eq!(id.organization, "MyOrg");
        assert_eq!(id.repository, "my-repo");
    }

    #[test]
    fn infer_repo_identity_from_known_layout_matches_github() {
        let path = Path::new("/home/user/github/MyOrg/my-repo/src");
        let id = infer_repo_identity_from_known_layout(path).expect("github match");
        assert_eq!(id.organization, "MyOrg");
        assert_eq!(id.repository, "my-repo");
    }

    #[test]
    fn infer_repo_identity_from_known_layout_matches_git() {
        let path = Path::new("/home/user/git/MyOrg/my-repo");
        let id = infer_repo_identity_from_known_layout(path).expect("git match");
        assert_eq!(id.organization, "MyOrg");
        assert_eq!(id.repository, "my-repo");
    }

    #[test]
    fn infer_repo_identity_from_known_layout_returns_none_when_no_marker() {
        let path = Path::new("/tmp/scratch/work/file.txt");
        assert!(infer_repo_identity_from_known_layout(path).is_none());
    }

    #[test]
    fn infer_repo_identity_from_known_layout_rejects_non_repo_name_segments() {
        // The `is_probably_repo_name` guard still applies after the loop fix.
        // Org/repo segments must be alphanumeric + `.-_` only.
        let path = Path::new("/Users/x/repos/My Org With Spaces/my-repo");
        assert!(infer_repo_identity_from_known_layout(path).is_none());
    }

    // ================================================================
    // Identity-leak regression: text mentions must not assert ownership
    // ================================================================

    #[test]
    fn known_layout_marker_matches_case_insensitive() {
        // Mac convention `/Users/<u>/Git/...` (capital G) must match the
        // lowercase `git` marker. Previously case-sensitive comparison rejected
        // it, sending identity inference into the now-removed text fallback.
        let path = Path::new("/Users/test-user/Git/VetCoders/ai-contexters/src/lib.rs");
        let id = infer_repo_identity_from_known_layout(path).expect("Git (capital) matches");
        assert_eq!(id.organization, "VetCoders");
        assert_eq!(id.repository, "ai-contexters");

        // Mixed-case markers also accepted (defensive).
        let mixed = Path::new("/srv/Hosted/OrgA/repo-x");
        let id_mixed = infer_repo_identity_from_known_layout(mixed).expect("Hosted match");
        assert_eq!(id_mixed.organization, "OrgA");
        assert_eq!(id_mixed.repository, "repo-x");
    }

    #[test]
    fn text_mention_with_disk_path_no_longer_resolves_to_assertable_tier() {
        // Regression: previously, a text mention containing any absolute path
        // that walked to a real `.git` on disk produced a Primary/Secondary
        // tier via `git remote get-url origin`. That leaked ownership from
        // local-clone-folder-name → remote-URL repo (e.g. cwd `vista-codex`
        // with chunk mentioning `/Users/.../ai-collaborators/.git/...` and that
        // repo's remote pointing to `Szowesgad/maciej-almanach.git`).
        //
        // After fix: `infer_tiered_identity_from_text` only reads `https://github.com/X/Y`
        // URL mentions and clamps the tier to Fallback. Path mentions are ignored.
        let root = mk_tmp_dir("text-path-no-leak");
        let real_git_repo = root.join("hosted").join("EvilOrg").join("evil-repo");
        fs::create_dir_all(&real_git_repo).unwrap();
        Command::new("git")
            .arg("init")
            .arg(&real_git_repo)
            .output()
            .expect("git init");
        Command::new("git")
            .arg("-C")
            .arg(&real_git_repo)
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:EvilOrg/evil-repo.git",
            ])
            .output()
            .expect("git remote add");

        let chunk_text = format!(
            "Scanning empty directories. Found: {} (this is just a mention)",
            real_git_repo.display()
        );
        let e = entry(
            (2026, 5, 19, 10, 0, 0),
            "sess-leak",
            "user",
            &chunk_text,
            None,
        );

        // No identity at all — text path mentions are no longer FS-validated.
        assert!(infer_tiered_identity_from_entry(&e, &ProjectHashRegistry::default()).is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn entry_identity_ignores_text_url_mentions() {
        // Round 2: even URL mentions are no longer entry identity. Promoting
        // them produced segment.repo smear and split sessions on context_switch
        // when current cwd-derived identity differed from a chunk's mention.
        let e = entry(
            (2026, 5, 19, 10, 0, 0),
            "sess-url",
            "user",
            "See https://github.com/VetCoders/aicx for context.",
            None,
        );
        assert!(
            infer_tiered_identity_from_entry(&e, &ProjectHashRegistry::default()).is_none(),
            "entry-level identity must come from cwd/registry only"
        );
    }

    #[test]
    fn resolve_bucket_still_surfaces_text_url_mentions() {
        // resolve_bucket is a standalone bucket-resolver (separate from segment
        // pipeline). Text URL mentions remain a ContentMention signal so future
        // search-hint use cases can read them — but they do NOT feed segment
        // identity. This test guards that contract.
        let e = entry(
            (2026, 5, 19, 10, 0, 0),
            "sess-bucket",
            "user",
            "Read https://github.com/VetCoders/aicx and tell me what you see.",
            None,
        );
        let resolution = resolve_bucket(&e, &ProjectHashRegistry::default());
        assert_eq!(resolution.source, BucketingSource::ContentMention);
        assert_eq!(resolution.bucket, "VetCoders/aicx");
    }

    #[test]
    fn semantic_segment_does_not_split_on_text_url_mention() {
        // Round-2 regression: a session whose cwd points at VetCoders/Vista
        // (Primary identity, real .git on disk) and whose middle chunk simply
        // mentions a different repo URL must stay as ONE segment with the
        // owner-correct identity. Pre-fix this split into two segments
        // (vetcoders/Vista → VetCoders/aicx Fallback) and the second chunk
        // routed away from its real owner.
        let root = mk_tmp_dir("segment-no-split-on-url");
        let repo = root.join("Git").join("VetCoders").join("vista");
        fs::create_dir_all(&repo).unwrap();
        Command::new("git").arg("init").arg(&repo).output().unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "remote",
                "add",
                "origin",
                "git@github.com:VetCoders/vista.git",
            ])
            .output()
            .unwrap();

        let cwd = repo.to_string_lossy().to_string();
        let entries = vec![
            entry(
                (2026, 5, 19, 10, 0, 0),
                "sess-no-split",
                "user",
                "Start working on Vista.",
                Some(&cwd),
            ),
            entry(
                (2026, 5, 19, 10, 1, 0),
                "sess-no-split",
                "assistant",
                "See https://github.com/VetCoders/aicx for the shared parser.",
                None,
            ),
            entry(
                (2026, 5, 19, 10, 2, 0),
                "sess-no-split",
                "user",
                "Back to Vista.",
                Some(&cwd),
            ),
        ];

        let segments = semantic_segments(&entries);
        assert_eq!(
            segments.len(),
            1,
            "single-owner session must not split on a text URL mention"
        );
        assert_eq!(segments[0].project_label(), "VetCoders/vista");
        assert!(segments[0].has_assertable_identity());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn is_probably_repo_name_rejects_date_patterns() {
        // Live bug: pseudo-projects like `CodeScribe/2026-01-22` made it into
        // ~/.aicx/store/. Block all three date shapes used by known layouts.
        assert!(!is_probably_repo_name("2026-01-22"));
        assert!(!is_probably_repo_name("2026_01_22"));
        assert!(!is_probably_repo_name("2026_0122"));
        // Sanity: real-shaped repo names with a date suffix still pass.
        assert!(is_probably_repo_name("release-2026"));
        assert!(is_probably_repo_name("v2026.01"));
        assert!(is_probably_repo_name("aicx"));
        assert!(is_probably_repo_name("ai-contexters"));
    }

    #[test]
    fn looks_like_date_pattern_recognizes_three_shapes() {
        assert!(looks_like_date_pattern("2026-01-22"));
        assert!(looks_like_date_pattern("2026_01_22"));
        assert!(looks_like_date_pattern("2026_0122"));
        // Out-of-range digits are still date-shaped — intent is what matters.
        assert!(looks_like_date_pattern("9999-99-99"));
        // Negative cases: wrong length, wrong separators, mixed separators.
        assert!(!looks_like_date_pattern("2026-0122")); // 9 chars but wrong sep at idx 4
        assert!(!looks_like_date_pattern("2026-01_22")); // mixed - and _
        assert!(!looks_like_date_pattern("202601-22")); // missing sep at idx 4
        assert!(!looks_like_date_pattern("v2026-01-22")); // extra prefix
        assert!(!looks_like_date_pattern("aicx"));
    }

    #[test]
    fn semantic_segment_text_only_path_stays_non_assertable() {
        // End-to-end: a session with no cwd but with chunks mentioning real
        // on-disk paths must NOT produce an assertable segment. Such segments
        // route to non-repository-contexts in store, not the canonical bucket.
        let root = mk_tmp_dir("segment-text-only");
        let real_git_repo = root.join("Git").join("OrgX").join("repo-y");
        fs::create_dir_all(&real_git_repo).unwrap();
        Command::new("git")
            .arg("init")
            .arg(&real_git_repo)
            .output()
            .expect("git init");

        let chunk_text = format!("Inspecting {}", real_git_repo.display());
        let entries = vec![
            entry(
                (2026, 5, 19, 10, 0, 0),
                "sess-e2e",
                "user",
                &chunk_text,
                None,
            ),
            entry(
                (2026, 5, 19, 10, 1, 0),
                "sess-e2e",
                "assistant",
                "Found some files.",
                None,
            ),
        ];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 1);
        assert!(
            !segments[0].has_assertable_identity(),
            "text-only path mention must never produce assertable identity"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn project_hash_registry_load_default_honors_aicx_home_then_falls_back_to_home() {
        // The registry path was hard-coded to `$HOME/.aicx/...` and
        // ignored the operator's `AICX_HOME` override. Confirm the
        // helper now honors AICX_HOME first, falls back to HOME/.aicx,
        // and returns an empty registry (not panic) when neither env
        // var resolves to an existing file.
        let aicx_home = mk_tmp_dir("registry-aicx-home");
        fs::create_dir_all(&aicx_home).unwrap();
        let map_path = aicx_home.join("gemini-project-map.json");
        fs::write(
            &map_path,
            r#"{ "mappings": { "abc123": "/tmp/aicx-test-registry-target" } }"#,
        )
        .unwrap();

        // Serialize env mutation so we don't fight other tests that
        // also touch HOME/AICX_HOME — the segmentation module owns no
        // other env-touching tests today, but this guard is cheap.
        let prev_aicx_home = std::env::var_os("AICX_HOME");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: This is a single-threaded test and we restore the
        // previous values before returning.
        unsafe {
            std::env::set_var("AICX_HOME", &aicx_home);
        }

        let loaded = ProjectHashRegistry::load_default();
        assert_eq!(
            loaded.mappings.get("abc123").map(String::as_str),
            Some("/tmp/aicx-test-registry-target"),
            "AICX_HOME-rooted registry must load via load_default"
        );

        // Falling back to HOME/.aicx when AICX_HOME is unset.
        let home_root = mk_tmp_dir("registry-home-fallback");
        let home_aicx = home_root.join(".aicx");
        fs::create_dir_all(&home_aicx).unwrap();
        fs::write(
            home_aicx.join("gemini-project-map.json"),
            r#"{ "mappings": { "def456": "/tmp/aicx-test-registry-home" } }"#,
        )
        .unwrap();
        unsafe {
            std::env::remove_var("AICX_HOME");
            std::env::set_var("HOME", &home_root);
        }
        let loaded_home = ProjectHashRegistry::load_default();
        assert_eq!(
            loaded_home.mappings.get("def456").map(String::as_str),
            Some("/tmp/aicx-test-registry-home"),
            "HOME/.aicx must remain the fallback when AICX_HOME is unset"
        );

        // Restore env to whatever the runner had before.
        unsafe {
            match prev_aicx_home {
                Some(v) => std::env::set_var("AICX_HOME", v),
                None => std::env::remove_var("AICX_HOME"),
            }
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = fs::remove_dir_all(&aicx_home);
        let _ = fs::remove_dir_all(&home_root);
    }
}
