//! Transmission-variant dedup and the shell-executor axis, through the real
//! registered-engine path (`SourceHandle` -> `ParserEngine::parse_registered`).
//!
//! The property under test is metamorphic: **the same logical event delivered
//! through different provider transports must collapse to the same result,
//! while genuinely distinct events must stay distinct.** Providers duplicate
//! transmissions in several ways — a replayed compaction history, a message
//! emitted once as a stream event and once as a response item, a command whose
//! body is repeated in a separate result record. None of those are the operator
//! saying something twice.
//!
//! The tests below deliberately never assert `unique(text)`: an operator who
//! types "tak" three times in three turns must still see three turns.

use aicx_parser::engine::{
    AgentKind, FrameClass, ParserEngine, SessionModel, ShellExecutor, SourceArtifact,
    SourceFraming, SourceHandle, TurnKind, TurnRole, ValidatedParse,
};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "aicx-transmission-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self { dir }
    }

    fn parse(&self, agent: AgentKind, session_id: &str, body: &str) -> SessionModel {
        let path = self.dir.join(format!("{session_id}.jsonl"));
        fs::write(&path, body).expect("write fixture");
        let artifact = SourceArtifact::validated_file(
            "source.jsonl".to_owned(),
            &path,
            SourceFraming::JsonLines,
        )
        .expect("valid artifact");
        let handle = SourceHandle::new(agent, session_id.to_owned(), None, vec![artifact])
            .expect("valid handle");
        match ParserEngine::default()
            .parse_registered(&handle)
            .expect("engine parse")
        {
            ValidatedParse::Session(session) => session.into_model(),
            ValidatedParse::Fatal(fatal) => {
                panic!("unexpected fatal parse: {:?}", fatal.coverage())
            }
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Only what a reader would call speech: human turns and assistant answers.
fn speech(model: &SessionModel) -> Vec<(TurnRole, String)> {
    model
        .turns
        .iter()
        .filter(|turn| matches!(turn.kind, TurnKind::UserMsg | TurnKind::AgentReply))
        .map(|turn| (turn.role, turn.text.clone()))
        .collect()
}

fn shell_actions(model: &SessionModel) -> Vec<(ShellExecutor, String)> {
    model
        .turns
        .iter()
        .filter_map(|turn| match turn.frame_class.as_ref() {
            Some(FrameClass::ShellAction { cmd, executor, .. }) => Some((*executor, cmd.clone())),
            _ => None,
        })
        .collect()
}

const CODEX_META: &str = r#"{"timestamp":"2026-09-10T04:00:00Z","type":"session_meta","payload":{"id":"s","cwd":"/repo"}}"#;

fn codex_user(ts: &str, text: &str) -> String {
    format!(
        r#"{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{text}"}}]}}}}"#
    )
}

fn codex_assistant(ts: &str, text: &str) -> String {
    format!(
        r#"{{"timestamp":"{ts}","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"{text}"}}]}}}}"#
    )
}

/// The stream-event twin of an assistant message: Codex emits some answers
/// both as `event_msg` and as `response_item`.
fn codex_event_assistant(ts: &str, text: &str) -> String {
    format!(
        r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"agent_message","message":"{text}"}}}}"#
    )
}

// ---------------------------------------------------------------------------
// Metamorphic: one logical event, several transports
// ---------------------------------------------------------------------------

#[test]
fn dual_emission_of_one_answer_collapses_to_one_turn() {
    let fixture = Fixture::new("dual");
    // The provider wrote the same answer twice: once on the event stream, once
    // as the durable response item.
    let both = format!(
        "{CODEX_META}\n{}\n{}\n{}\n",
        codex_user("2026-09-10T04:00:01Z", "pytanie"),
        codex_event_assistant("2026-09-10T04:00:02Z", "jedna odpowiedz"),
        codex_assistant("2026-09-10T04:00:02Z", "jedna odpowiedz"),
    );
    let once = format!(
        "{CODEX_META}\n{}\n{}\n",
        codex_user("2026-09-10T04:00:01Z", "pytanie"),
        codex_assistant("2026-09-10T04:00:02Z", "jedna odpowiedz"),
    );

    let dual = fixture.parse(AgentKind::Codex, "dual", &both);
    let single = fixture.parse(AgentKind::Codex, "single", &once);

    assert_eq!(
        speech(&dual),
        speech(&single),
        "the same logical exchange must project identically whether or not the \
         provider also emitted the stream twin"
    );
    assert_eq!(speech(&dual).len(), 2);
}

