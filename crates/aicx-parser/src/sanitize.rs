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

    // New path — walk ancestors until we find an existing base directory and validate it.
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| anyhow!("Cannot determine current directory: {}", e))?
            .join(path)
    };

    let mut ancestor = Some(candidate.as_path());
    let mut existing_ancestor = None;
    while let Some(current) = ancestor {
        if current.exists() {
            existing_ancestor = Some(canonicalize_existing(current)?);
            break;
        }
        ancestor = current.parent();
    }

    let canonical_base = existing_ancestor.ok_or_else(|| {
        anyhow!(
            "Cannot validate write path '{}': no existing ancestor found",
            path.display()
        )
    })?;

    if !is_under_allowed_base(&canonical_base)? {
        return Err(anyhow!(
            "Path '{}' would be created outside allowed directories",
            path.display()
        ));
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

/// Open a file for reading only after validating the path.
pub fn open_file_validated(path: &Path) -> Result<std::fs::File> {
    let validated = validate_read_path(path)?;
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    std::fs::File::open(&validated)
        .map_err(|e| anyhow!("Failed to open '{}': {}", validated.display(), e))
}

/// Create or truncate a file only after validating the write path.
pub fn create_file_validated(path: &Path) -> Result<std::fs::File> {
    let validated = validate_write_path(path)?;
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    std::fs::File::create(&validated)
        .map_err(|e| anyhow!("Failed to create '{}': {}", validated.display(), e))
}

/// Read a UTF-8 text file only after validating the path.
pub fn read_to_string_validated(path: &Path) -> Result<String> {
    let validated = validate_read_path(path)?;
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    std::fs::read_to_string(&validated)
        .map_err(|e| anyhow!("Failed to read '{}': {}", validated.display(), e))
}

/// Read a directory only after validating it as an allowed directory path.
pub fn read_dir_validated(path: &Path) -> Result<std::fs::ReadDir> {
    let validated = validate_dir_path(path)?;
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
    std::fs::read_dir(&validated)
        .map_err(|e| anyhow!("Failed to read dir '{}': {}", validated.display(), e))
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
    fn test_validate_write_path_rejects_non_allowed_ancestor() {
        let bad = Path::new("/etc/ai-contexters-test/nope/file.txt");
        assert!(validate_write_path(bad).is_err());
    }

    #[test]
    fn test_validate_write_path_relative_with_missing_parents() {
        let nested = Path::new("target/ai-ctx-sanitize-new/subdir/new.txt");
        assert!(validate_write_path(nested).is_ok());
    }

    #[test]
    fn test_validate_dir_path() {
        let tmp = std::env::temp_dir();
        assert!(validate_dir_path(&tmp).is_ok());
    }

    #[test]
    fn test_open_file_validated() {
        let tmp = std::env::temp_dir().join("ai-ctx-san-open-file");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let test_file = tmp.join("test.txt");
        fs::write(&test_file, "hello").unwrap();

        let mut opened = open_file_validated(&test_file).unwrap();
        let mut content = String::new();
        use std::io::Read as _;
        opened.read_to_string(&mut content).unwrap();
        assert_eq!(content, "hello");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_read_to_string_validated() {
        let tmp = std::env::temp_dir().join("ai-ctx-san-read-string");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let test_file = tmp.join("test.txt");
        fs::write(&test_file, "hello").unwrap();

        let content = read_to_string_validated(&test_file).unwrap();
        assert_eq!(content, "hello");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_create_file_validated() {
        let tmp = std::env::temp_dir().join("ai-ctx-san-create-file");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        let test_file = tmp.join("test.txt");

        let mut created = create_file_validated(&test_file).unwrap();
        use std::io::Write as _;
        created.write_all(b"hello").unwrap();
        drop(created);

        let content = fs::read_to_string(&test_file).unwrap();
        assert_eq!(content, "hello");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_read_dir_validated() {
        let tmp = std::env::temp_dir().join("ai-ctx-san-read-dir");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("a.txt"), "a").unwrap();

        let entries = read_dir_validated(&tmp)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .count();
        assert_eq!(entries, 1);

        let _ = fs::remove_dir_all(&tmp);
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
}

// ============================================================================
// Query normalization (PL/EN diacritics + case folding)
// ============================================================================

/// Normalize text for fuzzy matching: lowercase + strip Polish diacritics.
///
/// Maps: ą→a, ć→c, ę→e, ł→l, ń→n, ó→o, ś→s, ź→z, ż→z
/// Enables "wdrozenie" to match "wdrożenie", "zrodlo" to match "źródło", etc.
pub fn normalize_query(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'Ą' | 'ą' => 'a',
            'Ć' | 'ć' => 'c',
            'Ę' | 'ę' => 'e',
            'Ł' | 'ł' => 'l',
            'Ń' | 'ń' => 'n',
            'Ó' | 'ó' => 'o',
            'Ś' | 'ś' => 's',
            'Ź' | 'ź' | 'Ż' | 'ż' => 'z',
            _ => c,
        })
        .collect::<String>()
        .to_lowercase()
}

// ============================================================================
// Self-echo filtering (prevents feedback loops)
// ============================================================================

/// Patterns in messages that indicate aicx's own operational traffic.
/// These create feedback loops: search → log → extract → search matches own query.
/// Retired MCP tool names stay here so historical traces remain filterable.
const SELF_ECHO_PATTERNS: &[&str] = &[
    // MCP tool calls
    "aicx_search",
    "aicx_rank",
    "aicx_refs",
    "aicx_store",
    // Dashboard API calls
    "/api/search/fuzzy",
    "/api/search/semantic",
    "/api/search/cross",
    "/api/health",
    "/api/regenerate",
    "/api/status",
    // MCP JSON-RPC
    "\"method\":\"tools/call\"",
    "\"method\":\"tools/list\"",
    "\"method\":\"initialize\"",
    // CLI self-invocations
    "aicx store -H",
    "aicx rank -p",
    "aicx refs -H",
    "aicx serve",
    "aicx dashboard-serve",
];

/// Sentinel brackets for aicx read blocks injected by vc-init / vc-agents.
/// Content between these markers is recycled context, not original signal.
const AICX_READ_BEGIN: &str = "【aicx:read】";
const AICX_READ_END: &str = "【/aicx:read】";

/// Returns true if a message is aicx operational self-echo that should be
/// filtered from extraction to prevent feedback loops.
///
/// A message is self-echo if >50% of its non-empty lines match patterns,
/// excluding lines inside 【aicx:read】...【/aicx:read】 blocks (which are
/// counted as echo unconditionally).
pub fn is_self_echo(message: &str) -> bool {
    let lines: Vec<&str> = message
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    if lines.is_empty() {
        return false;
    }

    let mut echo_lines = 0usize;
    let mut inside_aicx_block = false;

    for line in &lines {
        if line.contains(AICX_READ_BEGIN) {
            inside_aicx_block = true;
            echo_lines += 1;
            continue;
        }
        if line.contains(AICX_READ_END) {
            inside_aicx_block = false;
            echo_lines += 1;
            continue;
        }
        if inside_aicx_block {
            echo_lines += 1;
            continue;
        }
        let lower = line.to_lowercase();
        if SELF_ECHO_PATTERNS
            .iter()
            .any(|pat| lower.contains(&pat.to_lowercase()))
        {
            echo_lines += 1;
        }
    }

    // Message is self-echo if majority of lines match
    echo_lines > 0 && echo_lines * 2 >= lines.len()
}

/// Filter a vec of timeline entries, removing self-echo messages.
pub fn filter_self_echo<T>(entries: Vec<T>, get_message: impl Fn(&T) -> &str) -> Vec<T> {
    entries
        .into_iter()
        .filter(|e| !is_self_echo(get_message(e)))
        .collect()
}

#[cfg(test)]
mod echo_tests {
    use super::*;

    #[test]
    fn test_normal_message_not_echo() {
        assert!(!is_self_echo("Fix the login regression in auth middleware"));
        assert!(!is_self_echo("Decision: use per-chunk scoring"));
        assert!(!is_self_echo("TODO: add tests for edge cases"));
    }

    #[test]
    fn test_search_call_is_echo() {
        assert!(is_self_echo(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"aicx_search","arguments":{"query":"deploy vistacare"}}}"#
        ));
    }

    #[test]
    fn test_api_call_is_echo() {
        assert!(is_self_echo(
            r#"curl -s "http://127.0.0.1:8033/api/search/fuzzy?q=deploy+vistacare&limit=3""#
        ));
    }

    #[test]
    fn test_cli_self_invocation_is_echo() {
        assert!(is_self_echo("aicx store -H 24 --incremental"));
        assert!(is_self_echo("aicx rank -p ai-contexters -H 72 --strict"));
    }

    #[test]
    fn test_mention_in_larger_message_not_echo() {
        // Mere mention of aicx in a discussion should NOT be filtered
        let msg = "We should add aicx_search to the MCP server.\n\
                   The architecture looks clean.\n\
                   Let's proceed with implementation.\n\
                   Decision: expose 4 tools via rmcp.";
        assert!(!is_self_echo(msg));
    }
}

#[cfg(test)]
mod normalize_tests {
    use super::*;

    #[test]
    fn test_normalize_query_strips_diacritics() {
        assert_eq!(normalize_query("wdrożenie"), "wdrozenie");
        assert_eq!(normalize_query("źródło ŁĄCZNOŚCI"), "zrodlo lacznosci");
        assert_eq!(normalize_query("Deploy Vista"), "deploy vista");
        assert_eq!(normalize_query("ąćęłńóśźż"), "acelnoszz");
    }

    #[test]
    fn test_safe_project_name_rejects_bad() {
        assert!(safe_project_name("../etc").is_err());
        assert!(safe_project_name("foo/bar").is_err());
        assert!(safe_project_name("").is_err());
        assert!(safe_project_name("foo\0bar").is_err());
    }
}
