# human_shape_004ffd2e

Bounded projection of Cursor CLI agent transcript `004ffd2e-8b2a-4a41-bd7d-dee9f7df9950`
(first cursor-agent session on this host, 2026-08-29).

## Source (not in repo)

`~/.cursor/projects/Users-polyversai-vibecrafted/agent-transcripts/004ffd2e-8b2a-4a41-bd7d-dee9f7df9950/004ffd2e-8b2a-4a41-bd7d-dee9f7df9950.jsonl`
(130 lines at capture time).

## Shape kept, in source order

1. plain user text (no harness wrappers)
2. assistant `tool_use` (`Shell`)
3. user text with `<timestamp>` + `<user_query>` wrappers — the wrapper
   stripping and timestamp parsing lanes
4. assistant `text` + `tool_use` in one record (multi-block line)
5. `{"type":"turn_ended","status":"success"}` control row

## Derivation (deterministic)

```bash
SRC=~/.cursor/projects/Users-polyversai-vibecrafted/agent-transcripts/004ffd2e-8b2a-4a41-bd7d-dee9f7df9950/004ffd2e-8b2a-4a41-bd7d-dee9f7df9950.jsonl
DST=tests/fixtures/parser_engine/cursor/human_shape_004ffd2e.jsonl
{ sed -n '1p;2p;5p;6p' "$SRC"; grep -m1 "turn_ended" "$SRC"; } > "$DST"
```

## Assertions of record

Live in `crates/aicx-parser/tests/cursor_adapter.rs`
(`cursor_human_shape_004ffd2e_*`): wrapper-free operator text, parsed RFC 3339
turn timestamp, `Shell` tool events, `turn_ended` consumed with zero skips.
