# aicx Monolith Decomposition for Maintainability

**Owner:** Grok (full vc-ownership)
**Date:** 2026-05-27
**Branch:** claude/aicx-uniformity (living tree)
**Goal:** Systematic decomposition of unmaintainable large files to enable further development. Not cosmetic — real
bounded modules with clear responsibilities.

## Context & Evidence

Current largest files (as measured):

- src/main.rs: 7144 LOC (CLI god + dispatch + extract batch logic + warnings)
- src/sources.rs: 6247 LOC (provider extractors + shared timeline glue)
- src/store.rs: 2195 LOC (already partially extracted: atomic_write, ignore, migration, paths)
- Others from operator list: intents, doctor, dashboard*, vector_index, output

House style (from git history on this branch):

- Prefer moving tests out first (see vector_index/iter3_tests, store/tests, cli tests splits)
- Extract clear surfaces (store migration, cli/failure + StructuredFailure)
- Small-to-medium focused extractions with descriptive commits
- Use `src/<area>/` subdirectories when natural

Key input reports (treated as requirements):

- docs/scope-overflow.md (W-D-4 I-1): Many uncapped BufReader/read_to_string sites in main.rs + sources.rs — accepted.
- bug-tracker followup passes: Mostly deferred for this charter (orthogonal).

## Decomposition Principles (my standards for this delivery)

1. **Natural boundaries over forced ones.** Extract what clearly wants to be separate (CLI model, provider extractors,
   conversation projection, etc.).
2. **Follow existing patterns.** Extend `src/cli/`, `src/sources/`, `src/store/` rather than inventing new top-level
   crates unless justified.
3. **Minimal public API.** Re-export only what the rest of the crate actually needs.
4. **Zero behavior change.** Only what is required for compilation + tests.
5. **Compile after every logical group.** Never leave the tree red for long.
6. **High signal commits.** One clear extraction per commit where possible.

## Prioritized Waves (my call as owner)

### Wave 1 (current — highest leverage, lowest risk)

**CLI Command Model Extraction**

- Move all clap types (Cli, Commands, *Args, *Emit, *Action, SourcesCommands, etc.) + pure CLI shaping helpers from
  main.rs → `src/cli/commands.rs`
- Clean re-exports from `src/cli.rs`
- main.rs becomes thin dispatcher + runtime
- Expected shrink: ~1400-1600 LOC from main.rs
- Directly improves the biggest god file and makes the command surface testable

### Wave 2

**Conversation / Extract Batch Logic**

- The `run_conversations_batch`, `run_extract_session`, `run_extract_file` + their helpers are a clear cluster in
  main.rs
- Candidate: `src/cli/extract.rs` or `src/extract/` (to be decided after Wave 1)

### Wave 3

**sources.rs Provider Split**

- Each major provider family (claude, codex, gemini + antigravity, codescribe, operator-md, junie) as its own submodule
  under `src/sources/`
- Shared: timeline building, project filtering, content warnings, dedup logic
- `to_conversation*` family likely stays or moves to a small shared module

### Wave 4+ (future, lower priority for this delivery)

- Further thinning of main.rs dispatch
- I/O capping hygiene (from scope-overflow report)
- Deeper work on store.rs, intents.rs, doctor.rs if they remain pain points

## Success Criteria

- main.rs meaningfully smaller and obviously a dispatcher
- sources.rs meaningfully smaller with clear provider modules
- No split-brain (old locations don't keep carrying weight)
- All extractions follow repo style and compile + relevant tests pass
- Final quality gate (make check / clippy) green or failures precisely documented as independent

This plan will be updated live as reality teaches us during execution.

---
Owner: Grok — full end-to-end delivery under vc-ownership.