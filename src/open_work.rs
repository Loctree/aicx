//! OPEN WORK — local branches carrying commits their checkout's HEAD lacks.
//!
//! On 2026-10-21 four finished fleet cuts sat for 30 hours on local branches
//! held by worktrees, absent from the integration branch, and no surface said
//! so. The continuity pack now does: for the filtered repository, every local
//! branch with commits HEAD does not carry by patch identity (`git cherry`),
//! the worktree holding it, and the newest such commit's subject, author date,
//! `Authored-By` and `session_id` trailers. Session ids are printed, never
//! resolved here.
//!
//! Read-only, local git only, bounded: [`BRANCH_CAP`] branches and
//! [`COMMIT_CAP`] commits examined per pack, across at most [`CHECKOUT_CAP`]
//! checkouts. Every git call goes through
//! [`crate::git_env::git_command_isolated`], so an inherited `GIT_DIR` cannot
//! point a query at another repository.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aicx_parser::engine::{WorkdirIdentity, normalize_workdir};
use chrono::{DateTime, Utc};

/// Local branches examined per pack, newest committer date first.
pub const BRANCH_CAP: usize = 50;
/// Commits `git cherry` may examine per pack.
pub const COMMIT_CAP: usize = 200;
/// Principal checkouts examined per pack, freshest recorded session first.
pub const CHECKOUT_CAP: usize = 4;

/// Examination budget for one checkout scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub branches: usize,
    pub commits: usize,
}

impl Caps {
    pub const DEFAULT: Self = Self {
        branches: BRANCH_CAP,
        commits: COMMIT_CAP,
    };
}

/// Open work for one pack: the filtered projects' checkouts on this host.
#[derive(Debug, Clone, Default)]
pub struct OpenWork {
    /// Filtered projects. Empty for an unfiltered pack: nothing is computed,
    /// because branches only compare within one repository.
    pub projects: Vec<String>,
    pub checkouts: Vec<CheckoutOpenWork>,
    /// Resolved checkouts left unexamined at [`CHECKOUT_CAP`].
    pub checkouts_not_examined: usize,
    /// Recorded workdirs that resolve to no git checkout on this host.
    pub workdirs_unresolved: usize,
    /// Resolved checkouts whose `origin` names another project.
    pub checkouts_foreign: usize,
    /// The catalog could not be read, so no checkout was resolved.
    pub catalog_error: Option<String>,
}

/// One principal checkout, linked worktrees folded in.
#[derive(Debug, Clone, Default)]
pub struct CheckoutOpenWork {
    pub root: PathBuf,
    /// Short name of HEAD's branch; `None` when detached or unread.
    pub head_branch: Option<String>,
    /// `None` when HEAD did not resolve: nothing else was examined.
    pub head_sha: Option<String>,
    /// Local branches other than HEAD's own.
    pub branches_total: usize,
    pub branches_examined: usize,
    pub commits_examined: usize,
    /// Branches ahead of HEAD by SHA whose commits did not fit the remaining
    /// commit budget, with that count. Their patch identity was not checked.
    pub over_budget: Vec<(String, usize)>,
    /// Newest open commit first.
    pub rows: Vec<OpenBranch>,
    /// `git worktree list` failed: no row can claim it is unheld.
    pub worktrees_unknown: bool,
    pub git_errors: usize,
    pub first_git_error: Option<String>,
}

/// A local branch carrying commits HEAD lacks by patch identity.
#[derive(Debug, Clone)]
pub struct OpenBranch {
    pub branch: String,
    pub worktree: Option<HeldBy>,
    /// Commits HEAD lacks by patch identity (`git cherry` `+`).
    pub open_commits: usize,
    /// Commits whose patch HEAD already carries under another SHA (`-`).
    pub integrated_commits: usize,
    pub newest: CommitFacts,
    /// A HEAD commit since the fork whose message carries the newest commit's
    /// subject and `session_id` trailer: a pick re-applied with changed context
    /// or a squash merge quoting it, both invisible to patch identity. A hint,
    /// not proof of integration.
    pub newest_reapplied_in_head: Option<String>,
}

