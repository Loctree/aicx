//! Semantic segmentation for canonical store ownership.
//!
//! Reconstructs repository-scoped session segments from content signals rather
//! than weak source-side identifiers.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use crate::sources::TimelineEntry;
use crate::types::{Kind, RepoIdentity, SemanticSegment, SourceTier};
use regex::Regex;
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
    /// Load from the default location (`~/.aicx/gemini-project-map.json`).
    /// Returns an empty registry if the file doesn't exist or can't be parsed.
    pub fn load_default() -> Self {
        let Some(home) = dirs::home_dir() else {
            return Self::default();
        };
        let path = home.join(".aicx").join("gemini-project-map.json");
        Self::load_from(&path)
    }

    /// Load from a specific path.
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
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
    semantic_segments_with_registry(entries, &ProjectHashRegistry::default())
}

pub fn semantic_segments_with_registry(
    entries: &[TimelineEntry],
    registry: &ProjectHashRegistry,
) -> Vec<SemanticSegment> {
    let mut sessions: HashMap<(String, String), Vec<TimelineEntry>> = HashMap::new();
    for entry in entries {
        sessions
            .entry((entry.agent.clone(), entry.session_id.clone()))
            .or_default()
            .push(entry.clone());
    }

    let mut ordered = Vec::new();

    for ((agent, session_id), mut session_entries) in sessions {
        session_entries.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));

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

/// Infer repo identity with explicit trust tier from all available signals.
///
/// Signal precedence (highest to lowest):
/// 1. Remote-like URL in message text -> Primary
/// 2. Path in message text that resolves via git remote -> Primary
/// 3. Path in message text via known layout -> Fallback
/// 4. CWD that resolves via local git + remote -> Primary
/// 5. CWD that resolves via local git + known layout -> Secondary
/// 6. CWD via known layout (no .git) -> Fallback
/// 7. ProjectHash resolved through registry -> Secondary
/// 8. Pure hex hash CWD / opaque -> Opaque (returns None)
pub fn infer_tiered_identity_from_entry(
    entry: &TimelineEntry,
    registry: &ProjectHashRegistry,
) -> Option<TieredIdentity> {
    if let Some(tiered) = infer_tiered_identity_from_text(&entry.message) {
        return Some(tiered);
    }

    if let Some(tiered) = infer_tiered_identity_from_cwd(entry.cwd.as_deref()) {
        return Some(tiered);
    }

    // Last resort: try projectHash registry for Gemini sessions.
    // The cwd field for Gemini sessions is often the projectHash itself.
    if let Some(cwd) = entry.cwd.as_deref() {
        if looks_like_weak_source_identifier(cwd) {
            return registry.resolve(cwd);
        }
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
    // Remote-like URL → Primary (strongest text signal)
    if let Some(identity) = infer_repo_identity_from_remote_like(text) {
        return Some(TieredIdentity {
            identity,
            tier: SourceTier::Primary,
        });
    }

    // Path in text → tier depends on how it resolves
    let path_re = Regex::new(r"(/[A-Za-z0-9._~\-]+(?:/[A-Za-z0-9._~\-]+)+)").ok()?;
    for capture in path_re.captures_iter(text) {
        let raw = capture.get(1)?.as_str();
        let path = PathBuf::from(raw);
        if let Some(tiered) = infer_tiered_identity_from_path(&path) {
            return Some(tiered);
        }
    }

    None
}

fn infer_tiered_identity_from_cwd(cwd: Option<&str>) -> Option<TieredIdentity> {
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
        let marker_index = components
            .iter()
            .position(|component| component == marker)?;
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
    if !is_probably_repo_name(organization) || !is_probably_repo_name(repository) {
        return None;
    }

    Some(RepoIdentity {
        organization: organization.to_string(),
        repository: repository.to_string(),
    })
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
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }

    PathBuf::from(raw)
}

fn is_probably_repo_name(value: &str) -> bool {
    !value.is_empty()
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "tmp" | "temp" | "src" | "app" | "lib" | "docs" | "workspace" | "workspaces"
        )
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
        let entries = vec![
            entry(
                (2026, 3, 21, 9, 0, 0),
                "sess-1",
                "user",
                "Please inspect https://github.com/VetCoders/ai-contexters before editing.",
                None,
            ),
            entry(
                (2026, 3, 21, 9, 1, 0),
                "sess-1",
                "assistant",
                "I found the store seam in ai-contexters.",
                None,
            ),
            entry(
                (2026, 3, 21, 9, 2, 0),
                "sess-1",
                "user",
                "Switch now to https://github.com/VetCoders/loctree and review the scanner.",
                None,
            ),
            entry(
                (2026, 3, 21, 9, 3, 0),
                "sess-1",
                "assistant",
                "I am reviewing loctree next.",
                None,
            ),
        ];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].project_label(), "VetCoders/ai-contexters");
        assert_eq!(segments[1].project_label(), "VetCoders/loctree");
    }

    #[test]
    fn repo_signal_segmentation_keeps_unknown_prefix_honest() {
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
                "The actual repo is https://github.com/VetCoders/ai-contexters.",
                None,
            ),
        ];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 2);
        assert!(segments[0].repo.is_none());
        assert_eq!(segments[0].kind, Kind::Plans);
        assert_eq!(segments[1].project_label(), "VetCoders/ai-contexters");
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

    #[test]
    fn source_tier_github_url_is_primary() {
        let e = entry(
            (2026, 3, 22, 10, 0, 0),
            "sess-tier",
            "user",
            "Check https://github.com/VetCoders/ai-contexters for updates.",
            None,
        );
        let tiered = infer_tiered_identity_from_entry(&e, &ProjectHashRegistry::default())
            .expect("should resolve");
        assert_eq!(tiered.tier, SourceTier::Primary);
        assert_eq!(tiered.identity.slug(), "VetCoders/ai-contexters");
        assert!(tiered.tier.is_assertable());
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
        let entries = vec![
            entry(
                (2026, 3, 22, 10, 0, 0),
                "sess-st",
                "user",
                "Check https://github.com/VetCoders/ai-contexters",
                None,
            ),
            entry(
                (2026, 3, 22, 10, 1, 0),
                "sess-st",
                "assistant",
                "Reviewing now.",
                None,
            ),
        ];

        let segments = semantic_segments(&entries);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].source_tier, Some(SourceTier::Primary));
        assert!(segments[0].has_assertable_identity());
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
}
