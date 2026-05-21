# AICX Retrieval Evaluation Harness

Operator-grade evaluation harness that gates retrieval PRs (tracks B/C/D/G/H/I).
The default target is portable and CI-safe: it validates the 50-query gold set
and committed baseline shape without requiring an operator-local semantic index.
The live target exercises production `hybrid_rrf` retrieval and compares it
against the committed baseline.

## Workflow

### Adding a new gold query
1. Add a new `[[queries]]` entry in `queries.toml`.
2. Determine `expected_top_3_paths` by finding real, canonical paths in `~/.aicx/store/`. These paths must exist!
3. Re-run `make test-retrieval-eval`.
4. On an operator host with a committed hybrid index, run `make test-retrieval-eval-live`.

### Re-baselining
You should only re-baseline after an intentional retrieval upgrade.
1. Delete `baseline.json`.
2. Ensure the live production hybrid index is committed and queryable.
3. Run `make test-retrieval-eval-rebaseline`. This establishes the new floor metrics based on the current implementation.
4. Commit the new `baseline.json`.

### Interpreting regression failures
The harness will fail if the recall drops by more than 0.05 (5 percentage points) compared to `baseline.json`. 
- Check the log for which queries regressed. 
- A drop in recall could indicate that the embedding pipeline or index search is failing to return expected chunks. Ensure that any new retrieval logic preserves the behavior expected by the gold set.
