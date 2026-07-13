#[path = "../src/session_catalog.rs"]
mod session_catalog;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use session_catalog::{AgentKind, CatalogError, MAX_HEADER_BYTES, MatchKind, SessionCatalog};

const UUID_A: &str = "019f0000-1111-7111-8111-000000000001";
const UUID_B: &str = "019f0000-2222-7222-8222-000000000002";
const UUID_C: &str = "019f9999-3333-7333-8333-000000000003";

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aicx-session-catalog-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn write(&self, relative: impl AsRef<Path>, content: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn codex_meta(id: &str) -> String {
    format!(r#"{{"type":"session_meta","payload":{{"id":"{id}"}}}}"#)
}

#[test]
fn exact_suffix_filename_alias_prefix_ambiguity_and_missing_are_deterministic() {
    let root = TestRoot::new("resolution-table");
    let stem_a = format!("rollout-2026-07-13T01-02-03-{UUID_A}");
    root.write(
        format!("2026/07/13/{stem_a}.jsonl"),
        &codex_meta("logical-a"),
    );

    let catalog = SessionCatalog::new(AgentKind::Codex, root.path()).unwrap();
    let filename_a = format!("{stem_a}.jsonl");
    let cases = [
        (UUID_A, MatchKind::ExactSourceId),
        (stem_a.as_str(), MatchKind::ExactFilenameAlias),
        (filename_a.as_str(), MatchKind::ExactFilenameAlias),
        ("000000000001", MatchKind::UuidSuffix),
        ("019f0000-1", MatchKind::UniquePrefix),
        ("logical-a", MatchKind::ExactLogicalId),
    ];
    for (query, expected) in cases {
        let resolved = catalog.resolve(query).unwrap();
        assert_eq!(resolved.source.source_id, UUID_A);
        assert_eq!(resolved.matched_by, expected, "query={query}");
    }

    let missing = catalog.resolve("not-present").unwrap_err();
    assert!(matches!(missing, CatalogError::Missing { .. }));

    root.write(
        format!("2026/07/14/rollout-2026-07-14T01-02-03-{UUID_B}.jsonl"),
        &codex_meta("logical-b"),
    );
    let ambiguous = catalog.resolve("019f0000").unwrap_err();
    let CatalogError::Ambiguous { candidates, .. } = ambiguous else {
        panic!("expected ambiguity");
    };
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| candidate.source_id.as_str())
            .collect::<Vec<_>>(),
        vec![UUID_A, UUID_B]
    );
}

#[test]
fn filename_uuid_owns_source_while_root_drift_and_children_stay_explicit() {
    let root = TestRoot::new("drift-matrix");
    root.write(
        format!("{UUID_A}.jsonl"),
        concat!(
            "{\"type\":\"user\",\"sessionId\":\"root-before-compact\"}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"child-agent-id\",\"isSidechain\":true,\"parentSessionId\":\"root-before-compact\"}\n",
            "{\"type\":\"user\",\"sessionId\":\"root-after-resume\"}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"root-after-compact\"}\n",
        ),
    );
    root.write(
        "physical-without-uuid.jsonl",
        concat!(
            "{\"type\":\"user\",\"sessionId\":\"first-root-id\"}\n",
            "{\"type\":\"assistant\",\"sessionId\":\"later-root-id\"}\n",
        ),
    );

    let catalog = SessionCatalog::new(AgentKind::Claude, root.path()).unwrap();
    let scan = catalog.scan_with_stats();
    let scanned = scan.result.unwrap();
    assert_eq!(scanned.len(), 2);
    assert_eq!(scan.stats.files_opened, 2);
    assert_eq!(scan.stats.body_reads, 0);
    let drifted = catalog.resolve("root-after-compact").unwrap();
    assert_eq!(drifted.source.source_id, UUID_A);
    assert_eq!(
        drifted.source.logical_session_id.as_deref(),
        Some("root-before-compact")
    );
    assert_eq!(
        drifted.source.aliases,
        vec!["root-after-resume", "root-after-compact"]
    );
    assert_eq!(drifted.source.scoped_children.len(), 1);
    assert_eq!(drifted.source.scoped_children[0].id, "child-agent-id");
    assert_eq!(
        drifted.source.scoped_children[0].parent_id.as_deref(),
        Some("root-before-compact")
    );
    assert!(catalog.resolve("child-agent-id").is_err());

    let no_uuid = catalog.resolve("later-root-id").unwrap();
    assert_eq!(no_uuid.source.source_id, "first-root-id");
    assert_eq!(no_uuid.source.aliases, vec!["later-root-id"]);
    assert!(!no_uuid.source.identity_inferred);
}

