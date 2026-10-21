#![cfg(feature = "app")]
//! W3-03: under `--for-inject`, OPEN WORK keeps branches whose newest open
//! commit sits in the pack window, and any branch a worktree still holds.
//! Every other branch folds into one line. Without `--for-inject` each
//! branch stays on its own row.
use aicx::catalog::{CATALOG_SCHEMA, CatalogEntry};
use chrono::{DateTime, Utc};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, MutexGuard},
};

static HOME_LOCK: Mutex<()> = Mutex::new(());

const PROJECT: &str = "fixture/inject-fold";
const NOW: &str = "2026-10-21T12:00:00Z";
const HOURS: &str = "72";
const NEWEST_STALE: &str = "2026-09-28T15:00:00+00:00";
/// `git log --format=%aI` prints a UTC offset of zero as `Z`.
const NEWEST_STALE_PRINTED: &str = "2026-09-28T15:00:00Z";
const STALE_COUNT: usize = 30;
const REAPPLIED: usize = 4;

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    previous_home: Option<std::ffi::OsString>,
    _home_lock: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new(hold_stale: bool) -> Self {
        let home_lock = HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "aicx-inject-fold-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(root.join(".aicx/synthetic")).unwrap();
        let previous_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &root);
        }
        let repo = root.join("repo");
        let fixture = Self {
            root,
            repo,
            previous_home,
            _home_lock: home_lock,
        };
        fixture.build_repository(hold_stale);
        fixture.write_session();
        fixture
    }

    fn home(&self) -> PathBuf {
        self.root.join(".aicx")
    }

    fn git(&self, dir: &Path, args: &[&str], date: &str) -> String {
        let output = aicx::git_env::git_command_isolated()
            .arg("-C")
            .arg(dir)
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
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn commit(&self, dir: &Path, file: &str, message: &str, date: &str) -> String {
        fs::write(dir.join(file), format!("{message}\n")).unwrap();
        self.git(dir, &["add", "--", file], date);
        self.git(dir, &["commit", "--quiet", "-m", message], date);
        self.git(dir, &["rev-parse", "HEAD"], date)
    }

    fn stale_message(index: usize) -> String {
        format!(
            "feat(stale): parked {index:02}\n\nAuthored-By: grok <agents@vetcoders.io>\nsession_id: 0193{index:04}-1111-4222-8333-444444444444\n"
        )
    }

    fn build_repository(&self, hold_stale: bool) {
        let (repo, base_date) = (&self.repo, "2026-09-01T08:00:00+00:00");
        fs::create_dir_all(repo).unwrap();
        self.git(repo, &["init", "--quiet", "-b", "integrate"], base_date);
        self.git(
            repo,
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/fixture/inject-fold.git",
            ],
            base_date,
        );
        self.commit(repo, "base.txt", "chore: base", base_date);

        for index in 0..STALE_COUNT {
            let branch = format!("parked/{index:02}");
            let date = if index + 1 == STALE_COUNT {
                NEWEST_STALE
            } else {
                "2026-09-10T08:00:00+00:00"
            };
            let message = Self::stale_message(index);
            self.git(repo, &["checkout", "--quiet", "-b", &branch], date);
            self.commit(repo, &format!("parked-{index:02}.txt"), &message, date);
            self.git(repo, &["checkout", "--quiet", "integrate"], date);
            // Same subject and session_id, different patch: cherry keeps the
            // branch open and the row carries newest_reapplied_in_head.
            if index < REAPPLIED {
                self.commit(
                    repo,
                    &format!("reapplied-{index:02}.txt"),
                    &message,
                    "2026-10-19T08:00:00+00:00",
                );
            }
        }

        self.git(
            repo,
            &["checkout", "--quiet", "-b", "cut/now-a"],
            "2026-10-20T08:00:00+00:00",
        );
        self.commit(
            repo,
            "now-a.txt",
            "feat(now): slice inside the window",
            "2026-10-20T08:00:00+00:00",
        );
        self.git(
            repo,
            &["checkout", "--quiet", "integrate"],
            "2026-10-20T08:00:00+00:00",
        );
        self.git(
            repo,
            &["checkout", "--quiet", "-b", "cut/now-b"],
            "2026-10-21T09:00:00+00:00",
        );
        self.commit(
            repo,
            "now-b.txt",
            "feat(now): newer slice inside the window",
            "2026-10-21T09:00:00+00:00",
        );
        self.git(
            repo,
            &["checkout", "--quiet", "integrate"],
            "2026-10-21T09:00:00+00:00",
        );

        if hold_stale {
            let held = self.root.join("wt-held");
            let path = held.to_string_lossy().into_owned();
            self.git(
                repo,
                &["worktree", "add", "--quiet", "-b", "cut/held", &path],
                "2026-09-05T08:00:00+00:00",
            );
            self.commit(
                &held,
                "held.txt",
                "feat(held): stale but still checked out",
                "2026-09-05T08:00:00+00:00",
            );
        }
    }

    fn write_session(&self) {
        let cwd = self.repo.to_string_lossy().into_owned();
        let session = "dddddddd-1111-4222-8333-444444444444";
        let path = self.home().join(format!("synthetic/{session}.jsonl"));
        let line = serde_json::json!({
            "type": "user",
            "sessionId": session,
            "cwd": cwd,
            "timestamp": "2026-10-21T10:00:00Z",
            "message": {"role": "user", "content": "Decision: fold September branches out of the inject pack."}
        });
        fs::write(&path, format!("{line}\n")).unwrap();
        let entry = CatalogEntry {
            schema: CATALOG_SCHEMA.into(),
            session_id: session.into(),
            agent: "claude".into(),
            project: Some(PROJECT.into()),
            date: Some("2026-10-21".into()),
            cwd: Some(cwd),
            source_path: path.to_string_lossy().into(),
            source_len: None,
            source_mtime_ns: None,
            source_bundle_fingerprint: None,
            title: None,
            machine: None,
            logical_session_id: None,
            session_kind: None,
        };
        let catalog = aicx::catalog::sessions_path_for(&self.home());
        fs::create_dir_all(catalog.parent().unwrap()).unwrap();
        fs::write(
            catalog,
            format!("{}\n", serde_json::to_string(&entry).unwrap()),
        )
        .unwrap();
    }

    fn render(&self, for_inject: bool) -> String {
        let now: DateTime<Utc> = NOW.parse().unwrap();
        let pack = aicx::continuity::build_with_scope_at(
            &self.home(),
            &[PROJECT.to_string()],
            HOURS.parse().unwrap(),
            false,
            now,
        )
        .unwrap();
        aicx::continuity::render(&pack, for_inject)
    }

    fn cli(&self, for_inject: bool) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aicx"));
        command
            .args([
                "continuity",
                "show",
                "-p",
                PROJECT,
                "-H",
                HOURS,
                "--until",
                NOW,
                "--no-refresh",
            ])
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("AICX_HOME", self.home())
            .env("AICX_ALLOW_TMP", "1");
        if for_inject {
            command.arg("--for-inject");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe {
            match &self.previous_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn open_work(markdown: &str) -> &str {
    markdown
        .split("## OPEN WORK\n")
        .nth(1)
        .unwrap_or_else(|| panic!("OPEN WORK section missing:\n{markdown}"))
        .split("\n## ")
        .next()
        .unwrap()
}

fn row<'a>(section: &'a str, branch: &str) -> Option<&'a str> {
    section
        .lines()
        .find(|line| line.starts_with(&format!("- {branch} · ")))
}

