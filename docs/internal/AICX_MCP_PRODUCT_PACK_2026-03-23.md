# AICX MCP Feature And API Surface Pack

Date: 2026-03-23

Primary brief: `.ai-agents/tmp/20260323_1205_aicx_mcp_050_user_smoke_context.md`

## Purpose

Turn the current smoke into a sprint-ready product pack for `aicx-mcp` and the adjacent CLI/dashboard read path.

This is not a rewrite plan. It is a user-facing contract pack aimed at making AICX obviously useful in one session:

`store -> discover -> open the right chunk -> continue work`

## Current State

AICX already works as a catalog:

- `store` refreshes recent history well
- `refs` helps find files
- `rank` is useful for triage
- `search` can find real signal

But the product still breaks at re-entry:

- discover surfaces mostly stop at paths, not readable records
- `search` exists in MCP and dashboard-server, but not as a first-class CLI read path
- `refs` and `rank` use file modification time for recency while showing canonical chunk dates, which makes the window semantics feel wrong
- `search` returns raw chunk hits with weak collapse, so one episode can dominate the result list
- project filtering semantics are too fuzzy for repo-centric truth
- user-facing docs still describe older `~/.ai-contexters` semantics while implementation has moved to `~/.aicx`

## Grounding In Current Contracts

These recommendations are anchored in the current codebase, not in speculative future architecture:

- `src/mcp.rs` currently exposes only `aicx_search`, `aicx_rank`, `aicx_refs`, and `aicx_store`
- `src/main.rs` has `refs`, `rank`, `dashboard`, `dashboard-serve`, and `intents`, but no CLI `search` or `read`
- `src/store.rs::context_files_since()` filters by filesystem `modified()` time, not by canonical chunk date/session date
- `src/rank.rs::fuzzy_search_store()` returns raw per-chunk hits, uses strict normalized AND matching, and does not expose a stable chunk/session reference
- `src/dashboard_server.rs` has fuzzy/semantic/cross search endpoints, but no shared chunk read/open endpoint
- `README.md`, `docs/COMMANDS.md`, `docs/STORE_LAYOUT.md`, and `docs/ARCHITECTURE.md` still describe older store roots and examples

## Prioritized Pack

### P0

#### 1. Direct chunk read/open after discover

Why it matters:

Today the user can find promising files, but the product still forces a filesystem hop to actually read one. That is the biggest break in the current workflow.

Surface should feel like:

- MCP: `aicx_read`
- CLI: `aicx read`
- Input should accept a stable chunk reference when available, with a safe absolute-path fallback for the first cut
- Output should include metadata plus readable content, not just a path
- The tool should be optimized for selective re-entry, not for dumping entire stores

Acceptance shape:

- `refs`, `rank`, and `search` return something chainable into `read`
- `read` returns at minimum: `project`, `kind`, `agent`, `date`, `session_id`, `chunk`, `path`, and `content`
- `read` supports truncation options such as `max_chars` or `max_lines`
- `read` validates paths through the existing sanitize layer

Likely landing zones:

- `src/mcp.rs`
- `src/main.rs`
- `src/store.rs`
- `src/sanitize.rs`

Likely tests:

- read by canonical ref
- read by validated path
- truncation behavior
- CLI parsing tests in `src/main.rs`
- MCP parameter/contract tests in `src/mcp.rs`

Notes:

This is the highest-leverage feature addition from the user point of view.

#### 2. First-class `latest` and `timeline` helpers with honest time semantics

Why it matters:

The smoke pain is not only “I found too many files.” It is also “I cannot quickly ask for the newest meaningful history for this repo.” Right now `refs` and `rank` are doing double duty as both discovery and recency helpers, and the recency contract is muddy.

Surface should feel like:

- MCP: `aicx_latest`, `aicx_timeline`
- CLI: `aicx latest`, `aicx timeline`
- Default sort/order should reflect canonical chunk/session time, not filesystem accident
- The user should be able to ask for “latest 5 meaningful chunks for this repo” or “timeline for this repo in the last 7 days”

