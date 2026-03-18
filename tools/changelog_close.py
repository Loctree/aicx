#!/usr/bin/env python3
"""Close CHANGELOG.md `## [Unreleased]` section with the current Cargo.toml version.

Keeps `## [Unreleased]` in place (empty) so the next cycle has a landing slot.
Idempotent: if the current version already has a dedicated section, exits 0 no-op.
"""

from __future__ import annotations

import datetime
import pathlib
import sys
import tomllib

UNRELEASED = "## [Unreleased]"


def main() -> int:
    cargo_path = pathlib.Path("Cargo.toml")
    changelog_path = pathlib.Path("CHANGELOG.md")

    if not cargo_path.is_file():
        print(f"Cargo.toml not found at {cargo_path.resolve()}", file=sys.stderr)
        return 1
    if not changelog_path.is_file():
        print(
            f"CHANGELOG.md not found at {changelog_path.resolve()}",
            file=sys.stderr,
        )
        return 1

    with cargo_path.open("rb") as fh:
        version = tomllib.load(fh)["package"]["version"]

    text = changelog_path.read_text(encoding="utf-8")
    today = datetime.date.today().isoformat()

    if f"## [{version}]" in text:
        print(
            f"CHANGELOG already has '## [{version}]' section; nothing to close."
        )
        return 0

    if UNRELEASED not in text:
        print(
            "CHANGELOG.md is missing '## [Unreleased]'; refusing to guess.",
            file=sys.stderr,
        )
        return 1

    replacement = f"{UNRELEASED}\n\n## [{version}] - {today}"
    new_text = text.replace(UNRELEASED, replacement, 1)
    changelog_path.write_text(new_text, encoding="utf-8")
    print(f"CHANGELOG closed: Unreleased -> [{version}] - {today}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
