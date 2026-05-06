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

The install flow resolves the matching platform package, downloads the official
GitHub Release archive, verifies the adjacent `.sha256`, and extracts both
binaries in place.

The npm package installs the product binaries only. Local embedder models remain
an explicit post-install choice; they are not hidden inside npm postinstall.