Acceptance shape:

- default window basis is canonical chunk date or session time, not file `mtime`
- optional explicit basis if needed later, for example `time_basis=indexed_at`
- timeline returns grouped, readable entries rather than a flat path list
- latest results are chainable into `read`

Likely landing zones:

- `src/store.rs`
- `src/main.rs`
- `src/mcp.rs`

Likely tests:

- files with fresh `mtime` but old canonical dates do not leak into event-time windows
- latest sorting is deterministic
- timeline grouping by project/session/kind works for both repo and non-repo buckets

Notes:

This directly addresses the smoke mismatch where `rank(hours=168)` could surface `2026-02-06` chunks.

#### 3. Small sharp win: make scope semantics explicit everywhere

Why it matters:

`store(project=...)` currently feels confusing because the user asks for one scope and sees output that still mentions other repos. The likely truth is that the source filter matched sessions that later segmented into multiple repo buckets, but the product does not say that clearly.

Surface should feel like:

- every human and JSON/MCP response distinguishes requested scope from resolved repo truth
- repo-scoped read/discover surfaces use exact repo slug matching by default
- any fuzzy matching is explicit, for example `project_contains`, not silently overloaded into `project`
- project filters should read as “source filter requested” or “repo scope requested,” not as ambiguous “project”
- docs should stop teaching the old store root and old mental model

Acceptance shape:

- `store --emit json` includes `requested_scope` and `resolved_projects`
- MCP store summaries use the same distinction
- `refs`, `rank`, `search`, `latest`, and `timeline` define whether `project` means exact slug or fuzzy scope, and do so consistently
- `README.md`, `docs/COMMANDS.md`, `docs/STORE_LAYOUT.md`, and `docs/ARCHITECTURE.md` consistently describe `~/.aicx`
- dashboard examples stop advertising flags the CLI does not support

Likely landing zones:

- `src/store.rs`
- `src/main.rs`
- `src/mcp.rs`
- `README.md`
- `docs/COMMANDS.md`
- `docs/STORE_LAYOUT.md`
- `docs/ARCHITECTURE.md`

Likely tests:

- JSON output shape tests for `store`
- CLI parse/docs consistency tests where they already exist

Notes:

This is the smallest sharp move with real product leverage because it reduces false confusion without waiting for deeper surface work.

### P1

#### 4. Dedup collapse for search and other discover surfaces

Why it matters:

Raw per-chunk hits are too noisy on larger stores. Users do not want five nearly identical hits from the same episode before they see the second relevant episode.

Surface should feel like:

- default `search` behavior collapses near-duplicate hits by session or bundle
- each grouped result shows representative matched lines plus `collapsed_hits`
- users can opt out with `collapse=none`

Acceptance shape:

- default search result list is breadth-first across episodes, not chunk-spam from one episode
- grouped results still expose the underlying chunk paths or refs
- the same collapse strategy is available to dashboard-server fuzzy search

Likely landing zones:

- `src/rank.rs`
- `src/mcp.rs`
- `src/dashboard_server.rs`

Likely tests:

- multiple matching chunks from the same session collapse into one group
- separate sessions do not collapse together
- `collapse=none` returns raw chunk hits

Notes:

This should be designed as a discover-surface primitive, not as a one-off tweak to MCP search only.

#### 5. CLI search parity for the discover -> read loop

Why it matters:

Today search is present in MCP and dashboard-server but missing from the main CLI. That means terminal users still have an awkward path compared with agent users.

Surface should feel like:

- CLI: `aicx search <query>`
- default human output mirrors MCP fields at a readable level
- `--emit json` gives an automation-friendly payload
- results chain cleanly into `aicx read`

Acceptance shape:

- CLI search reuses the same query semantics as MCP search
- JSON shape is intentionally close to MCP search output
- search supports project filtering and collapse options

Likely landing zones:

- `src/main.rs`
- `src/rank.rs`
- `docs/COMMANDS.md`
- `README.md`

Likely tests:

- CLI parse tests
- output-shape tests
- shared search helper tests

