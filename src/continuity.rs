//! Continuity pack — the multi-agent session-resume surface (P1).
//!
//! `aicx continuity show|write` renders one deterministic markdown pack per
//! project bucket + time window: open work (NOW), what each peer agent did
//! (PEERS), decision candidates, tasks, the evidence trail (SOURCES), and an
//! honest INDEX HEALTH line. It replaces "read the compact of yourself" as
//! the way a fresh session recovers context: live parse first, census
//! second, semantics never required for a hot window.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::intents::{self, IntentKind, IntentRecord, IntentsConfig};

/// Character budget for `--for-inject` (~6k tokens at ≈4 chars/token). The
/// pack is prompt preamble — it must never crowd out the actual task.
const INJECT_CHAR_BUDGET: usize = 24_000;

const NOW_CAP: usize = 10;
const PEER_SESSION_CAP: usize = 8;
const PEER_CLAIM_CAP: usize = 3;
const DECISION_CAP: usize = 15;
const TASK_CAP: usize = 15;
const SOURCE_CAP: usize = 20;

pub struct ContinuityPack {
    pub selection: Vec<intents::SourceSelection>,
    pub source_errors: usize,
    pub dropped_task_events: usize,
    pub window_end: String,
    pub project_label: String,
    pub hours: u64,
    pub live_sessions: usize,
    pub records: Vec<IntentRecord>,
    pub sources: Vec<SourceLine>,
    pub index_health: IndexHealthLine,
    /// Sessions whose structural scope is a mixed-workstream candidate
    /// (W2-R1). Their records are NOT distilled into NOW/DECISIONS unless
    /// the caller passed `distill_mixed`; they are listed instead.
    pub mixed_scope: Vec<crate::intents::MixedScopeSession>,
    /// Whether mixed candidates were distilled on explicit request.
    pub distilled_mixed: bool,
    /// Copied from `extraction.stats.candidate_cap` in `build_with_scope`.
    pub candidate_cap: usize,
    /// Copied from `extraction.stats.dropped_candidates` in `build_with_scope`.
    /// Zero means the cap was not reached.
    pub dropped_candidates: usize,
    /// Sessions served without frames whose turn window could not be placed.
    /// Those frames are never distilled, with or without `distill_mixed`,
    /// and the pack lists what it left out.
    pub unplaced_scope: Vec<crate::intents::UnplacedScopeSession>,
}

pub struct SourceLine {
    pub agent: String,
    pub path: String,
    /// Latest retained claim activity, never source-file mtime.
    pub latest_activity: Option<String>,
    pub live: bool,
}

pub struct IndexHealthLine {
    pub newest_session_updated_at: Option<String>,
    pub committed_at: Option<String>,
    pub pending: usize,
    pub sessions_newer_than_chunks: usize,
    pub readiness: String,
    pub mode: &'static str,
}

/// Collect the continuity pack for one window. Source order is the doctrine:
/// live parse (intents live window) → census fingerprints → index status.
/// No embedder involvement anywhere on this path.
///
/// Default = do not distill a mixed workstream into one history (W2-R1):
/// a session that proves more than one scope — a workdir conflict, a scope
/// hidden by `.aicxignore`, or more than one repository — or whose work ran
/// in a checkout other than its cataloged one is withheld. When the window
/// holds such a session and no homogeneous record is left — even when
/// upstream filters had already removed every frame of it — the pack refuses
/// instead of returning an empty or braided narrative: with
/// `RefusalReason::MixedWorkstream`, or with a plain error when the only such
/// work sits in one foreign checkout, which that reason cannot express.
///
/// Frames whose turn window could not be placed are never distilled. The
/// session they belong to is still served from its placed frames, and the pack
/// lists what it left out. A window with nothing else to distill refuses.
pub fn build(aicx_home: &Path, projects: &[String], hours: u64) -> Result<ContinuityPack> {
    build_with_scope(aicx_home, projects, hours, false)
}

/// [`build`] with the mixed-scope decision explicit. `distill_mixed = true`
/// is the operator's override: mixed candidates are distilled and the pack
/// says so.
pub fn build_with_scope(
    aicx_home: &Path,
    projects: &[String],
    hours: u64,
    distill_mixed: bool,
) -> Result<ContinuityPack> {
    build_with_scope_at(aicx_home, projects, hours, distill_mixed, Utc::now())
}

/// Frozen-clock variant: extraction and rendering share one window endpoint.
pub fn build_with_scope_at(
    aicx_home: &Path,
    projects: &[String],
    hours: u64,
    distill_mixed: bool,
    now: DateTime<Utc>,
) -> Result<ContinuityPack> {
    let config = IntentsConfig {
        project: projects.first().cloned().unwrap_or_default(),
        hours,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
        live: true,
    };
    let extraction = intents::extract_intents_from_root_at_for_projects_with_stats(
        &config, projects, aicx_home, now,
    )?;

    let index_health = collect_index_health(aicx_home, projects, extraction.stats.live_sessions);

    let mixed_scope = extraction.mixed_scope;
    let unplaced_scope = extraction.unplaced_scope;
    let mut records = extraction.records;
    if !distill_mixed {
        withhold_mixed_sessions(&mut records, &mixed_scope)?;
    }
    refuse_unplaced_only_window(&records, &unplaced_scope)?;
    for record in &mut records {
        if let Some(timestamp) = record_time(record) {
            record.timestamp = Some(timestamp.to_rfc3339());
        }
    }
    let sources = collect_sources(&extraction.selection, &records);

    Ok(ContinuityPack {
        selection: extraction.selection,
        source_errors: extraction.stats.source_errors,
        dropped_task_events: extraction.stats.dropped_task_events,
        window_end: now.to_rfc3339(),
        project_label: if projects.is_empty() {
            "(all projects)".to_string()
        } else {
            projects.join(", ")
        },
        hours,
        live_sessions: extraction.stats.live_sessions,
        records,
        sources,
        index_health,
        mixed_scope,
        distilled_mixed: distill_mixed,
        candidate_cap: extraction.stats.candidate_cap,
        dropped_candidates: extraction.stats.dropped_candidates,
        unplaced_scope,
    })
}

/// A window whose only work sat in turn windows no checkout could claim
/// refuses instead of rendering an empty NOW.
///
/// The intents filter withholds such frames before anything reaches this pack.
/// With nothing else left, an empty pack would read as "no work in this
/// window", when the truth is "work this pack cannot place". A pack that
/// still holds records lists the sessions it served in part instead.
fn refuse_unplaced_only_window(
    records: &[IntentRecord],
    unplaced_scope: &[intents::UnplacedScopeSession],
) -> Result<()> {
    if !records.is_empty() || unplaced_scope.is_empty() {
        return Ok(());
    }
    let frames: usize = unplaced_scope.iter().map(|session| session.frames).sum();
    anyhow::bail!(
        "continuity: nothing placed to distill; {frames} frame(s) from {} session(s) were withheld because their turn window ran where no checkout could claim it",
        unplaced_scope.len()
    )
}

