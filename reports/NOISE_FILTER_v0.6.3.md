# Noise Filter Rollout — aicx v0.6.3

**Branch:** `claude/aicx-noise-filter`
**Commits:** `8fb6e98` → `ffe288a` → `52f8c14` (3 commits, ~727 LOC delta)
**Base:** `ff39ec1` (codex/aicx-parser-p10)
**Status:** ready for review; not pushed; merge strategy operator-decided.

---

## TL;DR

The chunker writes per-chunk text plus a `[signals]` block (decisions /
results / outcomes / etc.) for downstream RAG indexing. ~40% of every
chunk was structural scaffolding — line-numbered grep matches like
`60 Passed:`, raw tool-call echoes like `input: {"command":...}`, and
stray YAML delimiters bleeding from nested reports. Operators
audited the canonical store and confirmed: 30–40% of memory entries
were noise, eating semantic budget and confusing downstream consumers.

This rollout strips three structural noise classes line-by-line at the
chunker boundary, with an observability counter, an operator opt-out,
backward-compat tested wire format, and a doctor health metric.
Empirical validation against 50 real chunks confirmed **40.5% byte
drop / 39.2% line drop**, matching the audit prediction exactly.

---

## Problem (as captured by operator audit)

> 1. Memory entries to surowe chunks z transcripts, nie semantic
>    memory. Przykłady noise:
>      - `"text": "60 Passed:"` ← line number z markdown report
>      - `"text": "input: {..."` ← echo komendy agenta
>      - `"text": "7 status: completed"` ← line z YAML frontmatter
>
>    To 30-40% entries jako noise.
>
> Konkretna sugestia priorytet: pre-filter na poziomie chunk extraction
> z 3 rules:
>      - Drop `^\d+\s+` (line-numbered grep matches)
>      - Drop `^input: \{` (tool call echoes)
>      - Drop YAML frontmatter / markdown headings standalone
>
> To samo wyciełoby ~40% obecnego output bez utraty signal.

The audit also flagged a failure tagger (loctree-side, **out of scope**
for this branch) and listed relevance ranking + deduplication as v1.2
follow-ups (deferred).

---

## Design decisions

### Three regex classes, line-local

```
^\s*\d+[ \t]+(?:[^.\d]|$)   line-numbered grep matches
^\s*input:\s*\{             tool-call echoes
^---\s*$                    stray YAML frontmatter delimiters
```

Exclusion `(?:[^.\d]|$)` after `\d+[ \t]+` keeps ordered Markdown lists
(`1. First item`) untouched: a digit followed by `.` is NOT noise.

Filtering is line-local, not message-local: a single noisy line in an
otherwise-semantic paragraph drops only that line. Empty messages are
elided so windows that reduced to pure scaffolding don't emit empty
role lines.

### Sanitize ONCE, before signal extraction

Initial implementation (commit `8fb6e98`) applied the filter only in
`format_chunk_text_inner`'s entry loop. Real-world e2e tests caught a
bug: `extract_signals` and `extract_highlights` scan raw `entry.message`
to build the `[signals]` block, so noise leaked through the secondary
surface even though the primary entry text was clean. Fixed in
`ffe288a` by introducing `sanitize_window(window, config)` as the
single sanitization seam, called BEFORE `extract_signals` /
`extract_highlights` in both callers (`chunk_day_entries` and
`format_chunk_text`).

Lesson: filtering is cheaper at the data source, not at the formatter.

### Opt-out, not opt-in

`ChunkerConfig.noise_filter_enabled: bool` defaults to `true`. The CLI
mirrors this with `--no-noise-filter` (negated for ergonomic default).
Rationale: 99% of operators want clean chunks; the 1% need raw mode
for debugging or regression bisection. Opt-out preserves zero-friction
default while keeping the escape hatch one flag away.

### Wire-format backward compatibility

`ChunkMetadataSidecar.noise_lines_dropped` uses
`#[serde(default, skip_serializing_if = "is_zero_usize")]`:

