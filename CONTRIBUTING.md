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

Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders
