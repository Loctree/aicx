use super::*;
use chrono::TimeZone;
use filetime::{FileTime, set_file_mtime};
use std::{env, fs};

// ──────────────────────────────────────────────────────────────────
// AICX-home / legacy archive / chunks-dir contract tests.
//
// The legacy tests asserted `path.contains(".aicx")` which was a
// literal-pattern relic from before `$AICX_HOME` override existed.
// Under any pinned `AICX_HOME` (e.g. the operator's
// `AICX_HOME=/Users/user/aicx`) those asserts silently failed and
// accumulated as "pre-existing baseline failures" — pass-4
// operator-agent + operator agreed that is exactly the anti-pattern
// we refuse to ship.
//
// Replacement strategy (pass-4):
//   * `aicx_home::root_for` / `chunks_dir_for` / `state_path_for`
//     are pure functions tested with explicit paths — parallel-safe,
//     deterministic, no env touched.
//   * `aicx_home::resolve` is tested under a process-wide Mutex
//     because env reads are global; the Mutex pattern keeps the two
//     env-touching tests serialized within `cargo test` without
//     pulling in a `serial_test` dependency.
// ──────────────────────────────────────────────────────────────────

/// Shared lock for tests that mutate `$AICX_HOME`. `Mutex<()>` is
/// const-constructible in modern Rust so no `Lazy` machinery is
/// needed. We always recover from poisoning via `into_inner()` —
/// a panicking test still leaves the env in a known shape because
/// every env-touching test restores via a [`Drop`] guard.
static AICX_HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard: restores `$AICX_HOME` to its prior value (or unset
/// state) when dropped. Used by env-resolution tests so panics do
/// not leak global env mutations into sibling tests.
struct AicxHomeEnvGuard {
    prev: Option<std::ffi::OsString>,
}

impl AicxHomeEnvGuard {
    fn capture() -> Self {
        Self {
            prev: env::var_os("AICX_HOME"),
        }
    }
}

impl Drop for AicxHomeEnvGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            // SAFETY: `set_var` / `remove_var` are `unsafe` from Rust
            // 2024 because they race against other threads reading
            // env. The `AICX_HOME_ENV_LOCK` Mutex guarantees we hold
            // exclusive access for the duration of the test +
            // restore, so the race window is closed.
            Some(value) => unsafe { env::set_var("AICX_HOME", value) },
            None => unsafe { env::remove_var("AICX_HOME") },
        }
    }
}

#[test]
fn test_aicx_home_root_for_is_identity_on_explicit_home() {
    let home = PathBuf::from("/tmp/test-aicx-base");
    assert_eq!(crate::aicx_home::root_for(&home), home);
}

#[test]
fn test_chunks_dir_for_lives_under_home_and_named_chunks() {
    let home = PathBuf::from("/tmp/test-aicx-chunks");
    let chunks = chunks_dir_for(&home);
    assert!(
        chunks.starts_with(&home),
        "chunks_dir_for should live under home; got {chunks:?}"
    );
    assert_eq!(
        chunks.file_name().and_then(|n| n.to_str()),
        Some("chunks"),
        "chunks_dir_for should end with `chunks`; got {chunks:?}"
    );
}

#[test]
fn legacy_cards_dir_for_is_pure_and_does_not_regrow_archive() {
    let home = retrieval_test_root("legacy-cards-path-pure");
    let _ = fs::remove_dir_all(&home);

    let archive = legacy_cards_dir_for(&home);

    assert_eq!(archive, home.join(LEGACY_CARDS_DIRNAME));
    assert!(
        !home.exists(),
        "resolving a legacy archive path must not create AICX home or store/"
    );
}

#[test]
fn test_aicx_home_resolve_honors_explicit_env_var() {
    let _serial = AICX_HOME_ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _guard = AicxHomeEnvGuard::capture();
    // SAFETY: lock is held; sibling env-touching tests cannot race.
    unsafe { env::set_var("AICX_HOME", "/tmp/test-aicx-resolve") };
    let resolved = crate::aicx_home::resolve().expect("aicx_home::resolve should succeed");
    assert_eq!(resolved, PathBuf::from("/tmp/test-aicx-resolve"));
}

#[test]
fn test_aicx_home_resolve_falls_back_to_dot_aicx_when_env_unset_and_no_bootstrap_config() {
    let home = std::env::temp_dir().join(format!("aicx-home-default-test-{}", std::process::id()));
    let resolved = crate::aicx_home::resolve_from(None, &home)
        .expect("aicx_home::resolve_from should succeed");
    assert_eq!(resolved, home.join(".aicx"));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn test_aicx_home_resolve_treats_empty_env_var_as_unset_without_bootstrap_config() {
    let home =
        std::env::temp_dir().join(format!("aicx-home-empty-env-test-{}", std::process::id()));
    let resolved = crate::aicx_home::resolve_from(Some("".into()), &home)
        .expect("aicx_home::resolve_from should succeed");
    assert_eq!(resolved, home.join(".aicx"));
    let _ = fs::remove_dir_all(home);
}

#[test]
fn test_aicx_home_resolve_uses_bootstrap_storage_home_when_env_unset() {
    let home = std::env::temp_dir().join(format!("aicx-storage-home-test-{}", std::process::id()));
    let default_home = home.join(".aicx");
    let configured = home.join("configured-aicx");
    fs::create_dir_all(&default_home).unwrap();
    fs::write(
        default_home.join("config.toml"),
        // TOML *literal* string (single quotes): a Windows path's backslashes
        // are invalid escapes in a basic ("...") string, so a literal string
        // keeps the fixture parseable on Windows (no-op for Unix paths).
        format!("[storage]\nhome = '{}'\n", configured.display()),
    )
    .unwrap();

    let resolved = crate::aicx_home::resolve_from(None, &home)
        .expect("bootstrap [storage].home should resolve");
    assert_eq!(resolved, configured);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn test_aicx_home_resolve_env_wins_over_bootstrap_storage_home() {
    let home =
        std::env::temp_dir().join(format!("aicx-storage-env-wins-test-{}", std::process::id()));
    let default_home = home.join(".aicx");
    let configured = home.join("configured-aicx");
    let pinned = home.join("env-pinned");
    fs::create_dir_all(&default_home).unwrap();
    fs::write(
        default_home.join("config.toml"),
        // TOML *literal* string (single quotes): a Windows path's backslashes
        // are invalid escapes in a basic ("...") string, so a literal string
        // keeps the fixture parseable on Windows (no-op for Unix paths).
        format!("[storage]\nhome = '{}'\n", configured.display()),
    )
    .unwrap();

    let resolved = crate::aicx_home::resolve_from(Some(pinned.clone().into_os_string()), &home)
        .expect("env-pinned home should resolve");
    assert_eq!(resolved, pinned);
    let _ = fs::remove_dir_all(home);
}

#[test]
fn test_aicx_home_resolve_rejects_relative_bootstrap_storage_home() {
    let home =
        std::env::temp_dir().join(format!("aicx-storage-relative-test-{}", std::process::id()));
    let default_home = home.join(".aicx");
    fs::create_dir_all(&default_home).unwrap();
    fs::write(
        default_home.join("config.toml"),
        "[storage]\nhome = \"relative\"\n",
    )
    .unwrap();

    let err = crate::aicx_home::resolve_from(None, &home)
        .expect_err("relative [storage].home should fail");
    assert!(
        err.to_string().contains("expected an absolute path"),
        "unexpected error: {err:#}"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn test_aicx_home_resolve_rejects_traversal_in_bootstrap_storage_home() {
    let home = std::env::temp_dir().join(format!(
        "aicx-storage-traversal-test-{}",
        std::process::id()
    ));
    let default_home = home.join(".aicx");
    fs::create_dir_all(&default_home).unwrap();
    fs::write(
        default_home.join("config.toml"),
        "[storage]\nhome = \"/tmp/storage/../../etc/aicx\"\n",
    )
    .unwrap();

    let err = crate::aicx_home::resolve_from(None, &home)
        .expect_err("[storage].home with `..` traversal should fail");
    assert!(
        err.to_string().contains("parent-directory traversal"),
        "unexpected error: {err:#}"
    );
    let _ = fs::remove_dir_all(home);
}

#[test]
fn test_aicx_home_resolve_rejects_control_chars_in_bootstrap_storage_home() {
    let home = std::env::temp_dir().join(format!(
        "aicx-storage-control-char-test-{}",
        std::process::id()
    ));
    let default_home = home.join(".aicx");
    fs::create_dir_all(&default_home).unwrap();
    fs::write(
        default_home.join("config.toml"),
        "[storage]\nhome = \"/tmp/storage\\nhome\"\n",
    )
    .unwrap();

    let err = crate::aicx_home::resolve_from(None, &home)
        .expect_err("[storage].home with control characters should fail");
    assert!(
        err.to_string().contains("control characters"),
        "unexpected error: {err:#}"
    );
    let _ = fs::remove_dir_all(home);
}

/// Integration guard: every canonical-root resolver in the workspace
/// must agree on `$AICX_HOME` when it is pinned. Covers the split-brain
/// regression (bugs #25 + #40) where seven sites still hard-coded
/// `dirs::home_dir().join(".aicx")` while the AICX-home resolver honored the
/// env override. If a future change re-introduces that drift this test
/// flips red.
#[test]
fn test_canonical_resolvers_agree_on_pinned_home() {
    let _serial = AICX_HOME_ENV_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let _guard = AicxHomeEnvGuard::capture();
    let pinned = PathBuf::from("/tmp/test-aicx-canonical-agree");
    // SAFETY: lock is held; sibling env-touching tests cannot race.
    unsafe { env::set_var("AICX_HOME", &pinned) };

    // 1. The resolver itself.
    let resolved = crate::aicx_home::resolve().expect("resolver should succeed");
    assert_eq!(resolved, pinned, "aicx_home::resolve mismatch");

    // 2. corpus::default_roots — first entry routes through resolver.
    let roots = crate::corpus::default_roots().expect("corpus roots should succeed");
    assert_eq!(
        roots.first(),
        Some(&pinned),
        "corpus::default_roots[0] should equal pinned AICX_HOME; got {roots:?}"
    );

    // 3. aicx-embeddings::config::config_search_paths — every
    //    AICX-rooted candidate must live under the pinned root. The
    //    `AICX_EMBEDDER_CONFIG` override is ignored here because it is
    //    a separate operator escape hatch, not an AICX_HOME consumer.
    //    Gated on the embedder feature flags that pull in the crate.
    #[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
    {
        let search_paths = aicx_embeddings::config_search_paths();
        let aicx_rooted: Vec<_> = search_paths
            .iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| name == "config.toml" || name == "embedder.toml")
            })
            .collect();
        assert!(
            !aicx_rooted.is_empty(),
            "config_search_paths should include AICX-rooted candidates"
        );
        for path in &aicx_rooted {
            assert!(
                path.starts_with(&pinned),
                "config_search_paths candidate {} should live under pinned AICX_HOME {}",
                path.display(),
                pinned.display()
            );
        }
    }
}

#[test]
fn test_write_context_creates_both_files() {
    let tmp = env::temp_dir().join("ai-ctx-test-store-new");
    let _ = fs::remove_dir_all(&tmp);
    let date_dir = tmp.join("TestProj").join("2026-01-22");
    fs::create_dir_all(&date_dir).unwrap();

    let entries = vec![
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 14, 30, 5).unwrap(),
            agent: "claude".to_string(),
            session_id: "sess-1".to_string(),
            role: "user".to_string(),
            message: "hello world".to_string(),
            branch: None,
            cwd: None,
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
            frame_kind: None,
        },
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 22, 14, 30, 12).unwrap(),
            agent: "claude".to_string(),
            session_id: "sess-1".to_string(),
            role: "assistant".to_string(),
            message: "hi there\nsecond line".to_string(),
            branch: None,
            cwd: None,
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
            frame_kind: None,
        },
    ];

    // Write md directly to verify format
    let md_path = date_dir.join("143005_claude-context.md");
    let mut content = String::new();
    content.push_str("# TestProj | claude | 2026-01-22\n\n");
    for entry in &entries {
        let ts = entry.timestamp.format("%Y-%m-%d %H:%M:%S UTC");
        content.push_str(&format!("### {} | {}\n", ts, entry.role));
        for line in entry.message.lines() {
            content.push_str(&format!("> {}\n", line));
        }
        content.push('\n');
    }
    fs::write(&md_path, &content).unwrap();

    let written = fs::read_to_string(&md_path).unwrap();
    assert!(written.contains("# TestProj | claude | 2026-01-22"));
    assert!(written.contains("### 2026-01-22 14:30:05 UTC | user"));
    assert!(written.contains("> hello world"));
    assert!(written.contains("> hi there"));
    assert!(written.contains("> second line"));

    // Write json
    let json_path = date_dir.join("143005_claude-context.json");
    let json_content = serde_json::to_string_pretty(&entries).unwrap();
    fs::write(&json_path, &json_content).unwrap();
    assert!(json_path.exists());

    let _ = fs::remove_dir_all(&tmp);
}

// ================================================================
// Kind classification tests
// ================================================================

fn make_entry(role: &str, message: &str) -> TimelineEntry {
    TimelineEntry {
        timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "test-session-abc123".to_string(),
        role: role.to_string(),
        message: message.to_string(),
        branch: None,
        cwd: None,
        timestamp_source: None,
        source_path: None,
        source_sha256: None,
        source_line_span: None,
        frame_kind: None,
    }
}

#[test]
fn test_kind_dir_names() {
    assert_eq!(Kind::Conversations.dir_name(), "conversations");
    assert_eq!(Kind::Plans.dir_name(), "plans");
    assert_eq!(Kind::Reports.dir_name(), "reports");
    assert_eq!(Kind::Other.dir_name(), "other");
}

#[test]
fn test_kind_parse_roundtrip() {
    for kind in [Kind::Conversations, Kind::Plans, Kind::Reports, Kind::Other] {
        let parsed = Kind::parse(kind.dir_name()).unwrap();
        assert_eq!(parsed, kind);
    }
    // Singular forms
    assert_eq!(Kind::parse("conversation"), Some(Kind::Conversations));
    assert_eq!(Kind::parse("plan"), Some(Kind::Plans));
    assert_eq!(Kind::parse("report"), Some(Kind::Reports));
    // Case insensitive
    assert_eq!(Kind::parse("PLANS"), Some(Kind::Plans));
    assert_eq!(Kind::parse("Reports"), Some(Kind::Reports));
    // Invalid
    assert_eq!(Kind::parse("bogus"), None);
}

#[test]
fn test_kind_serde_roundtrip() {
    let kind = Kind::Conversations;
    let json = serde_json::to_string(&kind).unwrap();
    assert_eq!(json, "\"conversations\"");
    let restored: Kind = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, Kind::Conversations);
}

#[test]
fn test_kind_default_is_other() {
    assert_eq!(Kind::default(), Kind::Other);
}

#[test]
fn test_classify_kind_empty_is_other() {
    assert_eq!(classify_kind(&[]), Kind::Other);
}

#[test]
fn test_classify_kind_conversation_first() {
    let entries = vec![
        make_entry("user", "Can you help me fix this bug?"),
        make_entry("assistant", "Sure, let me look at the code."),
        make_entry("user", "It crashes on startup."),
        make_entry("assistant", "I see the issue in the initialization."),
    ];
    assert_eq!(classify_kind(&entries), Kind::Conversations);
}

#[test]
fn test_classify_kind_plan() {
    let entries = vec![
        make_entry("user", "Plan the migration"),
        make_entry(
            "assistant",
            "## Plan\n\nStep 1: Audit current schema\nStep 2: Create migration scripts\nStep 3: Test on staging\nAction items for the team.",
        ),
        make_entry("user", "Looks good, what are the milestones?"),
        make_entry(
            "assistant",
            "Here are the milestones and acceptance criteria for each phase.",
        ),
    ];
    assert_eq!(classify_kind(&entries), Kind::Plans);
}

#[test]
fn test_classify_kind_report() {
    let entries = vec![
        make_entry("user", "Review the PR"),
        make_entry(
            "assistant",
            "## Findings\n\nThe code review reveals several issues.\n## Summary\nOverall quality is good.\n## Recommendations\nAdd more tests.",
        ),
        make_entry("user", "Any metrics?"),
        make_entry(
            "assistant",
            "## Metrics\nCoverage: 85%. Test results show 3 failures.\n## Conclusion\nReady after fixes.",
        ),
    ];
    assert_eq!(classify_kind(&entries), Kind::Reports);
}

#[test]
fn test_classify_kind_conservative_fallback() {
    // Ambiguous content with too few signals → Conversations (not Other)
    let entries = vec![
        make_entry("user", "What do you think about this approach?"),
        make_entry("assistant", "It could work. Let me think about the plan."),
    ];
    assert_eq!(classify_kind(&entries), Kind::Conversations);
}

#[test]
fn test_classify_kind_user_keywords_ignored() {
    // Keywords in user messages should not trigger plan/report classification
    let entries = vec![
        make_entry(
            "user",
            "## Plan\nStep 1: do this\nStep 2: do that\nStep 3: done\nAction items here",
        ),
        make_entry("assistant", "Understood, I'll help with that."),
    ];
    // Only 0 assistant plan keywords hit, so → Conversations
    assert_eq!(classify_kind(&entries), Kind::Conversations);
}

// ================================================================
// Session-first filename tests
// ================================================================

#[test]
fn test_session_basename_format() {
    let name = session_basename("2026-03-21", "claude", "abc123def456", 1);
    assert_eq!(name, "2026_0321_claude_abc123def456_001.md");
}

#[test]
fn test_session_basename_truncates_long_session_id() {
    let long_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
    let name = session_basename("2026-03-21", "claude", long_id, 3);
    // Truncates to 12 chars (dashes preserved since they're allowed)
    assert!(name.contains("a1b2c3d4-e5f"));
    assert!(name.ends_with("_003.md"));
    // Verify the full basename does NOT contain the entire UUID
    assert!(!name.contains("ef1234567890"));
}

#[test]
fn test_session_basename_chunk_ordering() {
    let a = session_basename("2026-03-21", "claude", "sess1", 1);
    let b = session_basename("2026-03-21", "claude", "sess1", 2);
    let c = session_basename("2026-03-21", "claude", "sess1", 10);
    assert!(a < b);
    assert!(b < c);
}

#[test]
fn test_session_basename_date_ordering() {
    let a = session_basename("2026-03-20", "claude", "sess1", 1);
    let b = session_basename("2026-03-21", "claude", "sess1", 1);
    assert!(a < b, "Earlier date should sort first: {} vs {}", a, b);
}

#[test]
fn test_session_basename_self_describing() {
    // A basename must be meaningful even without its directory path
    let name = session_basename("2026-03-21", "codex", "task-abc-123", 2);
    assert!(name.contains("2026_0321"), "Must contain date");
    assert!(name.contains("codex"), "Must contain agent");
    assert!(
        name.contains("task-abc-12"),
        "Must contain session fragment"
    );
    assert!(name.contains("002"), "Must contain chunk number");
    assert!(name.ends_with(".md"), "Must have .md extension");
}

#[test]
fn test_compact_date() {
    assert_eq!(compact_date("2026-03-21"), "2026_0321");
    assert_eq!(compact_date("2026-01-01"), "2026_0101");
    // Already compact
    assert_eq!(compact_date("2026_0321"), "2026_0321");
}

#[test]
fn test_truncate_session_id_short() {
    assert_eq!(truncate_session_id("abc"), "abc");
    assert_eq!(truncate_session_id(""), "");
}

#[test]
fn test_truncate_session_id_strips_non_alnum() {
    // Only alphanumeric and dashes survive
    assert_eq!(truncate_session_id("a/b:c!d@e#f"), "abcdef");
}

// ================================================================
// Chunk uniqueness within same session/day
// ================================================================

#[test]
fn test_chunk_uniqueness_same_session_day() {
    // Multiple chunks from the same session on the same day must have unique basenames
    let mut names = std::collections::HashSet::new();
    for chunk in 1..=20 {
        let name = session_basename("2026-03-21", "claude", "session-xyz", chunk);
        assert!(names.insert(name.clone()), "Duplicate basename: {}", name);
    }
}

#[test]
fn test_chunk_uniqueness_different_sessions_same_day() {
    let a = session_basename("2026-03-21", "claude", "session-aaa", 1);
    let b = session_basename("2026-03-21", "claude", "session-bbb", 1);
    assert_ne!(a, b, "Different sessions must produce different basenames");
}

