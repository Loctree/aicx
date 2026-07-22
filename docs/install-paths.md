# AICX Install Paths

This document maps the supported binary install channels and the shadow checks
that keep `PATH` from resolving older `aicx` or `aicx-mcp` binaries.

## Install Surface

```text
release bundle / install.sh
  -> ~/.local/bin/aicx
  -> ~/.local/bin/aicx-mcp

local source / install.sh
  -> ${CARGO_INSTALL_ROOT:-~/.cargo}/bin/aicx
  -> ${CARGO_INSTALL_ROOT:-~/.cargo}/bin/aicx-mcp

make install-bin
  -> ${CARGO_INSTALL_ROOT:-~/.cargo}/bin/aicx
  -> ${CARGO_INSTALL_ROOT:-~/.cargo}/bin/aicx-mcp

npm @loctree/aicx
  -> npm global bin shim for aicx
  -> npm global bin shim for aicx-mcp
  -> node_modules/@loctree/aicx-<platform>/aicx
  -> node_modules/@loctree/aicx-<platform>/aicx-mcp
```

## Channels

| Channel | Trigger | Target | Shadow behavior |
|---|---|---|---|
| Release bundle | `bash install.sh` from a release archive | `~/.local/bin` | removes older/equal cargo-bin and npm-bin shadows |
| Release download | `AICX_INSTALL_MODE=release bash install.sh` | `~/.local/bin` | delegates to bundle mode after checksum verification |
| Local source | `bash install.sh` in a checkout | cargo bin dir | removes older/equal `~/.local/bin` and npm-bin shadows |
| Make source | `make install-bin` | cargo bin dir | runs the same shell shadow check before `cargo install` and PATH check after |
| npm package | `npm install -g @loctree/aicx` | npm global package/bin surface | warns by default; removes older/equal local/cargo shadows only with `AICX_NPM_REPLACE_LOCAL=1` |

## Pre-install Scan

Shell installers run a preflight inventory for both binaries:

```bash
which -a aicx
which -a aicx-mcp
```

Each path is probed with `--version` when executable. If multiple binaries are
visible, or the current `PATH` winner is not the target install path,
interactive installs ask for confirmation. Automation can set:

```bash
AICX_INSTALL_FORCE=1 bash install.sh
```

Dry-run mode is available for audits:

```bash
bash install.sh --dry-run
AICX_INSTALL_MODE=local bash install.sh --dry-run
```

## Cleanup Rules

Cleanup is conservative and version-aware:

1. The candidate path must contain `aicx`.
2. The candidate and target versions must parse as `major.minor.patch`.
3. The candidate version must be older than or equal to the target version.
4. `aicx` and `aicx-mcp` are removed together from that directory.

Newer shadows are retained and reported. Unknown-version shadows are retained
because the installer cannot prove that removal is safe.

## Post-install Sanity

After shell installs, `install.sh` compares both runtime entry points:

```bash
<target>/aicx --version
command -v aicx
aicx --version
<target>/aicx-mcp --version
command -v aicx-mcp
aicx-mcp --version
```

If the versions or paths differ, the installer prints a warning with the target
path, the `PATH`-resolved path, and all other `which -a` entries for that
binary. The fix is to move the target directory earlier in `PATH` or remove the
older channel.

## MCP Runtime Drift

The CLI and MCP server are shipped as one versioned pair. Any long-running MCP
service, launchd unit, systemd unit, or hand-written wrapper must point at the
same installed `aicx-mcp` that the installer verifies on `PATH`. A copied
service-local binary can keep answering MCP health checks while running older
search behavior.

`aicx doctor` reports this drift class directly: the `binary_pair` check
compares the running CLI against the `aicx-mcp` resolved on `PATH`, `aicx_home`
shows which `AICX_HOME` was resolved (and whether `store/` + `indexed/` live
there), and `http_auth_token` shows where the HTTP token resolves from without
printing its value.

Minimum parity smoke after updating a service:

```bash
aicx --version
aicx-mcp --version
aicx-mcp --help | grep -- "--host"
aicx doctor --format json   # inspect binary_pair / aicx_home / http_auth_token
aicx index status --json
aicx search "operator decision" --json --limit 1
```

For streamable HTTP MCP, also call `aicx_index_status` and `aicx_search` through
the client transport and compare the reported `indexed_count`, `readiness`, and
`oracle_status.backend` with the CLI output. `aicx_search` should report
`hybrid_rrf` when the same CLI search does; if it reports `content_semantic` or
silent zero results while CLI returns `hybrid_rrf`, the service is stale or
running a different binary/config.

Remote MCP services that bind outside loopback also need HTTP `Host` validation
configured for the client-facing hostname/IP:

```bash
aicx-mcp --transport http --host 0.0.0.0 --allowed-host mcp.example.internal --port 8044
```

Without a matching `--allowed-host`, rmcp rejects the request before MCP tools
run and the client sees `403 Forbidden: Host header is not allowed`.

## Machine-readable runtime inspection

Run the checkout binary you intend to make authoritative:

```bash
cargo run --locked --bin aicx -- config inspect --json
```

The stable `aicx.runtime_inspection.v1` object reports, without changing any
configuration:

- the exact running executable plus semver, checkout SHA, and dirty state;
- resolved `AICX_HOME`, canonical/effective config paths, and config source;
- every visible checkout, Cargo, local, and npm/PATH `aicx`/`aicx-mcp`
  candidate with its resolved path, version, channel, and `match`/`drift` status;
- the configured embedder backend/model/dimension and a credential-free endpoint
  origin (userinfo, path, query, fragment, headers, and key values are omitted);
- the `_all` index generation and manifest embedder identity via bounded reads of
  `CURRENT` and `manifest.json` only—no corpus or store scan.

To compare an MCP client or mux target, pass its JSON or TOML config explicitly:

```bash
aicx config inspect --json --mcp-config ~/.config/rmcp-mux/config.toml
aicx config inspect --json --mcp-config ~/.codex/config.toml
```

Repeat `--mcp-config` for multiple clients. The inspector reports a missing,
unreadable, stale, or matching configured executable and an operator action; it
never rewrites the file. Wrapper commands that do not expose an identifiable
`aicx-mcp` executable remain `unavailable` rather than being guessed.

For mechanical channel comparison, run the same command through each entrypoint
and compare `.runtime.build.version` and `.runtime.executable_path`:

```bash
./target/debug/aicx config inspect --json
~/.cargo/bin/aicx config inspect --json
~/.local/bin/aicx config inspect --json
$(command -v aicx) config inspect --json
```

An npm global shim appears as channel `npm` when its visible path contains the
npm package surface. MCP `initialize.result.serverInfo.version` is sourced from
the same build identity as `.runtime.build.version`; disagreement means the MCP
response came from a different running artifact or service process.

## npm Opt-in Replacement

npm postinstall cannot safely prompt, so it warns by default. To let npm remove
older or equal local shadows during installation:

```bash
AICX_NPM_REPLACE_LOCAL=1 npm install -g @loctree/aicx
```

The npm cleanup only targets:

- `~/.local/bin/aicx`
- `~/.local/bin/aicx-mcp`
- `~/.cargo/bin/aicx`
- `~/.cargo/bin/aicx-mcp`

It does not uninstall other npm packages or rewrite shell startup files.

## Release Version Gate

Before release, run:

```bash
bash tools/release-channel-check.sh
```

The script compares `Cargo.toml`, the root npm package, all platform package
versions, and root npm optional dependency pins. Release CI runs the same check
before publishing artifacts.
