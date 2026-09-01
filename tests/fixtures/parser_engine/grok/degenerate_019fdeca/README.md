# degenerate_019fdeca

Grok session `019fdeca-a82d-7e62-b073-fdbf5c32aee9` copied whole except lock files.

## Source (not required in repo; copy is complete)

`/Users/polyversai/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/grok/019fdeca/`

`chat_history.jsonl` is two lines (system + one synthetic user skills dump, 62252 bytes). That is the degenerate 2-line shape.

## Derivation (deterministic)

```bash
SRC=/Users/polyversai/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/grok/019fdeca
DST=tests/fixtures/parser_engine/grok/degenerate_019fdeca
mkdir -p "$DST"
find "$SRC" -maxdepth 1 -type f ! -name '*.lock' -exec cp -p {} "$DST"/ \;
```

## Extract (baseline aicx@a687754)

```bash
aicx extract grok --file tests/fixtures/parser_engine/grok/degenerate_019fdeca/chat_history.jsonl --conversation -o /tmp/019fdeca.md
```

Today: exit 0, `Messages | 0`, empty body. The W2-T12 contract is a typed `RefusalReason`, not this silent empty success.
