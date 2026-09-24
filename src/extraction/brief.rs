//! `extract --brief`: the inverted-pyramid handoff rendering of the
//! per-agent lane distillate.
//!
//! One brief section per [`Segment`] is the binding shape (Design Contract
//! decision 4): a `mixed_candidate` session gets a multi-workstream header
//! and per-segment sections instead of averaged outcomes. The renderer is a
//! **view over [`SegmentDistillate`]** — the same values the card/index
//! materialization consumes — so search and `--brief` can never drift apart
//! (one implementation, two consumers; see `docs/DISTILL_CONTRACT.md`).
//!
//! Ordering inside a section is the inverted pyramid: outcome first, then
//! decisions, gates, open questions, handoff signals. Every claim keeps its
//! evidence locator (`segment/turn`), because a claim without evidence is
//! narration, not handoff.

use aicx_parser::engine::{Known, ScopeStatus, Segment, SessionModel};

use super::distill::{GateOutcome, SegmentDistillate};

/// Render the full brief document for one session.
///
/// `distillates` must be segment-ordered (the shape
/// [`AgentLaneDistiller::distill`](super::distill::AgentLaneDistiller::distill)
/// returns); the renderer never recomputes heuristics, it only lays out what
/// the lane distilled.
pub fn render_brief(model: &SessionModel, distillates: &[SegmentDistillate]) -> String {
    let mut out = String::new();
    render_header(&mut out, model, distillates);
    for distillate in distillates {
        render_segment(&mut out, model, distillate);
    }
    if distillates.is_empty() {
        out.push_str("\n_No segments: the substrate carries no distillable span._\n");
    }
    out
}

fn known_or<'a>(value: &'a Known<String>, fallback: &'a str) -> &'a str {
    match value {
        Known::Value(value) => value.as_str(),
        Known::Unknown(_) => fallback,
    }
}

/// Where a segment worked, as a reader should see it: the repository its
/// workdirs resolved to when that is known, the recorded cwd otherwise, and an
/// explicit marker when the workdirs proved several checkouts — the recorded
/// cwd alone would present a mixed span as ordinary baseline work.
fn segment_place(segment: &Segment) -> Option<String> {
    let place = segment
        .scope_root
        .as_deref()
        .or(match &segment.cwd {
            Known::Value(cwd) => Some(cwd.as_str()),
            Known::Unknown(_) => None,
        })
        .map(str::to_owned);
    if segment.scope_conflict {
        return Some(match place {
            Some(place) => format!("{place} (scope conflict)"),
            None => "(scope conflict)".to_owned(),
        });
    }
    place
}

fn render_header(out: &mut String, model: &SessionModel, distillates: &[SegmentDistillate]) {
    let agent = model.provenance.agent.as_str();
    out.push_str(&format!("# BRIEF {} · {agent}", model.session_id));
    if let Known::Value(cwd) = &model.provenance.cwd {
        out.push_str(&format!(" · {cwd}"));
    }
    out.push('\n');

    let mixed = model
        .segments
        .iter()
        .any(|segment| segment.scope_status == ScopeStatus::MixedCandidate)
        || multi_workstream(model);
    if model.segments.len() > 1 || mixed {
        out.push_str(&format!(
            "\n**Multi-workstream session** — {} segment(s){}; outcomes are reported per segment, never averaged.\n",
            model.segments.len(),
            if mixed { ", mixed_candidate" } else { "" },
        ));
        out.push_str("\n| Segment | cwd | branch | outcome |\n|---|---|---|---|\n");
        for distillate in distillates {
            let segment = model
                .segments
                .iter()
                .find(|segment| segment.segment_id == distillate.segment_id);
            let (cwd, branch) = segment
                .map(|s| {
                    (
                        segment_place(s).unwrap_or_else(|| "?".to_owned()),
                        known_or(&s.branch, "?"),
                    )
                })
                .unwrap_or_else(|| ("?".to_owned(), "?"));
            out.push_str(&format!(
                "| {} | {} | {} | {:?} |\n",
                distillate.segment_id, cwd, branch, distillate.outcome.agent_outcome
            ));
        }
    }
}