/// Scope gate (W2-R1): a mixed candidate is not distilled into one history by
/// default. Records from those sessions are withheld from the narrative (the
/// sessions stay listed), and when nothing homogeneous is left the pack
/// refuses loudly rather than rendering an empty NOW.
///
/// "Nothing left" is judged on what REMAINS, not on how much this gate
/// withheld. The intents filters fail closed upstream, so a mixed session can
/// arrive here with none of its frames — `.aicxignore` hid its baseline, or
/// no frame could be attributed to the project — and still be the only work
/// in the window. Refusing only when this gate had withheld something let
/// exactly that window through as a successful, empty pack.
fn withhold_mixed_sessions(
    records: &mut Vec<IntentRecord>,
    mixed_scope: &[intents::MixedScopeSession],
) -> Result<()> {
    if mixed_scope.is_empty() {
        return Ok(());
    }
    let mixed_keys: std::collections::BTreeSet<(&str, &str)> = mixed_scope
        .iter()
        .map(|session| (session.agent.as_str(), session.session_id.as_str()))
        .collect();
    let before = records.len();
    records.retain(|record| {
        !mixed_keys.contains(&(record.agent.as_str(), record.session_id.as_str()))
    });
    if records.is_empty() {
        return Err(mixed_window_refusal(mixed_scope, before));
    }
    Ok(())
}

/// The error a window of only mixed-workstream sessions refuses with.
///
/// Built from the session's WHOLE scope verdict. Rebuilding the report from
/// cwds alone zeroed the conflicts and hidden scopes that made a session
/// mixed, so a session mixed only by a conflict or a `.aicxignore`-hidden
/// checkout — at most one visible cwd — no longer looked mixed, and the
/// `expect` on the refusal panicked the whole command.
fn mixed_window_refusal(
    mixed_scope: &[intents::MixedScopeSession],
    withheld: usize,
) -> anyhow::Error {
    let context = format!(
        "continuity: {} mixed-workstream session(s) in the window and nothing homogeneous to distill; pass distill_mixed to override",
        mixed_scope.len()
    );
    let refusal = mixed_scope.iter().find_map(|session| {
        let report = crate::extraction::conversation::ScopeReport {
            hidden_scopes: session.hidden_scopes,
            status: session.status,
            cwds: session.cwds.clone(),
            branches: session.branches.clone(),
            entries: withheld,
            conflicts: session.conflicts,
        };
        crate::extraction::conversation::refuse_mixed_workstream(
            session.agent_kind(),
            &session.session_id,
            &report,
            false,
        )
    });
    match refusal {
        Some(refusal) => anyhow::Error::new(refusal).context(context),
        // A session is listed when its lane could not serve it whole: more
        // than one scope, which refuses structurally above, or ONE scope
        // foreign to its cataloged checkout, which that refusal cannot
        // express. The telemetry keeps the cataloged path out of `cwds`, so
        // a foreign-only session reads as one scope here. Still a refusal,
        // never a panic and never an empty pack.
        None => anyhow::anyhow!("{context} (the work ran outside its cataloged checkout)"),
    }
}

/// Sources are receipts for retained claims, not a second catalog/mtime scan.
/// Qualification without a classifier record remains visible in coverage.
fn collect_sources(
    selection: &[intents::SourceSelection],
    records: &[IntentRecord],
) -> Vec<SourceLine> {
    let mut sources: BTreeMap<(&str, &str), SourceLine> = BTreeMap::new();
    for selected in selection.iter().filter(|source| {
        matches!(
            source.status.as_str(),
            "qualified" | "unbounded_unknown_time"
        )
    }) {
        let retained: Vec<&IntentRecord> = records
            .iter()
            .filter(|record| {
                record.agent == selected.agent
                    && record.session_id == selected.session_id
                    && record.source_chunk == selected.path
            })
            .collect();
        if retained.is_empty() {
            continue;
        }
        let latest = newest_record_time(&retained).map(|time| time.to_rfc3339());
        let source = sources
            .entry((selected.agent.as_str(), selected.path.as_str()))
            .or_insert_with(|| SourceLine {
                agent: selected.agent.clone(),
                path: selected.path.clone(),
                latest_activity: None,
                live: !selected.admitted,
            });
        if latest > source.latest_activity {
            source.latest_activity = latest;
        }
    }
    let mut sources: Vec<SourceLine> = sources.into_values().collect();
    sources.sort_by(|a, b| {
        b.latest_activity
            .cmp(&a.latest_activity)
            .then_with(|| a.path.cmp(&b.path))
    });
    sources
}

fn collect_index_health(
    aicx_home: &Path,
    projects: &[String],
    live_sessions: usize,
) -> IndexHealthLine {
    // A `-p /repo` filter expands to several buckets; health of an arbitrary
    // first bucket (e.g. one dormant since spring) must not masquerade as the
    // pack's health. Report the bucket with the newest session activity —
    // that is the one whose staleness would actually poison this pack.
    let mut best = None;
    let scopes: Vec<Option<&str>> = if projects.is_empty() {
        vec![None]
    } else {
        projects.iter().map(|p| Some(p.as_str())).collect()
    };
    for scope in scopes {
        if let Ok(status) = crate::api::index_status_at(aicx_home, scope) {
            let fresher = best
                .as_ref()
                .is_none_or(|current: &crate::api::IndexStatus| {
                    status.newest_session_updated_at > current.newest_session_updated_at
                });
            if fresher {
                best = Some(status);
            }
        }
    }
    match best {
        Some(status) => IndexHealthLine {
            newest_session_updated_at: status.newest_session_updated_at,
            committed_at: status.committed_at,
            pending: status.pending_chunks,
            sessions_newer_than_chunks: status.sessions_newer_than_chunks,
            readiness: format!("{:?}", status.readiness).to_lowercase(),
            mode: if live_sessions > 0 { "live" } else { "census" },
        },
        None => IndexHealthLine {
            newest_session_updated_at: None,
            committed_at: None,
            pending: 0,
            sessions_newer_than_chunks: 0,
            readiness: "unknown".to_string(),
            mode: if live_sessions > 0 { "live" } else { "census" },
        },
    }
}

