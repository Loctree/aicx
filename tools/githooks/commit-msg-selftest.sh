#!/usr/bin/env bash
# Self-test for the commit provenance hooks: tools/githooks/commit-msg
# (validator) and tools/githooks/prepare-commit-msg (generator).
#
# Run via `make hooks-test`.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
hook="$root/tools/githooks/commit-msg"
prepare="$root/tools/githooks/prepare-commit-msg"

# Measurement env must not leak from the shell running the test into the
# hooks. An inherited CODEX_THREAD_ID or a live `aicx` would rewrite fixtures.
hook_env() {
    env \
        AICX_SESSION_ID= \
        CODEX_THREAD_ID= \
        CODEX_SESSION_ID= \
        CLAUDE_SESSION_ID= \
        CLAUDE_CODE_SESSION_ID= \
        CURSOR_CONVERSATION_ID= \
        GEMINI_SESSION_ID= \
        JUNIE_SESSION_ID= \
        KIMI_SESSION_ID= \
        GROK_SESSION_ID= \
        GROK_THREAD_ID= \
        ATUIN_SESSION= \
        CLAUDE_PID= \
        CODEX_PID= \
        VIBECRAFTED_COMMIT_RUNTIME= \
        VIBECRAFTED_RUNTIME= \
        TERM_PROGRAM= \
        VC_SESSION_PID=0 \
        VC_GIT_VERSION= \
        PATH="/usr/bin:/bin" \
        HOME="${HOME:-/tmp}" \
        TMPDIR="${TMPDIR:-/tmp}" \
        "$@"
}

pass() {
    local name="$1" body="$2"
    local tmp
    tmp="$(mktemp)"
    printf '%s\n' "$body" >"$tmp"
    if ! hook_env "$hook" "$tmp" >/dev/null 2>&1; then
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
    if hook_env "$hook" "$tmp" >/dev/null 2>"$err"; then
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

# A derivable mailbox is a measurement, so the generator corrects it and the
# validator then accepts the file. What stays a rejection is a human lane
# whose address cannot be derived (agent name + runtime manual).
reject human-lane-underivable-mailbox "$(
    cat <<'EOF'
[codex/manual] feat: describe the change

Why this commit exists.

Authored-By: codex <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
time: 2026-06-04T14:08:27-06:00
runtime: iterm2
EOF
)" "human lane"

reject legacy-beside-valid-time "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Why this commit exists.

Authored-By: codex <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
time: 2026-06-04T14:08:27-06:00
timestamp: 2026_0604_1408_MDT
runtime: iterm2
EOF
)" "legacy"

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

# VC_SESSION_PID=0 drops the trailer before validation. With recording left on
# and no measurable pid, a non-numeric declaration must still be rejected.
tmp="$(mktemp)"
err="$(mktemp)"
cat >"$tmp" <<'EOF'
[claude/interactive] fix: describe the change

Why this commit exists.

Authored-By: claude <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
time: 2026-06-04T14:08:27-06:00
runtime: iterm2
session_pid: not-a-pid
EOF
if hook_env env VC_SESSION_PID=1 "$hook" "$tmp" >/dev/null 2>"$err"; then
    printf 'FAIL reject: session-pid-not-numeric (hook accepted)\n' >&2
    rm -f "$tmp" "$err"
    exit 1
fi
if ! grep -Fq "decimal process id" "$err"; then
    printf 'FAIL reject: session-pid-not-numeric (missing decimal process id)\n' >&2
    cat "$err" >&2
    rm -f "$tmp" "$err"
    exit 1
fi
rm -f "$tmp" "$err"
printf 'ok reject: session-pid-not-numeric\n'

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
    # gen <message> [env assignments...] -> prints resulting message.
    # Starts from a scrubbed environment; assignments in "$@" override.
    local msg="$1"; shift
    local tmp
    tmp="$(mktemp)"
    printf '%s\n' "$msg" >"$tmp"
    hook_env "$@" "$prepare" "$tmp" message >/dev/null 2>&1 || true
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

