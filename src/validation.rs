//! Shared schema validation for repository-derived filesystem buckets.
//!
//! The canonical store bucket schema intentionally stays narrow: ASCII
//! alphanumeric first character, then ASCII alphanumeric, dot, underscore,
//! or dash. Semantic callers can add their own reserved-word rules on top.

pub fn is_valid_repo_bucket_name(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };

    first.is_ascii_alphanumeric()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_repo_bucket_names_allow_common_slugs() {
        assert!(is_valid_repo_bucket_name("VetCoders"));
        assert!(is_valid_repo_bucket_name("LibraxisAI"));
        assert!(is_valid_repo_bucket_name("local"));
        assert!(is_valid_repo_bucket_name("rust-memex"));
        assert!(is_valid_repo_bucket_name("foo.bar_baz"));
    }

    #[test]
    fn valid_repo_bucket_names_reject_templates_and_placeholders() {
        assert!(!is_valid_repo_bucket_name(""));
        assert!(!is_valid_repo_bucket_name("..."));
        assert!(!is_valid_repo_bucket_name("{target_owner}"));
        assert!(!is_valid_repo_bucket_name("$RELEASE_REPO"));
        assert!(!is_valid_repo_bucket_name("<owner>"));
        assert!(!is_valid_repo_bucket_name("name/with/slash"));
        assert!(!is_valid_repo_bucket_name("line\nbreak"));
    }
}
