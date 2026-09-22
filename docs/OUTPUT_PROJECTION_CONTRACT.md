# Output projection contract

Status: W1 structure, wired in W2-T13, class-carrying since W2-R1. This file
is the flag grammar and the default rendering; the Rust type is
`src/extraction/projection.rs`.

## Identity, class, lineage, scope (W2-R1)

The projection reads four things from the model that used to be flattened
away:

| Model field | Type | What the view does with it |
|---|---|---|
| `SessionModel::conversation` | `ProviderConversationRef` (tagged: Claude `session_id`/`agent_id`; Codex `tree_session_id`/`thread_id`/`forked_from_id`/`parent_thread_id`/`window_id`) | Names the conversation. `session_id` on the model is the store handle, a projection of this ref. Fields a rollout did not carry are `None` and listed in `unobserved`. |
| `SessionModel::snapshot` | `SourceSnapshotRef { path, content_hash, bytes, observed_at, cutoff }` | Names the bytes. Same hash = identical bytes, never "same event"; an appended turn changes it without a fork. `PackageIdentity` is this pair, not a conversation id. |
| `Turn::frame_class` → `TimelineEntry::frame_class` | `Option<FrameClass>` | `ProjectionKind::from_frame_class` decides the kind; `--dialog` reveals `EchoSeal` by class, `--kind inter_agent` selects `InterAgent` by class, `LineageMeta` is not `Inject`. The role / `frame_kind` bridge in `conversation.rs` is the fallback for class-less entries only (store chunks, importers, lanes the throne does not own). |
| `SessionModel::context_epochs` | `Vec<ContextEpochRef>` | A compaction is an epoch of the same conversation (summary provenance + replaced refs + trigger), never a second source in `--lineage`. |
| `Segment::scope_status` / `SessionModel::scope_status()` | `homogeneous \| mixed_candidate \| unknown` | Structural only (distinct cwds / branch drift). A `mixed_candidate` makes `continuity` refuse a single distilled history (`RefusalReason::MixedWorkstream`) unless `distill_mixed` is passed; `intents` records from such sessions carry a `scope_status=mixed_candidate …` evidence line. Topic-level mixing inside one cwd is not detected and is not guessed. |

`--lineage` builds a `LineageGraph` (nodes = tagged refs, edges =
`declared_fork` / `parent_thread` / `shared_prefix`). A parent laid under its
child never doubles inherited history: shared records stay once, tagged
`lineage_origin = inherited_from { conversation, via }`; the parent's own
continuation is tagged `parent_only`. Claude declares no session-level
parent; its fork shares the origin's record prefix (identical `uuid`s), which
`merge_inherited` counts once whenever both files are laid together.

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
this spec. `conversation.rs` / `mcp_session.rs` / `intents.rs` read
`ProjectionSpec`; none of them decides projection on its own.

Store-side types (`legacy_archive/canonical_projection.rs`,
`crates/aicx-parser/src/projections`) are a different layer. They are not
CLI output projection.

## Defaults

No flags means the razor view — complete, not cropped:

| Axis | Default | Why |
|---|---|---|
| Dialogue | Human (every channel) + `AssistantFinal` | Operator speech and the agent's last answers, in full. |
| Delayed human speech | **Shown** (W4 recovery) | Echo-bus / `queue-operation` are the operator's own words (`FrameClass::EchoSeal` / `Human { channel: Queue }` on the entry). `--user-only` that hid them returned 0 of the 25 messages the operator typed on `claude-67025fed`; the razor admits `EchoSeal`. `--dialog` additionally renders the seal/channel; `--kind human` narrows to the direct channel. |
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
| Echo-bus / queued speech with seals | `--dialog` | `dialog = true` (the speech itself is already in the razor) |
| Inter-agent lane | `--kind inter_agent` | `kinds` includes `InterAgent`; `roles` follows the kinds (`System` opens), so the lane is not empty by construction |
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
| `--agent-only` | `roles = [Assistant]`, `kinds = [AssistantFinal]` | Assistant **speech**. Reasoning (`inject`), `inter_agent` and `lineage_meta` are deliberately excluded: they are not the assistant talking to the operator. |
| `--user-commands` | `kinds = [ShellAction]`, `shell_executors = [Human]` | Commands the operator submitted (Codex `<user_shell_command>`). A command quoted in prose or proposed in an answer is not an execution. |
| `--agent-commands` | `kinds = [ShellAction]`, `shell_executors = [Agent]` | Tool / shell invocations the agent made. |
| `--conversation` | `kinds` razor minus `ShellAction` | The denoised speech view. It has **no** shell lane by contract, so it cannot be combined with the command flags — that pair is refused, not silently emptied. |
| `--max-message-chars N` | `max_message_chars` | `0` = unlimited dialogue. Does not change `result`. |
| `-p` / `--project` | `project` | Repeatable identity filter on the view (**OR** across projects, **AND** with every other axis), matched against each entry's recorded `cwd` via `project_filter_matches_path`. Fail-closed: an entry with no known cwd cannot be shown to belong to the requested project and is filtered out. A single value additionally names the output's project identity. |
| `-H` / `--hours` | `window.hours` | Filters **event** timestamps, never file mtimes, against one cutoff captured when the command starts (`apply_projection_at`). CLI `0` means unbounded → spec `None`; absence is never a silent lookback default. |
| `--since` / `--until` | `window.since` / `window.until` | Not on extract today; same window type as search. |
| `--kind <token>` | `kinds` (+ `roles` implied by the kinds) | **W2.** Tokens: `human`, `echo_seal`, `shell_action`, `inject`, `assistant_final`, `lineage_meta`, `inter_agent`. A lane carries its speaker: `inter_agent` / `inject` / `lineage_meta` open the `System` role; `--user-only` narrows back to `Human`. |
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
  result: Retained { text: "<412 lines of cargo output>", chars: …, hash: "c0ffee…" },
  executor: Human | Agent,
}
```

`executor` is decided once, at classification time, from the transport that
carried the frame — `TransportKind::UserShellCommand` proves a human
submission, `TransportKind::AgentToolCall` proves an agent invocation. It is
never re-derived from command text downstream: a command quoted in prose is
not an execution, and a proposal in an answer is not a run. Frames persisted
before this field decode as `Agent`, which is what every pre-split producer
except Codex's user envelope actually meant.

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
| `kinds` | `Vec<ProjectionKind>` | `Human`, `EchoSeal`, `AssistantFinal`, `ShellAction` |
| `result` | `None \| Head(n) \| Full` | `None` |
| `max_message_chars` | `usize` | `0` (unlimited) |
| `window` | `ProjectionWindow { hours, since, until }` | all `None` |
| `project` | `Vec<String>` | empty |
| `score` | `Option<u8>` | `None` |
| `dialog` | `bool` | `false` |
| `lineage_depth` | `Option<usize>` | `None` |
| `shell_executors` | `Vec<ShellExecutor>` | `Human`, `Agent` |

Empty `roles` / `kinds` vectors mean "emit nothing on that axis." They are
not a shortcut for default. Callers use `ProjectionSpec::default()` (razor)
or `ProjectionSpec::full()`.

## Out of scope (W2)

- Compilation. `BUILD/LINT/TEST` for this wave is embargoed (lifted in W3).
- Lanes the throne does not own (tool call/result, reasoning, Codex harness
  events): they carry no `frame_class` and keep `frame_kind` as their lane.

## Bulk projection (`aicx extract all`)

`aicx extract all` is the same projection applied to every compatible source
on the machine. It adds accounting, not a second taxonomy: discovery is the
session catalog, parsing is `parser_dispatch`, and the view is the
`ProjectionSpec` the flags above build.

Canonical types: `src/extraction/bulk.rs`.

| Concern | Rule |
|---|---|
| Agents | Enumerated from the parser registry (`bulk::ALL_AGENTS`), so a provider added to the registry is picked up without a second edit — and a provider that only exists in help text is not. |
| Incremental key | `source_fingerprint` (size + mtime) × `parser_version` × `projection_fingerprint`. Any of the three changing re-materializes; `--rebuild` ignores the state entirely. |
| Output paths | `<AICX_HOME>/extracts/<agent>/<session>[_conversation][_user][_<projection_fingerprint>].md`. Two filter sets over one session never overwrite each other. |
| Writes | Atomic (`legacy_archive::atomic_write`), and only after the parse succeeded. A fatal parse leaves nothing behind. |
| Manifest | `<AICX_HOME>/extracts/_bulk/manifest-<utc>.json` plus `manifest-latest.json`, schema `aicx.extract.all.manifest.v1`. |
| Buckets | `extracted`, `unchanged`, `empty_after_filter`, `unsupported`, `filtered_out`, `failed`. Every discovered source lands in exactly one; the totals reconcile against `discovered`. |
| `unsupported` vs `failed` | Decided by the adapter's own coverage ledger, never by the file name. A source the adapter consumed nothing from is `unsupported` **only if** no unit was skipped `Malformed` or `Oversized` — those two are verdicts on something the adapter *recognized* and could not deliver, so they are `failed`. Zero consumption alone proves nothing: a 281 MB gemini `chats/session-*.json` holding 158 real messages consumes zero units exactly like a `logs.json` that never was a session. The reason string carries the counts (`N of M raw unit(s) consumed; skipped …`) so triage never has to re-run the parse. |
| Unsupported visibility | `selected = discovered − unsupported`, so an unclaimed file never inflates the failure ratio. The summary prints a bounded per-provider rollup (`unsupported\t<provider>\t<n> source(s) …`) rather than a path dump; the manifest holds the paths. |
| `-H` fast path | A file's mtime is an upper bound on the newest event it can hold, so `mtime < cutoff - hours` **proves** the source has no in-window event and it is skipped without being opened. The row records which proof was used, so a deduction is never confused with "parsed and found nothing". A freshly copied archive of old events has a fresh mtime and is still parsed — this is a deduction, not the "select files by mtime" shortcut. |
| Exit status | `0` clean (**including an empty archive** — having no sessions is not an error), `3` partial, `4` every selected source failed. A partial run is never reported as overall success. |
| Streams | stdout carries the summary or, with `--json`, exactly one manifest document. Discovery and failure diagnostics go to stderr. Private session bodies are written to files, never dumped to stdout. |
| Sources | Read-only. A pass never rewrites, truncates or deletes a source. |

