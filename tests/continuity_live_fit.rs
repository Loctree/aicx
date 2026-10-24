#![cfg(feature = "app")]
//! Live-shaped regressions, exercised through the real cold-start command.
use aicx::catalog::{CATALOG_SCHEMA, CatalogEntry};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

const END: &str = "2026-10-21T10:00:00Z";
const AT: &str = "2026-10-21T09:00:00Z";
const SESSION: &str = "d1111111-2222-4333-8444-555555555555";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "aicx-live-fit-{}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let f = Self(root);
        fs::create_dir_all(f.0.join(".aicx/catalog")).unwrap();
        fs::create_dir_all(f.repo()).unwrap();
        let source =
            f.0.join(".claude/projects/fixture")
                .join(format!("{SESSION}.jsonl"));
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, format!("{}\n", json!({"type":"user", "sessionId":SESSION, "cwd":f.repo(), "timestamp":AT, "message":{"role":"user","content":"Decision: KEEP-NOW preserve the current assignment"}}))).unwrap();
        f.catalog(vec![f.entry(SESSION, "claude", source)]);
        f
    }
    fn repo(&self) -> PathBuf {
        self.0.join("Loctree/aicx")
    }
    fn entry(&self, id: &str, agent: &str, path: PathBuf) -> CatalogEntry {
        CatalogEntry {
            schema: CATALOG_SCHEMA.into(),
            session_id: id.into(),
            agent: agent.into(),
            project: Some("Loctree/aicx".into()),
            date: Some("2026-10-21".into()),
            cwd: Some(self.repo().to_string_lossy().into()),
            source_path: path.to_string_lossy().into(),
            source_len: None,
            source_mtime_ns: None,
            source_bundle_fingerprint: None,
            title: None,
            machine: None,
            logical_session_id: None,
            session_kind: None,
        }
    }
    fn catalog(&self, rows: Vec<CatalogEntry>) {
        fs::write(
            aicx::catalog::sessions_path_for(&self.0.join(".aicx")),
            rows.iter()
                .map(|r| format!("{}\n", serde_json::to_string(r).unwrap()))
                .collect::<String>(),
        )
        .unwrap();
    }
    fn voice(id: Option<&str>, text: &str, at: &str) -> Value {
        let mut row = json!({"schema":"codescribe.transcript-evidence.v1", "audience":"andrzej", "recipients":[{"name":"Andrzej","provider":"claude-code","provider_session_id":SESSION}], "rendered_text":text, "emitted_at":at, "reducer_revision":1, "reducer_action":"record_ledger_terminal_seal"});
        if let Some(id) = id {
            row["session_id"] = json!(id);
        }
        row
    }
    fn ledger(&self, name: &str, rows: &[Value]) {
        let path = self.0.join(".codescribe/agent-bridge/buses").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
    }
    fn run(&self, inject: bool) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aicx"));
        cmd.env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("AICX_HOME", self.0.join(".aicx"))
            .env("AICX_ALLOW_TMP", "1")
            .args([
                "continuity",
                "show",
                "-p",
                "Loctree/aicx",
                "--as",
                "Andrzej",
                "-H",
                "72",
                "--until",
                END,
                "--no-refresh",
            ]);
        if inject {
            cmd.arg("--for-inject");
        }
        cmd.output().unwrap()
    }
    fn pack(&self, inject: bool) -> String {
        let out = self.run(inject);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn git(&self, args: &[&str], input: Option<&str>) -> String {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = aicx::git_env::git_command_isolated()
            .arg("-C")
            .arg(self.repo())
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().into()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn section<'a>(pack: &'a str, heading: &str) -> &'a str {
    pack.split(&format!("## {heading}\n"))
        .nth(1)
        .unwrap()
        .split("\n## ")
        .next()
        .unwrap()
        .split("\n[continuity pack truncated")
        .next()
        .unwrap()
}