/// More than one distinct known cwd across segments = several workstreams,
/// even when each individual segment is internally drift-free.
fn multi_workstream(model: &SessionModel) -> bool {
    let mut cwds = std::collections::BTreeSet::new();
    for segment in &model.segments {
        if let Known::Value(cwd) = &segment.cwd {
            cwds.insert(cwd.as_str());
        }
    }
    cwds.len() > 1
}

fn render_segment(out: &mut String, model: &SessionModel, distillate: &SegmentDistillate) {
    let segment = model
        .segments
        .iter()
        .find(|segment| segment.segment_id == distillate.segment_id);
    out.push_str(&format!("\n## Segment {}", distillate.segment_id));
    if let Some(segment) = segment {
        if let Some(place) = segment_place(segment) {
            out.push_str(&format!(" · {place}"));
        }
        if let Known::Value(branch) = &segment.branch {
            out.push_str(&format!(" @ {branch}"));
        }
    }
    out.push('\n');

    // Inverted pyramid: the outcome is the first line a reader sees.
    out.push_str(&format!(
        "\n**Outcome:** {:?}{}\n",
        distillate.outcome.agent_outcome,
        distillate
            .outcome
            .ending
            .as_deref()
            .map(|ending| format!(" · ending: {ending}"))
            .unwrap_or_default(),
    ));

    if !distillate.decision_candidates.is_empty() {
        out.push_str("\n### Decisions (candidates)\n");
        for decision in &distillate.decision_candidates {
            let kind = if decision.kind.is_empty() {
                String::new()
            } else {
                format!(" `{}`", decision.kind)
            };
            out.push_str(&format!(
                "- {}{kind} {}\n",
                decision.text.trim(),
                locator(&decision.evidence)
            ));
        }
    }

    if !distillate.gates.is_empty() {
        out.push_str("\n### Gates\n");
        for gate in &distillate.gates {
            let verdict = match gate.outcome {
                GateOutcome::Pass => "PASS",
                GateOutcome::Fail => "FAIL",
                GateOutcome::Unknown => "UNKNOWN",
            };
            out.push_str(&format!(
                "- `{}` — {verdict} {}\n",
                gate.command.trim(),
                locator(&gate.evidence)
            ));
        }
    }

    if !distillate.open_questions.is_empty() {
        out.push_str("\n### Open questions\n");
        for question in &distillate.open_questions {
            let kind = if question.kind.is_empty() {
                String::new()
            } else {
                format!(" `{}`", question.kind)
            };
            out.push_str(&format!(
                "- {}{kind} {}\n",
                question.text.trim(),
                locator(&question.evidence)
            ));
        }
    }

    if !distillate.handoff_signals.is_empty() {
        out.push_str("\n### Handoff signals\n");
        for signal in &distillate.handoff_signals {
            out.push_str(&format!(
                "- {} {}\n",
                signal.text.trim(),
                locator(&signal.evidence)
            ));
        }
    }

    if distillate.decision_candidates.is_empty()
        && distillate.gates.is_empty()
        && distillate.open_questions.is_empty()
        && distillate.handoff_signals.is_empty()
    {
        out.push_str("\n_No distillable signals in this segment (honest empty, not a claim)._\n");
    }
}

fn locator(evidence: &super::distill::EvidenceLocator) -> String {
    match evidence.turn_idx {
        Some(turn) => format!("`[s{} t{turn}]`", evidence.segment_id),
        None => format!("`[s{}]`", evidence.segment_id),
    }
}

#[cfg(test)]
mod tests {
    use super::super::distill::{
        AgentOutcome, DecisionCandidate, EvidenceLocator, GateObservation, GateOutcome,
        LaneOutcome, SEGMENT_DISTILLATE_SCHEMA, SegmentDistillate,
    };
    use super::*;
    use aicx_parser::engine::{
        AgentKind, BoundaryFlags, CoverageReport, Known, ParseStatus, Provenance, ScopeStatus,
        Segment, SessionModel, TurnRange, VisibleCompleteness,
    };

