//! Continuity pack — the multi-agent session-resume surface (P1).
//!
//! `aicx continuity show|write` renders one deterministic markdown pack per
//! project bucket + time window: open work (NOW), what each peer agent did
//! (PEERS), decision candidates, tasks, the evidence trail (SOURCES), and an
//! honest INDEX HEALTH line. It replaces "read the compact of yourself" as
//! the way a fresh session recovers context: live parse first, census
//! second, semantics never required for a hot window.
//! `--as <name>` adds cross-project Codescribe bus speech addressed through
//! structured recipient/audience metadata. Dictation has no addressee and is
//! excluded. Names use AICX's Unicode/diacritic-folded, case-insensitive match.
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::intents::{self, IntentKind, IntentRecord, IntentsConfig};

/// Character budget for `--for-inject` (~6k tokens at ≈4 chars/token). The
/// pack is prompt preamble — it must never crowd out the actual task.
const INJECT_CHAR_BUDGET: usize = 24_000;

/// Newest NOW body lines kept whole while OPEN WORK rows, ADDRESSED TO rows,
/// and the other claim sections are still what the budget can cut.
const NOW_INJECT_FLOOR: usize = 5;

/// HONESTY lines that say which window this is and that every label is an
/// unverified candidate. With the pack title they outlive addressed speech;
/// the rest of HONESTY is cut before it.
const HONESTY_INJECT_HEAD: [&str; 3] = ["window:", "unbounded history:", "labels:"];

const NOW_CAP: usize = 10;
const PEER_SESSION_CAP: usize = 8;
const PEER_CLAIM_CAP: usize = 3;
const DECISION_CAP: usize = 15;
const TASK_CAP: usize = 15;
const SOURCE_CAP: usize = 20;

/// Output mode for `aicx continuity show|write --for <MODE>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum ContinuityMode {
    /// Full continuity pack (default).
    #[default]
    Pack,
    /// Prompt-inject bounded continuity pack (~6k tokens).
    Inject,
    /// Structured handoff document with explicit pack-write and event timestamps.
    Handoff,
    /// Condensed executive continuity summary with explicit timestamps.
    Summary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuityWriteReport {
    pub content: String,
    pub appended: bool,
    pub new_claims: usize,
    pub deduplicated_claims: usize,
    pub written_at: String,
}

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
    /// Local branches of the filtered repository carrying commits HEAD lacks
    /// by patch identity. `None` when not collected (hand-built packs); the
    /// section and its HONESTY line render only when collected.
    pub open_work: Option<crate::open_work::OpenWork>,
}

