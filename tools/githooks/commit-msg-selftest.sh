#!/usr/bin/env bash
# Self-test for the commit provenance hooks: tools/githooks/commit-msg
# (validator) and tools/githooks/prepare-commit-msg (generator).
#
# Run via `make hooks-test`.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
hook="$root/tools/githooks/commit-msg"
prepare="$root/tools/githooks/prepare-commit-msg"

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

# --- validator: accepted shapes -------------------------------------------

pass human-void "$(
    cat <<'EOF'
[maciej/manual] feat: Skip session re-scan during install

Install no longer runs aicx all -H 10000.

Authored-By: maciej <void@div0.space>
session_id: 019e93be-379d-7303-8ad4-ffae468db99f
time: 2026-08-18T21:16:04+02:00
runtime: vibecrafted
EOF
)"

pass agent-codex "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Why this commit exists.

Authored-By: codex <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
time: 2026-06-04T14:08:27-06:00
runtime: iterm2
EOF
)"

# session_pid is optional: a message carrying one is as valid as one without.
pass agent-with-session-pid "$(
    cat <<'EOF'
[claude/interactive] fix: separate two processes of one session

Why this commit exists.

Authored-By: claude <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
time: 2026-06-04T14:08:27-06:00
runtime: iterm2
session_pid: 35432
EOF
)"

pass utc-offset-z "$(
    cat <<'EOF'
[codex/ci] ci: run release signing on a reserved runner

Why this commit exists.

Authored-By: codex <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
time: 2026-06-04T20:08:27Z
runtime: github-actions
EOF
)"

# --- validator: rejected shapes -------------------------------------------

reject human-forced-agent-mailbox "$(
    cat <<'EOF'
[maciej/manual] feat: Skip session re-scan during install

Install no longer runs aicx all -H 10000.

Authored-By: maciej <agents@vetcoders.io>
session_id: 019e93be-379d-7303-8ad4-ffae468db99f
time: 2026-08-18T21:16:04+02:00
runtime: vibecrafted
EOF
)" "human lane"

reject agent-human-mailbox "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Why this commit exists.

Authored-By: codex <void@div0.space>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
time: 2026-06-04T14:08:27-06:00
runtime: iterm2
EOF
)" "Authored-By: codex <agents@vetcoders.io>"

# The fleet agreed on one write-path time key. Legacy keys are named in the
# error so the author is told how to migrate, not merely that they failed.
reject legacy-timestamp-key "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Why this commit exists.

Authored-By: codex <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
timestamp: 2026_0604_1408_MDT
runtime: iterm2
EOF
)" "legacy"

reject legacy-date-key "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Why this commit exists.

Authored-By: codex <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
date: 2026-06-04T15:36:27 MDT
runtime: iterm2
EOF
)" "legacy"

reject session-pid-not-numeric "$(
    cat <<'EOF'
[claude/interactive] fix: describe the change

Why this commit exists.

Authored-By: claude <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
time: 2026-06-04T14:08:27-06:00
runtime: iterm2
session_pid: not-a-pid
EOF
)" "decimal process id"

# A legacy trailer must not be mistaken for an explanatory body. Before the
# fleet agreed on `time:`, an unrecognized key counted as body text and let a
# bodyless commit through the one check vc-trust calls decisive.
reject legacy-trailer-is-not-a-body "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Authored-By: codex <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
timestamp: 2026_0604_1408_MDT
date: 2026-06-04T15:36:27 MDT
runtime: iterm2
EOF
)" "explanatory body"

# --- generator -------------------------------------------------------------

gen() {
    # gen <name> <message> [env assignments...] -> prints resulting message
    local msg="$1"; shift
    local tmp
    tmp="$(mktemp)"
    printf '%s\n' "$msg" >"$tmp"
    env "$@" "$prepare" "$tmp" message >/dev/null 2>&1 || true
    cat "$tmp"
    rm -f "$tmp"
}

expect_line() {
    local name="$1" out="$2" needle="$3"
    if ! printf '%s\n' "$out" | grep -Fq "$needle"; then
        printf 'FAIL generator: %s (missing %s)\n' "$name" "$needle" >&2
        printf '%s\n' "$out" >&2
        exit 1
    fi
    printf 'ok generator: %s\n' "$name"
}

expect_absent() {
    local name="$1" out="$2" needle="$3"
    if printf '%s\n' "$out" | grep -Fq "$needle"; then
        printf 'FAIL generator: %s (unexpected %s)\n' "$name" "$needle" >&2
        printf '%s\n' "$out" >&2
        exit 1
    fi
    printf 'ok generator: %s\n' "$name"
}

real_session="019e93be-379d-7303-9ad4-ffae468db99f"
subject_agent='[codex/interactive] chore: describe the change'
subject_human='[maciej/manual] feat: describe the change'

out="$(gen "$subject_agent

Why this commit exists." CLAUDE_CODE_SESSION_ID="$real_session" VC_SESSION_PID=0)"
expect_line fills-agent-mailbox "$out" "Authored-By: codex <agents@vetcoders.io>"
expect_line fills-session-id "$out" "session_id: $real_session"
expect_line fills-time-key "$out" "time: "
expect_absent no-legacy-key "$out" "timestamp: "

