#!/usr/bin/env bash
set -euo pipefail
# install-reindex-schedule.sh — background reindex cadence for AICX (macOS).
#
# Installs a per-user LaunchAgent that runs
#   aicx catalog refresh && aicx index
# every AICX_REINDEX_INTERVAL seconds (default 8640 = 2 h 24 min), so the catalog
# admits new sessions and the lexical index republishes without anyone
# remembering to run it. launchd serializes per-label, so a long rebuild
# never overlaps the next tick.
#
# Usage:
#   bash tools/install-reindex-schedule.sh              # install / refresh
#   bash tools/install-reindex-schedule.sh --uninstall  # remove agent + plist
#
# Env:
#   AICX_REINDEX_INTERVAL  seconds between runs (default 8640)
#   AICX_BIN               explicit aicx binary (default: resolve from PATH)
#
# Non-macOS hosts: prints a note and exits 0 (the schedule is launchd-only
# for now; a systemd user timer is the natural Linux counterpart).

LABEL="com.loctree.aicx.reindex"
LEGACY_LABEL="io.vetcoders.aicx.reindex"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LEGACY_PLIST="$HOME/Library/LaunchAgents/$LEGACY_LABEL.plist"
LOG_DIR="$HOME/.aicx/logs"
INTERVAL="${AICX_REINDEX_INTERVAL:-8640}"

note() { printf '  %s\n' "$*"; }

if [ "$(uname -s)" != "Darwin" ]; then
  note "reindex schedule: skipped (launchd-only; this host is $(uname -s))"
  exit 0
fi

gui_domain() { printf 'gui/%s' "$(id -u)"; }

# launchd removes a booted-out job asynchronously; an immediate bootstrap of
# the same label races that removal and fails with "Bootstrap failed: 5:
# Input/output error". Wait for the label to disappear, and retry bootstrap.
BOOTSTRAP_ATTEMPTS="${AICX_RUNTIME_BOOTSTRAP_ATTEMPTS:-5}"
case "$BOOTSTRAP_ATTEMPTS" in
  ''|*[!0-9]*|0)
    echo "Error: AICX_RUNTIME_BOOTSTRAP_ATTEMPTS must be a positive integer" >&2
    exit 1
    ;;
esac

wait_until_unloaded() {
  local label="$1"
  local tries=20
  while launchctl print "$(gui_domain)/$label" >/dev/null 2>&1; do
    tries=$((tries - 1))
    if [ "$tries" -le 0 ]; then
      return 1
    fi
    sleep 0.25
  done
  return 0
}

bootout_and_wait() {
  local label="$1"
  launchctl bootout "$(gui_domain)/$label" 2>/dev/null || true
  wait_until_unloaded "$label" || true
}

bootstrap_with_retry() {
  local plist="$1"
  local label="$2"
  local attempt=1
  local err=""
  while :; do
    err="$(launchctl bootstrap "$(gui_domain)" "$plist" 2>&1)" || true
    if launchctl print "$(gui_domain)/$label" >/dev/null 2>&1; then
      return 0
    fi
    if [ "$attempt" -ge "$BOOTSTRAP_ATTEMPTS" ]; then
      break
    fi
    attempt=$((attempt + 1))
    sleep 0.5
  done
  if [ -n "$err" ]; then
    printf '%s\n' "$err" >&2
  fi
  return 1
}

if [ "${1:-}" = "--uninstall" ]; then
  launchctl bootout "$(gui_domain)/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"
  launchctl bootout "$(gui_domain)/$LEGACY_LABEL" 2>/dev/null || true
  rm -f "$LEGACY_PLIST"
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

CANONICAL_BACKUP="$(mktemp "${TMPDIR:-/tmp}/aicx-reindex-canonical.XXXXXX")"
LEGACY_BACKUP="$(mktemp "${TMPDIR:-/tmp}/aicx-reindex-legacy.XXXXXX")"
CANONICAL_EXISTED=0
LEGACY_EXISTED=0
if [ -f "$PLIST" ]; then
  CANONICAL_EXISTED=1
  cp "$PLIST" "$CANONICAL_BACKUP"
fi
if [ -f "$LEGACY_PLIST" ]; then
  LEGACY_EXISTED=1
  cp "$LEGACY_PLIST" "$LEGACY_BACKUP"
fi
CANONICAL_WAS_LOADED=0
LEGACY_WAS_LOADED=0
SERVICES_TOUCHED=0
TRANSACTION_ACTIVE=1
LEGACY_ARCHIVE=""

restore_file() {
  local existed="$1"
  local backup="$2"
  local target="$3"
  if [ "$existed" = "1" ]; then
    cp "$backup" "$target"
  else
    rm -f "$target"
  fi
}

