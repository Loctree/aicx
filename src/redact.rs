//! Secret redaction helpers for ai-contexters outputs.
//!
//! Goal: avoid accidentally persisting sensitive tokens into:
//! - `.ai-context/*` artifacts
//! - `~/.aicx/store/<project>/<date>/*`
//! - chunks
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
static RE_ANTHROPIC_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsk-ant-api03-[A-Za-z0-9_-]{20,}\b").expect("regex"));
static RE_OPENAI_PROJECT_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsk-proj-[A-Za-z0-9_-]{20,}\b").expect("regex"));
static RE_GITHUB_PAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b").expect("regex"));
static RE_GITHUB_TOKENS_EXT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgh[psour]_[A-Za-z0-9]{36}\b").expect("regex"));
static RE_GITLAB_PAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bglpat-[A-Za-z0-9_-]{20,}\b").expect("regex"));
static RE_SLACK_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b").expect("regex"));
static RE_SLACK_APP_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bxapp-[0-9]-[A-Za-z0-9-]{10,}\b").expect("regex"));
static RE_AWS_ACCESS_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("regex"));
static RE_AWS_SESSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bASIA[0-9A-Z]{16}\b").expect("regex"));
static RE_GOOGLE_API_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bAIza[0-9A-Za-z_-]{35}\b").expect("regex"));
static RE_JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b")
        .expect("regex")
});
static RE_STRIPE_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{24,}\b").expect("regex"));

static RE_GCP_JSON_PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)(?P<prefix>"private_key"\s*:\s*)"-----BEGIN[^"]+-----END[^"]+""#)
        .expect("regex")
});
static RE_GCP_PRIVATE_KEY_ID_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?P<prefix>"private_key_id"\s*:\s*)"[^"]+""#).expect("regex"));
static RE_GCP_CLIENT_EMAIL_FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?P<prefix>"client_email"\s*:\s*)"[^"]+""#).expect("regex"));

static RE_AUTH_BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bAuthorization:\s*Bearer\s+\S+").expect("regex"));

static SECRET_LOOKUP_SET: LazyLock<RegexSet> = LazyLock::new(|| {
    // Fast negative path: if nothing matches here (and no env/private-key match),
    // we can return the input unchanged without running the full replacement pipeline.
    RegexSet::new([
        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        r"\bsk-[A-Za-z0-9]{20,}\b",
        r"\bsk-ant-api03-[A-Za-z0-9_-]{20,}\b",
        r"\bsk-proj-[A-Za-z0-9_-]{20,}\b",
        r"\bgithub_pat_[A-Za-z0-9_]{20,}\b",
        r"\bgh[psour]_[A-Za-z0-9]{36}\b",
        r"\bglpat-[A-Za-z0-9_-]{20,}\b",
        r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
        r"\bxapp-[0-9]-[A-Za-z0-9-]{10,}\b",
        r"\bAKIA[0-9A-Z]{16}\b",
        r"\bASIA[0-9A-Z]{16}\b",
        r"\bAIza[0-9A-Za-z_-]{35}\b",
        r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
        r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{24,}\b",
        r#"(?s)"private_key"\s*:\s*"-----BEGIN[^"]+-----END[^"]+""#,
        r#""private_key_id"\s*:\s*"[^"]+""#,
        r#""client_email"\s*:\s*"[^"]+""#,
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

static RE_INLINE_SENSITIVE_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    // Redact sensitive assignments that appear inside prose/code spans, not only
    // line-start env declarations. This catches agent reports such as
    // `BRAVE_API_KEY="..."` and code snippets like `api_key = "..."`.
    Regex::new(
        r#"(?P<prefix>\b(?P<key>[A-Za-z_][A-Za-z0-9_-]*)\b\s*[:=]\s*)(?P<quote>["']?)(?P<val>[A-Za-z0-9_./+=:@-]{8,})(?P<suffix>["']?)"#,
    )
    .expect("regex")
});

static RE_HEADER_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(X-API-KEY|X-Auth-Token|Api-Key|Token)\s*:\s*([^\s]+)").expect("regex")
});

