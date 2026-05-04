use aicx::mcp::{IntentsParams, RankParams, ReadParams, SearchParams, SteerParams};

#[test]
fn test_mcp_slim_defaults() {
    let params: SearchParams = serde_json::from_str(r#"{"query": "test"}"#).unwrap();
    assert_eq!(params.limit, 20);
    assert!(params.slim);
    assert!(!params.verbose);

    let params: RankParams = serde_json::from_str(r#"{"project": "test"}"#).unwrap();
    assert_eq!(params.limit, 20);
    assert!(params.slim);
    assert!(!params.verbose);

    let params: SteerParams = serde_json::from_str(r#"{}"#).unwrap();
    assert_eq!(params.limit, 20);
    assert!(params.slim);
    assert!(!params.verbose);

    let params: IntentsParams = serde_json::from_str(r#"{}"#).unwrap();
    assert_eq!(params.limit, 20);
    assert_eq!(params.emit, "markdown");
    assert!(params.slim);
    assert!(!params.verbose);

    let params: ReadParams =
        serde_json::from_str(r#"{"reference":"store/VetCoders/aicx/chunk.md"}"#).unwrap();
    assert_eq!(params.reference, "store/VetCoders/aicx/chunk.md");
    assert!(params.max_chars.is_none());
}
