# Releases and Distribution

GitHub Releases are the user-facing binary release lane for `aicx`. The source
registry, npm wrapper, and Homebrew are secondary distribution surfaces; none
of them replaces the signed GitHub asset set.

`Cargo.toml` owns the semantic version. `CHANGELOG.md` owns release notes.
`tools/release_sync.py` keeps versioned install and npm examples aligned.

## Canonical Asset Set

Tagging `vX.Y.Z` runs `.github/workflows/release.yml`. The workflow verifies
the tag/version contract and release gates, then publishes:

| Platform | Release asset | Platform proof |
| --- | --- | --- |
| macOS arm64 | `aicx-vX.Y.Z-aarch64-apple-darwin-slim.zip` | Developer ID codesign + Apple notarization |
| Linux x64 GNU | `aicx-vX.Y.Z-x86_64-linux-gnu-slim.tar.gz` | GPG detached signature |
| Windows x64 | `aicx-vX.Y.Z-x86_64-pc-windows-msvc-slim.zip` | GPG detached signature |
| Debian x64 | `aicx_X.Y.Z-1_amd64.deb` | GPG detached signature |

Every archive and Debian package has an adjacent `.sha256` and `.asc`.
`loctree-release-pubkey.asc` is published with the release. The macOS job also
publishes its notarization record.

Each bundle contains:

- `aicx`
- `aicx-mcp`
- `install.sh`
- `LICENSE`
- `README.md`
- `docs/COMMANDS.md`
- `docs/RELEASES.md`
- `release-manifest.json`

The Windows zip is portable and intentionally omits the Unix-only
`install.sh`; users invoke `aicx.exe` and `aicx-mcp.exe` directly.

The release workflow builds exactly three binary targets:

- `aarch64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`

The signed Windows leg runs on the operator-owned `windows-pc` runner because
that host owns the release GPG key. Cargo is serialized there with
`CARGO_BUILD_JOBS=1`, and that target overrides full cross-crate LTO with
`CARGO_PROFILE_RELEASE_LTO=false`. Serialization prevents concurrent compiler
pressure; disabling LTO prevents the final single `rustc` link from exhausting
the host by itself. Release `opt-level=3` and stripping remain enabled. The
hosted Windows merge gate remains the clean-machine proof that the same MSVC
source and bundle path compile independently of the signing host.

Do not revive the removed unsigned/musl archive lane. Adding a platform means
updating the build matrix, installer, npm metadata checks, documentation, and
cold-install smoke as one release-contract change.

## User Install

Download the installer from the same immutable tag as the desired release:

```bash
curl -fsSLO https://raw.githubusercontent.com/Loctree/aicx/v0.12.0/install.sh
AICX_INSTALL_MODE=release AICX_RELEASE_TAG=v0.13.0 bash install.sh
```

Release mode selects the platform archive, downloads its adjacent `.sha256`,
verifies the digest, and delegates to the installer inside the verified
bundle. It installs `aicx` and `aicx-mcp` into `~/.local/bin`.

For an unreleased checkout, use the local source path:

```bash
cargo install --path . --locked --force --bin aicx --bin aicx-mcp
```

`AICX_INSTALL_MODE=git` exists for maintainer testing of unreleased GitHub
source. It is not proof that a tagged binary release works.

## Maintainer Preparation

Prepare and inspect the release on the candidate branch:

```bash
make release-prepare VERSION=0.12.0
make release-check
git diff --check
```

`make release-check` enforces the version/tag channel, a closed changelog
section, formatting, tests, clippy, builds, and Semgrep. The release is still
blocked if the runtime acceptance criteria or public install path are not
proven, even when this gate is green.

The user-facing release must come from the intended commit on `main`. Verify
the exact commit before tagging:

```bash
git status --short --branch
git rev-parse HEAD
git rev-parse origin/main
git merge-base --is-ancestor origin/main HEAD
```

## Local Signed macOS Candidate

On the signing Mac, run the repository contract from an interactive shell so
the operator keychain profile is visible:

```bash
zsh -ic 'cd /path/to/aicx && make release-bundle KEYS="$HOME/.keys" CLEAN=0'
```

The operator-owned credential directory is `$HOME/.keys`. The bundle script
imports the Developer ID certificate into a temporary keychain, codesigns both
binaries with hardened runtime and timestamping, notarizes and staples the
bundle, writes SHA-256 and GPG detached signatures, and emits
`release-manifest.json`.

The signing session must be an Aqua GUI login:

```bash
launchctl managername
security find-identity -v -p codesigning
```

`launchctl managername` must report `Aqua`, and the codesigning identity must
be resolvable inside that same session. Do not print or copy credential
contents into logs or reports.

Local candidate success is pre-release evidence only. It does not replace a
cold install from the GitHub URL produced by the tag workflow.

## Publish Flow

After review, runtime acceptance, and merge to `main`:

