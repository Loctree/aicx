# Publishing Guide - aicx npm packages

> Status: aligned to the current `*-slim-unsigned.tar.gz` GitHub Release asset
> shape for macOS arm64 and Linux x64 GNU. Publish only after the matching
> release assets and `.sha256` sidecars exist for the target version.

This guide describes the publish flow for the single wrapper package and its
platform sub-packages under the `@loctree` npm scope.

## Architecture

One wrapper, two active platform packages.

The wrapper publishes two commands:

- `aicx`
- `aicx-mcp`

Platform packages install the matching release asset from
`https://github.com/Loctree/aicx/releases`.

| Wrapper | `bin` entries | Platform package pattern | Release repo |
| --- | --- | --- | --- |
| `@loctree/aicx` | `aicx`, `aicx-mcp` | `@loctree/aicx-{platform}` | `Loctree/aicx` |

Platform matrix:

- `darwin-arm64`
- `linux-x64-gnu`

Each platform package downloads:

- the release archive
- the adjacent `.sha256`

Then it:

- verifies SHA-256
- extracts the archive
- copies `aicx` and `aicx-mcp` into the package directory

## Prerequisites

1. `@loctree` npm org exists and you have publish rights.
2. GitHub releases exist for the target version with the asset names expected
   by the platform packages:
   - `aicx-v{V}-aarch64-apple-darwin-slim-unsigned.tar.gz`
   - `aicx-v{V}-x86_64-unknown-linux-gnu-slim-unsigned.tar.gz`
3. Each asset has an adjacent `.sha256`.
4. Node.js 14+.

## Publish flow

### Step 1 - Sync versions

```bash
node distribution/npm/sync-version.mjs 0.7.3
node distribution/npm/sync-version.mjs --check 0.7.3
node distribution/npm/verify-metadata.mjs 0.7.3
```

### Step 2 - Publish platform packages first

```bash
for plat in darwin-arm64 linux-x64-gnu; do
  (cd distribution/npm/aicx/platform-packages/$plat && npm publish --access public)
done
```

### Step 3 - Wait for npm registry propagation

```bash
sleep 30
```

### Step 4 - Publish the wrapper

```bash
(cd distribution/npm/aicx && npm publish --access public)
```

### Step 5 - Verify

```bash
mkdir -p /tmp/aicx-npm-verify && cd /tmp/aicx-npm-verify
npm init -y >/dev/null
npm install @loctree/aicx
npx aicx --version
npx aicx-mcp --version
```

## GitHub Actions path

The repo also includes a manual workflow:

- `.github/workflows/npm-publish.yml`

Run it with a concrete `x.y.z` version after the matching GitHub Release assets
exist. It publishes platform packages first, waits for registry propagation,
then publishes the wrapper.

## Troubleshooting

### "Platform package not found"

- Platform packages must be published before the wrapper.
- Wait 30-60 seconds after the platform publish for npm registry propagation.
- Verify names exactly match the wrapper `optionalDependencies`.

### Binary download failures

- Verify the GitHub release exists at the correct tag (`v{VERSION}` with the `v` prefix).
- Verify the release assets and `.sha256` files exist.
- Test download manually:

Real asset shape:

```bash
curl -LI https://github.com/Loctree/aicx/releases/download/v0.7.3/aicx-v0.7.3-aarch64-apple-darwin-slim-unsigned.tar.gz
curl -LI https://github.com/Loctree/aicx/releases/download/v0.7.3/aicx-v0.7.3-aarch64-apple-darwin-slim-unsigned.tar.gz.sha256
curl -LI https://github.com/Loctree/aicx/releases/download/v0.7.3/aicx-v0.7.3-x86_64-unknown-linux-gnu-slim-unsigned.tar.gz
curl -LI https://github.com/Loctree/aicx/releases/download/v0.7.3/aicx-v0.7.3-x86_64-unknown-linux-gnu-slim-unsigned.tar.gz.sha256
```

### optionalDependencies disabled

- Some CI / package manager configs disable optional deps.
- Check `.npmrc` / `.yarnrc` for `optional=false` or `--ignore-optional`.
