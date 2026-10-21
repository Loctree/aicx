#![cfg(feature = "app")]
//! W3-01 cold start: the morning of 2026-10-21 rebuilt as one fixture, read
//! back through the one call `docs/coldstart.md` teaches.
//!
//! That morning a fresh agent (Andrzej, channel 5) needed about 45 tool calls
//! and a human hint to find four things: four finished fleet cuts sitting on
//! local branches HEAD lacked, worker sessions whose dispatch brief read as
//! human speech, the Founder's assignment on the Codescribe bus under another
//! project, and old junie sessions that made every 72-hour `utterances` window
//! refuse. This fixture holds all four; the tests run the real binary.
use aicx::catalog::{CATALOG_SCHEMA, CatalogEntry};
use chrono::Utc;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// The cold-start call `docs/coldstart.md` teaches, verbatim. The tests run
/// exactly this argv with the placeholders filled and pin the doc to it.
const COLDSTART_COMMAND: &str = "aicx continuity show -p <org/repo> --as <name> -H 72 --for-inject";
const PROJECT: &str = "Loctree/aicx";
const OPERATOR: &str = "Andrzej";
/// Frozen clock: the morning the new Andrzej started cold.
const NOW: &str = "2026-10-21T06:00:00Z";
const INTEGRATION: &str = "agent/release-0.15.0-integrate";
const INJECT_BUDGET: usize = 24_000;

/// The operator agent's own interactive session, typed into by Maciej.
const OPERATOR_SESSION: &str = "a2db0000-1021-4052-8000-000000000005";
/// The previous Andrzej, cataloged under another project: the bus take
/// names this session as its recipient.
const PREVIOUS_OPERATOR_SESSION: &str = "a2db0000-1020-4210-8000-000000000004";
const FOUNDER_TAKE: &str = "Andrzej, przydział: nowy Andrzej ląduje na aicx. Cztery cięcia z wczoraj czekają na integrację.";
const JOHN_TAKE: &str = "John, zrób cargo clean na hoście buildowym.";
const EXPIRED_TAKE: &str = "Andrzej, stary przydział sprzed okna.";
const HUMAN_TODAY: &str =
    "Dzień dobry Andrzej. Zanim cokolwiek ruszysz, sprawdź co zostało po wczorajszej flocie.";
/// Every brief carries a decision-shaped line. A brief prescribes work; this
/// line must never surface as anyone's decision.
const BRIEF_DECOY: &str = "BRIEF-DECOY";
/// Old junie sessions from April to September: unreadable whole, and older
/// than any 72-hour window.
const OLD_JUNIE: [(&str, &str); 3] = [
    ("260415-134927-14mz", "2026-04-15T13:49:27Z"),
    ("260710-015110-zq5c", "2026-07-10T01:51:10Z"),
    ("260916-081500-ab12", "2026-09-16T08:15:00Z"),
];

