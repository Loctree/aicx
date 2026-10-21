#![cfg(feature = "app")]
//! Addressed speech is a cross-project bus view, never a free-text name search.
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
};

use aicx::catalog::{CATALOG_SCHEMA, CatalogEntry};
use chrono::Utc;
use serde_json::{Value, json};

const UNTIL: &str = "2026-10-20T12:00:00Z";
const RECEIVER: &str = "aaaaaaaa-1111-4222-8333-444444444444";
const OTHER: &str = "bbbbbbbb-1111-4222-8333-444444444444";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "aicx-continuity-addressed-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let fixture = Self(root);
        fs::create_dir_all(fixture.0.join(".aicx/catalog")).unwrap();
        let mut entries = Vec::new();
        for (session, project) in [
            (RECEIVER, "elsewhere/screenscribe"),
            (OTHER, "loctree/aicx"),
        ] {
            let cwd = fixture.0.join(project);
            fs::create_dir_all(&cwd).unwrap();
            let source = fixture
                .0
                .join(format!(".claude/projects/fixture/{session}.jsonl"));
            fs::create_dir_all(source.parent().unwrap()).unwrap();
            fs::write(&source, format!("{}\n", json!({
                "type":"user", "sessionId":session, "cwd":cwd,
                "timestamp":"2026-10-20T11:00:00Z",
                "message":{"role":"user","content":"Decision: retain the synthetic project boundary"}
            }))).unwrap();
            entries.push(CatalogEntry {
                schema: CATALOG_SCHEMA.into(),
                session_id: session.into(),
                agent: "claude".into(),
                project: Some(project.into()),
                date: Some("2026-10-20".into()),
                cwd: Some(cwd.to_string_lossy().into()),
                source_path: source.to_string_lossy().into(),
                source_len: None,
                source_mtime_ns: None,
                source_bundle_fingerprint: None,
                title: None,
                machine: None,
                logical_session_id: None,
                session_kind: None,
            });
        }
        fs::write(
            aicx::catalog::sessions_path_for(&fixture.0.join(".aicx")),
            entries
                .iter()
                .map(|entry| format!("{}\n", serde_json::to_string(entry).unwrap()))
                .collect::<String>(),
        )
        .unwrap();
        let party = |name: &str| json!({"name":name,"provider":"claude-code","provider_session_id":RECEIVER});
        let spoken = |id: &str, audience: &str, recipients: Value, text: &str, at: &str| {
            json!({
                "schema":"codescribe.transcript-evidence.v1", "session_id":id,
                "audience":audience,"recipients":recipients,"rendered_text":text,
                "emitted_at":at,"reducer_revision":1,"reducer_action":"record_ledger_terminal_seal"
            })
        };
        let typed = |name: &str, text: &str, at: &str| {
            json!({
                "schema":"codescribe.agent-user-message.v1", "name":name,
                "provider":"claude-code", "provider_session_id":RECEIVER,
                "recipients":[party(name)], "text":text,"emitted_at":at
            })
        };
        let rows = [
            spoken(
                "voice-andrzej",
                "andrzej",
                json!([party("andrzej")]),
                "Synthetic voice assignment across projects",
                "2026-10-20T10:00:00Z",
            ),
            typed(
                "ANDRZEJ",
                "Synthetic typed assignment across projects",
                UNTIL,
            ),
            spoken(
                "voice-other",
                "roman",
                json!([party("roman")]),
                "Andrzej is mentioned but this is Roman's work",
                "2026-10-20T11:00:00Z",
            ),
            typed(
                "andrzej",
                "Synthetic expired assignment",
                "2026-10-20T09:59:59Z",
            ),
            typed(
                "andrzej",
                "Synthetic future assignment",
                "2026-10-20T12:00:01Z",
            ),
            spoken(
                "voice-unicode",
                "Z\u{307}O\u{301}ŁC\u{301}",
                json!([]),
                "Synthetic Unicode audience assignment",
                "2026-10-20T11:00:00Z",
            ),
            json!({"schema":"codescribe.agent-reply.v1", "name":"andrzej", "provider":"claude-code", "provider_session_id":RECEIVER, "recipients":[party("andrzej")], "text":"Synthetic agent reply is not operator speech", "emitted_at":"2026-10-20T11:00:00Z"}),
            json!({"schema":"codescribe.transcript-evidence.v1", "session_id":"agent-channel-4-open", "audience":"andrzej", "recipients":[party("andrzej")], "rendered_text":"Synthetic open take must stay withheld", "emitted_at":"2026-10-20T11:00:00Z", "reducer_revision":1, "reducer_action":"apply_ledger_decision"}),
        ];
        let ledger = fixture
            .0
            .join(".codescribe/agent-bridge/buses/events/2026_1020/fixture.jsonl");
        fs::create_dir_all(ledger.parent().unwrap()).unwrap();
        fs::write(
            ledger,
            rows.iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let dictation = fixture
            .0
            .join(".codescribe/transcriptions/2026-10-20/110000_raw.txt");
        fs::create_dir_all(dictation.parent().unwrap()).unwrap();
        fs::write(
            dictation,
            "Andrzej: synthetic unaddressed dictation must stay out",
        )
        .unwrap();
        fixture
    }
    fn command(&self, binary: &str) -> Command {
        let mut command = Command::new(binary);
        command
            .env("HOME", &self.0)
            .env("USERPROFILE", &self.0)
            .env("AICX_HOME", self.0.join(".aicx"))
            .env("AICX_ALLOW_TMP", "1");
        command
    }
    fn cli(&self, action: &str, name: Option<&str>) -> String {
        let mut command = self.command(env!("CARGO_BIN_EXE_aicx"));
        command.args([
            "continuity",
            action,
            "-p",
            "loctree/aicx",
            "-H",
            "2",
            "--until",
            UNTIL,
            "--no-refresh",
        ]);
        if let Some(name) = name {
            command.args(["--as", name]);
        }
        if action == "write" {
            command.arg("--output").arg(self.0.join("pack.md"));
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if action == "write" {
            fs::read_to_string(self.0.join("pack.md")).unwrap()
        } else {
            String::from_utf8(output.stdout).unwrap()
        }
    }
    fn mcp(&self, name: &str) -> String {
        let mut child = self
            .command(env!("CARGO_BIN_EXE_aicx-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let send = |input: &mut std::process::ChildStdin, value: Value| {
            writeln!(input, "{value}").unwrap();
            input.flush().unwrap();
        };
        send(
            &mut input,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"continuity-addressed","version":"1"}}}),
        );
        read_response(&mut output, 1);
        send(
            &mut input,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        );
        send(
            &mut input,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"aicx_continuity","arguments":{"project":"loctree/aicx","hours":2,"until":UNTIL,"as":name}}}),
        );
        let response = read_response(&mut output, 2);
        drop(input);
        finish_child(child);
        assert!(response.get("error").is_none(), "{response}");
        assert_ne!(response["result"]["isError"], true, "{response}");
        let payload: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        payload["markdown"].as_str().unwrap().into()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn read_response(reader: &mut impl BufRead, id: u64) -> Value {
    loop {
        let mut line = String::new();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "MCP closed before response {id}"
        );
        let value: Value = serde_json::from_str(&line).unwrap();
        if value["id"] == id {
            return value;
        }
    }
}
fn finish_child(mut child: Child) {
    assert!(child.wait().unwrap().success());
}
fn addressed(markdown: &str) -> &str {
    markdown
        .split("## ADDRESSED TO ")
        .nth(1)
        .expect("addressed section")
        .split("\n## ")
        .next()
        .unwrap()
}

