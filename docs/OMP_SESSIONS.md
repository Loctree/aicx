# oh-my-pi and pi sessions

AICX reads local oh-my-pi (`omp`) sessions, and upstream `pi` sessions in the
same layout, through the same source-first pipeline as other providers. The
canonical agent name is `omp`. Accepted aliases are `pi` and `oh-my-pi`. The
provider identifies the session runtime, independently of the model the agent
selected.

## Source layout and identity

Both stores are scanned. A session that exists only under `~/.pi` is still
agent `omp`:

```text
~/.omp/agent/sessions/<encoded-cwd>/<ISO-timestamp>_<uuidv7>.jsonl
~/.pi/agent/sessions/<encoded-cwd>/<ISO-timestamp>_<uuidv7>.jsonl
```

The file stem's UUID is the source id. The `session` record (`version: 3`)
carries the same id plus `cwd`, which maps the session to a project. There is
no `OMP_HOME` override: discovery uses the OS user home.

Sibling directories named `<ISO-timestamp>_<uuid>/` hold tool logs
(`1.bash.log`, `17.async.log`, and similar). Those directories are artifacts,
not sessions, and AICX skips them the same way it skips Cursor `agent-tools/`.
A JSONL dropped directly under `sessions/`, or nested inside a tool-log
directory, is not a session.

No provider credentials or `omp`/`pi` executable are required to read existing
logs. Credential pin hashes are consumed as coverage and are not copied into
turns.

## Active branch

Records other than the `title` and `session` headers form a tree through
`parentId`. Upstream `SessionIndex` treats the last inserted entry as the
leaf. AICX walks `parentId` from that leaf back to the root, then emits turns
in root-to-leaf order. A cycle stops the walk. When the same id appears twice,
the later line wins.

Entries that are not on that path stay in coverage as `off_branch:<type>`.
They do not become speech, search documents, or human intent. Branching is
therefore visible in coverage and absent from the linear transcript.

## Human speech and injections

A `message` with `role: "user"` is human speech only when omp would treat it
as typed by a person:

- `attribution` is missing or `"user"`
- `synthetic` is not true
- `steering` and `liveSteered` are not true
- the text does not start with a Vibecrafted worker contract (`You are running under Vibecrafted core runtime.`) or `Read and follow the private task file: `

Everything else on a user-shaped record is an injection (`agent_authored`,
`synthetic_user`, `steering`, `dispatch_brief`, or `user_unattributed`) and
stays out of human intent. `custom_message` records, including harness cards
such as `fleet-loct-context-card`, are always injections. `developer` and
`bashExecution` are agent or harness lanes, not a person's turn.

Assistant `text` is the reply. `thinking` stays internal. `toolCall` and
`toolResult` keep their correlation id. A shell exit code in the result is
the gate verdict; transport success alone does not make a gate pass.

## Record coverage

Each JSONL line is one record. The `type` field selects the record. Unknown
types are counted as unsupported and make visible completeness partial. A
known type is never dropped without a coverage unit.

| Record | Treatment |
|---|---|
| `title`, `session` | Identity, title, cwd, parent session when declared |
| `message` `user` | Human speech, or an injection under the rules above |
| `message` `assistant` | Reply, thinking, and tool calls |
| `message` `toolResult` | Tool result correlated to the call |
| `message` `bashExecution` | Shell call plus exit verdict, not a second copy of `custom.tool_execution_start` |
| `custom_message` | Harness injection, with coverage |
| `custom`, `model_change`, `model_usage`, `credential_pin`, `title_change`, and the other bookkeeping types | Explicit coverage, not conversational speech |
| Unknown `type` | Visible gap (`UnknownPayloadType`), not a silent skip |
| Malformed JSON | Parser diagnostic and incomplete coverage |

`custom.tool_execution_start` is bookkeeping. The tool call already present on
the assistant message is the tool event, so the custom record does not emit
a second call.

## Commands

```bash
aicx catalog rebuild
aicx sessions list --agent omp --all --json
aicx sessions list --agent pi --all --json
aicx sessions show <session-id> --json
aicx extract omp --session <session-id> --conversation
aicx extract pi --session <session-id> --conversation --user-only
aicx extract oh-my-pi --session <session-id> --agent-commands --result full
aicx extract omp --session <session-id> --brief
aicx extract all --provider omp --conversation
aicx conversations --agent omp --hours 0 --out-dir ./conversations
aicx intents --agent omp --hours 0 --emit json
aicx index
aicx search 'past decision' --agent omp --hours 0 --json
aicx search 'past decision' --agent pi --hours 0 --json
```

`extract all` includes omp by default. `--agent omp`, `--agent pi`, and
`--agent oh-my-pi` select the same provider everywhere the Copilot agent
filter does. Intent extraction reads human requests. Assistant claims, shell
output, and harness injections do not become human intent.

Append-only growth of a session file is re-read on the next index or extract.
omp and pi scan the full window on `aicx all` because a later line can share
the timestamp of an earlier one; content dedup, not the wall-clock watermark,
decides what is new. A watermark recorded before omp joined does not hide
omp history, and it is not moved backwards.

## Direct files

```bash
aicx extract omp --file /absolute/path/session.jsonl \
  --conversation --output ./omp-conversation.md
```

Direct-file mode does not scan the global catalog or register a new default
root. In-process callers can give `SessionCatalog::new(AgentKind::Omp, root)`
either store. Source readers enforce canonical path containment and reject
symlink escapes.

## MCP and installation

`aicx_sessions` accepts `agent: "omp"` and the aliases `pi` and `oh-my-pi`.
`aicx_session` accepts that agent and the session id. `aicx_search` uses the
same filter, and `aicx_read` opens a returned readable extract reference.
`aicx_intents` uses the shared intent pipeline.

omp support ships inside the regular prebuilt AICX binaries. End-user
installation uses the [prebuilt distribution channels](install-paths.md);
it requires no Rust toolchain or build from source.
