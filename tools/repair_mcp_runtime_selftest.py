#!/usr/bin/env python3
"""Mocked Darwin regression checks for the in-place MCP runtime migration."""

import os
import plistlib
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).parents[1]
SCRIPT = Path(sys.argv[1]) if len(sys.argv) == 2 else ROOT / "tools/repair-mcp-runtime.sh"
SCHEDULER_SCRIPT = ROOT / "tools/install-reindex-schedule.sh"


def write_plist(path: Path, label: str, args: list[str]) -> None:
    path.write_bytes(
        plistlib.dumps(
            {
                "Label": label,
                "ProgramArguments": args,
                "EnvironmentVariables": {
                    "PATH": "/fixture/bin:/usr/bin:/bin",
                    "RUST_LOG": "mcp.audit=info,rmcp=info",
                },
                "KeepAlive": True,
                "StandardOutPath": "/fixture/log/aicx.log",
                "StandardErrorPath": "/fixture/log/aicx.log",
            },
            sort_keys=False,
        )
    )


def fixture(tmp: Path) -> tuple[Path, Path, dict[str, str], Path, Path]:
    home = tmp / "home"
    agents = home / "Library/LaunchAgents"
    fake_bin = tmp / "fake bin"
    agents.mkdir(parents=True)
    fake_bin.mkdir()

    launcher = fake_bin / "aicx"
    calls = tmp / "aicx-calls.log"
    launcher.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$*\" >> \"$AICX_SELFTEST_CALLS\"\n"
        "if [ \"$*\" = 'catalog refresh --json' ]; then\n"
        "  printf '{\"catalog_present\": %s}\\n' \"${AICX_SELFTEST_CATALOG_PRESENT:-false}\"\n"
        "fi\n"
        "exit 0\n",
        encoding="utf-8",
    )
    launcher.chmod(0o755)

    launchctl_log = tmp / "launchctl.log"
    launchctl_marker = tmp / "bootstrap-failed-once"
    launchctl_state = tmp / "launchctl-state"
    launchctl_state.mkdir()
    launchctl = fake_bin / "launchctl"
    launchctl.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$*\" >> \"$AICX_SELFTEST_LAUNCHCTL_LOG\"\n"
        "if [ \"${1:-}\" = managername ]; then echo Aqua; exit 0; fi\n"
        "if [ \"${1:-}\" = print ]; then\n"
        "  label=${2##*/}\n"
        "  [ -e \"$AICX_SELFTEST_LAUNCHCTL_STATE/$label\" ] || exit 1\n"
        "  printf 'state = running\\npid = %s\\n' \"${AICX_SELFTEST_JOB_PID:-4242}\"\n"
        "  exit 0\n"
        "fi\n"
        "if [ \"${1:-}\" = bootout ]; then\n"
        "  label=${2##*/}\n"
        "  rm -f \"$AICX_SELFTEST_LAUNCHCTL_STATE/$label\"\n"
        "  exit 0\n"
        "fi\n"
        "if [ \"${1:-}\" = bootstrap ]; then\n"
        "  target=${3:-}\n"
        "  label=${target##*/}\n"
        "  label=${label%.plist}\n"
        "  if [ -n \"${AICX_SELFTEST_EXPECT_FILE_AT_BOOTSTRAP:-}\" ]; then\n"
        "    if [ -e \"$AICX_SELFTEST_EXPECT_FILE_AT_BOOTSTRAP\" ]; then\n"
        "      printf 'bootstrap-presence=present\\n' >> \"$AICX_SELFTEST_LAUNCHCTL_LOG\"\n"
        "    else\n"
        "      printf 'bootstrap-presence=missing\\n' >> \"$AICX_SELFTEST_LAUNCHCTL_LOG\"\n"
        "    fi\n"
        "  fi\n"
        "  fail=0\n"
        "  [ \"${AICX_SELFTEST_FAIL_BOOTSTRAP_ONCE:-0}\" = 1 ] && fail=1\n"
        "  [ \"${AICX_SELFTEST_FAIL_BOOTSTRAP_LABEL:-}\" = \"$label\" ] && fail=1\n"
        "  if [ \"$fail\" = 1 ] && [ ! -e \"$AICX_SELFTEST_BOOTSTRAP_MARKER\" ]; then\n"
        "    : > \"$AICX_SELFTEST_BOOTSTRAP_MARKER\"\n"
        "    exit 1\n"
        "  fi\n"
        "  : > \"$AICX_SELFTEST_LAUNCHCTL_STATE/$label\"\n"
        "  exit 0\n"
        "fi\n"
        "exit 0\n",
        encoding="utf-8",
    )
    launchctl.chmod(0o755)

    lsof_log = tmp / "lsof.log"
    lsof = fake_bin / "lsof"
    lsof.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$*\" >> \"$AICX_SELFTEST_LSOF_LOG\"\n"
        "printf 'p%s\\n' \"${AICX_SELFTEST_LISTENER_PID:-${AICX_SELFTEST_JOB_PID:-4242}}\"\n",
        encoding="utf-8",
    )
    lsof.chmod(0o755)

    ps = fake_bin / "ps"
    ps.write_text(
        "#!/bin/sh\n"
        "pid=\n"
        "while [ \"$#\" -gt 0 ]; do\n"
        "  if [ \"$1\" = -p ]; then shift; pid=${1:-}; fi\n"
        "  shift\n"
        "done\n"
        "if [ \"$pid\" = \"${AICX_SELFTEST_LISTENER_PID:-}\" ]; then\n"
        "  printf '%s\\n' \"${AICX_SELFTEST_LISTENER_PARENT_PID:-1}\"\n"
        "else\n"
        "  printf '1\\n'\n"
        "fi\n",
        encoding="utf-8",
    )
    ps.chmod(0o755)

    curl_log = tmp / "curl.log"
    curl = fake_bin / "curl"
    curl.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$*\" >> \"$AICX_SELFTEST_CURL_LOG\"\n"
        "[ \"${AICX_SELFTEST_FAIL_HEALTH:-0}\" = 1 ] && exit 22\n"
        "printf '200'\n",
        encoding="utf-8",
    )
    curl.chmod(0o755)

    env = os.environ | {
        "HOME": str(home),
        "PATH": f"{fake_bin}:{os.environ['PATH']}",
        "AICX_BIN": str(launcher),
        "AICX_RUNTIME_HEALTH_ATTEMPTS": "1",
        "AICX_SELFTEST_CALLS": str(calls),
        "AICX_SELFTEST_LAUNCHCTL_LOG": str(launchctl_log),
        "AICX_SELFTEST_BOOTSTRAP_MARKER": str(launchctl_marker),
        "AICX_SELFTEST_LAUNCHCTL_STATE": str(launchctl_state),
        "AICX_SELFTEST_CURL_LOG": str(curl_log),
        "AICX_SELFTEST_LSOF_LOG": str(lsof_log),
    }
    return agents, launcher, env, launchctl_log, curl_log


