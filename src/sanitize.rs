//! Path and input sanitization for ai-contexters.
//!
//! Follows the established pattern from rmcp-memex/path_utils.rs:
//! traversal check → canonicalize → allowlist validation.
//!
//! Prevents path traversal and command injection from user-supplied inputs
//! (CLI arguments, project names, agent names).
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};

/// Known safe agent binary names.
const ALLOWED_AGENTS: &[&str] = &["claude", "codex"];

// ============================================================================
// Core helpers (mirroring rmcp-memex pattern)
// ============================================================================

/// Check if a path string contains traversal sequences.
fn contains_traversal(path: &str) -> bool {
    let path_lower = path.to_lowercase();
    path_lower.contains("..") || path.contains('\0') || path.contains('\n') || path.contains('\r')
}

/// Get the user's home directory.
fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| anyhow!("Cannot determine home directory from $HOME"))
}

/// Canonicalize a path, returning error if it doesn't exist.
fn canonicalize_existing(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .map_err(|e| anyhow!("Cannot canonicalize path '{}': {}", path.display(), e))
}

/// Validate that a path is under an allowed base directory.
fn is_under_allowed_base(path: &Path) -> Result<bool> {
    let home = home_dir()?;

    if path.starts_with(&home) {
        return Ok(true);
    }

    #[cfg(target_os = "macos")]
    if path.starts_with("/Users") {
        let components: Vec<_> = path.components().collect();
        if components.len() >= 3 {
            return Ok(true);
        }
    }

    // Temporary directories (tests)
    if path.starts_with("/tmp")
        || path.starts_with("/var/folders")
        || path.starts_with("/private/tmp")
        || path.starts_with("/private/var/folders")
    {
        return Ok(true);
    }

    Ok(false)
}

// ============================================================================
// Public API: path validation
// ============================================================================

/// Sanitize and validate a path that must exist (for reading).
///
/// Traversal check → canonicalize → allowlist.
pub fn validate_read_path(path: &Path) -> Result<PathBuf> {
    let path_str = path.to_string_lossy();
    if contains_traversal(&path_str) {
        return Err(anyhow!(
            "Path contains invalid traversal sequence: {}",
            path_str
        ));
    }

    if !path.exists() {
        return Err(anyhow!("Path does not exist: {}", path.display()));
    }

    let canonical = canonicalize_existing(path)?;

    if !is_under_allowed_base(&canonical)? {
        return Err(anyhow!(
            "Cannot read from path outside allowed directories: {}",
            canonical.display()
        ));
    }

    Ok(canonical)
}

/// Sanitize and validate a path for writing (may not exist yet).
///
/// Traversal check → validate parent → allowlist.
pub fn validate_write_path(path: &Path) -> Result<PathBuf> {
    let path_str = path.to_string_lossy();
    if contains_traversal(&path_str) {
        return Err(anyhow!(
            "Path contains invalid traversal sequence: {}",
            path_str
        ));
    }

    if path.exists() {
        let canonical = canonicalize_existing(path)?;
        if !is_under_allowed_base(&canonical)? {
            return Err(anyhow!(
                "Cannot write to path outside allowed directories: {}",
                canonical.display()
            ));
        }
        return Ok(canonical);
    }

    // New path — validate parent or grandparent
    if let Some(parent) = path.parent() {
        if parent.exists() {
            let canonical_parent = canonicalize_existing(parent)?;
            if !is_under_allowed_base(&canonical_parent)? {
                return Err(anyhow!(
                    "Parent directory '{}' is not under an allowed directory",
                    canonical_parent.display()
                ));
            }
        } else if let Some(grandparent) = parent.parent()
            && grandparent.exists()
        {
            let canonical_gp = canonicalize_existing(grandparent)?;
            if !is_under_allowed_base(&canonical_gp)? {
                return Err(anyhow!(
                    "Path '{}' would be created outside allowed directories",
                    path.display()
                ));
            }
        }
    }

    Ok(path.to_path_buf())
}

