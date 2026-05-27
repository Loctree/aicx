# Releases and Distribution

`aicx` now ships through these repo-owned channels:

1. Source install from a local checkout or accessible git remote.
2. GitHub Releases with prebuilt archives for users who do not want a Rust toolchain.
3. npm wrapper distribution under `@loctree/aicx` once platform packages match
   the current GitHub Release asset shape.
4. Homebrew tap packaging once the GitHub Release asset shape is promoted there.

There is also a maintainer-local macOS bundling path for signed production
archives. That is separate from the current public v0.6.5 slim unsigned
GitHub Release assets:

```bash
make release-bundle KEYS=~/.keys
make release-bundle KEYS=~/.keys NATIVE=1
make release-bundle KEYS=~/.keys NOTARY_PROFILE=my-notary-profile
```

This document is the maintainer path from green CI to public release artifacts.

## Current Shape

- The supported public v0.6.5 install path is GitHub Releases plus adjacent
  `.sha256` sidecars.
- Public v0.6.5 archives are slim unsigned `.tar.gz` bundles for macOS arm64,
  Linux x64 GNU, and Linux arm64 GNU.
- Linux cross builds pin `cross-rs` builder images to release `v0.2.5`
  (`0.2.5@sha256:9e5b39c09874bc1816c675ed11afca2c2ed6cee0c4ed2b3c1d5763c346c9ae3f`
  for x86_64 GNU and
  `0.2.5@sha256:702154f52b2d8091671aa2c84d5582d849f949977228c735ff8462f93cc0e1e4`
  for aarch64 musl). Operators manually bump both `Cross.toml` image refs
  when a new `cross-rs` release is worth tracking; automated image updates are
  outside the pass-3 scope.
- The npm surface lives under `distribution/npm/`, but it is not the supported
  v0.6.5 install path until its platform mapping is updated from the older
  zip/musl shape.
- Manual npm publication now has a dedicated workflow at `.github/workflows/npm-publish.yml`.
- `Cargo.toml` is the semantic version source of truth; `tools/release_sync.py` propagates that version into npm manifests and the user-facing install examples.
- `make version` shows the current package/tag state. `make version-patch` and
  `make bump-patch` are operator aliases for `make version-bump VERSION=patch`.
- `CHANGELOG.md` is the release-notes source of truth; the GitHub release workflow now derives its body from the matching version section instead of ad-hoc generated notes.
- `install.sh` prefers a colocated release bundle first, then a local checkout, and otherwise falls back to the published install path.
- `AICX_INSTALL_MODE=git` remains available for testing unreleased source directly from GitHub.

## What the Public Release Asset Set Contains

Tagging `vX.Y.Z` triggers `.github/workflows/release.yml`, which:

- verifies the tag matches `Cargo.toml`
- reruns the required release gates: `semgrep`, default clippy, native GGUF clippy, binary tests, native GGUF tests, and `cargo fmt -- --check`
- builds both shipped binaries: `aicx` and `aicx-mcp`
- builds Linux artifacts on the Linux release runner
- builds macOS artifacts on the macOS release runner
- packages archives plus `LICENSE`, `README.md`, `install.sh`, and command docs
- uploads SHA-256 checksum files alongside each archive
- creates or updates the matching GitHub Release using the current version section from `CHANGELOG.md`
- keeps self-hosted runners lean by not caching `target/` in release jobs and cleaning Cargo build artifacts after packaging/upload

Current binary targets:

- `aarch64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`

Archive naming is deterministic:

- `aicx-vX.Y.Z-aarch64-apple-darwin-slim-unsigned.tar.gz`
- `aicx-vX.Y.Z-x86_64-unknown-linux-gnu-slim-unsigned.tar.gz`
- `aicx-vX.Y.Z-aarch64-unknown-linux-gnu-slim-unsigned.tar.gz`

Each archive contains:

- `aicx`
- `aicx-mcp`
- `install.sh`
- `LICENSE`
- `README.md`
- `docs/COMMANDS.md`
- `docs/RELEASES.md`

### Asset verification

Download `SHA256SUMS` into the same directory as the `.tar.gz` release assets,
then run:

```bash
sha256sum -c SHA256SUMS
```

The command expects `SHA256SUMS` and the referenced archives to be colocated.
It exits non-zero if any archive is missing or does not match the published
checksum.

The maintainer-local macOS signing path expects these operator-owned inputs:

- `MACOS_CERT_P12_BASE64`
- `MACOS_CERT_PASSWORD`
- `MACOS_KEYCHAIN_PASSWORD`
- `MACOS_DEVELOPER_ID_APPLICATION`
- `APPLE_API_KEY_BASE64`
- `APPLE_API_KEY_ID`
- `APPLE_API_ISSUER_ID`

