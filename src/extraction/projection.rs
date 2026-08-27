//! Output projection: the single place that decides what leaves the substrate.
//!
//! The throne (W1-T4) retains every classified frame, including full
//! `ShellAction` results (`Retained { text, chars, hash }`). This module is
//! the view over that substrate. Flags populate a [`ProjectionSpec`]; they
//! never rewrite stored frames, extracts, or the index.
//!
//! W1-T6 was structure only; consumers attached in W2-T13. Since W2-R1 the
//! model carries `FrameClass` on every throne-owned turn, and this module
//! maps that class — not `role`/`frame_kind` strings — onto
//! [`ProjectionKind`] (`ProjectionKind::from_frame_class`).

/// Unbounded parent walk when `--lineage` is passed without a depth.
pub const LINEAGE_UNBOUNDED: usize = usize::MAX;

/// `0` means "do not truncate" — same encoding as today's
/// `extract --max-message-chars` default.
pub const MAX_MESSAGE_CHARS_UNLIMITED: usize = 0;

/// Invariant carried in the type so W2-T13 cannot "rediscover" it in a
/// consumer. The contract document repeats the same sentence.
pub const FLAGS_NEVER_MUTATE_THE_SUBSTRATE: &str = "flags never mutate the substrate";

/// Speaker axis. Independent of [`ProjectionKind`]: a `Human` kind can still
/// be dropped when `roles` is `--user-only`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectionRole {
    Human,
    Assistant,
    System,
}

impl ProjectionRole {
    pub fn as_cli_token(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Assistant => "assistant",
            Self::System => "system",
        }
    }
}

/// Throne-kind filter (Decision 1 + Decision 9). Tokens are the destination
/// `--kind` vocabulary. Today's `search --frame-kind` maps through
/// [`ProjectionKind::from_legacy_frame_kind`]; today's `search --kind`
/// (conversations|plans|reports) is a **document class**, not this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectionKind {
    Human,
    EchoSeal,
    ShellAction,
    Inject,
    AssistantFinal,
    LineageMeta,
    InterAgent,
}

impl ProjectionKind {
    pub fn as_cli_token(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::EchoSeal => "echo_seal",
            Self::ShellAction => "shell_action",
            Self::Inject => "inject",
            Self::AssistantFinal => "assistant_final",
            Self::LineageMeta => "lineage_meta",
            Self::InterAgent => "inter_agent",
        }
    }

    pub fn from_cli_token(token: &str) -> Option<Self> {
        match token {
            "human" => Some(Self::Human),
            "echo_seal" | "echo-seal" => Some(Self::EchoSeal),
            "shell_action" | "shell-action" => Some(Self::ShellAction),
            "inject" => Some(Self::Inject),
            "assistant_final" | "assistant-final" => Some(Self::AssistantFinal),
            "lineage_meta" | "lineage-meta" => Some(Self::LineageMeta),
            "inter_agent" | "inter-agent" => Some(Self::InterAgent),
            _ => None,
        }
    }

    /// The throne class → projection kind. Total: every `FrameClass` has
    /// exactly one kind. This is the mapping consumers use once the model
    /// carries the class (W2-R1); the role/`frame_kind` bridge in
    /// `conversation.rs` is the fallback for class-less entries only.
    pub const fn from_frame_class(class: &aicx_parser::engine::FrameClass) -> Self {
        use aicx_parser::engine::FrameClass;
        match class {
            FrameClass::Human { .. } => Self::Human,
            FrameClass::EchoSeal { .. } => Self::EchoSeal,
            FrameClass::ShellAction { .. } => Self::ShellAction,
            FrameClass::Inject { .. } => Self::Inject,
            FrameClass::AssistantFinal => Self::AssistantFinal,
            FrameClass::LineageMeta { .. } => Self::LineageMeta,
            FrameClass::InterAgent { .. } => Self::InterAgent,
        }
    }

    /// Map the live `search --frame-kind` tokens onto the throne vocabulary.
    pub fn from_legacy_frame_kind(token: &str) -> Option<Self> {
        match token {
            "user_msg" => Some(Self::Human),
            "agent_reply" => Some(Self::AssistantFinal),
            "internal_thought" => Some(Self::Inject),
            "tool_call" => Some(Self::ShellAction),
            _ => None,
        }
    }
}

