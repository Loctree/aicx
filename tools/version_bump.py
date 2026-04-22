#!/usr/bin/env python3
"""Compatibility wrapper for the centralized release sync entry point.

Usage:
    python3 tools/version_bump.py patch
    python3 tools/version_bump.py minor
    python3 tools/version_bump.py major
    python3 tools/version_bump.py 1.2.3
"""

from __future__ import annotations

import pathlib
import subprocess
import sys


def main() -> int:
    if len(sys.argv) != 2:
        print(
            "Usage: version_bump.py {patch|minor|major|x.y.z}",
            file=sys.stderr,
        )
        return 1

    release_sync = pathlib.Path(__file__).with_name("release_sync.py")
    if not release_sync.is_file():
        print(
            f"release_sync.py not found at {release_sync.resolve()}",
            file=sys.stderr,
        )
        return 1

    command = [sys.executable, str(release_sync), "bump", sys.argv[1]]
    return subprocess.call(command)


if __name__ == "__main__":
    sys.exit(main())
