//! card.v3: materialization of the lane distillate on the INDEX document.
//!
//! Architectural decision (W2-02, 2026-09-18, signed: claude): the live card
//! surface of aicx is the per-session index document (`source_index` →
//! tantivy `metadata_json`), not the legacy chunk store — `write_chunks_to_dir`
//! has no runtime callers, so materializing v3 there would decorate a dead
//! surface. `card.v3` therefore means: the index document's metadata carries
//! a `distill` block plus flat filterable scalars, and `card_schema` names
//! the version. Documents indexed before this cut (or reused from the
//! incremental cache) stay v2 until their session is re-parsed — coverage is
//! REPORTED, never faked (Design Contract: „jawnie, zamiast udawać pełnię").
//!
//! One implementation, two consumers: this module and `extraction::brief`
//! both read the same [`SegmentDistillate`] values from the same
//! [`LaneRegistry`] — search and `--brief` cannot drift apart.

use aicx_parser::engine::SessionModel;
use serde_json::{Value, json};

use super::{AgentOutcome, GateOutcome, LaneRegistry, SegmentDistillate};

/// `card_schema` value stamped on documents that carry a distill block.
pub const CARD_SCHEMA_V3: &str = "card.v3";

/// Compute the session's distillate once, through the agent's registered
/// lane (`GenericLane` fail-open). Shared entry for the index materializer
/// and any caller that wants the same values the brief renders.
pub fn session_distillates(model: &SessionModel) -> Vec<SegmentDistillate> {
    LaneRegistry::with_default_lanes()
        .lane_for(model.provenance.agent)
        .distill(model)
}

/// Materialize the distillate as the card.v3 metadata block for one index
/// document. Returns the `distill` object plus flat filter scalars; the
/// caller merges them into the document metadata.
///
/// Flat scalars exist because the tantivy adapter's `filter_matches` is a
/// generic equality over top-level metadata keys — `has_decisions: "true"`
/// is filterable today without any adapter or schema-version change (and
/// therefore without the full-reindex Founder button).
pub fn index_metadata(distillates: &[SegmentDistillate]) -> IndexDistillate {
    let mut decisions = Vec::new();
    let mut gates = Vec::new();
    let mut open_questions = Vec::new();
    let mut handoff_signals = Vec::new();
    let mut outcomes = Vec::new();
    let mut failing_gates = 0usize;
    for distillate in distillates {
        outcomes.push(outcome_token(distillate.outcome.agent_outcome).to_owned());
        for decision in &distillate.decision_candidates {
            decisions.push(json!({
                "text": decision.text,
                "kind": decision.kind,
                "segment": decision.evidence.segment_id,
            }));
        }
        for gate in &distillate.gates {
            if gate.outcome == GateOutcome::Fail {
                failing_gates += 1;
            }
            gates.push(json!({
                "command": gate.command,
                "outcome": gate_token(gate.outcome),
                "segment": gate.evidence.segment_id,
            }));
        }
        for question in &distillate.open_questions {
            open_questions.push(json!({
                "text": question.text,
                "kind": question.kind,
                "segment": question.evidence.segment_id,
            }));
        }
        for signal in &distillate.handoff_signals {
            handoff_signals.push(Value::String(signal.text.clone()));
        }
    }
    IndexDistillate {
        counts: DistillCounts {
            decisions: decisions.len(),
            gates: gates.len(),
            failing_gates,
            open_questions: open_questions.len(),
        },
        distill: json!({
            "schema": super::SEGMENT_DISTILLATE_SCHEMA,
            "segments": distillates.len(),
            "outcomes": outcomes,
            "decisions": decisions,
            "gates": gates,
            "open_questions": open_questions,
            "handoff_signals": handoff_signals,
        }),
    }
}

/// The materialized block plus the counts the flat filter scalars derive from.
pub struct IndexDistillate {
    pub distill: Value,
    pub counts: DistillCounts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DistillCounts {
    pub decisions: usize,
    pub gates: usize,
    pub failing_gates: usize,
    pub open_questions: usize,
}

impl IndexDistillate {
    /// Merge the card.v3 fields into one document's metadata object.
    pub fn merge_into(&self, metadata: &mut Value) {
        let Some(map) = metadata.as_object_mut() else {
            return;
        };
        map.insert("card_schema".into(), Value::String(CARD_SCHEMA_V3.into()));
        map.insert("distill".into(), self.distill.clone());
        map.insert(
            "has_decisions".into(),
            Value::String((self.counts.decisions > 0).to_string()),
        );
        map.insert(
            "has_failing_gates".into(),
            Value::String((self.counts.failing_gates > 0).to_string()),
        );
        map.insert(
            "has_open_questions".into(),
            Value::String((self.counts.open_questions > 0).to_string()),
        );
    }
}

fn outcome_token(outcome: AgentOutcome) -> &'static str {
    match outcome {
        AgentOutcome::Complete => "complete",
        AgentOutcome::Partial => "partial",
        AgentOutcome::Failed => "failed",
        AgentOutcome::Unknown => "unknown",
    }
}

