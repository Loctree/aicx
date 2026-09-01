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

The wrapper resolves both binaries from the matching platform package. That
package already contains the verified release binaries; npm installation does
not run lifecycle scripts, download release assets, or unpack archives.

The npm package installs the product binaries only. Local embedder models remain
an explicit operator choice.