/// The worktree holding a branch, as `git worktree list` names it.
#[derive(Debug, Clone)]
pub struct HeldBy {
    pub path: PathBuf,
    /// Git marks the worktree prunable: its directory is gone.
    pub prunable: bool,
}

#[derive(Debug, Clone)]
pub struct CommitFacts {
    pub sha: String,
    pub subject: String,
    /// Strict ISO 8601 author date as recorded (`%aI`).
    pub author_date: String,
    /// Unix author time, for ordering only.
    pub author_time: i64,
    pub authored_by: Vec<String>,
    pub session_ids: Vec<String>,
}

/// Resolve the filtered projects' checkouts from the catalog's recorded
/// workdirs and examine them under one shared budget.
///
/// Never fails the pack: an unreadable catalog, a vanished checkout or a git
/// error is counted and rendered, not raised. Recorded workdirs resolve
/// through [`normalize_workdir`], which folds a linked worktree into its
/// principal checkout — the HEAD that matters is the one fleet worktrees
/// branch from, not the HEAD of whichever worktree a session ran in.
pub fn collect(aicx_home: &Path, projects: &[String]) -> OpenWork {
    let mut open = OpenWork {
        projects: projects.to_vec(),
        ..OpenWork::default()
    };
    if projects.is_empty() {
        return open;
    }
    let entries = match crate::catalog::read_entries_at(aicx_home) {
        Ok(entries) => entries,
        Err(error) => {
            open.catalog_error = Some(format!("{error:#}"));
            return open;
        }
    };
    let in_scope = |project: &str| {
        projects
            .iter()
            .any(|wanted| wanted.eq_ignore_ascii_case(project))
    };
    // Newest recorded day per distinct workdir, then per principal checkout.
    let mut workdirs: BTreeMap<&str, Option<&str>> = BTreeMap::new();
    for entry in &entries {
        if !entry.project.as_deref().is_some_and(in_scope) {
            continue;
        }
        let Some(cwd) = entry
            .cwd
            .as_deref()
            .map(str::trim)
            .filter(|cwd| !cwd.is_empty())
        else {
            continue;
        };
        let newest = workdirs.entry(cwd).or_default();
        if entry.date.as_deref() > *newest {
            *newest = entry.date.as_deref();
        }
    }
    let mut roots: BTreeMap<PathBuf, Option<&str>> = BTreeMap::new();
    for (cwd, date) in workdirs {
        // No baseline: a relative workdir must not resolve against this
        // process's cwd, which is not where the session ran.
        match normalize_workdir(cwd, None) {
            WorkdirIdentity::Resolved(root) => {
                let newest = roots.entry(root).or_default();
                if date > *newest {
                    *newest = date;
                }
            }
            WorkdirIdentity::Unresolved(_) => open.workdirs_unresolved += 1,
        }
    }
    let mut roots: Vec<(PathBuf, Option<&str>)> = roots.into_iter().collect();
    roots.sort_by(|(path_a, a), (path_b, b)| b.cmp(a).then_with(|| path_a.cmp(path_b)));

    let mut budget = Caps::DEFAULT;
    for (root, _) in roots {
        if open.checkouts.len() >= CHECKOUT_CAP {
            open.checkouts_not_examined += 1;
            continue;
        }
        if origin_names_another_project(&root, projects) {
            open.checkouts_foreign += 1;
            continue;
        }
        let checkout = scan_checkout(&root, budget);
        budget.branches = budget.branches.saturating_sub(checkout.branches_examined);
        budget.commits = budget.commits.saturating_sub(checkout.commits_examined);
        open.checkouts.push(checkout);
    }
    open
}