Why this commit exists." CODEX_SESSION_ID="$real_session" VC_SESSION_PID=0)"
expect_line fills-agent-mailbox "$out" "Authored-By: codex <agents@vetcoders.io>"
expect_line fills-session-id "$out" "session_id: $real_session"
expect_line fills-time-key "$out" "time: "
expect_absent no-legacy-key "$out" "timestamp: "

# Provenance is a measurement, not a self-report: a measured session id replaces
# whatever the message claimed about itself.
out="$(gen "$subject_agent

Why this commit exists.

session_id: 00000000-0000-4000-8000-deadbeef0000" \
    CODEX_SESSION_ID="$real_session" VC_SESSION_PID=0)"
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

Why this commit exists." CLAUDE_CODE_SESSION_ID="$real_session" CODEX_THREAD_ID="$real_session" VC_SESSION_PID=0)"
expect_line human-lane-keeps-human-mailbox "$out" "Authored-By: maciej <void@div0.space>"
expect_absent human-lane-ignores-agent-session "$out" "session_id:"

# A human commit must never inherit an agent's transcript. With no session in
# the environment the aicx fallback would still report the machine's current
# agent session, so the human lane must not consult it.
out="$(gen "$subject_human

Why this commit exists." \
    CLAUDE_CODE_SESSION_ID= CODEX_SESSION_ID= ATUIN_SESSION= VC_SESSION_PID=0)"
expect_absent human-lane-never-borrows-agent-session "$out" "session_id:"

# session_pid belongs to the subject. A Codex commit must not record CLAUDE_PID
# just because that variable is also exported.
out="$(gen "$subject_agent

Why this commit exists." CODEX_SESSION_ID="$real_session" CODEX_PID=222 CLAUDE_PID=111 VC_SESSION_PID=1)"
expect_line session-pid-matches-subject "$out" "session_pid: 222"
expect_absent session-pid-ignores-other-agent "$out" "session_pid: 111"

out="$(gen "$subject_agent

Why this commit exists." CODEX_SESSION_ID="$real_session" CLAUDE_PID=35432 VC_SESSION_PID=1)"
expect_absent session-pid-omits-unmatched "$out" "session_pid:"

# The opt-out removes a pid already in the message. An enabled toggle with no
# matching pid leaves that declaration alone.
out="$(gen "$subject_agent

Why this commit exists.

session_pid: 111" CODEX_SESSION_ID="$real_session" VC_SESSION_PID=0)"
expect_absent session-pid-off-clears-existing "$out" "session_pid:"

out="$(gen "$subject_agent

Why this commit exists.

session_pid: 111" CODEX_SESSION_ID="$real_session" CLAUDE_PID=222 VC_SESSION_PID=1)"
expect_line session-pid-keeps-declaration-without-match "$out" "session_pid: 111"

# Trailers must land as one compact block. (Git's own parser rejects the block
# regardless, because `session_id` contains '_'; this is for readable messages
# and stable diffs.)
tmp="$(mktemp)"
printf '%s\n\nWhy this commit exists.\n' "$subject_agent" >"$tmp"
hook_env CODEX_SESSION_ID="$real_session" CODEX_PID=35432 VC_SESSION_PID=1 \
    TERM_PROGRAM=iTerm.app \
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
hook_env CODEX_SESSION_ID="$real_session" CODEX_PID=35432 VC_SESSION_PID=1 \
    TERM_PROGRAM=iTerm.app \
    "$prepare" "$tmp" message >/dev/null 2>&1 || true
if ! hook_env CODEX_SESSION_ID="$real_session" CODEX_PID=35432 VC_SESSION_PID=1 \
    TERM_PROGRAM=iTerm.app \
    "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL generator: output rejected by validator\n' >&2
    "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok generator: output satisfies validator\n'

