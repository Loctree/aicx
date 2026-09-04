# human_shape_9048328b

Whole-file copy of Gemini session `9048328b` (150034 bytes, under 200 KiB).

## Source

`~/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/gemini/session-2026-06-01T01-46-9048328b.jsonl`

## Derivation (deterministic)

```bash
SRC=~/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/gemini/session-2026-06-01T01-46-9048328b.jsonl
DST=tests/fixtures/parser_engine/gemini/human_shape_9048328b.jsonl
cp -p "$SRC" "$DST"
```

Raw mix: 1 header, 1 `type=user` (11746-byte content), 21 `type=gemini`, 12 `$set`.

## Extract (baseline aicx@a687754)

```bash
aicx extract gemini --file tests/fixtures/parser_engine/gemini/human_shape_9048328b.jsonl --conversation --user-only -o /tmp/9048328b-user.md
aicx extract gemini --file tests/fixtures/parser_engine/gemini/human_shape_9048328b.jsonl --conversation -o /tmp/9048328b.md
```

Today: `--user-only` renders 0 user headings (the raw user record is dropped). `--conversation` renders 21 assistant headings.
