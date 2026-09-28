//! Continuity pack — the multi-agent session-resume surface (P1).
//!
//! `aicx continuity show|write` renders one deterministic markdown pack per
//! project bucket + time window: open work (NOW), what each peer agent did
//! (PEERS), closed decisions, tasks, the evidence trail (SOURCES), and an
//! honest INDEX HEALTH line. It replaces "read the compact of yourself" as
//! the way a fresh session recovers context: live parse first, census
//! second, semantics never required for a hot window.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};

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
    pub mtime: Option<String>,
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
        &config,
        projects,
        aicx_home,
        Utc::now(),
    )?;

    let cutoff = Utc::now() - chrono::Duration::hours(hours.min(i64::MAX as u64) as i64);
    let sources = collect_sources(aicx_home, projects, cutoff);
    let index_health = collect_index_health(aicx_home, projects, extraction.stats.live_sessions);

    let mixed_scope = extraction.mixed_scope;
    let unplaced_scope = extraction.unplaced_scope;
    let mut records = extraction.records;
    if !distill_mixed {
        withhold_mixed_sessions(&mut records, &mixed_scope)?;
    }
    refuse_unplaced_only_window(&records, &unplaced_scope)?;

    Ok(ContinuityPack {
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

fn project_matches(entry_project: Option<&str>, projects: &[String]) -> bool {
    if projects.is_empty() {
        return true;
    }
    let Some(identity) = entry_project else {
        return false;
    };
    let (organization, repository) = identity.split_once('/').unwrap_or(("", identity));
    projects.iter().any(|filter| {
        crate::legacy_archive::project_filter_matches(organization, repository, filter)
    })
}

fn collect_sources(
    aicx_home: &Path,
    projects: &[String],
    cutoff: DateTime<Utc>,
) -> Vec<SourceLine> {
    let mut sources = Vec::new();
    let cutoff_ns = cutoff
        .timestamp_nanos_opt()
        .map(|nanos| nanos as u64)
        .unwrap_or(0);

    for entry in crate::catalog::read_entries_at(aicx_home).unwrap_or_default() {
        if !project_matches(entry.project.as_deref(), projects) {
            continue;
        }
        let conversation_date = entry
            .date
            .as_deref()
            .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok());
        if conversation_date.is_some_and(|date| date < cutoff.date_naive()) {
            continue;
        }
        let mtime_ns = crate::catalog::live_source_fingerprint(Path::new(&entry.source_path))
            .map(|(_, mtime)| mtime)
            .or(entry.source_mtime_ns);
        if conversation_date.is_none() && mtime_ns.is_none_or(|mtime| mtime < cutoff_ns) {
            continue;
        }
        sources.push(SourceLine {
            agent: entry.agent,
            path: entry.source_path,
            mtime: mtime_ns.and_then(mtime_ns_to_rfc3339),
            live: false,
        });
    }

    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    if let Ok(delta) = crate::catalog::live_delta(aicx_home, &user_home, cutoff_ns as u128) {
        for entry in delta.unadmitted {
            if !project_matches(entry.project.as_deref(), projects) {
                continue;
            }
            let conversation_date = entry
                .date
                .as_deref()
                .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok());
            if conversation_date.is_some_and(|date| date < cutoff.date_naive()) {
                continue;
            }
            if conversation_date.is_none()
                && entry.source_mtime_ns.is_none_or(|mtime| mtime < cutoff_ns)
            {
                continue;
            }
            sources.push(SourceLine {
                agent: entry.agent,
                path: entry.source_path,
                mtime: entry.source_mtime_ns.and_then(mtime_ns_to_rfc3339),
                live: true,
            });
        }
    }

    // Deterministic: newest first, path as tiebreaker.
    sources.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.path.cmp(&b.path)));
    sources.truncate(SOURCE_CAP);
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

