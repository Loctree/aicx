#!/usr/bin/env python3
"""Synchronize AICX release surfaces around a single semantic version.

This script keeps the release-visible version honest across:

- Cargo.toml
- distribution/npm package manifests
- versioned install / publish examples in docs
- CHANGELOG-derived release notes extraction

Usage:
    python3 tools/release_sync.py bump patch
    python3 tools/release_sync.py bump 0.7.0
    python3 tools/release_sync.py check
    python3 tools/release_sync.py check 0.7.0
    python3 tools/release_sync.py notes
    python3 tools/release_sync.py notes 0.7.0 --output dist/release-notes.md
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
import tomllib
from dataclasses import dataclass

ROOT = pathlib.Path(__file__).resolve().parent.parent
CARGO_TOML = ROOT / "Cargo.toml"
CHANGELOG = ROOT / "CHANGELOG.md"
SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+$")
VERSION_HEADER_RE = re.compile(r"^## \[(?P<version>v?\d+\.\d+\.\d+)](?: - .+)?$", re.MULTILINE)


@dataclass(frozen=True)
class TextSurface:
    path: pathlib.Path
    transforms: tuple


def root_relative(path: pathlib.Path) -> str:
    try:
        return path.relative_to(ROOT).as_posix()
    except ValueError:
        return path.as_posix()


def read_cargo_version() -> str:
    with CARGO_TOML.open("rb") as fh:
        return tomllib.load(fh)["package"]["version"]


def compute_bumped_version(current: str, target: str) -> str:
    parts = [int(part) for part in current.split(".")]
    if target == "patch":
        parts[2] += 1
    elif target == "minor":
        parts[1] += 1
        parts[2] = 0
    elif target == "major":
        parts[0] += 1
        parts[1] = 0
        parts[2] = 0
    elif SEMVER_RE.fullmatch(target):
        return target
    else:
        raise SystemExit(
            f"Invalid VERSION: {target!r}. Use patch|minor|major|x.y.z"
        )
    return ".".join(str(part) for part in parts)


def replace_once(
    text: str,
    pattern: str,
    replacement: object,
    *,
    flags: int = 0,
    label: str,
) -> str:
    new_text, count = re.subn(pattern, replacement, text, count=1, flags=flags)
    if count != 1:
        raise SystemExit(f"Could not sync {label}")
    return new_text


def make_text_surfaces(version: str) -> tuple[TextSurface, ...]:
    return (
        TextSurface(
            path=ROOT / "README.md",
            transforms=(
                (
                    r"(AICX_RELEASE_TAG=v)\d+\.\d+\.\d+",
                    rf"\g<1>{version}",
                    0,
                    "README install example",
                ),
            ),
        ),
        TextSurface(
            path=ROOT / "docs" / "RELEASES.md",
            transforms=(
                (
                    r"(AICX_RELEASE_TAG=v)\d+\.\d+\.\d+",
                    rf"\g<1>{version}",
                    0,
                    "docs/RELEASES install example",
                ),
                (
                    r'git tag -a v\d+\.\d+\.\d+ -m "(?:ai-contexters|aicx) v\d+\.\d+\.\d+"',
                    f'git tag -a v{version} -m "aicx v{version}"',
                    0,
                    "docs/RELEASES tag example",
                ),
                (
                    r"git push origin v\d+\.\d+\.\d+",
                    f"git push origin v{version}",
                    0,
                    "docs/RELEASES push example",
                ),
            ),
        ),
        TextSurface(
            path=ROOT / "distribution" / "npm" / "README.md",
            transforms=(
                (
                    r"(node distribution/npm/sync-version\.mjs )\d+\.\d+\.\d+",
                    rf"\g<1>{version}",
                    0,
                    "distribution/npm/README sync-version example",
                ),
                (
                    r"(node distribution/npm/sync-version\.mjs --check )\d+\.\d+\.\d+",
                    rf"\g<1>{version}",
                    0,
                    "distribution/npm/README sync-version check example",
                ),
            ),
        ),
        TextSurface(
            path=ROOT / "distribution" / "npm" / "PUBLISHING.md",
            transforms=(
                (
                    r"(node distribution/npm/sync-version\.mjs )\d+\.\d+\.\d+",
                    rf"\g<1>{version}",
                    0,
                    "distribution/npm/PUBLISHING sync-version example",
                ),
                (
                    r"(node distribution/npm/sync-version\.mjs --check )\d+\.\d+\.\d+",
                    rf"\g<1>{version}",
                    0,
                    "distribution/npm/PUBLISHING sync-version check example",
                ),
                (
                    r"https://github\.com/Loctree/aicx/releases/download/v\d+\.\d+\.\d+/aicx-v\d+\.\d+\.\d+-aarch64-apple-darwin\.zip",
                    f"https://github.com/Loctree/aicx/releases/download/v{version}/aicx-v{version}-aarch64-apple-darwin.zip",
                    0,
                    "distribution/npm/PUBLISHING release zip URL example",
                ),
                (
                    r"https://github\.com/Loctree/aicx/releases/download/v\d+\.\d+\.\d+/aicx-v\d+\.\d+\.\d+-aarch64-apple-darwin\.zip\.sha256",
                    f"https://github.com/Loctree/aicx/releases/download/v{version}/aicx-v{version}-aarch64-apple-darwin.zip.sha256",
                    0,
                    "distribution/npm/PUBLISHING checksum URL example",
                ),
            ),
        ),
    )


def transform_cargo_toml(text: str, version: str) -> str:
    return replace_once(
        text,
        r'^version = "[^"]*"',
        f'version = "{version}"',
        flags=re.MULTILINE,
        label="Cargo.toml package version",
    )


def npm_manifest_paths() -> list[pathlib.Path]:
    root = ROOT / "distribution" / "npm"
    manifests: list[pathlib.Path] = []
    if not root.is_dir():
        return manifests

    for wrapper_dir in sorted(root.iterdir()):
        if not wrapper_dir.is_dir():
            continue
        wrapper_manifest = wrapper_dir / "package.json"
        if wrapper_manifest.is_file():
            manifests.append(wrapper_manifest)
        platform_root = wrapper_dir / "platform-packages"
        if not platform_root.is_dir():
            continue
        for platform_dir in sorted(platform_root.iterdir()):
            manifest = platform_dir / "package.json"
            if manifest.is_file():
                manifests.append(manifest)
    return manifests


def transform_package_manifest(text: str, version: str) -> str:
    package = json.loads(text)
    package["version"] = version
    optional = package.get("optionalDependencies")
    if isinstance(optional, dict):
        for name in list(optional):
            if name.startswith("@loctree/"):
                optional[name] = version
    return f"{json.dumps(package, indent=2)}\n"


def sync_versions(version: str, *, write: bool) -> list[str]:
    changed: list[str] = []

    cargo_original = CARGO_TOML.read_text(encoding="utf-8")
    cargo_updated = transform_cargo_toml(cargo_original, version)
    if cargo_original != cargo_updated:
        changed.append(root_relative(CARGO_TOML))
        if write:
            CARGO_TOML.write_text(cargo_updated, encoding="utf-8")

    for manifest_path in npm_manifest_paths():
        original = manifest_path.read_text(encoding="utf-8")
        updated = transform_package_manifest(original, version)
        if original == updated:
            continue
        changed.append(root_relative(manifest_path))
        if write:
            manifest_path.write_text(updated, encoding="utf-8")

    for surface in make_text_surfaces(version):
        if not surface.path.is_file():
            continue
        original = surface.path.read_text(encoding="utf-8")
        updated = original
        for pattern, replacement, flags, label in surface.transforms:
            updated = replace_once(updated, pattern, replacement, flags=flags, label=label)
        if original == updated:
            continue
        changed.append(root_relative(surface.path))
        if write:
            surface.path.write_text(updated, encoding="utf-8")

    return changed


def version_header_regex(version: str) -> re.Pattern[str]:
    escaped = re.escape(version)
    escaped_v = re.escape(f"v{version}")
    return re.compile(
        rf"^## \[(?:{escaped}|{escaped_v})](?: - .+)?$",
        re.MULTILINE,
    )


def extract_version_notes(version: str) -> str:
    text = CHANGELOG.read_text(encoding="utf-8")
    match = version_header_regex(version).search(text)
    if match is None:
        raise SystemExit(
            f"CHANGELOG.md does not contain a dedicated section for version {version}"
        )

    body_start = match.end()
    next_match = VERSION_HEADER_RE.search(text, body_start)
    body = text[body_start : next_match.start() if next_match else len(text)].strip()
    if not body:
        return "No detailed release notes were recorded for this version."
    return body


def command_bump(args: argparse.Namespace) -> int:
    current = read_cargo_version()
    new_version = compute_bumped_version(current, args.target)
    changed = sync_versions(new_version, write=True)
    if not changed:
        print(f"Release surfaces already synced to {new_version}")
        return 0

    print(f"Release surfaces synced: {current} -> {new_version}")
    for path in changed:
        print(f"  - {path}")
    return 0


def command_check(args: argparse.Namespace) -> int:
    expected = args.version or read_cargo_version()
    changed = sync_versions(expected, write=False)

    errors: list[str] = []
    changelog_text = CHANGELOG.read_text(encoding="utf-8")
    if "## [Unreleased]" not in changelog_text:
        errors.append("CHANGELOG.md is missing '## [Unreleased]'")
    if args.require_version_section and version_header_regex(expected).search(changelog_text) is None:
        errors.append(f"CHANGELOG.md is missing dedicated section for {expected}")

    if changed:
        errors.append(
            "Release surfaces are out of sync for "
            f"{expected}: {', '.join(changed)}"
        )

    if errors:
        for error in errors:
            print(error, file=sys.stderr)
        return 1

    print(f"Release surfaces are synced to {expected}")
    if version_header_regex(expected).search(changelog_text):
        print(f"CHANGELOG section for {expected}: present")
    else:
        print(f"CHANGELOG section for {expected}: not yet closed (Unreleased still open)")
    return 0


def command_notes(args: argparse.Namespace) -> int:
    version = args.version or read_cargo_version()
    notes = extract_version_notes(version)
    if args.output:
        output_path = pathlib.Path(args.output)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(notes + "\n", encoding="utf-8")
        print(f"Wrote release notes for {version} to {output_path}")
        return 0

    print(notes)
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    bump_parser = subparsers.add_parser("bump", help="Bump and sync release surfaces")
    bump_parser.add_argument("target", help="patch|minor|major|x.y.z")
    bump_parser.set_defaults(func=command_bump)

    check_parser = subparsers.add_parser("check", help="Verify release surfaces are synced")
    check_parser.add_argument("version", nargs="?", help="Expected version; defaults to Cargo.toml")
    check_parser.add_argument(
        "--require-version-section",
        action="store_true",
        help="Fail if CHANGELOG.md does not yet contain a dedicated section for this version",
    )
    check_parser.set_defaults(func=command_check)

    notes_parser = subparsers.add_parser("notes", help="Extract release notes from CHANGELOG.md")
    notes_parser.add_argument("version", nargs="?", help="Version to extract; defaults to Cargo.toml")
    notes_parser.add_argument("--output", help="Write notes to a file instead of stdout")
    notes_parser.set_defaults(func=command_notes)

    return parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