#[test]
fn intentional_repetition_across_turns_is_preserved() {
    let fixture = Fixture::new("repeat");
    // The operator really did say the same word three times, in three separate
    // turns, with an answer between them. A global unique(text) would eat two.
    let body = format!(
        "{CODEX_META}\n{}\n{}\n{}\n{}\n{}\n",
        codex_user("2026-09-10T04:00:01Z", "tak"),
        codex_assistant("2026-09-10T04:00:02Z", "robie"),
        codex_user("2026-09-10T04:00:03Z", "tak"),
        codex_assistant("2026-09-10T04:00:04Z", "robie"),
        codex_user("2026-09-10T04:00:05Z", "tak"),
    );
    let model = fixture.parse(AgentKind::Codex, "repeat", &body);
    let said = speech(&model);
    assert_eq!(
        said.iter()
            .filter(|(role, text)| *role == TurnRole::User && text == "tak")
            .count(),
        3,
        "three deliberate repetitions must survive as three turns: {said:?}"
    );
    assert_eq!(
        said.iter()
            .filter(|(role, text)| *role == TurnRole::Assistant && text == "robie")
            .count(),
        2
    );
}

#[test]
fn identical_text_in_different_roles_never_merges() {
    let fixture = Fixture::new("roles");
    // A quoted-back answer is not the answer. Same bytes, different speaker.
    let body = format!(
        "{CODEX_META}\n{}\n{}\n",
        codex_user("2026-09-10T04:00:01Z", "gotowe"),
        codex_assistant("2026-09-10T04:00:02Z", "gotowe"),
    );
    let model = fixture.parse(AgentKind::Codex, "roles", &body);
    let said = speech(&model);
    assert_eq!(
        said.len(),
        2,
        "same text, two speakers, two turns: {said:?}"
    );
    assert_eq!(said[0].0, TurnRole::User);
    assert_eq!(said[1].0, TurnRole::Assistant);
}

#[test]
fn whitespace_and_code_fences_keep_their_meaning() {
    let fixture = Fixture::new("bytes");
    // Two messages that differ only in indentation are two different messages:
    // in code, leading whitespace is meaning, not noise.
    let body = format!(
        "{CODEX_META}\n{}\n{}\n",
        codex_user("2026-09-10T04:00:01Z", "```\\nfn a() {}\\n```"),
        codex_user("2026-09-10T04:00:02Z", "```\\n    fn a() {}\\n```"),
    );
    let model = fixture.parse(AgentKind::Codex, "bytes", &body);
    let said = speech(&model);
    assert_eq!(
        said.len(),
        2,
        "indentation is significant; these are not duplicates: {said:?}"
    );
    assert_ne!(said[0].1, said[1].1);
    assert!(
        said[1].1.contains("    fn a()"),
        "the indented body must survive byte-for-byte: {:?}",
        said[1].1
    );
}

#[test]
fn a_quoted_transcript_is_not_confused_with_the_turns_it_quotes() {
    let fixture = Fixture::new("quote");
    // The operator pastes an earlier exchange back into the conversation. The
    // paste is one new human turn; it must not cancel the originals.
    let body = format!(
        "{CODEX_META}\n{}\n{}\n{}\n",
        codex_user("2026-09-10T04:00:01Z", "zrob X"),
        codex_assistant("2026-09-10T04:00:02Z", "zrobione"),
        codex_user(
            "2026-09-10T04:00:03Z",
            "przypominam: 'zrob X' -> 'zrobione'"
        ),
    );
    let model = fixture.parse(AgentKind::Codex, "quote", &body);
    assert_eq!(speech(&model).len(), 3);
}

// ---------------------------------------------------------------------------
// The shell-executor axis
// ---------------------------------------------------------------------------

#[test]
fn a_human_submitted_shell_command_is_tagged_human() {
    let fixture = Fixture::new("humancmd");
    let body = format!(
        "{CODEX_META}\n{}\n",
        codex_user(
            "2026-09-10T04:00:01Z",
            "<user_shell_command>\\n<command>cargo test</command>\\n<result>ok</result>\\n</user_shell_command>"
        ),
    );
    let model = fixture.parse(AgentKind::Codex, "humancmd", &body);
    let actions = shell_actions(&model);
    assert_eq!(actions.len(), 1, "one execution: {actions:?}");
    assert_eq!(
        actions[0],
        (ShellExecutor::Human, "cargo test".to_owned()),
        "the operator submitted this command"
    );
}

#[test]
fn a_command_named_in_prose_is_not_an_execution() {
    let fixture = Fixture::new("prosecmd");
    // Both an inline mention and a fenced quotation. Neither ran.
    let body = format!(
        "{CODEX_META}\n{}\n{}\n",
        codex_assistant("2026-09-10T04:00:01Z", "uruchom `cargo test --workspace`"),
        codex_user(
            "2026-09-10T04:00:02Z",
            "przyklad:\\n```\\n<user_shell_command>\\n<command>rm -rf /</command>\\n</user_shell_command>\\n```\\nto cytat"
        ),
    );
    let model = fixture.parse(AgentKind::Codex, "prosecmd", &body);
    assert!(
        shell_actions(&model).is_empty(),
        "a proposal and a fenced quotation are speech, not executions: {:?}",
        shell_actions(&model)
    );
    assert_eq!(speech(&model).len(), 2, "both stay in the conversation");
}

