use std::process::Command;

/// Run `git <args>` and return trimmed stdout, or `None` on any failure
/// (git missing, not a repo, non-zero exit, empty output).
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Build-time identity stamp (pattern lifted from loctree-mcp/build.rs):
/// two binaries at the same crate version but different commits are otherwise
/// indistinguishable, so a fleet host running a STALE binary reads the same
/// `--version` as a fresh one and nobody notices. Stamping semver build
/// metadata (`0.11.0+g<sha>` / `+g<sha>.dirty`) makes the gap loud — proven
/// need: the 2026-07-16 fleet rollout compared binaries by shasum because
/// `--version` said `0.11.0` everywhere. Best-effort: without git the stamp
/// degrades to the plain crate version and the build never fails.
fn identity_stamp() {
    println!("cargo:rerun-if-env-changed=AICX_GIT_COMMIT");
    println!("cargo:rerun-if-env-changed=AICX_BUILD_VERSION");
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        if let Some(head_ref) = git(&["rev-parse", "--symbolic-full-name", "HEAD"]) {
            println!("cargo:rerun-if-changed={git_dir}/{head_ref}");
        }
    }
    // Explicit override wins (release/packaging pipelines pin a known commit).
    let commit = std::env::var("AICX_GIT_COMMIT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .or_else(|| git(&["rev-parse", "--short=8", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string());
    let dirty = git(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let describe = git(&["describe", "--always", "--dirty", "--tags"]).unwrap_or_else(|| {
        if dirty && commit != "unknown" {
            format!("{commit}-dirty")
        } else {
            commit.clone()
        }
    });
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let build_version = if let Ok(pinned) = std::env::var("AICX_BUILD_VERSION")
        && !pinned.trim().is_empty()
    {
        pinned
    } else if commit == "unknown" {
        pkg_version.clone()
    } else if dirty {
        format!("{pkg_version}+g{commit}.dirty")
    } else {
        format!("{pkg_version}+g{commit}")
    };
    println!("cargo:rustc-env=AICX_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=AICX_GIT_DESCRIBE={describe}");
    println!("cargo:rustc-env=AICX_BUILD_VERSION={build_version}");
}

fn main() {
    // Always re-run when the build script itself changes.
    println!("cargo:rerun-if-changed=build.rs");
    identity_stamp();
    // Windows defaults the main-thread stack to 1 MiB. The aicx clap command
    // tree is large enough that building it during argument parsing overflows
    // that stack on startup — every binary invocation (even `aicx --version`)
    // aborts with "thread 'main' has overflowed its stack" before any command
    // runs, which fails every integration test that shells out to the binary.
    // Raise the linked stack reservation to 8 MiB to match the Unix default.
    // Scoped to the MSVC target; a no-op everywhere else.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS");
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV");
    if target_os.as_deref() == Ok("windows") && target_env.as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
