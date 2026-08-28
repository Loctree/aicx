#![allow(unused_imports)]
use super::*;

use super::projection::{ProjectionKind, ProjectionRole, ProjectionSpec, ResultBody};
use aicx_parser::engine::{
    AgentKind, ContextEpochRef, EntryOrigin, FrameClass, ProviderConversationRef, RefusalEvidence,
    RefusalReason, ScopeStatus, sha256_hex,
};

const EXACT_SHORT_DUP_MAX_CHARS: usize = 1000;
const EXACT_SHORT_DUP_WINDOW_MS: i64 = 2_000;

/// Label carried in `ConversationExtractStats.conversation_projection` once the
/// transcript is a view over [`ProjectionSpec`] (W2-T13) instead of the old
/// hard-coded `user_assistant_only` reducer.
pub const PROJECTION_LABEL: &str = "projection_spec";

/// Synthetic role of the lineage entries emitted by `--lineage` for the
/// walk itself (graph edges). Entries that came out of the model carry
/// `frame_class = LineageMeta` and need no synthetic role.
pub const LINEAGE_ROLE: &str = "lineage";

// ---------------------------------------------------------------------------
// Bridge: model -> throne vocabulary (single speech decision, W2-T13 / W2-R1)
// ---------------------------------------------------------------------------
//
// Since W2-R1 every throne-owned turn carries its `FrameClass` on
// `TimelineEntry::frame_class`; the projection kind is read from the class
// (`ProjectionKind::from_frame_class`) — `EchoSeal`, `LineageMeta` and
// `InterAgent` are distinguishable here without any local reducer. The
// role / `frame_kind` fallback below exists ONLY for entries that never went
// through the model (store chunks, importers, legacy archives, lanes the
// throne does not own: tool call/result, reasoning, harness events). This
// bridge stays the one place that turns a role / frame-kind string into a
// projection decision; consumers ask the spec, never compare role strings.

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

/// Throne kind for one timeline entry: the carried `FrameClass` first
/// (W2-R1), then `frame_kind`, then role as the last fallback.
pub fn projection_kind_for_entry(entry: &TimelineEntry) -> Option<ProjectionKind> {
    if let Some(class) = &entry.frame_class {
        return Some(ProjectionKind::from_frame_class(class));
    }
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

/// Speaker axis for one timeline entry: the throne's `turn_role` when the
/// class is carried, the role string otherwise.
pub fn projection_role_for_entry(entry: &TimelineEntry) -> Option<ProjectionRole> {
    if let Some(class) = &entry.frame_class {
        return Some(match class.turn_role() {
            aicx_parser::engine::TurnRole::User => ProjectionRole::Human,
            aicx_parser::engine::TurnRole::Assistant | aicx_parser::engine::TurnRole::Tool => {
                ProjectionRole::Assistant
            }
            aicx_parser::engine::TurnRole::System => ProjectionRole::System,
        });
    }
    projection_role_for_role(&entry.role)
}

/// Human channel of an entry when the model carried it; `None` for
/// class-less entries (their channel is unknowable here, not `Direct`).
pub fn human_channel_for_entry(entry: &TimelineEntry) -> Option<aicx_parser::engine::HumanChannel> {
    entry
        .frame_class
        .as_ref()
        .and_then(FrameClass::human_channel)
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
    // Human channel (Decision 7): delayed speech (echo bus / queue) is a
    // channel the spec may withhold even when the kind is admitted.
    let channel_ok =
        human_channel_for_entry(entry).is_none_or(|channel| spec.emits_human_channel(channel));
    role_ok && kind_ok && channel_ok
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

// ---------------------------------------------------------------------------
// Lineage: a graph of tagged conversation refs, not a list of ids (W2-R1)
// ---------------------------------------------------------------------------

/// How a child conversation is attached to its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageEdgeKind {
    /// Provider wrote the pointer down (Codex `forked_from_id`).
    DeclaredFork,
    /// Sub-agent thread under a parent thread (Codex `parent_thread_id`).
    ParentThread,
    /// Established by identical records in both files (Claude `/fork`
    /// copies the origin's prefix with the same `uuid`s; no pointer exists).
    SharedPrefix,
}

impl LineageEdgeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeclaredFork => "declared_fork",
            Self::ParentThread => "parent_thread",
            Self::SharedPrefix => "shared_prefix",
        }
    }
}