/// Human transport channel (Decision 7): the throne's type, re-exported so
/// the projection speaks the same vocabulary as the substrate. `--dialog`
/// reveals EchoBus/Queue as speech with their seals. (W2-R1 removed the
/// local twin of this enum.)
pub use aicx_parser::engine::HumanChannel;

/// How a retained `ShellAction` result is rendered. The bytes stay in the
/// substrate regardless of this choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResultBody {
    /// Command marker + line count + hash. Default.
    None,
    /// First `n` lines of the retained text, plus an omitted-count marker.
    Head(usize),
    /// The retained text in full (`--result full`).
    Full,
}

/// Time window: `-H` / `--hours`, `--since`, `--until` (and search `-d`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectionWindow {
    pub hours: Option<u64>,
    pub since: Option<String>,
    pub until: Option<String>,
}

impl ProjectionWindow {
    pub fn unbounded() -> Self {
        Self::default()
    }

    pub fn is_unbounded(&self) -> bool {
        self.hours.is_none() && self.since.is_none() && self.until.is_none()
    }
}

/// Sole decision record for what a consumer emits.
///
/// Empty `roles` / `kinds` vectors mean "emit nothing on that axis" — they
/// are not a synonym for default. Construct through [`ProjectionSpec::default`]
/// or [`ProjectionSpec::full`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSpec {
    pub roles: Vec<ProjectionRole>,
    pub kinds: Vec<ProjectionKind>,
    pub result: ResultBody,
    pub max_message_chars: usize,
    pub window: ProjectionWindow,
    pub project: Vec<String>,
    pub score: Option<u8>,
    pub dialog: bool,
    pub lineage_depth: Option<usize>,
}

impl Default for ProjectionSpec {
    fn default() -> Self {
        Self::razor()
    }
}

impl ProjectionSpec {
    /// Razor default (no flags): human speech + final answers + shell stubs.
    ///
    /// Echo-bus / queued speech is withheld until `--dialog`. `InterAgent`
    /// is withheld until `--kind inter_agent`. Result bodies stay hashed.
    pub fn razor() -> Self {
        Self {
            roles: vec![ProjectionRole::Human, ProjectionRole::Assistant],
            kinds: vec![
                ProjectionKind::Human,
                ProjectionKind::AssistantFinal,
                ProjectionKind::ShellAction,
            ],
            result: ResultBody::None,
            max_message_chars: MAX_MESSAGE_CHARS_UNLIMITED,
            window: ProjectionWindow::unbounded(),
            project: Vec::new(),
            score: None,
            dialog: false,
            lineage_depth: None,
        }
    }

    /// Fullness: every kind, every role, full result bodies, dialog channels
    /// visible, unbounded lineage. Still a view — the substrate is unchanged.
    pub fn full() -> Self {
        Self {
            roles: vec![
                ProjectionRole::Human,
                ProjectionRole::Assistant,
                ProjectionRole::System,
            ],
            kinds: vec![
                ProjectionKind::Human,
                ProjectionKind::EchoSeal,
                ProjectionKind::ShellAction,
                ProjectionKind::Inject,
                ProjectionKind::AssistantFinal,
                ProjectionKind::LineageMeta,
                ProjectionKind::InterAgent,
            ],
            result: ResultBody::Full,
            max_message_chars: MAX_MESSAGE_CHARS_UNLIMITED,
            window: ProjectionWindow::unbounded(),
            project: Vec::new(),
            score: None,
            dialog: true,
            lineage_depth: Some(LINEAGE_UNBOUNDED),
        }
    }

    pub fn emits_role(&self, role: ProjectionRole) -> bool {
        self.roles.contains(&role)
    }

