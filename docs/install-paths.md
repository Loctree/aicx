# AICX Install Paths

This document maps the supported binary install channels and the shadow checks
that keep `PATH` from resolving an older `aicx`.

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

Shell installers run a preflight inventory:

```bash
which -a aicx
```

Each path is probed with `--version` when executable. If multiple binaries are
visible, or the current `PATH` winner is not the target install path, interactive
installs ask for confirmation. Automation can set:

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

After shell installs, `install.sh` compares:

```bash
<target>/aicx --version
command -v aicx
aicx --version
```

If the versions or paths differ, the installer prints a warning with the target
path, the `PATH`-resolved path, and all other `which -a aicx` entries. The fix is
to move the target directory earlier in `PATH` or remove the older channel.

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