#### 6. Dashboard/server read-path completion and deep linking

Why it matters:

The dashboard already embeds detail text, but the server surface still has no canonical “read this chunk by ref” API. That limits deep-linking, reuse, and parity with MCP/CLI.

Surface should feel like:

- dashboard-server exposes a chunk read endpoint
- UI links can deep-link to a record or ref
- the same shared reader powers MCP, CLI, and dashboard-server

Acceptance shape:

- `GET /api/chunks/read?...` or equivalent returns metadata plus content
- dashboard list/detail views can deep-link to a specific chunk
- read endpoint does not depend on rebuilding the full dashboard

Likely landing zones:

- `src/dashboard_server.rs`
- `src/dashboard.rs`
- `src/store.rs`

Likely tests:

- endpoint returns the expected record
- invalid refs are rejected safely
- deep-link state restores the expected detail view

### P2

#### 7. Bigger structural payoff: shared query layer with canonical chunk refs

Why it matters:

Right now query semantics are spread across `src/main.rs`, `src/mcp.rs`, `src/rank.rs`, and `src/dashboard_server.rs`. That drift is already visible in the smoke: recency semantics, project filtering, and read chaining are not one coherent contract.

This is the structural payoff item because it makes the rest of the pack cheaper and more consistent, but it should still serve user-facing work rather than become an internal architecture detour.

Surface should feel like:

- discover surfaces return a canonical `ref`
- read surfaces consume the same `ref`
- one internal query layer owns project matching, time windows, grouping, and result shaping

Acceptance shape:

- introduce a shared internal model such as `ChunkRef`, `ProjectScope`, and `TimeWindow`
- `refs`, `rank`, `search`, `latest`, and `timeline` all use the same store query helpers
- time semantics and project semantics are defined once, not reimplemented per surface

Likely landing zones:

- new module, likely `src/query.rs` or `src/store_query.rs`
- `src/store.rs`
- `src/rank.rs`
- `src/mcp.rs`
- `src/main.rs`
- `src/dashboard_server.rs`

Likely tests:

- ref round-trip tests
- shared query tests for exact project scope vs fuzzy/project-contains behavior
- shared time-window tests
- integration tests covering CLI/MCP/dashboard-server parity at the helper layer

#### 8. Search recall tuning after collapse is in place

Why it matters:

The smoke report includes at least one obvious false zero. Current search requires normalized AND matching across all query terms, which is too brittle for many real retrospective queries.

Surface should feel like:

- search supports a softer retrieval mode without becoming semantic search
- users can understand why a result matched

Acceptance shape:

- add `match_mode` such as `all`, `soft`, or `phrase`
- return `matched_terms` and optionally `missed_terms`
- default behavior improves recall without flooding low-signal junk

Likely landing zones:

- `src/rank.rs`
- `src/mcp.rs`
- `src/dashboard_server.rs`
- `src/main.rs` if CLI search lands

Likely tests:

- formerly zero-result cases match under soft mode
- precision remains acceptable for strict/all mode
- term accounting is stable

## Recommended Sprint Order

1. P0.3 scope semantics cleanup
2. P0.1 direct read/open primitive
3. P0.2 latest/timeline with honest time semantics
4. P1.4 dedup collapse
5. P1.5 CLI search parity
6. P2.7 shared query layer if the team wants to finish the sprint by removing duplicated semantics instead of leaving three partly-diverged surfaces

## Issue-Ready Breakdown

If this pack is converted into sprint tickets, the first issue set should be:

1. Add direct chunk read/open to MCP and CLI
2. Add latest/timeline helpers using canonical event-time semantics
3. Clarify requested scope vs resolved repo truth across `store` outputs and docs
4. Collapse duplicate-ish search hits by session/bundle
5. Add CLI search parity with MCP search

## Product Thesis Check

This pack keeps AICX in the right lane:

- not a heavy memory backpack
- not a file dump
- a history index with fast selective re-entry

The next sprint should make the answer to “what was I just working on, and let me open it” feel first-class.