    fn minimal_model(segments: Vec<Segment>) -> SessionModel {
        let provenance = Provenance {
            agent: AgentKind::Claude,
            model: Known::unknown(),
            cli_version: Known::unknown(),
            cwd: Known::Value("/repo".to_owned()),
            branch: Known::Value("main".to_owned()),
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
        let mut model = SessionModel::new("test-session", provenance, coverage);
        model.segments = segments;
        model
    }

    fn segment(id: u32, cwd: &str) -> Segment {
        Segment {
            segment_id: id,
            cwd: Known::Value(cwd.to_owned()),
            branch: Known::Value("main".to_owned()),
            started_at: Known::unknown(),
            ended_at: Known::unknown(),
            turn_range: TurnRange { start: 0, end: 0 },
            scope_status: ScopeStatus::NoDriftObserved,
            scope_conflict: false,
            scope_root: None,
            scope_workdirs: Vec::new(),
        }
    }

    fn distillate_for(id: u32) -> SegmentDistillate {
        SegmentDistillate {
            schema: SEGMENT_DISTILLATE_SCHEMA.to_owned(),
            agent: AgentKind::Claude,
            segment_id: id,
            scope_status: ScopeStatus::NoDriftObserved,
            decision_candidates: vec![DecisionCandidate {
                text: format!("decision in segment {id}"),
                kind: "explicit_choice".to_owned(),
                evidence: EvidenceLocator {
                    segment_id: id,
                    turn_idx: Some(3),
                    timestamp: None,
                },
            }],
            gates: vec![GateObservation {
                command: "cargo test".to_owned(),
                outcome: GateOutcome::Pass,
                evidence: EvidenceLocator {
                    segment_id: id,
                    turn_idx: Some(4),
                    timestamp: None,
                },
            }],
            open_questions: Vec::new(),
            handoff_signals: Vec::new(),
            outcome: LaneOutcome {
                agent_outcome: AgentOutcome::Complete,
                ending: None,
            },
        }
    }

    /// Single-segment brief: outcome leads, evidence locators attached, no
    /// multi-workstream header.
    #[test]
    fn single_segment_brief_is_inverted_pyramid() {
        let model = minimal_model(vec![segment(0, "/repo")]);
        let brief = render_brief(&model, &[distillate_for(0)]);
        assert!(brief.starts_with("# BRIEF test-session · claude"));
        assert!(!brief.contains("Multi-workstream"));
        let outcome_at = brief.find("**Outcome:** Complete").expect("outcome line");
        let decision_at = brief.find("decision in segment 0").expect("decision line");
        assert!(outcome_at < decision_at, "outcome must lead the section");
        assert!(brief.contains("`cargo test` — PASS `[s0 t4]`"));
    }

    /// Two segments with different cwds: multi-workstream header + one
    /// section per segment, no averaging.
    #[test]
    fn mixed_session_renders_per_segment_sections() {
        let model = minimal_model(vec![segment(0, "/repo-a"), segment(1, "/repo-b")]);
        let brief = render_brief(&model, &[distillate_for(0), distillate_for(1)]);
        assert!(brief.contains("Multi-workstream session"));
        assert!(brief.contains("## Segment 0 · /repo-a"));
        assert!(brief.contains("## Segment 1 · /repo-b"));
    }

    /// A conflict segment keeps its recorded cwd in the model, but the brief
    /// must not present it as ordinary baseline work; a re-scoped segment
    /// shows the repository its workdirs resolved to.
    #[test]
    fn scope_evidence_shapes_the_segment_heading() {
        let mut conflicted = segment(0, "/sessions/vista");
        conflicted.scope_conflict = true;
        conflicted.scope_status = ScopeStatus::MixedCandidate;
        let mut rescoped = segment(1, "/sessions/vista");
        rescoped.scope_root = Some("/repo/fleet-bus".to_owned());
        let model = minimal_model(vec![conflicted, rescoped]);
        let brief = render_brief(&model, &[distillate_for(0), distillate_for(1)]);
        assert!(
            brief.contains("## Segment 0 · /sessions/vista (scope conflict)"),
            "{brief}"
        );
        assert!(brief.contains("## Segment 1 · /repo/fleet-bus"), "{brief}");
        assert!(
            brief.contains("| 0 | /sessions/vista (scope conflict) | main |"),
            "{brief}"
        );
    }
}
