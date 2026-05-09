# distribution/npm - aicx npm release surface

This directory is the planned npm distribution surface for `aicx`.
The source of truth for the product lives in
[Loctree/aicx](https://github.com/Loctree/aicx); this folder contains only the
thin JS wrapper and platform-package manifests that ship to the `@loctree` npm
scope.

> Status: not active for `v0.6.5`. The current GitHub Release publishes
> `*-slim-unsigned.tar.gz` assets for macOS arm64, Linux x64 GNU, and Linux
> arm64 GNU. The npm platform packages in this directory still describe the
> older zip/musl/darwin-x64 matrix and must be realigned before publishing.

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

This is the intended install shape after the npm platform packages are updated.
For `v0.6.5`, use `AICX_INSTALL_MODE=release` with the GitHub Release assets
instead.

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
node distribution/npm/sync-version.mjs 0.7.0
node distribution/npm/sync-version.mjs --check 0.7.0
```

See [PUBLISHING.md](./PUBLISHING.md) for the publish flow.
