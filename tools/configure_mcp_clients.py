#!/usr/bin/env python3
"""Write AICX MCP client entries as stdio or Streamable HTTP.

Used by install.sh and tools/install-mcp-service.sh so a Darwin HTTP
LaunchAgent and the client JSON stay on the same transport.

HTTP desired shape (command/args dropped):
  {"url": "http://127.0.0.1:8044/mcp"}
plus Authorization when a token file is readable — `aicx serve` requires
Bearer auth by default (loopback included).

Stdio desired shape (url/headers/type dropped):
  {"command": "...", "args": []}
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from pathlib import Path
from typing import Any


DEFAULT_TARGETS: list[tuple[str, Path]] = [
    ("claude", Path.home() / ".claude" / "settings.json"),
    ("codex", Path.home() / ".codex" / "settings.json"),
    ("gemini", Path.home() / ".gemini" / "settings.json"),
]

WILDCARD_HOSTS = {"0.0.0.0", "::", "[::]"}


def client_url(host: str, port: str) -> str:
    host = host.strip()
    port = str(port).strip()
    if host in WILDCARD_HOSTS:
        host = "127.0.0.1"
    if host.startswith("[") and host.endswith("]"):
        return f"http://{host}:{port}/mcp"
    if ":" in host:
        return f"http://[{host}]:{port}/mcp"
    return f"http://{host}:{port}/mcp"


def url_from_plist(path: Path) -> str | None:
    try:
        import plistlib
    except ImportError:
        return None
    try:
        with path.open("rb") as handle:
            data = plistlib.load(handle)
    except (OSError, ValueError):
        return None
    args = data.get("ProgramArguments")
    if not isinstance(args, list):
        return None
    host = "127.0.0.1"
    port = "8044"
    pending: str | None = None
    for raw in args:
        item = str(raw)
        if pending == "host":
            host = item
            pending = None
            continue
        if pending == "port":
            port = item
            pending = None
            continue
        if item == "--host":
            pending = "host"
        elif item == "--port":
            pending = "port"
    return client_url(host, port)


def resolve_url(
    *,
    url: str | None = None,
    host: str | None = None,
    port: str | None = None,
    plist: Path | None = None,
) -> str:
    if url:
        return url.rstrip("/")
    if plist is not None and plist.is_file():
        from_plist = url_from_plist(plist)
        if from_plist:
            return from_plist
    resolved_host = host or os.environ.get("AICX_MCP_HOST") or "127.0.0.1"
    resolved_port = port or os.environ.get("AICX_MCP_PORT") or "8044"
    return client_url(resolved_host, resolved_port)


def read_token(token_file: Path | None) -> str | None:
    if token_file is None or not token_file.is_file():
        return None
    try:
        token = token_file.read_text(encoding="utf-8").strip()
    except OSError:
        return None
    return token or None


def desired_http(url: str, token: str | None) -> dict[str, Any]:
    entry: dict[str, Any] = {"url": url}
    if token:
        entry["headers"] = {"Authorization": f"Bearer {token}"}
    return entry


def desired_stdio(command: str, args: list[str]) -> dict[str, Any]:
    return {"command": command, "args": args}


def apply_entry(path: Path, desired: dict[str, Any]) -> str:
    if not path.parent.is_dir():
        return "skipped (dir not found)"
    if not path.is_file():
        path.write_text("{}\n", encoding="utf-8")
    try:
        data = json.loads(path.read_text(encoding="utf-8") or "{}")
    except json.JSONDecodeError:
        return "failed (invalid json)"
    if not isinstance(data, dict):
        return "failed (settings root is not an object)"
    servers = data.setdefault("mcpServers", {})
    if not isinstance(servers, dict):
        return "failed (mcpServers is not an object)"
    if servers.get("aicx") == desired:
        return "already configured"
    servers["aicx"] = desired
    serialized = json.dumps(data, indent=2) + "\n"
    path.write_text(serialized, encoding="utf-8")
    return "configured"


def default_token_file() -> Path:
    home = os.environ.get("AICX_HOME")
    if home:
        return Path(home) / "auth-token"
    return Path.home() / ".aicx" / "auth-token"


def default_plist() -> Path:
    return Path.home() / "Library" / "LaunchAgents" / "com.loctree.aicx.mcp.plist"


def wire_targets(
    targets: list[tuple[str, Path]],
    desired: dict[str, Any],
) -> int:
    failures = 0
    for name, path in targets:
        status = apply_entry(path, desired)
        print(f"  [{name}] {status}: {path}")
        if status.startswith("failed"):
            failures += 1
    return failures


def self_test() -> int:
    failures = 0

    def check(name: str, ok: bool) -> None:
        nonlocal failures
        if ok:
            print(f"ok {name}")
        else:
            print(f"FAIL {name}", file=sys.stderr)
            failures += 1

    check("wildcard host becomes loopback", client_url("0.0.0.0", "8044") == "http://127.0.0.1:8044/mcp")
    check("ipv6 wildcard becomes loopback", client_url("::", "8044") == "http://127.0.0.1:8044/mcp")
    check("tailscale host kept", client_url("100.82.232.70", "8044") == "http://100.82.232.70:8044/mcp")
    check("ipv6 literal is bracketed", client_url("::1", "8044") == "http://[::1]:8044/mcp")

    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        settings = root / "settings.json"
        settings.write_text(
            json.dumps(
                {
                    "mcpServers": {
                        "aicx": {"command": "/old/aicx-mcp", "args": []},
                        "other": {"command": "keep-me"},
                    }
                }
            )
            + "\n",
            encoding="utf-8",
        )
        status = apply_entry(settings, desired_http("http://127.0.0.1:8044/mcp", None))
        payload = json.loads(settings.read_text(encoding="utf-8"))
        aicx = payload["mcpServers"]["aicx"]
        check("http replace reports configured", status == "configured")
        check("http writes url only", aicx == {"url": "http://127.0.0.1:8044/mcp"})
        check("http drops command", "command" not in aicx)
        check("other servers preserved", payload["mcpServers"]["other"] == {"command": "keep-me"})

        again = apply_entry(settings, desired_http("http://127.0.0.1:8044/mcp", None))
        check("http idempotent", again == "already configured")

        with_token = apply_entry(
            settings, desired_http("http://127.0.0.1:8044/mcp", "unit-test-token")
        )
        aicx = json.loads(settings.read_text(encoding="utf-8"))["mcpServers"]["aicx"]
        check("http with token reports configured", with_token == "configured")
        check("http token stays in headers", aicx.get("headers") == {"Authorization": "Bearer unit-test-token"})

        stdio = apply_entry(settings, desired_stdio("/new/aicx-mcp", []))
        aicx = json.loads(settings.read_text(encoding="utf-8"))["mcpServers"]["aicx"]
        check("stdio replace reports configured", stdio == "configured")
        check("stdio drops url and headers", aicx == {"command": "/new/aicx-mcp", "args": []})

        missing_dir = apply_entry(root / "nope" / "settings.json", desired_stdio("/bin/aicx-mcp", []))
        check("missing dir is skipped", missing_dir == "skipped (dir not found)")

        plist = root / "com.loctree.aicx.mcp.plist"
        plist.write_bytes(
            b"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>ProgramArguments</key>
  <array>
    <string>/opt/aicx</string>
    <string>serve</string>
    <string>--transport</string>
    <string>http</string>
    <string>--host</string>
    <string>100.82.232.70</string>
    <string>--port</string>
    <string>8044</string>
  </array>
</dict>
</plist>
"""
        )
        check(
            "plist host/port become client url",
            url_from_plist(plist) == "http://100.82.232.70:8044/mcp",
        )

    return failures


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--print-url", action="store_true")
    parser.add_argument("--wire-defaults", action="store_true")
    parser.add_argument("--settings", action="append", default=[])
    parser.add_argument("--name", default="aicx")
    parser.add_argument("--transport", choices=("http", "stdio"))
    parser.add_argument("--url")
    parser.add_argument("--host")
    parser.add_argument("--port")
    parser.add_argument("--plist")
    parser.add_argument("--token-file")
    parser.add_argument("--command")
    parser.add_argument("--args-json", default="[]")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv if argv is not None else sys.argv[1:])
    if args.self_test:
        failures = self_test()
        return 1 if failures else 0

    plist = Path(args.plist) if args.plist else None
    url = resolve_url(url=args.url, host=args.host, port=args.port, plist=plist)
    if args.print_url:
        print(url)
        return 0

    if not args.transport:
        print("error: --transport is required unless --self-test or --print-url", file=sys.stderr)
        return 2

    if args.transport == "http":
        token_path = Path(args.token_file) if args.token_file else default_token_file()
        token = read_token(token_path)
        if token is None and args.token_file:
            print(
                "  warning: MCP HTTP is token-gated; no readable token file, writing url only",
                file=sys.stderr,
            )
        desired = desired_http(url, token)
    else:
        if not args.command:
            print("error: --command is required for stdio transport", file=sys.stderr)
            return 2
        try:
            parsed_args = json.loads(args.args_json)
        except json.JSONDecodeError as exc:
            print(f"error: --args-json is not valid JSON: {exc}", file=sys.stderr)
            return 2
        if not isinstance(parsed_args, list) or not all(isinstance(item, str) for item in parsed_args):
            print("error: --args-json must be a JSON array of strings", file=sys.stderr)
            return 2
        desired = desired_stdio(args.command, parsed_args)

    targets: list[tuple[str, Path]] = []
    if args.wire_defaults:
        targets.extend(DEFAULT_TARGETS)
    for index, raw in enumerate(args.settings):
        targets.append((args.name if len(args.settings) == 1 else f"{args.name}-{index}", Path(raw)))
    if not targets:
        print("error: pass --wire-defaults and/or --settings", file=sys.stderr)
        return 2
    return 1 if wire_targets(targets, desired) else 0


if __name__ == "__main__":
    raise SystemExit(main())