/// Coverage and render limits belong at the head so inject cannot silently
/// turn a bounded narrative into a claim of exhaustive project history.
fn push_honesty_preface(out: &mut String, pack: &ContinuityPack) {
    out.push_str("## HONESTY\n\n");
    let end = DateTime::parse_from_rfc3339(&pack.window_end)
        .ok()
        .map(|time| time.with_timezone(&Utc));
    let cutoff = end.map(|time| intents::window_cutoff(time, pack.hours));
    out.push_str(&format!(
        "window: [{}, {}] UTC, by qualifying frame timestamp; file mtime and catalog date do not admit a claim.\n",
        cutoff.map(|time| time.to_rfc3339()).as_deref().unwrap_or("unknown"),
        end.map(|time| time.to_rfc3339()).as_deref().unwrap_or("unknown")
    ));
    if pack.hours == 0 {
        out.push_str("unbounded history: undated claims can be retained as unknown-time candidates; no timestamp is inferred.\n");
    }
    out.push_str("labels: Decision, Intent, Outcome, and Task are classifier candidates, not verified Founder decisions or completed work (verification_state=not_verified_by_aicx). A user role alone does not authenticate quoted instructions.\n");
    out.push_str("now: open means a retained live_open record; missing records do not prove thread absence. Session ids and project handles are stored identities; truncated ids, aliases, renames and splits are not resolved here.\n");
    out.push_str("time: a later record or unrelated Outcome does not retire an earlier request; no explicit resolution links are available. Human requests and constraint candidates precede peer claims.\n");

    let qualified = pack
        .selection
        .iter()
        .filter(|source| source.status == "qualified")
        .count();
    let unknown_time: usize = pack
        .selection
        .iter()
        .map(|source| source.unknown_time_frames)
        .sum();
    let frames: usize = pack
        .selection
        .iter()
        .map(|source| source.qualified_frames)
        .sum();
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for source in &pack.selection {
        *reasons.entry(&source.status).or_default() += 1;
    }
    let reason_line = reasons
        .iter()
        .map(|(reason, count)| format!("{reason}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        "coverage: considered_sources={} · qualified_sources={qualified} · qualified_frames={frames} · represented_sources={} · sources_shown={} · qualified_without_retained_claim={} · source_rows_omitted={} · source_errors={} · unknown_time_frames={unknown_time}.\n",
        pack.selection.len(), pack.sources.len(), pack.sources.len().min(SOURCE_CAP),
        qualified.saturating_sub(pack.sources.len()), pack.sources.len().saturating_sub(SOURCE_CAP), pack.source_errors
    ));
    out.push_str(&format!(
        "source admission: discovered={} · catalog_admitted={} · live_unadmitted={}\n",
        pack.selection.len(),
        pack.selection.iter().filter(|s| s.admitted).count(),
        pack.selection.iter().filter(|s| !s.admitted).count()
    ));
    out.push_str(&format!("selection_status: {reason_line}\n"));
    out.push_str(&format!(
        "frame omissions: outside_window={} · scope_withheld={} · unknown_time={}\n",
        pack.selection
            .iter()
            .map(|s| s.outside_window_frames)
            .sum::<usize>(),
        pack.selection
            .iter()
            .map(|s| s.scope_withheld_frames)
            .sum::<usize>(),
        unknown_time
    ));
    out.push_str(&format!(
        "scope omissions: mixed_sessions={} · unplaced_frames={} · mixed_distilled={} (explicit request).\n",
        pack.mixed_scope.len(), pack.unplaced_scope.iter().map(|session| session.frames).sum::<usize>(), pack.distilled_mixed
    ));
    let providers: std::collections::BTreeSet<&str> = pack
        .selection
        .iter()
        .map(|source| source.agent.as_str())
        .chain(pack.records.iter().map(|record| record.agent.as_str()))
        .collect();
    for agent in providers {
        out.push_str(&format!(
            "provider {agent}: considered_sources={} · qualified_sources={} · represented_sources={} · sources_shown={} · retained_records={}\n",
            pack.selection.iter().filter(|source| source.agent == agent).count(),
            pack.selection.iter().filter(|source| source.agent == agent && source.status == "qualified").count(),
            pack.sources.iter().filter(|source| source.agent == agent).count(),
            pack.sources.iter().take(SOURCE_CAP).filter(|source| source.agent == agent).count(),
            pack.records.iter().filter(|record| record.agent == agent).count()
        ));
    }
    out.push_str("source cleaning: existing signal readers can remove harness/base64/overlong lines and cap each message at 262144 Unicode characters; parser coverage is not word-for-word payload coverage.\n");
    out.push_str("coverage is limited to discovered sources; provider completeness is not inferred. Index readiness is separate and cannot certify continuity coverage.\n");
    out.push_str(&format!(
        "extraction caps: candidate_cap={} · dropped_candidates={} · dropped_task_events={} · source_errors={}{}\n",
        pack.candidate_cap, pack.dropped_candidates, pack.dropped_task_events, pack.source_errors,
        if pack.dropped_candidates > 0 || pack.dropped_task_events > 0 || pack.source_errors > 0 {
            " · incomplete extraction"
        } else { " · these extraction caps not reached" }
    ));
    let requests = unresolved_intents(&pack.records).len();
    let human_decisions = pack
        .records
        .iter()
        .filter(|record| human_record(record) && record.kind == IntentKind::Decision)
        .count();
    let open_sessions: std::collections::BTreeSet<(&str, &str)> = pack
        .records
        .iter()
        .filter(|record| record.honesty.is_live_open())
        .map(|record| (record.agent.as_str(), record.session_id.as_str()))
        .collect();
    let decisions = pack
        .records
        .iter()
        .filter(|record| record.kind == IntentKind::Decision)
        .count();
    let tasks = pack
        .records
        .iter()
        .filter(|record| record.kind == IntentKind::Task)
        .count();
    let mut peer_sessions: BTreeMap<(&str, &str), Vec<&IntentRecord>> = BTreeMap::new();
    for record in &pack.records {
        peer_sessions
            .entry((record.agent.as_str(), record.session_id.as_str()))
            .or_default()
            .push(record);
    }
    let mut per_agent: BTreeMap<&str, Vec<(&str, Vec<&IntentRecord>)>> = BTreeMap::new();
    for ((agent, session), records) in peer_sessions {
        per_agent.entry(agent).or_default().push((session, records));
    }
    let mut peer_sessions_shown = 0;
    let mut peer_claims_shown = 0;
    let mut peer_sessions_total = 0;
    for sessions in per_agent.values_mut() {
        sessions.sort_by(|(id_a, a), (id_b, b)| {
            newest_record_time(b)
                .cmp(&newest_record_time(a))
                .then_with(|| id_a.cmp(id_b))
        });
        peer_sessions_total += sessions.len();
        peer_sessions_shown += sessions.len().min(PEER_SESSION_CAP);
        peer_claims_shown += sessions
            .iter()
            .take(PEER_SESSION_CAP)
            .map(|(_, records)| records.len().min(PEER_CLAIM_CAP))
            .sum::<usize>();
    }
    out.push_str(&format!(
        "render caps before inject: NOW open_sessions={}/{} (cap={NOW_CAP}); requests_and_human_constraints={}/{} (cap={NOW_CAP}); PEERS sessions={peer_sessions_shown}/{peer_sessions_total} (cap={PEER_SESSION_CAP}/provider), claims={peer_claims_shown}/{} (cap={PEER_CLAIM_CAP}/session); DECISIONS={}/{} (cap={DECISION_CAP}); TASKS={}/{} (cap={TASK_CAP}); SOURCES={}/{} (cap={SOURCE_CAP}); mixed_details={}/{}; unplaced_details={}/{}. Counts are shown/available; excess is omitted.\n\n",
        open_sessions.len().min(NOW_CAP), open_sessions.len(),
        (requests + human_decisions).min(NOW_CAP), requests + human_decisions,
        pack.records.len(), decisions.min(DECISION_CAP), decisions, tasks.min(TASK_CAP), tasks,
        pack.sources.len().min(SOURCE_CAP), pack.sources.len(),
        pack.mixed_scope.len().min(NOW_CAP), pack.mixed_scope.len(),
        pack.unplaced_scope.len().min(NOW_CAP), pack.unplaced_scope.len()
    ));
}

