#!/usr/bin/env python3
"""Static guardrails for the script-free npm publication workflow."""

import json
from pathlib import Path


ROOT = Path(__file__).parents[1]
WORKFLOW = ROOT / ".github/workflows/npm-publish.yml"
WRAPPER = ROOT / "distribution/npm/aicx/package.json"
PLATFORM_ROOT = ROOT / "distribution/npm/aicx/platform-packages"
PLATFORMS = ("darwin-arm64", "linux-x64-gnu", "win32-x64-gnu")
FORBIDDEN = ("preinstall", "install", "postinstall", "prepare")


def assert_script_free(path: Path) -> None:
    package = json.loads(path.read_text(encoding="utf-8"))
    scripts = package.get("scripts", {})
    for name in FORBIDDEN:
        if name in scripts:
            raise SystemExit(f"{path} contains forbidden lifecycle script {name}")


def main() -> None:
    assert_script_free(WRAPPER)
    for platform in PLATFORMS:
        assert_script_free(PLATFORM_ROOT / platform / "package.json")

    workflow = WORKFLOW.read_text(encoding="utf-8")
    required = (
        "pack-platform-packages:",
        "runs_on: macos-15",
        "runs_on: ubuntu-latest",
        "runs_on: windows-latest",
        "stage-platform-package.mjs",
        "verify-metadata.mjs",
        "npm publish npm-package/*.tgz",
        'npm view "${package}@${RELEASE_VERSION}" version',
        "@loctree/aicx-darwin-arm64",
        "@loctree/aicx-linux-x64-gnu",
        "@loctree/aicx-win32-x64-gnu",
        "npm@11.17.0",
        "install_mode: [normal, ignore-scripts]",
        'grep -qi "allow-scripts"',
        '"${bin_dir}/aicx" config inspect --json',
    )
    for contract in required:
        if contract not in workflow:
            raise SystemExit(f"npm publish workflow lost contract: {contract}")

    publish_tail = workflow.split("publish-platform-packages:", 1)[1]
    if "working-directory: distribution/npm/aicx/platform-packages" in publish_tail:
        raise SystemExit("publish jobs must consume prepacked tgz artifacts, not mutable directories")
    print("npm zero-lifecycle publish contract passed")


if __name__ == "__main__":
    main()