thread_id="019eba52-81db-7d31-bb28-6343f05c4b79"
other_id="00000000-0000-4000-8000-deadbeef0000"

out="$(gen "$subject_agent

Why this commit exists." CODEX_THREAD_ID="$thread_id" VC_SESSION_PID=0)"
expect_line reads-codex-thread-id "$out" "session_id: $thread_id"

out="$(gen "$subject_agent

Why this commit exists." CODEX_THREAD_ID="$thread_id" CODEX_SESSION_ID="$other_id" VC_SESSION_PID=0)"
expect_line thread-id-outranks-session-id "$out" "session_id: $thread_id"
expect_absent thread-id-drops-session-id "$out" "$other_id"

# Interactive commit: prepare-commit-msg sees an empty subject and must not
# invent trailers. commit-msg runs the generator after the editor.
tmp="$(mktemp)"
printf '\n# Please enter the commit message for your changes. Lines starting\n# with '\''#'\'' will be ignored, and an empty message aborts the commit.\n' >"$tmp"
hook_env CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app \
    "$prepare" "$tmp" commit >/dev/null 2>&1 || true
if grep -q '^Authored-By:' "$tmp"; then
    printf 'FAIL generator: pre-editor pass wrote trailers without a subject\n' >&2
    cat "$tmp" >&2
    rm -f "$tmp"
    exit 1
fi
printf '%s\n\nWhy this commit exists.\n\n# Please enter the commit message for your changes.\n' \
    "$subject_agent" >"$tmp"
if ! hook_env CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app \
    "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL generator: interactive commit-msg did not accept a filled message\n' >&2
    hook_env CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
# git commit -m/-F keeps comment lines. They must stay above the footer,
# not be moved under runtime: where they become the last stored block.
if ! awk -v id="$thread_id" '
    /^# Please enter/ { saw=1 }
    $0 == "session_id: " id { if (saw) found=1; else below=1 }
    END { exit !(found && !below) }
' "$tmp"; then
    printf 'FAIL generator: template comment landed below the footer\n' >&2
    cat "$tmp" >&2
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok generator: interactive commit fills trailers before the template\n'

# git commit -v / cleanup=scissors. The validator must accept because the
# footer is above the cut, and the stored prefix must actually contain it.
tmp="$(mktemp)"
cat >"$tmp" <<EOF
$subject_agent

Why this commit exists.

# ------------------------ >8 ------------------------
# Do not modify or remove the line above.
diff --git a/README.md b/README.md
session_id: $other_id
EOF
if ! hook_env CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app \
    "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL generator: scissors message rejected\n' >&2
    hook_env CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
if ! awk -v id="$thread_id" '
    /^#[[:space:]]*-{2,}[[:space:]]*>8[[:space:]]*-{2,}/ { exit }
    $0 == "session_id: " id { found=1 }
    END { exit !found }
' "$tmp"; then
    printf 'FAIL generator: session_id is not above the scissors line\n' >&2
    cat "$tmp" >&2
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok generator: trailers sit above the scissors line\n'

# A citation in the body is not a footer. The measured runtime is appended;
# the cited line stays.
out="$(gen "$subject_agent

Why this commit exists.
runtime: legacy-backend
That line cites an old value.

Authored-By: codex <agents@vetcoders.io>
session_id: $other_id
time: 2020-01-01T00:00:00Z
runtime: github-actions" \
    CODEX_SESSION_ID="$real_session" TERM_PROGRAM=iTerm.app VC_SESSION_PID=0)"
expect_line body-citation-survives "$out" "runtime: legacy-backend"
expect_line measured-runtime-in-footer "$out" "runtime: iterm2"
legacy_hits="$(printf '%s\n' "$out" | grep -c 'runtime: legacy-backend' || true)"
iterm_hits="$(printf '%s\n' "$out" | grep -c 'runtime: iterm2' || true)"
if [ "$legacy_hits" != "1" ] || [ "$iterm_hits" != "1" ]; then
    printf 'FAIL generator: body runtime citation was rewritten (%s legacy, %s iterm)\n' \
        "$legacy_hits" "$iterm_hits" >&2
    printf '%s\n' "$out" >&2
    exit 1
