//! Transport idioms expressed as data for the shared frame classifier.

use super::AgentKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectRuleKind {
    CodexInternalContext,
    AgentInstructions,
    CompactionReplay,
    TransportControl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InjectTagRule {
    pub tag: &'static str,
    pub kind: InjectRuleKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentFrameRules {
    pub agent: AgentKind,
    pub echo_promotion: bool,
    pub klops_guard: &'static [&'static str],
    pub queue_seal: bool,
    pub inject_tags: &'static [InjectTagRule],
}

const CODEX_INJECT_TAGS: &[InjectTagRule] = &[
    // # codex — transport envelope spellings normalized by codex.rs.
    InjectTagRule {
        tag: "codex_internal_context",
        kind: InjectRuleKind::CodexInternalContext,
    },
    InjectTagRule {
        tag: "AGENTS.md",
        kind: InjectRuleKind::AgentInstructions,
    },
    InjectTagRule {
        tag: "INSTRUCTIONS",
        kind: InjectRuleKind::AgentInstructions,
    },
    InjectTagRule {
        tag: "instructions",
        kind: InjectRuleKind::AgentInstructions,
    },
    InjectTagRule {
        tag: "user_instructions",
        kind: InjectRuleKind::AgentInstructions,
    },
    InjectTagRule {
        tag: "compacted.replacement_history",
        kind: InjectRuleKind::CompactionReplay,
    },
    InjectTagRule {
        tag: "environment_context",
        kind: InjectRuleKind::TransportControl,
    },
    InjectTagRule {
        tag: "permissions instructions",
        kind: InjectRuleKind::TransportControl,
    },
];

// # claude — transport idioms (owner: W2-T8). A tag is recognized by the
// adapter only at the head of an operator-lane payload (`<tag>` / `<tag `);
// what it means is decided here, by data.
const CLAUDE_INJECT_TAGS: &[InjectTagRule] = &[
    InjectTagRule {
        tag: "system",
        kind: InjectRuleKind::TransportControl,
    },
    InjectTagRule {
        tag: "compact_boundary",
        kind: InjectRuleKind::CompactionReplay,
    },
    // Background-task completion pushed by the harness, observed both as a
    // `queue-operation` enqueue body and as a user-row text block. Machine
    // chatter: a system note, never operator speech.
    InjectTagRule {
        tag: "task-notification",
        kind: InjectRuleKind::TransportControl,
    },
    // Harness-appended context block inside the user lane (hook output,
    // memory recall, mode reminders). Injected, never spoken.
    InjectTagRule {
        tag: "system-reminder",
        kind: InjectRuleKind::TransportControl,
    },
];
// # claude — end

// # gemini
const GEMINI_INJECT_TAGS: &[InjectTagRule] = &[
    InjectTagRule {
        tag: "system",
        kind: InjectRuleKind::TransportControl,
    },
    InjectTagRule {
        tag: "system_instruction",
        kind: InjectRuleKind::AgentInstructions,
    },
    InjectTagRule {
        tag: "environment_context",
        kind: InjectRuleKind::TransportControl,
    },
    InjectTagRule {
        tag: "context",
        kind: InjectRuleKind::TransportControl,
    },
];

const GENERIC_INJECT_TAGS: &[InjectTagRule] = &[InjectTagRule {
    tag: "system",
    kind: InjectRuleKind::TransportControl,
}];

// # grok
// Transport idioms for Grok chat_history.jsonl. `synthetic_reason` is a
// harness inject tag, not human speech. No echo-bus / queue-seal on this
// transport. `type=reasoning` has no FrameClass (BOUNDARY) and is delivered
// as Inject tag "reasoning" → Other → SystemNote.
const GROK_INJECT_TAGS: &[InjectTagRule] = &[
    InjectTagRule {
        tag: "system",
        kind: InjectRuleKind::TransportControl,
    },
    InjectTagRule {
        tag: "system_reminder",
        kind: InjectRuleKind::TransportControl,
    },
    InjectTagRule {
        tag: "project_instructions",
        kind: InjectRuleKind::AgentInstructions,
    },
];
// # grok

pub const AGENT_FRAME_RULES: &[AgentFrameRules] = &[
    AgentFrameRules {
        agent: AgentKind::Codex,
        echo_promotion: true,
        klops_guard: &["| tee", ">>"],
        queue_seal: false,
        inject_tags: CODEX_INJECT_TAGS,
    },
    AgentFrameRules {
        agent: AgentKind::Claude,
        echo_promotion: false,
        klops_guard: &[],
        queue_seal: true,
        inject_tags: CLAUDE_INJECT_TAGS,
    },
    // # gemini
    AgentFrameRules {
        agent: AgentKind::Gemini,
        echo_promotion: false,
        klops_guard: &[],
        queue_seal: false,
        inject_tags: GEMINI_INJECT_TAGS,
    },
    // # grok
    AgentFrameRules {
        agent: AgentKind::Grok,
        echo_promotion: false,
        klops_guard: &[],
        queue_seal: false,
        inject_tags: GROK_INJECT_TAGS,
    },
    // # grok
    AgentFrameRules {
        agent: AgentKind::Junie,
        echo_promotion: false,
        klops_guard: &[],
        queue_seal: false,
        inject_tags: GENERIC_INJECT_TAGS,
    },
];

pub fn rules_for(agent: AgentKind) -> &'static AgentFrameRules {
    AGENT_FRAME_RULES
        .iter()
        .find(|rules| rules.agent == agent)
        .expect("every AgentKind has frame rules")
}
