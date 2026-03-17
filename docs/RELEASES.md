# Releases and Distribution

`ai-contexters` now ships through two repo-owned channels:

1. Source install from a local checkout or accessible git remote.
2. GitHub Releases with prebuilt archives for users who do not want a Rust toolchain.

This document is the maintainer path from green CI to public release artifacts.

## Current Constraints

- `crates.io` publishing is not live yet.
- Until the repo is public or otherwise accessible, outside users still need a source checkout from a channel they can reach.
- `install.sh` is intentionally source-first today and only uses git install as a fallback when the repo is reachable.

## What the Release Workflow Produces

Tagging `vX.Y.Z` triggers `.github/workflows/release.yml`, which:

- verifies the tag matches `Cargo.toml`
- reruns the required release gates: `semgrep`, `cargo clippy --all-features --all-targets -- -D warnings`, `cargo test --bin aicx`, `cargo test --bin aicx-mcp`, `cargo fmt -- --check`, and `cargo publish --dry-run`
- builds both shipped binaries: `aicx` and `aicx-mcp`
- packages archives plus `LICENSE`, `README.md`, and command docs
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
- `LICENSE`
- `README.md`
- `docs/COMMANDS.md`
- `docs/RELEASES.md`

## Maintainer Release Flow

1. Update `Cargo.toml` version and `CHANGELOG.md`.
2. Merge to `main` only after CI is green and the product surface is honest.
3. Create an annotated tag that matches the crate version.

```bash
git tag -a v0.4.1 -m "ai-contexters v0.4.1"
git push origin v0.4.1
```

4. Wait for the `Release` workflow to finish and confirm the GitHub Release has all archives and `.sha256` files.
5. Smoke-test one archive on macOS or Linux before announcing it publicly.

## Publish-Ready Crate Flow

The repo is configured so `cargo publish --dry-run` is part of CI and release verification. When crates.io publication becomes part of the release lane, a maintainer only has one manual step left:

```bash
cargo publish
```

Keep crates.io publication manual until the team is ready to store `CRATES_IO_API_TOKEN` in repository secrets and automate that final step.

## Recovery and Reruns

- To rebuild a release for an existing tag, rerun the failed workflow or use `workflow_dispatch` with the same `vX.Y.Z` tag.
- If the tag does not match `Cargo.toml`, the workflow fails before any binaries are published.
- If `cargo publish --dry-run` fails, treat that as a publish-surface regression even if normal CI is green.