pub fn redact_secrets(text: &str) -> String {
    if !SECRET_LOOKUP_SET.is_match(text)
        && !RE_ENV_ASSIGNMENT.is_match(text)
        && !RE_INLINE_SENSITIVE_ASSIGNMENT.is_match(text)
    {
        return text.to_string();
    }

    // Apply the pipeline in-place, but only allocate when a replacement actually happens.
    // `replace_all` returns `Cow::Borrowed` when there are no matches.
    let mut out = text.to_string();

    if let Cow::Owned(s) = RE_GCP_JSON_PRIVATE_KEY.replace_all(&out, |caps: &Captures| {
        redact_json_string_field(caps, "[REDACTED_GCP_PRIVATE_KEY]")
    }) {
        out = s;
    }

    if let Cow::Owned(s) = redact_gcp_service_account_fields(&out) {
        out = s;
    }

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

    let inline_replaced = RE_INLINE_SENSITIVE_ASSIGNMENT.replace_all(&out, |caps: &Captures| {
        let prefix = caps.name("prefix").map(|m| m.as_str()).unwrap_or("");
        let key = caps.name("key").map(|m| m.as_str()).unwrap_or("");
        let full = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        if !is_sensitive_assignment_key(key) {
            return full.to_string();
        }
        let quote = caps.name("quote").map(|m| m.as_str()).unwrap_or("");
        let suffix = caps.name("suffix").map(|m| m.as_str()).unwrap_or(quote);
        format!("{prefix}{quote}[REDACTED]{suffix}")
    });

    if let Cow::Owned(s) = inline_replaced {
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
    if let Cow::Owned(s) = RE_ANTHROPIC_KEY.replace_all(&out, "[REDACTED_ANTHROPIC_KEY]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_OPENAI_PROJECT_KEY.replace_all(&out, "[REDACTED_OPENAI_PROJECT_KEY]")
    {
        out = s;
    }
    if let Cow::Owned(s) = RE_GITHUB_PAT.replace_all(&out, "[REDACTED_GITHUB_PAT]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_GITHUB_TOKENS_EXT.replace_all(&out, "[REDACTED_GITHUB_TOKEN]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_GITLAB_PAT.replace_all(&out, "[REDACTED_GITLAB_PAT]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_SLACK_TOKEN.replace_all(&out, "[REDACTED_SLACK_TOKEN]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_SLACK_APP_TOKEN.replace_all(&out, "[REDACTED_SLACK_APP_TOKEN]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_AWS_ACCESS_KEY.replace_all(&out, "[REDACTED_AWS_ACCESS_KEY]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_AWS_SESSION.replace_all(&out, "[REDACTED_AWS_SESSION_KEY]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_GOOGLE_API_KEY.replace_all(&out, "[REDACTED_GOOGLE_API_KEY]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_JWT.replace_all(&out, "[REDACTED_JWT]") {
        out = s;
    }
    if let Cow::Owned(s) = RE_STRIPE_KEY.replace_all(&out, "[REDACTED_STRIPE_KEY]") {
        out = s;
    }

    out
}

fn redact_gcp_service_account_fields(text: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(text);

    if RE_GCP_PRIVATE_KEY_ID_FIELD.is_match(out.as_ref()) {
        out = Cow::Owned(
            RE_GCP_PRIVATE_KEY_ID_FIELD
                .replace_all(out.as_ref(), |caps: &Captures| {
                    redact_json_string_field(caps, "[REDACTED_GCP_PRIVATE_KEY_ID]")
                })
                .into_owned(),
        );
    }

    if RE_GCP_CLIENT_EMAIL_FIELD.is_match(out.as_ref()) {
        out = Cow::Owned(
            RE_GCP_CLIENT_EMAIL_FIELD
                .replace_all(out.as_ref(), |caps: &Captures| {
                    redact_json_string_field(caps, "[REDACTED_GCP_CLIENT_EMAIL]")
                })
                .into_owned(),
        );
    }

    out
}

fn redact_json_string_field(caps: &Captures, replacement: &str) -> String {
    let prefix = caps.name("prefix").map(|m| m.as_str()).unwrap_or("");
    format!(r#"{prefix}"{replacement}""#)
}

fn is_sensitive_assignment_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    let normalized = lower.replace('-', "_");
    normalized == "token"
        || normalized == "secret"
        || normalized == "password"
        || normalized == "pat"
        || normalized.contains("api_key")
        || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
        || normalized.ends_with("_password")
        || normalized.starts_with("pat_")
        || normalized.contains("_pat_")
        || normalized.ends_with("_pat")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(ch: char, len: usize) -> String {
        std::iter::repeat_n(ch, len).collect()
    }

    fn assert_redacted(raw: &str, label: &str) {
        let r = redact_secrets(raw);
        assert!(!r.contains(raw), "raw secret leaked: {raw}");
        assert!(r.contains(label), "missing label {label}: {r}");
    }

    #[test]
    fn redacts_openai_key() {
        let s = "hello sk-abcdefghijklmnopqrstuvwxyz0123456789 world";
        let r = redact_secrets(s);
        assert!(!r.contains("sk-"));
        assert!(r.contains("[REDACTED_OPENAI_KEY]"));
    }

    #[test]
    fn redacts_anthropic_key() {
        let token = format!("sk-ant-api03-{}", chars('A', 24));
        assert_redacted(&token, "[REDACTED_ANTHROPIC_KEY]");
    }

    #[test]
    fn redacts_openai_project_key() {
        let token = format!("sk-proj-{}", chars('B', 24));
        assert_redacted(&token, "[REDACTED_OPENAI_PROJECT_KEY]");
    }

    #[test]
    fn redacts_github_server_to_server() {
        let token = format!("ghs_{}", chars('C', 36));
        assert_redacted(&token, "[REDACTED_GITHUB_TOKEN]");
    }

    #[test]
    fn redacts_github_oauth() {
        let token = format!("gho_{}", chars('D', 36));
        assert_redacted(&token, "[REDACTED_GITHUB_TOKEN]");
    }

    #[test]
    fn redacts_github_user_to_server() {
        let token = format!("ghu_{}", chars('E', 36));
        assert_redacted(&token, "[REDACTED_GITHUB_TOKEN]");
    }

    #[test]
    fn redacts_github_refresh() {
        let token = format!("ghr_{}", chars('F', 36));
        assert_redacted(&token, "[REDACTED_GITHUB_TOKEN]");
    }

    #[test]
    fn redacts_gitlab_pat() {
        let token = format!("glpat-{}", chars('G', 24));
        assert_redacted(&token, "[REDACTED_GITLAB_PAT]");
    }

    #[test]
    fn redacts_aws_session_key() {
        let token = format!("ASIA{}", chars('H', 16));
        assert_redacted(&token, "[REDACTED_AWS_SESSION_KEY]");
    }

    #[test]
    fn redacts_slack_app_token() {
        let token = format!("xapp-1-{}", chars('I', 16));
        assert_redacted(&token, "[REDACTED_SLACK_APP_TOKEN]");
    }

    #[test]
    fn redacts_jwt_three_parts() {
        let token = format!(
            "eyJ{}.eyJ{}.{}",
            chars('J', 12),
            chars('K', 12),
            chars('L', 16)
        );
        assert_redacted(&token, "[REDACTED_JWT]");
    }

    #[test]
    fn redacts_stripe_live_key() {
        let token = format!("sk_live_{}", chars('M', 24));
        assert_redacted(&token, "[REDACTED_STRIPE_KEY]");
    }

    #[test]
    fn redacts_stripe_test_key() {
        let token = format!("sk_test_{}", chars('N', 24));
        assert_redacted(&token, "[REDACTED_STRIPE_KEY]");
    }

    #[test]
    fn redacts_stripe_restricted_key() {
        let token = format!("rk_live_{}", chars('O', 24));
        assert_redacted(&token, "[REDACTED_STRIPE_KEY]");
    }

    #[test]
    fn redacts_gcp_service_account_json() {
        let private_key_id = chars('1', 40);
        let client_email = "aicx-redaction-test@aicx-test.iam.gserviceaccount.com";
        let private_key = format!(
            "{}{}{}{}{}",
            "-----BEGIN ",
            "PRIVATE KEY-----\\n",
            chars('P', 32),
            "\\n-----END PRIVATE KEY-----",
            "\\n"
        );
        let s = format!(
            r#"{{
  "type": "service_account",
  "project_id": "aicx-test",
  "private_key_id": "{private_key_id}",
  "private_key": "{private_key}",
  "client_email": "{client_email}",
  "token_uri": "https://oauth2.googleapis.com/token"
}}"#
        );

        let r = redact_secrets(&s);
        assert!(!r.contains(&private_key_id));
        assert!(!r.contains(&private_key));
        assert!(!r.contains(client_email));
        assert!(r.contains(r#""private_key_id": "[REDACTED_GCP_PRIVATE_KEY_ID]""#));
        assert!(r.contains(r#""private_key": "[REDACTED_GCP_PRIVATE_KEY]""#));
        assert!(r.contains(r#""client_email": "[REDACTED_GCP_CLIENT_EMAIL]""#));
    }

    #[test]
    fn redacts_gcp_service_account_fields_without_private_key_trigger() {
        let private_key_id = chars('2', 40);
        let client_email = "field-only-redaction@aicx-test.iam.gserviceaccount.com";
        let s = format!(
            r#"{{
  "private_key_id": "{private_key_id}",
  "client_email": "{client_email}"
}}"#
        );

        let r = redact_secrets(&s);
        assert!(!r.contains(&private_key_id));
        assert!(!r.contains(client_email));
        assert!(r.contains(r#""private_key_id": "[REDACTED_GCP_PRIVATE_KEY_ID]""#));
        assert!(r.contains(r#""client_email": "[REDACTED_GCP_CLIENT_EMAIL]""#));
    }

    #[test]
    fn redacts_authorization_bearer_still_works() {
        let token = format!("Authorization: Bearer {}", chars('Q', 32));
        let r = redact_secrets(&token);
        assert_eq!(r, "Authorization: Bearer [REDACTED]");
    }

    #[test]
    fn redacts_existing_token_patterns() {
        let google = format!("AIza{}", chars('R', 35));
        let legacy = vec![
            (format!("sk-{}", chars('S', 24)), "[REDACTED_OPENAI_KEY]"),
            (
                format!("github_pat_{}", chars('T', 24)),
                "[REDACTED_GITHUB_PAT]",
            ),
            (format!("ghp_{}", chars('U', 36)), "[REDACTED_GITHUB_TOKEN]"),
            (format!("xoxb-{}", chars('V', 16)), "[REDACTED_SLACK_TOKEN]"),
            (
                format!("AKIA{}", chars('W', 16)),
                "[REDACTED_AWS_ACCESS_KEY]",
            ),
            (google, "[REDACTED_GOOGLE_API_KEY]"),
        ];

        for (token, label) in legacy {
            assert_redacted(&token, label);
        }

        let private_key = "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----";
        assert_redacted(private_key, "[REDACTED_PRIVATE_KEY_BLOCK]");

        let header = format!("X-API-KEY: {}", chars('X', 16));
        let r = redact_secrets(&header);
        assert_eq!(r, "X-API-KEY: [REDACTED]");
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
    fn redacts_inline_sensitive_assignment_in_agent_report() {
        // Build the secret-like value at runtime so semgrep does not flag the
        // test fixture as a real leaked key (no literal long alphanumeric
        // string in source code).
        let value = "a".repeat(32);
        let s = format!(r#"- `BRAVE_API_KEY="{value}"` **exposed in plaintext**"#);
        let r = redact_secrets(&s);
        assert!(r.contains(r#"BRAVE_API_KEY="[REDACTED]""#));
        assert!(!r.contains(&value));
    }

    #[test]
    fn redacts_lowercase_code_style_api_key_assignment() {
        // Same runtime-built dummy value strategy — semgrep cannot fingerprint
        // the literal once it is constructed at runtime.
        let value = "b".repeat(32);
        let s = format!(r#"client = SearchClient(api_key = "{value}")"#);
        let r = redact_secrets(&s);
        assert!(r.contains(r#"api_key = "[REDACTED]""#));
        assert!(!r.contains(&value));
    }

    #[test]
    fn does_not_redact_token_usage_metadata() {
        let s = "token_usage: 12345678";
        let r = redact_secrets(s);
        assert_eq!(r, s);
    }

    #[test]
    fn does_not_redact_patient_code() {
        let s = "onPatientCreated={() => { setActiveMenuItem('visits'); }}\nselectedPatientId={selectedPatientId}";
        let r = redact_secrets(s);
        assert_eq!(r, s);
    }

    #[test]
    fn does_not_redact_sha1_commit_hash() {
        let s = format!("commit {}", chars('a', 40));
        let r = redact_secrets(&s);
        assert_eq!(r, s);
    }

    #[test]
    fn does_not_redact_sha256_hash() {
        let s = format!("sha256 {}", chars('b', 64));
        let r = redact_secrets(&s);
        assert_eq!(r, s);
    }

    #[test]
    fn does_not_redact_uuid_v4() {
        let s = "id 550e8400-e29b-41d4-a716-446655440000";
        let r = redact_secrets(s);
        assert_eq!(r, s);
    }

    #[test]
    fn does_not_redact_generic_base64_text() {
        let s = format!("attachment {}", chars('A', 32));
        let r = redact_secrets(&s);
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
