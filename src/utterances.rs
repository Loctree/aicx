//! Settlement listing of human utterances.
//!
//! Exit success means the list is complete for the asked window. A cut
//! session, an unreadable session, an empty human list, or a newest admitted
//! turn that stops before the window's end date is a refusal. Code-shaped
//! lines and agent replies are machine text. This surface has no decisions
//! heading.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};

use crate::intents::is_user_role;
use crate::timeline::FrameKind;

/// Process status for a settlement that must not be treated as complete.
pub const REFUSAL_EXIT_CODE: i32 = 2;

/// One session is read whole or not at all. Larger sources are omitted.
pub const SESSION_BYTE_BUDGET: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpokenFrame {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub source_path: String,
    pub project: String,
    pub role: String,
    pub frame_kind: Option<FrameKind>,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleReason {
    Unreadable,
    /// The source was over [`SESSION_BYTE_BUDGET`] and was not parsed.
    Omitted,
    /// The parser did not return the whole visible conversation.
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHole {
    pub session_id: String,
    pub source_path: String,
    pub project: String,
    pub reason: HoleReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRead {
    Whole {
        project: String,
        frames: Vec<SpokenFrame>,
    },
    Hole(SessionHole),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utterance {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub source_path: String,
    pub project: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    pub window: Window,
    pub projects: Vec<String>,
    pub span: Option<(DateTime<Utc>, DateTime<Utc>)>,
    pub human: Vec<Utterance>,
    pub machine: Vec<Utterance>,
    pub holes: Vec<SessionHole>,
    pub control_plane_excluded: usize,
}

impl Settlement {
    pub fn refusal_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        for hole in &self.holes {
            let why = match hole.reason {
                HoleReason::Unreadable => "could not be read",
                HoleReason::Omitted => "omitted whole; it was over the session byte budget",
                HoleReason::Partial => "was not read whole",
            };
            reasons.push(format!(
                "refused: session {} {why} ({})",
                hole.session_id, hole.source_path
            ));
        }
        if self.human.is_empty() {
            reasons.push("refused: no human utterance in the window".to_string());
        }
        match self.span {
            Some((_, newest)) if newest.date_naive() >= self.window.end.date_naive() => {}
            Some((_, newest)) => reasons.push(format!(
                "refused: newest admitted utterance {} is before window end {}",
                newest.date_naive(),
                self.window.end.date_naive()
            )),
            None => reasons.push(format!(
                "refused: newest admitted utterance is none; window ends {}",
                self.window.end.date_naive()
            )),
        }
        reasons
    }

    pub fn complete(&self) -> bool {
        self.refusal_reasons().is_empty()
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let span = match self.span {
            Some((oldest, newest)) => {
                format!("{} .. {}", oldest.to_rfc3339(), newest.to_rfc3339())
            }
            None => "none".to_string(),
        };
        out.push_str(&format!(
            "span: {span}; window ends {}\n",
            self.window.end.to_rfc3339()
        ));
        if self.projects.is_empty() {
            out.push_str("projects: (none)\n");
        } else {
            out.push_str("projects: ");
            out.push_str(&self.projects.join(", "));
            out.push('\n');
        }
        if self.control_plane_excluded > 0 {
            out.push_str(&format!(
                "control plane excluded: {}\n",
                self.control_plane_excluded
            ));
        }
        for reason in self.refusal_reasons() {
            out.push_str(&reason);
            out.push('\n');
        }
        push_section(&mut out, "human:", &self.human);
        push_section(&mut out, "machine text:", &self.machine);
        out
    }
}

pub fn window_from_bounds(
    now: DateTime<Utc>,
    hours: u64,
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
) -> Window {
    let start = if let Some(date) = since {
        date.and_time(NaiveTime::MIN).and_utc()
    } else if hours == 0 {
        DateTime::UNIX_EPOCH
    } else {
        now.checked_sub_signed(chrono::Duration::hours(hours.min(i64::MAX as u64) as i64))
            .unwrap_or(DateTime::UNIX_EPOCH)
    };
    let end = if let Some(date) = until {
        date.and_hms_opt(23, 59, 59)
            .expect("23:59:59 is a valid time")
            .and_utc()
    } else {
        now
    };
    Window { start, end }
}

pub fn settle(reads: &[SessionRead], window: Window, projects: &[String]) -> Settlement {
    let mut seen_projects = BTreeSet::new();
    for project in projects {
        if !project.is_empty() {
            seen_projects.insert(project.clone());
        }
    }
    let mut human = Vec::new();
    let mut machine = Vec::new();
    let mut holes = Vec::new();
    let mut span_lo: Option<DateTime<Utc>> = None;
    let mut span_hi: Option<DateTime<Utc>> = None;

    for read in reads {
        match read {
            SessionRead::Hole(hole) => {
                if !hole.project.is_empty() {
                    seen_projects.insert(hole.project.clone());
                }
                holes.push(hole.clone());
            }
            SessionRead::Whole { project, frames } => {
                if !project.is_empty() {
                    seen_projects.insert(project.clone());
                }
                for frame in frames {
                    if frame.timestamp < window.start || frame.timestamp > window.end {
                        continue;
                    }
                    if !frame.project.is_empty() {
                        seen_projects.insert(frame.project.clone());
                    }
                    span_lo = Some(match span_lo {
                        Some(oldest) => oldest.min(frame.timestamp),
                        None => frame.timestamp,
                    });
                    span_hi = Some(match span_hi {
                        Some(newest) => newest.max(frame.timestamp),
                        None => frame.timestamp,
                    });
                    let (prose, machine_text) = split_frame(frame);
                    if let Some(text) = prose {
                        human.push(utterance_from(frame, text));
                    }
                    if let Some(text) = machine_text {
                        machine.push(utterance_from(frame, text));
                    }
                }
            }
        }
    }

    human.sort_by(utterance_order);
    machine.sort_by(utterance_order);
    holes.sort_by(|left, right| {
        left.session_id
            .cmp(&right.session_id)
            .then_with(|| left.source_path.cmp(&right.source_path))
    });

    Settlement {
        window,
        projects: seen_projects.into_iter().collect(),
        span: span_lo.zip(span_hi),
        human,
        machine,
        holes,
        control_plane_excluded: 0,
    }
}

/// Read the durable catalog for `filters` inside `window`.
///
/// An empty filter list reads every catalog project. Catalog date does not
/// admit or exclude a row: for most agents that column is a file mtime, so a
/// session dated after the window can still hold an in-window turn. Every
/// matching session is read whole, omitted whole, or recorded as a hole.
/// Frames from a partial parse are discarded. Turn timestamps decide what
/// enters the list.
pub fn settle_catalog(aicx_home: &Path, filters: &[String], window: Window) -> Result<Settlement> {
    let entries = crate::catalog::read_entries_at(aicx_home)
        .with_context(|| format!("read catalog under {}", aicx_home.display()))?;
    let mut reads = Vec::new();
    let mut control_plane_excluded = 0usize;
    for original in entries {
        if !row_matches(original.project.as_deref(), filters) {
            continue;
        }
        let entry = match crate::catalog::recover_catalog_scope_at(aicx_home, &original) {
            Ok(entry) => entry,
            Err(_) => {
                reads.push(SessionRead::Hole(hole_from(
                    &original,
                    HoleReason::Unreadable,
                )));
                continue;
            }
        };
        if !row_matches(entry.project.as_deref(), filters) {
            continue;
        }
        if crate::sessions::is_guardian_session_kind(entry.session_kind.as_deref()) {
            control_plane_excluded += 1;
            continue;
        }
        if source_exceeds_budget(&entry) {
            reads.push(SessionRead::Hole(hole_from(&entry, HoleReason::Omitted)));
            continue;
        }
        match crate::source_index::read_catalog_conversation_checked_at(aicx_home, &entry) {
            Err(_) => reads.push(SessionRead::Hole(hole_from(&entry, HoleReason::Unreadable))),
            Ok((path, frames, _scope, coverage)) => {
                if !matches!(
                    coverage,
                    crate::source_index::ConversationCoverage::CompleteVisible
                ) {
                    reads.push(SessionRead::Hole(hole_from(&entry, HoleReason::Partial)));
                    continue;
                }
                let project = entry.project.clone().unwrap_or_default();
                let source_path = path.display().to_string();
                let spoken = frames
                    .into_iter()
                    .map(|frame| SpokenFrame {
                        timestamp: frame.timestamp,
                        session_id: entry.session_id.clone(),
                        source_path: source_path.clone(),
                        project: project.clone(),
                        role: frame.role,
                        frame_kind: frame.frame_kind,
                        text: frame.message,
                    })
                    .collect();
                reads.push(SessionRead::Whole {
                    project,
                    frames: spoken,
                });
            }
        }
    }
    let mut settlement = settle(&reads, window, filters);
    settlement.control_plane_excluded = control_plane_excluded;
    Ok(settlement)
}

fn hole_from(entry: &crate::catalog::CatalogEntry, reason: HoleReason) -> SessionHole {
    SessionHole {
        session_id: entry.session_id.clone(),
        source_path: entry.source_path.clone(),
        project: entry.project.clone().unwrap_or_default(),
        reason,
    }
}

fn source_exceeds_budget(entry: &crate::catalog::CatalogEntry) -> bool {
    let len = entry.source_len.or_else(|| {
        std::fs::metadata(&entry.source_path)
            .ok()
            .map(|meta| meta.len())
    });
    matches!(len, Some(len) if len > SESSION_BYTE_BUDGET)
}

fn row_matches(project: Option<&str>, filters: &[String]) -> bool {
    if filters.is_empty() {
        return true;
    }
    let Some(project) = project else {
        return false;
    };
    let (organization, repository) = project.split_once('/').unwrap_or(("", project));
    filters.iter().any(|filter| {
        crate::legacy_archive::project_filter_matches(organization, repository, filter)
    })
}

fn frame_is_human(role: &str, frame_kind: Option<FrameKind>) -> bool {
    match frame_kind {
        Some(FrameKind::UserMsg) => true,
        Some(_) => false,
        None => is_user_role(role),
    }
}

fn split_frame(frame: &SpokenFrame) -> (Option<String>, Option<String>) {
    let text = frame.text.trim();
    if text.is_empty() {
        return (None, None);
    }
    if !frame_is_human(&frame.role, frame.frame_kind) {
        return (None, Some(text.to_string()));
    }
    let (prose, machine) = partition_human_text(text);
    (nonempty_owned(prose), nonempty_owned(machine))
}

fn partition_human_text(text: &str) -> (String, String) {
    let mut prose = Vec::new();
    let mut machine = Vec::new();
    let mut in_fence = false;
    let mut in_block = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            machine.push(line);
            in_fence = !in_fence;
            continue;
        }
        if in_fence || in_block {
            machine.push(line);
            if in_block && trimmed.contains("*/") {
                in_block = false;
            }
            continue;
        }
        if trimmed.starts_with("/*") {
            machine.push(line);
            if !trimmed.contains("*/") {
                in_block = true;
            }
            continue;
        }
        if is_code_comment_line(trimmed) || is_diff_line(trimmed) {
            machine.push(line);
            continue;
        }
        if !trimmed.is_empty() {
            prose.push(line);
        }
    }
    (prose.join("\n"), machine.join("\n"))
}

fn is_code_comment_line(trimmed: &str) -> bool {
    trimmed.starts_with("//") || trimmed.starts_with("*/") || trimmed.starts_with("<!--")
}

fn is_diff_line(trimmed: &str) -> bool {
    trimmed.starts_with("diff --git ")
        || trimmed.starts_with("@@")
        || trimmed.starts_with("+++ ")
        || trimmed.starts_with("--- ")
}

fn nonempty_owned(text: String) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn utterance_from(frame: &SpokenFrame, text: String) -> Utterance {
    Utterance {
        timestamp: frame.timestamp,
        session_id: frame.session_id.clone(),
        source_path: frame.source_path.clone(),
        project: frame.project.clone(),
        text,
    }
}

fn utterance_order(left: &Utterance, right: &Utterance) -> std::cmp::Ordering {
    left.timestamp
        .cmp(&right.timestamp)
        .then_with(|| left.session_id.cmp(&right.session_id))
        .then_with(|| left.text.cmp(&right.text))
}

fn push_section(out: &mut String, title: &str, rows: &[Utterance]) {
    out.push_str(title);
    out.push('\n');
    if rows.is_empty() {
        out.push_str("  (none)\n");
        return;
    }
    for row in rows {
        out.push_str(&format!(
            "- {} session={} path={}\n",
            row.timestamp.to_rfc3339(),
            row.session_id,
            row.source_path
        ));
        for line in row.text.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn september_window() -> Window {
        Window {
            start: ts("2026-09-01T00:00:00Z"),
            end: ts("2026-09-29T23:59:59Z"),
        }
    }

    fn frame(when: &str, role: &str, kind: Option<FrameKind>, text: &str) -> SpokenFrame {
        SpokenFrame {
            timestamp: ts(when),
            session_id: "sess-september".to_string(),
            source_path: "/tmp/vista/session.jsonl".to_string(),
            project: "vetcoders/vista".to_string(),
            role: role.to_string(),
            frame_kind: kind,
            text: text.to_string(),
        }
    }

    fn human_body(report: &str) -> &str {
        report
            .split_once("human:\n")
            .expect("human section")
            .1
            .split_once("machine text:\n")
            .expect("machine section")
            .0
    }

    fn machine_body(report: &str) -> &str {
        report
            .split_once("machine text:\n")
            .expect("machine section")
            .1
    }

    #[test]
    fn september_gap_refuses_and_keeps_a_code_comment_out_of_human_speech() {
        let comment = "// keep the old parser";
        let review = "Review suggestion: rename the helper before shipping.";
        let settlement = settle(
            &[SessionRead::Whole {
                project: "vetcoders/vista".to_string(),
                frames: vec![
                    frame(
                        "2026-09-15T12:00:00Z",
                        "user",
                        Some(FrameKind::UserMsg),
                        comment,
                    ),
                    frame(
                        "2026-09-15T12:05:00Z",
                        "assistant",
                        Some(FrameKind::AgentReply),
                        review,
                    ),
                ],
            }],
            september_window(),
            &["marbles/vista".to_string(), "vetcoders/vista".to_string()],
        );
        let report = settlement.render();
        let first = report.lines().next().expect("span line");

        assert!(!settlement.complete());
        assert!(first.starts_with("span:"));
        assert!(first.contains("2026-09-15"), "{first}");
        assert!(first.contains("2026-09-29"), "{first}");
        assert!(
            settlement
                .refusal_reasons()
                .iter()
                .any(|reason| reason.contains("no human utterance"))
        );
        assert!(
            settlement
                .refusal_reasons()
                .iter()
                .any(|reason| reason.contains("before window end 2026-09-29"))
        );
        assert!(!human_body(&report).contains(comment));
        assert!(machine_body(&report).contains(comment));
        assert!(machine_body(&report).contains(review));
        assert!(!human_body(&report).contains(review));
        assert!(report.lines().all(|line| {
            let heading = line.trim().to_ascii_lowercase();
            heading != "decisions" && heading != "decisions:" && !heading.starts_with("decisions ")
        }));
    }

    #[test]
    fn human_prose_that_reaches_the_window_end_is_complete() {
        let speech = "Ship the census fix.";
        let comment = "// keep the old parser";
        let settlement = settle(
            &[SessionRead::Whole {
                project: "vetcoders/vista".to_string(),
                frames: vec![frame(
                    "2026-09-29T16:00:00Z",
                    "user",
                    Some(FrameKind::UserMsg),
                    &format!("{speech}\n{comment}"),
                )],
            }],
            september_window(),
            &["vetcoders/vista".to_string()],
        );
        let report = settlement.render();

        assert!(settlement.complete(), "{report}");
        assert!(human_body(&report).contains(speech));
        assert!(!human_body(&report).contains(comment));
        assert!(machine_body(&report).contains(comment));
        assert_eq!(settlement.human.len(), 1);
        assert_eq!(settlement.human[0].session_id, "sess-september");
        assert_eq!(settlement.human[0].source_path, "/tmp/vista/session.jsonl");
    }

    #[test]
    fn a_hole_refuses_even_when_human_speech_covers_the_window() {
        let leaked = "this omitted session must not be quoted";
        let settlement = settle(
            &[
                SessionRead::Whole {
                    project: "vetcoders/vista".to_string(),
                    frames: vec![frame(
                        "2026-09-29T16:00:00Z",
                        "human",
                        None,
                        "Ship the census fix.",
                    )],
                },
                SessionRead::Hole(SessionHole {
                    session_id: "sess-cut".to_string(),
                    source_path: "/tmp/vista/cut.jsonl".to_string(),
                    project: "vista".to_string(),
                    reason: HoleReason::Omitted,
                }),
            ],
            september_window(),
            &[],
        );
        let report = settlement.render();

        assert!(!settlement.complete());
        assert!(report.contains("sess-cut"));
        assert!(report.contains("omitted whole"));
        assert!(!report.contains(leaked));
        assert!(report.contains("vista"));
    }

    #[test]
    fn a_partial_session_is_a_hole_and_not_a_source_of_speech() {
        let settlement = settle(
            &[SessionRead::Hole(SessionHole {
                session_id: "sess-partial".to_string(),
                source_path: "/tmp/vista/partial.jsonl".to_string(),
                project: "vetcoders/vista".to_string(),
                reason: HoleReason::Partial,
            })],
            september_window(),
            &["vetcoders/vista".to_string()],
        );
        let report = settlement.render();
        let first = report.lines().next().expect("span line");

        assert!(!settlement.complete());
        assert!(first.contains("span: none"));
        assert!(first.contains("2026-09-29"));
        assert!(human_body(&report).contains("(none)"));
        assert!(report.contains("was not read whole"));
    }

    struct TempHome(std::path::PathBuf);

    impl TempHome {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("aicx-utterances-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn catalog_entry(
        session_id: &str,
        project: &str,
        date: &str,
        source_path: &std::path::Path,
        source_len: Option<u64>,
        session_kind: Option<&str>,
    ) -> crate::catalog::CatalogEntry {
        crate::catalog::CatalogEntry {
            schema: crate::catalog::CATALOG_SCHEMA.to_string(),
            session_id: session_id.to_string(),
            agent: "claude".to_string(),
            project: Some(project.to_string()),
            date: Some(date.to_string()),
            cwd: None,
            source_path: source_path.display().to_string(),
            source_len,
            source_mtime_ns: None,
            source_bundle_fingerprint: None,
            title: None,
            machine: None,
            logical_session_id: None,
            session_kind: session_kind.map(str::to_string),
        }
    }

    fn write_catalog(home: &std::path::Path, entries: &[crate::catalog::CatalogEntry]) {
        let path = crate::catalog::sessions_path_for(home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = String::new();
        for entry in entries {
            body.push_str(&serde_json::to_string(entry).unwrap());
            body.push('\n');
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn missing_catalog_refuses_without_inventing_speech() {
        let home = TempHome::new("missing");
        let settlement = settle_catalog(
            home.0.as_path(),
            &["vetcoders/vista".to_string()],
            september_window(),
        )
        .expect("missing catalog is an empty read");
        let report = settlement.render();

        assert!(!settlement.complete());
        assert!(report.lines().next().unwrap().contains("span: none"));
        assert!(report.contains("no human utterance"));
    }

    #[test]
    fn a_broken_catalog_is_an_error_not_a_complete_list() {
        let home = TempHome::new("broken");
        let path = crate::catalog::sessions_path_for(&home.0);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not-json\n").unwrap();

        assert!(settle_catalog(&home.0, &[], september_window()).is_err());
    }

    #[test]
    fn catalog_mtime_after_the_window_still_keeps_the_human_line() {
        let home = TempHome::new("later-date");
        let source = home.0.join("session.jsonl");
        let speech = "Ship the census fix.";
        let comment = "// keep the old parser";
        std::fs::write(
            &source,
            format!(
                "{{\"type\":\"user\",\"timestamp\":\"2026-09-29T16:00:00Z\",\"sessionId\":\"sess-later\",\"message\":{{\"role\":\"user\",\"content\":\"{speech}\\n{comment}\"}}}}\n"
            ),
        )
        .unwrap();
        let foreign = home.0.join("foreign-missing.jsonl");
        let guardian = home.0.join("guardian-missing.jsonl");
        write_catalog(
            &home.0,
            &[
                catalog_entry(
                    "sess-later",
                    "vetcoders/vista",
                    "2026-10-06",
                    &source,
                    Some(std::fs::metadata(&source).unwrap().len()),
                    None,
                ),
                catalog_entry(
                    "sess-other",
                    "other/repo",
                    "2026-09-15",
                    &foreign,
                    Some(4),
                    None,
                ),
                catalog_entry(
                    "sess-guardian",
                    "vetcoders/vista",
                    "2026-09-20",
                    &guardian,
                    Some(4),
                    Some("subagent:guardian"),
                ),
            ],
        );

        let settlement = settle_catalog(
            &home.0,
            &["vetcoders/vista".to_string()],
            september_window(),
        )
        .expect("catalog reads");
        let report = settlement.render();

        assert!(
            settlement.complete(),
            "later catalog date must not hide the September line\n{report}"
        );
        assert!(human_body(&report).contains(speech), "{report}");
        assert!(!human_body(&report).contains(comment), "{report}");
        assert!(machine_body(&report).contains(comment), "{report}");
        assert!(!report.contains("sess-other"), "{report}");
        assert!(!report.contains("sess-guardian"), "{report}");
        assert_eq!(settlement.control_plane_excluded, 1);
        assert!(report.contains("control plane excluded: 1"));
    }

    #[test]
    fn an_unreadable_or_oversized_catalog_row_refuses() {
        let home = TempHome::new("holes");
        let missing = home.0.join("missing.jsonl");
        let huge = home.0.join("huge.jsonl");
        write_catalog(
            &home.0,
            &[
                catalog_entry(
                    "sess-missing",
                    "vetcoders/vista",
                    "2026-10-06",
                    &missing,
                    Some(12),
                    None,
                ),
                catalog_entry(
                    "sess-huge",
                    "vetcoders/vista",
                    "2026-09-02",
                    &huge,
                    Some(SESSION_BYTE_BUDGET + 1),
                    None,
                ),
            ],
        );

        let settlement = settle_catalog(
            &home.0,
            &["vetcoders/vista".to_string()],
            september_window(),
        )
        .expect("holes are a settlement, not an I/O error");
        let report = settlement.render();

        assert!(!settlement.complete());
        assert!(report.contains("sess-missing"));
        assert!(report.contains("could not be read"));
        assert!(report.contains("sess-huge"));
        assert!(report.contains("omitted whole"));
        assert!(!report.contains("this file was never written"));
    }
}
