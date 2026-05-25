//! Structured failure-as-state for the aicx CLI boundary.
//!
//! Per Wave B identity-vs-drift catalogue §1.2–§1.3 (2026-05-25): every
//! CLI-boundary error emits either a multi-line text block (text mode)
//! or a structured JSON envelope (`--json` / `AICX_JSON=1`). This
//! replaces bare `anyhow!` chains and Clap-default messages at the
//! user-facing surface.
//!
//! The identity exemplar already shipped in
//! [`crate::search_engine::SemanticError`] (kind / reason /
//! recommendation, with optional fallback command). This module makes
//! the same shape available to every other CLI handler so the failure
//! identity is uniform across the binary.
//!
//! ## Text envelope (rendered by [`StructuredFailure::render_text`])
//!
//! ```text
//! aicx <cmd> failed.
//!   kind:           <stable_kind_token>
//!   reason:         <one-line human reason>
//!   recommendation: <copy-pasteable next step>
//!   fallback:       <command>            # only when set
//! ```
//!
//! ## JSON envelope (rendered by [`StructuredFailure::render_json`])
//!
//! ```json
//! {
//!   "ok": false,
//!   "error": "aicx <cmd> failed",
//!   "kind": "...",
//!   "reason": "...",
//!   "recommendation": "...",
//!   "fallback": { "available": true, "command": "..." }
//! }
//! ```
//!
//! The `kind` token is a stable snake_case identifier that machine
//! consumers (vibecrafted-mcp wrappers, dashboards, marbles workers)
//! can match on. The `reason` and `recommendation` strings are
//! human-readable and **may** change across releases.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use serde::Serialize;
use serde_json::json;

/// Optional follow-up command rendered alongside the recommendation.
#[derive(Debug, Clone, Serialize)]
pub struct FallbackCommand {
    /// Whether the fallback command is actually safe to run as-is.
    /// Reserved for future use (e.g. deferred fallbacks that depend on
    /// operator state); currently always `true` when the field is
    /// emitted.
    pub available: bool,
    /// Shell-paste-ready command string.
    pub command: String,
}

/// One CLI-boundary failure ready for rendering in either text or JSON
/// mode.
#[derive(Debug, Clone, Serialize)]
pub struct StructuredFailure {
    /// Stable snake_case token. Examples: `missing_required_arg`,
    /// `missing_subcommand`, `input_path_required`, `mode_mismatch`.
    pub kind: String,
    /// One-line human reason — printed verbatim under `reason:` in text
    /// mode and exposed as `"reason"` in JSON mode.
    pub reason: String,
    /// Actionable next step the operator can paste into a shell.
    pub recommendation: String,
    /// Optional fallback command (rendered as a dedicated line in text
    /// mode and a nested object in JSON mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<FallbackCommand>,
}

impl StructuredFailure {
    /// Build a failure without a fallback command. Use
    /// [`Self::with_fallback`] to attach one fluently.
    pub fn new(
        kind: impl Into<String>,
        reason: impl Into<String>,
        recommendation: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            reason: reason.into(),
            recommendation: recommendation.into(),
            fallback: None,
        }
    }

    /// Attach a fallback command (shell-paste-ready).
    pub fn with_fallback(mut self, command: impl Into<String>) -> Self {
        self.fallback = Some(FallbackCommand {
            available: true,
            command: command.into(),
        });
        self
    }

    /// Render the multi-line text envelope (§1.2). `cmd_name` is the
    /// fully-qualified command, e.g. `"aicx ingest"`.
    pub fn render_text(&self, cmd_name: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!("{cmd_name} failed.\n"));
        out.push_str(&format!("  kind:           {}\n", self.kind));
        out.push_str(&format!("  reason:         {}\n", self.reason));
        out.push_str(&format!("  recommendation: {}\n", self.recommendation));
        if let Some(fallback) = &self.fallback {
            out.push_str(&format!("  fallback:       {}\n", fallback.command));
        }
        out
    }

    /// Render the JSON envelope (§1.3). The `error` summary string is
    /// derived from `cmd_name` so consumers get a stable top-level
    /// signature alongside the structured payload.
    pub fn render_json(&self, cmd_name: &str) -> serde_json::Value {
        let mut payload = json!({
            "ok": false,
            "error": format!("{cmd_name} failed"),
            "kind": self.kind,
            "reason": self.reason,
            "recommendation": self.recommendation,
        });
        if let Some(fallback) = &self.fallback {
            payload["fallback"] = json!({
                "available": fallback.available,
                "command": fallback.command,
            });
        }
        payload
    }
}

