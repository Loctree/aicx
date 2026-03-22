# Store Snapshot — Repo-Centric Reconstruction (2026-03-21)

This file captures the current architectural truth about AICX store semantics,
the intended target shape, and the gaps we still need to close.

It is a snapshot of intent, not a claim that the current implementation already
matches the target.

## Why this snapshot exists

We discovered that the current store model is still too close to a dumb
input-driven extractor:

- some stored directories are still created from weak source-side identity
  such as Gemini `project_hash`
- migration only handles a narrow legacy case (`file: ...`)
- store pruning exists only for dedup state, not for the actual on-disk store
- the desired product is not "store whatever file we found", but "reconstruct
  work on a repository from signals found inside the data"

This is a semantic architecture change, not a naming cleanup.

## Canonical direction

AICX store must become:

- repo-centric, not input-path-centric
- conversation-aware, not artifact-dump-centric
- source-signal-driven, not folder-name-driven
- segment-based, so one input source may contribute to multiple repositories

The key rule:

> No output directory may be derived from source file names, source folder
> names, hashes, or other incidental filesystem input identifiers.

Store identity must come from signals extracted from inside the content.

## Canonical store target

Canonical root:

```text
~/.aicx/store/<organization>/<repository>/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md
```

Where:

- `<organization>/<repository>` is the canonical project identity
- `<YYYY_MMDD>` is derived from the timestamp of the source event stream, not
  from the moment `aicx store` was run
- `<kind>` is one of:
  - `conversations`
  - `plans`
  - `reports`
  - `other`
- `<agent>` is one of:
  - `claude`
  - `codex`
  - `gemini`
  - `other`
- `<session-id>` is the primary file identity
- `<chunk>` is the segment ordinal within the same session/repo/kind/day split

## Why the filename must include date

We considered a shorter name based only on:

```text
<agent>_<session-id>_<chunk>.md
```

That is weaker.

The stronger canonical basename is:

```text
<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md
```

Reason:

- files are often seen outside their original directory context
- Spotlight, Finder, exports, sync tools, and search results frequently expose
  only the basename
- the basename must remain self-describing even when detached from its path
- `session-id` provides identity, but the date provides operational meaning and
  lexicographic ordering

## Session identity over timestamp identity

We do **not** want timestamp-based uniqueness as the primary file identity.

The intended model is:

- `session-id` = identity
- `date` = partition
- `chunk` = segmentation of a session across repo/kind boundaries

Timestamps still matter, but they belong:

- inside file content
- in metadata
- in segmentation logic

They do not need to carry filename uniqueness if `session-id` exists.

## Segmentation rule

One input file may contain multiple changes of working context.

Therefore the store pipeline must insert a semantic layer between source input
and output store:

1. read source data
2. infer project/repo signals from content
3. detect context switches across the event stream
4. split into per-repo segments
5. classify each segment as `conversation`, `plan`, `report`, or `other`
6. emit to canonical repo-centric store

This means a single source file may legitimately produce:

- multiple repositories
- multiple days
- multiple chunks

## Non-repository contexts

Not every extracted session or segment will yield a trustworthy repository
identity.

When repo identity cannot be inferred honestly, the output must **not** be
forced into a fake repository bucket.

Those segments land here instead:

```text
~/.aicx/non-repository-contexts/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md
```

This gives them a real home without polluting canonical repo-centric truth.

## Current implementation truth

As of this snapshot, the implementation does **not** fully match the target.

### 1. Store grouping is still partially wrong

`run_extraction()` groups entries for store writes by:

- repo inferred from `repo_name_from_cwd(entry.cwd, ...)`
- agent
- date

This is correct in shape, but only as strong as `entry.cwd`.

### 2. Gemini CLI still writes weak identity into `cwd`

Current Gemini CLI parsing uses `project_hash` as pseudo-cwd.

That means the store can still produce directories like:

- `57cfd37b...`
- `94c6aaf0...`

Those are not canonical repository identities. They are source-side artifacts.

### 3. Migration is too narrow

Current migration only targets legacy projects prefixed with:

```text
file: ...
```

It does not handle:

- hash-derived project folders
- `unknown`
- weak last-path-segment buckets such as `tmp`, `src-tauri`, `hosted`, etc.

### 4. Pruning is not a real store prune

Current pruning only affects dedup state (`seen_hashes` in state).

It does **not** prune:

- store directories
- chunk files
- index entries
- stale or non-canonical project buckets

## Migration truth

True repo-centric migration is only possible when the original source files
still exist.

If source files are gone, we cannot honestly reconstruct:

- context-switch boundaries
- per-repo segmentation
- conversation vs plan vs report vs other separation
- correct session-derived grouping

So migration must be treated as:

## Rebuild + Salvage model

### A. Sweep old store

Scan `~/.ai-contexters/` and inventory everything currently stored.

### B. Rebuild from source when possible

If the original source still exists:

- rescan source
- reconstruct repo segments from content signals
- emit into the new `~/.aicx/store/...` canonical format

### C. Preserve as legacy when source is gone

If the original source no longer exists:

- do **not** pretend we can fully migrate it semantically
- copy/preserve it under:

```text
~/.aicx/legacy-store/
```

This keeps the data accessible without falsely promoting it to canonical truth.

## Open questions

These are still intentionally unresolved:

1. How exactly do we infer `<organization>/<repository>` when content only
   weakly references a repo?
2. What is the fallback when a session has no true `session-id`?
3. What exact heuristics and signal precedence determine `kind`:
   `conversations`, `plans`, `reports`, `other`?
4. Do we want a migration manifest, e.g.:

```text
~/.aicx/migration-index.json
```

to record:

- old path
- source path
- rebuilt yes/no
- legacy copied yes/no

5. Should canonical output always emit both `.md` and `.json`, or is `.md`
   enough for v1 of the new store?

## Immediate next truths

Highest-leverage fixes after this snapshot:

1. Stop generating new non-canonical repo buckets, especially from Gemini
   `project_hash`.
2. Promote the store pipeline from input-driven grouping to source-signal
   segmentation.
3. Add the non-repository fallback path instead of forcing weak repo guesses.
4. Build migration as rebuild-first, not rename-first.
5. Add a real store prune / retention model only after canonical layout lands.

## One-sentence principle

The new AICX store is not an archive of source files. It is a semantic library
of work performed on repositories, reconstructed from conversations and
artifacts.