/// One fleet worker from 2026-10-19 23:00: a headless claude session that
/// opened with the launcher envelope and left its cut on a local branch.
struct Worker {
    run: &'static str,
    prompt: &'static str,
    area: &'static str,
    subject: &'static str,
    session: &'static str,
    tasks: [&'static str; 6],
}

const WORKERS: [Worker; 4] = [
    Worker {
        run: "work-261019-223801-41101",
        prompt: "P1",
        area: "parser",
        subject: "classify shell tool calls as agent shell actions",
        session: "c0d10001-1019-4238-8001-000000000001",
        tasks: [
            "attach a shell action frame class to every Bash tool_use block in the claude adapter",
            "attach the same frame class to exec_command tool calls in the codex adapter",
            "keep the canonical fingerprint stable for fixtures that already pin a SHA256",
            "fold shell results into the preceding tool call before the conversation projection",
            "add adapter tests for a Bash call, an exec_command call and a plain assistant reply",
            "run cargo test for the parser crate and paste the result line into the report",
        ],
    },
    Worker {
        run: "work-261019-223812-41102",
        prompt: "P2",
        area: "mcp",
        subject: "forward distill_mixed through the MCP continuity tool",
        session: "c0d10002-1019-4238-8002-000000000002",
        tasks: [
            "add a defaulted distill_mixed parameter to the MCP continuity parameters",
            "forward the parameter to the continuity request instead of a hardcoded false",
            "name the switch in the mixed-workstream refusal under both spellings",
            "prove CLI and MCP render the same pack for one mixed-only window",
            "keep the default pack unchanged when the parameter is absent",
            "run the continuity pipeline tests and paste the result line into the report",
        ],
    },
    Worker {
        run: "work-261019-223823-41103",
        prompt: "P3",
        area: "service",
        subject: "verify the launchd service is loaded after bootstrap",
        session: "c0d10003-1019-4238-8003-000000000003",
        tasks: [
            "after bootstrap with retry, print the gui domain service and require it loaded",
            "verify the legacy label the same way when the legacy service was loaded",
            "fail the repair script loudly when the service is still missing",
            "cover both labels in the repair self-test with a fake launchctl",
            "keep the Linux service installer untouched by this cut",
            "run the repair self-test and paste the result line into the report",
        ],
    },
    Worker {
        run: "work-261019-223834-41104",
        prompt: "P4",
        area: "catalog",
        subject: "attribute sessions to the repository their worktree belongs to",
        session: "c0d10004-1019-4238-8004-000000000004",
        tasks: [
            "fold a linked worktree workdir into its principal checkout before project lookup",
            "read the origin remote of the principal checkout, not of the worktree directory",
            "keep sessions run in a removed worktree attributed by their recorded project",
            "add a catalog test with one principal checkout and two linked worktrees",
            "leave catalog dates untouched; they stay non-authoritative",
            "run the catalog tests and paste the result line into the report",
        ],
    },
];

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Parallel tests and a microsecond macOS clock: pid + time alone can
        // hand two fixtures one root (c67b24b). A counter keeps them apart.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "aicx-coldstart-{}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".aicx")).unwrap();
        let fixture = Self { root };
        fixture.build_repository();
        let mut entries = Vec::new();
        for worker in &WORKERS {
            entries.push(fixture.write_worker_session(worker));
        }
        entries.push(fixture.write_operator_session());
        entries.push(fixture.write_previous_operator_session());
        for (id, at) in OLD_JUNIE {
            entries.push(fixture.write_old_junie_session(id, at));
        }
        fixture.write_catalog(&entries);
        fixture.write_bus();
        fixture
    }

    fn home(&self) -> PathBuf {
        self.root.join(".aicx")
    }

    fn principal(&self) -> PathBuf {
        self.root.join("vc-workspace/Loctree/aicx")
    }

    fn worktree(&self, worker: &Worker) -> PathBuf {
        self.root
            .join(".vibecrafted/worktrees/Loctree/aicx/2026_1019")
            .join(format!("claude-{}", worker.run))
    }

    fn junie_source(&self, id: &str) -> PathBuf {
        self.root
            .join(format!(".junie/sessions/session-{id}/events.jsonl"))
    }

    /// Fixture git goes through the hermetic constructor the product uses,
    /// with this machine's global/system config and hooks shut out.
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
        let path = dir.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{message}\n")).unwrap();
        self.git(dir, &["add", "--", file], date);
        self.git(dir, &["commit", "--quiet", "-m", message], date);
        self.git(dir, &["rev-parse", "HEAD"], date)
    }

    /// The principal checkout sits on the integration branch. Four cuts
    /// wait on branches held by fleet worktrees, a fifth was already
    /// cherry-picked onto the integration branch, and `main` carries
    /// nothing of its own.
    fn build_repository(&self) {
        let (repo, base) = (self.principal(), "2026-10-18T08:00:00+00:00");
        fs::create_dir_all(&repo).unwrap();
        self.git(&repo, &["init", "--quiet", "-b", "main"], base);
        self.git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/Loctree/aicx.git",
            ],
            base,
        );
        self.commit(&repo, "README.md", "chore: 0.15.0 base", base);
        let opened = "2026-10-19T08:00:00+00:00";
        self.git(&repo, &["checkout", "--quiet", "-b", INTEGRATION], opened);
        self.commit(
            &repo,
            "CHANGELOG.md",
            "chore(release): open the 0.15.0 integration branch",
            opened,
        );

        for (index, worker) in WORKERS.iter().enumerate() {
            let worktree = self.worktree(worker);
            let worktree_arg = worktree.to_string_lossy().into_owned();
            let branch = format!("cut/claude-{}", worker.run);
            self.git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "-b",
                    &branch,
                    &worktree_arg,
                    INTEGRATION,
                ],
                opened,
            );
            // Trailers exactly as the repo hooks write them: `session_id`
            // carries an underscore git's own trailer parser rejects.
            let date = format!("2026-10-19T22:{:02}:00+00:00", 50 + index);
            self.commit(
                &worktree,
                &format!("src/{}.rs", worker.area),
                &format!(
                    "[claude/workflow] fix({}): {} {}\n\n- {}\n\nAuthored-By: claude <agents@vetcoders.io>\nsession_id: {}\ntime: {date}\nruntime: local-worktrees",
                    worker.area, worker.prompt, worker.subject, worker.tasks[0], worker.session
                ),
                &date,
            );
        }

        // Already integrated under another SHA: same patch, picked onto HEAD.
        let picked = self.root.join("wt-integrated");
        let picked_arg = picked.to_string_lossy().into_owned();
        self.git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "cut/claude-work-261019-221500-41100",
                &picked_arg,
                INTEGRATION,
            ],
            opened,
        );
        let picked_sha = self.commit(
            &picked,
            "src/integrated.rs",
            "[claude/workflow] fix(intents): P0 already integrated",
            "2026-10-19T22:20:00+00:00",
        );
        self.git(&repo, &["worktree", "remove", &picked_arg], opened);
        self.git(
            &repo,
            &["cherry-pick", &picked_sha],
            "2026-10-19T23:10:00+00:00",
        );
    }

    fn claude_source(&self, cwd: &Path, session: &str) -> PathBuf {
        let slug = cwd.to_string_lossy().replace(['/', '\\', ':', '.'], "-");
        let path = self
            .root
            .join(".claude/projects")
            .join(slug)
            .join(format!("{session}.jsonl"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        path
    }

    fn write_claude_session(
        &self,
        session: &str,
        cwd: &Path,
        turns: &[(&str, &str, &str)],
    ) -> PathBuf {
        let path = self.claude_source(cwd, session);
        let cwd = cwd.to_string_lossy().into_owned();
        let mut body = String::new();
        for (index, (at, role, text)) in turns.iter().enumerate() {
            let content: Value = if *role == "assistant" {
                json!([{"type": "text", "text": text}])
            } else {
                json!(text)
            };
            let line = json!({
                "type": role,
                "uuid": format!("{session}-{index}"),
                "sessionId": session,
                "cwd": cwd,
                "timestamp": at,
                "message": {"role": role, "content": content}
            });
            body.push_str(&format!("{line}\n"));
        }
        fs::write(&path, body).unwrap();
        path
    }

    fn entry(
        &self,
        session: &str,
        agent: &str,
        project: &str,
        date: &str,
        cwd: &Path,
        source: &Path,
    ) -> CatalogEntry {
        CatalogEntry {
            schema: CATALOG_SCHEMA.into(),
            session_id: session.into(),
            agent: agent.into(),
            project: Some(project.into()),
            date: Some(date.into()),
            cwd: Some(cwd.to_string_lossy().into()),
            source_path: source.to_string_lossy().into(),
            source_len: None,
            source_mtime_ns: None,
            source_bundle_fingerprint: None,
            title: None,
            machine: None,
            logical_session_id: None,
            session_kind: None,
        }
    }

    /// The launcher envelope as the first user turn, the way a headless
    /// fleet worker opens; then the worker's own report.
    fn write_worker_session(&self, worker: &Worker) -> CatalogEntry {
        let worktree = self.worktree(worker);
        let checklist = worker
            .tasks
            .iter()
            .map(|task| {
                format!(
                    "- [ ] {task} — verifier: `cargo test --locked -p aicx --test {}_contract`\n",
                    worker.area
                )
            })
            .collect::<String>();
        let brief = format!(
            "You are running under Vibecrafted core runtime.\n\nContract:\n- Work in repository root: {}\n- Skill: vc-workflow\n- Agent request: claude\n- Mode: workflow\n- Runtime request: headless\n\nOperator prompt:\n---\nprompt_id: {}\nplan_id: release-0.15.0\n---\n\n# {} — {}\n\nDecision: {BRIEF_DECOY} the operator agent rules that this brief is Founder speech.\n\n## Acceptance\n\n{checklist}",
            worktree.display(),
            worker.prompt,
            worker.prompt,
            worker.subject
        );
        let report = format!(
            "Done: {} landed on cut/claude-{}. test result: ok. 12 passed; 0 failed.",
            worker.subject, worker.run
        );
        let source = self.write_claude_session(
            worker.session,
            &worktree,
            &[
                ("2026-10-19T22:38:00Z", "user", &brief),
                ("2026-10-19T22:49:00Z", "assistant", &report),
            ],
        );
        self.entry(
            worker.session,
            "claude",
            PROJECT,
            "2026-10-19",
            &worktree,
            &source,
        )
    }

    /// Andrzej's own interactive session this morning: Maciej typing.
    fn write_operator_session(&self) -> CatalogEntry {
        let repo = self.principal();
        let source = self.write_claude_session(
            OPERATOR_SESSION,
            &repo,
            &[
                ("2026-10-21T05:20:00Z", "user", HUMAN_TODAY),
                (
                    "2026-10-21T05:21:00Z",
                    "assistant",
                    "Reading the repository state before touching anything.",
                ),
                (
                    "2026-10-21T05:40:00Z",
                    "user",
                    "Decision: nothing merges to main without my button.",
                ),
                (
                    "2026-10-21T05:44:00Z",
                    "user",
                    "Decision: integrate yesterday's four cuts before any new wave starts.",
                ),
                (
                    "2026-10-21T05:48:00Z",
                    "user",
                    "Decision: every cold start begins with the continuity pack, not with a compact of yourself.",
                ),
            ],
        );
        self.entry(
            OPERATOR_SESSION,
            "claude",
            PROJECT,
            "2026-10-21",
            &repo,
            &source,
        )
    }

    /// The previous Andrzej worked under another project; the Founder's
    /// assignment reached him there, on the bus.
    fn write_previous_operator_session(&self) -> CatalogEntry {
        let cwd = self.root.join("vc-workspace/vetcoders/vibecrafted");
        fs::create_dir_all(&cwd).unwrap();
        let source = self.write_claude_session(
            PREVIOUS_OPERATOR_SESSION,
            &cwd,
            &[(
                "2026-10-20T20:00:00Z",
                "user",
                "Decision: keep the launcher envelope literal stable.",
            )],
        );
        self.entry(
            PREVIOUS_OPERATOR_SESSION,
            "claude",
            "Vetcoders/vibecrafted",
            "2026-10-20",
            &cwd,
            &source,
        )
    }

    /// An events log with a cut tail: the reader cannot cover it whole.
    /// Its mtime dates it months before the window.
    fn write_old_junie_session(&self, id: &str, at: &str) -> CatalogEntry {
        let source = self.junie_source(id);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let event = json!({
            "type": "user_message",
            "sessionId": id,
            "timestamp": at,
            "text": "Old junie request that predates the window."
        });
        fs::write(
            &source,
            format!("{event}\n{{\"type\":\"agent_block\",\"tru"),
        )
        .unwrap();
        let when: chrono::DateTime<Utc> = at.parse().unwrap();
        filetime::set_file_mtime(
            &source,
            filetime::FileTime::from_unix_time(when.timestamp(), 0),
        )
        .unwrap();
        self.entry(id, "junie", PROJECT, &at[..10], &self.principal(), &source)
    }

    fn write_catalog(&self, entries: &[CatalogEntry]) {
        let path = aicx::catalog::sessions_path_for(&self.home());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            entries
                .iter()
                .map(|entry| format!("{}\n", serde_json::to_string(entry).unwrap()))
                .collect::<String>(),
        )
        .unwrap();
    }

    /// Channel 4 on the Codescribe bus: Maciej's sealed takes. Only the
    /// structured audience/recipients address them.
    fn write_bus(&self) {
        let party = |name: &str| json!({"name": name, "provider": "claude-code", "provider_session_id": PREVIOUS_OPERATOR_SESSION});
        let take = |id: &str, audience: &str, text: &str, at: &str| {
            json!({
                "schema": "codescribe.transcript-evidence.v1",
                "session_id": id,
                "audience": audience,
                "recipients": [party(audience)],
                "rendered_text": text,
                "emitted_at": at,
                "reducer_revision": 1,
                "reducer_action": "record_ledger_terminal_seal"
            })
        };
        let rows = [
            take(
                "voice-andrzej-expired",
                "andrzej",
                EXPIRED_TAKE,
                "2026-10-17T21:00:00Z",
            ),
            take(
                "voice-andrzej-aicx",
                "andrzej",
                FOUNDER_TAKE,
                "2026-10-20T21:10:00Z",
            ),
            take(
                "voice-john-clean",
                "john",
                JOHN_TAKE,
                "2026-10-20T21:12:00Z",
            ),
        ];
        let ledger = self
            .root
            .join(".codescribe/agent-bridge/buses/events/2026_1020/channel-4.jsonl");
        fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        fs::write(
            ledger,
            rows.iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
        )
        .unwrap();
    }

    /// The real binary against this fixture's isolated homes.
    fn run(&self, args: &[String]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aicx"))
            .args(args)
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("AICX_HOME", self.home())
            .env("AICX_ALLOW_TMP", "1")
            .output()
            .unwrap()
    }

    /// `COLDSTART_COMMAND` with its placeholders filled. The frozen clock
    /// and the skipped catalog refresh are the only additions: the fixture
    /// must not depend on today's date or on this machine's live sources.
    fn coldstart(&self, transform: impl Fn(Vec<String>) -> Vec<String>) -> String {
        let mut args: Vec<String> = COLDSTART_COMMAND
            .split_whitespace()
            .skip(1)
            .map(|word| match word {
                "<org/repo>" => PROJECT.to_string(),
                "<name>" => OPERATOR.to_string(),
                other => other.to_string(),
            })
            .collect();
        args.extend(["--until".into(), NOW.into(), "--no-refresh".into()]);
        let output = self.run(&transform(args));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn utterances(&self) -> Output {
        self.run(
            &[
                "utterances",
                "-p",
                PROJECT,
                "--since",
                "2026-10-18",
                "--until",
                "2026-10-21",
            ]
            .map(String::from),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn without(args: Vec<String>, flag: &str, values: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = 0;
    for arg in args {
        if skip > 0 {
            skip -= 1;
        } else if arg == flag {
            skip = values;
        } else {
            out.push(arg);
        }
    }
    out
}

fn section<'a>(markdown: &'a str, heading: &str) -> &'a str {
    markdown
        .split(&format!("\n## {heading}"))
        .nth(1)
        .unwrap_or_else(|| panic!("section {heading} missing:\n{markdown}"))
        .split("\n## ")
        .next()
        .unwrap()
}

fn worker_lines<'a>(markdown: &'a str, worker: &Worker) -> Vec<&'a str> {
    let tag = format!("[claude · {}]", worker.session);
    markdown
        .lines()
        .filter(|line| line.contains(&tag))
        .collect()
}

#[test]
fn coldstart_one_call_shows_open_work_the_addressed_take_and_sources() {
    let fixture = Fixture::new();
    let inject = fixture.coldstart(|args| args);
    let full = fixture.coldstart(|args| without(args, "--for-inject", 0));

    // The day's pack does not fit the inject budget: the cut below is real.
    assert!(
        full.chars().count() > INJECT_BUDGET,
        "the fixture must put the inject budget under pressure: {} chars",
        full.chars().count()
    );
    assert!(inject.chars().count() <= INJECT_BUDGET, "{inject}");
    assert!(
        inject.contains("[continuity pack truncated at inject budget"),
        "{inject}"
    );

    // OPEN WORK: the four cuts HEAD lacks, each tied to its worker session.
    let open = section(&inject, "OPEN WORK");
    let rows: Vec<&str> = open
        .lines()
        .filter(|line| line.starts_with("- cut/"))
        .collect();
    assert_eq!(rows.len(), 4, "{open}");
    assert!(open.contains(&format!("HEAD={INTEGRATION}")), "{open}");
    for worker in &WORKERS {
        let row = rows
            .iter()
            .find(|row| row.starts_with(&format!("- cut/claude-{} · ", worker.run)))
            .unwrap_or_else(|| panic!("no row for {}: {open}", worker.run));
        // Printed the way the pack prints paths: no Windows `\\?\` prefix.
        let worktree = aicx_parser::sanitize::strip_verbatim_prefix(
            &fs::canonicalize(fixture.worktree(worker)).unwrap(),
        );
        for expected in [
            "open=1".to_string(),
            format!("session_id={}", worker.session),
            "Authored-By=claude <agents@vetcoders.io>".to_string(),
            format!("worktree={}", worktree.display()),
            format!(
                "subject=[claude/workflow] fix({}): {}",
                worker.area, worker.prompt
            ),
        ] {
            assert!(row.contains(&expected), "missing {expected}: {row}");
        }
    }
    assert!(!open.contains("already integrated"), "{open}");

    // ADDRESSED TO ANDRZEJ: the take from another project's session, and
    // nothing addressed to anyone else or outside the window.
    let addressed = section(&inject, "ADDRESSED TO ");
    assert!(addressed.starts_with("ANDRZEJ\n"), "{addressed}");
    let take = addressed
        .lines()
        .find(|line| line.contains(FOUNDER_TAKE))
        .unwrap_or_else(|| panic!("the Founder take is missing: {addressed}"));
    assert!(take.starts_with("- [voice] "), "{take}");
    assert!(
        take.contains(&format!(
            "recipients=claude-code/{PREVIOUS_OPERATOR_SESSION}"
        )),
        "{take}"
    );
    for absent in [JOHN_TAKE, EXPIRED_TAKE] {
        assert!(!addressed.contains(absent), "leaked {absent}: {addressed}");
    }

    // The dispatch brief is never human speech, in either rendering.
    for view in [&inject, &full] {
        assert!(!view.contains(BRIEF_DECOY), "a brief line became a claim");
        for worker in &WORKERS {
            for line in worker_lines(view, worker) {
                assert!(
                    !line.contains("attribution=human_candidate"),
                    "dispatch brief attributed to a human: {line}"
                );
            }
        }
    }

    // SOURCES and INDEX HEALTH survive the inject cut with real rows.
    let sources = section(&inject, "SOURCES");
    let source_rows: Vec<&str> = sources
        .lines()
        .filter(|line| line.starts_with("- claude · "))
        .collect();
    assert!(!source_rows.is_empty(), "{sources}");
    assert!(
        source_rows
            .iter()
            .any(|row| row.contains(OPERATOR_SESSION) && row.contains("human=4")),
        "{sources}"
    );
    for worker in &WORKERS {
        if let Some(row) = source_rows.iter().find(|row| row.contains(worker.session)) {
            assert!(row.contains("human=0"), "a brief counted as human: {row}");
        }
    }
    assert!(
        section(&inject, "INDEX HEALTH").contains("- readiness: "),
        "{inject}"
    );
}

#[test]
fn coldstart_full_pack_keeps_each_brief_as_dispatch_evidence() {
    let fixture = Fixture::new();
    let full = fixture.coldstart(|args| without(args, "--for-inject", 0));
    let attributed: Vec<&str> = WORKERS
        .iter()
        .flat_map(|worker| worker_lines(&full, worker))
        .filter(|line| line.contains("attribution=dispatch_brief"))
        .collect();
    assert!(
        !attributed.is_empty(),
        "the briefs must stay as evidence, not vanish:\n{full}"
    );
    for line in &attributed {
        assert!(line.contains("role=user"), "{line}");
        assert!(line.contains("task candidate"), "{line}");
    }
    // Maciej's own words keep their human attribution next to the briefs.
    let human = full
        .lines()
        .find(|line| line.contains("nothing merges to main without my button"))
        .unwrap_or_else(|| panic!("the human decision is missing:\n{full}"));
    assert!(human.contains("attribution=human_candidate"), "{human}");
    assert!(
        human.contains(&format!("[claude · {OPERATOR_SESSION}]")),
        "{human}"
    );

    // Without `--as` the project narrative stays the project's: no bus
    // section, and the foreign recipient session never enters it.
    let baseline = fixture.coldstart(|args| without(without(args, "--for-inject", 0), "--as", 1));
    assert!(!baseline.contains("## ADDRESSED TO "), "{baseline}");
    assert!(!baseline.contains(FOUNDER_TAKE), "{baseline}");
    assert!(!baseline.contains(PREVIOUS_OPERATOR_SESSION), "{baseline}");
    assert_eq!(section(&baseline, "OPEN WORK"), section(&full, "OPEN WORK"));
}

