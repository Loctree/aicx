use aicx_parser::engine::{
    AgentKind, ParserEngine, SourceArtifact, SourceFraming, SourceHandle, ValidatedParse,
    canonical_bytes,
};
use std::path::PathBuf;
use std::time::Instant;

#[test]
#[ignore = "invoked by tools/bench_single_session.sh --engine-only"]
fn engine_only_benchmark() {
    let path = PathBuf::from(std::env::var("AICX_BENCH_SESSION").expect("AICX_BENCH_SESSION"));
    let threshold_ms: u128 = std::env::var("AICX_BENCH_THRESHOLD_MS")
        .expect("AICX_BENCH_THRESHOLD_MS")
        .parse()
        .expect("numeric threshold");
    let selected_bytes = path.metadata().expect("benchmark metadata").len();
    let artifact = SourceArtifact::validated_file("rollout.jsonl", &path, SourceFraming::JsonLines)
        .expect("validated benchmark file");
    let source_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("private-codex-rollout")
        .chars()
        .filter(|character| !character.is_control() && *character != '/' && *character != '\\')
        .take(500)
        .collect::<String>();
    let source = SourceHandle::new(AgentKind::Codex, source_id, None, vec![artifact])
        .expect("benchmark source");
    let engine = ParserEngine::default();
    let mut runs = Vec::new();
    for run in 1..=2 {
        let started = Instant::now();
        let parsed = engine.parse_registered(&source).expect("engine parse");
        let ValidatedParse::Session(session) = parsed else {
            panic!("benchmark source parsed fatal")
        };
        let canonical = canonical_bytes(&session).expect("canonical validation projection");
        let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
        assert!(
            elapsed_ms <= threshold_ms as f64,
            "engine-only run {run} exceeded hard threshold: {elapsed_ms:.3} > {threshold_ms} ms"
        );
        runs.push(serde_json::json!({
            "run": run,
            "parse_validate_projection_ms": elapsed_ms,
            "opened_source_files": 1,
            "opened_source_bytes": selected_bytes,
            "canonical_bytes": canonical.len(),
        }));
    }
    let result = serde_json::json!({
        "schema": "aicx.parser_engine_benchmark.v1",
        "mode": "engine_only",
        "profile": "release",
        "input": path,
        "input_copied_to_repo": false,
        "hard_threshold_ms": threshold_ms,
        "runs": runs,
        "result": "pass",
    });
    println!("AICX_BENCH_JSON={result}");
}