    pub fn emits_kind(&self, kind: ProjectionKind) -> bool {
        if self.kinds.contains(&kind) {
            return true;
        }
        // `--dialog` reveals delayed human speech (echo bus / queue-operation)
        // as speech, without opening Inject / InterAgent / LineageMeta.
        if self.dialog && matches!(kind, ProjectionKind::EchoSeal) {
            return self.kinds.contains(&ProjectionKind::Human);
        }
        if self.lineage_depth.is_some() && kind == ProjectionKind::LineageMeta {
            return true;
        }
        false
    }

    pub fn emits_human_channel(&self, channel: HumanChannel) -> bool {
        if !self.emits_kind(ProjectionKind::Human) && !self.emits_kind(ProjectionKind::EchoSeal) {
            return false;
        }
        match channel {
            HumanChannel::Direct => self.emits_kind(ProjectionKind::Human),
            HumanChannel::EchoBus | HumanChannel::Queue => {
                self.dialog || self.emits_kind(ProjectionKind::EchoSeal)
            }
        }
    }

    /// Render a retained shell result according to `self.result`.
    ///
    /// `retained_text` and `sha256` are substrate facts. This function does
    /// not hash, store, or truncate the substrate — it only formats a view.
    pub fn project_shell_result(&self, command: &str, retained_text: &str, sha256: &str) -> String {
        let line_count = count_lines(retained_text);
        match self.result {
            ResultBody::None => format_shell_action_stub(command, line_count, sha256),
            ResultBody::Full => format_shell_action_full(command, retained_text),
            ResultBody::Head(n) => format_shell_action_head(command, retained_text, n, sha256),
        }
    }

    /// Truncate a dialogue body when `max_message_chars` is non-zero.
    /// Character count, not bytes; never panics on a UTF-8 boundary.
    pub fn project_message_body(&self, body: &str) -> String {
        if self.max_message_chars == MAX_MESSAGE_CHARS_UNLIMITED {
            return body.to_string();
        }
        let mut chars = body.chars();
        let taken: String = chars.by_ref().take(self.max_message_chars).collect();
        if chars.next().is_none() {
            taken
        } else {
            format!("{taken}…")
        }
    }
}

/// Default shell-action line. Example:
/// `$ cargo test … [412 lines, sha256:…]`
pub fn format_shell_action_stub(command: &str, line_count: usize, sha256: &str) -> String {
    let lines_word = if line_count == 1 { "line" } else { "lines" };
    format!("$ {command} [{line_count} {lines_word}, sha256:{sha256}]")
}

fn format_shell_action_full(command: &str, retained_text: &str) -> String {
    if retained_text.is_empty() {
        format!("$ {command}")
    } else {
        format!("$ {command}\n{retained_text}")
    }
}

fn format_shell_action_head(command: &str, retained_text: &str, n: usize, sha256: &str) -> String {
    let total = count_lines(retained_text);
    if n == 0 || total == 0 {
        return format_shell_action_stub(command, total, sha256);
    }
    let head: Vec<&str> = retained_text.lines().take(n).collect();
    let omitted = total.saturating_sub(head.len());
    if omitted == 0 {
        return format_shell_action_full(command, retained_text);
    }
    let omitted_word = if omitted == 1 { "line" } else { "lines" };
    format!(
        "$ {command}\n{}\n[+{omitted} {omitted_word} omitted, sha256:{sha256}]",
        head.join("\n")
    )
}

fn count_lines(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.lines().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn razor_default_is_human_plus_final_plus_shell_stub() {
        let spec = ProjectionSpec::default();
        assert_eq!(
            spec.roles,
            vec![ProjectionRole::Human, ProjectionRole::Assistant]
        );
        assert_eq!(
            spec.kinds,
            vec![
                ProjectionKind::Human,
                ProjectionKind::AssistantFinal,
                ProjectionKind::ShellAction,
            ]
        );
        assert_eq!(spec.result, ResultBody::None);
        assert_eq!(spec.max_message_chars, 0);
        assert!(spec.window.is_unbounded());
        assert!(spec.project.is_empty());
        assert_eq!(spec.score, None);
        assert!(!spec.dialog);
        assert_eq!(spec.lineage_depth, None);
        assert!(spec.emits_kind(ProjectionKind::ShellAction));
        assert!(!spec.emits_kind(ProjectionKind::InterAgent));
        assert!(!spec.emits_kind(ProjectionKind::EchoSeal));
        assert!(!spec.emits_human_channel(HumanChannel::EchoBus));
        assert_eq!(
            FLAGS_NEVER_MUTATE_THE_SUBSTRATE,
            "flags never mutate the substrate"
        );
    }

    #[test]
    fn dialog_reveals_echo_seal_without_opening_inter_agent() {
        let mut spec = ProjectionSpec::default();
        spec.dialog = true;
        assert!(spec.emits_kind(ProjectionKind::EchoSeal));
        assert!(spec.emits_human_channel(HumanChannel::Queue));
        assert!(!spec.emits_kind(ProjectionKind::InterAgent));
        assert!(!spec.emits_kind(ProjectionKind::Inject));
    }

    #[test]
    fn full_spec_opens_every_kind_and_full_results() {
        let spec = ProjectionSpec::full();
        assert_eq!(spec.result, ResultBody::Full);
        assert!(spec.dialog);
        assert_eq!(spec.lineage_depth, Some(LINEAGE_UNBOUNDED));
        assert!(spec.emits_kind(ProjectionKind::InterAgent));
        assert!(spec.emits_kind(ProjectionKind::LineageMeta));
        assert!(spec.emits_role(ProjectionRole::System));
    }

    #[test]
    fn shell_stub_matches_contract_example_shape() {
        let spec = ProjectionSpec::default();
        let rendered = spec.project_shell_result("cargo test …", "x\n".repeat(412).trim_end(), "…");
        assert_eq!(rendered, "$ cargo test … [412 lines, sha256:…]");
    }

    #[test]
    fn result_full_emits_command_then_body() {
        let spec = ProjectionSpec::full();
        let body = "running 3 tests\ntest a ... ok\ntest result: ok.";
        let rendered = spec.project_shell_result("cargo test --workspace", body, "deadbeef");
        assert_eq!(rendered, format!("$ cargo test --workspace\n{body}"));
    }

    #[test]
    fn result_head_keeps_hash_and_omitted_count() {
        let mut spec = ProjectionSpec::default();
        spec.result = ResultBody::Head(2);
        let body = "one\ntwo\nthree\nfour";
        let rendered = spec.project_shell_result("cargo test", body, "abc");
        assert_eq!(
            rendered,
            "$ cargo test\none\ntwo\n[+2 lines omitted, sha256:abc]"
        );
    }

    #[test]
    fn max_message_chars_is_character_safe() {
        let mut spec = ProjectionSpec::default();
        spec.max_message_chars = 3;
        assert_eq!(spec.project_message_body("żółw"), "żół…");
        spec.max_message_chars = 0;
        assert_eq!(spec.project_message_body("żółw"), "żółw");
    }

    #[test]
    fn kind_cli_tokens_round_trip() {
        for kind in [
            ProjectionKind::Human,
            ProjectionKind::EchoSeal,
            ProjectionKind::ShellAction,
            ProjectionKind::Inject,
            ProjectionKind::AssistantFinal,
            ProjectionKind::LineageMeta,
            ProjectionKind::InterAgent,
        ] {
            let token = kind.as_cli_token();
            assert_eq!(ProjectionKind::from_cli_token(token), Some(kind));
        }
        assert_eq!(
            ProjectionKind::from_legacy_frame_kind("user_msg"),
            Some(ProjectionKind::Human)
        );
        assert_eq!(
            ProjectionKind::from_legacy_frame_kind("tool_call"),
            Some(ProjectionKind::ShellAction)
        );
        assert_eq!(ProjectionKind::from_cli_token("conversations"), None);
    }
}
