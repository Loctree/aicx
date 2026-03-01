# Commands

This is the exact CLI surface for `aicx` (generated from `src/main.rs` via clap).

For the shortest “it works” path, see `README.md`.

## Global Options

`--no-redact-secrets`
- Default behavior is redaction enabled.
- Passing this flag disables redaction (not recommended unless you fully trust inputs and outputs).

## `aicx list`

List available local sources and their sizes.

```bash
aicx list
```

## `aicx claude`

Extract timeline from Claude Code sessions.

```bash
aicx claude [OPTIONS]
```

Common options:
- `-p, --project <PROJECT>...` project directory filter(s)
- `-H, --hours <HOURS>` lookback window (default: `48`)
- `-o, --output <DIR>` write local report files (omit to only write to store)
- `-f, --format <md|json|both>` local output format (default: `both`)
- `--append-to <FILE>` append local output to a single file
- `--rotate <N>` keep only last N local output files (default: `0` = unlimited)
- `--incremental` incremental mode using a per-source watermark
- `--user-only` exclude assistant + reasoning messages (default: assistant included)
- `--loctree` include loctree snapshot in local output
- `--project-root <DIR>` project root for loctree snapshot (defaults to cwd)
- `--memex` also chunk + sync to memex after extraction
- `--force` ignore dedup hashes for this run
- `--emit <paths|json|none>` stdout mode (default: `paths`)

Examples:

```bash
# Last 24h, store-first chunks, print chunk paths to stdout
aicx claude -p CodeScribe -H 24

# Also write a local JSON report
aicx claude -p CodeScribe -H 24 -o ./reports -f json

# Automation-friendly JSON payload on stdout
aicx claude -p CodeScribe -H 24 --emit json | jq .
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

## `aicx codex`

Extract timeline from Codex history.

```bash
aicx codex [OPTIONS]
```

Same as `claude`, including assistant messages by default. Use `--user-only` if you want a user-only view.

Example:

```bash
aicx codex -p codescribe -H 48 --loctree --emit json | jq .
```

## `aicx all`

Extract from all supported agents (Claude + Codex + Gemini).

```bash
aicx all [OPTIONS]
```

Options are similar to `claude`, with one important detail:
- `all` does not expose `--format` because local report writing is hardcoded to `both`.

Examples:

```bash
# Everything, last 7 days, incremental
aicx all -H 168 --incremental

# User-only mode (exclude assistant + reasoning)
aicx all -H 48 --user-only
```

## `aicx store`

Write chunked contexts into the global store (`~/.ai-contexters/`) and optionally sync to memex.

```bash
aicx store [OPTIONS]
```

Options:
- `-p, --project <PROJECT>...` project name(s)
- `-a, --agent <AGENT>` `claude`, `codex`, `gemini` (default: all)
- `-H, --hours <HOURS>` lookback window (default: `48`)
- `--user-only` exclude assistant + reasoning messages (default: assistant included)
- `--memex` also chunk + sync to memex

Example:

```bash
aicx store -p CodeScribe --agent claude -H 720
```

## `aicx memex-sync`

Sync stored chunks to `rmcp-memex` vector memory.

```bash
aicx memex-sync [OPTIONS]
```

Options:
- `-n, --namespace <NAMESPACE>` vector namespace (default: `ai-contexts`)
- `--per-chunk` use per-chunk upsert instead of batch index
- `--db-path <DB_PATH>` override LanceDB path

Example:

```bash
aicx memex-sync --namespace ai-contexts
```

## `aicx refs`

List reference context files from the global store.

```bash
aicx refs [OPTIONS]
```

Options:
- `-H, --hours <HOURS>` filter by file mtime (default: `48`)
- `-p, --project <PROJECT>` filter by project

Example:

```bash
aicx refs -H 72 -p CodeScribe
```

## `aicx state`

Manage dedup state.

```bash
aicx state [OPTIONS]
```

Options:
- `--info` show state statistics
- `--reset` reset dedup hashes
- `-p, --project <PROJECT>` project scope for reset

Example:

```bash
aicx state --info
```

## `aicx init`

Initialize repo context and run an agent.

```bash
aicx init [OPTIONS]
```

Options:
- `-p, --project <PROJECT>` project name override
- `-a, --agent <AGENT>` `claude` or `codex`
- `--model <MODEL>` model override
- `-H, --hours <HOURS>` context horizon (default: `4800`)
- `--max-lines <MAX_LINES>` max lines per section (default: `1200`)
- `--user-only` exclude assistant + reasoning messages from context (default: assistant included)
- `--action <ACTION>` append a focus/action to the prompt
- `--agent-prompt <PROMPT>` append additional prompt text after core rules (verbatim)
- `--agent-prompt-file <PATH>` append prompt text loaded from a file (verbatim)
- `--no-run` build context/prompt only
- `--no-confirm` skip interactive confirmation
- `--no-gitignore` do not auto-modify `.gitignore`

Example:

```bash
aicx init --agent codex --no-confirm --action "Audit memory and propose a plan"
```

## Exit Codes

- `0` on success.
- `1` on errors (invalid args, IO failures, runtime errors).
- `--help` and `--version` exit `0`.
