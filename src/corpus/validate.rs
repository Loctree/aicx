use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

use crate::card_header::{card_body, is_bracket_header_line, parse_card_header};
use crate::corpus::io::{markdown_files, validate_optional_root};
use crate::corpus::roots::default_roots;
use crate::corpus::types::{
    CorpusCardFinding, CorpusValidateOptions, CorpusValidateReport, CorpusValidateTotals,
    RootValidateReport,
};
use crate::parser::{
    CARD_CLAIM_SCOPE_SESSION_CLOSE, CARD_FRESHNESS_CONTRACT_HISTORICAL, CARD_SCHEMA_VERSION,
    CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX, ChunkMetadataSidecar,
};
use crate::sanitize;
use crate::sources::shared::is_harness_injected_noise;

const MAX_SAMPLES_PER_ROOT: usize = 30;

pub fn validate_cards(options: &CorpusValidateOptions) -> Result<CorpusValidateReport> {
    let roots = if options.roots.is_empty() {
        default_roots()?
    } else {
        options.roots.clone()
    }
    .into_iter()
    .map(validate_optional_root)
    .collect::<Result<Vec<_>>>()?;

    let mut reports = Vec::new();
    let mut totals = CorpusValidateTotals::default();

    for root in roots {
        let report = validate_root(&root)?;
        if report.present {
            totals.roots_present += 1;
        } else {
            totals.roots_missing += 1;
        }
        totals.cards += report.cards;
        totals.ok += report.ok;
        totals.warn += report.warn;
        totals.error += report.error;
        totals.hard_violations += report.hard_violations;
        totals.warnings += report.warnings;
        merge_counts(&mut totals.violations_by_class, &report.violations_by_class);
        merge_counts(&mut totals.warnings_by_class, &report.warnings_by_class);
        merge_counts(&mut totals.verdicts, &report.verdicts);
        reports.push(report);
    }

    let passed = totals.hard_violations == 0;
    Ok(CorpusValidateReport {
        roots: reports,
        totals,
        strict: options.strict,
        passed,
    })
}

fn validate_root(root: &Path) -> Result<RootValidateReport> {
    if !root.is_dir() && !root.is_file() {
        return Ok(RootValidateReport {
            root: root.to_path_buf(),
            present: false,
            cards: 0,
            ok: 0,
            warn: 0,
            error: 0,
            hard_violations: 0,
            warnings: 0,
            violations_by_class: BTreeMap::new(),
            warnings_by_class: BTreeMap::new(),
            verdicts: BTreeMap::new(),
            samples: Vec::new(),
        });
    }

    let mut report = RootValidateReport {
        root: root.to_path_buf(),
        present: true,
        cards: 0,
        ok: 0,
        warn: 0,
        error: 0,
        hard_violations: 0,
        warnings: 0,
        violations_by_class: BTreeMap::new(),
        warnings_by_class: BTreeMap::new(),
        verdicts: BTreeMap::new(),
        samples: Vec::new(),
    };

    for path in markdown_files(root)? {
        report.cards += 1;
        let findings = validate_card(&path);
        let has_error = findings.iter().any(|finding| finding.severity == "error");
        let has_warning = findings.iter().any(|finding| finding.severity == "warn");
        let verdict = if has_error {
            report.error += 1;
            "error"
        } else if has_warning {
            report.warn += 1;
            "warn"
        } else {
            report.ok += 1;
            "ok"
        };
        inc(&mut report.verdicts, verdict);

        for finding in findings {
            if finding.severity == "error" {
                report.hard_violations += 1;
                inc(&mut report.violations_by_class, &finding.class);
            } else {
                report.warnings += 1;
                inc(&mut report.warnings_by_class, &finding.class);
            }
            record_sample(&mut report.samples, finding);
        }
    }

    Ok(report)
}