/// Facts the pack has, and the identities it does not. Printed before `## NOW`
/// because inject truncation keeps the head.
fn push_honesty_preface(out: &mut String, pack: &ContinuityPack) {
    out.push_str("## HONESTY\n\n");
    out.push_str("window: conversation date, else last frame if the catalog row is undated. File mtime does not admit a row.\n");
    out.push_str("now: open means a live_open intent record whose conversation date is inside the window. No intent record under this stored session id is not proof the thread is absent.\n");
    out.push_str("session: session id is printed as stored. A truncated session id is not a full thread identity.\n");
    out.push_str("entity: project handle is printed as stored. Rename, alias, and split are not resolved on this card.\n");
    out.push_str("labels: Decision, Intent, Outcome, and Task are classifier labels, not a confirmed operator decision (verification_state=not_verified_by_aicx). Turn role is not stored on the row.\n");
    out.push_str("time: a later record does not retire an earlier one. An ever-immutable pin is not inferred here.\n");
    if pack.dropped_candidates == 0 {
        out.push_str(&format!(
            "census: candidate cap {} not reached.\n",
            pack.candidate_cap
        ));
    } else {
        out.push_str(&format!(
            "census: truncated. candidate cap {} reached; {} candidate(s) dropped. This is not a census.\n",
            pack.candidate_cap, pack.dropped_candidates
        ));
    }
    out.push('\n');
}

fn mtime_ns_to_rfc3339(mtime_ns: u64) -> Option<String> {
    DateTime::<Utc>::from_timestamp(
        (mtime_ns / 1_000_000_000) as i64,
        (mtime_ns % 1_000_000_000) as u32,
    )
    .map(|dt| dt.to_rfc3339())
}

