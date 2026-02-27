# Contributing to ai-contexters

Thank you for your interest in contributing.

## Getting Started

1. Fork the repository
2. Create a feature branch from `develop`
3. Make your changes
4. Run checks before submitting:

```bash
cargo check
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt --check
cargo test
```

## Pull Requests

- Target the `develop` branch
- Use [Conventional Commits](https://www.conventionalcommits.org/) for commit messages
- Keep changes focused and atomic
- Include tests for new functionality

## VetCoders Skills Suite

If contributing to the skills suite, follow the methodology:

1. Run `vetcoders-init` to bootstrap context
2. Use `vetcoders-workflow` for non-trivial changes
3. Run `vetcoders-followup` before opening a PR

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
