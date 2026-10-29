#!/usr/bin/env bash
# Regenerate the aicx help lens: one file per command and subcommand, in the
# `~/.agents/loct-helps` shape. First line `########## aicx <cmd> [<sub>] --help ##########`,
# then the help exactly as the binary prints it. `_root.txt` holds `aicx --help-full`.
#
#   tools/dump_helps.sh <out-dir> [aicx-binary]
#
# The binary defaults to $AICX_BIN, then `aicx` on PATH: the lens describes the
# aicx you run, so it is regenerated from that binary instead of aging beside it.
# Commands come from `--help-full` (hidden power-user surfaces included), their
# subcommands from each command's own `--help`. Every page is written to a
# staging directory first; <out-dir> changes only when all of them succeeded.
# Lens pages a previous run wrote for commands that no longer exist (same header
# shape) are removed; any other file in <out-dir> is left alone.
#
# 𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
set -euo pipefail

usage() {
  echo "usage: $0 <out-dir> [aicx-binary]" >&2
  exit 64
}
[[ $# -ge 1 && $# -le 2 ]] || usage
out=$1
bin=${2:-${AICX_BIN:-}}
if [[ -z $bin ]]; then
  bin=$(command -v aicx) || {
    echo "dump_helps: no aicx on PATH; pass the binary or set AICX_BIN" >&2
    exit 69
  }
fi
[[ -x $bin ]] || {
  echo "dump_helps: not an executable: $bin" >&2
  exit 66
}

stage=$(mktemp -d "${TMPDIR:-/tmp}/aicx-helps.XXXXXX")
trap 'rm -rf "$stage"' EXIT

# Subcommand names under clap's `Commands:` heading, `help` excluded. Entries
# sit at a two-space indent; wrapped descriptions are indented deeper.
list_commands() {
  awk '/^Commands:/ { on = 1; next }
       on && /^$/ { exit }
       on && /^  [a-z]/ && $1 != "help" { print $1 }'
}

# Write one page: <file> <label> -- <argv...>. stdout only: a warning on stderr
# must not end up in the lens.
page() {
  local file=$1 label=$2 body
  shift 3
  if ! body=$("$bin" "$@" 2>"$stage/.stderr"); then
    echo "dump_helps: \`aicx $label\` failed:" >&2
    cat "$stage/.stderr" >&2
    return 1
  fi
  printf '########## aicx %s ##########\n%s\n' "$label" "$body" >"$stage/$file"
  printf '%s\n' "$body"
}

# One command and, recursively, its subcommands (three levels at most).
walk() {
  local depth=$1 help sub file
  shift
  file=$(printf '%s-' "$@")
  file=${file%-}.txt
  help=$(page "$file" "$* --help" -- "$@" --help)
  ((depth < 3)) || return 0
  while IFS= read -r sub; do
    walk $((depth + 1)) "$@" "$sub"
  done < <(printf '%s\n' "$help" | list_commands)
}

root=$(page _root.txt "--help-full" -- --help-full)
while IFS= read -r command; do
  walk 1 "$command"
done < <(printf '%s\n' "$root" | list_commands)
rm -f "$stage/.stderr"

written=$(find "$stage" -maxdepth 1 -type f -name '*.txt' | wc -l | tr -d ' ')
if ((written < 2)); then
  echo "dump_helps: $bin listed no commands under --help-full; lens not written" >&2
  exit 70
fi

mkdir -p "$out"
removed=0
for old in "$out"/*.txt; do
  [[ -f $old && ! -e $stage/$(basename "$old") ]] || continue
  if head -n 1 "$old" | grep -Eq '^########## aicx .* ##########$'; then
    rm -f "$old"
    removed=$((removed + 1))
  fi
done
cp "$stage"/*.txt "$out"/
# Quiet when scripted: callers count the files, and a merged stderr line must
# not land in their output.
if [[ -t 2 ]]; then
  echo "dump_helps: $written pages from $("$bin" --version 2>/dev/null || echo "$bin") into $out (stale pages removed: $removed)" >&2
fi
