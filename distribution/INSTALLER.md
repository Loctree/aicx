# Installer Contract

This document is the cross-repo source of truth for direct bundle/release
installers.

## Goals

- install a working product without requiring the repo
- keep the install path informative
- clean up stale binaries
- verify checksums for downloaded release artifacts
- keep optional heavy model selection explicit

## Required installer behavior

1. Prefer colocated release bundle binaries when present.
2. Install into a user-local bin directory such as `~/.local/bin`.
3. Remove stale previous copies from known user-local locations.
4. When downloading a release:
   - fetch the release artifact
   - fetch the adjacent `.sha256`
   - verify checksum before extraction
5. Configure MCP/tooling integrations only after binaries are present.
6. Keep optional embedder selection separate from core binary installation.

## Optional embedder picker

If the product has an optional local embedder:

- the installer may offer an interactive picker
- the picker should write a deterministic config file
- the picker must not silently download a heavy model
- if a model download is offered, it must be explicit and opt-in

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
- whether release checksum verification passed
- whether optional embedder setup was skipped, saved, or hydrated
- what the next command is if hydration was deferred