def run_repair(env: dict[str, str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(SCRIPT)],
        env=env,
        check=check,
        capture_output=True,
        text=True,
    )


def assert_common_plist_preserved(before: dict, after: dict) -> None:
    for key in (
        "EnvironmentVariables",
        "KeepAlive",
        "StandardOutPath",
        "StandardErrorPath",
    ):
        assert after[key] == before[key], f"repair changed operator-owned plist key {key}"


def launchctl_state(env: dict[str, str], label: str) -> Path:
    return Path(env["AICX_SELFTEST_LAUNCHCTL_STATE"]) / label


def test_clean_reader_and_scheduler(tmp: Path) -> None:
    agents, launcher, env, _, curl_log = fixture(tmp)
    plist_path = agents / "com.loctree.aicx.mcp.plist"
    original_args = [
        "/stale/aicx",
        "serve",
        "--transport",
        "http",
        "--host",
        "192.0.2.44",
        "--port",
        "9044",
        "--require-auth",
        "--auth-token",
        "fixture-token",
        "--allowed-host",
        "localhost",
        "--allowed-host",
        "tailnet.example",
        "--verbose",
        "--no-auto-refresh",
    ]
    write_plist(plist_path, "com.loctree.aicx.mcp", original_args)
    before = plistlib.loads(plist_path.read_bytes())

    result = run_repair(env)
    repaired = plistlib.loads(plist_path.read_bytes())
    assert repaired["ProgramArguments"] == [str(launcher), *original_args[1:]]
    assert repaired["ProgramArguments"].count("--no-auto-refresh") == 1
    assert "--experimental-auto-refresh" not in repaired["ProgramArguments"]
    assert_common_plist_preserved(before, repaired)
    assert "is ready at http://192.0.2.44:9044/health" in result.stdout
    assert "--no-auto-refresh" in result.stdout
    assert "http://192.0.2.44:9044/health" in curl_log.read_text(encoding="utf-8")
    assert "-iTCP@192.0.2.44:9044" in Path(
        env["AICX_SELFTEST_LSOF_LOG"]
    ).read_text(encoding="utf-8")

    # A current reader is already the fixed point: another repair may reload it,
    # but it must not change its plist contract or duplicate the reader flag.
    run_repair(env)
    assert plistlib.loads(plist_path.read_bytes()) == repaired

    subprocess.run(
        ["bash", str(SCHEDULER_SCRIPT)],
        env=env,
        check=True,
        capture_output=True,
        text=True,
    )
    scheduler_path = agents / "com.loctree.aicx.reindex.plist"
    scheduler = plistlib.loads(scheduler_path.read_bytes())
    scheduled_command = scheduler["ProgramArguments"][2]
    assert "catalog refresh --json" in scheduled_command
    assert '"catalog_present": false' in scheduled_command
    assert "catalog rebuild" in scheduled_command

    calls = Path(env["AICX_SELFTEST_CALLS"])
    subprocess.run(scheduler["ProgramArguments"], env=env, check=True, capture_output=True)
    assert calls.read_text(encoding="utf-8").splitlines() == [
        "catalog refresh --json",
        "catalog rebuild",
        "index",
    ]

    calls.unlink()
    current_env = env | {"AICX_SELFTEST_CATALOG_PRESENT": "true"}
    subprocess.run(
        scheduler["ProgramArguments"], env=current_env, check=True, capture_output=True
    )
    assert calls.read_text(encoding="utf-8").splitlines() == [
        "catalog refresh --json",
        "index",
    ]


