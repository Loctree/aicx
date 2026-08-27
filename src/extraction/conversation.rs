#![allow(unused_imports)]
use super::*;

use super::projection::{ProjectionKind, ProjectionRole, ProjectionSpec, ResultBody};
use aicx_parser::engine::sha256_hex;

const EXACT_SHORT_DUP_MAX_CHARS: usize = 1000;
const EXACT_SHORT_DUP_WINDOW_MS: i64 = 2_000;

/// Label carried in `ConversationExtractStats.conversation_projection` once the
/// transcript is a view over [`ProjectionSpec`] (W2-T13) instead of the old
/// hard-coded `user_assistant_only` reducer.
pub const PROJECTION_LABEL: &str = "projection_spec";

/// Synthetic role of the parent-pointer entries emitted by `--lineage`.
/// The flattened model has no `LineageMeta` turn; the CLI materializes the
/// walk from `session_meta.forked_from_id` as entries with this role so the
/// bridge below can hand them to the spec as [`ProjectionKind::LineageMeta`].
pub const LINEAGE_ROLE: &str = "lineage";

// ---------------------------------------------------------------------------
// Bridge: flattened model -> throne vocabulary (single speech decision, W2-T13)
// ---------------------------------------------------------------------------
//
// `TimelineEntry` carries `role` + `frame_kind` (`TurnKind` projection), not
// the `FrameClass` the adapters classified with (`crates/aicx-parser/src/
// engine/frames.rs`). Three throne classes are therefore invisible here:
//   * `EchoSeal` arrives as `UserMsg` (same `TurnKind` as `Human`);
//   * `LineageMeta` arrives as `SystemNote` (same as `Inject`);
//   * `InterAgent` has no `TurnKind` and never reaches this layer.
// This bridge is the ONLY place in the consumers that turns a role / frame
// kind string into a projection decision. Consumers ask the spec; they do
// not compare role strings themselves. Recovering the three hidden classes
// needs the model to carry `FrameClass` (BOUNDARY: `crates/**`, W3).

/// Speaker axis for a role string as the model emits it
/// (`output::report::role_str_for_turn`).
pub fn projection_role_for_role(role: &str) -> Option<ProjectionRole> {
    if role.eq_ignore_ascii_case("user") {
        Some(ProjectionRole::Human)
    } else if role.eq_ignore_ascii_case("assistant") || role.eq_ignore_ascii_case("model") {
        Some(ProjectionRole::Assistant)
    } else if role.eq_ignore_ascii_case("tool") {
        // Shell actions are the agent's actions: they follow the assistant
        // lane on the role axis and are dropped by `--user-only`.
        Some(ProjectionRole::Assistant)
    } else if role.eq_ignore_ascii_case("system") || role.eq_ignore_ascii_case(LINEAGE_ROLE) {
        Some(ProjectionRole::System)
    } else {
        None
    }
}

/// Throne kind for a role string alone (entries without `frame_kind`).
pub fn projection_kind_for_role(role: &str) -> Option<ProjectionKind> {
    if role.eq_ignore_ascii_case(LINEAGE_ROLE) {
        Some(ProjectionKind::LineageMeta)
    } else if role.eq_ignore_ascii_case("user") {
        Some(ProjectionKind::Human)
    } else if role.eq_ignore_ascii_case("assistant") || role.eq_ignore_ascii_case("model") {
        Some(ProjectionKind::AssistantFinal)
    } else if role.eq_ignore_ascii_case("tool") {
        Some(ProjectionKind::ShellAction)
    } else if role.eq_ignore_ascii_case("system") {
        Some(ProjectionKind::Inject)
    } else {
        None
    }
}

/// Throne kind for one timeline entry: `frame_kind` first, role as fallback.
pub fn projection_kind_for_entry(entry: &TimelineEntry) -> Option<ProjectionKind> {
    if entry.role.eq_ignore_ascii_case(LINEAGE_ROLE) {
        return Some(ProjectionKind::LineageMeta);
    }
    match entry.frame_kind {
        Some(FrameKind::UserMsg) => Some(ProjectionKind::Human),
        Some(FrameKind::AgentReply) => Some(ProjectionKind::AssistantFinal),
        Some(FrameKind::ToolCall) => Some(ProjectionKind::ShellAction),
        Some(FrameKind::InternalThought | FrameKind::SystemNote) => Some(ProjectionKind::Inject),
        None => projection_kind_for_role(&entry.role),
    }
}

