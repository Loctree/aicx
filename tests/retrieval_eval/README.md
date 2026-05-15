# AICX Retrieval Evaluation Harness

Operator-grade evaluation harness that gates retrieval PRs (tracks B/C/D/G/H/I).
This harness runs the brute-force NDJSON retrieval and compares performance against a measured baseline.

## Workflow

### Adding a new gold query
1. Add a new `[[queries]]` entry in `queries.toml`.
2. Determine `expected_top_3_paths` by finding real, canonical paths in `~/.aicx/store/`. These paths must exist!
3. Re-run `make test-retrieval-eval`. Note: if baseline changes significantly, you may need to re-baseline.

### Re-baselining
You should only re-baseline after an intentional retrieval upgrade.
1. Delete `baseline.json`.
2. Run `make test-retrieval-eval`. This establishes the new floor metrics based on the current implementation.
3. Commit the new `baseline.json`.

### Interpreting regression failures
The harness will fail if the recall drops by more than 0.05 (5 percentage points) compared to `baseline.json`. 
- Check the log for which queries regressed. 
- A drop in recall could indicate that the embedding pipeline or index search is failing to return expected chunks. Ensure that any new retrieval logic preserves the behavior expected by the gold set.
