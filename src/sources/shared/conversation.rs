#![allow(unused_imports)]
use super::*;

const EXACT_SHORT_DUP_MAX_CHARS: usize = 1000;
const EXACT_SHORT_DUP_WINDOW_MS: i64 = 2_000;

#[derive(Debug, Clone)]
pub struct ConversationProjection {
    pub messages: Vec<ConversationMessage>,
    pub exact_short_duplicates_dropped: usize,
    /// Count of harness-injected synthetic user turns removed from the
    /// projection (slash-command / skill bodies, inline `! command` local
    /// execution I/O, and system/hook reminders). See
    /// [`is_harness_injected_noise`].
    pub harness_noise_dropped: usize,
}

/// Head-anchored markers that identify a synthetic, harness-injected user turn
/// rather than real conversation. Detection requires the marker to sit at the
/// very head of the raw message body (see [`is_harness_injected_noise`]).
///
/// All entries are matched as a literal prefix on the left-trimmed message.
/// Most are `<…>` wrapper tags; `Base directory for this skill:` is the prose
/// preamble the harness prepends when it injects a loaded skill body as its own
/// user turn (the skill invocation `<command-name>` wrapper and the skill body
/// can arrive as separate messages).
const HARNESS_HEAD_MARKERS: [&str; 8] = [
    "<command-message>",      // slash-command / skill invocation (+ injected body)
    "<command-name>",         // slash-command / skill invocation name
    "<local-command-caveat>", // inline `! command` execution caveat
    "<bash-input>",           // inline `! command` input echo
    "<bash-stdout>",          // inline `! command` captured stdout/stderr turn
    "<bash-stderr>",          //
    "<system-reminder>",      // system / hook injected reminder
    "Base directory for this skill:", // injected skill body preamble
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntentLineModality {
    TypedDirective,
    PastedReference,
    Other,
}

const PASTED_REFERENCE_HEAD_MARKERS: [&str; 2] = [
    ">",              // Markdown blockquote pasted as reference material
    "[Pasted text #", // Claude clipboard placeholder for pasted content
];

const TYPED_DIRECTIVE_HEAD_MARKERS: [&str; 4] = [
    "zadanie:", // Polish "Task:" directive head
    "task:", "intent:", "[intent]",
];

/// Classify whether a user-authored line is a typed directive or pasted
/// reference material.
///
/// Like [`is_harness_injected_noise`], detection is role-disciplined and
/// head-anchored on the raw left-trimmed line. Markers that appear deeper in a
/// line are ordinary quoted content and do not change the modality.
pub(crate) fn intent_line_modality(role: &str, line: &str) -> IntentLineModality {
    if !role.eq_ignore_ascii_case("user") {
        return IntentLineModality::Other;
    }

    let head = line.trim_start();
    if PASTED_REFERENCE_HEAD_MARKERS
        .iter()
        .any(|marker| head.starts_with(marker))
    {
        return IntentLineModality::PastedReference;
    }

    let head_lower = head.to_lowercase();
    if TYPED_DIRECTIVE_HEAD_MARKERS
        .iter()
        .any(|marker| head_lower.starts_with(marker))
    {
        return IntentLineModality::TypedDirective;
    }

    IntentLineModality::Other
}

/// True when `message` is a harness-injected synthetic user turn rather than
/// real conversation: a slash-command / skill invocation (with its injected
/// skill body), inline `! command` local execution I/O, or a system/hook
/// reminder.
///
/// Detection is intentionally **head-anchored on the raw message** and limited
/// to the `user` role. This keeps two carve-outs honest:
///   * **Pasted transcripts** that merely *contain* these markers deeper in
///     the body (e.g. a user pasting a prior session log) are preserved — the
///     markers are not at the head, so the turn is treated as real input.
///   * **Assistant-authored content** (skill-creation bodies, hook-development
///     output) is never matched, because only user-role turns are considered.
fn is_harness_injected_noise(role: &str, message: &str) -> bool {
    if role != "user" {
        return false;
    }
    let head = message.trim_start();
    HARNESS_HEAD_MARKERS
        .iter()
        .any(|marker| head.starts_with(marker))
}

/// Project timeline entries into a denoised conversation stream.
///
/// Filters to only `user` and `assistant` roles, resolves repo/project identity
/// from `cwd` + project filter, and preserves provenance fields.
pub fn to_conversation(
    entries: &[TimelineEntry],
    project_filter: &[String],
) -> Vec<ConversationMessage> {
    to_conversation_with_stats(entries, project_filter).messages
}

pub fn to_conversation_with_stats(
    entries: &[TimelineEntry],
    project_filter: &[String],
) -> ConversationProjection {
    let mut harness_noise_dropped = 0usize;
    let messages: Vec<ConversationMessage> = entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.frame_kind,
                Some(FrameKind::UserMsg | FrameKind::AgentReply)
            ) || (entry.frame_kind.is_none() && (entry.role == "user" || entry.role == "assistant"))
        })
        .filter(|entry| {
            // Drop harness-injected synthetic user turns (slash-command / skill
            // bodies, inline `! command` I/O, system/hook reminders). Real
            // conversation — including pasted transcripts and assistant-authored
            // skill/hook content — is preserved. See `is_harness_injected_noise`.
            if is_harness_injected_noise(&entry.role, &entry.message) {
                harness_noise_dropped += 1;
                false
            } else {
                true
            }
        })
        .map(|e| {
            let (message_kind, collapse_stub_kind) = classify_conversation_message(&e.message);

            ConversationMessage {
                timestamp: e.timestamp,
                agent: e.agent.clone(),
                session_id: e.session_id.clone(),
                role: e.role.clone(),
                message: e.message.clone(),
                repo_project: repo_name_from_cwd(e.cwd.as_deref(), project_filter),
                source_path: e.cwd.clone(),
                branch: e.branch.clone(),
                message_kind,
                collapse_stub_kind,
            }
        })
        .collect();

    let mut projection = drop_exact_short_user_duplicates(messages);
    projection.harness_noise_dropped = harness_noise_dropped;
    projection
}

