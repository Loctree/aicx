# Compile embargo

This repository can temporarily protect W1/W2 architecture shaping from
compiler-driven redesign. The mechanism is narrow: an operator-owned marker may
defer only `cargo check`, `cargo clippy`, and `cargo test`. Formatting, manifest
portability, Semgrep, commit identity, and destination-ref safety stay enforced.

The marker is `.vibecrafted/embargo.toml`; its complete schema is tracked in
`.vibecrafted/embargo.toml.example`. Hooks reject a malformed marker. A valid
marker applies only when its `branch` exactly matches the checked-out branch,
its phase is `W1` or `W2`, and its attestation is `open`. Other branches and a
closed marker use the ordinary hook policy.

## Installation

Run `tools/git-hooks/install.sh` once in the checkout. It idempotently sets
`core.hooksPath` to `tools/git-hooks`; it does not copy files into `.git/hooks`.
That `pre-push` then runs for every push, including when no embargo marker is
open. On the first push of a branch the comparison base is the destination
remote. For `origin`, that is `origin/HEAD`, and a missing or `develop`
symref falls back to `origin/main`. For any other remote it is that remote's
HEAD, including when that default is `develop`. A missing local symref is
read with `git ls-remote --symref`. If that still does not resolve, or the
advertised remote tip is not in the local object database, the hook runs the full gate
instead of treating the push as the delta from `origin/main` or looking only
at the tip commit. It does not use `origin/develop`.
Run `tools/git-hooks/selftest.sh` to exercise the commit, push, and installer
contract in disposable repositories. The provenance selftest
(`make hooks-test`) checks that this pre-push does not merge-base against
`origin/develop` and that a non-origin remote supplies its own baseline.

## Recovery ref

While the marker is open, pre-commit still formats staged Rust but defers its
compile check. Pre-push accepts only the marker's exact
`refs/heads/embargo/<plan_id>` destination and runs the light gate: manifest,
format, and Semgrep. Trunk, feature, release, and tag destinations are rejected.
The existence of the marker never grants push authority; the current operator
mandate still controls remote mutation.

## Closing W2

Only the operator may attest `W2_STRUCTURALLY_CLOSED`. After reviewing the W2
structural evidence, the operator changes `attestation`, records their identity
in `signed_by`, and records the full lowercase SHA of the evidenced commit in
`commit`. The same attestation and SHA belong in the mission journal.

Once closed, the marker no longer defers anything. Before the next ordinary
feature checkpoint, run every deferred gate plus the repository's full gate set.
A failure reopens the declared recovery workflow; bypass flags remain forbidden.