fn record_time(record: &IntentRecord) -> Option<DateTime<Utc>> {
    record
        .timestamp
        .as_deref()
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|time| time.with_timezone(&Utc))
}

fn newest_record_time(records: &[&IntentRecord]) -> Option<DateTime<Utc>> {
    records
        .iter()
        .filter_map(|record| record_time(record))
        .max()
}

fn human_record(record: &IntentRecord) -> bool {
    record.provenance.as_ref().is_some_and(|provenance| {
        matches!(
            provenance.role.to_ascii_lowercase().as_str(),
            "user" | "human" | "founder" | "user_msg"
        )
    })
}

/// One atomic line includes the complete claim and its provenance. Embedded
/// newlines are escaped so an inject budget can never detach those two.
fn claim_line(record: &IntentRecord, label: &str, indent: &str) -> String {
    let (role, locator, scope, basis, attribution) = record
        .provenance
        .as_ref()
        .map(|provenance| {
            (
                provenance.role.as_str(),
                provenance.locator.as_str(),
                provenance.scope.as_deref().unwrap_or("unknown"),
                provenance.timestamp_basis.as_str(),
                provenance.attribution.as_str(),
            )
        })
        .unwrap_or((
            "unknown",
            record.source_chunk.as_str(),
            "unknown",
            "unknown",
            "unattributed",
        ));
    let timestamp = record_time(record)
        .map(|time| time.to_rfc3339())
        .unwrap_or_else(|| "unknown".to_string());
    let line = format!(
        "{indent}- {label} [{} · {}] · role={role} · locator={locator} · scope={scope} · timestamp={timestamp} · timestamp_basis={basis} · attribution={attribution} · status={} · claim_scope={} · source={}: {}",
        record.agent,
        record.session_id,
        record
            .honesty
            .verification_state
            .as_deref()
            .unwrap_or("not_verified_by_aicx"),
        record.honesty.claim_scope.as_deref().unwrap_or("unknown"),
        record.source_chunk,
        record.summary
    );
    format!("{}\n", line.replace('\r', "\\r").replace('\n', "\\n"))
}

/// Retain complete lines within a Unicode-character budget, including the
/// omission marker. Long claims are omitted whole, allowing later requests
/// to survive rather than stopping at one over-sized claim.
pub(crate) fn bounded_inject(out: String) -> String {
    if out.chars().count() <= INJECT_CHAR_BUDGET {
        return out;
    }
    const MARKER: &str = "\n[continuity pack truncated at inject budget; content omitted; claim lines retained whole]\n";
    let limit = INJECT_CHAR_BUDGET - MARKER.chars().count();
    let mut bounded = String::new();
    let mut used = 0;
    for line in out.split_inclusive('\n') {
        let count = line.chars().count();
        if used + count <= limit {
            bounded.push_str(line);
            used += count;
        }
    }
    bounded.push_str(MARKER);
    bounded
}

