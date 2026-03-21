# Store Layout

`aicx` writes two kinds of artifacts:
- Central store under `~/.ai-contexters/` (cross-repo, global, machine-local).
- Repo-local init workspace under `.ai-context/` (meant for collaboration within a repo).

## Central Store: `~/.ai-contexters/`

Created and managed by `src/store.rs`.

### Layout

Contexts are chunked and stored by project and date:

```
~/.ai-contexters/
  index.json
  <project>/
    2026-02-08/
      031122_claude-001.md
      031122_claude-002.md
      031122_codex-001.md
  memex/
    chunks/
      <chunk_id>.txt
```

Notes:
- `<project>` is derived from entry `cwd` via `repo_name_from_cwd` (`src/sources.rs`), so mixed runs across repos end up separated automatically.
- Chunk filenames are tied to the run timestamp (`HHMMSS`) and chunk sequence (`001`, `002`, ...).
- The content is “agent-readable” (compact, line-based, consistent header) and safe-truncated for huge messages (UTF-8 safe).

### `index.json`

`index.json` is a manifest used to quickly list stored projects, dates and totals.
It is updated on every store write.

### `memex/chunks/`

`memex/chunks/` contains pre-chunked `.txt` files written by `src/chunker.rs::write_chunks_to_dir`.

These are meant for indexing via `rmcp-memex`:
- batch mode: `rmcp-memex index <dir> ...`
- per-chunk mode: `rmcp-memex upsert <chunk_id> ...`

The `aicx memex-sync` command wraps this behavior and maintains a minimal sync state (see `src/memex.rs`).

## Identity Model & Compatibility Rules (v0.5.0+)

Historically, `aicx` grouped contexts directly extracted from specific files under a file-centric identity (e.g., `file: session.jsonl`). Starting in v0.5.0, AICX has shifted to a strictly repo-centric identity model:
- Project directories and memex namespaces are now grouped by the inferred repository name first.
- The source file path is retained only as secondary metadata (`provenance`).

**Compatibility Rules:**
- If you have scripts, queries, or memex pipelines that rely on the old `file: <name>` groupings, you should update them to query by your repository name.
- Older stored artifacts are NOT automatically orphaned or silently broken on read. However, they will no longer be updated.
- To maintain a single coherent history, run `aicx migrate`. This command will cleanly move your older `file: *` contexts into the correct repository-named directories and update your `index.json`.

## Repo Init Workspace: `.ai-context/`

Created by `aicx init` (see `src/init.rs`).

```
.ai-context/
  share/
    artifacts/
      SUMMARY.md
      TIMELINE.md
      TRIAGE.md
      prompts/
  local/
    config/
    context/
    logs/
    memex/
    prompts/
    runs/
    state/
```

Recommended sharing rules:
- Commit `share/artifacts/SUMMARY.md` and `share/artifacts/TIMELINE.md` by default.
- Decide case-by-case for `TRIAGE.md` and `prompts/` (often useful for multi-agent teams).
- Keep `.ai-context/local/` uncommitted.

