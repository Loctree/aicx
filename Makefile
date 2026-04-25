# AICX Build System
# Local developer flow + release/readiness helpers

.PHONY: all build build-native install install-bin install-config install-cargo git-hooks
.PHONY: precheck precheck-native test test-native check fmt fmt-check clippy clippy-native semgrep ci clean help manifest-check
.PHONY: embeddings-check embeddings-test embeddings-clippy embeddings-hydrate embeddings-info
.PHONY: version-show version-check version-bump changelog-close release-notes release-plan release-prepare release-check release-tag release-push package-check release-bundle

all: build

PACKAGE_NAME := $(shell python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml","rb"))["package"]["name"])')
VERSION := $(shell python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml","rb"))["package"]["version"])')
TAG := v$(VERSION)
KEYS ?= $(if $(AICX_KEYS_DIR),$(AICX_KEYS_DIR),$(HOME)/.keys)
NOTARY_PROFILE ?= $(AICX_NOTARY_PROFILE)
CLEAN ?= 1
EMBEDDER_PROFILE ?= base
NATIVE ?= 0
FEATURES ?=

build:
	cargo build --locked --release --bin aicx --bin aicx-mcp

build-native:
	cargo build --locked --release --features native-embedder --bin aicx --bin aicx-mcp

install:
	./install.sh
	@$(MAKE) git-hooks

install-bin:
	cargo install --path . --locked --force --bin aicx --bin aicx-mcp

install-config:
	./install.sh --skip-install

install-cargo:
	@echo "crates.io install is not the active AICX distribution path."
	@echo "Use GitHub Release bundles, npm, Homebrew tap, or a local checkout install."
	@echo "For this checkout, run: make install-bin"

git-hooks:
	@echo "Installing git hooks..."
	@bash ./tools/install-githooks.sh
	@echo "✓ pre-commit + pre-push hooks installed"

precheck:
	cargo check --locked -p aicx --all-targets
	cargo check --locked -p aicx-embeddings

precheck-native:
	cargo check --locked -p aicx-embeddings --features gguf
	cargo check --locked -p aicx --features native-embedder --all-targets

manifest-check:
	@python3 -c 'import tomllib; data = tomllib.load(open("Cargo.toml", "rb")); allow = {("dependencies", "rmcp-memex"), ("dependencies", "aicx-embeddings")}; bad = [(section, name, spec["path"]) for section in ("dependencies", "dev-dependencies", "build-dependencies") for name, spec in data.get(section, {}).items() if isinstance(spec, dict) and "path" in spec and (section, name) not in allow]; \
print("Manifest policy: ok (approved local product deps only)") if not bad else (_ for _ in ()).throw(SystemExit("Manifest policy check failed:\n" + "\n".join(f"  - {section}.{name} uses unexpected local path dependency {path}" for section, name, path in bad)))'

test:
	cargo test --locked -p aicx --all-targets
	cargo test --locked -p aicx-embeddings

test-native:
	cargo test --locked -p aicx-embeddings --features gguf
	cargo test --locked -p aicx --features native-embedder --test native_embedder

check:
	@echo "=== AICX Quality Gate ==="
	@echo "[1/10] Checking manifest portability..."
	@$(MAKE) manifest-check
	@echo "[2/10] Checking formatting..."
	@cargo fmt --all --check || (echo "Run 'make fmt' to fix formatting." && exit 1)
	@echo "[3/10] Running default cargo check..."
	@$(MAKE) precheck
	@echo "[4/10] Running native GGUF cargo check..."
	@$(MAKE) precheck-native
	@echo "[5/10] Running default clippy..."
	@$(MAKE) clippy
	@echo "[6/10] Running native GGUF clippy..."
	@$(MAKE) clippy-native
	@echo "[7/10] Running default tests..."
	@$(MAKE) test
	@echo "[8/10] Running native GGUF tests..."
	@$(MAKE) test-native
	@echo "[9/10] Building slim release binaries..."
	@cargo build --locked --release --bin aicx --bin aicx-mcp
	@echo "[10/10] Running Semgrep (if available)..."
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
	cargo clippy --locked -p aicx --all-targets -- -D warnings
	cargo clippy --locked -p aicx-embeddings -- -D warnings

clippy-native:
	cargo clippy --locked -p aicx-embeddings --features gguf -- -D warnings
	cargo clippy --locked -p aicx --features native-embedder --all-targets -- -D warnings

embeddings-info:
	@case "$(EMBEDDER_PROFILE)" in \
		base) repo="mradermacher/F2LLM-v2-0.6B-GGUF"; file="F2LLM-v2-0.6B.Q4_K_M.gguf"; size="~397 MB"; dim="1024";; \
		dev) repo="mradermacher/F2LLM-v2-1.7B-GGUF"; file="F2LLM-v2-1.7B.Q4_K_M.gguf"; size="~1.1 GB"; dim="2048";; \
		premium) repo="mradermacher/F2LLM-v2-1.7B-GGUF"; file="F2LLM-v2-1.7B.Q6_K.gguf"; size="~1.4 GB"; dim="2048";; \
		*) echo "Unsupported EMBEDDER_PROFILE=$(EMBEDDER_PROFILE). Use base, dev, or premium." >&2; exit 1;; \
	esac; \
	printf "profile: %s\nrepo: %s\nfile: %s\nsize: %s\ndim: %s\n" "$(EMBEDDER_PROFILE)" "$$repo" "$$file" "$$size" "$$dim"

