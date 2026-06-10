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
use std::path::Path;

// ── Export envelope (P0 temporal contract) ───────────────────────

/// Earliest/latest source-message timestamps covered by an export, RFC3339.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimeCoverage {
    pub earliest: String,
    pub latest: String,
}

/// Machine-readable export envelope every lane export is wrapped in (MASTER
/// "Required Output Contracts"). Carries the full temporal contract so a
/// downstream agent can always answer "when is this from?" — absolute
/// `generated_at` (with year), source time coverage, explicit timezone
/// assumptions, and warnings whenever any timestamp is partial or inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaneExport<T: Serialize> {
    /// Schema identifier+version for this envelope, e.g. `aicx.lanes.v1`.
    pub schema_version: String,
    /// Absolute extraction timestamp, RFC3339 with year.
    pub generated_at: String,
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_time_coverage: Option<TimeCoverage>,
    pub source_files: Vec<String>,
    /// Which lane produced this export: `claims` | `results` | `clarify`.
    pub extraction_mode: String,
    /// Role filter applied at the source: `agent_only` | `user_only` | `all`.
    pub role_filter: String,
    /// e.g. "all timestamps normalized to UTC (RFC3339) at source".
    pub timezone_assumptions: String,
    /// Non-empty whenever temporal truth is degraded (partial/inferred times,
    /// missing years, inferred session order). Never silently full.
    pub warnings: Vec<String>,
    pub payload: T,
}

pub const LANE_SCHEMA_VERSION: &str = "aicx.lanes.v1";
pub const UTC_TIMEZONE_ASSUMPTION: &str =
    "all timestamps normalized to UTC (RFC3339, full date+year) at source";

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
    /// Absolute source-message timestamp (RFC3339, full date+year), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// True when the source timestamp was inferred (fallback) or absent —
    /// partial temporal truth is marked, never silently presented as full.
    pub timestamp_partial: bool,
    /// Absolute extraction timestamp (RFC3339).
    pub extracted_at: String,
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

impl ClaimType {
    /// snake_case label, matching the serde representation.
    pub fn label(self) -> &'static str {
        match self {
            Self::Implemented => "implemented",
            Self::Fixed => "fixed",
            Self::Tested => "tested",
            Self::Verified => "verified",
            Self::Migrated => "migrated",
            Self::Installed => "installed",
            Self::Documented => "documented",
            Self::Green => "green",
            Self::ReadyToPush => "ready_to_push",
            Self::Shippable => "shippable",
            Self::NoBlockers => "no_blockers",
            Self::Blocked => "blocked",
        }
    }

    /// The "production ready" applause claims that must never be promoted to a
    /// Result without Lane 3 evidence.
    pub fn is_high_risk(self) -> bool {
        matches!(self, Self::ReadyToPush | Self::Shippable | Self::NoBlockers)
    }
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
    /// The decision question itself — asks what to decide, never what to look up.
    pub question: String,
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

// ── Lane 2 classification primitive ──────────────────────────────

