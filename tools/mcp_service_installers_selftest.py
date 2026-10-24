#!/usr/bin/env python3
"""Service installer regressions using real shell/PowerShell and mocked OS APIs."""

import os
import plistlib
import shutil
import subprocess
import tempfile
from pathlib import Path

from repair_mcp_runtime_selftest import fixture

ROOT = Path(__file__).resolve().parents[1]


def linux_installer(root: Path) -> None:
    root.mkdir()
    bin_dir = root / 'space %u $HOME "quote" \\slash'
    bin_dir.mkdir()
    launcher = bin_dir / "aicx"
    launcher.write_text("#!/bin/sh\nexit 0\n")
    launcher.chmod(0o755)
    fake_bin = root / "bin"
    fake_bin.mkdir()
    systemctl = fake_bin / "systemctl"
    systemctl.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$CALLS"\n')
    systemctl.chmod(0o755)
    env = os.environ | {
        "PATH": f"{fake_bin}:{os.environ['PATH']}",
        "HOME": str(root),
        "XDG_CONFIG_HOME": str(root / "config"),
        "AICX_BIN": str(launcher),
        "CALLS": str(root / "calls"),
    }
    script = ROOT / "tools/install-mcp-service-linux.sh"
    subprocess.run(["bash", str(script)], env=env, check=True, capture_output=True)
    unit = root / "config/systemd/user/aicx-mcp.service"
    content = unit.read_text()
    escaped = str(launcher).replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%").replace("$", "$$")
    assert f'ExecStart=/usr/bin/env "{escaped}" serve ' in content
    assert (root / "calls").read_text().splitlines() == [
        "--user daemon-reload", "--user enable --now aicx-mcp.service"
    ]
    if shutil.which("systemd-analyze"):
        verified = subprocess.run(["systemd-analyze", "verify", str(unit)], capture_output=True, text=True)
        assert verified.returncode == 0, verified.stderr
    before = unit.read_bytes()
    for port in ("bad\nRestart=always", "0", "65536"):
        result = subprocess.run(["bash", str(script)], env=env | {"AICX_MCP_PORT": port}, capture_output=True)
        assert result.returncode != 0
        assert unit.read_bytes() == before


def darwin_installer(root: Path) -> None:
    agents, launcher, env, launchctl_log, _ = fixture(root)
    # Isolate the actual installer from its production helper files.
    script = root / "install-mcp-service.sh"
    shutil.copyfile(ROOT / "tools/install-mcp-service.sh", script)
    scheduler = root / "install-reindex-schedule.sh"
    scheduler.write_text("#!/bin/sh\nexit 1\n")  # non-Aqua scheduler defers too
    standalone = launcher.parent / "aicx-mcp"
    standalone.write_text("#!/bin/sh\nexit 99\n")
    standalone.chmod(0o755)
    deferred_env = env | {
        "AICX_SELFTEST_MANAGER": "Background",
        "AICX_SKIP_MCP_CLIENTS": "1",
    }
    result = subprocess.run(["bash", str(script)], env=deferred_env, check=True, capture_output=True, text=True)
    plist = plistlib.loads((agents / "com.loctree.aicx.mcp.plist").read_bytes())
    assert plist["ProgramArguments"][:2] == [str(launcher), "serve"]
    assert "NOT loaded" in result.stdout
    assert "bootstrap " not in launchctl_log.read_text()
    # An explicit old standalone path must also resolve to the hybrid sibling.
    subprocess.run(["bash", str(script)], env=deferred_env | {"AICX_BIN": str(standalone)}, check=True, capture_output=True)
    plist = plistlib.loads((agents / "com.loctree.aicx.mcp.plist").read_bytes())
    assert plist["ProgramArguments"][:2] == [str(launcher), "serve"]
    launcher.unlink()
    result = subprocess.run(["bash", str(script)], env=deferred_env | {"AICX_BIN": str(standalone)}, capture_output=True)
    assert result.returncode != 0


def main() -> None:
    if os.name == "nt":
        print("MCP installer mocks run on POSIX hosts (including PowerShell); covered by Linux CI")
        return
    with tempfile.TemporaryDirectory(prefix=".aicx-service-test-", dir=ROOT) as raw:
        root = Path(raw)
        if os.uname().sysname == "Linux":
            linux_installer(root / "linux")
        darwin_installer(root / "darwin")
        if shutil.which("pwsh"):
            subprocess.run(
                ["pwsh", "-NoProfile", "-File", str(ROOT / "tools/install_mcp_service_selftest.ps1"), str(root / "windows")],
                check=True,
            )
        else:
            print("Windows scheduled task tests skipped: pwsh unavailable")
    print("MCP service installer contracts: passed")


if __name__ == "__main__":
    main()
