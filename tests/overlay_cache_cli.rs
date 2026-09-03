//! Black-box overlay cache contracts. Every child owns an isolated AICX home;
//! unresolved fixture claims deliberately require no embedding provider.
#![cfg(unix)]

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    loct: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "aicx-overlay-cli-{label}-{}-{nonce}",
            std::process::id()
        ));
        let home = root.join("home");
        let repo = root.join("example").join("overlay-fixture");
        fs::create_dir_all(home.join("catalog")).unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/lib.rs"), "pub fn unrelated_anchor() {}\n").unwrap();
        fs::write(
            root.join("anchors.json"),
            serde_json::to_vec(&json!({
                "repo_id": "example/overlay-fixture",
                "snapshot_commit": "deadbeef",
                "anchor_catalog_revision": "acr1:fixture",
                "producer_version": "fixture",
                "anchors": [{"anchor_id": "anc1:fixture", "normalized_path": "src/lib.rs",
                    "language": "rs", "qualified_symbol": null, "signature_hash": null}]
            }))
            .unwrap(),
        )
        .unwrap();
        let loct = root.join("loct");
        fs::write(
            &loct,
            concat!(
                "#!/bin/sh\nset -eu\n",
                "if [ -n \"${OVERLAY_FIXTURE_GATE:-}\" ]; then\n",
                "  : > \"$OVERLAY_FIXTURE_ENTERED\"\n",
                "  attempts=0\n",
                "  while [ ! -f \"$OVERLAY_FIXTURE_GATE\" ]; do\n",
                "    attempts=$((attempts + 1))\n",
                "    [ \"$attempts\" -lt 500 ] || exit 2\n",
                "    sleep 0.02\n",
                "  done\n",
                "fi\n",
                "exec /bin/cat \"$OVERLAY_FIXTURE_ANCHORS\"\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&loct, fs::Permissions::from_mode(0o755)).unwrap();
        let fixture = Self {
            root,
            home,
            repo,
            loct,
        };
        fixture.write_source(0, "preserve historical evidence");
        fixture.write_source(1, "retain operator decisions");
        fixture.write_catalog(&[0, 1]);
        fixture
    }

    fn source(&self, number: usize) -> PathBuf {
        self.home
            .join("runtime_runs")
            .join(format!("source-{number}.jsonl"))
    }

    fn cache_root(&self) -> PathBuf {
        let hash = hex::encode(Sha256::digest(b"example/overlay-fixture"));
        self.home.join("overlay-index-v1").join(&hash[..16])
    }

    fn write_source(&self, number: usize, decision: &str) {
        let path = self.source(number);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let session = format!("019f1111-2222-7333-8444-{number:012}");
        let rows = [
            json!({"timestamp":"2026-07-25T10:00:00Z", "type":"session_meta", "payload":{
                "id":session,"cwd":self.repo,"source":"cli"}}),
            json!({"timestamp":"2026-07-25T10:00:01Z", "type":"response_item", "payload":{
                "type":"message","role":"user","content":[{"type":"input_text",
                "text":format!("DECISION: {decision} in lib.rs for source {number}. WHY: full history must remain recoverable.")} ]}}),
            json!({"timestamp":"2026-07-25T10:00:02Z", "type":"response_item", "payload":{
                "type":"message","role":"assistant","content":[{"type":"output_text",
                "text":format!("Implemented evidence preservation in lib.rs for source {number}; verified the original source references.")} ]}}),
        ];
        let body = rows
            .iter()
            .map(|row| serde_json::to_string(row).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(path, body).unwrap();
    }

    fn write_catalog(&self, numbers: &[usize]) {
        let body = numbers
            .iter()
            .map(|number| {
                serde_json::to_string(&json!({
                    "schema":"aicx.catalog.session.v1",
                    "session_id":format!("019f1111-2222-7333-8444-{number:012}"),
                    "agent":"codex", "project":"example/overlay-fixture", "date":"2026-07-25",
                    "cwd":self.repo, "source_path":self.source(*number), "machine":"fixture"
                }))
                .unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        fs::write(self.home.join("catalog/sessions.jsonl"), body).unwrap();
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aicx"));
        command
            .args(["overlay", "--repo"])
            .arg(&self.repo)
            .args(["--format", "json"])
            .env("HOME", self.root.join("operator-home"))
            .env("AICX_HOME", &self.home)
            // The parser has an independent temporary-path gate. This is
            // confined to this child; SourceAllowlist still fences its home.
            .env("AICX_ALLOW_TMP", "1")
            .env("LOCT_BIN", &self.loct)
            .env("OVERLAY_FIXTURE_ANCHORS", self.root.join("anchors.json"))
            .env("AICX_NO_MUTATION_WARN", "1")
            .env_remove("AICX_OVERLAY_TEST_ROOT")
            .env_remove("AICX_OVERLAY_TEST_OUTPUT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, rebuild: bool) -> (Value, String) {
        let mut command = self.command();
        if rebuild {
            command.arg("--rebuild");
        }
        successful_output(command.output().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn successful_output(output: Output) -> (Value, String) {
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(output.status.success(), "overlay failed: {stderr}");
    let document: Value =
        serde_json::from_slice(&output.stdout).expect("stdout must contain only overlay JSON");
    assert_eq!(document["schema"], "loctree.overlay.intent.v1");
    assert!(
        !document["entries"].as_array().unwrap().is_empty()
            || !document["unresolved_attributions"]
                .as_array()
                .is_none_or(Vec::is_empty),
        "fixture must produce claims: {document}\n{stderr}"
    );
    (document, stderr)
}

fn stat(stderr: &str, name: &str) -> String {
    let prefix = format!("{name}=");
    stderr
        .split_whitespace()
        .find_map(|word| word.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("missing {name} instrumentation: {stderr}"))
        .to_owned()
}

fn parsed(stderr: &str) -> usize {
    stat(stderr, "source_sessions_parsed").parse().unwrap()
}

fn source_slot_count(root: &Path) -> usize {
    fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(hash) = name
                .strip_prefix("catalog-source-v1-")
                .and_then(|name| name.strip_suffix(".json"))
            else {
                return false;
            };
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .count()
}

#[test]
fn unchanged_cli_overlay_skips_parsing_and_rebuild_preserves_document() {
    let fixture = Fixture::new("warm");
    let (cold, first) = fixture.run(false);
    assert_eq!(
        parsed(&first),
        2,
        "each session must be parsed once, not once per frame lane"
    );
    assert_eq!(stat(&first, "feed_cache_hit"), "false");
    let (warm, second) = fixture.run(false);
    assert_eq!(
        cold, warm,
        "warm output must preserve all revisions and evidence"
    );
    assert_eq!(parsed(&second), 0);
    assert_eq!(stat(&second, "feed_cache_hit"), "true");
    let (rebuilt, third) = fixture.run(true);
    assert_eq!(parsed(&third), 2);
    assert_eq!(stat(&third, "feed_cache_hit"), "false");
    assert_eq!(rebuilt, cold);
}

#[test]
fn source_changes_without_catalog_rebuild_parse_only_changed_source() {
    let fixture = Fixture::new("incremental");
    let (before, _) = fixture.run(false);
    fixture.write_source(
        1,
        "retain every original operator decision and its provenance",
    );
    let (incremental, stats) = fixture.run(false);
    assert_eq!(parsed(&stats), 1);
    assert_eq!(stat(&stats, "source_sessions_reused"), "1");
    assert_ne!(before["store_revision"], incremental["store_revision"]);
    assert_eq!(
        incremental,
        fixture.run(true).0,
        "incremental semantics must equal a full rebuild"
    );
    assert_eq!(parsed(&fixture.run(false).1), 0);
}

#[test]
fn catalog_addition_removal_and_reordering_do_not_resurrect_claims() {
    let fixture = Fixture::new("catalog");
    fixture.run(false);
    fixture.write_source(2, "keep new source evidence");
    fixture.write_catalog(&[0, 1, 2]);
    let (added, stats) = fixture.run(false);
    assert_eq!(parsed(&stats), 1);
    assert_eq!(source_slot_count(&fixture.cache_root()), 3);
    fixture.write_catalog(&[2, 0]);
    let (removed, stats) = fixture.run(false);
    assert_eq!(parsed(&stats), 0);
    assert_eq!(
        source_slot_count(&fixture.cache_root()),
        2,
        "removed catalog rows must not retain full conversation slots"
    );
    assert_ne!(added["store_revision"], removed["store_revision"]);
    assert_eq!(removed, fixture.run(true).0);
    fixture.write_catalog(&[0, 2]);
    assert_eq!(fixture.run(false).0, removed);
}

#[test]
fn same_length_source_rewrite_with_restored_mtime_invalidates_cache() {
    let fixture = Fixture::new("same-mtime");
    let (before, _) = fixture.run(false);
    let source = fixture.source(0);
    let metadata = fs::metadata(&source).unwrap();
    let text = fs::read_to_string(&source).unwrap();
    let replacement = text.replace(
        "preserve historical evidence",
        "preserve historical archives",
    );
    assert_eq!(text.len(), replacement.len());
    fs::write(&source, replacement).unwrap();
    filetime::set_file_mtime(
        &source,
        filetime::FileTime::from_last_modification_time(&metadata),
    )
    .unwrap();
    let (after, stats) = fixture.run(false);
    assert_eq!(parsed(&stats), 1);
    assert_ne!(before["store_revision"], after["store_revision"]);
}

#[test]
fn timestamp_only_source_change_moves_the_published_revision() {
    let fixture = Fixture::new("timestamp");
    let (before, _) = fixture.run(false);
    let source = fixture.source(0);
    let text = fs::read_to_string(&source).unwrap();
    fs::write(&source, text.replace("T10:00:", "T11:00:")).unwrap();
    let (after, stats) = fixture.run(false);
    assert_eq!(parsed(&stats), 1);
    assert_ne!(
        before["store_revision"], after["store_revision"],
        "provenance time is part of the materialized feed, even when all message text is unchanged"
    );
    assert_ne!(before["overlay_revision"], after["overlay_revision"]);
    assert_eq!(after, fixture.run(true).0);
}

#[test]
fn oversized_codex_projection_is_warm_without_rereading_its_records() {
    let fixture = Fixture::new("oversized");
    let mut source = fs::OpenOptions::new()
        .write(true)
        .open(fixture.source(0))
        .unwrap();
    // A terminated oversized record after valid signal exercises the same
    // bounded-reader branch as historical tool/image-heavy rollouts.
    source.set_len(65 * 1024 * 1024).unwrap();
    source.seek(SeekFrom::End(0)).unwrap();
    source.write_all(b"\n").unwrap();
    drop(source);
    let started = Instant::now();
    let (cold, cold_stats) = fixture.run(false);
    let cold_elapsed = started.elapsed();
    assert_eq!(parsed(&cold_stats), 2);
    let started = Instant::now();
    let (warm, warm_stats) = fixture.run(false);
    let warm_elapsed = started.elapsed();
    assert_eq!(warm, cold);
    assert_eq!(parsed(&warm_stats), 0);
    assert_eq!(stat(&warm_stats, "feed_cache_hit"), "true");
    assert!(
        warm_stats.contains("bounded"),
        "warm hit must retain bounded-coverage warning: {warm_stats}"
    );
    assert!(!warm_stats.contains("019f1111"));
    assert!(!warm_stats.contains(fixture.home.to_string_lossy().as_ref()));
    eprintln!("65MiB CLI fixture: cold={cold_elapsed:?}, warm={warm_elapsed:?}, warm parsed=0");
}

#[test]
fn source_symlink_escape_never_reuses_previously_authorized_claims() {
    let fixture = Fixture::new("escape");
    let (before, _) = fixture.run(false);
    let source = fixture.source(1);
    let outside = fixture.root.join("outside.jsonl");
    fs::rename(&source, &outside).unwrap();
    symlink(&outside, &source).unwrap();
    let output = fixture.command().output().unwrap();
    if output.status.success() {
        let (after, _) = successful_output(output);
        assert_ne!(
            before["store_revision"], after["store_revision"],
            "escaped source cannot survive as trusted cached claims"
        );
        assert_eq!(after, fixture.run(true).0);
    } else {
        assert!(String::from_utf8_lossy(&output.stderr).contains("source"));
    }
}

#[test]
fn missing_catalog_source_is_rechecked_without_disabling_warm_cache() {
    let fixture = Fixture::new("missing");
    let (complete, _) = fixture.run(false);
    fs::remove_file(fixture.source(1)).unwrap();
    let (missing, _) = fixture.run(false);
    assert_eq!(
        source_slot_count(&fixture.cache_root()),
        1,
        "stable missing source must drop its obsolete conversation slot"
    );
    assert_ne!(complete["store_revision"], missing["store_revision"]);
    let (warm, stats) = fixture.run(false);
    assert_eq!(warm, missing);
    assert_eq!(parsed(&stats), 0);
    assert_eq!(stat(&stats, "feed_cache_hit"), "true");
    assert!(
        stats.contains("source file is missing"),
        "cached absence must remain visible"
    );
    assert!(!stats.contains("019f1111"));
    assert!(!stats.contains(fixture.home.to_string_lossy().as_ref()));
    fixture.write_source(1, "retain operator decisions");
    let (restored, stats) = fixture.run(false);
    assert_eq!(parsed(&stats), 1);
    assert_eq!(source_slot_count(&fixture.cache_root()), 2);
    assert_eq!(restored, complete);
}

#[test]
fn ignore_policy_change_cannot_reuse_cached_claims() {
    let fixture = Fixture::new("ignore");
    let (before, _) = fixture.run(false);
    fs::write(
        fixture.home.join(".aicxignore"),
        format!("{}\n", fixture.repo.display()),
    )
    .unwrap();
    let denied = fixture.command().output().unwrap();
    assert!(
        !denied.status.success(),
        "ignored repo must not reuse earlier claims"
    );
    assert!(denied.stdout.is_empty());
    fs::write(fixture.home.join(".aicxignore"), "").unwrap();
    assert_eq!(fixture.run(false).0, before);
}

#[test]
fn corrupt_private_feed_is_rebuilt_from_cached_sources_without_changing_evidence() {
    let fixture = Fixture::new("corrupt");
    let (before, _) = fixture.run(false);
    fs::write(
        fixture.cache_root().join("catalog-feed-v1.json"),
        "{incomplete",
    )
    .unwrap();
    let (after, stats) = fixture.run(false);
    assert_eq!(after, before);
    assert_eq!(parsed(&stats), 0);
    assert_eq!(stat(&stats, "source_sessions_reused"), "2");
    assert_eq!(stat(&stats, "feed_cache_hit"), "false");
    assert_eq!(stat(&fixture.run(false).1, "feed_cache_hit"), "true");
}

#[test]
fn derived_conversation_and_feed_files_are_owner_only() {
    let fixture = Fixture::new("private-cache");
    let mut command = fixture.command();
    use std::os::unix::process::CommandExt;
    // Only the forked test child changes its umask; the test runner and
    // concurrent fixtures keep their own process settings.
    unsafe {
        command.pre_exec(|| {
            libc::umask(0);
            Ok(())
        });
    }
    successful_output(command.output().unwrap());
    let mut checked = 0;
    for entry in fs::read_dir(fixture.cache_root()).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0,
                "derived content must be private even with permissive caller umask: {}",
                path.display()
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 4,
        "must inspect actual source, feed, and output cache files"
    );
}

fn wait_for_file(path: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "child exited before anchor barrier"
        );
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("timed out waiting for fixture anchor barrier");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn concurrent_cli_producers_share_the_completed_feed() {
    let fixture = Fixture::new("concurrent");
    let gate = fixture.root.join("release-anchors");
    let entered = fixture.root.join("entered-anchors");
    let mut command = fixture.command();
    command
        .env("OVERLAY_FIXTURE_GATE", &gate)
        .env("OVERLAY_FIXTURE_ENTERED", &entered);
    let mut first = command.spawn().unwrap();
    wait_for_file(&entered, &mut first);
    let second = command.spawn().unwrap();
    fs::write(&gate, b"release").unwrap();
    let (a, first_stats) = successful_output(first.wait_with_output().unwrap());
    let (b, second_stats) = successful_output(second.wait_with_output().unwrap());
    assert_eq!(a, b);
    assert_eq!(
        parsed(&first_stats) + parsed(&second_stats),
        2,
        "concurrent callers must not duplicate source parsing"
    );
    assert!(
        stat(&first_stats, "feed_cache_hit") == "true"
            || stat(&second_stats, "feed_cache_hit") == "true"
    );
}

#[test]
fn live_advisory_owner_blocks_producer_even_when_lock_file_is_old() {
    let fixture = Fixture::new("live-owner");
    let cache_root = fixture.cache_root();
    fs::create_dir_all(&cache_root).unwrap();
    let repo_hash = cache_root.file_name().unwrap().to_str().unwrap();
    let lock_path = cache_root.join(format!("producer-{repo_hash}.lock"));
    let owner = aicx::locks::acquire_exclusive(&lock_path).unwrap();
    // POSIX record locks are released when this process closes any fd for
    // their inode. Age the file in a separate process so the fixture does
    // not accidentally unlock its own owner while setting timestamps.
    assert!(
        Command::new("touch")
            .args(["-t", "197001010001.00"])
            .arg(&lock_path)
            .status()
            .unwrap()
            .success()
    );

    let entered = fixture.root.join("entered-anchors");
    let gate = fixture.root.join("release-anchors");
    fs::write(&gate, b"open").unwrap();
    let mut child = fixture
        .command()
        .env("OVERLAY_FIXTURE_GATE", &gate)
        .env("OVERLAY_FIXTURE_ENTERED", &entered)
        .spawn()
        .unwrap();
    wait_for_file(&entered, &mut child);
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        child.try_wait().unwrap().is_none(),
        "live owner cannot be displaced by lock age"
    );
    assert!(
        !cache_root.join("catalog-feed-v1.json").exists(),
        "waiting producer must not publish a feed"
    );
    assert!(
        !fs::read_dir(&cache_root).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("catalog-source-v1-")),
        "waiting producer must not parse/materialize source slots"
    );
    drop(owner);
    let (_, stats) = successful_output(child.wait_with_output().unwrap());
    assert_eq!(parsed(&stats), 2);
}