/// Emit a [`StructuredFailure`] using the active output mode and return
/// a typed `anyhow::Error` whose `Display` re-states the structured
/// reason — so callers can `?`-propagate without losing the structured
/// signal in `main`'s error tail.
///
/// `cmd_name` should be the fully-qualified command (e.g.
/// `"aicx ingest"`). When `json` is true, the envelope is emitted on
/// stdout (so machine consumers can pipe it); otherwise the text
/// envelope is emitted on stderr.
pub fn emit_and_error(cmd_name: &str, json: bool, failure: StructuredFailure) -> anyhow::Error {
    if json {
        let payload = failure.render_json(cmd_name);
        // Use compact pretty (consistent with other --json paths in main.rs).
        match serde_json::to_string_pretty(&payload) {
            Ok(rendered) => println!("{rendered}"),
            Err(_) => println!("{payload}"),
        }
    } else {
        eprint!("{}", failure.render_text(cmd_name));
    }
    anyhow::anyhow!("{}: {}", failure.kind, failure.reason)
}

/// Convenience: detect whether the active CLI invocation wants JSON
/// failure envelopes. Honours `AICX_JSON=1` env var as a global
/// override (so commands without an explicit `--json` flag can still
/// participate in the structured pipeline when wrapped from MCP).
pub fn want_json_envelope(explicit_json_flag: bool) -> bool {
    explicit_json_flag
        || std::env::var("AICX_JSON")
            .map(|v| !v.is_empty() && v != "0" && v.to_ascii_lowercase() != "false")
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_envelope_has_canonical_field_order() {
        let failure = StructuredFailure::new(
            "missing_required_arg",
            "argument --source is required",
            "rerun with --source claude",
        );
        let rendered = failure.render_text("aicx ingest");
        let mut lines = rendered.lines();
        assert_eq!(lines.next().unwrap(), "aicx ingest failed.");
        assert!(lines.next().unwrap().starts_with("  kind:"));
        assert!(lines.next().unwrap().starts_with("  reason:"));
        assert!(lines.next().unwrap().starts_with("  recommendation:"));
        assert!(lines.next().is_none());
    }

    #[test]
    fn text_envelope_includes_fallback_when_set() {
        let failure =
            StructuredFailure::new("k", "r", "rec").with_fallback("aicx ingest --source claude");
        let rendered = failure.render_text("aicx ingest");
        assert!(rendered.contains("  fallback:       aicx ingest --source claude"));
    }

    #[test]
    fn json_envelope_omits_fallback_when_unset() {
        let failure = StructuredFailure::new("k", "r", "rec");
        let payload = failure.render_json("aicx ingest");
        assert_eq!(payload["ok"], serde_json::Value::Bool(false));
        assert_eq!(
            payload["error"],
            serde_json::Value::String("aicx ingest failed".to_string())
        );
        assert_eq!(payload["kind"], serde_json::Value::String("k".to_string()));
        assert_eq!(
            payload["reason"],
            serde_json::Value::String("r".to_string())
        );
        assert_eq!(
            payload["recommendation"],
            serde_json::Value::String("rec".to_string())
        );
        assert!(payload.get("fallback").is_none());
    }

    #[test]
    fn json_envelope_includes_fallback_when_set() {
        let failure = StructuredFailure::new("k", "r", "rec").with_fallback("cmd");
        let payload = failure.render_json("aicx ingest");
        let fb = &payload["fallback"];
        assert_eq!(fb["available"], serde_json::Value::Bool(true));
        assert_eq!(fb["command"], serde_json::Value::String("cmd".to_string()));
    }

    // Env-mutating tests merged into one sequential block to avoid
    // cross-test contamination under parallel test runners.
    #[test]
    fn want_json_envelope_respects_flag_and_env() {
        // SAFETY: caller controls full lifetime of the env var within
        // this test; no other tests in this module read AICX_JSON.
        unsafe {
            std::env::remove_var("AICX_JSON");
        }
        assert!(want_json_envelope(true));
        assert!(!want_json_envelope(false));

        unsafe {
            std::env::set_var("AICX_JSON", "1");
        }
        assert!(want_json_envelope(false));
        unsafe {
            std::env::set_var("AICX_JSON", "0");
        }
        assert!(!want_json_envelope(false));
        unsafe {
            std::env::set_var("AICX_JSON", "false");
        }
        assert!(!want_json_envelope(false));
        unsafe {
            std::env::set_var("AICX_JSON", "");
        }
        assert!(!want_json_envelope(false));
        unsafe {
            std::env::remove_var("AICX_JSON");
        }
    }
}
