#!/usr/bin/env python3
"""Fail closed when the low-memory signed Windows release contract drifts."""

from pathlib import Path


WORKFLOW = Path(__file__).resolve().parents[1] / ".github/workflows/release.yml"
STEP = "      - name: Build binaries-only release (windows-pc, GPG-detached .zip)"
NEXT_STEP = "      - name: Smoke extracted release archive"


def main() -> int:
    workflow = WORKFLOW.read_text(encoding="utf-8")
    try:
        windows_step = workflow.split(STEP, 1)[1].split(NEXT_STEP, 1)[0]
    except IndexError as exc:
        raise SystemExit("Windows release build step is missing or renamed") from exc

    required = {
        'CARGO_BUILD_JOBS: "1"': "serialized Cargo compilation",
        'CARGO_PROFILE_RELEASE_LTO: "false"': "bounded final-link memory",
        'NO_DEFAULT_FEATURES: "1"': "LLVM-free Windows feature set",
        "FEATURES: app,cloud-embedder": "Windows cloud-only embedder contract",
    }
    missing = [description for token, description in required.items() if token not in windows_step]
    if missing:
        raise SystemExit("Windows release contract missing: " + ", ".join(missing))

    print("Windows release contract: serialized, non-LTO, LLVM-free MSVC bundle")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
