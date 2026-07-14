//! Frozen adapter boundary and exhaustive built-in adapter registry.

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod grok;
pub mod junie;

pub use claude::ClaudeAdapter;
pub use codex::CodexAdapter;
pub use gemini::GeminiAdapter;
pub use grok::GrokAdapter;
pub use junie::JunieAdapter;

use crate::engine::{
    AgentKind, RawUnitRef, SkippedReason, SourceHandle, SourceRead, UnvalidatedParse,
};
use std::fmt;

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// A classified unit is exhaustive input to assembly; one record must exist for
/// every reader unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedUnit {
    pub ordinal: u64,
    pub level: RawUnitLevel,
    pub evidence: RawUnitRef,
    pub disposition: ClassifiedDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawUnitLevel {
    Physical,
    Logical { parent_ordinal: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassifiedDisposition {
    Consumed {
        kind: String,
    },
    Skipped {
        reason: SkippedReason,
        visible: bool,
    },
}

/// Provider parser contract.
///
/// The trait is sealed so provider adapters can be added only inside this crate;
/// downstream crates consume `ParserEngine` and cannot bypass kernel validation.
///
/// ```compile_fail
/// use aicx_parser::adapters::AgentAdapter;
/// struct ExternalAdapter;
/// impl AgentAdapter for ExternalAdapter {}
/// ```
pub trait AgentAdapter: sealed::Sealed + Send + Sync {
    fn agent(&self) -> AgentKind;

    fn adapter_version(&self) -> &'static str;

    fn classify(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
    ) -> Result<Vec<ClassifiedUnit>, AdapterError>;

    fn assemble(
        &self,
        source: &SourceHandle,
        read: &SourceRead,
        classified: Vec<ClassifiedUnit>,
    ) -> Result<UnvalidatedParse, AdapterError>;
}

static CODEX: CodexAdapter = CodexAdapter;
static CLAUDE: ClaudeAdapter = ClaudeAdapter;
static GEMINI: GeminiAdapter = GeminiAdapter;
static GROK: GrokAdapter = GrokAdapter;
static JUNIE: JunieAdapter = JunieAdapter;

/// Return the one registered parser for an exhaustive [`AgentKind`].
pub const fn registered_adapter(agent: AgentKind) -> &'static dyn AgentAdapter {
    match agent {
        AgentKind::Codex => &CODEX,
        AgentKind::Claude => &CLAUDE,
        AgentKind::Gemini => &GEMINI,
        AgentKind::Grok => &GROK,
        AgentKind::Junie => &JUNIE,
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn registry_is_exhaustive_and_identity_preserving() {
        for agent in [
            AgentKind::Codex,
            AgentKind::Claude,
            AgentKind::Gemini,
            AgentKind::Grok,
            AgentKind::Junie,
        ] {
            let adapter = registered_adapter(agent);
            assert_eq!(adapter.agent(), agent);
            assert!(!adapter.adapter_version().is_empty());
        }
    }

    #[test]
    fn codex_and_grok_are_distinct_registrations() {
        assert_ne!(
            registered_adapter(AgentKind::Codex).agent(),
            registered_adapter(AgentKind::Grok).agent()
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterError {
    pub stage: &'static str,
    pub detail: String,
}

impl AdapterError {
    pub fn new(stage: &'static str, detail: impl Into<String>) -> Self {
        Self {
            stage,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "adapter {} failed: {}", self.stage, self.detail)
    }
}

impl std::error::Error for AdapterError {}
