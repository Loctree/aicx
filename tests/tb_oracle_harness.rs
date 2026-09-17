//! Differential oracle harness against a frozen Transcript Builder package.
//!
//! TB is the *differential oracle*, never a runtime dependency: the harness
//! reads a real `tbflow` package from `tests/fixtures/tb_oracle/_shared/`
//! (human.md frontmatter + index_payload.jsonl of session `ae59aa08`),
//! extracts the common field set both tools claim to know, and reports a
//! field-by-field diff. W0 proves the loader + diff on the package's own
//! internal consistency; W2 feeds the aicx-derived side into the same
//! `diff_common_fields`.
//!
//! Contract: `docs/DISTILL_CONTRACT.md` (TB→aicx field mapping).

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// The fields both TB and aicx claim to know about one session. Everything is
/// normalized (e.g. sha256 stripped of its `sha256:` prefix) before diffing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommonFields {
    agent: String,
    map_id: String,
    cwd: String,
    branch: String,
    source_sha256: String,
    segments: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldDiff {
    field: &'static str,
    left: String,
    right: String,
}

impl fmt::Display for FieldDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: left={:?} right={:?}",
            self.field, self.left, self.right
        )
    }
}

fn diff_common_fields(left: &CommonFields, right: &CommonFields) -> Vec<FieldDiff> {
    let mut diffs = Vec::new();
    let mut push = |field: &'static str, l: &str, r: &str| {
        if l != r {
            diffs.push(FieldDiff {
                field,
                left: l.to_owned(),
                right: r.to_owned(),
            });
        }
    };
    push("agent", &left.agent, &right.agent);
    push("map_id", &left.map_id, &right.map_id);
    push("cwd", &left.cwd, &right.cwd);
    push("branch", &left.branch, &right.branch);
    push("source_sha256", &left.source_sha256, &right.source_sha256);
    if left.segments != right.segments {
        diffs.push(FieldDiff {
            field: "segments",
            left: left.segments.to_string(),
            right: right.segments.to_string(),
        });
    }
    diffs
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tb_oracle/_shared")
}

fn grok_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tb_oracle/grok")
}

fn normalize_sha(value: &str) -> String {
    value.trim_start_matches("sha256:").to_owned()
}

fn yaml_str(doc: &serde_yaml::Value, key: &str) -> String {
    doc.get(key)
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or_else(|| panic!("human.md frontmatter missing `{key}`"))
        .to_owned()
}

/// Load the common fields from the TB `human.md` artifact: YAML frontmatter
/// plus the At-A-Glance `| segments | N |` row from the body.
fn common_fields_from_human(text: &str) -> CommonFields {
    let mut parts = text.splitn(3, "---\n");
    let _ = parts.next();
    let frontmatter = parts
        .next()
        .expect("human.md carries a `---` YAML frontmatter block");
    let body = parts.next().expect("human.md carries a body");
    let doc: serde_yaml::Value =
        serde_yaml::from_str(frontmatter).expect("human.md frontmatter parses as YAML");
    let segments = body
        .lines()
        .find_map(|line| {
            let row = line.trim();
            row.strip_prefix("| segments |")
                .map(|rest| rest.trim_matches(['|', ' ']).to_owned())
        })
        .expect("human.md body carries a `| segments | N |` row")
        .parse::<u64>()
        .expect("segments row parses as a count");
    CommonFields {
        agent: yaml_str(&doc, "agent"),
        map_id: yaml_str(&doc, "map_id"),
        cwd: yaml_str(&doc, "cwd"),
        branch: yaml_str(&doc, "branch"),
        source_sha256: normalize_sha(&yaml_str(&doc, "source_jsonl_sha256")),
        segments,
    }
}

fn payload_str(record: &serde_json::Value, key: &str) -> String {
    record
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("index_payload record missing `{key}`"))
        .to_owned()
}

