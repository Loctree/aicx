# GitHub Copilot CLI sessions

AICX reads local GitHub Copilot CLI sessions through the same source-first
pipeline as other providers. The canonical agent name is `copilot`; accepted
aliases are `copilot-cli`, `github-copilot`, and `github-copilot-cli`. The provider identifies the
session runtime, independently of the model Copilot selected.

## Source layout and identity

The default source root is `~/.copilot/session-state`. When `COPILOT_HOME` is
set, AICX follows `<COPILOT_HOME>/session-state`:

```text
~/.copilot/session-state/<session-id>/
    events.jsonl       required, canonical event content
    workspace.yaml     optional session metadata
```

Only session-level `events.jsonl` files are discovered. Global
`~/.copilot/events.jsonl`, checkpoint copies, and arbitrary nested tool logs
are excluded. A session ID can be a UUID or a native named SDK ID such as
`user-123-task-456`. The session directory supplies stable source identity; a
declared session ID in the event header supplies logical identity.

`session.start` metadata supplies cwd, repository context, branch, start time,
and model when present. `workspace.yaml` can fill missing cwd/repository,
title, and dates. Missing or malformed optional metadata does not suppress an
otherwise discoverable event file. Fingerprints account for both files, so a
metadata edit or event append invalidates derived caches without creating a
second session. No provider credentials or Copilot executable are needed to
read existing logs.

## Commands

```bash
aicx catalog rebuild
aicx sessions list --agent copilot --all --json
aicx sessions show <session-id> --json
aicx extract copilot --session <session-id> --conversation
aicx extract copilot --session <session-id> --conversation --user-only
aicx extract copilot --session <session-id> --agent-commands --result full
aicx extract copilot --session <session-id> --brief
aicx extract all --provider copilot --conversation
aicx conversations --agent copilot --hours 0 --out-dir ./conversations
aicx intents --agent copilot --hours 0 --emit json
aicx index --cache-extracts
aicx search 'past decision' --agent copilot --hours 0 --json
```

`extract all` includes Copilot by default. The `conversations --agent` batch
accepts every registered provider. Conversation JSON exports preserve
session identity and message provenance. Intent extraction reads human
requests and decisions; assistant claims and shell output do not become human
intent. Briefs retain evidence locators for handoff signals and observed
shell gates. A tool transport success is insufficient to prove a shell gate
passed: an explicit exit verdict or recognized test output is required.

The lexical index includes conversational signal and filters tool, internal,
and lifecycle noise. Appending to a session updates that session's content on
the next incremental index/extract pass, including events with equal
timestamps. `--cache-extracts` is optional; sources remain the content owner.

## Event coverage

Events use the Copilot envelope `{type, id, timestamp, parentId, data}`.
AICX preserves source event identity, timestamps, model provenance when
reported, and tool correlation IDs.

| Event category | Treatment |
|---|---|
| `session.start` and supported session metadata | Session identity, context and provenance |
| `user.message` | Human conversation and intent evidence |
| `assistant.message` | Assistant conversation; declared tool requests correlate with execution events |
| `tool.execution_start`, `tool.execution_complete` | Tool calls/results; shell commands can appear in command projections and briefs |
| `assistant.reasoning`, inline reasoning text/blocks | Internal thought retained separately from conversation and human intent |
| `assistant.usage`, `session.shutdown` model metrics | Reported token components with delta or cumulative semantics; unreported usage and currency remain unknown |
| Successful `session.compaction_complete` | Context epoch boundary and summary evidence; summary is not new speech |
| Known lifecycle and ephemeral bookkeeping | Explicit raw-unit accounting, outside conversational signal |
| Unknown event types | Explicit unsupported coverage; visible completeness becomes partial |
| Malformed or incomplete records | Parser diagnostics and incomplete coverage rather than silent loss |

Streaming deltas and persisted complete messages are distinct event classes;
matching deltas are accounted for as duplicate bodies when the durable message
exists. Orphan deltas expose partial coverage rather than inventing a final
message. New provider event types are
coverage boundaries until an adapter supports them. A partial parse is not a
claim that every event was understood.

The upstream format references are GitHub's
[streaming event guide](https://docs.github.com/en/copilot/how-tos/copilot-sdk/features/streaming-events)
and [generated session event types](https://github.com/github/copilot-sdk/blob/main/nodejs/src/generated/session-events.ts).
Those references describe the provider; AICX's parser diagnostics describe
the coverage of the source being read.

## Configuration and direct files

`COPILOT_HOME` selects the Copilot data directory, shared with Copilot CLI.
Set it to the directory containing `session-state`, rather than to an
individual session directory. The same override applies to discovery,
catalog, extraction, indexing/search, intents, and MCP source access:

```bash
export COPILOT_HOME=/absolute/path/copilot-data
aicx catalog rebuild
aicx index
aicx search 'past decision' --agent copilot
```

GitHub's [CLI command reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference)
documents `COPILOT_HOME`; Copilot's older `--config-dir` flag is deprecated.
With no override, AICX uses the OS user's `~/.copilot` directory. Inspect
registered roots with `aicx sources`.

`AICX_HOME` or `[storage].home` in `config.toml` relocates AICX's catalog,
extracts, and index independently. It does not relocate Copilot's source files.

For an explicit event file, use direct extraction:

```bash
aicx extract copilot --file /absolute/path/events.jsonl \
  --conversation --output ./copilot-conversation.md
```

Direct-file mode does not scan the global catalog or register a new default
root. In-process callers can give `SessionCatalog::new(AgentKind::Copilot,
root)` an explicit session-state root. Source readers enforce canonical path
containment and reject symlink escapes; copying only a catalog does not make
its absolute source paths readable on another machine.

## MCP and installation

`aicx_sessions` accepts `agent: "copilot"`; `aicx_session` accepts that agent
and the session ID. `aicx_search` uses the same agent filter, and `aicx_read`
opens a returned readable extract reference. `aicx_intents` and continuity
use the shared intent pipeline. Normal MCP service remains a reader; catalog
and index refresh have their own process ownership.

Copilot support ships inside the regular prebuilt AICX binaries. End-user
installation uses the [prebuilt distribution channels](install-paths.md);
it requires no Rust toolchain or build from source.