/// Classify a claim sentence into its [`ClaimType`] by surface claim-language.
///
/// Case-insensitive substring match in priority order (most specific first):
/// "no blockers" must not be misread as `Blocked`, and "ready to ship" must not
/// be swallowed by a generic ship/done marker. Returns `None` when no claim
/// marker is present — absence is NOT `Implemented` by default; a claim must
/// actually claim something.
///
/// `ReadyToPush` / `Shippable` / `NoBlockers` are the highest-risk claims (the
/// "production ready" applause verdict); classification only labels them — Lane 3
/// must still demand evidence before any of them becomes a Result.
pub fn classify_claim(text: &str) -> Option<ClaimType> {
    let t = text.to_lowercase();
    // (needle, type) — list order IS precedence; first contained needle wins.
    const RULES: &[(&str, ClaimType)] = &[
        ("no blocker", ClaimType::NoBlockers),
        ("zero blocker", ClaimType::NoBlockers),
        ("blocked", ClaimType::Blocked),
        ("blocker", ClaimType::Blocked),
        ("production ready", ClaimType::ReadyToPush),
        ("ready to push", ClaimType::ReadyToPush),
        ("ready to ship", ClaimType::ReadyToPush),
        ("ready to merge", ClaimType::ReadyToPush),
        ("shippable", ClaimType::Shippable),
        ("ship it", ClaimType::Shippable),
        ("migration complete", ClaimType::Migrated),
        ("migrated", ClaimType::Migrated),
        ("installed", ClaimType::Installed),
        ("docs updated", ClaimType::Documented),
        ("documented", ClaimType::Documented),
        ("verified", ClaimType::Verified),
        ("all green", ClaimType::Green),
        ("tests green", ClaimType::Green),
        ("green", ClaimType::Green),
        ("tests pass", ClaimType::Tested),
        ("passing", ClaimType::Tested),
        ("tested", ClaimType::Tested),
        ("fixed", ClaimType::Fixed),
        ("implemented", ClaimType::Implemented),
        ("shipped", ClaimType::Implemented),
        ("complete", ClaimType::Implemented),
        ("done", ClaimType::Implemented),
    ];
    RULES
        .iter()
        .find(|(needle, _)| t.contains(needle))
        .map(|(_, kind)| *kind)
}

/// A minimal source row for Lane 2 claim extraction: one message with its role
/// and provenance. Decoupled from any store/session reader so the pure transform
/// is testable without a corpus.
#[derive(Debug, Clone)]
pub struct ClaimSource {
    pub role: String,
    pub text: String,
    pub project: String,
    pub session_id: String,
    pub agent: Option<String>,
    pub source_ref: String,
    /// Absolute source-message timestamp (RFC3339), when known.
    pub timestamp: Option<String>,
    /// True when the timestamp came from a fallback (previous line, file
    /// mtime, extraction time) rather than the message itself.
    pub timestamp_partial: bool,
}

fn is_agent_role(role: &str) -> bool {
    matches!(
        role.to_lowercase().as_str(),
        "assistant" | "agent" | "model" | "gemini"
    )
}

/// Lane 2 stage (pure): turn agent/assistant source rows into Unverified
/// [`ClaimRecord`]s. Doctrine enforced here:
/// - claims are agent-originated — user rows never produce a claim;
/// - a row whose text carries no claim marker is skipped — no manufactured
///   claims (absence of a marker is not a claim);
/// - every claim is `Unverified` until Lane 3 supplies evidence;
/// - the applause claims (ready/shippable/no-blockers) are flagged high-risk.
pub fn extract_claims(sources: &[ClaimSource], extracted_at: &str) -> Vec<ClaimRecord> {
    sources
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            if !is_agent_role(&s.role) {
                return None;
            }
            // Classify the leading non-empty line, not the whole message: a long
            // reply incidentally contains markers ("fixed"/"ready"/"green") deep
            // in prose that are not the claim. Status claims lead their message.
            let claim_line = s
                .text
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("");
            let claim_type = classify_claim(claim_line)?;
            let risk_flags = if claim_type.is_high_risk() {
                vec!["high_risk_unverified_claim".to_string()]
            } else {
                Vec::new()
            };
            Some(ClaimRecord {
                id: format!("claim-{}-{i}", s.session_id),
                project: s.project.clone(),
                source_session: s.session_id.clone(),
                source_agent: s.agent.clone(),
                source_role: s.role.clone(),
                source_span: Some(s.source_ref.clone()),
                claim_text: claim_line.to_string(),
                claim_type,
                claimed_status: claim_type.label().to_string(),
                timestamp: s.timestamp.clone(),
                timestamp_partial: s.timestamp_partial || s.timestamp.is_none(),
                extracted_at: extracted_at.to_string(),
                claimed_files: Vec::new(),
                claimed_commands: Vec::new(),
                claimed_artifacts: Vec::new(),
                related_intents: Vec::new(),
                evidence_refs: Vec::new(),
                verification_status: VerificationStatus::Unverified,
                risk_flags,
            })
        })
        .collect()
}

