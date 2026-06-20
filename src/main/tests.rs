use super::*;
use filetime::{FileTime, set_file_mtime};
use std::fs;

#[test]
fn default_intents_markdown_uses_pack_report() {
    let sections = vec![
        IntentPackSection {
            title: "Decisions",
            records: vec![intents::IntentRecord {
                kind: intents::IntentKind::Decision,
                summary: "ship public seed only after privacy scrub".to_string(),
                context: None,
                evidence: vec!["P0 privacy gate".to_string()],
                project: "Loctree/aicx".to_string(),
                agent: "codex".to_string(),
                date: "2026-06-14".to_string(),
                timestamp: None,
                session_id: "s1".to_string(),
                count: Some(2),
                first_chunk: None,
                last_chunk: None,
                source_chunk: "chunk-a.md".to_string(),
                source: None,
            }],
        },
        IntentPackSection {
            title: "Tasks",
            records: Vec::new(),
        },
        IntentPackSection {
            title: "Human Intent",
            records: Vec::new(),
        },
        IntentPackSection {
            title: "Agent Claims / Self-Reports",
            records: Vec::new(),
        },
        IntentPackSection {
            title: "Unresolved Human Intent",
            records: Vec::new(),
        },
    ];

    let markdown = format_intents_pack_markdown("Loctree/aicx", 168, Some(20), &sections);

    assert!(markdown.starts_with("# Intent Report"));
    assert!(markdown.contains("- per_section_limit: 20"));
    assert!(markdown.contains("## Decisions"));
    assert!(markdown.contains("## Tasks"));
    assert!(markdown.contains("## Human Intent"));
    assert!(markdown.contains("## Agent Claims / Self-Reports"));
    assert!(markdown.contains("## Unresolved Human Intent"));
    assert!(markdown.contains("## Mission Keyword Hits"));
    assert!(markdown.contains("`privacy`"));
    assert!(markdown.contains("`public seed`"));
}

#[test]
fn pack_record_dedupes_joined_source_chunks() {
    assert_eq!(
        unique_source_chunks("a.md, b.md, a.md,  b.md, c.md"),
        vec!["a.md", "b.md", "c.md"]
    );
}

#[test]
fn default_pack_trigger_preserves_scoped_timeline_modes() {
    let filters = RetrievalFilters {
        limit: None,
        sort: None,
        score: None,
        agent: None,
        since: None,
        until: None,
        frame_kind: None,
    };
    assert!(should_render_intents_pack(
        &filters, "markdown", None, false, false
    ));
    assert!(!should_render_intents_pack(
        &filters, "json", None, false, false
    ));
    assert!(!should_render_intents_pack(
        &filters,
        "markdown",
        Some("decision"),
        false,
        false
    ));
    assert!(!should_render_intents_pack(
        &filters, "markdown", None, true, false
    ));
    assert!(!should_render_intents_pack(
        &filters, "markdown", None, false, true
    ));

    let scoped = RetrievalFilters {
        frame_kind: Some(FrameKindArg::UserMsg),
        ..filters
    };
    assert!(!should_render_intents_pack(
        &scoped, "markdown", None, false, false
    ));
}

/// Regression: B-P1-12 — detector must fire on the canonical bad shape.
#[test]
fn detect_config_show_flag_fires_on_canonical_mistake() {
    let args = ["config", "--show"].iter().map(|s| s.to_string());
    let hit = detect_config_show_flag_mistake(args).expect("should detect mistake");
    assert_eq!(hit.kind, "flag_not_recognized");
    assert!(hit.recommendation.contains("aicx config show"));
    assert!(
        hit.fallback
            .as_ref()
            .is_some_and(|fb| fb.command == "aicx config show")
    );
}

#[test]
fn detect_config_show_flag_ignores_legitimate_subcommand() {
    let args = ["config", "show"].iter().map(|s| s.to_string());
    assert!(detect_config_show_flag_mistake(args).is_none());
}

#[test]
fn detect_config_show_flag_ignores_init_subcommand() {
    let args = ["config", "init", "--show"].iter().map(|s| s.to_string());
    assert!(
        detect_config_show_flag_mistake(args).is_none(),
        "once the user picked a subcommand, --show is the subcommand's problem"
    );
}

#[test]
fn detect_config_show_flag_ignores_unrelated_commands() {
    let args = ["claude", "--show"].iter().map(|s| s.to_string());
    assert!(detect_config_show_flag_mistake(args).is_none());
}

#[test]
fn detect_config_show_flag_accepts_global_flags_before_config() {
    let args = ["--verbose", "config", "--show"]
        .iter()
        .map(|s| s.to_string());
    assert!(detect_config_show_flag_mistake(args).is_some());
}

/// Bug #26 regression: the branches of `aicx config show`
/// must each render a distinct marker so an operator can tell at
/// a glance which file the embedder actually loaded (env override,
/// legacy embedder.toml, canonical config.toml, bootstrap config, or built-in
/// defaults). Tests the pure marker formatter; the resolver itself
/// is covered in `aicx_embeddings::config::tests`.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
#[test]
fn config_show_marker_covers_all_branches() {
    use aicx::embedder::ConfigSource;

    let env_path = PathBuf::from("/tmp/op-override.toml");
    let (path, branch, marker) =
        crate::cli_config::describe_effective_config(&Some((env_path.clone(), ConfigSource::Env)));
    assert_eq!(branch, "env");
    assert_eq!(path, env_path.display().to_string());
    assert!(
        marker.contains("$AICX_EMBEDDER_CONFIG"),
        "env marker must name the override var: {marker}"
    );
    assert!(marker.contains(&env_path.display().to_string()));

    let canonical = PathBuf::from("/tmp/.aicx/config.toml");
    let (path, branch, marker) = crate::cli_config::describe_effective_config(&Some((
        canonical.clone(),
        ConfigSource::Canonical,
    )));
    assert_eq!(branch, "canonical");
    assert_eq!(path, canonical.display().to_string());
    assert!(marker.contains("canonical"));

    let legacy = PathBuf::from("/tmp/.aicx/embedder.toml");
    let (path, branch, marker) =
        crate::cli_config::describe_effective_config(&Some((legacy.clone(), ConfigSource::Legacy)));
    assert_eq!(branch, "legacy");
    assert_eq!(path, legacy.display().to_string());
    assert!(
        marker.contains("aicx config init"),
        "legacy marker must nudge migration: {marker}"
    );

    let bootstrap = PathBuf::from("/tmp/.aicx/config.toml");
    let (path, branch, marker) = crate::cli_config::describe_effective_config(&Some((
        bootstrap.clone(),
        ConfigSource::Bootstrap,
    )));
    assert_eq!(branch, "bootstrap");
    assert_eq!(path, bootstrap.display().to_string());
    assert!(
        marker.contains("[storage].home"),
        "bootstrap marker must name storage relocation: {marker}"
    );

    let (path, branch, marker) = crate::cli_config::describe_effective_config(&None);
    assert_eq!(branch, "defaults");
    assert_eq!(path, "<built-in defaults>");
    assert!(
        marker.contains("aicx config init"),
        "defaults marker must nudge `aicx config init`: {marker}"
    );
    assert!(
        marker.contains("no config file found"),
        "defaults marker must say no file was found: {marker}"
    );
}

#[test]
fn lookback_cutoff_zero_returns_all_time() {
    let cutoff = lookback_cutoff(0);
    assert_eq!(
        cutoff,
        all_time_cutoff(),
        "hours=0 must collapse to the Unix-epoch all-time sentinel"
    );
}

#[test]
fn test_refs_cutoff_zero_returns_unix_epoch() {
    let cutoff = crate::refs_cutoff(0);
    assert_eq!(
        cutoff,
        std::time::UNIX_EPOCH,
        "hours=0 must collapse to UNIX_EPOCH"
    );
}

#[test]
fn test_extraction_source_key_is_order_insensitive() {
    let project_a = vec!["a".to_string(), "b".to_string()];
    let project_b = vec!["b".to_string(), "a".to_string()];

    assert_eq!(
        extraction_source_key(LEGACY_ALL_WATERMARK_AGENTS, &project_a),
        extraction_source_key(LEGACY_ALL_WATERMARK_AGENTS, &project_b)
    );
    assert_eq!(
        extraction_source_key_aliases(LEGACY_ALL_WATERMARK_AGENTS, &project_a),
        extraction_source_key_aliases(LEGACY_ALL_WATERMARK_AGENTS, &project_b)
    );
}

#[test]
fn test_extraction_source_key_is_case_insensitive() {
    let project_a = vec!["Foo".to_string()];
    let project_b = vec!["foo".to_string()];

    assert_eq!(
        extraction_source_key(LEGACY_ALL_WATERMARK_AGENTS, &project_a),
        extraction_source_key(LEGACY_ALL_WATERMARK_AGENTS, &project_b)
    );
    assert_eq!(
        extraction_source_key_aliases(LEGACY_ALL_WATERMARK_AGENTS, &project_a),
        extraction_source_key_aliases(LEGACY_ALL_WATERMARK_AGENTS, &project_b)
    );
}

/// Bug #36 regression: prove `aicx index status -p X` and
/// `aicx index -p X` produce the same bucket set for every canonical
/// filter shape. Both surfaces must canonicalize through
/// `aicx::store::resolve_filters_to_slugs` before computing bucket
/// paths, so any `-p X` that `index` would build IS the bucket set
/// that `index status` reports on (and vice versa).
#[test]
fn index_status_routes_through_index_canonical_resolver() {
    use std::collections::BTreeSet;

    let root = unique_test_dir("index-status-canonical");
    let _ = fs::remove_dir_all(&root);
    let canonical_root = root.join("store");

    // Canonical on-disk store: 4 buckets across 2 orgs / 3 repo names.
    // Mixed case mirrors real-world GitHub slugs (filesystem preserves it).
    let bucket_slugs = [
        "VetCoders/Loctree",
        "VetCoders/aicx",
        "Szowesgad/Loctree",
        "Szowesgad/CodeScribe",
    ];
    for slug in bucket_slugs {
        fs::create_dir_all(canonical_root.join(slug)).unwrap();
    }

    // Corresponding semantic index buckets (lowercase + `/` → `_`).
    for bucket in [
        "vetcoders_loctree",
        "vetcoders_aicx",
        "szowesgad_loctree",
        "szowesgad_codescribe",
    ] {
        let dir = root.join("indexed").join(bucket);
        fs::create_dir_all(&dir).unwrap();
        // Header + one row so semantic_index_present flips to true.
        write_file(
            &dir.join("embeddings.ndjson"),
            "{\"schema_version\":\"1.0\"}\n{\"id\":\"a\"}\n",
        );
    }

    // The 4 canonical filter shapes from the bug brief.
    let shapes: &[(&str, &[&str])] = &[
        // strict slug
        ("VetCoders/Loctree", &["vetcoders_loctree"]),
        // org wildcard
        ("VetCoders/", &["vetcoders_aicx", "vetcoders_loctree"]),
        // cross-org repo
        ("/Loctree", &["szowesgad_loctree", "vetcoders_loctree"]),
        // bare name (matches as repo name across orgs)
        ("Loctree", &["szowesgad_loctree", "vetcoders_loctree"]),
    ];

    for (filter, expected_buckets) in shapes {
        // Step 1: canonical resolver (the shared chokepoint both
        // `aicx index` and `aicx index status` route through after
        // bug #36 is fixed).
        let resolved =
            aicx::store::resolve_filters_to_slugs_at(&canonical_root, &[(*filter).to_string()])
                .unwrap_or_else(|e| panic!("resolver failed for {filter:?}: {e}"));

        assert!(
            !resolved.is_empty(),
            "filter {filter:?} must resolve to at least one slug"
        );

        // Step 2: for each canonical slug, ask the public status API
        // (exactly what `run_index_status` calls). The bucket it
        // reports IS the bucket `run_index` would have built.
        let actual_buckets: BTreeSet<String> = resolved
            .iter()
            .map(|slug| {
                aicx::api::index_status_at(&root, Some(slug.as_str()))
                    .unwrap_or_else(|e| panic!("index_status_at failed for slug {slug:?}: {e}"))
                    .project_bucket
            })
            .collect();

        let expected: BTreeSet<String> =
            expected_buckets.iter().map(|s| (*s).to_string()).collect();

        assert_eq!(
            actual_buckets, expected,
            "filter {filter:?}: `aicx index status` bucket set must equal `aicx index` bucket set"
        );

        // And every reported bucket must actually be Ready on disk
        // — proves the canonical slug round-trips to an existing
        // index file, not a phantom like `_codescribe`.
        for bucket in &actual_buckets {
            let path = root.join("indexed").join(bucket).join("embeddings.ndjson");
            assert!(
                path.exists(),
                "filter {filter:?} resolved to bucket {bucket:?} but no index file exists at {}",
                path.display()
            );
        }
    }

    let _ = fs::remove_dir_all(&root);
}

