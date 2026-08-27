//! One content taxonomy between transport normalization and the session model.
//!
//! Adapters construct [`TransportFrame`] values. This module owns the single
//! decision about what those values mean. Projection remains a later concern:
//! shell results are retained here and no adapter is wired to this module yet.

use super::frames_rules::{InjectRuleKind, rules_for};
use super::{AgentKind, Known, RawUnitRef, TurnKind, sha256_hex};
use serde::{Deserialize, Serialize};

pub const FRAME_TAXONOMY_SCHEMA: &str = "aicx.parser.frame_taxonomy.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    DirectMessage,
    UserShellCommand,
    QueueOperation,
    InjectedContext,
    AssistantMessage,
    Lineage,
    AgentMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransportPayload {
    Text {
        role: TransportRole,
        content: String,
    },
    Shell {
        command: String,
        result: String,
    },
    Inject {
        tag: String,
        content: String,
    },
    Lineage {
        session_id: String,
        forked_from_id: Option<String>,
    },
    InterAgent {
        sender: String,
        task: String,
        message_type: String,
        content: String,
    },
}

/// Normalized adapter output immediately before the old per-adapter speech
/// decision. Transport-specific parsing belongs before this boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportFrame {
    pub agent: AgentKind,
    pub transport_kind: TransportKind,
    pub timestamp: Known<String>,
    pub payload: TransportPayload,
    pub evidence: RawUnitRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanChannel {
    Direct,
    EchoBus,
    Queue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retained {
    pub text: String,
    pub chars: u64,
    pub hash: String,
}

impl Retained {
    fn new(text: String) -> Self {
        Self {
            chars: text.chars().count() as u64,
            hash: sha256_hex(text.as_bytes()),
            text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "tag", rename_all = "snake_case")]
pub enum InjectKind {
    CodexInternalContext,
    AgentInstructions,
    CompactionReplay,
    TransportControl,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum FrameClass {
    Human {
        channel: HumanChannel,
    },
    EchoSeal {
        seal_ts: Known<String>,
        channel: HumanChannel,
    },
    ShellAction {
        cmd: String,
        result: Retained,
    },
    Inject {
        kind: InjectKind,
    },
    AssistantFinal,
    LineageMeta {
        session_id: String,
        forked_from_id: Option<String>,
    },
    InterAgent {
        sender: String,
        task: String,
        message_type: String,
    },
}

impl FrameClass {
    /// Maps the new taxonomy onto the existing model instead of replacing it.
    /// `InterAgent` deliberately has no `TurnKind`: forcing it into
    /// `AgentReply` would recreate the assistant-lane leak this taxonomy fixes.
    pub const fn turn_kind(&self) -> Option<TurnKind> {
        match self {
            Self::Human { .. } | Self::EchoSeal { .. } => Some(TurnKind::UserMsg),
            Self::ShellAction { .. } => Some(TurnKind::ToolCall),
            Self::Inject { .. } | Self::LineageMeta { .. } => Some(TurnKind::SystemNote),
            Self::AssistantFinal => Some(TurnKind::AgentReply),
            Self::InterAgent { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameSeal {
    pub seal_ts: Known<String>,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameOrigin {
    pub agent: AgentKind,
    pub transport_kind: TransportKind,
    pub evidence: RawUnitRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifiedFrame {
    pub schema: String,
    pub class: FrameClass,
    pub content: String,
    /// Stable identity across compaction, where transport `msg_*` ids rotate.
    pub content_hash: String,
    pub seal: FrameSeal,
    pub origin: FrameOrigin,
    pub turn_kind: Option<TurnKind>,
}

pub fn classify(frame: &TransportFrame) -> ClassifiedFrame {
    let rules = rules_for(frame.agent);
    let (class, content) = match &frame.payload {
        TransportPayload::Shell { command, result } => {
            if let Some(content) = echo_payload(command, rules.echo_promotion, rules.klops_guard) {
                (
                    FrameClass::EchoSeal {
                        seal_ts: frame.timestamp.clone(),
                        channel: HumanChannel::EchoBus,
                    },
                    content,
                )
            } else {
                (
                    FrameClass::ShellAction {
                        cmd: command.clone(),
                        result: Retained::new(result.clone()),
                    },
                    command.clone(),
                )
            }
        }
        TransportPayload::Inject { tag, content } => (
            FrameClass::Inject {
                kind: inject_kind(tag, rules.inject_tags),
            },
            content.clone(),
        ),
        TransportPayload::Lineage {
            session_id,
            forked_from_id,
        } => (
            FrameClass::LineageMeta {
                session_id: session_id.clone(),
                forked_from_id: forked_from_id.clone(),
            },
            session_id.clone(),
        ),
        TransportPayload::InterAgent {
            sender,
            task,
            message_type,
            content,
        } => (
            FrameClass::InterAgent {
                sender: sender.clone(),
                task: task.clone(),
                message_type: message_type.clone(),
            },
            content.clone(),
        ),
        TransportPayload::Text { role, content } => match role {
            TransportRole::User if frame.transport_kind == TransportKind::QueueOperation => {
                let class = if rules.queue_seal {
                    FrameClass::EchoSeal {
                        seal_ts: frame.timestamp.clone(),
                        channel: HumanChannel::Queue,
                    }
                } else {
                    FrameClass::Human {
                        channel: HumanChannel::Queue,
                    }
                };
                (class, content.clone())
            }
            TransportRole::User => (
                FrameClass::Human {
                    channel: HumanChannel::Direct,
                },
                content.clone(),
            ),
            TransportRole::Assistant => (FrameClass::AssistantFinal, content.clone()),
            TransportRole::System => (
                FrameClass::Inject {
                    kind: InjectKind::TransportControl,
                },
                content.clone(),
            ),
            TransportRole::Tool => (
                FrameClass::ShellAction {
                    cmd: content.clone(),
                    result: Retained::new(String::new()),
                },
                content.clone(),
            ),
        },
    };

    let content_hash = sha256_hex(content.as_bytes());
    let turn_kind = class.turn_kind();
    ClassifiedFrame {
        schema: FRAME_TAXONOMY_SCHEMA.to_owned(),
        class,
        content,
        seal: FrameSeal {
            seal_ts: frame.timestamp.clone(),
            content_hash: content_hash.clone(),
        },
        content_hash,
        origin: FrameOrigin {
            agent: frame.agent,
            transport_kind: frame.transport_kind,
            evidence: frame.evidence.clone(),
        },
        turn_kind,
    }
}

fn echo_payload(command: &str, enabled: bool, guards: &[&str]) -> Option<String> {
    if !enabled || guards.iter().any(|guard| command.contains(guard)) {
        return None;
    }
    let payload = command.trim().strip_prefix("echo")?;
    if !payload.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let payload = payload.trim();
    if payload.is_empty() {
        return None;
    }
    let payload = if payload.len() >= 2
        && ((payload.starts_with('\'') && payload.ends_with('\''))
            || (payload.starts_with('"') && payload.ends_with('"')))
    {
        &payload[1..payload.len() - 1]
    } else {
        payload
    };
    Some(payload.to_owned())
}

fn inject_kind(tag: &str, rules: &[super::frames_rules::InjectTagRule]) -> InjectKind {
    let Some(rule) = rules.iter().find(|rule| rule.tag == tag) else {
        return InjectKind::Other(tag.to_owned());
    };
    match rule.kind {
        InjectRuleKind::CodexInternalContext => InjectKind::CodexInternalContext,
        InjectRuleKind::AgentInstructions => InjectKind::AgentInstructions,
        InjectRuleKind::CompactionReplay => InjectKind::CompactionReplay,
        InjectRuleKind::TransportControl => InjectKind::TransportControl,
    }
}
