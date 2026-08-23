# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
## [Unreleased]

## [0.12.3] - 2026-08-19

### Added

- **`~/.aicx/.aicxignore` keeps listed checkouts out of search memory.**
  Write a full path or `~/…`; a directory covers every nested repo under
  it. Catalog still admits the session. Index drops only the frames whose
  `cwd` (the same per-turn path multi-root sessions already bucket by
  project) sits under a listed prefix. Rule changes invalidate `CURRENT`
  and cached extracts; unreadable or unsupported checkout rules fail closed.

- **MCP grew the missing session chain.** Agents can now list sessions
  (`aicx_sessions`), open one session and extract its conversation
  (`aicx_session`), and pull a continuity pack (`aicx_continuity`) without
  shelling out to the CLI. Project filters stay exact; empty `-p` is a
  numbered project miss, not `scanned=0`; ambiguous ids fail closed. The
  continuity pack is context only — it does not select native resume.

- **`aicx sessions list -p` is the same exact identity filter as MCP.**
  `/repo`, `owner/repo`, `owner/`, and a unique bare name all work. A
  resolved project with zero hits prints `scanned=N; matched=0` on
  stderr instead of a silent `[]`.

- **The Codex adapter understands the shape Codex has been writing since
  2026-07-11.** Five record types appeared that day and the adapter had
  never been taught any of them; two more had simply been renamed upstream.
  In the last 40 sessions that is 6 182 records the parser could not place —
  and the sessions are not rare: 840 of 871 July rollouts and 321 of 321
  August ones carry them. Now handled:
  - `event_msg/sub_agent_activity` (1 599) — Codex fans out to subagents,
    and a session that dispatched a dozen of them read as one agent working
    alone. Recorded as `subagent <kind>: <agent_path>`.
  - `event_msg/patch_apply_end` (2 836) — the record of what Codex actually
    changed on disk. Recorded as `patch apply ok|failed: <paths>` plus the
    first stderr line; the change bodies stay out of the transcript.
  - `response_item/agent_message` (177) — a third chat envelope carrying
    agent-to-agent dispatch (`author → recipient`), outside the existing
    dual-envelope suppression, so it was the one copy of that payload.
  - `event_msg/thread_goal_updated` (31) — carries the goal objective in
    prose, which is operator intent worth retrieving.
  - `event_msg/turn_aborted` (20) — an interrupted turn is operator
    behaviour, not noise.
  - `event_msg/mcp_tool_call_end` (299) and `event_msg/web_search_end` (22)
    are the current spellings of `mcp_tool_call_response` and
    `web_search_complete`; both spellings stay accepted.
  - `world_state` (875) and `inter_agent_communication_metadata` (177) are
    session state, not conversation. They are now classified as known and
    deliberately dropped, so they stop counting as visible coverage loss
    the way a genuinely unrecognized payload should.

### Fixed

- **Hybrid index publish no longer hoards every generation on disk.**
  `CURRENT` flip used to leave the previous `generations/<id>/` forever, so a
  5-minute MCP auto-refresh stacked hundreds of ~1G Tantivy copies (218G on
  one operator home). Publish now keeps the live generation plus one previous
  rollback snapshot and deletes the rest after the pointer is durable.

- **Full install no longer leaves MCP clients on stdio while the LaunchAgent
  speaks HTTP.** `configure_mcp` used to write `{"command": "…/aicx-mcp",
  "args": []}` *before* `install-mcp-service.sh` created
  `io.vetcoders.aicx.mcp` on `http://127.0.0.1:8044/mcp`. Darwin installs now
  start the service first and write `{"url": "http://<host>:<port>/mcp"}`
  (wildcard bind becomes `127.0.0.1`; host/port come from the plist). A
  readable `auth-token` is added as a Bearer header because `aicx serve`
  requires auth by default. `AICX_SKIP_MCP_SERVICE=1` keeps stdio.
  `make install-service` wires the same client URL.

- **Grok sessions now belong to a checkout.** Live layout is
  `~/.grok/sessions/<percent-encoded-cwd>/<uuid>/chat_history.jsonl`.
  Discovery had reused the Codex rollout reader, so `repo_path` stayed
  empty, `--cwd` and `-p /aicx` dropped every Grok session, and sibling
  `events.jsonl` files looked like extra sessions. The list now reads
  native `user`/`assistant` lines, infers cwd from the encoded parent,
  and treats only `chat_history.jsonl` as the session.

- **`make install` no longer re-extracts the entire agent history.** Step 4
  used to run `aicx all -H 10000` after every binary refresh, so a
  developer reinstall dumped hundreds of `[skip]` lines (Claude journals,
  Fatal-completeness sessions, hosted archives) and could rewrite a live
  catalog. Extract is now opt-in (`--extract` / `AICX_INSTALL_EXTRACT=1`,
  default window 24 hours). The existing `~/.aicx` catalog is left alone.

- **MCP LaunchAgent install no longer dies on launchctl Error 5.**
  `launchctl bootstrap gui/<uid>` only works from an Aqua login. Agent
  terminals and SSH returned `Bootstrap failed: 5: Input/output error` plus
  a "retry as root" hint, and `set -e` aborted the script before the
  already-written plist could be reported. The installer now detects a
  non-Aqua session, keeps the plist, and prints the GUI-session load
  command. Do not run the per-user agent as root.

- **Extracts are recorded verbatim instead of HTML-entity encoded.** Every
  message body went through an HTML escape on its way into the Markdown
  artifact, so a transcript reached its readers with `&#39;` for every
  apostrophe, `&quot;` for every quote, and `&lt;`/`&gt;` around every tag —
  1 340 such artifacts in one Codex session extract, 300+ in a Claude one.
  The escape was also redundant and actively wrong: the dashboard's own
  renderer (`AicxMarkdown` in `dashboard_inline_markdown.js`) escapes
  `& < >` per line before inlining, so a `<` in a session was displayed as
  `&amp;lt;`. Escaping now happens only at the rendering edge, where it
  belongs; the extract keeps what was actually said. Verified on the same
  Codex session: 1 340 artifacts → 0, and `<user_shell_command>` blocks read
  as themselves.
- **The extract header stopped describing a query nobody made.** It labelled
  the resolved project identity as `Filter` and the age of the oldest entry
  as `Period | last N hours`, so a single-session extract read as a
  time-windowed search narrowed to one repository. They are now `Project`
  and `Oldest entry | N h ago`.

- **`aicx catalog refresh` reached a fixed point instead of rewriting the
  whole census on every call.** The hot delta marked a session changed when
  its cataloged `project` differed from the freshly scanned one — but those
  two values come from different pipelines: the scan guesses identity from
  the directory layout (`vibecrafted-suite/vc-slack-agent`), while the
  catalog holds what `git remote get-url origin` resolved
  (`vetcoders/vc-slack`). Reattribution then rewrote the scanned guess back
  to the origin slug, so the next call found the same difference again. On
  the owner host that meant 272 phantom "changed" rows and a full rewrite of
  a 13 119-line catalog on every `continuity`, `dashboard`, and wizard call,
  forever. The delta now compares post-reattribution values on both sides;
  measured on a copy of the live census, the churn drops from 272 to 0 and
  consecutive refreshes converge.
- **Four red parser tests left behind by the org-identity fixture sweep.**
  The sweep rewrote expectations that read as string literals but could not
  see fixtures assembled from separate `.join()` components, so a segment
  built under `hosted/Vetcoders/loctree` was asserted to be
  `Loctree/loctree`, and a path lowercased to `/Git/vetcoders/…` was still
  asserted to yield `Vetcoders`. Each test is now internally consistent
  again, with the fixture directories carrying the true org.
- **`tests/fixtures/overlay-intent-v1/` is a true mirror again.** The
  directory is contract-bound (C0-01) to be byte-identical with
  `loctree-suite/docs/contracts/fixtures/overlay-intent-v1/`, and an
  aicx-owned fixture on a different schema
  (`aicx.overlay.semantic-fixture.v1`) had been parked inside it, failing
  `sync_fixtures.py --check`. It moved to `tests/fixtures/overlay-semantic-v1/`
  — it is live test input, `include_str!`-ed by the semantic dedup suite in
  `src/overlay.rs`, which now reads it from its own directory. The mirror
  verifies again: 10 fixtures byte-identical in both repos.