/// Speaker axis for one timeline entry.
pub fn projection_role_for_entry(entry: &TimelineEntry) -> Option<ProjectionRole> {
    projection_role_for_role(&entry.role)
}

/// `true` when the entry is the retained result half of a shell action
/// (`TurnKind::ToolResult`, role `tool`), i.e. the body that
/// `ProjectionSpec::project_shell_result` renders as stub / head / full.
pub fn is_shell_result_entry(entry: &TimelineEntry) -> bool {
    entry.frame_kind == Some(FrameKind::ToolCall) && entry.role.eq_ignore_ascii_case("tool")
}

/// Does the spec emit this entry on both axes (role AND kind)?
///
/// Entries the bridge cannot classify (unknown role, no frame kind) are
/// withheld: an unclassifiable frame is not speech.
pub fn spec_admits_entry(spec: &ProjectionSpec, entry: &TimelineEntry) -> bool {
    let role_ok = projection_role_for_entry(entry).is_some_and(|role| spec.emits_role(role));
    let kind_ok = projection_kind_for_entry(entry).is_some_and(|kind| spec.emits_kind(kind));
    role_ok && kind_ok
}

/// Window axis (`-H` / `--since` / `--until`) evaluated against `now`.
/// Unbounded window admits everything; absence of a flag is never a silent
/// 30-day default.
pub fn entry_in_window(spec: &ProjectionSpec, entry: &TimelineEntry, now: DateTime<Utc>) -> bool {
    if spec.window.is_unbounded() {
        return true;
    }
    if let Some(hours) = spec.window.hours
        && hours > 0
        && entry.timestamp < now - Duration::hours(hours as i64)
    {
        return false;
    }
    if let Some(since) = spec.window.since.as_deref()
        && let Some(lower) = window_bound(since, false)
        && entry.timestamp < lower
    {
        return false;
    }
    if let Some(until) = spec.window.until.as_deref()
        && let Some(upper) = window_bound(until, true)
        && entry.timestamp > upper
    {
        return false;
    }
    true
}

fn window_bound(day: &str, end_of_day: bool) -> Option<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(day.trim(), "%Y-%m-%d").ok()?;
    let time = if end_of_day {
        NaiveTime::from_hms_opt(23, 59, 59)?
    } else {
        NaiveTime::MIN
    };
    Some(Utc.from_utc_datetime(&NaiveDateTime::new(date, time)))
}

/// Retain only the entries the spec emits (role, kind, window). The vector
/// is a view being narrowed; the stored substrate is untouched
/// ([`super::projection::FLAGS_NEVER_MUTATE_THE_SUBSTRATE`]).
pub fn apply_projection(entries: &mut Vec<TimelineEntry>, spec: &ProjectionSpec) {
    let now = Utc::now();
    entries.retain(|entry| spec_admits_entry(spec, entry) && entry_in_window(spec, entry, now));
}

/// Spec for the `--user-only` axis shared by extract and the MCP session
/// surface: roles narrowed to `Human`, everything else razor.
pub fn spec_for_user_only(user_only: bool) -> ProjectionSpec {
    let mut spec = ProjectionSpec::default();
    if user_only {
        spec.roles = vec![ProjectionRole::Human];
    }
    spec
}

