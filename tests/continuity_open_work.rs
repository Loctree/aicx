#![cfg(feature = "app")]
//! OPEN WORK acceptance (W2-02): local branches of the filtered repository
//! that carry commits HEAD lacks by patch identity, rendered by the real
//! continuity pack and the real CLI from one git fixture.
//!
//! On 2026-10-21 four finished fleet cuts sat for 30 hours on local branches
//! held by worktrees, absent from the integration branch, and no surface said
//! so. These tests pin the surface that says so.
use aicx::catalog::{CATALOG_SCHEMA, CatalogEntry};
use chrono::{DateTime, Utc};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Mutex, MutexGuard},
};

// HOME is process-global; each fixture holds this lock for its whole life.
static HOME_LOCK: Mutex<()> = Mutex::new(());

const PROJECT: &str = "fixture/open-work";
const ALPHA_SESSION: &str = "aaaaaaaa-1111-4222-8333-444444444444";
const BETA_SESSION: &str = "bbbbbbbb-1111-4222-8333-444444444444";
const NOW: &str = "2026-10-21T12:00:00Z";

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    alpha_worktree: PathBuf,
    previous_home: Option<std::ffi::OsString>,
    _home_lock: MutexGuard<'static, ()>,
}

impl Fixture {
    /// One principal checkout on `integrate` with:
    /// - `cut/alpha`: two commits, held by the linked worktree `wt-alpha`;
    /// - `cut/beta`: one commit, its worktree already removed;
    /// - `cut/picked`: one commit cherry-picked onto `integrate` (new SHA);
    /// - `cut/merged`: no commit of its own.
    ///
    /// The catalog records one session whose workdir is the linked worktree,
    /// so the pack must fold it into the principal checkout to find HEAD.
    fn new() -> Self {
        // One failing test must not poison the lock and hide which contract broke.
        let home_lock = HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "aicx-open-work-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(root.join(".aicx/synthetic")).unwrap();
        let previous_home = std::env::var_os("HOME");
        // Serialized by HOME_LOCK: no other test thread touches HOME meanwhile.
        unsafe {
            std::env::set_var("HOME", &root);
        }
        let repo = root.join("repo");
        let alpha_worktree = root.join("wt-alpha");
        let fixture = Self {
            root,
            repo,
            alpha_worktree,
            previous_home,
            _home_lock: home_lock,
        };
        fixture.build_repository();
        fixture.write_session();
        fixture
    }

    fn home(&self) -> PathBuf {
        self.root.join(".aicx")
    }

    /// Fixture git goes through the same hermetic constructor the product
    /// uses, with this machine's global/system config and hooks shut out.
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

    fn build_repository(&self) {
        let (repo, base_date) = (&self.repo, "2026-10-19T08:00:00+00:00");
        fs::create_dir_all(repo).unwrap();
        self.git(repo, &["init", "--quiet", "-b", "integrate"], base_date);
        self.git(
            repo,
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/fixture/open-work.git",
            ],
            base_date,
        );
        self.commit(repo, "base.txt", "chore: base", base_date);

        // Held by a worktree: the shape the fleet left behind on 2026-10-21.
        let alpha = self.alpha_worktree.to_string_lossy().into_owned();
        self.git(
            repo,
            &["worktree", "add", "--quiet", "-b", "cut/alpha", &alpha],
            base_date,
        );
        self.commit(
            &self.alpha_worktree,
            "alpha-1.txt",
            "feat(alpha): first slice",
            "2026-10-20T08:00:00+00:00",
        );
        // Trailers exactly as the repo hooks write them: `session_id` carries
        // an underscore, which git's own trailer parser does not accept.
        self.commit(
            &self.alpha_worktree,
            "alpha-2.txt",
            &format!(
                "feat(alpha): finish the slice\n\n- body bullet\n\nAuthored-By: claude <agents@vetcoders.io>\nsession_id: {ALPHA_SESSION}\ntime: 2026-10-20T11:00:00+02:00\nruntime: local-worktrees"
            ),
            "2026-10-20T11:00:00+02:00",
        );