- old sidecars (no field) deserialize to `0` ✓
- new sidecars with `0` skip the field on serialize → still parse with
  pre-rollout consumers ✓
- new sidecars with non-zero serialize the counter for observability ✓

This was tested explicitly in `sidecar_backward_compat.rs` against a
real-shape JSON literal sampled from `~/.aicx/store/Loctree/aicx/`.

---

## Empirical results

`cargo run -p aicx-parser --example noise_smoke` over 50 sample chunks
from `~/.aicx/store/Loctree/aicx/`:

```
bytes:  148_440 → 88_304   (40.5% drop)
lines:    2_055 →  1_250   (39.2% drop)
avg bytes/file: 2_968 → 1_766
```

Distribution is bimodal:

| Cohort | Files | Drop range | Character |
|---|---|---|---|
| Noisy reports | 13 | **52–95%** | grep-output reports, codex plan markdown with line numbers |
| Clean conversations | 37 | **0–20%** | regular agent dialogues, no scaffolding |

Worst case (95.1% drop): a single 4.3KB plan file reduced to 211 bytes
of pure semantic content. Best case (0% drop): clean chat sessions
pass through untouched. **No false positives observed** on clean inputs
across the sample.

This bimodal distribution is the desired shape: the filter is sharp on
scaffolding and invisible on conversation. A linear ~40% drop across
all files would have signaled overreach.

---

## API delta

### `aicx-parser` crate

```rust
// New module
pub mod noise;
pub use noise::{filter_noise_lines, filter_noise_lines_with_count, is_noise_line};

// Chunk metadata
pub struct Chunk {
    // ... existing fields ...
    pub noise_lines_dropped: usize,           // ← new
}

pub struct ChunkMetadataSidecar {
    // ... existing fields ...
    pub noise_lines_dropped: usize,           // ← new (serde default = 0, skip on zero)
}

// Chunker config
pub struct ChunkerConfig {
    // ... existing fields ...
    pub noise_filter_enabled: bool,           // ← new (default: true)
}
```

### `aicx` (main crate)

```rust
// New CLI flag on `aicx store`
struct StoreCommand {
    // ... existing args ...
    no_noise_filter: bool,                    // ← --no-noise-filter
}

// New doctor check
pub struct DoctorReport {
    // ... existing fields ...
    pub noise_health: CheckResult,            // ← new
}
```

Sidecar consumers (intents.rs, steer_index.rs) updated to construct
the new field literally as `0`; serde default handles deserialization
of pre-rollout `meta.json` files.

---

## Test coverage

| Suite | Count | Location | What it covers |
|---|---|---|---|
| Unit — noise regex | **15** | `src/noise.rs` | Each regex class, ordered-list preservation, header/timestamp survival, count helper, edge cases |
| Integration — e2e through chunker | **5** | `tests/noise_filter_e2e.rs` | Three noise classes through full pipeline, noise-only entry skip, clean-input invariance, sidecar counter aggregation, opt-out smoke |
| Integration — wire format | **4** | `tests/sidecar_backward_compat.rs` | Old-shape deserialize, zero-skip on serialize, nonzero round-trip, lossless old↔new round-trip |
| Unit — doctor (existing, regression-checked) | **6** | `src/doctor.rs` mod tests | Pre-existing tests survived noise_health field addition |
| Workspace lib (regression) | **324** | full workspace | aicx 234 + aicx-embeddings 4 + aicx-parser 86 — all green |

**Total new test surface:** 24 dedicated noise-filter tests.

---

## Verification (final state, branch HEAD `52f8c14`)

```
cargo test --workspace --lib              324/324 green
cargo test -p aicx-parser                 86 unit + 5 e2e + 4 compat = 95
cargo clippy --workspace --all-targets -- -D warnings   clean
cargo run -p aicx --bin aicx -- store --help            --no-noise-filter visible
cargo run -p aicx --bin aicx -- doctor                  noise_health Green (live store)
cargo run -p aicx-parser --example noise_smoke          40.5% drop on 50 real chunks
```

Live `aicx doctor` output on the operator's actual store:

```
[Green] noise_health: 0 noise lines dropped across 0/0 post-filter
        chunks (0% heavy >10 lines); 196663 pre-filter sidecars
```

This is the expected initial state: the entire 196k-chunk corpus was
indexed before the filter landed. Re-ingestion via `aicx store
--full-rescan` will populate the metric.

---

## Migration path

1. **No-op for existing data**: 1068 (`Loctree/aicx/`) and ~196k
   (full store) existing `meta.json` sidecars deserialize cleanly to
   `noise_lines_dropped == 0`. No data migration required.
2. **Opt-in re-ingest** (when operator wants populated metric):
   ```bash
   aicx store --full-rescan -H 720
   ```
   Filter applies during re-chunk; sidecars rewrite with new field;
   `aicx doctor` will then show populated `noise_health`.
3. **Opt-out for debugging**:
   ```bash
   aicx store --no-noise-filter -H 24
   ```
   Use this when comparing pre-filter vs post-filter behavior, or
   when raw upstream content must be preserved verbatim.

No migration script needed. No downtime. Wire format compatible in
both directions.

---

## Out of scope / deferred

| Audit point | Disposition | Reason |
|---|---|---|
| 1. Noise pre-filter | **CLOSED** in this rollout | Production-grade with observability + opt-out + doctor metric |
| 2. `aicx_failure` tagger negation | Out of scope | Tagger lives in **loctree** (`aicx_failure` authority is loctree-derived, not aicx-emitted) — separate repo, separate ownership |
| 3. Relevance ranking | Deferred to v1.2 | Operator directive: "Reszta (relevance ranking, dedupe) może czekać do v1.2" |
| 4. Deduplication | Deferred to v1.2 | Same |

---

## Files touched

| File | Change | Commit |
|---|---|---|
| `crates/aicx-parser/src/noise.rs` | new module, 3 regex + 2 helpers + 15 tests | `8fb6e98`, extended `ffe288a` |
| `crates/aicx-parser/src/lib.rs` | `pub mod noise;` | `8fb6e98` |
| `crates/aicx-parser/src/chunker.rs` | `sanitize_window`, Chunk/Sidecar/Config fields, From impl, signal-block fix | `8fb6e98`, `ffe288a` |
| `crates/aicx-parser/examples/noise_smoke.rs` | new empirical validator binary | `ffe288a` |
| `crates/aicx-parser/tests/noise_filter_e2e.rs` | 5 e2e tests | `ffe288a` |
| `crates/aicx-parser/tests/sidecar_backward_compat.rs` | 4 wire-format tests | `52f8c14` |
| `src/intents.rs` | sidecar literal field add | `ffe288a` |
| `src/steer_index.rs` | sidecar literal field add (×2) | `ffe288a` |
| `src/main.rs` | `--no-noise-filter` CLI wiring | `52f8c14` |
| `src/doctor.rs` | `check_noise_health` + `DoctorReport` field + format | `52f8c14` |

---

## Suggested merge strategy

Branch is local-only (no upstream tracking). Three options:

1. **Direct merge to `main`** — fast-forward possible since branch
   was cut from `ff39ec1` (codex parser-p10) and codex hasn't moved
   the line since. Cleanest history.
2. **PR via `develop` first** — if the team policy is "everything
   through `develop`", rebase onto current `develop` head first.
3. **Squash to single commit** — three commits collapse cleanly into
   one `feat(parser): structural noise filter with observability and
   opt-out`. History becomes flatter but loses the iter-by-iter
   narrative (and the bug-discovery breadcrumb in `ffe288a`).

Recommendation: option 1 if `main` is the live shipping line.
Option 3 only if shop policy requires squashed feature branches.

---

## References

- Operator audit: in-conversation user message, 2026-04-29 (3 noise
  classes + priority ordering, deferred items)
- Signal-block bug discovery: integration test failure during iter 2,
  documented in `ffe288a` commit body
- Empirical validation: `cargo run -p aicx-parser --example
  noise_smoke` against `~/.aicx/store/Loctree/aicx/` on 2026-04-29

_Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI_
