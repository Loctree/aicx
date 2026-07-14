#!/usr/bin/env python3
"""Remove legacy parent extract options from generated shell completions."""

from __future__ import annotations

from pathlib import Path


BASH_LEGACY_EXTRACT_OPTS = {
    "-o",
    "-p",
    "-H",
    "--agent",
    "--format",
    "--session",
    "--output",
    "--project",
    "--hours",
    "--conversation",
    "--user-only",
    "--include-assistant",
    "--max-message-chars",
}


def canonicalize_bash(path: Path) -> None:
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    output: list[str] = []
    in_extract = False
    skipping_case = False
    for line in lines:
        stripped = line.strip()
        if stripped == "aicx__subcmd__extract)":
            in_extract = True
        elif in_extract and stripped.startswith("aicx__subcmd__") and stripped != "aicx__subcmd__extract)":
            in_extract = False

        if in_extract and 'opts="' in line:
            prefix, rest = line.split('opts="', 1)
            opts, suffix = rest.split('"', 1)
            kept = [opt for opt in opts.split() if opt not in BASH_LEGACY_EXTRACT_OPTS]
            output.append(f'{prefix}opts="{" ".join(kept)}"{suffix}')
            continue

        if in_extract and not skipping_case:
            for opt in BASH_LEGACY_EXTRACT_OPTS:
                if stripped == f"{opt})":
                    skipping_case = True
                    break
            if skipping_case:
                continue

        if skipping_case:
            if stripped == ";;":
                skipping_case = False
            continue

        output.append(line)
    path.write_text("".join(output), encoding="utf-8")


def canonicalize_fish(path: Path) -> None:
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    output = []
    parent_extract = "__fish_aicx_using_subcommand extract; and not __fish_seen_subcommand_from"
    for line in lines:
        if parent_extract in line:
            if any(f" -l {opt.removeprefix('--')}" in line for opt in BASH_LEGACY_EXTRACT_OPTS if opt.startswith("--")):
                continue
            if any(f" -s {opt.removeprefix('-')}" in line for opt in BASH_LEGACY_EXTRACT_OPTS if opt.startswith("-") and not opt.startswith("--")):
                continue
        output.append(line)
    path.write_text("".join(output), encoding="utf-8")


def main() -> int:
    canonicalize_bash(Path("completions/aicx.bash"))
    canonicalize_fish(Path("completions/aicx.fish"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())