#[test]
fn multi_ledger_take_renders_once_with_all_ledger_names() {
    let f = Fixture::new();
    let first = Fixture::voice(Some("stable-take"), "ONE-TAKE same assignment", AT);
    // Same take can have a different replay timestamp in a generation copy.
    let mut copy = first.clone();
    copy["emitted_at"] = json!("2026-10-21T09:00:01Z");
    for (name, row) in [
        ("channel-1.jsonl", first.clone()),
        ("channel-5.jsonl", copy),
        ("events/2026_1021/events.jsonl", first.clone()),
        ("events/2026_1021/events.compressed.jsonl", first),
    ] {
        f.ledger(name, &[row]);
    }
    let pack = f.pack(false);
    let addressed = section(&pack, "ADDRESSED TO ANDRZEJ");
    assert_eq!(addressed.matches("ONE-TAKE").count(), 1, "{addressed}");
    for ledger in [
        "channel-1.jsonl",
        "channel-5.jsonl",
        "events.jsonl",
        "events.compressed.jsonl",
    ] {
        assert!(addressed.contains(ledger), "{addressed}");
    }
    // A distinct stable take is never collapsed just because its words match.
    f.ledger(
        "channel-7.jsonl",
        &[Fixture::voice(
            Some("different-take"),
            "ONE-TAKE same assignment",
            AT,
        )],
    );
    assert_eq!(
        section(&f.pack(false), "ADDRESSED TO ANDRZEJ")
            .matches("ONE-TAKE")
            .count(),
        2
    );
}

#[test]
fn idless_take_uses_timestamp_and_normalized_text_without_merging_other_times() {
    let f = Fixture::new();
    f.ledger(
        "channel-1.jsonl",
        &[
            Fixture::voice(None, "FALLBACK take   text", AT),
            Fixture::voice(None, "FALLBACK take text", "2026-10-21T09:00:01Z"),
        ],
    );
    f.ledger(
        "channel-2.jsonl",
        &[Fixture::voice(None, "FALLBACK take text", AT)],
    );
    let pack = f.pack(false);
    assert_eq!(
        section(&pack, "ADDRESSED TO ANDRZEJ")
            .matches("FALLBACK")
            .count(),
        2,
        "{pack}"
    );
}

#[test]
fn inject_preserves_now_when_addressed_and_branch_work_exceed_budget() {
    let f = Fixture::new();
    f.git(&["init", "--quiet", "-b", "main"], None);
    f.git(
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/Loctree/aicx.git",
        ],
        None,
    );
    fs::write(f.repo().join("seed"), "base").unwrap();
    f.git(&["add", "seed"], None);
    f.git(&["commit", "--quiet", "-m", "base"], None);
    let base = f.git(&["rev-parse", "HEAD"], None);
    for i in 0..30 {
        let blob = f.git(
            &["hash-object", "-w", "--stdin"],
            Some(&format!("branch {i}")),
        );
        let tree = f.git(&["mktree"], Some(&format!("100644 blob {blob}\tseed\n")));
        let commit = f.git(
            &["commit-tree", &tree, "-p", &base],
            Some(&format!("work {i} {}\n", "subject ".repeat(100))),
        );
        f.git(
            &[
                "update-ref",
                &format!("refs/heads/cut/work-{i:02}"),
                &commit,
            ],
            None,
        );
    }
    let takes: Vec<_> = (0..20)
        .map(|i| {
            Fixture::voice(
                Some(&format!("take-{i}")),
                &format!("PRESSURE-{i} {}", "voice ".repeat(600)),
                AT,
            )
        })
        .collect();
    f.ledger("channel-5.jsonl", &takes);
    let full = f.pack(false);
    assert!(section(&full, "OPEN WORK").chars().count() > 24_000);
    assert!(section(&full, "ADDRESSED TO ANDRZEJ").chars().count() > 24_000);
    let inject = f.pack(true);
    assert!(inject.chars().count() <= 24_000);
    assert!(
        section(&inject, "NOW").contains("KEEP-NOW"),
        "NOW lost its assignment: {inject}"
    );
    let marker = inject
        .lines()
        .find(|l| l.contains("truncated at inject budget"))
        .unwrap();
    for name in ["OPEN WORK", "ADDRESSED TO ANDRZEJ"] {
        assert!(marker.contains(name), "{marker}");
    }
    assert!(section(&inject, "SOURCES").contains("- claude"));
    assert!(section(&inject, "INDEX HEALTH").contains("readiness:"));
}

