# W-D-4 I-1 BufReader/read_to_string cap audit - scope overflow

Date: 2026-05-21
Branch: `fix/bug-tracker-pass-2`
Baseline: pass-1 `1f7490f` (`src/sources.rs` caps)

## Verdict

Audit found more than 5 uncapped production call sites. Per the W-D-4 I-1
dispatch, this pass is audit-only. No runtime code was changed in this commit.

Loctree was used first:

- `find(name="BufReader", mode="symbols", lang="rs")`
- `find(name="MAX_LINE_BYTES", mode="where-symbol", lang="rs")`
- `find(name="read_to_string", mode="symbols", lang="rs")`
- `context(task="Audit BufReader::lines and read_to_string cap coverage...")`
- `slice(...)` for candidate files before classifying likely edit surfaces

Fallback `rg` was used only after Loctree because exact Rust call-site coverage
was incomplete for this audit. The Loctree fail was appended to
`~/.vibecrafted/loctree/loctree-fail.md`.

## Existing cap baseline

`MAX_LINE_BYTES` exists at `src/sources.rs:43` and remains unchanged.

Already capped via pass-1 `read_line_limited(..., MAX_LINE_BYTES)`:

- `src/sources.rs:1620`
- `src/sources.rs:1759`
- `src/sources.rs:2688`
- `src/sources.rs:2822`
- `src/sources.rs:3273`
- `src/sources.rs:3971`
- `src/sources.rs:5839`

Existing helper tests:

- `src/sources/tests.rs:1815`
- `src/sources/tests.rs:1823`

## Uncapped BufReader/read_line sites

These are production or production-adjacent and should be narrowed into a
follow-up implementation pass:

- `src/api.rs:352` - semantic index row count uses `BufReader::lines()`.
- `src/output.rs:501` - `find_last_sync_timestamp` scans markdown lines.
- `src/main.rs:2718` - Codex session metadata scan uses `BufReader::lines()`.
- `src/sources.rs:4613` - CodeScribe custom lexicon JSONL uses
  `BufReader::lines()`.
- `src/vector_index.rs:1018` / `src/vector_index.rs:1021` - tmp index header
  uses `read_line`.
- `src/vector_index.rs:1055` - committed index reader uses
  `BufReader::lines()`.
- `src/vector_index.rs:1136` - resume tmp index reader uses
  `BufReader::lines()`.
- `src/vector_index.rs:1260` - incremental baseline reader uses
  `BufReader::lines()`.
- `src/vector_index.rs:1388` - committed body copy uses `BufReader::lines()`.
- `src/vector_index.rs:1533` - query path scans semantic index with
  `BufReader::lines()`.
- `src/search_engine.rs:706` / `src/search_engine.rs:708` - header preview uses
  `read_line`.
- `src/search_engine.rs:718` - empty-index detection uses `BufReader::lines()`.
- `crates/aicx-retrieve/src/adapter_brute_force.rs:149` - brute-force NDJSON
  load uses `BufReader::lines()`.

Lower-risk / likely out-of-scope for I-1 implementation unless operator wants
complete pipe hygiene:

- `src/wizard/screens/store.rs:131` - child-process stderr pipe, internal
  command output.
- `src/wizard/screens/store.rs:140` - child-process stdout pipe, internal
  command output.

## read_to_string inventory

Central helper currently validates paths but does not cap total bytes:

- `crates/aicx-parser/src/sanitize.rs:240` -
  `read_to_string_validated(path)`.

Because many production reads route through that helper, capping it first would
cover these call sites without per-module rewrites:

- `src/sources.rs:1834`, `src/sources.rs:2000`, `src/sources.rs:2047`,
  `src/sources.rs:2528`, `src/sources.rs:3645`, `src/sources.rs:4815`,
  `src/sources.rs:5038`
- `src/mcp.rs:911`
- `src/main.rs:2111`, `src/main.rs:4112`
- `src/corpus.rs:211`, `src/corpus.rs:371`, `src/corpus.rs:571`,
  `src/corpus.rs:577`, `src/corpus.rs:811`
- `src/reports_extractor.rs:692`, `src/reports_extractor.rs:706`
- `src/rank.rs:738`, `src/rank.rs:783`, `src/rank.rs:1119`
- `src/vector_index.rs:241`, `src/vector_index.rs:703`,
  `src/vector_index.rs:851`, `src/vector_index.rs:973`,
  `src/vector_index.rs:1475`
- `src/store.rs:333`, `src/store.rs:1262`, `src/store.rs:1461`,
  `src/store.rs:2794`
