//! Shared schema validation for repository-derived filesystem buckets.
//!
//! The canonical store bucket schema intentionally stays narrow: ASCII
//! lowercase ASCII alphanumeric first character, then lowercase ASCII
//! alphanumeric, dot, underscore, or dash. Semantic callers can add their own
//! reserved-word rules on top.

pub fn is_valid_repo_bucket_name(value: &str) -> bool {
    if value.len() > 100 {
        return false;
    }

    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '_' | '-')
        })
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
        assert!(is_valid_repo_bucket_name("local"));
        assert!(is_valid_repo_bucket_name("rust-memex"));
        assert!(is_valid_repo_bucket_name("foo.bar_baz"));
        assert!(is_valid_repo_bucket_name("mlx-batch-server.git"));
    }

    #[test]
    fn valid_repo_bucket_names_reject_templates_and_placeholders() {
        assert!(!is_valid_repo_bucket_name(""));
        assert!(!is_valid_repo_bucket_name(&"a".repeat(101)));
        assert!(!is_valid_repo_bucket_name("..."));
        assert!(!is_valid_repo_bucket_name("VetCoders"));
        assert!(!is_valid_repo_bucket_name("LibraxisAI"));
        assert!(!is_valid_repo_bucket_name("{target_owner}"));
        assert!(!is_valid_repo_bucket_name("$RELEASE_REPO"));
        assert!(!is_valid_repo_bucket_name("<owner>"));
        assert!(!is_valid_repo_bucket_name("name/with/slash"));
        assert!(!is_valid_repo_bucket_name("line\nbreak"));
        assert!(!is_valid_repo_bucket_name("vibecrafted.git`"));
        assert!(!is_valid_repo_bucket_name("loctree\n\n**AICX"));
        assert!(!is_valid_repo_bucket_name("vc-skills.git\"><span"));
        assert!(!is_valid_repo_bucket_name("ai-contexters;"));
        assert!(!is_valid_repo_bucket_name("rmcp-memex\"\\necho"));
    }

    #[test]
    fn valid_repo_project_slugs_validate_each_path_segment() {
        assert!(is_valid_repo_project_slug("vetcoders/aicx"));
        assert!(is_valid_repo_project_slug("local"));
        assert!(is_valid_repo_project_slug("vetcoders/mlx-batch-server.git"));

        assert!(!is_valid_repo_project_slug("VetCoders/aicx"));
        assert!(!is_valid_repo_project_slug("VetCoders/vibecrafted.git`"));
        assert!(!is_valid_repo_project_slug("VetCoders/loctree\n\n**AICX"));
        assert!(!is_valid_repo_project_slug("VetCoders/aicx/extra"));
        assert!(!is_valid_repo_project_slug("/aicx"));
        assert!(!is_valid_repo_project_slug("VetCoders/"));
    }
}
