# Redaction (Regex Engine)

`aicx` redacts secrets by default before writing anything to disk.
The goal is pragmatic safety: avoid accidentally persisting tokens/keys in:
- `.ai-context/` artifacts,
- `~/.aicx/` store,
- memex chunks.

Implementation lives in `src/redact.rs`.

## What Gets Redacted

Patterns currently handled:
- Private key blocks: `-----BEGIN ... PRIVATE KEY----- ... -----END ... PRIVATE KEY-----`
- `Authorization: Bearer <token>`
- Uppercase env var assignments (best-effort heuristic)
- Header tokens: `X-API-KEY`, `X-Auth-Token`, `Api-Key`, `Token`
- Common API token formats: OpenAI `sk-...`, GitHub `github_pat_...` and `ghp_...`, Slack `xox[baprs]-...`, AWS access key `AKIA...`, Google API key `AIza...`

## Env Assignment Heuristic

`src/redact.rs` matches “env-style assignments” only for uppercase keys:

- optional `export `
- key: `[A-Z][A-Z0-9_]{2,}`
- equals sign
- value: a non-whitespace token

Then it redacts only if the key name looks sensitive (suffix/prefix checks like `*_API_KEY`, `*_TOKEN`, `*_SECRET`, `*_PASSWORD`, `PAT_*`, etc.).

This avoids false positives like `onPatientCreated={() => ...}` in code snippets.

## Known Limitations (Today)

This is best-effort:
- It may over-redact benign header lines (safe failure).
- It can under-redact env assignments where values contain spaces or quotes.
- Redaction is regex-based, not a full parser.

## Performance Notes

Redaction uses a fast negative path:
- `RegexSet` quickly checks whether any common secret patterns match at all.
- If nothing matches (and env assignments do not match), redaction returns without running the full pipeline.

The replacement pipeline avoids repeated re-allocations by only replacing the internal `String` when a regex actually matches.

If you extend the redactor, add tests for:
- a clean string returning unchanged,
- quoted env assignment values (if you extend env parsing),
- ensuring sensitive headers still redact correctly.