fn dummy_index_status(bucket: &str) -> aicx::IndexStatus {
    aicx::IndexStatus {
        canonical_chunks: 3,
        semantic_index_present: true,
        semantic_index_path: Some(format!("/tmp/{bucket}/embeddings.ndjson")),
        semantic_index_rows: 3,
        newest_chunk_mtime: Some("2026-05-24T00:00:00Z".to_string()),
        source_sessions: 0,
        newest_session_updated_at: None,
        sessions_newer_than_chunks: 0,
        sessions_without_timestamps: 0,
        chunking_lag_secs: None,
        semantic_index_mtime: Some("2026-05-24T00:01:00Z".to_string()),
        semantic_lag_secs: Some(60),
        pending_chunks: 0,
        temp_index_present: false,
        temp_index_path: None,
        temp_index_rows: 0,
        temp_index_mtime: None,
        temp_index_bytes: None,
        readiness: aicx::IndexReadiness::Ready,
        backend: "ndjson".to_string(),
        project_bucket: bucket.to_string(),
        committed_at: Some("2026-05-24T00:01:00Z".to_string()),
    }
}

#[test]
fn index_catch_up_plan_uses_oldest_lagging_chunk_timestamp() {
    let mut current = dummy_index_status("current");
    current.sessions_newer_than_chunks = 0;
    current.newest_chunk_mtime = Some("2026-06-12T10:00:00Z".to_string());

    let mut lagging_newer = dummy_index_status("lagging-newer");
    lagging_newer.sessions_newer_than_chunks = 3;
    lagging_newer.newest_chunk_mtime = Some("2026-06-12T09:00:00Z".to_string());

    let mut lagging_older = dummy_index_status("lagging-older");
    lagging_older.sessions_newer_than_chunks = 1;
    lagging_older.newest_chunk_mtime = Some("2026-06-11T12:33:26Z".to_string());

    let plan = index_catch_up_plan_from_statuses(&[current, lagging_newer, lagging_older]).unwrap();

    assert!(plan.needed);
    assert_eq!(
        plan.cutoff.unwrap().to_rfc3339(),
        "2026-06-11T12:33:26+00:00"
    );
}

#[test]
fn index_catch_up_plan_skips_store_when_no_chunking_lag_exists() {
    let status = dummy_index_status("ready");

    let plan = index_catch_up_plan_from_statuses(&[status]).unwrap();

    assert!(!plan.needed);
    assert!(plan.cutoff.is_none());
}

#[test]
fn index_catch_up_plan_uses_all_time_when_lagging_status_has_no_chunks() {
    let mut missing_chunks = dummy_index_status("missing-chunks");
    missing_chunks.sessions_newer_than_chunks = 5;
    missing_chunks.newest_chunk_mtime = None;

    let plan = index_catch_up_plan_from_statuses(&[missing_chunks]).unwrap();

    assert!(plan.needed);
    assert!(plan.cutoff.is_none());
}

#[test]
fn index_status_json_payload_is_always_array_for_single_scope() {
    let reports = vec![(None, dummy_index_status("_all"))];
    let payload = index_status_json_payload(&reports);
    let items = payload
        .as_array()
        .expect("index status JSON must be a stable array envelope");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["project"], "_all");
    assert_eq!(items[0]["status"]["project_bucket"], "_all");
}

#[test]
fn index_status_json_payload_is_array_for_multiple_scopes() {
    let reports = vec![
        (
            Some("VetCoders/aicx".to_string()),
            dummy_index_status("vetcoders_aicx"),
        ),
        (
            Some("Loctree/loctree-suite".to_string()),
            dummy_index_status("loctree_loctree-suite"),
        ),
    ];
    let payload = index_status_json_payload(&reports);
    let items = payload
        .as_array()
        .expect("multi-scope index status JSON must use the same envelope");

    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["project"], "VetCoders/aicx");
    assert_eq!(items[1]["project"], "Loctree/loctree-suite");
    assert_eq!(items[0]["status"]["project_bucket"], "vetcoders_aicx");
    assert_eq!(
        items[1]["status"]["project_bucket"],
        "loctree_loctree-suite"
    );
}

#[test]
fn run_search_rejects_limit_over_hard_cap_before_store_access() {
    let filters = RetrievalFilters {
        limit: Some(MAX_CLI_SEARCH_LIMIT + 1),
        sort: None,
        score: None,
        agent: None,
        since: None,
        until: None,
        frame_kind: None,
    };

    let err = run_search(SearchRunArgs {
        query: "dashboard",
        projects: &[],
        hours: 0,
        date: None,
        json: false,
        filters,
        kind: None,
        no_semantic: true,
    })
    .expect_err("oversized search limit must fail before reading the store");

    let rendered = err.to_string();
    assert!(rendered.contains("search --limit"));
    assert!(rendered.contains(&MAX_CLI_SEARCH_LIMIT.to_string()));
}

#[test]
fn fuzzy_fetch_limit_uses_semantic_filter_cap_constants() {
    assert_eq!(
        search_examined_fetch_limit(1, true),
        aicx::search_engine::FILTER_EXAMINED_CAP_MIN
    );
    assert_eq!(
        search_examined_fetch_limit(10, true),
        10 * aicx::search_engine::FILTER_EXAMINED_CAP_RATIO
    );
    assert_eq!(search_examined_fetch_limit(1, false), 1);
}

#[test]
fn session_id_table_value_preserves_full_id() {
    assert_eq!(
        session_id_table_value("zażółć-gęśla-jaźń"),
        "zażółć-gęśla-jaźń"
    );
    assert_eq!(session_id_table_value("séance"), "séance");
    assert_eq!(session_id_table_value(""), "");
    assert_eq!(session_id_table_value("0eb1a73c-1234"), "0eb1a73c-1234");
}

#[test]
fn sessions_table_project_uses_canonical_repo_identity() {
    let info = session_info("aicx", "/Users/me/hosted/Loctree/aicx");

    assert_eq!(session_project_label(&info), "Loctree/aicx");
}

#[test]
fn sessions_table_compacts_home_repo_path_for_scanning() {
    let Some(home) = dirs::home_dir().and_then(|path| path.into_os_string().into_string().ok())
    else {
        return;
    };

    let path = format!("{home}/Loctree/vetcoders/aicx");
    assert_eq!(compact_repo_path(&path), "~/L/v/aicx");
}

#[test]
fn sessions_table_user_comes_from_source_users_path() {
    let info = session_info("aicx", "/Users/dragon/Loctree/aicx");

    assert_eq!(session_user_label(&info), "dragon");
}

#[test]
fn sessions_table_time_is_minute_precision_with_explicit_utc_offset() {
    let time = Utc.with_ymd_and_hms(2026, 6, 14, 4, 49, 22).unwrap();

    assert_eq!(format_session_table_time(time), "2026-06-14T04:49(+0)");
}

#[test]
fn current_session_prefers_codex_thread_id_env() {
    let current = current_session_from_env_lookup(|key| match key {
        "CODEX_THREAD_ID" => Some("019eba52-81db-7d31-bb28-6343f05c4b79".to_string()),
        _ => None,
    })
    .expect("current session from env");

    assert_eq!(current.session_id, "019eba52-81db-7d31-bb28-6343f05c4b79");
    assert_eq!(current.source, "env:CODEX_THREAD_ID");
    assert_eq!(current.agent.as_deref(), Some("codex"));
}

#[test]
fn sessions_current_command_parses_json_flag() {
    let cli = Cli::try_parse_from(["aicx", "sessions", "current", "--json"])
        .expect("sessions current should parse");

    match cli.command {
        Some(Commands::Sessions {
            command: SessionsCommand::Current { json },
        }) => assert!(json),
        _ => panic!("expected sessions current command"),
    }
}

#[test]
fn retrieval_limit_is_a_true_option_so_explicit_ten_is_honored() {
    // P2-11: `--limit 10` used to collide with the default sentinel and
    // silently meant "no limit" for intents. Now omission is None and an
    // explicit 10 is Some(10).
    let cli = Cli::try_parse_from(["aicx", "intents", "--limit", "10"])
        .expect("intents accepts --limit 10");
    match cli.command {
        Some(Commands::Intents { filters, .. }) => assert_eq!(filters.limit, Some(10)),
        other => panic!("expected intents, got {other:?}"),
    }

    let cli = Cli::try_parse_from(["aicx", "intents"]).expect("intents parses without --limit");
    match cli.command {
        Some(Commands::Intents { filters, .. }) => assert_eq!(filters.limit, None),
        other => panic!("expected intents, got {other:?}"),
    }
}

#[test]
fn sessions_list_agent_filter_rejects_typos_at_parse_time() {
    // P2-07: a typo'd --agent must be a clap error, not a silent empty list.
    let err = Cli::try_parse_from(["aicx", "sessions", "list", "--agent", "claud"])
        .expect_err("unknown agent must fail parsing");
    assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);

    for agent in ["claude", "codex", "gemini", "junie"] {
        Cli::try_parse_from(["aicx", "sessions", "list", "--agent", agent])
            .unwrap_or_else(|e| panic!("agent '{agent}' must parse: {e}"));
    }
}

#[test]
fn clarify_max_enforces_documented_one_to_five_range() {
    // P3-11: the doc promises <1-5>; out-of-range values are clap errors.
    for bad in ["0", "6"] {
        let err = Cli::try_parse_from(["aicx", "clarify", "--session", "s", "--max", bad])
            .expect_err("out-of-range --max must fail parsing");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }
    let cli = Cli::try_parse_from(["aicx", "clarify", "--session", "s", "--max", "3"])
        .expect("--max 3 parses");
    match cli.command {
        Some(Commands::Clarify { max, .. }) => assert_eq!(max, 3),
        other => panic!("expected clarify, got {other:?}"),
    }
}

#[test]
fn sessions_report_max_enforces_documented_one_to_five_range() {
    for bad in ["0", "6"] {
        let err = Cli::try_parse_from(["aicx", "sessions", "report", "s", "--max", bad])
            .expect_err("out-of-range sessions report --max must fail parsing");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }
    let cli = Cli::try_parse_from(["aicx", "sessions", "report", "s", "--max", "3"])
        .expect("--max 3 parses");
    match cli.command {
        Some(Commands::Sessions {
            command: SessionsCommand::Report { max, .. },
        }) => assert_eq!(max, 3),
        other => panic!("expected sessions report, got {other:?}"),
    }
}

#[test]
fn lookback_cutoff_handles_normal_range() {
    let before = Utc::now();
    let cutoff = lookback_cutoff(8);
    let after = Utc::now();
    let lower = before - chrono::Duration::hours(8) - chrono::Duration::seconds(5);
    let upper = after - chrono::Duration::hours(8) + chrono::Duration::seconds(5);
    assert!(
        cutoff >= lower && cutoff <= upper,
        "cutoff out of range: {cutoff}"
    );
}

