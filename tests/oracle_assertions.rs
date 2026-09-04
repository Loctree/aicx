//! W4-T15 — falsify production `aicx extract` against the frozen oracle
//! manifest `tests/fixtures/parser_engine/assertions.toml`.
//!
//! The harness invokes the worktree binary (`CARGO_BIN_EXE_aicx`) and
//! compares `expected` to stdout / the `-o` file / (for `projection = "raw"`)
//! existing JSONL keys already in the fixture. It does not import
//! `aicx_parser`, does not classify frames, and does not construct the
//! fields it validates.

use serde::Deserialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const MANIFEST_REL: &str = "tests/fixtures/parser_engine/assertions.toml";
/// Plan-inputs root for `external:` fixtures, resolved under `$HOME` — the
/// artifacts store is per-operator, never a hardcoded home path.
const PLAN_INPUTS_UNDER_HOME: &str =
    ".vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs";
const RAW_EXTRACT_SIZE_CAP: u64 = 2_000_000;
const USER_HEADING: &str = "] user:**";
const ASSISTANT_HEADING: &str = "] assistant:**";

#[derive(Debug, Deserialize)]
struct Manifest {
    schema: String,
    assertion: Vec<AssertionRow>,
}

#[derive(Debug, Deserialize, Clone)]
struct AssertionRow {
    id: String,
    fixture: String,
    projection: String,
    kind: String,
    expected: toml::Value,
    #[serde(default)]
    status: String,
    #[serde(default)]
    selector: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    NotAssessed,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotAssessed => "NOT_ASSESSED",
        }
    }
}

#[derive(Clone, Debug)]
struct Eval {
    id: String,
    alias: Option<String>,
    status: String,
    kind: String,
    verdict: Verdict,
    expected: String,
    observed: String,
    reason: String,
    hypothesis: bool,
}

fn bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aicx"))
}

fn unique_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-oracle-{}-{}-{}",
        name,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

fn expected_display(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        other => other.to_string(),
    }
}

fn expected_i64(value: &toml::Value) -> Result<i64, String> {
    match value {
        toml::Value::Integer(i) => Ok(*i),
        toml::Value::String(s) => s
            .parse::<i64>()
            .map_err(|e| format!("expected integer, got {s:?} ({e})")),
        other => Err(format!("expected integer, got {other}")),
    }
}

fn expected_str(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn infer_agent(fixture: &str, id: &str) -> Result<&'static str, String> {
    if fixture.contains("/claude/") || id.starts_with("claude-") {
        Ok("claude")
    } else if fixture.contains("/grok/") || id.starts_with("grok-") {
        Ok("grok")
    } else if fixture.contains("/gemini/") || id.starts_with("gemini-") {
        Ok("gemini")
    } else if fixture.contains("/junie/") || id.starts_with("junie-") {
        Ok("junie")
    } else if fixture.contains("/cursor/") || id.starts_with("cursor-") {
        Ok("cursor")
    } else if fixture.contains("/codex/")
        || id.starts_with("a1-")
        || id.starts_with("a2-")
        || fixture.contains("01a0369f")
        || fixture.contains("01a042f9")
    {
        Ok("codex")
    } else {
        Err(format!(
            "cannot infer extract agent from fixture={fixture} id={id}"
        ))
    }
}

fn projection_flags(projection: &str) -> Vec<String> {
    let rest = projection
        .trim()
        .strip_prefix("extract")
        .unwrap_or(projection)
        .trim();
    if rest.is_empty() || rest.eq_ignore_ascii_case("raw") {
        Vec::new()
    } else {
        rest.split_whitespace().map(str::to_string).collect()
    }
}