/// Fold each shell action with its retained result into one entry whose
/// body is the spec's rendering (`$ cmd [N lines, sha256:…]`, head, or full).
///
/// The pair is `ToolCall` (command, role `assistant`) followed by
/// `ToolResult` (role `tool`) in the same session, as the adapters push them.
/// An orphan result (no command ahead of it) is dropped: it is not speech
/// and has no command marker to hang from. The hash is computed over the
/// retained text so `--result full` and the stub name the same body.
pub fn fold_shell_results(
    entries: Vec<TimelineEntry>,
    spec: &ProjectionSpec,
) -> Vec<TimelineEntry> {
    let mut folded: Vec<TimelineEntry> = Vec::with_capacity(entries.len());
    let mut iter = entries.into_iter().peekable();
    while let Some(mut entry) = iter.next() {
        if is_shell_result_entry(&entry) {
            continue;
        }
        if projection_kind_for_entry(&entry) == Some(ProjectionKind::ShellAction) {
            let retained = match iter.peek() {
                Some(next)
                    if is_shell_result_entry(next) && next.session_id == entry.session_id =>
                {
                    iter.next().map(|next| next.message)
                }
                _ => None,
            };
            let retained_text = retained.as_deref().unwrap_or("");
            let hash = sha256_hex(retained_text.as_bytes());
            entry.message =
                spec.project_shell_result(entry.message.trim_end(), retained_text, &hash);
        }
        folded.push(entry);
    }
    folded
}

/// Parse the `--result` grammar: `none` | `head=N` | `full`.
pub fn parse_result_body_token(token: &str) -> Result<ResultBody, String> {
    let token = token.trim();
    match token {
        "none" | "" => Ok(ResultBody::None),
        "full" => Ok(ResultBody::Full),
        _ => match token.strip_prefix("head=") {
            Some(n) => n
                .parse::<usize>()
                .map(ResultBody::Head)
                .map_err(|_| format!("--result head=N needs a non-negative integer, got `{n}`")),
            None => Err(format!(
                "--result accepts none | head=N | full, got `{token}`"
            )),
        },
    }
}

/// Parse the `--kind` tokens into throne kinds; the first unknown token is
/// returned as the error so the CLI can refuse loudly instead of silently
/// widening or narrowing the view.
pub fn parse_kind_tokens(tokens: &[String]) -> Result<Vec<ProjectionKind>, String> {
    let mut kinds = Vec::with_capacity(tokens.len());
    for token in tokens {
        for piece in token.split(',') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            match ProjectionKind::from_cli_token(piece) {
                Some(kind) => {
                    if !kinds.contains(&kind) {
                        kinds.push(kind);
                    }
                }
                None => {
                    return Err(format!(
                        "--kind `{piece}` is not a throne kind (human | echo_seal | shell_action | inject | assistant_final | lineage_meta | inter_agent)"
                    ));
                }
            }
        }
    }
    Ok(kinds)
}

/// One hop of the `--lineage` walk: `session_id` and the parent it names in
/// its `session_meta.forked_from_id`. `resolved` is `false` when the parent
/// id exists in the meta but the session catalog could not locate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageLink {
    pub depth: usize,
    pub session_id: String,
    pub forked_from_id: Option<String>,
    pub resolved: bool,
}

impl LineageLink {
    /// Render the link as the body of a `LineageMeta` entry.
    pub fn render(&self) -> String {
        match (&self.forked_from_id, self.resolved) {
            (Some(parent), true) => {
                format!(
                    "lineage[{}]: {} forked_from {}",
                    self.depth, self.session_id, parent
                )
            }
            (Some(parent), false) => format!(
                "lineage[{}]: {} forked_from {} (parent not in session catalog)",
                self.depth, self.session_id, parent
            ),
            (None, _) => format!(
                "lineage[{}]: {} (root; no forked_from_id)",
                self.depth, self.session_id
            ),
        }
    }
}

