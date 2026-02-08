# Distillation (Chunking)

`ai-contexters` distills raw timelines into chunked “agent-readable” context files.

This is the piece that makes the tool useful for:
- vector indexing (memex),
- fast onboarding for new agents,
- avoiding “paste 4000 lines of history” prompts.

Implementation lives in `src/chunker.rs`.

## Chunk Model

The chunker processes `TimelineEntry` streams and produces `Chunk` items:
- token estimate uses a simple heuristic: `tokens ≈ ceil(chars / 4)`
- target size defaults to ~1500 tokens with overlap (2 messages)
- extremely long messages are UTF-8 safely truncated (4000 bytes) in the chunk text

The output text format is stable and line-oriented:

```text
[project: <project> | agent: <agent> | date: <YYYY-MM-DD>]

[HH:MM:SS] <role>: <message>
[HH:MM:SS] <role>: <message>
...
```

## Tuning Knobs

Defaults (see `ChunkerConfig::default()` in `src/chunker.rs`):
- `target_tokens=1500`
- `min_tokens=500`
- `max_tokens=2500`
- `overlap_messages=2`

Practical guidance:
- Increase `target_tokens` if your queries need longer local context.
- Decrease `overlap_messages` if your store grows too fast.
- Keep `max_tokens` bounded to avoid “monster chunks” that get expensive to embed and hard to retrieve.

## Highlight Extraction

Chunks also compute lightweight “highlights” (see `extract_highlights` in `src/chunker.rs`).

Today, highlights:
- extract up to 3 first lines that match keyword heuristics (e.g. `Decision:`, `TODO:`, `Plan:`)
- are stored on the in-memory `Chunk` struct as `highlights: Vec<String>`

Current status:
- highlights are not written into chunk files yet
- highlights are not yet used as memex metadata

This is a deliberate staging step so we can refine the heuristic before persisting it.

## Efficiency Notes (Where To Improve)

The current chunker is correct and tested, but there are clear performance wins for very large timelines:

1. Avoid allocating a new `Vec<&TimelineEntry>` for every chunk window.
2. Precompute per-message token estimates once per day and reuse them across overlapping windows.
3. Replace the per-date `BTreeMap` grouping with a single-pass scan of already-sorted entries.
4. Reduce per-chunk string work in hot paths (chunk IDs and text builders).

If you implement these, keep tests in `src/chunker.rs` as the behavioral contract.

