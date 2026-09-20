# human_shape_67025fed

Bounded projection of Claude session `67025fed-6f58-4077-8472-f41a099dd498`.

## Source (not in repo)

`~/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/claude/67025fed-6f58-4077-8472-f41a099dd498.jsonl`

Raw size 1.2 MB. This file keeps every `type == "queue-operation"` record (52 lines, 13797 bytes) and drops attachment/user/assistant/system traffic. Mid-turn enqueue/dequeue/remove and their timestamps (seals) are intact. Production `aicx extract claude --file … --conversation --user-only` on this projection matches the full source: 25 `user` headings.

## Derivation (deterministic)

```bash
SRC=~/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/claude/67025fed-6f58-4077-8472-f41a099dd498.jsonl
DST=tests/fixtures/parser_engine/claude/human_shape_67025fed.jsonl
python3 -c '
import json, sys
src, dst = sys.argv[1], sys.argv[2]
with open(src) as f, open(dst, "w") as g:
    for line in f:
        if json.loads(line).get("type") == "queue-operation":
            g.write(line)
' "$SRC" "$DST"
```

Raw operation mix on this projection: enqueue=26, dequeue=6, remove=20.

## Extract (baseline aicx@a687754)

```bash
aicx extract claude --file tests/fixtures/parser_engine/claude/human_shape_67025fed.jsonl --conversation --user-only -o /tmp/67025fed-user.md
```
