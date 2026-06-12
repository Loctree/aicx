# AICX Build System
# Local developer flow + release/readiness helpers

# --- Cargo PATH discovery ---------------------------------------------------
# Surface cargo from ~/.cargo/bin when the calling shell (uv-run, sh -c,
# CI runners that strip PATH) didn't source ~/.cargo/env. Idempotent —
# no-op when cargo is already reachable, silent when rustup isn't installed.
ifeq (,$(shell command -v cargo 2>/dev/null))
  ifneq (,$(wildcard $(HOME)/.cargo/bin/cargo))
    export PATH := $(HOME)/.cargo/bin:$(PATH)
  endif
endif

.PHONY: all build build-native release-binaries install install-bin install-config install-cargo git-hooks
.PHONY: precheck precheck-native loctree-consumer-check test test-native check fmt fmt-check clippy clippy-native semgrep ci clean help manifest-check
.PHONY: embeddings-check embeddings-test embeddings-clippy embeddings-hydrate embeddings-info
.PHONY: version version-show version-check version-bump version-patch bump-patch changelog-close release-notes release-plan release-prepare release-check release-tag release-push package-check release-bundle release-bundle-only-binaries test-e2e

all: build

PACKAGE_NAME := $(shell grep '^name = ' Cargo.toml | head -n 1 | cut -d '"' -f 2)
VERSION := $(shell grep '^version = ' Cargo.toml | head -n 1 | cut -d '"' -f 2)
TAG := v$(VERSION)

# --- Python discovery (release tooling needs stdlib tomllib, i.e. 3.11+) ----
# macOS system python3 is 3.9 (no tomllib). Pick the newest available 3.11+
# from PATH; fall back to plain python3 (scripts will fail-fast with a clear
# message if it's < 3.11).
PYTHON := $(shell command -v python3.14 2>/dev/null || command -v python3.13 2>/dev/null || command -v python3.12 2>/dev/null || command -v python3.11 2>/dev/null || command -v python3)
KEYS ?= $(if $(AICX_KEYS_DIR),$(AICX_KEYS_DIR),$(HOME)/.keys)
NOTARY_PROFILE ?= $(AICX_NOTARY_PROFILE)
CLEAN ?= 1
EMBEDDER_PROFILE ?= base
NATIVE ?= 0
FEATURES ?=
TARGET ?= $(shell rustc -vV | sed -n 's/^host: //p')
CODESIGN ?= auto
CARGO_BUILD ?= cargo build
DIST_DIR ?= $(CURDIR)/dist
DRY_RUN ?= 0
RELEASE_BINARIES := aicx aicx-mcp

build:
	cargo build --locked --release --bin aicx --bin aicx-mcp

build-native:
	cargo build --locked --release --features native-embedder --bin aicx --bin aicx-mcp

release-binaries:
	@if [ -z "$(STAGING_DIR)" ]; then \
		echo "STAGING_DIR is required. Usage: make release-binaries STAGING_DIR=/tmp/stage TARGET=$(TARGET)" >&2; \
		exit 1; \
	fi
	$(CARGO_BUILD) --locked --release --target "$(TARGET)" --bin aicx --bin aicx-mcp
	@mkdir -p "$(STAGING_DIR)/bin" "$(STAGING_DIR)/components"
	@for bin in $(RELEASE_BINARIES); do \
		install -m 0755 "target/$(TARGET)/release/$$bin" "$(STAGING_DIR)/bin/$$bin"; \
		printf '  %s -> %s\n' "$$bin" "$(STAGING_DIR)/bin/$$bin"; \
	done
	@case "$(TARGET)" in \
		*apple-darwin) \
			if [ "$(CODESIGN)" = "0" ]; then \
				echo "  codesign skipped (CODESIGN=0)"; \
			elif [ -n "$${MACOS_DEVELOPER_ID_APPLICATION:-}" ]; then \
				for bin in $(RELEASE_BINARIES); do \
					codesign --force --timestamp --options runtime --sign "$$MACOS_DEVELOPER_ID_APPLICATION" "$(STAGING_DIR)/bin/$$bin"; \
					codesign --verify --verbose=2 "$(STAGING_DIR)/bin/$$bin" >/dev/null; \
					printf '  codesigned %s\n' "$$bin"; \
				done; \
			elif [ "$(CODESIGN)" = "1" ]; then \
				echo "MACOS_DEVELOPER_ID_APPLICATION is required for CODESIGN=1" >&2; \
				exit 1; \
			else \
				echo "  codesign skipped (set CODESIGN=1 and MACOS_DEVELOPER_ID_APPLICATION for release)"; \
			fi ;; \
	esac
	@$(PYTHON) -c 'import json, pathlib, sys; staging=pathlib.Path(sys.argv[1]); version=sys.argv[2]; commit=sys.argv[3]; data={"source":"loctree-aicx","commit":commit,"components":[{"name":"aicx","version":version,"source":"loctree-aicx"},{"name":"aicx-mcp","version":version,"source":"loctree-aicx"}]}; path=staging/"components"/"loctree-aicx.json"; path.write_text(json.dumps(data, indent=2)+"\n", encoding="utf-8"); print(f"  metadata -> {path}")' "$(STAGING_DIR)" "$(VERSION)" "$$(git rev-parse --short=12 HEAD)"

