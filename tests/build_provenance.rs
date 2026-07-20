#[path = "../build/build_support.rs"]
mod build_support;

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[test]
fn version_formatter_distinguishes_clean_dirty_and_archive_builds() {
    assert_eq!(
        build_support::format_build_version("0.12.0", "deadbeef", false),
        "0.12.0+gdeadbeef"
    );
    assert_eq!(
        build_support::format_build_version("0.12.0", "deadbeef", true),
        "0.12.0+gdeadbeef.dirty"
    );
    assert_eq!(
        build_support::format_build_version("0.12.0", "unknown", true),
        "0.12.0"
    );
}

#[test]
fn compiled_identity_matches_the_checkout_used_for_this_build() {
    let build_version = env!("AICX_BUILD_VERSION");
    assert!(build_version.starts_with(env!("CARGO_PKG_VERSION")));

    if let Some(head) = git(&["rev-parse", "--short=8", "HEAD"]) {
        assert_eq!(env!("AICX_GIT_COMMIT"), head);
        assert!(build_version.contains(&format!("+g{head}")));

        let dirty = git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
        assert_eq!(env!("AICX_GIT_DIRTY") == "1", dirty);
        assert_eq!(build_version.ends_with(".dirty"), dirty);
    }
}