fn record_sample(samples: &mut Vec<CorpusCardFinding>, finding: CorpusCardFinding) {
    if samples.len() < MAX_SAMPLES_PER_ROOT {
        samples.push(finding);
        return;
    }

    if finding.severity != "error" {
        return;
    }

    if let Some(slot) = samples
        .iter()
        .rposition(|sample| sample.severity != "error")
    {
        samples[slot] = finding;
    }
}

fn validate_card(path: &Path) -> Vec<CorpusCardFinding> {
    let sidecar_path = path.with_extension("meta.json");
    let mut findings = Vec::new();
    let content = match sanitize::read_to_string_validated(path) {
        Ok(content) => content,
        Err(err) => {
            push_error(
                &mut findings,
                path,
                Some(&sidecar_path),
                "markdown_read_error",
                format!("failed to read markdown: {err}"),
            );
            return findings;
        }
    };

    let header = detect_header(&content);
    if header.form == HeaderForm::Missing {
        push_error(
            &mut findings,
            path,
            Some(&sidecar_path),
            "missing_card_header",
            "markdown card header is missing".to_string(),
        );
    } else if parse_card_header(&content).is_none() {
        push_error(
            &mut findings,
            path,
            Some(&sidecar_path),
            "header_parse_error",
            "markdown card header could not be parsed".to_string(),
        );
    }
    if header.text.contains("${") || header.text.contains("{{") {
        push_error(
            &mut findings,
            path,
            Some(&sidecar_path),
            "unrendered_placeholder",
            "markdown header/frontmatter contains an unrendered placeholder".to_string(),
        );
    }
    if starts_with_harness_noise(&content) {
        push_warn(
            &mut findings,
            path,
            Some(&sidecar_path),
            "harness_noise_preamble",
            "card body starts with a harness-injected preamble".to_string(),
        );
    }

    let sidecar_text = match sanitize::read_to_string_validated(&sidecar_path) {
        Ok(text) => text,
        Err(err) => {
            push_error(
                &mut findings,
                path,
                Some(&sidecar_path),
                "sidecar_read_error",
                format!("failed to read sidecar: {err}"),
            );
            return findings;
        }
    };
    let sidecar = match serde_json::from_str::<ChunkMetadataSidecar>(&sidecar_text) {
        Ok(sidecar) => sidecar,
        Err(err) => {
            push_error(
                &mut findings,
                path,
                Some(&sidecar_path),
                "sidecar_parse_error",
                format!("sidecar JSON does not parse as a card sidecar: {err}"),
            );
            return findings;
        }
    };

    if !matches!(sidecar.schema_version, 1 | CARD_SCHEMA_VERSION) {
        push_error(
            &mut findings,
            path,
            Some(&sidecar_path),
            "unsupported_schema_version",
            format!("unsupported schema_version {}", sidecar.schema_version),
        );
    }

    if sidecar.schema_version == CARD_SCHEMA_VERSION {
        validate_v2_required_fields(path, &sidecar_path, &sidecar, &mut findings);
        if header.form != HeaderForm::Frontmatter {
            push_error(
                &mut findings,
                path,
                Some(&sidecar_path),
                "header_form_mismatch",
                "schema_version 2 cards must use YAML frontmatter".to_string(),
            );
        } else if !header
            .text
            .lines()
            .any(|line| line.trim() == "schema: card.v2")
        {
            push_error(
                &mut findings,
                path,
                Some(&sidecar_path),
                "frontmatter_schema_missing",
                "schema_version 2 frontmatter must declare schema: card.v2".to_string(),
            );
        }
    } else if sidecar.schema_version == 1 && header.form == HeaderForm::Frontmatter {
        push_warn(
            &mut findings,
            path,
            Some(&sidecar_path),
            "header_form_mismatch",
            "schema_version 1 card uses YAML frontmatter".to_string(),
        );
    }

    if let Some(expected) = sidecar.content_sha256.as_deref() {
        let actual = sha256_hex(&content);
        if expected != actual {
            push_error(
                &mut findings,
                path,
                Some(&sidecar_path),
                "content_sha256_mismatch",
                "sidecar content_sha256 does not match markdown content".to_string(),
            );
        }
    }

    validate_signal_presence(path, &sidecar_path, &content, &sidecar, &mut findings);
    findings
}