release-binaries-linux:
	@for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-musl; do \
		echo "==> Building $$target"; \
		cross build --release --target $$target --bin aicx --bin aicx-mcp || exit 1; \
		mkdir -p dist/aicx-v$(VERSION)-$$target-slim-unsigned; \
		cp target/$$target/release/aicx target/$$target/release/aicx-mcp dist/aicx-v$(VERSION)-$$target-slim-unsigned/; \
		(cd dist && tar -czf aicx-v$(VERSION)-$$target-slim-unsigned.tar.gz aicx-v$(VERSION)-$$target-slim-unsigned/); \
	done

install:
	./install.sh
	@$(MAKE) git-hooks

install-bin:
	AICX_INSTALL_MODE=local ./install.sh --shadow-check-only
	cargo install --path . --locked --force --bin aicx --bin aicx-mcp
	AICX_INSTALL_MODE=local ./install.sh --verify-path-only

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

loctree-consumer-check:
	cargo check --locked -p aicx --no-default-features --features loctree-consumer
	cargo test --locked -p aicx --lib --no-default-features --features loctree-consumer slim_profile_exposes_read_core_contract
	@hits=$$(cargo tree --locked --no-default-features --features loctree-consumer -p aicx | grep -ciE 'lancedb|llama' || true); \
	if [ "$$hits" != "0" ]; then \
		echo "loctree-consumer pulled forbidden lancedb/llama dependencies ($$hits hits)" >&2; \
		exit 1; \
	fi

manifest-check:
	@$(PYTHON) -c 'import sys, re; text = open("Cargo.toml", "r").read(); bad = [m.group(1) for m in re.finditer(r"^([\w-]+)\s*=.*path\s*=", text, re.MULTILINE) if m.group(1) not in ("rmcp-memex", "aicx-embeddings", "aicx-retrieve", "aicx-parser", "aicx-monitor", "aicx-progress-contracts", "path")]; sys.exit("Manifest policy check failed:\n  - Unexpected local path dependency: " + ", ".join(bad)) if bad else print("Manifest policy: ok (approved local product deps only)")'

test:
	cargo test --locked -p aicx --all-targets
	cargo test --locked -p aicx-embeddings

test-native:
	cargo test --locked -p aicx-embeddings --features gguf
	cargo test --locked -p aicx --features native-embedder --test native_embedder

# End-to-end pipeline test against operator's canonical ~/.aicx/config.toml.
# Fail-fast when preconditions missing (no config, empty corpus, embedder
# unreachable) per operator doctrine — distinct from a CI test that
# silently skips on missing infra.
test-e2e:
	cargo test --locked -p aicx --features e2e-aicx --test e2e_pipeline -- --nocapture
	cargo test --locked -p aicx --features e2e-aicx --test e2e_context_pack_ingest -- --nocapture

test-retrieval-eval:
	cargo test --test retrieval_eval_harness

test-retrieval-eval-live:
	cargo test --test retrieval_eval_harness --features e2e-aicx -- --nocapture

test-retrieval-eval-rebaseline:
	AICX_RETRIEVAL_EVAL_WRITE_BASELINE=1 cargo test --test retrieval_eval_harness --features e2e-aicx -- --nocapture