fi
printf 'ok generator: body citation kept and footer measured\n'

# No TERM_PROGRAM and no VIBECRAFTED_* runtime: do not invent "interactive",
# and do not overwrite a footer the author already wrote.
out="$(gen "$subject_agent

Why this commit exists.

runtime: github-actions" \
    CLAUDE_CODE_SESSION_ID="$real_session" VC_SESSION_PID=0)"
expect_line unmeasured-runtime-kept "$out" "runtime: github-actions"
expect_absent unmeasured-runtime-not-invented "$out" "runtime: interactive"

fake_bin="$(mktemp -d)"
cat >"$fake_bin/aicx" <<'EOF'
#!/bin/sh
agent="${AICX_FAKE_AGENT:-codex}"
printf '%s\n' "{\"session_id\":\"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee\",\"agent\":\"${agent}\",\"source\":\"disk\"}"
EOF
chmod +x "$fake_bin/aicx"
atuin_id="11111111-2222-4333-8444-555555555555"
out="$(gen "$subject_agent

Why this commit exists." \
    PATH="$fake_bin:/usr/bin:/bin" ATUIN_SESSION="$atuin_id" VC_SESSION_PID=0)"
expect_line agent-prefers-aicx-over-atuin "$out" "session_id: aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
expect_absent agent-does-not-take-atuin "$out" "$atuin_id"

out="$(gen "$subject_human

Why this commit exists." \
    PATH="$fake_bin:/usr/bin:/bin" ATUIN_SESSION="$atuin_id" \
    CLAUDE_CODE_SESSION_ID="$real_session" VC_SESSION_PID=0)"
expect_line human-atuin-fallback "$out" "session_id: $atuin_id"
expect_absent human-atuin-not-agent "$out" "$real_session"
expect_absent human-atuin-not-aicx "$out" "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
rm -f "$fake_bin/aicx"
rmdir "$fake_bin"

# Signed-off-by is part of the footer. The measured session replaces the
# stale one; the foreign trailer stays, and it is not duplicated.
out="$(gen "$subject_agent

Why this commit exists.

Authored-By: codex <agents@vetcoders.io>
session_id: $other_id
time: 2020-01-01T00:00:00Z
runtime: github-actions
Signed-off-by: Ada <ada@example.com>" \
    CODEX_SESSION_ID="$real_session" TERM_PROGRAM=iTerm.app VC_SESSION_PID=0)"
expect_line keeps-signed-off-by "$out" "Signed-off-by: Ada <ada@example.com>"
expect_line signed-off-overwrites-session "$out" "session_id: $real_session"
expect_absent signed-off-drops-stale-session "$out" "$other_id"
session_lines="$(printf '%s\n' "$out" | grep -c '^session_id: ' || true)"
signed_lines="$(printf '%s\n' "$out" | grep -c '^Signed-off-by: ' || true)"
if [ "$session_lines" != "1" ] || [ "$signed_lines" != "1" ]; then
    printf 'FAIL generator: footer duplicated (%s session_id, %s signed-off)\n' \
        "$session_lines" "$signed_lines" >&2
    printf '%s\n' "$out" >&2
    exit 1
fi
printf 'ok generator: foreign trailer stays inside one footer\n'

# core.commentChar=; makes Git emit a semicolon scissors line. Trailers must
# sit above it, and a session id under the cut must not satisfy the validator.
tmp="$(mktemp)"
cat >"$tmp" <<EOF
$subject_agent

Why this commit exists.

; ------------------------ >8 ------------------------
; Do not modify or remove the line above.
diff --git a/README.md b/README.md
session_id: $other_id
EOF
if ! hook_env \
    GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentChar GIT_CONFIG_VALUE_0=';' \
    CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app \
    "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL generator: semicolon scissors message rejected\n' >&2
    hook_env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentChar GIT_CONFIG_VALUE_0=';' \
        CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