#[test]
fn continuity_skips_old_sources_before_reading_but_keeps_margin_errors() {
    let f = Fixture::new();
    let mut rows = aicx::catalog::read_entries_at(&f.0.join(".aicx")).unwrap();
    let old = f.0.join(".junie/sessions/session-old/events.jsonl");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, "{broken truncated body").unwrap();
    let cutoff: DateTime<Utc> = "2026-10-17T10:00:00Z".parse().unwrap(); // start minus 24 h
    filetime::set_file_mtime(
        &old,
        filetime::FileTime::from_unix_time(cutoff.timestamp() - 1, 0),
    )
    .unwrap();
    rows.push(f.entry("old", "junie", old.clone()));
    f.catalog(rows);
    let pack = f.pack(false);
    let honesty = section(&pack, "HONESTY");
    assert!(honesty.contains("skipped_before_window=1"), "{honesty}");
    assert!(honesty.contains("source_errors=0"), "{honesty}");
    // Publish CURRENT as well: selection must skip before validating/re-reading
    // index candidates, not merely fix the catalog fallback's error count.
    let index = Command::new(env!("CARGO_BIN_EXE_aicx"))
        .env("HOME", &f.0)
        .env("USERPROFILE", &f.0)
        .env("AICX_HOME", f.0.join(".aicx"))
        .env("AICX_ALLOW_TMP", "1")
        .args(["index"])
        .output()
        .unwrap();
    assert!(
        index.status.success(),
        "{}",
        String::from_utf8_lossy(&index.stderr)
    );
    let indexed = f.pack(false);
    assert!(
        section(&indexed, "SOURCES")
            .replace('\\', "/")
            .contains("extracts/claude"),
        "CURRENT must serve the validated extract lane: {indexed}"
    );
    assert!(
        section(&indexed, "HONESTY").contains("skipped_before_window=1"),
        "{indexed}"
    );
    assert!(
        section(&indexed, "HONESTY").contains("source_errors=0"),
        "{indexed}"
    );
    // The boundary is strict; same unreadable file inside the margin is an error.
    filetime::set_file_mtime(
        &old,
        filetime::FileTime::from_unix_time(cutoff.timestamp(), 0),
    )
    .unwrap();
    let pack = f.pack(false);
    assert!(
        !section(&pack, "HONESTY").contains("skipped_before_window"),
        "{pack}"
    );
    assert!(
        !section(&pack, "HONESTY").contains("source_errors=0"),
        "{pack}"
    );
    // Unknown mtime cannot justify a skip: the missing source stays an error.
    fs::remove_file(&old).unwrap();
    let missing = f.pack(false);
    assert!(
        !section(&missing, "HONESTY").contains("skipped_before_window"),
        "{missing}"
    );
    assert!(
        !section(&missing, "HONESTY").contains("source_errors=0"),
        "{missing}"
    );
}

#[test]
fn replayed_channel_take_prefers_explicit_take_id_over_channel_session() {
    let mut row = Fixture::voice(Some("agent-channel-5-session"), "explicit identity", AT);
    row["take_id"] = json!("shared-original-take");
    let closed = json!({"schema":"codescribe.transcript-evidence.v1", "session_id":"agent-channel-5-session", "reducer_action":"session_ended"});
    let replay = aicx::importers::codescribe_bus::replay_bus_ledger(&format!("{row}\n{closed}\n"));
    assert_eq!(replay.messages.len(), 1);
    assert_eq!(
        replay.messages[0].take_id.as_deref(),
        Some("shared-original-take")
    );
}
