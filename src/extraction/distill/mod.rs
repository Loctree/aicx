//! Per-agent lane distillation over the parsed
//! [`SessionModel`](aicx_parser::engine::SessionModel).
//!
//! This module is the shared W0 contract for the per-agent distiller lanes
//! (W1). It is a **projection layer**: distillers read the substrate and
//! emit heuristic handoff signals; they never mutate the model, never feed
//! the canonical fingerprint (PARSER_NORMATIVE_CONTRACT C0A §1.1), and never
//! act as a second reducer (OUTPUT_PROJECTION_CONTRACT).
//!
//! Field names are ported from the Transcript Builder `index_payload.v1`
//! schema (`decision_candidates`, `gates`, `open_questions`, `agent_outcome`,
//! `ending`, `evidence_locator`) so the TB differential oracle
//! (`tests/tb_oracle_harness.rs`) can compare common fields name-for-name.
//! The full TB→aicx mapping lives in `docs/DISTILL_CONTRACT.md`.
//!
//! # Append-only etiquette (W1 waves)
//!
//! Seven parallel workers extend this file. Re-read it immediately before
//! editing; add new lane modules and registry entries at the marked spots;
//! never rewrite or reorder someone else's additions.

use aicx_parser::engine::{AgentKind, ScopeStatus, Segment, SessionModel};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Schema tag stamped on every [`SegmentDistillate`].
pub const SEGMENT_DISTILLATE_SCHEMA: &str = "aicx.distill.segment_distillate.v1";

/// Pointer from a distilled claim back into the substrate that produced it.
///
/// TB name: `evidence_locator`. Every heuristic claim must be traceable to a
/// concrete turn; a claim without a locator is narration, not evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceLocator {
    /// Segment the claim was observed in ([`Segment::segment_id`]).
    pub segment_id: u32,
    /// Turn index inside the session's `turns` vector, when attributable.
    pub turn_idx: Option<u64>,
    /// RFC 3339 timestamp of the evidence turn, when the substrate carries one.
    pub timestamp: Option<String>,
}

/// A decision the session *appears* to have made — candidate, not verdict.
///
/// TB name: `decision_candidates[]`. Distillers surface candidates; nothing
/// downstream may promote one to "decided" without operator/AICX intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionCandidate {
    /// The distilled decision statement, verbatim-leaning.
    pub text: String,
    /// Lane-specific classification (TB vocabulary is free-form strings,
    /// e.g. `explicit_choice`, `plan_commitment`); empty means unclassified.
    pub kind: String,
    /// Where in the substrate the candidate was observed.
    pub evidence: EvidenceLocator,
}

/// Outcome of one quality-gate observation.
///
/// TB name: `gates[]` (per-chunk). `Unknown` means the gate ran but the lane
/// could not read a verdict — it is not a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcome {
    Pass,
    Fail,
    Unknown,
}

/// One observed quality-gate run (test, lint, build) inside the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateObservation {
    /// The command as the substrate recorded it (e.g. `cargo test -p aicx`).
    pub command: String,
    /// Verdict read from the substrate, never guessed.
    pub outcome: GateOutcome,
    /// Where the gate run was observed.
    pub evidence: EvidenceLocator,
}

/// A question the session left unanswered.
///
/// TB name: `open_questions[]` with items `{kind, text, evidence_locator}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenQuestion {
    /// TB vocabulary passthrough (e.g. `user_report`); empty = unclassified.
    pub kind: String,
    /// The open question or unresolved report, verbatim-leaning.
    pub text: String,
    /// Where the question was left open.
    pub evidence: EvidenceLocator,
}

/// A handoff-relevant closing signal (TB human.md "Handoff Signals" section;
/// typically the last `assistant_final` of a segment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffSignal {
    /// The signal text, verbatim-leaning.
    pub text: String,
    /// Where the signal was emitted.
    pub evidence: EvidenceLocator,
}

/// Heuristic verdict on how the lane's work ended inside one segment.
///
/// TB name: `agent_outcome`. `Unknown` is the honest default; a lane may only
/// upgrade it on substrate evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentOutcome {
    Complete,
    Partial,
    Failed,
    #[default]
    Unknown,
}

/// Per-segment outcome block.
///
/// TB names: `agent_outcome` + `ending` (session-tail observation such as
/// `interrupted`, kept as TB vocabulary passthrough).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LaneOutcome {
    /// Heuristic completion verdict for the segment.
    pub agent_outcome: AgentOutcome,
    /// TB `ending` vocabulary passthrough (e.g. `interrupted`), when observed.
    pub ending: Option<String>,
}