/// Sanitize a directory path used for reading (e.g., chunks_dir, contexts_dir).
///
/// Traversal check → canonicalize → allowlist. Must be a directory.
pub fn validate_dir_path(path: &Path) -> Result<PathBuf> {
    let validated = validate_read_path(path)?;
    if !validated.is_dir() {
        return Err(anyhow!("Path is not a directory: {}", validated.display()));
    }
    Ok(validated)
}

// ============================================================================
// Public API: input validation
// ============================================================================

/// Validate an agent name against the allowlist.
///
/// Prevents command injection by ensuring only known agent binaries
/// are passed to `Command::new()`.
pub fn safe_agent_name(name: &str) -> Result<&str> {
    if ALLOWED_AGENTS.contains(&name) {
        Ok(name)
    } else {
        Err(anyhow!(
            "Unknown agent: {:?}. Allowed: {}",
            name,
            ALLOWED_AGENTS.join(", ")
        ))
    }
}

/// Sanitize a project name used in filesystem paths.
///
/// Rejects names containing path separators, traversal sequences,
/// or control characters.
pub fn safe_project_name(name: &str) -> Result<&str> {
    if name.is_empty() {
        return Err(anyhow!("Project name cannot be empty"));
    }
    if contains_traversal(name) || name.contains('/') || name.contains('\\') {
        return Err(anyhow!("Invalid project name: {:?}", name));
    }
    Ok(name)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_contains_traversal() {
        assert!(contains_traversal("../etc/passwd"));
        assert!(contains_traversal("foo/../bar"));
        assert!(contains_traversal("path\0with\0nulls"));
        assert!(contains_traversal("line\nbreak"));
        assert!(!contains_traversal("/normal/path"));
        assert!(!contains_traversal("simple_name"));
        assert!(!contains_traversal("./relative/path"));
    }

    #[test]
    fn test_validate_read_path_existing() {
        let tmp = std::env::temp_dir().join("ai-ctx-san-test-read");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let test_file = tmp.join("test.txt");
        fs::write(&test_file, "test").unwrap();

        let result = validate_read_path(&test_file);
        assert!(result.is_ok(), "Failed: {:?}", result);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_validate_read_path_traversal() {
        let bad = Path::new("/tmp/../../../etc/passwd");
        assert!(validate_read_path(bad).is_err());
    }

    #[test]
    fn test_validate_read_path_nonexistent() {
        let missing = Path::new("/tmp/ai-ctx-nonexistent-12345");
        assert!(validate_read_path(missing).is_err());
    }

    #[test]
    fn test_validate_write_path_new() {
        let tmp = std::env::temp_dir().join("ai-ctx-san-test-write");
        let _ = fs::create_dir_all(&tmp);
        let new_file = tmp.join("new.txt");
        let result = validate_write_path(&new_file);
        assert!(result.is_ok(), "Failed: {:?}", result);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_validate_write_path_traversal() {
        let bad = Path::new("/tmp/../../../etc/evil.txt");
        assert!(validate_write_path(bad).is_err());
    }

    #[test]
    fn test_validate_dir_path() {
        let tmp = std::env::temp_dir();
        assert!(validate_dir_path(&tmp).is_ok());
    }

    #[test]
    fn test_safe_agent_name_valid() {
        assert_eq!(safe_agent_name("claude").unwrap(), "claude");
        assert_eq!(safe_agent_name("codex").unwrap(), "codex");
    }

    #[test]
    fn test_safe_agent_name_rejects_unknown() {
        assert!(safe_agent_name("rm").is_err());
        assert!(safe_agent_name("bash").is_err());
        assert!(safe_agent_name("claude; rm -rf /").is_err());
    }

    #[test]
    fn test_safe_project_name_valid() {
        assert!(safe_project_name("my-project").is_ok());
        assert!(safe_project_name("lbrx-services").is_ok());
        assert!(safe_project_name("CodeScribe").is_ok());
    }

    #[test]
    fn test_safe_project_name_rejects_bad() {
        assert!(safe_project_name("../etc").is_err());
        assert!(safe_project_name("foo/bar").is_err());
        assert!(safe_project_name("").is_err());
        assert!(safe_project_name("foo\0bar").is_err());
    }
}
