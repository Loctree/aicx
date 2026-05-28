//! Conversation projection and deduplication logic.
//!
//! This module is responsible for turning raw timeline entries into clean,
//! denoised conversation streams suitable for downstream use (intents, reports, etc.).
//!
//! Extracted during the 2026-05-27 sources monolith decomposition.

use chrono::{DateTime, Utc};
use std::collections::HashMap;

use crate::timeline::{
    CollapseStubKind, ConversationMessage, FrameKind, MessageKind, TimelineEntry,
};

use super::project_filter::repo_name_from_cwd;

const EXACT_SHORT_DUP_MAX_CHARS: usize = 1000;
const EXACT_SHORT_DUP_WINDOW_MS: i64 = 2_000;

#[derive(Debug, Clone)]
pub struct ConversationProjection {
    pub messages: Vec<ConversationMessage>,
    pub exact_short_duplicates_dropped: usize,
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
    let messages: Vec<ConversationMessage> = entries
        .iter()
        .filter(|entry| {
            matches!(
                entry.frame_kind,
                Some(FrameKind::UserMsg | FrameKind::AgentReply)
            ) || (entry.frame_kind.is_none() && (entry.role == "user" || entry.role == "assistant"))
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

    drop_exact_short_user_duplicates(messages)
}

/// Compute a stable 64-bit key for `(agent, session_id, trimmed message)`.
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
    }
}

pub(crate) fn classify_conversation_message(
    message: &str,
) -> (MessageKind, Option<CollapseStubKind>) {
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

    if workflow_signals.iter().any(|s| message.contains(s)) {
        return (MessageKind::WorkflowPrompt, None);
    }

    (MessageKind::Conversation, None)
}