/// Compute a stable 64-bit key for `(agent, session_id, trimmed message)`
/// without allocating new `String`s on the hot dedup path. Uses SipHash-1-3
/// with null-byte delimiters between the fields to avoid prefix collisions
/// between e.g. `("a", "bc", "d")` and `("ab", "c", "d")`.
///
/// `agent` is part of the key because extractors can emit a shared fallback
/// session id (for example `extract_claude_history` uses `"history"` when
/// `sessionId` is absent). Without the agent in the key, identical short
/// prompts from two unrelated agent streams within a 2 s window would be
/// silently merged.
fn exact_short_dup_key(agent: &str, session_id: &str, trimmed: &str) -> u64 {
    use siphasher::sip::SipHasher13;
    use std::hash::{Hash, Hasher};
    let mut hasher = SipHasher13::new();
    agent.hash(&mut hasher);
    0u8.hash(&mut hasher);
    session_id.hash(&mut hasher);
    0u8.hash(&mut hasher);
    trimmed.hash(&mut hasher);
    hasher.finish()
}

fn drop_exact_short_user_duplicates(messages: Vec<ConversationMessage>) -> ConversationProjection {
    let mut deduped: Vec<ConversationMessage> = Vec::with_capacity(messages.len());
    let mut last_seen_user: HashMap<u64, DateTime<Utc>> = HashMap::new();
    let mut exact_short_duplicates_dropped = 0;

    for msg in messages {
        let trimmed = msg.message.trim();
        let is_short_user = msg.role == "user" && trimmed.len() <= EXACT_SHORT_DUP_MAX_CHARS;
        let is_exact_short_duplicate = if is_short_user {
            let key = exact_short_dup_key(&msg.agent, &msg.session_id, trimmed);
            let is_duplicate = last_seen_user.get(&key).is_some_and(|previous_timestamp| {
                msg.timestamp
                    .signed_duration_since(*previous_timestamp)
                    .num_milliseconds()
                    .abs()
                    <= EXACT_SHORT_DUP_WINDOW_MS
            });

            last_seen_user.insert(key, msg.timestamp);
            is_duplicate
        } else {
            false
        };

        if !is_exact_short_duplicate {
            deduped.push(msg);
        } else {
            exact_short_duplicates_dropped += 1;
        }
    }

    ConversationProjection {
        messages: deduped,
        exact_short_duplicates_dropped,
        harness_noise_dropped: 0,
    }
}

fn classify_conversation_message(message: &str) -> (MessageKind, Option<CollapseStubKind>) {
    let trimmed_start = message.trim_start();

    if trimmed_start.starts_with("<skill-ref:") {
        return (MessageKind::CollapseStub, Some(CollapseStubKind::SkillRef));
    }
    if trimmed_start.starts_with("<dedup-ref:") {
        return (MessageKind::CollapseStub, Some(CollapseStubKind::DedupRef));
    }

    if message.contains("This session is being continued")
        || message.contains("<local-command-caveat>")
        || message.contains("<command-name>/compact</command-name>")
    {
        return (MessageKind::ContinuationSummary, None);
    }

    let workflow_signals = [
        "run_id:",
        "prompt_id:",
        "status: prompt",
        "Perform the vc-",
        "VC Agents Worker Charter",
        "Report path:",
    ];
    let workflow_signal_count = workflow_signals
        .iter()
        .filter(|signal| message.contains(**signal))
        .count();
    if workflow_signal_count >= 2 {
        return (MessageKind::WorkflowPrompt, None);
    }

    (MessageKind::Conversation, None)
}

