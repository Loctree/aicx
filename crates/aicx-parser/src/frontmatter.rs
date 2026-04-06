//! YAML frontmatter parser for markdown reports.

use serde::Deserialize;

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
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub token_usage: Option<u64>,
    pub findings_count: Option<u32>,
}

/// Small, stable steering metadata that can route retrieval and framework behavior.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ReportFrontmatterSteering {
    #[serde(alias = "phase")]
    pub workflow_phase: Option<String>,
    pub mode: Option<String>,
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

    let frontmatter = serde_yaml::from_str::<ReportFrontmatter>(yaml_str).ok();
    (frontmatter, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_frontmatter() {
        let input = "---\nagent: codex\nrun_id: mrbl-001\nprompt_id: api-redesign_20260327\nphase: implement\nmode: session-first\nskill_code: vc-workflow\nframework_version: 2026-03\n---\n# Report\nContent here";
        let (frontmatter, body) = parse(input);
        let frontmatter = frontmatter.unwrap();

        assert_eq!(frontmatter.telemetry.agent.as_deref(), Some("codex"));
        assert_eq!(frontmatter.telemetry.run_id.as_deref(), Some("mrbl-001"));
        assert_eq!(
            frontmatter.telemetry.prompt_id.as_deref(),
            Some("api-redesign_20260327")
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
        let input = "---\n: this is not valid yaml [\n---\nBody";
        let (frontmatter, body) = parse(input);

        assert!(frontmatter.is_none());
        assert_eq!(body, "Body");
    }
}
