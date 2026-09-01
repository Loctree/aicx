# Parser-engine oracle assertions

Machine source: `assertions.toml` (`schema = "aicx.parser.oracle_assertions.v1"`).
This markdown is the operator index. W4-T15 runs the TOML against the production binary.
Synthetic parser-oracle envelopes stay in per-agent `expected.json`; they are a different contract.

Engine used to freeze counts: **aicx@a687754** (`aicx 0.12.5+ga687754c`, `cargo build --bin aicx` in this worktree).

UserMsg means rendered markdown headings of the form `**[HH:MM:SS] user:**` from `aicx extract <agent> --file <fixture> --conversation [--user-only]`.

## Fixtures in this cut

| Id | Path | Bound | Notes |
|---|---|---|---|
| claude 67025fed | `claude/human_shape_67025fed.jsonl` | 52 queue-operation lines | enqueue=26 dequeue=6 remove=20 |
| grok 019fdeca | `grok/degenerate_019fdeca/` | whole session minus locks | 2-line `chat_history.jsonl` |
| grok 01a038ec | `grok/human_shape_01a038ec/` | user+system + summary | 5 raw user records |
| junie 260528 | `junie/human_shape_260528.jsonl` | selected A2UX kinds | no UserPromptEvent |
| gemini 9048328b | `gemini/human_shape_9048328b.jsonl` | whole file 150034 B | 1 raw user, dropped by extract |
| A2 01a042f9 | `human_shape_01a042f9_interagent.jsonl` | 28 agent_message + 1 compacted | not under `codex/` (W0-T1) |

Derivation commands live in the sibling `*.README.md` / `README.md` files.

External (not copied):

- A1 raw: `external:inputs/fixtures/codex/rollout-2026-08-25T03-53-42-01a0369f-9313-7592-8303-3db46b6f8b47.jsonl` (262 MB)
- A2 raw: `external:inputs/fixtures/codex/diagnostic-01a042f9/rollout-2026-08-27T13-27-04-01a042f9-3803-7772-bf00-1887a50aaf89.jsonl` (34.7 MB)

## Frozen counts (aicx@a687754)

| Assertion id | kind | expected | status |
|---|---|---|---|
| claude-67025fed-enqueue-count | count | 26 | baseline |
| claude-67025fed-enqueue-seal | seal | `2026-08-25T19:53:58.988Z` | baseline |
| claude-67025fed-user-only-usermsg | count | 25 | baseline |
| claude-67025fed-enqueue-content-presence | presence | `Z mojej rozmowi z codexem` | baseline |
| claude-67025fed-dequeue-count | count | 6 | hypothesis |
| claude-67025fed-remove-count | count | 20 | hypothesis |
| grok-019fdeca-typed-refusal | presence | `RefusalReason` | expected_fail_until_W2-T12 |
| grok-019fdeca-usermsg-today | count | 0 | baseline |
| grok-01a038ec-raw-user-records | count | 5 | baseline |
| grok-01a038ec-user-only-usermsg | count | 2 | baseline |
| gemini-9048328b-raw-user-records | count | 1 | baseline |
| gemini-9048328b-user-only-usermsg | count | 0 | baseline |
| gemini-9048328b-conversation-assistant | count | 21 | baseline |
| junie-260528-user-only-usermsg | count | 0 | baseline |
| junie-260528-conversation-assistant | count | 1 | baseline |
| a2-01a042f9-agent-message-raw | count | 28 | baseline (external raw) |
| a2-01a042f9-conversation-agent-as-assistant | count | 0 | expected_fail_until_W2-T7 (today 28 assistant) |
| a2-01a042f9-kind-inter-agent | count | 28 | expected_fail_until_W2-T13 (`--kind` missing) |
| a2-01a042f9-compacted-no-new-utterances | count | 0 | baseline (see note in TOML) |
| a1-01a0369f-compacted-markers | count | 38 | hypothesis |
| a1-01a0369f-turn-context-cwd-changes | count | 0 | hypothesis |
| a1-01a0369f-multiline-echo-quoted-speakers | count | 1 | expected_fail_until_W2-T7 |

## Measurement notes (not status theatre)

- Grok 2-line: silent empty success, exit 0. Typed refusal is the W2-T12 contract.
- Claude dequeue/remove: bytes exist; meaning is hypothesis.
- A2 compacted replay: brief marked expected_fail; aicx@a687754 emits 0 extra headings from `replacement_history`. Frozen as expected=0 that W2-T7 must keep.
- A1 quoted-speaker echo: labels `Monika Szymańska:` / `Maciej Gad:` were not found in echo command bodies. Today the echo bus is absent from `--user-only` (0, not a 3-way split).
- Codex fixtures under `tests/fixtures/parser_engine/codex/` are W0-T1. Untouched.
