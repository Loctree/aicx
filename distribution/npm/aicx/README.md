# @loctree/aicx

Thin npm wrapper for the `aicx` product surface.

This package installs both shipped binaries:

- `aicx`
- `aicx-mcp`

## Install

```bash
npm install -g @loctree/aicx
```

Then:

```bash
aicx --help
aicx-mcp --version
```

On a macOS machine that already ran the background MCP service before this
release, migrate it once without changing its host/port configuration:

```bash
aicx doctor --repair-runtime
```

Repair keeps the HTTP MCP listener reader-only and preserves its configured
bind and auth. The wrapper accepts reader-only services with or without the
deprecated `--no-auto-refresh` flag; an embedded experimental writer requires
migration. Catalog/index maintenance is a separate process.

The wrapper resolves both binaries from the matching platform package. That
package already contains the verified release binaries; npm installation does
not run lifecycle scripts, download release assets, or unpack archives.

Supported platforms: macOS arm64 and x64, Linux arm64 and x64 (glibc), Windows
x64. On any other host (Alpine/musl, Windows arm64, FreeBSD, ...) or when the
platform package is missing, `aicx` prints the detected platform, arch and libc
plus the supported list to stderr and exits with code 1. A `node_modules`
installed on one OS or CPU does not run on another: reinstall inside the
container or machine that runs it.

The npm package installs the product binaries only. Local embedder models remain
an explicit operator choice.
