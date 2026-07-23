//! Canonical source-path resolver for operator-owned session artifacts.
//!
//! Catalog reads, source indexing, and migration identity manifests must open
//! only paths that resolve under approved roots after canonicalize. This is the
//! real cut for path-traversal taint — not `// nosemgrep` silencers.
//!
//! Approved roots (when present on the machine):
//! - `~/.claude/projects`
//! - `~/.codex/sessions`
//! - `~/.grok/sessions`
//! - `~/.gemini/tmp`
//! - `~/.junie/sessions`
//! - `~/.vibecrafted/control_plane/runtime_runs`
//! - the active AICX home (`$AICX_HOME` / `~/.aicx`)
//!
//! Unit tests register their tempfile roots explicitly via [`SourceAllowlist::from_roots`]
//! or [`SourceAllowlist::for_operator`] with a scratch `user_home` / `aicx_home`.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow};

/// Default relative roots under the operator home that may host session sources.
pub const DEFAULT_SOURCE_ROOT_RELATIVE: &[&str] = &[
    ".claude/projects",
    ".codex/sessions",
    ".grok/sessions",
    ".gemini/tmp",
    ".junie/sessions",
    ".vibecrafted/control_plane/runtime_runs",
];

/// Allowlist of approved roots for readable session/catalog artifacts.
#[derive(Debug, Clone)]
pub struct SourceAllowlist {
    /// Existing roots, preferred as already-canonical paths when possible.
    roots: Vec<PathBuf>,
}

impl SourceAllowlist {
    /// Build the production allowlist for one operator user + one AICX home.
    pub fn for_operator(user_home: &Path, aicx_home: &Path) -> Self {
        let mut roots = Vec::with_capacity(DEFAULT_SOURCE_ROOT_RELATIVE.len() + 1);
        for rel in DEFAULT_SOURCE_ROOT_RELATIVE {
            roots.push(user_home.join(rel));
        }
        roots.push(aicx_home.to_path_buf());
        Self::from_roots(roots)
    }

    /// Explicit roots (tests, narrow catalog-only reads under AICX home).
    pub fn from_roots(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut seen = Vec::new();
        for root in roots {
            if !seen.iter().any(|existing: &PathBuf| existing == &root) {
                seen.push(root);
            }
        }
        Self { roots: seen }
    }

    /// Roots currently configured (may not exist yet).
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Canonicalize `candidate`, require a regular file, and prove containment
    /// under one approved root. Rejects `..` components, missing paths, non-files,
    /// and symlink escapes that land outside the allowlist.
    pub fn resolve_file<P: AsRef<Path>>(&self, candidate: P) -> Result<PathBuf> {
        let candidate = candidate.as_ref();
        reject_traversal(candidate)?;
        if !candidate.exists() {
            return Err(anyhow!(
                "source path does not exist: {}",
                candidate.display()
            ));
        }
        let canonical = candidate
            .canonicalize()
            .with_context(|| format!("canonicalize source path {}", candidate.display()))?;
        if !canonical.is_file() {
            return Err(anyhow!(
                "source path is not a regular file: {}",
                canonical.display()
            ));
        }
        if !self.contains(&canonical)? {
            return Err(anyhow!(
                "source path escapes approved roots: {}",
                canonical.display()
            ));
        }
        Ok(canonical)
    }

    /// Open a readable file only after [`Self::resolve_file`].
    pub fn open_file<P: AsRef<Path>>(&self, candidate: P) -> Result<File> {
        let validated = self.resolve_file(candidate)?;
        open_validated_canonical(validated)
    }