### Changed

- **A hot refresh no longer re-reads sessions the census already knows.**
  `scan_hot_window` opened a bounded header for every source inside the
  window — thousands of files per call — to re-derive identity that was
  already on disk, byte for byte. Candidates whose `(path, len, mtime)`
  fingerprint matches the catalog are now skipped before the read.
  Measured on the live census (13 119 sessions): `catalog refresh --hours
  720` went **11.4s → 2.6s**, and the cost stopped scaling with the window
  (a 24h and a 720h refresh now cost the same). End to end,
  `continuity show -p /aicx -H 720` went **30.5s → 14.9s**. The trade: path-derived
  fields of an untouched source are not re-derived when the derivation
  itself improves — `aicx catalog rebuild` remains the pass that does.
- **`origin` resolution is memoized across calls.** Reattribution spawned
  one `git remote get-url origin` per checkout on every sweep — ~500 on the
  owner host, twice per refresh. The answers now live in
  `<home>/catalog/remotes.json`, keyed by the checkout's git-metadata mtime,
  so a re-pointed remote is still picked up on the next refresh without
  paying a subprocess per row. Any path that resolves identities maintains
  the memo, including `continuity show --no-refresh` — it writes that one
  cache file and nothing else. Checkouts with a relative `cwd` are no longer
  attributed at all: their answer was an accident of the invoking directory.

- **`aicx intents` reads the committed index instead of re-parsing every
  transcript.** The lexical index already stores each session's canonical
  extract verbatim (`ChunkRef.text`) next to its resolved identity —
  project, agent, date, session id, cwd — which is exactly what intent
  extraction was rebuilding from scratch on every invocation. It now
  reads those documents back through the new
  `TantivyAdapter::scan_chunks` surface, the body-carrying sibling of
  `scan_metadata`. Measured on the same query (`-p /vibecrafted --sort
  newest -H 480`, 2492 matching sessions / 8.24 GB of source JSONL):
  **5m27s → 21.8s**, returning the same 80 records as the census walk. The census walk remains the fallback for hot-window
  requests (`--live`, windows ≤ 48h) and for machines with no published
  `CURRENT`, so freshly written sessions are never silently dropped.
  Records served this way are stamped `identity_source: index-v1`.
  Full-history requests (`hours == 0`) also stay on the census: that is the
  durable-identity join `aicx overlay` performs, and it freezes `intent1:`
  evidence refs, so its input set must not move under it. Verified: overlay
  revision is byte-identical before and after this change.
  Frames are still narrowed to the requested `--frame-kind` before
  classification, so serving whole extracts does not quietly promote
  assistant prose to operator intent.

## [0.12.2] - 2026-08-13

### Fixed

- **Continuity is no longer blind to checkouts whose path spells the
  project differently than its identity.** Three stacked fixes: Claude
  session `cwd` is now sniffed from the session head instead of decoded
  from the lossy `~/.claude/projects/<slug>` directory name (every `-`
  inside a real path component used to become a bogus `/`, fabricating
  identities like `suite/vibecrafted` and cwds that do not exist);
  per-frame retention accepts frames whose cwd is the session checkout
  (or a subdirectory) so remote-derived identities survive suite dirs and
  renamed clones while the strict adjacent-segment anti-leak proof still
  guards frames that left the checkout; and the continuity INDEX HEALTH
  footer reports the bucket with the newest session activity across the
  expanded project filters instead of an arbitrary first bucket.

- **Continuity live window is mtime-in-window, not newer-than-census.**
  A `catalog rebuild` that stamps every fingerprint current no longer
  demotes today's open sessions to canonical-only, so NOW/PEERS stay
  populated. INDEX HEALTH warns when `pending_chunks` or
  `sessions_newer_than_chunks` is non-zero instead of rendering an
  unexplained empty pack. `aicx catalog rebuild --with-chunks` drains
  the lag through `aicx index` and always prints `pending_chunks`.
  `aicx doctor` now measures `continuity_freshness` (hot sources vs
  pending lag) so Overall Green cannot ignore a blind resume surface.
  Catalog admission case-folds project slugs
  (`VetCoders/vibecrafted` → `vetcoders/vibecrafted`).

- **Lexical schema downgrade is a controlled conflict, not silent
  corruption.** Publishing `CURRENT` refuses a generation whose lexical
  schema is provably older than the published one (or than the binary's
  supported schema), and search returns a typed conflict naming the foreign
  writer when `CURRENT` drifts from the binary — the failure mode observed
  when a file-sync tool (MEGA) replicated the machine-local
  `~/.aicx/indexed/` between hosts running different aicx builds
  (`v3_folded_dictation` ↔ `v2_fast_body`). `docs/MULTI_MACHINE.md` documents
  which AICX home paths are machine-local and must stay out of file sync.

- **`unsupported_visible_event` means something again on Claude sessions.**
  Since 2026-07 the harness emits thinking blocks signature-only — `thinking`
  is present but empty and the reasoning text never reaches the JSONL (live
  evidence: 400 of 400 blocks in one session). The adapter treated an empty
  body as an unrecognized shape, so it raised the boundary flag and a typed
  warning on every single one; any session that reasoned at all reported
  itself as carrying unsupported visible events. A known block with no body is
  now consumed silently, exactly like an empty text block. A block with a body
  is unchanged (`internal_thought`), and a block missing the field entirely is
  still an unsupported shape.

- **Modern Claude sessions no longer degrade their own parse status.** The
  harness moved file-history tracking from whole snapshots to per-message
  `file-history-delta` records, which the taxonomy did not declare. Every one
  of them terminated as `skipped(unknown_payload_type)` with a warning and
  raised `unsupported_visible_event` — a healthy session flagged as carrying
  unsupported visible events, purely from its own bookkeeping (48 such records
  in one observed session). The record is verified to be rewind/backup
  bookkeeping with no conversation content, so it is now recognized as
  `metadata_record` alongside `file-history-snapshot`.

- **Claude extracts no longer lose the messages you send mid-turn.** A message
  submitted while a turn is running lives in the session JSONL only as a
  `queue-operation` enqueue record; the Claude adapter consumed that record as
  `metadata_record` and threw the body away, so the text was unrecoverable for
  every downstream consumer (`--conversation`, chunks, intents). The record is
  still consumed as `metadata_record` — taxonomy and the frozen oracle fixture
  are unchanged — but its `enqueue` text is now projected as an ordinary
  `user` turn carrying the record's own timestamp and evidence. Two dedupe
  layers keep it honest: re-submitting the same text keeps the first enqueue,
  and a queued message the harness later delivers as a real `user` row loses
  to that row. Verified on a live 1180-entry session that had permanently
  dropped 20 operator messages. See `docs/PARSER_NORMATIVE_CONTRACT.md` §2.1.

### Added

- **Index generation provenance.** Retrieval manifests now record
  `writer_version` and `build_id` (`<version>+g<sha>[.dirty]`) of the writer
  that published the generation. Pre-provenance manifests stay readable and
  self-describe as `unknown (pre-provenance writer)`.

## [0.12.1] - 2026-07-24

### Fixed

- **npm cold installs now survive standard dependency hoisting and concurrent
  lifecycle scripts.** The wrapper resolves platform packages through Node's
  module resolver and waits, with a hard deadline, for the platform downloader
  to publish both binaries before validation.
- **macOS release checksum sidecars are portable.** Native signed/notarized
  bundles now record the archive basename rather than a self-hosted runner's
  absolute `dist` path.

## [0.12.0] - 2026-07-24

### Removed

- **`aicx store` and the per-frame card store concept.** The command exits
  with an argument-parse error, `src/store.rs` and the card-mill write path
  are gone, and no production namespace called `store` remains. Live memory
  truth is now `catalog rebuild → extract (optional cache) → index → CURRENT`
  under a single AICX home (`src/aicx_home.rs`). Existing on-disk card trees
  are handled read-only by `src/legacy_archive/**` (doctor, quarantine,
  migrate) and can neither regrow nor masquerade as the runtime store.
  `docs/STORE_LAYOUT.md` was replaced by `docs/AICX_HOME_LAYOUT.md`.

