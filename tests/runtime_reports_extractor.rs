use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-runtime-reports-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, content).expect("write file");
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
                "fallback cargo build --bin aicx failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                fallback.is_file(),
                "fallback cargo build succeeded but binary missing at {}",
                fallback.display()
            );

            fallback
        })
        .clone()
}

fn run_aicx(home: &Path, args: &[&str]) -> Output {
    fs::create_dir_all(home).expect("create temp HOME");
    Command::new(ensure_aicx_binary_exists())
        .args(args)
        .env("HOME", home)
        .output()
        .expect("run aicx")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reports_extractor_builds_html_and_default_bundle_from_vibecrafted_artifacts() {
    let root = unique_test_dir("reports-extractor");
    let home = root.join("home");
    let artifacts_root = root.join("artifacts");
    let repo_root = artifacts_root.join("VetCoders").join("ai-contexters");
    let html_output = root.join("out").join("report-explorer.html");
    let bundle_output = root.join("out").join("report-explorer.bundle.json");

    write_file(
        &repo_root
            .join("2026_0412")
            .join("reports")
            .join("20260412_report-artifacts_codex.md"),
        "---\nagent: codex\nrun_id: wf-20260412-001\nprompt_id: report-artifacts\nstatus: completed\ncreated: 2026-04-12T20:11:06+02:00\nskill_code: vc-workflow\n---\n# Report Artifacts Dashboard\n## Findings\n- build standalone HTML\n",
    );
    write_file(
        &repo_root
            .join("2026_0412")
            .join("reports")
            .join("20260412_report-artifacts_codex.meta.json"),
        r#"{
  "status": "completed",
  "agent": "codex",
  "run_id": "wf-20260412-001",
  "prompt_id": "report-artifacts",
  "duration_s": 12.5,
  "skill_code": "impl"
}"#,
    );
    write_file(
        &repo_root
            .join("2026_0411")
            .join("marbles")
            .join("reports")
            .join("20260411_1316_marbles-ancestor_L1_codex.meta.json"),
        &json!({
            "status": "launching",
            "agent": "codex",
            "run_id": "marb-131611-001",
            "prompt_id": "marbles-ancestor_L1_20260411",
            "transcript": repo_root
                .join("2026_0411")
                .join("marbles")
                .join("reports")
                .join("20260411_1316_marbles-ancestor_L1_codex.transcript.log")
                .display()
                .to_string()
        })
        .to_string(),
    );
    write_file(
        &repo_root
            .join("2026_0411")
            .join("marbles")
            .join("reports")
            .join("20260411_1316_marbles-ancestor_L1_codex.transcript.log"),
        "[13:16:11] assistant: booting artifact scan\n",
    );

    let output = run_aicx(
        &home,
        &[
            "reports-extractor",
            "--artifacts-root",
            &artifacts_root.display().to_string(),
            "--org",
            "VetCoders",
            "--repo",
            "ai-contexters",
            "--date-from",
            "2026-04-11",
            "--date-to",
            "2026-04-12",
            "--output",
            &html_output.display().to_string(),
        ],
    );
    assert_success(&output);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(&html_output.display().to_string()));
    assert!(html_output.exists());
    assert!(bundle_output.exists());

    let html = fs::read_to_string(&html_output).expect("read generated html");
    assert!(html.contains("Workflow Report Explorer"));
    assert!(html.contains("Import JSON Bundle"));

    let bundle: Value =
        serde_json::from_str(&fs::read_to_string(&bundle_output).expect("read bundle"))
            .expect("parse bundle");
    assert_eq!(bundle["stats"]["total_records"].as_u64(), Some(2));
    assert_eq!(bundle["stats"]["completed_records"].as_u64(), Some(1));
    assert_eq!(bundle["stats"]["incomplete_records"].as_u64(), Some(1));
    let workflows = bundle["records"]
        .as_array()
        .expect("records array")
        .iter()
        .map(|record| {
            record["workflow"]
                .as_str()
                .expect("workflow string")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert!(
        workflows
            .iter()
            .any(|workflow| workflow == "report-artifacts")
    );
    assert!(!workflows.iter().any(|workflow| workflow == "day-root"));

    let _ = fs::remove_dir_all(&root);
}
