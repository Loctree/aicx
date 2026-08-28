# Publishing Guide - aicx npm packages

The npm surface is one script-free wrapper plus three script-free platform
packages. Every platform tgz contains `aicx` and `aicx-mcp` (`.exe` on
Windows); installation never downloads or extracts a GitHub Release asset.

## Package matrix

| Platform package | Source release asset | Payload |
| --- | --- | --- |
| `@loctree/aicx-darwin-arm64` | `aicx-v{V}-aarch64-apple-darwin-slim.zip` | `bin/aicx`, `bin/aicx-mcp` |
| `@loctree/aicx-linux-x64-gnu` | `aicx-v{V}-x86_64-linux-gnu-slim.tar.gz` | `bin/aicx`, `bin/aicx-mcp` |
| `@loctree/aicx-win32-x64-gnu` | `aicx-v{V}-x86_64-pc-windows-msvc-slim.zip` | `bin/aicx.exe`, `bin/aicx-mcp.exe` |

The legacy Windows npm suffix remains `gnu`, but its fenced package carries the
MSVC release binaries.

## Publish contract

Publication is available only through the manual `npm-publish.yml` operator
button after the matching signed GitHub Release exists. For each platform, a
native runner:

1. downloads the archive, `.sha256`, `.asc`, and release public key;
2. verifies SHA-256 and the detached GPG signature;
3. extracts exactly one copy of each expected binary and rejects symlinks;
4. stages the pair under the platform package's `bin/` directory;
5. checks suffixes, executable mode, and both `--version` results;
6. inspects `npm pack` contents and uploads the resulting tgz as an attested CI artifact.

Publish jobs consume those immutable tgz artifacts rather than repacking a
checkout. Platform packages publish first; the wrapper publishes after registry
propagation. The workflow never creates a release, tag, or version bump.

## Local metadata gate

After staging the current host's signed release asset:

```bash
node distribution/npm/sync-version.mjs 0.12.6
node distribution/npm/stage-platform-package.mjs 0.12.6 darwin-arm64 /path/to/release-input
node distribution/npm/verify-metadata.mjs 0.12.6 --platform=darwin-arm64
```

Use the matching platform key on Linux or Windows. The verifier requires zero
`preinstall`, `install`, `postinstall`, and `prepare` scripts in every manifest,
checks the exact pack list, and refuses cross-platform version attestation.

## Cold-install acceptance

After publication, the workflow runs six isolated npm 11.17.0 jobs: normal and
`--ignore-scripts` installs on macOS arm64, Linux x64 GNU, and real Windows x64.
Each job uses a fresh prefix and cache, rejects any `allow-scripts` text in the
verbose log, runs both version commands, and executes `aicx config inspect --json`.

This migration intentionally makes platform tgz files much larger: they now
carry the product binaries instead of a downloader. Record the exact byte sizes
from the workflow job summaries in the release PR before pressing the publish
button.