fn gate_token(outcome: GateOutcome) -> &'static str {
    match outcome {
        GateOutcome::Pass => "pass",
        GateOutcome::Fail => "fail",
        GateOutcome::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicx_parser::engine::{
        AgentKind, RawUnitReader, ReaderPolicy, SourceArtifact, SourceFraming, SourceHandle,
        ValidatedParse, validate_parse,
    };
    use std::path::Path;

    /// Parse the frozen claude fixture through the public kernel surface —
    /// the same session the brief renderer and the TB oracle harness use.
    fn claude_fixture_model() -> SessionModel {
        use aicx_parser::adapters::registered_adapter;
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/tb_oracle/claude/6abdbe22_session.jsonl");
        let body = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("cannot read fixture {}: {error}", path.display()));
        let artifact = SourceArtifact::memory("session.jsonl", body, SourceFraming::JsonLines)
            .expect("memory artifact");
        let session_id = "6abdbe22-43e0-4544-a5ef-7314ece85078";
        let source = SourceHandle::new(
            AgentKind::Claude,
            session_id,
            Some(session_id.to_owned()),
            vec![artifact],
        )
        .expect("source handle");
        let read = RawUnitReader::new(ReaderPolicy::default())
            .read(&source)
            .expect("bounded read");
        let adapter = registered_adapter(AgentKind::Claude);
        let classified = adapter.classify(&source, &read).expect("classification");
        let parse = adapter
            .assemble(&source, &read, classified)
            .expect("assembly");
        match validate_parse(parse).expect("kernel validation") {
            ValidatedParse::Session(session) => session.into_model(),
            ValidatedParse::Fatal(fatal) => panic!("unexpected fatal parse: {fatal:?}"),
        }
    }

    /// Identity: the brief and the card materialize the SAME distillate —
    /// every decision/gate/open-question text in the card appears in the
    /// rendered brief, and the counts match section-for-section.
    #[test]
    fn brief_and_card_read_one_distillate() {
        let model = claude_fixture_model();
        let distillates = session_distillates(&model);
        let card = index_metadata(&distillates);
        let brief = crate::extraction::brief::render_brief(&model, &distillates);

        let expected_decisions: usize = distillates
            .iter()
            .map(|d| d.decision_candidates.len())
            .sum();
        assert_eq!(card.counts.decisions, expected_decisions);
        for distillate in &distillates {
            for decision in &distillate.decision_candidates {
                assert!(
                    brief.contains(decision.text.trim()),
                    "card decision missing from brief: {:?}",
                    decision.text
                );
            }
            for gate in &distillate.gates {
                assert!(
                    brief.contains(gate.command.trim()),
                    "card gate missing from brief: {:?}",
                    gate.command
                );
            }
            for question in &distillate.open_questions {
                assert!(
                    brief.contains(question.text.trim()),
                    "card open question missing from brief: {:?}",
                    question.text
                );
            }
        }
    }

    /// The merged metadata is filterable with plain equality: `card_schema`
    /// is stamped and the `has_*` scalars are string booleans.
    #[test]
    fn merged_metadata_carries_filter_scalars() {
        let model = claude_fixture_model();
        let card = index_metadata(&session_distillates(&model));
        let mut metadata = json!({"kind": "conversations", "agent": "claude"});
        card.merge_into(&mut metadata);
        assert_eq!(metadata["card_schema"], CARD_SCHEMA_V3);
        assert!(metadata["distill"]["segments"].as_u64().unwrap() >= 1);
        for key in ["has_decisions", "has_failing_gates", "has_open_questions"] {
            let value = metadata[key].as_str().expect("string boolean");
            assert!(value == "true" || value == "false", "{key}={value}");
        }
        // The claude fixture session carries at least one distilled decision;
        // if the lane ever regresses to empty, the filter surface dies with it.
        assert_eq!(metadata["has_decisions"], "true");
    }
}