### Changed

- **`aicx intents` reads the durable catalog and allowlisted session sources
  directly.** Project resolution no longer requires the retired card archive;
  oracle JSON reports `backend=catalog_sources` with source-read accounting so
  skipped or unreadable sources cannot look like a complete result. The legacy
  archive is consulted only when the durable catalog itself is absent.

- **Product verification walk (W5-01).** Architecture and store-layout docs
  aligned to code truth (hybrid generations, typed retrieval status, config
  inspect, doctor/health split). `docs/EMBEDDINGS.md` records OUTCOME 1 and
  labels the shell harness wall times as reference-model latency, never engine
  latency. Isolated multi-agent fixture walk (extract → store → search/intents
  → health → MCP) is evidence-only; no live `~/.aicx` mutation.
- **Mmap exact-scan hot loop profiled and repaired — OUTCOME 1 (W4-01c).**
  Profiling the release-mode `MmapDenseAdapter` at the same isolated
  500000×1024 scale showed the Rust scan was never the latency culprit: warm
  global query 0.578 s before / 0.371-0.468 s after the repair, project-scoped
  0.241 s / 0.212-0.221 s — both far inside the frozen budgets (8 s global,
  2 s project). The 317.87 s / 55.00 s recorded on 2026-07-22 measures the
  pure-Python reference scan inside `tools/bench_dense_migration.sh`
  (`run_mmap` struct-unpacks and dot-products every row in the interpreter);
  the harness never executes the Rust adapter, debug or release. Hot-loop
  repairs, with every contract preserved and scores bit-identical to the
  brute-force leg: unfiltered scans skip row-metadata decoding entirely
  (dropping a ~0.16 s/query serde_json + string-allocation tax at 500k rows),
  filtered scans deserialize only the `metadata` object, the query
  self-product is hoisted out of the scan, and scoring is
  distance-specialized and branch-free with fail-closed non-finite
  diagnostics moved off the hot path. The unmodified harness re-run at the
  same scale reproduced its Python-scan figures (321.96 s / 55.37 s, with its
  own pure-Python "legacy" scan at 37.9 s global) while parity 1.0, disk
  ratio 0.0971, and failed-copy safety all held. Verdict: the mmap exact scan
  stands; the Lance/memex-search contingency stays shelved.
- **Dense migration benchmark gate.** Added `tools/bench_dense_migration.sh`
  to build an isolated AICX_HOME-shaped corpus, compare legacy duplicate dense
  NDJSON against the mmap payload, verify failed-copy `CURRENT` safety, reverse
  query-order parity, top-k parity, disk ratio, and latency/RSS budget accounting
  without mutating live `~/.aicx`. The first 2026-07-22 ≥2 GiB isolated run
  recorded 317.87 s global / 55.00 s project as **reference-model (pure-Python)
  latency** inside the harness's `run_mmap()` — not the Rust engine. W4-01c
  reclassified that measurement and proved the release-mode `MmapDenseAdapter`
  meets the frozen budgets (see OUTCOME 1 above). The harness remains the CI
  contract for layout/parity/failed-copy; treat its wall times as reference
  model only until a follow-up drives the real binary.
- **Batched semantic-index embedding.** `aicx index` now embeds chunks in
  batches through the existing `embed_batch` API instead of one HTTP
  round-trip per chunk. For the cloud backend this collapses the dominant
  per-chunk latency of a full build (one OpenAI-compatible POST carries an
  `input` array of up to `batch_size` texts). Batch size is configurable
  via `[embedder.cloud] batch_size` (default 16) with an `AICX_EMBED_BATCH`
  env override; GGUF stays serial (`batch_size` 1) by default and opts in
  only via the env var. A failing batch retries once, then degrades to
  per-item embedding so one poison chunk cannot abort the run. Checkpoint,
  resume, `--sample N`, and per-chunk progress ticks are unchanged.

## [0.11.0] - 2026-07-13

Parser engine transplant — "Noc francuskiego łącznika". Full session-engine
swap executed overnight by a five-agent fleet (codex, claude, grok, junie, agy)
on a single Living Tree checkout; Transcript Builder served as differential
oracle, never a runtime dependency.

### Added

- Deterministic parser kernel (`crates/aicx-parser`): normative contract
  (`PARSER_ENGINE_CONTRACT.md` + machine-truth `normative_fields.toml`),
  visible-completeness + boundary flags instead of donor `parse_status`,
  typed `UsageEvent`, stable `evidence_event_id` identity.
- Bounded drift-aware `SessionCatalog` — locate-before-parse; session
  discovery is `O(one session)` instead of `O(entire store)`.
- Five per-agent adapters (Codex, Claude, Gemini, Grok, Junie), each built by
  its own agent, each emitting `parser_oracle.envelope.v1` verified by the
  differential oracle harness (`tests/parser_oracle/`).
- Canonical store projection with `store_revision`.

### Changed

- **BREAKING**: canonical CLI is now `aicx extract <agent> …` — the
  `--agent`/`--format` flags are removed.
- **BREAKING**: all session providers cut over to the new engine; legacy
  session engine removed (−9200 lines, fail-closed boundary).
- 52.8 MB session parse: **75.86 s → 0.466 s** (~160×).

### Fixed

- Dual-channel test isolation: subprocess tests isolate `HOME`/`USERPROFILE`
  so discovery never scans the operator's live `~/.claude` tree.

## [0.10.0] - 2026-07-05

### Added

- **Card schema v2** for the canonical store: versioned sidecars
  (`schema_version: 2`), YAML `card.v2` frontmatter replacing the legacy
  bracket header, an L0 provenance pointer (`source { path, sha256, span }`
  to the raw session file), and claim-honesty metadata
  (`claim_scope=session_close`, `freshness_contract=historical`,
  `verification_state=not_verified_by_aicx`) on every new card. Contract:
  `docs/CARD_CONTRACT.md`.
- **Typed signals**: `ChunkSignals` now serialize as structured
  `signals[]` records (`kind`, `text`, `line_span`, `extractor_version`) in
  the sidecar; the md `[signals]` block is a deterministic render of those
  records instead of the only artifact.
- `aicx corpus validate-cards [ROOT] [--strict] [--json]` — card contract
  gate: schema/versioning checks, full-file `content_sha256` verification,
  header-form consistency, placeholder ban, harness-noise heuristic, and
  md↔sidecar signal parity, with a born-v2 vs migrated-v2 severity policy.
- `aicx migrate --cards-v2 [ROOT] [--apply]` — in-place v1→v2 store
  migration: dry-run by default, streaming walk, per-file manifest with the
  old header preserved for reversibility, body-byte invariance enforced by
  a hard sha256 pre/post check, `migrated_from_schema: 1` marker, and
  refreshed `content_sha256` after the header rewrite.
- **Evidence mode** (`aicx search --evidence`, MCP `aicx_search`
  `evidence: true`): evidence packets with answer/support re-ranking,
  verified source paths, and oracle-status envelopes.
- **Search quality**: TOML-seeded quality eval harness
  (`aicx eval search-quality`), anchored-answer preference, content-first
  excerpts, scoped-fallback and project-bucket fixes, and a lighter Polish
  stemming profile in the Tantivy adapter.
- **Intent taxonomy** extended with Task & Commitment kinds; every
  `[signals]`-sourced record now carries provenance tags and is revalidated
  through the shared classifier (document-role awareness skips pasted
  commit/changelog blocks; code/log fragments are dropped).
- **Claim-honesty frame on display surfaces**: `aicx intents` (text + JSON)
  and MCP `aicx_intents`/`aicx_search` payloads label claims as
  `historical @ session close · not verified by aicx`.
- **CLI/MCP search parity**: shared `fuzzy_search_with_post_filters` +
  `finalize_fuzzy_results` so ordering/limit semantics are identical across
  surfaces; end-to-end parity test.
