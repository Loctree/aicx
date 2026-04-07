# Releases and Distribution

`ai-contexters` now ships through two repo-owned channels:

1. Source install from a local checkout or accessible git remote.
2. GitHub Releases with prebuilt archives for users who do not want a Rust toolchain.

This document is the maintainer path from green CI to public release artifacts.

## Current Shape

- Public install paths now exist through crates.io, GitHub Releases, and source checkout.
- `install.sh` prefers a local checkout when one exists and otherwise installs from crates.io.
- Extracted macOS/Linux release bundles now include `install.sh`, which copies the bundled binaries into `~/.local/bin` by default (or `AICX_BIN_DIR`) and configures MCP with an absolute `aicx-mcp` path.
- `AICX_INSTALL_MODE=git` remains available for testing unreleased source directly from GitHub.
- `aicx` is now the guided operator front door, while `aicx doctor` remains the deeper smoke test for sources, canonical store activity, and daemon readiness.
- `presence/` holds the repo-local one-pager used for demos, packaging, and public-facing explanation.

## What the Release Workflow Produces

Tagging `vX.Y.Z` triggers `.github/workflows/release.yml`, which:

- verifies the tag matches `Cargo.toml`
- reruns the required release gates: `semgrep`, `cargo clippy --all-features --all-targets -- -D warnings`, targeted `cargo check`/`cargo test` passes for `aicx-parser`, `aicx-memex`, and shipped root binaries, `cargo fmt -- --check`, and `cargo publish --dry-run` for all three workspace packages
- builds all shipped binaries: `aicx`, `aicx-mcp`, and `aicx-memex`
- packages archives plus `LICENSE`, `README.md`, `install.sh`, bundled docs, and the repo-local `presence/` one-pager
- uploads SHA-256 checksum files alongside each archive
- creates or updates the matching GitHub Release

Current binary targets:

- `x86_64-unknown-linux-musl`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-pc-windows-msvc`

Archive naming is deterministic:

- `ai-contexters-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz`
- `ai-contexters-vX.Y.Z-x86_64-apple-darwin.tar.gz`
- `ai-contexters-vX.Y.Z-aarch64-apple-darwin.tar.gz`
- `ai-contexters-vX.Y.Z-x86_64-pc-windows-msvc.zip`

Each archive contains:

- `aicx`
- `aicx-mcp`
- `aicx-memex`
- `install.sh`
- `LICENSE`
- `README.md`
- `docs/COMMANDS.md`
- `docs/RELEASES.md`
- `presence/index.html`
- `presence/app.js`
- `presence/styles.css`

## Maintainer Release Flow

1. Update `Cargo.toml` version and `CHANGELOG.md`.
2. Merge to `main` only after CI is green and the product surface is honest.
3. Create an annotated tag that matches the crate version.

```bash
git tag -a v0.4.3 -m "ai-contexters v0.4.3"
git push origin v0.4.3
```

4. Wait for the `Release` workflow to finish and confirm the GitHub Release has all archives and `.sha256` files.
5. Smoke-test one archive on macOS or Linux before announcing it publicly:

```bash
tar -xzf ai-contexters-vX.Y.Z-<target>.tar.gz
cd ai-contexters-vX.Y.Z-<target>
./install.sh
```

Confirm that:
- binaries land in `~/.local/bin` (or `AICX_BIN_DIR`)
- `aicx` shows the guided front door and suggests the next commands without extra setup
- `aicx doctor` runs cleanly after install
- `aicx dashboard --open` opens a local browser snapshot without extra setup
- MCP config points at the absolute bundled `aicx-mcp` path
- `aicx read <ref-or-path>` can open a chunk returned by `aicx search` or `aicx steer`
- `presence/index.html` opens locally and links to the bundled docs

## Publish-Ready Crate Flow

The repo is configured so `cargo publish --dry-run` is part of CI and release verification for all three workspace packages. When crates.io publication becomes part of the release lane, publish in dependency order:

```bash
cargo publish -p aicx-parser
cargo publish -p aicx-memex
cargo publish -p ai-contexters
```

Keep crates.io publication manual until the team is ready to store `CRATES_IO_API_TOKEN` in repository secrets and automate that final step.

## Recovery and Reruns

- To rebuild a release for an existing tag, rerun the failed workflow or use `workflow_dispatch` with the same `vX.Y.Z` tag.
- If the tag does not match `Cargo.toml`, the workflow fails before any binaries are published.
- If any package-level `cargo publish --dry-run` fails, treat that as a publish-surface regression even if normal CI is green.
