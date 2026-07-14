#!/usr/bin/env python3
"""Check an explicit commit or explicit stdin path list against an allowlist."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath


class AllowlistError(RuntimeError):
    pass


def run_git(repo: Path, *args: str, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        check=False,
    )
    if result.returncode != 0:
        raise AllowlistError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout


def normalize_path(raw: str) -> str:
    value = raw.strip().replace("\\", "/")
    if not value:
        raise AllowlistError("empty changed path")
    path = PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts:
        raise AllowlistError(f"changed path must be repository-relative: {raw!r}")
    normalized = str(path)
    if normalized in {"", "."}:
        raise AllowlistError(f"invalid changed path: {raw!r}")
    return normalized


def changed_paths_for_commit(repo: Path, commit: str) -> list[str]:
    if not commit.strip():
        raise AllowlistError("--commit requires a non-empty explicit cut commit")
    run_git(repo, "rev-parse", "--verify", f"{commit}^{{commit}}")
    output = run_git(
        repo,
        "diff-tree",
        "--root",
        "--no-commit-id",
        "--name-only",
        "-r",
        commit,
    )
    return [normalize_path(line) for line in output.splitlines() if line.strip()]


def changed_paths_from_stdin(payload: str) -> list[str]:
    separator = "\0" if "\0" in payload else "\n"
    return [normalize_path(item) for item in payload.split(separator) if item.strip()]


def check_paths(
    paths: list[str], *, exact: list[str], prefixes: list[str]
) -> list[str]:
    allowed_exact = {normalize_path(path) for path in exact}
    allowed_prefixes = []
    for prefix in prefixes:
        normalized = normalize_path(prefix).rstrip("/") + "/"
        allowed_prefixes.append(normalized)
    forbidden = []
    for path in paths:
        normalized = normalize_path(path)
        if normalized in allowed_exact:
            continue
        if any(normalized.startswith(prefix) for prefix in allowed_prefixes):
            continue
        forbidden.append(normalized)
    return forbidden


def self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="aicx-allowlist-") as tmp:
        repo = Path(tmp)
        run_git(repo, "init", "-q")
        env = os.environ.copy()
        env.update(
            {
                "GIT_AUTHOR_NAME": "allowlist-self-test",
                "GIT_AUTHOR_EMAIL": "allowlist@example.invalid",
                "GIT_COMMITTER_NAME": "allowlist-self-test",
                "GIT_COMMITTER_EMAIL": "allowlist@example.invalid",
            }
        )
        (repo / "README.md").write_text("base\n", encoding="utf-8")
        run_git(repo, "add", "README.md", env=env)
        run_git(repo, "commit", "-q", "-m", "base", env=env)

        allowed_dir = repo / "tests" / "parser_oracle"
        allowed_dir.mkdir(parents=True)
        (allowed_dir / "manifest.toml").write_text(
            "schema_version = 1\n", encoding="utf-8"
        )
        run_git(repo, "add", "tests/parser_oracle/manifest.toml", env=env)
        run_git(repo, "commit", "-q", "-m", "cut", env=env)
        cut_commit = run_git(repo, "rev-parse", "HEAD").strip()

        # A foreign concurrent commit after the cut must never enter the cut envelope.
        (repo / "foreign.txt").write_text("foreign\n", encoding="utf-8")
        run_git(repo, "add", "foreign.txt", env=env)
        run_git(repo, "commit", "-q", "-m", "foreign concurrent commit", env=env)
        paths = changed_paths_for_commit(repo, cut_commit)
        if paths != ["tests/parser_oracle/manifest.toml"]:
            raise AllowlistError(f"explicit cut commit was not isolated: {paths!r}")
        if check_paths(paths, exact=[], prefixes=["tests/parser_oracle"]):
            raise AllowlistError("allowed prefix unexpectedly failed")
        if check_paths(
            ["docs/PARSER_ENGINE_CONTRACT.md"],
            exact=["docs/PARSER_ENGINE_CONTRACT.md"],
            prefixes=[],
        ):
            raise AllowlistError("exact path unexpectedly failed")
        forbidden = check_paths(
            ["src/main.rs"], exact=[], prefixes=["tests/parser_oracle"]
        )
        if forbidden != ["src/main.rs"]:
            raise AllowlistError("forbidden path did not fail")
        stdin_paths = changed_paths_from_stdin("tools/a.py\ntools/b.py\n")
        if check_paths(stdin_paths, exact=[], prefixes=["tools"]):
            raise AllowlistError("stdin mode did not accept allowed prefix")
    print(
        "changed-file allowlist self-test: PASS (explicit commit isolates foreign HEAD)"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--commit", help="exact cut commit to inspect; never inferred")
    source.add_argument(
        "--stdin", action="store_true", help="read newline/NUL paths from stdin"
    )
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    parser.add_argument(
        "--allow", action="append", default=[], help="exact repository-relative path"
    )
    parser.add_argument(
        "--allow-prefix",
        action="append",
        default=[],
        help="repository-relative directory prefix",
    )
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    try:
        if args.self_test:
            self_test()
            return 0
        if not args.commit and not args.stdin:
            raise AllowlistError(
                "choose explicit --commit <cut-sha> or --stdin; HEAD^ is never inferred"
            )
        if not args.allow and not args.allow_prefix:
            raise AllowlistError("at least one --allow or --allow-prefix is required")
        repo = args.repo.resolve()
        paths = (
            changed_paths_for_commit(repo, args.commit)
            if args.commit
            else changed_paths_from_stdin(sys.stdin.read())
        )
        forbidden = check_paths(paths, exact=args.allow, prefixes=args.allow_prefix)
        if forbidden:
            raise AllowlistError("forbidden changed path(s): " + ", ".join(forbidden))
        print(f"changed-file allowlist: PASS ({len(paths)} path(s))")
        return 0
    except AllowlistError as exc:
        print(f"changed-file allowlist: FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
