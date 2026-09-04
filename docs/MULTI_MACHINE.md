# AICX multi-machine topology

Operator questions this answers:

- Will catalog pick up session JSONL I sync between machines?
- Can I use a store location other than the daily home?
- Can laptop 0.6b dense indexes sit next to the owner's 8b?
- Can one host own indexing + secure HTTP for remote agents?

## Pipeline layers (do not collapse)

```text
agent source roots  →  catalog (identity)  →  extracts?  →  index CURRENT  →  search / MCP
     (content)            sessions.jsonl      optional      lexical (+dense)
```

| Layer | Command | What "stale" means |
|---|---|---|
| Sources | disk / rsync | file missing or path wrong on this host |
| Catalog | `aicx catalog status` / `rebuild` | unadmitted sessions, fingerprint drift, missing_source |
| Index | `aicx index status` / `index` | pending chunks, stale_index vs catalog snapshot |
| Serve | `aicx serve --transport http` | Bearer auth + Host allowlist |

`catalog status` and `index status` are **orthogonal**. Catalog fresh with
`index readiness=stale_index` means: identity is current, search lag remains.

## Source admission rules

Catalog rebuild and status walk **only** these roots on the machine that runs
the command:

- `~/.claude/projects`
- `~/.codex/sessions`
- `~/.cursor/projects`
- `~/.gemini/tmp`
- `~/.grok/sessions`
- `~/.junie/sessions`
- `~/.vibecrafted/control_plane/runtime_runs` (transcript identity)

There is **no** alternate session drop directory and no mill-era store intake.
Putting JSONL under `~/.aicx/store` or a custom folder does **not** admit them.

### Sync recipe that works

1. Rsync agent session trees onto the index owner (`host-a`) preserving paths
   when possible.
2. On the owner host: `aicx catalog status --json` → inspect `unadmitted` / `missing_source`.
3. `aicx catalog rebuild`
4. `aicx index status` → `aicx index` (or `--cache-extracts` if you want readable extracts).

### Sync recipes that fail closed

| Move | Failure mode |
|---|---|
| Copy only `~/.aicx/catalog/sessions.jsonl` | `missing_source` — absolute paths from the other host |
| Copy only dense generation dirs with different embedder | dimension/model mismatch on `--deep` / dense path |
| Point two hosts at one shared `AICX_HOME` over flaky FS | lock/races on CURRENT publish; prefer single writer |
| Drop JSONL outside agent roots | forever `unadmitted=0` and invisible |
| File-sync `~/.aicx/indexed` between hosts (MEGA, Syncthing, …) | CURRENT swaps to another host's generation; on lexical schema drift search now refuses with a typed writer-identified conflict instead of mystery corruption |

## Machine-local vs durable state (file-sync tools)

The hybrid Tantivy index is **machine-local**: every host rebuilds it with
`aicx index` from its own catalog. Syncing it between hosts running different
aicx builds swaps `CURRENT` between schema generations (observed 2026-08-10:
MEGA flipping `v3_folded_dictation` ↔ `v2_fast_body`).

Exclude from any file-sync of `~/.aicx` (MEGA `.megaignore` syntax):

```text
-dN:indexed
-dN:catalog
-dN:extracts
-dN:tmp
-fN:auth-token
```

Keep `store/` and `context-corpus/` synced (durable data). Classify
`state.json` before deciding.

Defense in depth since 2026-08-10: manifests carry `writer_version` /
`build_id`, and both publish and search reject a provable lexical schema
downgrade with a typed conflict naming the foreign writer.

## Relocating AICX home (not session intake)

Precedence (`src/aicx_home.rs`):

1. non-empty `$AICX_HOME`
2. `[storage].home` in `~/.aicx/config.toml`
3. `~/.aicx`

This relocates **catalog + extracts + indexed + state**. It is not a second
pipeline for foreign session dumps.

## Dense 0.6b vs 8b

- Lexical Tantivy CURRENT is model-agnostic: rebuild from sources on the owner.
- Dense mmap / embeddings are **fail-closed** on dimension and embedder model drift.
- Do **not** park laptop 0.6b generations beside the owner's `qwen3-embedding:8b`
  (4096-d) as one CURRENT.
- Practical rule: **one embedder profile per AICX home**. Laptop can keep a
  separate `AICX_HOME` for offline experiments; the owner host is production owner.

## Recommended topology (one owner host)

```text
[ laptop / remote agents ]
        |  session jsonl rsync → agent roots on the owner host
        |  local: aicx index  (lexical only, fast)
        |  MCP streamable HTTP (Bearer) for deep / shared search
        v
[ host-a (index owner) ]
  - owns ~/.aicx (or AICX_HOME)
  - aicx catalog rebuild && aicx index
  - aicx index --semantic     # one dense CURRENT; opt-in, owner-only
  - embedder: cloud URL in config.toml (e.g. Ollama qwen3-embedding:8b)
  - aicx serve --transport http --host 0.0.0.0 --port 8044 \
      --auth-token "$TOKEN" --allowed-host host-a --allowed-host <tailscale-name>
  - optional: cloudflare tunnel for grok.com / slack / web agents
```

Status planes on every host:

```bash
aicx index status
# lexical_status: ready|missing
# dense_status:   ready|not_built|missing
```

Laptop `dense_status=not_built` with `lexical_status=ready` is **normal**.

Auth today is **Bearer token**, not OAuth. Token sources: CLI flag, env
`AICX_HTTP_AUTH_TOKEN`, or generated file under AICX home. Host header
validation stays on unless you deliberately use `--allow-any-host` on a
trusted network only.

Dashboard (`aicx dashboard --serve`) is a separate HTTP surface with its own
Bearer defaults — do not confuse it with MCP `serve`.

## Operator checklist after a sync

```bash
aicx catalog status --json   # identity pressure
aicx catalog rebuild         # only if needs_rebuild / missing
aicx index status -j         # CURRENT pressure (expect stale_index with pending)
aicx index                   # publish when ready
aicx search 'smoke query'    # runtime truth
```

If `catalog status` is `fresh` but search misses new sessions, the lag is
**index**, not catalog.

## See also

- [COMMANDS.md](./COMMANDS.md) — daily path and catalog classes
- [AICX_HOME_LAYOUT.md](./AICX_HOME_LAYOUT.md) — on-disk layout
- [EMBEDDINGS.md](./EMBEDDINGS.md) — embedder profiles
