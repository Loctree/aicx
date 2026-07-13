# Parser differential oracle

This directory is C0's executable oracle surface. It compares declared parser
truth, not whole Transcript Builder records.

Run the harness:

```bash
tests/parser_oracle/compare.py --self-test
tests/parser_oracle/compare.py --manifest tests/parser_oracle/manifest.toml
tests/parser_oracle/compare.py --case codex_minimal --actual /tmp/aicx-envelope.json
```

For donor-supported agents, materialize a donor record with the exact command
stored in `manifest.toml`, then adapt `session_record.json` into
`parser_oracle.envelope.v1`. Junie has no donor adapter and therefore compares
against a reviewed Rust-native golden.

`exact_fields` are compared recursively and fail with the first field path.
`[[case.heuristic_assertions]]` entries are semantic predicates; they are not
byte snapshots. C0A's `normative_fields.toml` remains the ownership authority.
This harness may consume it but must not change it.

Fixtures are small synthetic sessions. The private benchmark rollout is used
in place by `tools/bench_single_session.sh` and must never appear here.
