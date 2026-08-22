use anyhow::{Context, Result};
use globset::{Glob, GlobMatcher};
#[cfg(any(feature = "app", test))]
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::sanitize;

pub const AICX_IGNORE_FILENAME: &str = ".aicxignore";

#[derive(Debug, Clone)]
struct IgnoreRule {
    negate: bool,
    matcher: GlobMatcher,
}

#[derive(Debug, Clone, Default)]
pub struct StoreIgnoreMatcher {
    base: PathBuf,
    rules: Vec<IgnoreRule>,
}

impl StoreIgnoreMatcher {
    pub(crate) fn empty_at(base: &Path) -> Self {
        Self {
            base: base.to_path_buf(),
            rules: Vec::new(),
        }
    }

    fn load(base: &Path) -> Result<Self> {
        let path = base.join(AICX_IGNORE_FILENAME);
        if !path.exists() {
            return Ok(Self::empty_at(base));
        }

        let raw = sanitize::read_to_string_validated(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let mut rules = Vec::new();

        for (line_no, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let negate = trimmed.starts_with('!');
            let pattern = trimmed.trim_start_matches('!').trim();
            if pattern.is_empty() {
                continue;
            }

            let normalized = normalize_aicx_ignore_pattern(pattern);
            let matcher = Glob::new(&normalized)
                .with_context(|| {
                    format!(
                        "Invalid {} pattern at line {}: {}",
                        path.display(),
                        line_no + 1,
                        trimmed
                    )
                })?
                .compile_matcher();

            rules.push(IgnoreRule { negate, matcher });
        }

        Ok(Self {
            base: base.to_path_buf(),
            rules,
        })
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        if self.rules.is_empty() {
            return false;
        }

        let Ok(relative) = path.strip_prefix(&self.base) else {
            return false;
        };
        let relative = normalize_relative_store_path(relative);
        if relative.is_empty() {
            return false;
        }

        let mut ignored = false;
        for rule in &self.rules {
            if rule.matcher.is_match(&relative) {
                ignored = !rule.negate;
            }
        }
        ignored
    }
}

fn normalize_relative_store_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_aicx_ignore_pattern(pattern: &str) -> String {
    let mut normalized = pattern
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .replace('\\', "/");

    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }

    if normalized.ends_with('/') {
        normalized.push_str("**");
    }

    normalized
}

pub fn load_ignore_matcher_at(base: &Path) -> Result<StoreIgnoreMatcher> {
    StoreIgnoreMatcher::load(base)
}

pub fn filter_ignored_paths_at<P>(base: &Path, paths: &[P]) -> Result<(Vec<PathBuf>, usize)>
where
    P: AsRef<Path>,
{
    let matcher = load_ignore_matcher_at(base)?;
    if matcher.rules.is_empty() {
        return Ok((
            paths
                .iter()
                .map(|path| path.as_ref().to_path_buf())
                .collect(),
            0,
        ));
    }

    let mut kept = Vec::with_capacity(paths.len());
    let mut ignored = 0usize;

    for path in paths {
        let path = path.as_ref();
        if matcher.is_ignored(path) {
            ignored += 1;
        } else {
            kept.push(path.to_path_buf());
        }
    }

    Ok((kept, ignored))
}

/// Central `~/.aicx/.aicxignore` rules that name **filesystem checkout
/// paths**. A listed directory covers every nested repo under it.
///
/// Only absolute paths, `~`, and `~/…` participate. Relative lines stay
/// on the legacy store-card matcher and never hide a live session cwd.
#[derive(Debug, Clone, Default)]
pub struct RepoPathIgnoreMatcher {
    user_home: PathBuf,
    prefixes: Vec<String>,
}

impl RepoPathIgnoreMatcher {
    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty()
    }

    /// Stable, non-reversible identity of the active checkout deny list.
    ///
    /// Index and extract caches bind to this value so adding, removing, or
    /// changing a private path cannot reuse content filtered under old rules.
    #[cfg(any(feature = "app", test))]
    pub(crate) fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"aicx.repo_path_ignore.v1\0");
        for prefix in &self.prefixes {
            hasher.update(prefix.as_bytes());
            hasher.update([0]);
        }
        hex::encode(hasher.finalize())
    }

    /// True when `cwd` is the listed path or lives under it.
    pub fn ignores_cwd(&self, cwd: Option<&str>) -> bool {
        let Some(cwd) = cwd.map(str::trim).filter(|value| !value.is_empty()) else {
            return false;
        };
        if self.prefixes.is_empty() {
            return false;
        }
        let normalized = normalize_cwd_display(&expand_tilde(cwd, &self.user_home));
        self.prefixes
            .iter()
            .any(|prefix| normalized == *prefix || normalized.starts_with(&format!("{prefix}/")))
    }
}

