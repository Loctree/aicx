# human_shape_004ffd2e

Bounded, sanitized projection of a Cursor CLI agent transcript
(first cursor-agent session on the capture host, 2026-08-29).

## Source (not in repo)

`~/.cursor/projects/<project-slug>/agent-transcripts/<session-id>/<session-id>.jsonl`
(130 lines at capture time). Command payloads and assistant prose were replaced
with neutral equivalents; the structural shape of each line is preserved
verbatim from the source session.

## Shape kept, in source order

1. plain user text (no harness wrappers)
2. assistant `tool_use` (`Shell`)
3. user text with `<timestamp>` + `<user_query>` wrappers — the wrapper
   stripping and timestamp parsing lanes
4. assistant `text` + `tool_use` in one record (multi-block line)
5. assistant `text`-only record

## Derivation

Lines 1, 2, 5, 6 and one later assistant record of the source transcript,
projected in source order, then content-sanitized (structure untouched:
same block types, same wrappers, same timestamp text).

## Assertions of record

Live in `crates/aicx-parser/tests/cursor_adapter.rs`
(`cursor_human_shape_004ffd2e_*`): wrapper-free operator text, parsed RFC 3339
turn timestamp, `Shell` tool events, full visible coverage with zero skips.