if ! awk -v id="$thread_id" '
    /^;[[:space:]]*-{2,}[[:space:]]*>8/ { exit }
    $0 == "session_id: " id { found=1 }
    END { exit !found }
' "$tmp"; then
    printf 'FAIL generator: session_id is not above the semicolon scissors line\n' >&2
    cat "$tmp" >&2
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok generator: trailers sit above a semicolon scissors line\n'

# commentChar=auto: a non-trailing hash line strikes "#", so Git's next
# candidate is ";". The verbose diff is added after that choice. The "#"
# line has to sit above other body text; a trailing "#" line does not strike.
tmp="$(mktemp)"
cat >"$tmp" <<EOF
$subject_agent

# not a comment, so auto cannot pick hash
Why this commit exists.

; ------------------------ >8 ------------------------
; Do not modify or remove the line above.
diff --git a/README.md b/README.md
session_id: $other_id
EOF
if ! hook_env \
    GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentChar GIT_CONFIG_VALUE_0=auto \
    CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app \
    "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL generator: auto commentChar scissors message rejected\n' >&2
    hook_env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentChar GIT_CONFIG_VALUE_0=auto \
        CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
if ! awk -v id="$thread_id" '
    /^;[[:space:]]*-{2,}[[:space:]]*>8/ { exit }
    $0 == "session_id: " id { found=1 }
    END { exit !found }
' "$tmp"; then
    printf 'FAIL generator: session_id is not above the auto-mode scissors line\n' >&2
    cat "$tmp" >&2
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok generator: auto commentChar still cuts on the real scissors line\n'

# An authored ";" scissors is not Git's marker: the line itself blocks ";",
# so validation still sees a vendor footer underneath it.
tmp="$(mktemp)"
err="$(mktemp)"
cat >"$tmp" <<EOF
$subject_agent

Why this commit exists.

; ------------------------ >8 ------------------------
Co-Authored-By: Vendor <bot@openai.com>
EOF
if hook_env \
    GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentChar GIT_CONFIG_VALUE_0=auto \
    CODEX_SESSION_ID="$real_session" TERM_PROGRAM=iTerm.app \
    "$hook" "$tmp" >/dev/null 2>"$err"; then
    printf 'FAIL validator: authored semicolon scissors hid a vendor footer\n' >&2
    rm -f "$tmp" "$err"
    exit 1
fi
if ! grep -Fq "vendor footers" "$err"; then
    printf 'FAIL validator: authored semicolon scissors (missing vendor footers)\n' >&2
    cat "$err" >&2
    rm -f "$tmp" "$err"
    exit 1
fi
rm -f "$tmp" "$err"
printf 'ok validator: authored semicolon scissors is not a cut\n'

# git commit -F keeps an authored hash scissors line. The vendor footer under
# it must still be validated, because cleanup does not drop that suffix.
tmp="$(mktemp)"
err="$(mktemp)"
cat >"$tmp" <<EOF
$subject_agent

Why this commit exists.

# ------------------------ >8 ------------------------
Co-Authored-By: Vendor <bot@openai.com>
EOF
if hook_env CODEX_SESSION_ID="$real_session" TERM_PROGRAM=iTerm.app \
    "$hook" "$tmp" >/dev/null 2>"$err"; then
    printf 'FAIL validator: commit -F scissors hid a vendor footer\n' >&2
    rm -f "$tmp" "$err"
    exit 1
fi
if ! grep -Fq "vendor footers" "$err"; then
    printf 'FAIL validator: commit -F scissors (missing vendor footers)\n' >&2
    cat "$err" >&2
    rm -f "$tmp" "$err"
    exit 1
fi
rm -f "$tmp" "$err"
printf 'ok validator: commit -F keeps the suffix visible\n'

