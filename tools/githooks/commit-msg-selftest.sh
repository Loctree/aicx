#!/usr/bin/env bash
# Self-test for tools/githooks/commit-msg. Not wired into make test.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
hook="$root/tools/githooks/commit-msg"

pass() {
    local name="$1" body="$2"
    local tmp
    tmp="$(mktemp)"
    printf '%s\n' "$body" >"$tmp"
    if ! "$hook" "$tmp" >/dev/null 2>&1; then
        printf 'FAIL accept: %s\n' "$name" >&2
        "$hook" "$tmp" >&2 || true
        rm -f "$tmp"
        exit 1
    fi
    rm -f "$tmp"
    printf 'ok accept: %s\n' "$name"
}

reject() {
    local name="$1" body="$2" needle="$3"
    local tmp err
    tmp="$(mktemp)"
    err="$(mktemp)"
    printf '%s\n' "$body" >"$tmp"
    if "$hook" "$tmp" >/dev/null 2>"$err"; then
        printf 'FAIL reject: %s (hook accepted)\n' "$name" >&2
        rm -f "$tmp" "$err"
        exit 1
    fi
    if ! grep -Fq "$needle" "$err"; then
        printf 'FAIL reject: %s (missing %s)\n' "$name" "$needle" >&2
        cat "$err" >&2
        rm -f "$tmp" "$err"
        exit 1
    fi
    rm -f "$tmp" "$err"
    printf 'ok reject: %s\n' "$name"
}

pass human-void "$(
    cat <<'EOF'
[maciej/manual] feat: Skip session re-scan during install

Install no longer runs aicx all -H 10000.

Authored-By: maciej <void@div0.space>
session_id: 019e93be-379d-7303-8ad4-ffae468db99f
timestamp: 2026_0818_2116_CEST
runtime: vibecrafted
EOF
)"

pass agent-codex "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Why this commit exists.

Authored-By: codex <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
timestamp: 2026_0604_1408_MDT
runtime: iterm2
EOF
)"

reject human-forced-agent-mailbox "$(
    cat <<'EOF'
[maciej/manual] feat: Skip session re-scan during install

Install no longer runs aicx all -H 10000.

Authored-By: maciej <agents@vetcoders.io>
session_id: 019e93be-379d-7303-8ad4-ffae468db99f
timestamp: 2026_0818_2116_CEST
runtime: vibecrafted
EOF
)" "human lane"

reject agent-human-mailbox "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Why this commit exists.

Authored-By: codex <void@div0.space>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
timestamp: 2026_0604_1408_MDT
runtime: iterm2
EOF
)" "Authored-By: codex <agents@vetcoders.io>"

printf 'commit-msg selftest: all passed\n'
