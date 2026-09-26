use anyhow::{Context, Result};
use globset::{Glob, GlobMatcher};
#[cfg(any(feature = "app", test))]
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    /// Canonical spelling of every prefix that resolves on this host.
    ///
    /// Scope normalization stamps frames with the CANONICAL repo root, so a
    /// deny list written the way the operator types it (`/var/...`, or any
    /// checkout reached through a symlink) would otherwise never match the
    /// frame it is meant to hide.
    canonical_prefixes: Vec<String>,
    /// `fs::canonicalize` results for incoming cwds, for the lifetime of this
    /// matcher.
    ///
    /// The reverse direction of `canonical_prefixes` cannot be precomputed —
    /// a symlink's pre-images are not enumerable — so a frame spelled through
    /// a symlink has to be resolved here. Frames repeat cwds heavily, so this
    /// makes the cost one resolution per distinct cwd instead of one per
    /// frame. A matcher is loaded per operation, which is also the window in
    /// which a retargeted symlink could make a memoized answer stale.
    canonical_cwds: Arc<Mutex<HashMap<String, Option<String>>>>,
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
        hasher.update(b"aicx.repo_path_ignore.v3\0");
        for prefix in &self.prefixes {
            hasher.update(prefix.as_bytes());
            hasher.update([0]);
        }
        // Matching depends on the RESOLVED targets too, so identity must.
        // Retargeting a symlink changes which checkout a rule denies while
        // the rule's literal spelling is untouched; hashing only the spelling
        // let a cache built under the old target be reused under the new one,
        // republishing content the rule now hides.
        hasher.update(b"canonical\0");
        for prefix in &self.canonical_prefixes {
            hasher.update(prefix.as_bytes());
            hasher.update([0]);
        }
        hex::encode(hasher.finalize())
    }

    /// True when `cwd` is the listed path or lives under it.
    ///
    /// Compared in every spelling both sides can produce. Privacy is the one
    /// place that must fail CLOSED: an incoming cwd already rewritten to its
    /// canonical repo root still has to hit a prefix the operator wrote in the
    /// pre-symlink spelling, so a miss in one form is not an answer.
    pub fn ignores_cwd(&self, cwd: Option<&str>) -> bool {
        let Some(cwd) = cwd.map(str::trim).filter(|value| !value.is_empty()) else {
            return false;
        };
        if self.prefixes.is_empty() {
            return false;
        }
        let expanded = expand_tilde(cwd, &self.user_home);
        let literal = normalize_cwd_display(&expanded);
        if self.matches_any_prefix(&literal) {
            return true;
        }
        // Only a literal miss is worth a filesystem question, and only once
        // per distinct cwd: see `canonical_cwds`.
        match self.canonical_cwd(&expanded) {
            Some(canonical) if canonical != literal => self.matches_any_prefix(&canonical),
            _ => false,
        }
    }

    fn canonical_cwd(&self, expanded: &str) -> Option<String> {
        if let Ok(memo) = self.canonical_cwds.lock()
            && let Some(hit) = memo.get(expanded)
        {
            return hit.clone();
        }
        let resolved = canonical_cwd_display(expanded);
        if let Ok(mut memo) = self.canonical_cwds.lock() {
            memo.insert(expanded.to_owned(), resolved.clone());
        }
        resolved
    }

    fn matches_any_prefix(&self, value: &str) -> bool {
        self.prefixes
            .iter()
            .chain(self.canonical_prefixes.iter())
            .any(|prefix| value == prefix || value.starts_with(&format!("{prefix}/")))
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
            canonical_prefixes: Vec::new(),
            canonical_cwds: Arc::default(),
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
    // Resolved once at load time: the deny list is small, and a rule written
    // in the operator's spelling has to match a frame already stamped with
    // the canonical repo root.
    let mut canonical_prefixes: Vec<String> = prefixes
        .iter()
        .filter_map(|prefix| canonical_cwd_display(prefix))
        .filter(|canonical| !prefixes.contains(canonical))
        .collect();
    canonical_prefixes.sort();
    canonical_prefixes.dedup();

    Ok(RepoPathIgnoreMatcher {
        user_home: user_home.to_path_buf(),
        prefixes,
        canonical_prefixes,
        canonical_cwds: Arc::default(),
    })
}

