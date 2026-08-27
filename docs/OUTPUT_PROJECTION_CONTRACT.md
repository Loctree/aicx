# Output projection contract

Status: W1 structure. Wiring lands in W2-T13. This file is the flag grammar
and the default rendering; the Rust type is `src/extraction/projection.rs`.

The throne (W1-T4) keeps the full substrate: every classified frame, every
seal, every `ShellAction` result as `Retained { text, chars, hash }`.
Projection is a view. It is not a second reducer.

## Invariant: flags never mutate the substrate

No extract flag, search flag, MCP argument, or intents option may rewrite,
drop, hash-away, or restamp a stored frame. Filters populate
`ProjectionSpec`. The same session under `--user-only` and under
`--result full` is one substrate, two views.

`ConversationProjection` in `src/extraction/conversation.rs` is a denoised
transcript product (2 s short-user dedupe, harness-noise drop). It is not
this spec. W2-T13 must stop re-deciding projection in
`conversation.rs` / `mcp_session.rs` / `intents.rs` and read `ProjectionSpec`
instead.

Store-side types (`legacy_archive/canonical_projection.rs`,
`crates/aicx-parser/src/projections`) are a different layer. They are not
CLI output projection.

## Defaults

No flags means the razor view — complete, not cropped:

| Axis | Default | Why |
|---|---|---|
| Dialogue | Human (Direct channel) + `AssistantFinal` | Operator speech and the agent's last answers, in full. |
| Delayed human speech | Hidden until `--dialog` | Echo-bus / `queue-operation` are human, but they are a channel; `--dialog` shows them as speech with their seals. |
| `InterAgent` | Hidden until `--kind inter_agent` | Never rendered as `assistant` (Decision 9). |
| `Inject` / `LineageMeta` | Hidden | Noise and parent pointers; lineage is opt-in. |
| Shell results | Stub: `$ cmd [N lines, sha256:…]` | Count + command + hash are the facts an agent can act on. The body stays in the substrate. |
| `-H` / `--since` / `--until` / `-p` / `--score` | Unbounded / empty / none | Absence of a narrowing flag is not a silent 30-day window. |
| `--max-message-chars` | `0` (no truncation of dialogue) | The 800-line bomb is the **result body**, not the human turn. |

Default is sharp and complete: cardinality, seals, command markers, result
hash. It is not "write a file and hope the agent opens it."

## What turns fullness on

| Want | Flag (W2-T13) | `ProjectionSpec` |
|---|---|---|
| Shell result body | `--result full` | `result = Full` |
| First N lines of a result | `--result head=N` | `result = Head(N)` |
| Echo-bus / queued speech | `--dialog` | `dialog = true` (kind `echo_seal` also works) |
| Inter-agent lane | `--kind inter_agent` | `kinds` includes `InterAgent` |
| Parent sessions | `--lineage` / `--lineage=N` | `lineage_depth = Some(UNBOUNDED)` / `Some(N)` |
| Every kind, full bodies | combine the above, or construct `ProjectionSpec::full()` | not a CLI alias in W1 |

`--result none` is the default stub, not "omit the command." The command
line, line count, and hash still emit.

## Flag → field (`extract`)

Today's installed grammar (`aicx extract <agent> …`, 0.12.5) plus the W2
flags this spec is built for. Hidden extract flags already exist in
`src/main.rs` (`-H`, `--user-only`, `--max-message-chars`); they are not
wired to this type until W2-T13.

| Flag | `ProjectionSpec` field | Notes |
|---|---|---|
| `--user-only` | `roles = [Human]` | Today also applied in `mcp_session.rs` by string role. That copy goes away in W2-T13. |
| `--conversation` | `kinds` razor (Human + AssistantFinal + ShellAction stubs) | Today's conversation-first path. Not a second taxonomy. |
| `--max-message-chars N` | `max_message_chars` | `0` = unlimited dialogue. Does not change `result`. |
| `-p` / `--project` | `project` | Identity filter on the view, not on the parse. |
| `-H` / `--hours` | `window.hours` | Present but hidden on `Commands::Extract`. |
| `--since` / `--until` | `window.since` / `window.until` | Not on extract today; same window type as search. |
| `--kind <token>` | `kinds` | **W2.** Tokens: `human`, `echo_seal`, `shell_action`, `inject`, `assistant_final`, `lineage_meta`, `inter_agent`. |
| `--dialog` | `dialog` | **W2.** Delayed human speech as speech, with channel/seal. |
| `--lineage[=N]` | `lineage_depth` | **W2.** `Some(usize::MAX)` when the flag is bare. |
| `--result none\|head=N\|full` | `result` | **W2.** See shell examples below. |
| `--session` / `--file` / `-o` | (not projection) | Source selection and sink. They pick *which* substrate, not *how* it renders. |