def test_experimental_writer_becomes_reader(tmp: Path) -> None:
    agents, launcher, env, _, _ = fixture(tmp)
    plist_path = agents / "com.loctree.aicx.mcp.plist"
    original_args = [
        "/stale/aicx",
        "serve",
        "--transport",
        "http",
        "--host",
        "127.0.0.1",
        "--port",
        "8044",
        "--allowed-host",
        "localhost",
        "--no-require-auth",
        "--experimental-auto-refresh",
        "--refresh-interval-seconds",
        "17",
        "--experimental-auto-refresh",
    ]
    write_plist(plist_path, "com.loctree.aicx.mcp", original_args)

    run_repair(env)
    args = plistlib.loads(plist_path.read_bytes())["ProgramArguments"]
    assert args == [
        str(launcher),
        "serve",
        "--transport",
        "http",
        "--host",
        "127.0.0.1",
        "--port",
        "8044",
        "--allowed-host",
        "localhost",
        "--no-require-auth",
        "--refresh-interval-seconds",
        "17",
        "--no-auto-refresh",
    ]


def test_legacy_missing_serve_uses_native_subcommand(tmp: Path) -> None:
    agents, launcher, env, _, _ = fixture(tmp)
    legacy_path = agents / "io.vetcoders.aicx.mcp.plist"
    canonical_path = agents / "com.loctree.aicx.mcp.plist"
    original_args = [
        "/stale/aicx-mcp",
        "--transport",
        "http",
        "--host",
        "127.0.0.1",
        "--port",
        "8044",
        "--no-require-auth",
        "--experimental-auto-refresh",
    ]
    write_plist(legacy_path, "io.vetcoders.aicx.mcp", original_args)
    legacy_original = legacy_path.read_bytes()
    prior_archive = agents / "io.vetcoders.aicx.mcp.plist.migrated.prior"
    prior_archive.write_bytes(b"prior archive sentinel\n")

    result = run_repair(env)
    repaired = plistlib.loads(canonical_path.read_bytes())
    assert repaired["Label"] == "com.loctree.aicx.mcp"
    assert repaired["ProgramArguments"] == [
        str(launcher),
        "serve",
        "--transport",
        "http",
        "--host",
        "127.0.0.1",
        "--port",
        "8044",
        "--no-require-auth",
        "--no-auto-refresh",
    ]
    assert not legacy_path.exists()
    assert prior_archive.read_bytes() == b"prior archive sentinel\n"
    archives = list(agents.glob("io.vetcoders.aicx.mcp.plist.migrated.*"))
    migrated = [path for path in archives if path != prior_archive]
    assert len(migrated) == 1
    assert migrated[0].read_bytes() == legacy_original
    assert f"legacy MCP plist archived: {migrated[0]}" in result.stdout
    assert not launchctl_state(env, "io.vetcoders.aicx.mcp").exists()