fn branch_rows(section: &str) -> Vec<&str> {
    section
        .lines()
        .filter(|line| line.starts_with("- cut/") || line.starts_with("- parked/"))
        .collect()
}

fn fold_lines(section: &str) -> Vec<&str> {
    section
        .lines()
        .filter(|line| line.contains("outside the pack window"))
        .collect()
}

#[test]
fn inject_folds_thirty_stale_branches_and_keeps_the_two_inside_the_window() {
    let fixture = Fixture::new(false);
    let full_md = fixture.render(false);
    let inject_md = fixture.render(true);
    let full = open_work(&full_md);
    let inject = open_work(&inject_md);

    assert!(
        fold_lines(full).is_empty(),
        "without --for-inject every branch stays listed:\n{full}"
    );
    assert_eq!(branch_rows(full).len(), STALE_COUNT + 2, "{full}");
    for index in 0..STALE_COUNT {
        let name = format!("parked/{index:02}");
        let line = row(full, &name).unwrap_or_else(|| panic!("missing {name}:\n{full}"));
        if index < REAPPLIED {
            assert!(line.contains("newest_reapplied_in_head="), "{line}");
        } else {
            assert!(!line.contains("newest_reapplied_in_head"), "{line}");
        }
    }
    assert!(
        row(full, "parked/29")
            .unwrap()
            .contains(NEWEST_STALE_PRINTED),
        "{full}"
    );

    let kept = ["cut/now-b", "cut/now-a"];
    assert_eq!(
        branch_rows(inject),
        kept.iter()
            .map(|branch| row(full, branch).unwrap())
            .collect::<Vec<_>>(),
        "in-window rows must be byte-identical to the full section:\n{inject}"
    );
    for index in 0..STALE_COUNT {
        assert!(
            row(inject, &format!("parked/{index:02}")).is_none(),
            "{inject}"
        );
    }
    let folds = fold_lines(inject);
    assert_eq!(folds.len(), 1, "{inject}");
    assert_eq!(
        folds[0],
        format!(
            "- {STALE_COUNT} branch(es) outside the pack window · newest_author_date={NEWEST_STALE_PRINTED} · newest_reapplied_in_head={REAPPLIED}"
        ),
        "{inject}"
    );

    let cli_full = fixture.cli(false);
    let cli_inject = fixture.cli(true);
    assert_eq!(open_work(&cli_full), full);
    assert_eq!(open_work(&cli_inject), inject);
}

#[test]
fn inject_keeps_a_stale_worktree_branch_on_its_own_row() {
    let fixture = Fixture::new(true);
    let full_md = fixture.render(false);
    let inject_md = fixture.render(true);
    let full = open_work(&full_md);
    let inject = open_work(&inject_md);
    let held = fixture.root.join("wt-held");
    let worktree = aicx_parser::sanitize::strip_verbatim_prefix(&fs::canonicalize(&held).unwrap());

    let listed = row(inject, "cut/held").unwrap_or_else(|| panic!("{inject}"));
    assert_eq!(listed, row(full, "cut/held").unwrap());
    assert!(
        listed.contains(&format!("worktree={}", worktree.display())),
        "{listed}"
    );
    assert!(
        listed.contains("author_date=2026-09-05T08:00:00Z"),
        "{listed}"
    );
    assert!(row(inject, "parked/00").is_none(), "{inject}");
    let folds = fold_lines(inject);
    assert_eq!(
        folds,
        [
            "- 30 branch(es) outside the pack window · newest_author_date=2026-09-28T15:00:00Z · newest_reapplied_in_head=4"
        ],
        "{inject}"
    );
    assert!(fold_lines(full).is_empty(), "{full}");
    assert!(row(full, "cut/held").is_some());
    assert_eq!(branch_rows(full).len(), STALE_COUNT + 3, "{full}");
}