/// A checkout whose `origin` spells another project is a different
/// repository wearing a recorded workdir, not this project's open work. No
/// readable origin keeps the catalog's attribution.
fn origin_names_another_project(root: &Path, projects: &[String]) -> bool {
    let Ok(url) = git(root, &["remote", "get-url", "origin"]) else {
        return false;
    };
    crate::catalog::project_slug_from_remote(url.trim()).is_some_and(|slug| {
        !projects
            .iter()
            .any(|project| project.eq_ignore_ascii_case(&slug))
    })
}

/// Examine one checkout: every local branch other than HEAD's own, newest
/// committer date first, within `caps`.
///
/// `checkout` may be a linked worktree or a subdirectory; it is folded into
/// its principal checkout first. Per branch: `rev-list --count` (a graph walk)
/// decides whether anything is ahead by SHA; only then does `git cherry`
/// spend commit budget on patch identity. A branch that does not fit the
/// remaining budget is named with its SHA count, never silently dropped.
pub fn scan_checkout(checkout: &Path, caps: Caps) -> CheckoutOpenWork {
    let root = match normalize_workdir(&checkout.to_string_lossy(), None) {
        WorkdirIdentity::Resolved(root) => root,
        WorkdirIdentity::Unresolved(_) => checkout.to_path_buf(),
    };
    let mut scan = CheckoutOpenWork {
        root,
        ..CheckoutOpenWork::default()
    };
    let root = scan.root.clone();
    match git(&root, &["rev-parse", "--verify", "HEAD^{commit}"]) {
        Ok(sha) => scan.head_sha = Some(sha.trim().to_string()),
        Err(error) => {
            scan.note_error(error);
            return scan;
        }
    }
    let head_ref = git(&root, &["symbolic-ref", "--quiet", "HEAD"])
        .ok()
        .map(|reference| reference.trim().to_string());
    scan.head_branch = head_ref.as_deref().map(|reference| {
        reference
            .strip_prefix("refs/heads/")
            .unwrap_or(reference)
            .to_string()
    });

    let held = match git(&root, &["worktree", "list", "--porcelain", "-z"]) {
        Ok(raw) => parse_worktrees(&raw),
        Err(error) => {
            scan.worktrees_unknown = true;
            scan.note_error(error);
            BTreeMap::new()
        }
    };
    let branches = match git(
        &root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)",
            "refs/heads",
        ],
    ) {
        Ok(raw) => raw,
        Err(error) => {
            scan.note_error(error);
            return scan;
        }
    };
    let branches: Vec<&str> = branches
        .lines()
        .map(str::trim)
        .filter(|reference| !reference.is_empty() && Some(*reference) != head_ref.as_deref())
        .collect();
    scan.branches_total = branches.len();

    for reference in branches.into_iter().take(caps.branches) {
        scan.branches_examined += 1;
        let name = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        let ahead = match git(
            &root,
            &[
                "rev-list",
                "--count",
                "--no-merges",
                &format!("HEAD..{reference}"),
            ],
        )
        .and_then(|count| {
            count
                .trim()
                .parse::<usize>()
                .map_err(|error| format!("rev-list --count {name}: {error}"))
        }) {
            Ok(ahead) => ahead,
            Err(error) => {
                scan.note_error(error);
                continue;
            }
        };
        if ahead == 0 {
            continue;
        }
        if scan.commits_examined + ahead > caps.commits {
            scan.over_budget.push((name.to_string(), ahead));
            continue;
        }
        let cherry = match git(&root, &["cherry", "HEAD", reference]) {
            Ok(cherry) => cherry,
            Err(error) => {
                scan.note_error(error);
                continue;
            }
        };
        // Oldest first: the last `+` is the newest commit HEAD lacks.
        let (mut open, mut integrated, mut newest) = (0, 0, None);
        for line in cherry.lines() {
            match line.trim().split_once(' ') {
                Some(("+", sha)) => {
                    open += 1;
                    newest = Some(sha.trim());
                }
                Some(("-", _)) => integrated += 1,
                _ => {}
            }
        }
        scan.commits_examined += open + integrated;
        let Some(sha) = newest else {
            continue;
        };
        match commit_facts(&root, sha) {
            Ok(newest) => scan.rows.push(OpenBranch {
                branch: name.to_string(),
                worktree: held.get(reference).cloned(),
                open_commits: open,
                integrated_commits: integrated,
                newest_reapplied_in_head: reapplied_in_head(&root, reference, &newest),
                newest,
            }),
            Err(error) => scan.note_error(error),
        }
    }
    scan.rows.sort_by(|a, b| {
        b.newest
            .author_time
            .cmp(&a.newest.author_time)
            .then_with(|| a.branch.cmp(&b.branch))
    });
    scan
}

