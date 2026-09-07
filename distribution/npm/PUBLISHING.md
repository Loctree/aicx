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

Publication runs as the last job of `release.yml` (`npm-publish` →
`uses: ./.github/workflows/npm-publish.yml`) once the signed GitHub Release
exists; `workflow_dispatch` on `npm-publish.yml` stays as the manual fallback
and re-run path. The chain is explicit because GitHub never starts workflows
from events created with `GITHUB_TOKEN`, so a `release: published` trigger
cannot see releases that `release.yml` itself creates. For each platform, a
hosted runner:

1. downloads the archive, `.sha256`, `.asc`, and release public key;
2. verifies SHA-256 and the detached GPG signature;
3. extracts exactly one copy of each expected binary and rejects symlinks;
4. stages the pair under the platform package's `bin/` directory;
5. checks suffixes, executable mode, and both `--version` results;
6. inspects `npm pack` contents and uploads the resulting tgz as an attested CI artifact.

Publish jobs consume those immutable tgz artifacts rather than repacking a
checkout. Platform packages publish first; the wrapper publishes after registry
propagation (up to 15 minutes — the ~50 MB platform tarballs have taken more
than 5 minutes to become visible through `npm view`). The workflow never
creates a release, tag, or version bump.

Re-dispatching is safe after a partial run: packaging always runs from the
dispatched workflow revision (only the release assets are tag-addressed), and
every publish step is a no-op when the registry already carries that exact
version, so a retry finishes the packages that are still missing instead of
failing on the ones already published.

## Trusted publishers (OIDC)

The publish jobs carry no npm token. They authenticate with the GitHub OIDC
token (`permissions: id-token: write`, granted on the publish jobs and on the
`npm-publish` caller job in `release.yml`), and npm accepts it only when the
package lists a matching trusted publisher. npm validates the **calling**
workflow file, so every package needs two GitHub Actions publishers on
npmjs.com (Settings → Trusted Publisher), each with `Allow npm publish` on
and no environment name:

| Organization / Repository | Workflow filename | Covers |
| --- | --- | --- |
| `Loctree` / `aicx` | `release.yml` | the automatic chain after a signed Release |
| `Loctree` / `aicx` | `npm-publish.yml` | manual `workflow_dispatch` re-runs |

Repeat for `@loctree/aicx`, `@loctree/aicx-darwin-arm64`,
`@loctree/aicx-linux-x64-gnu`, and `@loctree/aicx-win32-x64-gnu` (eight
entries). From a web-authenticated npm login (`npm login`, 2FA):

```bash
for p in @loctree/aicx @loctree/aicx-darwin-arm64 @loctree/aicx-linux-x64-gnu @loctree/aicx-win32-x64-gnu; do
  npm trust github "$p" --file release.yml     --repo Loctree/aicx --allow-publish --yes
  npm trust github "$p" --file npm-publish.yml --repo Loctree/aicx --allow-publish --yes
done
```

Tokens that bypass 2FA cannot manage trusted publishers (the registry answers
403 on the trust endpoint). After the first OIDC publish succeeds, switch each
package's publishing access to "Require two-factor authentication and disallow
bypass 2fa tokens" — that retires the old `NPM_TOKEN` path for good.

## Local metadata gate

After staging the current host's signed release asset:

```bash
node distribution/npm/sync-version.mjs 0.13.0
node distribution/npm/stage-platform-package.mjs 0.13.0 darwin-arm64 /path/to/release-input
node distribution/npm/verify-metadata.mjs 0.13.0 --platform=darwin-arm64
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