## Flag → field (`search`)

Today: `aicx search -p -H -d --limit --sort --score --agent --since --until
--frame-kind --kind --session --literal --context --no-semantic --evidence
-j --deep`.

| Flag | `ProjectionSpec` field | Notes |
|---|---|---|
| `-p` / `--project` | `project` | Union of exact slugs; not a substring. |
| `-H` / `--hours` | `window.hours` | CLI `0` means all time → spec `None` (unbounded). |
| `--since` / `--until` | `window.since` / `window.until` | Shared `RetrievalFilters`. |
| `-d` / `--date` | `window` | Search-only date/range sugar over the same window. |
| `--score` | `score` | `0–100`. Floor on the view of hits, not a re-index. |
| `--frame-kind` | `kinds` via `ProjectionKind::from_legacy_frame_kind` | `user_msg`→Human, `agent_reply`→AssistantFinal, `internal_thought`→Inject, `tool_call`→ShellAction. |
| `--kind` | **collision** | Today this is a **document class** (`conversations`/`plans`/`reports`/`other`), not a throne kind. W2-T13 must not silently overload it; new throne filters belong on `--frame-kind` or a dedicated `--kind` on `extract`. `ProjectionKind::from_cli_token("conversations")` returns `None` on purpose. |
| `--dialog` / `--result` / `--lineage` | same fields as extract | **W2** on search hit rendering. |
| `--limit` / `--sort` / `--agent` / `--session` / `--literal` / `--context` / `--no-semantic` / `--evidence` / `-j` / `--deep` | (not `ProjectionSpec`) | Retrieval / ranking / emit format. They do not rewrite stored chunks. |

## Shell-action rendering

Substrate (always, regardless of flags):

```text
ShellAction {
  cmd: "cargo test --workspace --offline",
  result: Retained { text: "<412 lines of cargo output>", chars: …, hash: "c0ffee…" }
}
```

### Default (`result = None`)

```text
$ cargo test … [412 lines, sha256:…]
```

The command is visible. The line count is visible. The hash is a handle
for `--result full` or for a later fetch. An agent can act on this line.

### `--result head=2`

```text
$ cargo test --workspace --offline
running 3 tests
test extraction::projection::tests::razor_default_is_human_plus_final_plus_shell_stub ... ok
[+410 lines omitted, sha256:c0ffee…]
```

### `--result full`

```text
$ cargo test --workspace --offline
running 3 tests
test extraction::projection::tests::razor_default_is_human_plus_final_plus_shell_stub ... ok
test extraction::projection::tests::shell_stub_matches_contract_example_shape ... ok
test extraction::projection::tests::result_full_emits_command_then_body ... ok
…
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Fullness is a flag, not a second extract, not a path to a sidecar the
harness will refuse to open.

## `ProjectionSpec` fields

Canonical type: `src/extraction/projection.rs`.

| Field | Type | Razor default |
|---|---|---|
| `roles` | `Vec<ProjectionRole>` | `Human`, `Assistant` |
| `kinds` | `Vec<ProjectionKind>` | `Human`, `AssistantFinal`, `ShellAction` |
| `result` | `None \| Head(n) \| Full` | `None` |
| `max_message_chars` | `usize` | `0` (unlimited) |
| `window` | `ProjectionWindow { hours, since, until }` | all `None` |
| `project` | `Vec<String>` | empty |
| `score` | `Option<u8>` | `None` |
| `dialog` | `bool` | `false` |
| `lineage_depth` | `Option<usize>` | `None` |

Empty `roles` / `kinds` vectors mean "emit nothing on that axis." They are
not a shortcut for default. Callers use `ProjectionSpec::default()` (razor)
or `ProjectionSpec::full()`.

## Out of scope (this cut)

- Wiring `main.rs`, `mcp_session.rs`, `intents.rs`, `conversation.rs`.
- Changes to the throne.
- Compilation. `BUILD/LINT/TEST` for this wave is embargoed.
