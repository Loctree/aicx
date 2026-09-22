# Contributing to aicx

Thanks for your interest in contributing to aicx.

## Prerequisites

- Rust 1.85+ with cargo
- Git

## Development

Install the repo-local Git hooks:

```bash
make git-hooks
```

## Commit provenance

Commits carry trailers that say which agent or person produced them and which
session they came from:

```
[codex/interactive] chore: describe the change

Why this commit exists.

Authored-By: codex <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
time: 2026-06-04T14:08:27-06:00
runtime: iterm2
session_pid: 35432
```

`prepare-commit-msg` fills these in; `commit-msg` validates them independently.
Two rules govern the split:

- **A measured value wins.** The generator overwrites what a message claims
  about its own session, because provenance is a measurement, not a
  self-report.
- **An unmeasurable value is never invented.** If the session cannot be
  resolved, the hook writes nothing and says so. A plausible-looking but
  fabricated id is worse than a missing one.

`time:` is the fleet-wide key and must be ISO-8601 with an explicit offset. The
older `timestamp:` and `date:` keys are rejected on new commits — history keeps
them, and readers still recognize them, but nothing new is written that way.

`session_pid:` is optional. `session_id` identifies a *transcript*, so two live
processes resuming one session legitimately share it; the pid is what tells them
apart. Toggle it at any time, highest precedence first:

```bash
VC_SESSION_PID=0 git commit          # this commit only
git config vetcoders.sessionPid false  # this repo
git config --global vetcoders.sessionPid false
```

The validator never requires it, so manual commits and amends made outside an
agent process stay possible. Humans commit on a `manual` runtime lane and keep a
human address rather than the shared agent mailbox.

Verify both hooks:

```bash
make hooks-test
```

Build the release binary:

```bash
cargo build --release
```

Run the linter (must pass with zero warnings):

```bash
cargo clippy --all-features --all-targets -- -D warnings
```

Run tests:

```bash
cargo test
```

Local development commands that read or write tempfile-backed `/tmp` paths now
follow the same path-safety policy as release builds. Export
`AICX_ALLOW_TMP=1` when you intentionally run dev/smoke commands against `/tmp`
or macOS `/private/var/folders` paths. Cargo tests keep their `cfg(test)`
tempfile allowance.

Format code:

```bash
cargo fmt
```

## Pull Request Process

1. Fork the repository.
2. Create a feature branch from `develop`.
3. Make your changes and ensure all checks pass (`clippy`, `test`, `fmt`).
4. Open a pull request against the `develop` branch.
5. Describe what your change does and why.

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](https://www.contributor-covenant.org/version/2/1/code_of_conduct/).

---

Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders
