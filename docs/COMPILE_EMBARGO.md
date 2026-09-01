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
Run `tools/git-hooks/selftest.sh` to exercise the commit, push, and installer
contract in disposable repositories.

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