1. Run `make release-check` on the exact commit to tag.
2. Create a GPG-signed annotated tag with `make release-tag`.
3. Push only the tag with `make release-push`.
4. Wait for every `Release` workflow leg.
5. Verify the GitHub Release body matches the `CHANGELOG.md` version section.
6. Verify every archive has `.sha256` and `.asc`, and the public key is present.
7. Run cold-install smoke from the published `vX.Y.Z` URL in a clean temporary
   home.
8. Publish npm only after the exact GitHub assets exist and the cold smoke is
   green.

The equivalent explicit tag commands are:

```bash
git tag -as v0.13.0 -m "Release v0.13.0"
git push origin v0.13.0
```

Do not tag or push from a dirty tree, an unmerged branch, or a commit whose
runtime acceptance is operator-deferred.

## Verification

Verify a downloaded asset without trusting its filename:

```bash
curl -fsSLO https://github.com/Loctree/aicx/releases/download/v0.12.0/loctree-release-pubkey.asc
curl -fsSLO https://github.com/Loctree/aicx/releases/download/v0.12.0/aicx-v0.12.0-aarch64-apple-darwin-slim.zip
curl -fsSLO https://github.com/Loctree/aicx/releases/download/v0.12.0/aicx-v0.12.0-aarch64-apple-darwin-slim.zip.sha256
curl -fsSLO https://github.com/Loctree/aicx/releases/download/v0.12.0/aicx-v0.12.0-aarch64-apple-darwin-slim.zip.asc
shasum -a 256 -c aicx-v0.12.0-aarch64-apple-darwin-slim.zip.sha256
gpg --import loctree-release-pubkey.asc
gpg --verify aicx-v0.12.0-aarch64-apple-darwin-slim.zip.asc \
  aicx-v0.12.0-aarch64-apple-darwin-slim.zip
```

On macOS, also verify the extracted binaries and notarization:

```bash
codesign --verify --deep --strict --verbose=2 aicx
codesign --verify --deep --strict --verbose=2 aicx-mcp
spctl --assess --type execute --verbose=2 aicx
spctl --assess --type execute --verbose=2 aicx-mcp
```

Cold-install smoke must use a fresh temporary directory and the public release
URL, not the working tree or an existing `PATH` binary:

```bash
smoke_home="$(mktemp -d)"
HOME="$smoke_home" AICX_INSTALL_MODE=release AICX_RELEASE_TAG=v0.12.0 bash install.sh
HOME="$smoke_home" "$smoke_home/.local/bin/aicx" --version
HOME="$smoke_home" "$smoke_home/.local/bin/aicx-mcp" --version
```

Verify the version contains the intended release version and provenance. Then
exercise the release's user-visible acceptance queries against an explicitly
prepared test corpus. Do not turn a read-only release check into an implicit
publish of the operator's live index.

## npm

Cold-install smoke waits until the wrapper and every optional native package
at the requested version are visible from the registry. Waiting for the
wrapper alone is insufficient: npm can expose it before a just-published
platform tarball has propagated, causing a transient missing-binary failure on
one runner even though a later install succeeds.

Windows ZIP extraction is pinned to `%SystemRoot%\System32\tar.exe`. An
unqualified `tar` is not a portable Windows contract: Git Bash can put GNU tar
first on `PATH`, and GNU tar does not extract the release ZIP. The installer
validates the system extractor path and invokes it directly without a shell;
PowerShell and CI therefore exercise the same native Windows path.

`distribution/npm/` contains one wrapper plus three platform packages:

- `@loctree/aicx-darwin-arm64`
- `@loctree/aicx-linux-x64-gnu`
- `@loctree/aicx-win32-x64-gnu` (legacy npm suffix; MSVC binary)
- `@loctree/aicx`

Use `.github/workflows/npm-publish.yml` after the matching GitHub Release is
complete. The workflow checks the three release archive names and their
checksums, validates package metadata, publishes platform packages first, then
the wrapper. See `distribution/npm/PUBLISHING.md`.

The crates.io lane is source/library distribution, not the primary user-facing
binary release. `make publish-crates-dry` validates the leaf crates; the full
topological upload is an explicit maintainer action.

## Deployment Surface

The normal deployment is local CLI plus MCP stdio. `aicx serve` can expose
streamable HTTP, but remote exposure is an operator deployment decision:
non-loopback binds require Bearer auth, Host validation must be deliberate,
and TLS/reverse-proxy ownership sits outside the binary release.

Release verification must inventory the actual bind, auth, CORS/Host policy,
secret materialization, observability, and rollback path before treating an
HTTP deployment as public.

## Recovery and Rollback

- A failed workflow is rerun for the same tag only after its cause is fixed.
- A tag/version mismatch fails before publication.
- A bad immutable release is not silently overwritten. Mark it as affected,
  publish a corrected patch release, and keep the provenance trail.
- npm versions are immutable; deprecate or unpublish only within registry
  policy, then publish a corrected patch.
- Keep the previously proven release available until the new cold-install
  smoke and runtime acceptance pass.
- Never delete or rewrite operator indexes as part of release rollback.
