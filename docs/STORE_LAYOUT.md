# Store Layout

`aicx` writes artifacts to the canonical store under `~/.aicx/` (cross-repo, global, machine-local).

Optional store control file:
- `~/.aicx/.aicxignore` — glob patterns relative to `~/.aicx/`; matching chunk paths are excluded from memex materialization and steer indexing.

## Canonical Store: `~/.aicx/`

Created and managed by `src/store.rs`.

### Layout

Contexts are chunked and stored by project and date:

```
~/.aicx/
  index.json
  store/
    <organization>/
      <repository>/
        <YYYY_MMDD>/
          <kind>/
            <agent>/
              <YYYY_MMDD>_<agent>_<session-id>_<chunk>.md
              <YYYY_MMDD>_<agent>_<session-id>_<chunk>.meta.json
  non-repository-contexts/
    <YYYY_MMDD>/
      <kind>/
        <agent>/
          <YYYY_MMDD>_<agent>_<session-id>_<chunk>.md
  memex/
    sync_state.json
  steer_db/
    (LanceDB metadata index)
```

Notes:
- Repository identity is derived from entry `cwd` via `repo_name_from_cwd` (`src/sources.rs`).
- Chunks are stored in a canonical hierarchy: `store/<org>/<repo>/<date>/<kind>/<agent>/`.
- If no repository can be inferred, chunks fall back to `non-repository-contexts/`.
- Each `.md` chunk has a sibling `.meta.json` sidecar containing steering and telemetry metadata.
- `steer_db` is a fast LanceDB-backed index of all sidecar metadata, enabling millisecond filtering by `run_id`, `prompt_id`, `agent`, etc.

### `index.json`

`index.json` is a manifest used to quickly list stored projects, dates and totals.
It is updated on every store write.

### Memex Integration

The `aicx memex-sync` command (and `--memex` flag) syncs stored chunks to `rmcp-memex`:
- It maintains sync state in `~/.aicx/memex/sync_state.json`.
- It honors `~/.aicx/.aicxignore` before queueing chunks for embed/index work.
- It performs metadata-rich imports, ensuring `project`, `agent`, `date`, `session_id`, and `kind` are preserved in the semantic index.

## Identity Model & Compatibility Rules (v0.5.0+)

Historically, `aicx` grouped contexts under a file-centric identity (e.g., `file: session.jsonl`). Starting in v0.5.0, AICX shifted to a strictly repo-centric identity model.

**Compatibility Rules:**
- Older stored artifacts are NOT automatically orphaned or silently broken on read. However, they will no longer be updated.
- To maintain a single coherent history, run `aicx migrate`. This command will cleanly move your older `~/.ai-contexters` contexts into the correct repository-named directories in `~/.aicx/` and update your `index.json`.

## Repo-local Context Artifacts: `.ai-context/`

While `aicx init` has been retired in favor of framework-level orchestration (`/vc-init`), the framework still produces repo-local artifacts for agent awareness:

```
.ai-context/
  share/
    artifacts/
      SUMMARY.md
      TIMELINE.md
      TRIAGE.md
```

Recommended sharing rules:
- Commit `share/artifacts/SUMMARY.md` and `share/artifacts/TIMELINE.md` to help onboarding new agents.
- Keep other artifacts local or share case-by-case.