/// Load `$AICX_HOME/.aicxignore` and interpret full-path / `~/…` lines as
/// checkout denials. `user_home` expands `~`.
pub fn load_repo_path_ignore(aicx_home: &Path, user_home: &Path) -> Result<RepoPathIgnoreMatcher> {
    let path = aicx_home.join(AICX_IGNORE_FILENAME);
    if !path.exists() {
        return Ok(RepoPathIgnoreMatcher {
            user_home: user_home.to_path_buf(),
            prefixes: Vec::new(),
        });
    }

    let raw = sanitize::read_to_string_validated(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut prefixes = Vec::new();

    for (line_no, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let pattern = trimmed.trim_start_matches('!').trim();
        if pattern.is_empty() || !is_repo_path_pattern(pattern) {
            continue;
        }
        if trimmed.starts_with('!') || contains_glob_meta(pattern) {
            anyhow::bail!(
                "Invalid {} checkout rule at line {}: path prefixes do not support negation or globs: {}",
                path.display(),
                line_no + 1,
                trimmed
            );
        }
        let expanded = expand_tilde(pattern, user_home);
        prefixes.push(normalize_cwd_display(&expanded));
    }
    prefixes.sort();
    prefixes.dedup();

    Ok(RepoPathIgnoreMatcher {
        user_home: user_home.to_path_buf(),
        prefixes,
    })
}

fn is_repo_path_pattern(pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.starts_with('/') || pattern == "~" || pattern.starts_with("~/") {
        return true;
    }
    // Windows drive: `D:\work\private`
    let bytes = pattern.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn contains_glob_meta(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

fn expand_tilde(pattern: &str, user_home: &Path) -> String {
    let pattern = pattern.trim();
    if pattern == "~" {
        return user_home.to_string_lossy().replace('\\', "/");
    }
    if let Some(rest) = pattern.strip_prefix("~/") {
        return user_home.join(rest).to_string_lossy().replace('\\', "/");
    }
    pattern.replace('\\', "/")
}

fn normalize_cwd_display(cwd: &str) -> String {
    let mut normalized = cwd.trim().replace('\\', "/");
    while normalized.ends_with('/') && normalized.len() > 1 {
        normalized.pop();
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_ignore(home: &Path, body: &str) {
        fs::create_dir_all(home).unwrap();
        fs::write(home.join(AICX_IGNORE_FILENAME), body).unwrap();
    }

    #[test]
    fn tilde_directory_covers_every_nested_checkout() {
        let root = std::env::temp_dir().join(format!(
            "aicx-ignore-tilde-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let aicx_home = root.join(".aicx");
        let user_home = root.join("user");
        write_ignore(&aicx_home, "~/Repozytoria/moje_prywatne\n");
        let ignore = load_repo_path_ignore(&aicx_home, &user_home).unwrap();
        let private = user_home.join("Repozytoria").join("moje_prywatne");
        assert!(ignore.ignores_cwd(Some(&private.join("gole_baby").to_string_lossy())));
        assert!(ignore.ignores_cwd(Some(&private.join("historie_po_wodce").to_string_lossy())));
        assert!(
            ignore.ignores_cwd(Some(
                &private
                    .join("gole_chlopy__to-kolegi-nie-moje")
                    .to_string_lossy()
            ))
        );
        assert!(ignore.ignores_cwd(Some(&private.to_string_lossy())));
        assert!(
            !ignore.ignores_cwd(Some(
                &user_home
                    .join("Repozytoria")
                    .join("praca")
                    .to_string_lossy()
            ))
        );
        assert!(!ignore.ignores_cwd(None));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn absolute_path_is_boundary_aware_and_duplicate_rules_are_stable() {
        let root = std::env::temp_dir().join(format!(
            "aicx-ignore-abs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let aicx_home = root.join(".aicx");
        let user_home = root.join("user");
        let private = user_home.join("private");
        write_ignore(
            &aicx_home,
            &format!("{}\n{}/\n", private.display(), private.display()),
        );
        let ignore = load_repo_path_ignore(&aicx_home, &user_home).unwrap();
        assert!(ignore.ignores_cwd(Some(&private.join("secret").to_string_lossy())));
        assert!(!ignore.ignores_cwd(Some(&user_home.join("private-sibling").to_string_lossy())));
        let first = ignore.fingerprint();
        write_ignore(&aicx_home, &format!("{}/\n", private.display()));
        let deduplicated = load_repo_path_ignore(&aicx_home, &user_home).unwrap();
        assert_eq!(first, deduplicated.fingerprint());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn checkout_negation_and_globs_fail_closed() {
        let root = std::env::temp_dir().join(format!(
            "aicx-ignore-invalid-checkout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let aicx_home = root.join(".aicx");
        let user_home = root.join("user");

        write_ignore(&aicx_home, "!~/private/keep\n");
        assert!(load_repo_path_ignore(&aicx_home, &user_home).is_err());
        write_ignore(&aicx_home, "~/private/*\n");
        assert!(load_repo_path_ignore(&aicx_home, &user_home).is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unreadable_checkout_deny_list_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "aicx-ignore-unreadable-checkout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let aicx_home = root.join(".aicx");
        let user_home = root.join("user");
        fs::create_dir_all(aicx_home.join(AICX_IGNORE_FILENAME)).unwrap();

        assert!(load_repo_path_ignore(&aicx_home, &user_home).is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn relative_store_lines_do_not_hide_checkouts() {
        let root = std::env::temp_dir().join(format!(
            "aicx-ignore-rel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let aicx_home = root.join(".aicx");
        let user_home = root.join("user");
        write_ignore(&aicx_home, "store/vetcoders/secret/**\n");
        let ignore = load_repo_path_ignore(&aicx_home, &user_home).unwrap();
        assert!(!ignore.ignores_cwd(Some("/Volumes/secret")));
        let _ = fs::remove_dir_all(&root);
    }
}