        // A parked branch whose worktree is gone.
        let beta = self.root.join("wt-beta");
        let beta_path = beta.to_string_lossy().into_owned();
        self.git(
            repo,
            &["worktree", "add", "--quiet", "-b", "cut/beta", &beta_path],
            base_date,
        );
        self.commit(
            &beta,
            "beta.txt",
            &format!(
                "feat(beta): parked cut\n\nAuthored-By: codex <agents@vetcoders.io>\nsession_id: {BETA_SESSION}\nruntime: local-worktrees"
            ),
            "2026-10-20T07:00:00+00:00",
        );
        self.git(repo, &["worktree", "remove", &beta_path], base_date);

        // Integrated under another SHA: same patch, cherry-picked onto HEAD.
        let picked = self.root.join("wt-picked");
        let picked_path = picked.to_string_lossy().into_owned();
        self.git(
            repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "cut/picked",
                &picked_path,
            ],
            base_date,
        );
        let picked_sha = self.commit(
            &picked,
            "picked.txt",
            "fix(picked): already integrated",
            "2026-10-19T12:00:00+00:00",
        );
        self.git(repo, &["worktree", "remove", &picked_path], base_date);
        self.git(
            repo,
            &["cherry-pick", &picked_sha],
            "2026-10-19T13:00:00+00:00",
        );
        let head_copy = self.git(repo, &["rev-parse", "HEAD"], base_date);
        assert_ne!(head_copy, picked_sha, "the pick must land under a new SHA");

        self.git(repo, &["branch", "cut/merged", "integrate~1"], base_date);
    }

    fn write_session(&self) {
        let cwd = self.alpha_worktree.to_string_lossy().into_owned();
        let path = self.home().join(format!("synthetic/{ALPHA_SESSION}.jsonl"));
        let line = serde_json::json!({
            "type": "user",
            "sessionId": ALPHA_SESSION,
            "cwd": cwd,
            "timestamp": "2026-10-21T10:00:00Z",
            "message": {"role": "user", "content": "Decision: keep the alpha cut on its branch until the operator integrates it."}
        });
        fs::write(&path, format!("{line}\n")).unwrap();
        let entry = CatalogEntry {
            schema: CATALOG_SCHEMA.into(),
            session_id: ALPHA_SESSION.into(),
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

    fn pack(&self) -> String {
        let now: DateTime<Utc> = NOW.parse().unwrap();
        let pack = aicx::continuity::build_with_scope_at(
            &self.home(),
            &[PROJECT.to_string()],
            48,
            false,
            now,
        )
        .unwrap();
        aicx::continuity::render(&pack, false)
    }

    /// The real binary, with whatever extra environment the caller inherits.
    fn cli(&self, extra_env: &[(&str, &Path)]) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aicx"));
        command
            .args([
                "continuity",
                "show",
                "-p",
                PROJECT,
                "-H",
                "48",
                "--until",
                NOW,
                "--no-refresh",
            ])
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("AICX_HOME", self.home())
            .env("AICX_ALLOW_TMP", "1");
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let output: Output = command.output().unwrap();
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

fn honesty(markdown: &str) -> &str {
    markdown
        .lines()
        .find(|line| line.starts_with("open work:"))
        .unwrap_or_else(|| panic!("HONESTY carries no open work line:\n{markdown}"))
}

#[test]
fn two_branches_ahead_render_with_subject_trailers_and_worktree() {
    let fixture = Fixture::new();
    let markdown = fixture.pack();
    let section = open_work(&markdown);

    // The pack prints paths without Windows' `\\?\` verbatim prefix, which
    // `canonicalize` adds there (windows-latest CI on 24fe661).
    let principal =
        aicx_parser::sanitize::strip_verbatim_prefix(&fs::canonicalize(&fixture.repo).unwrap());
    assert!(
        section.contains(&format!("checkout {}", principal.display())),
        "the linked-worktree session must fold into its principal checkout:\n{section}"
    );
    assert!(section.contains("HEAD=integrate"), "{section}");

    let alpha = row(section, "cut/alpha").unwrap_or_else(|| panic!("{section}"));
    let worktree = aicx_parser::sanitize::strip_verbatim_prefix(
        &fs::canonicalize(&fixture.alpha_worktree).unwrap(),
    );
    for expected in [
        "open=2",
        "subject=feat(alpha): finish the slice",
        "Authored-By=claude <agents@vetcoders.io>",
        &format!("session_id={ALPHA_SESSION}"),
        &format!("worktree={}", worktree.display()),
        "author_date=2026-10-20T11:00:00+02:00",
    ] {
        assert!(alpha.contains(expected), "missing {expected}: {alpha}");
    }

    let beta = row(section, "cut/beta").unwrap_or_else(|| panic!("{section}"));
    for expected in [
        "open=1",
        "subject=feat(beta): parked cut",
        "Authored-By=codex <agents@vetcoders.io>",
        &format!("session_id={BETA_SESSION}"),
        "worktree=none",
    ] {
        assert!(beta.contains(expected), "missing {expected}: {beta}");
    }

    // Exactly two branch rows; the newest open commit leads.
    let rows: Vec<&str> = section
        .lines()
        .filter(|line| line.starts_with("- cut/"))
        .collect();
    assert_eq!(rows.len(), 2, "{section}");
    assert!(rows[0].starts_with("- cut/alpha · "), "{section}");

    let line = honesty(&markdown);
    for expected in [
        "branches_examined=4/4",
        "commits_examined=4",
        "branch_cap_reached=no",
        "over_commit_budget=0",
    ] {
        assert!(line.contains(expected), "missing {expected}: {line}");
    }

    // The CLI renders the same section from the same fixture.
    assert_eq!(open_work(&fixture.cli(&[])), section);
}

#[test]
fn a_branch_already_cherry_picked_into_head_is_not_listed() {
    let fixture = Fixture::new();
    let markdown = fixture.pack();
    let section = open_work(&markdown);
    assert!(row(section, "cut/picked").is_none(), "{section}");
    assert!(!section.contains("fix(picked)"), "{section}");
    // Nothing of its own is not open work either.
    assert!(row(section, "cut/merged").is_none(), "{section}");
}

#[test]
fn an_inherited_git_dir_does_not_change_the_open_work_section() {
    let fixture = Fixture::new();
    // A decoy repository with its own stranded branch: if GIT_DIR leaked into
    // the queries, its branch (or its HEAD) would replace the fixture's.
    let decoy = fixture.root.join("decoy");
    fs::create_dir_all(&decoy).unwrap();
    let date = "2026-10-20T10:00:00+00:00";
    fixture.git(&decoy, &["init", "--quiet", "-b", "decoy-main"], date);
    fixture.commit(&decoy, "decoy.txt", "chore: decoy base", date);
    fixture.git(&decoy, &["branch", "decoy/stranded"], date);
    fixture.git(&decoy, &["checkout", "--quiet", "decoy/stranded"], date);
    fixture.commit(&decoy, "stranded.txt", "feat(decoy): stranded", date);
    fixture.git(&decoy, &["checkout", "--quiet", "decoy-main"], date);

    let clean = fixture.cli(&[]);
    let leaked = fixture.cli(&[("GIT_DIR", &decoy.join(".git")), ("GIT_WORK_TREE", &decoy)]);
    assert_eq!(open_work(&leaked), open_work(&clean));
    assert!(!leaked.contains("decoy"), "{}", open_work(&leaked));
    assert!(row(open_work(&leaked), "cut/alpha").is_some());
}

/// The fleet's own integration shape: the Operator re-applies a cut onto a
/// moved integration branch, the hunk context changes, the patch id changes,
/// and `git cherry` keeps calling finished work open. The row stays (it IS
/// open by patch identity) but names the HEAD commit carrying the same
/// subject and `session_id` trailer.
#[test]
fn a_cut_reapplied_with_changed_context_names_its_head_commit() {
    let fixture = Fixture::new();
    let (repo, date) = (&fixture.repo, "2026-10-20T12:00:00+00:00");
    let session = "cccccccc-1111-4222-8333-444444444444";
    let message = format!(
        "feat(gamma): reworked on integration\n\nAuthored-By: omp <agents@vetcoders.io>\nsession_id: {session}"
    );
    fixture.git(repo, &["branch", "cut/gamma", "integrate"], date);
    let gamma = fixture.root.join("wt-gamma");
    let gamma_path = gamma.to_string_lossy().into_owned();
    fixture.git(
        repo,
        &["worktree", "add", "--quiet", &gamma_path, "cut/gamma"],
        date,
    );
    fixture.commit(&gamma, "gamma.txt", &message, date);
    // Same subject and session, different patch: integrate already moved.
    fs::write(repo.join("gamma.txt"), "context the cut never saw\n").unwrap();
    fixture.git(repo, &["add", "--", "gamma.txt"], date);
    fixture.git(repo, &["commit", "--quiet", "-m", &message], date);
    let reapplied = fixture.git(repo, &["rev-parse", "HEAD"], date);

    let markdown = fixture.pack();
    let section = open_work(&markdown);
    let gamma = row(section, "cut/gamma").unwrap_or_else(|| panic!("{section}"));
    assert!(gamma.contains("open=1"), "{gamma}");
    assert!(
        gamma.contains(&format!("newest_reapplied_in_head={}", &reapplied[..12])),
        "{gamma}"
    );
    // Work never re-applied carries no such claim.
    let beta = row(section, "cut/beta").unwrap_or_else(|| panic!("{section}"));
    assert!(!beta.contains("newest_reapplied_in_head"), "{beta}");
}

#[test]
fn a_branch_over_the_commit_budget_is_named_not_dropped() {
    let fixture = Fixture::new();
    // Scanned from the linked worktree: the scan must fold into the
    // principal checkout and compare against ITS HEAD, not cut/alpha.
    let scan = aicx::open_work::scan_checkout(
        &fixture.alpha_worktree,
        aicx::open_work::Caps {
            branches: aicx::open_work::BRANCH_CAP,
            commits: 1,
        },
    );
    assert_eq!(scan.root, fs::canonicalize(&fixture.repo).unwrap());
    assert_eq!(scan.head_branch.as_deref(), Some("integrate"));
    assert_eq!(
        scan.over_budget,
        [("cut/alpha".to_string(), 2), ("cut/picked".to_string(), 1)]
    );
    let rows: Vec<&str> = scan.rows.iter().map(|row| row.branch.as_str()).collect();
    assert_eq!(rows, ["cut/beta"]);
    assert_eq!(scan.commits_examined, 1);
}

#[test]
fn branch_cap_truncation_is_named_in_the_honesty_preface() {
    let fixture = Fixture::new();
    let date = "2026-10-19T08:00:00+00:00";
    for index in 0..52 {
        fixture.git(
            &fixture.repo,
            &["branch", &format!("parked/{index:02}"), "integrate~1"],
            date,
        );
    }
    let markdown = fixture.pack();
    let line = honesty(&markdown);
    for expected in ["branches_examined=50/56", "branch_cap_reached=yes"] {
        assert!(line.contains(expected), "missing {expected}: {line}");
    }
    // Newest-first examination keeps the fresh cuts inside the cap, and the
    // section itself says what the cap left unexamined.
    let section = open_work(&markdown);
    assert!(
        section.contains("- 6 of 56 local branch(es) not examined"),
        "{section}"
    );
    assert!(row(section, "cut/alpha").is_some(), "{section}");
    assert!(row(section, "cut/beta").is_some(), "{section}");
}