#[test]
fn lookback_cutoff_avoids_u64_to_i64_overflow() {
    // Without the `i32::MAX` clamp, casting `u64::MAX as i64` wraps to -1 and
    // places the cutoff one hour in the future. Verify the clamp keeps it
    // strictly in the past for the entire `u64` domain.
    let now = Utc::now();
    let cutoff = lookback_cutoff(u64::MAX);
    assert!(
        cutoff < now,
        "cutoff must not be in the future: {cutoff} vs now {now}"
    );
}

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-main-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn set_mtime(path: &Path, unix_seconds: i64) {
    set_file_mtime(path, FileTime::from_unix_time(unix_seconds, 0)).unwrap();
}

fn write_store_chunk(root: &Path, slug: &str, date: &str, session: &str) -> PathBuf {
    let path = root
        .join("store")
        .join(slug)
        .join(date)
        .join("conversations")
        .join("claude")
        .join(format!("{date}_claude_{session}_001.md"));
    write_file(&path, "[signals]\n- intent: test\n");
    path
}

fn encode_claude_project_dir(path: &Path) -> String {
    // Claude encodes a cwd into a single project-dir component by replacing the
    // path separators. On Windows the path is `\`-separated and drive-prefixed
    // (`C:\Users\x\Compass`), so a `/`-only replace leaves `:` and `\` in the
    // name — an invalid component, and `join`ing a drive-absolute string escapes
    // the projects root entirely. Replace both separators and the drive colon so
    // the encoded dir is valid and discoverable on every platform.
    path.display().to_string().replace(['/', '\\', ':'], "-")
}

fn session_info(project: &str, repo_path: &str) -> sessions::SessionInfo {
    sessions::SessionInfo {
        session_id: "session-1".to_string(),
        agent: "claude".to_string(),
        project: Some(project.to_string()),
        repo_path: Some(repo_path.to_string()),
        started_at: None,
        updated_at: None,
        message_count: 1,
        user_message_count: 1,
        agent_message_count: 0,
        title: None,
        source_path: PathBuf::from("/tmp/session.jsonl"),
        association: sessions::Association::Exact,
        temporal_confidence: sessions::TemporalConfidence::None,
    }
}

#[test]
fn intents_project_resolver_discovers_session_display_bridge_in_production_path() {
    let root = unique_test_dir("intents-project-discovered-display-store");
    let home = unique_test_dir("intents-project-discovered-display-home");
    let repo_parent = unique_test_dir("intents-project-discovered-display-repo");
    let repo = repo_parent.join("Compass");
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&repo_parent);

    write_store_chunk(&root, "vetcoders/field_ops", "2026_0612", "canonical");
    fs::create_dir_all(&repo).unwrap();
    let git_init = std::process::Command::new("git")
        .arg("init")
        .arg(&repo)
        .output()
        .expect("git init should run");
    assert!(git_init.status.success());
    let git_remote = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args([
            "remote",
            "add",
            "origin",
            "git@github.com:vetcoders/field_ops.git",
        ])
        .output()
        .expect("git remote add should run");
    assert!(git_remote.status.success());
    let encoded = encode_claude_project_dir(&repo);
    let session_path = home
        .join(".claude")
        .join("projects")
        .join(encoded)
        .join("session-1.jsonl");
    write_file(
        &session_path,
        &format!(
            "{{\"type\":\"user\",\"timestamp\":\"2026-06-14T00:00:00Z\",\"cwd\":{:?},\"message\":{{\"role\":\"user\",\"content\":\"remember this\"}}}}\n",
            repo.display().to_string()
        ),
    );

    let got = resolve_intents_project_filters_with_session_home_at(
        &["Compass".to_string()],
        &root,
        Some(&home),
        None,
    )
    .unwrap();

    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&home);
    let _ = fs::remove_dir_all(&repo_parent);

    assert_eq!(got.projects, vec!["vetcoders/field_ops"]);
    assert!(got.unresolved_filters.is_empty());
}

#[test]
fn intents_project_resolver_prefers_session_display_before_alias() {
    let root = unique_test_dir("intents-project-display");
    let _ = fs::remove_dir_all(&root);
    write_store_chunk(&root, "legacy/ScreenScribe", "2026_0612", "legacy");
    write_store_chunk(
        &root,
        "vetcoders/screen_scribe_depr",
        "2026_0612",
        "canonical",
    );
    let sessions = vec![session_info(
        "ScreenScribe",
        "git@github.com:vetcoders/screen_scribe_depr.git",
    )];

    let got =
        resolve_intents_project_filters_at(&["ScreenScribe".to_string()], &root, &sessions, None)
            .unwrap();
    let _ = fs::remove_dir_all(&root);

    assert_eq!(got.projects, vec!["vetcoders/screen_scribe_depr"]);
    assert!(got.unresolved_filters.is_empty());
}

#[test]
fn intents_project_resolver_errors_on_ambiguous_alias() {
    let root = unique_test_dir("intents-project-ambiguous");
    let _ = fs::remove_dir_all(&root);
    write_store_chunk(&root, "one/screen_scribe_depr", "2026_0612", "one");
    write_store_chunk(&root, "two/ScreenScribe", "2026_0612", "two");

    let err = resolve_intents_project_filters_at(&["ScreenScribe".to_string()], &root, &[], None)
        .expect_err("alias collision should force explicit bucket");
    let msg = err.to_string();
    let _ = fs::remove_dir_all(&root);

    assert!(msg.contains("ambiguous"));
    assert!(msg.contains("one/screen_scribe_depr"));
    assert!(msg.contains("two/ScreenScribe"));
}

#[test]
fn intents_project_resolver_does_not_resolve_bare_unknown_to_current_repo() {
    let root = unique_test_dir("intents-project-bare-unknown");
    let _ = fs::remove_dir_all(&root);
    write_store_chunk(&root, "Loctree/aicx", "2026_0612", "aicx");

    let got =
        resolve_intents_project_filters_at(&["ScreenScrib".to_string()], &root, &[], None).unwrap();
    let _ = fs::remove_dir_all(&root);

    assert!(got.projects.is_empty());
    assert_eq!(got.unresolved_filters, vec!["ScreenScrib"]);
}

#[test]
fn intents_empty_result_hint_ranks_nearby_recent_buckets() {
    let mut counts = BTreeMap::new();
    counts.insert("vetcoders/screen_scribe_depr".to_string(), 12);
    counts.insert("vetcoders/other".to_string(), 99);
    counts.insert("local/ScreenScribe".to_string(), 3);

    let hints = nearby_bucket_hints(&["ScreenScribe".to_string()], &counts);

    assert_eq!(
        hints,
        vec![
            BucketHint {
                slug: "vetcoders/screen_scribe_depr".to_string(),
                chunks: 12,
            },
            BucketHint {
                slug: "local/ScreenScribe".to_string(),
                chunks: 3,
            },
        ]
    );
}

#[test]
fn uuid_suffix_from_stem_extracts_rollout_uuid() {
    assert_eq!(
        uuid_suffix_from_stem("rollout-2026-05-14T00-47-35-019e2574-8a7f-7d33-a318-b365aa0ab970"),
        Some("019e2574-8a7f-7d33-a318-b365aa0ab970")
    );
    assert_eq!(uuid_suffix_from_stem("rollout-2026-05-14"), None);
}

#[test]
fn session_reference_resolver_accepts_unique_prefix() {
    let session_ids = BTreeSet::from([
        "019e2574-8a7f-7d33-a318-b365aa0ab970".to_string(),
        "119e2574-8a7f-7d33-a318-b365aa0ab970".to_string(),
    ]);

    let resolved = resolve_session_reference_from_candidates(
        "019e2574",
        &session_ids,
        BTreeSet::new(),
        "codex",
    )
    .unwrap();

    assert_eq!(
        resolved.canonical_id,
        "019e2574-8a7f-7d33-a318-b365aa0ab970"
    );
    assert!(resolved.note.is_some());
}

#[test]
fn session_reference_resolver_rejects_ambiguous_prefix() {
    let session_ids = BTreeSet::from([
        "019e2574-8a7f-7d33-a318-b365aa0ab970".to_string(),
        "019e2574-9999-7d33-a318-b365aa0ab970".to_string(),
    ]);

    let err = resolve_session_reference_from_candidates(
        "019e2574",
        &session_ids,
        BTreeSet::new(),
        "codex",
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("Ambiguous session reference"));
}

#[test]
fn session_reference_resolver_accepts_unique_suffix() {
    let session_ids = BTreeSet::from([
        "019e2574-8a7f-7d33-a318-b365aa0ab970".to_string(),
        "119e2574-8a7f-7d33-a318-000000000000".to_string(),
    ]);

    let resolved = resolve_session_reference_from_candidates(
        "b365aa0ab970",
        &session_ids,
        BTreeSet::new(),
        "codex",
    )
    .unwrap();

    assert_eq!(
        resolved.canonical_id,
        "019e2574-8a7f-7d33-a318-b365aa0ab970"
    );
}

#[test]
fn session_reference_resolver_accepts_codex_alias_match() {
    let session_ids = BTreeSet::from(["019e2574-8a7f-7d33-a318-b365aa0ab970".to_string()]);
    let aliases = BTreeSet::from(["019e2574-8a7f-7d33-a318-b365aa0ab970".to_string()]);

    let resolved = resolve_session_reference_from_candidates(
        "rollout-2026-05-14T00-47-35-019e2574-8a7f-7d33-a318-b365aa0ab970",
        &session_ids,
        aliases,
        "codex",
    )
    .unwrap();

    assert_eq!(
        resolved.canonical_id,
        "019e2574-8a7f-7d33-a318-b365aa0ab970"
    );
}

#[test]
fn read_codex_session_meta_id_skips_malformed_lines() {
    use std::io::Write;
    let tmp_dir = unique_test_dir("read-meta-malformed");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let path = tmp_dir.join("partial.jsonl");
    let mut file = std::fs::File::create(&path).unwrap();
    // First candidate line contains the `"session_meta"` substring
    // but is truncated mid-record (typical of a partially-flushed
    // rollout). Before the fix this caused `read_codex_session_meta_id`
    // to bail out and miss the valid record on the next line.
    writeln!(
        file,
        r#"{{"timestamp":"2026-05-15T00:00:00Z","type":"session_meta","payload":{{"id":"truncated"#
    )
    .unwrap();
    writeln!(
            file,
            r#"{{"timestamp":"2026-05-15T00:00:01Z","type":"session_meta","payload":{{"id":"019e0000-0000-0000-0000-000000000000","cwd":"/tmp"}}}}"#
        )
        .unwrap();
    drop(file);

    let id = read_codex_session_meta_id(&path);
    assert_eq!(
        id.as_deref(),
        Some("019e0000-0000-0000-0000-000000000000"),
        "malformed first line must not stop the scan"
    );

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[test]
fn session_reference_resolver_ignores_out_of_window_alias() {
    // Only one id is in the current `--hours`/`--project` window.
    let session_ids = BTreeSet::from(["019e27c0-e492-7790-9c33-52b3dddd1067".to_string()]);
    // The full sessions/ tree walk surfaced two aliases sharing the
    // `019e2` prefix: one in-window, one historical/out-of-window.
    let aliases = BTreeSet::from([
        "019e27c0-e492-7790-9c33-52b3dddd1067".to_string(),
        "019e2574-8a7f-7d33-a318-b365aa0ab970".to_string(),
    ]);

    let resolved =
        resolve_session_reference_from_candidates("019e2", &session_ids, aliases, "codex").unwrap();

    // Without the in-window filter the resolver would see two
    // candidates and bail "ambiguous". After the fix it resolves
    // uniquely to the in-window id.
    assert_eq!(
        resolved.canonical_id,
        "019e27c0-e492-7790-9c33-52b3dddd1067"
    );
}

fn default_session_extract_file_name(session_id: &str) -> String {
    default_session_extract_path_for("claude", session_id, false, false)
        .expect("default session extract path should resolve")
        .file_name()
        .expect("default session extract path should include a file name")
        .to_string_lossy()
        .into_owned()
}

fn default_session_conversation_extract_file_name(session_id: &str) -> String {
    default_session_extract_path_for("claude", session_id, true, false)
        .expect("default conversation extract path should resolve")
        .file_name()
        .expect("default conversation extract path should include a file name")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn default_session_extract_path_empty_session_uses_safe_fallback() {
    assert_eq!(default_session_extract_file_name(""), "session.md");
}

#[test]
fn default_session_conversation_extract_path_uses_distinct_suffix() {
    assert_eq!(
        default_session_conversation_extract_file_name("abc-123"),
        "abc-123_conversation.md"
    );
    assert_ne!(
        default_session_extract_file_name("abc-123"),
        default_session_conversation_extract_file_name("abc-123")
    );
}

#[test]
fn default_session_extract_paths_never_collide_across_mode_axes() {
    // The four (conversation, user_only) modes must each resolve to a distinct
    // file so a user-only extract never overwrites the both-roles one.
    let full = default_session_extract_path_for("claude", "abc-123", false, false)
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let full_user = default_session_extract_path_for("claude", "abc-123", false, true)
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let conv = default_session_extract_path_for("claude", "abc-123", true, false)
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let conv_user = default_session_extract_path_for("claude", "abc-123", true, true)
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    assert_eq!(full, "abc-123.md");
    assert_eq!(full_user, "abc-123_user.md");
    assert_eq!(conv, "abc-123_conversation.md");
    assert_eq!(conv_user, "abc-123_conversation_user.md");

    let all = [&full, &full_user, &conv, &conv_user];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b, "extract mode paths must be unique");
        }
    }
}

