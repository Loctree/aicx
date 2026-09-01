#!/usr/bin/env python3
"""Darwin integration check for the in-place MCP runtime migration."""

import os
import plistlib
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).parents[1]
SCRIPT = ROOT / "tools/repair-mcp-runtime.sh"
SCHEDULER_SCRIPT = ROOT / "tools/install-reindex-schedule.sh"


def main() -> None:
    if os.uname().sysname != "Darwin":
        print("repair MCP runtime self-test skipped (Darwin only)")
        return
    with tempfile.TemporaryDirectory(prefix="aicx-runtime-repair-") as raw_tmp:
        tmp = Path(raw_tmp)
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
        launchctl = fake_bin / "launchctl"
        launchctl.write_text(
            "#!/bin/sh\n[ \"${1:-}\" = managername ] && echo Aqua\nexit 0\n",
            encoding="utf-8",
        )
        launchctl.chmod(0o755)

        plist_path = agents / "com.loctree.aicx.mcp.plist"
        original_args = [
            "/stale/aicx", "serve", "--transport", "http",
            "--host", "192.0.2.44", "--port", "9044",
            "--allowed-host", "localhost", "--allowed-host", "tailnet.example",
        ]
        plist_path.write_bytes(plistlib.dumps({
            "Label": "com.loctree.aicx.mcp",
            "ProgramArguments": original_args,
            "KeepAlive": True,
        }))
        env = os.environ | {
            "HOME": str(home),
            "PATH": f"{fake_bin}:{os.environ['PATH']}",
            "AICX_BIN": str(launcher),
            "AICX_SELFTEST_CALLS": str(calls),
        }
        subprocess.run(["bash", str(SCRIPT)], env=env, check=True, capture_output=True, text=True)
        repaired = plistlib.loads(plist_path.read_bytes())
        args = repaired["ProgramArguments"]
        assert args[0] == str(launcher)
        assert args[1:-1] == original_args[1:]
        assert args[-1] == "--no-auto-refresh"
        assert args.count("--no-auto-refresh") == 1
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
        print("repair MCP runtime preserves network configuration: passed")


if __name__ == "__main__":
    main()
