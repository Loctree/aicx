//! Canonical runtime bridge from an already selected source to the parser engine.
//!
//! Discovery and id resolution stay in `session_catalog`; this module only
//! converts a finite source selection into `SourceHandle`, parses it once
//! through the exhaustive adapter registry, and exposes the typed model.

use anyhow::{Result, anyhow, bail};
use std::path::Path;

use aicx_parser::engine::{
    AgentKind, ParserEngine, SourceArtifact, SourceFraming, SourceHandle, ValidatedParse,
    ValidatedSession,
};

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

/// Parse an already-built finite handle without reopening discovery.
pub fn parse_handle(handle: &SourceHandle) -> Result<ValidatedSession> {
    match ParserEngine::default().parse_registered(handle)? {
        ValidatedParse::Session(session) => Ok(*session),
        ValidatedParse::Fatal(fatal) => bail!(
            "session parse failed with {:?} completeness",
            fatal.coverage().status.visible_completeness
        ),
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

    #[test]
    fn source_ids_are_validated_before_the_parser_boundary() {
        assert_eq!(safe_source_id("rollout/a b"), "rollout_a_b");
        assert_eq!(safe_source_id("../"), "session");
    }
}