/// Render the pack. `for_inject` bounds complete claim lines in Unicode chars.
pub fn render(pack: &ContinuityPack, for_inject: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# CONTINUITY · {} · {}h\n\n",
        pack.project_label, pack.hours
    ));
    push_honesty_preface(&mut out, pack);

    out.push_str("## NOW\n\n");
    let mut requests = unresolved_intents(&pack.records);
    requests.extend(
        pack.records
            .iter()
            .filter(|record| human_record(record) && record.kind == IntentKind::Decision),
    );
    requests.sort_by(|a, b| {
        human_record(b)
            .cmp(&human_record(a))
            .then_with(|| record_time(b).cmp(&record_time(a)))
            .then_with(|| a.agent.cmp(&b.agent))
            .then_with(|| a.session_id.cmp(&b.session_id))
            .then_with(|| a.source_chunk.cmp(&b.source_chunk))
    });
    for record in requests.into_iter().take(NOW_CAP) {
        let label = if record.kind == IntentKind::Intent {
            "unresolved intent candidate"
        } else {
            "human constraint/decision candidate"
        };
        out.push_str(&claim_line(record, label, ""));
    }
    let mut open_sessions: BTreeMap<(&str, &str), Option<DateTime<Utc>>> = BTreeMap::new();
    for record in pack
        .records
        .iter()
        .filter(|record| record.honesty.is_live_open())
    {
        let newest = open_sessions
            .entry((record.agent.as_str(), record.session_id.as_str()))
            .or_default();
        let timestamp = record_time(record);
        if timestamp > *newest {
            *newest = timestamp;
        }
    }
    let mut open_sessions: Vec<_> = open_sessions.into_iter().collect();
    open_sessions.sort_by(|(id_a, a), (id_b, b)| b.cmp(a).then_with(|| id_a.cmp(id_b)));
    if open_sessions.is_empty() {
        out.push_str("- no retained open-session records inside the window; thread absence is not established\n");
    }
    for ((agent, session), timestamp) in open_sessions.into_iter().take(NOW_CAP) {
        out.push_str(&format!(
            "- open: {agent} · {session} · {}\n",
            timestamp
                .map(|time| time.to_rfc3339())
                .as_deref()
                .unwrap_or("unknown")
        ));
    }
    if !pack.mixed_scope.is_empty() {
        out.push_str(&format!(
            "- mixed-workstream candidates: {} session(s) {} (scope_status=mixed_candidate)\n",
            pack.mixed_scope.len(),
            if pack.distilled_mixed {
                "distilled on request"
            } else {
                "NOT distilled into this pack"
            }
        ));
        for session in pack.mixed_scope.iter().take(NOW_CAP) {
            out.push_str(&format!(
                "  - {} · {} · cwds={} branches={}\n",
                session.agent,
                session.session_id,
                session.cwds.join(","),
                session.branches.join(",")
            ));
        }
    }
    if !pack.unplaced_scope.is_empty() {
        out.push_str(&format!("- unplaced work: {} frame(s) from {} session(s) NOT in this pack (their turn window ran where no checkout could claim it)\n",
            pack.unplaced_scope.iter().map(|session| session.frames).sum::<usize>(), pack.unplaced_scope.len()));
        for session in pack.unplaced_scope.iter().take(NOW_CAP) {
            out.push_str(&format!(
                "  - {} · {} · frames={}\n",
                session.agent, session.session_id, session.frames
            ));
        }
    }
    out.push('\n');

    out.push_str("## PEERS\n\n");
    let mut by_agent: BTreeMap<&str, BTreeMap<&str, Vec<&IntentRecord>>> = BTreeMap::new();
    for record in &pack.records {
        by_agent
            .entry(record.agent.as_str())
            .or_default()
            .entry(record.session_id.as_str())
            .or_default()
            .push(record);
    }
    if by_agent.is_empty() {
        out.push_str("- no retained records inside the window\n");
    }
    for (agent, sessions) in by_agent {
        out.push_str(&format!("### {agent}\n"));
        let mut ordered: Vec<_> = sessions.into_iter().collect();
        ordered.sort_by(|(id_a, a), (id_b, b)| {
            newest_record_time(b)
                .cmp(&newest_record_time(a))
                .then_with(|| id_a.cmp(id_b))
        });
        for (session, mut records) in ordered.into_iter().take(PEER_SESSION_CAP) {
            let marker = if records.iter().any(|record| record.honesty.is_live_open()) {
                " [open]"
            } else {
                ""
            };
            let timestamp = newest_record_time(&records)
                .map(|time| time.to_rfc3339())
                .unwrap_or_else(|| "unknown".to_string());
            out.push_str(&format!("- {session}{marker} · {timestamp}\n"));
            records.sort_by(|a, b| {
                human_record(b)
                    .cmp(&human_record(a))
                    .then_with(|| record_time(b).cmp(&record_time(a)))
                    .then_with(|| a.source_chunk.cmp(&b.source_chunk))
            });
            for record in records.into_iter().take(PEER_CLAIM_CAP) {
                out.push_str(&claim_line(
                    record,
                    &format!("{} candidate", record.kind.heading().to_lowercase()),
                    "  ",
                ));
            }
        }
    }
    out.push('\n');

    out.push_str("## DECISIONS (candidates; unverified)\n\n");
    let mut decisions: Vec<_> = pack
        .records
        .iter()
        .filter(|record| record.kind == IntentKind::Decision)
        .collect();
    decisions.sort_by(|a, b| {
        human_record(b)
            .cmp(&human_record(a))
            .then_with(|| record_time(b).cmp(&record_time(a)))
            .then_with(|| a.source_chunk.cmp(&b.source_chunk))
    });
    if decisions.is_empty() {
        out.push_str("- none classified in the window\n");
    }
    for record in decisions.into_iter().take(DECISION_CAP) {
        out.push_str(&claim_line(record, "decision candidate", ""));
    }
    out.push('\n');

    out.push_str("## TASKS\n\n");
    let mut tasks: Vec<_> = pack
        .records
        .iter()
        .filter(|record| record.kind == IntentKind::Task)
        .collect();
    tasks.sort_by(|a, b| {
        human_record(b)
            .cmp(&human_record(a))
            .then_with(|| record_time(b).cmp(&record_time(a)))
            .then_with(|| a.source_chunk.cmp(&b.source_chunk))
    });
    if tasks.is_empty() {
        out.push_str("- none classified in the window\n");
    }
    for record in tasks.into_iter().take(TASK_CAP) {
        out.push_str(&claim_line(record, "task candidate", ""));
    }
    out.push('\n');

    out.push_str("## SOURCES\n\n");
    if pack.sources.is_empty() {
        out.push_str(
            "- no source paths represented by retained records; see qualification coverage above\n",
        );
    }
    for source in pack.sources.iter().take(SOURCE_CAP) {
        out.push_str(&format!(
            "- {} · latest_retained_claim_activity={} · {}{}\n",
            source.agent,
            source.latest_activity.as_deref().unwrap_or("unknown"),
            source.path,
            if source.live { " [unadmitted]" } else { "" }
        ));
    }
    out.push('\n');

    out.push_str("## INDEX HEALTH\n\n");
    let health = &pack.index_health;
    out.push_str(&format!(
        "- newest_source_file_mtime: {} (file touch, not conversation time)\n",
        health
            .newest_session_updated_at
            .as_deref()
            .unwrap_or("<none>")
    ));
    out.push_str(&format!(
        "- index_committed: {}\n",
        health.committed_at.as_deref().unwrap_or("<none>")
    ));
    out.push_str(&format!(
        "- sessions_newer_than_chunks: {} · pending: {}\n",
        health.sessions_newer_than_chunks, health.pending
    ));
    out.push_str(&format!("- readiness: {} · mode: {} · live_sessions: {} · index readiness is not continuity coverage\n", health.readiness, health.mode, pack.live_sessions));
    if health.pending > 0 || health.sessions_newer_than_chunks > 0 {
        out.push_str(&format!("- warning: chunk lag (pending={}, sessions_newer_than_chunks={}); run `aicx catalog rebuild --with-chunks` or `aicx index` — empty NOW/PEERS is not proof of a quiet window\n", health.pending, health.sessions_newer_than_chunks));
    }
    if for_inject { bounded_inject(out) } else { out }
}