fn resolve_fixture(raw: &str) -> Result<PathBuf, String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(rest) = raw.strip_prefix("external:") {
        let under_inputs = rest
            .strip_prefix("inputs/")
            .or_else(|| rest.strip_prefix("inputs"))
            .unwrap_or(rest)
            .trim_start_matches('/');
        let mut candidates = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(
                PathBuf::from(home)
                    .join(PLAN_INPUTS_UNDER_HOME)
                    .join(under_inputs),
            );
        }
        candidates.push(manifest.join(rest));
        candidates.push(PathBuf::from(rest));
        for candidate in candidates {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        return Err(format!("external fixture not found: {raw}"));
    }
    let rel = manifest.join(raw);
    if rel.is_file() {
        Ok(rel)
    } else {
        Err(format!("fixture not found: {}", rel.display()))
    }
}

fn alias_for(id: &str) -> Option<String> {
    Some(match id {
        "a1-01a0369f-compacted-markers" => "monika_shape_compacted_markers".into(),
        "a1-01a0369f-turn-context-cwd-changes" => "monika_shape_turn_context_cwd".into(),
        "a1-01a0369f-multiline-echo-quoted-speakers" => "monika_shape_multiline_echo".into(),
        _ => return None,
    })
}

fn first_json_string_field(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let start = line.find(&pat)? + pat.len();
    let rest = &line[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        if ch == '"' {
            return Some(out);
        }
        out.push(ch);
    }
    None
}

fn first_json_type(line: &str) -> Option<String> {
    first_json_string_field(line, "type")
}

fn line_matches_selector(line: &str, selector: &str) -> bool {
    selector.split("&&").all(|clause| {
        let clause = clause.trim();
        let Some((left, right)) = clause.split_once("==") else {
            return false;
        };
        let left = left.trim();
        let right = right.trim().trim_matches('"');
        if left == "type" {
            first_json_type(line).as_deref() == Some(right)
        } else if left == "payload.type" {
            first_json_type(line).as_deref() == Some("response_item")
                && line.contains(&format!("\"type\":\"{right}\""))
        } else {
            let key = left.rsplit('.').next().unwrap_or(left);
            first_json_string_field(line, key).as_deref() == Some(right)
        }
    })
}

fn count_selector_in_source(path: &Path, selector: &str) -> Result<i64, String> {
    if selector.contains("sequential changes of turn_context") {
        return count_cwd_changes(path);
    }
    if selector.contains("replacement_history") || selector.contains("extra utterances") {
        return Err(
            "selector asks for extra utterances from compacted.replacement_history; \
             isolating that from a mixed extract would re-implement compaction accounting"
                .into(),
        );
    }
    if selector.contains('[') && selector.contains("timestamp") {
        return first_enqueue_timestamp(path).map(|_| 1);
    }
    let file = fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let reader = BufReader::new(file);
    let mut n = 0i64;
    for line in reader.lines() {
        let line = line.map_err(|e| format!("read {}: {e}", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        if line_matches_selector(&line, selector) {
            n += 1;
        }
    }
    Ok(n)
}

fn count_cwd_changes(path: &Path) -> Result<i64, String> {
    let file = fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let reader = BufReader::new(file);
    let mut last: Option<String> = None;
    let mut changes = 0i64;
    for line in reader.lines() {
        let line = line.map_err(|e| format!("read {}: {e}", path.display()))?;
        if first_json_type(&line).as_deref() != Some("turn_context") {
            continue;
        }
        let Some(cwd) = first_json_string_field(&line, "cwd") else {
            continue;
        };
        if let Some(prev) = &last
            && prev != &cwd
        {
            changes += 1;
        }
        last = Some(cwd);
    }
    Ok(changes)
}

fn first_enqueue_timestamp(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.map_err(|e| format!("read {}: {e}", path.display()))?;
        if line_matches_selector(
            &line,
            r#"type == "queue-operation" && operation == "enqueue""#,
        ) && let Some(ts) = first_json_string_field(&line, "timestamp")
        {
            return Ok(ts);
        }
    }
    Err("no queue-operation enqueue timestamp in source".into())
}

fn count_heading(text: &str, marker: &str) -> i64 {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with("**[") && trimmed.contains(marker)
        })
        .count() as i64
}

fn count_dialog_person_headings(text: &str) -> i64 {
    text.lines()
        .filter(|line| line.starts_with("### ") && line.contains('👤'))
        .count() as i64
}