/// The distillate of one segment: everything a handoff reader needs, with
/// evidence locators back into the substrate.
///
/// One distillate per segment is the binding shape (Design Contract decision
/// 4: brief per segment for `mixed_candidate` sessions — outcomes are never
/// averaged across workstreams).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentDistillate {
    /// Always [`SEGMENT_DISTILLATE_SCHEMA`].
    pub schema: String,
    /// Lane that produced this distillate.
    pub agent: AgentKind,
    /// Segment this distillate projects ([`Segment::segment_id`]).
    pub segment_id: u32,
    /// Structural scope verdict copied from the segment (never recomputed).
    pub scope_status: ScopeStatus,
    /// TB `decision_candidates[]`.
    pub decision_candidates: Vec<DecisionCandidate>,
    /// TB `gates[]`.
    pub gates: Vec<GateObservation>,
    /// TB `open_questions[]`.
    pub open_questions: Vec<OpenQuestion>,
    /// TB human.md "Handoff Signals".
    pub handoff_signals: Vec<HandoffSignal>,
    /// TB `agent_outcome` + `ending`.
    pub outcome: LaneOutcome,
}

impl SegmentDistillate {
    /// The empty-but-honest distillate: no claims, `Unknown` outcome. This is
    /// what [`GenericLane`] returns and what any lane should degrade to when
    /// the substrate carries no readable signal.
    pub fn empty(agent: AgentKind, segment: &Segment) -> Self {
        Self {
            schema: SEGMENT_DISTILLATE_SCHEMA.to_owned(),
            agent,
            segment_id: segment.segment_id,
            scope_status: segment.scope_status,
            decision_candidates: Vec::new(),
            gates: Vec::new(),
            open_questions: Vec::new(),
            handoff_signals: Vec::new(),
            outcome: LaneOutcome::default(),
        }
    }
}

/// One per-agent distiller lane.
///
/// Implementations read the substrate (`&SessionModel`) plus the segment in
/// scope and return a [`SegmentDistillate`]. They must be pure projections:
/// no I/O on the model, no mutation, no fingerprint-relevant output.
pub trait AgentLaneDistiller: Send + Sync {
    /// Which agent's sessions this lane understands.
    fn agent(&self) -> AgentKind;

    /// Stable lane name for diagnostics and registry introspection
    /// (`"generic"` for the fallback lane; per-agent lanes use the agent
    /// name, e.g. `"claude"`).
    fn lane_name(&self) -> &'static str;

    /// Distill one segment of the session.
    fn distill_segment(&self, model: &SessionModel, segment: &Segment) -> SegmentDistillate;

    /// Distill every segment, in segment order. Default loops over
    /// [`SessionModel::segments`]; lanes rarely need to override this.
    fn distill(&self, model: &SessionModel) -> Vec<SegmentDistillate> {
        model
            .segments
            .iter()
            .map(|segment| self.distill_segment(model, segment))
            .collect()
    }
}

/// Fallback lane for agents without a registered distiller.
///
/// Fail-open on emptiness: it returns [`SegmentDistillate::empty`] for every
/// segment — an empty distillate is an honest answer, a panic is not.
#[derive(Debug, Clone, Copy)]
pub struct GenericLane {
    agent: AgentKind,
}

impl GenericLane {
    pub const fn new(agent: AgentKind) -> Self {
        Self { agent }
    }
}

impl AgentLaneDistiller for GenericLane {
    fn agent(&self) -> AgentKind {
        self.agent
    }

    fn lane_name(&self) -> &'static str {
        "generic"
    }

    fn distill_segment(&self, _model: &SessionModel, segment: &Segment) -> SegmentDistillate {
        SegmentDistillate::empty(self.agent, segment)
    }
}

/// All parser-supported agents, in registry iteration order. W1 lanes cover
/// this list; anything absent from the registry falls back to [`GenericLane`].
const ALL_AGENTS: [AgentKind; 6] = [
    AgentKind::Claude,
    AgentKind::Codex,
    AgentKind::Gemini,
    AgentKind::Grok,
    AgentKind::Junie,
    AgentKind::Kimi,
];

/// Registry mapping [`AgentKind`] to its distiller lane.
///
/// `lane_for` never fails: an unregistered agent gets that agent's
/// pre-seeded [`GenericLane`].
pub struct LaneRegistry {
    lanes: HashMap<AgentKind, Box<dyn AgentLaneDistiller>>,
    fallbacks: HashMap<AgentKind, GenericLane>,
}

