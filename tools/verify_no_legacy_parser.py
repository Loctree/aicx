#!/usr/bin/env python3
"""Prove the S0 parser deletions remain absent and fail-closed."""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = REPO_ROOT / "tools/legacy_parser_manifest_v1.toml"


class LegacyError(RuntimeError):
    pass


def git(repo: Path, *args: str, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        check=False,
    )
    if result.returncode != 0:
        raise LegacyError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout


def load_manifest(path: Path) -> dict[str, Any]:
    try:
        with path.open("rb") as handle:
            data = tomllib.load(handle)
    except (OSError, tomllib.TOMLDecodeError) as exc:
        raise LegacyError(f"cannot read manifest {path}: {exc}") from exc
    if data.get("schema_version") != 1:
        raise LegacyError("legacy manifest schema_version must be 1")
    for key in (
        "baseline_commit",
        "scan_roots",
        "deleted_path",
        "deleted_symbols",
        "temporary_path",
    ):
        if not data.get(key):
            raise LegacyError(f"legacy manifest missing {key}")
    return data


def tracked_text_files(repo: Path, roots: list[str]) -> list[Path]:
    output = git(repo, "ls-files", "--", *roots)
    return [
        repo / line for line in output.splitlines() if line and (repo / line).is_file()
    ]


def scan_text(
    repo: Path, roots: list[str], symbols: list[str], fallbacks: list[str]
) -> None:
    symbol_patterns = [
        (symbol, re.compile(rf"(?<![A-Za-z0-9_]){re.escape(symbol)}(?![A-Za-z0-9_])"))
        for symbol in symbols
    ]
    for path in tracked_text_files(repo, roots):
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        rel = path.relative_to(repo)
        for symbol, pattern in symbol_patterns:
            match = pattern.search(text)
            if match:
                line = text.count("\n", 0, match.start()) + 1
                raise LegacyError(
                    f"deleted symbol reintroduced: {symbol} at {rel}:{line}"
                )
        for fallback in fallbacks:
            offset = text.find(fallback)
            if offset >= 0:
                line = text.count("\n", 0, offset) + 1
                raise LegacyError(
                    f"legacy fallback reintroduced at {rel}:{line}: {fallback}"
                )


def verify(
    repo: Path, manifest_path: Path, *, baseline_override: str | None = None
) -> None:
    data = load_manifest(manifest_path)
    baseline = baseline_override or data["baseline_commit"]
    git(repo, "rev-parse", "--verify", f"{baseline}^{{commit}}")
    baseline_paths = set(
        git(repo, "ls-tree", "-r", "--name-only", baseline).splitlines()
    )
    deleted_paths = [item["path"] for item in data["deleted_path"]]
    for path in deleted_paths:
        if path in baseline_paths:
            raise LegacyError(
                f"S0 deleted path still exists at baseline {baseline}: {path}"
            )
        if (repo / path).exists():
            raise LegacyError(f"S0 deleted path reintroduced in live tree: {path}")

    valid_dispositions = {"git-rm", "move-before-rm", "retain-facade-until-cutover"}
    for item in data["temporary_path"]:
        path = item.get("path")
        disposition = item.get("disposition")
        consumers = item.get("current_consumers")
        if disposition not in valid_dispositions:
            raise LegacyError(
                f"temporary path {path!r} has invalid disposition {disposition!r}"
            )
        if not isinstance(path, str) or not path:
            raise LegacyError("temporary path entry missing path")
        if not (repo / path).is_file():
            raise LegacyError(f"temporary boundary/facade path is missing: {path}")
        if not isinstance(consumers, list) or not consumers:
            raise LegacyError(
                f"temporary path has no current consumer inventory: {path}"
            )

    scan_text(
        repo,
        data["scan_roots"],
        data["deleted_symbols"],
        data.get("forbidden_fallbacks", []),
    )

    boundary = repo / "src/sources/providers/session_boundary.rs"
    if boundary.exists():
        text = boundary.read_text(encoding="utf-8")
        required = (
            "deliberately contains no parsing, discovery, fallback",
            "legacy session parser removed",
            "unavailable($agent)",
        )
        for marker in required:
            if marker not in text:
                raise LegacyError(
                    f"session boundary is no longer fail-closed; missing marker: {marker}"
                )


def self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="aicx-no-legacy-") as tmp:
        repo = Path(tmp)
        git(repo, "init", "-q")
        env = os.environ.copy()
        env.update(
            {
                "GIT_AUTHOR_NAME": "legacy-self-test",
                "GIT_AUTHOR_EMAIL": "legacy@example.invalid",
                "GIT_COMMITTER_NAME": "legacy-self-test",
                "GIT_COMMITTER_EMAIL": "legacy@example.invalid",
            }
        )
        (repo / "src").mkdir()
        (repo / "src" / "boundary.rs").write_text("sealed\n", encoding="utf-8")
        git(repo, "add", ".", env=env)
        git(repo, "commit", "-q", "-m", "baseline", env=env)
        baseline = git(repo, "rev-parse", "HEAD").strip()
        manifest = repo / "manifest.toml"
        manifest.write_text(
            f'''schema_version = 1
baseline_commit = "{baseline}"
scan_roots = ["src"]
deleted_symbols = ["OldParser"]
forbidden_fallbacks = ["parse every session then filter"]
[[deleted_path]]
path = "src/deleted.rs"
removed_by = "baseline"
[[temporary_path]]
path = "src/boundary.rs"
disposition = "git-rm"
remove_after = "cutover"
current_consumers = ["src/lib.rs"]
''',
            encoding="utf-8",
        )
        verify(repo, manifest)

        (repo / "src" / "deleted.rs").write_text("legacy\n", encoding="utf-8")
        try:
            verify(repo, manifest)
        except LegacyError as exc:
            if "deleted path reintroduced" not in str(exc):
                raise
        else:
            raise LegacyError("deleted-path mutation unexpectedly passed")
        (repo / "src" / "deleted.rs").unlink()

        (repo / "src" / "new.rs").write_text("struct OldParser;\n", encoding="utf-8")
        git(repo, "add", "src/new.rs", env=env)
        try:
            verify(repo, manifest)
        except LegacyError as exc:
            if "deleted symbol reintroduced" not in str(exc):
                raise
        else:
            raise LegacyError("deleted-symbol mutation unexpectedly passed")
        (repo / "src" / "new.rs").write_text(
            "parse every session then filter\n", encoding="utf-8"
        )
        try:
            verify(repo, manifest)
        except LegacyError as exc:
            if "legacy fallback reintroduced" not in str(exc):
                raise
        else:
            raise LegacyError("fallback mutation unexpectedly passed")
    print(
        "no-legacy parser self-test: PASS (path + symbol + fallback mutations rejected)"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=REPO_ROOT)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--baseline", help="explicit baseline override")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    try:
        if args.self_test:
            self_test()
            return 0
        verify(
            args.repo.resolve(),
            args.manifest.resolve(),
            baseline_override=args.baseline,
        )
        print("no-legacy parser verification: PASS")
        return 0
    except LegacyError as exc:
        print(f"no-legacy parser verification: FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
