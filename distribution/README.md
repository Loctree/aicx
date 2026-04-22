# Distribution Spine

This directory is the single source of truth for every release channel that is
not "cargo publish the crate and hope for the best."

## Channels

- `npm/`
  Canonical npm wrapper and platform-package release flow.
- `EMBEDDERS.md`
  Canonical cross-repo contract for optional model hydration and native embedder selection.
- `INSTALLER.md`
  Canonical installer contract for direct bundle/release installs.

## Principle

One channel, one home.

If a distribution path is real, it belongs in `distribution/`.

The same rule applies to optional model delivery:

- keep shipping binaries lean
- never hide heavy model payloads inside the default bundle
- make model hydration explicit and operator-visible
- store post-install choices in a deterministic config file, not in tribal shell state
