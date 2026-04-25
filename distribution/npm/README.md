# distribution/npm - aicx npm release surface

This directory is the canonical npm distribution surface for `aicx`.
The source of truth for the product lives in
[Loctree/aicx](https://github.com/Loctree/aicx); this folder contains only the
thin JS wrapper and platform-package manifests that ship to the `@loctree` npm
scope.

## Wrapper package

| Package | Binaries | Purpose | Release repo |
| --- | --- | --- | --- |
| `@loctree/aicx` | `aicx`, `aicx-mcp` | CLI + MCP server | `Loctree/aicx` |

The wrapper declares 4 platform sub-packages as `optionalDependencies`
(esbuild/swc pattern).

Current platform matrix:

- `darwin-arm64`
- `darwin-x64`
- `linux-x64-gnu`
- `linux-x64-musl`

Total: **1 wrapper + 4 platform packages = 5 npm packages.**

## Install

```bash
npm install -g @loctree/aicx
```

Then:

```bash
aicx --help
aicx-mcp --version
```

The wrapper's install flow validates that the matching platform package is
present. The platform package then downloads the matching GitHub Release asset,
verifies the adjacent `.sha256`, and extracts both binaries in place.

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
        ├── darwin-x64/
        ├── linux-x64-gnu/
        └── linux-x64-musl/
```

## Repo maintenance workflow

```bash
node distribution/npm/sync-version.mjs 0.6.2
node distribution/npm/sync-version.mjs --check 0.6.2
```

See [PUBLISHING.md](./PUBLISHING.md) for the publish flow.
