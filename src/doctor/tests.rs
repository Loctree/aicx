use super::*;
use std::path::PathBuf;

#[test]
fn test_severity_unknown_is_default_not_warning() {
    let default_res = CheckResult::default();
    assert_eq!(default_res.severity, Severity::Unknown);
}

#[test]
fn check_state_accepts_state_json_above_generic_cap() {
    // Regression for the case where doctor::check_state called
    // sanitize::read_to_string_validated (generic 8 MiB cap) on
    // state.json. Real installations with 200k+ chunks produce
    // state.json ≥ 20 MiB, which the generic reader rejected,
    // surfacing a spurious Critical. The fix routes through
    // sanitize::read_state_json_validated (dedicated 128 MiB cap,
    // see crates/aicx-parser/src/sanitize.rs). This test writes a
    // ≥ 9 MiB state.json (above generic, well below dedicated)
    // and asserts the check returns Green.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = std::env::temp_dir().join(format!(
        "aicx-doctor-state-cap-{}-{}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir_all(&base).expect("create base");
    let state_path = base.join("state.json");
    // 9 MiB of `A` padding embedded in a valid JSON envelope, above
    // the 8 MiB generic cap, well under the 128 MiB state cap.
    let padding = "A".repeat(9 * 1024 * 1024);
    let body = format!(r#"{{"pad":"{}","seen_hashes":{{}},"runs":[]}}"#, padding);
    std::fs::write(&state_path, &body).expect("write state.json");

    let result = check_state(&base);

    // Cleanup before assertion so a failure does not leave the dir.
    let _ = std::fs::remove_dir_all(&base);

    assert_eq!(
        result.severity,
        Severity::Green,
        "≥9 MiB state.json must pass under the dedicated 128 MiB cap; got: {:?}",
        result
    );
    assert!(
        result.detail.contains("parses cleanly"),
        "expected 'parses cleanly' detail, got: {}",
        result.detail
    );
}

#[test]
fn test_aggregation_unknown_does_not_inflate_warning() {
    assert_eq!(
        max_severity(&[
            Severity::Green,
            Severity::Unknown,
            Severity::Skipped,
            Severity::NotConfigured
        ]),
        Severity::Green
    );
    assert_eq!(
        max_severity(&[Severity::Green, Severity::Unknown, Severity::Warning]),
        Severity::Warning
    );
}

#[test]
fn test_legacy_report_deserialization_maps_missing_fields_to_unknown() {
    let legacy_json = r#"{
        "canonical_store": {"name":"canonical","severity":"green","detail":"ok"},
        "steer_lance": {"name":"lance","severity":"green","detail":"ok"},
        "steer_bm25": {"name":"bm25","severity":"green","detail":"ok"},
        "state": {"name":"state","severity":"green","detail":"ok"},
        "sidecars": {"name":"sidecars","severity":"green","detail":"ok"},
        "corpus_buckets": {"name":"buckets","severity":"green","detail":"ok"},
        "noise_health": {"name":"noise","severity":"green","detail":"ok"},
        "fixes_applied": [],
        "overall": "green"
    }"#;
    let report: DoctorReport = serde_json::from_str(legacy_json).unwrap();
    // Since CheckResult::default() uses Unknown, these should be Unknown!
    assert_eq!(report.semantic_health.severity, Severity::Unknown);
    assert_eq!(report.embedder_warmth.severity, Severity::Unknown);
    assert_eq!(report.schema_version, 2); // default_schema_version_2 returns 2
}

#[test]
fn test_check_embedder_warmth_without_smoke_returns_skipped() {
    let opts = DoctorOptions {
        rebuild_steer_index: false,
        fix_buckets: false,
        dry_run: false,
        rebuild_sidecars: false,
        prune_empty_bodies: false,
        apply_prune_empty_bodies: false,
        check_dedup: false,
        verbose: false,
        smoke: false,
    };
    // Even if config is missing or local, without smoke it skips.
    // Actually check_embedder_warmth reads from env, which could be anything in test,
    // but regardless, if smoke=false, it should return Skipped or NotConfigured (if no feature).
    let check = check_embedder_warmth(&opts);
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    assert_eq!(check.severity, Severity::Skipped);
    #[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
    assert_eq!(check.severity, Severity::NotConfigured);
}
#[test]
fn test_max_severity_promotes_critical() {
    assert_eq!(
        max_severity(&[Severity::Green, Severity::Warning]),
        Severity::Warning
    );
    assert_eq!(max_severity(&[Severity::Green]), Severity::Green);
}