#[test]
fn addressed_cli_cross_project_recipients_and_write() {
    let fixture = Fixture::new();
    let markdown = fixture.cli("show", Some("Andrzej"));
    let section = addressed(&markdown);
    assert!(section.starts_with("ANDRZEJ\n"), "{section}");
    for expected in [
        "Synthetic voice assignment across projects",
        "Synthetic typed assignment across projects",
        RECEIVER,
    ] {
        assert!(section.contains(expected), "missing {expected}: {section}");
    }
    for absent in [
        "Roman's work",
        "expired assignment",
        "future assignment",
        "agent reply",
        "Synthetic open take must stay withheld",
        "unaddressed dictation",
    ] {
        assert!(!section.contains(absent), "leaked {absent}: {section}");
    }
    assert_eq!(markdown, fixture.cli("write", Some("Andrzej")));
    let baseline = fixture.cli("show", None);
    assert!(!baseline.contains("## ADDRESSED TO "));
    assert!(
        !baseline.contains(RECEIVER),
        "foreign receiver entered project narrative"
    );
    assert_eq!(
        baseline,
        markdown.replacen(&format!("## ADDRESSED TO {section}\n"), "", 1)
    );
}

#[test]
fn addressed_mcp_matches_cli() {
    let fixture = Fixture::new();
    assert_eq!(
        addressed(&fixture.cli("show", Some("Andrzej"))),
        addressed(&fixture.mcp("Andrzej"))
    );
}

#[test]
fn addressed_unicode_audience_without_recipient() {
    let fixture = Fixture::new();
    let markdown = fixture.cli("show", Some("żółć"));
    let section = addressed(&markdown);
    assert!(section.starts_with("ŻÓŁĆ\n"), "{section}");
    assert!(
        section.contains("Synthetic Unicode audience assignment"),
        "{section}"
    );
    assert!(
        !section.contains("Synthetic voice assignment across projects"),
        "{section}"
    );
    assert!(addressed(&fixture.cli("show", Some("Andrzej2"))).contains("no addressed"));
}
