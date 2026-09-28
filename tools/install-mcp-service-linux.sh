#!/usr/bin/env bash
# Linux user-service install is not shipped. Report the gap; do not write a unit.
echo "aicx: Linux systemd --user service is not in this binary. No unit was written." >&2
exit 1