/// Without explicit request-to-outcome links every Intent remains a candidate
/// unresolved request. A same-session Outcome does not prove its resolution.
fn unresolved_intents(records: &[IntentRecord]) -> Vec<&IntentRecord> {
    let mut unresolved: Vec<_> = records
        .iter()
        .filter(|record| record.kind == IntentKind::Intent)
        .collect();
    unresolved.sort_by(|a, b| {
        record_time(b)
            .cmp(&record_time(a))
            .then_with(|| a.agent.cmp(&b.agent))
            .then_with(|| a.session_id.cmp(&b.session_id))
            .then_with(|| a.source_chunk.cmp(&b.source_chunk))
    });
    unresolved
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use aicx_parser::engine::{RefusalReason, ScopeStatus};

    /// Finding (P1-05): a session mixed only by a proven conflict or by a
    /// scope `.aicxignore` hid has at most one visible cwd. The refusal was
    /// rebuilt from cwds alone, so it no longer read as mixed, and the
    /// `expect` on it panicked the whole command.
    #[test]
    fn a_window_mixed_only_by_conflict_or_hidden_scope_refuses_without_panicking() {
        let session = |conflicts, hidden_scopes| intents::MixedScopeSession {
            agent: "codex".to_string(),
            session_id: "s1".to_string(),
            cwds: vec!["/repo/alpha".to_string()],
            branches: Vec::new(),
            conflicts,
            hidden_scopes,
            status: ScopeStatus::NoDriftObserved,
        };
        for (label, mixed) in [
            ("conflict-only", session(2, 0)),
            ("hidden-only", session(0, 1)),
        ] {
            let error = mixed_window_refusal(std::slice::from_ref(&mixed), 3);
            assert!(
                matches!(
                    error.downcast_ref::<RefusalReason>(),
                    Some(RefusalReason::MixedWorkstream { .. })
                ),
                "{label}: {error:#}"
            );
        }

        // A session listed for running wholly in one foreign checkout has one
        // visible scope, and an empty list has none: both still refuse — as a
        // plain error, never a panic.
        for mixed in [vec![session(0, 0)], Vec::new()] {
            let error = mixed_window_refusal(&mixed, 3);
            assert!(error.downcast_ref::<RefusalReason>().is_none());
            assert!(
                format!("{error:#}").contains("nothing homogeneous to distill"),
                "{error:#}"
            );
        }

        // Any session that supports the structured refusal gives it, not only
        // the first one listed.
        let error = mixed_window_refusal(&[session(0, 0), session(2, 0)], 3);
        assert!(
            matches!(
                error.downcast_ref::<RefusalReason>(),
                Some(RefusalReason::MixedWorkstream { .. })
            ),
            "{error:#}"
        );
    }

    /// Finding: the gate refused only when it had withheld something itself.
    /// A mixed session whose frames the fail-closed intents filters had
    /// already removed arrived with zero records, and the window returned a
    /// successful, empty pack.
    #[test]
    fn a_window_of_only_mixed_work_refuses_even_when_nothing_was_withheld() {
        let mixed = intents::MixedScopeSession {
            agent: "codex".to_string(),
            session_id: "s1".to_string(),
            cwds: vec!["/repo/alpha".to_string()],
            branches: Vec::new(),
            conflicts: 0,
            hidden_scopes: 1,
            status: ScopeStatus::NoDriftObserved,
        };
        let mut records: Vec<IntentRecord> = Vec::new();
        let error = withhold_mixed_sessions(&mut records, std::slice::from_ref(&mixed))
            .expect_err("a window holding only a mixed session refuses");
        assert!(
            matches!(
                error.downcast_ref::<RefusalReason>(),
                Some(RefusalReason::MixedWorkstream { .. })
            ),
            "{error:#}"
        );

        // A homogeneous record is kept and the mixed session's is withheld;
        // a window with no mixed session is not the gate's business.
        let record = |session_id: &str| IntentRecord {
            provenance: None,
            kind: IntentKind::Decision,
            summary: format!("decided in {session_id}"),
            context: None,
            evidence: Vec::new(),
            project: "Loctree/aicx".to_string(),
            agent: "codex".to_string(),
            date: "2026-09-24".to_string(),
            timestamp: None,
            session_id: session_id.to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: String::new(),
            source: None,
            honesty: Default::default(),
        };
        let mut records = vec![record("s1"), record("s2")];
        withhold_mixed_sessions(&mut records, std::slice::from_ref(&mixed))
            .expect("a homogeneous record is left");
        assert_eq!(records, vec![record("s2")]);
        let mut records: Vec<IntentRecord> = Vec::new();
        withhold_mixed_sessions(&mut records, &[]).expect("no mixed session, nothing to refuse");
    }

    /// One transcript, one dated catalog row, primed live delta. Shared by the
    /// section render and the real-path census test.
    fn write_continuity_real_path_home(label: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "aicx-continuity-{label}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&root);

        let source = root.join("continuity-a/session.jsonl");
        fs::create_dir_all(source.parent().expect("parent")).expect("create parent");
        fs::write(
            &source,
            format!("{}\n", serde_json::json!({"type":"user","timestamp":Utc::now().to_rfc3339(),"sessionId":"continuity-a","cwd":"/fixtures/Loctree/aicx","message":{"role":"user","content":"Decision: route continuity through the live window engine."}})),
        )
        .expect("write source");
        let catalog_path = crate::catalog::sessions_path_for(&root);
        fs::create_dir_all(catalog_path.parent().expect("catalog parent"))
            .expect("create catalog dir");
        let entry = crate::catalog::CatalogEntry {
            schema: crate::catalog::CATALOG_SCHEMA.to_string(),
            session_id: "continuity-a".to_string(),
            agent: "claude".to_string(),
            project: Some("Loctree/aicx".to_string()),
            date: Some(Utc::now().format("%Y-%m-%d").to_string()),
            cwd: Some("/fixtures/Loctree/aicx".into()),
            source_path: source.display().to_string(),
            source_len: None,
            source_mtime_ns: None,
            source_bundle_fingerprint: None,
            title: None,
            machine: Some("test".to_string()),
            logical_session_id: None,
            session_kind: None,
        };
        fs::write(
            &catalog_path,
            format!("{}\n", serde_json::to_string(&entry).expect("serialize")),
        )
        .expect("write catalog");
        let user_home = crate::os_user_home().unwrap_or_else(|| root.clone());
        let cutoff_ns = (Utc::now() - chrono::Duration::hours(24))
            .timestamp_nanos_opt()
            .map(|nanos| nanos.max(0) as u128)
            .unwrap_or(0);
        crate::catalog::prime_live_delta_cache_for_tests(
            &root,
            &user_home,
            cutoff_ns,
            crate::catalog::LiveDelta::default(),
        );
        (root, source)
    }

    fn render_pack_with_census(candidate_cap: usize, dropped_candidates: usize) -> String {
        let pack = ContinuityPack {
            selection: Vec::new(),
            source_errors: 0,
            dropped_task_events: 0,
            window_end: String::new(),
            project_label: "Loctree/aicx".into(),
            hours: 24,
            live_sessions: 0,
            records: Vec::new(),
            sources: Vec::new(),
            index_health: IndexHealthLine {
                newest_session_updated_at: None,
                committed_at: None,
                pending: 0,
                sessions_newer_than_chunks: 0,
                readiness: "unknown".into(),
                mode: "census",
            },
            mixed_scope: Vec::new(),
            distilled_mixed: false,
            candidate_cap,
            dropped_candidates,
            unplaced_scope: Vec::new(),
        };
        render(&pack, false)
    }

    #[test]
    fn continuity_pack_renders_all_sections_deterministically() {
        let (root, source) = write_continuity_real_path_home("sections");
        let projects = vec!["Loctree/aicx".to_string()];
        let now = Utc::now();
        let pack = build_with_scope_at(&root, &projects, 24, false, now).expect("build pack");
        let first = render(&pack, false);
        let second = render(
            &build_with_scope_at(&root, &projects, 24, false, now).expect("rebuild pack"),
            false,
        );

        for heading in [
            "# CONTINUITY · Loctree/aicx · 24h",
            "## HONESTY",
            "## NOW",
            "## PEERS",
            "## DECISIONS (candidates; unverified)",
            "## TASKS",
            "## SOURCES",
            "## INDEX HEALTH",
        ] {
            assert!(first.contains(heading), "missing {heading} in:\n{first}");
        }
        assert!(
            first.contains("continuity-a"),
            "peer session id missing:\n{first}"
        );
        assert!(
            first.contains(&source.display().to_string()),
            "source evidence path missing:\n{first}"
        );
        assert_eq!(first, second, "continuity pack must be deterministic");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn continuity_honesty_preface_names_truncation_and_classifier() {
        let open = render_pack_with_census(5_000, 0);
        let truncated = render_pack_with_census(5_000, 3);
        for rendered in [&open, &truncated] {
            let honesty = rendered.find("## HONESTY").expect("honesty heading");
            let now = rendered.find("## NOW").expect("now heading");
            assert!(honesty < now, "preface must precede NOW:\n{rendered}");
            for line in [
                "by qualifying frame timestamp; file mtime and catalog date do not admit a claim",
                "classifier candidates, not verified Founder decisions or completed work",
                "A user role alone does not authenticate quoted instructions",
                "Session ids and project handles are stored identities",
                "unrelated Outcome does not retire an earlier request",
                "Index readiness is separate and cannot certify continuity coverage",
            ] {
                assert!(rendered.contains(line), "missing preface line: {line}");
            }
        }
        assert!(open.contains("candidate_cap=5000 · dropped_candidates=0"));
        assert!(!open.contains("incomplete extraction"));
        assert!(truncated.contains("candidate_cap=5000 · dropped_candidates=3"));
        assert!(!truncated.contains("not reached"));
    }

    #[test]
    fn continuity_build_prints_census_from_real_path() {
        let (root, _source) = write_continuity_real_path_home("census");
        let projects = vec!["Loctree/aicx".to_string()];
        let pack = build(&root, &projects, 24).expect("build pack");
        let rendered = render(&pack, false);
        assert!(
            rendered.contains("coverage:"),
            "missing census line:\n{rendered}"
        );
        assert!(
            rendered.contains("not reached"),
            "small home must stay under the candidate cap:\n{rendered}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn continuity_now_lists_live_open_sessions() {
        let pack = ContinuityPack {
            selection: Vec::new(),
            source_errors: 0,
            dropped_task_events: 0,
            window_end: String::new(),
            project_label: "vetcoders/vibecrafted".into(),
            hours: 24,
            live_sessions: 1,
            records: vec![crate::intents::IntentRecord {
                provenance: None,
                kind: IntentKind::Intent,
                summary: "keep live window independent of census".into(),
                context: None,
                evidence: Vec::new(),
                project: "vetcoders/vibecrafted".into(),
                agent: "claude".into(),
                date: "2026-08-13".into(),
                timestamp: Some("2026-08-13T02:00:00Z".into()),
                session_id: "hot-open".into(),
                count: None,
                first_chunk: None,
                last_chunk: None,
                source_chunk: "sess.jsonl".into(),
                source: None,
                honesty: crate::oracle::ClaimHonesty::live_open(),
            }],
            sources: Vec::new(),
            index_health: IndexHealthLine {
                newest_session_updated_at: Some("2026-08-13T02:00:00Z".into()),
                committed_at: None,
                pending: 0,
                sessions_newer_than_chunks: 0,
                readiness: "ready".into(),
                mode: "live",
            },
            mixed_scope: Vec::new(),
            distilled_mixed: false,
            candidate_cap: 5_000,
            dropped_candidates: 0,
            unplaced_scope: Vec::new(),
        };
        let rendered = render(&pack, false);
        assert!(rendered.contains("open: claude · hot-open"));
        assert!(!rendered.contains("warning: chunk lag"));
    }

    #[test]
    fn continuity_now_states_live_open_rule() {
        let pack = ContinuityPack {
            selection: Vec::new(),
            source_errors: 0,
            dropped_task_events: 0,
            window_end: String::new(),
            project_label: "vetcoders/vibecrafted".into(),
            hours: 24,
            live_sessions: 2,
            records: vec![
                crate::intents::IntentRecord {
                    provenance: None,
                    kind: IntentKind::Intent,
                    summary: "keep live window independent of census".into(),
                    context: None,
                    evidence: Vec::new(),
                    project: "vetcoders/vibecrafted".into(),
                    agent: "claude".into(),
                    date: "2026-08-13".into(),
                    timestamp: Some("2026-08-13T02:00:00Z".into()),
                    session_id: "hot-open".into(),
                    count: None,
                    first_chunk: None,
                    last_chunk: None,
                    source_chunk: "sess.jsonl".into(),
                    source: None,
                    honesty: crate::oracle::ClaimHonesty::live_open(),
                },
                crate::intents::IntentRecord {
                    provenance: None,
                    kind: IntentKind::Intent,
                    summary: "open row whose conversation date was not stored".into(),
                    context: None,
                    evidence: Vec::new(),
                    project: "vetcoders/vibecrafted".into(),
                    agent: "claude".into(),
                    date: "2026-08-13".into(),
                    timestamp: None,
                    session_id: "open-no-ts".into(),
                    count: None,
                    first_chunk: None,
                    last_chunk: None,
                    source_chunk: "sess-no-ts.jsonl".into(),
                    source: None,
                    honesty: crate::oracle::ClaimHonesty::live_open(),
                },
                crate::intents::IntentRecord {
                    provenance: None,
                    kind: IntentKind::Intent,
                    summary: "canonical claim is not an open session".into(),
                    context: None,
                    evidence: Vec::new(),
                    project: "vetcoders/vibecrafted".into(),
                    agent: "codex".into(),
                    date: "2026-08-01".into(),
                    timestamp: Some("2026-08-01T00:00:00Z".into()),
                    session_id: "canonical-closed".into(),
                    count: None,
                    first_chunk: None,
                    last_chunk: None,
                    source_chunk: "canonical.jsonl".into(),
                    source: None,
                    honesty: crate::oracle::ClaimHonesty::canonical(),
                },
            ],
            sources: Vec::new(),
            index_health: IndexHealthLine {
                newest_session_updated_at: Some("2026-08-13T02:00:00Z".into()),
                committed_at: None,
                pending: 0,
                sessions_newer_than_chunks: 0,
                readiness: "ready".into(),
                mode: "live",
            },
            mixed_scope: Vec::new(),
            distilled_mixed: false,
            candidate_cap: 5_000,
            dropped_candidates: 0,
            unplaced_scope: Vec::new(),
        };
        let rendered = render(&pack, false);
        for sentence in [
            "open means a retained live_open record",
            "missing records do not prove thread absence",
            "truncated ids, aliases, renames and splits are not resolved here",
        ] {
            assert!(
                rendered.contains(sentence),
                "missing {sentence} in:\n{rendered}"
            );
        }
        let open_lines: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("- open:"))
            .collect();
        assert!(
            open_lines
                .iter()
                .any(|line| line.contains("claude · hot-open · 2026-08-13T02:00:00+00:00")),
            "live_open row missing: {open_lines:?}"
        );
        assert!(
            open_lines
                .iter()
                .any(|line| line.contains("claude · open-no-ts · unknown")),
            "missing conversation date must say so: {open_lines:?}"
        );
        assert!(
            rendered.contains("canonical-closed"),
            "canonical record was not rendered at all:\n{rendered}"
        );
        assert!(
            open_lines
                .iter()
                .all(|line| !line.contains("canonical-closed")),
            "canonical record leaked under - open:: {open_lines:?}"
        );
    }

    #[test]
    fn continuity_index_health_warns_on_pending_chunks() {
        let pack = ContinuityPack {
            selection: Vec::new(),
            source_errors: 0,
            dropped_task_events: 0,
            window_end: String::new(),
            project_label: "vetcoders/vibecrafted".into(),
            hours: 24,
            live_sessions: 1,
            records: Vec::new(),
            sources: Vec::new(),
            index_health: IndexHealthLine {
                newest_session_updated_at: Some("2026-08-13T01:48:00Z".into()),
                committed_at: Some("2026-08-13T01:48:00Z".into()),
                pending: 631,
                sessions_newer_than_chunks: 12,
                readiness: "stale_chunks".into(),
                mode: "live",
            },
            mixed_scope: Vec::new(),
            distilled_mixed: false,
            candidate_cap: 5_000,
            dropped_candidates: 0,
            unplaced_scope: Vec::new(),
        };
        let rendered = render(&pack, false);
        assert!(rendered.contains(
            "newest_source_file_mtime: 2026-08-13T01:48:00Z (file touch, not conversation time)"
        ));
        assert!(rendered.contains("warning: chunk lag (pending=631"));
        assert!(rendered.contains("aicx catalog rebuild --with-chunks"));
    }

    #[test]
    fn continuity_index_health_names_source_file_mtime() {
        let pack = ContinuityPack {
            selection: Vec::new(),
            source_errors: 0,
            dropped_task_events: 0,
            window_end: String::new(),
            project_label: "vetcoders/vibecrafted".into(),
            hours: 24,
            live_sessions: 1,
            records: Vec::new(),
            sources: Vec::new(),
            index_health: IndexHealthLine {
                newest_session_updated_at: Some("2026-09-21T03:30:00Z".into()),
                committed_at: None,
                pending: 0,
                sessions_newer_than_chunks: 0,
                readiness: "ready".into(),
                mode: "live",
            },
            mixed_scope: Vec::new(),
            distilled_mixed: false,
            candidate_cap: 5_000,
            dropped_candidates: 0,
            unplaced_scope: Vec::new(),
        };
        let rendered = render(&pack, false);
        assert!(rendered.contains(
            "newest_source_file_mtime: 2026-09-21T03:30:00Z (file touch, not conversation time)"
        ));
        assert!(rendered.contains("newest_source_file_mtime"));
        assert!(rendered.contains("file touch, not conversation time"));
    }

    #[test]
    fn continuity_now_lists_what_it_withheld_as_unplaced() {
        let unplaced = |session_id: &str, frames: usize| crate::intents::UnplacedScopeSession {
            agent: "codex".into(),
            session_id: session_id.into(),
            frames,
        };
        let pack = ContinuityPack {
            selection: Vec::new(),
            source_errors: 0,
            dropped_task_events: 0,
            window_end: String::new(),
            project_label: "vetcoders/vibecrafted".into(),
            hours: 24,
            live_sessions: 0,
            records: Vec::new(),
            sources: Vec::new(),
            index_health: IndexHealthLine {
                newest_session_updated_at: None,
                committed_at: None,
                pending: 0,
                sessions_newer_than_chunks: 0,
                readiness: "ready".into(),
                mode: "live",
            },
            mixed_scope: Vec::new(),
            distilled_mixed: false,
            candidate_cap: 5_000,
            dropped_candidates: 0,
            unplaced_scope: vec![unplaced("s-one", 3), unplaced("s-two", 1)],
        };
        let rendered = render(&pack, false);
        assert!(
            rendered.contains("- unplaced work: 4 frame(s) from 2 session(s) NOT in this pack"),
            "{rendered}"
        );
        assert!(
            rendered.contains("  - codex · s-one · frames=3"),
            "{rendered}"
        );
        assert!(
            rendered.contains("  - codex · s-two · frames=1"),
            "{rendered}"
        );
    }

    #[test]
    fn a_window_of_only_unplaced_frames_refuses() {
        let unplaced = [crate::intents::UnplacedScopeSession {
            agent: "codex".into(),
            session_id: "s-one".into(),
            frames: 2,
        }];
        let error = refuse_unplaced_only_window(&[], &unplaced)
            .expect_err("nothing placed to distill refuses");
        assert!(
            error
                .to_string()
                .contains("nothing placed to distill; 2 frame(s) from 1 session(s)"),
            "{error}"
        );
        refuse_unplaced_only_window(&[], &[]).expect("an empty window with nothing withheld");
    }
    #[test]
    fn continuity_contract_unrelated_outcome_does_not_close_intents() {
        let (root, _) = write_continuity_real_path_home("outcome-contract");
        let mut pack = build(&root, &["Loctree/aicx".into()], 24).unwrap();
        let mut record = pack.records[0].clone();
        record.kind = IntentKind::Intent;
        record.summary = "preserve all audio unless explicitly opted out".into();
        let mut other = record.clone();
        other.summary = "add separate delivery and revision buses".into();
        let mut outcome = record.clone();
        outcome.kind = IntentKind::Outcome;
        outcome.summary = "cargo check completed".into();
        pack.records = vec![record, other, outcome];
        assert_eq!(unresolved_intents(&pack.records).len(), 2);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn continuity_contract_unicode_injection_never_panics() {
        let (root, _) = write_continuity_real_path_home("unicode-contract");
        let mut pack = build(&root, &["Loctree/aicx".into()], 24).unwrap();
        // Exercise every byte alignment, rather than relying on one accidental boundary.
        for padding in 0..4 {
            pack.records[0].summary = format!("{}{}", "x".repeat(padding), "🦀".repeat(30_000));
            let result = std::panic::catch_unwind(|| render(&pack, true));
            assert!(result.is_ok(), "UTF-8 boundary at padding {padding}");
            assert!(result.unwrap().contains("truncated"));
        }
        fs::remove_dir_all(root).unwrap();
    }
}
