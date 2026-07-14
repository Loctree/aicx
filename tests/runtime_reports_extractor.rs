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
        // Windows resolves the home dir from USERPROFILE, not HOME (dirs::home_dir).
        .env("USERPROFILE", home)
        .env("AICX_ALLOW_TMP", "1")
        // Drop any operator-pinned AICX_HOME so the spawned binary
        // resolves under the test's temp HOME — see frame_kind_contract.rs
        // for the full reasoning.
        .env_remove("AICX_HOME")
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
fn reports_builds_html_and_default_bundle_from_vibecrafted_artifacts() {
    let root = unique_test_dir("reports-extractor");
    let home = root.join("home");
    let artifacts_root = root.join("artifacts");
    let repo_root = artifacts_root.join("Vetcoders").join("ai-contexters");
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
            "reports",
            "--artifacts-root",
            &artifacts_root.display().to_string(),
            "--org",
            "Vetcoders",
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

fn write_xss_fixture(repo_root: &Path) {
    write_file(
        &repo_root
            .join("2026_0413")
            .join("reports")
            .join("20260413_xss-probe_codex.md"),
        "---\nagent: codex\nrun_id: wf-20260413-xss\nprompt_id: xss-probe\nstatus: completed\ncreated: 2026-04-13T10:00:00+02:00\nskill_code: vc-workflow\ntitle: \"<script>alert(1)</script>\"\n---\n# </script><img onerror=alert(1)>\n\n- payload one: <script>alert('payload')</script>\n- payload two: </script><img onerror=alert(2)>\n- payload three: [click](javascript:alert(3))\n",
    );
    write_file(
        &repo_root
            .join("2026_0413")
            .join("reports")
            .join("20260413_xss-probe_codex.meta.json"),
        r#"{
  "status": "completed",
  "agent": "codex",
  "run_id": "wf-20260413-xss",
  "prompt_id": "xss-probe",
  "duration_s": 1.0,
  "skill_code": "impl"
}"#,
    );
}

