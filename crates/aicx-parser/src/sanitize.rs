//! Path and input sanitization for ai-contexters.
//!
//! Follows the established pattern:
//! traversal check → canonicalize → allowlist validation.
//!
//! Prevents path traversal and command injection from user-supplied inputs
//! (CLI arguments, project names, agent names).
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Result, anyhow};
use std::borrow::Cow;
use std::fmt;
use std::io::{self, BufRead, Read};
use std::path::{Component, Path, PathBuf};

/// Known safe extractor agent names.
pub const ALLOWED_AGENTS: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "junie",
    "codescribe",
    "operator-md",
];

const AICX_ALLOW_TMP_ENV: &str = "AICX_ALLOW_TMP";

pub const MAX_VALIDATED_BYTES: usize = 8 * 1024 * 1024;

/// Separate cap for long-lived JSON state files (e.g. `state.json`). The
/// generic 8 MiB validated-read cap is tuned for corpus / sidecar inputs,
/// but a real AICX install accumulates many projects' `seen_hashes` and
/// run history over months. Applying the generic cap would hard-fail on
/// startup for legitimate large states. 128 MiB is well above any
/// realistic state size yet still catches runaway growth or a corrupted
/// file masquerading as state.
pub const MAX_STATE_JSON_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SanitizeError {
    FileTooLarge {
        path: PathBuf,
        max_bytes: usize,
        actual_bytes: u64,
    },
    StateFileTooLarge {
        path: PathBuf,
        max_bytes: usize,
        actual_bytes: u64,
    },
}

impl fmt::Display for SanitizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SanitizeError::FileTooLarge {
                path,
                max_bytes,
                actual_bytes,
            } => write!(
                f,
                "File '{}' exceeds validated read cap: {} bytes > {} bytes",
                path.display(),
                actual_bytes,
                max_bytes
            ),
            SanitizeError::StateFileTooLarge {
                path,
                max_bytes,
                actual_bytes,
            } => write!(
                f,
                "State file '{}' is too large: {} bytes > {} bytes. \
                 This is not generic JSON corruption — the file exceeds the dedicated state cap. \
                 Investigate runaway `seen_hashes` growth or restore from `.bak` before retrying.",
                path.display(),
                actual_bytes,
                max_bytes
            ),
        }
    }
}

impl std::error::Error for SanitizeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentSanitizationWarning {
    NullByteStripped(usize),
    BidiOverride(char, usize),
    ZeroWidth(char, usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedContent<'a> {
    pub text: Cow<'a, str>,
    pub warnings: Vec<ContentSanitizationWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedLine {
    pub line: String,
    pub exceeded: bool,
}

// ============================================================================
// Core helpers (mirroring rmcp-memex pattern)
// ============================================================================

/// Check if a path string contains traversal sequences.
///
/// Genuine path traversal is `..` as its own path component (e.g. `../`,
/// `foo/../bar`). Substring matching against `..` falsely flags innocent
/// directory names like `...`, `foo..bar`, or `a..b/c`, which broke
/// real corpus iteration when ingest stored a literal three-dot folder.
/// We split the path into components and only flag the canonical
/// `Component::ParentDir`, plus the usual control characters.
fn contains_traversal(path: &str) -> bool {
    if path.contains('\0') || path.contains('\n') || path.contains('\r') {
        return true;
    }
    Path::new(path)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
}

fn current_user_allowed_bases() -> Result<Vec<PathBuf>> {
    let mut bases = Vec::new();
    for base in [dirs::home_dir(), dirs::cache_dir(), dirs::data_dir()]
        .into_iter()
        .flatten()
    {
        if !bases.iter().any(|existing| existing == &base) {
            bases.push(base);
        }
    }

    if bases.is_empty() {
        return Err(anyhow!(
            "Cannot determine current user allowed base directories"
        ));
    }

    Ok(bases)
}

/// Canonicalize a path, returning error if it doesn't exist.
fn canonicalize_existing(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .map_err(|e| anyhow!("Cannot canonicalize path '{}': {}", path.display(), e))
}

/// Cargo test builds allow tempfile-backed `/tmp` paths, while normal debug and
/// release builds require `AICX_ALLOW_TMP=1` so local dev runs follow the same
/// explicit opt-in contract as production.
fn temp_allowlist_enabled() -> bool {
    temp_allowlist_enabled_for_runtime(cfg!(test), running_under_cargo_test_harness())
}

fn temp_allowlist_enabled_for_runtime(is_test_build: bool, is_cargo_test_harness: bool) -> bool {
    is_test_build
        || is_cargo_test_harness
        || std::env::var(AICX_ALLOW_TMP_ENV).is_ok_and(|value| value == "1")
}

fn running_under_cargo_test_harness() -> bool {
    std::env::current_exe()
        .ok()
        .is_some_and(|exe| is_cargo_test_exe_path(&exe))
}

fn is_cargo_test_exe_path(path: &Path) -> bool {
    let has_deps_component = path.components().any(|component| {
        matches!(
            component,
            Component::Normal(text) if text == std::ffi::OsStr::new("deps")
        )
    });
    if !has_deps_component {
        return false;
    }

    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.rsplit_once('-'))
        .is_some_and(|(_, suffix)| {
            suffix.len() >= 8 && suffix.chars().all(|ch| ch.is_ascii_hexdigit())
        })
}

fn is_temp_allowlist_path(path: &Path) -> bool {
    path.starts_with("/tmp")
        || path.starts_with("/var/folders")
        || path.starts_with("/private/tmp")
        || path.starts_with("/private/var/folders")
}

/// Validate that a path is under an allowed base directory.
fn is_under_allowed_base(path: &Path) -> Result<bool> {
    for base in current_user_allowed_bases()? {
        if path.starts_with(base) {
            return Ok(true);
        }
    }

    if is_temp_allowlist_path(path) {
        return Ok(temp_allowlist_enabled());
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
    std::fs::OpenOptions::new()
        .read(true)
        .open(&validated)
        .map_err(|e| anyhow!("Failed to open '{}': {}", validated.display(), e))
}

/// Create or truncate a file only after validating the write path.
pub fn create_file_validated(path: &Path) -> Result<std::fs::File> {
    let validated = validate_write_path(path)?;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&validated)
        .map_err(|e| anyhow!("Failed to create '{}': {}", validated.display(), e))
}

/// Read a UTF-8 text file only after validating the path.
pub fn read_to_string_validated(path: &Path) -> Result<String> {
    read_to_string_with_cap(path, MAX_VALIDATED_BYTES, false)
}

/// Read AICX `state.json` (or its `.bak`) under the dedicated state cap.
///
/// Long-lived installs legitimately grow `seen_hashes`, run history, and
/// per-project state into the tens of MiB. The generic 8 MiB cap is
/// tuned for corpus inputs and would hard-fail those installs at
/// startup, ahead of backup recovery. This entry point uses
/// [`MAX_STATE_JSON_BYTES`] and surfaces a state-specific error so the
/// failure mode is unmistakable (and obviously distinct from generic
/// JSON corruption).
pub fn read_state_json_validated(path: &Path) -> Result<String> {
    read_to_string_with_cap(path, MAX_STATE_JSON_BYTES, true)
}

fn read_to_string_with_cap(path: &Path, max_bytes: usize, is_state: bool) -> Result<String> {
    let validated = validate_read_path(path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .open(&validated)
        .map_err(|e| anyhow!("Failed to open '{}': {}", validated.display(), e))?;
    let metadata = file
        .metadata()
        .map_err(|e| anyhow!("Failed to stat '{}': {}", validated.display(), e))?;
    if metadata.len() > max_bytes as u64 {
        return Err(too_large_error(validated, max_bytes, metadata.len(), is_state).into());
    }

    let mut reader = file.take(max_bytes.saturating_add(1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|e| anyhow!("Failed to read '{}': {}", validated.display(), e))?;
    if bytes.len() > max_bytes {
        return Err(too_large_error(validated, max_bytes, bytes.len() as u64, is_state).into());
    }
    String::from_utf8(bytes).map_err(|e| anyhow!("Failed to read '{}': {}", validated.display(), e))
}

fn too_large_error(
    path: PathBuf,
    max_bytes: usize,
    actual_bytes: u64,
    is_state: bool,
) -> SanitizeError {
    if is_state {
        SanitizeError::StateFileTooLarge {
            path,
            max_bytes,
            actual_bytes,
        }
    } else {
        SanitizeError::FileTooLarge {
            path,
            max_bytes,
            actual_bytes,
        }
    }
}

/// Read a directory only after validating it as an allowed directory path.
pub fn read_dir_validated(path: &Path) -> Result<std::fs::ReadDir> {
    let validated = validate_dir_path(path)?;
    // FP: `pub fn validate_dir_path(path: &Path) -> Result<PathBuf>`
    // (line 302) delegates to `validate_read_path(path: &Path)` (line 215),
    // which rejects traversal, canonicalizes the existing dir, and enforces
    // the allowed-base policy before this directory iterator is created.
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path -- FP: validate_dir_path(Path) at line 302 -> validate_read_path(Path) at line 215 rejects traversal, canonicalizes, and enforces allowed-base policy.
    std::fs::read_dir(&validated)
        .map_err(|e| anyhow!("Failed to read dir '{}': {}", validated.display(), e))
}

pub fn read_line_capped<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> io::Result<Option<CappedLine>> {
    let mut buf = Vec::new();
    let read = {
        let mut limited = reader.take(max_bytes.saturating_add(1) as u64);
        limited.read_until(b'\n', &mut buf)?
    };
    if read == 0 {
        return Ok(None);
    }

    let exceeded = buf.len() > max_bytes;
    if exceeded {
        let ended_at_newline = buf.last().copied() == Some(b'\n');
        buf.truncate(max_bytes);
        // Walk back past UTF-8 continuation bytes (0b10xxxxxx) so we don't
        // chop a multi-byte sequence mid-codepoint; otherwise a valid input
        // line would surface as InvalidData purely because of the cap.
        while let Some(&last) = buf.last() {
            if (last & 0xC0) == 0x80 {
                buf.pop();
            } else if last >= 0xC0 {
                // Lead byte of a multi-byte sequence whose continuation bytes
                // were just stripped — drop the lead too.
                buf.pop();
                break;
            } else {
                break;
            }
        }
        if !ended_at_newline {
            drain_until_newline(reader)?;
        }
    }

    let line =
        String::from_utf8(buf).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    Ok(Some(CappedLine { line, exceeded }))
}

fn drain_until_newline<R: BufRead>(reader: &mut R) -> io::Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let consume = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |idx| idx + 1);
        let ended_at_newline = available.get(consume.saturating_sub(1)) == Some(&b'\n');
        reader.consume(consume);
        if ended_at_newline {
            return Ok(());
        }
    }
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
// Public API: message content sanitization
// ============================================================================

pub fn sanitize_chunk_content(text: &str) -> SanitizedContent<'_> {
    let (text, warnings) = sanitize_message_content(text);
    SanitizedContent { text, warnings }
}