/// The newest HEAD commit since `reference` forked whose message carries both
/// `commit`'s subject and its `session_id` trailer.
///
/// The fleet integrates cuts onto a moving branch: a re-applied commit keeps
/// its message (the hooks' `session_id` and `time` trailers included) while
/// changed hunk context gives it a new patch id, so `git cherry` keeps calling
/// finished work open. A squash merge quotes the squashed messages, trailers
/// included, and hides the same work the same way. One bounded `git log` per
/// row, and only for commits that carry a session id — a subject alone is too
/// weak an identity.
fn reapplied_in_head(root: &Path, reference: &str, commit: &CommitFacts) -> Option<String> {
    let session_id = commit.session_ids.first()?;
    let found = git(
        root,
        &[
            "log",
            "-1",
            "--format=%H",
            "--fixed-strings",
            "--all-match",
            &format!("--grep={session_id}"),
            &format!("--grep={}", commit.subject),
            &format!("{reference}..HEAD"),
            "--",
        ],
    )
    .ok()?;
    Some(found.trim().to_string()).filter(|sha| !sha.is_empty())
}

impl CheckoutOpenWork {
    fn note_error(&mut self, error: String) {
        self.git_errors += 1;
        self.first_git_error.get_or_insert(error);
    }
}

fn commit_facts(root: &Path, sha: &str) -> Result<CommitFacts, String> {
    let raw = git(
        root,
        &[
            "log",
            "-1",
            "--no-show-signature",
            "--format=%H%x00%s%x00%aI%x00%at%x00%B",
            sha,
            "--",
        ],
    )?;
    let mut fields = raw.splitn(5, '\0');
    let mut next = || fields.next().unwrap_or_default();
    let (sha, subject, author_date, author_time, message) =
        (next(), next(), next(), next(), next());
    let (authored_by, session_ids) = trailers(message);
    Ok(CommitFacts {
        sha: sha.trim().to_string(),
        subject: subject.to_string(),
        author_date: author_date.to_string(),
        author_time: author_time.trim().parse().unwrap_or_default(),
        authored_by,
        session_ids,
    })
}

/// `Authored-By` and `session_id` values from the message's closing
/// paragraph, read by hand.
///
/// Git's trailer parser accepts only alphanumerics and `-` in a key, so the
/// hooks' `session_id:` line disqualifies the whole block and `%(trailers)`
/// answers with nothing for every commit this repository writes. A message
/// with no paragraph after its subject has no trailer block at all.
fn trailers(message: &str) -> (Vec<String>, Vec<String>) {
    let (mut authored_by, mut session_ids) = (Vec::new(), Vec::new());
    let Some((_, block)) = message.trim_end().rsplit_once("\n\n") else {
        return (authored_by, session_ids);
    };
    for line in block.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "authored_by" => authored_by.push(value.to_string()),
            "session_id" => session_ids.push(value.to_string()),
            _ => {}
        }
    }
    (authored_by, session_ids)
}