def test_bootstrap_failure_restores_previous_plist(tmp: Path) -> None:
    agents, _, env, launchctl_log, curl_log = fixture(tmp)
    plist_path = agents / "com.loctree.aicx.mcp.plist"
    write_plist(
        plist_path,
        "com.loctree.aicx.mcp",
        ["/stale/aicx", "serve", "--transport", "http", "--port", "8044"],
    )
    original = plist_path.read_bytes()
    launchctl_state(env, "com.loctree.aicx.mcp").touch()

    result = run_repair(env | {"AICX_SELFTEST_FAIL_BOOTSTRAP_ONCE": "1"}, check=False)
    assert result.returncode != 0
    assert plist_path.read_bytes() == original
    assert "did not load; rolling back" in result.stderr
    assert "previous MCP plist and loaded state restored" in result.stdout
    assert launchctl_log.read_text(encoding="utf-8").count("bootstrap ") == 2
    assert not curl_log.exists()


def test_health_failure_restores_previous_plist(tmp: Path) -> None:
    agents, _, env, launchctl_log, curl_log = fixture(tmp)
    plist_path = agents / "com.loctree.aicx.mcp.plist"
    write_plist(
        plist_path,
        "com.loctree.aicx.mcp",
        ["/stale/aicx", "serve", "--transport", "http", "--port", "8044"],
    )
    original = plist_path.read_bytes()
    assert not launchctl_state(env, "com.loctree.aicx.mcp").exists()

    result = run_repair(env | {"AICX_SELFTEST_FAIL_HEALTH": "1"}, check=False)
    assert result.returncode != 0
    assert plist_path.read_bytes() == original
    assert "did not own the listening socket and return HTTP 200" in result.stderr
    assert "previous MCP plist restored; previous services remained unloaded" in result.stdout
    assert "loaded state restored" not in result.stdout
    assert not launchctl_state(env, "com.loctree.aicx.mcp").exists()
    assert launchctl_log.read_text(encoding="utf-8").count("bootstrap ") == 1
    assert "http://127.0.0.1:8044/health" in curl_log.read_text(encoding="utf-8")


def test_foreign_http_listener_cannot_mask_bind_failure(tmp: Path) -> None:
    agents, _, env, launchctl_log, curl_log = fixture(tmp)
    plist_path = agents / "com.loctree.aicx.mcp.plist"
    write_plist(
        plist_path,
        "com.loctree.aicx.mcp",
        ["/stale/aicx", "serve", "--transport", "http", "--port", "8044"],
    )
    original = plist_path.read_bytes()
    foreign_env = env | {
        "AICX_SELFTEST_JOB_PID": "4242",
        "AICX_SELFTEST_LISTENER_PID": "9001",
        "AICX_SELFTEST_LISTENER_PARENT_PID": "1",
    }

    result = run_repair(foreign_env, check=False)
    assert result.returncode != 0
    assert plist_path.read_bytes() == original
    assert "did not own the listening socket and return HTTP 200" in result.stderr
    assert "previous MCP plist restored; previous services remained unloaded" in result.stdout
    assert not launchctl_state(env, "com.loctree.aicx.mcp").exists()
    assert "http://127.0.0.1:8044/health" in curl_log.read_text(encoding="utf-8")
    assert "-iTCP@127.0.0.1:8044" in Path(
        env["AICX_SELFTEST_LSOF_LOG"]
    ).read_text(encoding="utf-8")
    assert launchctl_log.read_text(encoding="utf-8").count("bootstrap ") == 1


def test_health_failure_restores_both_labels_and_registrations(tmp: Path) -> None:
    agents, _, env, launchctl_log, _ = fixture(tmp)
    canonical = agents / "com.loctree.aicx.mcp.plist"
    legacy = agents / "io.vetcoders.aicx.mcp.plist"
    write_plist(
        canonical,
        "com.loctree.aicx.mcp",
        ["/old/canonical", "serve", "--transport", "http", "--port", "8044"],
    )
    write_plist(
        legacy,
        "io.vetcoders.aicx.mcp",
        ["/old/legacy", "--transport", "http", "--port", "8044"],
    )
    canonical_original = canonical.read_bytes()
    legacy_original = legacy.read_bytes()
    launchctl_state(env, "io.vetcoders.aicx.mcp").touch()

    result = run_repair(env | {"AICX_SELFTEST_FAIL_HEALTH": "1"}, check=False)
    assert result.returncode != 0
    assert canonical.read_bytes() == canonical_original
    assert legacy.read_bytes() == legacy_original
    assert not launchctl_state(env, "com.loctree.aicx.mcp").exists()
    assert launchctl_state(env, "io.vetcoders.aicx.mcp").exists()
    assert "previous MCP plist and loaded state restored" in result.stdout
    log = launchctl_log.read_text(encoding="utf-8")
    assert log.count("bootstrap ") == 2
    assert str(canonical) in log
    assert str(legacy) in log