#[test]
fn default_session_extract_path_dot_session_gets_hashed_safe_name() {
    let file_name = default_session_extract_file_name(".");

    assert_ne!(file_name, "..md");
    assert!(
        file_name.starts_with("session-"),
        "expected session-prefixed fallback, got {file_name}"
    );
    assert!(file_name.ends_with(".md"));
}

#[test]
fn default_session_extract_path_dotdot_session_gets_hashed_safe_name() {
    let file_name = default_session_extract_file_name("..");

    assert_ne!(file_name, "...md");
    assert!(
        file_name.starts_with("session-"),
        "expected session-prefixed fallback, got {file_name}"
    );
    assert!(file_name.ends_with(".md"));
}

#[test]
fn default_session_extract_path_oversized_session_is_length_capped() {
    let file_name = default_session_extract_file_name(&"a".repeat(500));
    let stem = file_name
        .strip_suffix(".md")
        .expect("default extract filename should use markdown extension");
    let (_, suffix) = stem
        .rsplit_once('-')
        .expect("oversized session id should carry hash suffix");

    assert!(stem.len() <= DEFAULT_SESSION_EXTRACT_FILENAME_STEM_MAX_BYTES);
    assert_eq!(suffix.len(), 16);
    assert!(suffix.chars().all(|ch| ch.is_ascii_hexdigit()));
}

#[test]
fn default_session_extract_path_whitespace_only_uses_safe_fallback() {
    // Whitespace-only ids collapse to nothing safe → "session" stem + hash,
    // never a filename made of spaces. Pass-6 (AUD-5) coverage gap.
    let file_name = default_session_extract_file_name("   ");
    assert!(
        file_name.starts_with("session-"),
        "whitespace-only id must fall back to session-<hash>, got {file_name}"
    );
    assert!(file_name.ends_with(".md"));
    assert!(
        !file_name.chars().any(char::is_whitespace),
        "filename must contain no whitespace, got {file_name}"
    );
}

#[test]
fn default_session_extract_path_unicode_control_chars_are_stripped() {
    // RTL override (U+202E), zero-width space (U+200B) and a combining
    // acute (U+0301) must never survive into the on-disk filename.
    let file_name = default_session_extract_file_name("a\u{202E}b\u{200B}c\u{0301}");
    assert!(file_name.ends_with(".md"));
    assert!(
        file_name.is_ascii(),
        "filename must be pure ASCII, got {file_name:?}"
    );
    let stem = file_name.strip_suffix(".md").expect("md extension");
    assert!(
        stem.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "filename stem must only use safe chars, got {stem:?}"
    );
}

#[test]
fn default_session_extract_path_backslash_is_not_a_path_separator() {
    // A backslash in the session id must sanitize to a safe char, never
    // leak as a Windows path separator that could split the component.
    let path =
        default_session_extract_path_for("claude", "a\\b", false, false).expect("path resolves");
    let file_name = path
        .file_name()
        .expect("file name")
        .to_string_lossy()
        .into_owned();
    assert!(
        !file_name.contains('\\'),
        "no backslash in filename: {file_name}"
    );
    assert!(
        !file_name.contains('/'),
        "no slash in filename: {file_name}"
    );
    // The extract dir is .../extracts/<agent>/<file>; the backslash must not
    // have introduced an extra directory level.
    assert!(
        path.ends_with(
            std::path::PathBuf::from("extracts")
                .join("claude")
                .join(&file_name)
        ),
        "session id must stay a single path component, got {}",
        path.display()
    );
}

#[test]
fn default_session_extract_path_oversized_with_extension_like_suffix_is_capped() {
    // Distinct from the pure-'a' oversized case: a surviving '.' (an
    // extension-like suffix in the input) must not break the byte-slice cap.
    let file_name = default_session_extract_file_name(&format!("{}.txt", "a".repeat(300)));
    let stem = file_name
        .strip_suffix(".md")
        .expect("default extract filename should use markdown extension");
    let (_, suffix) = stem
        .rsplit_once('-')
        .expect("oversized session id should carry hash suffix");
    assert!(stem.len() <= DEFAULT_SESSION_EXTRACT_FILENAME_STEM_MAX_BYTES);
    assert_eq!(suffix.len(), 16);
    assert!(suffix.chars().all(|ch| ch.is_ascii_hexdigit()));
}

#[test]
fn default_session_extract_path_normal_input_passthrough() {
    // Closes Klaudiusz audit gap I-3 (P3): a UUID-like session id that
    // uses only the safe charset (`[a-zA-Z0-9-_.]`) and fits under
    // DEFAULT_SESSION_EXTRACT_FILENAME_STEM_MAX_BYTES must round-trip
    // verbatim into the filename — no hash suffix, no sanitization
    // collapse. Regression guard for the `is_already_safe` fast path.
    let session_id = "019e27c0-e492-7790-9c33-52b3dddd1067";
    let file_name = default_session_extract_file_name(session_id);
    assert_eq!(
        file_name,
        format!("{session_id}.md"),
        "normal UUID-like session id must pass through unchanged"
    );
}

#[test]
fn conversation_batch_safe_session_filename_passes_through_safe_ids() {
    let id = "019e27c0-e492-7790-9c33-52b3dddd1067";
    assert_eq!(conversation_batch_safe_session_filename(id), id);
}

#[test]
fn conversation_batch_safe_session_filename_preserves_safe_underscore_runs() {
    // Both ids only use safe characters. Earlier behavior collapsed
    // `__` to `_` for every input, so "a__b" and "a_b" mapped to the
    // same filename without a hash suffix and silently overwrote each
    // other. Safe inputs must round-trip verbatim.
    assert_eq!(conversation_batch_safe_session_filename("a__b"), "a__b");
    assert_eq!(conversation_batch_safe_session_filename("a_b"), "a_b");
}

#[test]
fn conversation_batch_safe_session_filename_disambiguates_collisions() {
    // Two distinct ids that collapse to the same sanitized base must
    // produce different filenames so one export cannot overwrite the
    // other.
    let a = conversation_batch_safe_session_filename("a/b");
    let b = conversation_batch_safe_session_filename("a:b");
    assert_ne!(a, b, "distinct ids must not collide after sanitization");
    assert!(
        a.starts_with("a_b-"),
        "expected sanitized base prefix, got {a}"
    );
    assert!(
        b.starts_with("a_b-"),
        "expected sanitized base prefix, got {b}"
    );
}

#[test]
fn conversation_batch_safe_session_filename_falls_back_to_session() {
    // All chars sanitized away — base becomes "session" plus a hash
    // (still unique because the sanitization touched the id).
    let safe = conversation_batch_safe_session_filename("///");
    assert!(
        safe.starts_with("session-"),
        "expected session-prefixed name, got {safe}"
    );
}

#[test]
fn claude_defaults_to_silent_stdout() {
    let cli = Cli::try_parse_from(["aicx", "claude"]).expect("claude command should parse");

    match cli.command {
        Some(Commands::Claude { emit, .. }) => {
            assert!(matches!(emit, StdoutEmit::None));
        }
        _ => panic!("expected claude command"),
    }
}

#[test]
fn codex_defaults_to_silent_stdout() {
    let cli = Cli::try_parse_from(["aicx", "codex"]).expect("codex command should parse");

    match cli.command {
        Some(Commands::Codex { emit, .. }) => {
            assert!(matches!(emit, StdoutEmit::None));
        }
        _ => panic!("expected codex command"),
    }
}

#[test]
fn all_defaults_to_silent_stdout() {
    let cli = Cli::try_parse_from(["aicx", "all"]).expect("all command should parse");

    match cli.command {
        Some(Commands::All { emit, .. }) => {
            assert!(matches!(emit, StdoutEmit::None));
        }
        _ => panic!("expected all command"),
    }
}

#[test]
fn store_defaults_to_silent_stdout() {
    let cli = Cli::try_parse_from(["aicx", "store"]).expect("store command should parse");

    match cli.command {
        Some(Commands::Store { emit, .. }) => {
            assert!(matches!(emit, StdoutEmit::None));
        }
        other => panic!("expected store command, got {:?}", other.map(|_| "other")),
    }
}

#[test]
fn store_accepts_explicit_paths_emit() {
    let cli = Cli::try_parse_from(["aicx", "store", "--emit", "paths"])
        .expect("store command with explicit emit should parse");

    match cli.command {
        Some(Commands::Store { emit, .. }) => {
            assert!(matches!(emit, StdoutEmit::Paths));
        }
        other => panic!("expected store command, got {:?}", other.map(|_| "other")),
    }
}

#[test]
fn ingest_accepts_operator_markdown_source_and_since() {
    let cli = Cli::try_parse_from([
        "aicx",
        "ingest",
        "--source",
        "operator-md",
        "--since",
        "2026-05-01",
        "--emit",
        "json",
    ])
    .expect("operator markdown ingest command should parse");

    match cli.command {
        Some(Commands::Ingest {
            source,
            since,
            emit,
            ..
        }) => {
            assert!(matches!(source, IngestSource::OperatorMd));
            assert_eq!(since.as_deref(), Some("2026-05-01"));
            assert!(matches!(emit, StdoutEmit::Json));
        }
        other => panic!("expected ingest command, got {:?}", other.map(|_| "other")),
    }
}

#[test]
fn refs_default_to_summary_stdout() {
    let cli = Cli::try_parse_from(["aicx", "refs"]).expect("refs command should parse");

    match cli.command {
        Some(Commands::Refs { emit, .. }) => {
            assert!(matches!(emit, RefsEmit::Summary));
        }
        _ => panic!("expected refs command"),
    }
}

#[test]
fn refs_accept_explicit_paths_emit() {
    let cli = Cli::try_parse_from(["aicx", "refs", "--emit", "paths"])
        .expect("refs command with explicit emit should parse");

    match cli.command {
        Some(Commands::Refs { emit, .. }) => {
            assert!(matches!(emit, RefsEmit::Paths));
        }
        _ => panic!("expected refs command"),
    }
}

#[test]
fn search_accepts_score_and_json_flags() {
    let cli = Cli::try_parse_from(["aicx", "search", "dashboard", "--score", "60", "--json"])
        .expect("search command with score/json should parse");

    match cli.command {
        Some(Commands::Search {
            filters,
            json,
            project,
            ..
        }) => {
            assert_eq!(filters.score, Some(60));
            assert!(json);
            assert!(project.is_empty());
        }
        _ => panic!("expected search command"),
    }
}

#[test]
fn search_accepts_no_semantic_escape_hatch() {
    let cli = Cli::try_parse_from(["aicx", "search", "dashboard", "--no-semantic"])
        .expect("search command with --no-semantic should parse");

    match cli.command {
        Some(Commands::Search { no_semantic, .. }) => {
            assert!(no_semantic);
        }
        _ => panic!("expected search command"),
    }
}

#[test]
fn search_accepts_frame_kind_filter() {
    let cli = Cli::try_parse_from([
        "aicx",
        "search",
        "dashboard",
        "--frame-kind",
        "internal_thought",
    ])
    .expect("search command with frame-kind should parse");

    match cli.command {
        Some(Commands::Search { filters, .. }) => {
            assert_eq!(filters.frame_kind, Some(FrameKindArg::InternalThought));
        }
        _ => panic!("expected search command"),
    }
}

#[test]
fn search_accepts_corpus_kind_filter() {
    let cli = Cli::try_parse_from(["aicx", "search", "dashboard", "--kind", "conversations"])
        .expect("search command with corpus kind should parse");

    match cli.command {
        Some(Commands::Search { kind, .. }) => {
            assert_eq!(kind.as_deref(), Some("conversations"));
        }
        _ => panic!("expected search command"),
    }
}

#[test]
fn search_accepts_multiple_project_filters() {
    let cli = Cli::try_parse_from([
        "aicx",
        "search",
        "rust-mux",
        "-p",
        "vc-operator",
        "-p",
        "vibecrafted",
        "-p",
        "loctree",
    ])
    .expect("search should accept repeated project filters");

    match cli.command {
        Some(Commands::Search { project, .. }) => {
            assert_eq!(project, vec!["vc-operator", "vibecrafted", "loctree"]);
        }
        _ => panic!("expected search command"),
    }
}

#[test]
fn index_accepts_explicit_dry_run_false_for_materialization() {
    let cli = Cli::try_parse_from(["aicx", "index", "--dry-run=false"])
        .expect("index --dry-run=false should parse");

    match cli.command {
        Some(Commands::Index {
            dry_run,
            project,
            sample,
            ..
        }) => {
            assert!(!dry_run);
            assert!(project.is_empty());
            assert_eq!(sample, 0, "default materialization should index all chunks");
        }
        _ => panic!("expected index command"),
    }
}

#[test]
fn index_defaults_to_materialization() {
    let cli = Cli::try_parse_from(["aicx", "index"]).expect("index command should parse");

    match cli.command {
        Some(Commands::Index {
            dry_run, project, ..
        }) => {
            assert!(!dry_run);
            assert!(project.is_empty());
        }
        _ => panic!("expected index command"),
    }
}

#[test]
fn index_accepts_dry_run_preview() {
    let cli = Cli::try_parse_from(["aicx", "index", "--dry-run"]).expect("index --dry-run parses");

    match cli.command {
        Some(Commands::Index { dry_run, .. }) => {
            assert!(dry_run);
        }
        _ => panic!("expected index command"),
    }
}

#[test]
fn index_accepts_multiple_project_filters() {
    let cli = Cli::try_parse_from([
        "aicx",
        "index",
        "-p",
        "vc-operator",
        "-p",
        "vibecrafted",
        "-p",
        "loctree",
    ])
    .expect("index should accept repeated project filters");

    match cli.command {
        Some(Commands::Index { project, .. }) => {
            assert_eq!(project, vec!["vc-operator", "vibecrafted", "loctree"]);
        }
        _ => panic!("expected index command"),
    }
}

#[test]
fn intents_accepts_multiple_project_filters() {
    let cli = Cli::try_parse_from([
        "aicx",
        "intents",
        "-p",
        "vc-operator",
        "-p",
        "vibecrafted",
        "-p",
        "loctree",
    ])
    .expect("intents should accept repeated project filters");

    match cli.command {
        Some(Commands::Intents { project, .. }) => {
            assert_eq!(project, vec!["vc-operator", "vibecrafted", "loctree"]);
        }
        _ => panic!("expected intents command"),
    }
}

#[test]
fn steer_accepts_multiple_project_filters() {
    let cli = Cli::try_parse_from([
        "aicx",
        "steer",
        "-p",
        "vc-operator",
        "-p",
        "vibecrafted",
        "-p",
        "loctree",
    ])
    .expect("steer should accept repeated project filters");

    match cli.command {
        Some(Commands::Steer { project, .. }) => {
            assert_eq!(project, vec!["vc-operator", "vibecrafted", "loctree"]);
        }
        _ => panic!("expected steer command"),
    }
}

#[test]
fn steer_accepts_frame_kind_filter() {
    let cli = Cli::try_parse_from(["aicx", "steer", "--frame-kind", "user_msg"])
        .expect("steer command with frame-kind should parse");

    match cli.command {
        Some(Commands::Steer { filters, .. }) => {
            assert_eq!(filters.frame_kind, Some(FrameKindArg::UserMsg));
        }
        _ => panic!("expected steer command"),
    }
}

#[test]
fn intents_accepts_frame_kind_filter() {
    let cli = Cli::try_parse_from(["aicx", "intents", "--frame-kind", "tool_call"])
        .expect("intents command with frame-kind should parse");

    match cli.command {
        Some(Commands::Intents { filters, .. }) => {
            assert_eq!(filters.frame_kind, Some(FrameKindArg::ToolCall));
        }
        _ => panic!("expected intents command"),
    }
}

#[test]
fn rank_subcommand_is_rejected() {
    let err = Cli::try_parse_from(["aicx", "rank", "-p", "foo"])
        .expect_err("rank subcommand should be rejected");
    let rendered = err.to_string();
    assert!(rendered.contains("unrecognized subcommand"));
    assert!(rendered.contains("rank"));
}

#[test]
fn top_level_help_hides_retired_init_from_primary_surface() {
    let mut cmd = Cli::command();
    let rendered = cmd.render_help().to_string();

    assert!(!rendered.contains("\n  init "));
    assert!(!rendered.contains("Retired compatibility shim"));
    assert!(!rendered.contains("Initialize repo context and run an agent"));
}

#[test]
fn top_level_help_does_not_advertise_dead_root_flags() {
    let mut cmd = Cli::command();
    let rendered = cmd.render_long_help().to_string();

    assert!(!rendered.contains("used if no subcommand is provided"));
    assert!(!rendered.contains("Project filter (used if no subcommand is provided)"));
    assert!(!rendered.contains("Hours to look back (used if no subcommand is provided)"));
}

#[test]
fn primary_help_does_not_expose_layer_one_jargon() {
    let mut cmd = Cli::command();
    let mut rendered = cmd.render_long_help().to_string();

    for subcommand in ["claude", "codex", "all", "store", "refs", "dashboard"] {
        let mut subcmd = Cli::command();
        let subcmd = subcmd
            .find_subcommand_mut(subcommand)
            .unwrap_or_else(|| panic!("{subcommand} subcommand should exist"));
        rendered.push_str(&subcmd.render_long_help().to_string());
    }

    assert!(
        !rendered.to_lowercase().contains("(layer 1"),
        "primary help should describe the corpus directly, not leak layer-one jargon"
    );
}

#[test]
fn top_level_help_uses_semantic_index_language() {
    let mut cmd = Cli::command();
    let rendered = cmd.render_long_help().to_string();

    assert!(rendered.contains("Layer 2 (optional semantic index)"));
    assert!(!rendered.contains("retrieval kernel"));
}

#[test]
fn init_help_explains_retirement_and_hides_legacy_flags() {
    let mut cmd = Cli::command();
    let init = cmd
        .find_subcommand_mut("init")
        .expect("init subcommand should exist for compatibility");
    let rendered = init.render_long_help().to_string();

    assert!(rendered.contains("aicx init has been retired."));
    assert!(rendered.contains("/vc-init inside Claude Code."));
    assert!(!rendered.contains("--agent"));
    assert!(!rendered.contains("--action"));
    assert!(!rendered.contains("--no-run"));
    assert!(!rendered.contains("Initialize repo context and run an agent"));
}

#[test]
fn serve_accepts_http_and_legacy_sse_transport_names() {
    let http = Cli::try_parse_from(["aicx", "serve", "--transport", "http"])
        .expect("http transport should parse");
    let legacy = Cli::try_parse_from(["aicx", "serve", "--transport", "sse"])
        .expect("legacy sse alias should parse");

    match http.command {
        Some(Commands::Serve { transport, .. }) => {
            assert_eq!(transport, McpTransport::Http);
        }
        _ => panic!("expected serve command for http transport"),
    }

    match legacy.command {
        Some(Commands::Serve { transport, .. }) => {
            assert_eq!(transport, McpTransport::Http);
        }
        _ => panic!("expected serve command for legacy sse transport"),
    }
}

#[test]
fn serve_help_prefers_http_name_and_stays_compact() {
    let mut cmd = Cli::command();
    let serve = cmd
        .find_subcommand_mut("serve")
        .expect("serve subcommand should exist");
    let rendered = serve.render_long_help().to_string();

    assert!(rendered.contains("Transport: stdio (default) or http."));
    assert!(!rendered.contains("Transport: stdio (default) or sse"));
    assert!(!rendered.contains("embedding mode"));
    assert!(
        rendered.lines().count() < 30,
        "serve help should stay compact"
    );
}

#[test]
fn search_help_explains_semantic_first_with_fuzzy_fallback() {
    // `aicx search` is semantic-first and automatically degrades to
    // filesystem-fuzzy when semantic cannot be served. The help text must
    // surface both legs of the contract so operators know which retrieval ran.
    let mut cmd = Cli::command();
    let search = cmd
        .find_subcommand_mut("search")
        .expect("search subcommand should exist");
    let rendered = search.render_long_help().to_string();

    // Semantic leg must be visible — this is the new default.
    assert!(
        rendered.to_lowercase().contains("semantic"),
        "search --help must mention semantic retrieval (the new default)"
    );
    // Fuzzy leg must be visible too — operators need to know it is
    // the fallback, not a hidden behaviour.
    assert!(
        rendered.to_lowercase().contains("fuzzy"),
        "search --help must mention fuzzy as the fallback"
    );
    // Fallback contract must be named, not implied.
    assert!(
        rendered.to_lowercase().contains("fallback"),
        "search --help must call out the fallback path explicitly"
    );
    // Old "filesystem-only" framing must be gone — it would mislead
    // operators about what a build with `native-embedder` actually does.
    assert!(
        !rendered.contains("filesystem-only"),
        "search --help must not advertise the legacy filesystem-only contract"
    );
}

#[test]
fn read_command_parses_discover_path_and_json_mode() {
    let cli = Cli::try_parse_from([
        "aicx",
        "read",
        "store/VetCoders/aicx/2026_0502/reports/codex/chunk.md",
        "--max-chars",
        "400",
        "--json",
    ])
    .expect("read command should parse");

    match cli.command {
        Some(Commands::Read {
            reference,
            max_chars,
            json,
        }) => {
            assert_eq!(
                reference,
                "store/VetCoders/aicx/2026_0502/reports/codex/chunk.md"
            );
            assert_eq!(max_chars, Some(400));
            assert!(json);
        }
        _ => panic!("expected read command"),
    }
}

#[test]
fn open_alias_parses_as_read_for_loctree_chunk_refs() {
    let cli = Cli::try_parse_from(["aicx", "open", "chunk:590b30cd", "--max-chars", "240"])
        .expect("open alias should parse as read");

    match cli.command {
        Some(Commands::Read {
            reference,
            max_chars,
            json,
        }) => {
            assert_eq!(reference, "chunk:590b30cd");
            assert_eq!(max_chars, Some(240));
            assert!(!json);
        }
        _ => panic!("expected open alias to parse as read command"),
    }
}

#[test]
fn steer_help_stays_short_and_scope_oriented() {
    let mut cmd = Cli::command();
    let steer = cmd
        .find_subcommand_mut("steer")
        .expect("steer subcommand should exist");
    let rendered = steer.render_help().to_string();

    assert!(rendered.contains("Retrieve chunks by steering metadata"));
    assert!(rendered.contains("--project <PROJECT>"));
    assert!(!rendered.contains("aicx steer --run-id mrbl-001"));
    assert!(!rendered.contains("--no-redact-secrets"));
    assert!(!rendered.contains("--hours <HOURS>"));
    assert!(
        rendered.lines().count() < 45,
        "steer help should stay compact"
    );
}

#[test]
fn top_level_help_hides_legacy_dashboard_and_reports_commands() {
    let mut cmd = Cli::command();
    let rendered = cmd.render_long_help().to_string();

    assert!(!rendered.contains("dashboard-serve"));
    assert!(!rendered.contains("reports-extractor"));
    assert!(rendered.contains("\n  dashboard "));
    assert!(rendered.contains("\n  reports "));
}

#[test]
fn dashboard_help_describes_generate_and_serve_modes() {
    let mut cmd = Cli::command();
    let dashboard = cmd
        .find_subcommand_mut("dashboard")
        .expect("dashboard subcommand should exist");
    let rendered = dashboard.render_long_help().to_string();

    assert!(rendered.contains("--serve"));
    assert!(rendered.contains("--generate-html"));
    assert!(rendered.contains("~/.aicx/aicx-dashboard.html"));
    assert!(rendered.contains("--project <PROJECT>"));
    assert!(rendered.contains("--hours <HOURS>"));
    assert!(rendered.contains("--bg"));
    assert!(rendered.contains("--allow-cors-origins"));
    assert!(!rendered.contains("--artifact"));
}

#[test]
fn dashboard_server_only_flags_require_serve_mode() {
    let err = Cli::try_parse_from(["aicx", "dashboard", "--host", "0.0.0.0"])
        .expect_err("server-only host flag should require --serve");
    let rendered = err.to_string();

    assert!(rendered.contains("--serve"));
}

#[test]
fn dashboard_server_remote_flags_parse_with_explicit_cors_policy() {
    let cli = Cli::try_parse_from([
        "aicx",
        "dashboard",
        "--serve",
        "--host",
        "0.0.0.0",
        "--allow-cors-origins",
        "all",
        "--allow-no-origin",
        "--bg",
    ])
    .expect("remote dashboard serve flags should parse");

    match cli.command {
        Some(Commands::Dashboard(args)) => {
            assert!(args.serve);
            assert!(args.bg);
            assert!(args.allow_no_origin);
            assert_eq!(args.host.as_deref(), Some("0.0.0.0"));
            assert_eq!(args.allow_cors_origins.as_deref(), Some("all"));
        }
        _ => panic!("expected dashboard command"),
    }
}

#[test]
fn reports_help_describes_embedded_html_and_bundle() {
    let mut cmd = Cli::command();
    let reports = cmd
        .find_subcommand_mut("reports")
        .expect("reports subcommand should exist");
    let rendered = reports.render_long_help().to_string();

    assert!(rendered.contains("standalone HTML explorer"));
    assert!(rendered.contains("~/.vibecrafted/artifacts"));
    assert!(rendered.contains("~/.aicx/aicx-reports.html"));
    assert!(rendered.contains("--bundle-output"));
    assert!(rendered.contains("--date-from"));
    assert!(rendered.contains("--date-to"));
    assert!(!rendered.contains("canonical store"));
}

#[test]
fn corpus_audit_and_repair_commands_parse() {
    let audit = Cli::try_parse_from(["aicx", "corpus", "audit", "--emit", "json"])
        .expect("corpus audit should parse");
    match audit.command {
        Some(Commands::Corpus(CorpusArgs {
            command: CorpusCommand::Audit(args),
        })) => assert!(matches!(args.emit, CorpusEmit::Json)),
        _ => panic!("expected corpus audit command"),
    }

    let repair = Cli::try_parse_from([
        "aicx",
        "corpus",
        "repair",
        "--root",
        "/tmp/aicx-store",
        "--dry-run",
        "--backup",
        "--manifest",
        "/tmp/aicx-repair-preview.json",
    ])
    .expect("corpus repair should parse");
    match repair.command {
        Some(Commands::Corpus(CorpusArgs {
            command: CorpusCommand::Repair(args),
        })) => {
            assert_eq!(args.roots.root, vec![PathBuf::from("/tmp/aicx-store")]);
            assert!(args.dry_run);
            assert!(!args.apply);
            assert!(args.backup);
            assert_eq!(
                args.manifest,
                Some(PathBuf::from("/tmp/aicx-repair-preview.json"))
            );
        }
        _ => panic!("expected corpus repair command"),
    }
}

#[test]
fn doctor_apply_requires_prune_empty_bodies() {
    let cli = Cli::try_parse_from(["aicx", "doctor", "--prune-empty-bodies", "--apply"])
        .expect("doctor prune apply should parse");
    match cli.command {
        Some(Commands::Doctor {
            prune_empty_bodies,
            apply,
            ..
        }) => {
            assert!(prune_empty_bodies);
            assert!(apply);
        }
        _ => panic!("expected doctor command"),
    }

    assert!(
        Cli::try_parse_from(["aicx", "doctor", "--apply"]).is_err(),
        "--apply is only valid as a --prune-empty-bodies modifier"
    );
}

#[test]
fn store_agent_filter_is_explicit_and_includes_junie() {
    let mut cmd = Cli::command();
    let store = cmd
        .find_subcommand_mut("store")
        .expect("store subcommand should exist");
    let rendered = store.render_long_help().to_string();

    assert!(
        rendered.contains("claude, codex, gemini, junie, grok")
            || rendered.contains("claude, codex, gemini, junie")
    );
    assert!(rendered.contains("codescribe"));
    assert!(rendered.contains("operator-md"));

    let cli = Cli::try_parse_from(["aicx", "store", "--agent", "junie"])
        .expect("store should accept junie agent filter");
    match cli.command {
        Some(Commands::Store { agent, .. }) => {
            assert_eq!(agent.as_deref(), Some("junie"));
        }
        _ => panic!("expected store command"),
    }

    let cli = Cli::try_parse_from(["aicx", "store", "--agent", "codescribe"])
        .expect("store should accept codescribe agent filter");
    match cli.command {
        Some(Commands::Store { agent, .. }) => {
            assert_eq!(agent.as_deref(), Some("codescribe"));
        }
        _ => panic!("expected store command"),
    }

    let cli = Cli::try_parse_from(["aicx", "store", "--agent", "operator-md"])
        .expect("store should accept operator-md agent filter");
    match cli.command {
        Some(Commands::Store { agent, .. }) => {
            assert_eq!(agent.as_deref(), Some("operator-md"));
        }
        _ => panic!("expected store command"),
    }

    let err = Cli::try_parse_from(["aicx", "store", "--agent", "oops"])
        .expect_err("store should reject unknown agent filters");
    assert!(err.to_string().contains("possible values"));
}

#[test]
fn list_help_names_all_discovered_agent_sources() {
    let mut cmd = Cli::command();
    let list = cmd
        .find_subcommand_mut("list")
        .expect("list subcommand should exist");
    let rendered = list.render_long_help().to_string();

    assert!(
        rendered.contains("Claude Code, Codex, Gemini, Junie, and Grok log paths")
            || rendered.contains("Claude Code, Codex, Gemini, and Junie log paths")
    );
}

#[test]
fn legacy_dashboard_serve_subcommand_still_parses_hidden_compatibility_path() {
    let cli = Cli::try_parse_from(["aicx", "dashboard-serve", "--port", "9480"])
        .expect("legacy dashboard-serve alias should parse");

    match cli.command {
        Some(Commands::DashboardServeLegacy(args)) => {
            assert_eq!(args.port, 9480);
        }
        _ => panic!("expected hidden dashboard-serve compatibility command"),
    }
}

#[test]
fn legacy_reports_extractor_subcommand_still_parses_hidden_compatibility_path() {
    let cli = Cli::try_parse_from(["aicx", "reports-extractor", "--repo", "demo"])
        .expect("legacy reports-extractor alias should parse");

    match cli.command {
        Some(Commands::ReportsExtractorLegacy(args)) => {
            assert_eq!(args.repo.as_deref(), Some("demo"));
        }
        _ => panic!("expected hidden reports-extractor compatibility command"),
    }
}

#[test]
fn root_only_shortcuts_without_subcommand_are_rejected() {
    let err = Cli::try_parse_from(["aicx", "-H", "24"])
        .expect_err("root-only shortcut mode should not parse");
    let rendered = err.to_string();

    assert!(rendered.contains("unexpected argument '-H'"));
}

#[test]
fn non_corpus_commands_reject_redaction_flags() {
    let err = Cli::try_parse_from(["aicx", "search", "dashboard", "--no-redact-secrets"])
        .expect_err("search should not accept corpus-building-only redaction flags");
    let rendered = err.to_string();

    assert!(rendered.contains("--no-redact-secrets"));
}

#[test]
fn corpus_builders_accept_redaction_flags() {
    let cli = Cli::try_parse_from(["aicx", "claude", "--no-redact-secrets"])
        .expect("claude should accept corpus-building redaction flags");

    match cli.command {
        Some(Commands::Claude { redaction, .. }) => {
            assert!(!redaction.redact_secrets);
        }
        _ => panic!("expected claude command"),
    }
}

#[test]
fn extract_accepts_gemini_antigravity_format() {
    let cli = Cli::try_parse_from([
        "aicx",
        "extract",
        "--format",
        "gemini-antigravity",
        "/tmp/brain/uuid",
        "-o",
        "/tmp/report.md",
    ])
    .expect("extract command with gemini-antigravity should parse");

    match cli.command {
        Some(Commands::Extract { format, .. }) => {
            assert!(matches!(
                format,
                Some(ExtractInputFormat::GeminiAntigravity)
            ));
        }
        _ => panic!("expected extract command"),
    }
}

#[test]
fn extract_accepts_junie_format() {
    let cli = Cli::try_parse_from([
        "aicx",
        "extract",
        "--format",
        "junie",
        "/tmp/session/events.jsonl",
        "-o",
        "/tmp/report.md",
    ])
    .expect("extract command with junie should parse");

    match cli.command {
        Some(Commands::Extract { format, .. }) => {
            assert!(matches!(format, Some(ExtractInputFormat::Junie)));
        }
        _ => panic!("expected extract command"),
    }
}

#[test]
fn extract_accepts_session_mode() {
    let cli = Cli::try_parse_from([
        "aicx",
        "extract",
        "--session",
        "11111111-2222-3333-4444-555555555555",
        "--agent",
        "claude",
    ])
    .expect("extract --session should parse without positional input");

    match cli.command {
        Some(Commands::Extract {
            session,
            agent,
            input,
            output,
            ..
        }) => {
            assert_eq!(
                session.as_deref(),
                Some("11111111-2222-3333-4444-555555555555")
            );
            assert!(matches!(agent, Some(ExtractInputFormat::Claude)));
            assert!(input.is_none());
            assert!(output.is_none());
        }
        _ => panic!("expected extract command"),
    }
}

#[test]
fn extract_session_and_input_are_mutually_exclusive() {
    let res = Cli::try_parse_from([
        "aicx",
        "extract",
        "--session",
        "abc",
        "--agent",
        "junie",
        "/tmp/session/events.jsonl",
    ]);
    assert!(
        res.is_err(),
        "--session must conflict with positional INPUT path"
    );
}

#[test]
fn conversations_accepts_claude_agent_and_out_dir() {
    let cli = Cli::try_parse_from([
        "aicx",
        "conversations",
        "--agent",
        "claude",
        "--hours",
        "24",
        "--limit",
        "5",
        "--out-dir",
        "/tmp/aicx-conversations",
    ])
    .expect("conversations command should parse");

    match cli.command {
        Some(Commands::Conversations {
            agent,
            hours,
            limit,
            out_dir,
            ..
        }) => {
            assert_eq!(agent, "claude");
            assert_eq!(hours, 24);
            assert_eq!(limit, Some(5));
            assert_eq!(out_dir, PathBuf::from("/tmp/aicx-conversations"));
        }
        _ => panic!("expected conversations command"),
    }
}

#[test]
fn conversations_rejects_non_claude_agent_for_v1() {
    let err = Cli::try_parse_from([
        "aicx",
        "conversations",
        "--agent",
        "codex",
        "--out-dir",
        "/tmp/aicx-conversations",
    ])
    .expect_err("conversations v1 should reject non-claude agents");

    assert!(err.to_string().contains("possible values"));
}

#[test]
fn conversations_sanitizes_session_filename() {
    // Sanitized filenames append a SipHash suffix so distinct ids that
    // collapse to the same base do not collide on disk. Assert the
    // sanitized prefix is correct; the suffix is intentionally opaque.
    let sanitized = conversation_batch_safe_session_filename("abc/def:ghi 123");
    assert!(
        sanitized.starts_with("abc_def_ghi_123-"),
        "expected sanitized base prefix, got {sanitized}"
    );
    let empty_id = conversation_batch_safe_session_filename("");
    // Empty input has no chars to sanitize → no suffix needed.
    assert_eq!(empty_id, "session");
}

#[test]
fn conversations_output_path_is_deterministic() {
    let path =
        conversation_batch_output_path(Path::new("/tmp/aicx-conversations"), "claude", "abc/def");
    // Path contains the sanitized base + SipHash suffix; assert the
    // shape, not a fixed hash literal. `Path::join` uses `\` on Windows, so
    // normalize to forward slash before the shape assertion (no-op on Unix).
    let path_str = path.to_string_lossy().replace('\\', "/");
    assert!(
        path_str.starts_with("/tmp/aicx-conversations/claude/abc_def-"),
        "unexpected path: {path_str}"
    );
    assert!(path_str.ends_with(".json"), "unexpected path: {path_str}");

    // Determinism: same input must yield the same path.
    let path2 =
        conversation_batch_output_path(Path::new("/tmp/aicx-conversations"), "claude", "abc/def");
    assert_eq!(path, path2, "sanitized path must be deterministic");
}

#[test]
fn conversations_batch_writes_synthetic_sessions_without_store_path() {
    let root = unique_test_dir("conversations-batch");
    let out_dir = root.join("out");
    let ts = Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap();
    let entries = vec![
        timeline::TimelineEntry {
            timestamp: ts,
            agent: "claude".to_string(),
            session_id: "session-one".to_string(),
            role: "user".to_string(),
            message: "hello one".to_string(),
            frame_kind: None,
            branch: Some("main".to_string()),
            cwd: Some("/tmp/project-one".to_string()),
            timestamp_source: None,
        },
        timeline::TimelineEntry {
            timestamp: ts + chrono::Duration::seconds(1),
            agent: "claude".to_string(),
            session_id: "session-two/unsafe".to_string(),
            role: "assistant".to_string(),
            message: "hello two".to_string(),
            frame_kind: None,
            branch: None,
            cwd: Some("/tmp/project-two".to_string()),
            timestamp_source: None,
        },
    ];

    let summary = write_conversation_batch_outputs(ConversationBatchWriteOptions {
        agent_label: "claude",
        entries,
        project_filter: vec![],
        out_dir: out_dir.clone(),
        limit: None,
        dry_run: false,
        redaction_enabled: false,
    })
    .expect("synthetic batch should write conversation JSON files");

    assert_eq!(summary.sessions_discovered, 2);
    assert_eq!(summary.sessions_written, 2);
    assert_eq!(summary.failed_sessions, 0);
    assert_eq!(summary.messages_total, 2);
    // session-one needed no sanitization — bare filename.
    assert!(out_dir.join("claude/session-one.json").exists());
    // session-two/unsafe contains an unsafe `/` → sanitized base is
    // `session-two_unsafe`, with a SipHash suffix appended. Locate the
    // file by walking the directory rather than hardcoding the hash.
    let claude_dir = out_dir.join("claude");
    let entries: Vec<String> = fs::read_dir(&claude_dir)
        .expect("claude output dir must exist")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        entries
            .iter()
            .any(|name| name.starts_with("session-two_unsafe-") && name.ends_with(".json")),
        "expected a session-two_unsafe-<hash>.json file, got {entries:?}"
    );
    assert!(!out_dir.starts_with(aicx::store::store_base_dir().unwrap()));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn migrate_accepts_custom_roots() {
    let cli = Cli::try_parse_from([
        "aicx",
        "migrate",
        "--dry-run",
        "--no-intent-schema",
        "--legacy-root",
        "/tmp/legacy",
        "--store-root",
        "/tmp/aicx",
    ])
    .expect("migrate command with explicit roots should parse");

    match cli.command {
        Some(Commands::Migrate {
            dry_run,
            legacy_root,
            store_root,
            no_intent_schema,
        }) => {
            assert!(dry_run);
            assert!(no_intent_schema);
            assert_eq!(legacy_root, Some(PathBuf::from("/tmp/legacy")));
            assert_eq!(store_root, Some(PathBuf::from("/tmp/aicx")));
        }
        _ => panic!("expected migrate command"),
    }
}

#[test]
fn migrate_intent_schema_accepts_missing_project_and_defaults_to_dry_run() {
    let cli = Cli::try_parse_from(["aicx", "migrate-intent-schema"])
        .expect("migrate-intent-schema should parse without explicit project");

    match cli.command {
        Some(Commands::MigrateIntentSchema {
            project,
            store_root,
            dry_run,
        }) => {
            assert_eq!(project, None);
            assert_eq!(store_root, None);
            assert!(dry_run);
        }
        _ => panic!("expected migrate-intent-schema command"),
    }
}

