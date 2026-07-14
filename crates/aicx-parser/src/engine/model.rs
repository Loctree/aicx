//! Typed parser model. Heuristic intent/outcome/title fields do not live here.

use super::coverage::CoverageReport;
use super::source::AgentKind;
use serde::{Deserialize, Serialize};

pub const SESSION_MODEL_SCHEMA: &str = "aicx.parser.session_model.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Known<T> {
    Unknown(UnknownValue),
    Value(T),
}

impl<T> Known<T> {
    pub const fn unknown() -> Self {
        Self::Unknown(UnknownValue::Unknown)
    }

    pub const fn value(value: T) -> Self {
        Self::Value(value)
    }

    pub const fn as_ref(&self) -> Known<&T> {
        match self {
            Self::Value(value) => Known::Value(value),
            Self::Unknown(_) => Known::Unknown(UnknownValue::Unknown),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnknownValue {
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub agent: AgentKind,
    pub model: Known<String>,
    pub cli_version: Known<String>,
    pub cwd: Known<String>,
    pub branch: Known<String>,
    pub started_at: Known<String>,
    pub ended_at: Known<String>,
    pub original_source_hash: String,
    pub original_source_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub segment_id: u32,
    pub cwd: Known<String>,
    pub branch: Known<String>,
    pub started_at: Known<String>,
    pub ended_at: Known<String>,
    pub turn_range: TurnRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInvocation {
    pub turn_idx: u64,
    pub skill_name: String,
    pub payload_hash: String,
    pub payload_bytes: u64,
    pub first_invoked_at: Known<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKind {
    UserMsg,
    AgentReply,
    InternalThought,
    ToolCall,
    ToolResult,
    SystemNote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawUnitRef {
    pub evidence_event_id: String,
    pub coverage_ordinal: u64,
    pub physical_ordinal: u64,
    pub locator: String,
    pub unit_kind: String,
    pub artifact: String,
    pub content_hash: String,
    pub original_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub turn_idx: u64,
    pub role: TurnRole,
    pub timestamp: Known<String>,
    pub kind: TurnKind,
    /// Full source text is retained for deterministic projections. The canonical
    /// fingerprint uses only `text_hash` and `text_chars`.
    pub text: String,
    pub text_hash: String,
    pub text_chars: u64,
    pub tool_name: Known<String>,
    pub segment_id: u32,
    pub raw_unit_refs: Vec<RawUnitRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEventKind {
    Call,
    Result,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolEvent {
    pub kind: ToolEventKind,
    pub turn_idx: u64,
    pub tool_name: String,
    pub correlation_id: Known<String>,
    pub payload_hash: String,
    pub payload_bytes: u64,
    pub raw_unit_refs: Vec<RawUnitRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterSemantics {
    Snapshot,
    Delta,
    Cumulative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenComponents {
    pub input: Known<u64>,
    pub output: Known<u64>,
    pub reasoning: Known<u64>,
    pub cache_read: Known<u64>,
    pub cache_creation: Known<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportedCost {
    pub amount: f64,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSpan {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageEvent {
    pub provider: String,
    pub model: Known<String>,
    pub tokens: TokenComponents,
    pub cost: Known<ReportedCost>,
    pub timestamp: Known<String>,
    pub span: Known<UsageSpan>,
    pub counter_semantics: CounterSemantics,
    pub evidence: RawUnitRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionModel {
    pub schema: String,
    pub session_id: String,
    pub provenance: Provenance,
    pub segments: Vec<Segment>,
    pub skill_invocations: Vec<SkillInvocation>,
    pub turns: Vec<Turn>,
    pub tool_events: Vec<ToolEvent>,
    pub usage_events: Vec<UsageEvent>,
    pub coverage: CoverageReport,
}

impl SessionModel {
    pub fn new(
        session_id: impl Into<String>,
        provenance: Provenance,
        coverage: CoverageReport,
    ) -> Self {
        Self {
            schema: SESSION_MODEL_SCHEMA.to_owned(),
            session_id: session_id.into(),
            provenance,
            segments: Vec::new(),
            skill_invocations: Vec::new(),
            turns: Vec::new(),
            tool_events: Vec::new(),
            usage_events: Vec::new(),
            coverage,
        }
    }
}