/// One conversation in the lineage graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageNode {
    /// Hops from the requested conversation (0 = the request itself).
    pub depth: usize,
    /// Provider-tagged identity — Codex nodes carry `tree_session_id` /
    /// `thread_id` / `parent_thread_id`, Claude nodes `session_id`.
    pub conversation: ProviderConversationRef,
    /// `false` when a parent is named but the catalog could not locate it.
    pub resolved: bool,
    /// Entries native to this node in the merged timeline.
    pub own_entries: usize,
    /// Entries this node shares with a parent (counted once, tagged
    /// `EntryOrigin::InheritedFrom`).
    pub inherited_entries: usize,
    /// Compaction boundaries of this node: context epochs, never sources.
    pub epochs: Vec<ContextEpochRef>,
}

impl LineageNode {
    pub fn node_id(&self) -> &str {
        self.conversation.node_id()
    }
}

/// One parent pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageEdge {
    pub child: String,
    pub parent: String,
    pub kind: LineageEdgeKind,
    pub resolved: bool,
}

/// The lineage of one requested conversation: nodes keyed by
/// `ProviderConversationRef::node_id`, edges child → parent, and the
/// branch boundary of each node (where its own history starts in the
/// merged timeline). Inherited history is stored once.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LineageGraph {
    pub root: String,
    pub nodes: Vec<LineageNode>,
    pub edges: Vec<LineageEdge>,
}

impl LineageGraph {
    pub fn new(root: ProviderConversationRef, epochs: Vec<ContextEpochRef>) -> Self {
        let root_id = root.node_id().to_owned();
        Self {
            root: root_id,
            nodes: vec![LineageNode {
                depth: 0,
                conversation: root,
                resolved: true,
                own_entries: 0,
                inherited_entries: 0,
                epochs,
            }],
            edges: Vec::new(),
        }
    }

    pub fn node(&self, id: &str) -> Option<&LineageNode> {
        self.nodes.iter().find(|node| node.node_id() == id)
    }

    pub fn node_mut(&mut self, id: &str) -> Option<&mut LineageNode> {
        self.nodes.iter_mut().find(|node| node.node_id() == id)
    }

    /// Attach `parent` under `child`. Returns `false` (and adds nothing)
    /// when the parent is already in the graph — a cycle or a diamond is
    /// reported by the caller, not silently walked twice.
    pub fn attach_parent(
        &mut self,
        child: &str,
        parent: ProviderConversationRef,
        kind: LineageEdgeKind,
        resolved: bool,
        epochs: Vec<ContextEpochRef>,
    ) -> bool {
        let parent_id = parent.node_id().to_owned();
        if self.node(&parent_id).is_some() {
            return false;
        }
        let depth = self.node(child).map_or(0, |node| node.depth) + 1;
        self.nodes.push(LineageNode {
            depth,
            conversation: parent,
            resolved,
            own_entries: 0,
            inherited_entries: 0,
            epochs,
        });
        self.edges.push(LineageEdge {
            child: child.to_owned(),
            parent: parent_id,
            kind,
            resolved,
        });
        true
    }

    /// Record an unresolved parent pointer (named in the meta, absent from
    /// the catalog) as a dangling node so the report says so.
    pub fn attach_unresolved(&mut self, child: &str, parent_id: &str, kind: LineageEdgeKind) {
        let agent = self
            .node(child)
            .map_or(AgentKind::Codex, |node| node.conversation.agent());
        let parent = ProviderConversationRef::from_store_id(agent, parent_id);
        self.attach_parent(child, parent, kind, false, Vec::new());
    }

    /// The node that owns no parent edge.
    pub fn is_root_only(&self) -> bool {
        self.edges.is_empty()
    }

    /// One line per node/edge, in walk order: what the graph knows.
    pub fn render_lines(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.nodes.len() + self.edges.len());
        for node in &self.nodes {
            let id = node.node_id();
            let parents: Vec<&LineageEdge> =
                self.edges.iter().filter(|edge| edge.child == id).collect();
            if parents.is_empty() {
                let unobserved = node.conversation.unobserved();
                let note = if unobserved.is_empty() {
                    String::new()
                } else {
                    format!(" (unobserved: {})", unobserved.join(","))
                };
                lines.push(format!(
                    "lineage[{}]: {} {} (root; no parent pointer){note}",
                    node.depth,
                    node.conversation.agent().as_str(),
                    id
                ));
            }
            for edge in parents {
                lines.push(format!(
                    "lineage[{}]: {} {} <- {} via {}{}",
                    node.depth,
                    node.conversation.agent().as_str(),
                    id,
                    edge.parent,
                    edge.kind.as_str(),
                    if edge.resolved {
                        ""
                    } else {
                        " (parent not in session catalog)"
                    }
                ));
            }
            for epoch in &node.epochs {
                lines.push(format!(
                    "lineage[{}]: {} epoch #{} (compaction; {} replaced ref(s); not a source)",
                    node.depth,
                    id,
                    epoch.compaction_index,
                    epoch.replacement_refs.len()
                ));
            }
            if node.inherited_entries > 0 {
                lines.push(format!(
                    "lineage[{}]: {} shares {} entrie(s) with its parent (counted once)",
                    node.depth, id, node.inherited_entries
                ));
            }
        }
        lines
    }
}

/// Materialize the lineage graph as timeline entries the projection can emit
/// under `ProjectionKind::LineageMeta`. Timestamps come from the child's
/// first entry so the lines sort ahead of the history they annotate.
pub fn lineage_entries(
    agent: &str,
    graph: &LineageGraph,
    anchor: DateTime<Utc>,
) -> Vec<TimelineEntry> {
    graph
        .render_lines()
        .into_iter()
        .map(|line| TimelineEntry {
            timestamp: anchor,
            agent: agent.to_string(),
            session_id: graph.root.clone(),
            role: LINEAGE_ROLE.to_string(),
            message: line,
            frame_class: None,
            lineage_origin: Some(EntryOrigin::Own),
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

/// Result of laying a parent's timeline under a child's.
#[derive(Debug, Clone)]
pub struct InheritedMerge {
    pub entries: Vec<TimelineEntry>,
    /// Child entries that also exist in the parent: kept once, tagged
    /// `EntryOrigin::InheritedFrom`.
    pub inherited_from_parent: usize,
    /// Parent entries beyond the branch point: the parent's own
    /// continuation, tagged `EntryOrigin::ParentOnly`.
    pub parent_only: usize,
}

fn record_key(entry: &TimelineEntry) -> (i64, String, String) {
    (
        entry.timestamp.timestamp_millis(),
        entry.role.to_ascii_lowercase(),
        entry.message.trim().to_owned(),
    )
}

/// Lay `parent` under `child` without doubling the history the child
/// inherited. A record present in both (same seal timestamp, role and body
/// — the Claude fork prefix is a byte copy with identical `uuid`s, so this
/// is exact, not fuzzy) stays once, on the child, tagged
/// `InheritedFrom { conversation: parent, via }`. Parent records the child
/// does not carry are the parent's own branch and are tagged `ParentOnly`.
/// Nothing is dropped and nothing is emitted twice.
pub fn merge_inherited(
    child: Vec<TimelineEntry>,
    parent: Vec<TimelineEntry>,
    parent_ref: &ProviderConversationRef,
    via: LineageEdgeKind,
) -> InheritedMerge {
    let mut parent_keys: HashMap<(i64, String, String), usize> = HashMap::new();
    for entry in &parent {
        *parent_keys.entry(record_key(entry)).or_insert(0) += 1;
    }
    let mut inherited_from_parent = 0usize;
    let mut entries: Vec<TimelineEntry> = Vec::with_capacity(child.len() + parent.len());
    for mut entry in child {
        let key = record_key(&entry);
        if let Some(remaining) = parent_keys.get_mut(&key)
            && *remaining > 0
        {
            *remaining -= 1;
            inherited_from_parent += 1;
            entry.lineage_origin = Some(EntryOrigin::InheritedFrom {
                conversation: parent_ref.clone(),
                via: via.as_str().to_owned(),
            });
        } else if entry.lineage_origin.is_none() {
            entry.lineage_origin = Some(EntryOrigin::Own);
        }
        entries.push(entry);
    }
    let mut parent_only = 0usize;
    for mut entry in parent {
        let key = record_key(&entry);
        // Keys still counted here were never matched by a child record.
        if let Some(remaining) = parent_keys.get_mut(&key)
            && *remaining > 0
        {
            *remaining -= 1;
            parent_only += 1;
            entry.lineage_origin = Some(EntryOrigin::ParentOnly {
                conversation: parent_ref.clone(),
            });
            entries.push(entry);
        }
    }
    entries.sort_by_key(|entry| entry.timestamp);
    InheritedMerge {
        entries,
        inherited_from_parent,
        parent_only,
    }
}

// ---------------------------------------------------------------------------
// Scope: is this span one workstream? (W2-R1)
// ---------------------------------------------------------------------------

/// Structural scope of a span of entries, with the evidence that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeReport {
    pub status: ScopeStatus,
    pub cwds: Vec<String>,
    pub branches: Vec<String>,
    pub entries: usize,
}

/// Scope from the entries' own `cwd` / `branch` evidence. Unknown values do
/// not count as a second workstream; topic-level mixing inside one cwd is
/// invisible here and is not guessed at.
pub fn scope_report_for_entries(entries: &[TimelineEntry]) -> ScopeReport {
    let cwds: BTreeSet<String> = entries
        .iter()
        .filter_map(|entry| entry.cwd.as_deref())
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(str::to_owned)
        .collect();
    let branches: BTreeSet<String> = entries
        .iter()
        .filter_map(|entry| entry.branch.as_deref())
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
        .collect();
    let status = ScopeStatus::from_evidence(
        cwds.iter().map(String::as_str),
        branches.iter().map(String::as_str),
    );
    ScopeReport {
        status,
        cwds: cwds.into_iter().collect(),
        branches: branches.into_iter().collect(),
        entries: entries.len(),
    }
}

/// The refusal a single-history distiller (`continuity`) must return for a
/// mixed candidate instead of `Ok(empty)` or a braided narrative. `None`
/// when the scope is not mixed or the caller explicitly asked to distill
/// mixed spans.
pub fn refuse_mixed_workstream(
    agent: AgentKind,
    session_id: &str,
    report: &ScopeReport,
    distill_mixed: bool,
) -> Option<RefusalReason> {
    if distill_mixed || report.status != ScopeStatus::MixedCandidate {
        return None;
    }
    let mut consumed_by_kind = std::collections::BTreeMap::new();
    consumed_by_kind.insert("timeline_entry".to_owned(), report.entries as u64);
    let mut examined = Vec::with_capacity(report.cwds.len() + report.branches.len() + 1);
    examined.push(format!(
        "scope_status={} cwds={} branches={}",
        report.status.as_str(),
        report.cwds.len(),
        report.branches.len()
    ));
    examined.extend(report.cwds.iter().map(|cwd| format!("cwd={cwd}")));
    examined.extend(
        report
            .branches
            .iter()
            .map(|branch| format!("branch={branch}")),
    );
    Some(RefusalReason::MixedWorkstream {
        agent,
        session_id: session_id.to_owned(),
        cwds: report.cwds.clone(),
        branches: report.branches.clone(),
        evidence: RefusalEvidence {
            raw_unit_count: report.entries as u64,
            consumed_by_kind,
            known_skipped: std::collections::BTreeMap::new(),
            threshold: 1,
            found: report.cwds.len().max(report.branches.len()) as u64,
            examined,
        },
    })
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
            frame_class: None,
            lineage_origin: None,
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
        let mut graph = LineageGraph::new(codex_ref("child", Some("parent")), Vec::new());
        assert!(graph.attach_parent(
            "child",
            codex_ref("parent", None),
            LineageEdgeKind::DeclaredFork,
            true,
            Vec::new(),
        ));
        let lineage = lineage_entries("codex", &graph, Utc.timestamp_opt(1, 0).unwrap());
        assert_eq!(
            projection_kind_for_entry(&lineage[0]),
            Some(ProjectionKind::LineageMeta)
        );
        assert_eq!(
            lineage[0].message,
            "lineage[0]: codex child <- parent via declared_fork"
        );
        assert!(
            lineage[1]
                .message
                .starts_with("lineage[1]: codex parent (root; no parent pointer)")
        );
    }

    fn codex_ref(thread: &str, forked_from: Option<&str>) -> ProviderConversationRef {
        ProviderConversationRef::Codex {
            tree_session_id: thread.to_owned(),
            thread_id: Some(thread.to_owned()),
            forked_from_id: forked_from.map(str::to_owned),
            parent_thread_id: None,
            window_id: None,
            unobserved: Vec::new(),
        }
    }

    fn claude_ref(session: &str) -> ProviderConversationRef {
        ProviderConversationRef::Claude {
            session_id: session.to_owned(),
            agent_id: None,
            unobserved: Vec::new(),
        }
    }

    fn classed(role: &str, message: &str, ts: i64, class: FrameClass) -> TimelineEntry {
        let mut entry = entry(role, message, ts);
        entry.frame_kind = class.turn_kind().map(|kind| match kind {
            aicx_parser::engine::TurnKind::UserMsg => FrameKind::UserMsg,
            aicx_parser::engine::TurnKind::AgentReply => FrameKind::AgentReply,
            aicx_parser::engine::TurnKind::InternalThought => FrameKind::InternalThought,
            aicx_parser::engine::TurnKind::ToolCall | aicx_parser::engine::TurnKind::ToolResult => {
                FrameKind::ToolCall
            }
            aicx_parser::engine::TurnKind::SystemNote => FrameKind::SystemNote,
        });
        entry.frame_class = Some(class);
        entry
    }

    #[test]
    fn carried_frame_class_wins_over_frame_kind_and_role() {
        use aicx_parser::engine::{HumanChannel, Known};
        // EchoSeal used to arrive as `UserMsg` and was indistinguishable
        // from direct human speech; with the class carried it is its own
        // kind, withheld by the razor and revealed by `--dialog`.
        let echo = classed(
            "user",
            "sealed later",
            1,
            FrameClass::EchoSeal {
                seal_ts: Known::unknown(),
                channel: HumanChannel::EchoBus,
            },
        );
        assert_eq!(
            projection_kind_for_entry(&echo),
            Some(ProjectionKind::EchoSeal)
        );
        assert!(!spec_admits_entry(&ProjectionSpec::default(), &echo));
        let dialog = ProjectionSpec {
            dialog: true,
            ..ProjectionSpec::default()
        };
        assert!(spec_admits_entry(&dialog, &echo));

        // InterAgent rides the system lane (`SystemNote`) but is selected by
        // class, never rendered as assistant, never admitted by the razor.
        let inter = classed(
            "system",
            "task handed over",
            2,
            FrameClass::InterAgent {
                sender: "codex".into(),
                task: "t1".into(),
                message_type: "dispatch".into(),
            },
        );
        assert_eq!(
            projection_kind_for_entry(&inter),
            Some(ProjectionKind::InterAgent)
        );
        assert_eq!(
            projection_role_for_entry(&inter),
            Some(ProjectionRole::System)
        );
        assert!(!spec_admits_entry(&ProjectionSpec::default(), &inter));
        let only_inter = ProjectionSpec {
            kinds: vec![ProjectionKind::InterAgent],
            roles: vec![ProjectionRole::System],
            ..ProjectionSpec::default()
        };
        assert!(spec_admits_entry(&only_inter, &inter));

        // LineageMeta from the model is its own kind, not `Inject`.
        let lineage = classed(
            "system",
            "child",
            3,
            FrameClass::LineageMeta {
                session_id: "child".into(),
                forked_from_id: Some("parent".into()),
            },
        );
        assert_eq!(
            projection_kind_for_entry(&lineage),
            Some(ProjectionKind::LineageMeta)
        );
        // A class-less entry still goes through the string bridge.
        let mut plain = entry("system", "reminder", 4);
        plain.frame_kind = Some(FrameKind::SystemNote);
        assert_eq!(
            projection_kind_for_entry(&plain),
            Some(ProjectionKind::Inject)
        );
    }

    /// Fixture shaped after `fork-vs-parent-user-messages-side-by-side.md`
    /// (2026-08-27): parent `b92324bf` and fork `fbf5c7b2` share a 12-record
    /// prefix with identical UUIDs; the fork continues with its own turns
    /// and the parent with its own. The shared prefix is counted once.
    #[test]
    fn fork_prefix_is_inherited_once_and_parent_branch_is_tagged() {
        let shared: Vec<TimelineEntry> = (0..12)
            .map(|i| entry("user", &format!("shared prefix record {i}"), 1_000 + i))
            .collect();
        let mut fork = shared.clone();
        fork.extend((0..5).map(|i| entry("user", &format!("fork only {i}"), 5_000 + i)));
        let mut parent = shared.clone();
        parent.extend((0..3).map(|i| entry("user", &format!("parent only {i}"), 6_000 + i)));
        for entry in &mut fork {
            entry.session_id = "fbf5c7b2".into();
        }
        for entry in &mut parent {
            entry.session_id = "b92324bf".into();
        }

        let parent_ref = claude_ref("b92324bf");
        let merged = merge_inherited(fork, parent, &parent_ref, LineageEdgeKind::SharedPrefix);
        assert_eq!(merged.inherited_from_parent, 12);
        assert_eq!(merged.parent_only, 3);
        assert_eq!(
            merged.entries.len(),
            12 + 5 + 3,
            "nothing doubled, nothing dropped"
        );
        let inherited = merged
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.lineage_origin,
                    Some(EntryOrigin::InheritedFrom { ref via, .. }) if via == "shared_prefix"
                )
            })
            .count();
        assert_eq!(inherited, 12);
        assert_eq!(
            merged
                .entries
                .iter()
                .filter(|entry| matches!(entry.lineage_origin, Some(EntryOrigin::Own)))
                .count(),
            5
        );
        assert_eq!(
            merged
                .entries
                .iter()
                .filter(|entry| matches!(
                    entry.lineage_origin,
                    Some(EntryOrigin::ParentOnly { .. })
                ))
                .count(),
            3
        );
        // Every inherited record names the parent by its tagged ref.
        assert!(
            merged
                .entries
                .iter()
                .all(|entry| match &entry.lineage_origin {
                    Some(EntryOrigin::InheritedFrom { conversation, .. }) =>
                        conversation == &parent_ref,
                    _ => true,
                })
        );
    }

    #[test]
    fn mixed_scope_refuses_single_history_by_default() {
        let mut a = entry("user", "work on LV1", 1);
        a.cwd = Some("/repos/loctree-suite".into());
        a.branch = Some("main".into());
        let mut b = entry("user", "fix the installer", 2);
        b.cwd = Some("/repos/vibecrafted".into());
        b.branch = Some("main".into());
        let report = scope_report_for_entries(&[a.clone(), b]);
        assert_eq!(report.status, ScopeStatus::MixedCandidate);
        assert_eq!(report.cwds.len(), 2);
        let refusal = refuse_mixed_workstream(AgentKind::Claude, "s1", &report, false)
            .expect("mixed candidate refuses by default");
        assert_eq!(refusal.tag(), "mixed_workstream");
        assert!(refuse_mixed_workstream(AgentKind::Claude, "s1", &report, true).is_none());

        let homogeneous = scope_report_for_entries(&[a]);
        assert_eq!(homogeneous.status, ScopeStatus::NoDriftObserved);
        assert!(refuse_mixed_workstream(AgentKind::Claude, "s1", &homogeneous, false).is_none());
        let unknown = scope_report_for_entries(&[entry("user", "no cwd", 3)]);
        assert_eq!(unknown.status, ScopeStatus::Unknown);
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
        let full = ProjectionSpec {
            result: ResultBody::Full,
            ..ProjectionSpec::default()
        };
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