- **MCP host contract**: `--host`, `--allowed-host` (repeatable),
  `--allow-any-host`, HTTP `Host`-header validation with an explicit
  trust policy, Bearer-auth token cascade documentation, and
  `aicx doctor` MCP version-pair diagnostics.
- Operator-markdown imports carry structural provenance
  (`source_file`/`source_format`/content-hash `import_id`); ChatGPT exports
  are dated from their `Created` header instead of file mtime.

### Changed

- Card readers are header-agnostic (bracket v1 or frontmatter v2) through a
  single shared `card_header` helper, and prefer sidecar metadata over
  re-parsing the md header.
- Repository deprivatized for public release: personal names, contact
  addresses, internal infra references, and internal planning docs removed;
  npm/crate author metadata now `Vetcoders <hello@vetcoders.io>`.
- GitHub Actions workflows pin every action to a full commit SHA
  (supply-chain hardening; semgrep `github-actions-mutable-action-tag`
  gate is clean).

### Fixed

- MCP HTTP security posture: non-loopback binds refuse to start without
  auth; loopback-only `--no-require-auth`; a bare all-interfaces bind
  without `--allowed-host` disables Host validation explicitly (tailnet
  flow) while staying Bearer-gated.
- CLI pre-parse hints (`--source` requirement, `config --show` hint) fire
  only on the top-level subcommand instead of matching anywhere in argv.
- `~/`-prefixed frontmatter `cwd` values expand with native path separators
  on every platform (fixes windows-latest CI on operator-md ingest).
- Search-seed project discovery paths guarded in the eval harness.

## [0.9.4] - 2026-06-20

### Added

- Windows (`x86_64-pc-windows-msvc`) is now a first-class, prebuilt release
  target: native file locking (`LockFileEx` shared/exclusive byte-range),
  process-liveness checks, and DACL-restricted auth-token persistence. The
  release pipeline builds a signed, GPG-detached Windows `.zip` alongside the
  notarized macOS and GPG-detached Linux bundles, and ships a
  `@loctree/aicx-win32-x64-gnu` npm platform package.

### Fixed

- Path handling across the Windows boundary: canonical chunk refs, config /
  lookup / manifest paths, and the reports lane filter are normalized to
  forward slash so cross-OS comparisons match; the `\\?\` verbatim prefix is
  stripped at the single `canonicalize` source so validated paths compare
  cleanly (gemini step entries, ignore-matcher bases) instead of leaking the
  verbatim form into messages and keys.
- Traversal guard now catches a bare `..` segment under a Windows verbatim
  prefix and on both path separators, closing a guard bypass.
- Migration extracts Windows drive-letter source paths (`C:\…\rollout.jsonl`)
  from legacy bundles, so rebuilds are not silently downgraded to salvage on
  Windows runners.
- `distribution/npm/sync-version.mjs` now includes the `win32-x64-gnu`
  platform package, so the Windows manifest no longer drifts out of the
  release-channel version check.
- Windows `extern` block marked `unsafe` for Rust edition 2024.

## [0.9.3] - 2026-06-12

### Added

- `aicx index status`: truthful sessions→chunks freshness — new fields
  `source_sessions`, `newest_session_updated_at`, `sessions_newer_than_chunks`,
  `sessions_without_timestamps`, `chunking_lag_secs`; readiness now reports
  `stale_chunks`/`stale_index` instead of a false clean `ready` when source
  sessions are newer than canonical chunks. MCP `IndexStatus` carries the
  same fields.
- `aicx index`: canonical catch-up stage — when chunking lag exists, source
  sessions are materialized into the canonical store (cutoff derived from the
  oldest lagging `newest_chunk_mtime`) before semantic indexing; skipped
  entirely when no lag exists.
- `aicx intents`: voice-transcript provenance — `<codescribe>`-tagged
  transcriptions get `source: voice_transcript`, a `[voice]` timeline marker,
  and sort below typed intents; deterministic garble gate drops incoherent
  voice-only intents that carry neither WHY nor EVIDENCE.
- `aicx intents`: `--unresolved-mode <session|intent>` — intent-level
  closure matching (keyword-overlap join) in addition to the session-level
  default; empty results under the default mode now print a hint instead of
  a bare false-empty.
- `aicx intents`: `--min-confidence <1..5>` exposes the structural confidence
  threshold; `--strict` now maps to confidence ≥4 and measurably cuts
  low-confidence noise.
- Intents epistemic spine (lanes 3–5): `audit_claims_against_evidence`
  (EvidenceRecord/EvidenceKind), `detect_contract_fractures` (contradicted /
  unsupported-high-risk / orphaned-intent taxonomy), and `generate_clarify`
  (deterministic, capped, priority-ordered clarify questions).
- Typed `ChunkRefSpec` resolver in the store: `aicx read` accepts
  `chunk:<hex-id>` (8-hex SHA-256 prefix of the canonical chunk path), bare
  hex ids, absolute/store-relative paths, and legacy compact refs through one
  shared resolver (CLI + MCP); unknown ids fail with a query-bearing error,
  ambiguous prefixes list candidates.

### Fixed

- CLI no longer panics with `failed printing to stdout: Broken pipe` when
  output is piped into `head`/`less` on Unix — SIGPIPE default disposition is
  restored at process start (regression-tested).
- Mutation warning and installer messages print the *resolved* AICX home
  (bootstrap `[storage].home` / `AICX_HOME`) instead of a hardcoded
  `~/.aicx`; installer output distinguishes `config:` from `storage root:`
  when the two diverge.
- `aicx-parser`: segment-kind scoring no longer overflows on report-heavy
  sessions (score accumulation widened u8→u16; regression covered with a
  300-entry fixture).

## [0.9.2] - 2026-06-11

### Added

- `[storage].home` bootstrap config: `$HOME/.aicx/config.toml` can pin the
  AICX home directory (`AICX_HOME` env still wins). Value is validated:
  absolute path or `~/...` only, no `..` traversal, no control characters;
  the config read goes through the size-capped validated reader.
- `aicx intents`: supersession winner is promoted to active state and the
  loser stamped with `superseded_by` (chain-based `detect_supersedes`).
- `aicx search`: automatic filesystem-fuzzy fallback when semantic search
  is unavailable; `--no-semantic` still forces the fuzzy path explicitly.

### Changed

- Workspace version sync: all internal crates (`aicx-parser`,
  `aicx-embeddings`, `aicx-retrieve`, `aicx-progress-contracts`,
  `aicx-monitor`) now version-track the main `aicx` crate and are published
  to crates.io alongside it, so `aicx` is consumable as a library
  dependency (Loctree consumer path).
- `src/doctor.rs` decomposed from a 2602-line monolith into
  `doctor/{types,checks,cleanup,quarantine,report}` behind a re-exporting
  facade; public API unchanged. Stale never-compiled orphan modules
  (`doctor/checks.rs` old copy, `sources/shared` Faza-1 placeholders)
  removed.
- `toml` promoted from dev-dependency to runtime dependency (bootstrap
  config parsing).

### Fixed

- `aicx intents`: legacy chunks without a sidecar (or with a sidecar written
  before `frame_kind` existed) are classified into the default `user_msg`
  lane instead of being silently dropped, so intent extraction no longer
  returns empty on stores created before the field was introduced.
- Bootstrap `[storage].home` validation is consistent across every consumer
  (runtime resolver, the `aicx-embeddings` config mirror, and `install.sh`):
  control characters and parent-directory (`..`) traversal are rejected
  wherever the value is read, so one component cannot resolve a home another
  refuses to start on.
- Tainted-path hardening on the bootstrap config read (size-capped, validated
  reader instead of a raw read).
- macOS release signing: the temporary signing keychain is set as the default
  before `codesign`, so a non-interactive CI runner session resolves the
  signing identity by name (previously failed with "no identity found"
  despite a successful certificate import).
- Windows release bundle: build the gnu target under Git Bash with the
  mingw-w64 linker, skip the protoc step the slim bundle does not need, and
  stop overwriting `PATH`; the binaries-only bundler no longer refuses Windows
  targets.

### Internal

- Release tooling (`tools/release_sync.py`, `make release-prepare`) syncs and
  validates every workspace crate manifest and internal dependency
  requirement, so a version bump cannot silently desync the workspace.
- The pre-push gate delegates to the Makefile gate targets and only runs the
  full Rust suite (clippy + tests) when Rust or Cargo files actually change;
  Semgrep is a required, non-optional gate (semgrep / `uvx` / `pipx`).


## [0.9.1] - 2026-05-26

### Added

- `aicx::cli::failure::StructuredFailure` module — canonical failure-as-state
  pattern with `kind` / `reason` / `recommendation` / `fallback` fields,
  rendered as a multi-line text block at the CLI boundary in text mode or as
  a `{ok: false, error, kind, reason, recommendation, fallback}` JSON envelope
  in `--json` mode. The pattern was already shipped in `aicx search`
  semantic-down failures and `aicx steer` feature-gate errors; this release
  promotes it into a shared module consumed by `aicx ingest`, `aicx
  conversations`, `aicx extract`, `aicx sources`, `aicx doctor`, and
  `aicx config`.
- Non-blocking mutation warning on bare no-arg invocations of `aicx all`,
  `aicx claude`, `aicx codex`, `aicx store`, `aicx migrate`,
  `aicx migrate-intent-schema`, and `aicx index`. Emits a single-line note
  to stderr, then waits 3 seconds before starting the mutation so an
  operator who invoked accidentally can `Ctrl-C` to abort. Scripted callers
  (`vc-init`, `vibecrafted-mcp`, `install.sh`) suppress the warning entirely
  with `AICX_NO_MUTATION_WARN=1`. Delay is overridable via
  `AICX_MUTATION_WARN_DELAY_SECONDS`.
- `aicx conversations --dry-run` is now dual-channel: a JSON envelope is
  emitted on stdout with `agent`, `by_agent`, `by_kind`, `dry_run`,
  `filters_applied`, `messages_total`, `output_dir`, and
  `sessions_discovered` keys, while the existing human-readable summary is
  preserved under a `=== Conversations Dry-Run ===` banner on stderr.
  Mirrors the `aicx migrate-intent-schema --dry-run` gold-standard pattern.
  Pipe consumers can now `aicx conversations --dry-run | jq .` cleanly.
- Help text bodies for the shared retrieval grammar flags `--score`,
  `--agent`, `--since`, `--until`, and `--frame-kind` across `aicx search`,
  `aicx steer`, `aicx intents`, and `aicx tail` — these previously had
  empty help bodies because the shared filter struct was never decorated.
- Structured-failure hint on `aicx config --show` flag mistake — emits the
  canonical `kind: flag_not_recognized` block with a `recommendation: use
  the subcommand form: aicx config show` and a `fallback: aicx config show`
  suggestion.

### Changed

- `aicx doctor` now has an operator cleanup flow: default TTY runs use an
  interactive multi-select + dry-run/apply gate, `--force --yes --format json`
  emits machine-readable cleanup phases, and empty-body quarantine writes a
  restore manifest consumed by `--restore-quarantine <slug>`.
- `aicx index status --json` now always emits an array of
  `{project, status}` objects, including the default `_all` scope, so
  machine consumers no longer need to handle a single-scope object shape.
- `aicx search --limit` now fails above the explicit 10,000 result cap
  instead of allowing unbounded candidate-pool expansion, and the explicit
  fuzzy fallback uses the same filter examined-pool ratio as semantic search.
- `aicx doctor --fix` renamed to `aicx doctor --rebuild-steer-index` so the
  flag matches what it actually does (rebuild the steer index from the
  canonical store — it does not orchestrate the broader remediations
  recommended by the report). The old `--fix` flag is preserved as a
  deprecation alias and emits `aicx doctor: warning: '--fix' is deprecated;
  use '--rebuild-steer-index'. The old flag will be removed in v1.0.` Old
  shell scripts continue to work unchanged.
- CLI-boundary failure surfaces for `aicx ingest`, `aicx conversations`,
  `aicx extract`, and `aicx sources` no-arg invocations are now wrapped in
  the canonical `kind: missing_required_arg` block with a concrete
  `recommendation` and a runnable `fallback` command, replacing the bare
  Clap-default `error: the following required arguments...` and bare
  anyhow chains.
- `aicx config show` sentinel for missing optional values changed from
  `<unset>` to canonical `<none>` so it aligns with the `aicx index status`
  baseline. JSON output continues to emit `null`.
- `aicx state --info` now honors the `--project` filter (previously only
  honored when `--reset` was set). When the filter is applied, the output
  carries a `Filtered by project: <owner>/<repo>` banner; totals show
  `(filtered)` suffix. Filter supports the same four shapes as the rest of
  the suite: `owner/repo`, `owner/`, `/repo`, and bare `name`.
- `aicx tail --help` description now reads `"Print recent intents/chunks
  (snapshot mode); add --follow to stream new arrivals"` instead of only
  documenting the follow-mode behavior. Snapshot mode is the no-arg default.
