#![cfg(feature = "app")]
//! W3-05: `--for-inject` keeps a floor of NOW lines while OPEN WORK and
//! ADDRESSED TO are still over budget. The omission marker names those
//! sections with the number of rows it dropped. A pack that fits is left
//! byte-identical.
//!
//! Addressed speech is the last unprotected content to go: OPEN WORK gives
//! its rows back first, and ADDRESSED TO loses its oldest takes first.
use aicx::catalog::{CATALOG_SCHEMA, CatalogEntry};
use chrono::Utc;
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

const END: &str = "2026-10-21T10:00:00Z";
const AUTHOR_DATE: &str = "2026-10-21T09:30:00Z";
const PROJECT: &str = "Loctree/aicx";
/// Newest open human requests reserved whole under the inject budget.
const NOW_FLOOR: usize = 5;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "aicx-now-floor-{}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(root.join(".aicx/catalog")).unwrap();
        fs::create_dir_all(root.join("Loctree/aicx")).unwrap();
        Self { root }
    }

    fn repo(&self) -> PathBuf {
        self.root.join("Loctree/aicx")
    }

    fn entry(&self, id: &str, path: PathBuf) -> CatalogEntry {
        CatalogEntry {
            schema: CATALOG_SCHEMA.into(),
            session_id: id.into(),
            agent: "claude".into(),
            project: Some(PROJECT.into()),
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

    fn decisions(&self, count: usize) {
        let mut rows = Vec::new();
        for index in 0..count {
            let id = format!("d1111111-2222-4333-8444-{index:012x}");
            let path = self
                .root
                .join(".claude/projects/fixture")
                .join(format!("{id}.jsonl"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            // One human decision per session, newest last. NOW renders
            // requests before open-session lines, newest first.
            let at = format!("2026-10-21T09:{index:02}:00Z");
            let text = format!("Decision: NOW-FLOOR-{index} keep this request whole");
            fs::write(
                &path,
                format!(
                    "{}\n",
                    json!({
                        "type": "user",
                        "sessionId": id,
                        "cwd": self.repo(),
                        "timestamp": at,
                        "message": {"role": "user", "content": text}
                    })
                ),
            )
            .unwrap();
            rows.push(self.entry(&id, path));
        }
        fs::write(
            aicx::catalog::sessions_path_for(&self.root.join(".aicx")),
            rows.iter()
                .map(|row| format!("{}\n", serde_json::to_string(row).unwrap()))
                .collect::<String>(),
        )
        .unwrap();
    }

    fn branches(&self, count: usize) {
        self.git(&["init", "--quiet", "-b", "main"], None);
        self.git(
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/Loctree/aicx.git",
            ],
            None,
        );
        fs::write(self.repo().join("seed"), "base").unwrap();
        self.git(&["add", "seed"], None);
        self.git(&["commit", "--quiet", "-m", "base"], None);
        let base = self.git(&["rev-parse", "HEAD"], None);
        let subject = format!("open-row {}", "branch ".repeat(180));
        for index in 0..count {
            let blob = self.git(
                &["hash-object", "-w", "--stdin"],
                Some(&format!("branch {index}")),
            );
            let tree = self.git(&["mktree"], Some(&format!("100644 blob {blob}\tseed\n")));
            let commit = self.git(
                &["commit-tree", &tree, "-p", &base],
                Some(&format!("work {index} {subject}\n")),
            );
            self.git(
                &[
                    "update-ref",
                    &format!("refs/heads/cut/work-{index:02}"),
                    &commit,
                ],
                None,
            );
        }
    }

    fn addressed(&self, count: usize) {
        let rows: Vec<_> = (0..count)
            .map(|index| {
                let mut row = json!({
                    "schema": "codescribe.transcript-evidence.v1",
                    "audience": "andrzej",
                    "recipients": [{
                        "name": "Andrzej",
                        "provider": "claude-code",
                        "provider_session_id": "d1111111-2222-4333-8444-000000000000"
                    }],
                    "rendered_text": format!("PRESSURE-{index} {}", "voice ".repeat(400)),
                    "emitted_at": format!("2026-10-21T08:{index:02}:00Z"),
                    "reducer_revision": 1,
                    "reducer_action": "record_ledger_terminal_seal",
                    "session_id": format!("take-{index}")
                });
                row["take_id"] = json!(format!("take-{index}"));
                row
            })
            .collect();
        let path = self
            .root
            .join(".codescribe/agent-bridge/buses/channel-5.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            rows.iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
        )
        .unwrap();
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
            .env("GIT_AUTHOR_DATE", AUTHOR_DATE)
            .env("GIT_COMMITTER_DATE", AUTHOR_DATE)
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
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn run(&self, inject: bool) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aicx"));
        cmd.env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("AICX_HOME", self.root.join(".aicx"))
            .env("AICX_ALLOW_TMP", "1")
            .args([
                "continuity",
                "show",
                "-p",
                PROJECT,
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
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn section<'a>(pack: &'a str, heading: &str) -> &'a str {
    pack.split(&format!("## {heading}\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("missing {heading}:\n{pack}"))
        .split("\n## ")
        .next()
        .unwrap()
        .split("\n[continuity pack truncated")
        .next()
        .unwrap()
}

fn content_rows(section_body: &str) -> Vec<&str> {
    section_body
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// Lines present before the budget and absent after it.
fn dropped_rows(full: &str, inject: &str) -> usize {
    let mut kept = content_rows(inject);
    let mut dropped = 0;
    for line in content_rows(full) {
        if let Some(index) = kept.iter().position(|kept_line| *kept_line == line) {
            kept.remove(index);
        } else {
            dropped += 1;
        }
    }
    dropped
}

fn marker_line(pack: &str) -> &str {
    pack.lines()
        .find(|line| line.contains("truncated at inject budget"))
        .unwrap_or_else(|| panic!("missing omission marker:\n{pack}"))
}

fn marked_drops(marker: &str, section_name: &str) -> usize {
    let token = format!("{section_name} (");
    let rest = marker
        .split(&token)
        .nth(1)
        .unwrap_or_else(|| panic!("marker did not count {section_name}: {marker}"));
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    assert!(
        rest.starts_with(&format!("{digits} row")),
        "count for {section_name} is not a row count: {marker}"
    );
    digits
        .parse()
        .unwrap_or_else(|_| panic!("no row count for {section_name}: {marker}"))
}

fn now_floor_hits(pack: &str, count: usize) -> usize {
    let now = section(pack, "NOW");
    (0..count)
        .filter(|index| now.contains(&format!("NOW-FLOOR-{index}")))
        .count()
}

/// Indices of the addressed takes still printed, oldest first. The trailing
/// space keeps `PRESSURE-1` from matching `PRESSURE-10`.
fn kept_takes(pack: &str, count: usize) -> Vec<usize> {
    let addressed = section(pack, "ADDRESSED TO ANDRZEJ");
    (0..count)
        .filter(|index| addressed.contains(&format!("PRESSURE-{index} ")))
        .collect()
}

#[test]
fn inject_keeps_the_now_floor_and_counts_dropped_rows() {
    let fixture = Fixture::new();
    fixture.decisions(7);
    fixture.branches(18);
    fixture.addressed(12);

    let full = fixture.pack(false);
    let full_now = now_floor_hits(&full, 7);
    assert!(
        full_now >= NOW_FLOOR,
        "fixture rendered only {full_now} NOW requests; need at least {NOW_FLOOR}:\n{}",
        section(&full, "NOW")
    );
    assert!(
        section(&full, "OPEN WORK").chars().count() > 24_000,
        "OPEN WORK must exceed the inject budget on its own"
    );
    assert!(
        section(&full, "ADDRESSED TO ANDRZEJ").chars().count() > 24_000,
        "ADDRESSED TO must exceed the inject budget on its own"
    );

    let inject = fixture.pack(true);
    let chars = inject.chars().count();
    assert!(
        chars <= 24_000,
        "inject pack is {chars} chars, budget is 24000"
    );
    let kept = now_floor_hits(&inject, 7);
    let floor = full_now.min(NOW_FLOOR);
    assert!(
        kept >= floor,
        "NOW kept {kept} of {full_now} requests; floor is {floor}:\n{}",
        section(&inject, "NOW")
    );
    // The newest requests are the floor. Cutting them while OPEN WORK is
    // still the overflow is the bug this cut closes.
    for index in (7 - floor)..7 {
        assert!(
            section(&inject, "NOW").contains(&format!("NOW-FLOOR-{index}")),
            "newest NOW-FLOOR-{index} was cut:\n{}",
            section(&inject, "NOW")
        );
    }

    let marker = marker_line(&inject);
    let open_dropped = dropped_rows(section(&full, "OPEN WORK"), section(&inject, "OPEN WORK"));
    let addressed_dropped = dropped_rows(
        section(&full, "ADDRESSED TO ANDRZEJ"),
        section(&inject, "ADDRESSED TO ANDRZEJ"),
    );
    assert!(open_dropped > 0, "OPEN WORK was not trimmed:\n{marker}");
    assert!(
        addressed_dropped > 0,
        "ADDRESSED TO was not trimmed:\n{marker}"
    );
    assert_eq!(marked_drops(marker, "OPEN WORK"), open_dropped, "{marker}");
    assert_eq!(
        marked_drops(marker, "ADDRESSED TO ANDRZEJ"),
        addressed_dropped,
        "{marker}"
    );
    assert!(section(&inject, "SOURCES").contains("- claude"));
    assert!(section(&inject, "INDEX HEALTH").contains("readiness:"));
}

#[test]
fn inject_keeps_every_now_line_when_the_section_is_shorter_than_the_floor() {
    let fixture = Fixture::new();
    fixture.decisions(2);
    fixture.branches(18);
    fixture.addressed(12);
    let full = fixture.pack(false);
    assert_eq!(now_floor_hits(&full, 2), 2, "{}", section(&full, "NOW"));
    let inject = fixture.pack(true);
    assert!(inject.chars().count() <= 24_000);
    assert_eq!(
        now_floor_hits(&inject, 2),
        2,
        "a short NOW section must survive whole:\n{}",
        section(&inject, "NOW")
    );
}

#[test]
fn inject_trims_open_work_before_addressed_speech() {
    let fixture = Fixture::new();
    fixture.decisions(2);
    fixture.branches(18);
    fixture.addressed(3);

    let full = fixture.pack(false);
    assert!(
        section(&full, "OPEN WORK").chars().count() > 24_000,
        "OPEN WORK must exceed the inject budget on its own"
    );
    assert_eq!(kept_takes(&full, 3), vec![0, 1, 2]);

    let inject = fixture.pack(true);
    assert!(inject.chars().count() <= 24_000);
    let marker = marker_line(&inject);
    assert!(
        dropped_rows(section(&full, "OPEN WORK"), section(&inject, "OPEN WORK")) > 0,
        "OPEN WORK was not trimmed:\n{marker}"
    );
    // Every take fits once OPEN WORK gives its rows back. Cutting a take
    // while an OPEN WORK row is still printed is the bug this cut closes.
    assert_eq!(
        kept_takes(&inject, 3),
        vec![0, 1, 2],
        "addressed speech was cut before OPEN WORK:\n{marker}\n{}",
        section(&inject, "ADDRESSED TO ANDRZEJ")
    );
    assert!(!marker.contains("ADDRESSED TO"), "{marker}");
    assert_eq!(now_floor_hits(&inject, 2), 2, "{}", section(&inject, "NOW"));
}

#[test]
fn inject_drops_the_oldest_addressed_speech_first() {
    let fixture = Fixture::new();
    fixture.decisions(1);
    fixture.branches(0);
    fixture.addressed(12);

    let full = fixture.pack(false);
    assert!(
        section(&full, "ADDRESSED TO ANDRZEJ").chars().count() > 24_000,
        "ADDRESSED TO must exceed the inject budget on its own"
    );
    let inject = fixture.pack(true);
    assert!(inject.chars().count() <= 24_000);
    let kept = kept_takes(&inject, 12);
    let addressed = section(&inject, "ADDRESSED TO ANDRZEJ");
    assert!(
        !kept.is_empty() && kept.len() < 12,
        "kept {kept:?}:\n{addressed}"
    );
    // Takes render newest first; the budget takes them from the tail, so
    // what survives is the newest run of takes.
    assert_eq!(
        kept,
        ((12 - kept.len())..12).collect::<Vec<_>>(),
        "{addressed}"
    );
    assert_eq!(now_floor_hits(&inject, 1), 1, "{}", section(&inject, "NOW"));
    // The rest of HONESTY goes before addressed speech; the pack title and
    // the window/labels lines do not.
    let marker = marker_line(&inject);
    assert!(marker.contains("HONESTY"), "{marker}");
    assert_eq!(
        inject.lines().next(),
        full.lines().next(),
        "pack title was cut:\n{inject}"
    );
    let honesty = section(&inject, "HONESTY");
    for head in ["window: [", "labels: "] {
        assert!(honesty.contains(head), "HONESTY lost {head}:\n{honesty}");
    }
}

#[test]
fn inject_leaves_a_fitting_pack_byte_identical() {
    let fixture = Fixture::new();
    fixture.decisions(1);
    fixture.branches(0);
    let full = fixture.pack(false);
    let inject = fixture.pack(true);
    assert!(
        full.chars().count() <= 24_000,
        "fitting fixture overflowed: {}",
        full.chars().count()
    );
    assert_eq!(inject, full);
}