# Git before 2.45 ignores core.commentString. A ;; value must not hide the
# hash marker Git actually wrote.
tmp="$(mktemp)"
cat >"$tmp" <<EOF
$subject_agent

Why this commit exists.

# ------------------------ >8 ------------------------
# Do not modify or remove the line above.
diff --git a/README.md b/README.md
session_id: $other_id
EOF
if ! hook_env VC_GIT_VERSION='git version 2.43.0' \
    GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentString GIT_CONFIG_VALUE_0=';;' \
    CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app \
    "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL generator: old Git lost trailers under a hash scissors line\n' >&2
    hook_env VC_GIT_VERSION='git version 2.43.0' \
        GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentString GIT_CONFIG_VALUE_0=';;' \
        CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >&2 || true
    cat "$tmp" >&2
    rm -f "$tmp"
    exit 1
fi
if ! awk -v id="$thread_id" '
    /^#[[:space:]]*-{2,}[[:space:]]*>8/ { exit }
    $0 == "session_id: " id { found=1 }
    END { exit !found }
' "$tmp"; then
    printf 'FAIL generator: old Git did not place session_id above the hash cut\n' >&2
    cat "$tmp" >&2
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok generator: Git 2.43 ignores commentString\n'

# Git 2.45+ honors commentString, so the cut prefix is that string.
tmp="$(mktemp)"
cat >"$tmp" <<EOF
$subject_agent

Why this commit exists.

;; ------------------------ >8 ------------------------
;; Do not modify or remove the line above.
diff --git a/README.md b/README.md
session_id: $other_id
EOF
if ! hook_env VC_GIT_VERSION='git version 2.51.0' \
    GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentString GIT_CONFIG_VALUE_0=';;' \
    CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app \
    "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL generator: commentString scissors message rejected\n' >&2
    hook_env VC_GIT_VERSION='git version 2.51.0' \
        GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentString GIT_CONFIG_VALUE_0=';;' \
        CODEX_THREAD_ID="$thread_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >&2 || true
    cat "$tmp" >&2
    rm -f "$tmp"
    exit 1
fi
if ! awk -v id="$thread_id" '
    /^;;[[:space:]]*-{2,}[[:space:]]*>8/ { exit }
    $0 == "session_id: " id { found=1 }
    END { exit !found }
' "$tmp"; then
    printf 'FAIL generator: session_id is not above the commentString cut\n' >&2
    cat "$tmp" >&2
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok generator: Git 2.45+ honors commentString\n'

# auto mode, no scissors: a "#" line above the footer is body when Git has
# to pick another character. It must not be skipped as a "#" comment.
tmp="$(mktemp)"
cat >"$tmp" <<EOF
$subject_agent

# explanation
EOF
if ! hook_env \
    GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentChar GIT_CONFIG_VALUE_0=auto \
    CODEX_SESSION_ID="$real_session" TERM_PROGRAM=iTerm.app \
    "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL validator: auto mode rejected a hash body line\n' >&2
    hook_env GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.commentChar GIT_CONFIG_VALUE_0=auto \
        CODEX_SESSION_ID="$real_session" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok validator: auto mode keeps a hash line as body\n'

# A body citation of the old key is not a legacy trailer.
out="$(gen "$subject_agent

Why this commit exists.
timestamp: 2026_0604_1408_MDT
is how the old key looked.

Authored-By: codex <agents@vetcoders.io>
session_id: $real_session
time: 2026-06-04T14:08:27-06:00
runtime: iterm2" \
    PATH=/usr/bin:/bin VC_SESSION_PID=0)"
tmp="$(mktemp)"
printf '%s\n' "$out" >"$tmp"
if ! hook_env "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL validator: body citation of timestamp: was rejected\n' >&2
    hook_env "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok validator: timestamp citation in the body is not a footer\n'

other_bin="$(mktemp -d)"
cat >"$other_bin/aicx" <<'EOF'
#!/bin/sh
printf '%s\n' '{"session_id":"aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee","agent":"claude","source":"disk"}'
EOF
chmod +x "$other_bin/aicx"
out="$(gen "$subject_agent

