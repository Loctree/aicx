# Commands

This is the exact CLI surface for `ai-contexters` (generated from `src/main.rs` via clap).

For the shortest “it works” path, see `README.md`.

## Global Options

`--no-redact-secrets`
- Default behavior is redaction enabled.
- Passing this flag disables redaction (not recommended unless you fully trust inputs and outputs).

## `ai-contexters list`

List available local sources and their sizes.

```bash
ai-contexters list
```

## `ai-contexters claude`

Extract timeline from Claude Code sessions.

```bash
ai-contexters claude [OPTIONS]
```

Common options:
- `-p, --project <PROJECT>...` project directory filter(s)
- `-H, --hours <HOURS>` lookback window (default: `48`)
- `-o, --output <DIR>` write local report files (omit to only write to store)
- `-f, --format <md|json|both>` local output format (default: `both`)
- `--append-to <FILE>` append local output to a single file
- `--rotate <N>` keep only last N local output files (default: `0` = unlimited)
- `--incremental` incremental mode using a per-source watermark
- `--include-assistant` include assistant messages (larger contexts)
- `--loctree` include loctree snapshot in local output
- `--project-root <DIR>` project root for loctree snapshot (defaults to cwd)
- `--memex` also chunk + sync to memex after extraction
- `--force` ignore dedup hashes for this run
- `--emit <paths|json|none>` stdout mode (default: `paths`)

Examples:

```bash
# Last 24h, store-first chunks, print chunk paths to stdout
ai-contexters claude -p CodeScribe -H 24

# Also write a local JSON report
ai-contexters claude -p CodeScribe -H 24 -o ./reports -f json

# Automation-friendly JSON payload on stdout
ai-contexters claude -p CodeScribe -H 24 --emit json | jq .
```

`--emit json` payload shape (stable fields):

```json
{
  "generated_at": "2026-02-08T03:12:34Z",
  "project_filter": "CodeScribe",
  "hours_back": 24,
  "total_entries": 123,
  "sessions": ["..."],
  "entries": [{ "...": "..." }],
  "store_paths": ["~/.ai-contexters/..."]
}
```

## `ai-contexters codex`

Extract timeline from Codex history.

```bash
ai-contexters codex [OPTIONS]
```

Same as `claude` except it does not have `--include-assistant` (Codex entries are always user-side).

Example:

```bash
ai-contexters codex -p codescribe -H 48 --loctree --emit json | jq .
```

## `ai-contexters all`

Extract from all supported agents (Claude + Codex + Gemini).

```bash
ai-contexters all [OPTIONS]
```

Options are similar to `claude`, with one important detail:
- `all` does not expose `--format` because local report writing is hardcoded to `both`.

Examples:

```bash
# Everything, last 7 days, incremental
ai-contexters all -H 168 --incremental

# Include assistant messages (Claude-heavy, can be large)
ai-contexters all -H 48 --include-assistant
```

## `ai-contexters store`

Write chunked contexts into the global store (`~/.ai-contexters/`) and optionally sync to memex.

```bash
ai-contexters store [OPTIONS]
```

Options:
- `-p, --project <PROJECT>...` project name(s)
- `-a, --agent <AGENT>` `claude`, `codex`, `gemini` (default: all)
- `-H, --hours <HOURS>` lookback window (default: `48`)
- `--include-assistant` include assistant messages
- `--memex` also chunk + sync to memex

Example:

```bash
ai-contexters store -p CodeScribe --agent claude -H 720 --include-assistant
```

## `ai-contexters memex-sync`

Sync stored chunks to `rmcp-memex` vector memory.

```bash
ai-contexters memex-sync [OPTIONS]
```

Options:
- `-n, --namespace <NAMESPACE>` vector namespace (default: `ai-contexts`)
- `--per-chunk` use per-chunk upsert instead of batch index
- `--db-path <DB_PATH>` override LanceDB path

Example:

```bash
ai-contexters memex-sync --namespace ai-contexts
```

## `ai-contexters refs`

List reference context files from the global store.

```bash
ai-contexters refs [OPTIONS]
```

Options:
- `-H, --hours <HOURS>` filter by file mtime (default: `48`)
- `-p, --project <PROJECT>` filter by project

Example:

```bash
ai-contexters refs -H 72 -p CodeScribe
```

## `ai-contexters state`

Manage dedup state.

```bash
ai-contexters state [OPTIONS]
```

Options:
- `--info` show state statistics
- `--reset` reset dedup hashes
- `-p, --project <PROJECT>` project scope for reset

Example:

```bash
ai-contexters state --info
```

## `ai-contexters init`

Initialize repo context and run an agent.

```bash
ai-contexters init [OPTIONS]
```

Options:
- `-p, --project <PROJECT>` project name override
- `-a, --agent <AGENT>` `claude` or `codex`
- `--model <MODEL>` model override
- `-H, --hours <HOURS>` context horizon (default: `4800`)
- `--max-lines <MAX_LINES>` max lines per section (default: `1200`)
- `--include-assistant` include assistant messages in context
- `--action <ACTION>` append a focus/action to the prompt
- `--no-run` build context/prompt only
- `--no-confirm` skip interactive confirmation
- `--no-gitignore` do not auto-modify `.gitignore`

Example:

```bash
ai-contexters init --agent codex --no-confirm --action "Audit memory and propose a plan"
```

## Exit Codes

- `0` on success.
- `1` on errors (invalid args, IO failures, runtime errors).
- `--help` and `--version` exit `0`.