## Local macOS Signed Bundle

For local production-style signed macOS artifacts, use:

```bash
make release-bundle KEYS=/path/to/.keys
make release-bundle KEYS=/path/to/.keys NATIVE=1
```

The target:

- builds `aicx` and `aicx-mcp` for the local Apple target
- builds slim binaries by default, or native GGUF-capable binaries with `NATIVE=1`
- assembles a release bundle in `dist/`
- includes `install.sh` for post-download install into `~/.local/bin`
- imports the signing certificate into a temporary keychain
- signs both binaries with timestamps and hardened runtime
- writes a SHA-256 checksum next to the archive
- cleans `target/<triple>` after the bundle is safely written by default; use `CLEAN=0` if you intentionally want to keep local release artifacts

Expected key layout matches the current daily operator structure under `~/.keys`:

- `signing-identity.txt`
- `Certificates.p12`
- `cert_password.txt`
- `.notary.env`

Optional notarization auth paths, if that maintainer lane is re-enabled:

1. `NOTARY_PROFILE=<keychain-profile>` on the `make` command line.
2. `AICX_NOTARY_PROFILE` in the shell environment.
3. `NOTARY_KEYCHAIN_PROFILE` inside `KEYS/.notary.env`.
4. Fallback to `NOTARY_APPLE_ID`, `NOTARY_TEAM_ID`, and `NOTARY_PASSWORD` from `KEYS/.notary.env`.

Examples:

```bash
make release-bundle KEYS=~/.keys
make release-bundle KEYS=~/.keys NATIVE=1
make release-bundle KEYS=~/.keys NOTARY_PROFILE=vc-notary
make release-bundle KEYS=~/.keys CLEAN=0
AICX_KEYS_DIR=~/.keys AICX_NOTARY_PROFILE=vc-notary make release-bundle
bash install.sh
AICX_INSTALL_MODE=release AICX_RELEASE_TAG=v0.9.1 bash install.sh
```

Notes:

- This target is macOS-only.
- Public v0.6.5 release assets are slim unsigned `.tar.gz` archives. Do not
  describe them as notarized unless the release workflow actually notarizes
  that asset set.
- The target does not print secret values; it only reads the files from the operator-owned keys directory.
- `install.sh` inside the bundle copies binaries into `~/.local/bin` and removes stale user-local / `~/.cargo/bin` copies before configuring MCP.
- That install path does not require Rust or a local memex compile on the target machine.
- `AICX_INSTALL_MODE=release` downloads the official release asset, fetches the adjacent `.sha256`, verifies the checksum, and only then delegates to the bundled installer.
- On macOS, `AICX_INSTALL_MODE=release` expects the
  `aicx-vX.Y.Z-aarch64-apple-darwin-slim-unsigned.tar.gz` asset.

## Maintainer Release Flow

1. Run `make release-prepare VERSION={patch|minor|major|x.y.z}` to bump the version, sync docs + npm surfaces, close `CHANGELOG.md`, preview release notes, and refresh `Cargo.lock` for this crate.
2. Merge to `main` only after CI is green and the product surface is honest.
3. Create an annotated tag that matches the crate version.

```bash
git tag -a v0.9.1 -m "aicx v0.9.1"
git push origin v0.9.1
```

4. Wait for the `Release` workflow to finish and confirm the GitHub Release has all archives, `.sha256` files, and the expected body copied from `CHANGELOG.md`.
5. Smoke-test one archive on macOS or Linux before announcing it publicly.

## Publish Track

The current publish track is binary-first:

1. Git tag triggers GitHub Release assets.
2. macOS arm64 archives are currently slim unsigned `.tar.gz` bundles.
3. Linux GNU archives are built for x64 and arm64.
4. Each archive gets an adjacent `.sha256`.
5. npm platform packages publish only after their platform mapping matches the
   GitHub Release assets.
6. Homebrew should consume the same GitHub Release assets and checksums.

Crates.io publication is intentionally not the active release lane. The root
crate depends on local first-party product crates (`rust-memex` and
`aicx-embeddings`) that are not published to crates.io. Treat `cargo publish` /
`cargo publish --dry-run` failures as expected unless the product decision
changes and all first-party crates get a crates.io-compatible publication plan.

## Recovery and Reruns

- To rebuild a release for an existing tag, rerun the failed workflow or use `workflow_dispatch` with the same `vX.Y.Z` tag.
- If the tag does not match `Cargo.toml`, the workflow fails before any binaries are published.
- If npm publish fails, verify the GitHub Release assets and `.sha256` files exist before republishing the wrapper.
