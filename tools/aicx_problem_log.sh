#!/usr/bin/env bash
# Append an AICX problem entry to the operator-managed problem log.

set -euo pipefail

BACKLOG_PATH="${AICX_PROBLEM_LOG:-${HOME}/AI_notes/projects/aicx/aicx-problems.md}"
CREATED_DATE="2026-06-28"

usage() {
  cat <<'USAGE'
Usage:
  tools/aicx_problem_log.sh --init
  tools/aicx_problem_log.sh --path
  tools/aicx_problem_log.sh --lock
  tools/aicx_problem_log.sh --unlock
  tools/aicx_problem_log.sh "short title" <<'EOF'
  - Source: ...
  - Symptom: ...
  - Evidence: ...
  - Impact: ...
  - Next: ...
  EOF

Environment:
  AICX_PROBLEM_LOG=/custom/path.md  Override the canonical log path.
USAGE
}

ensure_backlog_file() {
  mkdir -p "$(dirname "${BACKLOG_PATH}")"
  if [[ -s "${BACKLOG_PATH}" ]]; then
    return 0
  fi

  cat > "${BACKLOG_PATH}" <<EOF
---
title: "AICX problem log"
maintainer: operator-managed (append-only)
created: ${CREATED_DATE}
status: appending
---

# AICX problem log

Kanoniczny, prywatny log problemów zauważonych podczas pracy z \`aicx\`.

Problem = bug, regression risk, flaky behavior, contract drift,
docs/runtime mismatch, tooling failure, test gap, zombie path, unsafe fallback,
albo decyzja robocza która może wrócić jako product debt.

## Zasady appendowania

- Pisz na końcu pliku. Nie nadpisuj, nie reorganizuj.
- Powielony problem = sygnał, nie błąd.
- Nie zapisuj sekretów, tokenów, PII, PHI ani pełnych prywatnych payloadów.
- Loctree failures zapisuj w \`.loctree/loctree-fail.md\`; tutaj zapisuj
  problemy AICX product/runtime/tooling.
- Wpis ma zawierać minimum: objaw, evidence, impact i proponowany next step.

## Template

\`\`\`
### YYYY-MM-DD HH:MM UTC — krótki tytuł

- **Source:** agent / operator / test / runtime
- **Objaw:** co poszło nie tak
- **Evidence:** komenda, plik, linia, artifact albo bezpieczna reprodukcja
- **Impact:** dlaczego to ma znaczenie
- **Next:** najmniejszy sensowny kolejny krok

---
\`\`\`

## Wpisy — append below

EOF
}

lock_backlog_file() {
  ensure_backlog_file
  if ! command -v chflags >/dev/null 2>&1; then
    echo "chflags unavailable; lock skipped" >&2
    return 0
  fi
  chflags uappnd "${BACKLOG_PATH}"
  echo "locked append-only: ${BACKLOG_PATH}" >&2
}

unlock_backlog_file() {
  if [[ ! -e "${BACKLOG_PATH}" ]]; then
    echo "log missing: ${BACKLOG_PATH}" >&2
    return 0
  fi
  if ! command -v chflags >/dev/null 2>&1; then
    echo "chflags unavailable; unlock skipped" >&2
    return 0
  fi
  chflags nouappnd "${BACKLOG_PATH}"
  echo "unlocked: ${BACKLOG_PATH}" >&2
}

append_entry() {
  local title="$1"
  local timestamp
  local details
  timestamp="$(date -u '+%Y-%m-%d %H:%M UTC')"
  details="$(cat || true)"
  ensure_backlog_file

  {
    printf "### %s — %s\n\n" "${timestamp}" "${title}"
    if [[ -z "${details}" ]]; then
      cat <<'EOF'
- **Source:** agent
- **Objaw:** TODO
- **Evidence:** TODO
- **Impact:** TODO
- **Next:** TODO

EOF
    else
      printf "%s\n\n" "${details}"
    fi
    printf -- "---\n\n"
  } >> "${BACKLOG_PATH}"

  echo "appended: ${BACKLOG_PATH}" >&2
}

main() {
  case "${1:-}" in
    -h|--help)
      usage
      ;;
    --init)
      ensure_backlog_file
      echo "ready: ${BACKLOG_PATH}" >&2
      ;;
    --path)
      echo "${BACKLOG_PATH}"
      ;;
    --lock)
      lock_backlog_file
      ;;
    --unlock)
      unlock_backlog_file
      ;;
    "")
      usage >&2
      return 2
      ;;
    *)
      append_entry "$1"
      ;;
  esac
}

main "$@"
