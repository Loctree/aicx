#!/bin/sh
set -eu

repo_root=$(git rev-parse --show-toplevel 2>/dev/null) || {
  echo "git-hooks install: not inside a Git repository" >&2
  exit 1
}
cd "$repo_root"

for hook in pre-commit pre-push commit-msg embargo.sh selftest.sh install.sh; do
  [ -f "tools/git-hooks/$hook" ] || {
    echo "git-hooks install: missing tools/git-hooks/$hook" >&2
    exit 1
  }
done

chmod +x tools/git-hooks/pre-commit tools/git-hooks/pre-push \
  tools/git-hooks/commit-msg tools/git-hooks/embargo.sh \
  tools/git-hooks/selftest.sh tools/git-hooks/install.sh
git config core.hooksPath tools/git-hooks

configured=$(git config --get core.hooksPath)
[ "$configured" = "tools/git-hooks" ] || {
  echo "git-hooks install: core.hooksPath verification failed" >&2
  exit 1
}
echo "Repository hooks active: $configured"