#[test]
fn oracle_readiness_is_ready_when_semantic_and_freshness_are_green() {
    let report = DoctorReport {
        schema_version: 2,
        canonical_store: CheckResult {
            name: "canonical".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        steer_lance: CheckResult {
            name: "metadata_steer_index lance".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        steer_bm25: CheckResult {
            name: "metadata_steer_index bm25".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        state: CheckResult {
            name: "state".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        sidecars: CheckResult {
            name: "sidecars".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        corpus_buckets: CheckResult {
            name: "buckets".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        noise_health: CheckResult {
            name: "noise".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        semantic_health: CheckResult {
            name: "semantic".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        index_freshness: CheckResult {
            name: "freshness".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        index_consistency: CheckResult {
            name: "index_consistency".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        sidecar_coverage: CheckResult {
            name: "sidecar_coverage".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        embedder_warmth: CheckResult {
            name: "warmth".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        empty_body_chunks: CheckResult {
            name: "empty_body_chunks".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        content_dedup: CheckResult {
            name: "content_dedup".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        context_corpus: CheckResult {
            name: "context_corpus".to_string(),
            severity: Severity::Green,
            detail: "ok".to_string(),
            recommendation: None,
        },
        aicx_home: CheckResult::default(),
        binary_pair: CheckResult::default(),
        http_auth_token: CheckResult::default(),
        rebuild_sidecars_script: None,
        prune_empty_bodies_script: None,
        fixes_applied: Vec::new(),
        overall: Severity::Green,
    };

    let readiness = oracle_readiness(&report);
    assert_eq!(readiness.readiness_label, "ready");
    assert_eq!(readiness.content_semantic_index_health, Severity::Green);
    assert_eq!(
        readiness.dashboard_semantic_route_health,
        Severity::NotConfigured
    );
}

#[test]
fn index_consistency_flags_orphaned_and_missing_tuples() {
    let tmp = unique_test_dir("index-consistency");
    let dir = tmp
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0506")
        .join("conversations")
        .join("codex");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("2026_0506_codex_sess-a_001.md"), "real chunk").unwrap();
    std::fs::write(
        tmp.join("index.json"),
        serde_json::json!({
            "projects": {
                "Vetcoders/aicx": {
                    "agents": {
                        "codex": {
                            "dates": ["2026_0505"],
                            "total_entries": 1,
                            "last_updated": "2026-05-06T00:00:00Z"
                        }
                    }
                }
            },
            "last_updated": "2026-05-06T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();

    let check = check_index_consistency(&tmp);
    assert_eq!(check.severity, Severity::Warning);
    assert!(check.detail.contains("1 orphaned index tuple"));
    assert!(check.detail.contains("1 missing index tuple"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_canonical_store_warns_when_missing() {
    let tmp = std::env::temp_dir().join(format!("aicx-doctor-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let result = check_canonical_store(&tmp);
    assert_eq!(result.severity, Severity::Warning);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_aicx_home_is_green_when_store_present() {
    let tmp = unique_test_dir("home-present");
    std::fs::create_dir_all(tmp.join("store")).unwrap();
    let result = check_aicx_home(&tmp);
    assert_eq!(result.name, "aicx_home");
    assert_eq!(result.severity, Severity::Green);
    assert!(result.detail.contains("store/ present"));
    assert!(result.detail.contains(&tmp.display().to_string()));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_aicx_home_warns_and_advises_when_store_missing() {
    let tmp = unique_test_dir("home-missing");
    std::fs::create_dir_all(&tmp).unwrap();
    let result = check_aicx_home(&tmp);
    assert_eq!(result.name, "aicx_home");
    assert_eq!(result.severity, Severity::Warning);
    assert!(result.detail.contains("store/ missing"));
    assert!(result.recommendation.unwrap().contains("AICX_HOME"));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_http_auth_token_is_informational_and_never_leaks_value() {
    let result = check_http_auth_token();
    assert_eq!(result.name, "http_auth_token");
    // Always Green: it reports a source, it does not gate health.
    assert_eq!(result.severity, Severity::Green);
    assert!(result.detail.starts_with("HTTP auth token source:"));
    let _ = result.recommendation;
}

#[test]
fn check_corpus_buckets_green_when_only_valid_names() {
    let tmp = unique_test_dir("valid-buckets");
    let store = tmp.join("store");
    std::fs::create_dir_all(store.join("vetcoders").join("aicx")).unwrap();
    std::fs::create_dir_all(store.join("libraxisai").join("vista")).unwrap();
    std::fs::create_dir_all(store.join("local")).unwrap();

    let result = check_corpus_buckets(&tmp);
    assert_eq!(result.severity, Severity::Green);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_corpus_buckets_flags_template_literals() {
    let tmp = unique_test_dir("bad-buckets");
    let store = tmp.join("store");
    std::fs::create_dir_all(store.join("vetcoders")).unwrap();
    // Mid-segment garbage: backtick, newlines, asterisks. Validator
    // rejects mid-segment non-`[A-Za-z0-9._-]` chars regardless of
    // case (relaxed 2026-05-12 only loosened case + leading-char
    // rules; mid-segment garbage stays junk).
    std::fs::create_dir_all(store.join("vetcoders").join("vibecrafted.git`")).unwrap();
    // Names containing Windows-illegal characters (`*`, control chars, `<`,
    // `>`) cannot exist on NTFS at all, so the scanner can never encounter
    // them on Windows — they are an impossible input there, not a skipped
    // case. Create + assert them on non-Windows only.
    #[cfg(not(windows))]
    std::fs::create_dir_all(store.join("vetcoders").join("loctree\n\n**AICX")).unwrap();
    // Template-placeholder leaks (leading `{`, `<`, `$`):
    std::fs::create_dir_all(store.join("{target_owner}")).unwrap();
    #[cfg(not(windows))]
    std::fs::create_dir_all(store.join("<owner>")).unwrap();
    std::fs::create_dir_all(store.join("$RELEASE_REPO")).unwrap();
    // (Note: pure dot-string `"..."` was previously asserted as junk,
    // but `.` is now an allowed leading-and-mid char, so dotfile-only
    // names pass — semantically weird but not validator-relevant.)

    let result = check_corpus_buckets(&tmp);
    assert_eq!(result.severity, Severity::Warning);
    assert!(result.detail.contains("{target_owner}"));
    #[cfg(not(windows))]
    assert!(result.detail.contains("<owner>"));
    assert!(result.detail.contains("$RELEASE_REPO"));
    assert!(result.detail.contains("vetcoders/vibecrafted.git`"));
    #[cfg(not(windows))]
    assert!(result.detail.contains("vetcoders/loctree"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn quarantine_moves_bucket_atomically() {
    let tmp = unique_test_dir("quarantine-move");
    let store = tmp.join("store");
    let bad = store.join("{x}");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("test.md"), "content").unwrap();

    let dest = quarantine_bucket(&store, "{x}").unwrap();
    assert!(dest.exists());
    assert!(!bad.exists());
    assert!(dest.join("test.md").exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

// The nested bucket name carries Windows-illegal characters (`"`, `<`, `>`),
// so it cannot exist on NTFS — quarantine of a legal-but-invalid bucket is
// covered portably by `quarantine_moves_bucket_atomically` (`{x}`).
#[cfg(not(windows))]
#[test]
fn quarantine_moves_nested_repo_bucket_atomically() {
    let tmp = unique_test_dir("quarantine-nested-move");
    let store = tmp.join("store");
    let bad = store.join("vetcoders").join("vc-skills.git\"><span");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("test.md"), "content").unwrap();

    let dest = quarantine_bucket(&store, "vetcoders/vc-skills.git\"><span").unwrap();
    assert!(dest.exists());
    assert!(!bad.exists());
    assert!(dest.join("test.md").exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn quarantine_skips_when_no_buckets_match() {
    let tmp = unique_test_dir("quarantine-noop");
    let store = tmp.join("store");
    std::fs::create_dir_all(store.join("vetcoders").join("aicx")).unwrap();
    std::fs::create_dir_all(store.join("local")).unwrap();

    let suspicious = scan_corpus_buckets(&store).unwrap();
    assert!(suspicious.is_empty());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn scan_passes_camelcase_dotfile_underscore_buckets_through() {
    let tmp = unique_test_dir("scan-passthrough");
    let store = tmp.join("store");

    // Legitimate CamelCase GitHub orgs — case-preserving canonical form.
    // Pre-2026-05-12 these would have been mass-quarantined by the
    // lowercase-only validator (the `20260509_023025` incident).
    // Post-relax they pass the validator unchanged.
    std::fs::create_dir_all(store.join("LibraxisAI").join("vista")).unwrap();
    std::fs::create_dir_all(store.join("Vetcoders").join("Vista")).unwrap();
    std::fs::create_dir_all(store.join("Loctree").join("aicx")).unwrap();
    std::fs::create_dir_all(store.join("Szowesgad").join("family-onko-portal")).unwrap();

    // Already-lowercase legacy buckets:
    std::fs::create_dir_all(store.join("vetcoders").join("aicx")).unwrap();
    std::fs::create_dir_all(store.join("local")).unwrap();

    // Dot-prefix buckets — UNIX hidden-dir + GitHub `.github`-style
    // convention. Validator accepts directly (relaxed 2026-05-12).
    std::fs::create_dir_all(store.join(".scripts")).unwrap();
    std::fs::create_dir_all(store.join(".aicx")).unwrap();
    std::fs::create_dir_all(store.join(".github")).unwrap();

    // Underscore-prefix buckets — code-convention naming.
    std::fs::create_dir_all(store.join("_internal")).unwrap();

    // Truly invalid (template placeholders, leading non-alphanumeric):
    std::fs::create_dir_all(store.join("{target_owner}").join("repo")).unwrap();
    // `<owner>` holds Windows-illegal characters — impossible on NTFS.
    #[cfg(not(windows))]
    std::fs::create_dir_all(store.join("<owner>")).unwrap();
    std::fs::create_dir_all(store.join("$RELEASE_REPO")).unwrap();

    let suspicious = scan_corpus_buckets(&store).unwrap();

    // Legit names of every relaxed shape are NOT suspicious:
    for legit in [
        "LibraxisAI",
        "Vetcoders",
        "Loctree",
        "Szowesgad",
        "vetcoders",
        "local",
        ".scripts",
        ".aicx",
        ".github",
        "_internal",
    ] {
        assert!(
            suspicious.iter().all(|n| n != legit),
            "{legit} should not be in suspicious"
        );
    }

    // Real text-extracted junk and placeholder leaks ARE suspicious:
    assert!(suspicious.iter().any(|n| n == "{target_owner}"));
    #[cfg(not(windows))]
    assert!(suspicious.iter().any(|n| n == "<owner>"));
    assert!(suspicious.iter().any(|n| n == "$RELEASE_REPO"));

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn empty_body_chunks_red_when_over_threshold_and_script_is_reviewable() {
    let tmp = unique_test_dir("empty-bodies");
    let dir = tmp
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0506")
        .join("conversations")
        .join("claude");
    std::fs::create_dir_all(&dir).unwrap();
    let empty = dir.join("2026_0506_claude_sess-empty_001.md");
    let full = dir.join("2026_0506_claude_sess-full_001.md");
    std::fs::write(
        &empty,
        "[project: Vetcoders/aicx | agent: claude | date: 2026-05-06 | frame_kind: internal_thought]\n\n",
    )
    .unwrap();
    std::fs::write(
        &full,
        "[project: Vetcoders/aicx | agent: claude | date: 2026-05-06]\n\nThis chunk carries enough real body content to avoid the empty-body threshold.",
    )
    .unwrap();

    let check = check_empty_body_chunks(&tmp);
    assert_eq!(check.severity, Severity::Critical);
    assert!(check.detail.contains("1 empty-body"));

    let script = render_prune_empty_bodies_script(&tmp).unwrap();
    assert!(script.starts_with("#!/usr/bin/env bash"));
    assert!(script.contains("mv -n --"));
    assert!(!script.contains("rm -f --"));
    assert!(script.contains("sess-empty"));
    assert!(!script.contains("sess-full"));
    assert!(empty.exists(), "script generation must not delete files");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn apply_prune_empty_bodies_moves_chunks_to_quarantine_and_rechecks() {
    let tmp = unique_test_dir("apply-empty-bodies");
    let dir = tmp
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0506")
        .join("conversations")
        .join("claude");
    std::fs::create_dir_all(&dir).unwrap();
    let empty = dir.join("2026_0506_claude_sess-empty_001.md");
    let full = dir.join("2026_0506_claude_sess-full_001.md");
    let empty_sidecar = empty.with_extension("meta.json");
    std::fs::write(
        &empty,
        "[project: Vetcoders/aicx | agent: claude | date: 2026-05-06 | frame_kind: internal_thought]\n\n",
    )
    .unwrap();
    std::fs::write(&empty_sidecar, "{}").unwrap();
    std::fs::write(
        &full,
        "[project: Vetcoders/aicx | agent: claude | date: 2026-05-06]\n\nThis chunk carries enough real body content to avoid the empty-body threshold.",
    )
    .unwrap();

    let opts = DoctorOptions {
        rebuild_steer_index: false,
        fix_buckets: false,
        dry_run: false,
        rebuild_sidecars: false,
        prune_empty_bodies: true,
        apply_prune_empty_bodies: true,
        check_dedup: false,
        verbose: false,
        smoke: false,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let report = rt.block_on(run_at(&tmp, &opts)).unwrap();

    assert_eq!(
        report.empty_body_chunks.severity,
        Severity::Green,
        "detail: {}; fixes: {:?}",
        report.empty_body_chunks.detail,
        report.fixes_applied
    );
    assert!(report.empty_body_chunks.detail.contains("0 empty-body"));
    assert!(report.prune_empty_bodies_script.is_none());
    assert!(
        report
            .fixes_applied
            .iter()
            .any(|entry| entry.contains("quarantined 1 empty-body chunk(s) and 1 sidecar(s)"))
    );
    assert!(!empty.exists());
    assert!(!empty_sidecar.exists());
    assert!(full.exists());

    let quarantine_root = std::fs::read_dir(tmp.join("quarantine"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("empty-bodies-"))
        })
        .expect("empty-body quarantine root should exist");
    // After D1 (B-P0-02 fix), quarantine layout mirrors paths relative
    // to the aicx canonical root (`base`) instead of `<base>/store/`,
    // so previously-stored chunks gain a `store/` prefix in quarantine
    // and `non-repository-contexts/` chunks survive the rename instead
    // of crashing on the prefix check.
    let moved = quarantine_root
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0506")
        .join("conversations")
        .join("claude")
        .join("2026_0506_claude_sess-empty_001.md");
    assert!(moved.exists());
    assert!(moved.with_extension("meta.json").exists());

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_doctor_sidecars_and_coverage_share_check_result() {
    let tmp = unique_test_dir("sidecars-shared-result");
    let dir = tmp
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0506")
        .join("conversations")
        .join("codex");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("2026_0506_codex_sess-sidecars_001.md"),
        "chunk without a sidecar",
    )
    .unwrap();

    let opts = DoctorOptions {
        rebuild_steer_index: false,
        fix_buckets: false,
        dry_run: false,
        rebuild_sidecars: false,
        prune_empty_bodies: false,
        apply_prune_empty_bodies: false,
        check_dedup: false,
        verbose: false,
        smoke: false,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let report = rt.block_on(run_at(&tmp, &opts)).unwrap();

    assert_eq!(report.sidecars, report.sidecar_coverage);
    assert_eq!(report.sidecars.name, "sidecars");
    assert_eq!(report.sidecars.severity, Severity::Critical);

    let _ = std::fs::remove_dir_all(&tmp);
}

/// Bug #37: freshness must inspect the actual semantic index at
/// `<base>/indexed/<bucket>/embeddings.ndjson` and recommend the
/// post-A-1 canonical `--full-rescan` flag.
#[test]
fn index_freshness_reports_missing_when_chunks_exist_but_no_indexed_dir() {
    let tmp = unique_test_dir("freshness-missing");
    // Plant one chunk in the canonical store.
    let chunk_dir = tmp
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0506")
        .join("conversations")
        .join("claude");
    std::fs::create_dir_all(&chunk_dir).unwrap();
    std::fs::write(chunk_dir.join("2026_0506_claude_sess-a_001.md"), "chunk").unwrap();
    // NO `indexed/` directory at all.

    let result = check_index_freshness(&tmp);
    assert_eq!(
        result.severity,
        Severity::Critical,
        "missing index buckets must be Critical"
    );
    assert!(
        result.detail.contains("no semantic index buckets"),
        "detail must mention missing buckets, got: {}",
        result.detail
    );
    let rec = result
        .recommendation
        .expect("recovery hint required when missing");
    assert!(
        rec.contains("aicx index"),
        "recovery must invoke `aicx index`, got: {rec}"
    );
    assert!(
        rec.contains("--full-rescan"),
        "recovery must reference canonical `--full-rescan` flag, got: {rec}"
    );
    assert!(
        !rec.contains(&format!("--{}", "fresh ")),
        "recovery must NOT use legacy fresh flag, got: {rec}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn index_freshness_reports_missing_when_bucket_dir_lacks_embeddings_file() {
    let tmp = unique_test_dir("freshness-bucket-empty");
    let chunk_dir = tmp.join("store").join("Acme").join("svc");
    std::fs::create_dir_all(&chunk_dir).unwrap();
    std::fs::write(chunk_dir.join("chunk.md"), "chunk").unwrap();
    // Bucket dir exists but no embeddings.ndjson committed — only the
    // temp checkpoint, mirroring a crashed embed loop.
    let bucket_dir = tmp.join("indexed").join("_all");
    std::fs::create_dir_all(&bucket_dir).unwrap();
    std::fs::write(bucket_dir.join("embeddings.ndjson.tmp"), "partial").unwrap();

    let result = check_index_freshness(&tmp);
    assert_eq!(result.severity, Severity::Critical);
    assert!(
        result.detail.contains("missing"),
        "detail must say missing, got: {}",
        result.detail
    );
    assert!(
        result
            .recommendation
            .as_ref()
            .is_some_and(|r| r.contains("--full-rescan"))
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn index_freshness_reports_stale_when_chunk_mtime_exceeds_index_mtime() {
    use filetime::{FileTime, set_file_mtime};
    let tmp = unique_test_dir("freshness-stale");

    let chunk_dir = tmp.join("store").join("Vetcoders").join("aicx");
    std::fs::create_dir_all(&chunk_dir).unwrap();
    let chunk_path = chunk_dir.join("chunk.md");
    std::fs::write(&chunk_path, "chunk body").unwrap();

    let bucket_dir = tmp.join("indexed").join("_all");
    std::fs::create_dir_all(&bucket_dir).unwrap();
    let index_path = bucket_dir.join("embeddings.ndjson");
    std::fs::write(&index_path, "{\"id\":\"a\"}\n").unwrap();

    // newest_mtime walks parent dirs too — their creation mtimes land
    // at ~now. Use "now" as the chunk reference frame and back-date the
    // index 2h so the lag is a clean <72h Warning regardless of
    // directory metadata.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    set_file_mtime(&chunk_path, FileTime::from_unix_time(now, 0)).unwrap();
    set_file_mtime(&index_path, FileTime::from_unix_time(now - 7_200, 0)).unwrap();

    let result = check_index_freshness(&tmp);
    assert_eq!(result.severity, Severity::Warning, "<72h lag is Warning");
    assert!(
        result.detail.contains("stale"),
        "detail must say stale, got: {}",
        result.detail
    );
    let rec = result
        .recommendation
        .expect("stale must carry recovery hint");
    assert!(rec.contains("aicx index"), "got: {rec}");
    assert!(rec.contains("--full-rescan"), "got: {rec}");
    assert!(
        !rec.contains(&format!("--{}", "fresh ")),
        "no legacy fresh-flag literal, got: {rec}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn index_freshness_reports_fresh_when_index_mtime_meets_or_exceeds_chunks() {
    use filetime::{FileTime, set_file_mtime};
    let tmp = unique_test_dir("freshness-fresh");

    let chunk_dir = tmp.join("store").join("Vetcoders").join("aicx");
    std::fs::create_dir_all(&chunk_dir).unwrap();
    let chunk_path = chunk_dir.join("chunk.md");
    std::fs::write(&chunk_path, "chunk body").unwrap();

    let bucket_dir = tmp.join("indexed").join("_all");
    std::fs::create_dir_all(&bucket_dir).unwrap();
    let index_path = bucket_dir.join("embeddings.ndjson");
    std::fs::write(&index_path, "{\"id\":\"a\"}\n").unwrap();

    // Index committed AFTER the chunk → fresh.
    let t0 = 1_700_000_000i64;
    set_file_mtime(&chunk_path, FileTime::from_unix_time(t0, 0)).unwrap();
    set_file_mtime(&index_path, FileTime::from_unix_time(t0 + 60, 0)).unwrap();
    // Also pin parent dir mtimes so `newest_mtime` (which walks dirs)
    // does not pick up a post-creation directory mtime that races
    // ahead of the index file.
    set_file_mtime(&chunk_dir, FileTime::from_unix_time(t0, 0)).unwrap();
    set_file_mtime(tmp.join("store"), FileTime::from_unix_time(t0, 0)).unwrap();
    set_file_mtime(
        tmp.join("store").join("Vetcoders"),
        FileTime::from_unix_time(t0, 0),
    )
    .unwrap();

    let result = check_index_freshness(&tmp);
    assert_eq!(
        result.severity,
        Severity::Green,
        "got detail: {}",
        result.detail
    );
    assert!(
        result.detail.contains("fresh"),
        "detail must say fresh, got: {}",
        result.detail
    );
    assert!(
        result.recommendation.is_none(),
        "fresh state needs no recovery hint"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn empty_body_frame_kind_prefers_sidecar_and_falls_back_to_header() {
    let tmp = unique_test_dir("empty-body-frame-kind-source");
    let dir = tmp
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0702")
        .join("conversations")
        .join("claude");
    std::fs::create_dir_all(&dir).unwrap();

    // Sidecar and header disagree — the sidecar must win.
    let with_sidecar = dir.join("2026_0702_claude_sess-sidecar_001.md");
    std::fs::write(
        &with_sidecar,
        "[project: Vetcoders/aicx | agent: claude | date: 2026-07-02 | frame_kind: internal_thought]\n\n",
    )
    .unwrap();
    std::fs::write(
        with_sidecar.with_extension("meta.json"),
        r#"{"id":"sess-sidecar_001","project":"Vetcoders/aicx","agent":"claude","date":"2026-07-02","session_id":"sess-sidecar","kind":"conversations","frame_kind":"system_note"}"#,
    )
    .unwrap();

    // No sidecar — the legacy bracket header fills in.
    std::fs::write(
        dir.join("2026_0702_claude_sess-header_001.md"),
        "[project: Vetcoders/aicx | agent: claude | date: 2026-07-02 | frame_kind: internal_thought]\n\n",
    )
    .unwrap();

    // No sidecar, YAML frontmatter header — the v2 form fills in the same way.
    std::fs::write(
        dir.join("2026_0702_claude_sess-front_001.md"),
        "---\nproject: Vetcoders/aicx\nagent: claude\ndate: 2026-07-02\nframe_kind: tool_call\n---\n\n",
    )
    .unwrap();

    let report = empty_body_report(&tmp);
    assert_eq!(report.empty, 3, "all three fixtures are empty-body cards");
    assert_eq!(
        report.by_frame_kind.get("system_note"),
        Some(&1),
        "sidecar frame_kind must beat the disagreeing header: {:?}",
        report.by_frame_kind
    );
    assert_eq!(report.by_frame_kind.get("internal_thought"), Some(&1));
    assert_eq!(report.by_frame_kind.get("tool_call"), Some(&1));
    assert_eq!(report.by_frame_kind.get("unknown"), None);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn empty_body_detection_is_header_agnostic_for_frontmatter_cards() {
    let tmp = unique_test_dir("empty-bodies-frontmatter");
    let dir = tmp
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0702")
        .join("conversations")
        .join("claude");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("2026_0702_claude_sess-fm-empty_001.md"),
        "---\nproject: Vetcoders/aicx\nagent: claude\ndate: 2026-07-02\n---\n\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("2026_0702_claude_sess-fm-full_001.md"),
        "---\nproject: Vetcoders/aicx\nagent: claude\ndate: 2026-07-02\n---\n\nThis chunk carries enough real body content to avoid the empty-body threshold.",
    )
    .unwrap();

    let check = check_empty_body_chunks(&tmp);
    assert!(
        check.detail.contains("1 empty-body"),
        "frontmatter header must not hide an empty body nor mask a real one: {}",
        check.detail
    );

    let script = render_prune_empty_bodies_script(&tmp).unwrap();
    assert!(script.contains("sess-fm-empty"));
    assert!(!script.contains("sess-fm-full"));

    let _ = std::fs::remove_dir_all(&tmp);
}

fn unique_test_dir(label: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "aicx-doctor-{label}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}