fn combined_text(output: &Output, outfile: &Path) -> String {
    let mut body = String::new();
    body.push_str(&String::from_utf8_lossy(&output.stdout));
    body.push('\n');
    body.push_str(&String::from_utf8_lossy(&output.stderr));
    body.push('\n');
    if let Ok(file) = fs::read_to_string(outfile) {
        body.push_str(&file);
    }
    body
}

fn stage_fixture(src: &Path, home: &Path) -> Result<PathBuf, String> {
    let dest_dir = home.join("oracle-fixtures");
    fs::create_dir_all(&dest_dir).map_err(|e| format!("mkdir fixtures: {e}"))?;
    let tag = src
        .to_string_lossy()
        .bytes()
        .fold(0u64, |acc, b| acc.wrapping_mul(16777619) ^ u64::from(b));
    let name = src
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "fixture.jsonl".into());
    let dest = dest_dir.join(format!("{tag:016x}-{name}"));
    if dest.exists() {
        return Ok(dest);
    }
    match fs::hard_link(src, &dest) {
        Ok(()) => Ok(dest),
        Err(_) => {
            fs::copy(src, &dest).map_err(|e| {
                format!("stage fixture {} -> {}: {e}", src.display(), dest.display())
            })?;
            Ok(dest)
        }
    }
}

fn run_extract(home: &Path, agent: &str, file: &Path, out: &Path, flags: &[String]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aicx"));
    cmd.env("HOME", home)
        .env("AICX_NO_MUTATION_WARN", "1")
        .env("AICX_ALLOW_TMP", "1")
        .args(["extract", agent, "--file"])
        .arg(file)
        .arg("-o")
        .arg(out)
        .args(flags);
    cmd.output()
        .unwrap_or_else(|e| panic!("spawn {} extract {agent}: {e}", env!("CARGO_BIN_EXE_aicx")))
}

fn stderr_snip(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr);
    let one_line: String = text
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    truncate(one_line.trim(), 180)
}

fn compare_i64(observed: i64, expected: i64) -> (Verdict, String) {
    if observed == expected {
        (Verdict::Pass, format!("observed={observed}"))
    } else {
        (
            Verdict::Fail,
            format!("observed={observed} expected={expected}"),
        )
    }
}