impl LaneRegistry {
    /// An empty registry: every agent resolves to its [`GenericLane`].
    pub fn new() -> Self {
        Self {
            lanes: HashMap::new(),
            fallbacks: ALL_AGENTS
                .into_iter()
                .map(|agent| (agent, GenericLane::new(agent)))
                .collect(),
        }
    }

    /// Register (or replace) the lane for `lane.agent()`.
    pub fn register(&mut self, lane: Box<dyn AgentLaneDistiller>) {
        self.lanes.insert(lane.agent(), lane);
    }

    /// Resolve the lane for `agent`; unregistered agents get [`GenericLane`].
    pub fn lane_for(&self, agent: AgentKind) -> &dyn AgentLaneDistiller {
        match self.lanes.get(&agent) {
            Some(lane) => lane.as_ref(),
            None => &self.fallbacks[&agent],
        }
    }

    /// The registry every consumer should start from: all W1 lanes wired in.
    ///
    /// W1 workers: append your lane's `registry.register(...)` line below,
    /// keeping existing registrations untouched (append-only etiquette).
    pub fn with_default_lanes() -> Self {
        let mut registry = Self::new();
        // W1 append zone — one `registry.register(Box::new(...))` per lane.
        registry.register(Box::new(claude_lane::ClaudeLane));
        registry.register(Box::new(codex_lane::CodexLane));
        registry.register(Box::new(gemini::GeminiLane::new()));
        registry.register(Box::new(grok_lane::GrokLane));
        registry.register(Box::new(junie::JunieLane::new()));
        registry
    }
}

impl Default for LaneRegistry {
    fn default() -> Self {
        Self::with_default_lanes()
    }
}

// W1 append zone — one `pub mod <agent>_lane;` per worker, added below this
// line without touching the shared contract above.
pub mod claude_lane;
pub mod codex_lane;
pub mod gemini;
pub mod grok_lane;
pub mod junie;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unregistered_agent_gets_generic_lane() {
        let registry = LaneRegistry::new();
        for agent in ALL_AGENTS {
            let lane = registry.lane_for(agent);
            assert_eq!(lane.agent(), agent);
            assert_eq!(lane.lane_name(), "generic");
        }
    }

    #[test]
    fn generic_lane_distills_to_empty_not_panic() {
        let segment = Segment {
            segment_id: 7,
            cwd: aicx_parser::engine::Known::unknown(),
            branch: aicx_parser::engine::Known::unknown(),
            started_at: aicx_parser::engine::Known::unknown(),
            ended_at: aicx_parser::engine::Known::unknown(),
            turn_range: aicx_parser::engine::TurnRange { start: 0, end: 0 },
            scope_status: ScopeStatus::Unknown,
        };
        let lane = GenericLane::new(AgentKind::Kimi);
        let model = minimal_model(AgentKind::Kimi);
        let distillate = lane.distill_segment(&model, &segment);
        assert_eq!(distillate.schema, SEGMENT_DISTILLATE_SCHEMA);
        assert_eq!(distillate.segment_id, 7);
        assert_eq!(distillate.agent, AgentKind::Kimi);
        assert!(distillate.decision_candidates.is_empty());
        assert!(distillate.gates.is_empty());
        assert!(distillate.open_questions.is_empty());
        assert!(distillate.handoff_signals.is_empty());
        assert_eq!(distillate.outcome.agent_outcome, AgentOutcome::Unknown);
    }

    fn minimal_model(agent: AgentKind) -> SessionModel {
        use aicx_parser::engine::{
            BoundaryFlags, CoverageReport, Known, ParseStatus, Provenance, VisibleCompleteness,
        };
        let provenance = Provenance {
            agent,
            model: Known::unknown(),
            cli_version: Known::unknown(),
            cwd: Known::unknown(),
            branch: Known::unknown(),
            started_at: Known::unknown(),
            ended_at: Known::unknown(),
            original_source_hash: "sha256:test".to_owned(),
            original_source_bytes: 0,
        };
        let coverage = CoverageReport {
            raw_line_count: 0,
            raw_unit_count: 0,
            consumed_count: 0,
            skipped_count: 0,
            consumed_ranges: Vec::new(),
            consumed: Vec::new(),
            skipped: Vec::new(),
            warnings: Vec::new(),
            status: ParseStatus {
                visible_completeness: VisibleCompleteness::CompleteVisible,
                boundary_flags: BoundaryFlags::default(),
                malformed_tail_present: false,
                visible_event_lost: false,
            },
            consumed_by_kind: Default::default(),
            known_skipped: Default::default(),
        };
        SessionModel::new("distill-test", provenance, coverage)
    }
}