#[cfg(test)]
mod harness_noise_tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn entry(role: &str, message: &str, ts: i64) -> TimelineEntry {
        TimelineEntry {
            timestamp: Utc.timestamp_opt(ts, 0).unwrap(),
            agent: "claude".into(),
            session_id: "s1".into(),
            role: role.into(),
            message: message.into(),
            frame_kind: Some(if role == "user" {
                FrameKind::UserMsg
            } else {
                FrameKind::AgentReply
            }),
            branch: None,
            cwd: None,
            timestamp_source: None,
        }
    }

    #[test]
    fn drops_head_anchored_slash_command_and_skill_body() {
        let entries = vec![
            entry(
                "user",
                "<command-message>vc-init</command-message>\n<command-name>/vc-init</command-name>\n\nBase directory for this skill: /Users/x/.claude/skills/vc-init\n# vc-init — Technical Due Diligence",
                1,
            ),
            entry("assistant", "Sure, here is the plan.", 2),
        ];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].role, "assistant");
        assert!(
            !projection
                .messages
                .iter()
                .any(|m| m.message.contains("Base directory for this skill"))
        );
    }

    #[test]
    fn drops_local_command_io_turns() {
        let entries = vec![
            entry(
                "user",
                "<local-command-caveat>Caveat: generated while running local commands.</local-command-caveat>\n<bash-input>git status</bash-input>",
                1,
            ),
            entry("user", "<bash-stdout>working tree clean</bash-stdout>", 2),
            entry("assistant", "Looks clean.", 3),
        ];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].message, "Looks clean.");
    }

    #[test]
    fn preserves_pasted_transcript_with_markers_mid_body() {
        // A genuine user turn that pastes a prior transcript containing harness
        // markers deeper in the body must be preserved — the markers are not at
        // the head, so this is real conversation, not a harness injection.
        let pasted = "Analyze this transcript please:\n\n> <command-name>/foo</command-name>\n> <local-command-caveat>noise</local-command-caveat>\nWhat do you make of it?";
        let entries = vec![entry("user", pasted, 1)];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].message, pasted);
    }

    #[test]
    fn classifies_head_blockquote_as_pasted_reference() {
        assert_eq!(
            intent_line_modality("user", "> intent: ship the mirrored plan"),
            IntentLineModality::PastedReference
        );
    }

    #[test]
    fn classifies_head_pasted_text_placeholder_as_pasted_reference() {
        assert_eq!(
            intent_line_modality("user", "[Pasted text #1 +12 lines] Let's ship it"),
            IntentLineModality::PastedReference
        );
    }

    #[test]
    fn classifies_zadanie_head_as_typed_directive() {
        assert_eq!(
            intent_line_modality("user", "Zadanie: dopnij modality gate"),
            IntentLineModality::TypedDirective
        );
    }

    #[test]
    fn preserves_typed_directive_with_reference_markers_mid_body() {
        let line = "Zadanie: analyze quoted material, not as command\n\n> intent: old plan\n[Pasted text #2 +4 lines]";
        assert_eq!(
            intent_line_modality("user", line),
            IntentLineModality::TypedDirective
        );
    }

    #[test]
    fn preserves_assistant_authored_skill_and_hook_content() {
        // Skill-creation / hook-development: assistant authoring skill bodies or
        // hook code is real conversation. Only user-role harness injections are
        // dropped, so assistant content is never matched.
        let entries = vec![entry(
            "assistant",
            "<command-name>/foo</command-name>\nBase directory for this skill: ./skills/foo\nHere is the hook body I propose.",
            1,
        )];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].role, "assistant");
    }

    #[test]
    fn drops_standalone_skill_body_but_keeps_pasted_transcript_quoting_it() {
        let standalone_skill_body = "Base directory for this skill: /Users/x/.claude/skills/vc-init\n\n# vc-init — Technical Due Diligence\n\nThis is harness-injected skill content.";
        // A pasted transcript that QUOTES a skill body deeper in the body must
        // survive: it does not start with the skill-body signature.
        let pasted = "1\t# Conversation Transcript\n2\t\n3\tBase directory for this skill: /Users/x/.claude/skills/vc-init\nPASTED_KEEP";
        let entries = vec![
            entry("user", standalone_skill_body, 1),
            entry("user", pasted, 2),
        ];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.harness_noise_dropped, 1);
        assert!(projection.messages[0].message.contains("PASTED_KEEP"));
        assert!(
            !projection.messages[0]
                .message
                .starts_with("Base directory for this skill")
        );
    }

    #[test]
    fn drops_system_reminder_injection_keeps_real_question() {
        let entries = vec![
            entry("user", "<system-reminder>hook fired</system-reminder>", 1),
            entry("user", "What does store.rs do?", 2),
        ];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].message, "What does store.rs do?");
    }
}