fn validate_v2_required_fields(
    path: &Path,
    sidecar_path: &Path,
    sidecar: &ChunkMetadataSidecar,
    findings: &mut Vec<CorpusCardFinding>,
) {
    if sidecar
        .source
        .as_ref()
        .is_none_or(|source| source.path.trim().is_empty())
    {
        push_error(
            findings,
            path,
            Some(sidecar_path),
            "missing_required_field",
            "schema_version 2 sidecar is missing source.path".to_string(),
        );
    }
    if sidecar.claim_scope.as_deref() != Some(CARD_CLAIM_SCOPE_SESSION_CLOSE) {
        push_error(
            findings,
            path,
            Some(sidecar_path),
            "missing_required_field",
            "schema_version 2 sidecar must set claim_scope=session_close".to_string(),
        );
    }
    if sidecar.freshness_contract.as_deref() != Some(CARD_FRESHNESS_CONTRACT_HISTORICAL) {
        push_error(
            findings,
            path,
            Some(sidecar_path),
            "missing_required_field",
            "schema_version 2 sidecar must set freshness_contract=historical".to_string(),
        );
    }
    if sidecar.verification_state.as_deref() != Some(CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX) {
        push_error(
            findings,
            path,
            Some(sidecar_path),
            "missing_required_field",
            "schema_version 2 sidecar must set verification_state=not_verified_by_aicx".to_string(),
        );
    }
    if sidecar.content_sha256.as_deref().is_none_or(str::is_empty) {
        push_error(
            findings,
            path,
            Some(sidecar_path),
            "missing_required_field",
            "schema_version 2 sidecar is missing content_sha256".to_string(),
        );
    }
}

fn validate_signal_presence(
    path: &Path,
    sidecar_path: &Path,
    content: &str,
    sidecar: &ChunkMetadataSidecar,
    findings: &mut Vec<CorpusCardFinding>,
) {
    let sidecar_has_signals = sidecar
        .signals
        .as_ref()
        .is_some_and(|signals| !signals.is_empty());
    let markdown_has_signals = markdown_has_signal_block(content);
    if sidecar_has_signals == markdown_has_signals {
        return;
    }

    let message = "sidecar signals[] presence does not match markdown [signals] block".to_string();
    if sidecar.schema_version == CARD_SCHEMA_VERSION {
        push_error(
            findings,
            path,
            Some(sidecar_path),
            "signals_mismatch",
            message,
        );
    } else {
        push_warn(
            findings,
            path,
            Some(sidecar_path),
            "signals_mismatch",
            message,
        );
    }
}

fn markdown_has_signal_block(content: &str) -> bool {
    content.contains("[signals]") && content.contains("[/signals]")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderForm {
    Frontmatter,
    Bracket,
    Missing,
}

struct HeaderRegion<'a> {
    form: HeaderForm,
    text: &'a str,
}

fn detect_header(content: &str) -> HeaderRegion<'_> {
    if content.starts_with("---\n")
        && let Some(end) = content[4..].find("\n---\n")
    {
        let end = 4 + end + "\n---\n".len();
        return HeaderRegion {
            form: HeaderForm::Frontmatter,
            text: &content[..end],
        };
    }

    let first_line = content.lines().next().unwrap_or_default();
    if is_bracket_header_line(first_line) {
        return HeaderRegion {
            form: HeaderForm::Bracket,
            text: first_line,
        };
    }

    HeaderRegion {
        form: HeaderForm::Missing,
        text: "",
    }
}

fn starts_with_harness_noise(content: &str) -> bool {
    let mut body = card_body(content).trim_start();
    if body.starts_with("[signals]\n")
        && let Some(end) = body.find("[/signals]")
    {
        body = body[end + "[/signals]".len()..].trim_start();
    }

    let Some(line) = body.lines().find(|line| !line.trim().is_empty()) else {
        return false;
    };
    let Some((role, message)) = parse_message_line(line) else {
        return is_harness_injected_noise("user", line);
    };
    is_harness_injected_noise(role, message)
}

