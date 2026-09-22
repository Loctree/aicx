//! Canonical runtime bridge from an already selected source to the parser engine.
//!
//! Discovery and id resolution stay in `session_catalog`; this module only
//! converts a finite source selection into `SourceHandle`, parses it once
//! through the exhaustive adapter registry, and exposes the typed model.

use anyhow::{Result, anyhow};
use std::fmt;
use std::path::Path;

use aicx_parser::engine::{
    AgentKind, ParserEngine, SkippedReason, SourceArtifact, SourceFraming, SourceHandle,
    ValidatedParse, ValidatedSession,
};
use std::collections::BTreeMap;

/// Build the sealed parser input for one already-selected regular file.
pub fn source_handle_for_file(
    agent: AgentKind,
    source_id: &str,
    logical_session_id: Option<String>,
    path: &Path,
) -> Result<SourceHandle> {
    let artifact_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.starts_with('.') && !name.contains(['/', '\\']))
        .unwrap_or("source.jsonl")
        .to_string();
    let framing = if agent == AgentKind::Gemini
        && path.extension().and_then(|ext| ext.to_str()) == Some("json")
    {
        SourceFraming::WholeDocument
    } else {
        SourceFraming::JsonLines
    };
    let artifact = SourceArtifact::validated_file(artifact_name, path, framing)
        .map_err(|error| anyhow!("invalid source artifact: {error}"))?;
    SourceHandle::new(
        agent,
        safe_source_id(source_id),
        logical_session_id,
        vec![artifact],
    )
    .map_err(|error| anyhow!("invalid source handle: {error}"))
}

/// Parse exactly one selected file through `SourceHandle -> ParserEngine -> adapter`.
pub fn parse_file(
    agent: AgentKind,
    source_id: &str,
    logical_session_id: Option<String>,
    path: &Path,
) -> Result<ValidatedSession> {
    let handle = source_handle_for_file(agent, source_id, logical_session_id, path)?;
    parse_handle(&handle)
}

/// Why a parse produced no session, carrying the coverage evidence that
/// decided it.
///
/// [`parse_handle`] flattens this to one string, which is right for the
/// single-file path: the operator selected one source, so the message is the
/// whole report. A bulk pass cannot afford that flattening. Across thousands
/// of discovered sources, "the adapter framed nothing at all" and "the adapter
/// framed some units and still refused" are different facts, and reporting
/// them as one trains operators to ignore real failures.
#[derive(Debug)]
pub enum ParseRefusal {
    /// The engine ran to completion and refused to call the result a session.
    Fatal {
        message: String,
        raw_unit_count: u64,
        consumed_count: u64,
        /// The adapter's own ledger of what it knowingly skipped and why.
        skipped_by_reason: BTreeMap<SkippedReason, u64>,
    },
    /// The engine could not run at all: reader, IO or registry error.
    Error(anyhow::Error),
}

impl ParseRefusal {
    /// Is this source simply *not a conversation of this provider*?
    ///
    /// Zero consumption alone does not prove it. Measured on a real gemini
    /// tree (2026-09-10): a 281 MB `chats/session-*.json` holding 158 genuine
    /// messages consumes zero units, exactly like a `logs.json` that never was
    /// a session. Calling both "unsupported" would hide real lost sessions
    /// behind a clean exit code.
    ///
    /// The adapter's own ledger does separate them. [`SkippedReason::Malformed`]
    /// and [`SkippedReason::Oversized`] are verdicts on something the adapter
    /// *recognized* and could not deliver — that is a failure. Every other
    /// reason means it never claimed the payload as its own.
    ///
    /// Matched exhaustively on purpose: a new [`SkippedReason`] must not
    /// silently inherit either verdict.
    pub fn is_structural_non_match(&self) -> bool {
        let Self::Fatal {
            consumed_count: 0,
            skipped_by_reason,
            ..
        } = self
        else {
            return false;
        };
        !skipped_by_reason
            .iter()
            .any(|(reason, count)| *count > 0 && recognized_but_undeliverable(*reason))
    }

    /// Units the reader produced and the adapter consumed, when the engine got
    /// far enough to count them.
    pub fn evidence(&self) -> Option<(u64, u64, String)> {
        match self {
            Self::Fatal {
                raw_unit_count,
                consumed_count,
                skipped_by_reason,
                ..
            } => Some((
                *raw_unit_count,
                *consumed_count,
                skipped_by_reason
                    .iter()
                    .map(|(reason, count)| format!("{reason:?}={count}"))
                    .collect::<Vec<_>>()
                    .join(","),
            )),
            Self::Error(_) => None,
        }
    }
}

impl fmt::Display for ParseRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fatal { message, .. } => formatter.write_str(message),
            Self::Error(error) => write!(formatter, "{error}"),
        }
    }
}

impl From<ParseRefusal> for anyhow::Error {
    fn from(refusal: ParseRefusal) -> Self {
        match refusal {
            ParseRefusal::Fatal { message, .. } => anyhow!(message),
            ParseRefusal::Error(error) => error,
        }
    }
}

/// Did the adapter recognize this unit as its own and still fail to deliver
/// it? Then the source is a session that was lost, not a foreign file.
const fn recognized_but_undeliverable(reason: SkippedReason) -> bool {
    match reason {
        // Recognized: the adapter framed the unit, then a size cap or damaged
        // bytes stopped it. The session existed and did not arrive.
        SkippedReason::Malformed | SkippedReason::Oversized => true,
        // Never claimed as this provider's speech.
        SkippedReason::UnknownPayloadType
        | SkippedReason::EncryptedOpaque
        | SkippedReason::Unsupported
        // Deliberate, healthy skips: they only appear alongside consumption.
        | SkippedReason::CompactionReplay
        | SkippedReason::DuplicateBody => false,
    }
}

