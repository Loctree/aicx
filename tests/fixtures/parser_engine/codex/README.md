# Codex parser fixtures

## `human_shape_01a0369f.jsonl`

Golden for `codex_frozen_human_shape_has_25_human_plus_9_echo_seals`
(`crates/aicx-parser/tests/codex_adapter.rs`): 38 lines, 38 670 B —
1 `session_meta` + 37 `response_item/message/role=user`. Expected shape on
`codex-rollout-v3`: 34 `UserMsg` (25 human prose + 9 echo-seals), the
remaining envelopes are shell actions (`ToolCall` marker + verbatim
`ToolResult`).

Source: Codex rollout
`~/.codex/sessions/2026/08/25/rollout-2026-08-25T03-53-42-01a0369f-9313-7592-8303-3db46b6f8b47.jsonl`
(262 237 251 B, 79 user messages in total; mirrored under the mission
inputs at
`~/.vibecrafted/artifacts/Loctree/aicx/2026_0827/plans/aicx-one-taxonomy-fusion-260827/inputs/fixtures/codex/`).

`derivation: partial (worker died)` — the worker `work-260825-223415-07065`
that cut this fixture left no derivation command. What is verifiable
against the raw rollout (2026-08-27, W0-T1):

- **Not a byte-verbatim slice.** `grep -F -x -f fixture raw` matches 0/38
  lines; the lines are `jq`-reserialized (`ordinal`, `session_id`,
  `forked_from_id` and other raw fields dropped).
- **`session_meta` is synthetic.** Raw: `timestamp 2026-08-25T01:53:42.885Z`
  with full payload. Fixture: `2026-08-25T01:54:00.000Z`, payload reduced to
  `{id, cwd}`.
- **Timestamps are real.** All 37 message timestamps occur in the raw
  rollout. The worker's transcript names a cut-off of
  `2026-08-25T19:26:19.279Z` (56 user messages before it in the raw file;
  the fixture keeps 37 — the dropped 19 are the control envelopes /
  duplicates the worker filtered; exact filter unknown).
- **One result body is synthetic.** The `~/.scripts/rust-target-cleaner.sh
  ~/vc-workspace` envelope (`2026-08-25T03:30:41.147Z`) has its `<result>`
  replaced by a ~700-line `rust-target-cleaner output sentinel` dump that
  does not exist in the raw rollout.

Reproduction closest to the worker's shape (not byte-exact, see above):

```sh
RAW=~/.codex/sessions/2026/08/25/rollout-2026-08-25T03-53-42-01a0369f-9313-7592-8303-3db46b6f8b47.jsonl
jq -c 'select(.type=="response_item" and .payload.type=="message" and .payload.role=="user")
       | {timestamp, type, payload}' "$RAW" \
  | awk -F'"' '$4 <= "2026-08-25T19:26:19.279Z"'
```

Do not regenerate the golden from this recipe without re-checking the
34/25/9 split and the two anchored echo-seal timestamps
(`2026-08-25T04:37:55.745Z`, `2026-08-25T04:40:59.057Z`) in the test.

## Other files

- `minimal.jsonl`, `expected.json` — frozen differential oracle
  (`codex_differential_envelope_matches_frozen_oracle`).
- `dual_envelope.jsonl`, `compaction_markers.jsonl` — boundary shapes
  for the opaque/compaction tests.