#[test]
fn test_chunk_uniqueness_different_agents_same_session() {
    let a = session_basename("2026-03-21", "claude", "session-xyz", 1);
    let b = session_basename("2026-03-21", "codex", "session-xyz", 1);
    assert_ne!(a, b, "Different agents must produce different basenames");
}

// ================================================================
// Output path integration test
// ================================================================

#[test]
fn output_session_first_path_structure() {
    // Verify the full directory structure matches canonical layout
    let date = "2026-03-21";
    let kind = Kind::Conversations;
    let agent = "claude";
    let project = "ai-contexters";

    // Preserve recognition of the retired session-first archive layout.
    let expected_subpath = format!("{}/{}/{}/{}", project, date, kind.dir_name(), agent);

    let basename = session_basename(date, agent, "sess-abc123", 1);
    let full_subpath = format!("{}/{}", expected_subpath, basename);

    assert!(full_subpath.contains("conversations/claude"));
    assert!(full_subpath.ends_with("2026_0321_claude_sess-abc123_001.md"));
}

fn write_dir_sha_cache_test_sidecar(dir: &Path, stem: &str, sha: &str) {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{stem}.meta.json"));
    let sidecar = serde_json::json!({
        "id": stem,
        "project": "Vetcoders/aicx",
        "agent": "claude",
        "date": "2026-05-22",
        "session_id": "dir-sha-cache-test",
        "kind": "reports",
        "content_sha256": sha,
    });
    fs::write(path, serde_json::to_vec_pretty(&sidecar).unwrap()).unwrap();
}