- `aicx steer --help` and `aicx steer` in the top-level help carry a
  `(requires --features lance)` annotation so operators can see at a glance
  that the subcommand is feature-gated and currently unavailable in slim
  builds. Invocations still emit the existing structured fallback pointing
  at `aicx search`.
- `aicx all`, `aicx claude`, `aicx codex`, and `aicx store` description
  strings no longer end with the internal architecture suffix `(layer 1)`.
- `aicx doctor --oracle` output documents its verdict mapping in `--help`:
  `ready` corresponds to `Green`, `degraded` to `Warning`, and
  `unsafe_for_loctree_scope` to `Critical`. Output style remains distinct
  from the standard severity-bracketed report and is suitable for
  short-form readiness probes.

### Fixed

- `aicx doctor --prune-empty-bodies` no longer hard-crashes with a bare
  anyhow chain when encountering the first empty-body chunk that lives in
  `~/.aicx/non-repository-contexts/` or any other canonical root outside
  `~/.aicx/store/`. The store-root prefix check was widened from
  `<base>/store/` to all canonical roots under `~/.aicx/`. On the current
  corpus (4418 empty-body candidates, many of which are non-repo) the
  command now successfully emits the reviewable bash script described in
  `--help` instead of failing on the first non-store-rooted candidate.
- Duplicate `sidecars` / `sidecar_coverage` rows in `aicx doctor` text
  output eliminated — the report now has a single canonical
  sidecars-coverage row.
- `aicx intents` stderr no longer leaks the Rust internal module prefix
  `aicx::intents:` when the candidates cap is reached; the warning now
  reads `aicx intents: warning: ...` in the operator-styled format. A
  binary-string guard test walks the compiled rodata to catch future
  regressions.
- `aicx::cli::failure` clippy hygiene: an internal lowercase comparison
  uses `eq_ignore_ascii_case` instead of a manual case-fold, restoring a
  clean `cargo clippy -- -D warnings` build.

## [0.9.0] - 2026-05-23

### Breaking

- state.json hash algorithm is now `blake3-128-v2` with length-prefixed
  field encoding (closes the raw-concat hash-splitting risk). Any older
  state — including legacy `siphash13-v1` (introduced in pass-2 G-1) and
  any interim `blake3-128-v1` builds — is treated as a legacy cache:
  current code migrates directly to `v2` on load and clears
  `seen_hashes` once. After upgrade, the first `aicx store` will
  re-process the recent `-H` window once. No data loss, but timeline
  may show duplicates if a parallel ingest is running.

### Added
- `aicx extract` batch conversation export command for emitting multiple
  session transcripts in a single pass without writing to the canonical store.
- `extract --conversation` output now carries `message_kind` and
  `collapse_stub_kind` metadata per message and surfaces extract
  statistics in the JSON projection.
- `aicx-monitor` crate for live CPU, RAM, GPU, and embedder process telemetry
  snapshots during long-running aicx pipelines.
- `aicx-progress-contracts` crate for shared indexing progress event contracts,
  telemetry snapshots, and sink traits across producers and UI consumers.
