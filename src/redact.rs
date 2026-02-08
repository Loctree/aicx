//! Secret redaction helpers for ai-contexters outputs.
//!
//! Goal: avoid accidentally persisting sensitive tokens into:
//! - `.ai-context/*` artifacts
//! - `~/.ai-contexters/<project>/<date>/*`
//! - memex chunks
//!
//! This is best-effort and intentionally conservative.
//!
//! Created by M&K (c)2026 VetCoders

use regex::{Captures, Regex, RegexSet};
use std::borrow::Cow;
use std::sync::LazyLock;

static RE_BLOCK_PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----")
        .expect("regex")
});

static RE_OPENAI_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsk-[A-Za-z0-9]{20,}\b").expect("regex"));
static RE_GITHUB_PAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b").expect("regex"));
static RE_GITHUB_CLASSIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bghp_[A-Za-z0-9]{36}\b").expect("regex"));
static RE_SLACK_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b").expect("regex"));
static RE_AWS_ACCESS_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("regex"));
static RE_GOOGLE_API_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bAIza[0-9A-Za-z_-]{35}\b").expect("regex"));

static RE_AUTH_BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bAuthorization:\s*Bearer\s+\S+").expect("regex"));

static SECRET_LOOKUP_SET: LazyLock<RegexSet> = LazyLock::new(|| {
    // Fast negative path: if nothing matches here (and no env/private-key match),
    // we can return the input unchanged without running the full replacement pipeline.
    RegexSet::new([
        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        r"\bsk-[A-Za-z0-9]{20,}\b",
        r"\bgithub_pat_[A-Za-z0-9_]{20,}\b",
        r"\bghp_[A-Za-z0-9]{36}\b",
        r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
        r"\bAKIA[0-9A-Z]{16}\b",
        r"\bAIza[0-9A-Za-z_-]{35}\b",
        r"(?i)\bAuthorization:\s*Bearer\s+\S+",
        r"(?i)\b(X-API-KEY|X-Auth-Token|Api-Key|Token)\s*:\s*([^\s]+)",
    ])
    .expect("regexset")
});

static RE_ENV_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    // Only redact env-var style assignments (UPPERCASE names), to avoid false positives
    // like `onPatientCreated={() => ...}` or `selectedPatientId=...` in code snippets.
    //
    // We match "export " optionally, then a UPPERCASE identifier, then "=".
    // The decision whether the key is sensitive is done in code (suffix/prefix checks).
    Regex::new(
        r"(?m)^(?P<prefix>\s*(?:export\s+)?)?(?P<key>[A-Z][A-Z0-9_]{2,})\s*=\s*(?P<val>[^\s]+)",
    )
    .expect("regex")
});

static RE_HEADER_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(X-API-KEY|X-Auth-Token|Api-Key|Token)\s*:\s*([^\s]+)").expect("regex")
});

pub fn redact_secrets(text: &str) -> String {
    if !SECRET_LOOKUP_SET.is_match(text) && !RE_ENV_ASSIGNMENT.is_match(text) {
        return text.to_string();
    }

    // Apply the pipeline in-place, but only allocate when a replacement actually happens.
    // `replace_all` returns `Cow::Borrowed` when there are no matches.
    let mut out = text.to_string();

    if let Cow::Owned(s) = RE_BLOCK_PRIVATE_KEY.replace_all(&out, "[REDACTED_PRIVATE_KEY_BLOCK]") {
        out = s;
    }

    if let Cow::Owned(s) = RE_AUTH_BEARER.replace_all(&out, "Authorization: Bearer [REDACTED]") {
        out = s;
    }

    let env_replaced = RE_ENV_ASSIGNMENT.replace_all(&out, |caps: &Captures| {
        let prefix = caps.name("prefix").map(|m| m.as_str()).unwrap_or("");
        let key = caps.name("key").map(|m| m.as_str()).unwrap_or("");
        let full = caps.get(0).map(|m| m.as_str()).unwrap_or("");

        let is_sensitive = key.ends_with("API_KEY")
            || key.ends_with("OAUTH_TOKEN")
            || key.ends_with("TOKEN")
            || key.ends_with("SECRET")
            || key.ends_with("PASSWORD")
            || key.starts_with("PAT_")
            || key.contains("_PAT_")
            || key.ends_with("_PAT");

        if is_sensitive {
            format!("{prefix}{key}=[REDACTED]")
        } else {
            full.to_string()
        }
    });

    if let Cow::Owned(s) = env_replaced {
        out = s;
    }

    if let Cow::Owned(s) =
        RE_HEADER_TOKEN.replace_all(&out, |caps: &Captures| format!("{}: [REDACTED]", &caps[1]))
    {
        out = s;
    }

    if let Cow::Owned(s) = RE_OPENAI_KEY.replace_all(&out, "[REDACTED_OPENAI_KEY]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_GITHUB_PAT.replace_all(&out, "[REDACTED_GITHUB_PAT]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_GITHUB_CLASSIC.replace_all(&out, "[REDACTED_GITHUB_TOKEN]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_SLACK_TOKEN.replace_all(&out, "[REDACTED_SLACK_TOKEN]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_AWS_ACCESS_KEY.replace_all(&out, "[REDACTED_AWS_ACCESS_KEY]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_GOOGLE_API_KEY.replace_all(&out, "[REDACTED_GOOGLE_API_KEY]") {
        out = s;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_openai_key() {
        let s = "hello sk-abcdefghijklmnopqrstuvwxyz0123456789 world";
        let r = redact_secrets(s);
        assert!(!r.contains("sk-"));
        assert!(r.contains("[REDACTED_OPENAI_KEY]"));
    }

    #[test]
    fn redacts_env_assignments() {
        let s =
            "LIBRAXIS_API_KEY=abc123\nOAUTH_TOKEN = xyz\nPASSWORD=pass\nexport GITHUB_TOKEN=zzz";
        let r = redact_secrets(s);
        assert!(r.contains("LIBRAXIS_API_KEY=[REDACTED]"));
        assert!(r.contains("OAUTH_TOKEN=[REDACTED]"));
        assert!(r.contains("PASSWORD=[REDACTED]"));
        assert!(r.contains("GITHUB_TOKEN=[REDACTED]"));
        assert!(!r.contains("abc123"));
        assert!(!r.contains("xyz"));
        assert!(!r.contains("pass"));
    }

    #[test]
    fn does_not_redact_patient_code() {
        let s = "onPatientCreated={() => { setActiveMenuItem('visits'); }}\nselectedPatientId={selectedPatientId}";
        let r = redact_secrets(s);
        assert_eq!(r, s);
    }

    #[test]
    fn redacts_private_key_block() {
        let s = "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----";
        let r = redact_secrets(s);
        assert_eq!(r, "[REDACTED_PRIVATE_KEY_BLOCK]");
    }

    #[test]
    fn no_match_returns_identity() {
        let s = "nothing to redact here";
        let r = redact_secrets(s);
        assert_eq!(r, s);
    }
}
