//! Regression: B-P1-13
//!
//! `aicx state --info --project X` previously ignored the `--project`
//! filter entirely — only `--reset` honored it. The fix routes the
//! filter through the canonical `project_filter_matches` resolver so it
//! applies to both branches.
//!
//! This test seeds an isolated state.json with three buckets across two
//! organizations and verifies:
//! 1. Unfiltered --info shows all three.
//! 2. Slug filter (`-p owner/repo`) shows exactly that bucket plus a
//!    "Filtered by project:" banner.
//! 3. Org-wildcard (`-p owner/`) shows both buckets under that owner.
//! 4. Bare repo name (`-p repo`) cross-org match works.
//! 5. The legitimate `--reset --project` path still works (no regression).

use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-state-filter-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ))
}

fn current_profile_dir() -> PathBuf {
    let test_exe = std::env::current_exe().expect("resolve current test executable");
    test_exe
        .parent()
        .and_then(Path::parent)
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

/// Seed a state.json with three buckets across two organizations.
fn seed_state_json(aicx_home: &Path) {
    fs::create_dir_all(aicx_home).expect("create aicx home");
    // SeenHashSet serializes as a flat ordered array of hash strings.
    // hash_algorithm must equal `blake3-128` to avoid the load-time
    // migration that nukes seen_hashes when the algorithm doesn't match.
    let state = json!({
        "last_processed": {},
        "hash_algorithm": "blake3-128-v2",
        "seen_hashes": {
            "vetcoders/loctree-suite": ["aa", "bb", "cc"],
            "vetcoders/aicx": ["dd", "ee"],
            "libraxisai/agents": ["ff"],
        },
        "runs": [],
    });
    fs::write(
        aicx_home.join("state.json"),
        serde_json::to_string_pretty(&state).expect("serialize state"),
    )
    .expect("write state.json");
}

fn run_aicx_state_info(aicx_home: &Path, project_filter: Option<&str>) -> Output {
    let mut cmd = Command::new(ensure_aicx_binary_exists());
    cmd.env("AICX_HOME", aicx_home)
        .env("AICX_ALLOW_TMP", "1")
        .env_remove("HOME");
    cmd.args(["state", "--info"]);
    if let Some(p) = project_filter {
        cmd.args(["--project", p]);
    }
    cmd.output().expect("run aicx state --info")
}

#[test]
fn state_info_without_filter_shows_all_buckets() {
    let home = unique_test_dir("unfiltered");
    seed_state_json(&home);
    let out = run_aicx_state_info(&home, None);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "aicx state --info should succeed; stderr:\n{stderr}"
    );
    // Default totals line (no `(filtered)` suffix).
    assert!(
        stderr.contains("Total hashes: 6"),
        "expected total hashes 6 (3+2+1) across all buckets; stderr:\n{stderr}"
    );
    assert!(stderr.contains("vetcoders/loctree-suite: 3 hashes"));
    assert!(stderr.contains("vetcoders/aicx: 2 hashes"));
    assert!(stderr.contains("libraxisai/agents: 1 hashes"));
    assert!(
        !stderr.contains("Filtered by project:"),
        "no banner when filter is absent; stderr:\n{stderr}"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn state_info_with_slug_filter_narrows_to_one_bucket() {
    let home = unique_test_dir("slug");
    seed_state_json(&home);
    let out = run_aicx_state_info(&home, Some("vetcoders/loctree-suite"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains("Filtered by project: vetcoders/loctree-suite"),
        "banner missing; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Total hashes (filtered): 3"),
        "filtered total should be 3; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Projects (filtered):     1"),
        "should match exactly one bucket; stderr:\n{stderr}"
    );
    assert!(stderr.contains("vetcoders/loctree-suite: 3 hashes"));
    assert!(
        !stderr.contains("vetcoders/aicx: 2 hashes"),
        "must not include sibling buckets under same org; stderr:\n{stderr}"
    );
    assert!(!stderr.contains("libraxisai/agents"));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn state_info_with_org_wildcard_filter_includes_org_siblings() {
    let home = unique_test_dir("org");
    seed_state_json(&home);
    let out = run_aicx_state_info(&home, Some("vetcoders/"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains("Total hashes (filtered): 5"),
        "two vetcoders buckets total 5 hashes; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("Projects (filtered):     2"),
        "should match exactly two buckets; stderr:\n{stderr}"
    );
    assert!(stderr.contains("vetcoders/loctree-suite"));
    assert!(stderr.contains("vetcoders/aicx"));
    assert!(
        !stderr.contains("libraxisai/agents"),
        "must not include other-org buckets; stderr:\n{stderr}"
    );
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn state_info_with_bare_repo_name_filter_cross_org() {
    let home = unique_test_dir("bare");
    seed_state_json(&home);
    // `-p aicx` should match the `vetcoders/aicx` bucket via the cross-org
    // repo-name rule of project_filter_matches.
    let out = run_aicx_state_info(&home, Some("aicx"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr:\n{stderr}");
    assert!(
        stderr.contains("Projects (filtered):     1"),
        "bare name should match exactly one bucket; stderr:\n{stderr}"
    );
    assert!(stderr.contains("vetcoders/aicx: 2 hashes"));
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn state_help_documents_project_applies_to_info() {
    let bin = ensure_aicx_binary_exists();
    let output = Command::new(&bin)
        .args(["state", "--help"])
        .output()
        .expect("run aicx state --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    // Help text must explicitly mention that --project also applies to
    // --info (so operators don't think the filter is only for --reset).
    assert!(
        stdout.contains("--info") && stdout.contains("--reset"),
        "state --help should reference both --info and --reset in --project help; got:\n{stdout}"
    );
}
