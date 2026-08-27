# AICX command guide

`aicx` is source-first. Live agent logs are cataloged, rendered as readable
session extracts, and indexed into one global lexical generation. Project
selection is metadata filtering over that generation.

The authoritative grammar is always:

```bash
aicx --help
aicx <command> --help
```

## Daily path

```bash
# Bounded daily admission; also repairs cwd-guessed identities from git origin.
aicx catalog refresh

# Incrementally parse changed sources and publish CURRENT.
aicx index

# Search CURRENT. Lexical + recency is the default.
aicx search 'arrows vc-frame'
aicx search -p vetcoders/vibecrafted 'routing strzałek taby'

# Opt into dense mmap re-ranking only when needed.
aicx search --deep 'routing strzałek taby'
```

`aicx index --cache-extracts` additionally caches one readable conversation
file per session under `~/.aicx/extracts/`. Without that flag, source logs and
the published index remain the content owners.

Checkout prefixes in `~/.aicx/.aicxignore` are part of index and cache
identity. Editing the file makes the next `aicx index` rebuild automatically;
an unreadable file or unsupported checkout glob/negation aborts rather than
indexing without the deny list.

## Runtime artifacts

| Surface | Purpose |
|---|---|
| `~/.aicx/catalog/sessions.jsonl` | Session identity, project, agent, date, cwd, source path, title, machine, and source fingerprint |
| `~/.aicx/extracts/` | Optional whole-session readable extract cache |
| `~/.aicx/indexed/_all/hybrid/CURRENT` | Pointer to the published global search generation |
| live agent source roots | Canonical session content |

No command in the current ingestion or indexing path writes per-frame cards.

## Wizard (TUI + vc-frame assimilation)

```bash
aicx wizard
aicx wizard --view search
aicx wizard --view search --query 'problemy dziwne' -p /pensieve -a claude
```

Screens: `1` sessions · `2` doctor · `3` intents · `4` refresh · `5` search.
`/` from any screen opens the Search query box and switches to Search.

Wizard entry infers the exact current `owner/repo` from git origin, performs a
bounded hot refresh, and never requires a session id. Screen 4 runs hot refresh
plus incremental indexing; full rebuild remains an explicit census repair.

Search uses the same engine as `aicx search` (lexical CURRENT). Results always
declare who answered (`index CURRENT …` vs `fallback FS (bounded, recency)`).
Missing/stale index shows a drift banner + repair path — never silent empty.

JSON surfaces for plugins / vc-frame Session Gallery:

```bash
aicx search --json '<query>'
aicx sessions list --json
aicx sessions list --cwd --json
aicx sessions list -p /aicx --json
aicx sessions show --json <session-id>
# aliases also work: sessions list --format json
```

`sessions list -p` uses the same exact identity rules as search and
MCP (`owner/repo`, `/repo`, `owner/`, unique bare name). An empty list
for a resolved project prints `scanned=N; matched=0` — that is a
project miss, not a missing source tree. `--cwd` matches the current
checkout, including Grok's percent-encoded session roots.

## Catalog

```bash
aicx catalog status
aicx catalog status --json
aicx catalog refresh
aicx catalog rebuild
aicx catalog resolve <session-id>
```

`catalog status` compares durable `sessions.jsonl` fingerprints to live agent
source roots **without writing**. Classes:

| Class | Meaning | Operator move |
|---|---|---|
| `current` | catalog size+mtime matches live file | none |
| `stale` | same session, live append/edit | `aicx catalog refresh`; index already re-parses on live fp |
| `unadmitted` | live primary source not in catalog | `aicx catalog refresh` |
| `missing_source` | catalog path does not resolve here | fix paths / sync sources onto this host |
| `fingerprint_unknown` | no usable stats | rebuild to stamp `source_len` / `source_mtime_ns` |

Readiness values: `missing` · `empty` · `fresh` · `needs_rebuild` · `sources_missing`.

Orthogonal surfaces:

- **catalog status** = will rebuild admit/change identity rows?
- **index status** = is CURRENT lagging the catalog/corpus?

`catalog rebuild` walks the registered Claude, Codex, Grok, Gemini, Junie, and
Vibecrafted runtime roots. It writes the compact catalog and prints counts,
including `pending_chunks`. It does not materialize session content unless
`--with-chunks` is passed, which drains the lag through `aicx index`.

`catalog refresh` scans only a bounded hot window, merges new/changed sessions
under the catalog lock, and reattributes existing rows whose `cwd` resolves to
a git origin. It refuses to create an initial partial census: one full rebuild
is still required on a new AICX home.

`$AICX_HOME/.aicxignore` is the operator deny list for search memory. Each
line is a checkout path (`~/Repozytoria/moje_prywatne` or absolute). That
directory and every repo under it are excluded at **frame** grain: a
session that wandered in for one turn still indexes its other project
buckets. Re-run `aicx index` after editing the file (cached extracts are
not reused while ignore rules are present).