Why this commit exists." \
    PATH="$other_bin:/usr/bin:/bin" VC_SESSION_PID=0)"
expect_absent aicx-other-agent-not-used "$out" "session_id:"
rm -f "$other_bin/aicx"
rmdir "$other_bin"
printf 'ok generator: aicx fallback from another agent is not measured\n'

subject_claude='[claude/interactive] chore: describe the change'
claude_id="019eba52-81db-7d31-bb28-6343f05c4b79"
out="$(gen "$subject_claude

Why this commit exists." \
    CODEX_THREAD_ID="$other_id" CLAUDE_CODE_SESSION_ID="$claude_id" VC_SESSION_PID=0)"
expect_line claude-ignores-codex-env "$out" "session_id: $claude_id"
expect_absent claude-does-not-record-codex "$out" "$other_id"

out="$(gen "$subject_claude

Why this commit exists." \
    CODEX_THREAD_ID="$other_id" VC_SESSION_PID=0)"
expect_absent claude-without-own-session "$out" "session_id:"
printf 'ok generator: agent env must belong to the subject\n'

subject_junie='[junie/interactive] fix: keep the native session id'
junie_id="260408-214715-abcd"
out="$(gen "$subject_junie

Why this commit exists." JUNIE_SESSION_ID="$junie_id" VC_SESSION_PID=0)"
expect_line junie-native-id "$out" "session_id: $junie_id"
tmp="$(mktemp)"
printf '%s\n' "$out" >"$tmp"
if ! hook_env JUNIE_SESSION_ID="$junie_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL validator: native Junie session id rejected\n' >&2
    hook_env JUNIE_SESSION_ID="$junie_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok validator: native Junie session id accepted\n'

subject_gemini='[gemini/interactive] fix: keep the native session id'
gemini_id="session-gemini-marble-l1-coverage"
out="$(gen "$subject_gemini

Why this commit exists." GEMINI_SESSION_ID="$gemini_id" VC_SESSION_PID=0)"
expect_line gemini-native-id "$out" "session_id: $gemini_id"
tmp="$(mktemp)"
printf '%s\n' "$out" >"$tmp"
if ! hook_env GEMINI_SESSION_ID="$gemini_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL validator: native Gemini session id rejected\n' >&2
    hook_env GEMINI_SESSION_ID="$gemini_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok validator: native Gemini session id accepted\n'

subject_kimi='[kimi/interactive] fix: keep the scoped session id'
kimi_id='11111111-1111-4111-8111-111111111111:worker'
out="$(gen "$subject_kimi

Why this commit exists." KIMI_SESSION_ID="$kimi_id" VC_SESSION_PID=0)"
expect_line kimi-scoped-id "$out" "session_id: $kimi_id"
tmp="$(mktemp)"
printf '%s\n' "$out" >"$tmp"
if ! hook_env KIMI_SESSION_ID="$kimi_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >/dev/null 2>&1; then
    printf 'FAIL validator: scoped Kimi session id rejected\n' >&2
    hook_env KIMI_SESSION_ID="$kimi_id" TERM_PROGRAM=iTerm.app "$hook" "$tmp" >&2 || true
    rm -f "$tmp"
    exit 1
fi
rm -f "$tmp"
printf 'ok validator: scoped Kimi session id accepted\n'

out="$(gen "$subject_agent

Why this commit exists." KIMI_SESSION_ID="$kimi_id" VC_SESSION_PID=0)"
expect_absent codex-does-not-record-kimi "$out" "session_id:"

out="$(gen "$subject_agent

Why this commit exists." GEMINI_SESSION_ID="$gemini_id" VC_SESSION_PID=0)"
expect_absent codex-does-not-record-gemini "$out" "session_id:"

out="$(gen "$subject_agent

