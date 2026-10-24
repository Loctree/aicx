#![cfg(feature = "app")]
//! Acceptance through the real CLI and shared MCP route on one isolated, frozen corpus.
use aicx::{
    catalog::{CATALOG_SCHEMA, CatalogEntry},
    legacy_archive::ProjectMatchMode,
};
use chrono::{DateTime, Utc};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Mutex, MutexGuard},
};

// HOME is process-global; each fixture holds this lock for its whole life.
static HOME_LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    root: PathBuf,
    previous_home: Option<std::ffi::OsString>,
    _home_lock: MutexGuard<'static, ()>,
}
impl Fixture {
    fn new() -> Self {
        // One failing test must not poison the lock and hide which contract broke.
        let home_lock = HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let root = std::env::temp_dir().join(format!(
            "aicx-continuity-pipeline-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(root.join(".aicx/synthetic")).unwrap();
        let previous_home = std::env::var_os("HOME");
        // Serialized by HOME_LOCK: no other test thread touches HOME meanwhile.
        unsafe {
            std::env::set_var("HOME", &root);
        }
        Self {
            root,
            previous_home,
            _home_lock: home_lock,
        }
    }
    fn home(&self) -> PathBuf {
        self.root.join(".aicx")
    }
    fn source(
        &self,
        index: usize,
        agent: &str,
        project: &str,
        time: Option<&str>,
        text: &str,
    ) -> CatalogEntry {
        let id = format!("11111111-2222-3333-4444-{index:012}");
        let path = self.home().join(format!("synthetic/{id}.jsonl"));
        let cwd = format!("/fixtures/{project}");
        let mut body = String::new();
        if agent == "codex" {
            body.push_str(&serde_json::json!({"type":"session_meta","timestamp":"2026-09-24T00:00:00Z","payload":{"id":id,"cwd":cwd}}).to_string());
            body.push('\n');
            body.push_str(&serde_json::json!({"timestamp":time,"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}}).to_string());
        } else {
            let mut message = serde_json::json!({"type":"user","sessionId":id,"cwd":cwd,"message":{"role":"user","content":text}});
            if let Some(time) = time {
                message["timestamp"] = time.into();
            }
            body.push_str(&message.to_string());
        }
        body.push('\n');
        fs::write(&path, body).unwrap();
        CatalogEntry {
            schema: CATALOG_SCHEMA.into(),
            session_id: id,
            agent: agent.into(),
            project: Some(project.into()),
            date: Some("2026-09-24".into()),
            cwd: Some(cwd),
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
    fn catalog(&self, entries: &[CatalogEntry]) {
        let path = aicx::catalog::sessions_path_for(&self.home());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            entries
                .iter()
                .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
                .collect::<String>(),
        )
        .unwrap();
    }
    /// One Codex rollout launched in repository `alpha` whose later turns
    /// ran tool calls in `beta` and, conflicting, in `beta` and `gamma`: a
    /// single session proving more than one scope.
    fn mixed_source(&self) -> CatalogEntry {
        let repo = |name: &str| {
            let repo = self.root.join("workspaces").join(name);
            fs::create_dir_all(repo.join(".git")).unwrap();
            serde_json::Value::String(repo.to_string_lossy().into())
        };
        let (alpha, beta, gamma) = (repo("alpha"), repo("beta"), repo("gamma"));
        let id = "55555555-4444-3333-2222-111111111111";
        let exec = |call: &str, workdir: &serde_json::Value| {
            format!(
                "const {call} = await tools.exec_command({{cmd:\"ls\",\"workdir\":{workdir}}});"
            )
        };
        let lines = [
            serde_json::json!({"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":id,"cwd":alpha}}),
            serde_json::json!({"timestamp":"2026-01-01T00:01:00Z","type":"turn_context","payload":{"cwd":alpha}}),
            serde_json::json!({"timestamp":"2026-01-01T00:01:10Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Decision: keep the alpha opening decision"}]}}),
            serde_json::json!({"timestamp":"2026-01-01T00:02:00Z","type":"turn_context","payload":{"cwd":alpha}}),
            serde_json::json!({"timestamp":"2026-01-01T00:02:05Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Decision: keep the beta bus decision"}]}}),
            serde_json::json!({"timestamp":"2026-01-01T00:02:10Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c1","input":exec("r", &beta)}}),
            serde_json::json!({"timestamp":"2026-01-01T00:03:00Z","type":"turn_context","payload":{"cwd":alpha}}),
            serde_json::json!({"timestamp":"2026-01-01T00:03:05Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c2","input":exec("a", &beta)}}),
            serde_json::json!({"timestamp":"2026-01-01T00:03:10Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c3","input":exec("b", &gamma)}}),
        ];
        let path = self.home().join(format!("synthetic/rollout-{id}.jsonl"));
        fs::write(
            &path,
            lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
        )
        .unwrap();
        CatalogEntry {
            schema: CATALOG_SCHEMA.into(),
            session_id: id.into(),
            agent: "codex".into(),
            project: Some("workspaces/alpha".into()),
            date: Some("2026-01-01".into()),
            cwd: alpha.as_str().map(Into::into),
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
    /// Run the real binary against this fixture's isolated homes.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_aicx"))
            .args(args)
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("AICX_HOME", self.home())
            .env("AICX_ALLOW_TMP", "1")
            .output()
            .unwrap()
    }
    fn cli(&self, inject: bool) -> String {
        let mut args = vec![
            "continuity",
            "show",
            "-p",
            "Loctree/aicx",
            "-H",
            "96",
            "--until",
            "2026-10-03T04:47:00Z",
            "--no-refresh",
        ];
        if inject {
            args.push("--for-inject");
        }
        let output = self.run(&args);
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
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn normalized_mcp(markdown: &str) -> String {
    markdown
        .lines()
        .filter(|line| {
            !line.trim().is_empty()
                && !line.starts_with('>')
                && !line.starts_with("_Context pack only.")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn cli_startup_uses_persisted_phrases_and_invalid_override_uses_embedded_defaults() {
    let fixture = Fixture::new();
    let custom = "Operatorvow: preserve the violet orchard constraint.";
    let builtin = "Decision: preserve the embedded classification contract.";
    fixture.catalog(&[
        fixture.source(
            0,
            "claude",
            "Loctree/aicx",
            Some("2026-10-02T12:00:00Z"),
            custom,
        ),
        fixture.source(
            1,
            "claude",
            "Loctree/aicx",
            Some("2026-10-02T12:00:00Z"),
            builtin,
        ),
    ]);
    let path = fixture.home().join("intent_phrases.toml");
    fs::write(
        &path,
        aicx::parser::intent_phrases::embedded_source().replacen(
            "policy = [",
            "policy = [\"operatorvow:\",",
            1,
        ),
    )
    .unwrap();
    let persisted = fixture.cli(false);
    assert!(persisted.contains(custom), "{persisted}");
    assert!(
        persisted.contains("preserve the embedded classification contract."),
        "{persisted}"
    );

    fs::write(&path, "invalid = [").unwrap();
    let fallback = fixture.cli(false);
    assert!(!fallback.contains(custom), "{fallback}");
    assert!(
        fallback.contains("preserve the embedded classification contract."),
        "{fallback}"
    );
}

#[test]
fn frozen_cli_mcp_inject_census_time_role_and_error_contract() {
    let fixture = Fixture::new();
    let mut entries = Vec::new();
    for n in 0..24 {
        entries.push(fixture.source(
            n,
            if n % 2 == 0 { "claude" } else { "codex" },
            "Loctree/aicx",
            Some("2026-10-02T12:00:00Z"),
            &format!("Decision: preserve synthetic audio constraint number {n}; 🦀 Unicode."),
        ));
    }
    entries.push(fixture.source(
        24,
        "claude",
        "other/unrelated",
        Some("2026-10-02T12:00:00Z"),
        "Decision: merely mentioning aicx must not assign this foreign request.",
    ));
    // Copilot is a supported adapter. This body is Claude-shaped on purpose:
    // discovery must keep the provider in the census, and an empty parse is
    // `no_frames`, not a silent drop.
    entries.push(fixture.source(
        25,
        "copilot",
        "Loctree/aicx",
        Some("2026-10-02T12:00:00Z"),
        "Decision: an unreadable copilot body must stay in the census.",
    ));
    entries.push(fixture.source(
        26,
        "claude",
        "Loctree/aicx",
        None,
        "Decision: no utterance clock may be invented from a fresh file.",
    ));
    entries.push(fixture.source(
        27,
        "claude",
        "Loctree/aicx",
        Some("2026-09-29T04:46:59Z"),
        "Decision: exclude pre-window material.",
    ));
    // Partial parser coverage must survive alongside a usable claim.
    {
        use std::io::Write;
        let mut source = fs::OpenOptions::new()
            .append(true)
            .open(&entries[0].source_path)
            .unwrap();
        writeln!(source, "{{malformed-tail").unwrap();
    }
    fixture.catalog(&entries);
    let now: DateTime<Utc> = "2026-10-03T04:47:00Z".parse().unwrap();
    let pack = aicx::continuity::build_with_scope_at(
        &fixture.home(),
        &["Loctree/aicx".into()],
        96,
        false,
        now,
    )
    .unwrap();
    assert_eq!(pack.records.len(), 24);
    assert_eq!(
        pack.sources.len(),
        24,
        "source cap applies to rendering, not census"
    );
    assert_eq!(pack.selection.len(), 28);
    assert!(
        pack.source_errors >= 2,
        "empty copilot parse and malformed visible tail are separate holes"
    );
    assert_eq!(
        pack.selection
            .iter()
            .filter(|s| s.status == "qualified")
            .count(),
        24
    );
    assert!(
        pack.selection
            .iter()
            .any(|s| s.agent == "copilot" && s.status == "no_frames")
    );
    assert!(
        pack.selection
            .iter()
            .any(|s| s.unknown_time_frames > 0 && s.status == "unknown_time")
    );
    for record in &pack.records {
        assert_eq!(
            record.timestamp.as_deref(),
            Some("2026-10-02T12:00:00+00:00")
        );
        let origin = record.provenance.as_ref().expect("raw-frame provenance");
        assert_eq!(origin.role, "user");
        assert_eq!(origin.attribution, "human_candidate");
        assert!(origin.locator.contains("message-line:"));
        assert!(Path::new(&record.source_chunk).exists());
    }
    let cli = fixture.cli(false);
    let inject = fixture.cli(true);
    for view in [&cli, &inject] {
        assert!(view.contains("qualified_sources=24"));
        assert!(view.contains("sources_shown=20"));
        assert!(view.contains("source_rows_omitted=4"));
        assert!(view.contains("provider codex:") && view.contains("provider copilot:"));
        assert!(view.contains("unknown_time_frames=1"));
        assert!(view.contains("source_errors="));
        assert!(!view.contains("merely mentioning aicx"));
        assert!(!view.contains("exclude pre-window material"));
        assert!(view.contains("not verified Founder decisions"));
    }
    assert_eq!(cli, aicx::continuity::render(&pack, false));
    let mcp = aicx::mcp_session::continuity_pack(aicx::mcp_session::ContinuityRequest {
        aicx_home: &fixture.home(),
        project: Some("Loctree/aicx"),
        projects: &[],
        project_match: ProjectMatchMode::Exact,
        hours: 96,
        until: Some(now),
        for_inject: false,
        distill_mixed: false,
    })
    .unwrap();
    assert_eq!(normalized_mcp(&mcp.markdown), normalized_mcp(&cli));
    assert!(mcp.warnings.iter().any(|w| w.contains("source errors")));
    assert!(inject.chars().count() <= 24_000);
    let mcp_inject = aicx::mcp_session::continuity_pack(aicx::mcp_session::ContinuityRequest {
        aicx_home: &fixture.home(),
        project: Some("Loctree/aicx"),
        projects: &[],
        project_match: ProjectMatchMode::Exact,
        hours: 96,
        until: Some(now),
        for_inject: true,
        distill_mixed: false,
    })
    .unwrap();
    assert!(mcp_inject.markdown.chars().count() <= 24_000);
    assert!(mcp_inject.markdown.contains("qualified_sources=24"));
    assert!(mcp_inject.markdown.contains("unknown_time_frames=1"));
    assert!(
        mcp_inject
            .markdown
            .contains("preserve synthetic audio constraint")
    );
}

/// Founder CLI failure (2026-10-20, Issue #88 round 8): a window of only
/// mixed-workstream work refused with "pass distill_mixed to override", and
/// `--distill_mixed` was rejected as an unexpected argument — the override
/// the refusal named existed on no surface.
#[test]
fn cli_distill_mixed_overrides_a_mixed_only_window_under_both_spellings() {
    let fixture = Fixture::new();
    fixture.catalog(&[fixture.mixed_source()]);
    let base = [
        "continuity",
        "show",
        "-p",
        "workspaces/alpha",
        "-H",
        "24",
        "--until",
        "2026-01-01T12:00:00Z",
        "--no-refresh",
    ];
    let with = |flag: &'static str| [&base[..], &[flag]].concat();

    let refused = fixture.run(&base);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a mixed-only window refuses by default"
    );
    assert!(stderr.contains("1 mixed-workstream session(s)"), "{stderr}");
    assert!(
        stderr.contains("pass --distill-mixed (MCP: distill_mixed) to distill their 1 record(s)"),
        "{stderr}"
    );

    let mut shown = Vec::new();
    for flag in ["--distill-mixed", "--distill_mixed"] {
        let output = fixture.run(&with(flag));
        assert!(
            output.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        for line in [
            "keep the alpha opening decision",
            "mixed-workstream candidates: 1 session(s) distilled on request; 1 with retained records",
            "mixed_distilled=true (explicit request)",
        ] {
            assert!(stdout.contains(line), "{flag}: missing {line}:\n{stdout}");
        }
        assert!(!stdout.contains("beta bus decision"), "{flag}:\n{stdout}");
        shown.push(stdout);
    }
    assert_eq!(shown[0], shown[1], "both spellings are one flag");

    // `write` takes the same switch and writes the same pack.
    let out = fixture.root.join("CONTINUITY.md");
    let out_arg = out.to_string_lossy().into_owned();
    let mut write = with("--distill_mixed");
    write[1] = "write";
    write.extend(["-o", out_arg.as_str()]);
    let written = fixture.run(&write);
    assert!(
        written.status.success(),
        "{}",
        String::from_utf8_lossy(&written.stderr)
    );
    assert_eq!(fs::read_to_string(&out).unwrap(), shown[0]);

    // MCP forwards the same switch to the same pack.
    let mcp = aicx::mcp_session::continuity_pack(aicx::mcp_session::ContinuityRequest {
        aicx_home: &fixture.home(),
        project: Some("workspaces/alpha"),
        projects: &[],
        project_match: ProjectMatchMode::Exact,
        hours: 24,
        until: Some("2026-01-01T12:00:00Z".parse().unwrap()),
        for_inject: false,
        distill_mixed: true,
    })
    .unwrap();
    assert_eq!(normalized_mcp(&mcp.markdown), normalized_mcp(&shown[0]));
}
