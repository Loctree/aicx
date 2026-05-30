#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT_DIR"

read_version() {
  local path="$1"
  python3 - "$path" <<'PY'
import json
import sys
import tomllib
from pathlib import Path

path = Path(sys.argv[1])
if path.suffix == ".json":
    print(json.loads(path.read_text(encoding="utf-8"))["version"])
else:
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    print(data.get("workspace", {}).get("package", {}).get("version") or data["package"]["version"])
PY
}

workspace_version=$(read_version Cargo.toml)
err=0

echo "Release channel versions:"

check_version() {
  local name="$1"
  local version="$2"
  printf '  %s -> %s\n' "$name" "$version"
  if [ "$version" != "$workspace_version" ]; then
    printf '    mismatch vs workspace (%s)\n' "$workspace_version"
    err=1
  fi
}

check_version "workspace" "$workspace_version"
check_version "npm-main" "$(read_version distribution/npm/aicx/package.json)"
check_version "npm-darwin" "$(read_version distribution/npm/aicx/platform-packages/darwin-arm64/package.json)"
check_version "npm-linux" "$(read_version distribution/npm/aicx/platform-packages/linux-x64-gnu/package.json)"
check_version "npm-win" "$(read_version distribution/npm/aicx/platform-packages/win32-x64-gnu/package.json)"

optional_versions=$(
  python3 - <<'PY'
import json
from pathlib import Path

data = json.loads(Path("distribution/npm/aicx/package.json").read_text(encoding="utf-8"))
for name, version in sorted(data.get("optionalDependencies", {}).items()):
    if name.startswith("@loctree/aicx-"):
        print(f"{name}:{version}")
PY
)

while IFS=: read -r name version; do
  [ -n "${name:-}" ] || continue
  check_version "optional:${name}" "$version"
done <<< "$optional_versions"

if [ "$err" -ne 0 ]; then
  echo ""
  echo "ERROR: Version mismatch between release channels. Sync all channels to $workspace_version before release."
  exit 1
fi

echo ""
echo "All release channels in sync: $workspace_version"