/// Render the pack. `for_inject` bounds the output to the prompt budget.
pub fn render(pack: &ContinuityPack, for_inject: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# CONTINUITY · {} · {}h\n\n",
        pack.project_label, pack.hours
    ));

    push_honesty_preface(&mut out, pack);

    // ── NOW: open sessions + unresolved human intent ─────────────────────
    // Preface states the selection rule. It does not union truncated ids
    // into one thread; that is a different cut.
    out.push_str("## NOW\n\n");
    out.push_str(
        "open means a live_open intent record whose conversation date is inside the window\n",
    );
    out.push_str(
        "no intent record under this stored session id is not proof the thread is absent\n",
    );
    out.push_str("a truncated session id is not a full thread identity\n\n");
    let live_records: Vec<&IntentRecord> = pack
        .records
        .iter()
        .filter(|record| record.honesty.is_live_open())
        .collect();
    let mut open_sessions: BTreeMap<(&str, &str), Option<&str>> = BTreeMap::new();
    for record in &live_records {
        open_sessions
            .entry((record.agent.as_str(), record.session_id.as_str()))
            .or_insert(record.timestamp.as_deref());
    }
    if open_sessions.is_empty() {
        out.push_str("- no open sessions inside the window\n");
    }
    for ((agent, session), timestamp) in open_sessions.iter().take(NOW_CAP) {
        out.push_str(&format!(
            "- open: {agent} · {session} · {}\n",
            timestamp.unwrap_or("no conversation timestamp")
        ));
    }
    let unresolved = unresolved_intents(&pack.records);
    for record in unresolved.iter().take(NOW_CAP) {
        out.push_str(&format!(
            "- unresolved intent ({}): {}\n",
            record.agent, record.summary
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
        out.push_str(&format!(
            "- unplaced work: {} frame(s) from {} session(s) NOT in this pack (their turn window ran where no checkout could claim it)\n",
            pack.unplaced_scope
                .iter()
                .map(|session| session.frames)
                .sum::<usize>(),
            pack.unplaced_scope.len()
        ));
        for session in pack.unplaced_scope.iter().take(NOW_CAP) {
            out.push_str(&format!(
                "  - {} · {} · frames={}\n",
                session.agent, session.session_id, session.frames
            ));
        }
    }
    out.push('\n');

    // ── PEERS: per-agent fairness blocks, newest sessions first ──────────
    out.push_str("## PEERS\n\n");
    let mut by_agent: BTreeMap<&str, Vec<&IntentRecord>> = BTreeMap::new();
    for record in &pack.records {
        by_agent
            .entry(record.agent.as_str())
            .or_default()
            .push(record);
    }
    if by_agent.is_empty() {
        out.push_str("- no sessions inside the window\n");
    }
    for (agent, records) in &by_agent {
        out.push_str(&format!("### {agent}\n"));
        let mut sessions: BTreeMap<&str, Vec<&IntentRecord>> = BTreeMap::new();
        for record in records {
            sessions
                .entry(record.session_id.as_str())
                .or_default()
                .push(record);
        }
        let mut ordered: Vec<(&str, Vec<&IntentRecord>)> = sessions.into_iter().collect();
        fn newest<'a>(records: &'a [&IntentRecord]) -> Option<&'a str> {
            records
                .iter()
                .map(|record| record.timestamp.as_deref())
                .max()
                .flatten()
        }
        ordered
            .sort_by(|(id_a, a), (id_b, b)| newest(b).cmp(&newest(a)).then_with(|| id_a.cmp(id_b)));
        for (session, session_records) in ordered.into_iter().take(PEER_SESSION_CAP) {
            let live_marker = if session_records.iter().any(|r| r.honesty.is_live_open()) {
                " [open]"
            } else {
                ""
            };
            let mtime = session_records
                .iter()
                .filter_map(|r| r.timestamp.as_deref())
                .max()
                .unwrap_or("-");
            out.push_str(&format!("- {session}{live_marker} · {mtime}\n"));
            for record in session_records.iter().take(PEER_CLAIM_CAP) {
                out.push_str(&format!(
                    "  - {}: {}\n",
                    record.kind.heading().to_lowercase(),
                    record.summary
                ));
            }
        }
    }
    out.push('\n');

    // ── DECISIONS (closed) ───────────────────────────────────────────────
    out.push_str("## DECISIONS (closed)\n\n");
    let decisions: Vec<&IntentRecord> = pack
        .records
        .iter()
        .filter(|r| r.kind == IntentKind::Decision && !r.honesty.is_live_open())
        .take(DECISION_CAP)
        .collect();
    if decisions.is_empty() {
        out.push_str("- none captured in the window\n");
    }
    for record in decisions {
        out.push_str(&format!("- [{}] {}\n", record.agent, record.summary));
    }
    out.push('\n');

    // ── TASKS ────────────────────────────────────────────────────────────
    out.push_str("## TASKS\n\n");
    let tasks: Vec<&IntentRecord> = pack
        .records
        .iter()
        .filter(|r| r.kind == IntentKind::Task)
        .take(TASK_CAP)
        .collect();
    if tasks.is_empty() {
        out.push_str("- none captured in the window\n");
    }
    for record in tasks {
        out.push_str(&format!("- [{}] {}\n", record.agent, record.summary));
    }
    out.push('\n');

    // ── SOURCES: evidence, not magic ─────────────────────────────────────
    out.push_str("## SOURCES\n\n");
    if pack.sources.is_empty() {
        out.push_str("- no session sources inside the window\n");
    }
    for source in &pack.sources {
        out.push_str(&format!(
            "- {} · {} · {}{}\n",
            source.agent,
            source.mtime.as_deref().unwrap_or("mtime-unknown"),
            source.path,
            if source.live { " [unadmitted]" } else { "" }
        ));
    }
    out.push('\n');

    // ── INDEX HEALTH: honesty line ───────────────────────────────────────
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
    out.push_str(&format!(
        "- readiness: {} · mode: {} · live_sessions: {}\n",
        health.readiness, health.mode, pack.live_sessions
    ));
    if health.pending > 0 || health.sessions_newer_than_chunks > 0 {
        out.push_str(&format!(
            "- warning: chunk lag (pending={}, sessions_newer_than_chunks={}); run `aicx catalog rebuild --with-chunks` or `aicx index` — empty NOW/PEERS is not proof of a quiet window\n",
            health.pending, health.sessions_newer_than_chunks
        ));
    }

    if for_inject && out.len() > INJECT_CHAR_BUDGET {
        // Keep the head (NOW/PEERS carry the sharpest context) and stamp the
        // truncation so the consumer knows the pack is bounded, not complete.
        out.truncate(INJECT_CHAR_BUDGET);
        out.push_str("\n\n[continuity pack truncated at inject budget]\n");
    }
    out
}