#[test]
fn test_dir_sha_cache_contains_after_insert() {
    let root = retrieval_test_root("dir-sha-cache-insert");
    let _ = fs::remove_dir_all(&root);
    let dir = root.join("store").join("Vetcoders").join("aicx");
    fs::create_dir_all(&dir).unwrap();

    let mut cache = DirShaCache::default();
    assert!(!cache.contains(&dir, "sha-after-insert").unwrap());
    cache.insert(&dir, "sha-after-insert".to_string());
    assert!(cache.contains(&dir, "sha-after-insert").unwrap());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_dir_sha_cache_lazy_population() {
    let root = retrieval_test_root("dir-sha-cache-lazy");
    let _ = fs::remove_dir_all(&root);
    let dir = root.join("store").join("Vetcoders").join("aicx");
    write_dir_sha_cache_test_sidecar(&dir, "old", "sha-old");

    let mut cache = DirShaCache::default();
    assert!(cache.contains(&dir, "sha-old").unwrap());

    write_dir_sha_cache_test_sidecar(&dir, "new", "sha-new");
    assert!(
        !cache.contains(&dir, "sha-new").unwrap(),
        "cache must not rescan a dir after first lazy population"
    );

    cache.insert(&dir, "sha-new".to_string());
    assert!(cache.contains(&dir, "sha-new").unwrap());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_dir_sha_cache_does_not_cross_dirs() {
    let root = retrieval_test_root("dir-sha-cache-dir-isolation");
    let _ = fs::remove_dir_all(&root);
    let left = root.join("store").join("Vetcoders").join("aicx");
    let right = root.join("store").join("Loctree").join("aicx");
    write_dir_sha_cache_test_sidecar(&left, "left", "sha-left");
    write_dir_sha_cache_test_sidecar(&right, "right", "sha-right");

    let mut cache = DirShaCache::default();
    assert!(cache.contains(&left, "sha-left").unwrap());
    assert!(!cache.contains(&left, "sha-right").unwrap());
    assert!(cache.contains(&right, "sha-right").unwrap());
    assert!(!cache.contains(&right, "sha-left").unwrap());

    let _ = fs::remove_dir_all(&root);
}

// ================================================================
// Repo-centric retrieval tests
// ================================================================

fn retrieval_test_root(name: &str) -> PathBuf {
    let temp = env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| env::temp_dir());
    temp.join(format!(
        "aicx-retrieval-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

fn write_chunk_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn repo_chunk_path(root: &Path, session_id: &str, chunk: u32) -> PathBuf {
    root.join("store")
        .join("Vetcoders")
        .join("ai-contexters")
        .join("2026_0321")
        .join("conversations")
        .join("claude")
        .join(format!("2026_0321_claude_{session_id}_{chunk:03}.md"))
}

fn set_mtime(path: &Path, unix_seconds: i64) {
    set_file_mtime(path, FileTime::from_unix_time(unix_seconds, 0)).unwrap();
}

#[test]
fn scan_retrieves_repo_centric_files_with_correct_metadata() {
    let root = retrieval_test_root("repo-scan");
    let _ = fs::remove_dir_all(&root);

    // Create canonical repo-centric layout:
    // store/Vetcoders/ai-contexters/2026_0321/conversations/claude/<file>.md
    let chunk_dir = root
        .join("store")
        .join("Vetcoders")
        .join("ai-contexters")
        .join("2026_0321")
        .join("conversations")
        .join("claude");
    write_chunk_file(
        &chunk_dir.join("2026_0321_claude_sess-abc123_001.md"),
        "Decision: use repo-centric store layout",
    );

    let scanned = scan_context_files_at(&root).expect("scan should succeed");
    assert_eq!(scanned.len(), 1);

    let file = &scanned[0];
    assert_eq!(file.project, "Vetcoders/ai-contexters");
    assert_eq!(file.agent, "claude");
    assert_eq!(file.kind, Kind::Conversations);
    assert_eq!(file.date_compact, "2026_0321");
    assert_eq!(file.date_iso, "2026-03-21");
    assert_eq!(file.session_id, "sess-abc123");
    assert_eq!(file.chunk, 1);
    assert!(file.repo.is_some());
    assert_eq!(
        file.repo.as_ref().unwrap().slug(),
        "Vetcoders/ai-contexters"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn scan_and_read_accept_four_digit_and_collision_suffix_chunks() {
    let root = retrieval_test_root("wide-seq-and-collision-scan");
    let _ = fs::remove_dir_all(&root);

    let chunk_dir = root
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0321")
        .join("conversations")
        .join("claude");
    let four_digit = chunk_dir.join("2026_0321_claude_sess-big_1000.md");
    let collision = chunk_dir.join("2026_0321_claude_sess-collide_022-cabcdef.md");
    write_chunk_file(&four_digit, "Chunk one thousand must remain discoverable.");
    write_chunk_file(
        &collision,
        "Collision-disambiguated chunk must remain readable.",
    );
    let four_digit = four_digit.canonicalize().unwrap();
    let collision = collision.canonicalize().unwrap();

    let scanned = scan_context_files_at(&root).expect("scan should succeed");
    assert_eq!(
        scanned.len(),
        2,
        "scanner must not drop valid writer output"
    );
    assert!(scanned.iter().any(|file| {
        file.path == four_digit && file.session_id == "sess-big" && file.chunk == 1000
    }));
    assert!(scanned.iter().any(|file| {
        file.path == collision && file.session_id == "sess-collide" && file.chunk == 22
    }));

    let by_four_digit = read_context_chunk_at(&root, four_digit.to_str().unwrap(), Some(32))
        .expect("absolute _1000 path should read");
    assert_eq!(by_four_digit.chunk, 1000);
    assert!(by_four_digit.truncated);

    let by_collision = read_context_chunk_at(&root, collision.to_str().unwrap(), None)
        .expect("absolute -cHASH path should read");
    assert_eq!(by_collision.chunk, 22);
    assert_eq!(by_collision.session_id, "sess-collide");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn load_sidecar_accepts_context_corpus_raw_sibling_sidecars_layout() {
    let root = retrieval_test_root("context-corpus-sidecar-sibling");
    let _ = fs::remove_dir_all(&root);

    let pack_dir = root
        .join("context-corpus")
        .join("vetcoders")
        .join("aicx")
        .join("2026_0508")
        .join("loct-context-pack")
        .join("batch-alpha");
    let raw_path = pack_dir.join("raw").join("ctx-example.md");
    let sidecar_path = pack_dir.join("sidecars").join("ctx-example.json");

    write_text(&raw_path, "Decision: corpus examples are retrieval-only");
    write_text(
        &sidecar_path,
        &serde_json::json!({
            "id": "ctx-example",
            "project": "vetcoders/aicx",
            "agent": "loct-context-pack",
            "date": "2026-05-08",
            "session_id": "batch-alpha",
            "kind": "reports",
            "artifact_family": "loct-context-pack",
            "schema_version": "context_corpus.v1",
            "truth_status": {
                "role": "example",
                "runtime_authoritative": false,
                "stale_against_current_head": true
            }
        })
        .to_string(),
    );

    assert_eq!(sidecar_path_for_chunk(&raw_path), sidecar_path);
    let sidecar = load_sidecar(&raw_path).expect("load context-corpus sidecar");
    assert_eq!(sidecar.id, "ctx-example");
    assert!(is_context_corpus_sidecar(&sidecar));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn load_sidecar_keeps_adjacent_meta_json_priority_for_legacy_chunks() {
    let root = retrieval_test_root("sidecar-adjacent-priority");
    let _ = fs::remove_dir_all(&root);

    let chunk_dir = root
        .join("store")
        .join("Vetcoders")
        .join("aicx")
        .join("2026_0508")
        .join("reports")
        .join("codex");
    let chunk_path = chunk_dir.join("2026_0508_codex_live-sess_001.md");
    let adjacent = chunk_path.with_extension("meta.json");
    let sidecars = chunk_dir
        .join("sidecars")
        .join("2026_0508_codex_live-sess_001.json");

    write_text(&chunk_path, "Decision: live adjacent metadata wins");
    write_text(
        &adjacent,
        &serde_json::json!({
            "id": "legacy-live",
            "project": "Vetcoders/aicx",
            "agent": "codex",
            "date": "2026-05-08",
            "session_id": "live-sess",
            "kind": "reports"
        })
        .to_string(),
    );
    write_text(
        &sidecars,
        &serde_json::json!({
            "id": "sidecars-example",
            "project": "Vetcoders/aicx",
            "agent": "codex",
            "date": "2026-05-08",
            "session_id": "live-sess",
            "kind": "reports",
            "artifact_family": "loct-context-pack",
            "truth_status": {
                "role": "example",
                "runtime_authoritative": false
            }
        })
        .to_string(),
    );

    assert_eq!(sidecar_path_for_chunk(&chunk_path), adjacent);
    let sidecar = load_sidecar(&chunk_path).expect("load adjacent sidecar");
    assert_eq!(sidecar.id, "legacy-live");
    assert!(!is_context_corpus_sidecar(&sidecar));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn read_context_chunk_accepts_relative_path_file_name_and_compact_ref() {
    let root = retrieval_test_root("read-chunk");
    let _ = fs::remove_dir_all(&root);

    let chunk_path = root
        .join("store")
        .join("Vetcoders")
        .join("ai-contexters")
        .join("2026_0321")
        .join("conversations")
        .join("claude")
        .join("2026_0321_claude_sess-abc123_001.md");
    write_chunk_file(&chunk_path, "Decision: read is the re-entry primitive");

    let by_relative = read_context_chunk_at(
            &root,
            "store/Vetcoders/ai-contexters/2026_0321/conversations/claude/2026_0321_claude_sess-abc123_001.md",
            Some(14),
        )
        .expect("read by relative path");
    assert_eq!(by_relative.project, "Vetcoders/ai-contexters");
    assert_eq!(by_relative.kind, "conversations");
    assert_eq!(by_relative.session_id, "sess-abc123");
    assert_eq!(by_relative.chunk, 1);
    assert_eq!(by_relative.content, "Decision: read");
    assert!(by_relative.truncated);

    let by_file = read_context_chunk_at(&root, "2026_0321_claude_sess-abc123_001.md", None)
        .expect("read by file name");
    assert!(by_file.content.contains("re-entry primitive"));

    let by_compact = read_context_chunk_at(
        &root,
        "Vetcoders/ai-contexters|2026-03-21|conversations|claude|sess-abc123|001",
        None,
    )
    .expect("read by compact ref");
    assert_eq!(by_compact.relative_path, by_relative.relative_path);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn chunk_ref_spec_parse_accepts_paths_and_chunk_ids() {
    assert_eq!(
        ChunkRefSpec::parse("/tmp/aicx/chunk.md").unwrap(),
        ChunkRefSpec::Path(PathBuf::from("/tmp/aicx/chunk.md"))
    );
    assert_eq!(
        ChunkRefSpec::parse(
            "store/Vetcoders/ai-contexters/2026_0321/conversations/claude/chunk.md"
        )
        .unwrap(),
        ChunkRefSpec::Path(PathBuf::from(
            "store/Vetcoders/ai-contexters/2026_0321/conversations/claude/chunk.md"
        ))
    );
    assert_eq!(
        ChunkRefSpec::parse("chunk:10B84A3F").unwrap(),
        ChunkRefSpec::Id("10b84a3f".to_string())
    );
    assert_eq!(
        ChunkRefSpec::parse("10b84a3f").unwrap(),
        ChunkRefSpec::Id("10b84a3f".to_string())
    );
}

#[test]
fn read_context_chunk_resolves_chunk_id_from_absolute_path_hash() {
    let root = retrieval_test_root("read-chunk-id");
    let _ = fs::remove_dir_all(&root);

    let chunk_path = repo_chunk_path(&root, "sess-pathhash", 1);
    write_chunk_file(&chunk_path, "Decision: path hash ids are compact handles");

    let scanned = scan_context_files_at(&root).expect("scan fixture");
    assert_eq!(scanned.len(), 1);
    let id = chunk_path_ref_id(&scanned[0]);

    let by_chunk_ref =
        read_context_chunk_at(&root, &format!("chunk:{id}"), Some(16)).expect("read by chunk id");
    assert_eq!(by_chunk_ref.path, scanned[0].path);
    assert_eq!(by_chunk_ref.content, "Decision: path h");
    assert!(by_chunk_ref.truncated);

    let by_bare_id = read_context_chunk_at(&root, &id, None).expect("read by bare id");
    assert_eq!(by_bare_id.path, by_chunk_ref.path);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn read_context_chunk_reports_unknown_chunk_id() {
    let root = retrieval_test_root("read-chunk-id-miss");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();

    let err = read_context_chunk_at(&root, "chunk:deadbeef", None).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("chunk:deadbeef"));
    assert!(message.contains("not found"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn read_context_chunk_rejects_ambiguous_chunk_id_prefix_with_candidates() {
    let root = retrieval_test_root("read-chunk-id-ambiguous");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let canonical_root = sanitize::validate_dir_path(&root).unwrap();

    let mut by_prefix = std::collections::HashMap::<String, (PathBuf, String)>::new();
    let (left_path, right_path, prefix, left_id, right_id) = (0..70_000)
        .find_map(|idx| {
            let session_id = format!("ambig-{idx}");
            let path = repo_chunk_path(&canonical_root, &session_id, 1);
            let id = content_sha256(path.to_string_lossy().as_ref())
                .chars()
                .take(8)
                .collect::<String>();
            let prefix = id.chars().take(4).collect::<String>();
            if let Some((prev_path, prev_id)) = by_prefix.get(&prefix) {
                Some((prev_path.clone(), path, prefix, prev_id.clone(), id))
            } else {
                by_prefix.insert(prefix, (path, id));
                None
            }
        })
        .expect("find deterministic 4-hex prefix collision");

    write_chunk_file(&left_path, "first ambiguous chunk");
    write_chunk_file(&right_path, "second ambiguous chunk");

    let err = read_context_chunk_at(&root, &format!("chunk:{prefix}"), None).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("ambiguous chunk id"));
    assert!(message.contains("candidates:"));
    assert!(message.contains(&format!("chunk:{left_id}")));
    assert!(message.contains(&format!("chunk:{right_id}")));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn scan_retrieves_non_repository_files_with_explicit_project_label() {
    let root = retrieval_test_root("non-repo-scan");
    let _ = fs::remove_dir_all(&root);

    // Create non-repository layout:
    // non-repository-contexts/2026_0321/plans/codex/<file>.md
    let chunk_dir = root
        .join("non-repository-contexts")
        .join("2026_0321")
        .join("plans")
        .join("codex");
    write_chunk_file(
        &chunk_dir.join("2026_0321_codex_sess-xyz789_001.md"),
        "Migration plan before repo identity is known",
    );

    let scanned = scan_context_files_at(&root).expect("scan should succeed");
    assert_eq!(scanned.len(), 1);

    let file = &scanned[0];
    assert_eq!(file.project, NON_REPOSITORY_CONTEXTS);
    assert_eq!(file.agent, "codex");
    assert_eq!(file.kind, Kind::Plans);
    assert!(file.repo.is_none());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn scan_retrieves_both_repo_and_non_repo_files_together() {
    let root = retrieval_test_root("combined-scan");
    let _ = fs::remove_dir_all(&root);

    // Repo-centric file
    let repo_dir = root
        .join("store")
        .join("Vetcoders")
        .join("loctree")
        .join("2026_0320")
        .join("reports")
        .join("gemini");
    write_chunk_file(
        &repo_dir.join("2026_0320_gemini_sess-rpt001_001.md"),
        "## Report\nCoverage report for loctree scanner",
    );

    // Non-repo file
    let non_repo_dir = root
        .join("non-repository-contexts")
        .join("2026_0321")
        .join("other")
        .join("claude");
    write_chunk_file(
        &non_repo_dir.join("2026_0321_claude_sess-misc01_001.md"),
        "Unscoped brainstorm notes",
    );

    let scanned = scan_context_files_at(&root).expect("scan should succeed");
    assert_eq!(scanned.len(), 2);

    let repo_file = scanned.iter().find(|f| f.project == "Vetcoders/loctree");
    let non_repo_file = scanned
        .iter()
        .find(|f| f.project == NON_REPOSITORY_CONTEXTS);

    assert!(repo_file.is_some(), "repo-centric file must be found");
    assert!(non_repo_file.is_some(), "non-repository file must be found");

    let repo_file = repo_file.unwrap();
    assert_eq!(repo_file.kind, Kind::Reports);
    assert_eq!(repo_file.agent, "gemini");
    assert!(repo_file.repo.is_some());

    let non_repo_file = non_repo_file.unwrap();
    assert_eq!(non_repo_file.kind, Kind::Other);
    assert_eq!(non_repo_file.agent, "claude");
    assert!(non_repo_file.repo.is_none());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn context_files_since_uses_canonical_chunk_date_not_mtime() {
    let root = retrieval_test_root("context-files-since-date");
    let _ = fs::remove_dir_all(&root);

    let recent = root
        .join("store")
        .join("Vetcoders")
        .join("ai-contexters")
        .join("2026_0331")
        .join("reports")
        .join("claude")
        .join("2026_0331_claude_sess-new_001.md");
    let old = root
        .join("store")
        .join("Vetcoders")
        .join("ai-contexters")
        .join("2026_0328")
        .join("reports")
        .join("claude")
        .join("2026_0328_claude_sess-old_001.md");

    write_chunk_file(&recent, "Fresh canonical chunk");
    write_chunk_file(&old, "Stale canonical chunk");

    // Reverse the mtimes to prove recency follows the canonical store date.
    set_mtime(&recent, 1);
    set_mtime(&old, 2_000_000_000);

    let cutoff: SystemTime = Utc.with_ymd_and_hms(2026, 3, 30, 0, 0, 0).unwrap().into();
    let files = context_files_since_at(&root, cutoff, Some("ai-contexters"))
        .expect("context file filtering should succeed");

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].date_iso, "2026-03-31");
    assert_eq!(
        files[0]
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap(),
        "2026_0331_claude_sess-new_001.md"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn context_files_since_does_not_leak_substring_into_neighbor_repos() {
    // Regression guard: `context_files_since_at` previously filtered
    // by `file.project.contains(needle)`, so `-p vista` returned
    // entries from `vista-portal` AND `vista-datasets`. We migrated
    // to the strict `project_filter_matches` matcher; lock that in.
    let root = retrieval_test_root("context-files-no-substring-leak");
    let _ = fs::remove_dir_all(&root);

    let vista = root
        .join("store")
        .join("Vetcoders")
        .join("vista")
        .join("2026_0401")
        .join("reports")
        .join("claude")
        .join("2026_0401_claude_sess-vista_001.md");
    let vista_portal = root
        .join("store")
        .join("Vetcoders")
        .join("vista-portal")
        .join("2026_0401")
        .join("reports")
        .join("claude")
        .join("2026_0401_claude_sess-portal_001.md");
    write_chunk_file(&vista, "vista canonical chunk");
    write_chunk_file(&vista_portal, "vista-portal canonical chunk");

    let cutoff: SystemTime = Utc.with_ymd_and_hms(2026, 3, 30, 0, 0, 0).unwrap().into();
    let files =
        context_files_since_at(&root, cutoff, Some("vista")).expect("strict filter should succeed");

    // Exactly one file matches `-p vista` (the literal `vista`
    // repo); `vista-portal` must NOT slip in via substring match.
    assert_eq!(files.len(), 1, "got {files:?}");
    assert!(
        files[0]
            .path
            .to_string_lossy()
            // Canonical store paths are compared forward-slash; the stored
            // `path` carries the OS separator, so normalize before matching
            // (`\vista\…` on Windows must satisfy the `/vista/…` literal).
            .replace('\\', "/")
            .contains("/vista/2026_0401/"),
        "expected vista hit, got {:?}",
        files[0].path
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn chunks_by_run_id_does_not_leak_substring_into_neighbor_repos() {
    // Regression: `chunks_by_run_id_at` previously matched `-p vista`
    // against `vista-portal`, `vista-datasets`, etc. via substring.
    // Keep it aligned with strict `project_filter_matches` semantics.
    let root = retrieval_test_root("chunks-by-run-id-no-substring-leak");
    let _ = fs::remove_dir_all(&root);

    let run_id = "just-122007-20901";
    let vista = root
        .join("store")
        .join("Vetcoders")
        .join("vista")
        .join("2026_0401")
        .join("reports")
        .join("claude")
        .join("2026_0401_claude_sess-vista_001.md");
    let vista_portal = root
        .join("store")
        .join("Vetcoders")
        .join("vista-portal")
        .join("2026_0401")
        .join("reports")
        .join("claude")
        .join("2026_0401_claude_sess-portal_001.md");
    write_chunk_file(&vista, "vista run chunk");
    write_chunk_file(&vista_portal, "vista-portal run chunk");
    write_text(
        &vista.with_extension("meta.json"),
        &serde_json::json!({
            "id": "vista-run",
            "project": "Vetcoders/vista",
            "agent": "claude",
            "date": "2026-04-01",
            "session_id": "sess-vista",
            "kind": "reports",
            "run_id": run_id
        })
        .to_string(),
    );
    write_text(
        &vista_portal.with_extension("meta.json"),
        &serde_json::json!({
            "id": "vista-portal-run",
            "project": "Vetcoders/vista-portal",
            "agent": "claude",
            "date": "2026-04-01",
            "session_id": "sess-portal",
            "kind": "reports",
            "run_id": run_id
        })
        .to_string(),
    );

    let cutoff: SystemTime = Utc.with_ymd_and_hms(2026, 3, 30, 0, 0, 0).unwrap().into();
    let files = chunks_by_run_id_at(&root, run_id, Some("vista"), cutoff)
        .expect("strict run-id project filter should succeed");

    assert_eq!(files.len(), 1, "got {files:?}");
    assert!(
        files[0]
            .path
            .to_string_lossy()
            // Canonical store paths are compared forward-slash; the stored
            // `path` carries the OS separator, so normalize before matching
            // (`\vista\…` on Windows must satisfy the `/vista/…` literal).
            .replace('\\', "/")
            .contains("/vista/2026_0401/"),
        "expected vista hit, got {:?}",
        files[0].path
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn scan_context_files_respects_aicxignore_and_negation() {
    let root = retrieval_test_root("context-files-ignore");
    let _ = fs::remove_dir_all(&root);

    let ignored = root
        .join("store")
        .join("Vetcoders")
        .join("ai-contexters")
        .join("2026_0331")
        .join("reports")
        .join("codex")
        .join("2026_0331_codex_sess-rpt_001.md");
    let kept = root
        .join("store")
        .join("Vetcoders")
        .join("ai-contexters")
        .join("2026_0331")
        .join("conversations")
        .join("codex")
        .join("2026_0331_codex_sess-conv_001.md");

    write_chunk_file(&ignored, "## Report\nIgnore this chunk");
    write_chunk_file(&kept, "Conversation that should remain visible");
    fs::write(
        root.join(AICX_IGNORE_FILENAME),
        "store/Vetcoders/ai-contexters/**\n!store/Vetcoders/ai-contexters/**/conversations/**\n",
    )
    .unwrap();

    let scanned = scan_context_files_at(&root).expect("ignore-aware scan should succeed");
    assert_eq!(scanned.len(), 1);
    assert_eq!(
        scanned[0]
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap(),
        "2026_0331_codex_sess-conv_001.md"
    );

    let raw = scan_context_files_raw_at(&root).expect("raw scan should succeed");
    assert_eq!(raw.len(), 2);

    let (filtered, ignored_count) =
        filter_ignored_paths_at(&root, &[ignored.clone(), kept.clone()])
            .expect("ignore filter should succeed");
    assert_eq!(ignored_count, 1);
    assert_eq!(filtered, vec![kept]);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn load_ignore_matcher_rejects_traversal_base() {
    let root = retrieval_test_root("context-files-ignore-traversal");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join(AICX_IGNORE_FILENAME), "store/**\n").unwrap();

    // `retrieval_test_root` canonicalizes the temp dir, so on Windows `root` is
    // a `\\?\` verbatim path. Building the base with `root.join("nested")
    // .join("..")` does not preserve the `..` to the validator there — CI showed
    // `load` receiving a base with the `..` already gone (and the real
    // `.aicxignore` resolved), so the traversal guard was never exercised. Build
    // the base as a de-verbatim'd literal string instead: the `..` survives into
    // `contains_traversal` (which splits on both separators), while the OS still
    // resolves `..` for the file-exists probe on a non-verbatim path. No-op on
    // Unix (no `\\?\` prefix to strip; separator is `/`).
    let root_str = root.display().to_string();
    let root_str = root_str.strip_prefix(r"\\?\").unwrap_or(root_str.as_str());
    let traversal_base = PathBuf::from(format!(
        "{root_str}{sep}nested{sep}..",
        sep = std::path::MAIN_SEPARATOR
    ));
    let err = load_ignore_matcher_at(&traversal_base)
        .expect_err("traversal base should be rejected by validated read");
    let message = err.to_string().to_lowercase();
    assert!(message.contains("traversal"), "unexpected error: {err:#}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn migration_rebuilds_existing_sources_into_canonical_store() {
    let root = migration_test_root("rebuild-canonical");
    let legacy_root = root.join("legacy");
    let store_root = root.join("aicx");
    let repo_root = root.join("hosted").join("Vetcoders").join("ai-contexters");
    let source = root
        .join("sources")
        .join("rollout-rebuild-canonical-019be5e4.jsonl");
    let _ = fs::remove_dir_all(&root);

    fs::create_dir_all(&repo_root).unwrap();
    fs::create_dir_all(repo_root.join(".git")).unwrap();
    fs::create_dir_all(legacy_root.join("demo").join("2026-03-21")).unwrap();
    write_codex_history(
        &source,
        "sess-rebuild",
        Some(repo_root.to_string_lossy().as_ref()),
        &[
            ("user", 1_742_560_000, "Please inspect the migration seam."),
            (
                "assistant",
                1_742_560_060,
                "Reviewing the repo-centric store now.",
            ),
        ],
    );

    write_text(
        &legacy_root
            .join("demo")
            .join("2026-03-21")
            .join("101045_codex-001.md"),
        &format!("input: {}\n", source.display()),
    );

    let manifest = run_migration_at(&legacy_root, &store_root, false, &SourceLocator::default())
        .expect("run migration");

    assert_eq!(manifest.totals.rebuild_items, 1);
    assert_eq!(manifest.totals.rebuilt_items, 0);
    assert_eq!(manifest.totals.salvaged_items, 1);
    // The legacy parser is intentionally absent: migration must salvage the
    // source rather than silently fabricate canonical output.
    assert!(
        manifest
            .items
            .iter()
            .all(|item| item.canonical_paths.is_empty())
    );
    assert!(
        !store_root
            .join("store")
            .join("Vetcoders")
            .join("ai-contexters")
            .is_dir()
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn migration_salvages_legacy_bundle_when_source_is_missing() {
    let root = migration_test_root("salvage-missing");
    let legacy_root = root.join("legacy");
    let store_root = root.join("aicx");
    let missing_source = root
        .join("sources")
        .join("rollout-missing-source-019be5e4.jsonl");
    let _ = fs::remove_dir_all(&root);

    write_text(
        &legacy_root
            .join("demo")
            .join("2026-03-21")
            .join("101045_codex-001.md"),
        &format!("input: {}\n", missing_source.display()),
    );

    let manifest = run_migration_at(&legacy_root, &store_root, false, &SourceLocator::default())
        .expect("run migration");
    let item = manifest.items.first().expect("migration item");

    assert_eq!(item.action, MigrationAction::Salvage);
    assert_eq!(item.action_reason, "missing_source");
    assert!(item.canonical_paths.is_empty());
    assert!(
        item.salvage_paths
            .iter()
            .any(|path| { path.contains("/legacy-store/demo/2026-03-21/101045_codex-001.md") })
    );
    assert!(item.salvage_paths.iter().any(|path| {
        path.contains("/legacy-store/demo/2026-03-21/101045_codex.migration-provenance.json")
    }));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn migration_writes_manifest_report_and_non_repo_rebuilds() {
    let root = migration_test_root("manifest-report");
    let legacy_root = root.join("legacy");
    let store_root = root.join("aicx");
    let source = root.join("sources").join("rollout-non-repo-019be5e4.jsonl");
    let _ = fs::remove_dir_all(&root);

    write_codex_history(
        &source,
        "sess-non-repo",
        None,
        &[
            (
                "user",
                1_742_560_000,
                "Draft a migration plan before we know the repo.",
            ),
            (
                "assistant",
                1_742_560_060,
                "Working in non-repository mode for now.",
            ),
        ],
    );
    write_text(
        &legacy_root
            .join("demo")
            .join("2026-03-21")
            .join("101045_codex-001.md"),
        &format!("input: {}\n", source.display()),
    );
    write_text(&legacy_root.join("state.json"), "{\"seen_hashes\":[]}");

    let manifest = run_migration_at(&legacy_root, &store_root, false, &SourceLocator::default())
        .expect("run migration");
    let report = fs::read_to_string(&manifest.report_path).expect("read report");
    let manifest_json = fs::read_to_string(&manifest.manifest_path).expect("read manifest json");

    assert_eq!(manifest.totals.rebuilt_items, 0);
    assert!(
        manifest
            .items
            .iter()
            .all(|item| item.canonical_paths.is_empty())
    );
    assert!(manifest.items.iter().any(|item| {
        item.action_reason == "non_context_legacy_file"
            && item
                .salvage_paths
                .iter()
                .any(|path| path.contains("/legacy-store/state.json"))
    }));
    assert!(report.contains("## Rebuilt"));
    assert!(report.contains("## Unclassified Legacy Items"));
    assert!(report.contains("non_context_legacy_file"));
    assert!(manifest_json.contains("\"report_path\""));
    assert!(PathBuf::from(&manifest.report_path).exists());
    assert!(PathBuf::from(&manifest.manifest_path).exists());

    let _ = fs::remove_dir_all(&root);
}

fn migration_test_root(name: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "aicx-migration-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

fn write_text(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn write_codex_history(
    path: &Path,
    session_id: &str,
    cwd: Option<&str>,
    records: &[(&str, i64, &str)],
) {
    let mut lines = Vec::new();
    for (role, ts, text) in records {
        lines.push(
            serde_json::json!({
                "session_id": session_id,
                "text": text,
                "ts": ts,
                "role": role,
                "cwd": cwd,
            })
            .to_string(),
        );
    }

    write_text(path, &lines.join("\n"));
}

// ================================================================
// project_filter_matches — semantic filter for `aicx … -p <filter>`
// ================================================================

#[test]
fn project_filter_strict_owner_repo_match() {
    assert!(project_filter_matches(
        "Vetcoders",
        "Codescribe",
        "Vetcoders/Codescribe"
    ));
    // Case-insensitive both sides.
    assert!(project_filter_matches(
        "Vetcoders",
        "Codescribe",
        "vetcoders/codescribe"
    ));
    assert!(!project_filter_matches(
        "Vetcoders",
        "Codescribe",
        "Vetcoders/Vista"
    ));
    assert!(!project_filter_matches(
        "Vetcoders",
        "Codescribe",
        "OtherOrg/Codescribe"
    ));
}

#[test]
fn project_filter_org_wildcard_with_trailing_slash() {
    // `-p owner/` matches every repo under that owner.
    assert!(project_filter_matches("example-org", "lab", "example-org/"));
    assert!(project_filter_matches(
        "example-org",
        "spotlight-convo-pipeline-v2",
        "example-org/"
    ));
    assert!(project_filter_matches("EXAMPLE-ORG", "lab", "example-org/"));
    assert!(!project_filter_matches("vetcoders", "lab", "example-org/"));
}

#[test]
fn project_filter_repo_wildcard_with_leading_slash() {
    // `-p /repo` matches the same repo name across every owner.
    assert!(project_filter_matches(
        "Vetcoders",
        "Codescribe",
        "/Codescribe"
    ));
    assert!(project_filter_matches(
        "OtherOrg",
        "codescribe",
        "/Codescribe"
    ));
    assert!(!project_filter_matches("Vetcoders", "Vista", "/Codescribe"));
    // Exact name only — no substring leakage.
    assert!(!project_filter_matches(
        "Vetcoders",
        "Codescribe-extra",
        "/Codescribe"
    ));
}

#[test]
fn project_filter_bare_name_matches_org_or_repo() {
    // Cross-org repo match.
    assert!(project_filter_matches(
        "Vetcoders",
        "Codescribe",
        "codescribe"
    ));
    assert!(project_filter_matches(
        "OtherOrg",
        "codescribe",
        "codescribe"
    ));
    // Org match (regression for `-p example-org` use case).
    assert!(project_filter_matches(
        "example-org",
        "spotlight-convo-pipeline-v2",
        "example-org"
    ));
    // No match — different name.
    assert!(!project_filter_matches("vetcoders", "Vista", "codescribe"));
    // ---- Bug A-CLI regression ----
    // `-p vista` must NOT match `vista-portal`, `VistaBrain`, etc.
    // Substring matching is gone.
    assert!(!project_filter_matches(
        "vetcoders",
        "vista-portal",
        "vista"
    ));
    assert!(!project_filter_matches("vetcoders", "VistaBrain", "vista"));
    assert!(!project_filter_matches(
        "LibraxisAI",
        "vista-datasets",
        "vista"
    ));
    assert!(!project_filter_matches(
        "local",
        "nextra-docs-vista",
        "vista"
    ));
    // Exact "vista" still matches `vetcoders/Vista` (case-insensitive).
    assert!(project_filter_matches("vetcoders", "Vista", "vista"));
}

#[test]
fn ownerless_project_identity_is_addressable_and_participates_in_ambiguity() {
    let corpus = vec!["A/repo".to_string(), "_/repo".to_string()];

    let ambiguous =
        require_project_resolution(&["repo".to_string()], &corpus, ProjectMatchMode::Exact)
            .expect_err("bare repo must fail closed across owned and ownerless buckets");
    assert_eq!(ambiguous.candidates(), ["A/repo", "_/repo"]);

    let ownerless =
        require_project_resolution(&["_/repo".to_string()], &corpus, ProjectMatchMode::Exact)
            .expect("explicit ownerless address must resolve");
    assert_eq!(ownerless.selected, ["_/repo"]);

    let wildcard =
        require_project_resolution(&["/repo".to_string()], &corpus, ProjectMatchMode::Exact)
            .expect("cross-org wildcard must include the ownerless sentinel");
    assert_eq!(wildcard.selected, ["A/repo", "_/repo"]);
}

#[test]
fn store_scan_surfaces_ownerless_bucket_under_virtual_sentinel() {
    let root = migration_test_root("ownerless-scan");
    let directory = root
        .join(LEGACY_CARDS_DIRNAME)
        .join("repo")
        .join("2026_0717")
        .join("conversations")
        .join("codex");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join(session_basename("2026-07-17", "codex", "ownerless", 1)),
        "[project: repo | agent: codex | date: 2026-07-17]\n",
    )
    .unwrap();

    assert_eq!(project_identities_in_store_at(&root).unwrap(), ["_/repo"]);
    let files = scan_context_files_at(&root).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].project, "_/repo");
    assert_eq!(
        files[0].repo.as_ref().map(RepoIdentity::slug).as_deref(),
        Some("_/repo")
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn resolve_filters_to_slugs_expands_short_name_to_canonical() {
    let root = migration_test_root("resolve-short");
    let canonical = root.join(LEGACY_CARDS_DIRNAME);
    fs::create_dir_all(
        canonical
            .join("example-org")
            .join("spotlight-convo-pipeline-v2"),
    )
    .unwrap();
    fs::create_dir_all(canonical.join("example-org").join("lab")).unwrap();
    fs::create_dir_all(canonical.join("vetcoders").join("Codescribe")).unwrap();

    let resolved =
        resolve_filters_to_slugs_at(&canonical, &["spotlight-convo-pipeline-v2".to_string()])
            .unwrap();
    assert_eq!(resolved, vec!["example-org/spotlight-convo-pipeline-v2"]);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_filters_to_slugs_supports_explicit_syntax() {
    let root = migration_test_root("resolve-explicit");
    let canonical = root.join(LEGACY_CARDS_DIRNAME);
    fs::create_dir_all(canonical.join("example-org").join("lab")).unwrap();
    fs::create_dir_all(canonical.join("example-org").join("badi")).unwrap();
    fs::create_dir_all(canonical.join("vetcoders").join("Codescribe")).unwrap();
    fs::create_dir_all(canonical.join("OtherOrg").join("Codescribe")).unwrap();

    // owner/ → all repos under owner
    let mut got = resolve_filters_to_slugs_at(&canonical, &["example-org/".to_string()]).unwrap();
    got.sort();
    assert_eq!(got, vec!["example-org/badi", "example-org/lab"]);

    // /repo → cross-org repo match
    let mut got = resolve_filters_to_slugs_at(&canonical, &["/Codescribe".to_string()]).unwrap();
    got.sort();
    assert_eq!(got, vec!["OtherOrg/Codescribe", "vetcoders/Codescribe"]);

    // strict slug match
    let got =
        resolve_filters_to_slugs_at(&canonical, &["vetcoders/Codescribe".to_string()]).unwrap();
    assert_eq!(got, vec!["vetcoders/Codescribe"]);

    // strict slug match stays case-insensitive, but resolves to stored canonical casing
    let got =
        resolve_filters_to_slugs_at(&canonical, &["VETCODERS/codescribe".to_string()]).unwrap();
    assert_eq!(got, vec!["vetcoders/Codescribe"]);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_filters_to_slugs_no_match_returns_empty_vec() {
    let root = migration_test_root("resolve-empty");
    let canonical = root.join(LEGACY_CARDS_DIRNAME);
    fs::create_dir_all(canonical.join("foo").join("bar")).unwrap();

    let got = resolve_filters_to_slugs_at(&canonical, &["nonexistent".to_string()]).unwrap();
    assert!(got.is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_filters_to_slugs_at_or_error_rejects_unknown_filters() {
    let root = migration_test_root("resolve-error");
    let canonical = root.join(LEGACY_CARDS_DIRNAME);
    fs::create_dir_all(canonical.join("foo").join("bar")).unwrap();

    let err = resolve_filters_to_slugs_at_or_error(&canonical, &["nonexistent".to_string()])
        .expect_err("unknown filters should fail");
    let msg = err.to_string();
    assert!(msg.contains("no project matches filter(s): \"nonexistent\""));
    assert!(msg.contains("accepted forms"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_filters_to_store_or_index_slugs_supports_index_bucket_dirs() {
    // Doctrine 2026-07-23: project identities come from bucket directory
    // names (and store dirs / catalog), never from streaming embeddings.ndjson.
    let root = migration_test_root("resolve-index-only");
    fs::create_dir_all(root.join("indexed").join("vetcoders_Vista")).unwrap();
    fs::create_dir_all(root.join("indexed").join("_all")).unwrap();
    // Poison NDJSON must be ignored.
    fs::write(
        root.join("indexed").join("_all").join("embeddings.ndjson"),
        r#"{"project":"must-not-be-read"}\n"#,
    )
    .unwrap();

    let got = resolve_filters_to_store_or_index_slugs_at_or_error(
        &root,
        &["vetcoders/vista".to_string()],
    )
    .unwrap();
    assert!(
        got.iter()
            .any(|id| id.eq_ignore_ascii_case("vetcoders/Vista")),
        "expected directory-derived identity, got {got:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_filters_to_store_or_index_slugs_merges_store_and_index_bucket_dirs() {
    let root = migration_test_root("resolve-store-and-index");
    fs::create_dir_all(root.join(LEGACY_CARDS_DIRNAME).join("foo").join("bar")).unwrap();
    fs::create_dir_all(root.join("indexed").join("baz_qux")).unwrap();
    fs::create_dir_all(root.join("indexed").join("_all")).unwrap();

    let got = resolve_filters_to_store_or_index_slugs_at_or_error(
        &root,
        &["foo/bar".to_string(), "baz/qux".to_string()],
    )
    .unwrap();
    assert!(got.iter().any(|id| id == "foo/bar"), "got {got:?}");
    assert!(
        got.iter()
            .any(|id| id == "baz/qux" || id.eq_ignore_ascii_case("baz/qux")),
        "got {got:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_filters_to_index_slugs_uses_dedicated_bucket_dirs() {
    let root = migration_test_root("resolve-index-dedicated-and-all");
    let indexed = root.join("indexed");
    fs::create_dir_all(indexed.join("foo_bar")).unwrap();
    fs::create_dir_all(indexed.join("baz_qux")).unwrap();
    fs::create_dir_all(indexed.join("_all")).unwrap();

    let got = resolve_filters_to_index_slugs_at(
        &indexed,
        &["foo/bar".to_string(), "baz/qux".to_string()],
    )
    .unwrap();
    assert!(got.iter().any(|id| id == "foo/bar"), "got {got:?}");
    assert!(got.iter().any(|id| id == "baz/qux"), "got {got:?}");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn bare_filter_ambiguity_uses_directory_bucket_identities() {
    // Two dedicated buckets both ending in "vista" → bare filter fails closed.
    let root = migration_test_root("resolve-index-silver-topology");
    let indexed = root.join("indexed");
    fs::create_dir_all(indexed.join("A_vista")).unwrap();
    fs::create_dir_all(indexed.join("B_vista")).unwrap();
    fs::create_dir_all(indexed.join("B_vista-portal")).unwrap();
    fs::create_dir_all(indexed.join("_all")).unwrap();

    let error = resolve_filters_to_index_slugs_at(&indexed, &["vista".to_string()])
        .expect_err("two exact * /vista buckets must stay ambiguous");
    let resolution_error = error
        .downcast_ref::<ProjectResolutionError>()
        .expect("typed project-resolution error");
    let candidates = resolution_error.candidates();
    assert!(
        candidates.iter().any(|c| c == "A/vista"),
        "candidates={candidates:?}"
    );
    assert!(
        candidates.iter().any(|c| c == "B/vista"),
        "candidates={candidates:?}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_identities_for_search_uses_index_dirs_not_ndjson() {
    // Doctrine 2026-07-23: search `-p` resolution must never stream
    // multi-GB embeddings.ndjson. Bucket directory names + store dirs only.
    let root = migration_test_root("resolve-index-dirs-only");
    let indexed = root.join("indexed");
    fs::create_dir_all(indexed.join("vetcoders_vista")).unwrap();
    fs::create_dir_all(indexed.join("_all")).unwrap();
    // Poison NDJSON — if anyone reopens streaming, this would be the bait.
    fs::write(
        indexed.join("_all").join("embeddings.ndjson"),
        "{not valid ndjson and would hang if scanned line-by-line at multi-GB scale}\n",
    )
    .unwrap();

    let got = project_identities_for_search_at(&root).unwrap();
    assert!(
        got.iter()
            .any(|id| id == "vetcoders/vista" || id == "vetcoders_vista"),
        "expected bucket-derived identity, got {got:?}"
    );
    assert!(
        !got.iter().any(|id| id == "_all"),
        "_all must not surface as a project filter"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn resolve_filters_to_slugs_empty_input_returns_empty() {
    // Empty filters list means "all projects" by caller convention.
    let root = migration_test_root("resolve-no-filter");
    let canonical = root.join(LEGACY_CARDS_DIRNAME);
    fs::create_dir_all(canonical.join("foo").join("bar")).unwrap();

    let got = resolve_filters_to_slugs_at(&canonical, &[]).unwrap();
    assert!(got.is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn bare_project_filter_fails_closed_with_all_candidates() {
    let root = migration_test_root("ambiguous-codex");
    let canonical = root.join(LEGACY_CARDS_DIRNAME);
    fs::create_dir_all(canonical.join("codex").join("some-repo")).unwrap();
    fs::create_dir_all(canonical.join("openai").join("codex")).unwrap();
    fs::create_dir_all(canonical.join("unrelated").join("lab")).unwrap();

    let err = resolve_filters_to_slugs_at(&canonical, &["codex".to_string()])
        .expect_err("ambiguous bare filter must fail closed");
    let resolution_error = err
        .downcast_ref::<ProjectResolutionError>()
        .expect("typed project-resolution error");
    assert_eq!(
        resolution_error.candidates(),
        ["codex/some-repo", "openai/codex"]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn project_resolver_keeps_exact_and_fuzzy_modes_separate() {
    let corpus = vec![
        "B/vista".to_string(),
        "B/vista-portal".to_string(),
        "B/RepoScribe-dev".to_string(),
    ];

    let exact =
        require_project_resolution(&["B/vista".to_string()], &corpus, ProjectMatchMode::Exact)
            .unwrap();
    assert_eq!(exact.selected, ["B/vista"]);

    let bare = require_project_resolution(&["vista".to_string()], &corpus, ProjectMatchMode::Exact)
        .unwrap();
    assert_eq!(bare.selected, ["B/vista"]);

    let fuzzy =
        require_project_resolution(&["vista".to_string()], &corpus, ProjectMatchMode::Fuzzy)
            .unwrap();
    assert_eq!(fuzzy.selected, ["B/vista", "B/vista-portal"]);

    let suffix = require_project_resolution(
        &["B/RepoScribe".to_string()],
        &corpus,
        ProjectMatchMode::Exact,
    )
    .expect_err("owner/repo exact must not match a family suffix");
    assert_eq!(suffix.kind(), "project_not_found");
}

#[test]
fn project_filter_edge_cases() {
    // Empty / whitespace filter rejects all.
    assert!(!project_filter_matches("vetcoders", "Vista", ""));
    assert!(!project_filter_matches("vetcoders", "Vista", "   "));
    // Lone or malformed separators reject all.
    assert!(!project_filter_matches("vetcoders", "Vista", "/"));
    assert!(!project_filter_matches("vetcoders", "Vista", "//"));
    // `/owner/repo` strips one leading slash — the remainder still has `/`
    // and a repo name never contains `/`, so reject.
    assert!(!project_filter_matches(
        "vetcoders",
        "Vista",
        "/vetcoders/Vista"
    ));
    // `owner/repo/extra` is not a valid slug — reject.
    assert!(!project_filter_matches("vetcoders", "Vista", "foo/bar/baz"));
}

#[test]
fn test_malformed_index_json_returns_error_not_default() {
    let root = retrieval_test_root("malformed-index");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let index_path = root.join("index.json");

    // Primary alone is malformed and no .bak exists → Err.
    fs::write(&index_path, b"{ this is not valid json").unwrap();
    let err = load_index_at(&root).expect_err("malformed index must surface an error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("parse failed") || msg.contains(".bak"),
        "error message should mention parse failure / bak fallback: {msg}"
    );

    // Add a valid .bak sibling → recovery returns it.
    let bak_path = index_path.with_extension("json.bak");
    let now = Utc::now();
    let good = StoreIndex {
        projects: std::collections::HashMap::from([(
            "Vetcoders/aicx".to_string(),
            ProjectIndex {
                agents: std::collections::HashMap::from([(
                    "claude".to_string(),
                    AgentIndex {
                        dates: vec!["2026_0520".to_string()],
                        total_entries: 7,
                        last_updated: now,
                    },
                )]),
            },
        )]),
        last_updated: now,
    };
    fs::write(
        &bak_path,
        serde_json::to_string_pretty(&good).unwrap().as_bytes(),
    )
    .unwrap();
    let recovered = load_index_at(&root).expect("recovery via .bak must succeed");
    assert!(
        recovered
            .projects
            .get("Vetcoders/aicx")
            .and_then(|p| p.agents.get("claude"))
            .is_some(),
        "recovered index must contain the .bak payload"
    );

    let _ = fs::remove_dir_all(&root);
}

// ──────────────────────────────────────────────────────────────────
// Bug #34: loct context pack homogeneity validation.
// Pre-ingest check rejects packs whose sidecars declare more than
// one (org, repo) tuple, before any chunk hits disk. Homogeneous
// packs are unaffected (covered by tests/e2e_context_pack_ingest.rs).
// ──────────────────────────────────────────────────────────────────

fn unique_pack_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    env::temp_dir().join(format!(
        "aicx-pack-consistency-{name}-{}-{nanos}",
        std::process::id(),
    ))
}

fn write_pack_sidecar(pack: &Path, stem: &str, project: &str, date: &str) {
    let raw = pack.join("raw").join(format!("{stem}.md"));
    let sidecar = pack.join("sidecars").join(format!("{stem}.json"));
    fs::create_dir_all(raw.parent().unwrap()).unwrap();
    fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    fs::write(
            &raw,
            format!(
                "[project: {project} | agent: loct-context-pack | date: {date}]\n\n[signals]\nDecision:\n- [decision] test\n[/signals]\n",
            ),
        )
        .unwrap();
    let body = serde_json::json!({
        "id": stem,
        "project": project,
        "agent": "loct-context-pack",
        "date": date,
        "session_id": stem,
        "kind": "reports",
        "artifact_family": "loct-context-pack",
        "schema_version": "context_corpus.v1",
        "truth_status": {
            "role": "example",
            "runtime_authoritative": false,
            "stale_against_current_head": false,
        },
    });
    fs::write(&sidecar, body.to_string()).unwrap();
}

#[test]
fn ingest_loct_context_pack_rejects_mixed_projects() {
    let pack = unique_pack_dir("mixed");
    write_pack_sidecar(&pack, "alpha", "Vetcoders/aicx", "2026-05-08");
    write_pack_sidecar(&pack, "beta", "Loctree/aicx", "2026-05-08");

    let err = ingest_loct_context_pack(&pack)
        .expect_err("mixed-project pack must fail pre-ingest validation");
    let message = format!("{err:#}");

    // Names both project tuples.
    assert!(
        message.contains("Vetcoders/aicx") || message.contains("vetcoders/aicx"),
        "error should name first project tuple; got {message}"
    );
    assert!(
        message.contains("Loctree/aicx") || message.contains("loctree/aicx"),
        "error should name offending project tuple; got {message}"
    );
    // Names at least one offending sidecar path.
    assert!(
        message.contains("beta.json"),
        "error should name offending sidecar path; got {message}"
    );
    // Explicit "mixes projects" framing.
    assert!(
        message.contains("mixes projects"),
        "error should use the mixes-projects framing; got {message}"
    );

    let _ = fs::remove_dir_all(&pack);
}

#[test]
fn ingest_loct_context_pack_rejects_empty_pack() {
    let pack = unique_pack_dir("empty");
    fs::create_dir_all(pack.join("raw")).unwrap();
    fs::create_dir_all(pack.join("sidecars")).unwrap();

    let err = ingest_loct_context_pack(&pack)
        .expect_err("pack with no raw/*.md chunks must fail pre-ingest validation");
    let message = format!("{err:#}");
    // Distinct failure mode from mixed-projects (#34 brief edge call).
    assert!(
        message.contains("no raw/*.md chunks"),
        "empty pack must surface the empty-pack failure mode, not 'mixes projects'; got {message}"
    );
    assert!(
        !message.contains("mixes projects"),
        "empty pack must not be reported as mixed-projects; got {message}"
    );

    let _ = fs::remove_dir_all(&pack);
}

// ──────────────────────────────────────────────────────────────────
// Bug #35: re-ingest must preserve index.jsonl rows.
// Previously the index was truncated and rewritten with only the
// NEW rows from the second pack, dropping the manifest entries for
// chunks that were stored by the first pack. The fix merges new
// rows into the loaded set by id and atomic-writes the union.
// ──────────────────────────────────────────────────────────────────

fn unique_ingest_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    env::temp_dir().join(format!(
        "aicx-index-preserve-{name}-{}-{nanos}",
        std::process::id(),
    ))
}

fn reset_pack_dir(pack: &Path) {
    let _ = fs::remove_dir_all(pack);
    fs::create_dir_all(pack.join("raw")).unwrap();
    fs::create_dir_all(pack.join("sidecars")).unwrap();
}

fn write_pack_chunk(pack: &Path, stem: &str, project: &str, date: &str, body: &str) {
    let raw = pack.join("raw").join(format!("{stem}.md"));
    let sidecar = pack.join("sidecars").join(format!("{stem}.json"));
    fs::create_dir_all(raw.parent().unwrap()).unwrap();
    fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    let raw_content = format!(
        "[project: {project} | agent: loct-context-pack | date: {date}]\n\n[signals]\nDecision:\n- [decision] {body}\n[/signals]\n",
    );
    fs::write(&raw, raw_content).unwrap();
    let sidecar_body = serde_json::json!({
        "id": stem,
        "project": project,
        "agent": "loct-context-pack",
        "date": date,
        "session_id": stem,
        "kind": "reports",
        "artifact_family": "loct-context-pack",
        "schema_version": "context_corpus.v1",
        "truth_status": {
            "role": "example",
            "runtime_authoritative": false,
            "stale_against_current_head": false,
        },
    });
    fs::write(&sidecar, sidecar_body.to_string()).unwrap();
}

fn read_index_ids(index_path: &Path) -> Vec<String> {
    let raw = fs::read_to_string(index_path).expect("index.jsonl readable");
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("index row is valid json");
            value["id"]
                .as_str()
                .expect("index row has string id")
                .to_string()
        })
        .collect()
}

fn with_temp_ingest_home<F: FnOnce(&Path)>(label: &str, body: F) {
    let aicx_home = unique_ingest_dir(label);
    fs::create_dir_all(&aicx_home).unwrap();
    body(&aicx_home);
    let _ = fs::remove_dir_all(&aicx_home);
}

#[test]
fn ingest_loct_context_pack_preserves_rows_for_subset_reingest() {
    with_temp_ingest_home("subset", |home| {
        let pack = unique_ingest_dir("subset-pack").join("batch-alpha");
        reset_pack_dir(&pack);
        write_pack_chunk(
            &pack,
            "alpha",
            "Vetcoders/aicx",
            "2026-05-08",
            "first chunk",
        );
        write_pack_chunk(
            &pack,
            "beta",
            "Vetcoders/aicx",
            "2026-05-08",
            "second chunk",
        );
        let first = ingest_loct_context_pack_into(&pack, Some(home)).expect("first ingest");
        let first_ids = read_index_ids(&first.index_path);
        assert_eq!(first_ids.len(), 2);
        assert!(first_ids.iter().any(|id| id == "alpha"));
        assert!(first_ids.iter().any(|id| id == "beta"));

        // The second pack only re-presents alpha. The old bug rewrote
        // index.jsonl from the second batch and dropped beta.
        reset_pack_dir(&pack);
        write_pack_chunk(
            &pack,
            "alpha",
            "Vetcoders/aicx",
            "2026-05-08",
            "first chunk",
        );

        let second = ingest_loct_context_pack_into(&pack, Some(home)).expect("second ingest");
        assert_eq!(second.deduped_chunks, 1, "alpha is content-identical");
        assert_eq!(
            second.raw_written, 0,
            "no brand-new chunks in subset re-ingest"
        );
        let second_ids = read_index_ids(&second.index_path);
        assert_eq!(second_ids.len(), 2);
        assert!(second_ids.iter().any(|id| id == "alpha"));
        assert!(
            second_ids.iter().any(|id| id == "beta"),
            "index.jsonl must preserve chunks not re-presented by the second pack"
        );

        let _ = fs::remove_dir_all(pack.parent().unwrap());
    });
}

#[test]
fn ingest_loct_context_pack_preserves_rows_for_identical_reingest() {
    with_temp_ingest_home("identical", |home| {
        let pack = unique_ingest_dir("identical-pack").join("batch-alpha");
        reset_pack_dir(&pack);
        write_pack_chunk(
            &pack,
            "alpha",
            "Vetcoders/aicx",
            "2026-05-08",
            "first chunk",
        );
        write_pack_chunk(
            &pack,
            "beta",
            "Vetcoders/aicx",
            "2026-05-08",
            "second chunk",
        );
        let first = ingest_loct_context_pack_into(&pack, Some(home)).expect("first ingest");
        assert_eq!(read_index_ids(&first.index_path).len(), 2);

        reset_pack_dir(&pack);
        write_pack_chunk(
            &pack,
            "alpha",
            "Vetcoders/aicx",
            "2026-05-08",
            "first chunk",
        );
        write_pack_chunk(
            &pack,
            "beta",
            "Vetcoders/aicx",
            "2026-05-08",
            "second chunk",
        );

        let second = ingest_loct_context_pack_into(&pack, Some(home)).expect("second ingest");
        assert_eq!(
            second.deduped_chunks, 2,
            "both chunks are content-identical"
        );
        assert_eq!(
            second.raw_written, 0,
            "identical re-ingest writes no new raw chunks"
        );
        let second_ids = read_index_ids(&second.index_path);
        assert_eq!(second_ids.len(), 2);
        assert!(second_ids.iter().any(|id| id == "alpha"));
        assert!(second_ids.iter().any(|id| id == "beta"));

        let _ = fs::remove_dir_all(pack.parent().unwrap());
    });
}

#[test]
fn ingest_loct_context_pack_unions_old_and_new_on_reingest() {
    with_temp_ingest_home("union", |home| {
        let pack = unique_ingest_dir("union-pack").join("batch-alpha");
        reset_pack_dir(&pack);
        write_pack_chunk(
            &pack,
            "alpha",
            "Vetcoders/aicx",
            "2026-05-08",
            "first chunk",
        );
        let first = ingest_loct_context_pack_into(&pack, Some(home)).expect("first ingest");
        let first_ids = read_index_ids(&first.index_path);
        assert_eq!(first_ids, vec!["alpha".to_string()]);

        // Second pack carries the same alpha chunk plus a brand-new
        // gamma chunk; the union must include both.
        reset_pack_dir(&pack);
        write_pack_chunk(
            &pack,
            "alpha",
            "Vetcoders/aicx",
            "2026-05-08",
            "first chunk",
        );
        write_pack_chunk(
            &pack,
            "gamma",
            "Vetcoders/aicx",
            "2026-05-08",
            "third chunk",
        );

        let second = ingest_loct_context_pack_into(&pack, Some(home)).expect("second ingest");
        assert_eq!(second.deduped_chunks, 1, "alpha is content-identical");
        assert_eq!(second.raw_written, 1, "gamma is brand-new");
        let second_ids = read_index_ids(&second.index_path);
        assert_eq!(second_ids.len(), 2);
        assert!(second_ids.iter().any(|id| id == "alpha"));
        assert!(second_ids.iter().any(|id| id == "gamma"));

        let _ = fs::remove_dir_all(pack.parent().unwrap());
    });
}

#[test]
fn chunk_body_is_empty_is_header_agnostic() {
    // Legacy bracket header — empty and non-empty bodies.
    assert!(chunk_body_is_empty(
        "[project: demo | agent: codex | date: 2026-07-02]\n\n"
    ));
    assert!(!chunk_body_is_empty(
        "[project: demo | agent: codex | date: 2026-07-02]\n\nDecision: real content survives\n"
    ));

    // Card schema v2 YAML frontmatter — same verdicts.
    assert!(chunk_body_is_empty(
        "---\nproject: demo\nagent: codex\ndate: 2026-07-02\n---\n\n"
    ));
    assert!(!chunk_body_is_empty(
        "---\nproject: demo\nagent: codex\ndate: 2026-07-02\n---\n\nDecision: real content survives\n"
    ));

    // A body that merely opens with a horizontal rule is NOT a header and
    // must never be chopped into an empty verdict.
    assert!(!chunk_body_is_empty(
        "---\nsome unrelated text\n---\nDecision: real content survives\n"
    ));
}
