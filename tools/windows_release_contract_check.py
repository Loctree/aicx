#!/usr/bin/env python3
"""Fail closed when the hosted Windows release contract (LLVM-free, LTO off) drifts."""

from pathlib import Path


WORKFLOW = Path(__file__).resolve().parents[1] / ".github/workflows/release.yml"
STEP = "      - name: Build binaries-only release (windows-x64, GPG-detached .zip)"
NEXT_STEP = "      - name: Smoke extracted release archive"


def main() -> int:
    workflow = WORKFLOW.read_text(encoding="utf-8")
    try:
        windows_step = workflow.split(STEP, 1)[1].split(NEXT_STEP, 1)[0]
    except IndexError as exc:
        raise SystemExit("Windows release build step is missing or renamed") from exc

    required = {
        'NO_DEFAULT_FEATURES: "1"': "LLVM-free Windows feature set",
        "FEATURES: app,cloud-embedder": "Windows cloud-only embedder contract",
        'CARGO_PROFILE_RELEASE_LTO: "false"': "LTO off for Windows slim bundle parity",
    }
    missing = [description for token, description in required.items() if token not in windows_step]
    if missing:
        raise SystemExit("Windows release contract missing: " + ", ".join(missing))

    print("Windows release contract: hosted runner, LLVM-free, LTO-off MSVC bundle")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
