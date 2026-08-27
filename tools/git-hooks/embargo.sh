#!/bin/sh
# Shared validator for the repository-owned compile-embargo marker.
#
# Exit codes for `status`:
#   0 - an open W1/W2 embargo applies to the current branch
#   1 - no active embargo applies (marker absent, closed, or for another branch)
#   2 - marker exists but violates the contract
set -eu

command_name=${1:-status}
repo_root=$(git rev-parse --show-toplevel 2>/dev/null) || {
  echo "compile-embargo: not inside a Git repository" >&2
  exit 2
}
marker="$repo_root/.vibecrafted/embargo.toml"

case "$command_name" in
  status|recovery-ref) ;;
  *)
    echo "compile-embargo: usage: embargo.sh {status|recovery-ref}" >&2
    exit 2
    ;;
esac

[ -f "$marker" ] || exit 1
branch=$(git branch --show-current 2>/dev/null || true)

set +e
python3 - "$marker" "$branch" "$command_name" <<'PY'
import re
import sys
import tomllib

marker_path, current_branch, command = sys.argv[1:]

try:
    with open(marker_path, "rb") as handle:
        marker = tomllib.load(handle)
except (OSError, tomllib.TOMLDecodeError) as exc:
    print(f"compile-embargo: invalid marker: {exc}", file=sys.stderr)
    raise SystemExit(2)

required = {
    "plan_id": str,
    "phase": str,
    "deferred": list,
    "attestation": str,
    "branch": str,
    "recovery_ref": str,
    "signed_by": str,
    "commit": str,
}
errors = []
for key, expected_type in required.items():
    value = marker.get(key)
    if type(value) is not expected_type:
        errors.append(f"{key} must be {expected_type.__name__}")

if errors:
    print("compile-embargo: invalid marker: " + "; ".join(errors), file=sys.stderr)
    raise SystemExit(2)

plan_id = marker["plan_id"]
phase = marker["phase"]
deferred = marker["deferred"]
attestation = marker["attestation"]
declared_branch = marker["branch"]
recovery_ref = marker["recovery_ref"]
signed_by = marker["signed_by"]
commit = marker["commit"]

if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", plan_id):
    errors.append("plan_id must be a non-empty lowercase slug")
if phase not in {"W1", "W2"}:
    errors.append("phase must be W1 or W2")
allowed_deferred = {"cargo check", "cargo clippy", "cargo test"}
if any(type(item) is not str for item in deferred):
    errors.append("deferred entries must be strings")
elif set(deferred) != allowed_deferred or len(deferred) != len(allowed_deferred):
    errors.append("deferred must list cargo check, cargo clippy, and cargo test exactly once")
if not declared_branch or declared_branch.startswith("refs/"):
    errors.append("branch must be a non-empty short branch name")
expected_ref = f"refs/heads/embargo/{plan_id}"
if recovery_ref != expected_ref:
    errors.append(f"recovery_ref must be {expected_ref}")

if attestation == "open":
    if signed_by or commit:
        errors.append("open marker requires empty signed_by and commit")
elif attestation == "W2_STRUCTURALLY_CLOSED":
    if phase != "W2":
        errors.append("W2_STRUCTURALLY_CLOSED requires phase W2")
    if not signed_by:
        errors.append("closed marker requires signed_by")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        errors.append("closed marker requires a full lowercase commit SHA")
else:
    errors.append("attestation must be open or W2_STRUCTURALLY_CLOSED")

if errors:
    print("compile-embargo: invalid marker: " + "; ".join(errors), file=sys.stderr)
    raise SystemExit(2)

# A valid marker for another branch is deliberately inactive, not corrupt.
# That preserves ordinary hooks on every branch not explicitly named.
if declared_branch != current_branch or attestation != "open":
    raise SystemExit(10)

if command == "recovery-ref":
    print(recovery_ref)
raise SystemExit(0)
PY
status=$?
set -e

case "$status" in
  0) exit 0 ;;
  10) exit 1 ;;
  2) exit 2 ;;
  *)
    echo "compile-embargo: validator failed unexpectedly (status $status)" >&2
    exit 2
    ;;
esac
