#!/usr/bin/env python3
"""Fail if npm cold smoke stops waiting for every package it installs."""

from pathlib import Path


WORKFLOW = Path(__file__).parents[1] / ".github/workflows/npm-publish.yml"
PACKAGES = (
    "@loctree/aicx",
    "@loctree/aicx-darwin-arm64",
    "@loctree/aicx-linux-x64-gnu",
    "@loctree/aicx-win32-x64-gnu",
)


def main() -> None:
    workflow = WORKFLOW.read_text(encoding="utf-8")
    marker = "- name: Wait for wrapper and platform versions"
    if marker not in workflow:
        raise SystemExit("npm publish workflow lost the propagation gate")
    gate = workflow.split(marker, 1)[1]
    for package in PACKAGES:
        if f'"{package}"' not in gate:
            raise SystemExit(f"propagation gate does not wait for {package}")
    required = (
        'for package in "${packages[@]}"',
        'npm view "${package}@${RELEASE_VERSION}" version',
        'if [[ "${#missing[@]}" == 0 ]]',
    )
    for contract in required:
        if contract not in gate:
            raise SystemExit(f"propagation gate lost contract: {contract}")
    print("npm publish propagation contract passed")


if __name__ == "__main__":
    main()
