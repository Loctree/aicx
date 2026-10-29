# distribution/npm - aicx npm release surface

This directory is the npm distribution surface for `aicx`.
The source of truth for the product lives in
[Loctree/aicx](https://github.com/Loctree/aicx); this folder contains only the
thin JS wrapper and platform-package manifests that ship to the `@loctree` npm
scope.

> Status: aligned to the signed GitHub Release asset shape for macOS arm64,
> macOS x64, Linux arm64 GNU, Linux x64 GNU, and Windows x64 MSVC. Do not
> publish npm packages until the matching signed release assets exist for the
> target version.

## Wrapper package

| Package | Binaries | Purpose | Release repo |
| --- | --- | --- | --- |
| `@loctree/aicx` | `aicx`, `aicx-mcp` | CLI + MCP server | `Loctree/aicx` |

The wrapper declares active platform sub-packages as `optionalDependencies`
(esbuild/swc pattern).

Current platform matrix:

- `darwin-arm64`
- `darwin-x64`
- `linux-arm64-gnu` (glibc; the default Docker platform on Apple Silicon)
- `linux-x64-gnu`
- `win32-x64-gnu` (legacy npm package suffix; contains the MSVC build)

Total: **1 wrapper + 5 active platform packages = 6 npm packages.**

On a host outside this list (Linux musl/Alpine, Windows arm64, FreeBSD, ...)
the wrapper does not fall through silently: it prints `process.platform`,
`process.arch`, the detected libc, the supported platforms and the issues link
to stderr and exits with code 1.

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
- no npm lifecycle scripts
- no install-time release download or extraction
- no hidden local embedder model payload

## Layout

```text
distribution/npm/
├── README.md
├── PUBLISHING.md
├── stage-platform-package.mjs
├── sync-version.mjs
├── verify-metadata.mjs
└── aicx/
    ├── package.json
    ├── README.md
    ├── index.js
    ├── index.d.ts
    ├── bin/
    │   ├── aicx
    │   └── aicx-mcp
    └── platform-packages/
        ├── darwin-arm64/bin/{aicx,aicx-mcp}
        ├── darwin-x64/bin/{aicx,aicx-mcp}
        ├── linux-arm64-gnu/bin/{aicx,aicx-mcp}
        ├── linux-x64-gnu/bin/{aicx,aicx-mcp}
        └── win32-x64-gnu/bin/{aicx.exe,aicx-mcp.exe}
```

## Repo maintenance workflow

```bash
node distribution/npm/sync-version.mjs 0.14.0
node distribution/npm/sync-version.mjs --check 0.14.0
node distribution/npm/verify-metadata.mjs 0.14.0 --platform=<native-platform>
node --test distribution/npm/aicx/test/wrapper.test.js
```

See [PUBLISHING.md](./PUBLISHING.md) for the publish flow.