// ── Lane 3 stages — evidence collection + claim verification ─────

/// Pull path-looking tokens out of a claim sentence: backtick-quoted spans and
/// bare tokens containing `/` that look like repo-relative file paths. These are
/// the artifacts the claim implicitly stakes its truth on.
/// A token is only checkable evidence when it can be resolved against the repo
/// root (or absolutely). `~`-prefixed paths and glob patterns name surfaces we
/// cannot honestly test here — skipping them avoids manufacturing false
/// contradictions (absence of checkable evidence is NOT a Fail).
fn is_checkable_path(token: &str) -> bool {
    token.contains('/') && !token.contains("://") && !token.starts_with('~') && !token.contains('*')
}

fn path_tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // backtick-quoted spans first — the explicit form
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else { break };
        let token = after[..end].trim();
        if !token.contains(' ') && is_checkable_path(token) {
            out.push(token.to_string());
        }
        rest = &after[end + 1..];
    }
    // bare path-like tokens (contain '/', no URL scheme, end in a file-ish name)
    for word in text.split_whitespace() {
        let w = word.trim_matches(|c: char| ",.;:()[]\"'".contains(c));
        if is_checkable_path(w)
            && !w.starts_with('`')
            && w.rsplit('/').next().is_some_and(|f| f.contains('.'))
        {
            out.push(w.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Lane 3 stage (deterministic, read-only): for every file path a claim names,
/// check whether the artifact actually exists under `repo_root` (or absolutely).
/// Existence yields a `Pass` result, absence a `Fail` — both are evidence; a
/// claim that names no artifact gets no result here and stays unverified.
/// Nothing is executed — this collects repo-state evidence only.
pub fn collect_artifact_evidence(
    claims: &[ClaimRecord],
    repo_root: &Path,
    collected_at: &str,
) -> Vec<ResultRecord> {
    let mut results = Vec::new();
    for claim in claims {
        for token in path_tokens(&claim.claim_text) {
            let candidate = Path::new(&token);
            let exists = if candidate.is_absolute() {
                candidate.exists()
            } else {
                repo_root.join(candidate).exists()
            };
            let status = if exists {
                ResultStatus::Pass
            } else {
                ResultStatus::Fail
            };
            results.push(ResultRecord {
                id: format!("result-{}-{}", claim.id, results.len()),
                project: claim.project.clone(),
                evidence_type: "artifact_existence".to_string(),
                command: None,
                exit_status: None,
                artifact_path: Some(token.clone()),
                observed_output_excerpt: Some(if exists {
                    format!("{token}: exists in {}", repo_root.display())
                } else {
                    format!("{token}: NOT FOUND in {}", repo_root.display())
                }),
                timestamp: Some(collected_at.to_string()),
                related_claims: vec![claim.id.clone()],
                related_intents: Vec::new(),
                result_status: status,
                confidence: 8,
                reproducibility_notes: Some("ls-level filesystem check, re-runnable".to_string()),
            });
        }
    }
    results
}

/// Lane 3 verification (pure): fold evidence into claims. Pass evidence
/// verifies, Fail evidence contradicts, mixed evidence is Partial; a claim
/// nothing points at stays exactly as it was (Unverified by default — no
/// evidence means no result, and no result means no promotion).
pub fn verify_claims(claims: &mut [ClaimRecord], results: &[ResultRecord]) {
    for claim in claims.iter_mut() {
        let mut pass = 0usize;
        let mut fail = 0usize;
        for r in results
            .iter()
            .filter(|r| r.related_claims.contains(&claim.id))
        {
            claim.evidence_refs.push(r.id.clone());
            match r.result_status {
                ResultStatus::Pass => pass += 1,
                ResultStatus::Fail => fail += 1,
                ResultStatus::Partial | ResultStatus::Unknown => {}
            }
        }
        claim.verification_status = match (pass, fail) {
            (0, 0) => claim.verification_status, // untouched — stays Unverified
            (_, 0) => VerificationStatus::Verified,
            (0, _) => VerificationStatus::Contradicted,
            (_, _) => VerificationStatus::Partial,
        };
    }
}

// ── Lane 4 stage — contract fracture detection ───────────────────

/// Lane 4 stage (pure): surface promise-vs-runtime fractures from verified
/// claims. A contradicted claim IS a fracture (the agent said X, the repo says
/// not-X); an applause claim (ready/shippable/no-blockers) with no evidence is
/// a fracture-in-waiting and gets surfaced at Medium so it cannot pass as truth.
pub fn detect_fractures(claims: &[ClaimRecord]) -> Vec<ContractFracture> {
    let mut fractures = Vec::new();
    for claim in claims {
        match claim.verification_status {
            VerificationStatus::Contradicted => {
                let severity = if claim.claim_type.is_high_risk() {
                    FractureSeverity::Critical
                } else {
                    FractureSeverity::High
                };
                fractures.push(ContractFracture {
                    contract_source: format!(
                        "agent claim {} (session {})",
                        claim.id, claim.source_session
                    ),
                    promised_surface: claim.claim_text.clone(),
                    runtime_surface: "evidence contradicts the claim (named artifact missing)"
                        .to_string(),
                    evidence: claim.evidence_refs.clone(),
                    severity,
                    options: vec![
                        "A: implement/repair so the claim becomes true".to_string(),
                        "B: retract the claim and reopen the task".to_string(),
                        "C: accept the gap and record it as known debt".to_string(),
                    ],
                    recommended_clarify_question: Some(format!("clarify-{}", claim.id)),
                });
            }
            VerificationStatus::Unverified if claim.claim_type.is_high_risk() => {
                fractures.push(ContractFracture {
                    contract_source: format!(
                        "agent claim {} (session {})",
                        claim.id, claim.source_session
                    ),
                    promised_surface: claim.claim_text.clone(),
                    runtime_surface: "no evidence collected — applause verdict unbacked"
                        .to_string(),
                    evidence: Vec::new(),
                    severity: FractureSeverity::Medium,
                    options: vec![
                        "A: demand evidence (run gates) before trusting the verdict".to_string(),
                        "B: treat as unverified and keep hardening".to_string(),
                        "C: accept the verdict on trust and ship".to_string(),
                    ],
                    recommended_clarify_question: Some(format!("clarify-{}", claim.id)),
                });
            }
            _ => {}
        }
    }
    fractures
}

// ── Lane 5 stage — clarify generation ────────────────────────────

/// Hard ceiling on clarify questions per run — clarify is a decision-gathering
/// mechanism, not a questionnaire.
pub const CLARIFY_MAX_QUESTIONS: usize = 5;

/// Lane 5 stage (pure): turn the sharpest fractures into bounded A/B/C
/// decision questions. Questions ask what the human must DECIDE (keep the
/// promise, retract it, or ship with the gap) — never facts the system already
/// determined (the known facts ride along in `known_facts`). At most
/// `min(max, CLARIFY_MAX_QUESTIONS)` questions, severest fractures first.
pub fn generate_clarify(fractures: &[ContractFracture], max: usize) -> Vec<ClarifyQuestion> {
    let cap = max.min(CLARIFY_MAX_QUESTIONS);
    let mut ordered: Vec<&ContractFracture> = fractures.iter().collect();
    // severest first; FractureSeverity derives Low..Critical in declaration
    // order, so sort by reverse discriminant via a manual rank.
    let rank = |s: FractureSeverity| match s {
        FractureSeverity::Critical => 0,
        FractureSeverity::High => 1,
        FractureSeverity::Medium => 2,
        FractureSeverity::Low => 3,
    };
    ordered.sort_by_key(|f| rank(f.severity));
    ordered
        .into_iter()
        .take(cap)
        .enumerate()
        .map(|(i, f)| ClarifyQuestion {
            decision_id: f
                .recommended_clarify_question
                .clone()
                .unwrap_or_else(|| format!("clarify-{i}")),
            question: format!(
                "The promise \"{}\" does not match runtime ({}). Repair it, retract it, or ship with the gap?",
                f.promised_surface, f.runtime_surface
            ),
            why_now: format!(
                "{:?}-severity fracture between a recorded promise and the live repo; \
                 leaving it undecided lets a false claim harden into assumed truth",
                f.severity
            ),
            known_facts: {
                let mut facts = vec![
                    format!("promised: {}", f.promised_surface),
                    format!("observed: {}", f.runtime_surface),
                ];
                facts.extend(f.evidence.iter().map(|e| format!("evidence: {e}")));
                facts
            },
            options: f.options.clone(),
            default_recommendation: f
                .options
                .first()
                .cloned()
                .unwrap_or_else(|| "A: repair to match the promise".to_string()),
            cost_of_not_deciding: "the gap survives as invisible debt and every future agent \
                                   plans on top of a promise the runtime does not keep"
                .to_string(),
            linked_intents: Vec::new(),
            linked_claims: vec![f.contract_source.clone()],
            linked_results: f.evidence.clone(),
        })
        .collect()
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
            timestamp: Some("2026-06-08T18:06:26Z".into()),
            timestamp_partial: false,
            extracted_at: "2026-06-09T20:41:00Z".into(),
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
            question: "Build the promised renderer or drop the promise?".into(),
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

    #[test]
    fn classify_claim_maps_taxonomy_and_respects_precedence() {
        use ClaimType::*;
        // straight taxonomy hits
        assert_eq!(classify_claim("this is done"), Some(Implemented));
        assert_eq!(classify_claim("shipped the adapter"), Some(Implemented));
        assert_eq!(classify_claim("fixed the dead filter"), Some(Fixed));
        assert_eq!(classify_claim("all tests pass now"), Some(Tested));
        assert_eq!(classify_claim("verified against the repo"), Some(Verified));
        assert_eq!(classify_claim("migration complete"), Some(Migrated));
        assert_eq!(classify_claim("installed via pipx"), Some(Installed));
        assert_eq!(classify_claim("docs updated"), Some(Documented));
        assert_eq!(classify_claim("the suite is green"), Some(Green));
        assert_eq!(
            classify_claim("this is production ready"),
            Some(ReadyToPush)
        );
        assert_eq!(classify_claim("shippable as-is"), Some(Shippable));

        // precedence edges
        assert_eq!(
            classify_claim("no blockers remain"),
            Some(NoBlockers),
            "'no blockers' must not be misread as Blocked",
        );
        assert_eq!(classify_claim("blocked on review"), Some(Blocked));
        assert_eq!(
            classify_claim("it is ready to ship"),
            Some(ReadyToPush),
            "'ready to ship' must not be swallowed by Shippable/ship-it",
        );

        // no claim marker -> None (absence is not Implemented)
        assert_eq!(classify_claim("just exploring some options"), None);
        assert_eq!(classify_claim(""), None);
    }

    fn mk_source(role: &str, text: &str, refr: &str) -> ClaimSource {
        ClaimSource {
            role: role.to_string(),
            text: text.to_string(),
            project: "aicx".to_string(),
            session_id: "s1".to_string(),
            agent: Some("codex".to_string()),
            source_ref: refr.to_string(),
            timestamp: Some("2026-06-09T20:41:00Z".to_string()),
            timestamp_partial: false,
        }
    }

    #[test]
    fn extract_claims_keeps_agent_claims_drops_user_and_unmarked() {
        let sources = vec![
            // user row has a marker ("fixed") but claims are agent-originated -> dropped
            mk_source("user", "fixed the bug please", "u1"),
            mk_source("assistant", "fixed the dead filter", "a1"),
            // no claim marker -> dropped (absence is not a claim)
            mk_source("assistant", "just thinking out loud", "a2"),
            mk_source("assistant", "this is production ready", "a3"),
        ];

        let claims = extract_claims(&sources, "2026-06-09T20:45:00Z");
        assert_eq!(claims.len(), 2, "only marked agent rows survive");

        let fixed = &claims[0];
        assert_eq!(fixed.claim_type, ClaimType::Fixed);
        assert_eq!(fixed.source_role, "assistant");
        assert_eq!(fixed.claimed_status, "fixed");
        assert_eq!(fixed.verification_status, VerificationStatus::Unverified);
        assert!(fixed.risk_flags.is_empty());

        let ready = &claims[1];
        assert_eq!(ready.claim_type, ClaimType::ReadyToPush);
        // the applause verdict is flagged so Lane 3 must demand evidence
        assert_eq!(
            ready.risk_flags,
            vec!["high_risk_unverified_claim".to_string()]
        );
    }

    #[test]
    fn claims_carry_absolute_time_and_mark_partial_explicitly() {
        // P0 temporal: a claim must carry the absolute source timestamp (with
        // year) AND the extraction timestamp; a missing source time is marked
        // partial — never silently presented as full temporal truth.
        let mut with_time = mk_source("assistant", "fixed the parser", "a1");
        with_time.timestamp = Some("2026-06-09T20:41:00Z".to_string());
        let mut no_time = mk_source("assistant", "tests pass on the suite", "a2");
        no_time.timestamp = None;

        let claims = extract_claims(&[with_time, no_time], "2026-06-09T21:00:00Z");
        assert_eq!(claims.len(), 2);

        assert_eq!(claims[0].timestamp.as_deref(), Some("2026-06-09T20:41:00Z"));
        assert!(claims[0].timestamp.as_deref().unwrap().starts_with("2026-"));
        assert!(!claims[0].timestamp_partial);
        assert_eq!(claims[0].extracted_at, "2026-06-09T21:00:00Z");

        assert!(claims[1].timestamp.is_none());
        assert!(
            claims[1].timestamp_partial,
            "missing source time must be marked partial"
        );
    }

    #[test]
    fn lane_export_envelope_carries_temporal_contract() {
        let export = LaneExport {
            schema_version: LANE_SCHEMA_VERSION.to_string(),
            generated_at: "2026-06-09T21:00:00Z".to_string(),
            project: "aicx".to_string(),
            repo: Some("/repo".to_string()),
            session_id: Some("s1".to_string()),
            source_time_coverage: Some(TimeCoverage {
                earliest: "2026-06-09T20:00:00Z".to_string(),
                latest: "2026-06-09T20:59:00Z".to_string(),
            }),
            source_files: vec!["~/.claude/projects/x/s1.jsonl".to_string()],
            extraction_mode: "claims".to_string(),
            role_filter: "agent_only".to_string(),
            timezone_assumptions: UTC_TIMEZONE_ASSUMPTION.to_string(),
            warnings: vec!["1 claim has a partial timestamp".to_string()],
            payload: Vec::<ClaimRecord>::new(),
        };
        let json = serde_json::to_string(&export).unwrap();
        for key in [
            "schema_version",
            "generated_at",
            "source_time_coverage",
            "timezone_assumptions",
            "warnings",
            "2026-06-09T21:00:00Z",
        ] {
            assert!(json.contains(key), "envelope must expose {key}");
        }
    }

    #[test]
    fn evidence_verifies_and_contradiction_marks_contradicted() {
        let sources = vec![
            mk_source("assistant", "fixed `src/intents/schema.rs` for good", "a1"),
            mk_source(
                "assistant",
                "implemented `src/does_not_exist.rs` fully",
                "a2",
            ),
            mk_source("assistant", "verified the run end to end", "a3"),
        ];
        let mut claims = extract_claims(&sources, "2026-06-09T21:00:00Z");
        assert_eq!(claims.len(), 3);

        // repo root = this crate's source tree; schema.rs exists, the other not
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let results = collect_artifact_evidence(&claims, repo, "2026-06-09T21:01:00Z");
        assert_eq!(results.len(), 2, "only path-naming claims yield evidence");

        verify_claims(&mut claims, &results);
        assert_eq!(
            claims[0].verification_status,
            VerificationStatus::Verified,
            "existing artifact verifies the claim"
        );
        assert!(!claims[0].evidence_refs.is_empty());
        assert_eq!(
            claims[1].verification_status,
            VerificationStatus::Contradicted,
            "missing artifact contradicts the claim"
        );
        assert_eq!(
            claims[2].verification_status,
            VerificationStatus::Unverified,
            "claim without evidence stays unverified — never promoted"
        );
        assert!(claims[2].evidence_refs.is_empty());
    }

    #[test]
    fn fractures_surface_contradictions_and_unbacked_applause() {
        let sources = vec![
            mk_source("assistant", "implemented `src/nope.rs` end to end", "a1"),
            mk_source("assistant", "this is production ready", "a2"),
            mk_source("assistant", "fixed `src/intents/schema.rs`", "a3"),
        ];
        let mut claims = extract_claims(&sources, "2026-06-09T21:00:00Z");
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let results = collect_artifact_evidence(&claims, repo, "2026-06-09T21:01:00Z");
        verify_claims(&mut claims, &results);

        let fractures = detect_fractures(&claims);
        assert_eq!(
            fractures.len(),
            2,
            "contradicted claim + unbacked applause fracture; verified claim does not"
        );
        assert_eq!(fractures[0].severity, FractureSeverity::High);
        assert_eq!(fractures[1].severity, FractureSeverity::Medium);
        assert!(fractures.iter().all(|f| !f.options.is_empty()));
    }

    #[test]
    fn clarify_caps_at_five_and_asks_decisions_not_facts() {
        // 7 fractures in -> at most 5 questions out, severest first.
        let mk_fracture = |i: usize, severity| ContractFracture {
            contract_source: format!("agent claim claim-s1-{i}"),
            promised_surface: format!("promise {i}"),
            runtime_surface: "absent".to_string(),
            evidence: vec![format!("result-{i}")],
            severity,
            options: vec![
                "A: repair".to_string(),
                "B: retract".to_string(),
                "C: ship with gap".to_string(),
            ],
            recommended_clarify_question: Some(format!("clarify-{i}")),
        };
        let fractures: Vec<ContractFracture> = (0..7)
            .map(|i| {
                mk_fracture(
                    i,
                    if i == 6 {
                        FractureSeverity::Critical
                    } else {
                        FractureSeverity::Medium
                    },
                )
            })
            .collect();

        let questions = generate_clarify(&fractures, 10);
        assert_eq!(questions.len(), CLARIFY_MAX_QUESTIONS, "hard cap at 5");
        assert_eq!(
            questions[0].decision_id, "clarify-6",
            "severest fracture first"
        );

        for q in &questions {
            // decision-shaped: a real question with >=2 actionable options,
            // a default, and a named cost — not a fact lookup.
            assert!(q.question.ends_with('?'));
            assert!(
                q.question.contains("Repair it, retract it, or ship"),
                "question asks for a decision"
            );
            assert!(q.options.len() >= 2);
            assert!(!q.default_recommendation.is_empty());
            assert!(!q.cost_of_not_deciding.is_empty());
            assert!(
                !q.known_facts.is_empty(),
                "facts ride along instead of being asked"
            );
        }

        // an explicit lower max narrows further
        assert_eq!(generate_clarify(&fractures, 2).len(), 2);
    }
}