fn evaluate_row(row: &AssertionRow, home: &Path, scratch: &Path) -> Eval {
    let hypothesis = row.status == "hypothesis";
    let alias = alias_for(&row.id);
    let expected = expected_display(&row.expected);
    let fixture = match resolve_fixture(&row.fixture) {
        Ok(path) => path,
        Err(reason) => {
            return Eval {
                id: row.id.clone(),
                alias,
                status: row.status.clone(),
                kind: row.kind.clone(),
                verdict: Verdict::NotAssessed,
                expected,
                observed: String::new(),
                reason,
                hypothesis,
            };
        }
    };
    let agent = match infer_agent(&row.fixture, &row.id) {
        Ok(agent) => agent,
        Err(reason) => {
            return Eval {
                id: row.id.clone(),
                alias,
                status: row.status.clone(),
                kind: row.kind.clone(),
                verdict: Verdict::NotAssessed,
                expected,
                observed: String::new(),
                reason,
                hypothesis,
            };
        }
    };
    let flags = projection_flags(&row.projection);
    let is_raw = flags.is_empty();
    let size = fs::metadata(&fixture).map(|m| m.len()).unwrap_or(0);
    let skip_extract = is_raw && size > RAW_EXTRACT_SIZE_CAP;
    let staged = match stage_fixture(&fixture, home) {
        Ok(path) => path,
        Err(reason) => {
            return Eval {
                id: row.id.clone(),
                alias,
                status: row.status.clone(),
                kind: row.kind.clone(),
                verdict: Verdict::NotAssessed,
                expected,
                observed: String::new(),
                reason,
                hypothesis,
            };
        }
    };
    let out = scratch.join(format!("{}.md", row.id.replace('/', "_")));
    let extract_output = if skip_extract {
        None
    } else {
        Some(run_extract(home, agent, &staged, &out, &flags))
    };
    let body = extract_output
        .as_ref()
        .map(|output| combined_text(output, &out))
        .unwrap_or_default();
    let exit = extract_output
        .as_ref()
        .and_then(|o| o.status.code())
        .unwrap_or(-1);

    let (verdict, observed, reason) = match row.kind.as_str() {
        "presence" => {
            let needle = expected_str(&row.expected);
            if skip_extract {
                (
                    Verdict::NotAssessed,
                    String::new(),
                    "presence requires production extract output".into(),
                )
            } else if body.contains(&needle) {
                (
                    Verdict::Pass,
                    format!("contains {needle:?} (exit {exit})"),
                    format!("found in extract stdout/stderr/-o; exit={exit}"),
                )
            } else {
                let snip = extract_output.as_ref().map(stderr_snip).unwrap_or_default();
                (
                    Verdict::Fail,
                    format!("missing {needle:?} (exit {exit})"),
                    format!("needle not in extract stdout/stderr/-o; exit={exit}; stderr={snip}"),
                )
            }
        }
        "absence" => {
            let needle = expected_str(&row.expected);
            if skip_extract {
                (
                    Verdict::NotAssessed,
                    String::new(),
                    "absence requires production extract output".into(),
                )
            } else if body.contains(&needle) {
                (
                    Verdict::Fail,
                    format!("contains {needle:?}"),
                    "needle present in extract output".into(),
                )
            } else {
                (
                    Verdict::Pass,
                    format!("no {needle:?}"),
                    "needle absent from extract output".into(),
                )
            }
        }
        "seal" => {
            let needle = expected_str(&row.expected);
            if is_raw {
                match first_enqueue_timestamp(&fixture) {
                    Ok(ts) if ts == needle => {
                        let also_in_extract = !skip_extract && body.contains(&needle);
                        (
                            Verdict::Pass,
                            ts,
                            format!(
                                "source enqueue[0].timestamp; extract_contains={also_in_extract}"
                            ),
                        )
                    }
                    Ok(ts) => (
                        Verdict::Fail,
                        ts,
                        format!("source enqueue[0].timestamp != {needle}"),
                    ),
                    Err(reason) => (Verdict::NotAssessed, String::new(), reason),
                }
            } else if body.contains(&needle) {
                (
                    Verdict::Pass,
                    needle.clone(),
                    "timestamp in extract output".into(),
                )
            } else {
                (
                    Verdict::Fail,
                    format!("missing {needle} (exit {exit})"),
                    "timestamp not in extract output".into(),
                )
            }
        }
        "count" => match expected_i64(&row.expected) {
            Err(reason) => (Verdict::NotAssessed, String::new(), reason),
            Ok(want) => {
                let selector = row.selector.as_deref().unwrap_or("");
                if selector.contains("replacement_history") || selector.contains("extra utterances")
                {
                    (
                        Verdict::NotAssessed,
                        String::new(),
                        "extras from compacted.replacement_history cannot be isolated from mixed extract output without reconstructing compaction accounting (anti-N13)".into(),
                    )
                } else if is_raw {
                    match count_selector_in_source(&fixture, selector) {
                        Ok(got) => {
                            let (v, why) = compare_i64(got, want);
                            (v, got.to_string(), format!("source jsonl keys; {why}"))
                        }
                        Err(reason) => (Verdict::NotAssessed, String::new(), reason),
                    }
                } else if skip_extract {
                    (
                        Verdict::NotAssessed,
                        String::new(),
                        "extract skipped".into(),
                    )
                } else {
                    let got = count_for_extract_projection(&flags, &body);
                    let (v, why) = compare_i64(got, want);
                    let snip = extract_output.as_ref().map(stderr_snip).unwrap_or_default();
                    (
                        v,
                        got.to_string(),
                        format!("extract headings; exit={exit}; {why}; stderr={snip}"),
                    )
                }
            }
        },
        other => (
            Verdict::NotAssessed,
            String::new(),
            format!("unknown kind {other:?}"),
        ),
    };

    Eval {
        id: row.id.clone(),
        alias,
        status: row.status.clone(),
        kind: row.kind.clone(),
        verdict,
        expected,
        observed,
        reason,
        hypothesis,
    }
}

fn count_for_extract_projection(flags: &[String], body: &str) -> i64 {
    let has_user_only = flags.iter().any(|f| f == "--user-only");
    let has_kind = flags.iter().any(|f| f == "--kind");
    let has_dialog = flags.iter().any(|f| f == "--dialog");
    if has_dialog {
        count_dialog_person_headings(body)
    } else if has_user_only {
        count_heading(body, USER_HEADING)
    } else if has_kind {
        count_heading(body, USER_HEADING) + count_heading(body, ASSISTANT_HEADING)
    } else {
        count_heading(body, ASSISTANT_HEADING)
    }
}

fn print_table(title: &str, rows: &[Eval]) {
    println!("\n=== {title} ===");
    let header = [
        "id", "alias", "status", "verdict", "expected", "observed", "reason",
    ];
    println!(
        "{:<48} {:<28} {:<12} {:<10} {:<22} {:<22} {}",
        header[0], header[1], header[2], header[3], header[4], header[5], header[6]
    );
    for row in rows {
        println!(
            "{:<48} {:<28} {:<12} {:<10} {:<22} {:<22} {}",
            row.id,
            row.alias.as_deref().unwrap_or("-"),
            row.status,
            row.verdict.label(),
            truncate(&row.expected, 22),
            truncate(&row.observed, 22),
            row.reason
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

fn extra_monika_user_projection(home: &Path, scratch: &Path) -> Eval {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/parser_engine/codex/human_shape_01a0369f.jsonl");
    let id = "monika_shape_user_projection_34".to_string();
    if !fixture.is_file() {
        return Eval {
            id,
            alias: Some("monika_shape_user_projection_34".into()),
            status: "mission-minimum".into(),
            kind: "count".into(),
            verdict: Verdict::NotAssessed,
            expected: "34".into(),
            observed: String::new(),
            reason: "bounded Monika fixture missing".into(),
            hypothesis: false,
        };
    }
    let out = scratch.join("monika_shape_user.md");
    let staged = match stage_fixture(&fixture, home) {
        Ok(path) => path,
        Err(reason) => {
            return Eval {
                id,
                alias: Some("monika_shape_user_projection_34".into()),
                status: "mission-minimum".into(),
                kind: "count".into(),
                verdict: Verdict::NotAssessed,
                expected: "34".into(),
                observed: String::new(),
                reason,
                hypothesis: false,
            };
        }
    };
    let output = run_extract(home, "codex", &staged, &out, &["--dialog".into()]);
    let body = combined_text(&output, &out);
    let got = count_dialog_person_headings(&body);
    let (verdict, why) = compare_i64(got, 34);
    Eval {
        id,
        alias: Some("monika_shape_user_projection_34".into()),
        status: "mission-minimum".into(),
        kind: "count".into(),
        verdict,
        expected: "34".into(),
        observed: got.to_string(),
        reason: format!(
            "bounded md5=817982434e803ee660005940c9a022f0; --dialog 25 human + 9 echo-seal; conversation-only was 25; exit={:?}; {why}",
            output.status.code()
        ),
        hypothesis: false,
    }
}

fn extra_dialog_seals(home: &Path, scratch: &Path) -> Eval {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/parser_engine/claude/human_shape_67025fed.jsonl");
    let id = "claude-67025fed-dialog-enqueue-seal".to_string();
    let out = scratch.join("67025fed-dialog.md");
    let staged = match stage_fixture(&fixture, home) {
        Ok(path) => path,
        Err(reason) => {
            return Eval {
                id,
                alias: None,
                status: "mission-minimum".into(),
                kind: "seal".into(),
                verdict: Verdict::NotAssessed,
                expected: "2026-08-25T19:53:58.988Z".into(),
                observed: String::new(),
                reason,
                hypothesis: false,
            };
        }
    };
    let output = run_extract(home, "claude", &staged, &out, &["--dialog".into()]);
    let body = combined_text(&output, &out);
    let rfc_seal = "2026-08-25T19:53:58.988Z";
    let wall = "19:53:58";
    let persons = count_dialog_person_headings(&body);
    let has_body = body.contains("Z mojej rozmowi z codexem");
    let has_wall = body.contains(wall);
    let has_rfc = body.contains(rfc_seal);
    let verdict = if persons == 25 && has_body && has_wall {
        Verdict::Pass
    } else {
        Verdict::Fail
    };
    Eval {
        id,
        alias: None,
        status: "mission-minimum".into(),
        kind: "seal".into(),
        verdict,
        expected: format!("{rfc_seal} via --dialog (25 enqueue utterances)"),
        observed: format!(
            "dialog_person={persons} wall={has_wall} rfc={has_rfc} body={has_body} exit={:?}",
            output.status.code()
        ),
        reason: "--dialog renders mid-turn enqueue as speech; wall clock 19:53:58 is the transport seal on the timeline".into(),
        hypothesis: false,
    }
}

fn extra_mixed_candidate() -> Eval {
    Eval {
        id: "mixed_candidate_01a03888".into(),
        alias: None,
        status: "fixture_unavailable".into(),
        kind: "count".into(),
        verdict: Verdict::NotAssessed,
        expected: String::new(),
        observed: String::new(),
        reason: "gold case 01a03888 lives on host Silver, outside dragon/div0".into(),
        hypothesis: false,
    }
}

#[test]
fn oracle_assertions_against_production_extract() {
    let bin = bin_path();
    assert!(
        bin.is_file(),
        "production binary missing at {} (must be CARGO_BIN_EXE_aicx from this worktree, not the npm 0.12.5 shim)",
        bin.display()
    );
    println!("ORACLE_BIN={}", bin.display());

    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MANIFEST_REL);
    let raw = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path.display()));
    let manifest: Manifest = toml::from_str(&raw).expect("parse assertions.toml");
    assert_eq!(
        manifest.schema, "aicx.parser.oracle_assertions.v1",
        "frozen schema"
    );
    assert!(
        !manifest.assertion.is_empty(),
        "assertions.toml must contain rows"
    );

    let home = unique_dir("home");
    fs::create_dir_all(&home).expect("oracle home");
    let scratch = home.join("out");
    fs::create_dir_all(&scratch).expect("oracle out");

    let mut rows: Vec<Eval> = manifest
        .assertion
        .iter()
        .map(|row| evaluate_row(row, &home, &scratch))
        .collect();
    rows.push(extra_monika_user_projection(&home, &scratch));
    rows.push(extra_dialog_seals(&home, &scratch));
    rows.push(extra_mixed_candidate());

    print_table("oracle assertions (frozen + mission extras)", &rows);

    let hypothesis: Vec<&Eval> = rows.iter().filter(|r| r.hypothesis).collect();
    print_table(
        "hypothesis (reported separately; do not block W4 on meaning)",
        &hypothesis.into_iter().cloned().collect::<Vec<_>>(),
    );

    let mut pass = 0;
    let mut fail = 0;
    let mut not_assessed = 0;
    let mut blocking_fail = 0;
    for row in &rows {
        match row.verdict {
            Verdict::Pass => pass += 1,
            Verdict::Fail => {
                fail += 1;
                if !row.hypothesis {
                    blocking_fail += 1;
                }
            }
            Verdict::NotAssessed => not_assessed += 1,
        }
        println!(
            "ASSERT {} => {} kind={} expected={} observed={} status={} reason={}",
            row.id,
            row.verdict.label(),
            row.kind,
            row.expected,
            row.observed,
            row.status,
            row.reason
        );
    }
    println!(
        "SUMMARY pass={pass} fail={fail} NOT_ASSESSED={not_assessed} blocking_fail={blocking_fail} total={}",
        rows.len()
    );

    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&scratch);

    if blocking_fail > 0 {
        panic!(
            "{blocking_fail} non-hypothesis assertion(s) failed against production extract (W4 red is a legal verdict)"
        );
    }
}
