.PHONY: help fmt test clippy check

help:
	@echo "aicx - development commands"
	@echo ""
	@echo "Usage: make [target]"
	@echo ""
	@echo "  fmt      Run rustfmt"
	@echo "  test     Run tests"
	@echo "  clippy   Run clippy (deny warnings)"
	@echo "  check    Run fmt + test + clippy"

fmt:
	cargo fmt

test:
	cargo test

clippy:
	cargo clippy -- -D warnings

check: fmt test clippy