#[test]
fn run_extract_file_uses_repo_identity_over_file_provenance() {
    let root = unique_test_dir("extract-repo-identity");
    let brain = root.join("brain").join("conv-9");
    let step_output = brain
        .join(".system_generated")
        .join("steps")
        .join("001")
        .join("output.txt");
    let report = root.join("report.md");

    write_file(
        &step_output,
        r#"{"project":"/Users/tester/workspace/RepoDelta","decision":"Group by repo identity."}"#,
    );
    set_mtime(&step_output, 1_706_745_900);

    run_extract_file(
        ExtractInputFormat::GeminiAntigravity,
        None,
        brain,
        report.clone(),
        ExtractFileOptions {
            include_assistant: true,
            max_message_chars: 0,
            redact_secrets: false,
            conversation: false,
        },
    )
    .unwrap();

    let output = fs::read_to_string(&report).unwrap();
    assert!(output.contains("| Filter | repodelta |"));
    assert!(output.contains("Gemini Antigravity recovery report"));
    assert!(!output.contains("| Filter | file:"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn extractor_help_states_hours_zero_is_all_time() {
    let mut cmd = Cli::command();
    for subcommand in ["all", "claude", "codex", "store"] {
        let command = cmd
            .find_subcommand_mut(subcommand)
            .expect("extractor subcommand should exist");
        let rendered = command.render_long_help().to_string();
        assert!(
            rendered.contains("0 = all time"),
            "{subcommand} --help must state the zero-hours contract"
        );
    }
}

// ====================================================================
// Pipeline-reorder cluster tests (#6, #8, #19)
// ====================================================================

fn mk_entry(
    agent: &str,
    session: &str,
    ts_secs: i64,
    message: &str,
    cwd: Option<&str>,
) -> timeline::TimelineEntry {
    timeline::TimelineEntry {
        timestamp: chrono::DateTime::<chrono::Utc>::from_timestamp(ts_secs, 0).unwrap(),
        agent: agent.to_string(),
        session_id: session.to_string(),
        role: "user".to_string(),
        message: message.to_string(),
        frame_kind: None,
        branch: None,
        cwd: cwd.map(str::to_string),
        timestamp_source: None,
    }
}

fn mk_segment(
    repo: Option<(&str, &str)>,
    agent: &str,
    session: &str,
    entries: Vec<timeline::TimelineEntry>,
) -> timeline::SemanticSegment {
    timeline::SemanticSegment {
        repo: repo.map(|(org, name)| timeline::RepoIdentity {
            organization: org.to_string(),
            repository: name.to_string(),
        }),
        source_tier: repo.map(|_| timeline::SourceTier::Primary),
        kind: timeline::Kind::default(),
        agent: agent.to_string(),
        session_id: session.to_string(),
        entries,
    }
}

/// #6: redaction must run BEFORE dedup so seen_hashes accumulate the
/// post-redact form. If dedup hashed the pre-redact form, incremental
/// and full_rescan runs would diverge on the hash domain — the audit's
/// "two competing seen-sets" pathology.
#[test]
fn test_pipeline_redacts_once_before_dedup() {
    let raw = "my key sk-abc1234567890abcdef1234567890abcdef1234";
    let redacted = aicx::redact::redact_secrets(raw);
    assert_ne!(raw, redacted, "redact_secrets must rewrite a known key");

    // The pipeline mutates message in place pre-dedup. Simulate that
    // and verify the helper hashes the redacted form, not the raw.
    let entry_raw = mk_entry("claude", "s1", 1_700_000_000, raw, Some("/tmp/repo"));
    let mut entry_redacted = entry_raw.clone();
    entry_redacted.message = redacted.clone();

    let hash_raw = StateManager::content_hash(
        &entry_raw.agent,
        entry_raw.timestamp.timestamp(),
        &entry_raw.message,
    );
    let hash_redacted = StateManager::content_hash(
        &entry_redacted.agent,
        entry_redacted.timestamp.timestamp(),
        &entry_redacted.message,
    );
    assert_ne!(
        hash_raw, hash_redacted,
        "pre-redact and post-redact hashes must differ — proves order matters"
    );

    // Now confirm dedup_segments_per_repo, given the redacted form,
    // marks `seen_hashes` under the post-redact hash (not the raw).
    let mut state = StateManager::default();
    let seg = mk_segment(
        Some(("VetCoders", "aicx")),
        "claude",
        "s1",
        vec![entry_redacted],
    );
    let kept = dedup_segments_per_repo(vec![seg], &mut state, false, |_| {});
    assert_eq!(kept.iter().map(|s| s.entries.len()).sum::<usize>(), 1);
    assert!(
        !state.is_new("VetCoders/aicx", &hash_redacted),
        "post-redact hash must be in seen_hashes after dedup"
    );
    assert!(
        state.is_new("VetCoders/aicx", &hash_raw),
        "pre-redact hash must NOT appear in seen_hashes — proves redaction ran before dedup"
    );
}

/// #8: dedup is keyed per canonical repo slug, not on `_global`. Two
/// segments with the SAME content hash but DIFFERENT canonical repos
/// must both survive — cross-repo collisions are real (e.g. shared
/// boilerplate, operator-md task-notification stubs).
#[test]
fn test_dedup_keyed_per_canonical_repo() {
    let same_message = "echo of the same boilerplate operator-md stub";
    let entry_a = mk_entry(
        "claude",
        "session-a",
        1_700_000_000,
        same_message,
        Some("/tmp/a"),
    );
    let entry_b = mk_entry(
        "claude",
        "session-b",
        1_700_000_001,
        same_message,
        Some("/tmp/b"),
    );

    // Two segments, two different canonical repos, identical content.
    let seg_a = mk_segment(
        Some(("VetCoders", "repo-a")),
        "claude",
        "session-a",
        vec![entry_a.clone()],
    );
    let seg_b = mk_segment(
        Some(("VetCoders", "repo-b")),
        "claude",
        "session-b",
        vec![entry_b.clone()],
    );

    let mut state = StateManager::default();
    let kept = dedup_segments_per_repo(vec![seg_a, seg_b], &mut state, false, |_| {});
    let total: usize = kept.iter().map(|s| s.entries.len()).sum();
    assert_eq!(
        total, 2,
        "cross-repo content collision must NOT dedup — both entries should survive"
    );

    // Verify the two hashes landed in DISTINCT seen_hashes buckets.
    let hash = StateManager::content_hash(
        &entry_a.agent,
        entry_a.timestamp.timestamp(),
        &entry_a.message,
    );
    assert!(
        !state.is_new("VetCoders/repo-a", &hash),
        "hash must be marked under repo-a's bucket"
    );
    // Different timestamps → different exact hashes. Just verify the
    // repo-b bucket has its own entry under its own hash.
    let hash_b = StateManager::content_hash(
        &entry_b.agent,
        entry_b.timestamp.timestamp(),
        &entry_b.message,
    );
    assert!(
        !state.is_new("VetCoders/repo-b", &hash_b),
        "hash must be marked under repo-b's bucket"
    );

    // And critically: the legacy `_global` bucket stays empty — proof
    // the new keying path doesn't pollute the cross-repo store.
    assert!(
        state.is_new("_global", &hash),
        "_global bucket must remain untouched under per-canonical-repo keying"
    );

    // Re-running dedup with the same segments should now SKIP both,
    // because each repo bucket already saw its own hash.
    let seg_a2 = mk_segment(
        Some(("VetCoders", "repo-a")),
        "claude",
        "session-a",
        vec![entry_a],
    );
    let seg_b2 = mk_segment(
        Some(("VetCoders", "repo-b")),
        "claude",
        "session-b",
        vec![entry_b],
    );
    let kept2 = dedup_segments_per_repo(vec![seg_a2, seg_b2], &mut state, false, |_| {});
    let total2: usize = kept2.iter().map(|s| s.entries.len()).sum();
    assert_eq!(
        total2, 0,
        "second pass must dedup both — proves per-repo state persists"
    );
}

/// PR #8 review regression guard (chatgpt-codex-connector P1):
/// under `--full-rescan` the in-memory dedup state must be shared
/// across all segments of the same canonical repo, not recreated
/// per segment. Before the fix in this commit, two segments of the
/// same repo carrying the same logical entry both survived dedup,
/// regressing full_rescan to segment-local behavior.
#[test]
fn test_full_rescan_dedups_across_segments_within_same_repo() {
    // Same content, same timestamp, same agent — both entries
    // produce identical `content_hash` and `overlap_hash`. The
    // segments share a canonical repo (VetCoders/repo-a) but live
    // in distinct sessions (the realistic shape: one repo touched
    // by several Claude sessions over time).
    let dup_message = "echo across sessions";
    let dup_ts = 1_700_000_000;
    let entry_a1 = mk_entry("claude", "s1", dup_ts, dup_message, Some("/tmp/a"));
    let entry_a2 = mk_entry("claude", "s2", dup_ts, dup_message, Some("/tmp/a"));

    let seg_s1 = mk_segment(
        Some(("VetCoders", "repo-a")),
        "claude",
        "s1",
        vec![entry_a1.clone()],
    );
    let seg_s2 = mk_segment(
        Some(("VetCoders", "repo-a")),
        "claude",
        "s2",
        vec![entry_a2.clone()],
    );

    let mut state = StateManager::default();
    // full_rescan = true: incremental `state.is_new` is bypassed,
    // dedup relies purely on the in-memory per-repo HashSets that
    // this regression guard pins as run-wide (not segment-local).
    let kept = dedup_segments_per_repo(vec![seg_s1, seg_s2], &mut state, true, |_| {});
    let total: usize = kept.iter().map(|s| s.entries.len()).sum();
    assert_eq!(
        total, 1,
        "full_rescan must dedup duplicates across segments of the \
             same repo; got {total} kept (regressed before fix)"
    );

    // And the cross-repo invariant still holds — a second repo with
    // the same content survives because each canonical repo owns
    // its own dedup bucket.
    let entry_b1 = mk_entry("claude", "s3", dup_ts, dup_message, Some("/tmp/b"));
    let seg_b = mk_segment(
        Some(("VetCoders", "repo-b")),
        "claude",
        "s3",
        vec![entry_b1],
    );
    let entry_a3 = mk_entry("claude", "s4", dup_ts, dup_message, Some("/tmp/a"));
    let seg_a3 = mk_segment(
        Some(("VetCoders", "repo-a")),
        "claude",
        "s4",
        vec![entry_a3],
    );
    let mut state2 = StateManager::default();
    let kept2 = dedup_segments_per_repo(vec![seg_a3, seg_b], &mut state2, true, |_| {});
    let total2: usize = kept2.iter().map(|s| s.entries.len()).sum();
    assert_eq!(
        total2, 2,
        "cross-repo collision MUST survive full_rescan dedup — \
             each canonical repo owns its own bucket; got {total2}"
    );
}

/// #19: watermark advances from the raw-extract latest captured
/// BEFORE self-echo / dedup filters, not from `entries.last()` after
/// filtering. This closes the self-echo-tail re-extract loop.
#[test]
fn test_watermark_advances_from_raw_extract_latest() {
    // Three entries [A (T-2), B (T-1), C (T)] where C is a self-echo
    // candidate that filtering will drop.
    let t_a = 1_700_000_000;
    let t_b = 1_700_000_001;
    let t_c = 1_700_000_002;
    let entries = vec![
        mk_entry("claude", "s1", t_a, "real signal A", Some("/tmp/repo")),
        mk_entry("claude", "s1", t_b, "real signal B", Some("/tmp/repo")),
        // A genuine self-echo marker recognized by aicx::sanitize::is_self_echo.
        mk_entry(
            "claude",
            "s1",
            t_c,
            "【aicx:read】 store-read echo\n【/aicx:read】",
            Some("/tmp/repo"),
        ),
    ];

    // 1) The new pipeline captures raw_extract_latest BEFORE filters.
    let raw_extract_latest = entries.last().map(|e| e.timestamp);
    assert_eq!(raw_extract_latest.map(|ts| ts.timestamp()), Some(t_c));

    // 2) Simulate the self-echo filter dropping the tail.
    let filtered: Vec<_> = entries
        .into_iter()
        .filter(|e| !aicx::sanitize::is_self_echo(&e.message))
        .collect();
    assert_eq!(
        filtered.last().map(|e| e.timestamp.timestamp()),
        Some(t_b),
        "self-echo filter must have dropped the tail entry — otherwise the test premise is broken"
    );

    // 3) Watermark must come from raw_extract_latest, NOT filtered.last()
    let mut state = StateManager::default();
    if let Some(latest) = raw_extract_latest {
        state.update_watermark("source-key", latest);
    }
    assert_eq!(
        state.get_watermark("source-key").map(|ts| ts.timestamp()),
        Some(t_c),
        "watermark must advance to the raw-extract tail (T), not the filtered survivor (T-1)"
    );

    // 4) Negative control: if we had written the legacy way, the
    // watermark would lag at T-1 and the self-echo tail would be
    // re-extracted on every subsequent run.
    let mut legacy_state = StateManager::default();
    if let Some(latest) = filtered.last() {
        legacy_state.update_watermark("source-key", latest.timestamp);
    }
    assert_eq!(
        legacy_state
            .get_watermark("source-key")
            .map(|ts| ts.timestamp()),
        Some(t_b),
        "legacy ordering would have produced this lagging watermark — verified for contrast"
    );
    assert_ne!(
        state.get_watermark("source-key"),
        legacy_state.get_watermark("source-key"),
        "new and legacy watermark semantics must differ — proves the fix is load-bearing"
    );
}

// =============================================================================
// L36 (b): single source-of-truth gate for self-echo CLI patterns.
//
// `CLI_SUBCOMMAND_NAMES` in `aicx_parser::sanitize` materializes the list of
// `aicx <subcommand>` patterns used by the self-echo filter. If a new variant
// lands in `Commands` (here in `main.rs`) without a matching entry in that
// constant, every routine operator invocation of the new subcommand will
// leak into extracted chunks as substantive content. This test catches the
// drift at build time instead of three weeks later in a noisy corpus audit.
// =============================================================================
#[test]
fn cli_subcommand_names_match_commands_enum() {
    use std::collections::BTreeSet;

    // Live subcommand names as clap sees them — covers default kebab-case
    // (e.g. `MigrateIntentSchema` → `migrate-intent-schema`), every
    // explicit `#[command(name = "...")]` override (e.g.
    // `DashboardServeLegacy` → `dashboard-serve`), AND every alias
    // (e.g. `Sessions` alias `session`). Aliases matter because operators
    // type them at the shell, so `aicx session show ...` lines would leak
    // into extracts if the alias is missing from the self-echo constant.
    let cmd = Cli::command();
    let live: BTreeSet<String> = cmd
        .get_subcommands()
        .flat_map(|sub| {
            std::iter::once(sub.get_name().to_string())
                .chain(sub.get_all_aliases().map(str::to_string))
        })
        .collect();

    let registered: BTreeSet<String> = aicx_parser::sanitize::CLI_SUBCOMMAND_NAMES
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    let missing_in_constant: Vec<&String> = live.difference(&registered).collect();
    let extra_in_constant: Vec<&String> = registered.difference(&live).collect();

    assert!(
        missing_in_constant.is_empty() && extra_in_constant.is_empty(),
        "aicx_parser::sanitize::CLI_SUBCOMMAND_NAMES drift detected.\n\
         missing from constant (add these): {missing_in_constant:?}\n\
         extra in constant (remove or move to RETIRED_CLI_SUBCOMMANDS): {extra_in_constant:?}"
    );
}

#[test]
fn config_subcommands_parse_after_module_split() {
    let init = Cli::try_parse_from([
        "aicx",
        "config",
        "init",
        "--force",
        "--path",
        "/tmp/aicx-config.toml",
    ])
    .expect("config init command should parse");
    let show = Cli::try_parse_from(["aicx", "config", "show", "--json"])
        .expect("config show command should parse");

    match init.command {
        Some(Commands::Config { .. }) => {}
        _ => panic!("expected config init command"),
    }
    match show.command {
        Some(Commands::Config { .. }) => {}
        _ => panic!("expected config show command"),
    }
}

#[test]
fn lane_time_coverage_normalizes_offsets_to_utc_z() {
    // P2-06: the envelope declares UTC, so offset-bearing source stamps must
    // come out shifted to UTC with a literal `Z` suffix — never `+02:00`.
    let a = DateTime::parse_from_rfc3339("2026-06-08T10:30:00+02:00").unwrap();
    let b = DateTime::parse_from_rfc3339("2026-06-08T12:00:00+02:00").unwrap();
    let cov = lane_time_coverage([b, a]).expect("two stamps yield coverage");
    assert_eq!(
        cov.earliest, "2026-06-08T08:30:00Z",
        "hour shifted, Z suffix"
    );
    assert_eq!(cov.latest, "2026-06-08T10:00:00Z");

    // Already-UTC stamps pass through unchanged (and min/max order holds).
    let u = DateTime::parse_from_rfc3339("2026-06-08T08:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let cov = lane_time_coverage([u]).expect("single stamp is its own bounds");
    assert_eq!(cov.earliest, "2026-06-08T08:00:00Z");
    assert_eq!(cov.latest, "2026-06-08T08:00:00Z");

    // No timestamps -> no coverage (the envelope omits it, never fabricates).
    assert!(lane_time_coverage(Vec::<DateTime<Utc>>::new()).is_none());
}

#[test]
fn lane_claim_source_filter_uses_shared_agent_role_predicate() {
    // P2-04: load_session_claims filters claim sources with
    // intents::is_agent_role — the SAME predicate extract_claims re-guards
    // with. Assert the re-exported predicate (the one the binary links)
    // upholds the role_filter="agent_only" contract.
    for role in ["assistant", "agent", "model", "gemini", "Assistant"] {
        assert!(intents::is_agent_role(role), "{role} rows become sources");
    }
    for role in ["user", "system", "tool", "developer"] {
        assert!(
            !intents::is_agent_role(role),
            "{role} rows never become claim sources"
        );
    }
}