/// Session-level unresolved human intent: Intent records from sessions with
/// no Outcome, newest first.
fn unresolved_intents(records: &[IntentRecord]) -> Vec<&IntentRecord> {
    let resolved: std::collections::HashSet<&str> = records
        .iter()
        .filter(|record| record.kind == IntentKind::Outcome)
        .map(|record| record.session_id.as_str())
        .collect();
    let mut unresolved: Vec<&IntentRecord> = records
        .iter()
        .filter(|record| {
            record.kind == IntentKind::Intent && !resolved.contains(record.session_id.as_str())
        })
        .collect();
    unresolved.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
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

        let source = root.join("runtime_runs/continuity-a/transcript.log");
        fs::create_dir_all(source.parent().expect("parent")).expect("create parent");
        fs::write(
            &source,
            "We decided to route continuity through the live window engine.\n",
        )
        .expect("write source");
        let catalog_path = crate::catalog::sessions_path_for(&root);
        fs::create_dir_all(catalog_path.parent().expect("catalog parent"))
            .expect("create catalog dir");
        let entry = crate::catalog::CatalogEntry {
            schema: crate::catalog::CATALOG_SCHEMA.to_string(),
            session_id: "continuity-a".to_string(),
            agent: "vibecrafted".to_string(),
            project: Some("Loctree/aicx".to_string()),
            date: Some(Utc::now().format("%Y-%m-%d").to_string()),
            cwd: None,
            source_path: source.display().to_string(),
            source_len: None,
            source_mtime_ns: None,
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
        let pack = build(&root, &projects, 24).expect("build pack");
        let first = render(&pack, false);
        let second = render(&build(&root, &projects, 24).expect("rebuild pack"), false);

        for heading in [
            "# CONTINUITY · Loctree/aicx · 24h",
            "## HONESTY",
            "## NOW",
            "## PEERS",
            "## DECISIONS (closed)",
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
                "window: conversation date, else last frame if the catalog row is undated. File mtime does not admit a row.",
                "now: open means a live_open intent record whose conversation date is inside the window. No intent record under this stored session id is not proof the thread is absent.",
                "session: session id is printed as stored. A truncated session id is not a full thread identity.",
                "entity: project handle is printed as stored. Rename, alias, and split are not resolved on this card.",
                "labels: Decision, Intent, Outcome, and Task are classifier labels, not a confirmed operator decision (verification_state=not_verified_by_aicx). Turn role is not stored on the row.",
                "time: a later record does not retire an earlier one. An ever-immutable pin is not inferred here.",
            ] {
                assert!(rendered.contains(line), "missing preface line: {line}");
            }
        }
        assert!(open.contains("census: candidate cap 5000 not reached."));
        assert!(!open.contains("This is not a census"));
        assert!(truncated.contains(
            "census: truncated. candidate cap 5000 reached; 3 candidate(s) dropped. This is not a census."
        ));
        assert!(!truncated.contains("not reached"));
    }

    #[test]
    fn continuity_build_prints_census_from_real_path() {
        let (root, _source) = write_continuity_real_path_home("census");
        let projects = vec!["Loctree/aicx".to_string()];
        let pack = build(&root, &projects, 24).expect("build pack");
        let rendered = render(&pack, false);
        assert!(
            rendered.contains("census:"),
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
            project_label: "vetcoders/vibecrafted".into(),
            hours: 24,
            live_sessions: 1,
            records: vec![crate::intents::IntentRecord {
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
            project_label: "vetcoders/vibecrafted".into(),
            hours: 24,
            live_sessions: 2,
            records: vec![
                crate::intents::IntentRecord {
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
            "open means a live_open intent record whose conversation date is inside the window",
            "no intent record under this stored session id is not proof the thread is absent",
            "a truncated session id is not a full thread identity",
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
                .any(|line| line.contains("claude · hot-open · 2026-08-13T02:00:00Z")),
            "live_open row missing: {open_lines:?}"
        );
        assert!(
            open_lines
                .iter()
                .any(|line| line.contains("claude · open-no-ts · no conversation timestamp")),
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
}
