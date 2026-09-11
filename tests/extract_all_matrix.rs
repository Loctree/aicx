//! `aicx extract all` — provider matrix, projection axes, and manifest contract.
//!
//! Every test here drives the real binary against an isolated `HOME` /
//! `AICX_HOME` and asserts on **content, order and counts** — never on "the
//! file exists" or "rc was 0". A test that only checks for a file would have
//! passed on the pipeline that silently dropped every shell action.
//!
//! Fixtures are small, depersonalized, and shaped like the real provider
//! records (see `tests/fixtures/parser_engine/contract/taxonomy_units.toml`
//! for the per-provider unit vocabulary they mirror).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "aicx-extract-all-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("home")).expect("create sandbox home");
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// AICX_HOME lives *inside* HOME: the write-path allowlist is anchored on
    /// the user's home directory, which is also how a real install is laid out.
    fn aicx_home(&self) -> PathBuf {
        self.home().join(".aicx")
    }

    fn extracts(&self) -> PathBuf {
        self.aicx_home().join("extracts")
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.home().join(relative);
        fs::create_dir_all(path.parent().expect("fixture has a parent"))
            .expect("create fixture dir");
        fs::write(&path, contents).expect("write fixture");
        path
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aicx"))
            .env("HOME", self.home())
            .env("AICX_HOME", self.aicx_home())
            .env("AICX_NO_MUTATION_WARN", "1")
            .args(args)
            .output()
            .expect("run aicx")
    }

    fn manifest(&self) -> serde_json::Value {
        let path = self.extracts().join("_bulk").join("manifest-latest.json");
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        serde_json::from_str(&raw).expect("manifest is valid json")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Read every markdown extract under one agent directory, newest name first.
fn extract_bodies(sandbox: &Sandbox, agent: &str) -> Vec<(String, String)> {
    let dir = sandbox.extracts().join(agent);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("md"))
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let body = fs::read_to_string(entry.path()).expect("read extract");
            (name, body)
        })
        .collect();
    out.sort();
    out
}

fn totals(manifest: &serde_json::Value) -> &serde_json::Value {
    &manifest["totals"]
}

fn count(manifest: &serde_json::Value, bucket: &str) -> u64 {
    totals(manifest)[bucket]
        .as_u64()
        .unwrap_or_else(|| panic!("manifest totals missing `{bucket}`"))
}

/// The manifest must account for every discovered source exactly once.
fn assert_totals_reconcile(manifest: &serde_json::Value) {
    let sum = count(manifest, "extracted")
        + count(manifest, "unchanged")
        + count(manifest, "empty_after_filter")
        + count(manifest, "unsupported")
        + count(manifest, "filtered_out")
        + count(manifest, "failed");
    assert_eq!(
        sum,
        count(manifest, "discovered"),
        "every discovered source must land in exactly one bucket: {}",
        serde_json::to_string_pretty(totals(manifest)).unwrap()
    );
}

// ---------------------------------------------------------------------------
// Fixtures — one per provider, shaped like the real record vocabulary
// ---------------------------------------------------------------------------

const CODEX_SESSION: &str = "019f3333-4444-7555-8666-000000000001";
const CLAUDE_SESSION: &str = "aaaaaaaa-bbbb-4ccc-8ddd-000000000002";
const REPO_CWD: &str = "/Volumes/vc-workspace/Loctree/aicx";

fn codex_fixture(sandbox: &Sandbox) -> PathBuf {
    sandbox.write(
        &format!(".codex/sessions/rollout-2026-09-10T04-00-00-{CODEX_SESSION}.jsonl"),
        &format!(
            concat!(
                r#"{{"timestamp":"2026-09-10T04:00:00Z","type":"session_meta","payload":{{"id":"{id}","cwd":"{cwd}"}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-09-10T04:00:01Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"zbuduj to"}}]}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-09-10T04:00:02Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"<user_shell_command>\n<command>cargo build --workspace</command>\n<result>ok</result>\n</user_shell_command>"}}]}}}}"#,
                "\n",
                r#"{{"timestamp":"2026-09-10T04:00:03Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Zbudowane. Proponuje `cargo test` - to propozycja w tekscie, nie wykonanie."}}]}}}}"#,
                "\n",
            ),
            id = CODEX_SESSION,
            cwd = REPO_CWD
        ),
    )
}