def test_descendant_listener_is_owned_by_launchd_job(tmp: Path) -> None:
    agents, _, env, _, _ = fixture(tmp)
    plist_path = agents / "com.loctree.aicx.mcp.plist"
    write_plist(
        plist_path,
        "com.loctree.aicx.mcp",
        ["/stale/aicx", "serve", "--transport", "http", "--port", "8044"],
    )
    descendant_env = env | {
        "AICX_SELFTEST_JOB_PID": "4242",
        "AICX_SELFTEST_LISTENER_PID": "4343",
        "AICX_SELFTEST_LISTENER_PARENT_PID": "4242",
    }

    result = run_repair(descendant_env)
    assert "is ready at http://127.0.0.1:8044/health" in result.stdout


def test_ipv6_wildcard_uses_port_only_socket_ownership(tmp: Path) -> None:
    agents, _, env, _, curl_log = fixture(tmp)
    plist_path = agents / "com.loctree.aicx.mcp.plist"
    write_plist(
        plist_path,
        "com.loctree.aicx.mcp",
        [
            "/stale/aicx",
            "serve",
            "--transport",
            "http",
            "--host",
            "::",
            "--port",
            "8044",
            "--require-auth",
        ],
    )

    result = run_repair(env)
    assert "is ready at http://127.0.0.1:8044/health" in result.stdout
    assert "http://127.0.0.1:8044/health" in curl_log.read_text(encoding="utf-8")
    lsof_call = Path(env["AICX_SELFTEST_LSOF_LOG"]).read_text(encoding="utf-8")
    assert "-iTCP:8044" in lsof_call
    assert "-iTCP@[::]:8044" not in lsof_call