fn sanitize_message_content(input: &str) -> (Cow<'_, str>, Vec<ContentSanitizationWarning>) {
    let mut output: Option<String> = None;
    let mut warnings = Vec::new();
    let mut chars = input.char_indices().peekable();

    while let Some((offset, ch)) = chars.next() {
        match ch {
            '\0' => {
                warnings.push(ContentSanitizationWarning::NullByteStripped(offset));
                ensure_output(&mut output, input, offset);
            }
            '\r' => {
                ensure_output(&mut output, input, offset);
                output.as_mut().expect("output initialized").push('\n');
                if chars.peek().is_some_and(|(_, next)| *next == '\n') {
                    chars.next();
                }
            }
            _ => {
                if is_bidi_override(ch) {
                    warnings.push(ContentSanitizationWarning::BidiOverride(ch, offset));
                } else if is_zero_width(ch) {
                    warnings.push(ContentSanitizationWarning::ZeroWidth(ch, offset));
                }

                if let Some(out) = output.as_mut() {
                    out.push(ch);
                }
            }
        }
    }

    match output {
        Some(output) => (Cow::Owned(output), warnings),
        None => (Cow::Borrowed(input), warnings),
    }
}

fn ensure_output(output: &mut Option<String>, input: &str, offset: usize) {
    if output.is_none() {
        let mut owned = String::with_capacity(input.len());
        owned.push_str(&input[..offset]);
        *output = Some(owned);
    }
}

fn is_bidi_override(ch: char) -> bool {
    matches!(
        ch,
        '\u{202A}'
            | '\u{202B}'
            | '\u{202C}'
            | '\u{202D}'
            | '\u{202E}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
    )
}

fn is_zero_width(ch: char) -> bool {
    matches!(ch, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}')
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Mutex, MutexGuard};

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let guard = ENV_MUTEX.lock().unwrap();
            let previous = std::env::var_os(key);
            // SAFETY: these tests serialize all mutations of this process env
            // var and restore the previous value while holding the same mutex.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            Self {
                key,
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: the mutex guard is held until after restoration, keeping
            // this crate's env-var tests serialized around AICX_ALLOW_TMP.
            unsafe {
                match &self.previous {
                    Some(previous) => std::env::set_var(self.key, previous),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

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
    fn test_contains_traversal_does_not_flag_three_dot_folder() {
        // Regression: a literal `...` directory name (yes, it happens — we had
        // a broken ingest that wrote `~/.aicx/store/...`) is NOT path traversal
        // and must not nuke the entire corpus iteration.
        assert!(!contains_traversal("..."));
        assert!(!contains_traversal("/Users/foo/.aicx/store/..."));
        assert!(!contains_traversal("foo/.../bar"));
    }

    #[test]
    fn test_contains_traversal_does_not_flag_dot_dot_inside_name() {
        // `..` as a substring inside a normal component is fine; only a
        // standalone `..` component is genuine traversal.
        assert!(!contains_traversal("foo..bar"));
        assert!(!contains_traversal("a..b/c"));
        assert!(!contains_traversal("normal..text"));
        assert!(!contains_traversal("/srv/a..b/c"));
    }

    #[test]
    fn test_contains_traversal_carriage_return() {
        assert!(contains_traversal("path\rwith\rcr"));
    }

    #[test]
    fn test_tmp_allowlist_hybrid_policy() {
        {
            let _env = EnvVarGuard::set(AICX_ALLOW_TMP_ENV, None);
            assert!(temp_allowlist_enabled_for_runtime(true, false));
            assert!(temp_allowlist_enabled_for_runtime(false, true));
            assert!(!temp_allowlist_enabled_for_runtime(false, false));
        }

        {
            let _env = EnvVarGuard::set(AICX_ALLOW_TMP_ENV, Some("1"));
            assert!(temp_allowlist_enabled_for_runtime(false, false));
        }

        {
            let _env = EnvVarGuard::set(AICX_ALLOW_TMP_ENV, Some("true"));
            assert!(!temp_allowlist_enabled_for_runtime(false, false));
        }
    }

    #[test]
    fn test_cargo_test_exe_detection_accepts_custom_target_dir_layout() {
        let path =
            Path::new("/Users/runner/work/cache/aicx-macos/debug/deps/aicx-0b1797b9ba8904ee");
        assert!(is_cargo_test_exe_path(path));
    }

    #[test]
    fn test_cargo_test_exe_detection_rejects_normal_binary_paths() {
        let path = Path::new("/Users/runner/work/aicx/target/debug/aicx");
        assert!(!is_cargo_test_exe_path(path));
    }

    #[test]
    fn test_current_user_allowed_bases_are_accepted() {
        for base in current_user_allowed_bases().expect("current user dirs") {
            assert!(
                is_under_allowed_base(&base.join("aicx-sanitize-test")).expect("allowlist check"),
                "current user base should be allowed: {}",
                base.display()
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_macos_other_user_path_rejected() {
        let path = Path::new("/Users/other_user/Documents/secret.txt");
        assert!(
            !is_under_allowed_base(path).expect("allowlist check"),
            "macOS /Users allowlist must not generalize across users"
        );
    }

    #[test]
    fn test_validate_read_path_existing() {
        let _env = EnvVarGuard::set(AICX_ALLOW_TMP_ENV, None);
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
    fn test_sanitize_strips_nul_byte_and_warns() {
        let sanitized = sanitize_chunk_content("abc\0def\0");
        assert_eq!(sanitized.text, "abcdef");
        assert_eq!(
            sanitized.warnings,
            vec![
                ContentSanitizationWarning::NullByteStripped(3),
                ContentSanitizationWarning::NullByteStripped(7),
            ]
        );
    }

    #[test]
    fn test_sanitize_normalizes_crlf_to_lf() {
        let sanitized = sanitize_chunk_content("one\r\ntwo\rthree\nfour");
        assert_eq!(sanitized.text, "one\ntwo\nthree\nfour");
        assert!(sanitized.warnings.is_empty());
    }

    #[test]
    fn test_sanitize_preserves_unicode_emoji() {
        let sanitized = sanitize_chunk_content("ship it 🚀");
        assert_eq!(sanitized.text, "ship it 🚀");
        assert!(sanitized.warnings.is_empty());
    }

    #[test]
    fn test_sanitize_preserves_polish_diacritics_nfc() {
        let input = "Zażółć gęślą jaźń";
        let sanitized = sanitize_chunk_content(input);
        assert_eq!(sanitized.text, input);
        assert!(sanitized.warnings.is_empty());
    }

    #[test]
    fn test_sanitize_bidi_override_warns_but_does_not_strip() {
        let input = "safe \u{202E}txt";
        let sanitized = sanitize_chunk_content(input);
        assert_eq!(sanitized.text, input);
        assert_eq!(
            sanitized.warnings,
            vec![ContentSanitizationWarning::BidiOverride('\u{202E}', 5)]
        );
    }

    #[test]
    fn test_sanitize_zero_width_warns_but_does_not_strip() {
        let input = "zero\u{200B}width";
        let sanitized = sanitize_chunk_content(input);
        assert_eq!(sanitized.text, input);
        assert_eq!(
            sanitized.warnings,
            vec![ContentSanitizationWarning::ZeroWidth('\u{200B}', 4)]
        );
    }

    #[test]
    fn test_safe_agent_name_valid() {
        assert_eq!(safe_agent_name("claude").unwrap(), "claude");
        assert_eq!(safe_agent_name("codex").unwrap(), "codex");
        assert_eq!(safe_agent_name("gemini").unwrap(), "gemini");
        assert_eq!(safe_agent_name("junie").unwrap(), "junie");
        assert_eq!(safe_agent_name("codescribe").unwrap(), "codescribe");
        assert_eq!(safe_agent_name("operator-md").unwrap(), "operator-md");
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

/// Normalize text for fuzzy matching: NFC + lowercase + strip Polish diacritics.
///
/// NFC canonical composition is applied first so NFD-stored variants (e.g.
/// `o` + combining acute) collapse to the same code points as NFC-stored
/// composed variants (`ó`) before diacritic mapping. Without this step,
/// `źródło` typed via NFD on one platform would refuse to match the same
/// word typed via NFC on another.
///
/// Maps: ą→a, ć→c, ę→e, ł→l, ń→n, ó→o, ś→s, ź→z, ż→z
/// Enables "wdrozenie" to match "wdrożenie", "zrodlo" to match "źródło", etc.
pub fn normalize_query(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    text.nfc()
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

/// MCP tool names + dashboard HTTP routes that indicate aicx's own
/// operational traffic. These are the "stable surface" patterns; CLI
/// subcommand patterns are derived from [`CLI_SUBCOMMAND_NAMES`] and the
/// JSON-RPC catch-all lives in its own constant so each axis stays
/// auditable.
///
/// Retired MCP tool names stay here so historical traces remain filterable.
const STABLE_SELF_ECHO_PATTERNS: &[&str] = &[
    // MCP tool calls (current + retired names so historical traces remain filterable)
    "aicx_search",
    "aicx_read",
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
];

/// Generic MCP JSON-RPC catch-all. Any line containing a raw JSON-RPC 2.0
/// envelope is, by construction, aicx's own protocol traffic (or another
/// MCP client's, which is still recycled context — not original signal).
/// This subsumes the previous per-method patterns
/// (`"method":"tools/call"`, `"method":"tools/list"`,
/// `"method":"initialize"`) — any new method name lands here automatically
/// without recompile. L36 (c).
const MCP_JSONRPC_CATCH_ALL: &str = "\"jsonrpc\":\"2.0\"";

/// Retired CLI subcommand names. Kept here so historical traces (operator
/// shells, logged transcripts from older aicx versions) still classify as
/// self-echo. Parallel to retired MCP tool names in
/// `STABLE_SELF_ECHO_PATTERNS`.
const RETIRED_CLI_SUBCOMMANDS: &[&str] = &[
    // `aicx rank -p ...` predated the unified `intents`/`search` surface.
    "rank",
    // `aicx gemini` / `aicx junie` were single-source extractors before
    // they were folded into `aicx all`/`aicx store --agent <name>`.
    "gemini", "junie",
];

/// Canonical, kebab-case list of every `Commands::*` variant in `src/main.rs`.
///
/// **Single source of truth.** When a new subcommand is added (or renamed)
/// in the binary's `Commands` enum, update this constant — drift is caught
/// by `assert_cli_subcommand_names_match_clap` in `src/main/tests.rs`,
/// which walks `Cli::command().get_subcommands()` and fails the test if
/// the two lists disagree.
///
/// Entries match the names that clap emits at runtime (so `MigrateIntentSchema`
/// → `"migrate-intent-schema"`, `DashboardServeLegacy` → `"dashboard-serve"`
/// via the explicit `#[command(name = ...)]` override, etc.).
///
/// L36 (b): used to materialize `aicx <subcommand>` self-echo patterns
/// for every variant, replacing the prior hand-curated subset.
pub const CLI_SUBCOMMAND_NAMES: &[&str] = &[
    "all",
    "claims",
    "claude",
    "codex",
    "config",
    "conversations",
    "corpus",
    "dashboard",
    "dashboard-serve",
    "doctor",
    "extract",
    "health",
    "index",
    "ingest",
    "init",
    "intents",
    "list",
    "migrate",
    "migrate-intent-schema",
    "read",
    "refs",
    "reports",
    "reports-extractor",
    "search",
    "serve",
    "sessions",
    "sources",
    "state",
    "steer",
    "store",
    "tail",
    "warmup",
    "wizard",
];

/// Build the full self-echo pattern list (stable + JSON-RPC catch-all +
/// CLI subcommand patterns + caller-supplied extras). Pure function; no
/// I/O, no globals — keeps the public surface easy to unit-test.
///
/// L36 (b)+(c)+(d): consolidates every axis of self-echo detection
/// (MCP tools, dashboard HTTP, JSON-RPC envelopes, CLI subcommands,
/// operator config extras) into one materialized list.
fn build_self_echo_patterns(extra: &[String]) -> Vec<String> {
    let mut patterns: Vec<String> = Vec::with_capacity(
        STABLE_SELF_ECHO_PATTERNS.len()
            + CLI_SUBCOMMAND_NAMES.len()
            + RETIRED_CLI_SUBCOMMANDS.len()
            + extra.len()
            + 1,
    );
    for pat in STABLE_SELF_ECHO_PATTERNS {
        patterns.push((*pat).to_string());
    }
    patterns.push(MCP_JSONRPC_CATCH_ALL.to_string());
    for name in CLI_SUBCOMMAND_NAMES.iter().chain(RETIRED_CLI_SUBCOMMANDS) {
        patterns.push(format!("aicx {name}"));
    }
    for pat in extra {
        let trimmed = pat.trim();
        if !trimmed.is_empty() {
            patterns.push(trimmed.to_string());
        }
    }
    patterns
}

/// Process-cached lowercase pattern list. Hardcoded patterns plus
/// `[extraction].extra_self_echo_patterns` from `~/.aicx/config.toml`
/// (L36 (d)). Initialised on first call to [`is_self_echo`].
static SELF_ECHO_PATTERNS_CACHE: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Returns the active self-echo pattern list, lower-cased for
/// case-insensitive matching. First call reads config; subsequent calls
/// return the cached vec.
fn active_self_echo_patterns() -> &'static [String] {
    SELF_ECHO_PATTERNS_CACHE.get_or_init(|| {
        let extras = load_extra_self_echo_patterns_from_config();
        build_self_echo_patterns(&extras)
            .into_iter()
            .map(|p| p.to_lowercase())
            .collect()
    })
}

/// Read `[extraction].extra_self_echo_patterns` from
/// `$AICX_HOME/config.toml` (or `~/.aicx/config.toml`). Mirrors the
/// AICX_HOME resolution used by `ProjectHashRegistry::load_default` so
/// the parser crate stays self-contained (no embedder-crate coupling).
///
/// Returns an empty vec on any failure — missing file, parse error, or
/// schema mismatch. Treating "no config" and "broken config" identically
/// is intentional: a typo in the operator's TOML should never disable
/// the hardcoded patterns that protect extraction from feedback loops.
///
/// L36 (d).
fn load_extra_self_echo_patterns_from_config() -> Vec<String> {
    #[derive(serde::Deserialize, Default)]
    struct ExtractionFile {
        #[serde(default)]
        extraction: Option<ExtractionSection>,
    }
    #[derive(serde::Deserialize, Default)]
    struct ExtractionSection {
        #[serde(default)]
        extra_self_echo_patterns: Vec<String>,
    }

    let base = std::env::var_os("AICX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".aicx")));
    let Some(base) = base else {
        return Vec::new();
    };
    let path = base.join("config.toml");
    let raw = match read_to_string_validated(&path) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };
    let parsed: ExtractionFile = match toml::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(_) => return Vec::new(),
    };
    parsed
        .extraction
        .map(|section| section.extra_self_echo_patterns)
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

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
        if active_self_echo_patterns()
            .iter()
            .any(|pat| lower.contains(pat))
        {
            echo_lines += 1;
        }
    }

    // Message is self-echo only if a strict majority of lines match.
    echo_lines > 0 && echo_lines * 2 > lines.len()
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
        assert!(is_self_echo("aicx all -H 24 --emit none"));
        assert!(is_self_echo("aicx store -H 24 --full-rescan"));
        assert!(is_self_echo("aicx store --hours 24"));
        assert!(is_self_echo("aicx rank -p ai-contexters -H 72 --strict"));
        assert!(is_self_echo(
            "aicx dashboard --generate-html -p ai-contexters -H 24"
        ));
        assert!(is_self_echo(
            "aicx reports --repo ai-contexters --workflow marbles"
        ));
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

    #[test]
    fn test_self_echo_exactly_half_is_not_majority() {
        let msg = "aicx all -H 24 --emit none\n\
                   Decision: preserve real operator signal\n\
                   aicx store -H 24 --full-rescan\n\
                   Root cause: threshold was too wide";
        assert!(!is_self_echo(msg));
    }

    #[test]
    fn test_self_echo_just_above_half_is_echo() {
        let msg = "aicx all -H 24 --emit none\n\
                   Decision: preserve real operator signal\n\
                   aicx store -H 24 --full-rescan\n\
                   Root cause: threshold was too wide\n\
                   aicx refs -H 24";
        assert!(is_self_echo(msg));
    }

    #[test]
    fn test_self_echo_just_below_half_is_not_echo() {
        let msg = "aicx all -H 24 --emit none\n\
                   Decision: preserve real operator signal\n\
                   aicx store -H 24 --full-rescan\n\
                   Root cause: threshold was too wide\n\
                   Follow-up: add focused coverage";
        assert!(!is_self_echo(msg));
    }

    // -----------------------------------------------------------------------
    // L36 (b): CLI subcommand coverage — every Commands::* variant in the
    // binary surfaces as an `aicx <subcommand>` pattern. Previously only a
    // handful of subcommands (all/claude/codex/store/rank/refs/dashboard/...)
    // were hardcoded; common operator invocations like `aicx state --info`,
    // `aicx doctor`, `aicx index --dry-run`, etc. leaked through self-echo
    // and re-contaminated extractions. These tests pin the regression so
    // that whenever a new subcommand lands in `Commands`, the matching
    // pattern is added to `CLI_SUBCOMMAND_NAMES`.
    // -----------------------------------------------------------------------

    #[test]
    fn test_state_subcommand_is_echo() {
        // `aicx state --info` is a routine operator probe; it must not leak
        // into chunked extractions as substantive content.
        let msg = "aicx state --info\n\
                   aicx state --reset\n\
                   aicx state -p vetcoders/Vista";
        assert!(
            is_self_echo(msg),
            "`aicx state` invocations should be classified as self-echo"
        );
    }

    #[test]
    fn test_doctor_subcommand_is_echo() {
        let msg = "aicx doctor --oracle\n\
                   aicx doctor --rebuild-steer-index\n\
                   aicx doctor --check-dedup";
        assert!(
            is_self_echo(msg),
            "`aicx doctor` invocations should be classified as self-echo"
        );
    }

    // -----------------------------------------------------------------------
    // L36 (d): config-driven `[extraction].extra_self_echo_patterns`.
    //
    // The pure builder (`build_self_echo_patterns`) is tested here because
    // the cached top-level helper reads `~/.aicx/config.toml` via a process
    // OnceLock that can't be safely re-initialised mid-test. The integration
    // path is covered indirectly: the loader is a thin TOML read whose
    // contract is "missing file or parse error → empty vec" (also asserted
    // below by `build_self_echo_patterns` with an empty extras slice).
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_self_echo_patterns_includes_config_extras() {
        let extras = vec![
            "custom-tool-name".to_string(),
            "  /api/internal/foo  ".to_string(), // exercises trim
            "".to_string(),                      // empty entries silently dropped
        ];
        let patterns = build_self_echo_patterns(&extras);

        assert!(
            patterns.iter().any(|p| p == "custom-tool-name"),
            "non-empty extra pattern must appear verbatim in the materialized list"
        );
        assert!(
            patterns.iter().any(|p| p == "/api/internal/foo"),
            "extra patterns must be trimmed of surrounding whitespace"
        );
        assert!(
            !patterns.iter().any(|p| p.is_empty()),
            "empty extras must be filtered out"
        );

        // Baseline coverage: hardcoded + CLI + retired + catch-all all still present.
        assert!(patterns.iter().any(|p| p == "aicx_search"));
        assert!(patterns.iter().any(|p| p == "aicx doctor"));
        assert!(patterns.iter().any(|p| p == "aicx rank"));
        assert!(patterns.iter().any(|p| p == MCP_JSONRPC_CATCH_ALL));
    }

    #[test]
    fn test_build_self_echo_patterns_empty_extras_matches_baseline() {
        // L36 (d) safety property: a missing or empty `[extraction]`
        // section must not subtract hardcoded patterns.
        let baseline = build_self_echo_patterns(&[]);
        assert!(
            baseline.iter().any(|p| p == "aicx_search"),
            "MCP tool names must survive an empty extras list"
        );
        assert!(
            baseline.iter().any(|p| p == "aicx doctor"),
            "CLI subcommand patterns must survive an empty extras list"
        );
    }

    #[test]
    fn test_load_extra_self_echo_patterns_from_config_reads_extraction_section() {
        use std::fs;
        use std::sync::atomic::{AtomicUsize, Ordering};

        static UNIQ: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aicx-extraction-config-{}-{}",
            std::process::id(),
            UNIQ.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("config.toml"),
            r#"
[extraction]
extra_self_echo_patterns = [
    "operator-marker-alpha",
    "  /api/operator/beta  ",
    "",
]
"#,
        )
        .unwrap();

        // Drive the loader directly via $AICX_HOME so we don't touch the
        // OnceLock-cached top-level helper. Set+restore the env var to keep
        // other tests deterministic.
        let prev = std::env::var_os("AICX_HOME");
        // SAFETY: tests that touch the same env var must be serialized by
        // the test runner; aicx test suite already documents this as a
        // single-threaded constraint. See `crates/aicx-parser/tests/`.
        // Marked unsafe because std::env::set_var is unsafe in Rust 2024.
        unsafe {
            std::env::set_var("AICX_HOME", &dir);
        }
        let loaded = load_extra_self_echo_patterns_from_config();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("AICX_HOME", v),
                None => std::env::remove_var("AICX_HOME"),
            }
        }

        assert!(
            loaded.iter().any(|p| p == "operator-marker-alpha"),
            "loader must surface verbatim operator-supplied pattern; got {loaded:?}"
        );
        assert!(
            loaded.iter().any(|p| p == "/api/operator/beta"),
            "loader must trim whitespace; got {loaded:?}"
        );
        assert!(
            !loaded.iter().any(|p| p.is_empty()),
            "loader must drop empty entries; got {loaded:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_load_extra_self_echo_patterns_returns_empty_when_no_config() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static UNIQ: AtomicUsize = AtomicUsize::new(0);
        let nonexistent = std::env::temp_dir().join(format!(
            "aicx-extraction-config-nope-{}-{}",
            std::process::id(),
            UNIQ.fetch_add(1, Ordering::SeqCst)
        ));
        // Do NOT create the directory — loader must tolerate the absent path.

        let prev = std::env::var_os("AICX_HOME");
        unsafe {
            std::env::set_var("AICX_HOME", &nonexistent);
        }
        let loaded = load_extra_self_echo_patterns_from_config();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("AICX_HOME", v),
                None => std::env::remove_var("AICX_HOME"),
            }
        }

        assert!(
            loaded.is_empty(),
            "missing config must yield empty extras, not panic; got {loaded:?}"
        );
    }

    #[test]
    fn test_mcp_jsonrpc_catch_all_matches_unknown_method() {
        // L36 (c): previously only `"method":"tools/call"`,
        // `"method":"tools/list"`, and `"method":"initialize"` matched.
        // Any other JSON-RPC envelope (notifications, future methods,
        // resource/prompt endpoints) leaked through. The generic
        // `"jsonrpc":"2.0"` catch-all subsumes the entire surface so new
        // MCP method names are filtered without recompile.
        assert!(
            is_self_echo(
                r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}"#
            ),
            "unknown JSON-RPC method should be caught by the generic catch-all"
        );
        assert!(
            is_self_echo(r#"{"jsonrpc":"2.0","method":"resources/list","params":{}}"#),
            "resources/list JSON-RPC must be filtered as self-echo"
        );
        assert!(
            is_self_echo(
                r#"{"jsonrpc":"2.0","id":42,"result":{"contents":[{"uri":"aicx://chunk/abc"}]}}"#
            ),
            "JSON-RPC result envelope (no method name at all) must still classify as self-echo"
        );
    }

    #[test]
    fn test_corpus_index_warmup_wizard_config_subcommands_are_echo() {
        // Aggregate test — every subcommand below was historically missing
        // from SELF_ECHO_PATTERNS. Auto-derivation from `Commands::*` closes
        // the gap.
        for line in [
            "aicx corpus --audit",
            "aicx index --dry-run",
            "aicx warmup",
            "aicx wizard",
            "aicx config show",
            "aicx steer --run-id abc",
            "aicx tail --follow",
            "aicx ingest --source loct-context-pack",
            "aicx migrate --dry-run",
            "aicx sources audit",
            "aicx serve --transport stdio",
            "aicx extract --session abc --agent claude",
            "aicx read /some/path",
            "aicx search hello",
            "aicx refs -H 24",
            "aicx reports --workflow marbles",
            "aicx intents -H 720",
            "aicx conversations --out-dir /tmp",
            "aicx health",
            "aicx list",
            "aicx claude -H 24",
            "aicx codex -H 24",
            "aicx all -H 24",
            "aicx store -H 24",
        ] {
            assert!(
                is_self_echo(line),
                "`{line}` should be classified as self-echo (Commands variant missing from pattern list?)"
            );
        }
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
    fn test_normalize_query_unifies_nfc_and_nfd_inputs() {
        // D-11: same word stored in NFC ("ó" composed) vs NFD ("o" + combining
        // acute U+0301) must normalize to the same key. Without NFC pre-pass,
        // NFD lookups would not survive the diacritic mapping table.
        use unicode_normalization::UnicodeNormalization;
        let composed = "źródło";
        let decomposed: String = composed.nfd().collect();
        assert_ne!(
            composed, decomposed,
            "test pre-condition: NFC and NFD forms must differ at the byte level"
        );
        assert_eq!(
            normalize_query(composed),
            normalize_query(&decomposed),
            "NFC pre-pass must collapse pre-composed and decomposed diacritics"
        );
        assert_eq!(normalize_query(&decomposed), "zrodlo");
    }

    #[test]
    fn test_safe_project_name_rejects_bad() {
        assert!(safe_project_name("../etc").is_err());
        assert!(safe_project_name("foo/bar").is_err());
        assert!(safe_project_name("").is_err());
        assert!(safe_project_name("foo\0bar").is_err());
    }
}
