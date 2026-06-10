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

/// Claim taxonomy (MASTER §305). Note `Green`/`ReadyToPush`/`Shippable`/
/// `NoBlockers` are the highest-risk claims — the "all green" / "production
/// ready" applause verdict — and must never be promoted to a Result without
/// Lane 3 evidence.
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

    /// The applause claims ("production ready", "all green") that must never
    /// be promoted to a Result without Lane 3 evidence. `Green` is included:
    /// "all green" is the classic evidence-free applause verdict.
    pub fn is_high_risk(self) -> bool {
        matches!(
            self,
            Self::Green | Self::ReadyToPush | Self::Shippable | Self::NoBlockers
        )
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
    /// Stable id of the [`ClaimRecord`] this fracture was detected on — the
    /// machine-readable link Lane 5 uses for `linked_claims`.
    pub claim_id: String,
    /// Human-readable provenance ("agent claim ... (session ...)"); display
    /// only, never used as a join key.
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

/// Lowercased word tokens, split on non-alphanumeric boundaries. Unicode
/// letters (Polish diacritics included) count as word characters, so
/// "naprawiłem" survives as one token.
fn claim_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// True when `phrase` (space-separated lowercase words) appears in `tokens`
/// as a contiguous token sequence. Token-boundary matching, never substring:
/// "incomplete" does not contain the token "complete".
fn contains_phrase(tokens: &[String], phrase: &str) -> bool {
    let needle: Vec<&str> = phrase.split_whitespace().collect();
    if needle.is_empty() || needle.len() > tokens.len() {
        return false;
    }
    tokens
        .windows(needle.len())
        .any(|w| w.iter().zip(needle.iter()).all(|(t, n)| t == n))
}

/// Classify a claim sentence into its [`ClaimType`] by surface claim-language.
///
/// Case-insensitive token-boundary phrase match in priority order (most
/// specific first): "no blockers" must not be misread as `Blocked`, and
/// "ready to ship" must not be swallowed by a generic ship/done marker.
/// Matching is on word boundaries, never raw substrings — "incomplete" is not
/// `complete`, "abandoned" is not `done`, "greenfield" is not `green`.
/// Returns `None` when no claim marker is present — absence is NOT
/// `Implemented` by default; a claim must actually claim something.
///
/// Markers cover English and Polish (the repo deliberately supports PL claim
/// language, mirroring `TYPED_DIRECTIVE_HEAD_MARKERS`); Polish needles are
/// listed both with and without diacritics ("naprawiłem"/"naprawilem").
///
/// `Green` / `ReadyToPush` / `Shippable` / `NoBlockers` are the highest-risk
/// claims (the "production ready" / "all green" applause verdict);
/// classification only labels them — Lane 3 must still demand evidence before
/// any of them becomes a Result.
pub fn classify_claim(text: &str) -> Option<ClaimType> {
    let tokens = claim_tokens(text);
    if tokens.is_empty() {
        return None;
    }
    // (needle, type) — list order IS precedence; first matching phrase wins.
    const RULES: &[(&str, ClaimType)] = &[
        ("no blocker", ClaimType::NoBlockers),
        ("no blockers", ClaimType::NoBlockers),
        ("zero blocker", ClaimType::NoBlockers),
        ("zero blockers", ClaimType::NoBlockers),
        ("bez blokerów", ClaimType::NoBlockers),
        ("bez blokerow", ClaimType::NoBlockers),
        ("blocked", ClaimType::Blocked),
        ("blocker", ClaimType::Blocked),
        ("blockers", ClaimType::Blocked),
        ("zablokowane", ClaimType::Blocked),
        ("production ready", ClaimType::ReadyToPush),
        ("ready to push", ClaimType::ReadyToPush),
        ("ready to ship", ClaimType::ReadyToPush),
        ("ready to merge", ClaimType::ReadyToPush),
        ("gotowe do push", ClaimType::ReadyToPush),
        ("gotowe do pusha", ClaimType::ReadyToPush),
        ("można pushować", ClaimType::ReadyToPush),
        ("mozna pushowac", ClaimType::ReadyToPush),
        ("shippable", ClaimType::Shippable),
        ("ship it", ClaimType::Shippable),
        ("migration complete", ClaimType::Migrated),
        ("migrated", ClaimType::Migrated),
        ("installed", ClaimType::Installed),
        ("wdrożone", ClaimType::Implemented),
        ("wdrozone", ClaimType::Implemented),
        ("docs updated", ClaimType::Documented),
        ("documented", ClaimType::Documented),
        ("verified", ClaimType::Verified),
        ("działa", ClaimType::Verified),
        ("dziala", ClaimType::Verified),
        ("all green", ClaimType::Green),
        ("tests green", ClaimType::Green),
        ("green", ClaimType::Green),
        ("wszystko zielone", ClaimType::Green),
        ("testy zielone", ClaimType::Green),
        ("zielone", ClaimType::Green),
        ("tests pass", ClaimType::Tested),
        ("passing", ClaimType::Tested),
        ("tested", ClaimType::Tested),
        ("przetestowane", ClaimType::Tested),
        ("testy przechodzą", ClaimType::Tested),
        ("testy przechodza", ClaimType::Tested),
        ("fixed", ClaimType::Fixed),
        ("naprawione", ClaimType::Fixed),
        ("naprawiłem", ClaimType::Fixed),
        ("naprawilem", ClaimType::Fixed),
        ("implemented", ClaimType::Implemented),
        ("shipped", ClaimType::Implemented),
        ("complete", ClaimType::Implemented),
        ("completed", ClaimType::Implemented),
        ("done", ClaimType::Implemented),
        ("zrobione", ClaimType::Implemented),
        ("gotowe", ClaimType::Implemented),
    ];
    RULES
        .iter()
        .find(|(needle, _)| contains_phrase(&tokens, needle))
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

/// True when `role` names an agent-originated row. Lane 2 doctrine: claims
/// are agent-originated, so user/system/tool/developer rows never become
/// claim sources. Public as THE single role predicate shared by the source
/// build in the CLI (`load_session_claims`) and the in-lane re-guard in
/// [`extract_claims`] — no hand-synced duplicates.
pub fn is_agent_role(role: &str) -> bool {
    matches!(
        role.to_lowercase().as_str(),
        "assistant" | "agent" | "model" | "gemini"
    )
}

/// True when `role` names a human/operator-originated row. Lane 1 doctrine:
/// intents belong to humans — a strict allowlist (not merely "not an agent"),
/// so tool/system/unknown rows never enter the intent lane.
pub fn is_user_role(role: &str) -> bool {
    matches!(role.to_lowercase().as_str(), "user" | "human" | "operator")
}

// ── Lane 1 — Human Intent (per-session view) ─────────────────────

/// One classified user-originated line from a session, for the per-session
/// truth report. Corpus-level Lane 1 stays `aicx intents` (user frames by
/// default); this is the same-classifier per-session view.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UserIntentLine {
    pub id: String,
    pub session_id: String,
    pub source_role: String,
    /// Classified kind: intent | decision | question | assumption | …
    pub entry_type: String,
    pub confidence: f32,
    pub raw_text: String,
    pub source_ref: String,
    /// Absolute source-message timestamp (RFC3339), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub timestamp_partial: bool,
    pub extracted_at: String,
}

/// Lane 1 stage (pure, per-session): classify user-originated lines into
/// intent-bearing entries. Doctrine enforced here:
/// - intents are user-originated — agent/tool/system rows never produce one
///   ([`is_user_role`] allowlist, re-guarded in-lane like [`extract_claims`]);
/// - no manufactured intents — a line that classifies to nothing is skipped,
///   and pasted-reference material is rejected by the shared classifier;
/// - result/outcome-shaped lines belong to the evidence lane even when a user
///   pasted them — they never become human intent;
/// - identical lines are deduplicated (earliest occurrence wins), since agent
///   logs frequently double-record the same user turn.
pub fn extract_user_intent_lines(
    sources: &[ClaimSource],
    extracted_at: &str,
) -> Vec<UserIntentLine> {
    let mut out: Vec<UserIntentLine> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (idx, src) in sources.iter().enumerate() {
        if !is_user_role(&src.role) {
            continue;
        }
        for line in src.text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((entry_type, confidence)) = super::classify_line_entry_type(line, true) else {
                continue;
            };
            if matches!(
                entry_type,
                crate::types::EntryType::Result | crate::types::EntryType::Outcome
            ) {
                continue;
            }
            if !seen.insert(line.to_string()) {
                continue;
            }
            out.push(UserIntentLine {
                id: format!(
                    "intent-{}-{}-{}",
                    src.session_id,
                    idx,
                    claim_hash8(line, &src.source_ref)
                ),
                session_id: src.session_id.clone(),
                source_role: src.role.clone(),
                entry_type: entry_type.as_str().to_string(),
                confidence,
                raw_text: line.to_string(),
                source_ref: src.source_ref.clone(),
                timestamp: src.timestamp.clone(),
                timestamp_partial: src.timestamp_partial,
                extracted_at: extracted_at.to_string(),
            });
        }
    }
    out
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64-bit fold step — inline, dependency-free, deterministic.
fn fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// 8-hex deterministic content hash over `(claim_text, source_ref)`, used to
/// disambiguate claim ids across separate `extract_claims` calls. A NUL
/// separator keeps the pair unambiguous ("ab"+"c" vs "a"+"bc").
fn claim_hash8(claim_text: &str, source_ref: &str) -> String {
    let h = fnv1a64(FNV_OFFSET, claim_text.as_bytes());
    let h = fnv1a64(h, &[0]);
    let h = fnv1a64(h, source_ref.as_bytes());
    format!("{:08x}", (h ^ (h >> 32)) as u32)
}

/// Lane 2 stage (pure): turn agent/assistant source rows into Unverified
/// [`ClaimRecord`]s. Doctrine enforced here:
/// - claims are agent-originated — user rows never produce a claim;
/// - a row whose text carries no claim marker is skipped — no manufactured
///   claims (absence of a marker is not a claim);
/// - every claim is `Unverified` until Lane 3 supplies evidence;
/// - the applause claims (green/ready/shippable/no-blockers) are flagged
///   high-risk.
///
/// Claim id shape: `claim-<session_id>-<row_index>-<hash8>` where `hash8` is a
/// deterministic FNV-1a content hash of `(claim_text, source_ref)`. Uniqueness
/// scope: ids are unique within one call per source row, and the content hash
/// disambiguates rows that share `(session_id, index)` across separate calls
/// over different sources. Identical input rows deliberately produce identical
/// ids (deterministic, re-runnable extraction — not globally unique).
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
                id: format!(
                    "claim-{}-{i}-{}",
                    s.session_id,
                    claim_hash8(claim_line, &s.source_ref)
                ),
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

/// Redact the user's home prefix in an absolute path to `~` so excerpts do
/// not leak the local username into exported lane records. Home is resolved
/// the same way as everywhere else in the repo (`dirs::home_dir()`, which
/// honors `$HOME` on Unix). Only whole-component prefixes are redacted —
/// `/Users/silverton` is not touched when home is `/Users/silver`.
fn redact_home(path: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    if home.is_empty() || !path.starts_with(home) {
        return path.to_string();
    }
    let rest = &path[home.len()..];
    match rest.chars().next() {
        None => "~".to_string(),
        Some('/') => format!("~{rest}"),
        Some(_) => path.to_string(),
    }
}

/// Lane 3 stage (deterministic, read-only): for every file path a claim names,
/// check whether the artifact actually exists under `repo_root` (or absolutely).
/// Existence yields a `Pass` result, absence a `Fail` — both are evidence; a
/// claim that names no artifact gets no result here and stays unverified.
/// Nothing is executed — this collects repo-state evidence only. Absolute
/// paths under the user's home are redacted to `~` in
/// `observed_output_excerpt` (the raw `artifact_path` stays checkable).
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
                observed_output_excerpt: Some({
                    let shown_token = redact_home(&token);
                    let shown_root = redact_home(&repo_root.display().to_string());
                    if exists {
                        format!("{shown_token}: exists in {shown_root}")
                    } else {
                        format!("{shown_token}: NOT FOUND in {shown_root}")
                    }
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
/// not-X); an applause claim (green/ready/shippable/no-blockers) with no evidence is
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
                    claim_id: claim.id.clone(),
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
                    claim_id: claim.id.clone(),
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
/// `min(max, CLARIFY_MAX_QUESTIONS)` questions, severest fractures first;
/// severity ties break on `claim_id` so the selection under the cap is
/// deterministic regardless of input order.
pub fn generate_clarify(fractures: &[ContractFracture], max: usize) -> Vec<ClarifyQuestion> {
    let cap = max.min(CLARIFY_MAX_QUESTIONS);
    let mut ordered: Vec<&ContractFracture> = fractures.iter().collect();
    // severest first; FractureSeverity derives Low..Critical in declaration
    // order, so sort by reverse discriminant via a manual rank. Secondary key
    // claim_id keeps tie-breaking (and thus the cap cutoff) deterministic.
    let rank = |s: FractureSeverity| match s {
        FractureSeverity::Critical => 0,
        FractureSeverity::High => 1,
        FractureSeverity::Medium => 2,
        FractureSeverity::Low => 3,
    };
    ordered.sort_by(|a, b| {
        rank(a.severity)
            .cmp(&rank(b.severity))
            .then_with(|| a.claim_id.cmp(&b.claim_id))
    });
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
            linked_claims: vec![f.claim_id.clone()],
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
            claim_id: "claim-1".into(),
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
    fn user_intent_lines_keep_user_rows_and_drop_agent_and_tool_text() {
        let sources = vec![
            mk_source("user", "Decision: ship the lanes envelope first", "u1"),
            // agent text with an intent-shaped marker must NEVER enter Lane 1
            mk_source(
                "assistant",
                "Decision: I delivered everything already",
                "a1",
            ),
            // tool/system rows are outside the strict user allowlist
            mk_source("tool", "decision: noise from a tool row", "t1"),
            // unclassified user chatter is skipped — no manufactured intents
            mk_source("user", "hello there team", "u2"),
        ];

        let lines = extract_user_intent_lines(&sources, "2026-06-09T21:00:00Z");
        assert_eq!(lines.len(), 1, "only classified USER lines survive");
        let line = &lines[0];
        assert_eq!(line.source_role, "user");
        assert_eq!(line.entry_type, "decision");
        assert_eq!(line.session_id, "s1");
        // P0 temporal: absolute source timestamp + extraction stamp, explicit
        // partial marker
        assert_eq!(line.timestamp.as_deref(), Some("2026-06-09T20:41:00Z"));
        assert!(!line.timestamp_partial);
        assert_eq!(line.extracted_at, "2026-06-09T21:00:00Z");
    }

    #[test]
    fn assistant_completion_text_becomes_claim_never_intent() {
        // Lane separation: the same assistant row feeds Lane 2 and is invisible
        // to Lane 1.
        let sources = vec![mk_source(
            "assistant",
            "implemented the report command, suite is green",
            "a1",
        )];
        let claims = extract_claims(&sources, "2026-06-09T21:00:00Z");
        let lines = extract_user_intent_lines(&sources, "2026-06-09T21:00:00Z");
        assert_eq!(claims.len(), 1, "completion text is an audit target");
        assert!(lines.is_empty(), "agent text must never enter Lane 1");
    }

    #[test]
    fn user_intent_lines_propagate_partial_timestamp_marker() {
        let mut src = mk_source("user", "decision: keep UTC everywhere", "u1");
        src.timestamp_partial = true;
        let lines = extract_user_intent_lines(&[src], "2026-06-09T21:00:00Z");
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].timestamp_partial,
            "partial time is never silently presented as full"
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

        // machine-readable link: fracture carries the originating claim id,
        // while contract_source stays human-readable provenance
        assert_eq!(fractures[0].claim_id, claims[0].id);
        assert_eq!(fractures[1].claim_id, claims[1].id);
        assert!(fractures[0].contract_source.contains(&claims[0].id));

        // Lane 5 links questions to claims via claim_id, not contract_source
        let questions = generate_clarify(&fractures, 5);
        assert_eq!(questions[0].linked_claims, vec![claims[0].id.clone()]);
    }

    #[test]
    fn clarify_caps_at_five_and_asks_decisions_not_facts() {
        // 7 fractures in -> at most 5 questions out, severest first.
        let mk_fracture = |i: usize, severity| ContractFracture {
            claim_id: format!("claim-s1-{i}"),
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

    #[test]
    fn classify_claim_matches_word_boundaries_not_substrings() {
        // P2-12: substring matching produced false positives; token-boundary
        // matching must not see a marker inside a larger word.
        assert_eq!(classify_claim("the spec is incomplete"), None);
        assert_eq!(classify_claim("abandoned the approach"), None);
        assert_eq!(classify_claim("greenfield rewrite plan"), None);
        assert_eq!(classify_claim("no trespassing rules apply"), None);

        // the real markers still hit on their own word boundaries
        assert_eq!(
            classify_claim("the migration is complete"),
            Some(ClaimType::Implemented)
        );
        assert_eq!(classify_claim("it is green"), Some(ClaimType::Green));
    }

    #[test]
    fn classify_claim_supports_polish_markers() {
        use ClaimType::*;
        // with and without diacritics — both spellings occur in the wild
        assert_eq!(classify_claim("naprawione w parserze"), Some(Fixed));
        assert_eq!(classify_claim("naprawiłem ten bug"), Some(Fixed));
        assert_eq!(classify_claim("naprawilem ten bug"), Some(Fixed));
        assert_eq!(classify_claim("zrobione"), Some(Implemented));
        assert_eq!(classify_claim("gotowe"), Some(Implemented));
        assert_eq!(classify_claim("wdrożone na staging"), Some(Implemented));
        assert_eq!(classify_claim("wdrozone na staging"), Some(Implemented));
        assert_eq!(classify_claim("przetestowane lokalnie"), Some(Tested));
        assert_eq!(classify_claim("testy przechodzą"), Some(Tested));
        assert_eq!(classify_claim("testy przechodza"), Some(Tested));
        assert_eq!(classify_claim("działa na produkcji"), Some(Verified));
        assert_eq!(classify_claim("dziala end to end"), Some(Verified));
        assert_eq!(classify_claim("wszystko zielone"), Some(Green));
        assert_eq!(classify_claim("testy zielone"), Some(Green));
        assert_eq!(classify_claim("bez blokerów"), Some(NoBlockers));
        assert_eq!(classify_claim("bez blokerow"), Some(NoBlockers));
        assert_eq!(classify_claim("gotowe do push"), Some(ReadyToPush));
        assert_eq!(classify_claim("gotowe do pusha"), Some(ReadyToPush));
        assert_eq!(classify_claim("można pushować"), Some(ReadyToPush));
        assert_eq!(classify_claim("mozna pushowac"), Some(ReadyToPush));

        // precedence: the ReadyToPush phrase outranks the generic "gotowe"
        assert_eq!(
            classify_claim("gotowe do push po review"),
            Some(ReadyToPush)
        );
    }

    #[test]
    fn green_claim_is_high_risk_applause() {
        // P2-14: "all green" is the classic applause verdict without evidence.
        assert!(ClaimType::Green.is_high_risk());
        assert!(ClaimType::ReadyToPush.is_high_risk());
        assert!(!ClaimType::Fixed.is_high_risk());

        let claims = extract_claims(
            &[mk_source("assistant", "all green", "a1")],
            "2026-06-09T21:00:00Z",
        );
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].claim_type, ClaimType::Green);
        assert_eq!(
            claims[0].risk_flags,
            vec!["high_risk_unverified_claim".to_string()]
        );

        // an unverified green claim surfaces as a Medium fracture
        let fractures = detect_fractures(&claims);
        assert_eq!(fractures.len(), 1);
        assert_eq!(fractures[0].severity, FractureSeverity::Medium);
        assert_eq!(fractures[0].claim_id, claims[0].id);
    }

    #[test]
    fn claim_ids_are_deterministic_and_content_scoped() {
        // P2-02: claim-<sid>-<i> collided across separate extract_claims calls;
        // the content hash disambiguates while staying deterministic.
        let a = vec![mk_source("assistant", "fixed the parser", "a1")];
        let b = vec![mk_source("assistant", "fixed the linter", "a1")];

        let first = extract_claims(&a, "2026-06-09T21:00:00Z");
        let again = extract_claims(&a, "2026-06-09T22:00:00Z");
        let other = extract_claims(&b, "2026-06-09T21:00:00Z");

        assert_eq!(first[0].id, again[0].id, "same input -> same id");
        assert_ne!(
            first[0].id, other[0].id,
            "same (session, index), different claim text -> different id"
        );
        assert!(first[0].id.starts_with("claim-s1-0-"));
        let suffix = first[0].id.rsplit('-').next().unwrap();
        assert_eq!(suffix.len(), 8, "8-hex content hash suffix");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));

        // same text, different source_ref -> different id too
        let c = vec![mk_source("assistant", "fixed the parser", "a2")];
        let other_ref = extract_claims(&c, "2026-06-09T21:00:00Z");
        assert_ne!(first[0].id, other_ref[0].id);
    }

    #[test]
    fn clarify_breaks_severity_ties_deterministically_by_claim_id() {
        // P3-10: >cap fractures of the same severity must yield a stable,
        // input-order-independent selection (secondary sort key: claim_id).
        let mk = |id: &str| ContractFracture {
            claim_id: id.to_string(),
            contract_source: format!("agent claim {id}"),
            promised_surface: format!("promise {id}"),
            runtime_surface: "absent".to_string(),
            evidence: Vec::new(),
            severity: FractureSeverity::Medium,
            options: vec!["A: repair".to_string(), "B: retract".to_string()],
            recommended_clarify_question: None,
        };
        let shuffled = ["c-9", "c-3", "c-7", "c-1", "c-5", "c-8", "c-2"];
        let fractures: Vec<ContractFracture> = shuffled.iter().map(|id| mk(id)).collect();

        let picked: Vec<String> = generate_clarify(&fractures, 5)
            .into_iter()
            .map(|q| q.linked_claims[0].clone())
            .collect();
        assert_eq!(picked, vec!["c-1", "c-2", "c-3", "c-5", "c-7"]);

        // reversed input order picks the exact same set in the same order
        let reversed: Vec<ContractFracture> = shuffled.iter().rev().map(|id| mk(id)).collect();
        let picked_rev: Vec<String> = generate_clarify(&reversed, 5)
            .into_iter()
            .map(|q| q.linked_claims[0].clone())
            .collect();
        assert_eq!(picked, picked_rev);
    }

    #[test]
    fn artifact_evidence_redacts_home_prefix_in_excerpt() {
        // P2-03: absolute paths under the user's home must not leak the local
        // username into exported excerpts. Uses the real home dir (read-only,
        // no env mutation) with a path that is guaranteed not to exist.
        let home = dirs::home_dir().expect("home dir resolvable in tests");
        let missing = home.join("aicx-schema-redaction-test-does-not-exist.rs");
        let text = format!("implemented {} fully", missing.display());

        let claims = extract_claims(
            &[mk_source("assistant", &text, "a1")],
            "2026-06-09T21:00:00Z",
        );
        let results = collect_artifact_evidence(&claims, Path::new("/tmp"), "2026-06-09T21:01:00Z");
        assert_eq!(results.len(), 1);

        let excerpt = results[0].observed_output_excerpt.as_deref().unwrap();
        assert!(
            excerpt.starts_with("~/aicx-schema-redaction-test-does-not-exist.rs"),
            "home prefix redacted to ~ in excerpt: {excerpt}"
        );
        assert!(
            !excerpt.contains(home.to_string_lossy().as_ref()),
            "raw home path must not leak into excerpt: {excerpt}"
        );
        // the raw artifact_path stays checkable (unredacted)
        assert_eq!(
            results[0].artifact_path.as_deref(),
            Some(missing.display().to_string().as_str())
        );
    }

    #[test]
    fn empty_inputs_yield_empty_outputs() {
        // P3-14 edge: empty pipelines stay empty, no panics, no manufactured
        // records.
        assert!(extract_claims(&[], "2026-06-09T21:00:00Z").is_empty());
        assert!(generate_clarify(&[], 5).is_empty());
    }

    #[test]
    fn is_agent_role_accepts_agent_rows_and_rejects_the_rest() {
        // The single shared predicate behind role_filter="agent_only": both
        // the CLI source build and the extract_claims re-guard call THIS.
        for role in ["assistant", "agent", "model", "gemini"] {
            assert!(is_agent_role(role), "{role} is agent-originated");
        }
        // case-insensitive
        assert!(is_agent_role("Assistant"));
        assert!(is_agent_role("MODEL"));
        for role in ["user", "system", "tool", "developer", "operator", ""] {
            assert!(!is_agent_role(role), "{role:?} must never source a claim");
        }
    }
}