/// Parse an already-built finite handle without reopening discovery.
pub fn parse_handle(handle: &SourceHandle) -> Result<ValidatedSession> {
    parse_handle_detailed(handle).map_err(anyhow::Error::from)
}

/// [`parse_handle`], keeping the refusal typed.
///
/// The `Fatal` message is byte-identical to what [`parse_handle`] returns, so
/// callers that only render it — and the in-flight heuristic that matches on
/// it — see no change.
pub fn parse_handle_detailed(handle: &SourceHandle) -> Result<ValidatedSession, ParseRefusal> {
    match ParserEngine::default()
        .parse_registered(handle)
        .map_err(|error| ParseRefusal::Error(error.into()))?
    {
        ValidatedParse::Session(session) => Ok(*session),
        ValidatedParse::Fatal(fatal) => {
            let coverage = fatal.coverage();
            Err(ParseRefusal::Fatal {
                message: format!(
                    "session parse failed with {:?} completeness",
                    coverage.status.visible_completeness
                ),
                raw_unit_count: coverage.raw_unit_count,
                consumed_count: coverage.consumed_count,
                skipped_by_reason: coverage.known_skipped.clone(),
            })
        }
    }
}

fn safe_source_id(value: &str) -> String {
    let mut safe = String::with_capacity(value.len());
    let mut separator = false;
    for ch in value.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            ch
        } else {
            '_'
        };
        if mapped == '_' {
            if !separator {
                safe.push('_');
            }
            separator = true;
        } else {
            safe.push(mapped);
            separator = false;
        }
    }
    let safe = safe.trim_matches(['.', '_']);
    if safe.is_empty() {
        "session".to_owned()
    } else {
        safe.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fatal(consumed: u64, skipped: &[(SkippedReason, u64)]) -> ParseRefusal {
        ParseRefusal::Fatal {
            message: "session parse failed with Fatal completeness".to_owned(),
            raw_unit_count: 1,
            consumed_count: consumed,
            skipped_by_reason: skipped.iter().copied().collect(),
        }
    }

    #[test]
    fn a_recognized_session_the_run_could_not_deliver_is_never_called_unsupported() {
        // Measured on a real gemini tree: a 281 MB `chats/session-*.json`
        // holding 158 genuine messages consumes zero units and is skipped
        // `Oversized`. Calling that "unsupported" would hide a lost session
        // behind a clean exit code.
        assert!(!fatal(0, &[(SkippedReason::Oversized, 1)]).is_structural_non_match());
        assert!(!fatal(0, &[(SkippedReason::Malformed, 1)]).is_structural_non_match());
        // One recognized unit is enough, even buried among foreign payloads.
        assert!(
            !fatal(
                0,
                &[
                    (SkippedReason::UnknownPayloadType, 40),
                    (SkippedReason::Malformed, 1)
                ]
            )
            .is_structural_non_match()
        );
    }

    #[test]
    fn a_payload_no_adapter_claims_is_unsupported_not_failed() {
        // `logs.json`, checkpoints and state files that merely live in the
        // provider's directory.
        assert!(fatal(0, &[(SkippedReason::UnknownPayloadType, 1)]).is_structural_non_match());
        assert!(fatal(0, &[(SkippedReason::UnknownPayloadType, 24)]).is_structural_non_match());
        assert!(fatal(0, &[(SkippedReason::EncryptedOpaque, 3)]).is_structural_non_match());
    }

    #[test]
    fn consumption_outranks_the_skip_ledger() {
        // The adapter produced speech and still refused: that is a session
        // question, not a "wrong file" question.
        assert!(!fatal(5, &[(SkippedReason::UnknownPayloadType, 1)]).is_structural_non_match());
        // Zero units read at all is not a claim of non-membership either.
        assert!(fatal(0, &[]).is_structural_non_match());
        // An engine-level error carries no ledger, so it can never be one.
        assert!(!ParseRefusal::Error(anyhow!("reader exploded")).is_structural_non_match());
    }

    #[test]
    fn the_undeliverable_verdict_is_stated_for_every_skip_reason() {
        // Exhaustive by construction: adding a variant breaks this list, so a
        // new reason cannot silently inherit either verdict.
        for (reason, undeliverable) in [
            (SkippedReason::Malformed, true),
            (SkippedReason::Oversized, true),
            (SkippedReason::UnknownPayloadType, false),
            (SkippedReason::EncryptedOpaque, false),
            (SkippedReason::Unsupported, false),
            (SkippedReason::CompactionReplay, false),
            (SkippedReason::DuplicateBody, false),
        ] {
            assert_eq!(
                recognized_but_undeliverable(reason),
                undeliverable,
                "{reason:?} has no stated verdict"
            );
        }
    }

    #[test]
    fn the_fatal_message_is_unchanged_for_string_only_callers() {
        // `is_in_flight_failure` matches on this text; so do two extraction
        // tests. The typed variant must not shift it.
        assert_eq!(
            fatal(0, &[]).to_string(),
            "session parse failed with Fatal completeness"
        );
    }

    #[test]
    fn source_ids_are_validated_before_the_parser_boundary() {
        assert_eq!(safe_source_id("rollout/a b"), "rollout_a_b");
        assert_eq!(safe_source_id("../"), "session");
    }
}
