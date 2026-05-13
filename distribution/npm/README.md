# distribution/npm - aicx npm release surface

This directory is the planned npm distribution surface for `aicx`.
The source of truth for the product lives in
[Loctree/aicx](https://github.com/Loctree/aicx); this folder contains only the
thin JS wrapper and platform-package manifests that ship to the `@loctree` npm
scope.

> Status: aligned to the current `*-slim-unsigned.tar.gz` GitHub Release asset
> shape for macOS arm64 and Linux x64 GNU. Do not publish npm packages until the
> matching release assets and `.sha256` sidecars exist for the target version.

## Wrapper package

| Package | Binaries | Purpose | Release repo |
| --- | --- | --- | --- |
| `@loctree/aicx` | `aicx`, `aicx-mcp` | CLI + MCP server | `Loctree/aicx` |

The wrapper declares active platform sub-packages as `optionalDependencies`
(esbuild/swc pattern).

Current platform matrix:

- `darwin-arm64`
- `linux-x64-gnu`

Total: **1 wrapper + 2 active platform packages = 3 npm packages.**

## Install

```bash
npm install -g @loctree/aicx
```

Then:

```bash
aicx --help
aicx-mcp --version
```

That install surface is intentionally binary-only:

- no repo checkout
- no Rust toolchain
- no hidden local embedder model payload
- no surprise multi-GB postinstall download

## Layout

```text
distribution/npm/
├── README.md
├── PUBLISHING.md
├── sync-version.mjs
└── aicx/
    ├── package.json
    ├── README.md
    ├── index.js
    ├── index.d.ts
    ├── install.js
    ├── bin/
    │   ├── aicx
    │   └── aicx-mcp
    └── platform-packages/
        ├── darwin-arm64/
        └── linux-x64-gnu/
```

## Repo maintenance workflow

```bash
node distribution/npm/sync-version.mjs 0.7.3
node distribution/npm/sync-version.mjs --check 0.7.3
node distribution/npm/verify-metadata.mjs 0.7.3
```

See [PUBLISHING.md](./PUBLISHING.md) for the publish flow.
