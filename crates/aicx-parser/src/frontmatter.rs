//! YAML frontmatter parser for markdown reports.

use serde::Deserialize;

use crate::timeline::FrameKind;

/// Parsed frontmatter fields from an agent report.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ReportFrontmatter {
    #[serde(default, flatten)]
    pub telemetry: ReportFrontmatterTelemetry,
    #[serde(default, flatten)]
    pub steering: ReportFrontmatterSteering,
}

/// Passive report telemetry preserved for downstream analytics and correlation.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ReportFrontmatterTelemetry {
    pub agent: Option<String>,
    pub run_id: Option<String>,
    pub prompt_id: Option<String>,
    pub status: Option<String>,
    pub frame_kind: Option<FrameKind>,
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub token_usage: Option<u64>,
    pub findings_count: Option<u32>,
    /// Foreign-import provenance (operator-md and similar). Structural, so it
    /// reaches the sidecar instead of living as body text (Round II / oś 3+5).
    pub source_file: Option<String>,
    pub source_format: Option<String>,
    /// Stable content-hash identity for the imported source, so re-importing the
    /// same material (even moved/renamed) does not create a fresh record.
    pub import_id: Option<String>,
}

/// Small, stable steering metadata that can route retrieval and framework behavior.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ReportFrontmatterSteering {
    #[serde(alias = "phase")]
    pub workflow_phase: Option<String>,
    pub mode: Option<String>,
    #[serde(alias = "skill")]
    pub skill_code: Option<String>,
    pub framework_version: Option<String>,
}

fn split_block(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    let after_open = &trimmed[3..];
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);

    let end = after_open.find("\n---")?;
    let yaml_str = &after_open[..end];
    let body_start = end + 4; // skip "\n---"
    let body = after_open[body_start..]
        .strip_prefix('\n')
        .unwrap_or(&after_open[body_start..]);

    Some((yaml_str, body))
}

/// Split markdown text into optional frontmatter and body.
/// Returns `(Some(frontmatter), body)` if frontmatter exists, else `(None, full text)`.
pub fn parse(text: &str) -> (Option<ReportFrontmatter>, &str) {
    let Some((yaml_str, body)) = split_block(text) else {
        return (None, text);
    };

    let frontmatter = parse_frontmatter_fields(yaml_str);
    (frontmatter, body)
}

fn parse_frontmatter_fields(yaml_str: &str) -> Option<ReportFrontmatter> {
    let mut parsed = ReportFrontmatter::default();
    let mut saw_field = false;

    for raw_line in yaml_str.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (key, value) = line.split_once(':')?;
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            return None;
        }

        let value = value.trim();
        if looks_like_unsupported_yaml_value(value) {
            continue;
        }
        let value = normalize_scalar(value);
        saw_field = true;

        match key {
            "agent" => parsed.telemetry.agent = string_value(value),
            "run_id" => parsed.telemetry.run_id = string_value(value),
            "prompt_id" => parsed.telemetry.prompt_id = string_value(value),
            "status" => parsed.telemetry.status = string_value(value),
            "frame_kind" => parsed.telemetry.frame_kind = FrameKind::parse(value),
            "model" => parsed.telemetry.model = string_value(value),
            "started_at" => parsed.telemetry.started_at = string_value(value),
            "completed_at" => parsed.telemetry.completed_at = string_value(value),
            "token_usage" => parsed.telemetry.token_usage = value.parse::<u64>().ok(),
            "findings_count" => parsed.telemetry.findings_count = value.parse::<u32>().ok(),
            "source_file" => parsed.telemetry.source_file = string_value(value),
            "source_format" => parsed.telemetry.source_format = string_value(value),
            "import_id" => parsed.telemetry.import_id = string_value(value),
            "workflow_phase" | "phase" => parsed.steering.workflow_phase = string_value(value),
            "mode" => parsed.steering.mode = string_value(value),
            "skill_code" | "skill" => parsed.steering.skill_code = string_value(value),
            "framework_version" => parsed.steering.framework_version = string_value(value),
            _ => {}
        }
    }

    saw_field.then_some(parsed)
}

fn normalize_scalar(value: &str) -> &str {
    value.trim().trim_matches(|ch| matches!(ch, '"' | '\''))
}

