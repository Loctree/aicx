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
    InjectTagRule {
        tag: "codex_internal_context",
        kind: InjectRuleKind::CodexInternalContext,
    },
    InjectTagRule {
        tag: "AGENTS.md",
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

const CLAUDE_INJECT_TAGS: &[InjectTagRule] = &[
    InjectTagRule {
        tag: "system",
        kind: InjectRuleKind::TransportControl,
    },
    InjectTagRule {
        tag: "compact_boundary",
        kind: InjectRuleKind::CompactionReplay,
    },
];

const GENERIC_INJECT_TAGS: &[InjectTagRule] = &[InjectTagRule {
    tag: "system",
    kind: InjectRuleKind::TransportControl,
}];

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
    AgentFrameRules {
        agent: AgentKind::Gemini,
        echo_promotion: false,
        klops_guard: &[],
        queue_seal: false,
        inject_tags: GENERIC_INJECT_TAGS,
    },
    AgentFrameRules {
        agent: AgentKind::Grok,
        echo_promotion: false,
        klops_guard: &[],
        queue_seal: false,
        inject_tags: GENERIC_INJECT_TAGS,
    },
    // # junie — no transport idioms
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