def test_scheduler_failure_restores_canonical_file_and_loaded_state(tmp: Path) -> None:
    agents, _, env, launchctl_log, _ = fixture(tmp)
    canonical = agents / "com.loctree.aicx.reindex.plist"
    write_plist(canonical, "com.loctree.aicx.reindex", ["/old/aicx", "index"])
    original = canonical.read_bytes()
    launchctl_state(env, "com.loctree.aicx.reindex").touch()

    result = subprocess.run(
        ["bash", str(SCHEDULER_SCRIPT)],
        env=env | {"AICX_SELFTEST_FAIL_BOOTSTRAP_LABEL": "com.loctree.aicx.reindex"},
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert canonical.read_bytes() == original
    assert launchctl_state(env, "com.loctree.aicx.reindex").exists()
    assert "failed to register; rolling back" in result.stderr
    assert "previous plist files and loaded state restored" in result.stdout
    assert launchctl_log.read_text(encoding="utf-8").count("bootstrap ") == 2


def test_scheduler_failure_restores_legacy_file_and_loaded_state(tmp: Path) -> None:
    agents, _, env, launchctl_log, _ = fixture(tmp)
    canonical = agents / "com.loctree.aicx.reindex.plist"
    legacy = agents / "io.vetcoders.aicx.reindex.plist"
    write_plist(legacy, "io.vetcoders.aicx.reindex", ["/old/aicx", "index"])
    original = legacy.read_bytes()
    launchctl_state(env, "io.vetcoders.aicx.reindex").touch()

    result = subprocess.run(
        ["bash", str(SCHEDULER_SCRIPT)],
        env=env
        | {
            "AICX_SELFTEST_FAIL_BOOTSTRAP_LABEL": "com.loctree.aicx.reindex",
            "AICX_SELFTEST_EXPECT_FILE_AT_BOOTSTRAP": str(legacy),
        },
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert not canonical.exists()
    assert legacy.read_bytes() == original
    assert not launchctl_state(env, "com.loctree.aicx.reindex").exists()
    assert launchctl_state(env, "io.vetcoders.aicx.reindex").exists()
    assert "failed to register; rolling back" in result.stderr
    assert "previous plist files and loaded state restored" in result.stdout
    log = launchctl_log.read_text(encoding="utf-8")
    assert log.count("bootstrap ") == 2
    assert str(legacy) in log
    assert "bootstrap-presence=missing" not in log
    assert not list(agents.glob("io.vetcoders.aicx.reindex.plist.migrated.*"))


def test_scheduler_failure_preserves_both_unloaded_jobs(tmp: Path) -> None:
    agents, _, env, launchctl_log, _ = fixture(tmp)
    canonical = agents / "com.loctree.aicx.reindex.plist"
    legacy = agents / "io.vetcoders.aicx.reindex.plist"
    write_plist(canonical, "com.loctree.aicx.reindex", ["/old/canonical", "index"])
    write_plist(legacy, "io.vetcoders.aicx.reindex", ["/old/legacy", "index"])
    canonical_original = canonical.read_bytes()
    legacy_original = legacy.read_bytes()

    result = subprocess.run(
        ["bash", str(SCHEDULER_SCRIPT)],
        env=env | {"AICX_SELFTEST_FAIL_BOOTSTRAP_LABEL": "com.loctree.aicx.reindex"},
        check=False,
        capture_output=True,
        text=True,
    )
    assert result.returncode != 0
    assert canonical.read_bytes() == canonical_original
    assert legacy.read_bytes() == legacy_original
    assert not launchctl_state(env, "com.loctree.aicx.reindex").exists()
    assert not launchctl_state(env, "io.vetcoders.aicx.reindex").exists()
    assert launchctl_log.read_text(encoding="utf-8").count("bootstrap ") == 1


def test_scheduler_success_retires_legacy_after_canonical_load(tmp: Path) -> None:
    agents, _, env, launchctl_log, _ = fixture(tmp)
    canonical = agents / "com.loctree.aicx.reindex.plist"
    legacy = agents / "io.vetcoders.aicx.reindex.plist"
    write_plist(legacy, "io.vetcoders.aicx.reindex", ["/old/aicx", "index"])
    legacy_original = legacy.read_bytes()
    launchctl_state(env, "io.vetcoders.aicx.reindex").touch()

    result = subprocess.run(
        ["bash", str(SCHEDULER_SCRIPT)],
        env=env | {"AICX_SELFTEST_EXPECT_FILE_AT_BOOTSTRAP": str(legacy)},
        check=True,
        capture_output=True,
        text=True,
    )
    assert canonical.exists()
    assert not legacy.exists()
    archives = list(agents.glob("io.vetcoders.aicx.reindex.plist.migrated.*"))
    assert len(archives) == 1
    assert archives[0].read_bytes() == legacy_original
    assert launchctl_state(env, "com.loctree.aicx.reindex").exists()
    assert not launchctl_state(env, "io.vetcoders.aicx.reindex").exists()
    assert "reindex schedule: loaded every" in result.stdout
    assert f"legacy schedule archived: {archives[0]}" in result.stdout
    assert "bootstrap-presence=present" in launchctl_log.read_text(encoding="utf-8")


def main() -> None:
    if len(sys.argv) > 2:
        raise SystemExit(f"usage: {Path(sys.argv[0]).name} [repair-script]")
    if os.uname().sysname != "Darwin":
        print("repair MCP runtime self-test skipped (Darwin only)")
        return
    assert SCRIPT.is_file(), f"repair script not found: {SCRIPT}"
    with tempfile.TemporaryDirectory(prefix="aicx-runtime-repair-") as raw_tmp:
        base = Path(raw_tmp)
        test_clean_reader_and_scheduler(base / "clean")
        test_experimental_writer_becomes_reader(base / "writer")
        test_legacy_missing_serve_uses_native_subcommand(base / "legacy")
        test_bootstrap_failure_restores_previous_plist(base / "bootstrap-failure")
        test_health_failure_restores_previous_plist(base / "health-failure")
        test_foreign_http_listener_cannot_mask_bind_failure(base / "foreign-listener")
        test_health_failure_restores_both_labels_and_registrations(
            base / "two-label-rollback"
        )
        test_descendant_listener_is_owned_by_launchd_job(base / "descendant-listener")
        test_ipv6_wildcard_uses_port_only_socket_ownership(base / "ipv6-wildcard")
        test_scheduler_failure_restores_canonical_file_and_loaded_state(
            base / "scheduler-canonical-failure"
        )
        test_scheduler_failure_restores_legacy_file_and_loaded_state(
            base / "scheduler-legacy-failure"
        )
        test_scheduler_failure_preserves_both_unloaded_jobs(
            base / "scheduler-unloaded-failure"
        )
        test_scheduler_success_retires_legacy_after_canonical_load(
            base / "scheduler-legacy-success"
        )
    print("repair MCP runtime reader-only migration and rollback: passed")


if __name__ == "__main__":
    main()