fn claude_fixture(sandbox: &Sandbox) -> PathBuf {
    sandbox.write(
        &format!(".claude/projects/-tmp-other/{CLAUDE_SESSION}.jsonl"),
        &format!(
            concat!(
                r#"{{"type":"user","sessionId":"{id}","uuid":"u1","timestamp":"2026-09-10T05:00:00Z","cwd":"/tmp/other","message":{{"role":"user","content":"pytanie operatora"}}}}"#,
                "\n",
                r#"{{"type":"assistant","sessionId":"{id}","uuid":"u2","timestamp":"2026-09-10T05:00:01Z","cwd":"/tmp/other","message":{{"role":"assistant","content":[{{"type":"text","text":"odpowiedz asystenta"}}]}}}}"#,
                "\n",
            ),
            id = CLAUDE_SESSION
        ),
    )
}

// ---------------------------------------------------------------------------
// 1. The command exists and reports honest, reconciling totals
// ---------------------------------------------------------------------------

#[test]
fn extract_all_writes_one_extract_per_session_and_a_reconciling_manifest() {
    let sandbox = Sandbox::new("basic");
    codex_fixture(&sandbox);
    claude_fixture(&sandbox);

    let output = sandbox.run(&["extract", "all"]);
    assert!(
        output.status.success(),
        "clean run must exit 0\nstdout: {}\nstderr: {}",
        stdout(&output),
        stderr(&output)
    );

    let manifest = sandbox.manifest();
    assert_totals_reconcile(&manifest);
    assert_eq!(count(&manifest, "discovered"), 2);
    assert_eq!(count(&manifest, "extracted"), 2);
    assert_eq!(count(&manifest, "failed"), 0);

    // Provenance the manifest must carry for every row.
    for entry in manifest["entries"].as_array().expect("entries array") {
        assert!(!entry["provider"].as_str().unwrap().is_empty());
        assert!(!entry["source_id"].as_str().unwrap().is_empty());
        assert!(!entry["source_path"].as_str().unwrap().is_empty());
        assert!(!entry["source_fingerprint"].as_str().unwrap().is_empty());
        assert!(!entry["parser_version"].as_str().unwrap().is_empty());
        assert_eq!(entry["outcome"], "extracted");
    }

    // The filters block records the cutoff and the view actually applied.
    let filters = &manifest["filters"];
    assert!(!filters["cutoff_utc"].as_str().unwrap().is_empty());
    assert!(
        !filters["projection_fingerprint"]
            .as_str()
            .unwrap()
            .is_empty()
    );

    // Content, not existence: both sessions' words are on disk.
    let codex = extract_bodies(&sandbox, "codex");
    assert_eq!(codex.len(), 1, "one extract per codex session");
    assert!(codex[0].1.contains("zbuduj to"));
    assert!(codex[0].1.contains("Zbudowane."));

    let claude = extract_bodies(&sandbox, "claude");
    assert_eq!(claude.len(), 1);
    assert!(claude[0].1.contains("pytanie operatora"));
    assert!(claude[0].1.contains("odpowiedz asystenta"));
}

#[test]
fn empty_archive_is_success_and_says_so() {
    let sandbox = Sandbox::new("empty");
    // No provider directories at all.
    let output = sandbox.run(&["extract", "all"]);
    assert!(output.status.success(), "an empty archive is not an error");
    let text = stdout(&output);
    assert!(
        text.contains("no sessions found"),
        "an empty run must say it found nothing rather than printing a bare zero: {text}"
    );
    let manifest = sandbox.manifest();
    assert_eq!(count(&manifest, "discovered"), 0);
    assert_totals_reconcile(&manifest);
}

// ---------------------------------------------------------------------------
// 2. Shell actions survive, and executions are told apart from proposals
// ---------------------------------------------------------------------------

#[test]
fn shell_action_reaches_the_extract_with_its_command_and_body() {
    let sandbox = Sandbox::new("shell");
    codex_fixture(&sandbox);

    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex"])
            .status
            .success()
    );
    let bodies = extract_bodies(&sandbox, "codex");
    let body = &bodies[0].1;

    // The command marker is rendered from the substrate, with the retained
    // result named by hash (the `--result none` default).
    assert!(
        body.contains("$ cargo build --workspace"),
        "the executed command must appear in the extract:\n{body}"
    );
    assert!(
        body.contains("sha256:"),
        "the retained result must be named by hash even when not rendered:\n{body}"
    );
}

#[test]
fn result_full_renders_the_retained_body_and_none_only_names_it() {
    let sandbox = Sandbox::new("resultbody");
    codex_fixture(&sandbox);

    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex", "--result", "full"])
            .status
            .success()
    );
    let full = extract_bodies(&sandbox, "codex")
        .into_iter()
        .find(|(name, _)| name.contains('_'))
        .expect("a narrowed view gets its own file")
        .1;
    assert!(
        full.contains("<result>") || full.contains("ok"),
        "--result full must render the retained body:\n{full}"
    );
}

