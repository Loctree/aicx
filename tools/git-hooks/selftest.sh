#!/bin/sh
set -eu

source_root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
test_root=$(mktemp -d "${TMPDIR:-/tmp}/aicx-embargo-selftest.XXXXXX")
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

new_repo() {
  repo=$1
  mkdir -p "$repo/tools/git-hooks" "$repo/src" "$repo/.vibecrafted"
  cp "$source_root"/tools/git-hooks/* "$repo/tools/git-hooks/"
  chmod +x "$repo"/tools/git-hooks/*
  (
    cd "$repo"
    git init -q -b feat/one-taxonomy-fusion
    git config user.name selftest
    git config user.email selftest@example.invalid
    printf '%s\n' '[package]' 'name = "embargo-selftest"' 'version = "0.1.0"' 'edition = "2021"' >Cargo.toml
    printf '%s\n' 'fn main() { missing_symbol(); }' >src/main.rs
    printf '%s\n' 'fmt-check:' '	@true' 'semgrep:' '	@true' 'clippy:' '	@false' 'test:' '	@false' >Makefile
    git add Cargo.toml Makefile src/main.rs tools/git-hooks
  )
}

write_marker() {
  repo=$1
  branch=$(git -C "$repo" branch --show-current)
  marker="$repo/.vibecrafted/embargo.toml"
  {
    printf '%s\n' 'plan_id = "aicx-one-taxonomy-fusion-260827"'
    printf '%s\n' 'phase = "W1"'
    printf '%s\n' 'deferred = ["cargo check", "cargo clippy", "cargo test"]'
    printf '%s\n' 'attestation = "open"'
    printf 'branch = "%s"\n' "$branch"
    printf '%s\n' 'recovery_ref = "refs/heads/embargo/aicx-one-taxonomy-fusion-260827"'
    printf '%s\n' 'signed_by = ""'
    printf '%s\n' 'commit = ""'
  } >"$marker"
}

commit_reject="$test_root/commit-reject"
new_repo "$commit_reject"
if (cd "$commit_reject" && tools/git-hooks/pre-commit >/dev/null 2>&1); then
  echo "FAIL commit without marker: invalid Rust was accepted" >&2
  exit 1
fi
echo "ok commit without marker: cargo check rejects invalid Rust"

commit_accept="$test_root/commit-accept"
new_repo "$commit_accept"
write_marker "$commit_accept"
(cd "$commit_accept" && tools/git-hooks/pre-commit >/dev/null)
echo "ok commit with open W1 marker: cargo check alone is deferred"

push_reject="$test_root/push-reject"
new_repo "$push_reject"
write_marker "$push_reject"
head_sha=$(git -C "$push_reject" write-tree)
if printf 'refs/heads/feat/one-taxonomy-fusion %s refs/heads/main %040d\n' "$head_sha" 0 |
  (cd "$push_reject" && tools/git-hooks/pre-push origin example.invalid >/dev/null 2>&1); then
  echo "FAIL push under marker: trunk destination was accepted" >&2
  exit 1
fi
echo "ok push with marker to trunk: rejected before gates"

push_accept="$test_root/push-accept"
new_repo "$push_accept"
write_marker "$push_accept"
head_sha=$(git -C "$push_accept" write-tree)
printf 'refs/heads/feat/one-taxonomy-fusion %s refs/heads/embargo/aicx-one-taxonomy-fusion-260827 %040d\n' "$head_sha" 0 |
  (cd "$push_accept" && tools/git-hooks/pre-push origin example.invalid >/dev/null)
echo "ok push with marker to recovery ref: light gate accepted"

invalid_marker="$test_root/invalid-marker"
new_repo "$invalid_marker"
write_marker "$invalid_marker"
printf '%s\n' 'unexpected = true' >"$invalid_marker/.vibecrafted/embargo.toml"
if (cd "$invalid_marker" && tools/git-hooks/pre-commit >/dev/null 2>&1); then
  echo "FAIL malformed marker: hook did not fail closed" >&2
  exit 1
fi
status=0
(cd "$invalid_marker" && tools/git-hooks/embargo.sh status >/dev/null 2>&1) || status=$?
[ "$status" -eq 2 ] || {
  echo "FAIL malformed marker: expected status 2, got $status" >&2
  exit 1
}
echo "ok malformed marker: rejected fail-closed"

install_repo="$test_root/install"
new_repo "$install_repo"
(cd "$install_repo" && tools/git-hooks/install.sh >/dev/null && tools/git-hooks/install.sh >/dev/null)
[ "$(git -C "$install_repo" config --get core.hooksPath)" = "tools/git-hooks" ]
echo "ok install: idempotent core.hooksPath configuration"

echo "git-hooks selftest: all scenarios passed"