/// Load the common fields from the TB `index_payload.jsonl` artifact: the
/// session-scoped fields of the first record plus the count of distinct
/// `segment_id` values across all records.
fn common_fields_from_index_payload(text: &str) -> CommonFields {
    let mut segments = BTreeSet::new();
    let mut first: Option<serde_json::Value> = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("index_payload line parses as JSON");
        segments.insert(payload_str(&record, "segment_id"));
        first.get_or_insert(record);
    }
    let first = first.expect("index_payload carries at least one record");
    let source_sha = first
        .get("source")
        .and_then(|source| source.get("raw_sha256"))
        .and_then(serde_json::Value::as_str)
        .expect("index_payload record carries source.raw_sha256");
    CommonFields {
        agent: payload_str(&first, "agent"),
        map_id: payload_str(&first, "map_id"),
        cwd: payload_str(&first, "cwd"),
        branch: payload_str(&first, "branch"),
        source_sha256: normalize_sha(source_sha),
        segments: segments.len() as u64,
    }
}

fn load_fixture(name: &str) -> String {
    let path = fixture_dir().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read fixture {}: {error}", path.display()))
}

/// Green direction: the two artifacts of one frozen TB package agree on
/// every common field — the loader and the diff see the same session.
#[test]
fn tb_package_common_fields_agree() {
    let human = common_fields_from_human(&load_fixture("ae59aa08_human.md"));
    let payload = common_fields_from_index_payload(&load_fixture("ae59aa08_index-payload.jsonl"));
    let diffs = diff_common_fields(&human, &payload);
    assert!(
        diffs.is_empty(),
        "TB package disagrees with itself on common fields:\n{}",
        diffs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Red direction: a perturbed field must surface as exactly that field's
/// diff — the harness cannot go green on detuned data.
#[test]
fn perturbed_field_reports_red_diff() {
    let human = common_fields_from_human(&load_fixture("ae59aa08_human.md"));
    let mut detuned = human.clone();
    detuned.branch = "feat/some-other-branch".to_owned();
    detuned.segments += 1;
    let diffs = diff_common_fields(&human, &detuned);
    let fields: Vec<&str> = diffs.iter().map(|diff| diff.field).collect();
    assert_eq!(fields, ["branch", "segments"], "diffs: {diffs:?}");
}

/// Smoke: the written contract exists and carries the sections the W1 wave
/// builds on (TB→aicx mapping table + append-only rule).
#[test]
fn distill_contract_doc_sections_present() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/DISTILL_CONTRACT.md");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    for needle in [
        "## Mapowanie pól TB→aicx",
        "## Reguła append-only",
        "| TB (`index_payload.v1`",
        "AgentLaneDistiller",
    ] {
        assert!(
            text.contains(needle),
            "docs/DISTILL_CONTRACT.md missing section marker {needle:?}"
        );
    }
}

fn load_grok_fixture(name: &str) -> String {
    let path = grok_fixture_dir().join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read grok fixture {}: {error}", path.display()))
}

/// Green: the grok TB package agrees with itself on the W0 common field set.
#[test]
fn grok_tb_package_common_fields_agree() {
    let human = common_fields_from_human(&load_grok_fixture("human.md"));
    let payload = common_fields_from_index_payload(&load_grok_fixture("index-payload.jsonl"));
    let diffs = diff_common_fields(&human, &payload);
    assert!(
        diffs.is_empty(),
        "grok TB package disagrees with itself on common fields:\n{}",
        diffs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The grok TB package common fields match the redacted session fixture
/// (summary.json cwd/branch + chat_history sha256 + one segment).
#[test]
fn grok_tb_package_matches_session_fixture() {
    let human = common_fields_from_human(&load_grok_fixture("human.md"));
    let summary: serde_json::Value =
        serde_json::from_str(&load_grok_fixture("summary.json")).expect("summary.json parses");
    let cwd = summary
        .pointer("/info/cwd")
        .and_then(serde_json::Value::as_str)
        .expect("summary.info.cwd");
    let branch = summary
        .get("head_branch")
        .and_then(serde_json::Value::as_str)
        .expect("summary.head_branch");
    let chat = grok_fixture_dir().join("chat_history.jsonl");
    let bytes = std::fs::read(&chat).expect("read grok chat_history.jsonl");
    let sha = sha256_hex(&bytes);
    assert_eq!(human.agent, "grok");
    assert_eq!(human.cwd, cwd);
    assert_eq!(human.branch, branch);
    assert_eq!(human.source_sha256, sha);
    assert_eq!(human.segments, 1);
    assert_eq!(human.map_id, "grok__01a04490__2026-08-27__w2t9");
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}
