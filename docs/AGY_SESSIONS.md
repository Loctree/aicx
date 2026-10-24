# Antigravity CLI sessions

AICX reads native Google Antigravity CLI conversations, and the Antigravity IDE
store when it uses the same SQLite schema, as agent `agy`. Accepted aliases are
`antigravity` and `antigravity-cli`. The legacy `.pb` export and the
`gemini-antigravity` alias stay on the Gemini adapter. This page does not
change that path.

## Source layout

```text
~/.gemini/antigravity-cli/conversations/<cascade-uuid>.db
~/.gemini/antigravity/conversations/<cascade-uuid>.db
```

Each database is one conversation. The file stem is the catalog session id.
Sibling `.db-wal` and `.db-shm` files belong to that database. `.pb` files in
those directories, and files under `~/.antigravity/conversations/`, are the
older export. A database whose stem is not a UUID is not a session.
`history.jsonl` is a prompt log used only as a cross-check. It is not a second
source of truth.

AICX opens each database read-only (`mode=ro`, not `immutable`, so an
uncheckpointed WAL stays visible), sets `PRAGMA query_only=ON`, and uses a
short busy timeout. It does not write, checkpoint, or take a lock that blocks
a running `agy`. The parser crate never opens SQLite. The application
materializes one JSONL stream — a header row from `trajectory_metadata_blob`
and one row per `steps` record, sorted by `idx` — and the source digest is the
digest of that stream.

Materialization fails closed above 100,000 steps, 16 MiB for a single payload,
or 64 MiB of cumulative payload bytes (including metadata). These limits are
checked before copying SQLite blobs into the stream. Catalog previews apply
the same byte limits to metadata and the speech rows they inspect.

## What becomes speech

`step_payload` is protobuf without a published schema. AICX walks the wire
format with a depth and size limit and fails closed on truncated or illegal
input. A length-delimited value that is both UTF-8 and a complete message
stays a message when it wraps nested fields. Short tool results (under 128
bytes) are that shape: field `140.2.1` remains addressable. A leaf that is
only prose stays text.

| Step | Treatment |
|---|---|
| 14 `USER_INPUT` | Human speech is the text at field `19.3.1`, unless the text is a Vibecrafted worker contract. Those contracts are injected context and stay out of human intent. |
| 15 `PLANNER_RESPONSE` | Assistant prose is field `20.1` when it is non-empty, otherwise field `20.3`. An empty planner step is coverage with no turn. |
| 132 tool call | Call id, tool name, and JSON arguments. Rendered output is field `140.2.1` when that field is text. Otherwise the result bytes stay opaque: the coverage warning records their length and the bytes are not copied into a turn. |
| 23 and 101 | Bookkeeping. Consumed with no usage event. Field numbers inside type 23 do not match a public token schema, so AICX does not name them. |
| Any other `step_type` | Consumed as `unknown_step:<n>` with an `UnknownPayloadType` warning. The step is not dropped and is not promoted to speech. |
| `step_format` other than 0 | Visible gap. The payload is not decoded. |
| A JSONL row that is not `header` or `step` | Visible gap (`UnknownPayloadType`). |

A missing header warns and the steps are still classified. The first header
wins. A later header is a duplicate, not a second conversation.

## Project mapping

The header blob's field 7 is the workspace URI. A `file://` URI with a leading
`/` path is the session cwd. Field 6 is the cascade id and matches the database
file name. Field 18 is the project label when the provider wrote one. Field 3
is the trajectory id when present. AICX does not invent a parent session from
`parent_references`.

## Residual gaps

Steps 5, 7, 8, 9, 17, 21, 33, and 38 appear in live stores. Several of them
carry tool-shaped fields. This cut projects tool calls only for step 132, which
is the type measured as a tool call. The others stay covered and unprojected.
Type 17 can carry strings under field 24. Type 23 can carry large strings under
field 30. Neither becomes speech or a usage counter.

Opening with `mode=ro` can update the mtime of `.db-shm` while leaving the
`.db` and `.db-wal` mtimes and the shm content hash unchanged. Catalog
freshness uses the database and WAL mtimes, not the shm mtime.

## Commands

```bash
aicx sessions list --agent agy
aicx extract agy --session <cascade-id> --conversation
aicx search 'past decision' --agent antigravity
aicx intents --agent antigravity-cli --hours 168 --emit json
aicx catalog resolve <cascade-id>
```

`aicx read` opens one legacy archive chunk by path or `chunk:<id>`. A cascade
id is not that reference, so it does not open the live SQLite session.