### Multi-machine / sync (operator truth)

1. **Session JSONL sync** — catalog only discovers files under this host's agent
   roots (`~/.claude/projects`, `~/.codex/sessions`, `~/.gemini/tmp`,
   `~/.grok/sessions`, `~/.junie/sessions`, `~/.vibecrafted/control_plane/runtime_runs`).
   Drop synced JSONL into those trees, then `catalog status` → `catalog rebuild`.
2. **No alternate daily store intake** — there is no second "drop folder" for
   sessions. `AICX_HOME` / `[storage].home` relocates the **whole** home
   (catalog + index + extracts), not a parallel session root.
3. **Copying only `sessions.jsonl` fails** — rows keep absolute `source_path`;
   the indexing host must open those paths. Symptom: `missing_source`.
4. **0.6b next to 8b** — dense generations are model+dimension locked
   (fail-closed). Do not merge laptop 0.6b dense into the owner's 8b CURRENT.
   Rebuild lexical CURRENT on the owner host from shared sources; optional
   dense on that same embedder only.
5. **One host as index owner** — recommended: the owner host runs `catalog rebuild` +
   `index`, hosts embedder (`config.toml` cloud URL), and
   `aicx serve --transport http --auth-token … --allowed-host <tailscale-name>`.
   Remotes use streamable HTTP MCP with Bearer (not OAuth). See
   [MULTI_MACHINE.md](./MULTI_MACHINE.md).

## Extract

```bash
aicx extract claude --session <session-id> --conversation
aicx extract codex --session <session-id> --conversation
aicx extract grok --session <session-id> --conversation
aicx extract gemini --session <session-id> --conversation
```

Use `--output <path>` for an explicit file. Session mode resolves through the
catalog and opens only an allowlisted, canonical source path.

### Projection flags (W2-T13)

Every extract flag below populates one `ProjectionSpec`
(`src/extraction/projection.rs`, contract:
[OUTPUT_PROJECTION_CONTRACT.md](./OUTPUT_PROJECTION_CONTRACT.md)). Flags never
mutate the substrate: the same session under `--user-only` and under
`--result full` is one parse, two views.

```bash
aicx extract codex --session <id> --conversation                 # razor: human + assistant-final + `$ cmd [N lines, sha256:…]`
aicx extract codex --session <id> --conversation --dialog        # + delayed human speech (echo-bus / queue) with seals
aicx extract codex --session <id> --conversation --result head=5 # first 5 lines of each retained shell result
aicx extract codex --session <id> --conversation --result full   # whole retained result bodies
aicx extract codex --session <id> --lineage                      # walk session_meta.forked_from_id parents (unbounded)
aicx extract codex --session <id> --lineage=1                    # at most one parent
aicx extract codex --session <id> --kind human,echo_seal         # throne kinds only
aicx extract codex --session <id> --kind inter_agent             # inter-agent lane (never rendered as assistant)
aicx extract codex --session <id> -H 6 --conversation            # window on the view (0 = unbounded, the default)
```

| Flag | View |
|---|---|
| (none) | Razor: `human` + `assistant_final` + `shell_action` stubs. Cardinality, seals, `$ cmd [N lines, sha256:…]`. |
| `--dialog` | Delayed human speech (`echo_seal`: echo-bus / `queue-operation`) as speech, with its seal timestamp. |
| `--result none\|head=N\|full` | Retained shell result body. `none` is the stub (default); the hash names the same body `full` prints. |
| `--lineage[=N]` | Parent sessions read from `session_meta.forked_from_id` and resolved through the session catalog — meta, never filenames. Emits `lineage[d]: <id> forked_from <parent>` entries; parents are parsed once each through the same view. Needs `--session`. |
| `--kind <token>` | Throne kind filter: `human`, `echo_seal`, `shell_action`, `inject`, `assistant_final`, `lineage_meta`, `inter_agent`. Unknown tokens refuse (`invalid_kind_token`). |
| `--user-only` | Role axis `Human` only (also drops shell actions). Same spec on the MCP `aicx_session` surface. |
| `-H/--hours N` | Window on the view. `0` = unbounded; no silent 30-day default. |
| `-p/--project` | Identity on the view, not on the parse. |
| `--max-message-chars N` | Dialogue truncation; `0` = unlimited. Never touches result bodies (`--result` does). |

`inject` (harness reminders, compaction replays, agent instructions) never
enters intent distillation, and neither does the `inter_agent` lane —
`aicx intents` reads `human` + `echo_seal` only.

Known limits of this wave: the session model flattens the throne
(`EchoSeal` arrives as `user_msg`, `LineageMeta` as `system_note`,
`InterAgent` has no turn kind), so `--dialog` and `--kind inter_agent`
select on the spec but cannot yet separate those classes from `human` /
`inject` until the model carries `FrameClass`. Claude sources carry no
session-level `forked_from_id`; `--lineage` reports that instead of guessing.