embeddings-hydrate:
	@if ! command -v hf >/dev/null 2>&1; then \
		echo "Missing 'hf' CLI. Install HuggingFace CLI first, then retry."; \
		exit 1; \
	fi
	@case "$(EMBEDDER_PROFILE)" in \
		base) repo="mradermacher/F2LLM-v2-0.6B-GGUF"; file="F2LLM-v2-0.6B.Q4_K_M.gguf";; \
		dev) repo="mradermacher/F2LLM-v2-1.7B-GGUF"; file="F2LLM-v2-1.7B.Q4_K_M.gguf";; \
		premium) repo="mradermacher/F2LLM-v2-1.7B-GGUF"; file="F2LLM-v2-1.7B.Q6_K.gguf";; \
		*) echo "Unsupported EMBEDDER_PROFILE=$(EMBEDDER_PROFILE). Use base, dev, or premium." >&2; exit 1;; \
	esac; \
	echo "Hydrating AICX native embedder: $$repo $$file"; \
	hf download "$$repo" "$$file"

embeddings-check:
	cargo check --locked -p aicx-embeddings --features gguf

embeddings-test:
	cargo test --locked -p aicx-embeddings --features gguf

embeddings-clippy:
	cargo clippy --locked -p aicx-embeddings --features gguf -- -D warnings

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
	@python3 tools/release_sync.py check

version-bump:
ifeq ($(origin VERSION),command line)
	@python3 tools/release_sync.py bump "$(VERSION)"
	@echo ""
	@echo "Versioned release surfaces synced from Cargo.toml into docs + distribution/npm."
	@echo "Cargo.lock is intentionally not touched by version-bump."
	@echo "To sync the lockfile for this package only (no network):"
	@echo "  cargo update --package $(PACKAGE_NAME) --offline"
	@echo "Or rely on 'make release-prepare' to sync it for you."
else
	@echo "VERSION is required. Usage: make version-bump VERSION={patch|minor|major|x.y.z}" >&2 && exit 1
endif

changelog-close:
	@python3 tools/changelog_close.py $(if $(CHANGELOG_GENERATE),--generate-if-empty)

release-notes:
	@python3 tools/release_sync.py notes $(if $(origin VERSION),$(VERSION),) $(if $(OUTPUT),--output $(OUTPUT),)

release-plan:
	@echo "AICX release flow"
	@echo ""
	@echo "1. Ensure branch is merged and green."
	@echo "2. Prepare the release bundle:"
	@echo "     make release-prepare VERSION={patch|minor|major|x.y.z}"
	@echo "   (runs version-bump + changelog-close + release-notes preview + precheck)"
	@echo "3. Review diff, commit Cargo.toml + Cargo.lock + CHANGELOG.md + any synced docs/package manifests."
	@echo "4. Run: make release-check"
	@echo "5. Create annotated tag: make release-tag"
	@echo "6. Push tag: make release-push"
	@echo "7. Wait for GitHub Actions to build signed release archives from the pushed tag."
	@echo "8. Publish npm after GitHub Release assets exist:"
	@echo "     gh workflow run npm-publish.yml -f version=$(VERSION)"
	@echo "9. Optional local macOS signed bundle:"
	@echo "     make release-bundle KEYS=$(HOME)/.keys"
	@echo "     make release-bundle KEYS=$(HOME)/.keys NATIVE=1"
	@echo "     make release-bundle KEYS=$(HOME)/.keys NOTARY_PROFILE=my-notary-profile"
	@echo "     make release-bundle KEYS=$(HOME)/.keys CLEAN=0    # keep local target artifacts"
	@echo "10. Optional native embedder sanity:"
	@echo "     make embeddings-info EMBEDDER_PROFILE=base"
	@echo "     make embeddings-hydrate EMBEDDER_PROFILE=base"
	@echo "     make test-native"
	@echo "11. GitHub Actions release workflow builds archives and derives GitHub release notes from CHANGELOG.md."
	@echo ""
	@echo "Reference docs:"
	@echo "  - docs/RELEASES.md"
	@echo "  - docs/COMMANDS.md"