- Explicit `-p` filter syntax for `aicx index` and `aicx search`:
  `-p owner/repo` (strict slug), `-p owner/` (org wildcard),
  `-p /repo` (cross-org repo wildcard), `-p name` (cross-org match on
  organization or repository). Multiple `-p` flags or a comma list form a
  union. Filters resolve to canonical `<owner>/<repo>` slugs before
  downstream index lookup so a short repo name like
  `-p spotlight-convo-pipeline-v2` expands to its full
  `vetcoders/spotlight-convo-pipeline-v2` index path.

### Changed
- **Project filter is now word-boundary path match, not substring.**
  `--project test` no longer matches `cwd: /tmp/fastest-project`; multi-word
  filters compose with AND semantics across path words, and multiple filters
  with ANY. Path is split on `/`, `\`, `-`, `_`, `.`; filter on `-`, `_`, `.`.
  Message-text matching is dropped entirely — a transcript that *mentions*
  a project name does not belong to that project.
- **Canonical store project filter (`aicx index/search -p`) no longer
  substring-matches.** `-p vista` previously matched `vista-portal`,
  `VistaBrain`, `vista-datasets`, `nextra-docs-vista` etc., ballooning a
  single-project request into seven projects (~32k chunks). Now `-p vista`
  matches the exact repo or organization name `vista` (case-insensitive).
  For multi-project intent, repeat the flag or use the explicit wildcards.
- `aicx extract --conversation` deduplicates exact-equal short user messages
  within the same session (≤ 1000 chars, ≤ 2 s delta). Assistant messages and
  long bodies are untouched.

### Fixed

- **Segmentation identity leak: text mentions could be promoted to
  assertable ownership.** `infer_tiered_identity_from_text` walked any
  absolute path it found in chunk text into the filesystem and called
  `git remote get-url origin` to resolve identity — so a chunk that
  merely *mentioned* `/Users/foo/Downloads/ai-collaborators/...` could
  hijack a session into whatever GitHub repo that local clone's remote
  pointed to (e.g. `Szowesgad/maciej-almanach`). Round-1 cut the FS
  walk; round-2 also drops text-mention identity from
  `infer_tiered_identity_from_entry` entirely. Entry-level identity now
  comes only from cwd / projectHash registry. Text mentions stay
  accessible through the standalone `resolve_bucket` API
  (`BucketingSource::ContentMention`) for future search-hint use cases,
  but never enter segment routing — so a session no longer splits on
  context_switch when a chunk casually links to another repo, and
  `segment.repo` no longer carries non-ownership signals.
- **`is_probably_repo_name` accepted date-shaped names.** Strings like
  `2026-01-22`, `2026_01_22`, `2026_0122` passed the alphanumeric+`.-_`
  filter and produced pseudo-repos such as `Codescribe/2026-01-22` in
  the canonical store. New `looks_like_date_pattern` guard rejects
  these three shapes outright.
- **`aicx index/search -p <bare-name>` ambiguity is now reported.**
  When `-p codex` matches both an organization (`codex/*`) and a
  repository (`*/codex`), `resolve_project_filters_or_error` prints a
  stderr warning naming both matches and suggesting `-p codex/` or
  `-p /codex` to disambiguate. Filter behavior is unchanged (still
  returns the union); the warning just removes the silent WTF.
- `infer_repo_identity_from_known_layout` matches markers
  (`hosted`/`repos`/`repositories`/`github`/`git`) case-insensitively, so
  macOS conventions like `/Users/user/Git/Org/Repo` resolve through cwd
  instead of falling back to text inference.
- `aicx index -p` / `aicx search -p` reject filters with no matching
  project (instead of silently resolving to the `_all` bucket after a
  typo) and print accepted syntax in the error.
- Stale `embeddings.ndjson.tmp` checkpoint mismatch error now reports the
  checkpoint's recorded `schema/model/profile/dim` vs the active
  embedder's values, and suggests an exact `rm <path>` command.
- Junie extractor (`extract_junie_file`) now captures the full agent work
  trail — internal thoughts (`AgentThoughtBlockUpdatedEvent`), terminal
  commands (`TerminalBlockUpdatedEvent`), MCP calls (`McpBlockUpdatedEvent`),
  tool blocks (`ToolBlockUpdatedEvent`), file views
  (`ViewFilesBlockUpdatedEvent`), and file changes
  (`FileChangesBlockUpdatedEvent`) — in addition to the previously-only
  conversational user/assistant pairs. Sessions whose `ResultBlockUpdatedEvent`
  payloads are empty (most non-conversational steps) no longer index as
  bare prompts with zero context. Streaming snapshots are dedup'd per
  `(stepId, kind)` and pre-COMPLETED states are skipped for the streaming
  block kinds.
- Codex `extract --session <id>` now accepts a UUID prefix, suffix, or
  unique substring instead of requiring the full `session_meta.payload.id`.
  Ambiguous prefixes return a candidate list with an actionable error.
- Codex session parser surfaces aggregated diagnostics for missing
  `session_meta`, duplicate `session_meta.payload.id` values, filename ↔
  meta UUID mismatch, unparsable event_msg timestamps, and unrecognized
  event_msg `payload.type` values. Broad scans emit one summary line per
  run; direct file extracts emit per-file warning details.
- Codex `mcp_tool_call` and `mcp_tool_call_response` event types are now
  classified as `FrameKind::ToolCall` instead of being silently dropped.
- `infer_repo_identity_from_known_layout` (parser) now tries all five
  layout markers (`hosted`, `repos`, `repositories`, `github`, `git`).
  Previously a `?` inside the loop returned from the whole function on
  the first marker miss, so four of the five markers were dead code and
  paths like `~/repos/Org/Repo` fell back to the opaque bucket.
- Secret redaction now catches inline assignments such as
  `BRAVE_API_KEY="…"` or `api_key = "…"` embedded in prose and code
  spans, not only line-start environment declarations.

### Known Issues

- `cargo audit` still reports the RSA Marvin Attack advisory through the
  optional `rust-memex` transitive dependency surface. AICX does not use that
  RSA path as its own crypto hot path; the ignore rationale is tracked in
  `cargo-audit.toml` / `.cargo/audit.toml` until the upstream dependency stack
  clears it.

## [0.8.0] - 2026-05-15

### Added
- **Hybrid retrieval stack**: pure-Rust `BruteForceAdapter` for DenseIndex
  (zero C deps), Tantivy `LexicalIndex` adapter with Polish stemming and
  FilterCollector, retrieval evaluation harness with 50-query gold set and
  `make retrieval-eval` gate, fusion via Reciprocal Rank Fusion in the
  `aicx-retrieve` trait crate.
- **Live `aicx index` progress feedback**: per-chunk `IndexEvent` stream
  (RunStarted / ItemIndexed / ItemSkipped / ItemFailed / StatsTick /
  RunCompleted) with rolling rate and ETA. TTY-aware `IndicatifSink`
  shows a live progress bar with rate and ETA; piped runs fall through
  to structured `tracing` events. Previously the 75-minute embed loop
  emitted nothing on stdout until completion.
- **New workspace crates**: `aicx-progress-contracts` (typed event
  contracts, sink trait, rolling-rate helper) and `aicx-monitor` (live
  CPU/RAM/GPU and embedder process metrics via sysinfo, Apple Silicon
  GPU detection through ioreg).
- **Linux cross-compilation release matrix**: GitHub Actions workflow
  `release-linux.yml` plus `Cross.toml` config for x86_64/aarch64 musl
  and gnu targets.

### Changed
- **BREAKING**: NDJSON semantic index corruption now fails fast above the
  5% threshold instead of silent-swallowing corrupt lines. Operators
  running checkpoints from older builds may need `aicx index --sample 0`
  to rebuild cleanly.
- Zero-hour lookback (`--hours 0`) now aligns with the all-time contract
  across `aicx intents`, `aicx search`, and `aicx steer`.
- Active semantic index writer is reported as `busy` in `aicx doctor`
  output instead of falsely appearing idle.

### Fixed
- Partial semantic index builds resume from `.ndjson.tmp` checkpoint on
  subsequent runs instead of restarting from zero.
- Hybrid retrieve gate stabilized: fusion RRF orchestrator returns
  consistent ranks under mixed-adapter contention.

## [0.7.4] - 2026-05-15

## [0.7.3] - 2026-05-13

### Added
- Unified multi-project scope handling across search, intents, semantic index,
  MCP, dashboard, and doctor surfaces so operators can narrow to one or more
  projects with the same contract everywhere.

### Changed
- `aicx store` progress output is now bounded and human-readable: structured
  progress ticks remain machine-parseable while interactive terminals keep a
  stable three-line status view instead of flooding logs.

### Fixed
- Gemini JSONL extraction now treats `.jsonl` files as session transcripts,
  preserving `sessionId` metadata and allowing `aicx all` to ingest Gemini
  sources alongside Claude, Codex, Junie, and Codescribe.
- Junk corpus bucket slugs are covered so malformed or placeholder project
  names no longer leak into canonical project grouping.

## [0.7.1] - 2026-05-12

### Changed
- improve failure UX and make lance optionality clear

### Fixed
- install python before release version check

## [0.7.0] - 2026-05-08

### Added
- **Context Corpus Contract** for immutable `loct-context-pack` prism packs: sidecars now carry `artifact_family`, `schema_version`, `truth_status`, `learning_use`, `keywords`, and `content_sha256`; `aicx ingest --source loct-context-pack <PACK_DIR>` retains packs under `$HOME/.aicx/context-corpus/...` with `index.jsonl`.
- `aicx store` writes content hashes into sidecars and skips duplicate chunk bodies in the target bucket; `aicx doctor --check-dedup` reports duplicate content hashes across the live store and context corpus.
- `aicx doctor` surfaces the context-corpus state as a first-class check (`context_corpus` field on `DoctorReport`): reports `empty (will be created on first ingest)` when the directory is absent, `empty (no batches yet)` when the tree exists but holds no chunks, or a `N chunks across M batch(es) / R repo(s)` summary when populated. Operators no longer need to `ls ~/.aicx/context-corpus/` to confirm corpus existence.
- New operator documentation `docs/CONTEXT_CORPUS.md` covering the immutable-corpus contract: ingest source semantics, `~/.aicx/context-corpus/<org>/<repo>/<date>/loct-context-pack/<batch>/{raw,sidecars,index.jsonl}` retention layout, sidecar schema fields (`artifact_family`, `schema_version`, `truth_status`, `content_sha256`, `keywords`), immutability filter behavior (`aicx intents` and live-truth semantic indexes exclude `Example`-role chunks), and the parallel `context-corpus.embeddings.ndjson` materialization namespace. Cross-linked from `STORE_LAYOUT.md`, `COMMANDS.md`, and `README.md`.
- **9-type intent taxonomy** (`EntryType` enum): Intent, Why, Argue, Decision, Assumption, Outcome, Result, Question, Insight — replaces the flat 4-kind `IntentKind`.
- **Intent entry state machine** (`EntryState` enum): Proposed → Active → Done/Superseded/Contradicted with explicit lifecycle transitions.
- **Typed link graph** (`LinkType` + `Link`): DerivedFrom, Supersedes, Verifies, Contradicts, Supports, ResultsIn, Answers, LinksTo — first-class relations between intent entries.
- **`IntentEntry` struct** in `types.rs` with stable deterministic IDs, confidence scoring, topic tags, and cross-project linking.
- **`classify_chunk_entries()`** — per-chunk classifier covering all 9 types with marker-based and NL-pattern heuristics; abstain-first (confidence < 0.5 = skip).
- **Session-level post-processing**: unresolved intent detection (7-day threshold), supersedes chain detection (same topic, newer date), contradicted assumption detection (Result + failure words), insight-to-source `DerivedFrom` linking (top-3 in session).
- **`intent_entries` field** on `ChunkMetadataSidecar` for sidecar-level intent storage (backward compatible: empty Vec default).
- **`aicx migrate-intent-schema`** CLI subcommand with `--dry-run` (default) for classification count reports per-type and per-project.
- 25 new unit tests: 20 classifier tests (per-type + abstain + all-9 chunk + deterministic IDs + tag inference), 5 session-level tests (supersedes, contradicted, insight linking, unresolved threshold, recent not tagged).

### Changed
- `aicx intents` and semantic index writes exclude immutable `loct-context-pack` examples from the live-truth namespace; context-corpus embeddings materialize to a separate `context-corpus.embeddings.ndjson` namespace.
- Operator surface wording: "push" → "materialize" in CLI help text, progress messages, and doc comments to reinforce the two-layer mental model (canonical corpus first, semantic materialization second).
- Semantic compatibility validation now detects stale metadata even when no documents exist yet in the rust-memex index; reports diverged fields explicitly.
- Compatibility validation runs before file scanning in `memex-sync`, failing fast on config mismatches.
- `claude`, `codex`, `all`, and `store` now use watermark-tracked incremental refresh by default. `--full-rescan` is the explicit escape hatch for backfills, while legacy `--incremental` is accepted as a hidden no-op with a deprecation notice.
- `aicx dashboard` now owns both static HTML generation and live serving. `dashboard-serve` is kept as a hidden compatibility shim while public help/doc surfaces point to `aicx dashboard --serve`, including explicit `--allow-cors-origins` policies for non-loopback binds and `--bg` background launch.
- `aicx reports-extractor` is renamed to `aicx reports`, with default HTML output moved under `~/.aicx/` to avoid polluting the current working directory.

### Fixed
- Test isolation: source extraction tests use unique temp directories per test to prevent cross-test interference on parallel runs.

## [0.6.5] - 2026-05-06

### Added
- Public GitHub Release binaries for `aicx` and `aicx-mcp` on macOS arm64,
  Linux x64 GNU, and Linux arm64 GNU.
- Slim unsigned release archives with adjacent `.sha256` sidecars for each
  published target.
- Release-bundle install path that copies prebuilt `aicx` and `aicx-mcp`
  without requiring a Rust toolchain on the target machine.

### Changed
- GitHub Releases are the supported public binary install lane for this
  release. The npm wrapper lane remains present in-tree, but is not the active
  v0.6.5 install path until its platform packages match the release asset
  matrix.

## [0.5.5] - 2026-03-31

### Performance
- **Steer Indexing:** Integrated `rmcp-memex` (LanceDB backend) to dramatically speed up `aicx steer` and `aicx_steer` MCP queries. Metadata searches now take milliseconds instead of seconds by bypassing filesystem sidecar parsing in favor of a columnar metadata index.
- **Fast Text Search:** Upgraded `aicx_search` MCP tool to use the embedded `BM25Index` and `StorageManager` from `rmcp-memex`. Full-text searches across all stored contexts are now instantaneous, replacing the slow sequential file scans.

### Added
- **Frontmatter steering metadata** (`workflow_phase`, `mode`, `skill_code`, `framework_version`) on `Chunk` and `ChunkMetadataSidecar`.
- **`aicx steer` CLI command** — retrieves chunks by steering/sidecar metadata (run_id, prompt_id, agent, kind, project, date range).
- **`aicx_steer` MCP tool** — same steering-aware retrieval for MCP clients.
- **`/api/search/steer` dashboard endpoint** — HTTP GET with the same filtering surface.
- **Live search** with CLI `aicx search` subcommand and real-time result deduplication.
- **Resizable dashboard** with drag-to-resize panels.
- **Store progress reporting** on stderr (TTY-gated `Chunking... N/M segments`).
- Session metadata (agent, model, cwd) included in search output.
- `cwd` field on `Chunk` for working-directory awareness.

### Changed
- Frontmatter parser now separates `telemetry` from `steering` and strips detected frontmatter from chunk text even when YAML is malformed.
- Extracted shared types (`types.rs`) to break the `segmentation ↔ store` cycle; `segmentation` no longer depends on `store`.
- Removed `init` submodule and deprecated `Init` command (returns naturally instead of `process::exit`).
- Search results now strip aicx boilerplate for cleaner output.
- Docs: "memory extraction" → "timeline extraction", "vector memory" → "semantic index" across README, ARCHITECTURE, COMMANDS, and help text.

### Removed
- `src/init.rs` deleted (`git rm`); init flow fully retired.

## [0.5.4] - 2026-03-31 (Pre-release)

### Fixed
- Sync result reporting precise enough for framework orchestration.
- Hardened `aicx` to `rmcp-memex` transport seam.

## [0.5.3] - 2026-03-30

### Added
- **Frontmatter steering metadata** (`workflow_phase`, `mode`, `skill_code`, `framework_version`) on `Chunk` and `ChunkMetadataSidecar`.
- **`aicx steer` CLI command** — retrieves chunks by steering/sidecar metadata (run_id, prompt_id, agent, kind, project, date range).
- **`aicx_steer` MCP tool** — same steering-aware retrieval for MCP clients.
- **`/api/search/steer` dashboard endpoint** — HTTP GET with the same filtering surface.
- **Live search** with CLI `aicx search` subcommand and real-time result deduplication.
- **Resizable dashboard** with drag-to-resize panels.
- **Store progress reporting** on stderr (TTY-gated `Chunking... N/M segments`).
- Session metadata (agent, model, cwd) included in search output.
- `cwd` field on `Chunk` for working-directory awareness.

### Changed
- Frontmatter parser now separates `telemetry` from `steering` and strips detected frontmatter from chunk text even when YAML is malformed.
- Extracted shared types (`types.rs`) to break the `segmentation ↔ store` cycle; `segmentation` no longer depends on `store`.
- Removed `init` submodule and deprecated `Init` command (returns naturally instead of `process::exit`).
- Search results now strip aicx boilerplate for cleaner output.
- Docs: "memory extraction" → "timeline extraction", "vector memory" → "semantic index" across README, ARCHITECTURE, COMMANDS, and help text.

### Removed
- `src/init.rs` deleted (`git rm`); init flow fully retired.

## [0.5.2] - 2026-03-28

### Added
- **YAML frontmatter parsing** for chunk metadata extraction.
- **Sidecar files** (`.meta.yaml`) written alongside rust-memex chunks for external tooling.

## [0.5.1] - 2026-03-24

### Added
- **Repo-signal segmentation** in the store pipeline — chunks now carry repository identity signals.
- **rust-memex chunk sidecars** and `--preprocess` flag for pre-processing before memex push.
- **Makefile** with comprehensive build, test, lint, and release targets.
- Gemini truncation support and improved fuzzy search scoring.
- Test: repo-centric store runtime contract (`runtime_cli_store_contract.rs`).
- Test: legacy Codex format rejection (`legacy_codex_format_test.rs`).

### Changed
- Store contracts and migration scaffolding landed for repo-centric retrieval.
- Read/query surfaces hardened for repo-centric store paths.
- Checkpoint extraction seam hardened.

### Fixed
- Gemini JSON message structures preserved instead of being flattened (`sources.rs`).

## [0.5.0] - 2026-03-21

### Added
- **Repo-centric Migration Assistant:** Added the `aicx migrate` subcommand. This tool safely migrates older file-centric contexts (`file: <name>`) in your `~/.ai-contexters` store to the new canonical repo-centric directories. Use `aicx migrate --dry-run` to preview the changes.

### Changed
- **Behavioral Shift (Identity Model):** AICX now uses a canonical repo-centric identity model. Extracted contexts and stored artifacts are now grouped primarily by repository name rather than the raw filename of the agent log. This significantly improves retrieval quality and consistency, especially when syncing contexts to vector stores (rust-memex) or running direct extractions.
- Direct `extract` now infers repository identity when possible, demoting file provenance to secondary metadata.

## [0.4.3] - 2026-03-17

### Fixed

- Corrected the `SECURITY.md` disclosure path so private vulnerability reports go to the public `vetcoders/ai-contexters` repository instead of a stale owner link.
- Updated GitHub Actions workflow dependencies to current major versions for `checkout`, `cache`, `setup-python`, `upload-artifact`, and `download-artifact`, removing the Node 20 deprecation surface from future CI and release runs.

## [0.4.2] - 2026-03-17

### Added

- Tracked `Cargo.lock`, so `--locked` now works in CI and release automation instead of failing on GitHub runners.
- Shared validated filesystem helpers in `sanitize.rs` for safe file creation, file reads, and directory reads.

### Changed

- Public install docs and `install.sh` now reflect the live crates.io path, while still supporting local checkout and git install modes.
- Security-sensitive file and directory reads now go through validated helper paths across `init`, `intents`, `main`, `rank`, and `sources`.

## [0.4.1] - 2026-03-17

### Added

- Release/distribution docs now spell out the current source-first install path and the tag-driven GitHub Release lane.

### Changed

- Installer now prefers local checkout installs, supports a git fallback, and finishes setup with a quiet incremental refresh plus compact summary output.
- MCP background refresh and `aicx_store` now use the real incremental rescan path (`aicx all --emit none`, with `--full-rescan` reserved for backfills) instead of relying on a misleading stdout contract.
- `docs/COMMANDS.md` has been expanded to cover the active CLI surface and current stdout defaults.

## [0.4.0] - 2026-03-16

### Added

- **MCP server** (`aicx serve` / standalone `aicx-mcp` binary): 4 tools (search, rank, refs, store) over stdio and streamable HTTP transports.
- **Per-chunk quality scoring** (`rank.rs`): content-level signal/noise classification (0-10 scale) replacing the old all-SIGNAL output.
- `aicx rank` subcommand with `--strict` (hide noise) and `--top N` flags.
- **Dashboard search API**: `/api/search/fuzzy`, `/api/search/semantic`, `/api/search/cross` endpoints with rmcp-memex integration.
- `/api/health` and `/health` endpoints.
- Polish diacritics normalization for fuzzy search (wdrozenie matches wdrozenie).
- `project=` filter on fuzzy search (scopes to single project).
- Auto-rescan before search queries (incremental, milliseconds).
- Unified JSON error contract for all 400 responses.
- `aicx intents` subcommand for structured intent/decision extraction.

### Changed

- Rank made default command (`aicx -p proj` runs rank).
- Skills removed from repo — canonical source: vetcoders/vetcoders-skills.
- Package excludes: `*.html`, `*.patch`, `*.orig`, `.ai-agents/`, `skills/`.

### Added (Governance)

- LICENSE (BSL 1.1), CONTRIBUTING.md, CHANGELOG.md, SECURITY.md.
- GitHub Actions CI workflow (ubuntu + macos-14).
- Issue templates (bug report, feature request).
- Cargo.toml: keywords, categories, homepage, excludes.

### Fixed

- Bundle grouping bug in rank output.
- `.ai-agents/` paths now repo-relative, not absolute.
- Trailing whitespace in `is_noise_artifact`.
- Redundant closure in default command path.

## [0.3.1] - 2026-03-13

### Changed

- Refactored `run_extraction` to use `ExtractionParams` struct.

### Fixed

- Clippy `nonminimal-bool` warning.

## [0.3.0] - 2026-03-12

### Changed

- Renamed CLI binary from `agent-memory` to `aicx`.
- Updated showcase copy to Claude Code focus.

### Added

- Vetcoders skills suite and ai-contexters skill.
- `vetcoders-decorate` and showcase polish.
- Memex-first dashboard generator.

## [0.2.x] - 2026-02 to 2026-03

### Added

- Codex and Gemini support in extract.
- `extract` subcommand for direct Claude file processing.
- Intent and TODO signal surfacing in chunk output.
- Agent prompt defaults and init improvements.
- Claude stream-json mode with `--verbose` flag.
- Ultrathink/Insight and Plan Mode signal extraction.
- Chunk highlights and redaction optimizations.
- `action`/`emit` flags and artifacts layout.
- Semantic chunker and rust-memex integration.

### Changed

- Init mode and store command improvements.

### Fixed

- Assistant message extraction from content array.

## [0.1.0] - 2026-01

### Added

- Initial commit as `agent-memory` CLI tool.
- Claude Code JSONL extraction.
- Codex history support.
- Markdown and JSON output generation.

---

Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders
