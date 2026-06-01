#![allow(unused_imports)]
use super::files::MAX_LINE_BYTES;
use super::*;

pub(crate) fn describe_content_sanitization_warning(
    warning: &sanitize::ContentSanitizationWarning,
) -> String {
    match warning {
        sanitize::ContentSanitizationWarning::NullByteStripped(offset) => {
            format!("stripped NUL byte at byte offset {offset}")
        }
        sanitize::ContentSanitizationWarning::BidiOverride(ch, offset) => format!(
            "preserved bidi override U+{:04X} at byte offset {}",
            *ch as u32, offset
        ),
        sanitize::ContentSanitizationWarning::ZeroWidth(ch, offset) => format!(
            "preserved zero-width character U+{:04X} at byte offset {}",
            *ch as u32, offset
        ),
    }
}

pub(crate) fn push_unique_sample(samples: &mut Vec<String>, sample: String, max: usize) {
    if samples.len() < max && !samples.iter().any(|existing| existing == &sample) {
        samples.push(sample);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaudeSessionWarning {
    MissingSessionId {
        fallback: String,
    },
    SessionIdDrift {
        first: String,
        ignored: Vec<String>,
    },
    UnparsableTimestamp {
        count: usize,
        samples: Vec<String>,
    },
    FallbackTimestamp {
        count: usize,
        samples: Vec<String>,
    },
    InvalidEpochMillis {
        count: usize,
        samples: Vec<String>,
    },
    OversizedLine {
        count: usize,
        samples: Vec<String>,
    },
    ContentSanitization {
        warning: sanitize::ContentSanitizationWarning,
    },
}

impl ClaudeSessionWarning {
    pub(crate) fn describe(&self, path: &Path) -> String {
        match self {
            ClaudeSessionWarning::MissingSessionId { fallback } => format!(
                "Claude session warning: {} has no non-empty sessionId; using `{}` fallback",
                path.display(),
                fallback
            ),
            ClaudeSessionWarning::SessionIdDrift { first, ignored } => format!(
                "Claude session warning: {} has multiple sessionId values; using `{}` and ignoring {}",
                path.display(),
                first,
                ignored.join(", ")
            ),
            ClaudeSessionWarning::UnparsableTimestamp { count, samples } => format!(
                "Claude session warning: {} has {} unparsable timestamp(s); frames dropped. Sample(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            ClaudeSessionWarning::FallbackTimestamp { count, samples } => format!(
                "Claude session warning: {} has {} frames preserved with fallback timestamp; sample lines: {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            ClaudeSessionWarning::InvalidEpochMillis { count, samples } => format!(
                "Claude history warning: {} has {} invalid epoch millisecond timestamp(s); frames dropped. Sample(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            ClaudeSessionWarning::OversizedLine { count, samples } => format!(
                "Claude session warning: {} skipped {} oversized JSONL line(s) over {} bytes. Sample(s): {}",
                path.display(),
                count,
                MAX_LINE_BYTES,
                samples.join(", ")
            ),
            ClaudeSessionWarning::ContentSanitization { warning } => format!(
                "Claude content warning: {} {}",
                path.display(),
                describe_content_sanitization_warning(warning)
            ),
        }
    }
}

pub(crate) fn emit_claude_session_warnings(path: &Path, warnings: &[ClaudeSessionWarning]) {
    use crate::diagnostics::{self, DiagnosticKind};
    for warning in warnings {
        let line = warning.describe(path);
        diagnostics::log_describe(&line);
        match warning {
            ClaudeSessionWarning::FallbackTimestamp { count, .. } => {
                diagnostics::record("claude", DiagnosticKind::FallbackTimestamp, *count, path);
            }
            ClaudeSessionWarning::UnparsableTimestamp { count, .. } => {
                diagnostics::record("claude", DiagnosticKind::UnparsableTimestamp, *count, path);
            }
            ClaudeSessionWarning::InvalidEpochMillis { count, .. } => {
                diagnostics::record("claude", DiagnosticKind::InvalidEpochMillis, *count, path);
            }
            ClaudeSessionWarning::OversizedLine { count, .. } => {
                diagnostics::record("claude", DiagnosticKind::OversizedLine, *count, path);
            }
            ClaudeSessionWarning::MissingSessionId { .. } => {
                diagnostics::record("claude", DiagnosticKind::MissingSessionId, 1, path);
            }
            ClaudeSessionWarning::SessionIdDrift { .. } => {
                diagnostics::record("claude", DiagnosticKind::SessionIdDrift, 1, path);
            }
            ClaudeSessionWarning::ContentSanitization { warning } => {
                record_content_sanitization("claude", warning, path);
            }
        }
        if diagnostics::is_verbose() {
            eprintln!("{line}");
        }
    }
}

pub(crate) fn record_content_sanitization(
    extractor: &'static str,
    warning: &sanitize::ContentSanitizationWarning,
    path: &Path,
) {
    use crate::diagnostics::{self, DiagnosticKind};
    let kind = match warning {
        sanitize::ContentSanitizationWarning::BidiOverride(_, _) => DiagnosticKind::BidiOverride,
        sanitize::ContentSanitizationWarning::ZeroWidth(_, _) => DiagnosticKind::ZeroWidth,
        sanitize::ContentSanitizationWarning::NullByteStripped(_) => {
            DiagnosticKind::NullByteStripped
        }
    };
    diagnostics::record(extractor, kind, 1, path);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GeminiSessionWarning {
    MissingSessionId {
        fallback: String,
    },
    SessionIdDrift {
        first: String,
        ignored: Vec<String>,
    },
    UnparsableTimestamp {
        count: usize,
        samples: Vec<String>,
    },
    UnknownMsgType {
        count: usize,
        samples: Vec<String>,
    },
    ContentSanitization {
        warning: sanitize::ContentSanitizationWarning,
    },
}

impl GeminiSessionWarning {
    pub(crate) fn describe(&self, path: &Path) -> String {
        match self {
            GeminiSessionWarning::MissingSessionId { fallback } => format!(
                "Gemini session warning: {} has no non-empty sessionId; using `{}` fallback",
                path.display(),
                fallback
            ),
            GeminiSessionWarning::SessionIdDrift { first, ignored } => format!(
                "Gemini session warning: {} has multiple sessionId values; using `{}` and ignoring {}",
                path.display(),
                first,
                ignored.join(", ")
            ),
            GeminiSessionWarning::UnparsableTimestamp { count, samples } => format!(
                "Gemini session warning: {} has {} unparsable timestamp(s); frames dropped or fell back to parent timestamp. Sample(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            GeminiSessionWarning::UnknownMsgType { count, samples } => format!(
                "Gemini session warning: {} encountered {} message(s) with unrecognized type/role; preserved as system_note. Sample(s): {}",
                path.display(),
                count,
                samples.join(", ")
            ),
            GeminiSessionWarning::ContentSanitization { warning } => format!(
                "Gemini content warning: {} {}",
                path.display(),
                describe_content_sanitization_warning(warning)
            ),
        }
    }
}

pub(crate) fn emit_gemini_session_warnings(path: &Path, warnings: &[GeminiSessionWarning]) {
    use crate::diagnostics::{self, DiagnosticKind};
    for warning in warnings {
        let line = warning.describe(path);
        diagnostics::log_describe(&line);
        match warning {
            GeminiSessionWarning::UnparsableTimestamp { count, .. } => {
                diagnostics::record("gemini", DiagnosticKind::UnparsableTimestamp, *count, path);
            }
            GeminiSessionWarning::UnknownMsgType { count, .. } => {
                diagnostics::record("gemini", DiagnosticKind::UnknownMsgType, *count, path);
            }
            GeminiSessionWarning::MissingSessionId { .. } => {
                diagnostics::record("gemini", DiagnosticKind::MissingSessionId, 1, path);
            }
            GeminiSessionWarning::SessionIdDrift { .. } => {
                diagnostics::record("gemini", DiagnosticKind::SessionIdDrift, 1, path);
            }
            GeminiSessionWarning::ContentSanitization { warning } => {
                record_content_sanitization("gemini", warning, path);
            }
        }
        if diagnostics::is_verbose() {
            eprintln!("{line}");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JunieSessionWarning {
    JunieFallbackId {
        fallback: String,
    },
    OversizedLine {
        count: usize,
        samples: Vec<String>,
    },
    ContentSanitization {
        warning: sanitize::ContentSanitizationWarning,
    },
}

impl JunieSessionWarning {
    pub(crate) fn describe(&self, path: &Path) -> String {
        match self {
            JunieSessionWarning::JunieFallbackId { fallback } => format!(
                "Junie session warning: {} has no session-* ancestor; using `{}` fallback",
                path.display(),
                fallback
            ),
            JunieSessionWarning::OversizedLine { count, samples } => format!(
                "Junie session warning: {} skipped {} oversized JSONL line(s) over {} bytes. Sample(s): {}",
                path.display(),
                count,
                MAX_LINE_BYTES,
                samples.join(", ")
            ),
            JunieSessionWarning::ContentSanitization { warning } => format!(
                "Junie content warning: {} {}",
                path.display(),
                describe_content_sanitization_warning(warning)
            ),
        }
    }
}

pub(crate) fn emit_junie_session_warnings(path: &Path, warnings: &[JunieSessionWarning]) {
    use crate::diagnostics::{self, DiagnosticKind};
    for warning in warnings {
        let line = warning.describe(path);
        diagnostics::log_describe(&line);
        match warning {
            JunieSessionWarning::JunieFallbackId { .. } => {
                diagnostics::record("junie", DiagnosticKind::JunieFallbackId, 1, path);
            }
            JunieSessionWarning::OversizedLine { count, .. } => {
                diagnostics::record("junie", DiagnosticKind::OversizedLine, *count, path);
            }
            JunieSessionWarning::ContentSanitization { warning } => {
                record_content_sanitization("junie", warning, path);
            }
        }
        if diagnostics::is_verbose() {
            eprintln!("{line}");
        }
    }
}

impl PushContentSanitizationWarning for Vec<ClaudeSessionWarning> {
    fn push_content_sanitization_warning(&mut self, warning: sanitize::ContentSanitizationWarning) {
        self.push(ClaudeSessionWarning::ContentSanitization { warning });
    }
}

impl PushContentSanitizationWarning for Vec<GeminiSessionWarning> {
    fn push_content_sanitization_warning(&mut self, warning: sanitize::ContentSanitizationWarning) {
        self.push(GeminiSessionWarning::ContentSanitization { warning });
    }
}

impl PushContentSanitizationWarning for Vec<JunieSessionWarning> {
    fn push_content_sanitization_warning(&mut self, warning: sanitize::ContentSanitizationWarning) {
        self.push(JunieSessionWarning::ContentSanitization { warning });
    }
}
