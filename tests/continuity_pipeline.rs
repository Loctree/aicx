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
    process::Command,
};

struct Fixture {
    root: PathBuf,
    previous_home: Option<std::ffi::OsString>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "aicx-continuity-pipeline-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(root.join(".aicx/synthetic")).unwrap();
        let previous_home = std::env::var_os("HOME");
        // This integration target has one test; no other test thread accesses HOME.
        unsafe {
            std::env::set_var("HOME", &root);
        }
        Self {
            root,
            previous_home,
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
    fn cli(&self, inject: bool) -> String {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aicx"));
        command
            .args([
                "continuity",
                "show",
                "-p",
                "Loctree/aicx",
                "-H",
                "96",
                "--until",
                "2026-10-03T04:47:00Z",
                "--no-refresh",
            ])
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("AICX_HOME", self.home())
            .env("AICX_ALLOW_TMP", "1");
        if inject {
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
    // This baseline does not have a Copilot adapter; that is a disclosed hole, not 0 discovered Copilot.
    entries.push(fixture.source(
        25,
        "copilot",
        "Loctree/aicx",
        Some("2026-10-02T12:00:00Z"),
        "Decision: unsupported provider must be counted.",
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
        "unsupported provider and malformed visible tail are separate holes"
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
            .any(|s| s.agent == "copilot" && s.status == "source_error")
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