check:
	@echo "=== AICX Quality Gate ==="
	@echo "[1/11] Checking manifest portability..."
	@$(MAKE) manifest-check
	@echo "[2/11] Checking formatting..."
	@cargo fmt --all --check || (echo "Run 'make fmt' to fix formatting." && exit 1)
	@echo "[3/11] Running default cargo check..."
	@$(MAKE) precheck
	@echo "[4/11] Running loctree consumer profile check..."
	@$(MAKE) loctree-consumer-check
	@echo "[5/11] Running native GGUF cargo check..."
	@$(MAKE) precheck-native
	@echo "[6/11] Running default clippy..."
	@$(MAKE) clippy
	@echo "[7/11] Running native GGUF clippy..."
	@$(MAKE) clippy-native
	@echo "[8/11] Running default tests..."
	@$(MAKE) test
	@echo "[9/11] Running native GGUF tests..."
	@$(MAKE) test-native
	@echo "[10/11] Building slim release binaries..."
	@cargo build --locked --release --bin aicx --bin aicx-mcp
	@echo "[11/11] Running Semgrep (required)..."
	@$(MAKE) semgrep
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
	@if command -v semgrep >/dev/null 2>&1; then SEMGREP="semgrep"; \
	elif command -v uvx >/dev/null 2>&1; then SEMGREP="uvx semgrep"; \
	elif command -v pipx >/dev/null 2>&1; then SEMGREP="pipx run semgrep"; \
	else echo "[x] Semgrep is REQUIRED — no runner found. Install semgrep, or use 'uvx semgrep' / 'pipx run semgrep'." >&2; exit 1; fi; \
	$$SEMGREP --config auto --error --quiet . --exclude target

ci: check
	@echo "CI-equivalent local checks passed."

version: version-show

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
	@$(PYTHON) tools/release_sync.py check
	@bash tools/release-channel-check.sh

version-bump:
ifeq ($(origin VERSION),command line)
	@$(PYTHON) tools/release_sync.py bump "$(VERSION)"
	@echo ""
	@echo "Versioned release surfaces synced from Cargo.toml into workspace crates + docs + distribution/npm."
	@echo "Cargo.lock is intentionally not touched by version-bump."
	@echo "To sync the lockfile for all workspace packages (no network):"
	@echo "  cargo update --workspace --offline"
	@echo "Or rely on 'make release-prepare' to sync it for you."
else
	@echo "VERSION is required. Usage: make version-bump VERSION={patch|minor|major|x.y.z}" >&2 && exit 1
endif

version-patch bump-patch:
	@$(MAKE) version-bump VERSION=patch

changelog-close:
	@$(PYTHON) tools/changelog_close.py $(if $(CHANGELOG_GENERATE),--generate-if-empty)

release-notes:
	@$(PYTHON) tools/release_sync.py notes $(if $(origin VERSION),$(VERSION),) $(if $(OUTPUT),--output $(OUTPUT),)

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
	@cargo update --workspace --offline
	@$(MAKE) version-check
	@$(PYTHON) tools/release_sync.py notes --output dist/release-notes.md
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
	@bash tools/release-channel-check.sh
	@$(PYTHON) tools/release_sync.py check --require-version-section
	@$(MAKE) check
	@echo "Release readiness passed."

release-tag:
	@if git rev-parse --verify "refs/tags/$(TAG)" >/dev/null 2>&1; then \
		echo "Tag $(TAG) already exists."; \
		exit 1; \
	fi
	@key="$${LOCTREE_GPG_KEY_ID:-}"; \
	if [ -z "$$key" ]; then \
		echo "LOCTREE_GPG_KEY_ID is not set — refusing to create unsigned release tag." >&2; \
		echo "Export the org's GPG key id in your shell (zshrc) or pass LOCTREE_GPG_KEY_ID=... inline." >&2; \
		exit 1; \
	fi; \
	pp="$${LOCTREE_GPG_PASSPHRASE_FILE:-$$HOME/.keys/.gpg.passphrase}"; \
	if [ -r "$$pp" ]; then \
		echo "warmup" | gpg --batch --pinentry-mode loopback --passphrase-file "$$pp" --local-user "$$key" --detach-sign -o /dev/null 2>/dev/null || true; \
	fi; \
	git tag -as -u "$$key" "$(TAG)" -m "Release $(TAG)"
	@echo "Created GPG-signed annotated tag $(TAG)"

release-push:
	git push origin "$(TAG)"

package-check:
	@echo "crates.io packaging is intentionally disabled for aicx."
	@echo "Use GitHub Release archives + npm platform packages instead."
	@echo "Run: make release-check"

release-bundle:
	@KEYS="$(KEYS)" \
	NOTARY_PROFILE="$(NOTARY_PROFILE)" \
	TARGET="$(TARGET)" \
	DIST_DIR="$(DIST_DIR)" \
	DRY_RUN="$(DRY_RUN)" \
	AICX_CLEAN_AFTER_BUILD="$(CLEAN)" \
	NATIVE="$(NATIVE)" \
	FEATURES="$(FEATURES)" \
	PACKAGE_NAME="$(PACKAGE_NAME)" \
	./tools/release_bundle.sh