#[test]
fn coldstart_utterances_settle_despite_old_junie_sessions() {
    let fixture = Fixture::new();
    let output = fixture.utterances();
    let report = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{report}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(report.contains("skipped_before_window: 3"), "{report}");
    assert!(!report.contains("refused:"), "{report}");
    let human = report
        .split("human:")
        .nth(1)
        .and_then(|rest| rest.split("machine text:").next())
        .unwrap_or_else(|| panic!("no human section: {report}"));
    assert!(
        human.contains("sprawdź co zostało po wczorajszej flocie"),
        "{report}"
    );
    assert!(
        !human.contains("You are running under Vibecrafted core runtime."),
        "a dispatch brief was listed as human speech: {report}"
    );

    // Counterfactual: the same junie sources with a fresh mtime are read,
    // and they are what refused every 72-hour window on 2026-10-21.
    for (id, _) in OLD_JUNIE {
        filetime::set_file_mtime(fixture.junie_source(id), filetime::FileTime::now()).unwrap();
    }
    let refused = fixture.utterances();
    let report = String::from_utf8_lossy(&refused.stdout).into_owned();
    assert_eq!(refused.status.code(), Some(2), "{report}");
    for (id, _) in OLD_JUNIE {
        assert!(
            report
                .lines()
                .any(|line| line.starts_with("refused: session") && line.contains(id)),
            "{report}"
        );
    }
}