/// `refs/heads/<name>` → the worktree holding it, from `--porcelain -z`.
fn parse_worktrees(raw: &str) -> BTreeMap<String, HeldBy> {
    let mut held = BTreeMap::new();
    let (mut path, mut branch, mut prunable) = (None::<PathBuf>, None::<String>, false);
    // Records end with an empty field; the last one may not.
    for field in raw.split('\0').chain(std::iter::once("")) {
        if field.is_empty() {
            if let (Some(path), Some(branch)) = (path.take(), branch.take()) {
                held.insert(branch, HeldBy { path, prunable });
            }
            (path, branch, prunable) = (None, None, false);
        } else if let Some(value) = field.strip_prefix("worktree ") {
            let value = PathBuf::from(value);
            // Canonical like the checkout identity; a pruned path stays as named.
            path = Some(std::fs::canonicalize(&value).unwrap_or(value));
        } else if let Some(value) = field.strip_prefix("branch ") {
            branch = Some(value.to_string());
        } else if field == "prunable" || field.starts_with("prunable ") {
            prunable = true;
        }
    }
    held
}

/// Read-only git in `dir`, hermetic env, no color, no optional locks.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let verb = args.first().copied().unwrap_or("git");
    let output = crate::git_env::git_command_isolated()
        .arg("-C")
        .arg(dir)
        .args(["-c", "color.ui=false"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|error| format!("git {verb}: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git {verb}: {}",
            stderr.lines().next().unwrap_or("non-zero exit").trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn printed(path: &Path) -> String {
    aicx_parser::sanitize::strip_verbatim_prefix(path)
        .to_string_lossy()
        .into_owned()
}

/// One atomic line: embedded newlines never detach a field from its row.
fn one_line(text: &str) -> String {
    text.replace('\r', "\\r").replace('\n', "\\n")
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn listed(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        one_line(&values.join(", "))
    }
}

/// The HONESTY preface line: where checkouts came from, what the bounds cut,
/// and what patch identity cannot prove.
pub fn honesty_line(open: &OpenWork) -> String {
    if open.projects.is_empty() {
        return "open work: not computed for an unfiltered pack; branches compare within one repository.\n".to_string();
    }
    let examined: usize = open.checkouts.iter().map(|c| c.branches_examined).sum();
    let total: usize = open.checkouts.iter().map(|c| c.branches_total).sum();
    let commits: usize = open.checkouts.iter().map(|c| c.commits_examined).sum();
    let over: usize = open.checkouts.iter().map(|c| c.over_budget.len()).sum();
    let errors: usize = open.checkouts.iter().map(|c| c.git_errors).sum();
    let cap_reached = open
        .checkouts
        .iter()
        .any(|c| c.branches_examined < c.branches_total);
    format!(
        "open work: local git only, read-only, hermetic env; checkouts come from the filtered project's recorded workdirs, linked worktrees folded into their principal · checkouts_examined={} · checkouts_not_examined={} (cap={CHECKOUT_CAP}) · recorded_workdirs_unresolved={} · foreign_checkouts={} · branches_examined={examined}/{total} (cap={BRANCH_CAP}) · commits_examined={commits} (cap={COMMIT_CAP}) · over_commit_budget={over} · branch_cap_reached={} · git_errors={errors}{}. A listed branch lacks those commits by patch identity (git cherry): a squash merge or a reworked pick still lists it, and an unexamined branch is not proven integrated. newest_reapplied_in_head names a HEAD commit whose message carries the newest commit's subject and session_id trailer (a re-applied pick or a squash merge quoting it): a hint, not proof of integration.\n",
        open.checkouts.len(),
        open.checkouts_not_examined,
        open.workdirs_unresolved,
        open.checkouts_foreign,
        if cap_reached { "yes" } else { "no" },
        if open.catalog_error.is_some() {
            " · catalog unreadable"
        } else {
            ""
        },
    )
}

/// The OPEN WORK section. Empty for an unfiltered pack.
///
/// `inject_not_before` is set only for `--for-inject`. `None` lists every
/// open branch, byte for byte with the unbudgeted section. `Some` is the
/// pack window start: a branch whose newest open commit is strictly older
/// than that instant folds into one summary line, unless a worktree holds
/// it. A commit at or after the start stays listed, including one later
/// than the window end. That commit is not stale, and the inject character
/// budget still truncates the section.
pub fn render_section(open: &OpenWork, inject_not_before: Option<DateTime<Utc>>) -> String {
    if open.projects.is_empty() {
        return String::new();
    }
    let mut out = String::from("## OPEN WORK\n\n");
    if let Some(error) = &open.catalog_error {
        out.push_str(&format!(
            "- catalog unreadable; open work not measured: {}\n\n",
            one_line(error)
        ));
        return out;
    }
    if open.checkouts.is_empty() {
        out.push_str(&format!(
            "- no local checkout of {} resolved on this host (recorded workdirs unresolved={}, foreign checkouts={}); open work not measured\n\n",
            one_line(&open.projects.join(", ")),
            open.workdirs_unresolved,
            open.checkouts_foreign
        ));
        return out;
    }
    for checkout in &open.checkouts {
        let root = printed(&checkout.root);
        let Some(head) = checkout.head_sha.as_deref() else {
            out.push_str(&format!(
                "- checkout {root} · git failed: {}; open work not measured here\n",
                one_line(
                    checkout
                        .first_git_error
                        .as_deref()
                        .unwrap_or("unknown error")
                )
            ));
            continue;
        };
        out.push_str(&format!(
            "- checkout {root} · HEAD={} @ {} · branches_examined={}/{} · commits_examined={}\n",
            checkout.head_branch.as_deref().unwrap_or("detached"),
            short(head),
            checkout.branches_examined,
            checkout.branches_total,
            checkout.commits_examined
        ));
        let mut folded: Vec<&OpenBranch> = Vec::new();
        for row in &checkout.rows {
            if branch_listed_for_inject(checkout, row, inject_not_before) {
                out.push_str(&branch_row(checkout, row));
            } else {
                folded.push(row);
            }
        }
        if !folded.is_empty() {
            out.push_str(&fold_summary(&folded));
        }
        for (branch, ahead) in &checkout.over_budget {
            out.push_str(&format!(
                "- {} · not examined: {ahead} commit(s) ahead by SHA exceed the remaining commit budget; patch identity not checked\n",
                one_line(branch)
            ));
        }
        // A checkout the shared budget never reached must not read as clean:
        // "no examined branch" over zero examined branches is a quiet lie.
        let unexamined = checkout.branches_total - checkout.branches_examined;
        if unexamined > 0 {
            out.push_str(&format!(
                "- {unexamined} of {} local branch(es) not examined: the pack's branch budget (cap={BRANCH_CAP}, newest committer date first) ran out; they are not proven integrated\n",
                checkout.branches_total
            ));
        }
        if checkout.branches_total == 0 {
            out.push_str("- no local branch besides HEAD's own\n");
        } else if checkout.branches_examined > 0
            && checkout.rows.is_empty()
            && checkout.over_budget.is_empty()
        {
            out.push_str(&format!(
                "- none of the {} examined branch(es) carries commits HEAD lacks by patch identity\n",
                checkout.branches_examined
            ));
        }
        if let Some(error) = &checkout.first_git_error {
            out.push_str(&format!(
                "- git errors: {} (first: {})\n",
                checkout.git_errors,
                one_line(error)
            ));
        }
    }
    out.push('\n');
    out
}

fn branch_row(checkout: &CheckoutOpenWork, row: &OpenBranch) -> String {
    let worktree = match &row.worktree {
        Some(held) => format!(
            "{}{}",
            printed(&held.path),
            if held.prunable {
                " (prunable: directory gone)"
            } else {
                ""
            }
        ),
        None if checkout.worktrees_unknown => "unknown".to_string(),
        None => "none".to_string(),
    };
    format!(
        "- {} · open={} · already_in_head={} · newest={}{} · author_date={} · Authored-By={} · session_id={} · worktree={} · subject={}\n",
        one_line(&row.branch),
        row.open_commits,
        row.integrated_commits,
        short(&row.newest.sha),
        row.newest_reapplied_in_head
            .as_deref()
            .map(|sha| format!(" · newest_reapplied_in_head={}", short(sha)))
            .unwrap_or_default(),
        one_line(&row.newest.author_date),
        listed(&row.newest.authored_by),
        listed(&row.newest.session_ids),
        one_line(&worktree),
        one_line(&row.newest.subject)
    )
}

/// `None` lists every row. A worktree hold, or an unknown worktree list,
/// stays listed: the section must not claim a branch is unheld when git
/// could not say so. An unreadable author date is not proven stale.
fn branch_listed_for_inject(
    checkout: &CheckoutOpenWork,
    row: &OpenBranch,
    inject_not_before: Option<DateTime<Utc>>,
) -> bool {
    let Some(not_before) = inject_not_before else {
        return true;
    };
    if checkout.worktrees_unknown || row.worktree.is_some() {
        return true;
    }
    match DateTime::parse_from_rfc3339(row.newest.author_date.trim()) {
        Ok(author) => author.with_timezone(&Utc) >= not_before,
        Err(_) => true,
    }
}

/// One line for the branches older than the pack window: how many, the
/// newest author date among them, and how many carry a reapplied-in-HEAD hint.
fn fold_summary(rows: &[&OpenBranch]) -> String {
    let Some(newest) = rows.iter().max_by(|left, right| {
        left.newest
            .author_time
            .cmp(&right.newest.author_time)
            .then_with(|| left.newest.author_date.cmp(&right.newest.author_date))
            .then_with(|| left.branch.cmp(&right.branch))
    }) else {
        return String::new();
    };
    let reapplied = rows
        .iter()
        .filter(|row| row.newest_reapplied_in_head.is_some())
        .count();
    format!(
        "- {} branch(es) outside the pack window · newest_author_date={} · newest_reapplied_in_head={reapplied}\n",
        rows.len(),
        one_line(&newest.newest.author_date),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo hooks write `session_id:`; git's trailer parser rejects the
    /// underscore and with it the whole block, so this reader must not.
    #[test]
    fn trailers_are_read_from_the_closing_paragraph_including_session_id() {
        let message = "feat(x): subject\n\n- body: with a colon\n\nAuthored-By: claude <agents@vetcoders.io>\nsession_id: 0193-abc\ntime: 2026-10-21T09:11:42+02:00\nruntime: local-worktrees\n";
        let (authored_by, session_ids) = trailers(message);
        assert_eq!(authored_by, ["claude <agents@vetcoders.io>"]);
        assert_eq!(session_ids, ["0193-abc"]);

        // A body paragraph is not the trailer block, and a subject alone has none.
        let (authored_by, session_ids) =
            trailers("feat: x\n\nAuthored-By: a\n\n- closing bullet: y\n");
        assert!(authored_by.is_empty() && session_ids.is_empty());
        let (authored_by, _) = trailers("Authored-By: subject only");
        assert!(authored_by.is_empty());
    }

    #[test]
    fn porcelain_worktrees_map_branches_and_flag_prunable_ones() {
        let raw = "worktree /nowhere/principal\0HEAD 1111\0branch refs/heads/integrate\0\0worktree /nowhere/wt\0HEAD 2222\0branch refs/heads/cut/a\0prunable gitdir file points to non-existent location\0\0worktree /nowhere/detached\0HEAD 3333\0detached\0\0";
        let held = parse_worktrees(raw);
        assert_eq!(held.len(), 2);
        let cut = &held["refs/heads/cut/a"];
        assert_eq!(cut.path, PathBuf::from("/nowhere/wt"));
        assert!(cut.prunable);
        assert!(!held["refs/heads/integrate"].prunable);
    }
}