Batch report export remains available through `aicx claude`, `aicx codex`,
`aicx all`, and `aicx conversations`. Those commands write requested reports,
not card-mill files.

## Index

```bash
aicx index --dry-run
aicx index                    # lexical CURRENT only (default, every machine)
aicx index --semantic         # opt-in dense mmap (owner workstation)
aicx index --cache-extracts
aicx index --full-rescan
aicx index status             # lexical_status + dense_status planes
```

Persistent indexing always owns the global `_all` generation. `-p` on
`aicx index` limits dry-run inspection only; `aicx search -p` filters the
global catalog metadata.

**Planes (feature, not bug):**

| Command | What it publishes |
|---|---|
| `aicx index` | Tantivy lexical only (`dense_kind=optional_not_built`) |
| `aicx index --semantic` | Lexical + `dense.exact_mmap_v1.bin` via configured embedder |

Default search is lexical-first. Dense rerank needs `--semantic` once on the
owner host, then `aicx search --deep`.

Incremental reuse is gated by the live source fingerprint. A changed existing
session reparses on the next run even when no catalog rebuild happened.
`--semantic` will rebuild when dense is missing even if lexical CURRENT
already matches.

Signal filtering keeps user messages, agent replies, plans, and reports while
dropping tool-call noise, internal thought, system reminders, and image/base64
payloads.

## Search

```bash
aicx search <query>
aicx search -p owner/repo <query>
aicx search -p owner/ <query>
aicx search -p /repo <query>
aicx search --deep <query>
aicx search --no-semantic <query>
aicx search --session <session-id> <query>
aicx search --session <session-id> --literal --context 4 <query>
aicx search --json <query>
```

The default route is Tantivy lexical search with a recency prior. `--deep`
adds dense mmap re-ranking. A bounded, recency-ranked filesystem fallback is
used only when the typed failure is `IndexNotBuilt`; corrupt, stale, or busy
indexes fail honestly. `--no-semantic` is stricter: it reads the published
CURRENT lexical generation or returns a typed error, and never falls through
to filesystem fuzzy search.

`--session` is the last-mile passage route after a session-level hit. It reads
the cached whole-session extract when present; otherwise it parses the live
source through the same signal-only path as the index without writing a cache.
Token matching is the default. `--literal` performs an exact,
identifier-boundary match. Passages are source ordered, stably numbered, and
include a line span, source path, and ±2 context lines by default (`--context
N` overrides it).

Search hits render through the same `ProjectionSpec` as extract: `-p`,
`-H`, `--since`/`--until`, `--score`, and `--frame-kind` populate the spec;
`--dialog`, `--result none|head=N|full` apply to hit rendering (a `tool_call`
hit collapses to `$ cmd [N lines, sha256:…]` by default). `--lineage[=N]` is
accepted for grammar parity and reported as not applied — hits are chunks,
the parent walk lives on `extract`. `--kind` on search stays a document class
(`conversations` | `plans` | `reports` | `other`); throne kinds go through
`--frame-kind` (`user_msg`→`human`, `agent_reply`→`assistant_final`,
`internal_thought`→`inject`, `tool_call`→`shell_action`).

Every search prints `scanned N of M sessions; skipped: ...` to stderr.
Machine-readable output also carries the same structured `coverage` object.
Missing CURRENT rows are counted per extractor (for example,
`gemini_unindexed=116`) instead of becoming silent holes.

Project filters are exact by default. Ambiguous bare repository names fail
closed; `--project-fuzzy` is an explicit opt-in.

## Status and diagnostics

```bash
aicx index status
aicx health
aicx doctor
aicx wizard
```

`index status` is bounded and reports `missing` or `pending_scan_timeout`
instead of silently walking the operator home without a deadline.

The wizard's Refresh screen runs `catalog refresh` followed by incremental
`index`; it does not turn the daily UI into a full source-root walk.

## Legacy archive and migration

`aicx store` is removed. The command, card writers, canonical-projection
writer, and card-mill environment switch do not exist.

Read-only legacy archive and doctor/quarantine surfaces remain so operators can
inspect or recover old `~/.aicx/store/` corpses. `aicx migrate` and
`aicx migrate --cards-v2` are recovery tools for those existing artifacts;
they are not part of the live catalog/extract/index path.

Destructive removal of retired NDJSON index artifacts remains an explicit
operator action. The application does not delete `~/.aicx/indexed/*`
automatically.

## Project filter grammar

Accepted repeatable forms:

```text
-p owner/repo   exact project
-p owner/       every repository under an owner
-p /repo        repository name across owners
-p name         unique exact owner or repository; ambiguity fails
```

Comma lists and repeated flags form a union. Substring matching is not the
default contract.

## Machine-readable output

Commands that support `--json` emit structured stdout. Diagnostics and
progress go to stderr. Consult the command-specific help for exit codes and
the exact JSON envelope.
