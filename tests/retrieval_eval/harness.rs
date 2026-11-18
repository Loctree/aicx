//! Real source -> published index -> query acceptance on a public fixture.
//! Portable runs measure lexical retrieval. `e2e-aicx` runs the same questions
//! through the explicitly configured real embedder and hybrid fusion.
#![cfg(feature = "app")]
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Deserialize)]
struct FixtureConfig {
    schema: String,
    minimum_recall_at_5: f64,
    queries: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    query: String,
    #[serde(default)]
    semantic_only_query: Option<String>,
    answer: String,
    expected_snippet: String,
    #[serde(default)]
    tail_fixture: bool,
    #[serde(default)]
    context_fixture: bool,
}

#[derive(Serialize)]
struct Measurement {
    id: String,
    expected_rank: Option<usize>,
    recall_at_5: f64,
    latency_ms: u128,
    evidence_matches: bool,
    semantic_challenge: bool,
    lexical_control_found_target: Option<bool>,
}

struct IsolatedHome {
    root: PathBuf,
    previous: Option<std::ffi::OsString>,
}
impl Drop for IsolatedHome {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => unsafe { std::env::set_var("AICX_HOME", v) },
            None => unsafe { std::env::remove_var("AICX_HOME") },
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn reproducible_retrieval_acceptance() {
    let config: FixtureConfig = toml::from_str(include_str!("queries.toml")).unwrap();
    assert_eq!(config.schema, "aicx.retrieval_fixture.v1");
    assert!(config.queries.len() >= 12);
    assert_eq!(
        config.minimum_recall_at_5, 1.0,
        "every question needs its known answer"
    );
    let real_model =
        cfg!(feature = "e2e-aicx") || std::env::var_os("AICX_RETRIEVAL_EVAL_REAL_MODEL").is_some();
    if real_model {
        assert!(
            config
                .queries
                .iter()
                .any(|q| q.semantic_only_query.is_some()),
            "real-model acceptance needs a semantic-only control"
        );
        let config = std::env::var_os("AICX_EMBEDDER_CONFIG").expect(
            "live eval requires explicit AICX_EMBEDDER_CONFIG; no implicit production index",
        );
        assert!(PathBuf::from(config).is_file());
    }
    let root = std::env::temp_dir().join(format!(
        "aicx-retrieval-fixture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(root.join("catalog")).unwrap();
    let _home = IsolatedHome {
        root: root.clone(),
        previous: std::env::var_os("AICX_HOME"),
    };
    // Direct catalog+source API only: this does not enumerate provider homes.
    unsafe {
        std::env::set_var("AICX_HOME", &root);
    }
    let mut catalog = String::new();
    for (n, case) in config.queries.iter().enumerate() {
        assert!(!case.id.is_empty() && !case.expected_snippet.is_empty());
        let sid = format!("00000000-0000-4000-8000-{:012}", n + 1);
        let path = root.join(format!("{sid}.jsonl"));
        let answer = if case.tail_fixture {
            format!(
                "{}\n\n{}",
                "Rozmowa robocza o kolorach interfejsu i układzie przycisków.\n".repeat(240),
                case.answer
            )
        } else if case.context_fixture {
            // Keep the answer in one paragraph but beyond the former 200-char renderer cap.
            format!(
                "{}{}",
                "Neutral context preceding the recorded decision, ".repeat(9),
                case.answer
            )
        } else {
            case.answer.clone()
        };
        let cwd = env!("CARGO_MANIFEST_DIR");
        let rows = [
            serde_json::json!({"timestamp":"2026-10-01T10:00:00Z","type":"session_meta","payload":{"id":sid,"cwd":cwd,"timestamp":"2026-10-01T10:00:00Z"}}),
            serde_json::json!({"timestamp":"2026-10-01T10:00:01Z","type":"turn_context","payload":{"turn_id":"t1","cwd":cwd}}),
            serde_json::json!({"timestamp":"2026-10-01T10:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Please record the chosen behavior."}}),
            serde_json::json!({"timestamp":"2026-10-01T10:00:03Z","type":"event_msg","payload":{"type":"agent_message","message":answer}}),
        ];
        fs::write(
            &path,
            rows.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .unwrap();
        catalog.push_str(&format!(
            "{}\n",
            serde_json::json!({"schema":"aicx.catalog.session.v1","agent":"codex","session_id":sid,
            "source_path":path,"project":"loctree/aicx","cwd":cwd,"date":"2026-10-01"})
        ));
    }
    fs::write(root.join("catalog/sessions.jsonl"), catalog).unwrap();
    let report = aicx::source_index::build(&root, &[], false, false, real_model).unwrap();
    assert_eq!(report.lexical_docs, config.queries.len());
    if real_model {
        assert!(
            report.dense_docs > report.lexical_docs,
            "long-session tail requires multiple passages"
        );
    }
    let filters = aicx::search_engine::SemanticSearchFilters {
        deep: real_model,
        ..Default::default()
    };
    let mut measures = Vec::new();
    let mut failures = Vec::new();
    for (n, case) in config.queries.iter().enumerate() {
        let expected = format!("00000000-0000-4000-8000-{:012}", n + 1);
        let semantic_challenge = real_model && case.semantic_only_query.is_some();
        let query = if real_model {
            case.semantic_only_query.as_deref().unwrap_or(&case.query)
        } else {
            &case.query
        };
        // A lexical-only control must fail to recall the target anywhere in
        // this small corpus. Rank1 in hybrid mode must then come from dense
        // evidence, rather than accidentally passing with a useless model.
        let lexical_control_found_target = semantic_challenge.then(|| {
            let lexical = aicx::search_engine::try_semantic_search_filtered(
                &root,
                query,
                config.queries.len(),
                &[None],
                None,
                None,
                &aicx::search_engine::SemanticSearchFilters::default(),
            )
            .unwrap()
            .outcome;
            let found = lexical
                .results
                .iter()
                .any(|r| r.session_id.as_deref() == Some(expected.as_str()));
            assert!(
                !found,
                "semantic-only fixture unexpectedly has lexical recall"
            );
            found
        });
        let at = Instant::now();
        let outcome = aicx::search_engine::try_semantic_search_filtered(
            &root,
            query,
            5,
            &[None],
            None,
            None,
            &filters,
        )
        .unwrap()
        .outcome;
        assert_eq!(
            outcome.backend_label,
            if real_model {
                "hybrid_rrf"
            } else {
                "lexical_tantivy"
            }
        );
        let status = outcome
            .retrieval_status
            .as_ref()
            .expect("published manifest");
        assert_eq!(status.corpus_freshness, "current");
        let hit = outcome
            .results
            .iter()
            .take(5)
            .enumerate()
            .find(|(_, r)| r.session_id.as_deref() == Some(expected.as_str()));
        let recall = if hit.is_some() { 1.0 } else { 0.0 };
        let evidence_matches =
            hit.is_some_and(|(_, r)| r.matched_lines.join("\n").contains(&case.expected_snippet));
        measures.push(Measurement {
            id: case.id.clone(),
            expected_rank: hit.map(|(n, _)| n + 1),
            recall_at_5: recall,
            latency_ms: at.elapsed().as_millis(),
            evidence_matches,
            semantic_challenge,
            lexical_control_found_target,
        });
        if recall < config.minimum_recall_at_5
            || !evidence_matches
            || (semantic_challenge && hit.is_none_or(|(rank, _)| rank != 0))
        {
            failures.push(case.id.clone());
        }
    }
    let recall = measures.iter().map(|m| m.recall_at_5).sum::<f64>() / measures.len() as f64;
    let mrr = measures
        .iter()
        .map(|m| m.expected_rank.map_or(0.0, |r| 1.0 / r as f64))
        .sum::<f64>()
        / measures.len() as f64;
    let manifest = aicx_retrieve::Manifest::read_from_path(
        &aicx::vector_index::hybrid_manifest_path(None).unwrap(),
    )
    .unwrap();
    let tail = verify_cli_and_mcp(
        &root,
        config.queries.iter().find(|q| q.tail_fixture).unwrap(),
        real_model,
    );
    let lexical_context = verify_cli_and_mcp(
        &root,
        config.queries.iter().find(|q| q.context_fixture).unwrap(),
        false,
    );
    let transports = serde_json::json!({"tail": tail, "lexical_context": lexical_context});
    let receipt = serde_json::json!({"schema":"aicx.retrieval_fixture_result.v1","mode":if real_model {"real_model_hybrid"} else {"portable_lexical"},
        "model":manifest.embedder_model,"manifest_schema":manifest.schema_version,"transports":transports,
        "semantic_challenges":measures.iter().filter(|m| m.semantic_challenge).count(),
        "queries":measures,"recall_at_5":recall,"mrr":mrr,"failures":failures,"lexical_docs":report.lexical_docs,"dense_passages":report.dense_docs});
    println!("{receipt}");
    if let Some(path) = std::env::var_os("AICX_RETRIEVAL_EVAL_REPORT") {
        fs::write(path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    }
    assert!(
        failures.is_empty(),
        "retrieval/evidence acceptance failed for {failures:?}; receipt={receipt}"
    );
}

/// Exercise the actual shipped protocol entry points against the same public
/// source fixture, without a provider census or a production reader restart.
fn verify_cli_and_mcp(root: &std::path::Path, case: &Case, deep: bool) -> serde_json::Value {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::Duration;
    let mut cli = Command::new(env!("CARGO_BIN_EXE_aicx"));
    cli.env("AICX_HOME", root).env("HOME", root).args([
        "search",
        &case.query,
        "--json",
        "--limit",
        "5",
    ]);
    if case.context_fixture {
        cli.arg("--no-semantic");
    }
    let output = cli.output().expect("run fixture CLI search");
    assert!(
        output.status.success(),
        "CLI search failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli_result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("CLI JSON");
    assert!(
        cli_result.to_string().contains(&case.expected_snippet),
        "CLI must return expected evidence: {cli_result}"
    );
    let expected_path = if deep {
        "hybrid_fusion"
    } else {
        "lexical_only"
    };
    assert_eq!(
        cli_result["oracle_status"]["retrieval"]["executed_path"],
        expected_path
    );
    assert_eq!(cli_result["index_snapshot"]["freshness"], "current");

    struct Server(std::process::Child);
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_aicx-mcp"))
            .env("AICX_HOME", root)
            .env("HOME", root)
            .args(["--transport", "stdio", "--no-auto-refresh"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(root.join("mcp-stderr.log")).unwrap())
            .spawn()
            .expect("start isolated MCP reader"),
    );
    let mut stdin = server.0.stdin.take().unwrap();
    let stdout = server.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
                && tx.send(value).is_err()
            {
                break;
            }
        }
    });
    let request =
        |stdin: &mut std::process::ChildStdin, id: u64, method: &str, params: serde_json::Value| {
            writeln!(
                stdin,
                "{}",
                serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
            )
            .unwrap();
            stdin.flush().unwrap();
            let deadline = Instant::now() + Duration::from_secs(90);
            loop {
                let response = rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .unwrap_or_else(|e| {
                        panic!(
                            "MCP {method} response within 90 seconds: {e}; stderr={}",
                            fs::read_to_string(root.join("mcp-stderr.log")).unwrap_or_default()
                        )
                    });
                if response["id"] == id {
                    assert!(response.get("error").is_none(), "MCP error: {response}");
                    return response["result"].clone();
                }
            }
        };
    request(
        &mut stdin,
        1,
        "initialize",
        serde_json::json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"retrieval-fixture","version":"1"}}),
    );
    writeln!(
        stdin,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    stdin.flush().unwrap();
    let tools = request(&mut stdin, 2, "tools/list", serde_json::json!({}));
    let search = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "aicx_search")
        .unwrap();
    assert_eq!(
        search["inputSchema"]["properties"]["deep"]["type"],
        "boolean"
    );
    let mut args = serde_json::json!({"query":case.query,"strict_semantic":true,"slim":false,"verbose":true,"limit":5});
    if case.context_fixture {
        args["deep"] = false.into();
    }
    let result = request(
        &mut stdin,
        3,
        "tools/call",
        serde_json::json!({"name":"aicx_search","arguments":args}),
    );
    assert_ne!(result["isError"], true, "MCP tool failure: {result}");
    let payload: serde_json::Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap())
            .expect("MCP search JSON");
    assert!(
        payload.to_string().contains(&case.expected_snippet),
        "MCP must return expected evidence: {payload}"
    );
    assert_eq!(
        payload["oracle_status"]["retrieval"]["executed_path"],
        expected_path
    );
    assert_eq!(payload["index_snapshot"]["freshness"], "current");
    serde_json::json!({"cli":"passed","mcp_stdio":"passed","mcp_deep_property":true,"expected_evidence":true})
}
