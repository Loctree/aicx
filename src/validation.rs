//! Shared schema validation for repository-derived filesystem buckets.
//!
//! The canonical store bucket schema is **case-preserving**: ASCII
//! alphanumeric first character (either case), then ASCII alphanumeric,
//! dot, underscore, or dash. GitHub orgs are CamelCase by convention
//! (`LibraxisAI`, `Vetcoders`, `Loctree`, `Szowesgad`), and forcing
//! lowercase here loses preserved-case provenance information without a
//! corresponding correctness gain on case-insensitive filesystems
//! (macOS APFS, Windows NTFS) which already collapse case at the inode
//! level. Cross-platform sync between case-insensitive hosts (operator's
//! macOS workstations) preserves whichever case lands first; on
//! case-sensitive Linux, mixed-case duplicates would be a real concern,
//! but the operator's aicx mesh is currently macOS-only.
//!
//! Semantic callers can add their own reserved-word rules on top
//! (template-placeholder rejection, etc.).

pub fn is_valid_repo_bucket_name(value: &str) -> bool {
    if value.len() > 100 {
        return false;
    }

    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    (first.is_ascii_alphabetic() || first.is_ascii_digit() || matches!(first, '.' | '_'))
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

/// Validate a canonical store project slug before it becomes a filesystem path.
///
/// Accepts either one legacy bucket segment (`local`) or the canonical
/// `organization/repository` shape. Each segment must pass the same bucket
/// schema so bad content-extracted names cannot create junk directories under
/// `~/.aicx/store`.
pub fn is_valid_repo_project_slug(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(second) = parts.next() else {
        return is_valid_repo_bucket_name(first);
    };

    parts.next().is_none() && is_valid_repo_bucket_name(first) && is_valid_repo_bucket_name(second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_repo_bucket_names_allow_common_slugs() {
        // Lowercase legacy and various separators:
        assert!(is_valid_repo_bucket_name("local"));
        assert!(is_valid_repo_bucket_name("rust-memex"));
        assert!(is_valid_repo_bucket_name("foo.bar_baz"));
        assert!(is_valid_repo_bucket_name("mlx-batch-server.git"));

        // CamelCase GitHub org names — case-preserving canonical form
        // (relaxed 2026-05-12 from lowercase-only):
        assert!(is_valid_repo_bucket_name("Vetcoders"));
        assert!(is_valid_repo_bucket_name("LibraxisAI"));
        assert!(is_valid_repo_bucket_name("Loctree"));
        assert!(is_valid_repo_bucket_name("Szowesgad"));
        assert!(is_valid_repo_bucket_name("BurntSushi"));
        assert!(is_valid_repo_bucket_name("Mintplex-Labs"));
        assert!(is_valid_repo_bucket_name("PyCQA"));

        // Dot-prefix: standard UNIX hidden-dir convention plus GitHub special
        // repos (`.github` org-config repo, operator's `.aicx`, `.codescribe`,
        // `.scripts` local backups). Relaxed 2026-05-12 from prior leading
        // `[a-z0-9]`-only restriction.
        assert!(is_valid_repo_bucket_name(".github"));
        assert!(is_valid_repo_bucket_name(".aicx"));
        assert!(is_valid_repo_bucket_name(".codescribe"));
        assert!(is_valid_repo_bucket_name(".scripts"));
        assert!(is_valid_repo_bucket_name(".ai-memories"));

        // Underscore-prefix: code-convention "internal/private" naming:
        assert!(is_valid_repo_bucket_name("_internal"));
        assert!(is_valid_repo_bucket_name("_priv"));
    }

    #[test]
    fn valid_repo_bucket_names_reject_templates_and_placeholders() {
        assert!(!is_valid_repo_bucket_name(""));
        assert!(!is_valid_repo_bucket_name(&"a".repeat(101)));
        // Template placeholders (leading `{`, `$`, `<` — not in the
        // allowed leading-char set `[A-Za-z0-9._]`):
        assert!(!is_valid_repo_bucket_name("{target_owner}"));
        assert!(!is_valid_repo_bucket_name("$RELEASE_REPO"));
        assert!(!is_valid_repo_bucket_name("<owner>"));
        // (Note: pure dot-string `"..."` is now technically valid since
        // `.` is allowed both leading and mid. Semantically weird but
        // out of scope for this format-only validator.)
        // Path separator:
        assert!(!is_valid_repo_bucket_name("name/with/slash"));
        // Mid-segment garbage from text extraction (newlines, backticks,
        // brackets, quotes, semicolons, escape sequences):
        assert!(!is_valid_repo_bucket_name("line\nbreak"));
        assert!(!is_valid_repo_bucket_name("vibecrafted.git`"));
        assert!(!is_valid_repo_bucket_name("loctree\n\n**AICX"));
        assert!(!is_valid_repo_bucket_name("vc-skills.git\"><span"));
        assert!(!is_valid_repo_bucket_name("ai-contexters;"));
        assert!(!is_valid_repo_bucket_name("rmcp-memex\"\\necho"));
        assert!(!is_valid_repo_bucket_name(
            "loctxc_O)outcomqqqqqqq]]qqqqqqqqqqqqqqqqqqqqqqqqqqq;;'["
        ));
    }

    #[test]
    fn valid_repo_project_slugs_validate_each_path_segment() {
        // Lowercase paths still valid:
        assert!(is_valid_repo_project_slug("vetcoders/aicx"));
        assert!(is_valid_repo_project_slug("local"));
        assert!(is_valid_repo_project_slug("vetcoders/mlx-batch-server.git"));

        // CamelCase paths valid (case-preserving canonical):
        assert!(is_valid_repo_project_slug("Vetcoders/aicx"));
        assert!(is_valid_repo_project_slug("Vetcoders/Vista"));
        assert!(is_valid_repo_project_slug("LibraxisAI/lbrxAgents"));
        assert!(is_valid_repo_project_slug("Loctree/aicx"));

        // Mid-segment garbage still rejected (extractor-bug evidence):
        assert!(!is_valid_repo_project_slug("Vetcoders/vibecrafted.git`"));
        assert!(!is_valid_repo_project_slug("Vetcoders/loctree\n\n**AICX"));
        assert!(!is_valid_repo_project_slug(
            "Vetcoders/loctxc_O)outcomqqqqqqq]]qqqqqqqqqqqqqqqqqqqqqqqqqqq;;'["
        ));

        // Structural rejects:
        assert!(!is_valid_repo_project_slug("Vetcoders/aicx/extra"));
        assert!(!is_valid_repo_project_slug("/aicx"));
        assert!(!is_valid_repo_project_slug("Vetcoders/"));
    }
}
