//! Lane 2-5 schema anchor for the vc-intents truth pipeline.
//!
//! These are the stable, serializable record shapes that the downstream lanes
//! converge on (MASTER Phase 2 §3 "Define stable JSON schemas"). Lane 1 (human
//! intent) already exists as [`super::IntentRecord`]; this module adds the four
//! remaining lanes as the single anchor every lane stage must agree on:
//!
//! - Lane 2 — Agent Claim ([`ClaimRecord`]): what an agent asserted was done.
//! - Lane 3 — Evidence / Result ([`ResultRecord`]): what evidence proves.
//! - Lane 4 — Contract Fracture ([`ContractFracture`]): doc/contract vs runtime.
//! - Lane 5 — Clarify ([`ClarifyQuestion`]): a bounded human decision prompt.
//!
//! Doctrine carried in the types themselves: a claim is never a result
//! (`verification_status` defaults to `Unverified`); a result needs evidence
//! pointers; clarify asks decisions, not discoverable facts.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use serde::Serialize;

// ── Lane 2 — Agent Claim ─────────────────────────────────────────

/// A claim is an agent-originated assertion that something was done. Claims are
/// audit targets, never truth — every claim needs evidence or stays unverified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimRecord {
    pub id: String,
    pub project: String,
    pub source_session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_agent: Option<String>,
    /// Always `assistant` / `agent` — a claim cannot originate from user text.
    pub source_role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_span: Option<String>,
    pub claim_text: String,
    pub claim_type: ClaimType,
    pub claimed_status: String,
    pub claimed_files: Vec<String>,
    pub claimed_commands: Vec<String>,
    pub claimed_artifacts: Vec<String>,
    pub related_intents: Vec<String>,
    pub evidence_refs: Vec<String>,
    pub verification_status: VerificationStatus,
    pub risk_flags: Vec<String>,
}

/// Claim taxonomy (MASTER §305). Note `ReadyToPush`/`Shippable`/`NoBlockers` are
/// the highest-risk claims — the "production ready" applause verdict — and must
/// never be promoted to a Result without Lane 3 evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    Implemented,
    Fixed,
    Tested,
    Verified,
    Migrated,
    Installed,
    Documented,
    Green,
    ReadyToPush,
    Shippable,
    NoBlockers,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Unverified,
    Verified,
    Contradicted,
    Partial,
    Stale,
}

impl Default for VerificationStatus {
    /// A fresh claim is unverified until Lane 3 proves otherwise.
    fn default() -> Self {
        Self::Unverified
    }
}

// ── Lane 3 — Evidence / Result ───────────────────────────────────

/// A result exists only when backed by evidence (passing command, test result,
/// committed file, observable behavior...). No evidence means no result — it
/// stays a [`ClaimRecord`]. Named `ResultRecord` to avoid shadowing `std::Result`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResultRecord {
    pub id: String,
    pub project: String,
    pub evidence_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_output_excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub related_claims: Vec<String>,
    pub related_intents: Vec<String>,
    pub result_status: ResultStatus,
    pub confidence: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproducibility_notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Pass,
    Fail,
    Partial,
    Unknown,
}

// ── Lane 4 — Contract Fracture ───────────────────────────────────

/// A mismatch between a promised surface (docs/contract) and the runtime
/// surface: docs promise `spotlight.md` but runtime never emits it; a canonical
/// contract exists only untracked; tests are green but the promised behavior is
/// absent. A fracture carries the decision it forces, not just the gap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContractFracture {
    pub contract_source: String,
    pub promised_surface: String,
    pub runtime_surface: String,
    pub evidence: Vec<String>,
    pub severity: FractureSeverity,
    /// Candidate resolutions, typically A/B/C (e.g. implement runtime to match
    /// contract, or reduce contract to current runtime).
    pub options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended_clarify_question: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FractureSeverity {
    Low,
    Medium,
    High,
    Critical,
}

// ── Lane 5 — Clarify ─────────────────────────────────────────────

/// A bounded human decision prompt, generated only AFTER intents, claims, and
/// results are known. Clarify asks decisions (keep contract or reduce it? ship
/// with known gap or keep hardening?), never discoverable facts (where is the
/// file? did the test pass?). Always carries a default recommendation so a
/// non-dev operator can answer "I trust you" instead of guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClarifyQuestion {
    pub decision_id: String,
    pub why_now: String,
    pub known_facts: Vec<String>,
    /// Decision options, preferably A/B/C.
    pub options: Vec<String>,
    pub default_recommendation: String,
    pub cost_of_not_deciding: String,
    pub linked_intents: Vec<String>,
    pub linked_claims: Vec<String>,
    pub linked_results: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_schemas_serialize_to_json() {
        // Anchor smoke: each lane record must serialize (lane outputs are JSON).
        let claim = ClaimRecord {
            id: "claim-1".into(),
            project: "aicx".into(),
            source_session: "sess-a".into(),
            source_agent: Some("codex".into()),
            source_role: "assistant".into(),
            source_span: None,
            claim_text: "fixed F2 dead --unresolved".into(),
            claim_type: ClaimType::Fixed,
            claimed_status: "done".into(),
            claimed_files: vec!["src/main.rs".into()],
            claimed_commands: vec!["cargo test -p aicx --lib".into()],
            claimed_artifacts: vec![],
            related_intents: vec!["intent-1".into()],
            evidence_refs: vec![],
            verification_status: VerificationStatus::default(),
            risk_flags: vec![],
        };
        assert_eq!(claim.verification_status, VerificationStatus::Unverified);

        let result = ResultRecord {
            id: "result-1".into(),
            project: "aicx".into(),
            evidence_type: "command".into(),
            command: Some("cargo test -p aicx --lib".into()),
            exit_status: Some(0),
            artifact_path: None,
            observed_output_excerpt: Some("679 passed; 0 failed".into()),
            timestamp: None,
            related_claims: vec!["claim-1".into()],
            related_intents: vec![],
            result_status: ResultStatus::Pass,
            confidence: 9,
            reproducibility_notes: None,
        };

        let fracture = ContractFracture {
            contract_source: "docs/TB_ARTIFACT_CONTRACT.md".into(),
            promised_surface: "spotlight.md renderer".into(),
            runtime_surface: "absent (0 renderers in tb_core/)".into(),
            evidence: vec!["rg spotlight tb_core/ -> 0".into()],
            severity: FractureSeverity::High,
            options: vec!["build renderer".into(), "downgrade contract".into()],
            recommended_clarify_question: Some("clarify-1".into()),
        };

        let clarify = ClarifyQuestion {
            decision_id: "clarify-1".into(),
            why_now: "contract promises an artifact runtime never emits".into(),
            known_facts: vec!["spotlight.md has 0 renderers".into()],
            options: vec!["A build it".into(), "B drop promise".into()],
            default_recommendation: "A".into(),
            cost_of_not_deciding: "every fresh clone trusts a dead promise".into(),
            linked_intents: vec![],
            linked_claims: vec![],
            linked_results: vec!["result-1".into()],
        };

        for json in [
            serde_json::to_string(&claim).unwrap(),
            serde_json::to_string(&result).unwrap(),
            serde_json::to_string(&fracture).unwrap(),
            serde_json::to_string(&clarify).unwrap(),
        ] {
            assert!(!json.is_empty());
        }
    }
}
