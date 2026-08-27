# human_shape_01a038ec

Bounded projection of Grok session `01a038ec-d5f4-7c53-b665-56fa69882572`.

## Source (not in repo)

`/Users/polyversai/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/grok/01a038ec/`

Raw `chat_history.jsonl` is 1.0 MB (206 lines: 5 user, 1 system, 36 assistant, 36 reasoning, 128 tool_result). This projection keeps `type in {user, system}` (6 lines, 87625 bytes) plus `summary.json`. UserMsg heading count on the projection matches the full source.

## Derivation (deterministic)

```bash
SRC=/Users/polyversai/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/grok/01a038ec
DST=tests/fixtures/parser_engine/grok/human_shape_01a038ec
mkdir -p "$DST"
cp -p "$SRC/summary.json" "$DST/summary.json"
python3 -c '
import json, sys
src, dst = sys.argv[1], sys.argv[2]
with open(src) as f, open(dst, "w") as g:
    for line in f:
        if json.loads(line).get("type") in ("user", "system"):
            g.write(line)
' "$SRC/chat_history.jsonl" "$DST/chat_history.jsonl"
```

## Extract (baseline aicx@a687754)

```bash
aicx extract grok --file tests/fixtures/parser_engine/grok/human_shape_01a038ec/chat_history.jsonl --conversation --user-only -o /tmp/01a038ec-user.md
```

Today: 5 raw `type=user` records, 2 rendered `user` headings, `Wrote 5 entries`.