/// Materialize lineage links as timeline entries the projection can emit
/// under `ProjectionKind::LineageMeta`. Timestamps come from the child's
/// first entry so the links sort ahead of the session they annotate.
pub fn lineage_entries(
    agent: &str,
    links: &[LineageLink],
    anchor: DateTime<Utc>,
) -> Vec<TimelineEntry> {
    links
        .iter()
        .map(|link| TimelineEntry {
            timestamp: anchor,
            agent: agent.to_string(),
            session_id: link.session_id.clone(),
            role: LINEAGE_ROLE.to_string(),
            message: link.render(),
            frame_kind: Some(FrameKind::SystemNote),
            branch: None,
            cwd: None,
            timestamp_source: Some("lineage_walk".to_string()),
            source_path: None,
            source_sha256: None,
            source_line_span: None,
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct ConversationProjection {
    pub messages: Vec<ConversationMessage>,
    pub exact_short_duplicates_dropped: usize,
    /// Count of harness-injected synthetic user turns removed from the
    /// projection (slash-command / skill bodies, inline `! command` local
    /// execution I/O, and system/hook reminders). See
    /// [`is_harness_injected_noise`].
    pub harness_noise_dropped: usize,
}

/// Head-anchored markers that identify a synthetic, harness-injected user turn
/// rather than real conversation. Detection requires the marker to sit at the
/// very head of the raw message body (see [`is_harness_injected_noise`]).
///
/// All entries are matched as a literal prefix on the left-trimmed message.
/// Most are `<…>` wrapper tags; `Base directory for this skill:` is the prose
/// preamble the harness prepends when it injects a loaded skill body as its own
/// user turn (the skill invocation `<command-name>` wrapper and the skill body
/// can arrive as separate messages).
const HARNESS_HEAD_MARKERS: [&str; 8] = [
    "<command-message>",      // slash-command / skill invocation (+ injected body)
    "<command-name>",         // slash-command / skill invocation name
    "<local-command-caveat>", // inline `! command` execution caveat
    "<bash-input>",           // inline `! command` input echo
    "<bash-stdout>",          // inline `! command` captured stdout/stderr turn
    "<bash-stderr>",          //
    "<system-reminder>",      // system / hook injected reminder
    "Base directory for this skill:", // injected skill body preamble
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntentLineModality {
    TypedDirective,
    PastedReference,
    Other,
}

const PASTED_REFERENCE_HEAD_MARKERS: [&str; 2] = [
    ">",              // Markdown blockquote pasted as reference material
    "[Pasted text #", // Claude clipboard placeholder for pasted content
];

const TYPED_DIRECTIVE_HEAD_MARKERS: [&str; 4] = [
    "zadanie:", // Polish "Task:" directive head
    "task:", "intent:", "[intent]",
];

/// Classify whether a user-authored line is a typed directive or pasted
/// reference material.
///
/// Like [`is_harness_injected_noise`], detection is role-disciplined and
/// head-anchored on the raw left-trimmed line. Markers that appear deeper in a
/// line are ordinary quoted content and do not change the modality.
pub(crate) fn intent_line_modality(role: &str, line: &str) -> IntentLineModality {
    if projection_role_for_role(role) != Some(ProjectionRole::Human) {
        return IntentLineModality::Other;
    }

    let head = line.trim_start();
    if PASTED_REFERENCE_HEAD_MARKERS
        .iter()
        .any(|marker| head.starts_with(marker))
    {
        return IntentLineModality::PastedReference;
    }

    // Allocation-free case-insensitive prefix check: this runs for every
    // transcript line, and the markers are ASCII.
    if TYPED_DIRECTIVE_HEAD_MARKERS.iter().any(|marker| {
        head.get(..marker.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(marker))
    }) {
        return IntentLineModality::TypedDirective;
    }

    IntentLineModality::Other
}

/// True when `message` is a harness-injected synthetic user turn rather than
/// real conversation: a slash-command / skill invocation (with its injected
/// skill body), inline `! command` local execution I/O, or a system/hook
/// reminder.
///
/// Detection is intentionally **head-anchored on the raw message** and limited
/// to the `user` role. This keeps two carve-outs honest:
///   * **Pasted transcripts** that merely *contain* these markers deeper in
///     the body (e.g. a user pasting a prior session log) are preserved — the
///     markers are not at the head, so the turn is treated as real input.
///   * **Assistant-authored content** (skill-creation bodies, hook-development
///     output) is never matched, because only user-role turns are considered.
pub fn is_harness_injected_noise(role: &str, message: &str) -> bool {
    if projection_role_for_role(role) != Some(ProjectionRole::Human) {
        return false;
    }
    let head = message.trim_start();
    HARNESS_HEAD_MARKERS
        .iter()
        .any(|marker| head.starts_with(marker))
}

/// Project timeline entries into a denoised conversation stream.
///
/// Filters to only `user` and `assistant` roles, resolves repo/project identity
/// from `cwd` + project filter, and preserves provenance fields.
pub fn to_conversation(
    entries: &[TimelineEntry],
    project_filter: &[String],
) -> Vec<ConversationMessage> {
    to_conversation_with_stats(entries, project_filter).messages
}

pub fn to_conversation_with_stats(
    entries: &[TimelineEntry],
    project_filter: &[String],
) -> ConversationProjection {
    project_conversation(entries, project_filter, &ProjectionSpec::default())
}

/// Project timeline entries through one [`ProjectionSpec`] (W2-T13).
///
/// The spec is the only speech decision: which roles, which throne kinds,
/// which window, how a retained shell result renders, whether dialogue is
/// truncated. What remains here is transcript denoising that is not a role
/// decision — harness-injected turns and the 2 s exact-short-duplicate drop —
/// plus assembling the shell action (`$ cmd`) with its retained result into a
/// single rendered message.
pub fn project_conversation(
    entries: &[TimelineEntry],
    project_filter: &[String],
    spec: &ProjectionSpec,
) -> ConversationProjection {
    let now = Utc::now();
    let mut harness_noise_dropped = 0usize;
    let mut messages: Vec<ConversationMessage> = Vec::with_capacity(entries.len());
    for entry in fold_shell_results(entries.to_vec(), spec) {
        let entry = &entry;
        if !entry_in_window(spec, entry, now) {
            continue;
        }
        let Some(kind) = projection_kind_for_entry(entry) else {
            continue;
        };
        if !spec_admits_entry(spec, entry) {
            continue;
        }
        let mut rendered = entry.message.clone();
        // Drop harness-injected synthetic user turns (slash-command / skill
        // bodies, inline `! command` I/O, system/hook reminders). Real
        // conversation — including pasted transcripts and assistant-authored
        // skill/hook content — is preserved. See `is_harness_injected_noise`.
        if is_harness_injected_noise(&entry.role, &entry.message) {
            harness_noise_dropped += 1;
            continue;
        }
        if matches!(
            kind,
            ProjectionKind::Human | ProjectionKind::EchoSeal | ProjectionKind::AssistantFinal
        ) {
            rendered = spec.project_message_body(&rendered);
        }
        let (message_kind, collapse_stub_kind) = classify_conversation_message(&entry.message);
        messages.push(ConversationMessage {
            timestamp: entry.timestamp,
            agent: entry.agent.clone(),
            session_id: entry.session_id.clone(),
            role: entry.role.clone(),
            message: rendered,
            repo_project: repo_name_from_cwd(entry.cwd.as_deref(), project_filter),
            source_path: entry.cwd.clone(),
            branch: entry.branch.clone(),
            message_kind,
            collapse_stub_kind,
        });
    }

    let mut projection = drop_exact_short_user_duplicates(messages);
    projection.harness_noise_dropped = harness_noise_dropped;
    projection
}

/// Compute a stable 64-bit key for `(agent, session_id, trimmed message)`
/// without allocating new `String`s on the hot dedup path. Uses SipHash-1-3
/// with null-byte delimiters between the fields to avoid prefix collisions
/// between e.g. `("a", "bc", "d")` and `("ab", "c", "d")`.
///
/// `agent` is part of the key because imported streams can emit a shared
/// fallback session id when their source lacks one. Without the agent in the
/// key, identical short
/// prompts from two unrelated agent streams within a 2 s window would be
/// silently merged.
fn exact_short_dup_key(agent: &str, session_id: &str, trimmed: &str) -> u64 {
    use siphasher::sip::SipHasher13;
    use std::hash::{Hash, Hasher};
    let mut hasher = SipHasher13::new();
    agent.hash(&mut hasher);
    0u8.hash(&mut hasher);
    session_id.hash(&mut hasher);
    0u8.hash(&mut hasher);
    trimmed.hash(&mut hasher);
    hasher.finish()
}

fn drop_exact_short_user_duplicates(messages: Vec<ConversationMessage>) -> ConversationProjection {
    let mut deduped: Vec<ConversationMessage> = Vec::with_capacity(messages.len());
    let mut last_seen_user: HashMap<u64, DateTime<Utc>> = HashMap::new();
    let mut exact_short_duplicates_dropped = 0;

    for msg in messages {
        let trimmed = msg.message.trim();
        let is_short_user = projection_role_for_role(&msg.role) == Some(ProjectionRole::Human)
            && trimmed.len() <= EXACT_SHORT_DUP_MAX_CHARS;
        let is_exact_short_duplicate = if is_short_user {
            let key = exact_short_dup_key(&msg.agent, &msg.session_id, trimmed);
            let is_duplicate = last_seen_user.get(&key).is_some_and(|previous_timestamp| {
                msg.timestamp
                    .signed_duration_since(*previous_timestamp)
                    .num_milliseconds()
                    .abs()
                    <= EXACT_SHORT_DUP_WINDOW_MS
            });

            last_seen_user.insert(key, msg.timestamp);
            is_duplicate
        } else {
            false
        };

        if !is_exact_short_duplicate {
            deduped.push(msg);
        } else {
            exact_short_duplicates_dropped += 1;
        }
    }

    ConversationProjection {
        messages: deduped,
        exact_short_duplicates_dropped,
        harness_noise_dropped: 0,
    }
}

fn classify_conversation_message(message: &str) -> (MessageKind, Option<CollapseStubKind>) {
    let trimmed_start = message.trim_start();

    if trimmed_start.starts_with("<skill-ref:") {
        return (MessageKind::CollapseStub, Some(CollapseStubKind::SkillRef));
    }
    if trimmed_start.starts_with("<dedup-ref:") {
        return (MessageKind::CollapseStub, Some(CollapseStubKind::DedupRef));
    }

    if message.contains("This session is being continued")
        || message.contains("<local-command-caveat>")
        || message.contains("<command-name>/compact</command-name>")
    {
        return (MessageKind::ContinuationSummary, None);
    }

    let workflow_signals = [
        "run_id:",
        "prompt_id:",
        "status: prompt",
        "Perform the vc-",
        "VC Agents Worker Charter",
        "Report path:",
    ];
    let workflow_signal_count = workflow_signals
        .iter()
        .filter(|signal| message.contains(**signal))
        .count();
    if workflow_signal_count >= 2 {
        return (MessageKind::WorkflowPrompt, None);
    }

    (MessageKind::Conversation, None)
}

#[cfg(test)]
mod harness_noise_tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn entry(role: &str, message: &str, ts: i64) -> TimelineEntry {
        TimelineEntry {
            timestamp: Utc.timestamp_opt(ts, 0).unwrap(),
            agent: "claude".into(),
            session_id: "s1".into(),
            role: role.into(),
            message: message.into(),
            frame_kind: Some(if role == "user" {
                FrameKind::UserMsg
            } else {
                FrameKind::AgentReply
            }),
            branch: None,
            cwd: None,
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
        }
    }

    fn shell_pair(cmd: &str, result: &str, ts: i64) -> [TimelineEntry; 2] {
        let mut call = entry("assistant", cmd, ts);
        call.frame_kind = Some(FrameKind::ToolCall);
        let mut retained = entry("tool", result, ts + 1);
        retained.frame_kind = Some(FrameKind::ToolCall);
        [call, retained]
    }

    #[test]
    fn bridge_maps_flattened_model_onto_throne_kinds() {
        assert_eq!(
            projection_kind_for_entry(&entry("user", "hi", 1)),
            Some(ProjectionKind::Human)
        );
        assert_eq!(
            projection_kind_for_entry(&entry("assistant", "ok", 1)),
            Some(ProjectionKind::AssistantFinal)
        );
        let [call, retained] = shell_pair("cargo test", "ok", 1);
        assert_eq!(
            projection_kind_for_entry(&call),
            Some(ProjectionKind::ShellAction)
        );
        assert!(is_shell_result_entry(&retained));
        let mut note = entry("system", "<system-reminder>", 1);
        note.frame_kind = Some(FrameKind::SystemNote);
        assert_eq!(
            projection_kind_for_entry(&note),
            Some(ProjectionKind::Inject)
        );
        let lineage = &lineage_entries(
            "codex",
            &[LineageLink {
                depth: 0,
                session_id: "child".into(),
                forked_from_id: Some("parent".into()),
                resolved: true,
            }],
            Utc.timestamp_opt(1, 0).unwrap(),
        )[0];
        assert_eq!(
            projection_kind_for_entry(lineage),
            Some(ProjectionKind::LineageMeta)
        );
        assert_eq!(lineage.message, "lineage[0]: child forked_from parent");
    }

    #[test]
    fn razor_spec_admits_speech_and_shell_but_not_inject() {
        let spec = ProjectionSpec::default();
        assert!(spec_admits_entry(&spec, &entry("user", "hi", 1)));
        assert!(spec_admits_entry(&spec, &entry("assistant", "ok", 1)));
        let mut note = entry("system", "reminder", 1);
        note.frame_kind = Some(FrameKind::SystemNote);
        assert!(!spec_admits_entry(&spec, &note));
        let user_only = spec_for_user_only(true);
        assert!(user_only.emits_role(ProjectionRole::Human));
        assert!(!spec_admits_entry(&user_only, &entry("assistant", "ok", 1)));
        let [call, _] = shell_pair("cargo test", "ok", 1);
        assert!(!spec_admits_entry(&user_only, &call));
    }

    #[test]
    fn fold_renders_shell_stub_by_default_and_body_on_result_full() {
        let [call, retained] = shell_pair("cargo test --workspace", "one\ntwo\nthree", 1);
        let razor = fold_shell_results(
            vec![call.clone(), retained.clone()],
            &ProjectionSpec::default(),
        );
        assert_eq!(razor.len(), 1);
        assert!(
            razor[0]
                .message
                .starts_with("$ cargo test --workspace [3 lines, sha256:")
        );
        let mut full = ProjectionSpec::default();
        full.result = ResultBody::Full;
        let folded = fold_shell_results(vec![call, retained], &full);
        assert_eq!(
            folded[0].message,
            "$ cargo test --workspace\none\ntwo\nthree"
        );
        // Orphan result without a command is not speech.
        let [_, orphan] = shell_pair("x", "orphan", 5);
        assert!(fold_shell_results(vec![orphan], &ProjectionSpec::default()).is_empty());
    }

    #[test]
    fn project_conversation_keeps_shell_stub_in_razor_view() {
        let [call, retained] = shell_pair("ls", "a\nb", 1);
        let entries = vec![
            entry("user", "run ls", 0),
            call,
            retained,
            entry("assistant", "done", 3),
        ];
        let projection = project_conversation(&entries, &[], &ProjectionSpec::default());
        let messages: Vec<&str> = projection
            .messages
            .iter()
            .map(|m| m.message.as_str())
            .collect();
        assert_eq!(messages.len(), 3);
        assert!(messages[1].starts_with("$ ls [2 lines, sha256:"));
        let user_only = project_conversation(&entries, &[], &spec_for_user_only(true));
        assert_eq!(user_only.messages.len(), 1);
        assert_eq!(user_only.messages[0].message, "run ls");
    }

    #[test]
    fn window_hours_narrows_the_view_only() {
        let mut spec = ProjectionSpec::default();
        spec.window.hours = Some(1);
        let now = Utc.timestamp_opt(10_000, 0).unwrap();
        assert!(entry_in_window(&spec, &entry("user", "fresh", 9_000), now));
        assert!(!entry_in_window(&spec, &entry("user", "stale", 1), now));
        assert!(entry_in_window(
            &ProjectionSpec::default(),
            &entry("user", "any", 1),
            now
        ));
    }

    #[test]
    fn result_and_kind_tokens_refuse_unknown_values() {
        assert_eq!(parse_result_body_token("none"), Ok(ResultBody::None));
        assert_eq!(parse_result_body_token("head=3"), Ok(ResultBody::Head(3)));
        assert_eq!(parse_result_body_token("full"), Ok(ResultBody::Full));
        assert!(parse_result_body_token("tail=3").is_err());
        assert_eq!(
            parse_kind_tokens(&["human,echo_seal".to_string(), "inter_agent".to_string()]),
            Ok(vec![
                ProjectionKind::Human,
                ProjectionKind::EchoSeal,
                ProjectionKind::InterAgent
            ])
        );
        assert!(parse_kind_tokens(&["conversations".to_string()]).is_err());
    }

    #[test]
    fn drops_head_anchored_slash_command_and_skill_body() {
        let entries = vec![
            entry(
                "user",
                "<command-message>vc-init</command-message>\n<command-name>/vc-init</command-name>\n\nBase directory for this skill: /Users/x/.claude/skills/vc-init\n# vc-init — Technical Due Diligence",
                1,
            ),
            entry("assistant", "Sure, here is the plan.", 2),
        ];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].role, "assistant");
        assert!(
            !projection
                .messages
                .iter()
                .any(|m| m.message.contains("Base directory for this skill"))
        );
    }

    #[test]
    fn drops_local_command_io_turns() {
        let entries = vec![
            entry(
                "user",
                "<local-command-caveat>Caveat: generated while running local commands.</local-command-caveat>\n<bash-input>git status</bash-input>",
                1,
            ),
            entry("user", "<bash-stdout>working tree clean</bash-stdout>", 2),
            entry("assistant", "Looks clean.", 3),
        ];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].message, "Looks clean.");
    }

    #[test]
    fn preserves_pasted_transcript_with_markers_mid_body() {
        // A genuine user turn that pastes a prior transcript containing harness
        // markers deeper in the body must be preserved — the markers are not at
        // the head, so this is real conversation, not a harness injection.
        let pasted = "Analyze this transcript please:\n\n> <command-name>/foo</command-name>\n> <local-command-caveat>noise</local-command-caveat>\nWhat do you make of it?";
        let entries = vec![entry("user", pasted, 1)];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].message, pasted);
    }

    #[test]
    fn classifies_head_blockquote_as_pasted_reference() {
        assert_eq!(
            intent_line_modality("user", "> intent: ship the mirrored plan"),
            IntentLineModality::PastedReference
        );
    }

    #[test]
    fn classifies_head_pasted_text_placeholder_as_pasted_reference() {
        assert_eq!(
            intent_line_modality("user", "[Pasted text #1 +12 lines] Let's ship it"),
            IntentLineModality::PastedReference
        );
    }

    #[test]
    fn classifies_zadanie_head_as_typed_directive() {
        assert_eq!(
            intent_line_modality("user", "Zadanie: dopnij modality gate"),
            IntentLineModality::TypedDirective
        );
    }

    #[test]
    fn preserves_typed_directive_with_reference_markers_mid_body() {
        let line = "Zadanie: analyze quoted material, not as command\n\n> intent: old plan\n[Pasted text #2 +4 lines]";
        assert_eq!(
            intent_line_modality("user", line),
            IntentLineModality::TypedDirective
        );
    }

    #[test]
    fn preserves_assistant_authored_skill_and_hook_content() {
        // Skill-creation / hook-development: assistant authoring skill bodies or
        // hook code is real conversation. Only user-role harness injections are
        // dropped, so assistant content is never matched.
        let entries = vec![entry(
            "assistant",
            "<command-name>/foo</command-name>\nBase directory for this skill: ./skills/foo\nHere is the hook body I propose.",
            1,
        )];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].role, "assistant");
    }

    #[test]
    fn drops_standalone_skill_body_but_keeps_pasted_transcript_quoting_it() {
        let standalone_skill_body = "Base directory for this skill: /Users/x/.claude/skills/vc-init\n\n# vc-init — Technical Due Diligence\n\nThis is harness-injected skill content.";
        // A pasted transcript that QUOTES a skill body deeper in the body must
        // survive: it does not start with the skill-body signature.
        let pasted = "1\t# Conversation Transcript\n2\t\n3\tBase directory for this skill: /Users/x/.claude/skills/vc-init\nPASTED_KEEP";
        let entries = vec![
            entry("user", standalone_skill_body, 1),
            entry("user", pasted, 2),
        ];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.harness_noise_dropped, 1);
        assert!(projection.messages[0].message.contains("PASTED_KEEP"));
        assert!(
            !projection.messages[0]
                .message
                .starts_with("Base directory for this skill")
        );
    }

    #[test]
    fn drops_system_reminder_injection_keeps_real_question() {
        let entries = vec![
            entry("user", "<system-reminder>hook fired</system-reminder>", 1),
            entry("user", "What does store.rs do?", 2),
        ];
        let projection = to_conversation_with_stats(&entries, &[]);
        assert_eq!(projection.messages.len(), 1);
        assert_eq!(projection.messages[0].message, "What does store.rs do?");
    }
}
