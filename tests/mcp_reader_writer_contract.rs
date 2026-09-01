use aicx::mcp::McpLifecycleConfig;

#[test]
fn default_mcp_lifecycle_is_reader_only() {
    let lifecycle = McpLifecycleConfig::default();
    assert!(
        lifecycle.auto_refresh_interval.is_none(),
        "default long-lived MCP lifecycle must be reader-only; index maintenance requires explicit ownership"
    );
}

#[test]
fn aicx_serve_requires_explicit_writer_opt_in_in_source_contract() {
    let source = include_str!("../src/main.rs");

    assert!(
        source.contains("experimental_auto_refresh"),
        "aicx serve must expose an explicit embedded-writer opt-in"
    );
    assert!(
        !source.contains("auto_refresh_interval: (!no_auto_refresh)"),
        "aicx serve must not infer writer ownership from the absence of --no-auto-refresh"
    );
}

#[test]
fn http_service_installer_does_not_take_over_index_maintenance() {
    let installer = include_str!("../tools/install-mcp-service.sh");

    assert!(
        !installer.contains("retire_launchd_label \"com.loctree.aicx.reindex\""),
        "HTTP reader installer must not retire the separate maintenance owner"
    );
    assert!(
        !installer.contains("retire_launchd_label \"io.vetcoders.aicx.reindex\""),
        "HTTP reader installer must not retire the legacy maintenance label as a side effect of starting the reader"
    );
    assert!(
        !installer.contains("index refresh: owned by the MCP server"),
        "HTTP reader installer must never claim writer ownership"
    );
    assert!(
        !installer.contains("HTTP server owns bounded catalog/index refresh"),
        "HTTP reader installer must never encode embedded-writer doctrine"
    );
}

#[test]
fn separate_maintenance_process_boundary_remains_available() {
    let maintenance = include_str!("../tools/install-reindex-schedule.sh");

    assert!(maintenance.contains("aicx catalog refresh"));
    assert!(maintenance.contains("aicx index"));
    assert!(maintenance.contains("StartInterval"));
    assert!(maintenance.contains("LowPriorityBackgroundIO"));
}