fn looks_like_unsupported_yaml_value(value: &str) -> bool {
    value.starts_with('[') || value.starts_with('{')
}

fn string_value(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_frontmatter() {
        let input = "---\nagent: codex\nrun_id: mrbl-001\nprompt_id: api-redesign_20260327\nstatus: completed\nframe_kind: agent_reply\nphase: implement\nmode: session-first\nskill: vc-workflow\nframework_version: 2026-03\n---\n# Report\nContent here";
        let (frontmatter, body) = parse(input);
        let frontmatter = frontmatter.unwrap();

        assert_eq!(frontmatter.telemetry.agent.as_deref(), Some("codex"));
        assert_eq!(frontmatter.telemetry.run_id.as_deref(), Some("mrbl-001"));
        assert_eq!(
            frontmatter.telemetry.prompt_id.as_deref(),
            Some("api-redesign_20260327")
        );
        assert_eq!(frontmatter.telemetry.status.as_deref(), Some("completed"));
        assert_eq!(
            frontmatter.telemetry.frame_kind,
            Some(FrameKind::AgentReply)
        );
        assert_eq!(
            frontmatter.steering.workflow_phase.as_deref(),
            Some("implement")
        );
        assert_eq!(frontmatter.steering.mode.as_deref(), Some("session-first"));
        assert_eq!(
            frontmatter.steering.skill_code.as_deref(),
            Some("vc-workflow")
        );
        assert_eq!(
            frontmatter.steering.framework_version.as_deref(),
            Some("2026-03")
        );
        assert!(body.starts_with("# Report"));
    }

    #[test]
    fn returns_none_for_no_frontmatter() {
        let input = "# Just a report\nNo frontmatter here";
        let (frontmatter, body) = parse(input);

        assert!(frontmatter.is_none());
        assert_eq!(body, input);
    }

    #[test]
    fn handles_malformed_yaml_gracefully() {
        let input = "---\nagent: codex\nrun_id: mrbl-001\nunsupported: [not, scalar]\nstatus: completed\n---\nBody";
        let (frontmatter, body) = parse(input);
        let frontmatter = frontmatter.unwrap();

        assert_eq!(frontmatter.telemetry.agent.as_deref(), Some("codex"));
        assert_eq!(frontmatter.telemetry.run_id.as_deref(), Some("mrbl-001"));
        assert_eq!(frontmatter.telemetry.status.as_deref(), Some("completed"));
        assert_eq!(body, "Body");
    }

    #[test]
    fn preserves_scalars_when_value_is_unsupported_yaml_list() {
        let input = "---\nrun_id: abc123\nmode: [research, audit]\nstatus: completed\nmodel: claude-opus-4-7\nstarted_at: 2026-05-20T11:51:55Z\n---\nBody";
        let (frontmatter, body) = parse(input);
        let frontmatter = frontmatter.unwrap();

        assert_eq!(frontmatter.telemetry.run_id.as_deref(), Some("abc123"));
        assert_eq!(frontmatter.telemetry.status.as_deref(), Some("completed"));
        assert_eq!(
            frontmatter.telemetry.model.as_deref(),
            Some("claude-opus-4-7")
        );
        assert_eq!(
            frontmatter.telemetry.started_at.as_deref(),
            Some("2026-05-20T11:51:55Z")
        );
        assert_eq!(frontmatter.steering.mode, None);
        assert_eq!(body, "Body");
    }

    #[test]
    fn preserves_scalars_when_value_is_unsupported_yaml_map() {
        let input = "---\nrun_id: abc123\nagent_model: {provider}/claude-opus-4-7\nstatus: completed\nmodel: codex\n---\nBody";
        let (frontmatter, body) = parse(input);
        let frontmatter = frontmatter.unwrap();

        assert_eq!(frontmatter.telemetry.run_id.as_deref(), Some("abc123"));
        assert_eq!(frontmatter.telemetry.status.as_deref(), Some("completed"));
        assert_eq!(frontmatter.telemetry.model.as_deref(), Some("codex"));
        assert_eq!(body, "Body");
    }

    #[test]
    fn still_returns_none_when_no_delimiters() {
        let input = "run_id: abc123\nstatus: completed\nBody";
        let (frontmatter, body) = parse(input);

        assert!(frontmatter.is_none());
        assert_eq!(body, input);
    }
}