#[test]
fn every_shell_action_declares_an_executor() {
    let fixture = Fixture::new("executortotal");
    let body = format!(
        "{CODEX_META}\n{}\n{}\n",
        codex_user(
            "2026-09-10T04:00:01Z",
            "<user_shell_command>\\n<command>ls</command>\\n<result>a</result>\\n</user_shell_command>"
        ),
        codex_assistant("2026-09-10T04:00:02Z", "widze"),
    );
    let model = fixture.parse(AgentKind::Codex, "executortotal", &body);
    for turn in &model.turns {
        if let Some(FrameClass::ShellAction { executor, cmd, .. }) = turn.frame_class.as_ref() {
            // Never `unknown`: the transport always proves one of the two.
            assert!(
                matches!(executor, ShellExecutor::Human | ShellExecutor::Agent),
                "shell action `{cmd}` must declare who ran it"
            );
        }
    }
}

#[test]
fn shell_executor_serde_round_trips_and_defaults_to_agent() {
    // Frames persisted before the executor axis existed must stay readable,
    // and decode as the executor every pre-split producer but Codex meant.
    let legacy =
        r#"{"class":"shell_action","cmd":"ls","result":{"text":"a","chars":1,"hash":"h"}}"#;
    let decoded: FrameClass = serde_json::from_str(legacy).expect("legacy frame decodes");
    assert_eq!(decoded.shell_executor(), Some(ShellExecutor::Agent));

    let human = FrameClass::ShellAction {
        cmd: "ls".to_owned(),
        result: serde_json::from_str(r#"{"text":"a","chars":1,"hash":"h"}"#).unwrap(),
        executor: ShellExecutor::Human,
    };
    let encoded = serde_json::to_string(&human).expect("encode");
    let round: FrameClass = serde_json::from_str(&encoded).expect("decode");
    assert_eq!(round.shell_executor(), Some(ShellExecutor::Human));
    assert!(encoded.contains(r#""executor":"human""#), "{encoded}");
}

// ---------------------------------------------------------------------------
// Damaged and active sources
// ---------------------------------------------------------------------------

#[test]
fn a_torn_trailing_line_does_not_discard_the_records_before_it() {
    let fixture = Fixture::new("torn");
    let body = format!(
        "{CODEX_META}\n{}\n{}\n{}",
        codex_user("2026-09-10T04:00:01Z", "pierwsze"),
        codex_assistant("2026-09-10T04:00:02Z", "drugie"),
        r#"{"timestamp":"2026-09-10T04:00:03Z","type":"response_it"#,
    );
    // The engine may claim or refuse this source; what it may not do is claim
    // it while silently losing the two complete records ahead of the tear.
    let path = fixture.dir.join("torn.jsonl");
    fs::write(&path, &body).expect("write");
    let artifact =
        SourceArtifact::validated_file("source.jsonl".to_owned(), &path, SourceFraming::JsonLines)
            .expect("artifact");
    let handle = SourceHandle::new(AgentKind::Codex, "torn".to_owned(), None, vec![artifact])
        .expect("handle");
    match ParserEngine::default().parse_registered(&handle) {
        Ok(ValidatedParse::Session(session)) => {
            let model = session.into_model();
            let said = speech(&model);
            assert!(
                said.iter().any(|(_, text)| text == "pierwsze"),
                "a claimed source must keep its complete records: {said:?}"
            );
        }
        Ok(ValidatedParse::Fatal(_)) | Err(_) => {
            // An explicit refusal is the other honest answer.
        }
    }
    // The source is never rewritten by a read pass.
    assert_eq!(fs::read_to_string(&path).expect("still readable"), body);
}

#[test]
fn unknown_record_fields_do_not_discard_the_record() {
    let fixture = Fixture::new("unknownfields");
    let body = format!(
        "{CODEX_META}\n{}\n",
        r#"{"timestamp":"2026-09-10T04:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"nadal widoczne"}],"future_field":{"nested":true}},"another_future_field":42}"#,
    );
    let model = fixture.parse(AgentKind::Codex, "unknownfields", &body);
    assert!(
        speech(&model)
            .iter()
            .any(|(_, text)| text == "nadal widoczne"),
        "an unrecognized sibling field must not cost the message: {:?}",
        speech(&model)
    );
}
