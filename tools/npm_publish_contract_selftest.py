#!/usr/bin/env python3
"""Static guardrails for the script-free npm publication workflow."""

import json
from pathlib import Path


ROOT = Path(__file__).parents[1]
WORKFLOW = ROOT / ".github/workflows/npm-publish.yml"
RELEASE_WORKFLOW = ROOT / ".github/workflows/release.yml"
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

    wrapper_source = (WRAPPER.parent / "index.js").read_text(encoding="utf-8")
    wrapper_readme = (WRAPPER.parent / "README.md").read_text(encoding="utf-8")
    if "aicx doctor --repair-runtime" not in wrapper_source:
        raise SystemExit("npm wrapper lost the script-free runtime migration hint")
    if "aicx doctor --repair-runtime" not in wrapper_readme:
        raise SystemExit("npm README lost the runtime migration command")

    workflow = WORKFLOW.read_text(encoding="utf-8")
    required = (
        # Job markers first: the section splits below index on them, and a
        # missing marker must read as a lost contract, not an IndexError.
        "pack-platform-packages:",
        "pack-wrapper:",
        "publish-platform-packages:",
        "publish-wrapper:",
        "runs_on: macos-15",
        "runs_on: ubuntu-latest",
        "runs_on: windows-latest",
        "stage-platform-package.mjs",
        "verify-metadata.mjs",
        "npm publish ./npm-package/*.tgz",
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

    pack_platform = workflow.split("pack-platform-packages:", 1)[1].split("pack-wrapper:", 1)[0]
    pack_wrapper = workflow.split("pack-wrapper:", 1)[1].split("publish-platform-packages:", 1)[0]
    for job_name, job_source in (
        ("pack-platform-packages", pack_platform),
        ("pack-wrapper", pack_wrapper),
    ):
        if "ref: ${{ needs.verify.outputs.release_tag }}" in job_source:
            raise SystemExit(
                f"{job_name} must use the dispatched workflow revision so post-release fixes reach retries"
            )

    publish_tail = workflow.split("publish-platform-packages:", 1)[1]
    if "working-directory: distribution/npm/aicx/platform-packages" in publish_tail:
        raise SystemExit("publish jobs must consume prepacked tgz artifacts, not mutable directories")

    # release.yml creates the GitHub Release with GITHUB_TOKEN, and GitHub never
    # starts workflows from events made with that token, so `release: published`
    # alone leaves npm behind (v0.13.0, 2026-09-02). The chain must be explicit.
    if "workflow_call:" not in workflow:
        raise SystemExit("npm publish workflow must be callable from release.yml (workflow_call)")
    release_workflow = RELEASE_WORKFLOW.read_text(encoding="utf-8")
    if "uses: ./.github/workflows/npm-publish.yml" not in release_workflow:
        raise SystemExit("release.yml lost the npm-publish chain job")
    if "needs: [verify, github-release]" not in release_workflow:
        raise SystemExit("npm publish must run after the signed GitHub Release exists")

    # npm trusted publishing (OIDC): the run authenticates itself, so no long-lived
    # token may appear, and both the publish jobs and the caller job must be able
    # to mint an id-token.
    if "NPM_TOKEN" in workflow or "NODE_AUTH_TOKEN" in workflow:
        raise SystemExit("npm publish workflow must not use a long-lived npm token (OIDC trusted publishing)")
    if workflow.count("id-token: write") < 2:
        raise SystemExit("both npm publish jobs must request id-token: write")
    if "npm-publish:" not in release_workflow:
        raise SystemExit("release.yml lost the npm-publish job marker")
    chain_job = release_workflow.split("npm-publish:", 1)[1]
    if "id-token: write" not in chain_job:
        raise SystemExit("release.yml npm-publish job must grant id-token: write to the reusable workflow")
    print("npm zero-lifecycle publish contract passed")


if __name__ == "__main__":
    main()