# Provenance is a measurement, not a self-report: a measured session id replaces
# whatever the message claimed about itself.
out="$(gen "$subject_agent

Why this commit exists.

session_id: 00000000-0000-4000-8000-deadbeef0000" \
    CLAUDE_CODE_SESSION_ID="$real_session" VC_SESSION_PID=0)"
expect_line measured-overwrites-claim "$out" "session_id: $real_session"
expect_absent measured-drops-stale-claim "$out" "deadbeef0000"

# ...but an unmeasurable value never invents one and never destroys a declaration
# the author supplied. PATH is trimmed so the aicx fallback cannot resolve.
out="$(gen "$subject_agent

Why this commit exists.

session_id: 019e93be-379d-7303-9ad4-ffae468db99f" \
    PATH=/usr/bin:/bin CLAUDE_CODE_SESSION_ID= CODEX_SESSION_ID= ATUIN_SESSION= VC_SESSION_PID=0)"
expect_line unmeasured-keeps-declaration "$out" "session_id: $real_session"

out="$(gen "$subject_agent

Why this commit exists." \
    PATH=/usr/bin:/bin CLAUDE_CODE_SESSION_ID= CODEX_SESSION_ID= ATUIN_SESSION= VC_SESSION_PID=0)"
expect_absent unmeasured-invents-nothing "$out" "session_id:"

# The human lane keeps a human address; the generator must not force agents@.
out="$(gen "$subject_human

Why this commit exists." CLAUDE_CODE_SESSION_ID="$real_session" VC_SESSION_PID=0)"
expect_line human-lane-keeps-human-mailbox "$out" "Authored-By: maciej <void@div0.space>"

# A human commit must never inherit an agent's transcript. With no session in
# the environment the aicx fallback would still report the machine's current
# agent session, so the human lane must not consult it.
out="$(gen "$subject_human

Why this commit exists." \
    CLAUDE_CODE_SESSION_ID= CODEX_SESSION_ID= ATUIN_SESSION= VC_SESSION_PID=0)"
expect_absent human-lane-never-borrows-agent-session "$out" "session_id:"

# session_pid toggle, both directions.
out="$(gen "$subject_agent

Why this commit exists." CLAUDE_CODE_SESSION_ID="$real_session" CLAUDE_PID=35432 VC_SESSION_PID=1)"
expect_line session-pid-on "$out" "session_pid: 35432"

out="$(gen "$subject_agent

Why this commit exists." CLAUDE_CODE_SESSION_ID="$real_session" CLAUDE_PID=35432 VC_SESSION_PID=0)"
expect_absent session-pid-off "$out" "session_pid:"

# Trailers must land as one compact block. (Git's own parser rejects the block
# regardless, because `session_id` contains '_'; this is for readable messages
# and stable diffs.)
tmp="$(mktemp)"
printf '%s\n\nWhy this commit exists.\n' "$subject_agent" >"$tmp"
env CLAUDE_CODE_SESSION_ID="$real_session" CLAUDE_PID=35432 VC_SESSION_PID=1 \
    "$prepare" "$tmp" message >/dev/null 2>&1 || true
if awk '/^Authored-By: /{inblock=1} inblock && !NF {found=1} END{exit !found}' "$tmp"; then
    printf 'FAIL generator: blank line inside trailer block\n' >&2
    cat -A "$tmp" | tail -8 >&2
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok generator: trailer block is compact\n'

# Git-generated subjects are commit-msg's business, not the generator's.
out="$(gen "Merge branch 'main' into agent/example" CLAUDE_CODE_SESSION_ID="$real_session")"
expect_absent merge-subject-untouched "$out" "Authored-By:"

# What the generator writes must satisfy the validator it feeds.
tmp="$(mktemp)"
printf '%s\n\nWhy this commit exists.\n' "$subject_agent" >"$tmp"
env CLAUDE_CODE_SESSION_ID="$real_session" CLAUDE_PID=35432 VC_SESSION_PID=1 \
    "$prepare" "$tmp" message >/dev/null 2>&1 || true
if ! "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL generator: output rejected by validator\n' >&2
    "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok generator: output satisfies validator\n'

# --- parity between the two installable hook sets ---------------------------

# tools/githooks (symlink install) and tools/git-hooks (core.hooksPath embargo
# install) are separate delivery paths that each ship their own copy. Drift
# between them means one install mode silently enforces an older standard.
for name in commit-msg prepare-commit-msg; do
    if ! cmp -s "$root/tools/githooks/$name" "$root/tools/git-hooks/$name"; then
        printf 'FAIL parity: tools/githooks/%s != tools/git-hooks/%s\n' "$name" "$name" >&2
        diff -u "$root/tools/git-hooks/$name" "$root/tools/githooks/$name" >&2 || true
        exit 1
    fi
    printf 'ok parity: %s\n' "$name"
done

printf 'commit provenance selftest: all passed\n'
