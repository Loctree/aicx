# AICX Build System
# Local developer flow + release/readiness helpers

.PHONY: all build install install-bin install-config install-cargo
.PHONY: precheck test check fmt fmt-check clippy semgrep ci clean help
.PHONY: version-show version-check release-plan release-check release-tag release-push package-check

all: build

PACKAGE_NAME := ai-contexters
VERSION := $(shell python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml","rb"))["package"]["version"])')
TAG := v$(VERSION)

build:
	cargo build --locked --release --bin aicx --bin aicx-mcp

install:
	./install.sh

install-bin:
	cargo install --path . --locked --force --bin aicx --bin aicx-mcp

install-config:
	./install.sh --skip-install

install-cargo:
	cargo install $(PACKAGE_NAME) --locked

precheck:
	cargo check --locked --all-targets

test:
	cargo test --locked --all-targets

check:
	@echo "=== AICX Quality Gate ==="
	@echo "[1/6] Checking formatting..."
	@cargo fmt --all --check || (echo "Run 'make fmt' to fix formatting." && exit 1)
	@echo "[2/6] Running cargo check..."
	@cargo check --locked --all-targets
	@echo "[3/6] Running clippy..."
	@cargo clippy --locked --all-features --all-targets -- -D warnings
	@echo "[4/6] Running tests..."
	@cargo test --locked --all-targets
	@echo "[5/6] Building release binaries..."
	@cargo build --locked --release --bin aicx --bin aicx-mcp
	@echo "[6/6] Running Semgrep (if available)..."
	@if command -v semgrep >/dev/null 2>&1 || command -v pipx >/dev/null 2>&1; then \
		SEMGREP=$$(command -v semgrep || echo "pipx run semgrep"); \
		$$SEMGREP --config auto --error --quiet . --exclude target; \
	else \
		echo "[!] Semgrep not available, skipping (install: pipx install semgrep)"; \
	fi
	@echo "=== All checks passed ==="

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

clippy:
	cargo clippy --locked --all-features --all-targets -- -D warnings

semgrep:
	@if command -v semgrep >/dev/null 2>&1 || command -v pipx >/dev/null 2>&1; then \
		SEMGREP=$$(command -v semgrep || echo "pipx run semgrep"); \
		$$SEMGREP --config auto --error --quiet . --exclude target; \
	else \
		echo "[!] Semgrep not available, skipping (install: pipx install semgrep)"; \
	fi

ci: check
	@echo "CI-equivalent local checks passed."

version-show:
	@printf "package: %s\n" "$(PACKAGE_NAME)"
	@printf "version: %s\n" "$(VERSION)"
	@printf "tag: %s\n" "$(TAG)"
	@if git rev-parse --verify "refs/tags/$(TAG)" >/dev/null 2>&1; then \
		echo "tag-state: exists"; \
	else \
		echo "tag-state: missing"; \
	fi

version-check:
	@python3 -c 'import pathlib, sys, tomllib; version = tomllib.load(open("Cargo.toml","rb"))["package"]["version"]; changelog = pathlib.Path("CHANGELOG.md").read_text(encoding="utf-8"); \
assert "## [Unreleased]" in changelog or (_ for _ in ()).throw(SystemExit("CHANGELOG.md is missing '\''## [Unreleased]'\''")); \
print(f"Current version {version} already has a dedicated changelog section." if f"## [{version}]" in changelog or f"## [v{version}]" in changelog else f"Cargo.toml version {version} present; CHANGELOG has Unreleased section.")'

release-plan:
	@echo "AICX release flow"
	@echo ""
	@echo "1. Ensure branch is merged and green."
	@echo "2. Run: make release-check"
	@echo "3. Create annotated tag: make release-tag"
	@echo "4. Push tag: make release-push"
	@echo "5. GitHub Actions release workflow builds and publishes archives."
	@echo ""
	@echo "Reference docs:"
	@echo "  - docs/RELEASES.md"
	@echo "  - docs/COMMANDS.md"

release-check: version-check
	@echo "[extra] Verifying release package..."
	@cargo package --locked
	@$(MAKE) check
	@echo "Release readiness passed."

release-tag:
	@if git rev-parse --verify "refs/tags/$(TAG)" >/dev/null 2>&1; then \
		echo "Tag $(TAG) already exists."; \
		exit 1; \
	fi
	git tag -a "$(TAG)" -m "Release $(TAG)"
	@echo "Created annotated tag $(TAG)"

release-push:
	git push origin "$(TAG)"

package-check:
	cargo package --locked

clean:
	cargo clean

help:
	@echo "AICX Build System"
	@echo ""
	@echo "Core Commands:"
	@echo "  make build           - Build release binaries (aicx + aicx-mcp)"
	@echo "  make install         - Install binaries + configure local MCP clients via install.sh"
	@echo "  make install-bin     - Install only aicx + aicx-mcp from the current checkout"
	@echo "  make install-config  - Configure local MCP clients without reinstalling binaries"
	@echo "  make install-cargo   - Install published crate from crates.io"
	@echo "  make precheck        - Quick cargo check"
	@echo "  make check           - Full local gate (fmt, check, clippy, test, build, semgrep)"
	@echo "  make test            - Run all tests"
	@echo "  make fmt             - Format all Rust code"
	@echo "  make clean           - Clean build artifacts"
	@echo ""
	@echo "Release / Version:"
	@echo "  make version-show    - Show package version and tag state"
	@echo "  make version-check   - Validate Cargo.toml + CHANGELOG release readiness basics"
	@echo "  make release-plan    - Print the post-merge release flow"
	@echo "  make release-check   - Strict release readiness gate"
	@echo "  make release-tag     - Create annotated tag from Cargo.toml version"
	@echo "  make release-push    - Push the current release tag to origin"
	@echo "  make package-check   - Run cargo package --locked"
	@echo ""
	@echo "Quick start:"
	@echo "  make install         - Contributor/local operator setup"
	@echo "  make check           - Full local verification"
	@echo "  make release-plan    - Review release flow before tagging"
