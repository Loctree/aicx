//! Regression: B-P3-23
//!
//! Module-prefix stderr leak `aicx::intents:` was visible to end-users when
//! the MAX_CANDIDATES cap (5000 per bucket) was reached. End-user-facing
//! diagnostics must use the public spelling `aicx intents: warning: ...`
//! (space, not `::`), matching the rest of the CLI surface.
//!
//! Triggering the cap from a test is impractical (would need 5000 chunks),
//! so this check verifies the compiled binary's string table directly.
//! Any future regression that re-introduces a `aicx::<module>:` literal in
//! a user-facing eprintln!/println! will resurface this guard.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

fn current_profile_dir() -> PathBuf {
    let test_exe = std::env::current_exe().expect("resolve current test executable");
    test_exe
        .parent()
        .and_then(std::path::Path::parent)
        .expect("resolve cargo profile dir")
        .to_path_buf()
}

fn fallback_aicx_path() -> PathBuf {
    let mut path = current_profile_dir().join("aicx");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

fn ensure_aicx_binary_exists() -> PathBuf {
    static BIN_PATH: OnceLock<PathBuf> = OnceLock::new();

    BIN_PATH
        .get_or_init(|| {
            if let Some(env_path) = std::env::var_os("CARGO_BIN_EXE_aicx").map(PathBuf::from)
                && env_path.is_file()
            {
                return env_path;
            }

            let env_path = PathBuf::from(env!("CARGO_BIN_EXE_aicx"));
            if env_path.is_file() {
                return env_path;
            }

            let fallback = fallback_aicx_path();
            if fallback.is_file() {
                return fallback;
            }

            let cargo = std::env::var_os("CARGO")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("cargo"));
            let output = Command::new(&cargo)
                .args(["build", "--locked", "--bin", "aicx"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .expect("build fallback aicx binary");
            assert!(
                output.status.success(),
                "fallback cargo build --bin aicx failed\nstatus: {}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );

            fallback
        })
        .clone()
}

/// Scan the compiled aicx binary for any `aicx::<module>:` literal that would
/// indicate a user-facing module-prefix leak. Doc-comments don't make it into
/// the binary, so this is a high-signal guard.
#[test]
fn binary_does_not_contain_module_prefix_leak() {
    let bin = ensure_aicx_binary_exists();
    let bytes = fs::read(&bin).expect("read aicx binary");

    // The exact regression: "aicx::intents:" (single trailing `:`). Avoid
    // matching debug/type paths like `aicx::intents::types::...`.
    let needle = b"aicx::intents:";
    let hit = bytes
        .windows(needle.len() + 1)
        .any(|w| &w[..needle.len()] == needle && w[needle.len()] != b':');
    assert!(
        !hit,
        "binary {} contains literal `aicx::intents:` — module-prefix \
         leak regressed. Use `aicx intents: warning: ...` instead.",
        bin.display()
    );

    // Defensive: also catch the broader pattern for any future module by
    // scanning raw bytes. A user-facing leak looks like ASCII
    // `aicx::<lowercase_word>:<space-or-printable>`, and the trailing char
    // after the colon must NOT be another `:` (which would be a normal
    // Rust path like `aicx::store::project_filter_matches`).
    let prefix = b"aicx::";
    let mut found: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i + prefix.len() < bytes.len() {
        if &bytes[i..i + prefix.len()] == prefix {
            // Walk the module word: lowercase + underscore only.
            let mut j = i + prefix.len();
            while j < bytes.len() && (bytes[j].is_ascii_lowercase() || bytes[j] == b'_') {
                j += 1;
            }
            // Need at least one module-name byte and a `:` (not `::`) follow-up.
            if j > i + prefix.len()
                && j < bytes.len()
                && bytes[j] == b':'
                && (j + 1 >= bytes.len() || bytes[j + 1] != b':')
            {
                let module = std::str::from_utf8(&bytes[i + prefix.len()..j])
                    .unwrap_or("<non-utf8>")
                    .to_string();
                let candidate = format!("aicx::{module}:");
                if !found.contains(&candidate) {
                    found.push(candidate);
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    assert!(
        found.is_empty(),
        "binary {} contains module-prefix leak literals: {:?}. \
         Replace with public-form `aicx <module>: warning: ...`.",
        bin.display(),
        found
    );
}
