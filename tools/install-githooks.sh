#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HOOKS_DIR="$ROOT/.git/hooks"
PRE_COMMIT_SRC="$ROOT/tools/githooks/pre-commit"
PRE_PUSH_SRC="$ROOT/tools/githooks/pre-push"
COMMIT_MSG_SRC="$ROOT/tools/githooks/commit-msg"

if [[ ! -d "$HOOKS_DIR" ]]; then
  echo "No .git/hooks directory found. Are you in a git repo?" >&2
  exit 1
fi

chmod +x "$PRE_COMMIT_SRC" "$PRE_PUSH_SRC" "$COMMIT_MSG_SRC"
ln -sf "$PRE_COMMIT_SRC" "$HOOKS_DIR/pre-commit"
ln -sf "$PRE_PUSH_SRC" "$HOOKS_DIR/pre-push"
ln -sf "$COMMIT_MSG_SRC" "$HOOKS_DIR/commit-msg"

echo "Installed pre-commit hook -> $HOOKS_DIR/pre-commit"
echo "Installed pre-push hook   -> $HOOKS_DIR/pre-push"
echo "Installed commit-msg hook -> $HOOKS_DIR/commit-msg"
