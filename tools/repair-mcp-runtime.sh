#!/usr/bin/env bash
set -euo pipefail
# Repair an existing macOS MCP LaunchAgent without resetting its bind address,
# port, allowed-host list, logging, or other operator-owned configuration.

LABEL="com.loctree.aicx.mcp"
LEGACY_LABEL="io.vetcoders.aicx.mcp"
CANONICAL_PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LEGACY_PLIST="$HOME/Library/LaunchAgents/$LEGACY_LABEL.plist"

note() { printf '  %s\n' "$*"; }
gui_domain() { printf 'gui/%s' "$(id -u)"; }

if [ "$(uname -s)" != "Darwin" ]; then
  note "runtime repair: skipped (launchd-only; this host is $(uname -s))"
  exit 0
fi

MANAGER="$(launchctl managername 2>/dev/null || true)"
if [ "$MANAGER" != "Aqua" ]; then
  echo "Error: runtime repair must run from a macOS GUI login (launchctl managername=${MANAGER:-unknown}); no service changes made" >&2
  exit 1
fi

AICX_BIN="${AICX_BIN:-$(command -v aicx || true)}"
if [ -z "$AICX_BIN" ] || [ ! -x "$AICX_BIN" ]; then
  echo "Error: runtime repair could not resolve an executable aicx launcher" >&2
  exit 1
fi

SOURCE_PLIST="$CANONICAL_PLIST"
if [ ! -f "$SOURCE_PLIST" ]; then
  SOURCE_PLIST="$LEGACY_PLIST"
fi
if [ ! -f "$SOURCE_PLIST" ]; then
  echo "Error: no AICX MCP LaunchAgent found; install the service before repairing it" >&2
  exit 1
fi

mkdir -p "$HOME/Library/LaunchAgents"
if [ "$SOURCE_PLIST" = "$LEGACY_PLIST" ]; then
  cp "$LEGACY_PLIST" "$CANONICAL_PLIST"
fi
BACKUP_PLIST="$(mktemp "${TMPDIR:-/tmp}/aicx-mcp-plist.XXXXXX")"
cp "$CANONICAL_PLIST" "$BACKUP_PLIST"
restore_plist() {
  cp "$BACKUP_PLIST" "$CANONICAL_PLIST"
}
trap 'restore_plist; rm -f "$BACKUP_PLIST"' EXIT

# PlistBuddy updates array values in place. This deliberately preserves every
# existing network and logging argument while changing only runtime ownership.
/usr/libexec/PlistBuddy -c "Set :Label $LABEL" "$CANONICAL_PLIST"
/usr/libexec/PlistBuddy -c "Set :ProgramArguments:0 $AICX_BIN" "$CANONICAL_PLIST"
if ! plutil -extract ProgramArguments json -o - "$CANONICAL_PLIST" | grep -Fq '"--no-auto-refresh"'; then
  ARG_COUNT="$(plutil -extract ProgramArguments raw "$CANONICAL_PLIST")"
  /usr/libexec/PlistBuddy -c "Add :ProgramArguments:$ARG_COUNT string --no-auto-refresh" "$CANONICAL_PLIST"
fi
plutil -lint "$CANONICAL_PLIST" >/dev/null

launchctl bootout "$(gui_domain)/$LEGACY_LABEL" 2>/dev/null || true
launchctl bootout "$(gui_domain)/$LABEL" 2>/dev/null || true
if ! launchctl bootstrap "$(gui_domain)" "$CANONICAL_PLIST" || \
   ! launchctl print "$(gui_domain)/$LABEL" >/dev/null; then
  restore_plist
  launchctl bootout "$(gui_domain)/$LABEL" 2>/dev/null || true
  launchctl bootstrap "$(gui_domain)" "$CANONICAL_PLIST" 2>/dev/null || true
  echo "Error: repaired MCP service did not start; restored the previous plist" >&2
  exit 1
fi

trap - EXIT
rm -f "$BACKUP_PLIST"

if [ -f "$LEGACY_PLIST" ]; then
  mv "$LEGACY_PLIST" "$LEGACY_PLIST.migrated"
fi

note "mcp runtime: $LABEL now uses $AICX_BIN"
note "mcp refresh: disabled in the long-lived server (--no-auto-refresh)"