#[test]
fn reports_escapes_xss_payloads_in_embedded_json_and_html() {
    let root = unique_test_dir("reports-xss");
    let home = root.join("home");
    let artifacts_root = root.join("artifacts");
    let repo_root = artifacts_root.join("Vetcoders").join("aicx");
    let html_output = root.join("out").join("xss.html");

    write_xss_fixture(&repo_root);

    let output = run_aicx(
        &home,
        &[
            "reports",
            "--artifacts-root",
            &artifacts_root.display().to_string(),
            "--org",
            "Vetcoders",
            "--repo",
            "aicx",
            "--date-from",
            "2026-04-13",
            "--date-to",
            "2026-04-13",
            "--output",
            &html_output.display().to_string(),
        ],
    );
    assert_success(&output);

    let html = fs::read_to_string(&html_output).expect("read xss html");
    // The embedded JSON payload must escape every `<` / `>` so the inline
    // `<script>` blob from the markdown cannot break out of the
    // `<script type="application/json">` envelope.
    assert!(
        !html.contains("<script>alert(1)</script>"),
        "raw <script> tag from fixture leaked into HTML output"
    );
    assert!(
        !html.contains("</script><img onerror=alert(1)>"),
        "raw </script> break-out from fixture leaked into HTML output"
    );
    // Escaped forms must be present in the embedded payload: every `<` becomes
    // `<` so the inline JSON cannot break out of its `<script type="application/json">`
    // envelope.
    assert!(
        html.contains("\\u003cscript\\u003e"),
        "expected escaped `<script>` token in embedded JSON payload"
    );
    assert!(
        html.contains("\\u003c/script\\u003e"),
        "expected escaped `</script>` token in embedded JSON payload"
    );
    assert!(
        html.contains("\\u003cimg onerror="),
        "expected escaped `<img onerror=` token in embedded JSON payload"
    );
    // The HTML shell title must not be raw script either (html_escape path).
    assert!(!html.contains("<title><script>"));
    // Only the legitimate envelope <script> tags are allowed in raw HTML:
    // exactly two — `<script id="rx-data" type="application/json">` and the
    // app-wrapper `<script>...</script>`. Any extra means a payload leaked.
    let raw_open = html.matches("<script").count();
    let raw_close = html.matches("</script>").count();
    assert_eq!(
        raw_open, 2,
        "expected exactly 2 raw <script tags (envelope), got {raw_open}"
    );
    assert_eq!(
        raw_close, 2,
        "expected exactly 2 raw </script> tags (envelope), got {raw_close}"
    );
    // The `javascript:` URL in markdown must also survive only inside the
    // escaped JSON payload (as `&` characters or literal escaped form),
    // never as a usable href attribute.
    assert!(!html.contains("href=\"javascript:alert"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn reports_refuses_overwrite_without_force_and_succeeds_with_force() {
    let root = unique_test_dir("reports-force");
    let home = root.join("home");
    let artifacts_root = root.join("artifacts");
    let repo_root = artifacts_root.join("Vetcoders").join("aicx");
    let html_output = root.join("out").join("force.html");
    let bundle_output = root.join("out").join("force.bundle.json");

    write_file(
        &repo_root
            .join("2026_0414")
            .join("reports")
            .join("20260414_force_codex.md"),
        "---\nagent: codex\nrun_id: wf-20260414-force\nprompt_id: force-probe\nstatus: completed\ncreated: 2026-04-14T10:00:00+02:00\nskill_code: vc-workflow\n---\n# Force overwrite probe\n",
    );

    // First run: clean slate, must succeed.
    let first = run_aicx(
        &home,
        &[
            "reports",
            "--artifacts-root",
            &artifacts_root.display().to_string(),
            "--org",
            "Vetcoders",
            "--repo",
            "aicx",
            "--date-from",
            "2026-04-14",
            "--date-to",
            "2026-04-14",
            "--output",
            &html_output.display().to_string(),
        ],
    );
    assert_success(&first);
    assert!(html_output.exists());
    assert!(bundle_output.exists());
    let first_html_contents = fs::read_to_string(&html_output).expect("read first html");

    // Mutate the file so we can prove the second run did NOT overwrite it.
    fs::write(&html_output, "SENTINEL: must not be overwritten").expect("write sentinel");

    // Second run without --force: must refuse and leave the sentinel intact.
    let second = run_aicx(
        &home,
        &[
            "reports",
            "--artifacts-root",
            &artifacts_root.display().to_string(),
            "--org",
            "Vetcoders",
            "--repo",
            "aicx",
            "--date-from",
            "2026-04-14",
            "--date-to",
            "2026-04-14",
            "--output",
            &html_output.display().to_string(),
        ],
    );
    assert!(
        !second.status.success(),
        "second run without --force should refuse to overwrite, but it succeeded"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("--force"),
        "error message should mention --force, got: {stderr}"
    );
    let preserved = fs::read_to_string(&html_output).expect("read preserved html");
    assert_eq!(preserved, "SENTINEL: must not be overwritten");

    // Third run with --force: must succeed and replace the sentinel with a real HTML payload.
    let third = run_aicx(
        &home,
        &[
            "reports",
            "--artifacts-root",
            &artifacts_root.display().to_string(),
            "--org",
            "Vetcoders",
            "--repo",
            "aicx",
            "--date-from",
            "2026-04-14",
            "--date-to",
            "2026-04-14",
            "--output",
            &html_output.display().to_string(),
            "--force",
        ],
    );
    assert_success(&third);
    let after_force = fs::read_to_string(&html_output).expect("read after-force html");
    assert_ne!(after_force, "SENTINEL: must not be overwritten");
    assert!(after_force.contains("Workflow Report Explorer"));
    // The original HTML should also be re-derivable on subsequent --force runs.
    let _ = first_html_contents;

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn reports_composite_record_key_distinguishes_artifacts_with_same_run_id() {
    let root = unique_test_dir("reports-composite-key");
    let home = root.join("home");
    let artifacts_root = root.join("artifacts");
    let repo_root = artifacts_root.join("Vetcoders").join("aicx");
    let html_output = root.join("out").join("composite.html");
    let bundle_output = root.join("out").join("composite.bundle.json");

    // Two artifacts share the SAME run_id but live at different relative paths.
    // Before the composite-key fix, `build_record_key` returned `run:{id}`
    // for both and the JS mergePayload Map collapsed them silently. After the
    // fix the keys must be distinct because relative_path is part of the key.
    write_file(
        &repo_root
            .join("2026_0415")
            .join("reports")
            .join("20260415_a_codex.md"),
        "---\nagent: codex\nrun_id: shared-run-id\nprompt_id: artifact-a\nstatus: completed\ncreated: 2026-04-15T10:00:00+02:00\nskill_code: vc-workflow\n---\n# Artifact A\n",
    );
    write_file(
        &repo_root
            .join("2026_0415")
            .join("reports")
            .join("20260415_b_codex.md"),
        "---\nagent: codex\nrun_id: shared-run-id\nprompt_id: artifact-b\nstatus: completed\ncreated: 2026-04-15T11:00:00+02:00\nskill_code: vc-workflow\n---\n# Artifact B\n",
    );

    let output = run_aicx(
        &home,
        &[
            "reports",
            "--artifacts-root",
            &artifacts_root.display().to_string(),
            "--org",
            "Vetcoders",
            "--repo",
            "aicx",
            "--date-from",
            "2026-04-15",
            "--date-to",
            "2026-04-15",
            "--output",
            &html_output.display().to_string(),
        ],
    );
    assert_success(&output);

    let bundle: Value =
        serde_json::from_str(&fs::read_to_string(&bundle_output).expect("read bundle"))
            .expect("parse bundle");
    let records = bundle["records"].as_array().expect("records array");
    assert_eq!(
        records.len(),
        2,
        "both artifacts must survive into the bundle"
    );
    let key_a = records[0]["key"].as_str().expect("key a").to_string();
    let key_b = records[1]["key"].as_str().expect("key b").to_string();
    assert_ne!(
        key_a, key_b,
        "composite keys must differ when relative_path differs"
    );
    assert!(key_a.contains("@path:"), "key should be composite: {key_a}");
    assert!(key_b.contains("@path:"), "key should be composite: {key_b}");
    assert!(key_a.starts_with("run:shared-run-id@path:"));
    assert!(key_b.starts_with("run:shared-run-id@path:"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn reports_deterministic_flag_derives_generated_at_from_record_timestamps() {
    let root = unique_test_dir("reports-deterministic");
    let home = root.join("home");
    let artifacts_root = root.join("artifacts");
    let repo_root = artifacts_root.join("Vetcoders").join("aicx");
    let html_output_a = root.join("out").join("det-a.html");
    let bundle_output_a = root.join("out").join("det-a.bundle.json");
    let html_output_b = root.join("out").join("det-b.html");
    let bundle_output_b = root.join("out").join("det-b.bundle.json");

    write_file(
        &repo_root
            .join("2026_0416")
            .join("reports")
            .join("20260416_det_codex.md"),
        "---\nagent: codex\nrun_id: wf-det-01\nprompt_id: det-probe\nstatus: completed\ncreated: 2026-04-16T12:00:00+02:00\ncompleted_at: \"2026-04-16T12:34:56+00:00\"\nskill_code: vc-workflow\n---\n# Deterministic probe\n",
    );
    write_file(
        &repo_root
            .join("2026_0416")
            .join("reports")
            .join("20260416_det_codex.meta.json"),
        r#"{
  "status": "completed",
  "agent": "codex",
  "run_id": "wf-det-01",
  "prompt_id": "det-probe",
  "completed_at": "2026-04-16T12:34:56+00:00",
  "duration_s": 1.0
}"#,
    );

    let args = |out_html: &str| -> Vec<String> {
        vec![
            "reports".to_string(),
            "--artifacts-root".to_string(),
            artifacts_root.display().to_string(),
            "--org".to_string(),
            "Vetcoders".to_string(),
            "--repo".to_string(),
            "aicx".to_string(),
            "--date-from".to_string(),
            "2026-04-16".to_string(),
            "--date-to".to_string(),
            "2026-04-16".to_string(),
            "--output".to_string(),
            out_html.to_string(),
            "--deterministic".to_string(),
        ]
    };

    let args_a = args(&html_output_a.display().to_string());
    let args_a_ref: Vec<&str> = args_a.iter().map(String::as_str).collect();
    let out_a = run_aicx(&home, &args_a_ref);
    assert_success(&out_a);
    // Ensure wall-clock advances between runs so a non-deterministic
    // generated_at would visibly differ.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let args_b = args(&html_output_b.display().to_string());
    let args_b_ref: Vec<&str> = args_b.iter().map(String::as_str).collect();
    let out_b = run_aicx(&home, &args_b_ref);
    assert_success(&out_b);

    let bundle_a: Value =
        serde_json::from_str(&fs::read_to_string(&bundle_output_a).expect("read bundle a"))
            .expect("parse bundle a");
    let bundle_b: Value =
        serde_json::from_str(&fs::read_to_string(&bundle_output_b).expect("read bundle b"))
            .expect("parse bundle b");
    let gen_a = bundle_a["generated_at"].as_str().expect("generated_at a");
    let gen_b = bundle_b["generated_at"].as_str().expect("generated_at b");
    assert_eq!(
        gen_a, gen_b,
        "deterministic mode must produce identical generated_at across runs"
    );
    // Must be RFC3339 and anchored on the fixture timestamp (2026-04-16T12:34:56Z).
    assert!(gen_a.starts_with("2026-04-16T12:34:56"));

    let _ = fs::remove_dir_all(&root);
}