cleanup_backups() {
  rm -f "$CANONICAL_BACKUP" "$LEGACY_BACKUP"
}

rollback_on_exit() {
  status=$?
  trap - EXIT
  if [ "$status" -ne 0 ] && [ "$TRANSACTION_ACTIVE" = "1" ]; then
    if [ "$SERVICES_TOUCHED" = "1" ]; then
      bootout_and_wait "$LABEL"
      bootout_and_wait "$LEGACY_LABEL"
    fi
    restore_file "$CANONICAL_EXISTED" "$CANONICAL_BACKUP" "$PLIST"
    restore_file "$LEGACY_EXISTED" "$LEGACY_BACKUP" "$LEGACY_PLIST"
    if [ -n "$LEGACY_ARCHIVE" ]; then
      rm -f "$LEGACY_ARCHIVE"
    fi
    reload_failed=0
    if [ "$SERVICES_TOUCHED" = "1" ] && [ "$CANONICAL_WAS_LOADED" = "1" ]; then
      if ! bootstrap_with_retry "$PLIST" "$LABEL"; then
        reload_failed=1
        echo "Recover manually: launchctl bootstrap $(gui_domain) $PLIST" >&2
      fi
    fi
    if [ "$SERVICES_TOUCHED" = "1" ] && [ "$LEGACY_WAS_LOADED" = "1" ]; then
      if ! bootstrap_with_retry "$LEGACY_PLIST" "$LEGACY_LABEL"; then
        reload_failed=1
        echo "Recover manually: launchctl bootstrap $(gui_domain) $LEGACY_PLIST" >&2
      fi
    fi
    if [ "$reload_failed" = "0" ]; then
      note "reindex rollback: previous plist files and loaded state restored"
    else
      echo "Warning: previous reindex plist files restored, but a prior loaded job could not be reloaded" >&2
    fi
  fi
  cleanup_backups
  exit "$status"
}
trap rollback_on_exit EXIT

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
    <string>export PATH="$AICX_DIR:/usr/bin:/bin:\$HOME/.local/bin:\$HOME/.cargo/bin"; refresh_report="\$("$AICX_BIN" catalog refresh --json)" || exit 1; printf '%s\n' "\$refresh_report"; if printf '%s\n' "\$refresh_report" | /usr/bin/grep -q '"catalog_present": false'; then "$AICX_BIN" catalog rebuild || exit 1; fi; exec "$AICX_BIN" index</string>
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

MANAGER="$(launchctl managername 2>/dev/null || true)"
if [ "$MANAGER" != "Aqua" ]; then
  TRANSACTION_ACTIVE=0
  trap - EXIT
  cleanup_backups
  note "reindex schedule: plist written to $PLIST"
  note "reindex schedule: not loaded — this shell is not an Aqua login (launchctl managername=${MANAGER:-unknown})"
  note "reindex schedule: it loads at the next GUI login; do not run this as root"
  exit 0
fi

# Capture the exact launchd state before changing either registration. A failed
# bootstrap restores both plist files and reloads only jobs that were loaded.
if launchctl print "$(gui_domain)/$LABEL" >/dev/null 2>&1; then
  CANONICAL_WAS_LOADED=1
fi
if launchctl print "$(gui_domain)/$LEGACY_LABEL" >/dev/null 2>&1; then
  LEGACY_WAS_LOADED=1
fi
SERVICES_TOUCHED=1
bootout_and_wait "$LEGACY_LABEL"
bootout_and_wait "$LABEL"
if ! bootstrap_with_retry "$PLIST" "$LABEL"; then
  echo "Error: LaunchAgent $LABEL failed to register; rolling back" >&2
  exit 1
fi

# The canonical job is confirmed loaded. Only now retire the legacy path,
# preserving its exact bytes in a unique, recoverable migration archive.
if [ -f "$LEGACY_PLIST" ]; then
  LEGACY_ARCHIVE="$(mktemp "${LEGACY_PLIST}.migrated.$(date -u +%Y%m%dT%H%M%SZ).XXXXXX")"
  mv -f "$LEGACY_PLIST" "$LEGACY_ARCHIVE"
fi
TRANSACTION_ACTIVE=0
trap - EXIT
cleanup_backups

note "reindex schedule: loaded every ${INTERVAL}s via $LABEL (aicx: $AICX_BIN)"
if [ -n "$LEGACY_ARCHIVE" ]; then
  note "legacy schedule archived: $LEGACY_ARCHIVE"
fi
note "logs: $LOG_DIR/aicx-reindex.{out,err}.log"