/// Canonical spelling of an existing path, in the same display normalization
/// the prefixes use. `None` when the path is gone from this host, which is not
/// an error: the literal comparison still stands on its own.
fn canonical_cwd_display(path: &str) -> Option<String> {
    std::fs::canonicalize(path.trim())
        .ok()
        .map(|resolved| normalize_cwd_display(&resolved.to_string_lossy()))
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

    #[cfg(unix)]
    fn scratch(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aicx-ignore-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    /// Finding: the fingerprint drives cache and index invalidation but
    /// hashed only the literal rule spelling, while matching also depends on
    /// where those rules RESOLVE. Retargeting a symlink changes which
    /// checkout is denied without touching `.aicxignore`, so an unchanged
    /// fingerprint let `aicx index` reuse content filtered under the old
    /// target and republish what the rule now hides.
    #[cfg(unix)]
    #[test]
    fn retargeting_a_denied_symlink_changes_the_deny_list_identity() {
        let root = scratch("fingerprint-retarget");
        let aicx_home = root.join(".aicx");
        let user_home = root.join("user");
        fs::create_dir_all(root.join("alpha")).unwrap();
        fs::create_dir_all(root.join("beta")).unwrap();
        let link = root.join("prywatne");
        std::os::unix::fs::symlink(root.join("alpha"), &link).unwrap();

        // The rule text never changes across this test.
        write_ignore(&aicx_home, &format!("{}\n", link.display()));

        let before = load_repo_path_ignore(&aicx_home, &user_home)
            .unwrap()
            .fingerprint();
        assert!(
            load_repo_path_ignore(&aicx_home, &user_home)
                .unwrap()
                .ignores_cwd(Some(
                    &fs::canonicalize(root.join("alpha"))
                        .unwrap()
                        .to_string_lossy()
                )),
            "the rule denies alpha while the link points at it"
        );

        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(root.join("beta"), &link).unwrap();

        let after = load_repo_path_ignore(&aicx_home, &user_home).unwrap();
        assert!(
            after.ignores_cwd(Some(
                &fs::canonicalize(root.join("beta"))
                    .unwrap()
                    .to_string_lossy()
            )),
            "and denies beta once the link points there"
        );
        assert_ne!(
            before,
            after.fingerprint(),
            "a deny list that now hides a different checkout is a different deny list"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Finding: `ignores_cwd` canonicalized every incoming cwd, so a large
    /// history paid one filesystem resolution per signal frame — while the
    /// loader claimed the per-frame check was free of filesystem work.
    ///
    /// Resolution is now lazy (a literal hit never asks) and memoized per
    /// distinct cwd. Observed here by removing the symlink between two calls:
    /// only a memoized answer can survive its target disappearing.
    #[cfg(unix)]
    #[test]
    fn an_incoming_cwd_is_resolved_at_most_once() {
        let root = scratch("canonicalize-once");
        let aicx_home = root.join(".aicx");
        let user_home = root.join("user");
        let real = root.join("real").join("prywatne");
        fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();

        // Rule in canonical spelling, frame spelled through the symlink: the
        // one direction that cannot be precomputed at load time.
        let canonical_rule = fs::canonicalize(&real).unwrap();
        write_ignore(&aicx_home, &format!("{}\n", canonical_rule.display()));
        let ignore = load_repo_path_ignore(&aicx_home, &user_home).unwrap();

        let through_link = root.join("link").join("prywatne");
        let through_link = through_link.to_string_lossy().into_owned();
        assert!(
            ignore.ignores_cwd(Some(&through_link)),
            "a symlinked spelling of a denied checkout must be denied"
        );

        fs::remove_file(root.join("link")).unwrap();
        assert!(
            ignore.ignores_cwd(Some(&through_link)),
            "the second answer must come from the memo, not the filesystem"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Scope normalization stamps frames with the CANONICAL repo root. A deny
    /// list naming the same checkout through a symlink must still hide it —
    /// otherwise the private repository reaches extracts, index and intents.
    #[cfg(unix)]
    #[test]
    fn a_denied_checkout_stays_denied_under_its_canonical_spelling() {
        let root = std::env::temp_dir().join(format!(
            "aicx-ignore-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let aicx_home = root.join(".aicx");
        let user_home = root.join("user");
        let real_checkout = root.join("real").join("prywatne");
        fs::create_dir_all(&real_checkout).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();

        // The operator writes the path they see; the frame carries the path
        // the repo-identity pass resolved it to.
        write_ignore(
            &aicx_home,
            &format!("{}\n", root.join("link").join("prywatne").display()),
        );
        let ignore = load_repo_path_ignore(&aicx_home, &user_home).unwrap();
        let stamped = fs::canonicalize(&real_checkout).unwrap();

        assert!(
            ignore.ignores_cwd(Some(&stamped.to_string_lossy())),
            "canonical spelling of a denied checkout must still be denied"
        );
        assert!(
            ignore.ignores_cwd(Some(&stamped.join("src").to_string_lossy())),
            "and so must everything under it"
        );
        assert!(
            !ignore.ignores_cwd(Some(
                &fs::canonicalize(&root)
                    .unwrap()
                    .join("real")
                    .to_string_lossy()
            )),
            "resolving spellings must not widen the rule to the parent"
        );

        let _ = fs::remove_dir_all(&root);
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