release-bundle-only-binaries:
	@AICX_RELEASE_BUNDLE_ONLY_BINARIES=1 \
	TARGET="$(TARGET)" \
	DIST_DIR="$(DIST_DIR)" \
	DRY_RUN="$(DRY_RUN)" \
	AICX_CLEAN_AFTER_BUILD="$(CLEAN)" \
	NATIVE="$(NATIVE)" \
	FEATURES="$(FEATURES)" \
	PACKAGE_NAME="$(PACKAGE_NAME)" \
	./tools/release_bundle.sh

clean:
	cargo clean

# Help colors
HELP_C_CYAN   := \033[36m
HELP_C_GREEN  := \033[32m
HELP_C_YELLOW := \033[33m
HELP_C_RESET  := \033[0m

help:
	@printf '\n$(HELP_C_CYAN)%s$(HELP_C_RESET)\n' 'AICX Build System'
	@printf '\n'
	@printf '  $(HELP_C_YELLOW)%s$(HELP_C_RESET)\n' 'CORE COMMANDS'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'build' '- Build release binaries (aicx + aicx-mcp)'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'build-native' '- Build release binaries with native GGUF embedder support'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'install' '- Install binaries + configure local MCP clients via install.sh'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'install-bin' '- Install only aicx + aicx-mcp from the current checkout'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'install-config' '- Configure local MCP clients without reinstalling binaries'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'install-cargo' '- Explain why crates.io install is not the active path'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'git-hooks' '- Install repo-local pre-commit + pre-push hooks'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'precheck' '- Quick default cargo check'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'precheck-native' 'Quick native GGUF cargo check'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'manifest-check' '- Fail if Cargo.toml uses local path dependencies'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'check' '- Full local gate (fmt, check, clippy, test, build, semgrep)'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'test' '- Run all tests'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'test-native' '- Run native GGUF embedder tests'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'fmt' '- Format all Rust code'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'clean' '- Clean build artifacts'
	@printf '\n'
	@printf '  $(HELP_C_YELLOW)%s$(HELP_C_RESET)\n' 'NATIVE EMBEDDINGS'
	@printf '%s\n' '  make embeddings-info EMBEDDER_PROFILE=base|dev|premium     - Show GGUF profile details'
	@printf '%s\n' '  make embeddings-hydrate EMBEDDER_PROFILE=base|dev|premium  - Download selected GGUF into HF cache'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'embeddings-check' '- Check aicx-embeddings with GGUF backend'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'embeddings-test' '- Test aicx-embeddings with GGUF backend'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'embeddings-clippy' '- Clippy aicx-embeddings with GGUF backend'
	@printf '\n'
	@printf '  $(HELP_C_YELLOW)%s$(HELP_C_RESET)\n' 'RELEASE / VERSION'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'version' '- Alias for version-show'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'version-show' '- Show package version and tag state'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'version-check' '- Validate synced release surfaces (Cargo/docs/npm/changelog basics)'
	@printf '%s\n' '  make version-bump VERSION=X - Bump version and sync docs/npm surfaces. X={patch|minor|major|x.y.z}'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'version-patch' '- Alias for version-bump VERSION=patch'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'bump-patch' '- Alias for version-bump VERSION=patch'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'changelog-close' "- Close CHANGELOG '## [Unreleased]' to current version + date"
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'release-notes' '- Print release notes body derived from CHANGELOG current version section'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'release-plan' '- Print the full post-merge release flow'
	@printf '%s\n' '  make release-prepare VERSION=X - version-bump + changelog-close + notes preview + precheck. X={patch|minor|major|x.y.z}'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'release-check' '- Strict release readiness gate'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'release-tag' '- Create annotated tag from Cargo.toml version'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'release-push' '- Push the current release tag to origin'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'package-check' '- Explain binary-release packaging track'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'release-bundle' '- Local macOS bundle + codesign + notarize using KEYS/NOTARY_PROFILE (NATIVE=1 for GGUF backend)'
	@printf '\n'
	@printf '  $(HELP_C_YELLOW)%s$(HELP_C_RESET)\n' 'QUICK START'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'install' '- Contributor/local operator setup'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'check' '- Full local verification'
	@printf '    $(HELP_C_GREEN)%-18s$(HELP_C_RESET) %s\n' 'release-plan' '- Review release flow before tagging'
