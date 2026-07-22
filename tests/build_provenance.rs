#[path = "../build/build_support.rs"]
mod build_support;

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "aicx-runtime-inspect-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create temp directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[test]
fn version_formatter_distinguishes_clean_dirty_and_archive_builds() {
    assert_eq!(
        build_support::format_build_version("0.12.0", "deadbeef", false),
        "0.12.0+gdeadbeef"
    );
    assert_eq!(
        build_support::format_build_version("0.12.0", "deadbeef", true),
        "0.12.0+gdeadbeef.dirty"
    );
    assert_eq!(
        build_support::format_build_version("0.12.0", "unknown", true),
        "0.12.0"
    );
}

#[test]
fn compiled_identity_matches_the_checkout_used_for_this_build() {
    let build_version = env!("AICX_BUILD_VERSION");
    assert!(build_version.starts_with(env!("CARGO_PKG_VERSION")));

    if let Some(head) = git(&["rev-parse", "--short=8", "HEAD"]) {
        assert_eq!(env!("AICX_GIT_COMMIT"), head);
        assert!(build_version.contains(&format!("+g{head}")));

        let dirty = git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
        assert_eq!(env!("AICX_GIT_DIRTY") == "1", dirty);
        assert_eq!(build_version.ends_with(".dirty"), dirty);
    }
}

fn inspect(home: &Path, path: Option<&str>, mcp_config: Option<&Path>) -> serde_json::Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aicx"));
    command
        .args(["config", "inspect", "--json"])
        .env("AICX_HOME", home)
        .env("HOME", home)
        .env_remove("AICX_EMBEDDER_CONFIG");
    if let Some(path) = path {
        command.env("PATH", path);
    }
    if let Some(config) = mcp_config {
        command.arg("--mcp-config").arg(config);
    }
    let output = command.output().expect("run runtime inspection");
    assert!(
        output.status.success(),
        "inspection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("inspection JSON")
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write fake executable");
    let mut permissions = fs::metadata(path)
        .expect("fake executable metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake executable");
}

#[test]
fn runtime_inspection_and_mcp_server_info_share_build_identity() {
    let home = TempDir::new("parity-home");
    let payload = inspect(home.path(), None, None);

    let mut child = Command::new(env!("CARGO_BIN_EXE_aicx-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn MCP server");
    use std::io::Write as _;
    writeln!(
        child.stdin.as_mut().expect("MCP stdin"),
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-03-26","capabilities":{{}},"clientInfo":{{"name":"provenance-test","version":"1"}}}}}}"#
    )
    .expect("write initialize");
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for MCP EOF exit");
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(
        output
            .stdout
            .split(|byte| *byte == b'\n')
            .find(|line| !line.is_empty())
            .expect("MCP initialize response"),
    )
    .expect("MCP initialize JSON");

    assert_eq!(
        payload["runtime"]["build"]["version"],
        response["result"]["serverInfo"]["version"]
    );
    assert_eq!(
        payload["runtime"]["build"]["git_commit"],
        env!("AICX_GIT_COMMIT")
    );
}

#[test]
fn runtime_inspection_reports_stale_path_binary_and_missing_config_target() {
    let home = TempDir::new("drift-home");
    let fake_bin = home.path().join(".local/bin");
    fs::create_dir_all(&fake_bin).expect("create stale local bin");
    write_executable(
        &fake_bin.join("aicx-mcp"),
        "#!/bin/sh\necho 'aicx-mcp 0.1.0+gdeadbeef'\n",
    );
    let config = home.path().join("mcp.json");
    let config_body = r#"{"mcpServers":{"aicx":{"command":"/missing/aicx-mcp"},"aicx-wrapper":{"command":"rust-mux-proxy"},"aicx-secret":{"command":"https://operator:super-secret@example.test/aicx-mcp?token=never-print-me"}}}"#;
    fs::write(&config, config_body).expect("write MCP config");
    let before = fs::metadata(&config)
        .expect("config metadata")
        .modified()
        .ok();
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(fake_bin.clone()).chain(std::env::split_paths(&inherited_path)),
    )
    .expect("compose PATH");

    let payload = inspect(home.path(), path.to_str(), Some(&config));
    let mcp_candidates = payload["installations"]["aicx_mcp"]
        .as_array()
        .expect("MCP candidates");
    assert!(mcp_candidates.iter().any(|candidate| {
        candidate["path"] == fake_bin.join("aicx-mcp").display().to_string()
            && candidate["status"] == "drift"
    }));
    let configured = payload["mcp"]["configured_targets"]
        .as_array()
        .expect("configured targets");
    assert!(configured.iter().any(|target| {
        target["key_path"] == "mcpServers.aicx.command" && target["status"] == "missing"
    }));
    assert!(configured.iter().any(|target| {
        target["key_path"] == "mcpServers.aicx-wrapper.command"
            && target["status"] == "unavailable"
            && target["command"] == "rust-mux-proxy"
    }));
    assert!(configured.iter().any(|target| {
        target["key_path"] == "mcpServers.aicx-secret.command"
            && target["command"] == "<redacted-command>"
    }));
    let rendered = serde_json::to_string(&payload).unwrap();
    assert!(!rendered.contains("super-secret"));
    assert!(!rendered.contains("never-print-me"));
    assert_eq!(fs::read_to_string(&config).unwrap(), config_body);
    assert_eq!(
        fs::metadata(&config)
            .expect("config metadata after")
            .modified()
            .ok(),
        before,
        "inspection must not rewrite the MCP config"
    );
}

#[test]
fn runtime_inspection_redacts_secret_bearing_embedder_url() {
    let home = TempDir::new("redaction-home");
    fs::write(
        home.path().join("config.toml"),
        r#"
[embedder]
backend = "cloud"
[embedder.cloud]
url = "https://operator:super-secret@example.test/v1/embeddings?api_key=never-print-me"
model = "test-model"
api_key_env = "TEST_EMBEDDER_TOKEN"
"#,
    )
    .expect("write embedder config");

    let payload = inspect(home.path(), None, None);
    let rendered = serde_json::to_string(&payload).unwrap();
    assert!(!rendered.contains("super-secret"));
    assert!(!rendered.contains("never-print-me"));
    assert_eq!(
        payload["embedder"]["endpoint_origin"],
        "https://example.test"
    );
    assert_eq!(
        payload["embedder"]["api_key"]["source"],
        "env:TEST_EMBEDDER_TOKEN"
    );
    assert_eq!(payload["embedder"]["api_key"]["present"], false);
}
