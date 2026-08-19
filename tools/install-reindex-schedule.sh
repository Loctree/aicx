#!/usr/bin/env bash
set -euo pipefail
# install-reindex-schedule.sh — background reindex cadence for AICX (macOS).
#
# Installs a per-user LaunchAgent that runs
#   aicx catalog rebuild && aicx index
# every AICX_REINDEX_INTERVAL seconds (default 7200 = 2 h), so the catalog
# admits new sessions and the lexical index republishes without anyone
# remembering to run it. launchd serializes per-label, so a long rebuild
# never overlaps the next tick.
#
# Usage:
#   bash tools/install-reindex-schedule.sh              # install / refresh
#   bash tools/install-reindex-schedule.sh --uninstall  # remove agent + plist
#
# Env:
#   AICX_REINDEX_INTERVAL  seconds between runs (default 7200)
#   AICX_BIN               explicit aicx binary (default: resolve from PATH)
#
# Non-macOS hosts: prints a note and exits 0 (the schedule is launchd-only
# for now; a systemd user timer is the natural Linux counterpart).

LABEL="io.vetcoders.aicx.reindex"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG_DIR="$HOME/Library/Logs/vetcoders"
INTERVAL="${AICX_REINDEX_INTERVAL:-7200}"

note() { printf '  %s\n' "$*"; }

if [ "$(uname -s)" != "Darwin" ]; then
  note "reindex schedule: skipped (launchd-only; this host is $(uname -s))"
  exit 0
fi

gui_domain() { printf 'gui/%s' "$(id -u)"; }

if [ "${1:-}" = "--uninstall" ]; then
  launchctl bootout "$(gui_domain)/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"
  note "reindex schedule: removed ($LABEL)"
  exit 0
fi

# Resolve the aicx binary the agent should run. An absolute PATH export in
# the job covers helpers aicx may spawn; the resolved binary pins identity.
AICX_BIN="${AICX_BIN:-$(command -v aicx || true)}"
if [ -z "$AICX_BIN" ]; then
  note "reindex schedule: skipped (aicx not on PATH yet — rerun after install)"
  exit 0
fi
AICX_DIR="$(dirname "$AICX_BIN")"

case "$INTERVAL" in
  ''|*[!0-9]*)
    echo "Error: AICX_REINDEX_INTERVAL must be a positive integer (got '$INTERVAL')" >&2
    exit 1
    ;;
esac

mkdir -p "$HOME/Library/LaunchAgents" "$LOG_DIR"

cat > "$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-c</string>
    <string>export PATH="$AICX_DIR:/usr/bin:/bin:\$HOME/.local/bin:\$HOME/.cargo/bin"; "$AICX_BIN" catalog rebuild &amp;&amp; "$AICX_BIN" index</string>
  </array>
  <key>StartInterval</key>
  <integer>$INTERVAL</integer>
  <key>RunAtLoad</key>
  <false/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>LowPriorityBackgroundIO</key>
  <true/>
  <key>Nice</key>
  <integer>10</integer>
  <key>StandardOutPath</key>
  <string>$LOG_DIR/aicx-reindex.out.log</string>
  <key>StandardErrorPath</key>
  <string>$LOG_DIR/aicx-reindex.err.log</string>
</dict>
</plist>
PLIST_EOF

plutil -lint "$PLIST" >/dev/null

# Idempotent refresh: drop any prior registration, then bootstrap the new one.
launchctl bootout "$(gui_domain)/$LABEL" 2>/dev/null || true
launchctl bootstrap "$(gui_domain)" "$PLIST"

if launchctl print "$(gui_domain)/$LABEL" >/dev/null 2>&1; then
  note "reindex schedule: every ${INTERVAL}s via $LABEL (aicx: $AICX_BIN)"
  note "logs: $LOG_DIR/aicx-reindex.{out,err}.log"
else
  echo "Error: LaunchAgent $LABEL failed to register" >&2
  exit 1
fi