- `src/intents.rs:364`, `src/intents.rs:2456`
- `src/search_engine.rs:730`

Direct production `fs::read_to_string` sites needing explicit classification:

- `src/state.rs:195` and `src/state.rs:208` - state file and backup.
- `src/store.rs:601` - index JSON.
- `src/store.rs:1386` - sidecar JSON.
- `src/doctor.rs:605` - index JSON diagnostic.
- `src/doctor.rs:955` - state JSON diagnostic.
- `src/doctor.rs:1155` - doctor content scan over stored chunk files.
- `src/dashboard_server.rs:1105` - dashboard-served file content.
- `src/wizard/screens/corpus.rs:132` - selected chunk preview.
- `src/wizard/screens/intents.rs:88` - intent source chunk preview.
- `crates/aicx-parser/src/segmentation.rs:73` - Gemini project map.

Likely bounded / not-applicable direct reads:

- `crates/aicx-embeddings/src/config.rs:151` - embedder TOML config.
- `src/auth.rs:108` - auth token file.
- `src/locks.rs:204` - lock holder sidecar.
- `src/steer_index.rs:179` - generated steer metadata.
- `crates/aicx-parser/examples/noise_smoke.rs:41` - example binary input.
- Test-only `fs::read_to_string` calls under `tests/**` and module test
  blocks in `src/output.rs`, `src/store.rs`, `src/corpus.rs`,
  `src/state.rs`, `src/dashboard_server.rs`, `src/vector_index.rs`,
  `src/main.rs`, `crates/aicx-parser/src/chunker.rs`,
  `crates/aicx-parser/src/sanitize.rs`, and
  `crates/aicx-retrieve/tests/tantivy_adapter.rs`.

## Suggested follow-up split

1. Central IO helper pass:
   - Move or mirror the 8 MiB cap into a shared helper accessible from
     `aicx_parser::sanitize`.
   - Add capped `read_to_string_validated` behavior and a synthetic
     `MAX_LINE_BYTES + 1` regression test.
   - Add capped line iterator/helper equivalent to the `src/sources.rs`
     `read_line_limited` pattern.

2. Semantic index NDJSON pass:
   - Cap `src/vector_index.rs`, `src/api.rs`, `src/search_engine.rs`, and
     `crates/aicx-retrieve/src/adapter_brute_force.rs`.
   - Add oversized-line tests for vector index and brute-force adapter.

3. Extractor/UI diagnostic pass:
   - Cap `src/main.rs:2718`, `src/output.rs:501`, `src/sources.rs:4613`,
     `src/wizard/screens/corpus.rs:132`, `src/wizard/screens/intents.rs:88`,
     and `src/doctor.rs:1155`.
   - Decide whether state/lock/config metadata reads should be capped or
     documented permanently as bounded internal files.

## 2026-05-24 PR #5 polarize addendum

`vc-polarize` for SC-01 chose one boundary: PR #5 is a consolidated
stabilization review surface, not a place to absorb more release/security scope.
Keep new work out of this PR unless it closes an existing merge blocker.

Current live evidence:

- Loctree prism pass: score 11/15, band `9..12`, payload
  `/Users/maciejgad/.vibecrafted/artifacts/Loctree/aicx/2026_0524/polarize/polr-222423-57363/prism.json`.
- GitHub PR #5 is open, non-draft, mergeable, with green remote checks.
- Local `cargo test --lib dashboard::tests::test_inline_markdown -- --test-threads=1`
  is green after `2030d3f` switched the Node harness to
  `globalThis.AicxMarkdown`.
- Local full `make test` is still red: isolated
  `vector_index::iter3_tests::query_index_recovery_hint_uses_full_rescan_not_fresh`
  fails because the recovery message does not contain `--full-rescan`, and
  full parallel execution also exposed a flaky dashboard-server log-capture
  assertion that passes in isolation.
- `docs/BUGFIXES.md` already records M-13 as deferred; keep that truth unless a
  dedicated CSP nonce/header implementation lands in a separate scoped cut.

Split / block list before merge:

- Fix the vector-index recovery-hint regression so the canonical operator
  recovery path is `aicx index --full-rescan --project <name>`.
- Stabilize or quarantine the dashboard-server log-capture assertion under full
  parallel `make test`.
- Keep H-2 Layer 1 store/index reconciliation operator-side until a separate
  PR owns it.
- Keep broader extractor/UI diagnostic read-cap work in a separate follow-up.
- Do not add new release-channel, installer, CSP nonce, or heartbeat work to
  PR #5 unless the operator explicitly reopens the scope.
