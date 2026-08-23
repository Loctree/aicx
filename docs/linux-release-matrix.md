# Linux Release Matrix

The signed GitHub Release workflow is the only production Linux artifact
owner. Local cross-compilation may be useful for development, but it must not
create a competing unsigned release lane.

## Supported Release Target

| Target | Support | Artifact |
| --- | --- | --- |
| `x86_64-unknown-linux-gnu` | First-class | `aicx-vX.Y.Z-x86_64-linux-gnu-slim.tar.gz` |

The archive includes `aicx`, `aicx-mcp`, the installer, license, release
manifest, and command/release documentation. It is published with adjacent
`.sha256` and `.asc` files. The release also publishes
`loctree-release-pubkey.asc`.

Linux arm64 and musl are not in the current public matrix. They must not be
advertised until the build workflow, installer, npm mapping, checksums,
signatures, and cold-install smoke all support them.

## Production Build

Pushing a valid `vX.Y.Z` tag runs `.github/workflows/release.yml`. Its Linux
leg executes the repository bundle contract on the `ops-linux` runner:

```bash
TARGET=x86_64-unknown-linux-gnu \
  AICX_RELEASE_BUNDLE_ONLY_BINARIES=1 \
  AICX_BUNDLE_FLAVOR=slim \
  AICX_CARGO_BUILD_CMD="cargo zigbuild" \
  AICX_BUILD_TARGET=x86_64-unknown-linux-gnu.2.28 \
  ./tools/release_bundle.sh
```

The script normalizes the target name in the public asset to
`x86_64-linux-gnu`, writes SHA-256 and GPG detached signatures, and refuses to
publish when the signing key or passphrase material is unavailable.
`cargo zigbuild` links against the declared glibc 2.28 floor; before upload,
the workflow extracts both binaries, runs their version commands, and rejects
any imported `GLIBC_*` symbol newer than 2.28.

The same workflow produces a Debian package and signs it independently.

## Local Verification

Build the supported Linux target on a compatible host:

```bash
cargo zigbuild --locked --release --target x86_64-unknown-linux-gnu.2.28 \
  --bin aicx --bin aicx-mcp
```

Install the pinned release tool first with
`python3 -m pip install cargo-zigbuild==0.23.0`; that package also supplies the
Zig toolchain used by `cargo zigbuild`.

For a production-shaped binary-only bundle, use the same contract as CI with
the operator signing environment available:

```bash
make release-bundle-only-binaries \
  TARGET=x86_64-unknown-linux-gnu \
  CLEAN=0
```

Do not describe an ad-hoc `cargo` or `cross` archive as a release artifact.
Only the tag workflow produces the public asset set and outward provenance.

## Cold-Install Gate

After the GitHub Release is public, download `install.sh` from the immutable
tag and install into a clean temporary home:

```bash
smoke_home="$(mktemp -d)"
curl -fsSLO https://raw.githubusercontent.com/Loctree/aicx/v0.12.3/install.sh
HOME="$smoke_home" AICX_INSTALL_MODE=release AICX_RELEASE_TAG=v0.12.3 bash install.sh
HOME="$smoke_home" "$smoke_home/.local/bin/aicx" --version
HOME="$smoke_home" "$smoke_home/.local/bin/aicx-mcp" --version
```

The test must run from the published URL, not `target/`, `dist/`, or an
existing `PATH` binary.