#[test]
fn coldstart_doc_teaches_the_tested_command_and_four_lenses() {
    let doc = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/coldstart.md"))
        .expect("docs/coldstart.md");
    assert!(
        doc.contains(COLDSTART_COMMAND),
        "the doc must carry the exact command this test runs: {COLDSTART_COMMAND}"
    );
    for lens in [
        "Lens 0 — Who am I",
        "Lens 1 — Map",
        "Lens 2 — Now",
        "Lens 3 — What is red",
    ] {
        assert!(doc.contains(lens), "missing {lens}");
    }
    for section in [
        "NOW",
        "ADDRESSED TO",
        "OPEN WORK",
        "SOURCES",
        "INDEX HEALTH",
    ] {
        assert!(doc.contains(section), "missing pack section {section}");
    }
    assert!(doc.contains("tools/dump_helps.sh"), "the help lens script");
}

/// The help lens regenerates from the binary under test, in the
/// `~/.agents/loct-helps` shape, and already knows the cold-start flags.
#[cfg(unix)]
#[test]
fn coldstart_help_lens_regenerates_from_the_binary() {
    // No scenario here: the bare fixture only owns (and drops) a temp root.
    let fixture = Fixture {
        root: std::env::temp_dir().join(format!(
            "aicx-coldstart-helps-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        )),
    };
    let lens = fixture.root.join("aicx-helps");
    fs::create_dir_all(&fixture.root).unwrap();
    let output = Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/dump_helps.sh"))
        .arg(&lens)
        .arg(env!("CARGO_BIN_EXE_aicx"))
        .env("HOME", &fixture.root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut files: Vec<String> = fs::read_dir(&lens)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert!(files.len() >= 40, "{files:?}");
    for file in &files {
        let body = fs::read_to_string(lens.join(file)).unwrap();
        let header = body.lines().next().unwrap_or_default();
        // `aicx corpus validate-cards --help` lives in corpus-validate-cards.txt:
        // the command words joined by dashes name the file.
        let named = match file.as_str() {
            "_root.txt" => header == "########## aicx --help-full ##########",
            name => header
                .strip_prefix("########## aicx ")
                .and_then(|rest| rest.strip_suffix(" --help ##########"))
                .is_some_and(|words| format!("{}.txt", words.replace(' ', "-")) == name),
        };
        assert!(named, "{file}: {header}");
        assert!(body.lines().count() > 2, "{file} is empty");
    }
    let show = fs::read_to_string(lens.join("continuity-show.txt")).unwrap();
    for flag in ["--as <NAME>", "--for-inject", "--until <UNTIL>"] {
        assert!(show.contains(flag), "continuity-show.txt lacks {flag}");
    }
    assert!(files.contains(&"utterances.txt".to_string()), "{files:?}");
}