#[test]
fn user_commands_selects_real_executions_and_not_prose_proposals() {
    let sandbox = Sandbox::new("usercmds");
    codex_fixture(&sandbox);

    let output = sandbox.run(&["extract", "all", "--provider", "codex", "--user-commands"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = sandbox.manifest();
    assert_eq!(count(&manifest, "extracted"), 1);

    let body = extract_bodies(&sandbox, "codex")
        .into_iter()
        .find(|(name, _)| name.contains('_'))
        .expect("--user-commands writes its own variant")
        .1;

    assert!(
        body.contains("$ cargo build --workspace"),
        "the human's execution must be selected:\n{body}"
    );
    // The assistant *proposed* `cargo test` in prose. A proposal is not an
    // execution and must not be picked up by a command filter.
    assert!(
        !body.contains("cargo test"),
        "a command mentioned in prose is not an execution:\n{body}"
    );
    assert!(
        !body.contains("zbuduj to"),
        "--user-commands is the command lane, not human speech:\n{body}"
    );
}

#[test]
fn agent_commands_does_not_claim_the_humans_shell_command() {
    let sandbox = Sandbox::new("agentcmds");
    codex_fixture(&sandbox);

    let output = sandbox.run(&["extract", "all", "--provider", "codex", "--agent-commands"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = sandbox.manifest();
    // The only command in this session was submitted by the human.
    assert_eq!(count(&manifest, "extracted"), 0);
    assert_eq!(count(&manifest, "empty_after_filter"), 1);
    assert_totals_reconcile(&manifest);
}

#[test]
fn user_and_agent_command_lanes_write_distinct_files() {
    let sandbox = Sandbox::new("cmdvariants");
    codex_fixture(&sandbox);

    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex", "--user-commands"])
            .status
            .success()
    );
    let after_user: Vec<String> = extract_bodies(&sandbox, "codex")
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex", "--agent-only"])
            .status
            .success()
    );
    let after_both: Vec<String> = extract_bodies(&sandbox, "codex")
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    assert!(
        after_both.len() > after_user.len(),
        "a second filter set must not overwrite the first: {after_user:?} then {after_both:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. Role axes
// ---------------------------------------------------------------------------

#[test]
fn agent_only_keeps_assistant_answers_and_drops_human_speech() {
    let sandbox = Sandbox::new("agentonly");
    codex_fixture(&sandbox);

    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex", "--agent-only"])
            .status
            .success()
    );
    let body = extract_bodies(&sandbox, "codex")
        .into_iter()
        .find(|(name, _)| name.contains('_'))
        .expect("--agent-only writes its own variant")
        .1;

    assert!(
        body.contains("Zbudowane."),
        "assistant answer kept:\n{body}"
    );
    assert!(!body.contains("zbuduj to"), "human speech dropped:\n{body}");
    assert!(
        !body.contains("$ cargo build"),
        "a shell action is not assistant speech:\n{body}"
    );
}

#[test]
fn user_only_keeps_human_speech_and_drops_assistant_answers() {
    let sandbox = Sandbox::new("useronly");
    codex_fixture(&sandbox);

    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex", "--user-only"])
            .status
            .success()
    );
    let body = extract_bodies(&sandbox, "codex")
        .into_iter()
        .find(|(name, _)| name.contains("_user"))
        .expect("--user-only writes its own variant")
        .1;
    assert!(body.contains("zbuduj to"));
    assert!(!body.contains("Zbudowane."));
}

// ---------------------------------------------------------------------------
// 4. Conflicting flags fail loudly instead of returning an empty result
// ---------------------------------------------------------------------------

#[test]
fn contradictory_flag_pairs_are_refused_with_a_readable_reason() {
    let sandbox = Sandbox::new("conflicts");
    codex_fixture(&sandbox);

    for (args, expected_code) in [
        (
            vec!["extract", "all", "--user-only", "--agent-commands"],
            "conflicting_role_and_command_filters",
        ),
        (
            vec!["extract", "all", "--agent-only", "--user-commands"],
            "conflicting_role_and_command_filters",
        ),
        (
            vec!["extract", "all", "--kind", "human", "--user-commands"],
            "conflicting_kind_and_command_filters",
        ),
    ] {
        let output = sandbox.run(&args);
        assert!(
            !output.status.success(),
            "{args:?} must fail rather than return an empty projection"
        );
        let text = format!("{}{}", stdout(&output), stderr(&output));
        assert!(
            text.contains(expected_code),
            "{args:?} must name the conflict `{expected_code}`, got:\n{text}"
        );
    }
}

#[test]
fn mutually_exclusive_role_flags_are_rejected_by_the_grammar() {
    let sandbox = Sandbox::new("roleconflict");
    let output = sandbox.run(&["extract", "all", "--user-only", "--agent-only"]);
    assert!(!output.status.success());
    let text = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        text.contains("cannot be used with") || text.contains("conflicting_role_filters"),
        "the grammar must reject two speaker axes at once:\n{text}"
    );
}

#[test]
fn unknown_kind_token_is_refused_and_names_the_vocabulary() {
    let sandbox = Sandbox::new("badkind");
    let output = sandbox.run(&["extract", "all", "--kind", "not_a_kind"]);
    assert!(!output.status.success());
    let text = format!("{}{}", stdout(&output), stderr(&output));
    assert!(text.contains("invalid_kind_token"), "{text}");
    assert!(
        text.contains("assistant_final"),
        "the fix must list the vocabulary:\n{text}"
    );
}

// ---------------------------------------------------------------------------
// 5. `-p` is a real filter (OR across projects, AND with the other axes)
// ---------------------------------------------------------------------------

#[test]
fn project_filter_selects_by_recorded_cwd_not_by_output_label() {
    let sandbox = Sandbox::new("projectfilter");
    codex_fixture(&sandbox); // cwd = Loctree/aicx
    claude_fixture(&sandbox); // cwd = /tmp/other

    let output = sandbox.run(&["extract", "all", "-p", "Loctree/aicx"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = sandbox.manifest();
    assert_totals_reconcile(&manifest);
    assert_eq!(
        count(&manifest, "extracted"),
        1,
        "only the session whose cwd is under Loctree/aicx may be extracted"
    );
    assert_eq!(
        count(&manifest, "empty_after_filter"),
        1,
        "the other session must be visibly filtered, not silently missing"
    );
}

#[test]
fn repeatable_project_filter_is_an_or_across_projects() {
    let sandbox = Sandbox::new("projector");
    codex_fixture(&sandbox);
    claude_fixture(&sandbox);

    let output = sandbox.run(&["extract", "all", "-p", "Loctree/aicx", "-p", "/other"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = sandbox.manifest();
    assert_eq!(
        count(&manifest, "extracted"),
        2,
        "both projects are selected when both are named"
    );
    assert_eq!(
        manifest["filters"]["projects"]
            .as_array()
            .expect("projects recorded")
            .len(),
        2
    );
}

#[test]
fn a_non_matching_project_yields_an_empty_result_not_a_failure() {
    let sandbox = Sandbox::new("projectmiss");
    codex_fixture(&sandbox);

    let output = sandbox.run(&["extract", "all", "-p", "nobody/nothing"]);
    assert!(
        output.status.success(),
        "no matches is an empty result, not an error"
    );
    let manifest = sandbox.manifest();
    assert_eq!(count(&manifest, "extracted"), 0);
    assert_eq!(count(&manifest, "empty_after_filter"), 1);
}

#[test]
fn project_and_role_filters_compose_with_and() {
    let sandbox = Sandbox::new("projectand");
    codex_fixture(&sandbox);

    // Right project, but the assistant lane — the human line must be gone.
    assert!(
        sandbox
            .run(&["extract", "all", "-p", "Loctree/aicx", "--agent-only"])
            .status
            .success()
    );
    let body = extract_bodies(&sandbox, "codex")
        .into_iter()
        .find(|(name, _)| name.contains('_'))
        .expect("narrowed variant written")
        .1;
    assert!(body.contains("Zbudowane."));
    assert!(!body.contains("zbuduj to"));

    // Wrong project AND the assistant lane — nothing at all.
    let sandbox2 = Sandbox::new("projectand2");
    codex_fixture(&sandbox2);
    assert!(
        sandbox2
            .run(&["extract", "all", "-p", "nobody/nothing", "--agent-only"])
            .status
            .success()
    );
    assert_eq!(count(&sandbox2.manifest(), "extracted"), 0);
}

// ---------------------------------------------------------------------------
// 6. `-H` filters event time against one cutoff
// ---------------------------------------------------------------------------

#[test]
fn hours_window_filters_event_timestamps_not_file_mtimes() {
    let sandbox = Sandbox::new("hours");
    let path = codex_fixture(&sandbox);
    // The file is brand new; its events are from 2026-09-10T04:00Z. A
    // mtime-based filter would keep it, an event-time filter must not.
    let fresh = fs::metadata(&path).expect("fixture metadata");
    assert!(
        fresh
            .modified()
            .expect("mtime")
            .elapsed()
            .expect("clock")
            .as_secs()
            < 120,
        "fixture must be freshly written for this test to mean anything"
    );

    let output = sandbox.run(&["extract", "all", "--provider", "codex", "-H", "1"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = sandbox.manifest();
    assert_eq!(manifest["filters"]["hours"], 1);
    assert_eq!(
        count(&manifest, "empty_after_filter"),
        1,
        "events older than the window are excluded even though the file is fresh"
    );
    assert_eq!(count(&manifest, "extracted"), 0);
}

#[test]
fn an_old_source_is_skipped_by_proof_and_says_which_proof() {
    let sandbox = Sandbox::new("windowproof");
    let path = codex_fixture(&sandbox);
    // Backdate the file: its last write is far outside a 1-hour window, which
    // proves it cannot hold an in-window event.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 60 * 24 * 30);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(old))
        .expect("backdate fixture");

    let output = sandbox.run(&["extract", "all", "--provider", "codex", "-H", "1"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = sandbox.manifest();
    assert_eq!(count(&manifest, "empty_after_filter"), 1);
    let row = &manifest["entries"].as_array().unwrap()[0];
    assert!(
        row["reason"]
            .as_str()
            .unwrap()
            .contains("cannot hold an in-window event"),
        "the skip must state its proof rather than looking like a parse result: {row}"
    );
}

#[test]
fn a_recently_written_source_is_still_opened_even_with_old_events() {
    let sandbox = Sandbox::new("freshcopy");
    // Fresh mtime, events from hours ago: the deduction must NOT fire, because
    // a freshly copied archive of old events has a fresh mtime. The source is
    // opened and the *events* decide.
    codex_fixture(&sandbox);
    let output = sandbox.run(&["extract", "all", "--provider", "codex", "-H", "1"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = sandbox.manifest();
    let row = &manifest["entries"].as_array().unwrap()[0];
    assert_eq!(row["outcome"], "empty_after_filter");
    assert!(
        row["reason"].as_str().unwrap().contains("parsed"),
        "a fresh file must be parsed, not skipped by mtime: {row}"
    );
}

#[test]
fn h_zero_is_explicit_unbounded_not_a_silent_default() {
    let sandbox = Sandbox::new("hzero");
    codex_fixture(&sandbox);

    let output = sandbox.run(&["extract", "all", "--provider", "codex", "-H", "0"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let manifest = sandbox.manifest();
    assert!(
        manifest["filters"]["hours"].is_null(),
        "-H0 records an unbounded window, not `0 hours`"
    );
    assert_eq!(count(&manifest, "extracted"), 1);
}

#[test]
fn one_cutoff_is_shared_by_every_session_in_a_run() {
    let sandbox = Sandbox::new("cutoff");
    codex_fixture(&sandbox);
    claude_fixture(&sandbox);

    assert!(sandbox.run(&["extract", "all"]).status.success());
    let manifest = sandbox.manifest();
    let cutoff = manifest["filters"]["cutoff_utc"]
        .as_str()
        .expect("cutoff recorded");
    // A single instant for the whole run, recorded once — not one clock read
    // per session.
    assert!(
        chrono_parse_ok(cutoff),
        "cutoff must be a parseable RFC3339 instant, got `{cutoff}`"
    );
    assert_eq!(manifest["generated_at"].as_str().unwrap(), cutoff);
}

fn chrono_parse_ok(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

// ---------------------------------------------------------------------------
// 7. Incrementality: idempotent rerun, and parser/filter changes invalidate
// ---------------------------------------------------------------------------

#[test]
fn second_run_is_idempotent_and_reports_unchanged() {
    let sandbox = Sandbox::new("idempotent");
    codex_fixture(&sandbox);
    claude_fixture(&sandbox);

    assert!(sandbox.run(&["extract", "all"]).status.success());
    let first = extract_bodies(&sandbox, "codex");

    assert!(sandbox.run(&["extract", "all"]).status.success());
    let manifest = sandbox.manifest();
    assert_eq!(
        count(&manifest, "unchanged"),
        2,
        "a rerun re-materializes nothing"
    );
    assert_eq!(count(&manifest, "extracted"), 0);
    assert_totals_reconcile(&manifest);

    let second = extract_bodies(&sandbox, "codex");
    assert_eq!(
        first, second,
        "an idempotent rerun must leave byte-identical extracts"
    );
}

#[test]
fn a_changed_source_is_re_extracted() {
    let sandbox = Sandbox::new("changed");
    codex_fixture(&sandbox);
    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex"])
            .status
            .success()
    );
    assert_eq!(count(&sandbox.manifest(), "extracted"), 1);

    // Append one more turn: size and mtime both move.
    let path = sandbox.home().join(format!(
        ".codex/sessions/rollout-2026-09-10T04-00-00-{CODEX_SESSION}.jsonl"
    ));
    let mut body = fs::read_to_string(&path).expect("read fixture");
    body.push_str(&format!(
        "{}\n",
        format_args!(
            r#"{{"timestamp":"2026-09-10T04:00:04Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"jeszcze jedno"}}]}}}}"#
        )
    ));
    fs::write(&path, body).expect("append to fixture");

    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex"])
            .status
            .success()
    );
    let manifest = sandbox.manifest();
    assert_eq!(
        count(&manifest, "extracted"),
        1,
        "a changed source must be re-extracted, not reported unchanged"
    );
    assert!(
        extract_bodies(&sandbox, "codex")[0]
            .1
            .contains("jeszcze jedno")
    );
}

#[test]
fn rebuild_re_materializes_even_when_nothing_changed() {
    let sandbox = Sandbox::new("rebuild");
    codex_fixture(&sandbox);
    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex"])
            .status
            .success()
    );
    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex"])
            .status
            .success()
    );
    assert_eq!(count(&sandbox.manifest(), "unchanged"), 1);

    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex", "--rebuild"])
            .status
            .success()
    );
    let manifest = sandbox.manifest();
    assert_eq!(
        count(&manifest, "extracted"),
        1,
        "--rebuild ignores the state"
    );
    assert_eq!(count(&manifest, "unchanged"), 0);
}

#[test]
fn a_parser_version_change_invalidates_the_incremental_state() {
    let sandbox = Sandbox::new("parserversion");
    codex_fixture(&sandbox);
    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex"])
            .status
            .success()
    );

    // Rewrite the recorded parser version: the next run must not trust an
    // extract produced by a different parser.
    let state_path = sandbox.extracts().join("_bulk").join("state.json");
    let raw = fs::read_to_string(&state_path).expect("read state");
    let mut state: serde_json::Value = serde_json::from_str(&raw).expect("state json");
    for value in state["entries"]
        .as_object_mut()
        .expect("state entries")
        .values_mut()
    {
        value["parser_version"] = serde_json::Value::String("stale-parser".to_owned());
    }
    fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap()).expect("write state");

    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex"])
            .status
            .success()
    );
    assert_eq!(
        count(&sandbox.manifest(), "extracted"),
        1,
        "a parser-version change must re-materialize the projection"
    );
}

#[test]
fn a_deleted_extract_is_re_materialized() {
    let sandbox = Sandbox::new("deleted");
    codex_fixture(&sandbox);
    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex"])
            .status
            .success()
    );

    let target = sandbox.extracts().join("codex");
    for entry in fs::read_dir(&target).expect("read extracts").flatten() {
        fs::remove_file(entry.path()).expect("remove extract");
    }

    assert!(
        sandbox
            .run(&["extract", "all", "--provider", "codex"])
            .status
            .success()
    );
    assert_eq!(
        count(&sandbox.manifest(), "extracted"),
        1,
        "state must not claim `unchanged` for a file that is gone"
    );
}

// ---------------------------------------------------------------------------
// 8. Damaged and in-flight sources: partial, never a silent success
// ---------------------------------------------------------------------------

#[test]
fn a_broken_source_makes_the_run_partial_with_a_distinct_exit_code() {
    let sandbox = Sandbox::new("partial");
    codex_fixture(&sandbox);
    // A file the catalog will offer but the adapter cannot claim.
    sandbox.write(
        ".codex/sessions/rollout-2026-09-10T06-00-00-019f9999-0000-7000-8000-000000000009.jsonl",
        "{not json at all\n",
    );

    let output = sandbox.run(&["extract", "all", "--provider", "codex"]);
    let manifest = sandbox.manifest();
    assert_totals_reconcile(&manifest);

    if count(&manifest, "failed") > 0 {
        assert!(
            !output.status.success(),
            "a partial run must not report overall success"
        );
        assert_eq!(
            output.status.code(),
            Some(3),
            "partial runs use a dedicated exit code"
        );
        assert!(
            stderr(&output).contains("PARTIAL"),
            "the partial state must be visible on stderr:\n{}",
            stderr(&output)
        );
        // Every failure carries a reason and a way to reproduce it alone.
        for entry in manifest["entries"].as_array().unwrap() {
            if entry["outcome"] == "failed" {
                assert!(!entry["reason"].as_str().unwrap().is_empty());
                assert!(entry["recover"].as_str().unwrap().contains("aicx extract"));
            }
        }
    } else {
        // The catalog rejected the malformed file before the adapter saw it;
        // then the healthy source must still have been extracted.
        assert!(output.status.success());
        assert_eq!(count(&manifest, "extracted"), 1);
    }
}

#[test]
fn a_stray_file_in_the_session_directory_is_unsupported_not_failed() {
    let sandbox = Sandbox::new("unsupported");
    codex_fixture(&sandbox);
    // Shaped like the real archive's strays: gemini writes a `logs.json` next
    // to its `chats/` whose records *look* like a conversation — sessionId,
    // messageId, type, message, timestamp — and which no adapter claims. On a
    // real gemini tree these strays were 32 of 397 discovered sources.
    //
    // Two contracts meet here. Discovery knows the gemini layout: a `.json`
    // that is not under `chats/` is never a candidate, so the real stray does
    // not reach the manifest at all. And the `unsupported` bucket still
    // exists for what discovery cannot rule out by path — a file under
    // `chats/` that no adapter claims — because reporting that as a failed
    // session would bury the ones that genuinely broke.
    let stray_records = concat!(
        r#"[{"sessionId":"11111111-2222-4333-8444-555555555555","messageId":1,"#,
        r#""type":"user","message":"zapis do logu, nie tura rozmowy","#,
        r#""timestamp":"2026-09-10T08:00:00.000Z"}]"#,
    );
    sandbox.write(".gemini/tmp/some-project/logs.json", stray_records);
    sandbox.write(".gemini/tmp/some-project/chats/notes.json", stray_records);

    let output = sandbox.run(&[
        "extract",
        "all",
        "--provider",
        "codex",
        "--provider",
        "gemini",
    ]);
    let manifest = sandbox.manifest();
    assert_totals_reconcile(&manifest);

    assert_eq!(
        count(&manifest, "discovered"),
        2,
        "the stray next to chats/ is not a candidate at all:\n{}",
        serde_json::to_string_pretty(totals(&manifest)).unwrap()
    );
    assert!(
        manifest["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .all(|entry| !entry["source_path"]
                .as_str()
                .unwrap_or_default()
                .ends_with("some-project/logs.json")),
        "a log next to chats/ must never reach the manifest"
    );
    assert_eq!(
        count(&manifest, "unsupported"),
        1,
        "the stray under chats/ belongs in its own bucket:\n{}",
        serde_json::to_string_pretty(totals(&manifest)).unwrap()
    );
    assert_eq!(
        count(&manifest, "failed"),
        0,
        "nothing broke: no adapter ever claimed the stray"
    );
    assert_eq!(
        count(&manifest, "extracted"),
        1,
        "the healthy source in the same run must still land"
    );
    assert!(
        output.status.success(),
        "a run whose only anomaly is an unclaimed file is not a partial failure; stderr:\n{}",
        stderr(&output)
    );

    // The verdict must cite the adapter's ledger, not the file name.
    let stray = manifest["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["outcome"] == "unsupported")
        .expect("the stray is in the manifest");
    assert!(
        stray["source_path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("some-project/chats/notes.json")
    );
    let reason = stray["reason"]
        .as_str()
        .expect("unsupported carries a reason");
    assert!(
        reason.contains("claimed by the adapter"),
        "reason must name the evidence: {reason}"
    );
    assert!(
        reason.contains("raw unit(s)"),
        "reason must report what was actually read: {reason}"
    );

    // Visible without --json, and bounded: a rollup, not a path dump.
    let body = stdout(&output);
    assert!(
        body.contains("unsupported\tgemini\t1 source(s)"),
        "the summary must name the bucket:\n{body}"
    );
    assert!(
        !body.contains("some-project"),
        "the rollup must not dump stray paths:\n{body}"
    );
}

#[test]
fn a_trailing_partial_line_does_not_lose_the_completed_records() {
    let sandbox = Sandbox::new("inflight");
    let path = sandbox.home().join(format!(
        ".codex/sessions/rollout-2026-09-10T04-00-00-{CODEX_SESSION}.jsonl"
    ));
    codex_fixture(&sandbox);
    // Simulate a live append: the last line is half-written.
    let mut body = fs::read_to_string(&path).expect("read fixture");
    body.push_str(r#"{"timestamp":"2026-09-10T04:00:05Z","type":"response_it"#);
    fs::write(&path, body).expect("write partial line");

    let output = sandbox.run(&["extract", "all", "--provider", "codex"]);
    let manifest = sandbox.manifest();
    assert_totals_reconcile(&manifest);
    // Whatever the verdict, it must be explicit — and if the source was
    // claimed, the complete records ahead of the torn line must survive.
    if count(&manifest, "extracted") > 0 {
        let body = &extract_bodies(&sandbox, "codex")[0].1;
        assert!(
            body.contains("zbuduj to"),
            "records completed before the torn line must survive:\n{body}"
        );
    } else {
        assert!(
            count(&manifest, "failed") > 0,
            "a source that produced nothing must be reported as failed, not silently dropped"
        );
        assert!(!output.status.success());
    }
    // The source file itself is never modified by a read pass.
    let after = fs::read_to_string(&path).expect("source still readable");
    assert!(after.ends_with(r#"{"timestamp":"2026-09-10T04:00:05Z","type":"response_it"#));
}

// ---------------------------------------------------------------------------
// 9. Output discipline
// ---------------------------------------------------------------------------

#[test]
fn dry_run_writes_nothing_but_still_reports_the_plan() {
    let sandbox = Sandbox::new("dryrun");
    codex_fixture(&sandbox);

    let output = sandbox.run(&["extract", "all", "--provider", "codex", "--dry-run"]);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("DRY RUN"));
    assert!(
        !sandbox.extracts().join("codex").exists(),
        "a dry run must not materialize extracts"
    );
    assert!(
        !sandbox.extracts().join("_bulk").join("state.json").exists(),
        "a dry run must not persist incremental state"
    );
}

#[test]
fn json_output_is_the_manifest_and_diagnostics_stay_on_stderr() {
    let sandbox = Sandbox::new("json");
    codex_fixture(&sandbox);

    let output = sandbox.run(&["extract", "all", "--provider", "codex", "--json"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("stdout is exactly one JSON document");
    assert_eq!(parsed["schema"], "aicx.extract.all.manifest.v1");
    assert!(parsed["entries"].as_array().is_some());
    // Discovery chatter belongs on stderr so stdout stays machine-readable.
    assert!(stderr(&output).contains("source(s) discovered"));
}

#[test]
fn bulk_output_stays_in_the_canonical_place_and_never_dumps_sessions_to_stdout() {
    let sandbox = Sandbox::new("canonical");
    codex_fixture(&sandbox);

    let output = sandbox.run(&["extract", "all", "--provider", "codex"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(
        !text.contains("zbuduj to"),
        "stdout is a summary; private session bodies belong in files:\n{text}"
    );
    assert!(sandbox.extracts().join("codex").is_dir());
    assert!(
        sandbox
            .extracts()
            .join("_bulk")
            .join("manifest-latest.json")
            .is_file()
    );
}

#[test]
fn provider_selection_is_validated() {
    let sandbox = Sandbox::new("agentsel");
    let output = sandbox.run(&["extract", "all", "--provider", "not-a-provider"]);
    assert!(!output.status.success());
    let text = format!("{}{}", stdout(&output), stderr(&output));
    assert!(text.contains("unknown_provider"), "{text}");
}

#[test]
fn help_lists_all_alongside_the_agent_subcommands() {
    let sandbox = Sandbox::new("help");
    let output = sandbox.run(&["extract", "--help"]);
    assert!(output.status.success());
    let text = stdout(&output);
    for expected in ["all", "codex", "claude", "gemini", "grok", "junie"] {
        assert!(
            text.contains(expected),
            "`extract --help` must list `{expected}`:\n{text}"
        );
    }
    // The grammar summary at the top must not omit a target it accepts.
    assert!(
        text.contains("aicx extract all"),
        "the grammar line must show the bulk form:\n{text}"
    );
}

#[test]
fn sources_are_never_modified_by_an_extract_pass() {
    let sandbox = Sandbox::new("readonly");
    let codex = codex_fixture(&sandbox);
    let claude = claude_fixture(&sandbox);
    let before: Vec<(PathBuf, String)> = [codex, claude]
        .into_iter()
        .map(|path| {
            let body = fs::read_to_string(&path).expect("read source");
            (path, body)
        })
        .collect();

    assert!(sandbox.run(&["extract", "all"]).status.success());

    for (path, body) in before {
        assert_eq!(
            fs::read_to_string(&path).expect("source still readable"),
            body,
            "{} must be byte-identical after a read-only pass",
            path.display()
        );
    }
}

fn _assert_path_helper_used(_: &Path) {}
