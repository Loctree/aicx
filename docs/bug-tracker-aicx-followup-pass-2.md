# bug-tracker-aicx — Follow-up Pass 2

**Source.** Findings from vc-operator session 2026-05-20 (PR #4, base `9069b5e` plus `a170888` security hotfix). Consolidated report: `~/AI_notes/projects/aicx/reports/2026-05-20_all-findings-final-session-summary.md` (sections B + C2 + D).

**Plan baseline.** Branch state after PR #4 merge (or `fix/bug-tracker-pass-1@a170888` if still pre-merge). All references below cite **post-pass-1** code.

**⚠️ AUDIT FIRST.** This plan was written 2026-05-20 right after pass-1 close-out. By the time the next vc-operator session runs, some of these may already be addressed by spontaneous follow-up commits (especially B5 `rust-toolchain.toml` pin, B6 log-spam, C2 fallback timestamp — they're high-leverage and any contributor could pick them off). Each worker must verify current state with `loctree:find <symbol>` + `git log -S '<symbol>'` before implementing.

**Out of scope of this pass.** Vibecrafted runtime issues (Fleet wrapper zombie, gemini reliability, cancel API, etc.) — those live in `~/AI_notes/projects/vibecrafted/specs/vibecrafted-runtime-pass-1.md` for the vibecrafted repo session.

---

# AREA G — Migration & Lifecycle bugs (from pass-1 smoke)

> Plan-pass-1 introduced new behaviour (BLAKE3-128 dedup, atomic state save, B-2 strict load). Pass-2 closes the gaps surfaced when running pass-1's code against real-world legacy state.

## G-1 (P1) — State migration regression: legacy `siphash13-v1` → BLAKE3-128 hard-fail before migration runs

**Symptom.** Existing aicx users with `~/.aicx/state.json` from pre-pass-1 era (`hash_algorithm: "siphash13-v1"`, u64 hashes) cannot run `aicx all`/`aicx store` after upgrade:
```
Error: state.json corrupted, no backup; manual recovery needed: ~/.aicx/state.json
```

**Root cause.** W3-B-hardening (`b531954`) bumped storage to BLAKE3-128 (u128 / `[u8;16]` hashes). State load `serde_json::from_str(&contents)` at `src/state.rs:157` fails strict-type deserialize on legacy u64 array values BEFORE `apply_load_migrations()` (which would clear seen_hashes + bump algorithm) gets a chance to run. B-2 fix correctly rejects with "corrupted" message — but the migration code path is dead for this case.

**Files involved:**
- `src/state.rs:157-182` (load function)
- `src/state/migration.rs` (apply_load_migrations + algorithm constants)

**Fix direction.**
- (a) Version-aware deserialize: peek `hash_algorithm` field via untagged `serde_json::Value` first; if `siphash13-v1` → branch into legacy parser path that produces empty `seen_hashes` + bumps algorithm to current; then proceed.
- (b) Or: change struct to deserialize hashes via a `#[serde(deserialize_with = "...")]` helper that accepts both u64 (legacy) and u128/[u8;16] (current) arrays, then `apply_load_migrations` clears legacy.

**Acceptance:**
- [ ] Existing siphash13-v1 state.json loads successfully after upgrade with clear "migrated from legacy hash algorithm" warning.
- [ ] seen_hashes is cleared (legacy entries don't survive — content-equality fallback re-deduplicates on next ingest run via canonical store).
- [ ] hash_algorithm field is bumped to current value on first save after migration.
- [ ] New test in `src/state.rs::tests` or `tests/locks_contention.rs`: write a `siphash13-v1`-shaped state.json with u64 entries, load, assert migration ran (algorithm bumped + seen_hashes empty).

**Out of scope.** Don't touch atomic save path (B-2), outer state lock (B-3), or BLAKE3 implementation itself (B-6). Migration-only fix.

---

## G-2 (P2) — `aicx index` lacks lock liveness check; stale process holds `lance.lock` indefinitely

**Symptom.** Observed 2026-05-20: an `aicx index` PID from 14:52 was still holding exclusive `lance.lock` 2.5h later. New `aicx index` invocations timed out after 60s (W3-B `--lock-timeout` default) with `Error: timed out acquiring exclusive lock: ~/.aicx/locks/lance.lock`. POSIX fcntl auto-releases on process *crash* but not on *idle hang*.

**Files involved:**
- `src/locks.rs` (acquire_exclusive, acquire_exclusive_with_timeout)
- `src/store.rs` lance.lock holder sites
- New helper: lock holder PID sidecar

**Fix direction.**
- Lock holder writes its PID + timestamp + run kind to a `<lockname>.holder` sidecar file alongside the lock.
- Lock acquire flow: if acquire times out, read sidecar, check `pid_is_alive(holder_pid)` (already exists in `src/locks.rs:194-205`). If holder PID is dead → cleanup stale lock + retry. If alive but idle (heartbeat update timestamp older than N minutes) → emit warning advising operator to `kill <pid>` manually (don't auto-kill, operator decision).

**Acceptance:**
- [ ] Lock holder writes sidecar with PID + timestamp on acquire.
- [ ] Acquire-with-timeout flow checks sidecar on timeout, branches per pid_is_alive.
- [ ] Stale-dead-process scenario: integration test that simulates a "stale lock + dead PID" → next acquire succeeds with warning.
- [ ] Stale-alive-idle scenario: test that emits "lock held by idle PID N for M minutes; consider killing" without auto-killing.

**Out of scope.** Don't change the lock primitive (fcntl is correct). Don't reduce lock timeout below 60s (W3-B bumped intentionally).

---

## G-3 (P2/scale) — `aicx index` full rebuild via cloud-embed = 37h ETA for 76k chunks; no incremental path

**Symptom.** Observed 2026-05-20: after fresh `aicx all -H 24` adding 2617 chunks to existing 74k canonical store, `aicx index` started embedding ALL 76k chunks through cloud (`http://100.64.0.1:11434/v1/embeddings`) at ~2.5s/req. ETA: 2240+ minutes (37h). Unusable for daily ops.

**Files involved:**
- `src/vector_index.rs` (write_index, primary index build loop)
- `src/search_engine.rs` (index freshness detection)

**Fix direction.**
- Default `aicx index` walks new sidecars only (chunks with `created_at > index_mtime`). Embed only those, append to existing `embeddings.ndjson` (header.entry_count updated per D-2 pattern).
- Full rebuild only via explicit `--full-rescan` flag (already exists for `aicx store`; add to `aicx index`).
- `--dry-run` already exists and useful for ops; document it.
- Optional: GGUF local backend toggle in startup log (`Backend: cloud (slow, network)` vs `Backend: gguf (fast, local)` so operator knows what they're getting into).

**Acceptance:**
- [ ] `aicx index` defaults to incremental walk based on sidecar mtime vs `embeddings.ndjson` header `generated_at`.
- [ ] `--full-rescan` flag triggers full rebuild.
- [ ] Incremental run for a fresh `aicx all -H 1` (which adds maybe 50 new chunks) finishes in < 5 minutes via cloud (50 × 2.5s = ~2 min), not 37h.
- [ ] Test: small fixture corpus, run `aicx index`, add 5 new chunks, run `aicx index` again, assert only 5 new embeddings appended (not 5 + all originals).

**Out of scope.** Don't change embedder backend (still cloud or GGUF based on config). Don't change vector_index.rs header schema (D-2 already correct).

---

## G-4 (P1 UX) — Diagnostic warnings emit log-spam (>2000 stderr lines per `aicx all`)

**Symptom.** Operator's flesh reaction: *"log-spam na milion linii"*. A-3 timestamp + A-25 content sanitization diagnostics emit per-file warnings with 5 sample line numbers each. Vista folder (~60 jsonl files) → >2000 stderr lines on every `aicx all -H N` invocation. Drowns out actionable output; operator can't distinguish "something serious happened" from "background noise".

Example seen today:
```
Claude session warning: ~/.claude/projects/-Users-user-Git-vista/abc.jsonl
  has 134 unparsable timestamp(s); frames dropped.
  Sample(s): line 1: <missing>, line 5: <missing>, line 10: <missing>, line 11: <missing>, line 34: <missing>
Claude content warning: ... preserved zero-width character U+FEFF at byte offset 124
... × ~60 files ...
```

**Files involved:**
- `src/sources.rs` (Claude/Codex/Gemini/Junie extractor warning emit points)
- `crates/aicx-parser/src/sanitize.rs` (content sanitization warnings)
- `src/main.rs` (CLI output formatter for extractor stats)

**Fix direction.**
- Per-extractor SUMMARY line on stderr at end of run, e.g.:
  ```
  Claude diagnostics: 60 files / 1234 frames had timestamp issues; 4 files / 47 bidi/ZWS offsets preserved-with-warning.
  Run with --verbose for per-file detail, or check ~/.aicx/state/diagnostics-<run-id>.log
  ```
- Per-file warnings only with `--verbose` flag (existing in CLI surface? if not, add).
- Optional: structured JSON log to `~/.aicx/state/diagnostics-<run-id>.log` always written (for opt-in operator review without stderr noise).

**Acceptance:**
- [ ] Default `aicx all -H N` emits ≤ 5 lines of diagnostic SUMMARY on stderr regardless of corpus size.
- [ ] `--verbose` flag re-enables per-file details (current behaviour).
- [ ] Structured JSON log per run exists at `~/.aicx/state/diagnostics-<run-id>.log` (or similar).
- [ ] Test: simulated extract with 10 files each having 5 unparsable timestamps → default stderr ≤ 5 lines, `--verbose` ≥ 50 lines.

**Out of scope.** Don't suppress diagnostic detection itself — content preservation is C2 below. Don't change warning content; only output formatting / verbosity.

---

## G-5 (P1) — A-3 fix is HALF-implemented: diagnostic emit ✅, but `frames dropped` instead of `fallback timestamp` per plan

**Symptom.** Plan A-3 (from original `bug-tracker-aicx.md` lines 44–50) required THREE things: (1) no silent drop, (2) always log warning, (3) use fallback timestamp (previous or system). W3-A-sources (`1f7490f`) by codex implemented (1) and (2) but NOT (3). Result: message body from any Claude jsonl event without timestamp field is still dropped — just with a diagnostic trail.

**Affected.** Any Claude `.jsonl` event lacking `timestamp` (sidecar metadata events, system events, certain tool result frames). Content from those events never reaches canonical store today. Observed across most Vista folder sessions.

**Files involved:**
- `src/sources.rs` `extract_claude_line_entries` (Claude path; primary affected)
- Same pattern likely in Codex history / Gemini paths — verify before scope creep

**Fix direction.**
- In `extract_claude_line_entries`, when timestamp parse fails AND timestamp field is `<missing>`:
  - (a) reuse previous successful frame's timestamp from the same session — simplest, continuity-preserving;
  - (b) fall through to file mtime; or
  - (c) emit explicit `Timestamp::Inferred(<source>)` variant that downstream code respects + flags via sidecar metadata `timestamp_source: "fallback_previous"`.
- Keep emitting the diagnostic AND preserve the message body.

**Acceptance:**
- [ ] Claude jsonl event without timestamp field → message body lands in canonical store with fallback timestamp + diagnostic warn (no silent drop, no content drop).
- [ ] Diagnostic message format updated: instead of `frames dropped`, say `N frames preserved with fallback timestamp; sample lines: ...`.
- [ ] Sidecar `meta.json` records `timestamp_source: "fallback_previous"` or similar so downstream operators know which frames have inferred timestamps.
- [ ] Test: synthetic Claude jsonl with mixed (timestamp / no-timestamp) events; assert all preserved, no content drop, sidecar metadata correct.

**Out of scope.** Don't widen scope to Codex/Gemini in same task — those should be separate G-5b / G-5c follow-ups after G-5 lands clean on Claude path.

**⚠️ Sibling collision warning.** G-5 + G-4 both touch `src/sources.rs` Claude extractor. Pick one worker, sequential dispatch, NOT parallel. G-5 first (preserves content), G-4 second (formats the warnings).

---

# AREA H — Pre-existing repo issues surfaced by `aicx doctor` (carry over from pre-pass-1)

> These were not introduced by pass-1 but are now visible in operator's doctor output. They're below pass-2's main priority but worth noting in plan for triage.

## H-1 (P2) — 3018 empty-body chunks (4.07% of canonical store) — `--apply` modifier missing

Already in `docs/BACKLOG.md` 2026-05-12 entry. `aicx doctor --prune-empty-bodies` emits cleanup script but doesn't apply. Operator can't realistically review per-line for 12k chunks. Proposed: `--apply` modifier that moves empty-body chunks to `~/.aicx/quarantine/empty-bodies-<timestamp>/` (analogous to bucket quarantine, recoverable rename, not outright `rm`).

**Acceptance:** `--apply` modifier exists; quarantine dir created with timestamp; doctor warning level drops post-apply.

## H-2 (P2) — 188 orphaned + 40 missing index.json tuples

Already in `docs/BACKLOG.md` 2026-05-12 entry. Recovery: `aicx store --full-rescan`. Likely auto-resolved during G-3 incremental index work but worth verifying explicitly.

## H-3 (P3) — Lance index missing `_deletions/...arrow` test diagnostic noise

`tests/store_progress_markers.rs` + unittests stream `✗ steer_sync FAILED ... Lance index missing _deletions/...arrow` as intentional recovery-test diagnostic. Test result line says `ok` — actual test passes. But the visible "FAILED" message confuses operators reading test logs.

**Fix.** Add `tracing` target filter in test harness so this diagnostic is suppressed unless `RUST_LOG=lance=trace` (or similar). Test still asserts recovery; only the log noise goes away.

---

# AREA I — Pass-1 Area C tails (still open from original plan)

> Original `bug-tracker-aicx.md` Area C had several P3 items left open by pass-1. Worth picking up now for full closure.

## I-1 (P3) — Area C P3.1: BufReader caps — full coverage

W3-A-sources added BufReader caps at the audit-cited 8 sites in `src/sources.rs`, but the existing `MAX_LINE_BYTES` constant (likely 8 MiB) may not cover ALL `BufReader::lines()` call sites across the codebase. Audit second pass.

## I-2 (P3) — Area C P4.2: `is_self_echo` strict majority threshold

Audit found L573 still uses `echo_lines * 2 >= lines.len()` (50% threshold) even though comment claims "majority". Change `>=` → `>`. Two-line fix + 3 tests.

## I-3 (P3) — Area C P5.1: `default_session_extract_path` edge case guards

Audit found `""` → `.md`, `"."` → `..md`, `".."` → `...md`, no length cap. Mirror `conversation_batch_safe_session_filename` (`src/main.rs:2341-2396`) which already does hash-suffix on unsafe inputs.

## I-4 (P3, design decision pending) — Area C P5.3: `/tmp` allowlist policy

`crates/aicx-parser/src/sanitize.rs:74-81` unconditionally whitelists `/tmp` etc. Decision needed: (A) leave allowed for dev/smoke, (B) `AICX_ALLOW_TMP=1` opt-in, (C) `cfg(test)` only. Operator decision before implementation.

---

# Suggested wave grouping for pass-2 dispatch

```text
Wave A (foundation, single worker, sequential):
  G-1 state migration regression
    └─ unblocks all existing-user upgrades; highest-impact P1

Wave B (parallel, file-scope disjoint):
  G-5 A-3 fallback timestamp (sources.rs Claude path)
  G-2 lock liveness check (locks.rs + store.rs lance sites)
  G-3 incremental index (vector_index.rs)

Wave C (UX, after G-5 lands):
  G-4 diagnostic log-spam (sources.rs + main.rs CLI formatter)
    └─ DEPENDS_ON: G-5 (otherwise G-4 hides the warnings G-5 generates)

Wave D (cleanup, parallel):
  H-1 --apply modifier for prune-empty-bodies
  H-2 verify orphan/missing tuples
  H-3 test diagnostic tracing filter
  I-1, I-2, I-3, I-4 pass-1 Area C tails

Wave E (close-out):
  Verification + BUGFIXES.md entries + BACKLOG.md status updates + PR.
```

**Agent rotation for pass-2** (per AGENT FAIRNESS, balanced with pass-1 totals):
- claude × N · codex × N · gemini × N (skip gemini-3.1-pro-preview pending upstream loop-detection — see vibecrafted-runtime-pass-1 A-2)

**Cross-task sibling warnings:**
- G-4 and G-5 both touch `src/sources.rs` Claude extractor — sequential, G-5 first.
- G-1 and G-3 are both `src/state.rs` / `src/vector_index.rs` adjacent but disjoint enough — parallel OK.
- H-1 needs `aicx doctor` flow → likely touches `src/doctor.rs` which W2-F + W5-F already heavily edited. Read current state via loctree before edit.

---

# Plan source / context

This pass-2 distills aicx-side findings from the 25-finding consolidated session report at:
- `~/AI_notes/projects/aicx/reports/2026-05-20_all-findings-final-session-summary.md`

Per-task evidence chain: pass-2 task ↔ consolidated report section ↔ pass-1 audit citation:

| Pass-2 | Consolidated report | Pass-1 audit reference |
|---|---|---|
| G-1 | B1 | SUBAGENT_02 Audit B-6 (state migration aspect) |
| G-2 | B2 | SUBAGENT_06 Audit F-P1/P2-3 cross-ref (lock liveness theme) |
| G-3 | B3 | SUBAGENT_04 Audit D-1/D-3 (index lifecycle theme) |
| G-4 | B6 | NEW (surfaced post-pass-1 by operator smoke) |
| G-5 | C2 | SUBAGENT_01 Audit A-3 (verified half-fix today) |
| H-1..H-3 | D1..D3 | Pre-existing pre-pass-1 BACKLOG.md entries |
| I-1..I-4 | (pass-1 known follow-ups) | SUBAGENT_03 Audit C P3.1/P4.2/P5.1/P5.3 |

**Append to `docs/BUGFIXES.md` per task on close-out.** Update `docs/BACKLOG.md` items where applicable.

---

_Pass-2 plan written 2026-05-20 by Claude operator-agent in vc-operator mode, after PR #4 close-out. Read with `vc-init`/`vc-scaffold` skill at session start to validate against then-current HEAD before dispatching._