pub struct SourceLine {
    pub agent: String,
    pub path: String,
    /// Latest retained claim activity, never source-file mtime.
    pub latest_activity: Option<String>,
    pub live: bool,
    /// User-role turns the extraction kept for this source.
    pub human_messages: usize,
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
    refuse_unread_only_window(&records, extraction.stats.source_errors)?;
    for record in &mut records {
        if let Some(timestamp) = record_time(record) {
            record.timestamp = Some(timestamp.to_rfc3339());
        }
    }
    // Distilled mixed records are cited like any other: their sources are
    // receipts, not omissions.
    let withheld: &[intents::MixedScopeSession] = if distill_mixed { &[] } else { &mixed_scope };
    let sources = collect_sources(&extraction.selection, &records, withheld);

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
        open_work: Some(crate::open_work::collect(aicx_home, projects)),
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

/// A window with nothing distilled refuses when the extraction could not read
/// all of it.
///
/// `source_errors` counts sources the reader could not cover whole: an
/// unreadable or unsupported body, a malformed tail, a partial parse. With
/// records left, the honesty preface names those holes next to the work that
/// survived. With none left, the empty NOW would read as "a quiet window"
/// when the truth is "a window this pack could not read".
fn refuse_unread_only_window(records: &[IntentRecord], source_errors: usize) -> Result<()> {
    if !records.is_empty() || source_errors == 0 {
        return Ok(());
    }
    anyhow::bail!(
        "continuity: nothing to distill; {source_errors} source(s) in the window were unreadable, unsupported, or only partially covered, so an empty pack would not prove a quiet window"
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
    // The override restores what this gate withheld, nothing more. When no
    // record of these sessions reached it, naming the override as the remedy
    // would send the operator to an empty pack or a second refusal.
    let remedy = if withheld == 0 {
        "no record of theirs reached this gate (upstream filters or the classifier left none), so --distill-mixed (MCP: distill_mixed) would add nothing".to_string()
    } else {
        format!("pass --distill-mixed (MCP: distill_mixed) to distill their {withheld} record(s)")
    };
    let context = format!(
        "continuity: {} mixed-workstream session(s) in the window and nothing homogeneous to distill; {remedy}",
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
fn session_human_weights(selection: &[intents::SourceSelection]) -> BTreeMap<(&str, &str), usize> {
    let mut weights: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for source in selection {
        let weight = weights
            .entry((source.agent.as_str(), source.session_id.as_str()))
            .or_insert(0);
        *weight = (*weight).saturating_add(source.human_messages);
    }
    weights
}

fn weight_of(weights: &BTreeMap<(&str, &str), usize>, agent: &str, session_id: &str) -> usize {
    weights.get(&(agent, session_id)).copied().unwrap_or(0)
}

fn collect_sources(
    selection: &[intents::SourceSelection],
    records: &[IntentRecord],
    mixed: &[intents::MixedScopeSession],
) -> Vec<SourceLine> {
    let mut sources: BTreeMap<(&str, &str), SourceLine> = BTreeMap::new();
    for selected in selection.iter().filter(|source| {
        matches!(
            source.status.as_str(),
            "qualified" | "unbounded_unknown_time"
        )
    }) {
        if mixed.iter().any(|session| {
            session.agent == selected.agent && session.session_id == selected.session_id
        }) {
            continue;
        }
        let retained: Vec<&IntentRecord> = records
            .iter()
            .filter(|record| {
                record.agent == selected.agent
                    && record.session_id == selected.session_id
                    && record.source_chunk == selected.path
            })
            .collect();
        if retained.is_empty() && selected.human_messages == 0 {
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
                human_messages: 0,
            });
        source.human_messages = source.human_messages.max(selected.human_messages);
        if latest > source.latest_activity {
            source.latest_activity = latest;
        }
    }
    let mut sources: Vec<SourceLine> = sources.into_values().collect();
    sources.sort_by(|a, b| {
        b.human_messages
            .cmp(&a.human_messages)
            .then_with(|| b.latest_activity.cmp(&a.latest_activity))
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
    out.push_str("rank: human-turn weight, then recency. A newer session with fewer user turns does not outrank a heavier one. human=N is the user-role turn count this extraction kept.\n");

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
    if let Some(open_work) = &pack.open_work {
        out.push_str(&crate::open_work::honesty_line(open_work));
    }
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
    let weights = session_human_weights(&pack.selection);
    for (agent, sessions) in per_agent.iter_mut() {
        sessions.sort_by(|(id_a, a), (id_b, b)| {
            weight_of(&weights, agent, id_b)
                .cmp(&weight_of(&weights, agent, id_a))
                .then_with(|| newest_record_time(b).cmp(&newest_record_time(a)))
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
        printed_path(&record.source_chunk),
        record.summary
    );
    format!("{}\n", line.replace('\r', "\\r").replace('\n', "\\n"))
}

/// Evidence paths are printed the way an operator types them. Windows
/// canonical paths carry a `\\?\` prefix; identity comparisons keep it, the
/// rendered pack does not.
fn printed_path(path: &str) -> String {
    aicx_parser::sanitize::strip_verbatim_prefix(Path::new(path))
        .to_string_lossy()
        .into_owned()
}

/// Keep `--for-inject` inside [`INJECT_CHAR_BUDGET`].
///
/// The newest [`NOW_INJECT_FLOOR`] NOW body lines stay whole until claim
/// lines outside that floor, OPEN WORK rows, and ADDRESSED TO rows have been
/// dropped. Addressed speech is the last unprotected content to go: OPEN WORK
/// and every other unprotected line leave before an ADDRESSED TO row, and
/// that section loses its oldest takes first. Fewer NOW lines than the floor
/// are kept whole. The first NOW body line then stays with SOURCES and INDEX
/// HEALTH. The omission marker names every section that lost a line, and
/// OPEN WORK / ADDRESSED TO also say how many rows were dropped.
pub(crate) fn bounded_inject(out: String) -> String {
    if out.chars().count() <= INJECT_CHAR_BUDGET {
        return out;
    }
    reserve_inject_tail(out)
}

struct InjectLine {
    section: String,
    line: String,
    claim: bool,
    protected: bool,
    heading: bool,
    /// One of the newest NOW body lines. Dropped only after OPEN WORK rows,
    /// ADDRESSED TO rows, and the other claim lines are gone.
    now_floor: bool,
}

fn inject_section_key(heading: &str) -> String {
    for name in [
        "INDEX HEALTH",
        "DECISIONS",
        "SOURCES",
        "HONESTY",
        "PEERS",
        "TASKS",
        "NOW",
    ] {
        if heading.starts_with(name) {
            return name.to_string();
        }
    }
    heading.to_string()
}

fn inject_omission_marker(shortened: &[String]) -> String {
    if shortened.is_empty() {
        "\n[continuity pack truncated at inject budget; content omitted; claim lines retained whole]\n"
            .to_string()
    } else {
        format!(
            "\n[continuity pack truncated at inject budget; shortened: {}; claim lines retained whole]\n",
            shortened.join(", ")
        )
    }
}

fn addressed_section(section: &str) -> bool {
    section.starts_with("ADDRESSED TO")
}

fn counts_dropped_rows(section: &str) -> bool {
    section == "OPEN WORK" || addressed_section(section)
}

fn split_inject_lines(out: &str) -> Vec<InjectLine> {
    let mut lines = Vec::new();
    let mut section = String::new();
    let mut claim = false;
    let mut protected = false;
    let mut now_floor_used = 0usize;
    for line in out.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let heading = body.starts_with("## ");
        if heading {
            section = inject_section_key(body.trim_start_matches("## "));
            claim = matches!(section.as_str(), "NOW" | "PEERS" | "DECISIONS" | "TASKS");
            protected = matches!(section.as_str(), "SOURCES" | "INDEX HEALTH");
        }
        let now_floor = section == "NOW"
            && !heading
            && !body.trim().is_empty()
            && now_floor_used < NOW_INJECT_FLOOR;
        if now_floor {
            now_floor_used += 1;
        }
        // The single newest NOW line stays with SOURCES and INDEX HEALTH
        // once the floor itself has to give lines back.
        let protect_newest_now = now_floor && now_floor_used == 1;
        let honesty_head = (section.is_empty() && body.starts_with("# "))
            || (section == "HONESTY"
                && HONESTY_INJECT_HEAD
                    .iter()
                    .any(|prefix| body.starts_with(prefix)));
        lines.push(InjectLine {
            section: section.clone(),
            line: line.to_string(),
            claim,
            protected: protected || protect_newest_now || honesty_head,
            heading,
            now_floor,
        });
    }
    lines
}

fn shortened_inject_sections(lines: &[InjectLine], keep: &[bool]) -> Vec<String> {
    let mut names = Vec::new();
    let mut row_drops: BTreeMap<String, usize> = BTreeMap::new();
    for (piece, on) in lines.iter().zip(keep.iter()) {
        if *on || piece.section.is_empty() || piece.line.trim().is_empty() {
            continue;
        }
        if !names.contains(&piece.section) {
            names.push(piece.section.clone());
        }
        if !piece.heading && counts_dropped_rows(&piece.section) {
            *row_drops.entry(piece.section.clone()).or_default() += 1;
        }
    }
    names
        .into_iter()
        .map(
            |name| match row_drops.get(&name).copied().filter(|count| *count > 0) {
                Some(count) => format!(
                    "{name} ({count} {})",
                    if count == 1 { "row" } else { "rows" }
                ),
                None => name,
            },
        )
        .collect()
}

fn kept_inject_chars(lines: &[InjectLine], keep: &[bool]) -> usize {
    lines
        .iter()
        .zip(keep.iter())
        .filter(|(_, on)| **on)
        .map(|(piece, _)| piece.line.chars().count())
        .sum()
}

fn last_inject_line(
    lines: &[InjectLine],
    keep: &[bool],
    pred: impl Fn(&InjectLine) -> bool,
) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .rev()
        .find(|(index, piece)| keep[*index] && pred(piece))
        .map(|(index, _)| index)
}

/// Claim lines outside the NOW floor go first, then OPEN WORK rows from the
/// tail, then the remaining unprotected lines. ADDRESSED TO rows go next,
/// from the tail (the oldest take first), before the floor gives a line back.
/// One body line of each protected section (pack title, HONESTY head, newest
/// NOW, SOURCES, INDEX HEALTH) stays until nothing else can go.
fn next_inject_drop(lines: &[InjectLine], keep: &[bool]) -> Option<usize> {
    if let Some(index) = last_inject_line(lines, keep, |piece| {
        piece.claim && !piece.protected && !piece.now_floor && !piece.heading
    }) {
        return Some(index);
    }
    if let Some(index) = last_inject_line(lines, keep, |piece| {
        piece.section == "OPEN WORK" && !piece.heading
    }) {
        return Some(index);
    }
    if let Some(index) = last_inject_line(lines, keep, |piece| {
        !piece.protected && !piece.now_floor && !piece.heading && !addressed_section(&piece.section)
    }) {
        return Some(index);
    }
    if let Some(index) = last_inject_line(lines, keep, |piece| {
        addressed_section(&piece.section) && !piece.heading
    }) {
        return Some(index);
    }
    if let Some(index) = last_inject_line(lines, keep, |piece| piece.now_floor && !piece.protected)
    {
        return Some(index);
    }
    if let Some(index) = last_extra_protected_line(lines, keep) {
        return Some(index);
    }
    last_inject_line(lines, keep, |_| true)
}

fn last_extra_protected_line(lines: &[InjectLine], keep: &[bool]) -> Option<usize> {
    let mut body_count: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, piece) in lines.iter().enumerate() {
        if keep[index] && piece.protected && !piece.heading && !piece.line.trim().is_empty() {
            *body_count.entry(piece.section.as_str()).or_default() += 1;
        }
    }
    lines
        .iter()
        .enumerate()
        .rev()
        .find(|(index, piece)| {
            if !keep[*index] || !piece.protected || piece.heading {
                return false;
            }
            piece.line.trim().is_empty()
                || body_count.get(piece.section.as_str()).copied().unwrap_or(0) > 1
        })
        .map(|(index, _)| index)
}

fn reserve_inject_tail(out: String) -> String {
    let lines = split_inject_lines(&out);
    let mut keep = vec![true; lines.len()];
    loop {
        let shortened = shortened_inject_sections(&lines, &keep);
        let marker = inject_omission_marker(&shortened);
        if kept_inject_chars(&lines, &keep) + marker.chars().count() <= INJECT_CHAR_BUDGET {
            let mut bounded = String::new();
            for (piece, on) in lines.iter().zip(keep.iter()) {
                if *on {
                    bounded.push_str(&piece.line);
                }
            }
            bounded.push_str(&marker);
            return bounded;
        }
        match next_inject_drop(&lines, &keep) {
            Some(index) => keep[index] = false,
            None => {
                return inject_omission_marker(&[])
                    .chars()
                    .take(INJECT_CHAR_BUDGET)
                    .collect();
            }
        }
    }
}

/// Take identity wins over timestamp/text; idless legacy copies use both.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum AddressedTakeKey {
    Take(String),
    Legacy {
        spoken: bool,
        at: DateTime<Utc>,
        text: String,
    },
}

/// Render optional addressed speech without changing the project-scoped pack.
/// The bus is read live, including projects outside the narrative's filter.
/// Unreadable ledgers fail the request; replay omissions remain explicit.
pub fn render_as(
    pack: &ContinuityPack,
    for_inject: bool,
    addressee: Option<&str>,
) -> Result<String> {
    let Some(name) = addressee else {
        return Ok(render(pack, for_inject));
    };
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "addressed name must not be empty");
    let normalized = crate::sanitize::normalize_query(name);
    anyhow::ensure!(!normalized.is_empty(), "addressed name must contain a name");
    let user_home = crate::os_user_home()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve operator home for addressed speech"))?;
    let now = DateTime::parse_from_rfc3339(&pack.window_end)?.with_timezone(&Utc);
    let cutoff = intents::window_cutoff(now, pack.hours);
    let mut messages: BTreeMap<
        AddressedTakeKey,
        (
            Vec<std::path::PathBuf>,
            crate::importers::codescribe_bus::BusMessage,
        ),
    > = BTreeMap::new();
    let mut skipped = 0;
    let mut open = 0;
    let ledgers = crate::importers::codescribe_bus::discover_bus_ledgers(&user_home);
    for ledger in &ledgers {
        let replay = crate::importers::codescribe_bus::read_bus_ledger(ledger)?;
        skipped += replay.skipped_records();
        open += replay.open_takes;
        for message in replay.messages {
            if message.kind.is_human()
                && message.at >= cutoff
                && message.at <= now
                && (message.recipients.iter().any(|party| {
                    party
                        .name
                        .as_deref()
                        .is_some_and(|name| crate::sanitize::normalize_query(name) == normalized)
                }) || message.audience.as_deref().is_some_and(|audience| {
                    crate::sanitize::normalize_query(audience) == normalized
                }))
            {
                use crate::importers::codescribe_bus::BusMessageKind;
                let key = message.take_id.as_ref().map_or_else(
                    || AddressedTakeKey::Legacy {
                        spoken: matches!(message.kind, BusMessageKind::Spoken { .. }),
                        at: message.at,
                        text: message
                            .text
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" "),
                    },
                    |id| AddressedTakeKey::Take(id.clone()),
                );
                match messages.entry(key) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert((vec![ledger.clone()], message));
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let (paths, kept) = entry.get_mut();
                        if !paths.contains(ledger) {
                            paths.push(ledger.clone());
                        }
                        kept.at = kept.at.min(message.at);
                        kept.recipients.extend(message.recipients);
                        kept.recipients.sort();
                        kept.recipients.dedup();
                        // A certified copy does not erase a refused delivery copy.
                        if matches!(message.kind, BusMessageKind::Spoken { certified: false }) {
                            kept.kind = message.kind;
                        }
                    }
                }
            }
        }
    }
    let mut messages: Vec<_> = messages.into_values().collect();
    messages.sort_by(|(path_a, a), (path_b, b)| {
        b.at.cmp(&a.at)
            .then_with(|| path_a.cmp(path_b))
            .then_with(|| a.text.cmp(&b.text))
            .then_with(|| a.recipients.cmp(&b.recipients))
    });
    let mut section = format!(
        "## ADDRESSED TO {}\n\n",
        name.to_uppercase().replace(['\r', '\n'], " ")
    );
    section.push_str("- cross-project bus speech; structured recipients/audience only; dictation has no addressee and is excluded\n");
    if skipped > 0 || open > 0 {
        section.push_str(&format!(
            "- bus coverage: {skipped} skipped records; {open} open takes withheld (all ledgers, not window-scoped)\n"
        ));
    }
    if messages.is_empty() {
        section.push_str("- no addressed bus messages inside the window; absence is not proof of no assignment\n");
    }
    for (ledgers, message) in messages.iter().take(SOURCE_CAP) {
        use crate::importers::codescribe_bus::BusMessageKind;
        let kind = match message.kind {
            BusMessageKind::Spoken { certified: true } => "voice",
            BusMessageKind::Spoken { certified: false } => "voice, coverage refused",
            BusMessageKind::Typed => "typed",
            BusMessageKind::AgentReply => unreachable!("human messages only"),
        };
        let recipients = message
            .recipients
            .iter()
            .map(|party| format!("{}/{}", party.provider, party.session))
            .collect::<Vec<_>>()
            .join(", ");
        section.push_str(&format!(
            "- [{}] {} · {} · recipients={} · source={}\n",
            kind,
            message.text.replace('\r', "\\r").replace('\n', "\\n"),
            message.at.to_rfc3339(),
            recipients,
            ledgers
                .iter()
                .map(|ledger| printed_path(&ledger.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if messages.len() > SOURCE_CAP {
        section.push_str(&format!(
            "- {} addressed messages omitted at section cap\n",
            messages.len() - SOURCE_CAP
        ));
    }
    section.push('\n');
    Ok(render_with_addressed(pack, for_inject, &section))
}

/// Render the pack. `for_inject` bounds complete claim lines in Unicode chars.
pub fn render(pack: &ContinuityPack, for_inject: bool) -> String {
    render_with_addressed(pack, for_inject, "")
}

/// Same lower bound as the HONESTY window line. An unreadable end does not
/// fold: there is no window to measure a branch against.
fn open_work_window_start(pack: &ContinuityPack) -> Option<DateTime<Utc>> {
    let end = DateTime::parse_from_rfc3339(&pack.window_end)
        .ok()
        .map(|time| time.with_timezone(&Utc))?;
    Some(intents::window_cutoff(end, pack.hours))
}

fn render_with_addressed(pack: &ContinuityPack, for_inject: bool, addressed: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# CONTINUITY · {} · {}h\n\n",
        pack.project_label, pack.hours
    ));
    push_honesty_preface(&mut out, pack);

    out.push_str("## NOW\n\n");
    let weights = session_human_weights(&pack.selection);
    let mut requests = unresolved_intents(&pack.records);
    requests.extend(
        pack.records
            .iter()
            .filter(|record| human_record(record) && record.kind == IntentKind::Decision),
    );
    requests.sort_by(|a, b| {
        weight_of(&weights, b.agent.as_str(), b.session_id.as_str())
            .cmp(&weight_of(
                &weights,
                a.agent.as_str(),
                a.session_id.as_str(),
            ))
            .then_with(|| human_record(b).cmp(&human_record(a)))
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
    open_sessions.sort_by(|(id_a, a), (id_b, b)| {
        weight_of(&weights, id_b.0, id_b.1)
            .cmp(&weight_of(&weights, id_a.0, id_a.1))
            .then_with(|| b.cmp(a))
            .then_with(|| id_a.cmp(id_b))
    });
    if open_sessions.is_empty() {
        out.push_str("- no retained open-session records inside the window; thread absence is not established\n");
    }
    for ((agent, session), timestamp) in open_sessions.into_iter().take(NOW_CAP) {
        out.push_str(&format!(
            "- open: {agent} · {session} · {} · human={}\n",
            timestamp
                .map(|time| time.to_rfc3339())
                .as_deref()
                .unwrap_or("unknown"),
            weight_of(&weights, agent, session)
        ));
    }
    if !pack.mixed_scope.is_empty() {
        // Distilling on request admits a session; it does not restore records
        // that never reached the gate. Say how many actually contributed.
        let contributed = pack
            .mixed_scope
            .iter()
            .filter(|session| {
                pack.records.iter().any(|record| {
                    record.agent == session.agent && record.session_id == session.session_id
                })
            })
            .count();
        out.push_str(&format!(
            "- mixed-workstream candidates: {} session(s) {} (scope_status=mixed_candidate)\n",
            pack.mixed_scope.len(),
            if pack.distilled_mixed {
                format!("distilled on request; {contributed} with retained records")
            } else {
                "NOT distilled into this pack".to_string()
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
            weight_of(&weights, agent, id_b)
                .cmp(&weight_of(&weights, agent, id_a))
                .then_with(|| newest_record_time(b).cmp(&newest_record_time(a)))
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
            out.push_str(&format!(
                "- {session}{marker} · human={} · {timestamp}\n",
                weight_of(&weights, agent, session)
            ));
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
        weight_of(&weights, b.agent.as_str(), b.session_id.as_str())
            .cmp(&weight_of(
                &weights,
                a.agent.as_str(),
                a.session_id.as_str(),
            ))
            .then_with(|| human_record(b).cmp(&human_record(a)))
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
        weight_of(&weights, b.agent.as_str(), b.session_id.as_str())
            .cmp(&weight_of(
                &weights,
                a.agent.as_str(),
                a.session_id.as_str(),
            ))
            .then_with(|| human_record(b).cmp(&human_record(a)))
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

    if let Some(open_work) = &pack.open_work {
        // The pack stays as collected. Inject mode only decides which rows
        // the section spells out; the window start is the cutoff HONESTY
        // already prints.
        let inject_not_before = if for_inject {
            open_work_window_start(pack)
        } else {
            None
        };
        out.push_str(&crate::open_work::render_section(
            open_work,
            inject_not_before,
        ));
    }
    out.push_str(addressed);
    out.push_str("## SOURCES\n\n");
    if pack.sources.is_empty() {
        out.push_str(
            "- no source paths represented by retained records; see qualification coverage above\n",
        );
    }
    for source in pack.sources.iter().take(SOURCE_CAP) {
        out.push_str(&format!(
            "- {} · human={} · latest_retained_claim_activity={} · {}{}\n",
            source.agent,
            source.human_messages,
            source.latest_activity.as_deref().unwrap_or("unknown"),
            printed_path(&source.path),
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

fn normalize_targets(targets: &[String]) -> Result<Vec<String>> {
    let mut cleaned = Vec::new();
    for raw in targets {
        for piece in raw.split(',') {
            let trimmed = piece.trim();
            anyhow::ensure!(!trimmed.is_empty(), "addressed target must not be empty");
            let normalized = crate::sanitize::normalize_query(trimmed);
            anyhow::ensure!(
                !normalized.is_empty(),
                "addressed target must contain an identifier or name"
            );
            if !cleaned
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(trimmed))
            {
                cleaned.push(trimmed.to_string());
            }
        }
    }
    Ok(cleaned)
}

/// Match a session `(agent, session_id)` against a `--as <target>` selector:
/// - `<agent>/<session_id_or_prefix>` (e.g. `agy/79c9a5ab`)
/// - `<session_id>` or prefix of length >= 4 (e.g. `79c9a5ab` or full UUID)
/// - `<agent>` provider name (e.g. `agy`, `antigravity`, `claude`, `codex`, `gemini`)
pub fn session_matches_target(agent: &str, session_id: &str, raw_target: &str) -> bool {
    let target = raw_target.trim().to_ascii_lowercase();
    if target.is_empty() {
        return false;
    }
    let agent_lc = agent.trim().to_ascii_lowercase();
    let sid_lc = session_id.trim().to_ascii_lowercase();
    let provider_matches = |candidate: &str| {
        candidate == agent_lc
            || (matches!(candidate, "antigravity" | "antigravity-cli" | "agy")
                && matches!(agent_lc.as_str(), "agy" | "antigravity" | "antigravity-cli"))
    };
    if provider_matches(&target) {
        return true;
    }
    if let Some((provider, sid_prefix)) = target.split_once('/') {
        let provider = provider.trim();
        let sid_prefix = sid_prefix.trim();
        if (provider == "*" || provider_matches(provider))
            && !sid_prefix.is_empty()
            && (sid_lc == sid_prefix || (sid_prefix.len() >= 4 && sid_lc.starts_with(sid_prefix)))
        {
            return true;
        }
    }
    sid_lc == target || (target.len() >= 4 && sid_lc.starts_with(&target))
}

pub fn session_matches_any_target(agent: &str, session_id: &str, targets: &[String]) -> bool {
    targets
        .iter()
        .any(|target| session_matches_target(agent, session_id, target))
}

fn pack_has_matching_session(pack: &ContinuityPack, targets: &[String]) -> bool {
    if targets.is_empty() {
        return false;
    }
    pack.selection
        .iter()
        .any(|source| session_matches_any_target(&source.agent, &source.session_id, targets))
        || pack
            .records
            .iter()
            .any(|record| session_matches_any_target(&record.agent, &record.session_id, targets))
}

fn bus_message_matches_target(
    message: &crate::importers::codescribe_bus::BusMessage,
    raw_target: &str,
) -> bool {
    let normalized = crate::sanitize::normalize_query(raw_target);
    if normalized.is_empty() {
        return false;
    }
    let target_lc = raw_target.trim().to_ascii_lowercase();
    if message.recipients.iter().any(|party| {
        if party
            .name
            .as_deref()
            .is_some_and(|name| crate::sanitize::normalize_query(name) == normalized)
        {
            return true;
        }
        if party.session != "*"
            && session_matches_target(&party.provider, &party.session, raw_target)
        {
            return true;
        }
        party.provider != "broadcast"
            && (party.provider.eq_ignore_ascii_case(&target_lc)
                || (matches!(target_lc.as_str(), "agy" | "antigravity")
                    && matches!(
                        party.provider.to_ascii_lowercase().as_str(),
                        "agy" | "antigravity" | "gemini"
                    )))
    }) {
        return true;
    }
    message
        .audience
        .as_deref()
        .is_some_and(|audience| crate::sanitize::normalize_query(audience) == normalized)
}

fn render_addressed_section_for_targets(
    pack: &ContinuityPack,
    targets: &[String],
    seen_bus_lines: Option<&std::collections::BTreeSet<String>>,
) -> Result<(String, usize)> {
    if targets.is_empty() {
        return Ok((String::new(), 0));
    }
    let user_home = crate::os_user_home()
        .ok_or_else(|| anyhow::anyhow!("cannot resolve operator home for addressed speech"))?;
    let now = DateTime::parse_from_rfc3339(&pack.window_end)?.with_timezone(&Utc);
    let cutoff = intents::window_cutoff(now, pack.hours);
    let mut messages: BTreeMap<
        AddressedTakeKey,
        (
            Vec<std::path::PathBuf>,
            crate::importers::codescribe_bus::BusMessage,
        ),
    > = BTreeMap::new();
    let mut skipped = 0;
    let mut open = 0;
    let ledgers = crate::importers::codescribe_bus::discover_bus_ledgers(&user_home);
    for ledger in &ledgers {
        let replay = crate::importers::codescribe_bus::read_bus_ledger(ledger)?;
        skipped += replay.skipped_records();
        open += replay.open_takes;
        for message in replay.messages {
            if message.kind.is_human()
                && message.at >= cutoff
                && message.at <= now
                && targets
                    .iter()
                    .any(|target| bus_message_matches_target(&message, target))
            {
                use crate::importers::codescribe_bus::BusMessageKind;
                let key = message.take_id.as_ref().map_or_else(
                    || AddressedTakeKey::Legacy {
                        spoken: matches!(message.kind, BusMessageKind::Spoken { .. }),
                        at: message.at,
                        text: message
                            .text
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" "),
                    },
                    |id| AddressedTakeKey::Take(id.clone()),
                );
                match messages.entry(key) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert((vec![ledger.clone()], message));
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let (paths, kept) = entry.get_mut();
                        if !paths.contains(ledger) {
                            paths.push(ledger.clone());
                        }
                        kept.at = kept.at.min(message.at);
                        kept.recipients.extend(message.recipients);
                        kept.recipients.sort();
                        kept.recipients.dedup();
                        if matches!(message.kind, BusMessageKind::Spoken { certified: false }) {
                            kept.kind = message.kind;
                        }
                    }
                }
            }
        }
    }
    let mut messages: Vec<_> = messages.into_values().collect();
    messages.sort_by(|(path_a, a), (path_b, b)| {
        b.at.cmp(&a.at)
            .then_with(|| path_a.cmp(path_b))
            .then_with(|| a.text.cmp(&b.text))
            .then_with(|| a.recipients.cmp(&b.recipients))
    });
    let mut deduped_bus = 0usize;
    if let Some(seen) = seen_bus_lines {
        messages.retain(|(_, msg)| {
            let fingerprint = bus_message_fingerprint(&msg.text, &msg.at.to_rfc3339());
            if seen.contains(&fingerprint) {
                deduped_bus += 1;
                false
            } else {
                true
            }
        });
    }
    let heading_label = targets
        .iter()
        .map(|target| target.to_uppercase().replace(['\r', '\n'], " "))
        .collect::<Vec<_>>()
        .join(", ");
    let mut section = format!("## ADDRESSED TO {heading_label}\n\n");
    section.push_str("- cross-project bus speech; structured recipients/audience/session targets; dictation has no addressee and is excluded\n");
    if skipped > 0 || open > 0 {
        section.push_str(&format!(
            "- bus coverage: {skipped} skipped records; {open} open takes withheld (all ledgers, not window-scoped)\n"
        ));
    }
    if deduped_bus > 0 {
        section.push_str(&format!(
            "- deduplicated_prior_bus_messages: {deduped_bus}\n"
        ));
    }
    if messages.is_empty() {
        section.push_str("- no addressed bus messages inside the window; absence is not proof of no assignment\n");
    }
    for (ledgers, message) in messages.iter().take(SOURCE_CAP) {
        use crate::importers::codescribe_bus::BusMessageKind;
        let kind = match message.kind {
            BusMessageKind::Spoken { certified: true } => "voice",
            BusMessageKind::Spoken { certified: false } => "voice, coverage refused",
            BusMessageKind::Typed => "typed",
            BusMessageKind::AgentReply => unreachable!("human messages only"),
        };
        let recipients = message
            .recipients
            .iter()
            .map(|party| format!("{}/{}", party.provider, party.session))
            .collect::<Vec<_>>()
            .join(", ");
        section.push_str(&format!(
            "- [{}] {} · {} · recipients={} · source={}\n",
            kind,
            message.text.replace('\r', "\\r").replace('\n', "\\n"),
            message.at.to_rfc3339(),
            recipients,
            ledgers
                .iter()
                .map(|ledger| printed_path(&ledger.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    if messages.len() > SOURCE_CAP {
        section.push_str(&format!(
            "- {} addressed messages omitted at section cap\n",
            messages.len() - SOURCE_CAP
        ));
    }
    section.push('\n');
    Ok((section, deduped_bus))
}

/// Build a pack copy whose `selection`, `records`, and `sources` prioritize
/// sessions matching `targets` when session targets (`--as <session_id|agent>`)
/// are specified.
fn prioritize_pack_for_targets(pack: &ContinuityPack, targets: &[String]) -> ContinuityPack {
    let max_weight = pack
        .selection
        .iter()
        .map(|s| s.human_messages)
        .max()
        .unwrap_or(0)
        .saturating_add(10_000);
    let selection = pack
        .selection
        .iter()
        .map(|s| {
            let mut cloned = s.clone();
            if session_matches_any_target(&s.agent, &s.session_id, targets) {
                cloned.human_messages = cloned.human_messages.saturating_add(max_weight);
            }
            cloned
        })
        .collect();
    let mut sources: Vec<SourceLine> = pack
        .sources
        .iter()
        .map(|s| SourceLine {
            agent: s.agent.clone(),
            path: s.path.clone(),
            latest_activity: s.latest_activity.clone(),
            live: s.live,
            human_messages: s.human_messages,
        })
        .collect();
    sources.sort_by(|a, b| {
        let a_match = pack.selection.iter().any(|sel| {
            sel.path == a.path && session_matches_any_target(&sel.agent, &sel.session_id, targets)
        });
        let b_match = pack.selection.iter().any(|sel| {
            sel.path == b.path && session_matches_any_target(&sel.agent, &sel.session_id, targets)
        });
        b_match
            .cmp(&a_match)
            .then_with(|| b.human_messages.cmp(&a.human_messages))
            .then_with(|| b.latest_activity.cmp(&a.latest_activity))
            .then_with(|| a.path.cmp(&b.path))
    });
    ContinuityPack {
        selection,
        source_errors: pack.source_errors,
        dropped_task_events: pack.dropped_task_events,
        window_end: pack.window_end.clone(),
        project_label: pack.project_label.clone(),
        hours: pack.hours,
        live_sessions: pack.live_sessions,
        records: pack.records.clone(),
        sources,
        index_health: IndexHealthLine {
            newest_session_updated_at: pack.index_health.newest_session_updated_at.clone(),
            committed_at: pack.index_health.committed_at.clone(),
            pending: pack.index_health.pending,
            sessions_newer_than_chunks: pack.index_health.sessions_newer_than_chunks,
            readiness: pack.index_health.readiness.clone(),
            mode: pack.index_health.mode,
        },
        mixed_scope: pack.mixed_scope.clone(),
        distilled_mixed: pack.distilled_mixed,
        candidate_cap: pack.candidate_cap,
        dropped_candidates: pack.dropped_candidates,
        unplaced_scope: pack.unplaced_scope.clone(),
        open_work: pack.open_work.clone(),
    }
}

fn render_handoff_or_summary(
    pack: &ContinuityPack,
    mode: ContinuityMode,
    for_inject: bool,
    targets: &[String],
    written_at: DateTime<Utc>,
    addressed_section: &str,
) -> String {
    let is_summary = mode == ContinuityMode::Summary;
    let heading_kind = if is_summary { "SUMMARY" } else { "HANDOFF" };
    let end = DateTime::parse_from_rfc3339(&pack.window_end)
        .ok()
        .map(|time| time.with_timezone(&Utc));
    let cutoff = end.map(|time| intents::window_cutoff(time, pack.hours));
    let mut out = String::new();
    out.push_str(&format!(
        "# CONTINUITY {heading_kind} · {} · {}h\n\n",
        pack.project_label, pack.hours
    ));
    out.push_str("## METADATA & TIMESTAMPS\n\n");
    out.push_str(&format!("- written_at: {}\n", written_at.to_rfc3339()));
    out.push_str(&format!(
        "- event_window: [{}, {}] UTC\n",
        cutoff
            .map(|time| time.to_rfc3339())
            .as_deref()
            .unwrap_or("unknown"),
        end.map(|time| time.to_rfc3339())
            .as_deref()
            .unwrap_or("unknown")
    ));
    out.push_str(&format!(
        "- mode: {}\n",
        if is_summary { "summary" } else { "handoff" }
    ));
    out.push_str(&format!(
        "- targets: {}\n",
        if targets.is_empty() {
            "all project sessions".to_string()
        } else {
            targets.join(", ")
        }
    ));
    let qualified = pack
        .selection
        .iter()
        .filter(|source| source.status == "qualified")
        .count();
    out.push_str(&format!(
        "- coverage: considered_sources={} · qualified_sources={qualified} · live_sessions={} · retained_records={} · source_errors={}\n\n",
        pack.selection.len(),
        pack.live_sessions,
        pack.records.len(),
        pack.source_errors
    ));

    let weights = session_human_weights(&pack.selection);
    out.push_str("## FOCUS SESSIONS & ACTIVE STATE\n\n");
    let mut session_groups: BTreeMap<(&str, &str), Vec<&IntentRecord>> = BTreeMap::new();
    for record in &pack.records {
        session_groups
            .entry((record.agent.as_str(), record.session_id.as_str()))
            .or_default()
            .push(record);
    }
    for source in &pack.selection {
        if source.status == "qualified" && source.human_messages > 0 {
            session_groups
                .entry((source.agent.as_str(), source.session_id.as_str()))
                .or_default();
        }
    }
    let mut ordered_sessions: Vec<_> = session_groups.into_iter().collect();
    ordered_sessions.sort_by(|((agent_a, sid_a), recs_a), ((agent_b, sid_b), recs_b)| {
        let target_a = session_matches_any_target(agent_a, sid_a, targets);
        let target_b = session_matches_any_target(agent_b, sid_b, targets);
        target_b
            .cmp(&target_a)
            .then_with(|| newest_record_time(recs_b).cmp(&newest_record_time(recs_a)))
            .then_with(|| {
                weight_of(&weights, agent_b, sid_b).cmp(&weight_of(&weights, agent_a, sid_a))
            })
            .then_with(|| agent_a.cmp(agent_b))
            .then_with(|| sid_a.cmp(sid_b))
    });
    let session_cap = if is_summary { 5 } else { 10 };
    if ordered_sessions.is_empty() {
        out.push_str("- no active sessions with retained records inside the window\n");
    }
    for ((agent, session_id), records) in ordered_sessions.iter().take(session_cap) {
        let is_target = session_matches_any_target(agent, session_id, targets);
        let is_open = records.iter().any(|record| record.honesty.is_live_open());
        let latest_event = newest_record_time(records)
            .map(|time| time.to_rfc3339())
            .unwrap_or_else(|| "unknown".to_string());
        let mut tags = Vec::new();
        if is_target {
            tags.push("target");
        }
        if is_open {
            tags.push("open");
        }
        let tag_suffix = if tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tags.join(", "))
        };
        out.push_str(&format!(
            "- {agent} · {session_id}{tag_suffix} · human={} · latest_event_time={latest_event} · records={}\n",
            weight_of(&weights, agent, session_id),
            records.len()
        ));
    }
    out.push('\n');

    out.push_str("## LATEST HUMAN INTENTIONS & DECISIONS\n\n");
    let mut human_claims: Vec<&IntentRecord> = pack
        .records
        .iter()
        .filter(|record| {
            human_record(record) && matches!(record.kind, IntentKind::Intent | IntentKind::Decision)
        })
        .collect();
    human_claims.sort_by(|a, b| {
        let target_a = session_matches_any_target(&a.agent, &a.session_id, targets);
        let target_b = session_matches_any_target(&b.agent, &b.session_id, targets);
        target_b
            .cmp(&target_a)
            .then_with(|| record_time(b).cmp(&record_time(a)))
            .then_with(|| {
                weight_of(&weights, b.agent.as_str(), b.session_id.as_str()).cmp(&weight_of(
                    &weights,
                    a.agent.as_str(),
                    a.session_id.as_str(),
                ))
            })
            .then_with(|| a.source_chunk.cmp(&b.source_chunk))
    });
    let human_cap = if is_summary { 6 } else { 15 };
    if human_claims.is_empty() {
        out.push_str("- no human intention or decision candidates retained inside the window\n");
    }
    for record in human_claims.into_iter().take(human_cap) {
        let label = if record.kind == IntentKind::Intent {
            "human intent candidate"
        } else {
            "human decision candidate"
        };
        out.push_str(&claim_line(record, label, ""));
    }
    out.push('\n');

    out.push_str("## PEER & SESSION OUTCOMES\n\n");
    let per_session_claim_cap = if is_summary { 2 } else { 5 };
    if ordered_sessions.is_empty() {
        out.push_str("- no session outcomes retained inside the window\n");
    }
    for ((agent, session_id), records) in ordered_sessions.into_iter().take(session_cap) {
        if records.is_empty() {
            continue;
        }
        let is_target = session_matches_any_target(agent, session_id, targets);
        let latest_event = newest_record_time(&records)
            .map(|time| time.to_rfc3339())
            .unwrap_or_else(|| "unknown".to_string());
        out.push_str(&format!(
            "### {agent} · {session_id}{} · latest_event_time={latest_event}\n",
            if is_target { " [target]" } else { "" }
        ));
        let mut sorted = records;
        sorted.sort_by(|a, b| {
            record_time(b)
                .cmp(&record_time(a))
                .then_with(|| human_record(b).cmp(&human_record(a)))
                .then_with(|| a.source_chunk.cmp(&b.source_chunk))
        });
        for record in sorted.into_iter().take(per_session_claim_cap) {
            out.push_str(&claim_line(
                record,
                &format!("{} candidate", record.kind.heading().to_lowercase()),
                "  ",
            ));
        }
    }
    out.push('\n');

    if !is_summary && let Some(open_work) = &pack.open_work {
        let inject_not_before = if for_inject {
            open_work_window_start(pack)
        } else {
            None
        };
        out.push_str(&crate::open_work::render_section(
            open_work,
            inject_not_before,
        ));
    }
    out.push_str(addressed_section);

    out.push_str("## SOURCES\n\n");
    let source_cap = if is_summary { 8 } else { SOURCE_CAP };
    if pack.sources.is_empty() {
        out.push_str("- no source paths represented by retained records; see coverage above\n");
    }
    for source in pack.sources.iter().take(source_cap) {
        out.push_str(&format!(
            "- {} · human={} · latest_retained_claim_activity={} · {}{}\n",
            source.agent,
            source.human_messages,
            source.latest_activity.as_deref().unwrap_or("unknown"),
            printed_path(&source.path),
            if source.live { " [unadmitted]" } else { "" }
        ));
    }
    if for_inject { bounded_inject(out) } else { out }
}

/// Render a continuity pack according to `mode` (`pack`, `inject`, `handoff`,
/// `summary`) and optional multi-target `--as` selectors (bus addressee names,
/// `<session_id>` prefixes, `<agent>/<session_id>`, or agent names).
pub fn render_for_targets(
    pack: &ContinuityPack,
    mode: ContinuityMode,
    for_inject: bool,
    targets: &[String],
    written_at: DateTime<Utc>,
) -> Result<String> {
    let cleaned = normalize_targets(targets)?;
    let effective_inject = for_inject || mode == ContinuityMode::Inject;
    let has_session_match = pack_has_matching_session(pack, &cleaned);

    if matches!(mode, ContinuityMode::Pack | ContinuityMode::Inject) {
        if cleaned.is_empty() {
            return Ok(render(pack, effective_inject));
        }
        if cleaned.len() == 1 && !has_session_match {
            return render_as(pack, effective_inject, Some(&cleaned[0]));
        }
        let effective_pack = if has_session_match {
            prioritize_pack_for_targets(pack, &cleaned)
        } else {
            prioritize_pack_for_targets(pack, &[])
        };
        let (addressed, _) = render_addressed_section_for_targets(&effective_pack, &cleaned, None)?;
        return Ok(render_with_addressed(
            &effective_pack,
            effective_inject,
            &addressed,
        ));
    }

    let effective_pack = if has_session_match {
        prioritize_pack_for_targets(pack, &cleaned)
    } else {
        prioritize_pack_for_targets(pack, &[])
    };
    let (addressed, _) = render_addressed_section_for_targets(&effective_pack, &cleaned, None)?;
    Ok(render_handoff_or_summary(
        &effective_pack,
        mode,
        effective_inject,
        &cleaned,
        written_at,
        &addressed,
    ))
}

fn record_dedup_fingerprint(record: &IntentRecord) -> String {
    let locator = record
        .provenance
        .as_ref()
        .map(|p| p.locator.as_str())
        .unwrap_or(record.source_chunk.as_str());
    let summary = record
        .summary
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .trim()
        .to_string();
    format!(
        "[{} · {}]|locator={}|{}",
        record.agent, record.session_id, locator, summary
    )
}

fn bus_message_fingerprint(text: &str, timestamp_rfc3339: &str) -> String {
    let clean_text = text.replace('\r', "\\r").replace('\n', "\\n");
    format!("{clean_text} · {timestamp_rfc3339}")
}

fn extract_existing_fingerprints(
    existing: &str,
) -> (
    std::collections::BTreeSet<String>,
    std::collections::BTreeSet<String>,
) {
    let mut claim_keys = std::collections::BTreeSet::new();
    let mut bus_keys = std::collections::BTreeSet::new();
    for raw_line in existing.lines() {
        let line = raw_line.trim();
        if let (Some(open_bracket), Some(close_bracket), Some(loc_pos)) =
            (line.find('['), line.find(']'), line.find("· locator="))
            && open_bracket < close_bracket
            && close_bracket < loc_pos
        {
            let bracket_part = &line[open_bracket..=close_bracket];
            let after_loc = &line[loc_pos + "· locator=".len()..];
            if let Some(loc_end) = after_loc.find(" · ") {
                let locator = &after_loc[..loc_end];
                if let Some(source_pos) = after_loc.find(" · source=") {
                    let after_source = &after_loc[source_pos + " · source=".len()..];
                    if let Some((_, summary)) = after_source.split_once(": ") {
                        claim_keys.insert(format!(
                            "{bracket_part}|locator={locator}|{}",
                            summary.trim()
                        ));
                    }
                }
            }
        }
        if (line.starts_with("- [voice]")
            || line.starts_with("- [voice, coverage refused]")
            || line.starts_with("- [typed]"))
            && let Some(close_kind) = line.find("] ")
        {
            let rest = &line[close_kind + 2..];
            if let Some(rec_pos) = rest.find(" · recipients=") {
                bus_keys.insert(rest[..rec_pos].to_string());
            }
        }
    }
    (claim_keys, bus_keys)
}

/// Write or append a continuity pack/handoff/summary to `path`.
///
/// Doctrine:
/// - `CONTINUITY.md` is NEVER overwritten when it already exists and is non-empty:
///   prior entries are preserved and a new timestamped block is appended.
/// - Every appended block carries explicit `written_at` (pack write timestamp)
///   and `event_window` + per-event `timestamp` metadata.
/// - `CONTINUITY.md` is NEVER a duplicate factory: when appending to an existing
///   file, claims and addressed bus messages already recorded in earlier blocks
///   are deduplicated so only new delta items are emitted.
pub fn write_continuity_file(
    path: &Path,
    pack: &ContinuityPack,
    mode: ContinuityMode,
    for_inject: bool,
    targets: &[String],
    written_at: DateTime<Utc>,
) -> Result<ContinuityWriteReport> {
    use std::io::Read;
    let existing = match fs::OpenOptions::new().read(true).open(path) {
        Ok(mut file) => {
            let mut content = String::new();
            if file.read_to_string(&mut content).is_ok() && !content.trim().is_empty() {
                Some(content)
            } else {
                None
            }
        }
        Err(_) => None,
    };

    let Some(existing_content) = existing else {
        let rendered = render_for_targets(pack, mode, for_inject, targets, written_at)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &rendered)?;
        return Ok(ContinuityWriteReport {
            content: rendered,
            appended: false,
            new_claims: pack.records.len(),
            deduplicated_claims: 0,
            written_at: written_at.to_rfc3339(),
        });
    };

    let cleaned = normalize_targets(targets)?;
    let (seen_claims, seen_bus) = extract_existing_fingerprints(&existing_content);
    let mut new_records = Vec::new();
    let mut deduplicated_claims = 0usize;
    for record in &pack.records {
        let key = record_dedup_fingerprint(record);
        if seen_claims.contains(&key) {
            deduplicated_claims += 1;
        } else {
            new_records.push(record.clone());
        }
    }

    let delta_sources = collect_sources(&pack.selection, &new_records, &pack.mixed_scope);
    let delta_pack = ContinuityPack {
        selection: pack.selection.clone(),
        source_errors: pack.source_errors,
        dropped_task_events: pack.dropped_task_events,
        window_end: pack.window_end.clone(),
        project_label: pack.project_label.clone(),
        hours: pack.hours,
        live_sessions: pack.live_sessions,
        records: new_records,
        sources: delta_sources,
        index_health: IndexHealthLine {
            newest_session_updated_at: pack.index_health.newest_session_updated_at.clone(),
            committed_at: pack.index_health.committed_at.clone(),
            pending: pack.index_health.pending,
            sessions_newer_than_chunks: pack.index_health.sessions_newer_than_chunks,
            readiness: pack.index_health.readiness.clone(),
            mode: pack.index_health.mode,
        },
        mixed_scope: pack.mixed_scope.clone(),
        distilled_mixed: pack.distilled_mixed,
        candidate_cap: pack.candidate_cap,
        dropped_candidates: pack.dropped_candidates,
        unplaced_scope: pack.unplaced_scope.clone(),
        open_work: pack.open_work.clone(),
    };

    let end = DateTime::parse_from_rfc3339(&pack.window_end)
        .ok()
        .map(|time| time.with_timezone(&Utc));
    let cutoff = end.map(|time| intents::window_cutoff(time, pack.hours));
    let mode_label = match mode {
        ContinuityMode::Pack => "PACK",
        ContinuityMode::Inject => "INJECT",
        ContinuityMode::Handoff => "HANDOFF",
        ContinuityMode::Summary => "SUMMARY",
    };

    let (addressed_delta, deduped_bus) =
        render_addressed_section_for_targets(&delta_pack, &cleaned, Some(&seen_bus))?;

    let mut append_block = String::new();
    if !existing_content.ends_with('\n') {
        append_block.push('\n');
    }
    append_block.push_str("\n---\n\n");
    append_block.push_str(&format!(
        "<!-- continuity-append mode={} written_at={} window_start={} window_end={} new_claims={} deduplicated_prior_claims={} deduplicated_prior_bus={} -->\n",
        mode_label.to_ascii_lowercase(),
        written_at.to_rfc3339(),
        cutoff
            .map(|time| time.to_rfc3339())
            .as_deref()
            .unwrap_or("unknown"),
        end.map(|time| time.to_rfc3339())
            .as_deref()
            .unwrap_or("unknown"),
        delta_pack.records.len(),
        deduplicated_claims,
        deduped_bus
    ));

    if delta_pack.records.is_empty()
        && (cleaned.is_empty()
            || addressed_delta.contains("- no addressed bus messages inside the window"))
    {
        append_block.push_str(&format!(
            "# CONTINUITY DELTA ({mode_label}) · {} · {}h\n\n",
            pack.project_label, pack.hours
        ));
        append_block.push_str(&format!("- written_at: {}\n", written_at.to_rfc3339()));
        append_block.push_str(&format!(
            "- event_window: [{}, {}] UTC\n",
            cutoff
                .map(|time| time.to_rfc3339())
                .as_deref()
                .unwrap_or("unknown"),
            end.map(|time| time.to_rfc3339())
                .as_deref()
                .unwrap_or("unknown")
        ));
        append_block.push_str(&format!(
            "- delta_status: no new claims or addressed bus takes since prior append (deduplicated_prior_claims={deduplicated_claims}, deduplicated_prior_bus={deduped_bus})\n"
        ));
    } else if matches!(mode, ContinuityMode::Handoff | ContinuityMode::Summary) {
        let effective_pack = if pack_has_matching_session(&delta_pack, &cleaned) {
            prioritize_pack_for_targets(&delta_pack, &cleaned)
        } else {
            prioritize_pack_for_targets(&delta_pack, &[])
        };
        append_block.push_str(&format!(
            "- append_metadata: written_at={} · new_claims={} · deduplicated_prior_claims={deduplicated_claims}\n\n",
            written_at.to_rfc3339(),
            delta_pack.records.len()
        ));
        append_block.push_str(&render_handoff_or_summary(
            &effective_pack,
            mode,
            for_inject,
            &cleaned,
            written_at,
            &addressed_delta,
        ));
    } else {
        let effective_pack = if pack_has_matching_session(&delta_pack, &cleaned) {
            prioritize_pack_for_targets(&delta_pack, &cleaned)
        } else {
            prioritize_pack_for_targets(&delta_pack, &[])
        };
        append_block.push_str(&format!(
            "- append_metadata: written_at={} · event_window=[{}, {}] UTC · new_claims={} · deduplicated_prior_claims={deduplicated_claims}\n\n",
            written_at.to_rfc3339(),
            cutoff
                .map(|time| time.to_rfc3339())
                .as_deref()
                .unwrap_or("unknown"),
            end.map(|time| time.to_rfc3339())
                .as_deref()
                .unwrap_or("unknown"),
            delta_pack.records.len()
        ));
        append_block.push_str(&render_with_addressed(
            &effective_pack,
            for_inject || mode == ContinuityMode::Inject,
            &addressed_delta,
        ));
    }

    let mut combined = existing_content;
    combined.push_str(&append_block);
    fs::write(path, &combined)?;

    Ok(ContinuityWriteReport {
        content: combined,
        appended: true,
        new_claims: delta_pack.records.len(),
        deduplicated_claims,
        written_at: written_at.to_rfc3339(),
    })
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
            // The remedy names the real switch and what it would restore.
            assert!(
                format!("{error:#}").contains(
                    "pass --distill-mixed (MCP: distill_mixed) to distill their 3 record(s)"
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
        // Finding (Issue #88, round 8): this refusal named `distill_mixed` as
        // the override, yet the gate had withheld nothing, so the override
        // could only lead to an empty pack or a second refusal.
        let message = format!("{error:#}");
        assert!(message.contains("would add nothing"), "{message}");
        assert!(!message.contains("to override"), "{message}");

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
            open_work: None,
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
        // The pack prints the resolved source. `temp_dir()` may be a short
        // (`RUNNER~1`) or symlinked (`/var` -> `/private/var`) spelling of it,
        // so the expectation is the canonical path as the pack prints it.
        let printed_source = printed_path(
            &source
                .canonicalize()
                .expect("canonical source")
                .display()
                .to_string(),
        );
        assert!(
            first.contains(&printed_source),
            "source evidence path {printed_source:?} missing:\n{first}"
        );
        assert!(
            !first.contains(r"\\?\"),
            "a Windows verbatim prefix leaked into the pack:\n{first}"
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
            open_work: None,
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
            open_work: None,
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
            open_work: None,
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
            open_work: None,
        };
        let rendered = render(&pack, false);
        assert!(rendered.contains(
            "newest_source_file_mtime: 2026-09-21T03:30:00Z (file touch, not conversation time)"
        ));
        assert!(rendered.contains("newest_source_file_mtime"));
        assert!(rendered.contains("file touch, not conversation time"));
    }

    /// Distilling on request admits a mixed session; it cannot restore records
    /// that never reached the gate. The pack says how many of the admitted
    /// sessions actually contributed.
    #[test]
    fn continuity_now_counts_distilled_mixed_sessions_that_contributed() {
        let mixed = |session_id: &str| crate::intents::MixedScopeSession {
            agent: "codex".into(),
            session_id: session_id.into(),
            cwds: vec!["/repo/alpha".into(), "/repo/beta".into()],
            branches: Vec::new(),
            conflicts: 1,
            hidden_scopes: 0,
            status: ScopeStatus::MixedCandidate,
        };
        let pack = ContinuityPack {
            selection: Vec::new(),
            source_errors: 0,
            dropped_task_events: 0,
            window_end: String::new(),
            project_label: "Loctree/aicx".into(),
            hours: 24,
            live_sessions: 0,
            records: vec![weighted_decision(
                "mixed-a",
                "a.jsonl",
                "2026-10-03T22:18:00Z",
                "kept from a",
            )],
            sources: Vec::new(),
            index_health: IndexHealthLine {
                newest_session_updated_at: None,
                committed_at: None,
                pending: 0,
                sessions_newer_than_chunks: 0,
                readiness: "ready".into(),
                mode: "live",
            },
            mixed_scope: vec![mixed("mixed-a"), mixed("mixed-b")],
            distilled_mixed: true,
            candidate_cap: 5_000,
            dropped_candidates: 0,
            unplaced_scope: Vec::new(),
            open_work: None,
        };
        let rendered = render(&pack, false);
        assert!(
            rendered.contains(
                "- mixed-workstream candidates: 2 session(s) distilled on request; 1 with retained records"
            ),
            "{rendered}"
        );
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
            open_work: None,
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

    /// Finding (Issue #88, round 10): `source_errors` was counted and shown in
    /// HONESTY, but a window with nothing distilled still returned a pack
    /// whose NOW read as a quiet window — over sources it never read.
    #[test]
    fn a_window_with_nothing_read_refuses_instead_of_reading_as_quiet() {
        let (root, source) = write_continuity_real_path_home("unread");
        fs::remove_file(&source).expect("drop the cataloged source");
        let projects = vec!["Loctree/aicx".to_string()];
        for distill_mixed in [false, true] {
            let error = match build_with_scope_at(&root, &projects, 24, distill_mixed, Utc::now()) {
                Ok(pack) => panic!(
                    "distill_mixed={distill_mixed}: an unread window must refuse, got:\n{}",
                    render(&pack, false)
                ),
                Err(error) => format!("{error:#}"),
            };
            assert!(
                error.contains("nothing to distill; 1 source(s) in the window were unreadable"),
                "distill_mixed={distill_mixed}: {error}"
            );
        }
        let _ = fs::remove_dir_all(&root);

        // A window with records left keeps them; the preface names the holes.
        let record = weighted_decision("s1", "s1.jsonl", "2026-10-03T22:18:00Z", "kept");
        refuse_unread_only_window(std::slice::from_ref(&record), 3)
            .expect("records left alongside source errors");
        // Nothing read wrong and nothing found: a quiet window, not a refusal.
        refuse_unread_only_window(&[], 0).expect("a quiet window with every source read");
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

    fn weighted_decision(session: &str, path: &str, when: &str, summary: &str) -> IntentRecord {
        IntentRecord {
            provenance: Some(crate::intents::IntentProvenance {
                role: "user".into(),
                locator: "line:1".into(),
                scope: None,
                timestamp_basis: "conversation".into(),
                attribution: "transcript".into(),
            }),
            kind: IntentKind::Decision,
            summary: summary.into(),
            context: None,
            evidence: Vec::new(),
            project: "codescribe".into(),
            agent: "codex".into(),
            date: "2026-10-03".into(),
            timestamp: Some(when.into()),
            session_id: session.into(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: path.into(),
            source: None,
            honesty: crate::oracle::ClaimHonesty::canonical(),
        }
    }

    fn qualified_source(
        session: &str,
        path: &str,
        human_messages: usize,
    ) -> intents::SourceSelection {
        intents::SourceSelection {
            agent: "codex".into(),
            session_id: session.into(),
            path: path.into(),
            status: "qualified".into(),
            admitted: true,
            human_messages,
            ..Default::default()
        }
    }

    #[test]
    fn heavier_older_session_outranks_newer_thin_ones() {
        let heavy_id = "01a0f7e1-d2c3-72b2-aacf-d029ac5370cc";
        let mut records = vec![weighted_decision(
            heavy_id,
            "heavy.jsonl",
            "2026-10-03T22:18:00Z",
            "keep the heavy session",
        )];
        let mut selection = vec![qualified_source(heavy_id, "heavy.jsonl", 344)];
        for index in 0..8 {
            let session = format!("thin-{index}");
            let path = format!("thin-{index}.jsonl");
            records.push(weighted_decision(
                &session,
                &path,
                &format!("2026-10-05T19:10:{index:02}Z"),
                &format!("thin decision {index}"),
            ));
            selection.push(qualified_source(&session, &path, 6));
        }
        selection.push(qualified_source("silent-heavy", "silent.jsonl", 344));
        let mixed = vec![crate::intents::MixedScopeSession {
            agent: "codex".into(),
            session_id: "mixed-heavy".into(),
            cwds: vec!["/a".into(), "/b".into()],
            branches: Vec::new(),
            conflicts: 1,
            hidden_scopes: 0,
            status: aicx_parser::engine::ScopeStatus::MixedCandidate,
        }];
        selection.push(qualified_source("mixed-heavy", "mixed.jsonl", 344));
        let sources = collect_sources(&selection, &records, &mixed);
        assert_eq!(sources[0].path, "heavy.jsonl");
        assert_eq!(sources[0].human_messages, 344);
        assert!(sources.iter().any(|source| source.path == "silent.jsonl"));
        assert!(sources.iter().all(|source| source.path != "mixed.jsonl"));
        let pack = ContinuityPack {
            selection,
            source_errors: 0,
            dropped_task_events: 0,
            window_end: "2026-10-06T12:00:00Z".into(),
            project_label: "codescribe".into(),
            hours: 120,
            live_sessions: 10,
            records,
            sources,
            index_health: IndexHealthLine {
                newest_session_updated_at: None,
                committed_at: None,
                pending: 0,
                sessions_newer_than_chunks: 0,
                readiness: "ready".into(),
                mode: "live",
            },
            mixed_scope: mixed,
            distilled_mixed: false,
            candidate_cap: 5_000,
            dropped_candidates: 0,
            unplaced_scope: Vec::new(),
            open_work: None,
        };
        let rendered = render(&pack, false);
        let peers = rendered
            .split("## PEERS")
            .nth(1)
            .and_then(|section| section.split("## DECISIONS").next())
            .unwrap_or("");
        assert!(peers.contains(heavy_id), "{peers}");
        assert!(peers.contains("human=344"), "{peers}");
        assert!(!peers.contains("thin-0"), "{peers}");
        let decisions = rendered
            .split("## DECISIONS")
            .nth(1)
            .and_then(|section| section.split("## TASKS").next())
            .unwrap_or("");
        let heavy_at = decisions
            .find("keep the heavy session")
            .expect("heavy decision");
        let thin_at = decisions
            .find("thin decision 7")
            .expect("newest thin decision");
        assert!(heavy_at < thin_at, "{decisions}");
        let source_block = rendered.split("## SOURCES").nth(1).unwrap_or("");
        let heavy_source = source_block.find("human=344").expect("weighted source");
        let thin_source = source_block.find("human=6").expect("thin source");
        assert!(heavy_source < thin_source, "{source_block}");
    }

    fn overflow_inject_pack() -> ContinuityPack {
        // Medium claim lines fill the budget from the front. The tail lines are
        // longer than one claim, so a leftover sliver cannot admit them.
        let fat = |tag: &str, index: usize| {
            format!("CLAIMSTART-{tag}-{index}-{}-CLAIMEND", "x".repeat(1_800))
        };
        let record = |kind: IntentKind, session: String, summary: String| IntentRecord {
            provenance: Some(crate::intents::IntentProvenance {
                role: "user".into(),
                locator: "line:1".into(),
                scope: None,
                timestamp_basis: "conversation".into(),
                attribution: "transcript".into(),
            }),
            kind,
            summary,
            context: None,
            evidence: Vec::new(),
            project: "Loctree/aicx".into(),
            agent: "codex".into(),
            date: "2026-10-21".into(),
            timestamp: Some("2026-10-21T00:00:00Z".into()),
            session_id: session,
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: "claims.jsonl".into(),
            source: None,
            honesty: crate::oracle::ClaimHonesty::canonical(),
        };
        let mut records = Vec::new();
        for index in 0..8 {
            records.push(record(
                IntentKind::Intent,
                format!("now-{index}"),
                fat("now", index),
            ));
            records.push(record(
                IntentKind::Decision,
                format!("decision-{index}"),
                fat("decision", index),
            ));
            records.push(record(
                IntentKind::Task,
                format!("task-{index}"),
                fat("task", index),
            ));
        }
        ContinuityPack {
            selection: Vec::new(),
            source_errors: 0,
            dropped_task_events: 0,
            window_end: "2026-10-21T00:00:00Z".into(),
            project_label: "Loctree/aicx".into(),
            hours: 72,
            live_sessions: 0,
            records,
            sources: vec![SourceLine {
                agent: "codex".into(),
                path: format!("sources-kept.jsonl{}", "s".repeat(3_000)),
                latest_activity: Some("2026-10-21T00:00:00Z".into()),
                live: false,
                human_messages: 3,
            }],
            index_health: IndexHealthLine {
                newest_session_updated_at: Some("2026-10-21T00:00:00Z".into()),
                committed_at: Some("2026-10-21T00:00:00Z".into()),
                pending: 0,
                sessions_newer_than_chunks: 0,
                readiness: format!("inject-health-kept{}", "h".repeat(3_000)),
                mode: "live",
            },
            mixed_scope: Vec::new(),
            distilled_mixed: false,
            candidate_cap: 5_000,
            dropped_candidates: 0,
            unplaced_scope: Vec::new(),
            open_work: None,
        }
    }

    fn inject_section_body<'a>(rendered: &'a str, heading: &str) -> &'a str {
        let rest = rendered
            .split(heading)
            .nth(1)
            .unwrap_or_else(|| panic!("missing {heading} in:\n{rendered}"));
        let rest = rest.strip_prefix('\n').unwrap_or(rest);
        let end = rest
            .find("\n## ")
            .or_else(|| rest.find("\n[continuity pack truncated"));
        match end {
            Some(index) => &rest[..index],
            None => rest,
        }
    }

    #[test]
    fn inject_budget_keeps_sources_and_index_health_when_claims_overflow() {
        let pack = overflow_inject_pack();
        let full = render(&pack, false);
        assert!(
            full.chars().count() > INJECT_CHAR_BUDGET,
            "fixture must exceed the inject budget so truncation is real"
        );
        let rendered = render(&pack, true);
        let sources = inject_section_body(&rendered, "## SOURCES");
        let health = inject_section_body(&rendered, "## INDEX HEALTH");
        assert!(
            sources.contains("sources-kept.jsonl"),
            "SOURCES body empty:\n{sources}\n--- pack tail ---\n{}",
            rendered.chars().rev().take(800).collect::<String>()
        );
        assert!(
            health.contains("inject-health-kept"),
            "INDEX HEALTH body empty:\n{health}"
        );
        assert!(
            health.contains("readiness:"),
            "INDEX HEALTH lost its readiness line:\n{health}"
        );
    }

    #[test]
    fn inject_budget_keeps_marker_names_shortened_sections() {
        let rendered = render(&overflow_inject_pack(), true);
        let marker = rendered
            .lines()
            .find(|line| line.contains("truncated at inject budget"))
            .unwrap_or_else(|| panic!("missing omission marker:\n{rendered}"));
        assert!(
            marker.contains("shortened:"),
            "marker must name the shortened sections: {marker}"
        );
        for section in ["NOW", "PEERS", "DECISIONS"] {
            assert!(
                marker.contains(section),
                "marker omitted {section}: {marker}"
            );
        }
    }

    #[test]
    fn inject_budget_keeps_output_within_char_budget() {
        let rendered = render(&overflow_inject_pack(), true);
        let chars = rendered.chars().count();
        assert!(
            chars <= INJECT_CHAR_BUDGET,
            "inject pack is {chars} chars, budget is {INJECT_CHAR_BUDGET}"
        );
        for line in rendered.lines() {
            if line.contains("CLAIMSTART") {
                assert!(
                    line.contains("CLAIMEND"),
                    "claim line was cut inside the budget: {line}"
                );
            }
        }
    }

    #[test]
    fn inject_budget_keeps_fitting_pack_unchanged() {
        let pack = render_pack_with_census(5_000, 0);
        assert!(pack.chars().count() <= INJECT_CHAR_BUDGET);
        assert_eq!(bounded_inject(pack.clone()), pack);
    }

    #[test]
    fn session_matches_target_supports_prefix_full_id_agent_and_pair() {
        assert!(session_matches_target(
            "agy",
            "79c9a5ab-f30e-4a23-9c03-ff1a1e178b7c",
            "79c9a5ab"
        ));
        assert!(session_matches_target(
            "agy",
            "79c9a5ab-f30e-4a23-9c03-ff1a1e178b7c",
            "agy/79c9a5ab"
        ));
        assert!(session_matches_target(
            "agy",
            "79c9a5ab-f30e-4a23-9c03-ff1a1e178b7c",
            "antigravity/79c9a5ab"
        ));
        assert!(session_matches_target(
            "agy",
            "79c9a5ab-f30e-4a23-9c03-ff1a1e178b7c",
            "agy"
        ));
        assert!(!session_matches_target(
            "claude",
            "11112222-3333-4444-5555-666677778888",
            "79c9a5ab"
        ));
    }

    #[test]
    fn write_continuity_file_appends_with_timestamps_and_deduplicates_prior_claims() {
        let dir = std::env::temp_dir().join(format!(
            "aicx-continuity-append-dedup-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("CONTINUITY.md");

        let make_record = |session_id: &str, locator: &str, ts: &str, summary: &str| IntentRecord {
            provenance: Some(crate::intents::IntentProvenance {
                role: "user".to_string(),
                locator: locator.to_string(),
                scope: Some("/repo".to_string()),
                timestamp_basis: "frame".to_string(),
                attribution: "transcript_role".to_string(),
            }),
            kind: IntentKind::Decision,
            summary: summary.to_string(),
            context: None,
            evidence: Vec::new(),
            project: "div0-space/andrzej-dad".to_string(),
            agent: "agy".to_string(),
            date: "2026-11-08".to_string(),
            timestamp: Some(ts.to_string()),
            session_id: session_id.to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: "/tmp/79c9a5ab.db".to_string(),
            source: None,
            honesty: Default::default(),
        };

        let mut pack = ContinuityPack {
            selection: vec![intents::SourceSelection {
                agent: "agy".to_string(),
                session_id: "79c9a5ab-f30e-4a23-9c03-ff1a1e178b7c".to_string(),
                path: "/tmp/79c9a5ab.db".to_string(),
                status: "qualified".to_string(),
                admitted: true,
                qualified_frames: 2,
                outside_window_frames: 0,
                scope_withheld_frames: 0,
                unknown_time_frames: 0,
                human_messages: 2,
                ..Default::default()
            }],
            source_errors: 0,
            dropped_task_events: 0,
            window_end: "2026-11-08T10:00:00Z".to_string(),
            project_label: "div0-space/andrzej-dad".to_string(),
            hours: 24,
            live_sessions: 1,
            records: vec![make_record(
                "79c9a5ab-f30e-4a23-9c03-ff1a1e178b7c",
                "seg:0:msg:1",
                "2026-11-08T09:30:00Z",
                "Keep CONTINUITY.md append-only and never overwrite history",
            )],
            sources: Vec::new(),
            index_health: IndexHealthLine {
                newest_session_updated_at: None,
                committed_at: None,
                pending: 0,
                sessions_newer_than_chunks: 0,
                readiness: "ready".to_string(),
                mode: "live",
            },
            mixed_scope: Vec::new(),
            distilled_mixed: false,
            candidate_cap: 5000,
            dropped_candidates: 0,
            unplaced_scope: Vec::new(),
            open_work: None,
        };

        let t1 = DateTime::parse_from_rfc3339("2026-11-08T10:01:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let first = write_continuity_file(
            &path,
            &pack,
            ContinuityMode::Handoff,
            false,
            &["79c9a5ab".to_string()],
            t1,
        )
        .expect("first write");
        assert!(!first.appended);
        assert_eq!(first.new_claims, 1);
        assert_eq!(first.deduplicated_claims, 0);
        assert!(
            first
                .content
                .contains("# CONTINUITY HANDOFF · div0-space/andrzej-dad · 24h")
        );
        assert!(
            first
                .content
                .contains("- written_at: 2026-11-08T10:01:00+00:00")
        );
        assert!(
            first
                .content
                .contains("timestamp=2026-11-08T09:30:00+00:00")
        );

        // Second write with the exact same record: must append a delta marker and NOT duplicate the claim!
        let t2 = DateTime::parse_from_rfc3339("2026-11-08T10:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let second = write_continuity_file(&path, &pack, ContinuityMode::Handoff, false, &[], t2)
            .expect("second write");
        assert!(second.appended);
        assert_eq!(second.new_claims, 0);
        assert_eq!(second.deduplicated_claims, 1);
        assert!(second.content.contains("deduplicated_prior_claims=1"));
        assert_eq!(
            second
                .content
                .matches("Keep CONTINUITY.md append-only and never overwrite history")
                .count(),
            2, // Once in LATEST HUMAN INTENTIONS & DECISIONS and once in PEER & SESSION OUTCOMES of the first write; 0 in the second write!
        );

        // Third write adds one new record: must append only the new record and deduplicate the first!
        pack.records.push(make_record(
            "79c9a5ab-f30e-4a23-9c03-ff1a1e178b7c",
            "seg:1:msg:3",
            "2026-11-08T10:08:00Z",
            "Deduplicate prior claims on every append",
        ));
        let t3 = DateTime::parse_from_rfc3339("2026-11-08T10:10:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let third = write_continuity_file(&path, &pack, ContinuityMode::Summary, false, &[], t3)
            .expect("third write");
        assert!(third.appended);
        assert_eq!(third.new_claims, 1);
        assert_eq!(third.deduplicated_claims, 1);
        assert!(
            third
                .content
                .contains("# CONTINUITY SUMMARY · div0-space/andrzej-dad · 24h")
        );
        assert!(
            third
                .content
                .contains("Deduplicate prior claims on every append")
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
