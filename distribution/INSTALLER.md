# Installer Contract

This document is the cross-repo source of truth for direct bundle/release
installers.

## Goals

- install a working product without requiring the repo
- keep the install path informative
- clean up stale binaries
- verify checksums for downloaded release artifacts
- keep local model selection explicit

## Required installer behavior

1. Prefer colocated release bundle binaries when present.
2. Install into a user-local bin directory such as `~/.local/bin`.
3. Remove stale previous copies from known user-local locations.
4. When downloading a release:
   - fetch the release artifact
   - fetch the adjacent `.sha256`
   - verify checksum before extraction
5. Configure MCP/tooling integrations only after binaries are present.
6. Keep local embedder selection separate from core binary installation.

## Shadow installations

Every installer must scan the active `PATH` before writing binaries:

```bash
which -a aicx
aicx --version
```

If multiple `aicx` binaries exist, the installer must print every path and
version it can resolve. Interactive shell installers should ask for confirmation
unless `AICX_INSTALL_FORCE=1` is set. Non-interactive installers should warn
clearly and continue only when their package manager cannot safely prompt.

Cleanup is version-aware:

- bundle installs target `~/.local/bin` and remove older or equal cargo/npm
  shadows
- source installs target cargo's bin directory and remove older or equal
  `~/.local/bin` shadows
- npm installs warn by default and remove older or equal `~/.local/bin` /
  cargo-bin shadows only with `AICX_NPM_REPLACE_LOCAL=1`

After install, shell installers must compare the installed binary with the
binary that `PATH` resolves. If they differ, print both versions and the other
`which -a aicx` entries.

## Embedder picker

If the product has a local embedder:

- the installer may offer an interactive picker
- the picker should write a deterministic config file
- the picker must not silently download a heavy model
- if a model download is offered, it must be explicit and opt-in
- the picker must not rewrite active rmcp/rust-memex provider settings
- heavy retrieval provider settings remain owned by the retrieval engine config, not by the installer picker

Recommended picker outcomes:

- `skip`
- `base`
- `dev`
- `premium`
- `explicit path`

## Messaging contract

The installer should tell the operator:

- what got installed
- where it was installed
- whether another `aicx` shadows the newly installed binary on `PATH`
- whether release checksum verification passed
- whether optional embedder setup was skipped, saved, or hydrated
- what the next command is if hydration was deferred