Why this commit exists." JUNIE_SESSION_ID="$junie_id" VC_SESSION_PID=0)"
expect_absent codex-does-not-record-junie "$out" "session_id:"

out="$(gen "$subject_claude

Why this commit exists." CLAUDE_CODE_SESSION_ID="$claude_id" CLAUDE_PID=111 CODEX_PID=222 VC_SESSION_PID=1)"
expect_line claude-pid-matches-subject "$out" "session_pid: 111"
expect_absent claude-pid-ignores-codex "$out" "session_pid: 222"

# An authored comment kept by git commit -F stays above the footer.
out="$(gen "$subject_agent

Why this commit exists.

# authored note" CODEX_SESSION_ID="$real_session" VC_SESSION_PID=0)"
if ! printf '%s\n' "$out" | awk '
    /^# authored note$/ { saw=1 }
    /^session_id: / { if (saw) found=1; else below=1 }
    END { exit !(found && !below) }
'; then
    printf 'FAIL generator: authored comment landed below the footer\n' >&2
    printf '%s\n' "$out" >&2
    exit 1
fi
printf 'ok generator: authored comment stays above the footer\n'

reject prose-scissors-hides-nothing "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Why this commit exists.

NOTE ------------------------ >8 ------------------------
Co-Authored-By: Vendor <bot@openai.com>

Authored-By: codex <agents@vetcoders.io>
session_id: 019e93be-379d-7303-9ad4-ffae468db99f
time: 2026-06-04T14:08:27-06:00
runtime: iterm2
EOF
)" "vendor footers"

reject codex-rejects-junie-shaped-id "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Why this commit exists.

Authored-By: codex <agents@vetcoders.io>
session_id: 260408-214715-abcd
time: 2026-06-04T14:08:27-06:00
runtime: iterm2
EOF
)" "session_id"

reject body-session-is-not-a-trailer "$(
    cat <<'EOF'
[codex/interactive] chore: describe the change

Why this commit exists.
session_id: 00000000-0000-4000-8000-deadbeef0000
is a citation, not the footer.

Authored-By: codex <agents@vetcoders.io>
time: 2026-06-04T14:08:27-06:00
runtime: iterm2
EOF
)" "session_id"

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

# pre-push is intentionally not byte-identical: the embargo copy also owns
# the compile-embargo gate. Both copies must map a develop symref to
# origin/main and must not merge-base against origin/develop directly.
for copy in "$root/tools/githooks/pre-push" "$root/tools/git-hooks/pre-push"; do
    if grep -q 'merge-base "$lsha" origin/develop' "$copy"; then
        printf 'FAIL pre-push: %s still merge-bases against origin/develop\n' "$copy" >&2
        exit 1
    fi
    if ! grep -q 'origin/develop) default_ref=origin/main' "$copy"; then
        printf 'FAIL pre-push: %s does not retarget a develop symref to origin/main\n' "$copy" >&2
        exit 1
    fi
    if grep -q 'git show --name-only --format=' "$copy"; then
        printf 'FAIL pre-push: %s still classifies a missing baseline from the tip commit\n' "$copy" >&2
        exit 1
    fi
    if ! grep -q 'refs/remotes/${remote}/HEAD' "$copy"; then
        printf 'FAIL pre-push: %s does not base a first push on the destination remote\n' "$copy" >&2
        exit 1
    fi
done
printf 'ok pre-push: default branch is not origin/develop\n'

# A prose token in front of >8 must not be treated as Git's scissors marker.
for name in commit-msg prepare-commit-msg; do
    for dir in githooks git-hooks; do
        if grep -F -q '[^[:space:]]+[[:space:]]*-{2,}' "$root/tools/$dir/$name"; then
            printf 'FAIL scissors: %s/%s still accepts any word as the cut prefix\n' "$dir" "$name" >&2
            exit 1
        fi
    done
done
printf 'ok scissors: cut prefix is not an arbitrary word\n'

printf 'commit provenance selftest: all passed\n'