    /// Read an entire file as UTF-8 after validation.
    pub fn read_to_string<P: AsRef<Path>>(&self, candidate: P) -> Result<String> {
        let validated = self.resolve_file(candidate)?;
        let mut file = open_validated_canonical(validated.clone())?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)
            .with_context(|| format!("read validated source {}", validated.display()))?;
        Ok(buf)
    }

    /// Read raw bytes after validation (catalog fingerprint, binary-safe).
    pub fn read_bytes<P: AsRef<Path>>(&self, candidate: P) -> Result<Vec<u8>> {
        let validated = self.resolve_file(candidate)?;
        let mut file = open_validated_canonical(validated.clone())?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .with_context(|| format!("read validated source {}", validated.display()))?;
        Ok(buf)
    }

    fn contains(&self, canonical_path: &Path) -> Result<bool> {
        // Strict containment only. Temp paths are accepted solely when an
        // explicit root under tempfile was registered (unit tests pass their
        // scratch aicx_home / agent roots). A bare `/tmp` free-for-all would
        // re-open symlink and outside-root escapes.
        for root in &self.roots {
            if is_path_within_root(canonical_path, root)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Convenience: resolve + open using the live operator home + AICX home.
pub fn open_operator_source(candidate: &Path) -> Result<File> {
    let user_home = crate::os_user_home().context("resolve operator home")?;
    let aicx_home = crate::store::resolve_aicx_home()?;
    SourceAllowlist::for_operator(&user_home, &aicx_home).open_file(candidate)
}

/// Convenience: resolve a catalog entry source path under the live allowlist.
pub fn resolve_operator_source(candidate: &Path) -> Result<PathBuf> {
    let user_home = crate::os_user_home().context("resolve operator home")?;
    let aicx_home = crate::store::resolve_aicx_home()?;
    SourceAllowlist::for_operator(&user_home, &aicx_home).resolve_file(candidate)
}

/// Resolve a path that must live under a single AICX home (catalog, extracts,
/// migration manifests). Narrower than the full operator allowlist.
pub fn resolve_under_aicx_home(aicx_home: &Path, candidate: &Path) -> Result<PathBuf> {
    SourceAllowlist::from_roots([aicx_home.to_path_buf()]).resolve_file(candidate)
}

pub fn open_under_aicx_home(aicx_home: &Path, candidate: &Path) -> Result<File> {
    SourceAllowlist::from_roots([aicx_home.to_path_buf()]).open_file(candidate)
}

pub fn read_under_aicx_home(aicx_home: &Path, candidate: &Path) -> Result<String> {
    SourceAllowlist::from_roots([aicx_home.to_path_buf()]).read_to_string(candidate)
}

pub fn read_bytes_under_aicx_home(aicx_home: &Path, candidate: &Path) -> Result<Vec<u8>> {
    SourceAllowlist::from_roots([aicx_home.to_path_buf()]).read_bytes(candidate)
}

/// Open a path that has already passed [`SourceAllowlist::resolve_file`].
///
/// Re-canonicalize immediately before open so static path-traversal analysis
/// treats the open target as a sanitizer output (canonical absolute path under
/// an already-proven allowlist root), not the original candidate string.
fn open_validated_canonical(validated: PathBuf) -> Result<File> {
    let display = validated.display().to_string();
    let canonical = validated
        .canonicalize()
        .with_context(|| format!("re-canonicalize validated source {display}"))?;
    std::fs::OpenOptions::new()
        .read(true)
        .open(&canonical)
        .with_context(|| format!("open validated source {}", canonical.display()))
}

fn reject_traversal(path: &Path) -> Result<()> {
    let text = path.to_string_lossy();
    if text.split(['/', '\\']).any(|segment| segment == "..") {
        return Err(anyhow!(
            "path contains traversal segment: {}",
            path.display()
        ));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(anyhow!(
            "path contains parent-dir component: {}",
            path.display()
        ));
    }
    Ok(())
}

/// True when `canonical_path` is the root itself or a descendant after both
/// sides are canonicalized (symlink-safe containment).
fn is_path_within_root(canonical_path: &Path, root: &Path) -> Result<bool> {
    if !root.exists() {
        return Ok(false);
    }
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("canonicalize approved root {}", root.display()))?;
    Ok(canonical_path.starts_with(&canonical_root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(label: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "aicx-source-path-{label}-{}-{n}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn accepts_file_under_approved_root() {
        let root = test_dir("ok");
        let aicx = root.join(".aicx");
        let src_root = root.join(".claude").join("projects");
        fs::create_dir_all(&src_root).unwrap();
        let file = src_root.join("session.jsonl");
        fs::write(&file, b"hello").unwrap();
        let allow = SourceAllowlist::for_operator(&root, &aicx);
        let resolved = allow.resolve_file(&file).unwrap();
        assert_eq!(fs::read_to_string(&resolved).unwrap(), "hello");
        let body = allow.read_to_string(&file).unwrap();
        assert_eq!(body, "hello");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_path_outside_approved_roots() {
        let root = test_dir("outside");
        let aicx = root.join(".aicx");
        let outsider = root.join("not-a-source").join("secret.txt");
        fs::create_dir_all(outsider.parent().unwrap()).unwrap();
        fs::write(&outsider, b"nope").unwrap();
        let allow = SourceAllowlist::for_operator(&root, &aicx);
        let err = allow.resolve_file(&outsider).unwrap_err();
        assert!(
            err.to_string().contains("escapes approved roots"),
            "got {err}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_traversal_components_before_open() {
        let root = test_dir("trav");
        let aicx = root.join(".aicx");
        let src_root = root.join(".codex").join("sessions");
        fs::create_dir_all(&src_root).unwrap();
        let evil = root.join("evil.txt");
        fs::write(&evil, b"secret").unwrap();
        // Candidate still carries a `..` segment in the path string.
        let sneaky = src_root.join("..").join("evil.txt");
        let allow = SourceAllowlist::for_operator(&root, &aicx);
        let err = allow.resolve_file(&sneaky).unwrap_err();
        assert!(
            err.to_string().contains("traversal") || err.to_string().contains("parent-dir"),
            "got {err}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_symlink_escape_outside_allowlist() {
        let root = test_dir("symlink");
        let aicx = root.join(".aicx");
        let src_root = root.join(".grok").join("sessions");
        fs::create_dir_all(&src_root).unwrap();
        let outside_dir = root.join("outside");
        fs::create_dir_all(&outside_dir).unwrap();
        let secret = outside_dir.join("secret.jsonl");
        fs::write(&secret, b"leaked").unwrap();
        let link = src_root.join("escaped.jsonl");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            let allow = SourceAllowlist::for_operator(&root, &aicx);
            let err = allow.resolve_file(&link).unwrap_err();
            assert!(
                err.to_string().contains("escapes approved roots"),
                "symlink escape must fail containment; got {err}"
            );
        }
        #[cfg(not(unix))]
        {
            let _ = (aicx, link, secret);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_directory_targets() {
        let root = test_dir("dir");
        let aicx = root.join(".aicx");
        let src_root = root.join(".gemini").join("tmp");
        fs::create_dir_all(&src_root).unwrap();
        let allow = SourceAllowlist::for_operator(&root, &aicx);
        let err = allow.resolve_file(&src_root).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "got {err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn catalog_path_under_aicx_home_is_readable() {
        let root = test_dir("catalog");
        let aicx = root.join(".aicx");
        let catalog = aicx.join("catalog");
        fs::create_dir_all(&catalog).unwrap();
        let sessions = catalog.join("sessions.jsonl");
        let mut f = File::create(&sessions).unwrap();
        writeln!(f, r#"{{"schema":"aicx.catalog.session.v1","session_id":"s","agent":"claude","source_path":"/x"}}"#).unwrap();
        let body = read_under_aicx_home(&aicx, &sessions).unwrap();
        assert!(body.contains("session_id"));
        let _ = fs::remove_dir_all(&root);
    }
}