fn parse_message_line(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix('[')?.split_once("] ")?.1;
    rest.split_once(": ")
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn push_error(
    findings: &mut Vec<CorpusCardFinding>,
    path: &Path,
    sidecar_path: Option<&Path>,
    class: &str,
    message: String,
) {
    push_finding(findings, path, sidecar_path, "error", class, message);
}

fn push_warn(
    findings: &mut Vec<CorpusCardFinding>,
    path: &Path,
    sidecar_path: Option<&Path>,
    class: &str,
    message: String,
) {
    push_finding(findings, path, sidecar_path, "warn", class, message);
}

fn push_finding(
    findings: &mut Vec<CorpusCardFinding>,
    path: &Path,
    sidecar_path: Option<&Path>,
    severity: &str,
    class: &str,
    message: String,
) {
    findings.push(CorpusCardFinding {
        path: path.to_path_buf(),
        sidecar_path: sidecar_path.map(Path::to_path_buf),
        severity: severity.to_string(),
        class: class.to_string(),
        message,
    });
}

fn merge_counts(target: &mut BTreeMap<String, usize>, source: &BTreeMap<String, usize>) {
    for (key, value) in source {
        *target.entry(key.clone()).or_default() += value;
    }
}

fn inc(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.to_string()).or_default() += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-tmp")
            .join(format!("aicx-validate-cards-{name}-{nonce}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_card(root: &Path, stem: &str, markdown: &str, mut sidecar: Value) -> PathBuf {
        let path = root.join(format!("{stem}.md"));
        fs::write(&path, markdown).unwrap();
        if sidecar.get("content_sha256").and_then(Value::as_str) == Some("__AUTO__") {
            sidecar["content_sha256"] = Value::String(sha256_hex(markdown));
        }
        fs::write(
            path.with_extension("meta.json"),
            serde_json::to_vec_pretty(&sidecar).unwrap(),
        )
        .unwrap();
        path
    }

    fn v1_sidecar(id: &str) -> Value {
        json!({
            "id": id,
            "project": "vetcoders/aicx",
            "agent": "codex",
            "date": "2026-07-02",
            "session_id": "session-1",
            "kind": "conversations"
        })
    }

    fn v2_sidecar(id: &str) -> Value {
        json!({
            "id": id,
            "schema_version": 2,
            "project": "vetcoders/aicx",
            "agent": "codex",
            "date": "2026-07-02",
            "session_id": "session-2",
            "kind": "conversations",
            "source": {"path": "/tmp/source.jsonl"},
            "claim_scope": "session_close",
            "freshness_contract": "historical",
            "verification_state": "not_verified_by_aicx",
            "content_sha256": "__AUTO__"
        })
    }

    fn valid_v1_markdown() -> &'static str {
        "[project: vetcoders/aicx | agent: codex | date: 2026-07-02]\n\n[00:00:00] user: hello\n"
    }

    fn valid_v2_markdown() -> &'static str {
        "---\nproject: vetcoders/aicx\nagent: codex\ndate: 2026-07-02\nschema: card.v2\n---\n\n[00:00:00] user: hello\n"
    }

    #[test]
    fn validate_cards_accepts_valid_v1_and_v2_cards() {
        let root = temp_root("valid");
        write_card(&root, "v1", valid_v1_markdown(), v1_sidecar("v1"));
        write_card(&root, "v2", valid_v2_markdown(), v2_sidecar("v2"));

        let report = validate_cards(&CorpusValidateOptions {
            roots: vec![root.clone()],
            strict: false,
        })
        .unwrap();

        assert_eq!(report.totals.cards, 2);
        assert_eq!(report.totals.ok, 2);
        assert_eq!(report.totals.hard_violations, 0);
        assert!(report.passed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validate_cards_reports_fixture_violation_classes() {
        let root = temp_root("mixed");
        write_card(
            &root,
            "valid-v1",
            valid_v1_markdown(),
            v1_sidecar("valid-v1"),
        );
        write_card(
            &root,
            "valid-v2",
            valid_v2_markdown(),
            v2_sidecar("valid-v2"),
        );

        let mut bad_sha = v2_sidecar("bad-sha");
        bad_sha["content_sha256"] = Value::String("bad".to_string());
        write_card(&root, "bad-sha", valid_v2_markdown(), bad_sha);

        let mut missing_source = v2_sidecar("missing-source");
        missing_source.as_object_mut().unwrap().remove("source");
        write_card(&root, "missing-source", valid_v2_markdown(), missing_source);

        let placeholder = "---\nproject: ${PROJECT}\nagent: codex\ndate: 2026-07-02\nschema: card.v2\n---\n\n[00:00:00] user: hello\n";
        write_card(&root, "placeholder", placeholder, v2_sidecar("placeholder"));

        let noise = "---\nproject: vetcoders/aicx\nagent: codex\ndate: 2026-07-02\nschema: card.v2\n---\n\n[00:00:00] user: <command-message>synthetic\n";
        write_card(&root, "noise", noise, v2_sidecar("noise"));

        let report = validate_cards(&CorpusValidateOptions {
            roots: vec![root.clone()],
            strict: true,
        })
        .unwrap();

        assert_eq!(report.totals.cards, 6);
        assert_eq!(
            report.totals.violations_by_class["content_sha256_mismatch"],
            1
        );
        assert_eq!(
            report.totals.violations_by_class["missing_required_field"],
            1
        );
        assert_eq!(
            report.totals.violations_by_class["unrendered_placeholder"],
            1
        );
        assert_eq!(report.totals.warnings_by_class["harness_noise_preamble"], 1);
        assert!(!report.passed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validate_cards_reports_v2_signal_mismatch_as_error() {
        let root = temp_root("signals-v2");
        let markdown = "---\nproject: vetcoders/aicx\nagent: codex\ndate: 2026-07-02\nschema: card.v2\n---\n\n[signals]\nDecision:\n- [decision] keep\n[/signals]\n\n[00:00:00] user: hello\n";
        write_card(&root, "signals", markdown, v2_sidecar("signals"));

        let report = validate_cards(&CorpusValidateOptions {
            roots: vec![root.clone()],
            strict: true,
        })
        .unwrap();

        assert_eq!(report.totals.violations_by_class["signals_mismatch"], 1);
        assert_eq!(report.totals.warnings, 0);
        assert!(!report.passed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validate_cards_reports_v1_signal_mismatch_as_warning() {
        let root = temp_root("signals-v1");
        let markdown = "[project: vetcoders/aicx | agent: codex | date: 2026-07-02]\n\n[signals]\nDecision:\n- [decision] keep\n[/signals]\n\n[00:00:00] user: hello\n";
        write_card(&root, "signals", markdown, v1_sidecar("signals"));

        let report = validate_cards(&CorpusValidateOptions {
            roots: vec![root.clone()],
            strict: false,
        })
        .unwrap();

        assert_eq!(report.totals.warnings_by_class["signals_mismatch"], 1);
        assert_eq!(report.totals.hard_violations, 0);
        assert!(report.passed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validate_cards_json_has_stable_top_level_keys() {
        let root = temp_root("json");
        write_card(&root, "v2", valid_v2_markdown(), v2_sidecar("v2"));
        let report = validate_cards(&CorpusValidateOptions {
            roots: vec![root.clone()],
            strict: false,
        })
        .unwrap();

        let value = serde_json::to_value(&report).unwrap();
        let keys: std::collections::BTreeSet<_> =
            value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "passed".to_string(),
                "roots".to_string(),
                "strict".to_string(),
                "totals".to_string(),
            ])
        );
        let totals_keys: std::collections::BTreeSet<_> = value["totals"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            totals_keys,
            std::collections::BTreeSet::from([
                "cards".to_string(),
                "error".to_string(),
                "hard_violations".to_string(),
                "ok".to_string(),
                "roots_missing".to_string(),
                "roots_present".to_string(),
                "verdicts".to_string(),
                "violations_by_class".to_string(),
                "warn".to_string(),
                "warnings".to_string(),
                "warnings_by_class".to_string(),
            ])
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validate_cards_reports_sidecar_parse_and_schema_errors() {
        let root = temp_root("sidecar");
        let path = root.join("bad-json.md");
        fs::write(&path, valid_v2_markdown()).unwrap();
        fs::write(path.with_extension("meta.json"), "{").unwrap();

        let mut bad_schema = v2_sidecar("bad-schema");
        bad_schema["schema_version"] = Value::from(3);
        write_card(&root, "bad-schema", valid_v2_markdown(), bad_schema);

        let report = validate_cards(&CorpusValidateOptions {
            roots: vec![root.clone()],
            strict: true,
        })
        .unwrap();

        assert_eq!(report.totals.violations_by_class["sidecar_parse_error"], 1);
        assert_eq!(
            report.totals.violations_by_class["unsupported_schema_version"],
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validate_cards_samples_keep_hard_errors_when_warnings_fill_cap() {
        let root = temp_root("samples");
        for idx in 0..MAX_SAMPLES_PER_ROOT {
            let markdown = format!(
                "[project: vetcoders/aicx | agent: codex | date: 2026-07-02]\n\n[signals]\nDecision:\n- [decision] keep {idx}\n[/signals]\n\n[00:00:00] user: hello\n"
            );
            write_card(
                &root,
                &format!("warn-{idx:02}"),
                &markdown,
                v1_sidecar("warn"),
            );
        }

        let mut bad_sha = v2_sidecar("bad-sha");
        bad_sha["content_sha256"] = Value::String("bad".to_string());
        write_card(&root, "bad-sha", valid_v2_markdown(), bad_sha);

        let report = validate_cards(&CorpusValidateOptions {
            roots: vec![root.clone()],
            strict: true,
        })
        .unwrap();

        assert_eq!(report.roots[0].samples.len(), MAX_SAMPLES_PER_ROOT);
        assert!(report.roots[0].samples.iter().any(|sample| {
            sample.severity == "error" && sample.class == "content_sha256_mismatch"
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn strict_mode_failure_is_report_derived() {
        let root = temp_root("strict");
        let mut sidecar = v2_sidecar("bad-sha");
        sidecar["content_sha256"] = Value::String("bad".to_string());
        write_card(&root, "bad-sha", valid_v2_markdown(), sidecar);

        let report = validate_cards(&CorpusValidateOptions {
            roots: vec![root.clone()],
            strict: true,
        })
        .unwrap();

        assert!(report.strict);
        assert!(report.totals.hard_violations > 0);
        assert!(!report.passed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "performance proof: cargo test -p aicx corpus::validate::tests::validate_cards_10k_fixture_under_30s -- --ignored --nocapture"]
    fn validate_cards_10k_fixture_under_30s() {
        let root = temp_root("10k");
        for idx in 0..10_000 {
            let markdown = format!(
                "---\nproject: vetcoders/aicx\nagent: codex\ndate: 2026-07-02\nschema: card.v2\n---\n\n[00:00:00] user: hello {idx}\n"
            );
            write_card(
                &root,
                &format!("card-{idx:05}"),
                &markdown,
                v2_sidecar(&format!("card-{idx:05}")),
            );
        }

        let started = Instant::now();
        let report = validate_cards(&CorpusValidateOptions {
            roots: vec![root.clone()],
            strict: true,
        })
        .unwrap();
        let elapsed = started.elapsed();
        eprintln!("validated {} cards in {:?}", report.totals.cards, elapsed);

        assert_eq!(report.totals.cards, 10_000);
        assert!(report.passed);
        assert!(elapsed.as_secs_f64() < 30.0);
        fs::remove_dir_all(root).unwrap();
    }
}