release-prepare:
ifeq ($(origin VERSION),command line)
	@$(MAKE) version-bump VERSION=$(VERSION)
	@$(MAKE) changelog-close CHANGELOG_GENERATE=1
	@cargo update --package $(PACKAGE_NAME) --offline
	@$(MAKE) version-check
	@python3 tools/release_sync.py notes --output dist/release-notes.md
	@$(MAKE) precheck
else
	@echo "VERSION is required. Usage: make release-prepare VERSION={patch|minor|major|x.y.z}" >&2 && exit 1
endif
	@echo ""
	@echo "=== Release prepared ==="
	@echo "Next: review diff, commit, then:"
	@echo "  make release-check"
	@echo "  make release-tag"
	@echo "  make release-push"
	@echo "  cat dist/release-notes.md   # preview GitHub release body"
	@echo "  make release-bundle KEYS=$(HOME)/.keys [CLEAN=0]"

release-check:
	@python3 tools/release_sync.py check --require-version-section
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
	@echo "crates.io packaging is intentionally disabled for aicx."
	@echo "Use GitHub Release archives + npm platform packages instead."
	@echo "Run: make release-check"

release-bundle:
	@KEYS="$(KEYS)" \
	NOTARY_PROFILE="$(NOTARY_PROFILE)" \
	AICX_CLEAN_AFTER_BUILD="$(CLEAN)" \
	NATIVE="$(NATIVE)" \
	FEATURES="$(FEATURES)" \
	PACKAGE_NAME="$(PACKAGE_NAME)" \
	./tools/release_bundle.sh

clean:
	cargo clean

help:
	@echo "AICX Build System"
	@echo ""
	@echo "Core Commands:"
	@echo "  make build           - Build release binaries (aicx + aicx-mcp)"
	@echo "  make build-native    - Build release binaries with native GGUF embedder support"
	@echo "  make install         - Install binaries + configure local MCP clients via install.sh"
	@echo "  make install-bin     - Install only aicx + aicx-mcp from the current checkout"
	@echo "  make install-config  - Configure local MCP clients without reinstalling binaries"
	@echo "  make install-cargo   - Explain why crates.io install is not the active path"
	@echo "  make git-hooks       - Install repo-local pre-commit + pre-push hooks"
	@echo "  make precheck        - Quick default cargo check"
	@echo "  make precheck-native - Quick native GGUF cargo check"
	@echo "  make manifest-check  - Fail if Cargo.toml uses local path dependencies"
	@echo "  make check           - Full local gate (fmt, check, clippy, test, build, semgrep)"
	@echo "  make test            - Run all tests"
	@echo "  make test-native     - Run native GGUF embedder tests"
	@echo "  make fmt             - Format all Rust code"
	@echo "  make clean           - Clean build artifacts"
	@echo ""
	@echo "Native Embeddings:"
	@echo "  make embeddings-info EMBEDDER_PROFILE=base|dev|premium     - Show GGUF profile details"
	@echo "  make embeddings-hydrate EMBEDDER_PROFILE=base|dev|premium  - Download selected GGUF into HF cache"
	@echo "  make embeddings-check                                      - Check aicx-embeddings with GGUF backend"
	@echo "  make embeddings-test                                       - Test aicx-embeddings with GGUF backend"
	@echo "  make embeddings-clippy                                     - Clippy aicx-embeddings with GGUF backend"
	@echo ""
	@echo "Release / Version:"
	@echo "  make version-show          - Show package version and tag state"
	@echo "  make version-check         - Validate synced release surfaces (Cargo/docs/npm/changelog basics)"
	@echo "  make version-bump VERSION=X - Bump version and sync docs/npm surfaces. X={patch|minor|major|x.y.z}"
	@echo "  make changelog-close       - Close CHANGELOG '## [Unreleased]' to current version + date"
	@echo "  make release-notes         - Print release notes body derived from CHANGELOG current version section"
	@echo "  make release-plan          - Print the full post-merge release flow"
	@echo "  make release-prepare VERSION=X - version-bump + changelog-close + notes preview + precheck. X={patch|minor|major|x.y.z}"
	@echo "  make release-check         - Strict release readiness gate"
	@echo "  make release-tag           - Create annotated tag from Cargo.toml version"
	@echo "  make release-push          - Push the current release tag to origin"
	@echo "  make package-check         - Explain binary-release packaging track"
	@echo "  make release-bundle        - Local macOS bundle + codesign + notarize using KEYS/NOTARY_PROFILE (NATIVE=1 for GGUF backend)"
	@echo ""
	@echo "Quick start:"
	@echo "  make install         - Contributor/local operator setup"
	@echo "  make check           - Full local verification"
	@echo "  make release-plan    - Review release flow before tagging"
