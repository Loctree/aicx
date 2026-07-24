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
# Refresh source identity and project attribution.
aicx catalog rebuild

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

## Runtime artifacts

| Surface | Purpose |
|---|---|
| `~/.aicx/catalog/sessions.jsonl` | Session identity, project, agent, date, cwd, source path, title, machine, and source fingerprint |
| `~/.aicx/extracts/` | Optional whole-session readable extract cache |
| `~/.aicx/indexed/_all/hybrid/CURRENT` | Pointer to the published global search generation |
| live agent source roots | Canonical session content |

No command in the current ingestion or indexing path writes per-frame cards.

## Catalog

```bash
aicx catalog rebuild
aicx catalog resolve <session-id>
```

`catalog rebuild` walks the registered Claude, Codex, Grok, Gemini, Junie, and
Vibecrafted runtime roots. It writes the compact catalog and prints counts; it
does not materialize session content.

## Extract

```bash
aicx extract claude --session <session-id> --conversation
aicx extract codex --session <session-id> --conversation
aicx extract grok --session <session-id> --conversation
aicx extract gemini --session <session-id> --conversation
```

Use `--output <path>` for an explicit file. Session mode resolves through the
catalog and opens only an allowlisted, canonical source path.

Batch report export remains available through `aicx claude`, `aicx codex`,
`aicx all`, and `aicx conversations`. Those commands write requested reports,
not card-mill files.

## Index

```bash
aicx index --dry-run
aicx index
aicx index --cache-extracts
aicx index --full-rescan
aicx index status
```

Persistent indexing always owns the global `_all` generation. `-p` on
`aicx index` limits dry-run inspection only; `aicx search -p` filters the
global catalog metadata.

Incremental reuse is gated by the live source fingerprint. A changed existing
session reparses on the next run even when no catalog rebuild happened.

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

The wizard's Rebuild screen runs `catalog rebuild` followed by `index`; it
does not shell a removed command.

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
