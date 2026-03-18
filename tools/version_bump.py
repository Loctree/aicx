#!/usr/bin/env python3
"""Bump ai-contexters package version in Cargo.toml.

Usage:
    python3 tools/version_bump.py patch
    python3 tools/version_bump.py minor
    python3 tools/version_bump.py major
    python3 tools/version_bump.py 1.2.3
"""

from __future__ import annotations

import pathlib
import re
import sys
import tomllib

SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+$")


def bump(current: str, target: str) -> str:
    parts = [int(p) for p in current.split(".")]
    if target == "patch":
        parts[2] += 1
    elif target == "minor":
        parts[1] += 1
        parts[2] = 0
    elif target == "major":
        parts[0] += 1
        parts[1] = 0
        parts[2] = 0
    elif SEMVER_RE.match(target):
        return target
    else:
        raise SystemExit(
            f"Invalid VERSION: {target!r}. Use patch|minor|major|x.y.z"
        )
    return ".".join(str(p) for p in parts)


def main() -> int:
    if len(sys.argv) != 2:
        print(
            "Usage: version_bump.py {patch|minor|major|x.y.z}",
            file=sys.stderr,
        )
        return 1

    cargo_path = pathlib.Path("Cargo.toml")
    if not cargo_path.is_file():
        print(f"Cargo.toml not found at {cargo_path.resolve()}", file=sys.stderr)
        return 1

    with cargo_path.open("rb") as fh:
        current = tomllib.load(fh)["package"]["version"]

    new_version = bump(current, sys.argv[1])
    if new_version == current:
        print(f"Cargo.toml already at {current}; no change.")
        return 0

    text = cargo_path.read_text(encoding="utf-8")
    new_text, n = re.subn(
        r'^version = "[^"]*"',
        f'version = "{new_version}"',
        text,
        count=1,
        flags=re.MULTILINE,
    )
    if n != 1:
        print(
            "Could not find top-level `version = \"...\"` line in Cargo.toml",
            file=sys.stderr,
        )
        return 1

    cargo_path.write_text(new_text, encoding="utf-8")
    print(f"Cargo.toml: {current} -> {new_version}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