#[test]
fn missing_record_id_uses_validated_source_identity_without_fabricating_logical_id() {
    let root = TestRoot::new("missing-id");
    root.write(
        "189-missing-id.jsonl",
        "{\"type\":\"user\",\"message\":{}}\n",
    );

    let catalog = SessionCatalog::new(AgentKind::Claude, root.path()).unwrap();
    let resolved = catalog.resolve("189-missing-id").unwrap();
    assert_eq!(resolved.source.source_id, "189-missing-id");
    assert!(resolved.source.logical_session_id.is_none());
    assert!(resolved.source.aliases.is_empty());
    assert!(resolved.source.identity_inferred);
}

#[test]
fn alias_collision_is_an_error_with_sorted_candidates_regardless_of_creation_order() {
    let orders = [[UUID_B, UUID_A], [UUID_A, UUID_B]];
    let mut observed = Vec::new();
    for (run, order) in orders.into_iter().enumerate() {
        let root = TestRoot::new(&format!("alias-order-{run}"));
        for (index, id) in order.into_iter().enumerate() {
            root.write(
                format!("dir-{index}/rollout-2026-07-13T01-02-0{index}-{id}.jsonl"),
                &codex_meta("shared-resume-alias"),
            );
        }
        let catalog = SessionCatalog::new(AgentKind::Codex, root.path()).unwrap();
        let CatalogError::Ambiguous { candidates, .. } =
            catalog.resolve("shared-resume-alias").unwrap_err()
        else {
            panic!("expected alias collision");
        };
        observed.push(
            candidates
                .into_iter()
                .map(|candidate| candidate.source_id)
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(observed[0], vec![UUID_A, UUID_B]);
    assert_eq!(observed[0], observed[1]);
}

#[test]
fn exact_filename_uuid_opens_only_the_selected_header_among_3000_unrelated_sources() {
    let root = TestRoot::new("scale");
    for index in 0..3000_u32 {
        let id = format!("aaaaaaaa-bbbb-7ccc-8ddd-{index:012x}");
        root.write(format!("bulk/{id}.jsonl"), "{}\n");
    }
    root.write(
        format!("target/{UUID_C}.jsonl"),
        &codex_meta("target-logical"),
    );

    let catalog = SessionCatalog::new(AgentKind::Codex, root.path()).unwrap();
    let lookup = catalog.resolve_with_stats(UUID_C);
    assert_eq!(lookup.result.unwrap().source.source_id, UUID_C);
    assert_eq!(lookup.stats.metadata_candidates, 3001);
    assert_eq!(lookup.stats.files_opened, 1);
    assert!(lookup.stats.header_bytes_read <= MAX_HEADER_BYTES);
    assert_eq!(lookup.stats.body_reads, 0);
}

#[test]
fn bounded_header_reader_never_crosses_byte_cap() {
    let root = TestRoot::new("bounded-header");
    let huge = format!("{{\"padding\":\"{}\"}}\n", "x".repeat(MAX_HEADER_BYTES * 2));
    root.write("huge-header.jsonl", &huge);

    let catalog = SessionCatalog::new(AgentKind::Claude, root.path()).unwrap();
    let lookup = catalog.resolve_with_stats("huge-header");
    let resolved = lookup.result.unwrap();
    assert!(resolved.source.identity_inferred);
    assert!(resolved.source.header_truncated);
    assert_eq!(lookup.stats.files_opened, 1);
    assert!(lookup.stats.header_bytes_read <= MAX_HEADER_BYTES);
    assert_eq!(lookup.stats.body_reads, 0);
}

#[cfg(unix)]
#[test]
fn symlinks_and_traversal_queries_are_rejected_without_following_targets() {
    use std::os::unix::fs::symlink;

    let root = TestRoot::new("path-adversarial");
    let outside = TestRoot::new("outside-target");
    let target = outside.write("outside.jsonl", &codex_meta(UUID_A));
    symlink(&target, root.path().join(format!("{UUID_A}.jsonl"))).unwrap();

    let catalog = SessionCatalog::new(AgentKind::Codex, root.path()).unwrap();
    let traversal = catalog.resolve("../outside").unwrap_err();
    assert!(matches!(traversal, CatalogError::InvalidQuery(_)));

    let lookup = catalog.resolve_with_stats(UUID_A);
    assert!(matches!(lookup.result, Err(CatalogError::Missing { .. })));
    assert_eq!(lookup.stats.files_opened, 0);
    assert_eq!(lookup.stats.rejected_paths, 1);
}

#[test]
fn every_lookup_refreshes_metadata_so_add_remove_rename_and_rewrite_cannot_go_stale() {
    let root = TestRoot::new("refresh");
    let first = root.write(format!("{UUID_A}.jsonl"), &codex_meta("alias-old"));
    let catalog = SessionCatalog::new(AgentKind::Codex, root.path()).unwrap();

    let initial = catalog.resolve("alias-old").unwrap();
    let initial_fingerprint = initial.source.fingerprint;
    fs::write(
        &first,
        format!("{}\n{}", codex_meta("alias-new"), "{}".repeat(8)),
    )
    .unwrap();
    let rewritten = catalog.resolve("alias-new").unwrap();
    assert_ne!(rewritten.source.fingerprint, initial_fingerprint);
    assert!(catalog.resolve("alias-old").is_err());

    fs::remove_file(&first).unwrap();
    let second = root.write(format!("{UUID_B}.jsonl"), &codex_meta("second-alias"));
    assert!(catalog.resolve(UUID_A).is_err());
    assert_eq!(catalog.resolve(UUID_B).unwrap().source.source_id, UUID_B);

    let renamed = root.path().join(format!("{UUID_C}.jsonl"));
    fs::rename(second, &renamed).unwrap();
    assert!(catalog.resolve(UUID_B).is_err());
    assert_eq!(catalog.resolve(UUID_C).unwrap().source.source_id, UUID_C);
}

#[test]
fn scan_matrix_covers_all_agent_header_shapes() {
    let fixtures = [
        (
            AgentKind::Codex,
            UUID_A,
            codex_meta("codex-logical"),
            "codex-logical",
        ),
        (
            AgentKind::Grok,
            UUID_A,
            codex_meta("grok-logical"),
            "grok-logical",
        ),
        (
            AgentKind::Claude,
            "claude-file",
            "{\"sessionId\":\"claude-logical\"}".to_string(),
            "claude-logical",
        ),
        (
            AgentKind::Gemini,
            "gemini-file",
            "{\"sessionId\":\"gemini-logical\"}".to_string(),
            "gemini-logical",
        ),
        (
            AgentKind::Junie,
            "junie-file",
            "{\"session_id\":\"junie-logical\"}".to_string(),
            "junie-logical",
        ),
    ];

    for (agent, stem, content, logical) in fixtures {
        let root = TestRoot::new(agent.as_str());
        let extension = if agent == AgentKind::Gemini {
            "json"
        } else {
            "jsonl"
        };
        root.write(format!("{stem}.{extension}"), &content);
        let catalog = SessionCatalog::new(agent, root.path()).unwrap();
        let resolved = catalog.resolve(logical).unwrap();
        assert_eq!(
            resolved.source.logical_session_id.as_deref(),
            Some(logical),
            "agent={agent}"
        );
    }
}
